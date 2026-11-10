//! A terminal pane: a PTY running a shell, a vt100 screen, and the glue that
//! makes programs inside behave exactly as they would in the real terminal.

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use std::io::{self, Read, Write};
use sv_proto::ShellEvent;

use crate::shell::Launch;

/// Side effects collected while parsing shell output.
#[derive(Default)]
pub struct Effects {
    /// Shellvisor events reported by the shell hooks or `sv`.
    pub events: Vec<ShellEvent>,
    /// Answers to terminal queries (cursor position, device attributes) → back to the PTY.
    pub replies: Vec<u8>,
    /// Sequences to pass straight to the real terminal (bell, title, clipboard).
    pub outer: Vec<u8>,
}

/// vt100 callbacks: catch what the emulator doesn't handle itself.
#[derive(Default)]
pub struct Hooks {
    fx: Effects,
}

impl vt100::Callbacks for Hooks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.fx.outer.push(0x07);
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.fx.outer.extend_from_slice(b"\x1b]2;");
        self.fx
            .outer
            .extend(title.iter().filter(|b| !b.is_ascii_control()));
        self.fx.outer.push(0x07);
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, name: &[u8]) {
        self.fx.outer.extend_from_slice(b"\x1b]1;");
        self.fx
            .outer
            .extend(name.iter().filter(|b| !b.is_ascii_control()));
        self.fx.outer.push(0x07);
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        // OSC 52: lets vim / tmux-style "copy to system clipboard" keep working.
        self.fx.outer.extend_from_slice(b"\x1b]52;");
        self.fx.outer.extend_from_slice(ty);
        self.fx.outer.push(b';');
        self.fx.outer.extend_from_slice(data);
        self.fx.outer.push(0x07);
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        if let Some(ev) = sv_proto::parse_osc(params) {
            self.fx.events.push(ev);
        }
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (c, i1) {
            // Device status report: "are you ok?" / "where is the cursor?"
            ('n', None) if first == 5 => self.fx.replies.extend_from_slice(b"\x1b[0n"),
            ('n', None) if first == 6 => {
                let (row, col) = screen.cursor_position();
                self.fx
                    .replies
                    .extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // Primary device attributes: VT220-class with ANSI colour.
            ('c', None) if first == 0 => self.fx.replies.extend_from_slice(b"\x1b[?62;22c"),
            // Secondary device attributes.
            ('c', Some(b'>')) if first == 0 => self.fx.replies.extend_from_slice(b"\x1b[>1;10;0c"),
            _ => {}
        }
    }
}

/// Input modes a program inside can switch on. The real terminal must be in
/// the same mode, because it is the one encoding keys and mouse events.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct InputModes {
    keypad: bool,
    cursor: bool,
    paste: bool,
    mouse: vt100::MouseProtocolMode,
    encoding: vt100::MouseProtocolEncoding,
}

impl InputModes {
    /// The modes the real terminal should be in: the program's own, except
    /// that when it doesn't use the mouse and `capture` is on, Shellvisor takes
    /// mouse events itself (SGR encoding) so the wheel can scroll history.
    pub fn effective(self, capture: bool) -> Self {
        if capture && self.mouse == vt100::MouseProtocolMode::None {
            // drags too, so text can be selected with the mouse
            InputModes {
                mouse: vt100::MouseProtocolMode::ButtonMotion,
                encoding: vt100::MouseProtocolEncoding::Sgr,
                ..self
            }
        } else {
            self
        }
    }

    fn of(s: &vt100::Screen) -> Self {
        InputModes {
            keypad: s.application_keypad(),
            cursor: s.application_cursor(),
            paste: s.bracketed_paste(),
            mouse: s.mouse_protocol_mode(),
            encoding: s.mouse_protocol_encoding(),
        }
    }

    /// Escape sequences that move the real terminal from `prev` to `self`.
    pub fn diff(&self, prev: &InputModes) -> Vec<u8> {
        use vt100::MouseProtocolEncoding as E;
        use vt100::MouseProtocolMode as M;
        let mut out = Vec::new();
        if self.keypad != prev.keypad {
            out.extend_from_slice(if self.keypad { b"\x1b=" } else { b"\x1b>" });
        }
        if self.cursor != prev.cursor {
            out.extend_from_slice(if self.cursor {
                b"\x1b[?1h"
            } else {
                b"\x1b[?1l"
            });
        }
        if self.paste != prev.paste {
            out.extend_from_slice(if self.paste {
                b"\x1b[?2004h"
            } else {
                b"\x1b[?2004l"
            });
        }
        if self.mouse != prev.mouse {
            out.extend_from_slice(b"\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l");
            match self.mouse {
                M::None => {}
                M::Press => out.extend_from_slice(b"\x1b[?9h"),
                M::PressRelease => out.extend_from_slice(b"\x1b[?1000h"),
                M::ButtonMotion => out.extend_from_slice(b"\x1b[?1002h"),
                M::AnyMotion => out.extend_from_slice(b"\x1b[?1003h"),
            }
        }
        if self.encoding != prev.encoding {
            out.extend_from_slice(b"\x1b[?1005l\x1b[?1006l");
            match self.encoding {
                E::Default => {}
                E::Utf8 => out.extend_from_slice(b"\x1b[?1005h"),
                E::Sgr => out.extend_from_slice(b"\x1b[?1006h"),
            }
        }
        out
    }

    /// Sequences that turn every mode off (used on exit).
    pub fn reset() -> &'static [u8] {
        b"\x1b>\x1b[?1l\x1b[?2004l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l"
    }
}

pub struct Pane {
    parser: vt100::Parser<Hooks>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The shell's process id.
    pid: Option<u32>,
    /// Lines scrolled back from the bottom (0 = live).
    scroll: usize,
}

pub struct Spawned {
    pub pane: Pane,
    pub reader: Box<dyn Read + Send>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pane {
    pub fn spawn(
        launch: &Launch,
        extra_env: &[(String, String)],
        cwd: Option<&std::path::Path>,
        rows: u16,
        cols: u16,
        scrollback: usize,
    ) -> io::Result<Spawned> {
        let pty = portable_pty::native_pty_system();
        let pair = pty
            .openpty(size(rows, cols))
            .map_err(|e| io::Error::other(e.to_string()))?;
        let mut cmd = CommandBuilder::new(&launch.program);
        cmd.args(&launch.args);
        match cwd.filter(|d| d.is_dir()) {
            Some(d) => cmd.cwd(d),
            None => {
                if let Ok(d) = std::env::current_dir() {
                    cmd.cwd(d);
                }
            }
        }
        cmd.env_remove("COLUMNS");
        cmd.env_remove("LINES");
        // Shellvisor is the terminal now: the outer terminal's markers would
        // make shell scripts (e.g. Ubuntu's vte.sh) think they run inside it
        for k in [
            "VTE_VERSION",
            "ITERM_SESSION_ID",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "WEZTERM_PANE",
            "TERM_SESSION_ID",
        ] {
            cmd.env_remove(k);
        }
        for (k, v) in launch.env.iter().chain(extra_env) {
            cmd.env(k, v);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let killer = child.clone_killer();
        let pid = child.process_id();
        let parser = vt100::Parser::new_with_callbacks(rows, cols, scrollback, Hooks::default());
        Ok(Spawned {
            pane: Pane {
                parser,
                writer,
                master: pair.master,
                killer,
                pid,
                scroll: 0,
            },
            reader,
            child,
        })
    }

    /// Feed shell output. Answers terminal queries directly and returns the
    /// remaining side effects for the caller.
    pub fn feed(&mut self, bytes: &[u8]) -> Effects {
        self.parser.process(bytes);
        let mut fx = std::mem::take(&mut self.parser.callbacks_mut().fx);
        if !fx.replies.is_empty() {
            let replies = std::mem::take(&mut fx.replies);
            let _ = self.write(&replies);
        }
        fx
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(size(rows, cols));
        self.set_scroll(self.scroll);
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Scroll back by `delta` lines (negative = towards live output).
    pub fn scroll_by(&mut self, delta: isize) {
        let target = (self.scroll as isize + delta).max(0) as usize;
        self.set_scroll(target);
    }

    fn set_scroll(&mut self, lines: usize) {
        self.parser.screen_mut().set_scrollback(lines);
        // vt100 clamps to what is actually available.
        self.scroll = self.parser.screen().scrollback();
    }

    /// Scroll so that `lines` of scrollback are above the screen's top (0 = live).
    pub fn scroll_to(&mut self, lines: usize) {
        self.set_scroll(lines);
    }

    /// Forget this pane's scrollback (its screen stays).
    pub fn clear_scrollback(&mut self) {
        self.parser.process(b"\x1b[3J");
        self.scroll = 0;
    }

    /// The shell's process id.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// The process group in the foreground of this terminal (the running
    /// job), if it isn't the shell itself.
    pub fn foreground_job(&self) -> Option<i32> {
        let pg = self.master.process_group_leader()?;
        (Some(pg as u32) != self.pid).then_some(pg)
    }

    /// Input modes the program inside has asked for.
    pub fn input_modes(&self) -> InputModes {
        InputModes::of(self.parser.screen())
    }

    pub fn mouse_mode(&self) -> vt100::MouseProtocolMode {
        self.parser.screen().mouse_protocol_mode()
    }

    pub fn mouse_encoding(&self) -> vt100::MouseProtocolEncoding {
        self.parser.screen().mouse_protocol_encoding()
    }

    /// Does the program inside want mouse events itself?
    pub fn wants_mouse(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    pub fn in_alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Where the cursor should be drawn, if visible.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let s = self.parser.screen();
        if self.scroll > 0 || s.hide_cursor() {
            return None;
        }
        let (row, col) = s.cursor_position();
        Some((col, row))
    }
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Draws a vt100 screen into a ratatui buffer.
pub struct TermView<'a>(pub &'a vt100::Screen);

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut m = Modifier::empty();
    if cell.bold() {
        m |= Modifier::BOLD;
    }
    if cell.dim() {
        m |= Modifier::DIM;
    }
    if cell.italic() {
        m |= Modifier::ITALIC;
    }
    if cell.underline() {
        m |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        m |= Modifier::REVERSED;
    }
    Style::default()
        .fg(color(cell.fgcolor()))
        .bg(color(cell.bgcolor()))
        .add_modifier(m)
}

impl Widget for TermView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.0.size();
        for row in 0..area.height.min(rows) {
            let y = area.y + row;
            let mut col = 0;
            while col < area.width.min(cols) {
                let x = area.x + col;
                let Some(cell) = self.0.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                let style = cell_style(cell);
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                if cell.is_wide() && col + 1 < area.width {
                    buf.set_stringn(x, y, text, 2, style);
                    col += 2;
                } else {
                    buf[(x, y)].set_symbol(text).set_style(style);
                    col += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> vt100::Parser<Hooks> {
        vt100::Parser::new_with_callbacks(5, 20, 100, Hooks::default())
    }

    /// Small deterministic generator, so a failure can be replayed.
    pub(crate) struct Rng(u64);
    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// Terminal-ish noise: escape sequences cut at any point, our own hook
    /// OSCs with good and bad payloads, UTF-8 (wide, combining, broken),
    /// control characters and plain random bytes.
    fn noise(r: &mut Rng) -> Vec<u8> {
        const BITS: &[&[u8]] = &[
            b"\x1b[",
            b"\x1b]",
            b"\x1bP",
            b"\x1b\\",
            b"\x07",
            b"\x1b(0",
            b"\x1b)B",
            b"\x1b7",
            b"\x1b8",
            b"\x1bc",
            b"\x1bM",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[?25l",
            b"\x1b[?1000h",
            b"\x1b[?1006h",
            b"\x1b[?2004h",
            b"\x1b[r",
            b"\x1b[3;5r",
            b"\x1b[2J",
            b"\x1b[3J",
            b"\x1b[K",
            b"\x1b[@",
            b"\x1b[P",
            b"\x1b[L",
            b"\x1b[M",
            b"\x1b[S",
            b"\x1b[T",
            b"\x1b[6n",
            b"\x1b[c",
            b"\x1b[>c",
            b"\x1b[38;2;1;2;3m",
            b"\x1b[48;5;200m",
            b"\x1b[1;4;7m",
            b"\x1b[0m",
            b"\x1b[999;999H",
            b"\x1b[65535;65535H",
            b"\x1b[0;0H",
            b"\x1b[99999999999X",
            b"\x1b[4h",
            b"\x1b[4l",
            b"\x1b[20h",
            b"\x1b[?7l",
            b"\x1b[?6h",
            b"\x1b]7;file://host/tmp/a%20b\x07",
            b"\x1b]7;file://\x07",
            b"\x1b]7770;start\x07",
            b"\x1b]7770;prompt\x1b\\",
            b"\x1b]7770;end;1\x07",
            b"\x1b]7770;end;x\x07",
            b"\x1b]7770;end;99999999999999\x07",
            b"\x1b]7770;cmd;bHMgLWxh\x07",
            b"\x1b]7770;cmd;!!!\x07",
            b"\x1b]7770;mode;exit\x07",
            b"\x1b]7770;mode;zzz\x07",
            b"\x1b]7770;table;W10=\x07",
            b"\x1b]7770;flow;e30=\x07",
            b"\x1b]7770\x07",
            b"\x1b]2;title\x1b[31m\x07",
            b"\x1b]52;c;aGk=\x07",
            b"\x1b]8;;http://x\x07",
            b"\x1b]133;A\x07",
            "\u{4e16}\u{754c}".as_bytes(),
            "e\u{301}".as_bytes(),
            "\u{1f600}".as_bytes(),
            b"\xf0\x9f",
            b"\xc3",
            b"\xff\xfe",
            b"\t",
            b"\r\n",
            b"\n",
            b"\x08",
            b"\x0e",
            b"\x0f",
            b"\x00",
            b"\x7f",
            b"\x9b",
            b"abc",
            b"0123456789",
            b";",
            b"?",
            b":",
        ];
        let mut v = Vec::new();
        for _ in 0..r.below(40) {
            if r.below(5) == 0 {
                for _ in 0..r.below(12) {
                    v.push(r.next() as u8);
                }
            } else {
                let b = BITS[r.below(BITS.len())];
                // sometimes only part of it: sequences split across reads
                let n = if r.below(4) == 0 {
                    r.below(b.len() + 1)
                } else {
                    b.len()
                };
                v.extend_from_slice(&b[..n]);
            }
        }
        v
    }

    #[test]
    fn random_input_never_panics_or_leaks_escapes_to_the_terminal() {
        // SV_FUZZ_SEED / SV_FUZZ_ROUNDS for longer runs (the weekly CI job);
        // SV_FUZZ_LOG=1 prints every step so a failure can be replayed.
        let seed = std::env::var("SV_FUZZ_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5eed_5eed_1234_5678u64);
        let rounds = std::env::var("SV_FUZZ_ROUNDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1500);
        let mut r = Rng(seed.max(1));
        for round in 0..rounds {
            let rows = 1 + r.below(40) as u16;
            let cols = 1 + r.below(120) as u16;
            let sb = r.below(200);
            let mut p = vt100::Parser::new_with_callbacks(rows, cols, sb, Hooks::default());
            if std::env::var("SV_FUZZ_LOG").is_ok() {
                eprintln!("R{round} new {rows}x{cols} {sb}");
            }
            for _ in 0..1 + r.below(6) {
                let bytes = noise(&mut r);
                // fed in random pieces, like reads from a pty
                let mut i = 0;
                while i < bytes.len() {
                    let n = 1 + r.below(bytes.len() - i);
                    if std::env::var("SV_FUZZ_LOG").is_ok() {
                        eprintln!(
                            "P {}",
                            bytes[i..i + n]
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>()
                        );
                    }
                    p.process(&bytes[i..i + n]);
                    i += n;
                }
                if r.below(3) == 0 {
                    let (nr, nc) = (1 + r.below(50) as u16, 1 + r.below(150) as u16);
                    if std::env::var("SV_FUZZ_LOG").is_ok() {
                        eprintln!("S {nr}x{nc}");
                    }
                    p.screen_mut().set_size(nr, nc);
                }
                if r.below(4) == 0 {
                    let n = r.below(300);
                    if std::env::var("SV_FUZZ_LOG").is_ok() {
                        eprintln!("B {n}");
                    }
                    p.screen_mut().set_scrollback(n);
                }
                let (h, w) = p.screen().size();
                let (cy, cx) = p.screen().cursor_position();
                assert!(
                    cy < h && cx <= w,
                    "round {round}: cursor {cy},{cx} outside {h}x{w}"
                );
                let _ = p.screen().contents();
                let _ = p.screen().contents_formatted();
                let fx = std::mem::take(&mut p.callbacks_mut().fx);
                // What goes to the real terminal is only our own frames:
                // title, icon name, clipboard (ESC ] … BEL) and the bell.
                // Nothing a program prints may smuggle other escapes out.
                let mut k = 0;
                while k < fx.outer.len() {
                    match fx.outer[k] {
                        0x07 => k += 1,
                        0x1b => {
                            assert_eq!(
                                fx.outer.get(k + 1),
                                Some(&b']'),
                                "round {round}: {:?}",
                                fx.outer
                            );
                            let end = fx.outer[k..]
                                .iter()
                                .position(|b| *b == 0x07)
                                .expect("framed");
                            let body = &fx.outer[k + 2..k + end];
                            assert!(
                                !body.contains(&0x1b),
                                "round {round}: ESC inside {:?}",
                                body
                            );
                            k += end + 1;
                        }
                        b => panic!("round {round}: stray byte {b} in {:?}", fx.outer),
                    }
                }
            }
        }
    }

    fn rows_of(p: &vt100::Parser<Hooks>) -> Vec<String> {
        let (_, cols) = p.screen().size();
        p.screen()
            .rows(0, cols)
            .map(|r| r.trim_end().to_string())
            .collect()
    }

    #[test]
    fn resizing_reflows_lines_like_iterm() {
        let mut p = vt100::Parser::new_with_callbacks(5, 20, 100, Hooks::default());
        // one long line (soft-wrapped at 20), one short one, then a prompt
        p.process(b"abcdefghijklmnopqrstuvwxyz0123\r\nshort line\r\n$ ");
        assert_eq!(
            rows_of(&p)[..4],
            ["abcdefghijklmnopqrst", "uvwxyz0123", "short line", "$"]
        );
        // wider: the long line is one row again; the cursor stays after "$ "
        p.screen_mut().set_size(5, 40);
        assert_eq!(
            rows_of(&p)[..3],
            ["abcdefghijklmnopqrstuvwxyz0123", "short line", "$"]
        );
        assert_eq!(p.screen().cursor_position(), (2, 2));
        // much narrower: nothing is cut, it wraps
        p.screen_mut().set_size(5, 8);
        let all = p.screen().contents();
        assert!(all.contains("short li") && all.contains("ne"), "{all:?}");
        let (r, c) = p.screen().cursor_position();
        assert_eq!(rows_of(&p)[usize::from(r)], "$");
        assert_eq!(c, 2);
        // and back: exactly as it was
        p.screen_mut().set_size(5, 20);
        assert_eq!(
            rows_of(&p)[..4],
            ["abcdefghijklmnopqrst", "uvwxyz0123", "short line", "$"]
        );
        // typing continues at the right place
        p.process(b"ls");
        assert_eq!(rows_of(&p)[3], "$ ls");
    }

    #[test]
    fn a_full_width_line_survives_narrow_and_back() {
        // printed wide, then the split is dragged narrow and wide again
        let mut p = vt100::Parser::new_with_callbacks(4, 40, 100, Hooks::default());
        p.process(b"==> Installing at-spi2-core dependency: gtk+3\r\n$ ");
        p.screen_mut().set_size(4, 12);
        p.screen_mut().set_size(4, 40);
        assert_eq!(rows_of(&p)[0], "==> Installing at-spi2-core dependency:");
        assert_eq!(rows_of(&p)[1], "gtk+3");
        p.screen_mut().set_size(4, 60);
        assert_eq!(
            rows_of(&p)[0],
            "==> Installing at-spi2-core dependency: gtk+3"
        );
    }

    #[test]
    fn fewer_rows_keep_the_cursor_and_push_lines_to_scrollback() {
        let mut p = vt100::Parser::new_with_callbacks(8, 20, 100, Hooks::default());
        for i in 1..=6 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        p.process(b"$ ");
        p.screen_mut().set_size(3, 20);
        assert_eq!(rows_of(&p), ["line 5", "line 6", "$"]);
        assert_eq!(p.screen().cursor_position(), (2, 2));
        // the rest is in the scrollback
        p.screen_mut().set_scrollback(4);
        assert_eq!(rows_of(&p)[0], "line 1");
        p.screen_mut().set_scrollback(0);
        // taller again: the scrollback stays where it is, the prompt too
        p.screen_mut().set_size(8, 20);
        assert_eq!(p.screen().cursor_position().1, 2);
        assert!(rows_of(&p).iter().any(|r| r == "$"));
    }

    #[test]
    fn wide_characters_reflow_whole() {
        let mut p = vt100::Parser::new_with_callbacks(4, 10, 100, Hooks::default());
        p.process("ab世界cd世界ef\r\n$ ".as_bytes());
        p.screen_mut().set_size(4, 5);
        for r in rows_of(&p) {
            assert!(!r.contains('\u{fffd}'), "{r:?}");
        }
        p.screen_mut().set_size(4, 20);
        assert_eq!(rows_of(&p)[0], "ab世界cd世界ef");
    }

    #[test]
    fn shellvisor_osc_becomes_event_and_is_not_drawn() {
        let mut p = parser();
        p.process(b"hi\x1b]7;file://h/tmp\x07\x1b]7770;end;2\x1b\\there\x1b]133;C\x07");
        let fx = std::mem::take(&mut p.callbacks_mut().fx);
        assert_eq!(
            fx.events,
            vec![
                ShellEvent::Cwd("/tmp".into()),
                ShellEvent::CommandFinished(Some(2))
            ]
        );
        assert_eq!(p.screen().contents(), "hithere");
    }

    #[test]
    fn osc_split_across_reads() {
        let mut p = parser();
        p.process(b"\x1b]7770;mo");
        p.process(b"de;exit\x07");
        let fx = std::mem::take(&mut p.callbacks_mut().fx);
        assert_eq!(fx.events, vec![ShellEvent::Mode(sv_proto::Mode::Exit)]);
    }

    #[test]
    fn cursor_position_query_is_answered() {
        let mut p = parser();
        p.process(b"abc\x1b[6n");
        let fx = std::mem::take(&mut p.callbacks_mut().fx);
        assert_eq!(fx.replies, b"\x1b[1;4R");
    }

    #[test]
    fn title_and_bell_pass_through() {
        let mut p = parser();
        p.process(b"\x1b]2;vim\x07\x07");
        let fx = std::mem::take(&mut p.callbacks_mut().fx);
        assert_eq!(fx.outer, b"\x1b]2;vim\x07\x07");
    }

    #[test]
    fn input_modes_are_mirrored() {
        let mut p = parser();
        let before = InputModes::of(p.screen());
        p.process(b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h");
        let after = InputModes::of(p.screen());
        let d = after.diff(&before);
        let s = String::from_utf8_lossy(&d);
        assert!(s.contains("\x1b[?1h") && s.contains("\x1b[?2004h"));
        assert!(s.contains("\x1b[?1002h") && s.contains("\x1b[?1006h"));
        assert!(after.diff(&after).is_empty());
        let back = before.diff(&after);
        assert!(String::from_utf8_lossy(&back).contains("\x1b[?2004l"));
    }

    #[test]
    fn renders_wide_and_styled_cells() {
        let mut p = parser();
        p.process("\x1b[1;31mR\x1b[0m界x".as_bytes());
        let area = Rect::new(0, 0, 20, 5);
        let mut buf = Buffer::empty(area);
        TermView(p.screen()).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "R");
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));
        assert!(buf[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(1, 0)].symbol(), "界");
        assert_eq!(buf[(3, 0)].symbol(), "x");
    }
}

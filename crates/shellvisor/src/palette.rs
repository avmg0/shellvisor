//! The design boards' dark palette: a very dark blue-grey background,
//! slightly lighter panels and bars, cool grey text, amber / cyan / green /
//! red accents (the same hex values as the boards' CSS).
//!
//! Panes are drawn with Shellvisor's indexed colours; this pass turns them
//! into the design's colours after each frame. Inside shell panes only the
//! default background and text change, so programs keep their own colours.
//! Terminals without 24-bit colour get the nearest of the 256 colours —
//! except the page, panel and shell backgrounds, which are left to the
//! terminal's own background there (256 colours have no very dark blue, only
//! greys; `sv terminal-colors` gives Terminal.app the design's background).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

pub const BG: (u8, u8, u8) = (0x0b, 0x0e, 0x12);
pub const PANEL: (u8, u8, u8) = (0x11, 0x15, 0x1b);
pub const BAR: (u8, u8, u8) = (0x15, 0x1a, 0x21);
pub const SELECT: (u8, u8, u8) = (0x25, 0x2b, 0x35);
/// Behind a shell's text.
pub const SHELL: (u8, u8, u8) = (0x0e, 0x12, 0x17);
pub const BORDER: (u8, u8, u8) = (0x26, 0x2c, 0x36);
pub const TEXT: (u8, u8, u8) = (0xd6, 0xdb, 0xe3);
pub const DIM: (u8, u8, u8) = (0x8a, 0x94, 0xa3);
pub const FAINT: (u8, u8, u8) = (0x6b, 0x74, 0x82);
pub const AMBER: (u8, u8, u8) = (0xe0, 0xa8, 0x4a);
pub const CYAN: (u8, u8, u8) = (0x6c, 0xc9, 0xd6);
pub const GREEN: (u8, u8, u8) = (0x8f, 0xd1, 0x8f);
pub const RED: (u8, u8, u8) = (0xf0, 0x71, 0x78);
pub const BLUE: (u8, u8, u8) = (0x82, 0xaa, 0xff);
pub const MAGENTA: (u8, u8, u8) = (0xc7, 0x92, 0xea);
pub const SHELL_TINT: (u8, u8, u8) = (0x3a, 0x2f, 0x1a);
/// Cards (flow stages, downloads).
pub const CARD: (u8, u8, u8) = (0x12, 0x18, 0x21);
/// Small badges ("1", "2" on stage cards).
pub const BADGE: (u8, u8, u8) = (0x2a, 0x31, 0x40);
/// Outlines of cards, the command box, pills.
pub const STROKE: (u8, u8, u8) = (0x2f, 0x36, 0x42);
pub const ERR_BG: (u8, u8, u8) = (0x16, 0x0f, 0x12);
pub const ERR_BORDER: (u8, u8, u8) = (0x4a, 0x2a, 0x30);

/// Colours some panes draw with directly (cards, pills, tracks): exact in
/// the design palette, plain indexed colours otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Card,
    Badge,
    Stroke,
    /// The empty part of a progress bar.
    Track,
    Bar,
    ErrBg,
    ErrBorder,
    /// Text selected with the mouse.
    Selection,
}

use std::sync::atomic::{AtomicU8, Ordering};
/// 0: not the design palette (terminal background, light), 1: design in
/// 256 colours, 2: design in 24-bit colour.
static MODE: AtomicU8 = AtomicU8::new(0);

pub fn set_mode(design: bool, true_color: bool) {
    MODE.store(
        match (design, true_color) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        },
        Ordering::Relaxed,
    );
}

pub fn role(r: Role) -> Color {
    let c = match r {
        Role::Card => CARD,
        Role::Badge => BADGE,
        Role::Stroke => STROKE,
        Role::Track => SELECT,
        Role::Bar => BAR,
        Role::ErrBg => ERR_BG,
        Role::ErrBorder => ERR_BORDER,
        Role::Selection => (0x2c, 0x40, 0x5a),
    };
    match MODE.load(Ordering::Relaxed) {
        2 => Color::Rgb(c.0, c.1, c.2),
        1 => match r {
            // too close to the page in 256 colours: no fill
            Role::Card | Role::ErrBg => Color::Reset,
            _ => Color::Indexed(nearest_256(c)),
        },
        _ => match r {
            Role::Card | Role::ErrBg => Color::Reset,
            Role::Badge => Color::Indexed(238),
            Role::Stroke => Color::Indexed(240),
            Role::Track => Color::Indexed(237),
            Role::Bar => Color::Indexed(236),
            Role::ErrBorder => Color::Indexed(167),
            Role::Selection => Color::Indexed(24),
        },
    }
}

/// Does the outer terminal show 24-bit colour? ($COLORTERM, or a terminal
/// known to.) Read before Shellvisor sets these for its own shells.
pub fn truecolor_terminal() -> bool {
    if std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal") {
        // Terminal.app shows 24-bit colour from macOS 26 on
        return macos_major().is_some_and(|v| v >= 26);
    }
    let ct = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_lowercase();
    if ct.contains("truecolor") || ct.contains("24bit") {
        return true;
    }
    let tp = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    matches!(
        tp.as_str(),
        "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Hyper" | "Tabby" | "rio" | "WarpTerminal"
    ) || term.contains("kitty")
        || term.contains("alacritty")
        || term.contains("direct")
}

/// macOS's major version (26 for macOS 26 "Tahoe"), from SystemVersion.plist.
pub fn macos_major() -> Option<u32> {
    let t = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let i = t.find("<key>ProductVersion</key>")?;
    let rest = &t[i..];
    let s = rest.find("<string>")? + 8;
    let e = rest[s..]
        .find('.')
        .map(|e| s + e)
        .or_else(|| rest[s..].find('<').map(|e| s + e))?;
    rest[s..e].trim().parse().ok()
}

/// The xterm 256-colour index closest to an RGB colour.
pub fn nearest_256((r, g, b): (u8, u8, u8)) -> u8 {
    let levels = [0u8, 95, 135, 175, 215, 255];
    let dist = |a: (u8, u8, u8)| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, r) + d(a.1, g) + d(a.2, b)
    };
    let mut best = (16u8, i32::MAX);
    for (i, rr) in levels.iter().enumerate() {
        for (j, gg) in levels.iter().enumerate() {
            for (k, bb) in levels.iter().enumerate() {
                let dd = dist((*rr, *gg, *bb));
                if dd < best.1 {
                    best = (16 + 36 * i as u8 + 6 * j as u8 + k as u8, dd);
                }
            }
        }
    }
    for n in 0..24u8 {
        let v = 8 + 10 * n;
        let dd = dist((v, v, v));
        if dd < best.1 {
            best = (232 + n, dd);
        }
    }
    best.0
}

fn rgb(c: (u8, u8, u8), true_color: bool) -> Color {
    if true_color {
        Color::Rgb(c.0, c.1, c.2)
    } else if c == BG || c == SHELL || c == PANEL {
        // 256 colours only have greys this dark: keep the terminal's own
        Color::Reset
    } else {
        Color::Indexed(nearest_256(c))
    }
}

fn is_box(sym: &str) -> bool {
    sym.chars()
        .next()
        .is_some_and(|c| ('\u{2500}'..='\u{257f}').contains(&c))
}

/// A Shellvisor colour in the design palette (None: keep it).
fn map(c: Color, fg: bool, border: bool) -> Option<(u8, u8, u8)> {
    Some(match c {
        Color::Reset => {
            if fg {
                TEXT
            } else {
                BG
            }
        }
        Color::Indexed(n) => match n {
            // the terminal's 16 colours (folder colours from settings)
            1 | 9 => RED,
            2 | 10 => GREEN,
            3 | 11 => AMBER,
            4 | 12 => BLUE,
            5 | 13 => MAGENTA,
            6 | 14 => CYAN,
            8 => FAINT,
            179 | 178 => AMBER,
            73 | 37 => CYAN,
            108 | 35 => GREEN,
            167 | 160 => RED,
            110 | 39 => BLUE,
            170 => MAGENTA,
            250..=255 | 15 => TEXT,
            244..=249 => DIM,
            240..=243 if fg && border => BORDER,
            240..=243 if fg => FAINT,
            240..=243 => BORDER,
            238 | 237 => SELECT,
            236 => BAR,
            235 => PANEL,
            232..=234 => BG,
            16 => BG,
            94 => SHELL_TINT,
            _ => return None,
        },
        _ => return None,
    })
}

/// Where things are on screen, for the recolouring.
#[derive(Debug, Default, Clone)]
pub struct Areas {
    /// Shell panes' text areas: only the default colours change there.
    pub shells: Vec<Rect>,
    /// Whole shell panes (frame included): the shell background.
    pub shell_panes: Vec<Rect>,
    /// Whole tool panes (files, system, flow): the panel background.
    pub tool_panes: Vec<Rect>,
}

fn inside(rs: &[Rect], x: u16, y: u16) -> bool {
    rs.iter()
        .any(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
}

/// Recolour a drawn frame in the design palette.
pub fn apply(buf: &mut Buffer, a: &Areas, true_color: bool) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if inside(&a.shells, x, y) {
                if cell.fg == Color::Reset {
                    cell.fg = rgb(TEXT, true_color);
                }
                if cell.bg == Color::Reset {
                    cell.bg = rgb(SHELL, true_color);
                }
                continue;
            }
            let ground = if inside(&a.tool_panes, x, y) {
                PANEL
            } else if inside(&a.shell_panes, x, y) {
                SHELL
            } else {
                BG
            };
            let border = is_box(cell.symbol());
            if let Some(c) = map(cell.fg, true, border) {
                cell.fg = rgb(c, true_color);
            }
            if cell.bg == Color::Reset {
                cell.bg = rgb(ground, true_color);
            } else if let Some(c) = map(cell.bg, false, false) {
                cell.bg = rgb(c, true_color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_map_to_the_design() {
        assert_eq!(map(Color::Reset, false, false), Some(BG));
        assert_eq!(map(Color::Indexed(179), true, false), Some(AMBER));
        assert_eq!(
            map(Color::Indexed(243), true, true),
            Some(BORDER),
            "faint box lines are borders"
        );
        assert_eq!(map(Color::Indexed(243), true, false), Some(FAINT));
        assert_eq!(
            map(Color::Indexed(200), true, false),
            None,
            "agent colours stay"
        );
        assert_eq!(nearest_256((0, 0, 0)), 16);
        assert_eq!(nearest_256((0xd6, 0xdb, 0xe3)), 253);
        assert!((232..=234).contains(&nearest_256(BG)));
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        buf[(0, 0)].set_symbol("│").set_fg(Color::Indexed(243));
        buf[(2, 0)].set_fg(Color::Indexed(9));
        let areas = Areas {
            shells: vec![Rect::new(2, 0, 2, 1)],
            shell_panes: vec![Rect::new(2, 0, 2, 1)],
            tool_panes: vec![Rect::new(1, 0, 1, 1)],
        };
        apply(&mut buf, &areas, true);
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x26, 0x2c, 0x36));
        assert_eq!(
            buf[(0, 0)].bg,
            Color::Rgb(0x0b, 0x0e, 0x12),
            "outside panes: the page background"
        );
        assert_eq!(
            buf[(1, 0)].bg,
            Color::Rgb(0x11, 0x15, 0x1b),
            "tool panes: the panel background"
        );
        assert_eq!(
            buf[(2, 0)].fg,
            Color::Indexed(9),
            "a program's colour in a shell stays"
        );
        assert_eq!(
            buf[(3, 0)].bg,
            Color::Rgb(0x0e, 0x12, 0x17),
            "a shell's default background"
        );
    }
}

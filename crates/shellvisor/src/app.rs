//! The workspace: tabs of split panes, each a real shell, plus the event loop.
//!
//! Threads (tiny, blocking — no async runtime):
//!   input            stdin bytes                ─┐
//!   pty-<id>         one per pane, shell output   ├─▶ channel ─▶ app thread
//!   wait-<id>        one per pane, exit status    │
//!   signal           resize / terminate          ─┘
//!
//! The app thread owns all state and redraws only when something changed.

use crate::history::{Entry, History};
use crate::keys::{self, Cmd};
use crate::mouse::MouseEv;
use crate::shell::Launch;
use crate::term::{InputModes, Pane};
use crate::ui;
use crate::workspace::{Axis, PaneId, Tab, neighbor};
use crossterm::{cursor, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};
use sv_proto::{Mode, ShellEvent};

const FRAME: Duration = Duration::from_millis(16);
const SPIN: Duration = Duration::from_millis(100);
pub const NOTICE: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Tab bar, bordered panes, status bar.
    Full,
    /// Just the focused shell, full screen — looks like a plain terminal.
    Basic,
}

pub struct Options {
    pub launch: Launch,
    pub extra_env: Vec<(String, String)>,
    pub scrollback: usize,
    pub mouse: bool,
    pub history: History,
    pub start_view: View,
}

enum Ev {
    Input(Vec<u8>),
    Output(PaneId, Vec<u8>),
    PaneExit(PaneId, i32),
    Resize,
    Quit,
}

/// One shell and what we know about it.
pub struct PaneState {
    pub pane: Pane,
    pub shell: String,
    pub cwd: String,
    pub command: String,
    pub started: Option<Instant>,
    pub running: bool,
    pub last_exit: Option<i32>,
    /// New output while the pane's tab wasn't on screen.
    pub unseen: bool,
    pub bell: bool,
    size: (u16, u16),
}

/// Something waiting for a y/n answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Quit,
    CloseTab(usize),
    ClosePane(PaneId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    /// ctrl+] pressed, waiting for the next key.
    Leader,
    Help,
    Rename(String),
    Confirm(Pending, String),
}

pub struct App {
    pub panes: HashMap<PaneId, PaneState>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub view: View,
    pub modal: Modal,
    pub notice: Option<(String, Instant)>,
    pub screen: Rect,
    pub epoch: Instant,
    outer_modes: InputModes,
    next_id: PaneId,
    opts: Options,
    tx: Sender<Ev>,
    option_hint_shown: bool,
    quit: Option<i32>,
    dirty: bool,
}

static RESTORED: AtomicBool = AtomicBool::new(false);

/// Put the real terminal back the way we found it. Safe to call twice.
fn restore_terminal() {
    if RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = out.write_all(InputModes::reset());
    let _ = execute!(out, terminal::LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();
}

fn spawn_thread<F: FnOnce() + Send + 'static>(name: String, f: F) -> io::Result<()> {
    thread::Builder::new()
        .name(name)
        .stack_size(128 * 1024)
        .spawn(f)
        .map(|_| ())
}

pub fn run(opts: Options) -> io::Result<i32> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let (tx, rx) = mpsc::channel::<Ev>();

    {
        use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
        let tx = tx.clone();
        let mut signals = signal_hook::iterator::Signals::new([SIGWINCH, SIGTERM, SIGHUP])?;
        spawn_thread("signal".into(), move || {
            for s in signals.forever() {
                let ev = if s == SIGWINCH { Ev::Resize } else { Ev::Quit };
                if tx.send(ev).is_err() {
                    break;
                }
            }
        })?;
    }

    let mut app = App {
        panes: HashMap::new(),
        tabs: Vec::new(),
        active: 0,
        view: opts.start_view,
        modal: Modal::None,
        notice: None,
        screen: Rect::new(0, 0, cols, rows),
        epoch: Instant::now(),
        outer_modes: InputModes::default(),
        next_id: 1,
        opts,
        tx: tx.clone(),
        option_hint_shown: false,
        quit: None,
        dirty: true,
    };
    let first = app.new_pane(None)?;
    app.tabs.push(Tab::new(first));

    // Take over the screen. From here on, always restore — even on panic.
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), terminal::EnterAlternateScreen)?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev_hook(info);
    }));

    {
        let tx = tx.clone();
        spawn_thread("input".into(), move || {
            let stdin = io::stdin();
            let mut lock = stdin.lock();
            let mut buf = [0u8; 8192];
            loop {
                match lock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Ev::Input(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;
    }
    drop(tx);

    let result = app.event_loop(rx);
    for p in app.panes.values_mut() {
        p.pane.kill();
    }
    restore_terminal();
    result
}

/// A blank full-screen ratatui terminal. Built directly (not via
/// `Terminal::clear`/`autoresize`) so we never ask the real terminal for its
/// cursor position.
fn fresh_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    let mut out = io::stdout();
    execute!(out, terminal::Clear(terminal::ClearType::All))?;
    Terminal::new(CrosstermBackend::new(out))
}

pub fn shrink(r: Rect) -> Rect {
    if r.width < 3 || r.height < 3 {
        return r;
    }
    Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2)
}

impl App {
    // ---------------------------------------------------------------- panes

    fn new_pane(&mut self, cwd: Option<PathBuf>) -> io::Result<PaneId> {
        let id = self.next_id;
        self.next_id += 1;
        let (rows, cols) = (self.screen.height.max(2), self.screen.width.max(2));
        let spawned = Pane::spawn(
            &self.opts.launch,
            &self.opts.extra_env,
            cwd.as_deref(),
            rows,
            cols,
            self.opts.scrollback,
        )?;
        {
            let tx = self.tx.clone();
            let mut reader = spawned.reader;
            spawn_thread(format!("pty-{id}"), move || {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(Ev::Output(id, buf[..n].to_vec())).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;
        }
        {
            let tx = self.tx.clone();
            let mut child = spawned.child;
            spawn_thread(format!("wait-{id}"), move || {
                let code = child.wait().map(|s| s.exit_code() as i32).unwrap_or(1);
                let _ = tx.send(Ev::PaneExit(id, code));
            })?;
        }
        let cwd = cwd
            .or_else(|| std::env::current_dir().ok())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.panes.insert(
            id,
            PaneState {
                pane: spawned.pane,
                shell: self.opts.launch.name.clone(),
                cwd,
                command: String::new(),
                started: None,
                running: false,
                last_exit: None,
                unseen: false,
                bell: false,
                size: (rows, cols),
            },
        );
        Ok(id)
    }

    pub fn focused(&self) -> PaneId {
        self.tabs[self.active].focus
    }

    fn focused_cwd(&self) -> Option<PathBuf> {
        self.panes
            .get(&self.focused())
            .map(|p| PathBuf::from(&p.cwd))
    }

    /// (tab bar, content, status bar)
    pub fn regions(&self) -> (Option<Rect>, Rect, Option<Rect>) {
        let s = self.screen;
        match self.view {
            View::Basic => (None, s, None),
            View::Full if s.height < 4 => (None, s, None),
            View::Full => (
                Some(Rect::new(s.x, s.y, s.width, 1)),
                Rect::new(s.x, s.y + 1, s.width, s.height - 2),
                Some(Rect::new(s.x, s.y + s.height - 1, s.width, 1)),
            ),
        }
    }

    /// Visible panes: (id, outer rect, inner rect where the terminal is drawn).
    pub fn visible(&self) -> Vec<(PaneId, Rect, Rect)> {
        let (_, content, _) = self.regions();
        match self.view {
            View::Basic => vec![(self.focused(), content, content)],
            View::Full => self.tabs[self.active]
                .layout(content)
                .into_iter()
                .map(|(id, r)| (id, r, shrink(r)))
                .collect(),
        }
    }

    /// Give every visible pane's shell the size it is drawn at.
    fn relayout(&mut self) {
        for (id, _, inner) in self.visible() {
            if let Some(p) = self.panes.get_mut(&id) {
                let want = (inner.height.max(1), inner.width.max(1));
                if p.size != want {
                    p.pane.resize(want.0, want.1);
                    p.size = want;
                }
            }
        }
        self.dirty = true;
    }

    fn notice(&mut self, msg: impl Into<String>) {
        self.notice = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    fn busy(&self, ids: &[PaneId], except: Option<PaneId>) -> Vec<String> {
        ids.iter()
            .filter(|id| Some(**id) != except)
            .filter_map(|id| self.panes.get(id))
            .filter(|p| p.running)
            .map(|p| p.command.clone())
            .collect()
    }

    fn all_panes(&self) -> Vec<PaneId> {
        self.panes.keys().copied().collect()
    }

    fn describe(cmds: &[String]) -> String {
        match cmds {
            [one] => format!("`{one}` is still running"),
            many => format!("{} commands are still running", many.len()),
        }
    }

    // ---------------------------------------------------------------- loop

    fn event_loop(&mut self, rx: mpsc::Receiver<Ev>) -> io::Result<i32> {
        let mut term = fresh_terminal()?;
        self.relayout();
        self.sync_modes(&mut term)?;
        let mut last_draw = Instant::now() - FRAME;

        loop {
            let timeout = self.next_wakeup(last_draw);
            let mut batch = Vec::new();
            match rx.recv_timeout(timeout) {
                Ok(ev) => batch.push(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
            while batch.len() < 512 {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }

            for ev in batch {
                match ev {
                    Ev::Output(id, bytes) => self.on_output(id, &bytes, &mut term)?,
                    Ev::Input(bytes) => self.on_input(&bytes)?,
                    Ev::PaneExit(id, code) => self.on_pane_exit(id, code),
                    Ev::Resize => {
                        let (cols, rows) = terminal::size().unwrap_or((80, 24));
                        self.screen = Rect::new(0, 0, cols, rows);
                        term = fresh_terminal()?;
                        self.relayout();
                    }
                    Ev::Quit => return Ok(0),
                }
                if let Some(code) = self.quit {
                    return Ok(code);
                }
            }
            self.sync_modes(&mut term)?;

            if let Some((_, at)) = &self.notice {
                if at.elapsed() >= NOTICE {
                    self.notice = None;
                    self.dirty = true;
                }
            }
            if self.spinning() && last_draw.elapsed() >= SPIN {
                self.dirty = true;
            }
            if self.dirty && last_draw.elapsed() >= FRAME {
                term.draw(|f| ui::draw(f, self))?;
                self.dirty = false;
                last_draw = Instant::now();
            }
        }
    }

    /// Is a loading spinner on screen (some command running, full view)?
    fn spinning(&self) -> bool {
        self.view == View::Full && self.panes.values().any(|p| p.running)
    }

    fn next_wakeup(&self, last_draw: Instant) -> Duration {
        if self.dirty {
            return FRAME.saturating_sub(last_draw.elapsed());
        }
        let mut t = Duration::from_secs(60); // clock in the status bar
        if self.spinning() {
            t = t.min(
                SPIN.saturating_sub(last_draw.elapsed())
                    .max(Duration::from_millis(5)),
            );
        }
        if let Some((_, at)) = &self.notice {
            t = t.min((*at + NOTICE).saturating_duration_since(Instant::now()));
        }
        t
    }

    /// Keep the real terminal's input modes in line with the focused program.
    /// In full view we always take the mouse (clicks focus panes).
    fn sync_modes<W: Write>(&mut self, term: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()> {
        let focused = self
            .panes
            .get(&self.focused())
            .map(|p| p.pane.input_modes())
            .unwrap_or_default();
        let capture = self.opts.mouse || self.view == View::Full;
        let want = focused.effective(capture);
        if want != self.outer_modes {
            term.backend_mut()
                .write_all(&want.diff(&self.outer_modes))?;
            term.backend_mut().flush()?;
            self.outer_modes = want;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- output

    fn tab_of(&self, id: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(id))
    }

    fn on_output<W: Write>(
        &mut self,
        id: PaneId,
        bytes: &[u8],
        term: &mut Terminal<CrosstermBackend<W>>,
    ) -> io::Result<()> {
        let focused = self.focused();
        let on_screen = self.visible().iter().any(|(v, _, _)| *v == id);
        let Some(p) = self.panes.get_mut(&id) else {
            return Ok(());
        };
        let fx = p.pane.feed(bytes);
        if !on_screen && !p.unseen {
            p.unseen = true;
            self.dirty = self.view == View::Full;
        }
        // Title only from the focused pane; bell and clipboard from any.
        let mut outer = fx.outer;
        if id != focused {
            outer = strip_titles(&outer);
        }
        if outer.contains(&0x07) && !on_screen {
            p.bell = true;
        }
        if !outer.is_empty() {
            term.backend_mut().write_all(&outer)?;
            term.backend_mut().flush()?;
        }
        for e in fx.events {
            self.on_shell_event(id, e);
        }
        if on_screen {
            self.dirty = true;
        }
        Ok(())
    }

    fn on_shell_event(&mut self, id: PaneId, e: ShellEvent) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        match e {
            ShellEvent::Cwd(path) => {
                p.cwd = path;
                self.dirty = true;
            }
            ShellEvent::CommandText(t) => p.command = t,
            ShellEvent::CommandStarted => {
                p.started = Some(Instant::now());
                p.running = true;
                self.dirty = true;
            }
            ShellEvent::CommandFinished(code) => {
                if !p.command.is_empty() {
                    let duration_ms = p
                        .started
                        .map(|s| s.elapsed().as_millis() as u64)
                        .unwrap_or(0);
                    self.opts.history.append(&Entry {
                        ts: History::now(),
                        cwd: &p.cwd,
                        cmd: &p.command,
                        exit: code,
                        duration_ms,
                        shell: &p.shell,
                    });
                }
                p.running = false;
                p.started = None;
                p.last_exit = code;
                p.command.clear();
                self.dirty = true;
            }
            ShellEvent::PromptShown => {}
            ShellEvent::Mode(Mode::On) => self.set_view(View::Full),
            ShellEvent::Mode(Mode::Off) => self.set_view(View::Basic),
            ShellEvent::Mode(Mode::Exit) => self.request_quit(Some(id)),
        }
    }

    fn on_pane_exit(&mut self, id: PaneId, code: i32) {
        if self.panes.remove(&id).is_some() {
            self.detach(id, code);
        }
        // else: we closed it ourselves already
    }

    /// Take a (removed) pane out of its tab; close the tab if it was the last
    /// pane; quit with `code` if it was the last tab.
    fn detach(&mut self, id: PaneId, code: i32) {
        if let Some(t) = self.tab_of(id) {
            let tab = &mut self.tabs[t];
            match tab.root.clone().remove(id) {
                Some(root) => {
                    tab.root = root;
                    if tab.focus == id {
                        tab.focus = tab.root.panes()[0];
                    }
                    tab.zoomed = false;
                }
                None => {
                    self.tabs.remove(t);
                    if self.tabs.is_empty() {
                        self.quit = Some(code);
                        return;
                    }
                    if self.active >= self.tabs.len() || self.active > t {
                        self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
                    }
                }
            }
        }
        self.relayout();
    }

    // ---------------------------------------------------------------- input

    fn on_input(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut forward: Vec<u8> = Vec::new();
        for key in keys::tokenize(bytes) {
            // Modal states swallow keys first.
            match self.modal.clone() {
                Modal::Help => {
                    self.modal = Modal::None;
                    self.dirty = true;
                    continue;
                }
                Modal::Leader => {
                    self.modal = Modal::None;
                    self.dirty = true;
                    if key == [0x1d] {
                        forward.push(0x1d); // ctrl+] twice = a real ctrl+]
                    } else if let Some(cmd) = keys::after_leader(key) {
                        self.flush_forward(&mut forward)?;
                        self.run_cmd(cmd)?;
                    }
                    continue;
                }
                Modal::Rename(mut name) => {
                    match key {
                        b"\r" => {
                            let n = name.trim().to_string();
                            self.tabs[self.active].name = (!n.is_empty()).then_some(n);
                            self.modal = Modal::None;
                        }
                        b"\x1b" | [0x03] => self.modal = Modal::None,
                        [0x7f] | [0x08] => {
                            name.pop();
                            self.modal = Modal::Rename(name);
                        }
                        k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                            if name.chars().count() < 30 {
                                name.push_str(&String::from_utf8_lossy(k));
                            }
                            self.modal = Modal::Rename(name);
                        }
                        _ => {}
                    }
                    self.dirty = true;
                    continue;
                }
                Modal::Confirm(pending, _) => {
                    match key {
                        b"y" | b"Y" | b"\r" | [0x11] => {
                            self.modal = Modal::None;
                            self.confirmed(pending);
                        }
                        b"n" | b"N" | b"\x1b" | [0x03] => self.modal = Modal::None,
                        _ => {}
                    }
                    self.dirty = true;
                    if self.quit.is_some() {
                        return Ok(());
                    }
                    continue;
                }
                Modal::None => {}
            }

            if let Some(ev) = MouseEv::parse_sgr(key) {
                self.flush_forward(&mut forward)?;
                self.on_mouse(ev)?;
                continue;
            }
            if let Some(cmd) = keys::global(key) {
                self.flush_forward(&mut forward)?;
                self.run_cmd(cmd)?;
                if self.quit.is_some() {
                    return Ok(());
                }
                continue;
            }
            if keys::is_option_char(key) && !self.option_hint_shown {
                self.option_hint_shown = true;
                self.notice(format!(
                    "Alt shortcuts need Option as Alt: {}",
                    keys::option_fix_hint()
                ));
            }
            forward.extend_from_slice(key);
        }
        self.flush_forward(&mut forward)
    }

    fn flush_forward(&mut self, buf: &mut Vec<u8>) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let id = self.focused();
        if let Some(p) = self.panes.get_mut(&id) {
            if p.pane.scroll() > 0 {
                p.pane.scroll_by(isize::MIN / 2);
                self.dirty = true;
            }
            p.pane.write(buf)?;
        }
        buf.clear();
        Ok(())
    }

    fn on_mouse(&mut self, ev: MouseEv) -> io::Result<()> {
        let (tabbar, _, _) = self.regions();
        if let Some(bar) = tabbar {
            if ev.y == bar.y {
                if ev.is_left_press() {
                    if let Some(i) = ui::tab_at(self, ev.x) {
                        self.switch_tab(i);
                    }
                }
                return Ok(());
            }
        }
        let Some((id, _, inner)) = self
            .visible()
            .into_iter()
            .find(|(_, outer, _)| outer.contains((ev.x, ev.y).into()))
        else {
            return Ok(());
        };
        if ev.is_left_press() && id != self.focused() {
            self.tabs[self.active].focus = id;
            self.dirty = true;
        }
        let Some(p) = self.panes.get_mut(&id) else {
            return Ok(());
        };
        let inside = inner.contains((ev.x, ev.y).into());
        if inside && p.pane.wants_mouse() && p.pane.scroll() == 0 {
            if let Some(bytes) = ev.encode(
                ev.x - inner.x,
                ev.y - inner.y,
                p.pane.mouse_mode(),
                p.pane.mouse_encoding(),
            ) {
                p.pane.write(&bytes)?;
            }
            return Ok(());
        }
        if let Some(dir) = ev.wheel() {
            if p.pane.in_alternate_screen() {
                // Like real terminals: wheel in a full-screen app = arrow keys.
                let key: &[u8] = match (dir > 0, p.pane.application_cursor()) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1b[A",
                    (false, true) => b"\x1bOB",
                    (false, false) => b"\x1b[B",
                };
                p.pane.write(&key.repeat(3))?;
            } else {
                p.pane.scroll_by(dir * 3);
                self.dirty = true;
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- commands

    fn set_view(&mut self, v: View) {
        if self.view != v {
            self.view = v;
            self.relayout();
        }
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            for id in self.tabs[i].root.panes() {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.unseen = false;
                    p.bell = false;
                }
            }
            self.relayout();
        }
    }

    fn request_quit(&mut self, origin: Option<PaneId>) {
        let busy = self.busy(&self.all_panes(), origin);
        if busy.is_empty() {
            self.quit = Some(0);
        } else {
            let msg = format!("Quit {}? {}. (y / n)", crate::NAME, Self::describe(&busy));
            self.modal = Modal::Confirm(Pending::Quit, msg);
            self.dirty = true;
        }
    }

    fn confirmed(&mut self, p: Pending) {
        match p {
            Pending::Quit => self.quit = Some(0),
            Pending::CloseTab(i) => self.close_tab(i),
            Pending::ClosePane(id) => self.close_pane(id),
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        if let Some(mut p) = self.panes.remove(&id) {
            p.pane.kill();
        }
        self.detach(id, 0);
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        for id in self.tabs[i].root.panes() {
            self.close_pane(id);
            if self.quit.is_some() {
                return;
            }
        }
    }

    fn run_cmd(&mut self, cmd: Cmd) -> io::Result<()> {
        let tab = self.active;
        self.dirty = true;
        match cmd {
            Cmd::Quit => self.request_quit(None),
            Cmd::Leader => self.modal = Modal::Leader,
            Cmd::Help => self.modal = Modal::Help,
            Cmd::ToggleView => {
                let v = if self.view == View::Full {
                    View::Basic
                } else {
                    View::Full
                };
                self.set_view(v);
            }
            Cmd::NewTab => {
                let id = self.new_pane(self.focused_cwd())?;
                self.tabs.insert(tab + 1, Tab::new(id));
                self.switch_tab(tab + 1);
                if self.view == View::Basic {
                    self.notice(format!(
                        "tab {} of {} · alt+` shows tabs",
                        tab + 2,
                        self.tabs.len()
                    ));
                }
            }
            Cmd::CloseTab => {
                let busy = self.busy(&self.tabs[tab].root.panes(), None);
                if busy.is_empty() {
                    self.close_tab(tab);
                } else {
                    let msg = format!("Close tab {}? {}. (y / n)", tab + 1, Self::describe(&busy));
                    self.modal = Modal::Confirm(Pending::CloseTab(tab), msg);
                }
            }
            Cmd::ClosePane => {
                let id = self.focused();
                let busy = self.busy(&[id], None);
                if busy.is_empty() {
                    self.close_pane(id);
                } else {
                    let msg = format!("Close this pane? {}. (y / n)", Self::describe(&busy));
                    self.modal = Modal::Confirm(Pending::ClosePane(id), msg);
                }
            }
            Cmd::NextTab => self.switch_tab((tab + 1) % self.tabs.len()),
            Cmd::PrevTab => self.switch_tab((tab + self.tabs.len() - 1) % self.tabs.len()),
            Cmd::GoTab(i) => {
                if i < self.tabs.len() {
                    self.switch_tab(i);
                }
            }
            Cmd::RenameTab => {
                let current = self.tabs[tab].name.clone().unwrap_or_default();
                self.modal = Modal::Rename(current);
            }
            Cmd::SplitRight | Cmd::SplitDown => {
                let axis = if cmd == Cmd::SplitRight {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let focus = self.focused();
                let id = self.new_pane(self.focused_cwd())?;
                let t = &mut self.tabs[tab];
                t.root.split(focus, axis, id);
                t.focus = id;
                t.zoomed = false;
                if self.view == View::Basic {
                    self.view = View::Full;
                }
                self.relayout();
            }
            Cmd::Focus(dir) => {
                let rects: Vec<_> = self
                    .visible()
                    .into_iter()
                    .map(|(id, r, _)| (id, r))
                    .collect();
                if let Some(n) = neighbor(&rects, self.focused(), dir) {
                    self.tabs[tab].focus = n;
                }
            }
            Cmd::NextPane => self.tabs[tab].next_pane(),
            Cmd::Resize(dir) => {
                let focus = self.focused();
                if self.tabs[tab].root.resize(focus, dir, 0.05) {
                    self.relayout();
                }
            }
            Cmd::Zoom => {
                let t = &mut self.tabs[tab];
                if t.root.panes().len() > 1 {
                    t.zoomed = !t.zoomed;
                    self.relayout();
                }
            }
            Cmd::ScrollUp | Cmd::ScrollDown => {
                let id = self.focused();
                if let Some(p) = self.panes.get_mut(&id) {
                    let page = (p.size.0 as isize / 2).max(1);
                    p.pane
                        .scroll_by(if cmd == Cmd::ScrollUp { page } else { -page });
                }
            }
        }
        Ok(())
    }
}

/// Drop OSC 0/1/2 (window title) sequences — only the focused pane may set the title.
fn strip_titles(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"\x1b]1;")
            || rest.starts_with(b"\x1b]2;")
            || rest.starts_with(b"\x1b]0;")
        {
            match rest.iter().position(|&b| b == 0x07) {
                Some(end) => {
                    i += end + 1;
                    continue;
                }
                None => break,
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_stripped_but_bell_kept() {
        assert_eq!(strip_titles(b"\x1b]2;vim\x07\x07"), b"\x07");
        assert_eq!(strip_titles(b"\x1b]52;c;aGk=\x07"), b"\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn shrink_borders() {
        assert_eq!(shrink(Rect::new(0, 1, 80, 22)), Rect::new(1, 2, 78, 20));
        assert_eq!(shrink(Rect::new(0, 0, 2, 2)), Rect::new(0, 0, 2, 2));
    }
}

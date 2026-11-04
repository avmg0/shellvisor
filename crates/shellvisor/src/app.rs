//! The workspace: tabs of split panes — real shells and tool panes (files,
//! system) — plus the event loop.
//!
//! Threads (tiny, blocking — no async runtime):
//!   input            stdin bytes                ─┐
//!   pty-<id>         one per shell, output        │
//!   wait-<id>        one per shell, exit status   ├─▶ channel ─▶ app thread
//!   job              short-lived: git, find, diff │
//!   power            battery check once a minute  │
//!   signal           resize / terminate          ─┘
//!
//! The app thread owns all state and redraws only when something changed.

use crate::activity::{self, Activity};
use crate::config::{LowPower, Restore};
use crate::data::value::Data;
use crate::files::{self, Act, Done, FilesPane, Job, Outcome};
use crate::history::{Entry, History};
use crate::keys::{self, Cmd, Keymap, Preset};
use crate::mouse::MouseEv;
use crate::session::{Leaf, SavedNode, SavedTab, Session};
use crate::shell::Launch;
use crate::sysmon::SysPane;
use crate::tableview::{TableAct, TableView};
use crate::term::{InputModes, Pane};
use crate::ui;
use crate::workspace::{Axis, Node, PaneId, Tab, neighbor};
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
const SPIN_LOW_POWER: Duration = Duration::from_millis(250);
/// How often a running download's progress is re-read from its screen.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
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
    pub keymap: Keymap,
    pub restore: Restore,
    pub low_power: LowPower,
    pub session_path: PathBuf,
    /// Problems found in config.yaml, shown once at start.
    pub warnings: Vec<String>,
}

/// The tab menu (alt+m).
pub const MENU: &[(&str, Cmd)] = &[
    ("Files pane", Cmd::ToggleFiles),
    ("System pane", Cmd::ToggleSystem),
    ("Split side by side", Cmd::SplitRight),
    ("Split stacked", Cmd::SplitDown),
    ("Layout: dev", Cmd::Preset(Preset::Dev)),
    ("Layout: monitor", Cmd::Preset(Preset::Monitor)),
    ("Layout: files", Cmd::Preset(Preset::Files)),
    ("Rearrange splits", Cmd::NextLayout),
    ("Zoom split", Cmd::Zoom),
    ("Close split", Cmd::ClosePane),
    ("Rename tab", Cmd::RenameTab),
    ("New tab", Cmd::NewTab),
    ("Close tab", Cmd::CloseTab),
    ("Low-power mode", Cmd::LowPower),
    ("Basic view", Cmd::ToggleView),
    ("All keys", Cmd::Help),
];

/// A pane that isn't a shell.
pub enum Tool {
    Files(FilesPane),
    System(SysPane),
}

impl Tool {
    pub fn kind(&self) -> &'static str {
        match self {
            Tool::Files(_) => "files",
            Tool::System(_) => "system",
        }
    }

    pub fn title(&self) -> String {
        match self {
            Tool::Files(f) => f.title(),
            Tool::System(s) => s.title(),
        }
    }
}

const SAVE_EVERY: Duration = Duration::from_secs(1);

enum Ev {
    Input(Vec<u8>),
    Output(PaneId, Vec<u8>),
    PaneExit(PaneId, i32),
    Job(PaneId, Box<Done>),
    Battery(Option<bool>),
    /// A command-bar pipeline finished: (title, rows or error).
    TableReady(String, Box<Result<Data, String>>),
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
    /// A download/install spotted from the command line.
    pub activity: Option<Activity>,
    /// Rows of the last table `sv` printed here (for F4).
    pub table: Option<PathBuf>,
    progress_at: Instant,
    /// New output since progress was last read.
    progress_due: bool,
    /// When this shell last had focus (bigger = more recent).
    focused_at: u64,
    size: (u16, u16),
}

/// Something waiting for a y/n answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Quit,
    CloseTab(usize),
    ClosePane(PaneId),
    Restore(Box<Session>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    /// ctrl+] pressed, waiting for the next key.
    Leader,
    Help,
    Rename(String),
    Confirm(Pending, String),
    /// Tab menu, with the highlighted row.
    Menu(usize),
    /// The command bar (alt+;), with what's typed so far.
    Bar(String),
}

pub struct App {
    pub panes: HashMap<PaneId, PaneState>,
    pub tools: HashMap<PaneId, Tool>,
    /// The table viewer, when open (over the panes).
    pub table: Option<TableView>,
    /// A command-bar pipeline is running.
    pub bar_running: Option<String>,
    bar_history: Vec<String>,
    bar_pos: Option<usize>,
    pub low_power: bool,
    power_manual: bool,
    focus_tick: u64,
    last_focus: PaneId,
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
    session_dirty: bool,
    /// Don't overwrite the saved session while we're still asking about it.
    session_locked: bool,
    last_save: Instant,
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

    let low_power = opts.low_power == LowPower::On;
    let auto_power = opts.low_power == LowPower::Auto;
    let mut app = App {
        panes: HashMap::new(),
        tools: HashMap::new(),
        table: None,
        bar_running: None,
        bar_history: Vec::new(),
        bar_pos: None,
        low_power,
        power_manual: !auto_power,
        focus_tick: 0,
        last_focus: 0,
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
        session_dirty: false,
        session_locked: false,
        last_save: Instant::now(),
    };
    let first = app.new_pane(None)?;
    app.tabs.push(Tab::new(first));
    app.offer_restore()?;
    if let Some(w) = app.opts.warnings.first().cloned() {
        let more = app.opts.warnings.len() - 1;
        app.notice(if more > 0 {
            format!("{w} (+{more} more)")
        } else {
            w
        });
    }

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
    if auto_power {
        let tx = tx.clone();
        spawn_thread("power".into(), move || {
            let mut last = None;
            loop {
                let now = crate::osinfo::on_battery();
                if now != last {
                    last = now;
                    if tx.send(Ev::Battery(now)).is_err() {
                        break;
                    }
                }
                thread::sleep(Duration::from_secs(60));
            }
        })?;
    }
    drop(tx);

    let result = app.event_loop(rx);
    app.save_session();
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
    pub fn keymap(&self) -> &Keymap {
        &self.opts.keymap
    }

    // ---------------------------------------------------------------- session

    fn offer_restore(&mut self) -> io::Result<()> {
        if self.opts.restore == Restore::Never {
            return Ok(());
        }
        let here = self
            .panes
            .values()
            .next()
            .map(|p| p.cwd.clone())
            .unwrap_or_default();
        let Some(saved) = Session::load(&self.opts.session_path).filter(|s| !s.is_trivial(&here))
        else {
            return Ok(());
        };
        match self.opts.restore {
            Restore::Always => self.restore(saved)?,
            _ => {
                let msg = format!(
                    "Restore last session — {}? (y / n)",
                    saved.summary(ui::short_path)
                );
                self.session_locked = true;
                self.modal = Modal::Confirm(Pending::Restore(Box::new(saved)), msg);
            }
        }
        Ok(())
    }

    /// Replace the current tabs with the saved ones (new shells in the saved folders).
    fn restore(&mut self, saved: Session) -> io::Result<()> {
        let old: Vec<PaneId> = self.tabs.iter().flat_map(|t| t.root.panes()).collect();
        let mut tabs = Vec::new();
        for t in &saved.tabs {
            let root = t.layout.rebuild(&mut |l: &Leaf| self.new_leaf(l))?;
            let panes = root.panes();
            tabs.push(Tab {
                focus: panes[t.focus.min(panes.len() - 1)],
                zoomed: t.zoomed && panes.len() > 1,
                name: t.name.clone(),
                arrangement: None,
                root,
            });
        }
        for id in old {
            if let Some(mut p) = self.panes.remove(&id) {
                p.pane.kill();
            }
            self.tools.remove(&id);
        }
        let n = tabs.len();
        self.tabs = tabs;
        self.active = saved.active.min(n - 1);
        self.session_locked = false;
        self.relayout();
        self.notice(format!("restored {n} tab{}", if n == 1 { "" } else { "s" }));
        Ok(())
    }

    fn capture_session(&self) -> Session {
        let cwd = |id: PaneId| match self.tools.get(&id) {
            Some(t) => Leaf {
                cwd: match t {
                    Tool::Files(f) => f.root.display().to_string(),
                    Tool::System(_) => String::new(),
                },
                tool: Some(t.kind().to_string()),
            },
            None => Leaf {
                cwd: self
                    .panes
                    .get(&id)
                    .map(|p| p.cwd.clone())
                    .unwrap_or_default(),
                tool: None,
            },
        };
        Session {
            version: crate::session::VERSION,
            view: if self.view == View::Full {
                "full"
            } else {
                "basic"
            }
            .into(),
            active: self.active,
            tabs: self
                .tabs
                .iter()
                .map(|t| SavedTab {
                    name: t.name.clone(),
                    zoomed: t.zoomed,
                    focus: t
                        .root
                        .panes()
                        .iter()
                        .position(|p| *p == t.focus)
                        .unwrap_or(0),
                    layout: SavedNode::capture(&t.root, &cwd),
                })
                .collect(),
        }
    }

    /// Write the session now (or delete it if no tabs are left).
    fn save_session(&mut self) {
        self.session_dirty = false;
        self.last_save = Instant::now();
        if self.session_locked {
            return;
        }
        if self.tabs.is_empty() {
            Session::remove(&self.opts.session_path);
        } else {
            let _ = self.capture_session().save(&self.opts.session_path);
        }
    }

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
                activity: None,
                table: None,
                progress_at: Instant::now(),
                progress_due: false,
                focused_at: 0,
                size: (rows, cols),
            },
        );
        Ok(id)
    }

    /// A saved leaf: a shell in its folder, or a tool pane.
    fn new_leaf(&mut self, l: &Leaf) -> io::Result<PaneId> {
        match l.tool.as_deref() {
            Some(kind @ ("files" | "system")) => {
                let cwd = if l.cwd.is_empty() {
                    std::env::current_dir().unwrap_or_default()
                } else {
                    PathBuf::from(&l.cwd)
                };
                Ok(self.new_tool(kind, cwd))
            }
            _ => self.new_pane(Some(PathBuf::from(&l.cwd))),
        }
    }

    fn new_tool(&mut self, kind: &str, cwd: PathBuf) -> PaneId {
        let id = self.next_id;
        self.next_id += 1;
        let tool = if kind == "system" {
            Tool::System(SysPane::new())
        } else {
            let (f, jobs) = FilesPane::new(cwd);
            self.run_jobs(id, jobs);
            Tool::Files(f)
        };
        self.tools.insert(id, tool);
        id
    }

    fn run_jobs(&self, id: PaneId, jobs: Vec<Job>) {
        for job in jobs {
            let tx = self.tx.clone();
            let _ = spawn_thread("job".into(), move || {
                let _ = tx.send(Ev::Job(id, Box::new(files::run_job(job))));
            });
        }
    }

    pub fn focused(&self) -> PaneId {
        self.tabs[self.active].focus
    }

    /// The pane keys go to: the focused one, or in basic view the shell shown.
    pub fn input_target(&self) -> PaneId {
        match self.view {
            View::Basic => self.basic_pane(),
            View::Full => self.focused(),
        }
    }

    /// Basic view shows a shell: the focused one, else the tab's last-used shell.
    fn basic_pane(&self) -> PaneId {
        let f = self.focused();
        if self.panes.contains_key(&f) {
            f
        } else {
            self.paired_shell(self.active).unwrap_or(f)
        }
    }

    /// The shell a tab's tool panes work with: the most recently focused one.
    pub fn paired_shell(&self, tab: usize) -> Option<PaneId> {
        self.tabs
            .get(tab)?
            .root
            .panes()
            .into_iter()
            .filter_map(|id| self.panes.get(&id).map(|p| (id, p.focused_at)))
            .max_by_key(|(id, at)| (*at, std::cmp::Reverse(*id)))
            .map(|(id, _)| id)
    }

    fn focused_cwd(&self) -> Option<PathBuf> {
        let id = match self.panes.contains_key(&self.focused()) {
            true => Some(self.focused()),
            false => self.paired_shell(self.active),
        };
        id.and_then(|id| self.panes.get(&id))
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
            View::Basic => vec![(self.basic_pane(), content, content)],
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
        self.session_dirty = true;
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
                    Ev::Job(id, done) => {
                        if let Some(Tool::Files(f)) = self.tools.get_mut(&id) {
                            let out = f.done(*done);
                            self.apply(id, out);
                        }
                    }
                    Ev::Battery(b) => self.on_battery(b),
                    Ev::TableReady(title, res) => {
                        self.bar_running = None;
                        match *res {
                            Ok(Data::Value(v))
                                if !matches!(
                                    v,
                                    crate::data::value::Value::List(_)
                                        | crate::data::value::Value::Record(_)
                                ) =>
                            {
                                self.notice(format!("{title} = {}", v.display()))
                            }
                            Ok(d) => self.table = Some(TableView::new(title, d)),
                            Err(e) => self.notice(e),
                        }
                        self.dirty = true;
                    }
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
            self.sync_tools();

            if let Some((_, at)) = &self.notice {
                if at.elapsed() >= NOTICE {
                    self.notice = None;
                    self.dirty = true;
                }
            }
            if self.spinning() && last_draw.elapsed() >= self.spin_every() {
                self.dirty = true;
            }
            if self.session_dirty && self.last_save.elapsed() >= SAVE_EVERY {
                self.save_session();
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

    fn spin_every(&self) -> Duration {
        if self.low_power { SPIN_LOW_POWER } else { SPIN }
    }

    /// Re-read a download's progress from its screen (at most every 250 ms).
    fn update_progress(&mut self, id: PaneId) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if !p.progress_due || p.activity.is_none() || p.progress_at.elapsed() < PROGRESS_EVERY {
            return;
        }
        p.progress_due = false;
        p.progress_at = Instant::now();
        let cols = p.size.1;
        let rows: Vec<String> = p.pane.screen().rows(0, cols).collect();
        let progress = activity::progress(rows.iter().map(String::as_str));
        if let Some(a) = p.activity.as_mut() {
            if progress.is_some() && progress != a.progress {
                a.progress = progress;
                self.dirty = true;
            }
        }
    }

    /// Tool panes on screen right now.
    fn visible_tools(&self) -> Vec<PaneId> {
        self.visible()
            .into_iter()
            .map(|(id, _, _)| id)
            .filter(|id| self.tools.contains_key(id))
            .collect()
    }

    /// Per-batch upkeep: remember which shell had focus, let file panes
    /// follow their shell, and sample the system pane when due.
    fn sync_tools(&mut self) {
        let due: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.progress_due)
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            self.update_progress(id);
        }
        let target = self.input_target();
        if target != self.last_focus {
            self.last_focus = target;
            if let Some(p) = self.panes.get_mut(&target) {
                self.focus_tick += 1;
                p.focused_at = self.focus_tick;
            }
        }
        let shell_cwd = self
            .paired_shell(self.active)
            .and_then(|id| self.panes.get(&id))
            .map(|p| PathBuf::from(&p.cwd));
        let low = self.low_power;
        let shown = self.visible_tools();
        for (id, t) in self.tools.iter_mut() {
            if let Tool::System(sp) = t {
                if !shown.contains(id) && sp.idle_for() > Duration::from_secs(30) {
                    sp.sleep(); // free the process list while hidden
                }
            }
        }
        for id in shown {
            let mut out = Outcome::default();
            match self.tools.get_mut(&id) {
                Some(Tool::Files(f)) => {
                    if let Some(cwd) = &shell_cwd {
                        if f.follow && *cwd != f.seen_cwd && cwd.is_dir() {
                            f.seen_cwd = cwd.clone();
                            out.jobs = f.set_root(cwd.clone());
                            out.redraw = true;
                        }
                    }
                    if !low && f.poll_due_in().is_zero() {
                        let o = f.poll();
                        out.jobs.extend(o.jobs);
                        out.redraw |= o.redraw;
                    }
                }
                Some(Tool::System(sp)) if sp.due_in(low).is_zero() => {
                    sp.sample();
                    out.redraw = true;
                }
                Some(Tool::System(_)) => {}
                None => {}
            }
            self.apply(id, out);
        }
    }

    /// Act on what a tool pane asked for.
    fn apply(&mut self, id: PaneId, out: Outcome) {
        if out.redraw {
            self.dirty = true;
        }
        self.run_jobs(id, out.jobs);
        for act in out.acts {
            self.tool_act(id, act);
        }
    }

    fn tool_act(&mut self, id: PaneId, act: Act) {
        if let Act::Notice(m) = act {
            self.notice(m);
            return;
        }
        let Some(tab) = self.tab_of(id) else { return };
        let Some(shell) = self.paired_shell(tab) else {
            self.notice("no shell in this tab — split one with alt+\\");
            return;
        };
        let (busy, cwd) = match self.panes.get(&shell) {
            Some(p) => (p.running, PathBuf::from(&p.cwd)),
            None => return,
        };
        let text = match act {
            Act::Insert(path) => format!("{} ", shell_quote(&relative(&path, &cwd))),
            Act::Cd(dir) => {
                if busy {
                    self.notice("the shell is busy — wait for the command to finish");
                    return;
                }
                if let Some(Tool::Files(f)) = self.tools.get_mut(&id) {
                    f.follow = true;
                }
                format!("\x05\x15cd {}\r", shell_quote(&relative(&dir, &cwd)))
            }
            Act::Edit(file, line) => {
                if busy {
                    self.notice("the shell is busy — wait for the command to finish");
                    return;
                }
                format!(
                    "\x05\x15{}\r",
                    edit_command(&editor(), &relative(&file, &cwd), line)
                )
            }
            Act::Notice(_) => return,
        };
        if let Some(p) = self.panes.get_mut(&shell) {
            let _ = p.pane.write(text.as_bytes());
        }
        self.tabs[tab].focus = shell;
        self.dirty = true;
    }

    fn on_battery(&mut self, b: Option<bool>) {
        if self.power_manual {
            return;
        }
        let low = b == Some(true);
        if low != self.low_power {
            self.low_power = low;
            self.notice(if low {
                "on battery — low-power mode on (slower refresh)"
            } else {
                "on power — low-power mode off"
            });
        }
    }

    fn next_wakeup(&self, last_draw: Instant) -> Duration {
        if self.dirty {
            return FRAME.saturating_sub(last_draw.elapsed());
        }
        let mut t = Duration::from_secs(60); // clock in the status bar
        if self.spinning() {
            t = t.min(
                self.spin_every()
                    .saturating_sub(last_draw.elapsed())
                    .max(Duration::from_millis(5)),
            );
        }
        for id in self.visible_tools() {
            let due = match &self.tools[&id] {
                Tool::System(sp) => sp.due_in(self.low_power),
                Tool::Files(_) if self.low_power => continue,
                Tool::Files(f) => f.poll_due_in(),
            };
            t = t.min(due.max(Duration::from_millis(5)));
        }
        if let Some((_, at)) = &self.notice {
            t = t.min((*at + NOTICE).saturating_duration_since(Instant::now()));
        }
        if self.session_dirty {
            t = t.min((self.last_save + SAVE_EVERY).saturating_duration_since(Instant::now()));
        }
        t
    }

    /// Keep the real terminal's input modes in line with the focused program.
    /// In full view we always take the mouse (clicks focus panes).
    fn sync_modes<W: Write>(&mut self, term: &mut Terminal<CrosstermBackend<W>>) -> io::Result<()> {
        let focused = self
            .panes
            .get(&self.input_target())
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
        let focused = self.input_target();
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
        // After the events: the command may have started in this same chunk.
        if let Some(p) = self.panes.get_mut(&id) {
            if p.activity.is_some() {
                p.progress_due = true;
            }
        }
        self.update_progress(id);
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
                self.session_dirty = true;
            }
            ShellEvent::CommandText(t) => {
                p.command = t;
                if p.running {
                    p.activity = activity::classify(&p.command);
                }
            }
            ShellEvent::CommandStarted => {
                p.started = Some(Instant::now());
                p.running = true;
                p.activity = activity::classify(&p.command);
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
                let took = p.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                let done = p.activity.take();
                p.running = false;
                p.started = None;
                p.last_exit = code;
                p.command.clear();
                self.dirty = true;
                if let Some(a) = done {
                    self.notice(match code {
                        Some(0) | None => {
                            format!("✔ {} finished · {}", a.label, ui::fmt_secs(took))
                        }
                        Some(c) => {
                            format!("✖ {} failed (exit {c}) · {}", a.label, ui::fmt_secs(took))
                        }
                    });
                }
                // Commands change files: refresh this tab's file panes.
                if let Some(t) = self.tab_of(id) {
                    for pid in self.tabs[t].root.panes() {
                        if let Some(Tool::Files(f)) = self.tools.get_mut(&pid) {
                            let jobs = f.refresh();
                            self.run_jobs(pid, jobs);
                        }
                    }
                }
            }
            ShellEvent::Request(words) => self.on_request(&words),
            ShellEvent::Table(path) => {
                p.table = Some(PathBuf::from(path));
                if !self.option_hint_shown || self.notice.is_none() {
                    self.notice("▦ F4 explores this table — sort, filter, copy");
                }
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
            let remaining = self.tabs[t]
                .root
                .clone()
                .remove(id)
                .filter(|r| r.panes().iter().any(|p| self.panes.contains_key(p)));
            if remaining.is_none() {
                // no shell left: tool panes go with the tab
                for p in self.tabs[t].root.panes() {
                    self.tools.remove(&p);
                }
            }
            let tab = &mut self.tabs[t];
            match remaining {
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
            if self.table.is_some() && self.modal == Modal::None {
                self.table_key(key)?;
                if self.quit.is_some() {
                    return Ok(());
                }
                continue;
            }
            // Modal states swallow keys first.
            match self.modal.clone() {
                Modal::Bar(mut line) => {
                    self.dirty = true;
                    match key {
                        b"\r" => {
                            self.modal = Modal::None;
                            self.run_bar(line);
                        }
                        b"\x1b\r" => {
                            // alt+enter: put it in the shell instead
                            self.modal = Modal::None;
                            let cmd = format!("sv {}", app_quote_pipeline(&line));
                            self.type_in_shell(&cmd);
                        }
                        b"\x1b" | [0x03] => self.modal = Modal::None,
                        [0x7f] | [0x08] => {
                            line.pop();
                            self.modal = Modal::Bar(line);
                        }
                        [0x15] => self.modal = Modal::Bar(String::new()),
                        b"\x1b[A" | b"\x1bOA" | b"\x1b[B" | b"\x1bOB" => {
                            let n = self.bar_history.len();
                            if n > 0 {
                                let up = key[2] == b'A';
                                let pos = match (self.bar_pos, up) {
                                    (None, true) => Some(n - 1),
                                    (None, false) => None,
                                    (Some(p), true) => Some(p.saturating_sub(1)),
                                    (Some(p), false) if p + 1 < n => Some(p + 1),
                                    (Some(_), false) => None,
                                };
                                self.bar_pos = pos;
                                let text =
                                    pos.map(|p| self.bar_history[p].clone()).unwrap_or_default();
                                self.modal = Modal::Bar(text);
                            }
                        }
                        k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                            line.push_str(&String::from_utf8_lossy(k));
                            self.modal = Modal::Bar(line);
                        }
                        _ => {}
                    }
                    continue;
                }
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
                            self.session_dirty = true;
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
                            self.confirmed(pending)?;
                        }
                        b"n" | b"N" | b"\x1b" | [0x03] => {
                            self.modal = Modal::None;
                            if self.session_locked {
                                // "No" to restoring: from now on save the new session.
                                self.session_locked = false;
                                self.session_dirty = true;
                            }
                        }
                        _ => {}
                    }
                    self.dirty = true;
                    if self.quit.is_some() {
                        return Ok(());
                    }
                    continue;
                }
                Modal::Menu(i) => {
                    let n = MENU.len();
                    self.dirty = true;
                    match key {
                        b"\x1b[A" | b"\x1bOA" | b"k" => self.modal = Modal::Menu((i + n - 1) % n),
                        b"\x1b[B" | b"\x1bOB" | b"j" => self.modal = Modal::Menu((i + 1) % n),
                        b"\r" | b" " => {
                            self.modal = Modal::None;
                            self.flush_forward(&mut forward)?;
                            self.run_cmd(MENU[i].1)?;
                            if self.quit.is_some() {
                                return Ok(());
                            }
                        }
                        [d @ b'1'..=b'9'] if ((d - b'1') as usize) < n => {
                            self.modal = Modal::None;
                            self.run_cmd(MENU[(d - b'1') as usize].1)?;
                        }
                        b"0" if n >= 10 => {
                            self.modal = Modal::None;
                            self.run_cmd(MENU[9].1)?;
                        }
                        _ => self.modal = Modal::None, // esc, q, alt+m, anything else closes
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
            if let Some(cmd) = self.opts.keymap.lookup(key) {
                self.flush_forward(&mut forward)?;
                self.run_cmd(cmd)?;
                if self.quit.is_some() {
                    return Ok(());
                }
                continue;
            }
            let target = self.input_target();
            if self.tools.contains_key(&target) {
                self.flush_forward(&mut forward)?;
                self.tool_key(target, key);
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

    fn table_key(&mut self, key: &[u8]) -> io::Result<()> {
        if let Some(ev) = MouseEv::parse_sgr(key) {
            if let (Some(t), Some(dir)) = (self.table.as_mut(), ev.wheel()) {
                t.wheel(dir);
                self.dirty = true;
            }
            return Ok(());
        }
        // A few global keys still work over the table.
        match self.opts.keymap.lookup(key) {
            Some(c @ (Cmd::Quit | Cmd::Help | Cmd::CommandBar)) => return self.run_cmd(c),
            Some(Cmd::ExploreTable) => {
                self.table = None;
                self.dirty = true;
                return Ok(());
            }
            _ => {}
        }
        let Some(t) = self.table.as_mut() else {
            return Ok(());
        };
        let (redraw, act) = t.key(key);
        if redraw {
            self.dirty = true;
        }
        match act {
            None => {}
            Some(TableAct::Close) => self.table = None,
            Some(TableAct::Copy(text, fmt)) => {
                let rows = text.lines().count();
                self.copy(&text);
                self.notice(format!("copied {rows} lines as {fmt}"));
            }
            Some(TableAct::Insert(text)) => {
                self.table = None;
                let q = format!("{} ", shell_quote(&text));
                self.type_in_shell_raw(&q);
            }
        }
        Ok(())
    }

    /// System clipboard, else OSC 52 through the real terminal.
    fn copy(&mut self, text: &str) {
        if !crate::osinfo::copy_to_clipboard(text) {
            let seq = format!("\x1b]52;c;{}\x07", sv_proto::b64(text.as_bytes()));
            let mut out = io::stdout();
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
        }
    }

    /// Type text at the prompt of the active tab's shell (no Enter), and focus it.
    fn type_in_shell_raw(&mut self, text: &str) {
        let Some(shell) = self.paired_shell(self.active) else {
            self.notice("no shell in this tab");
            return;
        };
        if let Some(p) = self.panes.get_mut(&shell) {
            let _ = p.pane.write(text.as_bytes());
        }
        self.tabs[self.active].focus = shell;
        self.dirty = true;
    }

    /// Replace the prompt line with `cmd` (no Enter), so the user can run or edit it.
    fn type_in_shell(&mut self, cmd: &str) {
        self.type_in_shell_raw(&format!("\x05\x15{cmd}"));
    }

    /// F4: open the last table `sv` printed in this tab's shell.
    fn explore_table(&mut self) {
        let path = self
            .paired_shell(self.active)
            .and_then(|id| self.panes.get(&id))
            .and_then(|p| p.table.clone())
            .or_else(|| self.panes.values().filter_map(|p| p.table.clone()).max());
        let Some(path) = path else {
            self.notice(
                "no table yet — run an sv command (e.g. sv ls), or alt+; for the command bar",
            );
            return;
        };
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| crate::data::format::parse_json(&t))
        {
            Ok(d) => self.table = Some(TableView::new("last sv table", d)),
            Err(e) => self.notice(format!("table: {e}")),
        }
        self.dirty = true;
    }

    /// Run a command-bar pipeline as `shellvisor sv '…'` in the shell's folder.
    fn run_bar(&mut self, line: String) {
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        if self.bar_history.last() != Some(&line) {
            self.bar_history.push(line.clone());
            if self.bar_history.len() > 100 {
                self.bar_history.remove(0);
            }
        }
        self.bar_pos = None;
        let cwd = self
            .focused_cwd()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let exe = std::env::current_exe().unwrap_or_else(|_| "shellvisor".into());
        let tx = self.tx.clone();
        self.bar_running = Some(line.clone());
        let _ = spawn_thread("bar".into(), move || {
            let out = std::process::Command::new(exe)
                .arg("sv")
                .arg(&line)
                .current_dir(&cwd)
                .stdin(std::process::Stdio::null())
                .output();
            let res = match out {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    if text.trim().is_empty() {
                        Ok(Data::Table(Vec::new()))
                    } else {
                        crate::data::format::parse_auto(&text)
                    }
                }
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                Err(e) => Err(format!("sv: {e}")),
            };
            let _ = tx.send(Ev::TableReady(line, Box::new(res)));
        });
        self.dirty = true;
    }

    fn tool_key(&mut self, id: PaneId, key: &[u8]) {
        let out = match self.tools.get_mut(&id) {
            Some(Tool::Files(f)) => f.key(key),
            Some(Tool::System(sp)) => Outcome {
                redraw: sp.key(key),
                ..Default::default()
            },
            None => return,
        };
        self.apply(id, out);
    }

    fn flush_forward(&mut self, buf: &mut Vec<u8>) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let id = self.input_target();
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
        if let Some(tool) = self.tools.get_mut(&id) {
            let inside = inner.contains((ev.x, ev.y).into());
            let out = match (tool, ev.wheel()) {
                (Tool::Files(f), Some(dir)) => f.wheel(dir),
                (Tool::System(sp), Some(dir)) => {
                    sp.wheel(dir);
                    Outcome {
                        redraw: true,
                        ..Default::default()
                    }
                }
                (Tool::Files(f), None) if inside && ev.is_left_press() => {
                    f.click(ev.x - inner.x, ev.y - inner.y, inner.width)
                }
                _ => Outcome::default(),
            };
            self.apply(id, out);
            return Ok(());
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
            if v == View::Basic {
                // basic view shows a shell; leave focus on it
                let b = self.basic_pane();
                self.tabs[self.active].focus = b;
            }
            self.view = v;
            self.relayout();
        }
    }

    /// `sv files`, `sv sys`, `sv layout dev`, `sv power auto`…
    fn on_request(&mut self, words: &[String]) {
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        let r = match w.as_slice() {
            ["files"] => self.show_tool("files", false),
            ["files", view] => {
                let _ = self.show_tool("files", false);
                let id = self.focused();
                match (self.tools.get_mut(&id), files::View::parse(view)) {
                    (Some(Tool::Files(f)), Some(v)) => {
                        let jobs = f.set_view(v);
                        self.run_jobs(id, jobs);
                    }
                    _ => self.notice(format!(
                        "sv files: unknown view `{view}` (tree, list, git, find)"
                    )),
                }
                Ok(())
            }
            ["system"] => self.show_tool("system", false),
            ["layout", name] => match Preset::parse(name) {
                Some(p) => self.apply_preset(p),
                None => {
                    self.notice(format!(
                        "sv layout: unknown layout `{name}` (dev, monitor, files)"
                    ));
                    Ok(())
                }
            },
            ["power", m] => {
                match *m {
                    "on" | "off" => {
                        self.power_manual = true;
                        self.set_low_power(*m == "on");
                    }
                    _ => {
                        self.power_manual = false;
                        let b = crate::osinfo::on_battery();
                        self.low_power = b == Some(true);
                        self.notice(format!(
                            "low-power: auto (now {})",
                            if self.low_power { "on" } else { "off" }
                        ));
                    }
                }
                Ok(())
            }
            _ => {
                self.notice(format!("sv: unknown request `{}`", w.join(" ")));
                Ok(())
            }
        };
        if let Err(e) = r {
            self.notice(format!("sv: {e}"));
        }
    }

    fn set_low_power(&mut self, on: bool) {
        self.low_power = on;
        self.notice(format!("low-power mode {}", if on { "on" } else { "off" }));
    }

    /// Open (or focus; with `toggle`, close if focused) a tool pane in this tab.
    fn show_tool(&mut self, kind: &str, toggle: bool) -> io::Result<()> {
        let tab = self.active;
        let existing = self.tabs[tab]
            .root
            .panes()
            .into_iter()
            .find(|id| self.tools.get(id).is_some_and(|t| t.kind() == kind));
        if self.view == View::Basic {
            self.view = View::Full;
        }
        match existing {
            Some(id) if toggle && id == self.focused() => {
                self.close_pane(id);
                if let Some(s) = self.paired_shell(self.active) {
                    self.tabs[self.active].focus = s;
                }
            }
            Some(id) => {
                let t = &mut self.tabs[tab];
                t.focus = id;
                t.zoomed = false;
            }
            None => {
                let cwd = self
                    .focused_cwd()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                let target = self.focused();
                let id = self.new_tool(kind, cwd);
                let t = &mut self.tabs[tab];
                // the tool takes the right-hand third
                t.root.split_at(target, Axis::Horizontal, id, 0.62);
                t.focus = id;
                t.zoomed = false;
            }
        }
        self.relayout();
        Ok(())
    }

    /// Lay the tab out as a preset. A tab with one pane is reshaped in
    /// place; a busier one is left alone and the preset opens in a new tab.
    fn apply_preset(&mut self, p: Preset) -> io::Result<()> {
        let cwd = self.focused_cwd();
        let tab = self.active;
        let panes = self.tabs[tab].root.panes();
        let (tab, shell) = if panes.len() == 1 && self.panes.contains_key(&panes[0]) {
            (tab, panes[0])
        } else {
            let id = self.new_pane(cwd.clone())?;
            self.tabs.insert(tab + 1, Tab::new(id));
            self.active = tab + 1;
            (tab + 1, id)
        };
        let here = cwd
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let leaf = |id| Box::new(Node::Leaf(id));
        let root = match p {
            Preset::Dev => {
                let f = self.new_tool("files", here.clone());
                let s = self.new_tool("system", here);
                Node::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.6,
                    a: leaf(shell),
                    b: Box::new(Node::Split {
                        axis: Axis::Vertical,
                        ratio: 0.55,
                        a: leaf(f),
                        b: leaf(s),
                    }),
                }
            }
            Preset::Monitor => {
                let s = self.new_tool("system", here);
                Node::Split {
                    axis: Axis::Vertical,
                    ratio: 0.62,
                    a: leaf(s),
                    b: leaf(shell),
                }
            }
            Preset::Files => {
                let f = self.new_tool("files", here);
                Node::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.38,
                    a: leaf(f),
                    b: leaf(shell),
                }
            }
        };
        let t = &mut self.tabs[tab];
        t.root = root;
        t.focus = shell;
        t.zoomed = false;
        t.arrangement = None;
        self.view = View::Full;
        self.notice(format!("layout: {}", p.name()));
        self.relayout();
        Ok(())
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

    fn confirmed(&mut self, p: Pending) -> io::Result<()> {
        match p {
            Pending::Quit => self.quit = Some(0),
            Pending::CloseTab(i) => self.close_tab(i),
            Pending::ClosePane(id) => self.close_pane(id),
            Pending::Restore(saved) => self.restore(*saved)?,
        }
        Ok(())
    }

    fn close_pane(&mut self, id: PaneId) {
        if let Some(mut p) = self.panes.remove(&id) {
            p.pane.kill();
        }
        self.tools.remove(&id);
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
            Cmd::TabMenu => self.modal = Modal::Menu(0),
            Cmd::NextLayout => match self.tabs[tab].next_arrangement() {
                Some(a) => {
                    self.notice(format!("layout: {}", a.name()));
                    self.relayout();
                }
                None => self.notice("Only one split here — split first with alt+\\ or alt+-"),
            },
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
            Cmd::ToggleFiles => self.show_tool("files", true)?,
            Cmd::ToggleSystem => self.show_tool("system", true)?,
            Cmd::Preset(p) => self.apply_preset(p)?,
            Cmd::LowPower => {
                self.power_manual = true;
                self.set_low_power(!self.low_power);
            }
            Cmd::ExploreTable => self.explore_table(),
            Cmd::CommandBar => {
                self.table = None;
                self.bar_pos = None;
                self.modal = Modal::Bar(String::new());
            }
            Cmd::ScrollUp | Cmd::ScrollDown => {
                let id = self.input_target();
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

/// `p` relative to `cwd` when it's inside it ("." for cwd itself).
fn relative(p: &std::path::Path, cwd: &std::path::Path) -> String {
    match p.strip_prefix(cwd) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.display().to_string(),
        Err(_) => p.display().to_string(),
    }
}

/// `ls | where size > 1mb` → `'ls | where size > 1mb'` for `sv` at a prompt.
fn app_quote_pipeline(line: &str) -> String {
    format!("'{}'", line.replace('\'', "'\\''"))
}

/// Quote for a POSIX shell only when needed.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-+@%,:=".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn editor() -> String {
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// The command line that opens `path` (at `line`) in `editor`.
pub fn edit_command(editor: &str, path: &str, line: Option<usize>) -> String {
    let q = shell_quote(path);
    let Some(n) = line else {
        return format!("{editor} {q}");
    };
    let prog = editor.split_whitespace().next().unwrap_or("");
    let prog = prog.rsplit('/').next().unwrap_or(prog);
    match prog {
        "vi" | "vim" | "nvim" | "nano" | "emacs" | "emacsclient" | "micro" | "kak" | "mg" => {
            format!("{editor} +{n} {q}")
        }
        "code" | "code-insiders" | "cursor" | "codium" => {
            format!("{editor} -g {}", shell_quote(&format!("{path}:{n}")))
        }
        "subl" | "zed" | "hx" | "helix" => {
            format!("{editor} {}", shell_quote(&format!("{path}:{n}")))
        }
        _ => format!("{editor} {q}"),
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
    fn quoting_and_editors() {
        assert_eq!(shell_quote("src/main.rs"), "src/main.rs");
        assert_eq!(shell_quote("My File.txt"), "'My File.txt'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(edit_command("vim", "a.rs", Some(3)), "vim +3 a.rs");
        assert_eq!(
            edit_command("code -w", "a b.rs", Some(3)),
            "code -w -g 'a b.rs:3'"
        );
        assert_eq!(edit_command("/usr/bin/nano", "x", None), "/usr/bin/nano x");
        assert_eq!(edit_command("weird", "x", Some(9)), "weird x");
        let cwd = std::path::Path::new("/p");
        assert_eq!(relative(std::path::Path::new("/p/src/a"), cwd), "src/a");
        assert_eq!(relative(cwd, cwd), ".");
        assert_eq!(relative(std::path::Path::new("/q"), cwd), "/q");
    }

    #[test]
    fn shrink_borders() {
        assert_eq!(shrink(Rect::new(0, 1, 80, 22)), Rect::new(1, 2, 78, 20));
        assert_eq!(shrink(Rect::new(0, 0, 2, 2)), Rect::new(0, 0, 2, 2));
    }
}

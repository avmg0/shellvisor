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
use crate::agents::{self, Agent, Cli, FileChange};
use crate::config::{LowPower, Restore};
use crate::data::value::Data;
use crate::files::{self, Act, Done, FilesPane, Job, Outcome};
use crate::flow::{FlowAct, FlowPane};
use crate::history::{Entry, History};
use crate::keys::{self, Cmd, Keymap, Preset};
use crate::mouse::MouseEv;
use crate::session::{Leaf, SavedNode, SavedTab, Session};
use crate::shell::Launch;
use crate::sysmon::SysPane;
use crate::tableview::{TableAct, TableView};
use crate::term::{InputModes, Pane};
use crate::ui;
use crate::workspace::{Arrangement, Axis, Node, PaneId, Tab, arrange, neighbor};
use crossterm::{cursor, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
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
    /// Light theme for Shellvisor's own UI.
    pub light: bool,
    /// New agents get their own git worktree.
    pub agent_worktree: bool,
    pub agent_limits: agents::Limits,
    pub planner: Option<String>,
    /// The agent message bus socket (`sv agent …`).
    pub bus_path: PathBuf,
    /// Approval kinds answered automatically (config agents.approvals).
    pub auto_approve: Vec<agents::Action>,
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
    ("Layout: data", Cmd::Preset(Preset::Data)),
    ("Flow pane", Cmd::ToggleFlow),
    ("New agent", Cmd::NewAgent),
    ("Make this an agent column", Cmd::AttachAgent),
    ("Rearrange splits", Cmd::NextLayout),
    ("Zoom split", Cmd::Zoom),
    ("Close split", Cmd::ClosePane),
    ("Rename tab", Cmd::RenameTab),
    ("Connect… (saved sessions)", Cmd::Sessions),
    ("Duplicate session", Cmd::Duplicate),
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
    Flow(FlowPane),
    Remote(crate::remote::RemotePane),
}

impl Tool {
    pub fn kind(&self) -> &'static str {
        match self {
            Tool::Files(f) if f.view == files::View::Git => "git",
            Tool::Files(_) => "files",
            Tool::System(s) if s.thermal => "thermal",
            Tool::System(_) => "system",
            Tool::Flow(_) => "flow",
            Tool::Remote(_) => "remote",
        }
    }

    pub fn title(&self) -> String {
        match self {
            Tool::Files(f) => f.title(),
            Tool::System(s) => s.title(),
            Tool::Flow(f) => f.title(),
            Tool::Remote(r) => r.title(),
        }
    }
}

const SAVE_EVERY: Duration = Duration::from_secs(1);

/// A running team.yaml.
struct TeamRun {
    team: crate::team::Team,
    goal: String,
    folder: PathBuf,
    /// Members started so far.
    started: Vec<String>,
    /// Over budget: these agents were paused for it.
    capped: Option<Vec<String>>,
    warned: bool,
    /// Members that have finished their task.
    done: Vec<String>,
    /// Members running at once (auto-plan dispatch); None: no limit.
    parallel: Option<usize>,
    /// Step by step: members of stages up to this one may start.
    step: Option<u32>,
}

/// Ask the planner: the prompt on its stdin, its stdout back.
fn run_planner(cmd: &str, prompt: &str, folder: &std::path::Path) -> Result<String, String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(folder)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run the planner `{cmd}`: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && !text.contains('{') {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        return Err(format!("the planner `{cmd}` failed: {line}"));
    }
    Ok(text)
}

/// One agent's files changed, tokens, and the CLI's own helper agents.
type AgentReport = (PaneId, Vec<FileChange>, Option<u64>, Vec<String>);

enum Ev {
    Input(Vec<u8>),
    Output(PaneId, Vec<u8>),
    PaneExit(PaneId, i32),
    Job(PaneId, Box<Done>),
    /// Remote files: a listing, upload or download finished.
    Remote(PaneId, Box<crate::remote::Done>),
    Battery(Option<bool>),
    /// A command-bar pipeline finished: (title, rows or error).
    TableReady(String, Box<Result<Data, String>>),
    /// Files each agent has changed (from git).
    AgentFiles(Vec<AgentReport>),
    /// The planner's answer: (run, its output or why it failed).
    Plan(u64, Result<String, String>),
    Notice(String),
    /// A request on the agent bus, and where its reply goes.
    Bus(serde_json::Value, Sender<serde_json::Value>),
    Resize,
    Quit,
}

/// Text selected with the mouse in a shell pane: cells as (row, col) in
/// the pane, `a` where the drag started, `b` where it is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub pane: PaneId,
    pub a: (u16, u16),
    pub b: (u16, u16),
    /// The pane's scrollback offset when it was made (it goes if that changes).
    pub scroll: usize,
    pub dragging: bool,
}

impl Selection {
    /// (first, last) cell, in reading order.
    pub fn range(&self) -> ((u16, u16), (u16, u16)) {
        if self.a <= self.b {
            (self.a, self.b)
        } else {
            (self.b, self.a)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (s, e) = self.range();
        (row, col) >= s && (row, col) <= e
    }
}

/// What a command palette entry does.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAct {
    Cmd(Cmd),
    Setting(usize),
    Session(String, String),
    Layout(String),
    Tab(usize),
}

/// The command palette: query, selected row, every entry (label, detail, act).
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub query: String,
    pub sel: usize,
    pub items: Vec<(String, String, PaletteAct)>,
}

impl Palette {
    /// Entries matching the query, best first.
    pub fn matches(&self) -> Vec<usize> {
        let mut scored: Vec<(i64, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, (label, detail, _))| {
                fuzzy(&self.query, label)
                    .map(|s| s + 50)
                    .or_else(|| fuzzy(&self.query, detail))
                    .map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

/// Fuzzy match: every query letter in order. Higher is better (words that
/// start with the letters, letters close together).
pub fn fuzzy(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut ti = 0usize;
    let mut last: Option<usize> = None;
    for qc in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (ti..t.len()).find(|&i| t[i] == qc)?;
        let word_start = found == 0 || !t[found - 1].is_alphanumeric();
        score += if word_start { 10 } else { 1 };
        if let Some(l) = last {
            score -= (found - l - 1).min(10) as i64;
        }
        last = Some(found);
        ti = found + 1;
    }
    if t.iter().collect::<String>().contains(&query.to_lowercase()) {
        score += 30;
    }
    Some(score)
}

/// A command that failed: its pane, text, exit code, time and last lines.
#[derive(Debug, Clone, PartialEq)]
pub struct FailCard {
    pub pane: PaneId,
    pub cmd: String,
    pub code: i32,
    pub took: u64,
    pub lines: Vec<String>,
}

/// How long a failed-command card stays up.
pub const FAIL_CARD: Duration = Duration::from_secs(15);

/// Programs whose non-zero exit isn't an "error" worth a card.
fn no_card_for(cmd: &str) -> bool {
    let first = cmd
        .split_whitespace()
        .find(|w| !w.contains('=') && *w != "sudo")
        .unwrap_or("");
    let prog = first.rsplit('/').next().unwrap_or(first);
    matches!(
        prog,
        "vi" | "vim"
            | "nvim"
            | "less"
            | "more"
            | "man"
            | "nano"
            | "top"
            | "htop"
            | "btop"
            | "ssh"
            | "mosh"
            | "python"
            | "python3"
            | "node"
            | "irb"
            | "watch"
            | "tail"
            | "fzf"
            | "grep"
            | "rg"
            | "diff"
            | "test"
            | "["
            | "false"
            | "exit"
            | "sv"
            | "shellvisor"
    )
}

/// Find in a pane's scrollback (ctrl+] /).
#[derive(Debug, Clone, PartialEq)]
pub struct Find {
    pub pane: PaneId,
    pub query: String,
    /// (line in all_lines(), first column, length in characters)
    pub matches: Vec<(usize, u16, u16)>,
    pub cur: usize,
}

/// Matches of `query` in `lines` (smart case: case-insensitive unless the
/// query has a capital letter).
pub fn find_in(lines: &[String], query: &str) -> Vec<(usize, u16, u16)> {
    if query.is_empty() {
        return Vec::new();
    }
    let exact = query.chars().any(char::is_uppercase);
    let q: Vec<char> = if exact {
        query.chars().collect()
    } else {
        query.to_lowercase().chars().collect()
    };
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let l: Vec<char> = if exact {
            line.chars().collect()
        } else {
            line.to_lowercase().chars().collect()
        };
        let mut c = 0;
        while c + q.len() <= l.len() {
            if l[c..c + q.len()] == q[..] {
                out.push((i, c as u16, q.len() as u16));
                c += q.len();
            } else {
                c += 1;
            }
        }
    }
    out
}

/// The right-click menu: where it opened and the highlighted item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextMenu {
    pub pane: PaneId,
    pub x: u16,
    pub y: u16,
    pub sel: usize,
    /// Pane-local cell that was right-clicked (for Open link).
    pub cell: (u16, u16),
}

pub const CONTEXT_ITEMS: [&str; 11] = [
    "Copy",
    "Paste",
    "Select all",
    "Open link",
    "Find…",
    "Clear",
    "Log to file",
    "Duplicate session",
    "Reconnect",
    "Split side by side",
    "Split stacked",
];

/// The URL (http, https, file, ftp) around `col` in `line`, if any.
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let ok = |c: char| !c.is_whitespace() && !"\"'<>`|".contains(c);
    if col >= chars.len() || !ok(chars[col]) {
        return None;
    }
    let mut a = col;
    while a > 0 && ok(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < chars.len() && ok(chars[b + 1]) {
        b += 1;
    }
    let word: String = chars[a..=b].iter().collect();
    let start = ["https://", "http://", "file://", "ftp://"]
        .iter()
        .filter_map(|p| word.find(p))
        .min()?;
    let url = word[start..].trim_end_matches(['.', ',', ')', ']', ':', ';']);
    (url.len() > 8).then(|| url.to_string())
}

/// A word for double-click selection: anything but spaces and brackets/quotes.
fn word_char(c: char) -> bool {
    !c.is_whitespace() && !"\"'()[]{}<>|`,;".contains(c)
}

/// The (first, last) column of the word at `col` in `line`.
pub fn word_at(line: &str, col: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || !word_char(chars[col]) {
        return None;
    }
    let mut a = col;
    while a > 0 && word_char(chars[a - 1]) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < chars.len() && word_char(chars[b + 1]) {
        b += 1;
    }
    Some((a, b))
}

/// A download/install that finished.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub label: String,
    pub cmd: String,
    pub target: Option<PathBuf>,
    pub ok: bool,
    pub code: Option<i32>,
    /// "412 MB", as far as the tool said.
    pub amount: Option<String>,
    pub src: String,
    /// Seconds it ran.
    pub took: u64,
    pub at: Instant,
    /// The shell it ran in.
    pub pane: PaneId,
}

/// How long a finished download's actions stay on offer.
pub const TOAST_ACT: Duration = Duration::from_secs(12);

/// How long a finish toast shows: longer when it offers c / o.
pub fn toast_life(d: &Finished) -> Duration {
    if d.ok && d.target.is_some() {
        TOAST_ACT
    } else {
        Duration::from_secs(6)
    }
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
    /// A download/install a coding agent in this pane is running (from its screen).
    pub agent_act: Option<Activity>,
    /// Everything this pane prints goes to a text file (sv log start).
    pub log: Option<crate::sessionlog::Log>,
    /// The ssh command that just dropped (ctrl+] R reconnects).
    pub reconnect: Option<String>,
    /// Rows of the last table `sv` printed here (for F4).
    pub table: Option<PathBuf>,
    /// Stats file of the last `sv flow` run here.
    pub flow: Option<PathBuf>,
    /// This shell runs a coding agent.
    pub agent: Option<Agent>,
    /// Typed once the shell shows its first prompt (so it isn't echoed twice).
    pending_input: Option<(String, Instant)>,
    /// Don't look for approval prompts before this (just answered one).
    agent_quiet_until: Instant,
    agent_check_at: Instant,
    agent_check_due: bool,
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
    /// The New agent dialog.
    NewAgent(Box<NewAgentForm>),
    /// Agents waiting for approval, with the highlighted row.
    Approvals(usize),
}

/// The New agent dialog's fields.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAgentForm {
    pub clis: Vec<Cli>,
    pub cli: usize,
    pub name: String,
    pub prompt: String,
    /// The command line, for a custom agent.
    pub custom: String,
    pub worktree: bool,
    /// 0 CLI · 1 name · 2 instruction / command · 3 worktree
    pub field: usize,
    pub error: Option<String>,
    /// Where it will run.
    pub folder: String,
}

impl NewAgentForm {
    pub fn cli(&self) -> Cli {
        self.clis[self.cli]
    }

    fn text_field(&mut self) -> Option<&mut String> {
        let generic = self.cli() == Cli::Generic;
        match self.field {
            1 => Some(&mut self.name),
            2 if generic => Some(&mut self.custom),
            2 => Some(&mut self.prompt),
            _ => None,
        }
    }
}

pub enum Answer {
    Allow,
    Always,
    Deny,
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
    /// Agents started so far (for colours and default names).
    agents_made: usize,
    agent_files_at: Instant,
    agent_files_busy: bool,
    /// Overlap warnings already shown.
    overlaps_seen: Vec<String>,
    /// The tab F9 came from.
    before_agents: Option<usize>,
    /// Last agents.json written (for `sv agents`).
    agents_json: String,
    /// `sv agent wait` callers: (agent name, where to reply).
    waiters: Vec<(String, Sender<serde_json::Value>)>,
    /// The team started with `sv agent run` (one at a time).
    team: Option<TeamRun>,
    /// The agent board (alt+g).
    pub board: Option<crate::board::Board>,
    /// Dragging a split's divider with the mouse: its path in the tree.
    drag: Option<Vec<bool>>,
    /// The layout editor (alt+e).
    pub editor: Option<crate::layouted::Editor>,
    /// The settings screen (alt+s).
    pub settings: Option<crate::settings::Screen>,
    /// Text selected with the mouse.
    pub sel: Option<Selection>,
    /// The last left click (for double / triple click): when, where, how many.
    click: Option<(Instant, PaneId, u16, u16, u8)>,
    /// The right-click menu.
    pub ctx: Option<ContextMenu>,
    /// Find in a pane's scrollback.
    pub find: Option<Find>,
    /// The command palette (ctrl+] space).
    pub palette: Option<Palette>,
    /// Shells that get what you type (broadcast, per tab).
    pub broadcast: std::collections::HashSet<PaneId>,
    /// The last failed command, while its card shows.
    pub fail: Option<(FailCard, Instant)>,
    /// The saved-sessions picker: selected row, (name, address, command).
    pub sessions: Option<(usize, Vec<crate::sessions::Entry>)>,
    /// The activity list (ctrl+] j, `sv activity`), with the selected row.
    pub acts_list: Option<usize>,
    /// Downloads and installs that finished, newest first.
    pub done: std::collections::VecDeque<Finished>,
    /// The last finished download's actions (ctrl+] c cd · o open), while its toast shows.
    pub toast_act: Option<(Finished, Instant)>,
    /// Current settings (folder colours …).
    pub ui: crate::settings::Settings,
    /// Kept while the board is closed, so the plan survives.
    plan_kept: Option<crate::board::Plan>,
    /// What happened between agents (sv agent log): (unix time, text).
    pub bus_log: std::collections::VecDeque<(i64, String)>,
    pub low_power: bool,
    pub light: bool,
    pub truecolor: bool,
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
        agents_made: 0,
        agent_files_at: Instant::now(),
        agent_files_busy: false,
        overlaps_seen: Vec::new(),
        before_agents: None,
        agents_json: String::new(),
        waiters: Vec::new(),
        team: None,
        board: None,
        plan_kept: None,
        drag: None,
        editor: None,
        settings: None,
        acts_list: None,
        sel: None,
        click: None,
        ctx: None,
        find: None,
        sessions: None,
        palette: None,
        broadcast: std::collections::HashSet::new(),
        fail: None,
        done: std::collections::VecDeque::new(),
        toast_act: None,
        ui: crate::settings::Settings::load(),
        bus_log: std::collections::VecDeque::new(),
        low_power,
        light: opts.light,
        truecolor: crate::settings::Settings::load().true_color(),
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
    crate::loading::set_style(&app.ui.spinner);
    crate::loading::set_bar(app.ui.progress_bar);
    crate::loading::set_low_power(app.low_power);
    let first = app.new_pane(None)?;
    app.tabs.push(Tab::new(first));
    app.offer_restore()?;
    // a fresh start (nothing restored or asked): the folder's or default layout
    if !matches!(app.modal, Modal::Confirm(..))
        && app.tabs.len() == 1
        && app.tabs[0].root.panes().len() == 1
    {
        let here = app.focused_cwd().unwrap_or_default();
        if let Some(name) = crate::layouted::Prefs::load().start_layout(&here) {
            if let Err(e) = app.open_layout(&name) {
                app.notice(format!("start layout: {e}"));
            }
        }
    }
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
    {
        let tx = tx.clone();
        if let Err(e) = crate::bus::listen(&app.opts.bus_path, move |req, reply| {
            let _ = tx.send(Ev::Bus(req, reply));
        }) {
            app.opts.warnings.push(format!(
                "agent bus: {e} — sv agent spawn/wait/tree won't work"
            ));
        }
    }
    drop(tx);

    let result = app.event_loop(rx);
    let _ = std::fs::remove_file(&app.opts.bus_path);
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
                    Tool::System(_) | Tool::Flow(_) | Tool::Remote(_) => String::new(),
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
        self.new_pane_env(cwd, Vec::new())
    }

    /// A new shell with extra environment variables (e.g. an agent's name).
    fn new_pane_env(
        &mut self,
        cwd: Option<PathBuf>,
        more_env: Vec<(String, String)>,
    ) -> io::Result<PaneId> {
        let id = self.next_id;
        self.next_id += 1;
        let (rows, cols) = (self.screen.height.max(2), self.screen.width.max(2));
        let mut env = self.opts.extra_env.clone();
        env.extend(more_env);
        let spawned = Pane::spawn(
            &self.opts.launch,
            &env,
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
                agent_act: None,
                log: None,
                reconnect: None,
                table: None,
                flow: None,
                agent: None,
                pending_input: None,
                agent_quiet_until: Instant::now(),
                agent_check_at: Instant::now(),
                agent_check_due: false,
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
            Some(kind @ ("files" | "git" | "system" | "thermal" | "flow")) => {
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
        let tool = if kind == "system" || kind == "thermal" {
            let mut sp = SysPane::new();
            sp.thermal = kind == "thermal";
            Tool::System(sp)
        } else if kind == "flow" {
            Tool::Flow(FlowPane::new())
        } else {
            let (mut f, mut jobs) = FilesPane::new(cwd);
            f.folder_style = Self::folder_style(&self.ui);
            if kind == "git" {
                jobs.extend(f.set_view(files::View::Git));
            }
            self.run_jobs(id, jobs);
            Tool::Files(f)
        };
        self.tools.insert(id, tool);
        self.update_loading_marks();
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

    pub fn focused_cwd(&self) -> Option<PathBuf> {
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
                    Ev::AgentFiles(list) => self.on_agent_files(list),
                    Ev::Bus(req, reply) => self.on_bus(req, reply),
                    Ev::Plan(run, out) => self.on_plan(run, out),
                    Ev::Notice(msg) => self.notice(msg),
                    Ev::Remote(id, done) => {
                        if let Some(Tool::Remote(r)) = self.tools.get_mut(&id) {
                            let jobs = r.done(*done);
                            self.run_remote(id, jobs);
                            self.dirty = true;
                        }
                    }
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

            if self
                .fail
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() >= FAIL_CARD)
            {
                self.fail = None;
                self.dirty = true;
            }
            if self
                .toast_act
                .as_ref()
                .is_some_and(|(d, at)| at.elapsed() >= toast_life(d))
            {
                self.toast_act = None;
                self.dirty = true;
            }
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
        if !p.progress_due
            || (p.activity.is_none() && p.agent.is_none())
            || p.progress_at.elapsed() < PROGRESS_EVERY
        {
            return;
        }
        p.progress_due = false;
        p.progress_at = Instant::now();
        let cols = p.size.1;
        let rows: Vec<String> = p.pane.screen().rows(0, cols).collect();
        let reading = activity::read_screen(rows.iter().map(String::as_str));
        let mut marks = false;
        if let Some(a) = p.activity.as_mut() {
            let before = a.progress;
            if a.update(reading.clone()) {
                self.dirty = true;
            }
            marks |= a.progress != before || a.target.is_some() && before.is_none();
        }
        // a coding agent running an install: from what its screen shows
        if let Some(agent) = p.agent.as_ref() {
            let working = matches!(agent.status, agents::Status::Working) && !agent.paused;
            let cmd = working
                .then(|| activity::agent_command(rows.iter().map(String::as_str)))
                .flatten();
            match cmd {
                Some(cmd) => {
                    if p.agent_act.as_ref().is_none_or(|a| a.cmd != cmd) {
                        p.agent_act = activity::start(&cmd, Path::new(&p.cwd));
                        marks = true;
                    }
                    if let Some(a) = p.agent_act.as_mut() {
                        a.update(reading);
                    }
                    self.dirty = true;
                }
                None if p.agent_act.is_some() => {
                    p.agent_act = None;
                    marks = true;
                    self.dirty = true;
                }
                None => {}
            }
        }
        if marks {
            self.update_loading_marks();
        }
    }

    /// Show a loading mark on the folders downloads are filling (files panes).
    fn update_loading_marks(&mut self) {
        let mut marks: HashMap<PathBuf, Option<u8>> = HashMap::new();
        for p in self.panes.values() {
            for a in [p.activity.as_ref(), p.agent_act.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Some(t) = &a.target {
                    marks.insert(t.clone(), a.progress);
                }
            }
        }
        for t in self.tools.values_mut() {
            if let Tool::Files(fp) = t {
                if fp.loading.keys().ne(marks.keys()) || fp.loading != marks {
                    fp.loading = marks.clone();
                    self.dirty = true;
                }
            }
        }
    }

    /// Every download and install in flight: (shell pane, activity, agent name).
    pub fn activities(&self) -> Vec<(PaneId, &Activity, Option<&str>)> {
        let mut v: Vec<(PaneId, &Activity, Option<&str>)> = Vec::new();
        for (id, p) in &self.panes {
            if let Some(a) = &p.activity {
                v.push((*id, a, None));
            }
            if let Some(a) = &p.agent_act {
                v.push((*id, a, p.agent.as_ref().map(|g| g.name.as_str())));
            }
        }
        v.sort_by_key(|(_, a, _)| a.started);
        v
    }

    /// cd into a finished download (its folder), in the shell it ran in.
    fn done_cd(&mut self, d: &Finished) {
        let Some(target) = d.target.clone() else {
            return;
        };
        let dir = if target.is_dir() {
            target
        } else {
            match target.parent() {
                Some(p) => p.to_path_buf(),
                None => return,
            }
        };
        let id = if self.panes.contains_key(&d.pane) {
            d.pane
        } else {
            self.focused()
        };
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if p.running {
            self.notice("the shell is busy — wait for the command to finish");
            return;
        }
        let cwd = PathBuf::from(&p.cwd);
        let text = format!("\x05\x15cd {}\r", shell_quote(&relative(&dir, &cwd)));
        let _ = p.pane.write(text.as_bytes());
        self.focus_pane(id);
        self.toast_act = None;
        self.notice = None;
        self.dirty = true;
    }

    /// Open a finished download in the files pane of its tab (a folder: its
    /// contents; a file: selected in its folder).
    fn done_open(&mut self, d: &Finished) -> io::Result<()> {
        let Some(target) = d.target.clone() else {
            return Ok(());
        };
        if let Some(t) = self.tab_of(d.pane) {
            self.switch_tab(t);
        }
        if self.view == View::Basic {
            self.set_view(View::Full);
        }
        let tab = self.active;
        let existing = self.tabs[tab]
            .root
            .panes()
            .into_iter()
            .find(|id| matches!(self.tools.get(id), Some(Tool::Files(_))));
        let fid = match existing {
            Some(id) => id,
            None => {
                self.show_tool("files", false)?;
                self.focused()
            }
        };
        if let Some(Tool::Files(f)) = self.tools.get_mut(&fid) {
            let jobs = f.reveal(&target);
            self.run_jobs(fid, jobs);
        }
        let t = &mut self.tabs[tab];
        t.focus = fid;
        t.zoomed = false;
        self.toast_act = None;
        self.notice = None;
        self.relayout();
        Ok(())
    }

    /// Keys in the activity list.
    fn acts_key(&mut self, key: &[u8]) -> io::Result<()> {
        self.dirty = true;
        let Some(sel) = self.acts_list else {
            return Ok(());
        };
        let running: Vec<PaneId> = self.activities().iter().map(|(id, _, _)| *id).collect();
        let n = running.len() + self.done.len();
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => self.acts_list = Some(sel.saturating_sub(1)),
            b"\x1b[B" | b"\x1bOB" | b"j" => {
                self.acts_list = Some((sel + 1).min(n.saturating_sub(1)))
            }
            b"\r" => {
                self.acts_list = None;
                let id = if sel < running.len() {
                    Some(running[sel])
                } else {
                    self.done.get(sel - running.len()).map(|d| d.pane)
                };
                if let Some(id) = id.filter(|id| self.panes.contains_key(id)) {
                    self.focus_pane(id);
                }
            }
            b"c" | b"o" if sel >= running.len() => {
                if let Some(d) = self.done.get(sel - running.len()).cloned() {
                    if d.target.is_none() {
                        self.notice("nothing to open: Shellvisor can't tell where that went");
                    } else {
                        self.acts_list = None;
                        if key == b"c" {
                            self.done_cd(&d);
                        } else {
                            self.done_open(&d)?;
                        }
                    }
                }
            }
            b"c" | b"o" => self.notice("still running — c and o work once it has finished"),
            b"x" => {
                self.done.clear();
                self.acts_list = Some(sel.min(running.len().saturating_sub(1)));
            }
            b"\x1b" | b"q" => self.acts_list = None,
            _ => {}
        }
        Ok(())
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
        self.tick_agents();
        let target = self.input_target();
        if target != self.last_focus {
            self.last_focus = target;
            if let Some(p) = self.panes.get_mut(&target) {
                self.focus_tick += 1;
                p.focused_at = self.focus_tick;
            }
        }
        let shell = self
            .paired_shell(self.active)
            .and_then(|id| self.panes.get(&id));
        let shell_cwd = shell.map(|p| PathBuf::from(&p.cwd));
        // What the Flow pane watches: the job, the sv flow file, the command.
        let watch = shell.map(|p| {
            (
                p.pane.foreground_job(),
                p.flow.clone(),
                if p.running {
                    p.command.clone()
                } else {
                    String::new()
                },
                p.started,
            )
        });
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
                Some(Tool::Flow(fp)) => {
                    if fp.due_in(low).is_zero() {
                        let (pg, flow, cmd, started) =
                            watch.clone().unwrap_or((None, None, String::new(), None));
                        let before = (fp.job.clone(), fp.flow.clone());
                        fp.sample(pg, flow.as_deref(), &cmd, started);
                        out.redraw = before != (fp.job.clone(), fp.flow.clone()) || pg.is_some();
                    }
                }
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
                    if let Some(a) = sp.alert.take() {
                        out.acts.push(files::Act::Notice(a));
                    }
                    out.redraw = true;
                }
                Some(Tool::System(_)) | Some(Tool::Remote(_)) => {}
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
                Tool::Flow(fp) => fp.due_in(self.low_power),
                Tool::System(sp) => sp.due_in(self.low_power),
                Tool::Remote(_) => continue,
                Tool::Files(_) if self.low_power => continue,
                Tool::Files(f) => f.poll_due_in(),
            };
            t = t.min(due.max(Duration::from_millis(5)));
        }
        if let Some((_, at)) = &self.notice {
            t = t.min((*at + NOTICE).saturating_duration_since(Instant::now()));
        }
        if let Some((d, at)) = &self.toast_act {
            t = t.min((*at + toast_life(d)).saturating_duration_since(Instant::now()));
        }
        if let Some((_, at)) = &self.fail {
            t = t.min((*at + FAIL_CARD).saturating_duration_since(Instant::now()));
        }
        if self.session_dirty {
            t = t.min((self.last_save + SAVE_EVERY).saturating_duration_since(Instant::now()));
        }
        if self.panes.values().any(|p| p.pending_input.is_some()) {
            t = t.min(Duration::from_millis(50));
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
        // "Mouse: terminal" in settings: the terminal keeps it (only what the
        // program in the pane asks for is passed on)
        let capture = (self.opts.mouse || self.view == View::Full) && self.ui.mouse != "terminal";
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

    pub fn tab_of(&self, id: PaneId) -> Option<usize> {
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
        if let Some(l) = p.log.as_mut() {
            l.feed(bytes);
        }

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
            if p.activity.is_some() || p.agent.is_some() {
                p.progress_due = true;
            }
            if let Some(a) = p.agent.as_mut() {
                a.last_output = Instant::now();
                if matches!(a.status, agents::Status::Idle | agents::Status::Starting) && p.running
                {
                    a.status = agents::Status::Working;
                    self.dirty = true;
                }
                p.agent_check_due = true;
            }
        }
        self.update_progress(id);
        self.check_agent(id);
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
                    p.activity = activity::start(&p.command, Path::new(&p.cwd));
                }
            }
            ShellEvent::CommandStarted => {
                p.reconnect = None;
                if self.fail.as_ref().is_some_and(|(c, _)| c.pane == id) {
                    self.fail = None;
                }
                p.started = Some(Instant::now());
                p.running = true;
                p.activity = activity::start(&p.command, Path::new(&p.cwd));
                if let Some(a) = p.agent.as_mut() {
                    // the agent (or another command) runs again in its column
                    let cli = Cli::of_command(&p.command);
                    if cli != Cli::Generic || !a.launched {
                        a.cli = if cli == Cli::Generic { a.cli } else { cli };
                    }
                    a.status = agents::Status::Working;
                    a.started = Instant::now();
                }
                self.dirty = true;
            }
            ShellEvent::CommandFinished(_) if p.agent.as_ref().is_some_and(|a| a.paused) => {
                // the shell saw the paused agent's job stop; it hasn't ended
                p.running = false;
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
                let agent_done = p
                    .agent
                    .as_mut()
                    .filter(|a| !matches!(a.status, agents::Status::Exited(_)))
                    .map(|a| {
                        a.status = agents::Status::Exited(code);
                        (a.name.clone(), a.files_summary(), a.branch.clone())
                    });
                let first = p.command.split_whitespace().next().unwrap_or("");
                let dropped = (first == "ssh" || first == "mosh") && code == Some(255);
                // a failed command: its last lines, for the card
                let card = match code {
                    Some(c)
                        if c != 0
                            && ![130, 131, 141, 143, 146, 148].contains(&c)
                            && !p.command.trim().is_empty()
                            && p.agent.is_none()
                            && p.activity.is_none()
                            && !no_card_for(&p.command) =>
                    {
                        let screen = p.pane.screen();
                        let (crow, ccol) = screen.cursor_position();
                        let cols = screen.size().1;
                        let cmd = p.command.trim().to_string();
                        // the new prompt may already be on the cursor's row
                        let end = if ccol > 0 { crow } else { crow + 1 };
                        let mut lines: Vec<String> = (0..end)
                            .rev()
                            .map(|r| {
                                screen
                                    .contents_between(r, 0, r, cols)
                                    .trim_end()
                                    .to_string()
                            })
                            .filter(|l| !l.trim().is_empty())
                            .take_while(|l| !l.ends_with(&cmd))
                            .take(5)
                            .collect();
                        lines.reverse();
                        Some(FailCard {
                            pane: id,
                            cmd,
                            code: c,
                            took: p.started.map(|s| s.elapsed().as_secs()).unwrap_or(0),
                            lines,
                        })
                    }
                    _ => None,
                };
                if dropped {
                    p.reconnect = Some(p.command.clone());
                }
                let done = p.activity.take();
                p.running = false;
                p.started = None;
                p.last_exit = code;
                p.command.clear();
                self.dirty = true;
                if dropped {
                    self.notice("the ssh connection closed · ctrl+] R (or right-click) reconnects");
                }
                if let Some(c) = card.filter(|_| self.ui.fail_card) {
                    self.fail = Some((c, Instant::now()));
                }
                if let Some(a) = done {
                    let ok = matches!(code, Some(0) | None);
                    let d = Finished {
                        label: a.label.clone(),
                        cmd: a.cmd.clone(),
                        target: a.target.clone().filter(|t| t.exists()),
                        ok,
                        code,
                        amount: a.amount.clone(),
                        src: a.src.clone(),
                        took,
                        at: Instant::now(),
                        pane: id,
                    };
                    self.done.push_front(d.clone());
                    self.done.truncate(20);
                    // the finish toast (ui draws it; ctrl+] c / o while it shows)
                    self.toast_act = Some((d, Instant::now()));
                    self.dirty = true;
                    self.update_loading_marks();
                }
                if let Some((name, files, branch)) = agent_done {
                    let what = if files.is_empty() {
                        "no changes".to_string()
                    } else {
                        files
                    };
                    let on = branch.map(|b| format!(" on {b}")).unwrap_or_default();
                    self.notice(match code {
                        Some(0) | None => format!("✔ agent {name} finished · {what}{on}"),
                        Some(c) => format!("✖ agent {name} exited ({c}) · {what}{on}"),
                    });
                    self.agent_files_at = Instant::now() - Duration::from_secs(60);
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
            ShellEvent::Flow(path) => {
                p.flow = Some(PathBuf::from(path));
                self.dirty = true;
            }
            ShellEvent::Table(path) => {
                p.table = Some(PathBuf::from(path));
                if !self.option_hint_shown || self.notice.is_none() {
                    self.notice("▦ F4 explores this table — sort, filter, copy");
                }
            }
            ShellEvent::PromptShown => {
                // the line editor starts right after the prompt hook: type
                // a moment later so the terminal doesn't echo it twice
                if let Some((_, at)) = p.pending_input.as_mut() {
                    *at = Instant::now() - Duration::from_millis(1850);
                }
            }
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
            if self.palette.is_some() && self.modal == Modal::None {
                self.flush_forward(&mut forward)?;
                self.palette_key(key)?;
                continue;
            }
            if self.sessions.is_some() && self.modal == Modal::None {
                self.flush_forward(&mut forward)?;
                self.sessions_key(key)?;
                continue;
            }
            if self.find.is_some() && self.modal == Modal::None {
                self.flush_forward(&mut forward)?;
                self.find_key(key);
                continue;
            }
            if let Some(m) = self.ctx {
                self.dirty = true;
                match key {
                    b"\x1b[A" | b"\x1bOA" => {
                        self.ctx = Some(ContextMenu {
                            sel: (m.sel + CONTEXT_ITEMS.len() - 1) % CONTEXT_ITEMS.len(),
                            ..m
                        })
                    }
                    b"\x1b[B" | b"\x1bOB" => {
                        self.ctx = Some(ContextMenu {
                            sel: (m.sel + 1) % CONTEXT_ITEMS.len(),
                            ..m
                        })
                    }
                    b"\r" => {
                        self.ctx = None;
                        self.context_act_menu(&m, m.sel)?;
                    }
                    _ => self.ctx = None,
                }
                continue;
            }
            if self.sel.is_some_and(|s| !s.dragging) && !key.starts_with(b"\x1b[<") {
                self.sel = None;
                self.dirty = true;
            }
            if self.board.is_some() && self.modal == Modal::None {
                self.board_key(key);
                continue;
            }
            if self.settings.is_some() && self.modal == Modal::None {
                self.settings_key(key);
                continue;
            }
            if self.acts_list.is_some() && self.modal == Modal::None {
                self.flush_forward(&mut forward)?;
                self.acts_key(key)?;
                continue;
            }
            if self.editor.is_some() && self.modal == Modal::None && self.table.is_none() {
                self.editor_key(key)?;
                continue;
            }
            if self.table.is_some() && self.modal == Modal::None {
                self.table_key(key)?;
                if self.quit.is_some() {
                    return Ok(());
                }
                continue;
            }
            // Modal states swallow keys first.
            match self.modal.clone() {
                Modal::NewAgent(form) => {
                    self.dirty = true;
                    self.new_agent_key(*form, key);
                    continue;
                }
                Modal::Approvals(sel) => {
                    self.dirty = true;
                    let list = self.pending_approvals();
                    let n = list.len();
                    if n == 0 {
                        self.modal = Modal::None;
                        continue;
                    }
                    let sel = sel.min(n - 1);
                    let id = list[sel];
                    match key {
                        b"\x1b[A" | b"\x1bOA" | b"k" => {
                            self.modal = Modal::Approvals((sel + n - 1) % n)
                        }
                        b"\x1b[B" | b"\x1bOB" | b"j" => {
                            self.modal = Modal::Approvals((sel + 1) % n)
                        }
                        b"y" | b"Y" => self.answer(id, Answer::Allow),
                        b"a" | b"A" => self.answer(id, Answer::Always),
                        b"n" | b"N" => self.answer(id, Answer::Deny),
                        b"\r" => {
                            // go to that agent to answer it yourself
                            self.modal = Modal::None;
                            self.focus_pane(id);
                        }
                        _ => self.modal = Modal::None,
                    }
                    if matches!(self.modal, Modal::Approvals(_))
                        && self.pending_approvals().is_empty()
                    {
                        self.modal = Modal::None;
                    }
                    continue;
                }
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
                    let offer = self
                        .toast_act
                        .as_ref()
                        .filter(|(d, at)| at.elapsed() < TOAST_ACT && d.ok && d.target.is_some())
                        .map(|(d, _)| d.clone());
                    if key == [0x1d] {
                        forward.push(0x1d); // ctrl+] twice = a real ctrl+]
                    } else if let (Some(d), b"c") = (&offer, key) {
                        if d.target.as_ref().is_some_and(|t| t.is_dir()) {
                            self.flush_forward(&mut forward)?;
                            self.done_cd(d);
                        } else if let Some(cmd) = keys::after_leader(key) {
                            self.flush_forward(&mut forward)?;
                            self.run_cmd(cmd)?;
                        }
                    } else if let (Some(d), b"o") = (&offer, key) {
                        self.flush_forward(&mut forward)?;
                        self.done_open(d)?;
                    } else if let (Some((card, _)), b"c" | b"r") = (self.fail.clone(), key) {
                        self.flush_forward(&mut forward)?;
                        if key == b"c" {
                            let mut text = format!("$ {}\n", card.cmd);
                            text.push_str(&card.lines.join("\n"));
                            self.copy(&text);
                            self.notice("copied the command and its last lines");
                        } else if let Some(p) = self.panes.get_mut(&card.pane) {
                            if p.running {
                                self.notice("the shell is busy — wait for the command to finish");
                            } else {
                                let _ = p.pane.write(format!("\x05\x15{}\r", card.cmd).as_bytes());
                                self.fail = None;
                                self.focus_pane(card.pane);
                            }
                        }
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

    fn run_remote(&self, id: PaneId, jobs: Vec<crate::remote::Job>) {
        for job in jobs {
            let tx = self.tx.clone();
            let _ = spawn_thread("remote".into(), move || {
                let _ = tx.send(Ev::Remote(id, Box::new(crate::remote::run(job))));
            });
        }
    }

    /// A remote files pane for the ssh session in this tab (the focused
    /// shell, else any shell in the tab running or just out of ssh).
    fn open_remote(&mut self) -> io::Result<()> {
        let tab = self.active;
        let mut ids = vec![self.focused()];
        ids.extend(self.tabs[tab].root.panes());
        let found = ids.iter().find_map(|id| {
            let p = self.panes.get(id)?;
            let cmd = if p.running {
                Some(p.command.clone())
            } else {
                p.reconnect.clone()
            }?;
            crate::remote::ssh_target(&cmd).map(|a| (*id, a, PathBuf::from(&p.cwd)))
        });
        let Some((shell, args, local)) = found else {
            self.notice("no ssh session in this tab — connect first (sv ssh NAME, or ssh host)");
            return Ok(());
        };
        if self.view == View::Basic {
            self.view = View::Full;
        }
        let id = self.next_id;
        self.next_id += 1;
        let (pane, job) = crate::remote::RemotePane::new(args, local);
        self.tools.insert(id, Tool::Remote(pane));
        self.run_remote(id, vec![job]);
        let t = &mut self.tabs[tab];
        t.root.split_at(shell, Axis::Horizontal, id, 0.55);
        t.focus = id;
        t.zoomed = false;
        self.relayout();
        Ok(())
    }

    fn toggle_broadcast(&mut self) {
        let tab = self.active;
        let shells: Vec<PaneId> = self.tabs[tab]
            .root
            .panes()
            .into_iter()
            .filter(|id| self.panes.contains_key(id))
            .collect();
        if shells.len() < 2 {
            self.notice("broadcast needs two or more shells in this tab — split one with alt+\\");
            return;
        }
        if shells.iter().all(|id| self.broadcast.contains(id)) {
            for id in &shells {
                self.broadcast.remove(id);
            }
            self.notice("broadcast off");
        } else {
            self.broadcast.extend(shells.iter().copied());
            self.notice(format!(
                "broadcast on: what you type goes to all {} shells in this tab · ctrl+] B stops",
                shells.len()
            ));
        }
        self.dirty = true;
    }

    fn open_palette(&mut self) {
        let mut items: Vec<(String, String, PaletteAct)> = Vec::new();
        for (name, cmd, _keys, desc) in keys::ACTIONS {
            if desc.is_empty() || matches!(cmd, Cmd::Leader | Cmd::Palette) {
                continue;
            }
            let label = desc.split(" (also").next().unwrap_or(desc);
            let mut c = label.chars();
            let label = match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            };
            let shown = self.opts.keymap.shown(name).to_string();
            items.push((label, shown, PaletteAct::Cmd(*cmd)));
        }
        for (i, row) in crate::settings::row_names().iter().enumerate() {
            items.push((
                format!("Setting: {row}"),
                "settings".into(),
                PaletteAct::Setting(i),
            ));
        }
        for (name, addr, cmd) in crate::sessions::entries() {
            items.push((
                format!("Connect: {name}"),
                addr,
                PaletteAct::Session(name, cmd),
            ));
        }
        for p in keys::Preset::ALL {
            items.push((
                format!("Layout: {}", p.name()),
                "preset".into(),
                PaletteAct::Cmd(Cmd::Preset(p)),
            ));
        }
        for name in crate::layouted::saved_layouts() {
            items.push((
                format!("Layout: {name}"),
                "saved".into(),
                PaletteAct::Layout(name),
            ));
        }
        for i in 0..self.tabs.len() {
            let label = self.tabs[i]
                .name
                .clone()
                .or_else(|| {
                    self.panes
                        .get(&self.tabs[i].focus)
                        .map(|p| format!("{} · {}", p.shell, crate::ui::short_path(&p.cwd)))
                })
                .unwrap_or_default();
            items.push((
                format!("Tab {}: {label}", i + 1),
                "go to tab".into(),
                PaletteAct::Tab(i),
            ));
        }
        for dev in crate::serial::ports() {
            items.push((
                format!("Serial: {dev}"),
                "115200 baud".into(),
                PaletteAct::Session(
                    format!("serial · {}", dev.rsplit('/').next().unwrap_or(&dev)),
                    format!("sv serial --here {} 115200", shell_quote(&dev)),
                ),
            ));
        }
        self.palette = Some(Palette {
            query: String::new(),
            sel: 0,
            items,
        });
        self.dirty = true;
    }

    fn palette_key(&mut self, key: &[u8]) -> io::Result<()> {
        self.dirty = true;
        let Some(pal) = self.palette.as_mut() else {
            return Ok(());
        };
        let n = pal.matches().len();
        match key {
            b"\x1b" => self.palette = None,
            b"\x1b[A" | b"\x1bOA" => pal.sel = pal.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" => pal.sel = (pal.sel + 1).min(n.saturating_sub(1)),
            [0x7f] | [0x08] => {
                pal.query.pop();
                pal.sel = 0;
            }
            [0x15] => {
                pal.query.clear();
                pal.sel = 0;
            }
            b"\r" => {
                let m = pal.matches();
                let Some(&i) = m.get(pal.sel) else {
                    return Ok(());
                };
                let act = pal.items[i].2.clone();
                self.palette = None;
                match act {
                    PaletteAct::Cmd(c) => self.run_cmd(c)?,
                    PaletteAct::Setting(row) => {
                        let mut sc = crate::settings::Screen::new();
                        sc.sel = row;
                        self.settings = Some(sc);
                    }
                    PaletteAct::Session(name, cmd) => self.open_session_tab(&name, &cmd)?,
                    PaletteAct::Layout(name) => {
                        if let Err(e) = self.open_layout(&name) {
                            self.notice(format!("layout: {e}"));
                        }
                    }
                    PaletteAct::Tab(t) => self.switch_tab(t),
                }
            }
            k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                pal.query.push_str(&String::from_utf8_lossy(k));
                pal.sel = 0;
            }
            _ => {}
        }
        Ok(())
    }

    /// Start or stop logging a pane to a text file.
    fn set_log(&mut self, pane: PaneId, on: bool, path: Option<PathBuf>) {
        let name = match self.tab_of(pane) {
            Some(t) => self.tabs[t]
                .name
                .clone()
                .unwrap_or_else(|| format!("tab{}", t + 1)),
            None => "pane".into(),
        };
        let Some(p) = self.panes.get_mut(&pane) else {
            self.notice("logging works in shell panes");
            return;
        };
        if !on {
            match p.log.take() {
                Some(l) => {
                    let msg = format!("stopped logging · saved in {}", l.path.display());
                    drop(l);
                    self.notice(msg);
                }
                None => self.notice("this pane isn't being logged"),
            }
            return;
        }
        if let Some(l) = &p.log {
            let msg = format!("already logging to {}", l.path.display());
            self.notice(msg);
            return;
        }
        let path = path.unwrap_or_else(|| crate::sessionlog::default_path(&name));
        match crate::sessionlog::Log::open(&path) {
            Ok(l) => {
                p.log = Some(l);
                self.notice(format!(
                    "logging this pane to {} · sv log stop",
                    path.display()
                ));
            }
            Err(e) => self.notice(format!("log: {}: {e}", path.display())),
        }
        self.dirty = true;
    }

    /// A new tab in the same folder; an ssh/mosh session in it runs again.
    fn duplicate(&mut self, pane: PaneId) -> io::Result<()> {
        let (cwd, cmd) = match self.panes.get(&pane) {
            Some(p) => {
                let first = p.command.split_whitespace().next().unwrap_or("");
                let remote = p.running && (first == "ssh" || first == "mosh");
                let again = if remote {
                    Some(p.command.clone())
                } else {
                    p.reconnect.clone()
                };
                (Some(PathBuf::from(&p.cwd)), again)
            }
            None => (self.focused_cwd(), None),
        };
        let id = self.new_pane(cwd)?;
        if let (Some(c), Some(p)) = (cmd, self.panes.get_mut(&id)) {
            p.pending_input = Some((format!("{c}\r"), Instant::now()));
        }
        let tab = self.active;
        self.tabs.insert(tab + 1, Tab::new(id));
        self.switch_tab(tab + 1);
        self.session_dirty = true;
        Ok(())
    }

    /// Run a dropped ssh session's command again.
    fn reconnect(&mut self, pane: PaneId) {
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        match p.reconnect.take() {
            Some(cmd) if !p.running => {
                let _ = p.pane.write(format!("\x05\x15{cmd}\r").as_bytes());
                self.notice(format!("reconnecting · {cmd}"));
            }
            Some(cmd) => {
                p.reconnect = Some(cmd);
                self.notice("the shell is busy — wait for the command to finish");
            }
            None => self.notice("nothing to reconnect here (an ssh session that dropped)"),
        }
    }

    fn open_sessions(&mut self) {
        let list = crate::sessions::entries();
        if list.is_empty() {
            self.notice(
                "no saved sessions yet — sv ssh save NAME user@host [-p port] [-i key] [-L spec]",
            );
            return;
        }
        self.sessions = Some((0, list));
        self.dirty = true;
    }

    /// Open a saved session (or ~/.ssh/config host) in a new tab named after it.
    fn open_session_tab(&mut self, name: &str, cmd: &str) -> io::Result<()> {
        let id = self.new_pane(self.focused_cwd())?;
        if let Some(p) = self.panes.get_mut(&id) {
            p.pending_input = Some((format!("{cmd}\r"), Instant::now()));
        }
        let tab = self.active;
        let mut t = Tab::new(id);
        t.name = Some(name.to_string());
        self.tabs.insert(tab + 1, t);
        self.switch_tab(tab + 1);
        self.session_dirty = true;
        Ok(())
    }

    fn sessions_key(&mut self, key: &[u8]) -> io::Result<()> {
        self.dirty = true;
        let Some((sel, list)) = self.sessions.as_mut() else {
            return Ok(());
        };
        let n = list.len();
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => *sel = (*sel + n - 1) % n,
            b"\x1b[B" | b"\x1bOB" | b"j" => *sel = (*sel + 1) % n,
            b"\r" => {
                let (name, _, cmd) = list[*sel].clone();
                self.sessions = None;
                self.open_session_tab(&name, &cmd)?;
            }
            b"\x1b" | b"q" => self.sessions = None,
            _ => {}
        }
        Ok(())
    }

    fn open_find(&mut self, pane: PaneId) {
        let query = self
            .find
            .as_ref()
            .filter(|f| f.pane == pane)
            .map(|f| f.query.clone())
            .unwrap_or_default();
        self.find = Some(Find {
            pane,
            query,
            matches: Vec::new(),
            cur: 0,
        });
        self.refind();
        self.dirty = true;
    }

    /// Search again for the query, starting at the newest match.
    fn refind(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let Some(p) = self.panes.get(&f.pane) else {
            self.find = None;
            return;
        };
        let lines = p.pane.screen().all_lines();
        f.matches = find_in(&lines, &f.query);
        f.cur = f.matches.len().saturating_sub(1);
        self.show_match();
    }

    /// Scroll the pane so the current match is in view (middle of the screen).
    fn show_match(&mut self) {
        let Some(f) = self.find.as_ref() else { return };
        let Some(&(line, _, _)) = f.matches.get(f.cur) else {
            return;
        };
        let pane = f.pane;
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        let sb = p.pane.screen().scrollback_lines();
        let rows = p.pane.screen().size().0 as usize;
        let top = sb - p.pane.scroll();
        if line >= top && line < top + rows {
            return; // already on screen
        }
        let want_top = line.saturating_sub(rows / 2);
        p.pane.scroll_to(sb.saturating_sub(want_top));
        self.dirty = true;
    }

    fn find_key(&mut self, key: &[u8]) {
        self.dirty = true;
        let Some(f) = self.find.as_mut() else { return };
        let n = f.matches.len();
        match key {
            b"\x1b" => {
                self.find = None;
                return;
            }
            b"\r" | b"\x1b[B" | b"\x1bOB" => {
                if n > 0 {
                    f.cur = (f.cur + n - 1) % n; // the one before (older)
                }
            }
            b"\x1b[A" | b"\x1bOA" => {
                if n > 0 {
                    f.cur = (f.cur + 1) % n; // the one after (newer)
                }
            }
            [0x7f] | [0x08] => {
                f.query.pop();
                self.refind();
                return;
            }
            [0x15] => {
                f.query.clear();
                self.refind();
                return;
            }
            k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                f.query.push_str(&String::from_utf8_lossy(k));
                self.refind();
                return;
            }
            _ => return,
        }
        self.show_match();
    }

    /// Clear a pane and its scrollback; the shell redraws its prompt.
    fn clear_pane(&mut self, pane: PaneId) {
        let Some(p) = self.panes.get_mut(&pane) else {
            self.notice("clear works in shell panes");
            return;
        };
        p.pane.clear_scrollback();
        if !p.running {
            // ctrl+L: the shell clears the screen and redraws the prompt
            let _ = p.pane.write(b"\x0c");
        }
        if self.find.as_ref().is_some_and(|f| f.pane == pane) {
            self.find = None;
        }
        self.sel = None;
        self.dirty = true;
    }

    fn open_url(&mut self, url: &str) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let ok = std::process::Command::new(opener)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok();
        self.notice(if ok {
            format!("opening {url}")
        } else {
            format!("couldn't run {opener}")
        });
    }

    /// The selected text (trailing spaces trimmed on each line).
    pub fn selection_text(&self) -> Option<String> {
        let s = self.sel?;
        let p = self.panes.get(&s.pane)?;
        if p.pane.scroll() != s.scroll {
            return None;
        }
        let ((r0, c0), (r1, c1)) = s.range();
        let text = p.pane.screen().contents_between(r0, c0, r1, c1 + 1);
        let text: Vec<&str> = text.lines().map(str::trim_end).collect();
        let text = text.join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// Copy the selection to the clipboard (as soon as it's made, like
    /// most terminals and tmux).
    fn copy_selection(&mut self) {
        if let Some(t) = self.selection_text() {
            self.copy(&t);
            let n = t.chars().count();
            self.notice(format!(
                "copied {n} character{} · right-click for paste",
                if n == 1 { "" } else { "s" }
            ));
        }
    }

    /// Where the right-click menu is drawn.
    pub fn context_rect(&self, m: &ContextMenu) -> Rect {
        let w = 24u16;
        let h = CONTEXT_ITEMS.len() as u16 + 2;
        let x = m.x.min(self.screen.right().saturating_sub(w));
        let y = if m.y + h <= self.screen.bottom() {
            m.y
        } else {
            m.y.saturating_sub(h)
        };
        Rect::new(x, y, w.min(self.screen.width), h.min(self.screen.height))
    }

    /// The link under the right-click, if there is one.
    pub fn context_link(&self, m: &ContextMenu) -> Option<String> {
        let p = self.panes.get(&m.pane)?;
        let cols = p.pane.screen().size().1;
        let (r, c) = m.cell;
        let line = p.pane.screen().contents_between(r, 0, r, cols);
        url_at(&line, c as usize)
    }

    fn context_act_menu(&mut self, m: &ContextMenu, item: usize) -> io::Result<()> {
        if item == 3 {
            match self.context_link(m) {
                Some(url) => self.open_url(&url),
                None => self.notice("no link there — right-click on a URL to open it"),
            }
            return Ok(());
        }
        self.context_act(m.pane, item)
    }

    /// Do a right-click menu item for a pane.
    fn context_act(&mut self, pane: PaneId, item: usize) -> io::Result<()> {
        self.dirty = true;
        match item {
            0 => match self.selection_text() {
                Some(_) => self.copy_selection(),
                None => self.notice("nothing selected — drag over text to select it"),
            },
            1 => {
                let Some(text) = crate::osinfo::paste_from_clipboard() else {
                    self.notice("the clipboard is empty (or can't be read here)");
                    return Ok(());
                };
                if let Some(p) = self.panes.get_mut(&pane) {
                    // like a terminal's own paste: bracketed when the program asked
                    let clean = text.replace("\x1b[201~", "");
                    let bytes = if p.pane.screen().bracketed_paste() {
                        format!("\x1b[200~{clean}\x1b[201~")
                    } else {
                        clean.replace("\r\n", "\r").replace('\n', "\r")
                    };
                    p.pane.write(bytes.as_bytes())?;
                }
                self.focus_pane(pane);
            }
            2 => {
                if let Some(p) = self.panes.get(&pane) {
                    let (rows, cols) = p.pane.screen().size();
                    let last = (0..rows)
                        .rev()
                        .find(|r| {
                            !p.pane
                                .screen()
                                .contents_between(*r, 0, *r, cols)
                                .trim()
                                .is_empty()
                        })
                        .unwrap_or(0);
                    self.sel = Some(Selection {
                        pane,
                        a: (0, 0),
                        b: (last, cols.saturating_sub(1)),
                        scroll: p.pane.scroll(),
                        dragging: false,
                    });
                    self.notice("selected · right-click → Copy");
                }
            }
            4 => {
                self.focus_pane(pane);
                self.open_find(pane);
            }
            5 => self.clear_pane(pane),
            6 => {
                let on = self.panes.get(&pane).is_some_and(|p| p.log.is_some());
                self.set_log(pane, !on, None);
            }
            7 => {
                self.focus_pane(pane);
                self.duplicate(pane)?;
            }
            8 => self.reconnect(pane),
            9 | 10 => {
                self.focus_pane(pane);
                self.run_cmd(if item == 9 {
                    Cmd::SplitRight
                } else {
                    Cmd::SplitDown
                })?;
            }
            _ => {}
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

    // ---------------------------------------------------------------- agents

    /// The tab holding agent columns.
    pub fn agents_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|t| {
            t.root
                .panes()
                .iter()
                .any(|id| self.panes.get(id).is_some_and(|p| p.agent.is_some()))
        })
    }

    /// Agent panes, oldest first.
    pub fn agent_ids(&self) -> Vec<PaneId> {
        let mut v: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.agent.is_some())
            .map(|(id, _)| *id)
            .collect();
        v.sort();
        v
    }

    fn agent_by_name(&self, name: &str) -> Option<&Agent> {
        self.panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .find(|a| a.name == name)
    }

    fn agent_pane(&self, name: &str) -> Option<PaneId> {
        self.agent_ids().into_iter().find(|id| {
            self.panes
                .get(id)
                .and_then(|p| p.agent.as_ref())
                .is_some_and(|a| a.name == name)
        })
    }

    /// Record something that happened between agents (sv agent log).
    fn bus_event(&mut self, text: impl Into<String>) {
        if self.bus_log.len() >= 300 {
            self.bus_log.pop_front();
        }
        self.bus_log
            .push_back((crate::data::value::now(), text.into()));
    }

    /// Finished = exited, or worked on its task and is now waiting for input.
    fn agent_finished(a: &Agent) -> bool {
        match a.status {
            agents::Status::Exited(_) => true,
            agents::Status::Idle => a.worked && !a.paused,
            _ => false,
        }
    }

    fn agent_summary(&self, id: PaneId) -> serde_json::Value {
        let Some(p) = self.panes.get(&id) else {
            return serde_json::Value::Null;
        };
        let Some(a) = p.agent.as_ref() else {
            return serde_json::Value::Null;
        };
        let status = if a.paused {
            "paused".to_string()
        } else {
            match &a.status {
                agents::Status::Starting => "starting".into(),
                agents::Status::Working => "working".into(),
                agents::Status::Idle if a.worked => "done".into(),
                agents::Status::Idle => "idle".into(),
                agents::Status::NeedsApproval(_) => "needs approval".into(),
                agents::Status::Exited(Some(0)) | agents::Status::Exited(None) => "exited".into(),
                agents::Status::Exited(Some(c)) => format!("exited {c}"),
            }
        };
        serde_json::json!({
            "name": a.name,
            "parent": a.parent,
            "depth": a.depth,
            "cli": a.cli.name(),
            "status": status,
            "task": a.task,
            "branch": a.branch,
            "folder": p.cwd,
            "files": a.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            "added": a.files.iter().map(|f| f.added).sum::<u64>(),
            "removed": a.files.iter().map(|f| f.removed).sum::<u64>(),
            "tokens": a.tokens,
            "helpers": a.internal,
        })
    }

    /// A request from `sv agent …` (any shell or agent inside Shellvisor).
    fn on_bus(&mut self, req: serde_json::Value, reply: Sender<serde_json::Value>) {
        use crate::bus::{err, ok};
        let op = req["op"].as_str().unwrap_or("").to_string();
        let caller = req["caller"].as_str().map(String::from);
        let field = |k: &str| req[k].as_str().unwrap_or("").to_string();
        let name = field("name");
        let answer = match op.as_str() {
            "spawn" => self.bus_spawn(&req, caller),
            "send" => {
                let from = caller.clone().unwrap_or_else(|| "you".into());
                match self.send_to_agent(&field("to"), &field("text"), &from) {
                    Ok(()) => ok(serde_json::json!({})),
                    Err(e) => err(e),
                }
            }
            "run" => self.bus_run(&req),
            "emit" => {
                let ev = field("event");
                let split = match ev.split_once('.') {
                    Some((a, e)) => Some((a.to_string(), e.to_string())),
                    None => caller.clone().map(|c| (c, ev.clone())),
                };
                match split {
                    None => err("from outside an agent, write <agent>.<event>"),
                    Some((_, event)) if !agents::valid_name(&event) => {
                        err(format!("event `{event}` — letters, digits, - and _"))
                    }
                    Some((agent, _)) if self.agent_pane(&agent).is_none() => {
                        err(format!("no agent called {agent}"))
                    }
                    Some((agent, event)) => {
                        let n = self.fire(&agent, &event, &field("text"));
                        ok(serde_json::json!({ "rules": n }))
                    }
                }
            }
            "usage" => {
                let who = Some(name.clone())
                    .filter(|n| !n.is_empty())
                    .or(caller.clone())
                    .unwrap_or_default();
                match (self.agent_pane(&who), req["tokens"].as_u64()) {
                    (None, _) => err(format!("no agent called {who}")),
                    (_, None) => err("how many tokens? sv agent usage <tokens>"),
                    (Some(id), Some(t)) => {
                        if let Some(a) = self.panes.get_mut(&id).and_then(|p| p.agent.as_mut()) {
                            a.tokens = Some(t);
                        }
                        self.dirty = true;
                        self.check_budget();
                        ok(serde_json::json!({}))
                    }
                }
            }
            "budget" => self.bus_budget(req["tokens"].as_u64()),
            "wait" => match self.agent_pane(&name) {
                None => err(format!("no agent called {name}")),
                Some(id) => {
                    let done = self
                        .panes
                        .get(&id)
                        .and_then(|p| p.agent.as_ref())
                        .is_some_and(Self::agent_finished);
                    if done {
                        ok(serde_json::json!({ "agent": self.agent_summary(id) }))
                    } else {
                        self.waiters.push((name, reply));
                        return;
                    }
                }
            },
            "tree" => {
                let rows: Vec<_> = self
                    .agent_ids()
                    .into_iter()
                    .map(|id| self.agent_summary(id))
                    .collect();
                ok(serde_json::json!({ "agents": rows, "team": self.team_summary() }))
            }
            "logs" => match self.agent_pane(&name) {
                None => err(format!("no agent called {name}")),
                Some(id) => {
                    let p = &self.panes[&id];
                    let cols = p.size.1;
                    let lines: Vec<String> = p
                        .pane
                        .screen()
                        .rows(0, cols)
                        .map(|r| r.trim_end().to_string())
                        .collect();
                    ok(serde_json::json!({ "lines": lines }))
                }
            },
            "pause" | "resume" | "stop" if req["all"].as_bool() == Some(true) => {
                self.bus_signal_all(&op)
            }
            "pause" | "resume" | "stop" => {
                self.bus_signal(&op, &name, req["tree"].as_bool().unwrap_or(false))
            }
            "log" => {
                let events: Vec<_> = self
                    .bus_log
                    .iter()
                    .map(|(t, e)| serde_json::json!({ "time": crate::data::value::iso_date(*t), "event": e }))
                    .collect();
                ok(serde_json::json!({ "events": events }))
            }
            other => err(format!("unknown request `{other}`")),
        };
        let _ = reply.send(answer);
    }

    /// Type a message into an agent (then Enter on its own, so the CLI submits it).
    fn send_to_agent(&mut self, to: &str, text: &str, from: &str) -> Result<(), String> {
        let id = self
            .agent_pane(to)
            .ok_or_else(|| format!("no agent called {to}"))?;
        if text.trim().is_empty() {
            return Err("nothing to send".into());
        }
        let p = self.panes.get_mut(&id).ok_or("gone")?;
        if p.agent.as_ref().is_some_and(|a| a.paused) {
            return Err(format!("{to} is paused — sv agent resume {to}"));
        }
        let _ = p.pane.write(text.as_bytes());
        let _ = p.pane.write(b"\r");
        if let Some(a) = p.agent.as_mut() {
            a.task = text.to_string();
            a.worked = false;
            a.last_output = Instant::now();
        }
        self.bus_event(format!(
            "{from} → {to}: {}",
            text.chars().take(80).collect::<String>()
        ));
        self.dirty = true;
        Ok(())
    }

    /// Limits for a spawn: config, or the running team's, and the parent's own
    /// max_children when it is a team member.
    fn spawn_limits(&self, parent: Option<&str>, name: &str) -> Result<agents::Limits, String> {
        let mut l = self.opts.agent_limits;
        let Some(tr) = &self.team else { return Ok(l) };
        l.max_agents = tr.team.max_agents.unwrap_or(l.max_agents);
        l.max_depth = tr.team.max_depth.unwrap_or(l.max_depth);
        if let Some(pm) = parent.and_then(|p| tr.team.member(p)) {
            pm.may_spawn(name)?;
            l.max_children = pm.children_allowed().unwrap_or(0);
        }
        Ok(l)
    }

    fn refuse_spawn(&mut self, who: Option<&str>, name: &str, why: String) -> serde_json::Value {
        self.bus_event(format!(
            "{} tried to spawn {name} — {why}",
            who.unwrap_or("you")
        ));
        self.notice(format!("agent spawn {why}"));
        crate::bus::err(why)
    }

    /// Start a team member (from team.yaml) with what it was told to do.
    fn start_member(
        &mut self,
        id: &str,
        parent: Option<(String, u8)>,
        task: &str,
    ) -> Result<String, String> {
        let Some(tr) = &self.team else {
            return Err("no team is running".into());
        };
        let m = tr
            .team
            .member(id)
            .cloned()
            .ok_or(format!("{id} isn't in team {}", tr.team.name))?;
        let task = [task.trim(), m.task.as_str(), tr.goal.as_str()]
            .into_iter()
            .find(|t| !t.is_empty())
            .unwrap_or("")
            .to_string();
        let prompt = tr.team.brief(&m, &task);
        let team_name = tr.team.name.clone();
        let folder = tr.folder.clone();
        let cli = if m.cmd.is_empty() {
            Cli::parse(&m.cli).unwrap_or(Cli::Claude)
        } else {
            Cli::Generic
        };
        let wt =
            m.worktree.unwrap_or(self.opts.agent_worktree) && agents::repo_head(&folder).is_some();
        let name = self.start_agent(parent, cli, &m.id, &prompt, &m.cmd, wt, Some(folder))?;
        if let Some(a) = self
            .agent_pane(&name)
            .and_then(|pid| self.panes.get_mut(&pid))
            .and_then(|p| p.agent.as_mut())
        {
            a.team = Some(team_name);
            for x in m.approve.iter().filter_map(|x| agents::Action::parse(x)) {
                if !a.always.contains(&x) {
                    a.always.push(x);
                }
            }
        }
        if let Some(tr) = self.team.as_mut() {
            tr.started.push(name.clone());
        }
        Ok(name)
    }

    /// `sv agent run team.yaml "goal"`
    fn bus_run(&mut self, req: &serde_json::Value) -> serde_json::Value {
        use crate::bus::{err, ok};
        if let Some(tr) = &self.team {
            let name = tr.team.name.clone();
            if self.team_agents(&name).iter().any(|(_, running)| *running) {
                return err(format!(
                    "team {name} is still running — sv agent stop --all, or wait for it"
                ));
            }
        }
        let team: crate::team::Team = match serde_json::from_value(req["team"].clone()) {
            Ok(t) => t,
            Err(e) => return err(format!("bad team: {e}")),
        };
        let folder = req["folder"]
            .as_str()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| self.focused_cwd())
            .unwrap_or_default();
        let goal = req["goal"].as_str().unwrap_or("").trim().to_string();
        match self.begin_team(team, goal, folder, None, None) {
            Ok((tname, started)) => ok(serde_json::json!({ "team": tname, "started": started })),
            Err(e) => err(e),
        }
    }

    /// Start a team: everything that can start now (team.yaml, or a dispatched plan).
    fn begin_team(
        &mut self,
        team: crate::team::Team,
        goal: String,
        folder: PathBuf,
        parallel: Option<usize>,
        step: Option<u32>,
    ) -> Result<(String, Vec<String>), String> {
        let tname = team.name.clone();
        let budget = team.budget_tokens;
        self.team = Some(TeamRun {
            team,
            goal,
            folder,
            started: Vec::new(),
            capped: None,
            warned: false,
            done: Vec::new(),
            parallel,
            step,
        });
        let started = match self.schedule() {
            Ok(s) => s,
            Err(e) => {
                let names = self
                    .team
                    .as_ref()
                    .map(|t| t.started.clone())
                    .unwrap_or_default();
                for n in &names {
                    self.signal_agent(n, "stop");
                }
                self.team = None;
                return Err(e);
            }
        };
        self.bus_event(format!(
            "team {tname} started: {}{}",
            started.join(", "),
            budget
                .map(|b| format!(" · budget {}", agents::fmt_tokens(b)))
                .unwrap_or_default()
        ));
        Ok((tname, started))
    }

    /// Members ready to start: `now`, or every `after:` agent done — within the
    /// team's parallel limit and released stage. Returns who started.
    fn schedule(&mut self) -> Result<Vec<String>, String> {
        let mut started = Vec::new();
        while let Some(tr) = &self.team {
            if tr.capped.is_some() {
                break;
            }
            // busy members: running and not finished with their task
            let running = self
                .team_agents(&tr.team.name)
                .iter()
                .filter(|(n, r)| *r && tr.team.member(n).is_some() && !tr.done.contains(n))
                .count();
            if tr.parallel.is_some_and(|p| running >= p) {
                break;
            }
            let next = tr.team.agents.iter().find(|m| {
                !tr.started.contains(&m.id)
                    && tr.step.is_none_or(|s| m.stage <= s)
                    && match &m.start {
                        crate::team::Start::Now => true,
                        crate::team::Start::After(deps) => deps.iter().all(|d| tr.done.contains(d)),
                        crate::team::Start::Spawned => false,
                    }
            });
            let Some(id) = next.map(|m| m.id.clone()) else {
                break;
            };
            match self.start_member(&id, None, "") {
                Ok(n) => started.push(n),
                Err(e) => {
                    if let Some(tr) = self.team.as_mut() {
                        tr.started.push(id.clone()); // don't retry forever
                    }
                    self.bus_event(format!("couldn't start {id}: {e}"));
                    return Err(format!("{id}: {e}"));
                }
            }
        }
        Ok(started)
    }

    /// (name, still running) for every agent of a team.
    fn team_agents(&self, team: &str) -> Vec<(String, bool)> {
        self.panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .filter(|a| a.team.as_deref() == Some(team))
            .map(|a| {
                (
                    a.name.clone(),
                    !matches!(a.status, agents::Status::Exited(_)),
                )
            })
            .collect()
    }

    fn team_tokens(&self, team: &str) -> u64 {
        self.panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .filter(|a| a.team.as_deref() == Some(team))
            .filter_map(|a| a.tokens)
            .sum()
    }

    fn team_summary(&self) -> serde_json::Value {
        let Some(tr) = &self.team else {
            return serde_json::Value::Null;
        };
        serde_json::json!({
            "name": tr.team.name,
            "goal": tr.goal,
            "tokens": self.team_tokens(&tr.team.name),
            "budget_tokens": tr.team.budget_tokens,
            "capped": tr.capped.is_some(),
        })
    }

    /// Past the team's budget: pause every agent of the team, once.
    fn check_budget(&mut self) {
        let Some(tr) = &self.team else { return };
        let Some(budget) = tr.team.budget_tokens else {
            return;
        };
        if tr.capped.is_some() {
            return;
        }
        let name = tr.team.name.clone();
        let used = self.team_tokens(&name);
        if used >= budget {
            let mut paused = Vec::new();
            for (n, running) in self.team_agents(&name) {
                let already = self.agent_by_name(&n).is_some_and(|a| a.paused);
                if running && !already && self.signal_agent(&n, "pause") {
                    paused.push(n);
                }
            }
            let msg = format!(
                "team {name} reached its budget ({} of {}) — paused {}. sv agent budget <tokens> raises it and resumes them",
                agents::fmt_tokens(used),
                agents::fmt_tokens(budget),
                if paused.is_empty() {
                    "nothing".into()
                } else {
                    paused.join(", ")
                }
            );
            self.bus_event(msg.clone());
            self.notice(format!("⏸ {msg}"));
            if let Some(tr) = self.team.as_mut() {
                tr.capped = Some(paused);
            }
            self.dirty = true;
        } else if used * 10 >= budget * 8 && !tr.warned {
            self.notice(format!(
                "team {name} has used {} of its {} budget",
                agents::fmt_tokens(used),
                agents::fmt_tokens(budget)
            ));
            if let Some(tr) = self.team.as_mut() {
                tr.warned = true;
            }
        }
    }

    /// `sv agent budget [tokens]`: show, or raise and resume what the cap paused.
    fn bus_budget(&mut self, tokens: Option<u64>) -> serde_json::Value {
        use crate::bus::{err, ok};
        let Some(tr) = self.team.as_mut() else {
            return err("no team is running (sv agent run team.yaml \"goal\")");
        };
        let name = tr.team.name.clone();
        let mut resumed = Vec::new();
        if let Some(t) = tokens {
            tr.team.budget_tokens = Some(t);
            tr.warned = false;
            let paused = tr.capped.take().unwrap_or_default();
            for n in paused {
                if self.signal_agent(&n, "resume") {
                    resumed.push(n);
                }
            }
            self.bus_event(format!(
                "team {name} budget set to {}{}",
                agents::fmt_tokens(t),
                if resumed.is_empty() {
                    String::new()
                } else {
                    format!(" — resumed {}", resumed.join(", "))
                }
            ));
            self.check_budget();
            let _ = self.schedule();
        }
        let mut v = self.team_summary();
        v["resumed"] = serde_json::json!(resumed);
        ok(v)
    }

    /// pause / resume / stop every agent.
    fn bus_signal_all(&mut self, op: &str) -> serde_json::Value {
        let names: Vec<String> = self
            .panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .filter(|a| !matches!(a.status, agents::Status::Exited(_)))
            .map(|a| a.name.clone())
            .collect();
        let done: Vec<String> = names
            .into_iter()
            .filter(|n| self.signal_agent(n, op))
            .collect();
        if op == "resume" {
            if let Some(tr) = self.team.as_mut() {
                tr.capped = None;
            }
        }
        self.bus_event(format!("{op} all: {}", done.join(", ")));
        self.dirty = true;
        crate::bus::ok(serde_json::json!({ "agents": done }))
    }

    /// Something happened to an agent: log it, run the team's `on:` rules and
    /// `start: after:` members. Returns how many rules ran.
    fn fire(&mut self, agent: &str, event: &str, log: &str) -> usize {
        let log = if log.trim().is_empty() {
            self.screen_tail(agent, 12)
        } else {
            log.trim().to_string()
        };
        self.bus_event(format!("{agent}.{event}"));
        let Some(tr) = &self.team else { return 0 };
        let in_team = self
            .agent_by_name(agent)
            .is_some_and(|a| a.team.as_deref() == Some(tr.team.name.as_str()));
        if !in_team {
            return 0;
        }
        let goal = tr.goal.clone();
        let rules: Vec<(String, String)> = tr
            .team
            .rules
            .iter()
            .filter(|r| (r.agent == agent || r.agent == "*") && r.event == event)
            .map(|r| {
                (
                    r.send.clone(),
                    crate::team::fill(&r.text, agent, event, &log, &goal),
                )
            })
            .collect();
        if event == "done" && tr.team.member(agent).is_some() && !tr.done.iter().any(|d| d == agent)
        {
            if let Some(tr) = self.team.as_mut() {
                tr.done.push(agent.to_string());
            }
            let _ = self.schedule();
        }
        let n = rules.len();
        let from = format!("{agent}.{event}");
        for (to, text) in rules {
            let running = self.agent_pane(&to).is_some();
            let res = if running {
                self.send_to_agent(&to, &text, &from)
            } else {
                self.start_member(&to, None, &text).map(|_| ())
            };
            if let Err(e) = res {
                self.bus_event(format!("{from} → {to} failed: {e}"));
            }
        }
        n
    }

    /// The last non-empty lines of an agent's screen, as one line.
    fn screen_tail(&self, agent: &str, n: usize) -> String {
        let Some(p) = self.agent_pane(agent).and_then(|id| self.panes.get(&id)) else {
            return String::new();
        };
        let lines: Vec<String> = p
            .pane
            .screen()
            .rows(0, p.size.1)
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .collect();
        let tail = lines[lines.len().saturating_sub(n)..].join(" | ");
        tail.chars().take(1000).collect()
    }

    /// Turn status changes into events (started, done, failed, exited,
    /// needs_approval) and end a team when all its agents have exited.
    fn agent_events(&mut self) {
        let mut events: Vec<(String, &'static str)> = Vec::new();
        for p in self.panes.values_mut() {
            let Some(a) = p.agent.as_mut() else { continue };
            let key: &'static str = match &a.status {
                agents::Status::Exited(Some(0)) | agents::Status::Exited(None) => "exited",
                agents::Status::Exited(_) => "failed",
                _ if a.paused => "paused",
                agents::Status::Starting => "starting",
                agents::Status::Working => "working",
                agents::Status::Idle if a.worked => "done",
                agents::Status::Idle => "idle",
                agents::Status::NeedsApproval(_) => "approval",
            };
            if key == a.reported {
                continue;
            }
            let old = std::mem::replace(&mut a.reported, key);
            a.history.push((Instant::now(), key));
            if old.is_empty() {
                events.push((a.name.clone(), "started"));
            }
            match key {
                "done" => events.push((a.name.clone(), "done")),
                "approval" => events.push((a.name.clone(), "needs_approval")),
                "exited" => {
                    if old != "done" {
                        events.push((a.name.clone(), "done"));
                    }
                    events.push((a.name.clone(), "exited"));
                }
                "failed" => {
                    events.push((a.name.clone(), "failed"));
                    events.push((a.name.clone(), "exited"));
                }
                _ => {}
            }
        }
        for (name, ev) in events {
            self.fire(&name, ev, "");
        }
        if self.team.is_some() {
            let _ = self.schedule(); // slots freed by agents that exited
        }
        if let Some(tr) = &self.team {
            let name = tr.team.name.clone();
            let all = self.team_agents(&name);
            // a member that can still start: queued, held for the next
            // step, or after agents that are all done or still running
            let waiting = tr.team.agents.iter().any(|m| {
                !tr.started.contains(&m.id)
                    && match &m.start {
                        crate::team::Start::Now => true,
                        crate::team::Start::After(deps) => deps
                            .iter()
                            .all(|d| tr.done.contains(d) || all.iter().any(|(n, r)| n == d && *r)),
                        crate::team::Start::Spawned => false,
                    }
            });
            if !all.is_empty() && all.iter().all(|(_, running)| !running) && !waiting {
                let used = self.team_tokens(&name);
                self.bus_event(format!(
                    "team {name} finished{}",
                    if used > 0 {
                        format!(" · {}", agents::fmt_tokens(used))
                    } else {
                        String::new()
                    }
                ));
                self.notice(format!("✔ team {name} finished"));
                self.team = None;
            }
        }
    }

    // ------------------------------------------------------------ agent board

    /// The planner command, and the agent that plans (and later reviews).
    fn planner(&self) -> Option<(String, String)> {
        if let Some(cmd) = self.opts.planner.clone().filter(|c| !c.trim().is_empty()) {
            let who = cmd
                .split_whitespace()
                .next()
                .unwrap_or("planner")
                .to_string();
            let who = if Cli::parse(&who).is_some() {
                who
            } else {
                String::new()
            };
            return Some((cmd, who));
        }
        agents::detect().into_iter().find_map(|(c, _)| match c {
            Cli::Claude => Some(("claude -p".to_string(), "claude".to_string())),
            Cli::Codex => Some(("codex exec -".into(), "codex".into())),
            Cli::Gemini => Some(("gemini".into(), "gemini".into())),
            _ => None,
        })
    }

    fn open_board(
        &mut self,
        view: Option<crate::board::View>,
        goal: Option<(String, Option<PathBuf>)>,
    ) {
        use crate::board::{Board, Phase, Plan, View};
        let folder = goal
            .as_ref()
            .and_then(|(_, f)| f.clone())
            .or_else(|| self.focused_cwd())
            .unwrap_or_default();
        let mut plan = self.plan_kept.take().unwrap_or_else(|| {
            let (planner, who) = self.planner().unwrap_or_default();
            let clis: Vec<String> = agents::detect()
                .into_iter()
                .filter(|(c, _)| *c != Cli::Generic)
                .map(|(c, _)| c.name().to_string())
                .collect();
            Plan {
                goal: String::new(),
                phase: Phase::Goal,
                tasks: Vec::new(),
                sel: 0,
                edit: None,
                parallel: 3,
                budget: None,
                clis,
                planner: if planner.is_empty() {
                    "no planner".into()
                } else {
                    planner
                },
                reviewer: who,
                folder: folder.clone(),
                step: false,
                run: 0,
            }
        });
        let start = goal.is_some();
        if let Some((g, _)) = goal {
            plan.goal = g;
            plan.folder = folder;
            plan.tasks.clear();
            plan.phase = Phase::Goal;
        }
        let view = view.unwrap_or(match (&plan.phase, self.agent_ids().is_empty()) {
            (Phase::Dispatched(_), _) | (_, true) => View::Plan,
            _ => View::Tree,
        });
        self.board = Some(Board::new(view, plan));
        self.dirty = true;
        if start {
            self.start_planner();
        }
    }

    fn close_board(&mut self) {
        if let Some(b) = self.board.take() {
            self.plan_kept = Some(b.plan);
        }
        self.dirty = true;
    }

    fn board_key(&mut self, key: &[u8]) {
        let snap = self.board_snapshot();
        let Some(b) = self.board.as_mut() else { return };
        if key == b"\x1bg" && !b.typing() {
            self.close_board(); // alt+g again
            return;
        }
        let (redraw, act) = b.key(key, &snap);
        if redraw {
            self.dirty = true;
        }
        use crate::board::BoardAct;
        match act {
            None => {}
            Some(BoardAct::Close) => self.close_board(),
            Some(BoardAct::Open(name)) => {
                if let Some(id) = self.agent_pane(&name) {
                    self.close_board();
                    self.focus_pane(id);
                }
            }
            Some(BoardAct::Signal(op, name)) => {
                let v = self.bus_signal(op, &name, true);
                if let Some(e) = v["error"].as_str() {
                    self.notice(e.to_string());
                }
            }
            Some(BoardAct::Merge(name)) => self.merge_agent(&name),
            Some(BoardAct::Plan) => self.start_planner(),
            Some(BoardAct::Dispatch) => self.dispatch_plan(),
            Some(BoardAct::NextStage) => {
                let msg = match self.team.as_mut() {
                    Some(tr) if tr.step.is_some() => {
                        let last = tr.team.agents.iter().map(|m| m.stage).max().unwrap_or(0);
                        let s = (tr.step.unwrap_or(0) + 1).min(last);
                        tr.step = Some(s);
                        format!("stage {s} released")
                    }
                    _ => "the plan isn't running step by step".into(),
                };
                self.bus_event(msg.clone());
                self.notice(msg);
                let _ = self.schedule();
            }
        }
    }

    /// Run the planner in the background; its answer comes back as Ev::Plan.
    fn start_planner(&mut self) {
        let Some((cmd, _)) = self.planner() else {
            if let Some(b) = self.board.as_mut() {
                b.plan.phase = crate::board::Phase::Failed(
                    "no planner: install Claude Code, Codex or Gemini CLI, or set agents.planner in config.yaml".into(),
                );
            }
            return;
        };
        let Some(b) = self.board.as_mut() else { return };
        let p = &mut b.plan;
        p.run += 1;
        p.phase = crate::board::Phase::Planning(Instant::now());
        let run = p.run;
        let prompt = crate::board::planner_prompt(&p.goal, &p.clis);
        let folder = p.folder.clone();
        let tx = self.tx.clone();
        self.dirty = true;
        let _ = spawn_thread("planner".into(), move || {
            let out = run_planner(&cmd, &prompt, &folder);
            let _ = tx.send(Ev::Plan(run, out));
        });
    }

    fn on_plan(&mut self, run: u64, out: Result<String, String>) {
        let default = self
            .board
            .as_ref()
            .map(|b| &b.plan)
            .or(self.plan_kept.as_ref())
            .map(|p| p.reviewer.clone())
            .filter(|r| !r.is_empty())
            .or_else(|| agents::detect().first().map(|(c, _)| c.name().to_string()))
            .unwrap_or_else(|| "claude".into());
        let plan = match (self.board.as_mut(), self.plan_kept.as_mut()) {
            (Some(b), _) => &mut b.plan,
            (None, Some(p)) => p,
            _ => return,
        };
        if plan.run != run {
            return;
        }
        use crate::board::Phase;
        plan.phase = match out.and_then(|text| crate::board::parse_plan(&text, &default)) {
            Ok(tasks) => {
                // a command planner (no CLI name): its tasks name the agents, and
                // the first task's agent reviews
                if plan.reviewer.is_empty() {
                    plan.reviewer = tasks.first().map(|t| t.agent.clone()).unwrap_or_default();
                }
                plan.tasks = tasks;
                plan.sel = 0;
                Phase::Ready
            }
            Err(e) => Phase::Failed(e),
        };
        self.dirty = true;
    }

    fn dispatch_plan(&mut self) {
        let Some(b) = self.board.as_ref() else { return };
        let p = b.plan.clone();
        if let Some(tr) = &self.team {
            let name = tr.team.name.clone();
            if self.team_agents(&name).iter().any(|(_, r)| *r) {
                self.notice(format!(
                    "team {name} is still running — stop it first (sv agent stop --all)"
                ));
                return;
            }
        }
        let name = format!("plan-{}", self.agents_made + 1);
        let reviewer = if p.reviewer.is_empty() {
            p.tasks.first().map(|t| t.agent.clone()).unwrap_or_default()
        } else {
            p.reviewer.clone()
        };
        let team = crate::board::plan_to_team(&name, &p.tasks, &p.goal, &reviewer, p.budget);
        let step = p.step.then_some(1);
        match self.begin_team(
            team,
            p.goal.clone(),
            p.folder.clone(),
            Some(p.parallel),
            step,
        ) {
            Ok((tname, started)) => {
                if let Some(b) = self.board.as_mut() {
                    b.plan.phase = crate::board::Phase::Dispatched(tname.clone());
                }
                self.notice(format!("team {tname} started: {}", started.join(", ")));
            }
            Err(e) => self.notice(format!("dispatch: {e}")),
        }
        self.dirty = true;
    }

    /// Merge an agent's branch into the checkout it came from.
    fn merge_agent(&mut self, name: &str) {
        let Some(a) = self.agent_by_name(name) else {
            return;
        };
        let (Some(repo), Some(branch)) = (a.repo.clone(), a.branch.clone()) else {
            self.notice(format!("{name} has no branch of its own"));
            return;
        };
        let tx = self.tx.clone();
        self.bus_event(format!("merging {branch}"));
        let _ = spawn_thread("merge".into(), move || {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["merge", "--no-ff", "--no-edit", &branch])
                .output();
            let msg = match out {
                Ok(o) if o.status.success() => format!("✔ merged {branch}"),
                Ok(o) => {
                    let e = String::from_utf8_lossy(&o.stdout).to_string()
                        + &String::from_utf8_lossy(&o.stderr);
                    let line = e
                        .lines()
                        .find(|l| {
                            l.contains("CONFLICT") || l.contains("error") || l.contains("fatal")
                        })
                        .unwrap_or("git merge failed");
                    format!("✖ merge {branch}: {line} — finish it in your shell (git status)")
                }
                Err(e) => format!("✖ merge {branch}: {e}"),
            };
            let _ = tx.send(Ev::Notice(msg));
        });
    }

    /// What the board shows.
    pub fn board_snapshot(&self) -> crate::board::Snap {
        use crate::board::{Node, Snap, TeamInfo};
        let now = Instant::now();
        let nodes = self
            .agent_ids()
            .into_iter()
            .filter_map(|id| {
                let p = self.panes.get(&id)?;
                let a = p.agent.as_ref()?;
                let v = self.agent_summary(id);
                let orchestrator = self
                    .team
                    .as_ref()
                    .and_then(|t| t.team.member(&a.name))
                    .is_some_and(|m| m.orchestrator);
                let tail: Vec<String> = p
                    .pane
                    .screen()
                    .rows(0, p.size.1)
                    .map(|r| r.trim_end().to_string())
                    .filter(|r| !r.trim().is_empty())
                    .collect();
                let tail = tail[tail.len().saturating_sub(12)..].to_vec();
                Some(Node {
                    name: a.name.clone(),
                    parent: a.parent.clone(),
                    depth: a.depth,
                    // a command agent: its command, if the team says what it is
                    cli: match self.team.as_ref().and_then(|t| t.team.member(&a.name)) {
                        Some(m) if a.cli == Cli::Generic && !m.cmd.is_empty() => m.cmd.clone(),
                        _ => a.cli.name().to_string(),
                    },
                    status: v["status"].as_str().unwrap_or("").to_string(),
                    tokens: a.tokens,
                    task: a.task.clone(),
                    branch: a.branch.clone(),
                    files: a.files.len(),
                    added: a.files.iter().map(|f| f.added).sum(),
                    removed: a.files.iter().map(|f| f.removed).sum(),
                    helpers: a.internal.clone(),
                    orchestrator,
                    color: a.color,
                    history: a
                        .history
                        .iter()
                        .map(|(t, k)| (t.saturating_duration_since(a.started).as_secs_f64(), *k))
                        .collect(),
                    age: now.duration_since(a.started).as_secs_f64(),
                    tail,
                })
            })
            .collect();
        let team = self.team.as_ref().map(|t| TeamInfo {
            name: t.team.name.clone(),
            used: self.team_tokens(&t.team.name),
            budget: t.team.budget_tokens,
            capped: t.capped.is_some(),
            step: t.step,
            stages: t.team.agents.iter().map(|m| m.stage).max().unwrap_or(0),
        });
        let l = self.opts.agent_limits;
        Snap {
            nodes,
            team,
            log: self.bus_log.iter().cloned().collect(),
            max_depth: self
                .team
                .as_ref()
                .and_then(|t| t.team.max_depth)
                .unwrap_or(l.max_depth),
            max_agents: self
                .team
                .as_ref()
                .and_then(|t| t.team.max_agents)
                .unwrap_or(l.max_agents),
            max_children: l.max_children,
        }
    }

    fn bus_spawn(&mut self, req: &serde_json::Value, caller: Option<String>) -> serde_json::Value {
        use crate::bus::{err, ok};
        let field = |k: &str| req[k].as_str().unwrap_or("").trim().to_string();
        let active: Vec<(String, Option<String>, u8)> = self
            .panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .filter(|a| !matches!(a.status, agents::Status::Exited(_)))
            .map(|a| (a.name.clone(), a.parent.clone(), a.depth))
            .collect();
        // Sub-agent when an agent asks; `--top` from an agent starts a sibling tree.
        let parent = caller.filter(|_| !req["top"].as_bool().unwrap_or(false));
        let name = field("name");
        let limits = match self.spawn_limits(parent.as_deref(), &name) {
            Ok(l) => l,
            Err(why) => return self.refuse_spawn(parent.as_deref(), &name, why),
        };
        let depth = match agents::check_spawn(limits, &active, parent.as_deref()) {
            Ok(d) => d,
            Err(why) => return self.refuse_spawn(parent.as_deref(), &name, why),
        };
        // A team member (team.yaml) starts as the file describes it.
        if self
            .team
            .as_ref()
            .is_some_and(|t| t.team.member(&name).is_some())
        {
            let info = parent.clone().map(|p| (p, depth));
            return match self.start_member(&name, info, &field("task")) {
                Ok(n) => ok(serde_json::json!({ "name": n, "parent": parent, "depth": depth })),
                Err(e) => err(e),
            };
        }
        let cmd = field("cmd");
        let cli = if !cmd.is_empty() {
            Cli::Generic
        } else if let Some(c) = Cli::parse(&field("cli")) {
            c
        } else if field("cli").is_empty() {
            // same CLI as the parent, else the first one installed
            parent
                .as_deref()
                .and_then(|p| self.agent_by_name(p).map(|a| a.cli))
                .or_else(|| agents::detect().first().map(|(c, _)| *c))
                .unwrap_or(Cli::Claude)
        } else {
            return err(format!(
                "unknown agent CLI `{}` — claude, codex, gemini, aider, or --cmd",
                field("cli")
            ));
        };
        // Children share their parent's folder (worktree); top-level agents get their own.
        let (folder, worktree) = match parent.as_deref().and_then(|p| self.agent_pane(p)) {
            Some(pid) => (Some(PathBuf::from(&self.panes[&pid].cwd)), false),
            None => {
                let folder = Some(PathBuf::from(field("folder"))).filter(|p| p.is_dir());
                let want = req["worktree"]
                    .as_bool()
                    .unwrap_or(self.opts.agent_worktree);
                let in_repo = folder
                    .clone()
                    .or_else(|| self.focused_cwd())
                    .as_deref()
                    .and_then(agents::repo_head)
                    .is_some();
                (folder, want && in_repo)
            }
        };
        let parent_info = parent.clone().map(|p| (p, depth));
        match self.start_agent(
            parent_info,
            cli,
            &field("name"),
            &field("task"),
            &cmd,
            worktree,
            folder,
        ) {
            Ok(name) => ok(serde_json::json!({ "name": name, "parent": parent, "depth": depth })),
            Err(e) => err(e),
        }
    }

    /// pause / resume / stop an agent (and with `tree`, its sub-agents).
    fn bus_signal(&mut self, op: &str, name: &str, tree: bool) -> serde_json::Value {
        use crate::bus::{err, ok};
        if self.agent_pane(name).is_none() {
            return err(format!("no agent called {name}"));
        }
        let mut targets = vec![name.to_string()];
        if tree {
            let all: Vec<(String, Option<String>)> = self
                .panes
                .values()
                .filter_map(|p| p.agent.as_ref())
                .map(|a| (a.name.clone(), a.parent.clone()))
                .collect();
            targets.extend(agents::descendants(&all, name));
        }
        let mut done = Vec::new();
        for t in &targets {
            if self.signal_agent(t, op) {
                done.push(t.clone());
            }
        }
        self.bus_event(format!("{op} {}", done.join(", ")));
        self.dirty = true;
        ok(serde_json::json!({ "agents": done }))
    }

    /// Send SIGSTOP / SIGCONT / SIGTERM to the agent CLI (its process group).
    fn signal_agent(&mut self, name: &str, op: &str) -> bool {
        let Some(id) = self.agent_pane(name) else {
            return false;
        };
        let Some(p) = self.panes.get_mut(&id) else {
            return false;
        };
        let Some(a) = p.agent.as_mut() else {
            return false;
        };
        if matches!(a.status, agents::Status::Exited(_)) {
            return false;
        }
        let Some(pg) = a.paused_pg.or_else(|| p.pane.foreground_job()) else {
            return false;
        };
        match op {
            "pause" => {
                if !a.paused {
                    unsafe { libc::kill(-pg, libc::SIGSTOP) };
                    a.paused = true;
                    a.paused_pg = Some(pg);
                }
            }
            "resume" => {
                if a.paused {
                    unsafe { libc::kill(-pg, libc::SIGCONT) };
                    a.paused = false;
                    a.paused_pg = None;
                    a.last_output = Instant::now();
                    // The shell noticed the stop and took the terminal back:
                    // `fg` hands it to the agent again.
                    if p.pane.foreground_job().is_none() {
                        let _ = p.pane.write(b"fg\r");
                    }
                }
            }
            _ => {
                unsafe {
                    libc::kill(-pg, libc::SIGCONT);
                    libc::kill(-pg, libc::SIGTERM);
                }
                if a.paused {
                    // its command-end was already seen (and ignored) at the pause
                    a.status = agents::Status::Exited(Some(128 + libc::SIGTERM));
                }
                a.paused = false;
                a.paused_pg = None;
            }
        }
        true
    }

    /// Answer `sv agent wait` callers whose agent has finished.
    fn check_waiters(&mut self) {
        if self.waiters.is_empty() {
            return;
        }
        let waiters = std::mem::take(&mut self.waiters);
        for (name, reply) in waiters {
            match self.agent_pane(&name) {
                None => {
                    let _ = reply.send(crate::bus::err(format!("agent {name} is gone")));
                }
                Some(id) => {
                    let done = self
                        .panes
                        .get(&id)
                        .and_then(|p| p.agent.as_ref())
                        .is_some_and(Self::agent_finished);
                    if done {
                        let _ = reply.send(crate::bus::ok(
                            serde_json::json!({ "agent": self.agent_summary(id) }),
                        ));
                    } else {
                        self.waiters.push((name, reply));
                    }
                }
            }
        }
    }

    pub fn pending_approvals(&self) -> Vec<PaneId> {
        let mut v: Vec<(Instant, PaneId)> = self
            .panes
            .iter()
            .filter_map(|(id, p)| p.agent.as_ref()?.approval().map(|a| (a.since, *id)))
            .collect();
        v.sort();
        v.into_iter().map(|(_, id)| id).collect()
    }

    fn focus_pane(&mut self, id: PaneId) {
        if let Some(t) = self.tab_of(id) {
            self.switch_tab(t);
            self.tabs[t].focus = id;
            if self.view == View::Basic {
                self.view = View::Full;
            }
            self.relayout();
        }
    }

    fn toggle_agents(&mut self) {
        match self.agents_tab() {
            Some(t) if t == self.active => {
                let back = self
                    .before_agents
                    .take()
                    .filter(|b| *b < self.tabs.len() && *b != t)
                    .unwrap_or(0);
                self.switch_tab(back);
            }
            Some(t) => {
                self.before_agents = Some(self.active);
                if self.view == View::Basic {
                    self.view = View::Full;
                }
                self.switch_tab(t);
                self.relayout();
            }
            None => self.open_new_agent(),
        }
    }

    fn open_new_agent(&mut self) {
        let mut clis: Vec<Cli> = agents::detect().into_iter().map(|(c, _)| c).collect();
        let none = clis.is_empty();
        clis.push(Cli::Generic);
        let folder = self
            .focused_cwd()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let in_repo = agents::repo_head(&folder).is_some();
        let mut form = NewAgentForm {
            clis,
            cli: 0,
            name: String::new(),
            prompt: String::new(),
            custom: String::new(),
            worktree: self.opts.agent_worktree && in_repo,
            field: 2,
            error: none.then(|| "no agent CLI found on PATH (claude, codex, gemini, aider) — run any command as a custom agent".to_string()),
            folder: ui::short_path(&folder.to_string_lossy()),
        };
        form.name = self.default_agent_name(form.cli());
        self.modal = Modal::NewAgent(Box::new(form));
    }

    fn default_agent_name(&self, cli: Cli) -> String {
        let base = match cli {
            Cli::Generic => "agent",
            c => c.name(),
        };
        let taken: Vec<String> = self
            .panes
            .values()
            .filter_map(|p| p.agent.as_ref().map(|a| a.name.clone()))
            .collect();
        if !taken.iter().any(|t| t == base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|n| !taken.contains(n))
            .unwrap()
    }

    fn new_agent_key(&mut self, mut f: NewAgentForm, key: &[u8]) {
        match key {
            b"\x1b" | [0x03] => {
                self.modal = Modal::None;
                return;
            }
            b"\t" | b"\x1b[B" | b"\x1bOB" => f.field = (f.field + 1) % 4,
            b"\x1b[Z" | b"\x1b[A" | b"\x1bOA" => f.field = (f.field + 3) % 4,
            b"\x1b[C" | b"\x1bOC" | b"\x1b[D" | b"\x1bOD" if f.field == 0 => {
                let n = f.clis.len();
                let old_default = self.default_agent_name(f.cli());
                f.cli = if key.ends_with(b"C") {
                    (f.cli + 1) % n
                } else {
                    (f.cli + n - 1) % n
                };
                if f.name == old_default || f.name.is_empty() {
                    f.name = self.default_agent_name(f.cli());
                }
            }
            b" " | b"\x1b[C" | b"\x1b[D" if f.field == 3 => f.worktree = !f.worktree,
            b"\r" => {
                let (cli, name, prompt, custom, wt) = (
                    f.cli(),
                    f.name.trim().to_string(),
                    f.prompt.clone(),
                    f.custom.clone(),
                    f.worktree,
                );
                match self.start_agent(None, cli, &name, &prompt, &custom, wt, None) {
                    Ok(_) => {
                        self.modal = Modal::None;
                        return;
                    }
                    Err(e) => f.error = Some(e),
                }
            }
            [0x7f] | [0x08] => {
                if let Some(t) = f.text_field() {
                    t.pop();
                }
            }
            [0x15] => {
                if let Some(t) = f.text_field() {
                    t.clear();
                }
            }
            k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                if let Some(t) = f.text_field() {
                    if t.chars().count() < 2000 {
                        t.push_str(&String::from_utf8_lossy(k));
                    }
                }
            }
            _ => {}
        }
        self.modal = Modal::NewAgent(Box::new(f));
    }

    /// Start an agent CLI in a new shell (in its own worktree when asked)
    /// and put it in the agents tab.
    #[allow(clippy::too_many_arguments)] // one call per way an agent can start
    pub fn start_agent(
        &mut self,
        parent: Option<(String, u8)>,
        cli: Cli,
        name: &str,
        prompt: &str,
        custom: &str,
        worktree: bool,
        folder: Option<PathBuf>,
    ) -> Result<String, String> {
        let name = if name.is_empty() {
            self.default_agent_name(cli)
        } else {
            name.to_string()
        };
        if !agents::valid_name(&name) {
            return Err(format!("name `{name}`: use letters, digits, - and _"));
        }
        if self
            .panes
            .values()
            .any(|p| p.agent.as_ref().is_some_and(|a| a.name == name))
        {
            return Err(format!("there is already an agent called {name}"));
        }
        if cli == Cli::Generic && custom.trim().is_empty() {
            return Err("type the command that starts your agent".into());
        }
        let here = folder
            .or_else(|| self.focused_cwd())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let mut agent = Agent::new(name.clone(), cli, self.agents_made);
        agent.task = prompt.trim().to_string();
        if let Some((p, depth)) = &parent {
            agent.parent = Some(p.clone());
            agent.depth = *depth;
            // a sub-agent inherits its parent's "always allow" answers
            if let Some(pa) = self.agent_by_name(p) {
                agent.always = pa.always.clone();
                agent.team = pa.team.clone();
            }
        }
        let cwd = if worktree {
            let wt = agents::create_worktree(&here, &name)?;
            agent.repo = Some(wt.repo);
            agent.branch = Some(wt.branch);
            agent.base = Some(wt.base);
            agent.worktree = Some(wt.path.clone());
            wt.path
        } else {
            if let Some((repo, head)) = agents::repo_head(&here) {
                agent.repo = Some(repo);
                agent.base = Some(head);
            }
            here
        };
        let id = self
            .new_pane_env(
                Some(cwd),
                vec![
                    (crate::bus::ENV_AGENT.to_string(), name.clone()),
                    (crate::bus::ENV_TASK.to_string(), prompt.trim().to_string()),
                ],
            )
            .map_err(|e| e.to_string())?;
        self.agents_made += 1;
        let line = cli.launch(prompt, custom);
        if let Some(p) = self.panes.get_mut(&id) {
            p.agent = Some(agent);
            p.pending_input = Some((format!("{line}\r"), Instant::now()));
        }
        self.place_in_agents_tab(id);
        self.notice(format!(
            "agent {name} started · F9 agents · alt+a approvals"
        ));
        self.bus_event(match &parent {
            Some((p, d)) => format!("{p} spawned {name} (depth {d})"),
            None => format!("{name} started ({})", cli.name()),
        });
        Ok(name)
    }

    /// Add a pane to the agents tab (creating it), as one more column.
    fn place_in_agents_tab(&mut self, id: PaneId) {
        let t = match self
            .agents_tab()
            .filter(|t| !self.tabs[*t].root.contains(id))
        {
            Some(t) => {
                let mut panes = self.tabs[t].root.panes();
                panes.push(id);
                let how = if panes.len() <= 4 {
                    Arrangement::Columns
                } else {
                    Arrangement::Grid
                };
                self.tabs[t].root = arrange(&panes, how);
                t
            }
            None => {
                let mut tab = Tab::new(id);
                tab.name = Some("agents".into());
                self.tabs.push(tab);
                self.tabs.len() - 1
            }
        };
        if self.active != t {
            self.before_agents = Some(self.active);
        }
        self.tabs[t].focus = id;
        self.tabs[t].zoomed = false;
        if self.view == View::Basic {
            self.view = View::Full;
        }
        self.switch_tab(t);
        self.relayout();
    }

    /// The focused split runs an agent CLI: make it an agent column.
    fn attach_agent(&mut self) {
        let id = self.input_target();
        let Some(p) = self.panes.get(&id) else { return };
        if p.agent.is_some() {
            self.notice("this split is already an agent column");
            return;
        }
        if !p.running {
            self.notice("start the agent in this split first (e.g. claude), then attach it");
            return;
        }
        let cli = Cli::of_command(&p.command);
        let name = self.default_agent_name(cli);
        let mut agent = Agent::new(name.clone(), cli, self.agents_made);
        agent.launched = false;
        agent.status = agents::Status::Working;
        if let Some((repo, head)) = agents::repo_head(Path::new(&p.cwd)) {
            agent.repo = Some(repo);
            agent.base = Some(head);
        }
        self.agents_made += 1;
        // take it out of its tab (without stopping it) and into the agents tab
        let target = self.agents_tab();
        if let Some(t) = self.tab_of(id) {
            if target != Some(t) {
                match self.tabs[t].root.clone().remove(id) {
                    Some(root) => {
                        if self.tabs[t].focus == id {
                            self.tabs[t].focus = root.panes()[0];
                        }
                        self.tabs[t].root = root;
                    }
                    None => {
                        // it was the tab's only pane: the tab becomes the agents tab
                        self.tabs[t].name = Some("agents".into());
                    }
                }
            }
        }
        if let Some(p) = self.panes.get_mut(&id) {
            p.agent = Some(agent);
        }
        self.place_in_agents_tab(id);
        self.notice(format!("{name} is now an agent column"));
    }

    /// Answer an agent's approval prompt with its CLI's own keys.
    fn answer(&mut self, id: PaneId, ans: Answer) {
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(a) = p.agent.as_mut() else { return };
        let Some(ap) = a.approval().cloned() else {
            return;
        };
        let k = a.cli.keys();
        let keys = match ans {
            Answer::Allow => k.allow,
            Answer::Always => {
                if !a.always.contains(&ap.action) {
                    a.always.push(ap.action);
                }
                k.always
            }
            Answer::Deny => k.deny,
        };
        let _ = p.pane.write(keys.as_bytes());
        a.status = agents::Status::Working;
        p.agent_quiet_until = Instant::now() + Duration::from_millis(800);
        self.dirty = true;
    }

    /// Look at an agent's screen for an approval prompt (≤ 5×/s).
    fn check_agent(&mut self, id: PaneId) {
        let auto = self.opts.auto_approve.clone();
        let Some(p) = self.panes.get_mut(&id) else {
            return;
        };
        if p.agent.is_none()
            || !p.agent_check_due
            || p.agent_check_at.elapsed() < Duration::from_millis(200)
        {
            return;
        }
        if Instant::now() < p.agent_quiet_until {
            return;
        }
        p.agent_check_due = false;
        p.agent_check_at = Instant::now();
        let cols = p.size.1;
        let rows: Vec<String> = p.pane.screen().rows(0, cols).collect();
        let a = p.agent.as_mut().unwrap();
        let found = if p.running {
            agents::detect_approval(a.cli, &rows)
        } else {
            None
        };
        let mut notice = None;
        match found {
            Some(ap) => {
                let same = a.approval().is_some_and(|old| old.what == ap.what);
                if !same {
                    if a.always.contains(&ap.action) || auto.contains(&ap.action) {
                        let _ = p.pane.write(a.cli.keys().allow.as_bytes());
                        p.agent_quiet_until = Instant::now() + Duration::from_millis(800);
                        notice = Some(format!(
                            "auto-approved {} for {}: {}",
                            ap.action.name(),
                            a.name,
                            ap.what
                        ));
                        a.status = agents::Status::Working;
                    } else {
                        notice = Some(format!(
                            "⚑ {} asks to {}: {} — alt+a",
                            a.name,
                            ap.action.name(),
                            ap.what
                        ));
                        a.status = agents::Status::NeedsApproval(ap);
                    }
                    self.dirty = true;
                }
            }
            None => {
                if a.approval().is_some() {
                    a.status = agents::Status::Working;
                    self.dirty = true;
                }
            }
        }
        if let Some(n) = notice {
            self.notice(n);
        }
    }

    /// state/agents.json for `sv agents` — rewritten when something changed.
    fn write_agents_json(&mut self) {
        let now = crate::data::value::now();
        let rows: Vec<serde_json::Value> = self
            .agent_ids()
            .iter()
            .filter_map(|id| {
                let p = self.panes.get(id)?;
                let a = p.agent.as_ref()?;
                let add: u64 = a.files.iter().map(|f| f.added).sum();
                let del: u64 = a.files.iter().map(|f| f.removed).sum();
                Some(serde_json::json!({
                    "name": a.name,
                    "cli": a.cli.name(),
                    "status": if a.paused { "paused".to_string() } else { a.status.label().trim_start_matches(|c: char| !c.is_alphanumeric()).to_string() },
                    "parent": a.parent.clone().unwrap_or_default(),
                    "depth": a.depth,
                    "branch": a.branch.clone().unwrap_or_default(),
                    "files": a.files.len(),
                    "added": add,
                    "removed": del,
                    "tokens": a.tokens,
                    "waiting": a.approval().map(|x| x.what.clone()).unwrap_or_default(),
                    "started": crate::data::value::iso_date(now - a.started.elapsed().as_secs() as i64),
                    "folder": p.cwd,
                }))
            })
            .collect();
        let text = serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into());
        if text != self.agents_json {
            if let Some(dir) = self.opts.session_path.parent() {
                let _ = std::fs::create_dir_all(dir);
                let tmp = dir.join("agents.json.tmp");
                if std::fs::write(&tmp, &text).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join("agents.json"));
                }
            }
            self.agents_json = text;
        }
    }

    /// Mark files agents changed in the files panes ("◆ api").
    fn update_agent_badges(&mut self) {
        let mut marks: HashMap<PathBuf, (String, u8)> = HashMap::new();
        for p in self.panes.values() {
            let Some(a) = p.agent.as_ref() else { continue };
            let Some(repo) = a.repo.as_ref() else {
                continue;
            };
            for f in &a.files {
                marks
                    .entry(repo.join(&f.path))
                    .or_insert((a.name.clone(), a.color));
            }
        }
        for t in self.tools.values_mut() {
            if let Tool::Files(fp) = t {
                if fp.agent_marks != marks {
                    fp.agent_marks = marks.clone();
                    self.dirty = true;
                }
            }
        }
    }

    /// Idle detection, trailing approval checks, and git change polling.
    fn tick_agents(&mut self) {
        // typed at a new shell's first prompt (sessions, duplicates, agents);
        // a shell without our hooks never says "prompt": type it anyway
        for p in self.panes.values_mut() {
            if p.pending_input
                .as_ref()
                .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2))
            {
                let (text, _) = p.pending_input.take().unwrap();
                let _ = p.pane.write(text.as_bytes());
            }
        }
        self.check_waiters();
        // an agent that stopped working isn't installing anything any more
        let mut cleared = false;
        for p in self.panes.values_mut() {
            let stale = p.agent.as_ref().is_none_or(|a| {
                !matches!(a.status, agents::Status::Working)
                    || a.paused
                    || a.last_output.elapsed() > Duration::from_secs(30)
            });
            if p.agent_act.is_some() && stale {
                p.agent_act = None;
                cleared = true;
            }
        }
        if cleared {
            self.update_loading_marks();
            self.dirty = true;
        }
        let ids = self.agent_ids();
        if ids.is_empty() {
            if !self.agents_json.is_empty() && self.agents_json != "[]" {
                self.write_agents_json();
            }
            return;
        }
        self.write_agents_json();
        for id in &ids {
            if let Some(p) = self.panes.get_mut(id) {
                // a shell without our hooks never says "prompt": type it anyway
                if p.pending_input
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(2))
                {
                    let (text, _) = p.pending_input.take().unwrap();
                    let _ = p.pane.write(text.as_bytes());
                }
                if let Some(a) = p.agent.as_mut() {
                    if matches!(a.status, agents::Status::Working) {
                        a.worked = true;
                        if a.last_output.elapsed() > agents::IDLE_AFTER {
                            a.status = agents::Status::Idle;
                            self.dirty = true;
                        }
                    }
                }
            }
            self.check_agent(*id);
        }
        self.agent_events();
        let every = if self.low_power {
            Duration::from_secs(15)
        } else {
            Duration::from_secs(5)
        };
        if self.agent_files_busy || self.agent_files_at.elapsed() < every {
            return;
        }
        let work: Vec<(PaneId, PathBuf, Option<String>, Cli, std::time::SystemTime)> = ids
            .iter()
            .filter_map(|id| {
                let p = self.panes.get(id)?;
                let a = p.agent.as_ref()?;
                let dir = a
                    .worktree
                    .clone()
                    .or_else(|| a.repo.clone())
                    .unwrap_or_else(|| PathBuf::from(&p.cwd));
                Some((*id, dir, a.base.clone(), a.cli, a.started_at))
            })
            .collect();
        self.agent_files_at = Instant::now();
        if work.is_empty() {
            return;
        }
        self.agent_files_busy = true;
        let tx = self.tx.clone();
        let _ = spawn_thread("agent-git".into(), move || {
            let out = work
                .into_iter()
                .map(|(id, dir, base, cli, since)| {
                    let files = base.map(|b| agents::changes(&dir, &b)).unwrap_or_default();
                    let (tokens, helpers) = agents::usage(cli, &dir, since);
                    (id, files, tokens, helpers)
                })
                .collect();
            let _ = tx.send(Ev::AgentFiles(out));
        });
    }

    fn on_agent_files(&mut self, list: Vec<AgentReport>) {
        self.agent_files_busy = false;
        for (id, files, tokens, helpers) in list {
            if let Some(a) = self.panes.get_mut(&id).and_then(|p| p.agent.as_mut()) {
                if a.internal != helpers {
                    a.internal = helpers;
                    self.dirty = true;
                }
                if tokens.is_some() && a.tokens != tokens {
                    a.tokens = tokens;
                    self.dirty = true;
                }
                if a.files != files {
                    a.files = files;
                    self.dirty = true;
                }
            }
        }
        self.update_agent_badges();
        self.check_budget();
        let over = agents::overlaps(self.panes.values().filter_map(|p| p.agent.as_ref()));
        for (path, names) in over {
            if !self.overlaps_seen.contains(&path) {
                self.overlaps_seen.push(path.clone());
                self.notice(format!("⚠ {} both changed {path}", names.join(" and ")));
            }
        }
    }

    fn tool_key(&mut self, id: PaneId, key: &[u8]) {
        let out = match self.tools.get_mut(&id) {
            Some(Tool::Files(f)) => f.key(key),
            Some(Tool::System(sp)) => Outcome {
                redraw: sp.key(key),
                ..Default::default()
            },
            Some(Tool::Flow(fp)) => {
                if let Some(FlowAct::Type(cmd)) = fp.key(key) {
                    self.type_in_shell(&cmd);
                }
                return;
            }
            Some(Tool::Remote(r)) => {
                let jobs = r.key(key);
                self.run_remote(id, jobs);
                self.dirty = true;
                return;
            }
            None => return,
        };
        self.apply(id, out);
    }

    fn flush_forward(&mut self, buf: &mut Vec<u8>) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let id = self.input_target();
        // broadcast: the same keys to every shell in this tab that has it on
        let mut targets = vec![id];
        if self.broadcast.contains(&id) {
            if let Some(t) = self.tab_of(id) {
                for other in self.tabs[t].root.panes() {
                    if other != id && self.broadcast.contains(&other) {
                        targets.push(other);
                    }
                }
            }
        }
        for t in targets {
            if let Some(p) = self.panes.get_mut(&t) {
                if p.pane.scroll() > 0 {
                    p.pane.scroll_by(isize::MIN / 2);
                    self.dirty = true;
                }
                p.pane.write(buf)?;
            }
        }
        buf.clear();
        Ok(())
    }

    fn on_mouse(&mut self, ev: MouseEv) -> io::Result<()> {
        if let Some(ed) = self.editor.as_mut() {
            if ed.mouse(ev.x, ev.y, ev.is_left_press(), ev.is_motion(), ev.release) {
                self.dirty = true;
            }
            return Ok(());
        }
        // the right-click menu: a click on an item does it, anywhere else closes it
        if let Some(m) = self.ctx {
            if ev.is_left_press() || (!ev.release && ev.code & 0b11 == 2) {
                self.ctx = None;
                self.dirty = true;
                let r = self.context_rect(&m);
                if ev.is_left_press() && r.contains((ev.x, ev.y).into()) {
                    let i = ev.y.saturating_sub(r.y + 1) as usize;
                    if i < CONTEXT_ITEMS.len() {
                        self.context_act_menu(&m, i)?;
                    }
                    return Ok(());
                }
                if ev.is_left_press() {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }
        // dragging out a selection: follow the mouse within that pane
        if let Some(mut s) = self.sel.filter(|s| s.dragging) {
            if let Some((_, _, inner)) = self.visible().into_iter().find(|(v, _, _)| *v == s.pane) {
                let row = ev.y.clamp(inner.y, inner.bottom().saturating_sub(1)) - inner.y;
                let col = ev.x.clamp(inner.x, inner.right().saturating_sub(1)) - inner.x;
                if ev.is_motion() || ev.release {
                    s.b = (row, col);
                    if ev.release {
                        s.dragging = false;
                        if s.a == s.b {
                            self.sel = None;
                        } else {
                            self.sel = Some(s);
                            self.copy_selection();
                        }
                    } else {
                        self.sel = Some(s);
                    }
                    self.dirty = true;
                    return Ok(());
                }
            } else {
                self.sel = None;
            }
        }
        // drag a divider between splits to resize them
        if let Some(path) = self.drag.clone() {
            if ev.release {
                self.drag = None;
                self.relayout();
            } else if ev.is_motion() {
                let (_, content, _) = self.regions();
                if self.tabs[self.active]
                    .root
                    .drag_divider(content, &path, ev.x, ev.y)
                {
                    self.relayout();
                }
            }
            return Ok(());
        }
        if ev.is_left_press() && self.view == View::Full && !self.tabs[self.active].zoomed {
            let (_, content, _) = self.regions();
            if let Some(path) = self.tabs[self.active].root.divider_at(content, ev.x, ev.y) {
                self.drag = Some(path);
                return Ok(());
            }
        }
        let (tabbar, _, status) = self.regions();
        if let Some(st) = status {
            if ev.y == st.y && ev.is_left_press() && ev.x < st.x + st.width / 2 {
                if !self.activities().is_empty() || !self.done.is_empty() {
                    self.acts_list = Some(0);
                    self.dirty = true;
                }
                return Ok(());
            }
        }
        if let Some(bar) = tabbar {
            if ev.y == bar.y {
                if ev.is_left_press() {
                    if let Some(i) = ui::tab_at(self, ev.x) {
                        self.switch_tab(i);
                    } else if ev.x > bar.x + bar.width / 2 && !self.activities().is_empty() {
                        self.acts_list = Some(0);
                        self.dirty = true;
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
        // select text with the mouse, right-click for the menu (with shift
        // when the program in the pane uses the mouse itself)
        let shift = ev.code & 4 != 0;
        let program = p.pane.wants_mouse() && p.pane.scroll() == 0 && !shift;
        if inside && !program && ev.wheel().is_none() && !ev.release && !ev.is_motion() {
            let (row, col) = (ev.y - inner.y, ev.x - inner.x);
            let scroll = p.pane.scroll();
            let cols = p.pane.screen().size().1;
            // ctrl-click a link opens it (terminals don't report ⌘ in clicks)
            if ev.code & 0b11 == 0 && ev.code & 16 != 0 {
                let line = p.pane.screen().contents_between(row, 0, row, cols);
                if let Some(url) = url_at(&line, col as usize) {
                    self.open_url(&url);
                    return Ok(());
                }
            }
            // option-click on the line being typed moves the cursor there
            if ev.code & 0b11 == 0
                && ev.code & 8 != 0
                && !p.running
                && scroll == 0
                && !p.pane.screen().alternate_screen()
            {
                let (cr, cc) = p.pane.screen().cursor_position();
                let delta = (row as i64 - cr as i64) * cols as i64 + (col as i64 - cc as i64);
                if delta != 0 && delta.abs() <= 4000 {
                    let app = p.pane.screen().application_cursor();
                    let key: &[u8] = match (delta > 0, app) {
                        (true, false) => b"\x1b[C",
                        (true, true) => b"\x1bOC",
                        (false, false) => b"\x1b[D",
                        (false, true) => b"\x1bOD",
                    };
                    p.pane.write(&key.repeat(delta.unsigned_abs() as usize))?;
                }
                return Ok(());
            }
            match ev.code & 0b11 {
                0 => {
                    let n = match self.click {
                        Some((t, pid, r, c, n))
                            if pid == id
                                && t.elapsed() < Duration::from_millis(450)
                                && r == row
                                && c.abs_diff(col) <= 1 =>
                        {
                            n % 3 + 1
                        }
                        _ => 1,
                    };
                    self.click = Some((Instant::now(), id, row, col, n));
                    let cols = p.pane.screen().size().1;
                    let line = p.pane.screen().contents_between(row, 0, row, cols);
                    let sel = |a, b, dragging| Selection {
                        pane: id,
                        a,
                        b,
                        scroll,
                        dragging,
                    };
                    match n {
                        1 => self.sel = Some(sel((row, col), (row, col), true)),
                        2 => {
                            self.sel = word_at(&line, col as usize)
                                .map(|(a, b)| sel((row, a as u16), (row, b as u16), false));
                            self.copy_selection();
                        }
                        _ => {
                            let end = line.trim_end().chars().count().max(1) as u16 - 1;
                            self.sel = Some(sel((row, 0), (row, end), false));
                            self.copy_selection();
                        }
                    }
                    self.dirty = true;
                    return Ok(());
                }
                1 => {
                    // middle-click pastes (like PuTTY and X11 terminals)
                    self.context_act(id, 1)?;
                    return Ok(());
                }
                2 if self.ui.right_click == "paste" && ev.code & 16 == 0 => {
                    // PuTTY: right-click pastes (ctrl+right-click: the menu)
                    self.context_act(id, 1)?;
                    return Ok(());
                }
                2 => {
                    self.ctx = Some(ContextMenu {
                        pane: id,
                        x: ev.x,
                        y: ev.y,
                        cell: (row, col),
                        sel: if self.sel.is_some_and(|s| s.pane == id) {
                            0
                        } else {
                            1
                        },
                    });
                    self.dirty = true;
                    return Ok(());
                }
                _ => {}
            }
        }
        let Some(p) = self.panes.get_mut(&id) else {
            return Ok(());
        };
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
                        "sv files: unknown view `{view}` (tree, list, columns, grid, git, find, size, recent)"
                    )),
                }
                Ok(())
            }
            ["system"] => self.show_tool("system", false),
            ["resize", rest @ ..] => {
                let tab = self.active;
                let focus = self.focused();
                let changed = match rest {
                    [d] | [d, _] if crate::cli::resize_dir(d).is_some() => {
                        let dir = crate::cli::resize_dir(d).unwrap();
                        let n: usize = rest.get(1).and_then(|n| n.parse().ok()).unwrap_or(1);
                        let mut any = false;
                        for _ in 0..n.clamp(1, 20) {
                            any |= self.tabs[tab].root.resize(focus, dir, 0.05);
                        }
                        any
                    }
                    [pct, more @ ..] if pct.ends_with('%') => {
                        let share =
                            pct.trim_end_matches('%').parse::<f32>().unwrap_or(50.0) / 100.0;
                        let axis = match more.first().copied() {
                            Some("width") => Some(Axis::Horizontal),
                            Some("height") => Some(Axis::Vertical),
                            _ => None,
                        };
                        self.tabs[tab].root.set_share(focus, share, axis)
                    }
                    _ => false,
                };
                if changed {
                    self.relayout();
                } else {
                    self.notice(
                        "sv resize: nothing to resize here (one pane, or already at the limit)",
                    );
                }
                Ok(())
            }
            ["serial", dev, baud] => {
                let name = dev.rsplit('/').next().unwrap_or(dev).to_string();
                let cmd = format!("sv serial --here {} {baud}", shell_quote(dev));
                self.open_session_tab(&format!("serial · {name}"), &cmd)
            }
            ["palette"] => {
                self.open_palette();
                Ok(())
            }
            ["ssh"] => {
                self.open_sessions();
                Ok(())
            }
            ["ssh", name, cmd] => self.open_session_tab(name, cmd),
            ["log", rest @ ..] => {
                let id = self.focused();
                match rest {
                    ["start"] => self.set_log(id, true, None),
                    ["start", path] => self.set_log(id, true, Some(PathBuf::from(path))),
                    ["stop"] => self.set_log(id, false, None),
                    _ => {
                        let msg = match self.panes.get(&id).and_then(|p| p.log.as_ref()) {
                            Some(l) => format!("logging this pane to {}", l.path.display()),
                            None => "not logging this pane (sv log start [file])".to_string(),
                        };
                        self.notice(msg);
                    }
                }
                Ok(())
            }
            ["activity"] => {
                self.acts_list = Some(0);
                self.dirty = true;
                Ok(())
            }
            ["settings"] => {
                self.settings = Some(crate::settings::Screen::new());
                self.dirty = true;
                Ok(())
            }
            ["agent", "board", view] => {
                self.open_board(crate::board::View::parse(view), None);
                Ok(())
            }
            ["agent", "plan", goal, cwd] => {
                let unb = |s: &str| {
                    sv_proto::b64_decode(s)
                        .map(|v| String::from_utf8_lossy(&v).into_owned())
                        .unwrap_or_default()
                };
                let folder = Some(PathBuf::from(unb(cwd))).filter(|p| p.is_dir());
                self.open_board(Some(crate::board::View::Plan), Some((unb(goal), folder)));
                Ok(())
            }
            ["agent", "attach"] => {
                self.attach_agent();
                Ok(())
            }
            ["agent", "new", cli, name, mode, prompt, cmd, cwd] => {
                let unb = |s: &str| {
                    sv_proto::b64_decode(s)
                        .map(|v| String::from_utf8_lossy(&v).into_owned())
                        .unwrap_or_default()
                };
                let cli = Cli::parse(cli).unwrap_or(Cli::Generic);
                let name = if *name == "-" {
                    String::new()
                } else {
                    name.to_string()
                };
                let folder = Some(PathBuf::from(unb(cwd))).filter(|p| p.is_dir());
                let wt = *mode == "worktree" && self.opts.agent_worktree;
                // no repo → just run it here
                let wt = wt && folder.as_deref().and_then(agents::repo_head).is_some();
                if let Err(e) =
                    self.start_agent(None, cli, &name, &unb(prompt), &unb(cmd), wt, folder)
                {
                    self.notice(format!("sv agent: {e}"));
                }
                Ok(())
            }
            ["flow"] => self.show_tool("flow", false),
            ["layout", "agents"] => {
                self.toggle_agents();
                Ok(())
            }
            ["layout", name] => match Preset::parse(name) {
                Some(p) => self.apply_preset(p),
                None => {
                    if let Err(e) = self.open_layout(name) {
                        self.notice(format!(
                            "sv layout: {e} (presets: dev, monitor, files, data; saved: {})",
                            crate::layouted::saved_layouts().join(", ")
                        ));
                    }
                    Ok(())
                }
            },
            ["theme", t] => {
                self.light = match *t {
                    "light" => true,
                    "dark" => false,
                    _ => crate::config::Theme::Auto.is_light(),
                };
                self.notice(format!(
                    "theme: {}",
                    if self.light { "light" } else { "dark" }
                ));
                self.dirty = true;
                Ok(())
            }
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
        crate::loading::set_low_power(on);
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

    // ------------------------------------------------------------ layout editor

    fn settings_key(&mut self, key: &[u8]) {
        self.dirty = true;
        let Some(sc) = self.settings.as_mut() else {
            return;
        };
        let (changed, close) = sc.key(key);
        if changed {
            let s = sc.s.clone();
            if let Err(e) = s.save() {
                self.notice(format!("settings: {e}"));
            }
            self.apply_ui(s);
        }
        if close {
            self.settings = None;
        }
    }

    fn folder_style(s: &crate::settings::Settings) -> ratatui::style::Style {
        use ratatui::style::{Color, Modifier, Style};
        match crate::settings::color_index(&s.list_folder) {
            Some(c) => {
                let st = Style::default().fg(Color::Indexed(c));
                if s.list_bold {
                    st.add_modifier(Modifier::BOLD)
                } else {
                    st
                }
            }
            None => Style::default(),
        }
    }

    /// Use new settings: folder colour in files panes, theme.
    fn apply_ui(&mut self, s: crate::settings::Settings) {
        let style = Self::folder_style(&s);
        for t in self.tools.values_mut() {
            if let Tool::Files(f) = t {
                f.folder_style = style;
            }
        }
        crate::loading::set_style(&s.spinner);
        crate::loading::set_bar(s.progress_bar);
        self.truecolor = s.true_color();
        match s.theme.as_str() {
            "light" => self.light = true,
            "dark" => self.light = false,
            "auto" => self.light = crate::config::Theme::Auto.is_light(),
            _ => {}
        }
        self.ui = s;
    }

    fn open_editor(&mut self) {
        use crate::layouted::{Editor, Preset, builtin_presets, kind_of};
        let t = &self.tabs[self.active];
        let kinds: Vec<(PaneId, crate::layouted::Kind)> = t
            .root
            .panes()
            .into_iter()
            .map(|id| (id, kind_of(self.tools.get(&id).map(|t| t.kind()))))
            .collect();
        let mut presets = builtin_presets();
        for name in crate::layouted::saved_layouts() {
            if let Ok(node) = crate::layouted::load_layout(&name) {
                presets.push(Preset {
                    name,
                    saved: true,
                    node,
                });
            }
        }
        let folder = self.focused_cwd().unwrap_or_default();
        self.editor = Some(Editor::new(
            &t.root.clone(),
            &kinds,
            t.focus,
            folder,
            presets,
        ));
        self.dirty = true;
    }

    fn editor_key(&mut self, key: &[u8]) -> io::Result<()> {
        self.dirty = true;
        let Some(ed) = self.editor.as_mut() else {
            return Ok(());
        };
        match ed.key(key) {
            None => {}
            Some(crate::layouted::EdAct::Cancel) => self.editor = None,
            Some(crate::layouted::EdAct::Apply) => self.apply_editor(),
        }
        Ok(())
    }

    /// Put the edited layout on the tab: keep running panes, create new
    /// ones, close the ones left out (not while they run something), save.
    fn apply_editor(&mut self) {
        let Some(ed) = self.editor.clone() else {
            return;
        };
        let dropped = ed.dropped();
        let busy = self.busy(&dropped, None);
        if !busy.is_empty() {
            if let Some(e) = self.editor.as_mut() {
                e.message = Some(format!(
                    "{} still running in a pane you removed — finish it or keep that pane (u)",
                    Self::describe(&busy)
                ));
            }
            return;
        }
        let here = ed.folder.clone();
        let mut ids: Vec<Option<PaneId>> = vec![None; ed.slots.len()];
        for i in ed.tree.panes() {
            let slot = &ed.slots[i as usize];
            let id = match (slot.existing, slot.kind.tool) {
                (Some(id), _) => id,
                (None, Some(tool)) => self.new_tool(tool, here.clone()),
                (None, None) => match self.new_pane(Some(here.clone())) {
                    Ok(id) => id,
                    Err(e) => {
                        self.notice(format!("layout: {e}"));
                        return;
                    }
                },
            };
            ids[i as usize] = Some(id);
        }
        fn real(n: &Node, ids: &[Option<PaneId>]) -> Node {
            match n {
                Node::Leaf(i) => Node::Leaf(ids[*i as usize].unwrap_or(0)),
                Node::Split { axis, ratio, a, b } => Node::Split {
                    axis: *axis,
                    ratio: *ratio,
                    a: Box::new(real(a, ids)),
                    b: Box::new(real(b, ids)),
                },
            }
        }
        let root = real(&ed.tree, &ids);
        for id in dropped {
            if let Some(mut p) = self.panes.remove(&id) {
                p.pane.kill();
            }
            self.tools.remove(&id);
        }
        let t = &mut self.tabs[self.active];
        t.root = root;
        t.focus = ids[ed.sel as usize].unwrap_or(t.focus);
        t.zoomed = false;
        t.arrangement = None;
        self.editor = None;
        self.relayout();
        if ed.name.is_empty() {
            self.notice("layout applied");
            return;
        }
        match crate::layouted::save_layout(&ed.name, &ed.saved()) {
            Ok(_) => {
                let mut prefs = crate::layouted::Prefs::load();
                let mut extra = String::new();
                if ed.make_default {
                    prefs.default = Some(ed.name.clone());
                    extra += " · used for new sessions";
                }
                if ed.for_folder {
                    prefs
                        .folders
                        .insert(here.display().to_string(), ed.name.clone());
                    extra += &format!(
                        " · opens when Shellvisor starts in {}",
                        ui::short_path(&here.to_string_lossy())
                    );
                }
                if ed.make_default || ed.for_folder {
                    let _ = prefs.save();
                }
                self.notice(format!("saved layout {}{extra}", ed.name));
            }
            Err(e) => self.notice(format!("couldn't save layout: {e}")),
        }
    }

    /// Open a saved layout: in this tab if it holds a single shell (which is
    /// kept), else in a new tab.
    fn open_layout(&mut self, name: &str) -> Result<(), String> {
        let saved = crate::layouted::load_layout(name)?;
        let here = self
            .focused_cwd()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let tab = self.active;
        let panes = self.tabs[tab].root.panes();
        let mut reuse = (panes.len() == 1 && self.panes.contains_key(&panes[0])).then(|| panes[0]);
        let reused = reuse.is_some();
        let root = saved
            .rebuild(&mut |l: &Leaf| -> io::Result<PaneId> {
                match (&l.tool, reuse.take()) {
                    (None, Some(id)) => Ok(id),
                    (None, None) => self.new_pane(Some(here.clone())),
                    (Some(k), r) => {
                        reuse = r;
                        Ok(self.new_tool(k, here.clone()))
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let focus = root
            .panes()
            .into_iter()
            .find(|id| self.panes.contains_key(id))
            .unwrap_or_else(|| root.panes()[0]);
        if reused {
            let t = &mut self.tabs[tab];
            t.root = root;
            t.focus = focus;
            t.zoomed = false;
            t.arrangement = None;
        } else {
            let mut t = Tab::new(focus);
            t.root = root;
            self.tabs.insert(tab + 1, t);
            self.active = tab + 1;
        }
        self.view = View::Full;
        self.notice(format!("layout: {name}"));
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
                // The default 4-pane workspace: files │ shell │ system over flow.
                let f = self.new_tool("files", here.clone());
                let s = self.new_tool("system", here.clone());
                let fl = self.new_tool("flow", here);
                Node::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.22,
                    a: leaf(f),
                    b: Box::new(Node::Split {
                        axis: Axis::Horizontal,
                        ratio: 0.62,
                        a: leaf(shell),
                        b: Box::new(Node::Split {
                            axis: Axis::Vertical,
                            ratio: 0.6,
                            a: leaf(s),
                            b: leaf(fl),
                        }),
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
            Preset::Data => {
                let fl = self.new_tool("flow", here);
                Node::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.5,
                    a: leaf(shell),
                    b: leaf(fl),
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
            Cmd::Agents => self.toggle_agents(),
            Cmd::AgentBoard => self.open_board(None, None),
            Cmd::Settings => {
                self.settings = Some(crate::settings::Screen::new());
            }
            Cmd::Activity => self.acts_list = Some(0),
            Cmd::Find => {
                let id = self.focused();
                if self.panes.contains_key(&id) {
                    self.open_find(id);
                } else {
                    self.notice("find works in shell panes");
                }
            }
            Cmd::ClearPane => {
                let id = self.focused();
                self.clear_pane(id);
            }
            Cmd::Sessions => self.open_sessions(),
            Cmd::Palette => self.open_palette(),
            Cmd::Broadcast => self.toggle_broadcast(),
            Cmd::RemoteFiles => self.open_remote()?,
            Cmd::Reconnect => {
                let id = self.focused();
                self.reconnect(id);
            }
            Cmd::Duplicate => {
                let id = self.focused();
                self.duplicate(id)?;
            }
            Cmd::LayoutEditor => {
                if self.view == View::Basic {
                    self.set_view(View::Full);
                }
                self.tabs[tab].zoomed = false;
                self.open_editor();
            }
            Cmd::NewAgent => self.open_new_agent(),
            Cmd::Approvals => {
                if self.pending_approvals().is_empty() {
                    self.notice("no agent is waiting for approval");
                } else {
                    self.modal = Modal::Approvals(0);
                }
            }
            Cmd::AttachAgent => self.attach_agent(),
            Cmd::ToggleFlow => self.show_tool("flow", true)?,
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

#[cfg(test)]
mod select_tests {
    use super::*;

    #[test]
    fn words_and_links() {
        let line = "see https://github.com/avmg0/shellvisor. and ~/PROJECTS/app now";
        assert_eq!(word_at(line, 0), Some((0, 2)));
        assert_eq!(word_at(line, 3), None, "a space");
        let p = line.find("~/").unwrap();
        assert_eq!(word_at(line, p + 3), Some((p, p + 13)));
        assert_eq!(
            url_at(line, 10).as_deref(),
            Some("https://github.com/avmg0/shellvisor")
        );
        assert_eq!(url_at(line, 1), None);
        let s = Selection {
            pane: 1,
            a: (3, 5),
            b: (1, 2),
            scroll: 0,
            dragging: false,
        };
        assert_eq!(s.range(), ((1, 2), (3, 5)));
        assert!(s.contains(2, 0) && s.contains(1, 2) && !s.contains(1, 1) && !s.contains(3, 6));
        let lines = vec![
            "Error one".to_string(),
            "no".into(),
            "error two ERROR".into(),
        ];
        assert_eq!(
            find_in(&lines, "error"),
            vec![(0, 0, 5), (2, 0, 5), (2, 10, 5)]
        );
        assert_eq!(
            find_in(&lines, "ERROR"),
            vec![(2, 10, 5)],
            "a capital: exact case"
        );
        assert!(find_in(&lines, "").is_empty());
        assert!(fuzzy("spl", "Split side by side").is_some());
        assert!(fuzzy("xyz", "Split").is_none());
        assert!(fuzzy("sb", "Split side by side").is_some());
        assert!(fuzzy("split", "Split stacked").unwrap() > fuzzy("split", "sxpxlxixt").unwrap());
    }
}

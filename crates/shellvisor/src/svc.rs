//! Panes for the services your shells start: Ports, Docker, Logs and HTTP.
//!
//! - **Ports**: what is listening, the process, and which tab started it;
//!   ⏎ opens it in the browser, `k` stops it, `t` goes to its tab.
//! - **Docker**: containers (docker, or podman); `l` follows its logs in a
//!   split, `s` starts / stops, `r` restarts, `x` a shell inside it.
//! - **Logs**: follows several files at once, coloured by level; `/` filters
//!   by text or with the `sv` table engine (`where level == error`), `F4`
//!   opens what is shown in the table viewer.
//! - **HTTP**: a small request runner for the APIs you're serving: method,
//!   URL, headers and body, the answer with status, time and pretty JSON;
//!   a history of requests; `c` copies it as a curl command.
//!
//! Data is read on background threads; the panes only draw what came back.

use crate::data::value::{Data, Record, Value};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const CYAN: Color = Color::Indexed(73);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const TEXT: Color = Color::Indexed(252);
const SEL_BG: Color = Color::Indexed(237);
const FIELD_BG: Color = Color::Indexed(235);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ports,
    Docker,
    Logs,
    Http,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "ports" => Kind::Ports,
            "docker" | "containers" => Kind::Docker,
            "logs" => Kind::Logs,
            "http" => Kind::Http,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Ports => "ports",
            Kind::Docker => "docker",
            Kind::Logs => "logs",
            Kind::Http => "http",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Kind::Ports => "PORTS",
            Kind::Docker => "DOCKER",
            Kind::Logs => "LOGS",
            Kind::Http => "HTTP",
        }
    }
    /// Key strip for the window's bottom bar.
    pub fn keys(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Kind::Ports => vec![
                ("↑↓", "Port"),
                ("⏎", "Open in browser"),
                ("t", "Its tab"),
                ("k K", "Stop · kill"),
                ("c", "Copy URL"),
            ],
            Kind::Docker => vec![
                ("↑↓", "Container"),
                ("l", "Logs in a split"),
                ("s", "Start / stop"),
                ("r", "Restart"),
                ("x", "Shell inside"),
                ("a", "All / running"),
            ],
            Kind::Logs => vec![
                ("/", "Filter (text or where …)"),
                ("space", "Pause"),
                ("a x", "Add · remove file"),
                ("F4", "Table"),
                ("↑↓", "Scroll"),
            ],
            Kind::Http => vec![
                ("m", "Method"),
                ("u h b", "URL · headers · body"),
                ("⏎", "Send"),
                ("↑↓", "History"),
                ("c", "Copy as curl"),
            ],
        }
    }
}

/// What the app should do for a pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    OpenUrl(String),
    Signal(i64, i32),
    /// A shell split under this pane, running the command.
    RunSplit(String),
    /// Focus the shell whose process tree holds this pid.
    FocusPid(Vec<i64>),
    Notice(String),
    Table(String, Data),
    Copy(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub port: i64,
    pub address: String,
    pub pid: Option<i64>,
    pub process: String,
    /// The pid and its parents, nearest first (to find the tab).
    pub ancestors: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Response {
    pub status: String,
    pub ms: u64,
    pub headers: Vec<String>,
    pub body: Vec<String>,
    pub error: Option<String>,
}

enum Msg {
    Ports(Vec<Port>),
    Docker(Result<Vec<Container>, String>),
    Http(Response),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Url,
    Headers,
    Body,
    Filter,
    AddFile,
}

struct LogFile {
    path: PathBuf,
    offset: u64,
    color: u8,
}

pub struct SvcPane {
    pub kind: Kind,
    pub sel: usize,
    rx: Option<Receiver<Msg>>,
    next_at: Instant,
    confirm: Option<(String, Act)>,
    input: Option<(Field, String)>,
    // ports
    pub ports: Vec<Port>,
    // docker
    docker: Option<Result<Vec<Container>, String>>,
    all: bool,
    tool: Option<&'static str>,
    // logs
    files: Vec<LogFile>,
    lines: VecDeque<(usize, String)>,
    filter: String,
    paused: bool,
    scroll: usize,
    // http
    pub req: Request,
    resp: Option<Response>,
    sending: Option<Instant>,
    history: Vec<Request>,
    hist_pos: Option<usize>,
}

const LOG_KEEP: usize = 5000;
const LOG_COLORS: [u8; 6] = [73, 179, 108, 140, 175, 110];
const METHODS: [&str; 6] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"];

fn history_path() -> PathBuf {
    crate::paths::state_dir().join("http-history.json")
}

impl SvcPane {
    /// `args`: files for Logs, a URL for HTTP.
    pub fn new(kind: Kind, cwd: &Path, args: &[String]) -> SvcPane {
        let mut p = SvcPane {
            kind,
            sel: 0,
            rx: None,
            next_at: Instant::now(),
            confirm: None,
            input: None,
            ports: Vec::new(),
            docker: None,
            all: false,
            tool: ["docker", "podman"]
                .into_iter()
                .find(|t| crate::shell::find_in_path(t).is_some()),
            files: Vec::new(),
            lines: VecDeque::new(),
            filter: String::new(),
            paused: false,
            scroll: 0,
            req: Request {
                method: "GET".into(),
                url: String::new(),
                headers: String::new(),
                body: String::new(),
            },
            resp: None,
            sending: None,
            history: Vec::new(),
            hist_pos: None,
        };
        match kind {
            Kind::Logs => {
                let found: Vec<PathBuf> = if args.is_empty() {
                    find_logs(cwd)
                } else {
                    args.iter().map(|a| cwd.join(a)).collect()
                };
                for f in found {
                    let _ = p.add_file(f);
                }
                // several files: their first lines interleaved by time, when
                // every line has one
                if p.files.len() > 1 && p.lines.iter().all(|(_, l)| !time_of(l).is_empty()) {
                    p.lines.make_contiguous().sort_by_key(|(_, l)| time_of(l));
                }
            }
            Kind::Http => {
                p.history = std::fs::read_to_string(history_path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<Vec<serde_json::Value>>(&t).ok())
                    .unwrap_or_default()
                    .iter()
                    .map(|v| Request {
                        method: v["method"].as_str().unwrap_or("GET").into(),
                        url: v["url"].as_str().unwrap_or("").into(),
                        headers: v["headers"].as_str().unwrap_or("").into(),
                        body: v["body"].as_str().unwrap_or("").into(),
                    })
                    .collect();
                p.req.url = args
                    .first()
                    .cloned()
                    .or_else(|| p.history.first().map(|r| r.url.clone()))
                    .unwrap_or_else(|| "http://localhost:3000/".into());
            }
            _ => {}
        }
        p
    }

    pub fn title(&self) -> String {
        match self.kind {
            Kind::Logs => format!("{} file(s)", self.files.len()),
            Kind::Http => format!("{} {}", self.req.method, self.req.url),
            k => k.name().to_string(),
        }
    }

    fn every(&self, low: bool) -> Duration {
        let base = match self.kind {
            Kind::Ports => 3,
            Kind::Docker => 3,
            Kind::Logs => return Duration::from_millis(if low { 2000 } else { 500 }),
            Kind::Http => return Duration::from_secs(3600),
        };
        Duration::from_secs(if low { base * 3 } else { base })
    }

    pub fn due_in(&self, low: bool) -> Duration {
        if self.rx.is_some() || self.sending.is_some() {
            return Duration::from_millis(100);
        }
        self.next_at
            .saturating_duration_since(Instant::now())
            .min(self.every(low))
    }

    /// Collect results and start what is due. True: draw again.
    pub fn poll(&mut self, low: bool) -> bool {
        let mut redraw = false;
        if let Some(rx) = &self.rx {
            if let Ok(m) = rx.try_recv() {
                self.rx = None;
                redraw = true;
                match m {
                    Msg::Ports(v) => {
                        self.ports = v;
                        self.sel = self.sel.min(self.ports.len().saturating_sub(1));
                    }
                    Msg::Docker(v) => {
                        if let Ok(c) = &v {
                            self.sel = self.sel.min(c.len().saturating_sub(1));
                        }
                        self.docker = Some(v);
                    }
                    Msg::Http(r) => {
                        self.resp = Some(r);
                        self.sending = None;
                    }
                }
            }
        }
        if self.kind == Kind::Logs && !self.paused && Instant::now() >= self.next_at {
            self.next_at = Instant::now() + self.every(low);
            redraw |= self.read_logs();
        }
        if self.rx.is_none() && Instant::now() >= self.next_at {
            self.next_at = Instant::now() + self.every(low);
            let (tx, rx) = channel();
            match self.kind {
                Kind::Ports => {
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Ports(read_ports()));
                    });
                    self.rx = Some(rx);
                }
                Kind::Docker => {
                    let (tool, all) = (self.tool, self.all);
                    std::thread::spawn(move || {
                        let _ = tx.send(Msg::Docker(read_containers(tool, all)));
                    });
                    self.rx = Some(rx);
                }
                _ => {}
            }
        }
        redraw
    }

    // ---------------------------------------------------------------- logs

    /// Follow a file; false (and nothing added) when it isn't text.
    pub fn add_file(&mut self, path: PathBuf) -> bool {
        if self.files.iter().any(|f| f.path == path) {
            return true;
        }
        if !is_text(&path) {
            return false;
        }
        let color = LOG_COLORS[self.files.len() % LOG_COLORS.len()];
        let idx = self.files.len();
        // start with the last 200 lines
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let start = len.saturating_sub(64 * 1024);
        let mut tail = read_from(&path, start).unwrap_or_default();
        if start > 0 {
            if let Some(i) = tail.find('\n') {
                tail = tail[i + 1..].to_string();
            }
        }
        let lines: Vec<&str> = tail.lines().collect();
        for l in &lines[lines.len().saturating_sub(200)..] {
            self.lines.push_back((idx, clean(l)));
        }
        while self.lines.len() > LOG_KEEP {
            self.lines.pop_front();
        }
        self.files.push(LogFile {
            path,
            offset: len,
            color,
        });
        true
    }

    fn read_logs(&mut self) -> bool {
        let mut any = false;
        for (i, f) in self.files.iter_mut().enumerate() {
            let len = std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0);
            if len < f.offset {
                // truncated or rotated: start over
                f.offset = 0;
                self.lines.push_back((i, "── file was truncated ──".into()));
            }
            if len > f.offset {
                let text = read_from(&f.path, f.offset).unwrap_or_default();
                // only whole lines; the rest waits for its newline
                let upto = text.rfind('\n').map(|n| n + 1).unwrap_or(0);
                for l in text[..upto].lines() {
                    self.lines.push_back((i, clean(l)));
                }
                f.offset += upto as u64;
                any |= upto > 0;
            }
        }
        while self.lines.len() > LOG_KEEP {
            self.lines.pop_front();
        }
        any
    }

    /// Log lines as records for the table engine.
    pub fn log_records(&self) -> Vec<Record> {
        self.lines
            .iter()
            .map(|(i, l)| {
                let file = self
                    .files
                    .get(*i)
                    .and_then(|f| f.path.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                vec![
                    ("file".to_string(), Value::Str(file)),
                    ("level".to_string(), Value::Str(level_of(l).to_string())),
                    ("time".to_string(), Value::Str(time_of(l))),
                    ("message".to_string(), Value::Str(l.clone())),
                ]
            })
            .collect()
    }

    /// The lines that pass the filter: text, or `where …` / any `sv` filter
    /// pipeline over file, level, time, message.
    pub fn shown_lines(&self) -> Result<Vec<(usize, String)>, String> {
        let f = self.filter.trim();
        if f.is_empty() {
            return Ok(self.lines.iter().cloned().collect());
        }
        let first = f.split_whitespace().next().unwrap_or("");
        let is_engine = matches!(
            first,
            "where" | "reject" | "first" | "last" | "skip" | "uniq" | "sort-by" | "reverse"
        );
        if !is_engine {
            let needle = f.to_lowercase();
            return Ok(self
                .lines
                .iter()
                .filter(|(_, l)| l.to_lowercase().contains(&needle))
                .cloned()
                .collect());
        }
        let stages = crate::data::ops::split_pipeline(f)?;
        let mut data = Data::Table(self.log_records());
        for st in &stages {
            data = crate::data::ops::filter(&st[0], &st[1..], data)?;
        }
        let rows = data.into_rows();
        let names: HashMap<String, usize> = self
            .files
            .iter()
            .enumerate()
            .filter_map(|(i, f)| Some((f.path.file_name()?.to_string_lossy().into_owned(), i)))
            .collect();
        Ok(rows
            .into_iter()
            .map(|r| {
                let get = |k: &str| {
                    r.iter()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| match v {
                            Value::Str(s) => s.clone(),
                            v => v.display(),
                        })
                        .unwrap_or_default()
                };
                (*names.get(&get("file")).unwrap_or(&0), get("message"))
            })
            .collect())
    }

    // ---------------------------------------------------------------- http

    fn send(&mut self) {
        let req = self.req.clone();
        self.history.retain(|h| h != &req);
        self.history.insert(0, req.clone());
        self.history.truncate(50);
        let json: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|r| {
                serde_json::json!({"method": r.method, "url": r.url, "headers": r.headers, "body": r.body})
            })
            .collect();
        let _ = std::fs::create_dir_all(crate::paths::state_dir());
        let _ = std::fs::write(
            history_path(),
            serde_json::to_string_pretty(&json).unwrap_or_default(),
        );
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Http(run_request(&req)));
        });
        self.rx = Some(rx);
        self.sending = Some(Instant::now());
        self.resp = None;
        self.hist_pos = None;
    }

    // ---------------------------------------------------------------- keys

    /// (redraw, what the app should do)
    pub fn key(&mut self, key: &[u8]) -> (bool, Vec<Act>) {
        // y / n for a question
        if let Some((_, act)) = self.confirm.clone() {
            self.confirm = None;
            return match key {
                b"y" | b"Y" => (true, vec![act]),
                _ => (true, Vec::new()),
            };
        }
        if let Some((field, mut text)) = self.input.take() {
            match key {
                b"\x1b" => {}
                b"\r" => return (true, self.submit(field, text)),
                [0x7f] | [0x08] => {
                    text.pop();
                    self.input = Some((field, text));
                }
                [0x15] => self.input = Some((field, String::new())),
                // headers and body take several lines: alt+⏎ / ctrl+j
                b"\x1b\r" | [0x0a] if matches!(field, Field::Headers | Field::Body) => {
                    text.push('\n');
                    self.input = Some((field, text));
                }
                k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                    text.push_str(&String::from_utf8_lossy(k));
                    self.input = Some((field, text));
                }
                _ => self.input = Some((field, text)),
            }
            if field == Field::Filter {
                // filter as you type
                self.filter = self
                    .input
                    .as_ref()
                    .map(|(_, t)| t.clone())
                    .unwrap_or_default();
                self.scroll = 0;
            }
            return (true, Vec::new());
        }
        let n = self.rows();
        let nav = |sel: &mut usize| -> bool {
            let before = *sel;
            *sel = match key {
                b"\x1b[A" | b"\x1bOA" | b"k" => sel.saturating_sub(1),
                b"\x1b[B" | b"\x1bOB" | b"j" => (*sel + 1).min(n.saturating_sub(1)),
                b"g" | b"\x1b[H" => 0,
                b"G" | b"\x1b[F" => n.saturating_sub(1),
                _ => return false,
            };
            let _ = before;
            true
        };
        match self.kind {
            Kind::Ports => {
                if nav(&mut self.sel) {
                    return (true, Vec::new());
                }
                let Some(p) = self.ports.get(self.sel).cloned() else {
                    return (false, Vec::new());
                };
                let url = format!("http://localhost:{}/", p.port);
                match key {
                    b"\r" | b"o" => (true, vec![Act::OpenUrl(url)]),
                    b"c" => (true, vec![Act::Copy(url)]),
                    b"t" => (true, vec![Act::FocusPid(p.ancestors.clone())]),
                    b"K" | b"x" if p.pid.is_some() => {
                        self.confirm = Some((
                            format!(
                                "kill {} (pid {}) on :{}? y/n",
                                p.process,
                                p.pid.unwrap(),
                                p.port
                            ),
                            Act::Signal(p.pid.unwrap(), libc::SIGKILL),
                        ));
                        (true, Vec::new())
                    }
                    b"k" | b"\x1b[3~" if p.pid.is_some() => {
                        self.confirm = Some((
                            format!(
                                "stop {} (pid {}) on :{}? y/n",
                                p.process,
                                p.pid.unwrap(),
                                p.port
                            ),
                            Act::Signal(p.pid.unwrap(), libc::SIGTERM),
                        ));
                        (true, Vec::new())
                    }
                    _ => (false, Vec::new()),
                }
            }
            Kind::Docker => {
                if nav(&mut self.sel) {
                    return (true, Vec::new());
                }
                if key == b"a" {
                    self.all = !self.all;
                    self.next_at = Instant::now();
                    return (true, Vec::new());
                }
                let Some(tool) = self.tool else {
                    return (false, Vec::new());
                };
                let Some(c) = self
                    .docker
                    .as_ref()
                    .and_then(|d| d.as_ref().ok())
                    .and_then(|v| v.get(self.sel))
                    .cloned()
                else {
                    return (false, Vec::new());
                };
                let running = c.state == "running";
                let quiet = |args: Vec<String>| {
                    std::thread::spawn(move || {
                        let _ = Command::new(args[0].clone())
                            .args(&args[1..])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    });
                };
                let act = match key {
                    b"l" | b"\r" => Act::RunSplit(format!("{tool} logs -f --tail 200 {}", c.name)),
                    b"x" => Act::RunSplit(format!("{tool} exec -it {} sh", c.name)),
                    b"s" => {
                        let op = if running { "stop" } else { "start" };
                        quiet(vec![tool.into(), op.into(), c.name.clone()]);
                        self.next_at = Instant::now() + Duration::from_millis(800);
                        Act::Notice(format!("{tool} {op} {}", c.name))
                    }
                    b"r" => {
                        quiet(vec![tool.into(), "restart".into(), c.name.clone()]);
                        self.next_at = Instant::now() + Duration::from_millis(800);
                        Act::Notice(format!("{tool} restart {}", c.name))
                    }
                    _ => return (false, Vec::new()),
                };
                (true, vec![act])
            }
            Kind::Logs => match key {
                b"/" => {
                    self.input = Some((Field::Filter, self.filter.clone()));
                    (true, Vec::new())
                }
                b"a" => {
                    self.input = Some((Field::AddFile, String::new()));
                    (true, Vec::new())
                }
                b"x" => {
                    if let Some(f) = self.files.pop() {
                        let i = self.files.len();
                        self.lines.retain(|(k, _)| *k != i);
                        return (
                            true,
                            vec![Act::Notice(format!(
                                "stopped following {}",
                                f.path.display()
                            ))],
                        );
                    }
                    (false, Vec::new())
                }
                b" " | b"p" => {
                    self.paused = !self.paused;
                    (true, Vec::new())
                }
                b"\x1b[A" | b"\x1bOA" | b"k" => {
                    self.scroll = (self.scroll + 1).min(self.lines.len());
                    (true, Vec::new())
                }
                b"\x1b[B" | b"\x1bOB" | b"j" => {
                    self.scroll = self.scroll.saturating_sub(1);
                    (true, Vec::new())
                }
                b"\x1b[5~" => {
                    self.scroll = (self.scroll + 20).min(self.lines.len());
                    (true, Vec::new())
                }
                b"\x1b[6~" | b"G" => {
                    self.scroll = if key == b"G" {
                        0
                    } else {
                        self.scroll.saturating_sub(20)
                    };
                    (true, Vec::new())
                }
                b"\x1bOS" | b"\x1b[14~" | b"T" => {
                    // F4: what is shown, in the table viewer
                    match self.shown_lines() {
                        Ok(_) => {
                            let mut d = Data::Table(self.log_records());
                            if !self.filter.trim().is_empty() {
                                if let Ok(st) = crate::data::ops::split_pipeline(&self.filter) {
                                    for s in &st {
                                        if let Ok(x) =
                                            crate::data::ops::filter(&s[0], &s[1..], d.clone())
                                        {
                                            d = x;
                                        }
                                    }
                                }
                            }
                            (true, vec![Act::Table("logs".into(), d)])
                        }
                        Err(e) => (true, vec![Act::Notice(e)]),
                    }
                }
                _ => (false, Vec::new()),
            },
            Kind::Http => match key {
                b"m" => {
                    let i = METHODS
                        .iter()
                        .position(|m| *m == self.req.method)
                        .unwrap_or(0);
                    self.req.method = METHODS[(i + 1) % METHODS.len()].into();
                    (true, Vec::new())
                }
                b"u" | b"e" => {
                    self.input = Some((Field::Url, self.req.url.clone()));
                    (true, Vec::new())
                }
                b"h" => {
                    self.input = Some((Field::Headers, self.req.headers.clone()));
                    (true, Vec::new())
                }
                b"b" => {
                    self.input = Some((Field::Body, self.req.body.clone()));
                    (true, Vec::new())
                }
                b"\r" => {
                    self.send();
                    (true, Vec::new())
                }
                b"c" => (
                    true,
                    vec![
                        Act::Copy(curl_line(&self.req)),
                        Act::Notice("copied as a curl command".into()),
                    ],
                ),
                b"\x1b[A" | b"\x1bOA" | b"\x1b[B" | b"\x1bOB" if !self.history.is_empty() => {
                    let up = key.ends_with(b"A");
                    let last = self.history.len() - 1;
                    let pos = match (self.hist_pos, up) {
                        (None, true) => 0,
                        (None, false) => return (false, Vec::new()),
                        (Some(p), true) => (p + 1).min(last),
                        (Some(0), false) => {
                            self.hist_pos = None;
                            return (true, Vec::new());
                        }
                        (Some(p), false) => p - 1,
                    };
                    self.hist_pos = Some(pos);
                    self.req = self.history[pos].clone();
                    (true, Vec::new())
                }
                _ => (false, Vec::new()),
            },
        }
    }

    fn submit(&mut self, field: Field, text: String) -> Vec<Act> {
        match field {
            Field::Url => self.req.url = text.trim().to_string(),
            Field::Headers => self.req.headers = text,
            Field::Body => self.req.body = text,
            Field::Filter => {
                self.filter = text;
                if let Err(e) = self.shown_lines() {
                    return vec![Act::Notice(e)];
                }
            }
            Field::AddFile => {
                let t = text.trim();
                let home = std::env::var("HOME").unwrap_or_default();
                let p = match t.strip_prefix("~/") {
                    Some(r) => PathBuf::from(&home).join(r),
                    None => PathBuf::from(t),
                };
                if !p.is_file() {
                    return vec![Act::Notice(format!("no file {t}"))];
                }
                if !self.add_file(p) {
                    return vec![Act::Notice(format!("{t} isn't text — not followed"))];
                }
            }
        }
        Vec::new()
    }

    fn rows(&self) -> usize {
        match self.kind {
            Kind::Ports => self.ports.len(),
            Kind::Docker => self
                .docker
                .as_ref()
                .and_then(|d| d.as_ref().ok())
                .map(|v| v.len())
                .unwrap_or(0),
            _ => 0,
        }
    }

    // ---------------------------------------------------------------- draw

    /// `tabs`: shell pid → "tab 2 · bash", to say who started a port.
    pub fn render(&self, r: Rect, buf: &mut Buffer, focus: bool, tabs: &HashMap<i64, String>) {
        if r.width < 10 || r.height < 2 {
            return;
        }
        let w = r.width as usize;
        let mut bottom = r.bottom();
        if let Some((q, _)) = &self.confirm {
            bottom -= 1;
            buf.set_stringn(
                r.x,
                bottom,
                q,
                w,
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            );
        }
        let area = Rect::new(r.x, r.y, r.width, bottom - r.y);
        match self.kind {
            Kind::Ports => self.draw_ports(area, buf, focus, tabs),
            Kind::Docker => self.draw_docker(area, buf, focus),
            Kind::Logs => self.draw_logs(area, buf),
            Kind::Http => self.draw_http(area, buf),
        }
    }

    fn draw_ports(&self, r: Rect, buf: &mut Buffer, focus: bool, tabs: &HashMap<i64, String>) {
        let w = r.width as usize;
        let wide = w >= 70;
        let head = if wide {
            format!(
                "{:<7} {:<16} {:<8} {:<16} {}",
                "PORT", "ADDRESS", "PID", "PROCESS", "STARTED IN"
            )
        } else {
            format!("{:<7} {:<12} {}", "PORT", "PROCESS", "STARTED IN")
        };
        buf.set_stringn(r.x, r.y, head, w, Style::default().fg(DIM));
        if self.ports.is_empty() {
            let msg = if self.rx.is_some() {
                "looking…"
            } else {
                "nothing is listening"
            };
            buf.set_stringn(r.x, r.y + 1, msg, w, Style::default().fg(FAINT));
            return;
        }
        for (i, p) in self.ports.iter().enumerate().take(r.height as usize - 1) {
            let y = r.y + 1 + i as u16;
            let tab = p
                .ancestors
                .iter()
                .find_map(|a| tabs.get(a).cloned())
                .unwrap_or_else(|| "—".into());
            let line = if wide {
                format!(
                    "{:<7} {:<16} {:<8} {:<16} {}",
                    format!(":{}", p.port),
                    p.address,
                    p.pid.map(|x| x.to_string()).unwrap_or_default(),
                    cut(&p.process, 16),
                    tab
                )
            } else {
                format!(
                    "{:<7} {:<12} {}",
                    format!(":{}", p.port),
                    cut(&p.process, 12),
                    tab
                )
            };
            let mut st = Style::default().fg(TEXT);
            if i == self.sel && focus {
                st = st.bg(SEL_BG);
                buf.set_style(Rect::new(r.x, y, r.width, 1), Style::default().bg(SEL_BG));
            }
            buf.set_stringn(r.x, y, line, w, st);
            buf.set_stringn(
                r.x,
                y,
                format!(":{}", p.port),
                7,
                st.fg(CYAN).add_modifier(Modifier::BOLD),
            );
        }
    }

    fn draw_docker(&self, r: Rect, buf: &mut Buffer, focus: bool) {
        let w = r.width as usize;
        let Some(tool) = self.tool else {
            buf.set_stringn(
                r.x,
                r.y,
                "docker or podman isn't installed",
                w,
                Style::default().fg(FAINT),
            );
            return;
        };
        let list = match &self.docker {
            None => {
                buf.set_stringn(
                    r.x,
                    r.y,
                    format!("asking {tool}…"),
                    w,
                    Style::default().fg(FAINT),
                );
                return;
            }
            Some(Err(e)) => {
                buf.set_stringn(r.x, r.y, e, w, Style::default().fg(RED));
                return;
            }
            Some(Ok(v)) => v,
        };
        let wide = w >= 80;
        let which = format!("{tool}{}", if self.all { " · all" } else { " · running" });
        let head = if wide {
            format!(
                "{:<20} {:<24} {:<22} {}  ({which})",
                "NAME", "IMAGE", "STATUS", "PORTS"
            )
        } else {
            format!("{:<20} {}  ({which})", "NAME", "STATUS")
        };
        buf.set_stringn(r.x, r.y, head, w, Style::default().fg(DIM));
        if list.is_empty() {
            buf.set_stringn(
                r.x,
                r.y + 1,
                if self.all {
                    "no containers"
                } else {
                    "no running containers · a shows all"
                },
                w,
                Style::default().fg(FAINT),
            );
        }
        for (i, c) in list.iter().enumerate().take(r.height as usize - 1) {
            let y = r.y + 1 + i as u16;
            let mark = if c.state == "running" { "●" } else { "○" };
            let line = if wide {
                format!(
                    "{mark} {:<18} {:<24} {:<22} {}",
                    cut(&c.name, 18),
                    cut(&c.image, 24),
                    cut(&c.status, 22),
                    c.ports
                )
            } else {
                format!("{mark} {:<18} {}", cut(&c.name, 18), c.status)
            };
            let mut st = Style::default().fg(TEXT);
            if i == self.sel && focus {
                st = st.bg(SEL_BG);
                buf.set_style(Rect::new(r.x, y, r.width, 1), Style::default().bg(SEL_BG));
            }
            buf.set_stringn(r.x, y, line, w, st);
            let col = if c.state == "running" { GREEN } else { FAINT };
            buf.set_stringn(r.x, y, mark, 1, st.fg(col));
        }
    }

    fn draw_logs(&self, r: Rect, buf: &mut Buffer) {
        let w = r.width as usize;
        // files line
        let mut x = r.x;
        for f in &self.files {
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (nx, _) = buf.set_stringn(
                x,
                r.y,
                format!("■ {name}  "),
                (r.right().saturating_sub(x)) as usize,
                Style::default().fg(Color::Indexed(f.color)),
            );
            x = nx;
        }
        if self.files.is_empty() {
            buf.set_stringn(
                r.x,
                r.y,
                "no log files here · a adds one (or sv pane logs FILE…)",
                w,
                Style::default().fg(FAINT),
            );
        }
        let mut status = String::new();
        if self.paused {
            status.push_str("⏸ paused  ");
        }
        if self.scroll > 0 {
            status.push_str(&format!("↑ {} lines up  ", self.scroll));
        }
        let shown = self.shown_lines();
        if !self.filter.trim().is_empty() {
            if let Ok(s) = &shown {
                status.push_str(&format!("{} match  ", s.len()));
            }
        }
        if !status.is_empty() {
            let sw = status.chars().count() as u16;
            buf.set_string(
                r.right().saturating_sub(sw),
                r.y,
                &status,
                Style::default().fg(AMBER),
            );
        }
        // filter line
        let mut top = r.y + 1;
        let editing = matches!(self.input, Some((Field::Filter | Field::AddFile, _)));
        if editing || !self.filter.is_empty() {
            let (label, text) = match &self.input {
                Some((Field::AddFile, t)) => ("add file ", format!("{t}▏")),
                Some((Field::Filter, t)) => ("filter ", format!("{t}▏")),
                _ => ("filter ", self.filter.clone()),
            };
            let (fx, _) = buf.set_stringn(r.x, top, label, 9, Style::default().fg(DIM));
            buf.set_stringn(
                fx,
                top,
                &text,
                w.saturating_sub(9),
                Style::default().bg(FIELD_BG).fg(TEXT),
            );
            top += 1;
        }
        let lines = match shown {
            Ok(l) => l,
            Err(e) => {
                buf.set_stringn(r.x, top, e, w, Style::default().fg(RED));
                return;
            }
        };
        let h = r.bottom().saturating_sub(top) as usize;
        let end = lines.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(h);
        for (k, (fi, l)) in lines[start..end].iter().enumerate() {
            let y = top + k as u16;
            let col = self.files.get(*fi).map(|f| f.color).unwrap_or(245);
            buf.set_string(r.x, y, "▏", Style::default().fg(Color::Indexed(col)));
            let st = match level_of(l) {
                "error" => Style::default().fg(RED),
                "warn" => Style::default().fg(AMBER),
                "debug" => Style::default().fg(FAINT),
                _ => Style::default().fg(TEXT),
            };
            buf.set_stringn(r.x + 1, y, l, w.saturating_sub(1), st);
        }
    }

    fn draw_http(&self, r: Rect, buf: &mut Buffer) {
        let w = r.width as usize;
        let d = Style::default().fg(DIM);
        let field = |buf: &mut Buffer, y: u16, label: &str, f: Field, val: &str| -> u16 {
            buf.set_stringn(r.x, y, label, 9, d);
            let editing = matches!(&self.input, Some((g, _)) if *g == f);
            let text = match &self.input {
                Some((g, t)) if *g == f => format!("{t}▏"),
                _ => val.to_string(),
            };
            let lines: Vec<&str> = if text.is_empty() {
                vec![""]
            } else {
                text.lines().collect()
            };
            let n = lines.len().clamp(1, 5) as u16;
            for (i, l) in lines.iter().take(5).enumerate() {
                let st = if editing {
                    Style::default().bg(FIELD_BG).fg(TEXT)
                } else {
                    Style::default().fg(TEXT)
                };
                buf.set_stringn(r.x + 9, y + i as u16, l, w.saturating_sub(9), st);
            }
            n
        };
        let mut y = r.y;
        if r.height < 14 && self.input.is_none() {
            // short pane: the request on one line, the room for the answer
            let (mx, _) = buf.set_stringn(
                r.x,
                y,
                format!(" {} ", self.req.method),
                10,
                Style::default()
                    .fg(Color::Indexed(16))
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD),
            );
            let extra = match (self.req.headers.lines().count(), self.req.body.is_empty()) {
                (0, true) => String::new(),
                (h, true) => format!("  · {h} header(s)"),
                (h, false) => format!("  · {h} header(s) · body"),
            };
            buf.set_stringn(
                mx + 1,
                y,
                format!("{}{extra}", self.req.url),
                w.saturating_sub(10),
                Style::default().fg(TEXT),
            );
            y += 1;
            return self.draw_answer(
                Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y)),
                buf,
            );
        }
        buf.set_stringn(r.x, y, "method", 9, d);
        buf.set_stringn(
            r.x + 9,
            y,
            format!(" {} ", self.req.method),
            10,
            Style::default()
                .fg(Color::Indexed(16))
                .bg(AMBER)
                .add_modifier(Modifier::BOLD),
        );
        y += 1;
        y += field(buf, y, "url", Field::Url, &self.req.url);
        let hdr = if self.req.headers.is_empty() && !matches!(self.input, Some((Field::Headers, _)))
        {
            "— (h: one per line, Name: value)".to_string()
        } else {
            self.req.headers.clone()
        };
        y += field(buf, y, "headers", Field::Headers, &hdr);
        let body = if self.req.body.is_empty() && !matches!(self.input, Some((Field::Body, _))) {
            "— (b; ctrl+j for a new line)".to_string()
        } else {
            self.req.body.clone()
        };
        y += field(buf, y, "body", Field::Body, &body);
        if let Some(p) = self.hist_pos {
            buf.set_stringn(
                r.x,
                y,
                format!("history {} of {}", p + 1, self.history.len()),
                w,
                Style::default().fg(FAINT),
            );
        }
        y += 1;
        if y >= r.bottom() {
            return;
        }
        self.draw_answer(
            Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y)),
            buf,
        )
    }

    fn draw_answer(&self, r: Rect, buf: &mut Buffer) {
        let w = r.width as usize;
        let d = Style::default().fg(DIM);
        let mut y = r.y;
        if r.height == 0 {
            return;
        }
        buf.set_stringn(
            r.x,
            y,
            "─".repeat(w),
            w,
            Style::default().fg(Color::Indexed(238)),
        );
        y += 1;
        if let Some(at) = self.sending {
            buf.set_stringn(
                r.x,
                y,
                format!("sending… {:.1}s", at.elapsed().as_secs_f64()),
                w,
                Style::default().fg(CYAN),
            );
            return;
        }
        let Some(resp) = &self.resp else {
            buf.set_stringn(r.x, y, "⏎ sends it", w, Style::default().fg(FAINT));
            return;
        };
        if let Some(e) = &resp.error {
            buf.set_stringn(r.x, y, e, w, Style::default().fg(RED));
            return;
        }
        let code: u32 = resp
            .status
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let col = match code {
            200..=299 => GREEN,
            300..=399 => CYAN,
            400..=499 => AMBER,
            _ => RED,
        };
        let (nx, _) = buf.set_stringn(
            r.x,
            y,
            &resp.status,
            w,
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        );
        let size: usize = resp.body.iter().map(|l| l.len() + 1).sum();
        buf.set_stringn(nx + 2, y, format!("{} ms · {}", resp.ms, human(size)), w, d);
        y += 1;
        // the body comes first; headers in full only when there is room
        let left = r.bottom().saturating_sub(y) as usize;
        if left > resp.body.len() + resp.headers.len().min(4) {
            for h in resp.headers.iter().take(4) {
                buf.set_stringn(r.x, y, h, w, d);
                y += 1;
            }
        } else if !resp.headers.is_empty() && left > 2 {
            let ct = resp
                .headers
                .iter()
                .find(|h| h.to_lowercase().starts_with("content-type"))
                .cloned()
                .unwrap_or_default();
            buf.set_stringn(
                r.x,
                y,
                format!("{} headers · {ct}", resp.headers.len()),
                w,
                d,
            );
            y += 1;
        }
        for l in &resp.body {
            if y >= r.bottom() {
                return;
            }
            let st = if l.trim_start().starts_with('"') {
                Style::default().fg(CYAN)
            } else {
                Style::default().fg(TEXT)
            };
            buf.set_stringn(r.x, y, l, w, st);
            y += 1;
        }
    }
}

/// Looks like text: no NUL bytes and almost all valid UTF-8 in the first 8 KB.
pub fn is_text(path: &Path) -> bool {
    use std::io::Read;
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut b = Vec::new();
    if f.take(8192).read_to_end(&mut b).is_err() {
        return false;
    }
    if b.contains(&0) {
        return false;
    }
    let bad = String::from_utf8_lossy(&b).matches('\u{fffd}').count();
    bad * 100 <= b.len().max(1)
}

/// A log line safe to draw: escape sequences and control characters out,
/// tabs as spaces.
pub fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // CSI … final byte, or OSC … BEL / ST: dropped
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        for d in chars.by_ref() {
                            if ('@'..='~').contains(&d) {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        while let Some(d) = chars.next() {
                            if d == '\x07' || (d == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                                break;
                            }
                        }
                    }
                    _ => {
                        chars.next();
                    }
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

fn human(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn read_from(path: &Path, offset: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut b = Vec::new();
    f.take(4 * 1024 * 1024).read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// `*.log` files here and in `logs/` (newest first, at most 6).
pub fn find_logs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for d in [dir.to_path_buf(), dir.join("logs"), dir.join("log")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "log") && p.is_file() && is_text(&p) {
                    let t = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    v.push((t, p));
                }
            }
        }
    }
    v.sort();
    v.into_iter().rev().take(6).map(|(_, p)| p).collect()
}

/// error · warn · info · debug · "" — from the usual words in a line.
pub fn level_of(line: &str) -> &'static str {
    let up = line.to_uppercase();
    let has = |w: &str| {
        up.match_indices(w).any(|(i, _)| {
            let before = up[..i].chars().last();
            let after = up[i + w.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric())
                && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
    };
    if has("ERROR")
        || has("ERR")
        || has("FATAL")
        || has("PANIC")
        || has("CRITICAL")
        || has("EXCEPTION")
    {
        "error"
    } else if has("WARN") || has("WARNING") {
        "warn"
    } else if has("DEBUG") || has("TRACE") {
        "debug"
    } else if has("INFO") {
        "info"
    } else {
        ""
    }
}

/// A time at the start of the line (`12:01:02`, `2026-11-07T12:01:02`…).
pub fn time_of(line: &str) -> String {
    let re = regex_lite::Regex::new(r"\d{1,2}:\d{2}:\d{2}(?:[.,]\d+)?").unwrap();
    re.find(&line[..line.len().min(40)])
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// Listening ports, with each owner's parent chain.
fn read_ports() -> Vec<Port> {
    let rows = match crate::data::sources::source("ports", &[]) {
        Ok(d) => d.into_rows(),
        Err(_) => Vec::new(),
    };
    let mut out: Vec<Port> = Vec::new();
    for r in rows {
        let get = |k: &str| r.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let port = match get("port") {
            Some(Value::Int(p)) => p,
            _ => continue,
        };
        let pid = match get("pid") {
            Some(Value::Int(p)) => Some(p),
            _ => None,
        };
        let address = match get("address") {
            Some(Value::Str(s)) => s,
            _ => String::new(),
        };
        // one row per port (v4 and v6 of the same server)
        if out.iter().any(|p| p.port == port && p.pid == pid) {
            continue;
        }
        let process = match get("process") {
            Some(Value::Str(s)) => s,
            _ => String::new(),
        };
        out.push(Port {
            port,
            address,
            pid,
            process,
            ancestors: pid.map(ancestors).unwrap_or_default(),
        });
    }
    out
}

fn parent_of(pid: i64) -> Option<i64> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let after = stat.rsplit_once(')')?.1;
        return after.split_whitespace().nth(1)?.parse().ok();
    }
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn ancestors(pid: i64) -> Vec<i64> {
    let mut v = vec![pid];
    let mut cur = pid;
    for _ in 0..12 {
        match parent_of(cur) {
            Some(p) if p > 1 && !v.contains(&p) => {
                v.push(p);
                cur = p;
            }
            _ => break,
        }
    }
    v
}

fn read_containers(tool: Option<&str>, all: bool) -> Result<Vec<Container>, String> {
    let tool = tool.ok_or("docker or podman isn't installed")?;
    let mut cmd = Command::new(tool);
    cmd.args(["ps", "--format", "{{json .}}"]);
    if all {
        cmd.arg("-a");
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        let first = e.lines().next().unwrap_or("").trim();
        return Err(if first.contains("daemon") || first.contains("connect") {
            format!("{tool} isn't running: {first}")
        } else {
            format!("{tool}: {first}")
        });
    }
    Ok(parse_containers(&String::from_utf8_lossy(&out.stdout)))
}

/// `docker ps --format '{{json .}}'` lines (podman's names differ a little).
pub fn parse_containers(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| {
            let s = |k: &str| match &v[k] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
                    .join(","),
                serde_json::Value::Null => String::new(),
                x => x.to_string(),
            };
            Container {
                id: s("ID"),
                name: {
                    let n = s("Names");
                    if n.is_empty() { s("Name") } else { n }
                },
                image: s("Image"),
                state: s("State").to_lowercase(),
                status: s("Status"),
                ports: s("Ports"),
            }
        })
        .collect()
}

/// The request as one `curl` command line.
pub fn curl_line(r: &Request) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut s = format!("curl -sS -X {} {}", r.method, q(&r.url));
    for h in r.headers.lines().filter(|h| h.contains(':')) {
        s.push_str(&format!(" -H {}", q(h.trim())));
    }
    if !r.body.is_empty() {
        s.push_str(&format!(" --data-binary {}", q(&r.body)));
    }
    s
}

fn run_request(r: &Request) -> Response {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-i", "--max-time", "30", "-X", &r.method]);
    if r.method == "HEAD" {
        cmd.arg("-I");
    }
    for h in r.headers.lines().filter(|h| h.contains(':')) {
        cmd.args(["-H", h.trim()]);
    }
    if !r.body.is_empty() {
        cmd.args(["--data-binary", "@-"]);
    }
    cmd.arg(&r.url);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let t0 = Instant::now();
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Response {
                error: Some(format!("curl: {e}")),
                ..Default::default()
            };
        }
    };
    let mut child = child;
    if !r.body.is_empty() {
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(r.body.as_bytes());
        }
    } else {
        drop(child.stdin.take());
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return Response {
                error: Some(format!("curl: {e}")),
                ..Default::default()
            };
        }
    };
    let ms = t0.elapsed().as_millis() as u64;
    if !out.status.success() && out.stdout.is_empty() {
        return Response {
            ms,
            error: Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            ..Default::default()
        };
    }
    let mut resp = parse_response(&String::from_utf8_lossy(&out.stdout));
    resp.ms = ms;
    resp
}

/// `curl -i` output: status line, headers, blank line, body (JSON pretty-printed).
pub fn parse_response(text: &str) -> Response {
    let text = text.replace("\r\n", "\n");
    // skip "100 Continue" and proxy preambles: the last status block wins
    let mut rest = text.as_str();
    while let Some((_, body)) = rest
        .split_once("\n\n")
        .filter(|(_, b)| b.starts_with("HTTP/"))
    {
        rest = body;
    }
    let (head, body) = rest.split_once("\n\n").unwrap_or((rest, ""));
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("").trim().to_string();
    let headers: Vec<String> = lines.map(|l| l.trim().to_string()).collect();
    let pretty = serde_json::from_str::<serde_json::Value>(body.trim())
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok());
    let body: Vec<String> = pretty
        .as_deref()
        .unwrap_or(body)
        .lines()
        .map(|l| l.to_string())
        .collect();
    Response {
        status,
        ms: 0,
        headers,
        body,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_times_and_responses() {
        assert_eq!(level_of("2026-11-07 12:01:02 ERROR db down"), "error");
        assert_eq!(level_of("[warn] slow query"), "warn");
        assert_eq!(level_of("terrors of the deep"), "");
        assert_eq!(level_of("DEBUG x"), "debug");
        assert_eq!(time_of("2026-11-07T12:01:02.123Z INFO up"), "12:01:02.123");
        let r = parse_response(
            "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Type: application/json\r\n\r\n{\"id\":7,\"ok\":true}",
        );
        assert_eq!(r.status, "HTTP/1.1 201 Created");
        assert_eq!(r.headers, ["Content-Type: application/json"]);
        assert_eq!(r.body, ["{", "  \"id\": 7,", "  \"ok\": true", "}"]);
        let c = curl_line(&Request {
            method: "POST".into(),
            url: "http://localhost:3000/alerts".into(),
            headers: "Content-Type: application/json\n".into(),
            body: "{\"it's\":1}".into(),
        });
        assert_eq!(
            c,
            "curl -sS -X POST 'http://localhost:3000/alerts' -H 'Content-Type: application/json' --data-binary '{\"it'\\''s\":1}'"
        );
    }

    #[test]
    fn containers_from_docker_and_podman() {
        let d = parse_containers(
            "{\"ID\":\"ab12\",\"Names\":\"db\",\"Image\":\"postgres:16\",\"State\":\"running\",\"Status\":\"Up 2 hours\",\"Ports\":\"0.0.0.0:5432->5432/tcp\"}\n\
             {\"Id\":\"cd34\",\"Names\":[\"cache\"],\"Image\":\"redis\",\"State\":\"exited\",\"Status\":\"Exited (0)\",\"Ports\":null}",
        );
        assert_eq!(d.len(), 2);
        assert_eq!((d[0].name.as_str(), d[0].state.as_str()), ("db", "running"));
        assert_eq!((d[1].name.as_str(), d[1].ports.as_str()), ("cache", ""));
    }

    #[test]
    fn logs_follow_files_and_filter_with_the_table_engine() {
        let dir = std::env::temp_dir().join(format!("sv-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("api.log");
        let b = dir.join("worker.log");
        std::fs::write(&a, "12:00:01 INFO started\n12:00:02 ERROR db down\n").unwrap();
        std::fs::write(&b, "12:00:03 WARN slow job\n").unwrap();
        let mut p = SvcPane::new(Kind::Logs, &dir, &[]);
        assert_eq!(p.files.len(), 2);
        assert_eq!(p.lines.len(), 3);
        let first: Vec<&str> = p.lines.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(first[2], "12:00:03 WARN slow job", "interleaved by time");
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        write!(f, "12:00:04 ERROR again\npartial").unwrap();
        p.next_at = Instant::now();
        assert!(p.poll(false));
        assert_eq!(p.lines.len(), 4, "a partial line waits for its newline");
        p.filter = "where level == error".into();
        let shown: Vec<String> = p.shown_lines().unwrap().into_iter().map(|x| x.1).collect();
        assert_eq!(shown, ["12:00:02 ERROR db down", "12:00:04 ERROR again"]);
        p.filter = "slow".into();
        assert_eq!(p.shown_lines().unwrap().len(), 1);
        // binary files aren't followed; colours in a line are taken out
        std::fs::write(dir.join("blob.log"), [0u8, 159, 146, 150, 1, 2]).unwrap();
        assert!(!p.add_file(dir.join("blob.log")));
        assert_eq!(
            clean("\x1b[31mERROR\x1b[0m\tdown\x1b]0;t\x07!"),
            "ERROR    down!"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

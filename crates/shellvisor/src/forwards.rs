//! Board 20: ssh port forwards — local (-L), remote (-R) and SOCKS (-D)
//! tunnels for your saved sessions, with their state, connections and
//! traffic, added and closed without reconnecting.
//!
//! Each forward runs its own `ssh -N` (your keys, agent and ~/.ssh/config
//! apply; no password prompts, it runs in batch mode) and Shellvisor relays
//! the connections through a small proxy of its own, which is how it counts
//! connections and bytes. The ssh session you type in is never touched.
//!
//! Opened with `ctrl+] P` or `sv ssh forward`. Saved forwards live with the
//! session in ~/.config/shellvisor/sessions.yaml; the ones marked "open on
//! login" start when the session's tab opens.

use crate::mcpscreen::{
    AMBER, BAR, BORDER, CYAN, DIM, GREEN, INK, PANEL, RED, SELECT, TEXT, boxed, fit, hms, put,
    put_n, put_right, put_w, ratio, st,
};
use crate::palette::{Role, role};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAGENTA: Color = Color::Indexed(170);
const BLUE: Color = Color::Indexed(110);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// -L: a port here goes to a host:port the server can reach.
    Local,
    /// -R: a port on the server comes back to a host:port here.
    Remote,
    /// -D: a SOCKS proxy here, through the server.
    Socks,
}

impl Kind {
    pub fn letter(self) -> &'static str {
        match self {
            Kind::Local => "L",
            Kind::Remote => "R",
            Kind::Socks => "D",
        }
    }
    fn color(self) -> Color {
        match self {
            Kind::Local => CYAN,
            Kind::Remote => MAGENTA,
            Kind::Socks => BLUE,
        }
    }
}

/// One forward, as ssh writes it: `-L 5433:db.internal:5432`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub kind: Kind,
    /// The port that listens (here for L / D, on the server for R).
    pub port: u16,
    /// Where it goes (unused for D).
    pub host: String,
    pub to_port: u16,
}

impl Spec {
    /// Parse "-L 8080:localhost:80", "-L127.0.0.1:8080:host:80", "-D 1080", "-R 9000:localhost:3000".
    pub fn parse(s: &str) -> Option<Spec> {
        let s = s.trim();
        let (flag, rest) = s.split_at(2.min(s.len()));
        let kind = match flag {
            "-L" => Kind::Local,
            "-R" => Kind::Remote,
            "-D" => Kind::Socks,
            _ => return None,
        };
        let parts: Vec<&str> = rest.trim().split(':').collect();
        let num = |x: &str| x.trim().parse::<u16>().ok();
        match (kind, parts.as_slice()) {
            (Kind::Socks, [p]) | (Kind::Socks, [_, p]) => Some(Spec {
                kind,
                port: num(p)?,
                host: String::new(),
                to_port: 0,
            }),
            (Kind::Socks, _) => None,
            (_, [p, h, tp]) | (_, [_, p, h, tp]) => Some(Spec {
                kind,
                port: num(p)?,
                host: h.trim_matches(|c| c == '[' || c == ']').to_string(),
                to_port: num(tp)?,
            }),
            _ => None,
        }
    }

    pub fn to_arg(&self) -> String {
        match self.kind {
            Kind::Socks => format!("-D {}", self.port),
            k => format!(
                "-{} {}:{}:{}",
                k.letter(),
                self.port,
                self.host,
                self.to_port
            ),
        }
    }

    /// "localhost:5433" / "prod-web:9000" — where it listens.
    pub fn listen_text(&self, session: &str) -> String {
        match self.kind {
            Kind::Remote => format!("{session}:{}", self.port),
            _ => format!("localhost:{}", self.port),
        }
    }

    /// "db.internal:5432" / "your localhost:3000" / "SOCKS proxy through prod-web".
    pub fn dest_text(&self, session: &str) -> String {
        match self.kind {
            Kind::Local => format!("{}:{}", self.host, self.to_port),
            Kind::Remote => {
                let h = if self.host == "localhost" || self.host == "127.0.0.1" {
                    "your localhost".to_string()
                } else {
                    self.host.clone()
                };
                format!("{h}:{}", self.to_port)
            }
            Kind::Socks => format!("SOCKS proxy through {session}"),
        }
    }
}

/// A well-known service for a port, to label a forward.
pub fn service(port: u16) -> &'static str {
    match port {
        22 => "ssh",
        80 | 8080 => "http",
        443 => "https",
        3000 => "dev server",
        3306 => "mysql",
        5432 => "postgres",
        6379 => "redis",
        9200 => "elasticsearch",
        27017 => "mongodb",
        _ => "",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Starting,
    Listening,
    /// The port is taken: by what.
    Busy(String),
    Paused,
    Failed(String),
}

/// A running (or stopped) forward.
pub struct Live {
    pub session: String,
    pub spec: Spec,
    pub state: Arc<Mutex<State>>,
    pub conns: Arc<AtomicUsize>,
    /// Bytes from the far side to here, and from here out.
    pub bytes_in: Arc<AtomicU64>,
    pub bytes_out: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Started with this session's tab: stopped when the tab's shell goes.
    pub with_pane: Option<u32>,
}

impl Live {
    pub fn state(&self) -> State {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(State::Starting)
    }

    /// Stop listening and end its ssh.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.child.lock() {
            if let Some(ch) = c.as_mut() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
            *c = None;
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop();
    }
}

fn ssh_program() -> String {
    std::env::var("SHELLVISOR_SSH").unwrap_or_else(|_| "ssh".into())
}

/// A free port on 127.0.0.1 (for ssh's own end of the tunnel).
fn free_port() -> Option<u16> {
    TcpListener::bind(("127.0.0.1", 0))
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

/// What holds a port: "ssh -D (pid 9112)", from lsof and ps.
pub fn port_holder(port: u16) -> String {
    let pid = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|l| l.trim().to_string())
        })
        .filter(|p| !p.is_empty());
    let Some(pid) = pid else {
        return "another program".into();
    };
    let args = Command::new("ps")
        .args(["-o", "args=", "-p", &pid])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut w = args.split_whitespace();
    let prog = w
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let flag = w.find(|x| x.len() == 2 && x.starts_with('-') && x != &"-o");
    match (prog.is_empty(), flag) {
        (true, _) => format!("pid {pid}"),
        (false, Some(f)) => format!("{prog} {f} (pid {pid})"),
        (false, None) => format!("{prog} (pid {pid})"),
    }
}

/// Copy one way, counting bytes; on EOF shut the other side's write half.
fn pump(mut from: TcpStream, mut to: TcpStream, count: Arc<AtomicU64>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
                count.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
    let _ = to.shutdown(std::net::Shutdown::Write);
}

/// Relay one accepted connection to `addr` (retrying while ssh comes up).
fn relay(
    client: TcpStream,
    addr: (String, u16),
    conns: Arc<AtomicUsize>,
    bin: Arc<AtomicU64>,
    bout: Arc<AtomicU64>,
) {
    let mut far = None;
    for _ in 0..60 {
        if let Ok(s) = TcpStream::connect((addr.0.as_str(), addr.1)) {
            far = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let Some(far) = far else { return };
    let _ = client.set_nodelay(true);
    let _ = far.set_nodelay(true);
    conns.fetch_add(1, Ordering::Relaxed);
    let (Ok(c2), Ok(f2)) = (client.try_clone(), far.try_clone()) else {
        conns.fetch_sub(1, Ordering::Relaxed);
        return;
    };
    let up = std::thread::spawn(move || pump(c2, f2, bout));
    pump(far, client, bin);
    let _ = up.join();
    conns.fetch_sub(1, Ordering::Relaxed);
}

/// Accept on `l` until stopped, relaying each connection to `addr`.
fn accept_loop(
    l: TcpListener,
    addr: (String, u16),
    stop: Arc<AtomicBool>,
    conns: Arc<AtomicUsize>,
    bin: Arc<AtomicU64>,
    bout: Arc<AtomicU64>,
) {
    let _ = l.set_nonblocking(true);
    while !stop.load(Ordering::Relaxed) {
        match l.accept() {
            Ok((c, _)) => {
                let _ = c.set_nonblocking(false);
                let (a, n, i, o) = (addr.clone(), conns.clone(), bin.clone(), bout.clone());
                std::thread::spawn(move || relay(c, a, n, i, o));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Explain why ssh ended.
fn explain(stderr: &str) -> String {
    let e = stderr.trim();
    if e.contains("Permission denied") || e.contains("password") {
        "the server wants a password: forwards need a key or ssh-agent login".into()
    } else if e.contains("remote port forwarding failed") {
        "that port is busy on the server".into()
    } else if e.is_empty() {
        "ssh ended".into()
    } else {
        e.lines().last().unwrap_or(e).to_string()
    }
}

/// Start a forward for a session (its ssh options and destination).
pub fn start(session: &str, ssh_args: &[String], spec: Spec) -> Live {
    let live = Live {
        session: session.to_string(),
        spec: spec.clone(),
        state: Arc::new(Mutex::new(State::Starting)),
        conns: Arc::new(AtomicUsize::new(0)),
        bytes_in: Arc::new(AtomicU64::new(0)),
        bytes_out: Arc::new(AtomicU64::new(0)),
        stop: Arc::new(AtomicBool::new(false)),
        child: Arc::new(Mutex::new(None)),
        with_pane: None,
    };
    let set = |s: State| {
        if let Ok(mut st) = live.state.lock() {
            *st = s;
        }
    };
    // our end: where people connect (L, D), or where ssh brings R back to
    let (listener, ssh_spec, relay_to) = match spec.kind {
        Kind::Local | Kind::Socks => {
            let l = match TcpListener::bind(("127.0.0.1", spec.port)) {
                Ok(l) => l,
                Err(_) => {
                    set(State::Busy(port_holder(spec.port)));
                    return live;
                }
            };
            let Some(p2) = free_port() else {
                set(State::Failed("no free port for the tunnel".into()));
                return live;
            };
            let s = match spec.kind {
                Kind::Socks => format!("127.0.0.1:{p2}"),
                _ => format!("127.0.0.1:{p2}:{}:{}", spec.host, spec.to_port),
            };
            let flag = if spec.kind == Kind::Socks { "-D" } else { "-L" };
            (l, vec![flag.to_string(), s], ("127.0.0.1".to_string(), p2))
        }
        Kind::Remote => {
            let Ok(l) = TcpListener::bind(("127.0.0.1", 0)) else {
                set(State::Failed("no free port for the tunnel".into()));
                return live;
            };
            let p2 = l.local_addr().map(|a| a.port()).unwrap_or(0);
            (
                l,
                vec!["-R".to_string(), format!("{}:127.0.0.1:{p2}", spec.port)],
                (spec.host.clone(), spec.to_port),
            )
        }
    };
    let mut cmd = Command::new(ssh_program());
    cmd.args([
        "-N",
        "-o",
        "BatchMode=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ControlMaster=no",
    ]);
    cmd.args(&ssh_spec);
    cmd.args(ssh_args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            set(State::Failed(format!("can't run ssh: {e}")));
            return live;
        }
    };
    let stderr = {
        let mut c = child;
        let e = c.stderr.take();
        if let Ok(mut slot) = live.child.lock() {
            *slot = Some(c);
        }
        e
    };
    set(State::Listening);
    // ssh ending on its own is a failure (unless we stopped it)
    {
        let (stop, state, child) = (live.stop.clone(), live.state.clone(), live.child.clone());
        std::thread::spawn(move || {
            let mut err = String::new();
            if let Some(mut e) = stderr {
                let _ = e.read_to_string(&mut err);
            }
            // stderr closes when ssh exits
            if let Ok(mut c) = child.lock() {
                if let Some(ch) = c.as_mut() {
                    let _ = ch.wait();
                }
            }
            if !stop.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                if let Ok(mut s) = state.lock() {
                    *s = State::Failed(explain(&err));
                }
            }
        });
    }
    {
        let (stop, n, i, o) = (
            live.stop.clone(),
            live.conns.clone(),
            live.bytes_in.clone(),
            live.bytes_out.clone(),
        );
        std::thread::spawn(move || accept_loop(listener, relay_to, stop, n, i, o));
    }
    live
}

// ------------------------------------------------------------------ the screen

/// What the screen shows about one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessInfo {
    pub name: String,
    pub address: String,
    pub keepalive: u32,
    /// The tab its ssh runs in (1-based), when open.
    pub tab: Option<usize>,
    /// Saved forwards and whether each opens on login.
    pub saved: Vec<(Spec, bool)>,
}

/// A forward as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub spec: Spec,
    pub state: Option<State>,
    pub conns: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub saved: bool,
    pub auto: bool,
}

pub struct Snap {
    pub sessions: Vec<SessInfo>,
    /// Rows for every session, by name.
    pub rows: Vec<(String, Row)>,
}

impl Snap {
    pub fn rows_of(&self, s: &str) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|(n, _)| n == s)
            .map(|(_, r)| r)
            .collect()
    }
}

/// What the screen asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum FwdAct {
    Close,
    OpenSession(String),
    Add {
        session: String,
        spec: Spec,
        save: bool,
        auto: bool,
    },
    /// Pause or resume.
    Toggle(String, Spec),
    /// Stop it and forget it.
    Remove(String, Spec),
    /// A busy port: move it to the next free one.
    Repick(String, Spec),
    Copy(String),
    OpenUrl(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddForm {
    pub kind: Kind,
    pub listen: String,
    pub dest: String,
    pub save: bool,
    pub auto: bool,
    /// 0 type, 1 listen on, 2 goes to, 3 save, 4 open on login, 5 button
    pub field: usize,
}

impl Default for AddForm {
    fn default() -> Self {
        AddForm {
            kind: Kind::Local,
            listen: String::new(),
            dest: String::new(),
            save: true,
            auto: false,
            field: 0,
        }
    }
}

impl AddForm {
    fn spec(&self) -> Result<Spec, String> {
        let port: u16 = self
            .listen
            .rsplit(':')
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .map_err(|_| "listen on: a port number")?;
        if self.kind == Kind::Socks {
            return Ok(Spec {
                kind: Kind::Socks,
                port,
                host: String::new(),
                to_port: 0,
            });
        }
        let (h, p) = self
            .dest
            .trim()
            .rsplit_once(':')
            .ok_or("goes to: host:port")?;
        let to_port: u16 = p.parse().map_err(|_| "goes to: host:port")?;
        if h.is_empty() {
            return Err("goes to: host:port".into());
        }
        Ok(Spec {
            kind: self.kind,
            port,
            host: h.to_string(),
            to_port,
        })
    }
}

pub struct FwdScreen {
    pub sel: usize,
    pub row: usize,
    /// 0 sessions, 1 forwards, 2 add form
    pub area: u8,
    pub form: AddForm,
    pub note: Option<String>,
}

impl FwdScreen {
    pub fn new(sel: usize) -> Self {
        FwdScreen {
            sel,
            row: 0,
            area: 1,
            form: AddForm::default(),
            note: None,
        }
    }

    pub fn key(&mut self, key: &[u8], snap: &Snap) -> Option<FwdAct> {
        self.note = None;
        let n = snap.sessions.len();
        let Some(sess) = snap.sessions.get(self.sel.min(n.saturating_sub(1))) else {
            return matches!(key, b"\x1b" | b"q").then_some(FwdAct::Close);
        };
        let name = sess.name.clone();
        let rows = snap.rows_of(&name);
        if key == b"\x1b\x1b" {
            return Some(FwdAct::Close);
        }
        // typing into the form
        if self.area == 2 && matches!(self.form.field, 1 | 2) {
            let f = &mut self.form;
            let text = if f.field == 1 {
                &mut f.listen
            } else {
                &mut f.dest
            };
            match key {
                [0x7f] | [0x08] => {
                    text.pop();
                    return None;
                }
                k if k.first().is_some_and(|b| *b > 0x20) && !k.starts_with(b"\x1b") => {
                    text.push_str(&String::from_utf8_lossy(k));
                    return None;
                }
                _ => {}
            }
        }
        match key {
            b"\x1b" | b"q" if self.area == 2 => self.area = 1,
            b"\x1b" | b"q" => return Some(FwdAct::Close),
            b"\t" => self.area = (self.area + 1) % 3,
            b"\x1b[Z" => self.area = (self.area + 2) % 3,
            b"a" if self.area != 2 => {
                self.area = 2;
                self.form = AddForm::default();
            }
            _ => match self.area {
                0 => match key {
                    b"\x1b[A" | b"k" => {
                        self.sel = self.sel.saturating_sub(1);
                        self.row = 0;
                    }
                    b"\x1b[B" | b"j" => {
                        self.sel = (self.sel + 1).min(n.saturating_sub(1));
                        self.row = 0;
                    }
                    b"\r" => return Some(FwdAct::OpenSession(name)),
                    _ => {}
                },
                1 => {
                    let r = rows.get(self.row).copied();
                    match key {
                        b"\x1b[A" | b"k" => self.row = self.row.saturating_sub(1),
                        b"\x1b[B" | b"j" => {
                            self.row = (self.row + 1).min(rows.len().saturating_sub(1))
                        }
                        b"\x1b[D" | b"h" => self.area = 0,
                        b"\r" => {
                            if let Some(r) = r {
                                if r.spec.kind == Kind::Local {
                                    return Some(FwdAct::OpenUrl(format!(
                                        "http://localhost:{}",
                                        r.spec.port
                                    )));
                                }
                                self.note = Some("⏎ opens local forwards in the browser".into());
                            }
                        }
                        b"c" => {
                            if let Some(r) = r {
                                return Some(FwdAct::Copy(match r.spec.kind {
                                    Kind::Remote => format!("{}:{}", name, r.spec.port),
                                    _ => format!("localhost:{}", r.spec.port),
                                }));
                            }
                        }
                        b"x" => {
                            if let Some(r) = r {
                                return Some(FwdAct::Remove(name, r.spec.clone()));
                            }
                        }
                        b" " => {
                            if let Some(r) = r {
                                return Some(FwdAct::Toggle(name, r.spec.clone()));
                            }
                        }
                        b"p" => {
                            if let Some(r) = r.filter(|r| matches!(r.state, Some(State::Busy(_)))) {
                                return Some(FwdAct::Repick(name, r.spec.clone()));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    let f = &mut self.form;
                    match key {
                        b"\x1b[A" => f.field = f.field.saturating_sub(1),
                        b"\x1b[B" => {
                            f.field = (f.field + 1).min(5);
                            if f.kind == Kind::Socks && f.field == 2 {
                                f.field = 3;
                            }
                        }
                        b"\x1b[D" | b"\x1b[C" if f.field == 0 => {
                            let order = [Kind::Local, Kind::Remote, Kind::Socks];
                            let i = order.iter().position(|k| *k == f.kind).unwrap_or(0);
                            let j = if key == b"\x1b[C" {
                                (i + 1) % 3
                            } else {
                                (i + 2) % 3
                            };
                            f.kind = order[j];
                        }
                        b"\x1b[D" | b"\x1b[C" if matches!(f.field, 3 | 4) => {
                            f.field = if f.field == 3 { 4 } else { 3 };
                        }
                        b"l" if f.field == 0 => f.kind = Kind::Local,
                        b"r" if f.field == 0 => f.kind = Kind::Remote,
                        b"d" if f.field == 0 => f.kind = Kind::Socks,
                        b" " if f.field == 3 => f.save = !f.save,
                        b" " if f.field == 4 => f.auto = !f.auto,
                        b"\r" => {
                            if f.field < 5 && f.field != 4 && f.field != 3 {
                                f.field += 1;
                                if f.kind == Kind::Socks && f.field == 2 {
                                    f.field = 3;
                                }
                                if f.field < 5 {
                                    return None;
                                }
                            }
                            match f.spec() {
                                Ok(spec) => {
                                    let act = FwdAct::Add {
                                        session: name,
                                        spec,
                                        save: f.save,
                                        auto: f.auto && f.save,
                                    };
                                    self.form = AddForm::default();
                                    self.area = 1;
                                    return Some(act);
                                }
                                Err(e) => self.note = Some(e),
                            }
                        }
                        _ => {}
                    }
                }
            },
        }
        None
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, snap: &Snap) {
        let shell = role(Role::Shell);
        // header
        let hr = Rect::new(area.x, area.y, area.width, 1);
        buf.set_style(hr, Style::default().bg(BAR));
        let x = put_w(
            buf,
            hr,
            area.x + 1,
            area.y,
            "▍shellvisor",
            st(AMBER).bg(BAR).add_modifier(Modifier::BOLD),
        );
        let title_x = x + 2;
        let listening = snap
            .rows
            .iter()
            .filter(|(_, r)| r.state == Some(State::Listening))
            .count();
        let busy = snap
            .rows
            .iter()
            .filter(|(_, r)| matches!(r.state, Some(State::Busy(_)) | Some(State::Failed(_))))
            .count();
        let mut pill: Vec<(String, Style)> = vec![
            ("● ".into(), st(GREEN).bg(BAR)),
            (format!("{listening} listening"), st(TEXT).bg(BAR)),
        ];
        if busy > 0 {
            pill.push((" · ".into(), st(TEXT).bg(BAR)));
            pill.push(("✖ ".into(), st(RED).bg(BAR)));
            pill.push((
                format!(
                    "{busy} {}",
                    if busy == 1 {
                        "port busy"
                    } else {
                        "not working"
                    }
                ),
                st(TEXT).bg(BAR),
            ));
        }
        let clock: String = hms(now_secs()).chars().take(8).collect();
        let cx = area
            .right()
            .saturating_sub(clock.chars().count() as u16 + 2);
        put(buf, hr, cx, area.y, &clock, st(TEXT).bg(BAR));
        let pw: u16 = pill
            .iter()
            .map(|(t, _)| t.chars().count() as u16)
            .sum::<u16>()
            + 2;
        let mut px = cx.saturating_sub(pw + 2);
        px = put_w(buf, hr, px, area.y, " ", st(TEXT).bg(BAR));
        for (t, s) in &pill {
            px = put_w(buf, hr, px, area.y, t, *s);
        }
        let _ = px;
        let room = cx.saturating_sub(pw + 4).saturating_sub(title_x);
        put(
            buf,
            hr,
            title_x,
            area.y,
            fit(
                &[
                    "ssh · port forwards · tunnels to and from your sessions",
                    "ssh · port forwards",
                    "",
                ],
                room,
            ),
            st(DIM).bg(BAR),
        );

        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let lw = ratio(body.width, 330, 1280).max(30);
        let left = Rect::new(body.x + 1, body.y, lw, body.height);
        let rx = left.right() + 1;
        let right = Rect::new(rx, body.y, body.right().saturating_sub(rx + 1), body.height);
        let add_h = ratio(body.height, 300, 770).clamp(11, 17);
        let tab_r = Rect::new(
            right.x,
            right.y,
            right.width,
            right.height.saturating_sub(add_h),
        );
        let add_r = Rect::new(right.x, tab_r.bottom(), right.width, add_h);

        // ---- sessions
        let si = boxed(
            buf,
            left,
            if self.area == 0 { AMBER } else { BORDER },
            PANEL,
            None,
        );
        put(
            buf,
            si,
            si.x + 1,
            si.y,
            "SESSIONS",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        put_right(
            buf,
            si,
            si.y,
            fit(
                &["saved · ~/.ssh/config", "saved"],
                si.width.saturating_sub(10),
            ),
            st(DIM).bg(PANEL),
        );
        let mut y = si.y + 1;
        if snap.sessions.is_empty() {
            put(
                buf,
                si,
                si.x + 1,
                y + 1,
                "no saved sessions yet",
                st(DIM).bg(PANEL),
            );
            put(
                buf,
                si,
                si.x + 1,
                y + 2,
                "sv ssh save NAME user@host",
                st(CYAN).bg(PANEL),
            );
        }
        let card_h = 5u16;
        let fits = (si.bottom().saturating_sub(y + 1) / card_h).max(1) as usize;
        let first = self.sel.saturating_sub(fits - 1);
        for (i, s) in snap.sessions.iter().enumerate().skip(first).take(fits) {
            let r = Rect::new(si.x, y, si.width, card_h);
            let ci = boxed(
                buf,
                r,
                if i == self.sel {
                    AMBER
                } else {
                    role(Role::Stroke)
                },
                role(Role::Card),
                None,
            );
            let cb = role(Role::Card);
            let (dot, dc, state, scol) = match s.tab {
                Some(_) => ("●", GREEN, "connected", GREEN),
                None => ("○", DIM, "not open", DIM),
            };
            let x = put_w(buf, ci, ci.x + 1, ci.y, &format!("{dot} "), st(dc).bg(cb));
            let x = put_w(
                buf,
                ci,
                x,
                ci.y,
                &s.name,
                st(TEXT).bg(cb).add_modifier(Modifier::BOLD),
            );
            if let Some(t) = s.tab {
                let tt = format!(" · tab {t}");
                if x + tt.chars().count() as u16 + state.chars().count() as u16 + 2 <= ci.right() {
                    put(buf, ci, x, ci.y, &tt, st(DIM).bg(cb));
                }
            }
            put_right(buf, ci, ci.y, state, st(scol).bg(cb));
            let with_ka = format!("{} · keepalive {} s", s.address, s.keepalive);
            let line2 = if s.keepalive > 0 {
                fit(&[&with_ka, &s.address], ci.width.saturating_sub(2))
            } else {
                &s.address
            };
            put(buf, ci, ci.x + 1, ci.y + 1, line2, st(DIM).bg(cb));
            let rows = snap.rows_of(&s.name);
            let live = rows.iter().filter(|r| r.state.is_some()).count();
            let nfw = rows.len();
            let left_t = match (nfw, live, s.tab) {
                (0, _, _) => "no forwards".to_string(),
                (n, 0, None) => format!("{n} forward{} saved", if n == 1 { "" } else { "s" }),
                (n, _, _) => format!("{n} forward{}", if n == 1 { "" } else { "s" }),
            };
            put(buf, ci, ci.x + 1, ci.y + 2, &left_t, st(TEXT).bg(cb));
            let moved: u64 = rows.iter().map(|r| r.bytes_in + r.bytes_out).sum();
            let right_t = if moved > 0 {
                format!("{} moved", human(moved))
            } else if nfw > 0 && live == 0 && rows.iter().any(|r| r.auto) {
                "opens with the session".into()
            } else {
                String::new()
            };
            put_right(buf, ci, ci.y + 2, &right_t, st(DIM).bg(cb));
            y += card_h;
        }
        put(
            buf,
            si,
            si.x + 1,
            si.bottom().saturating_sub(1),
            fit(
                &[
                    "⏎ open session · ctrl+] S picker · sv ssh NAME",
                    "⏎ open · sv ssh NAME",
                ],
                si.width.saturating_sub(2),
            ),
            st(DIM).bg(PANEL),
        );

        // ---- forwards
        let sess = snap.sessions.get(self.sel);
        let sname = sess.map(|s| s.name.clone()).unwrap_or_default();
        let title = format!("{sname} · FORWARDS");
        let ti = boxed(
            buf,
            tab_r,
            if self.area == 1 { AMBER } else { BORDER },
            shell,
            Some((&title, st(AMBER).add_modifier(Modifier::BOLD))),
        );
        let w = ti.width.saturating_sub(2);
        let narrow = w < 80;
        let num_w = if narrow { 0u16 } else { 6 };
        let io_w = if w > 80 { 14 } else { 11 };
        let state_w = if narrow { 12u16 } else { 14 };
        let listen_w = (w / 5).clamp(15, 20);
        let type_w = 4u16;
        let dest_w = w.saturating_sub(type_w + listen_w + state_w + num_w + io_w + 5);
        let cols = [type_w, listen_w, dest_w, state_w, num_w, io_w];
        let xs: Vec<u16> = {
            let mut x = ti.x + 1;
            cols.iter()
                .map(|c| {
                    let s = x;
                    x += c + 1;
                    s
                })
                .collect()
        };
        let hy = ti.y + 1;
        for (i, h) in ["type", "listen on", "goes to", "state", "conns", "in / out"]
            .iter()
            .enumerate()
        {
            if cols[i] == 0 {
                continue;
            }
            let t = if i >= 4 {
                format!("{h:>width$}", width = cols[i] as usize)
            } else {
                h.to_string()
            };
            put_n(buf, xs[i], hy, &t, st(DIM).bg(shell), cols[i]);
        }
        let rows = snap.rows_of(&sname);
        let mut y = hy + 1;
        if rows.is_empty() {
            put(
                buf,
                ti,
                ti.x + 1,
                y + 1,
                "no forwards for this session · a adds one",
                st(DIM).bg(shell),
            );
        }
        for (i, r) in rows.iter().enumerate() {
            if y + 1 >= ti.bottom() {
                break;
            }
            let sel = self.area == 1 && i == self.row;
            let busy = matches!(r.state, Some(State::Busy(_)) | Some(State::Failed(_)));
            let bg = if busy {
                role(Role::Warn)
            } else if sel {
                SELECT
            } else {
                shell
            };
            buf.set_style(Rect::new(ti.x, y, ti.width, 1), Style::default().bg(bg));
            put_n(
                buf,
                xs[0] + 1,
                y,
                r.spec.kind.letter(),
                st(r.spec.kind.color()).bg(bg),
                1,
            );
            put_n(
                buf,
                xs[1],
                y,
                &r.spec.listen_text(&sname),
                st(TEXT).bg(bg),
                cols[1],
            );
            let dest = r.spec.dest_text(&sname);
            let svc = service(r.spec.to_port);
            let svc = if r.spec.kind == Kind::Socks || svc.is_empty() || narrow {
                String::new()
            } else {
                format!(" · {svc}")
            };
            let dx = xs[2]
                + put_w(
                    buf,
                    ti,
                    xs[2],
                    y,
                    &fit_s(&dest, cols[2]),
                    st(if r.spec.kind == Kind::Socks {
                        DIM
                    } else {
                        TEXT
                    })
                    .bg(bg),
                )
                .saturating_sub(xs[2]);
            put_n(
                buf,
                dx,
                y,
                &svc,
                st(DIM).bg(bg),
                (xs[2] + cols[2]).saturating_sub(dx),
            );
            let (stt, sc) = match &r.state {
                Some(State::Listening) => ("● listening".to_string(), GREEN),
                Some(State::Starting) => ("◌ starting".into(), AMBER),
                Some(State::Paused) => ("▮▮ paused".into(), AMBER),
                Some(State::Busy(_)) => ("✖ port busy".into(), RED),
                Some(State::Failed(_)) => ("✖ failed".into(), RED),
                None if sess.is_some_and(|s| s.tab.is_none()) && r.auto => {
                    ("○ with session".into(), DIM)
                }
                None => ("○ off".into(), DIM),
            };
            put_n(buf, xs[3], y, &stt, st(sc).bg(bg), cols[3]);
            let live = matches!(r.state, Some(State::Listening) | Some(State::Paused));
            let conns = if live {
                r.conns.to_string()
            } else {
                "—".into()
            };
            if num_w > 0 {
                put_n(
                    buf,
                    xs[4],
                    y,
                    &format!("{conns:>width$}", width = num_w as usize),
                    st(TEXT).bg(bg),
                    num_w,
                );
            }
            let io = if live {
                format!("{} / {}", human(r.bytes_in), human(r.bytes_out))
            } else {
                "—".into()
            };
            put_n(
                buf,
                xs[5],
                y,
                &format!("{io:>width$}", width = io_w as usize),
                st(TEXT).bg(bg),
                io_w,
            );
            y += 1;
            // why it isn't working, under the row
            let why = match &r.state {
                Some(State::Busy(h)) => Some(format!(
                    "{} is used by {h} · p pick another port",
                    r.spec.port
                )),
                Some(State::Failed(e)) => Some(e.clone()),
                _ => None,
            };
            if let Some(why) = why {
                if y + 1 < ti.bottom() {
                    let x = put_w(buf, ti, xs[1], y, "", st(DIM).bg(shell));
                    put(buf, ti, x, y, &why, st(DIM).bg(shell));
                    y += 1;
                }
            }
        }
        let foot = ti.bottom().saturating_sub(1);
        let sel_row = rows.get(self.row);
        let open_hint = match sel_row {
            Some(r) if r.spec.kind == Kind::Local => {
                format!("⏎ open http://localhost:{} · ", r.spec.port)
            }
            _ => String::new(),
        };
        put(
            buf,
            ti,
            ti.x + 1,
            foot,
            &format!("{open_hint}c copy address · x close forward · space pause"),
            st(DIM).bg(shell),
        );
        if ti.width > 90 {
            put_right(buf, ti, foot, "saved with the session", st(DIM).bg(shell));
        }

        // ---- add a forward
        let ai = boxed(
            buf,
            add_r,
            if self.area == 2 { AMBER } else { BORDER },
            PANEL,
            None,
        );
        let half = ai.width / 2;
        let lf = Rect::new(ai.x, ai.y, half.saturating_sub(1), ai.height);
        let rf = Rect::new(
            ai.x + half + 1,
            ai.y,
            ai.width.saturating_sub(half + 1),
            ai.height,
        );
        put(
            buf,
            lf,
            lf.x + 1,
            lf.y,
            "Add a forward",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        put_right(
            buf,
            lf,
            lf.y,
            fit(
                &[
                    &format!("to {sname} · no reconnect needed"),
                    &format!("to {sname}"),
                ],
                lf.width.saturating_sub(16),
            ),
            st(DIM).bg(PANEL),
        );
        let f = &self.form;
        let active = |n: usize| self.area == 2 && f.field == n;
        let kx = lf.x + 1;
        let vx = lf.x + 13;
        let tight = ai.height < 13;
        let gap = if tight { 1 } else { 2 };
        let mut y = lf.y + gap;
        put(buf, lf, kx, y, "type", st(DIM).bg(PANEL));
        let mut x = vx;
        let labels: [(Kind, &str); 3] = if lf.width >= 46 {
            [
                (Kind::Local, " Local  L "),
                (Kind::Remote, " Remote  R "),
                (Kind::Socks, " SOCKS  D "),
            ]
        } else {
            [
                (Kind::Local, " L "),
                (Kind::Remote, " R "),
                (Kind::Socks, " D "),
            ]
        };
        for (k, label) in labels {
            let s = if f.kind == k {
                st(INK).bg(AMBER)
            } else {
                st(TEXT).bg(role(Role::Card))
            };
            x = put_w(buf, lf, x, y, label, s) + 1;
        }
        if active(0) {
            put(buf, lf, x + 1, y, "←→", st(DIM).bg(PANEL));
        }
        y += gap;
        let field = |buf: &mut Buffer, y: u16, label: &str, val: &str, ph: &str, n: usize| {
            put(buf, lf, kx, y, label, st(DIM).bg(PANEL));
            let fw = lf.right().saturating_sub(vx + 1);
            let fill = role(Role::Card);
            buf.set_style(Rect::new(vx, y, fw, 1), Style::default().bg(fill));
            let border = if active(n) { AMBER } else { role(Role::Stroke) };
            put(buf, lf, vx, y, "▕", st(border).bg(fill));
            let (t, c) = if val.is_empty() {
                (ph.to_string(), DIM)
            } else {
                (val.to_string(), TEXT)
            };
            let t = if active(n) { format!("{t}▏") } else { t };
            put_n(buf, vx + 2, y, &t, st(c).bg(fill), fw.saturating_sub(3));
            put(
                buf,
                lf,
                vx + fw.saturating_sub(1),
                y,
                "▏",
                st(border).bg(fill),
            );
        };
        field(
            buf,
            y,
            "listen on",
            &f.listen,
            if f.kind == Kind::Remote {
                "port on the server, e.g. 9000"
            } else {
                "localhost:6380"
            },
            1,
        );
        y += 1;
        if f.kind != Kind::Socks {
            field(
                buf,
                y,
                "goes to",
                &f.dest,
                if f.kind == Kind::Remote {
                    "localhost:3000 (here)"
                } else {
                    "redis.internal:6379"
                },
                2,
            );
        } else {
            put(buf, lf, kx, y, "goes to", st(DIM).bg(PANEL));
            put(
                buf,
                lf,
                vx,
                y,
                "anywhere the server can reach (SOCKS)",
                st(DIM).bg(PANEL),
            );
        }
        y += 1;
        put(buf, lf, kx, y, "keep", st(DIM).bg(PANEL));
        let cb = |on: bool, cur: bool| {
            if cur {
                st(INK).bg(AMBER)
            } else {
                st(if on { GREEN } else { DIM }).bg(PANEL)
            }
        };
        let x = put_w(
            buf,
            lf,
            vx,
            y,
            if f.save { "☑" } else { "☐" },
            cb(f.save, active(3)),
        );
        let x = put_w(
            buf,
            lf,
            x,
            y,
            &format!(" save in {sname}"),
            st(TEXT).bg(PANEL),
        );
        let x = put_w(buf, lf, x, y, " · ", st(DIM).bg(PANEL));
        let x = put_w(
            buf,
            lf,
            x,
            y,
            if f.auto { "☑" } else { "☐" },
            cb(f.auto, active(4)),
        );
        put(
            buf,
            lf,
            x,
            y,
            " open on login",
            st(if f.auto { TEXT } else { DIM }).bg(PANEL),
        );
        y += gap;
        let bs = if active(5) {
            st(INK)
                .bg(AMBER)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            st(INK).bg(AMBER).add_modifier(Modifier::BOLD)
        };
        let x = put_w(
            buf,
            lf,
            kx,
            y.min(lf.bottom().saturating_sub(1)),
            " ⏎ Add ",
            bs,
        );
        put(
            buf,
            lf,
            x + 2,
            y.min(lf.bottom().saturating_sub(1)),
            "its own ssh · your session isn't touched",
            st(DIM).bg(PANEL),
        );
        // separator
        for yy in ai.y..ai.bottom() {
            put_n(buf, ai.x + half, yy, "│", st(BORDER).bg(PANEL), 1);
        }
        // what it means
        put(
            buf,
            rf,
            rf.x + 1,
            rf.y,
            "What it means",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        let spec = f.spec().ok().unwrap_or(Spec {
            kind: f.kind,
            port: 6380,
            host: "redis.internal".into(),
            to_port: 6379,
        });
        let here = if cfg!(target_os = "macos") {
            "your Mac"
        } else {
            "this computer"
        };
        let pad = here.chars().count().max(sname.chars().count().min(12)) + 1;
        let lab = |t: &str| format!("{:<pad$}", trunc(t, pad - 1));
        let lines: Vec<Vec<(String, Color)>> = match spec.kind {
            Kind::Local => vec![
                vec![(lab(here), DIM), (format!("localhost:{}", spec.port), TEXT)],
                vec![
                    ("   │".into(), CYAN),
                    (format!("  ssh tunnel to {sname}"), DIM),
                ],
                vec![("   ▼".into(), CYAN)],
                vec![
                    (lab(&sname), DIM),
                    (format!("→ {}:{}", spec.host, spec.to_port), TEXT),
                ],
            ],
            Kind::Remote => vec![
                vec![(lab(&sname), DIM), (format!("port {}", spec.port), TEXT)],
                vec![
                    ("   │".into(), MAGENTA),
                    ("  back through the tunnel".into(), DIM),
                ],
                vec![("   ▼".into(), MAGENTA)],
                vec![
                    (lab(here), DIM),
                    (format!("→ {}:{}", spec.host, spec.to_port), TEXT),
                ],
            ],
            Kind::Socks => vec![
                vec![
                    (lab(here), DIM),
                    (format!("SOCKS on localhost:{}", spec.port), TEXT),
                ],
                vec![
                    ("   │".into(), BLUE),
                    (format!("  every connection through {sname}"), DIM),
                ],
                vec![("   ▼".into(), BLUE)],
                vec![(lab("anywhere"), DIM), (format!("{sname} can reach"), TEXT)],
            ],
        };
        let bh = if rf.height >= lines.len() as u16 + 8 {
            lines.len() as u16 + 2
        } else {
            0
        };
        let db = boxed(
            buf,
            Rect::new(rf.x + 1, rf.y + 1, rf.width.saturating_sub(2), bh),
            BORDER,
            shell,
            None,
        );
        for (i, parts) in lines.iter().enumerate().take(db.height as usize) {
            let mut x = db.x + 1;
            for (t, c) in parts {
                x = put_w(buf, db, x, db.y + i as u16, t, st(*c).bg(shell));
            }
        }
        let yy = rf.y + 1 + bh;
        put(buf, rf, rf.x + 1, yy, "same as", st(DIM).bg(PANEL));
        let sb = boxed(
            buf,
            Rect::new(rf.x + 1, yy + 1, rf.width.saturating_sub(2), 3),
            BORDER,
            shell,
            None,
        );
        let x = put_w(buf, sb, sb.x + 1, sb.y, "ssh ", st(TEXT).bg(shell));
        let x = put_w(buf, sb, x, sb.y, &spec.to_arg(), st(AMBER).bg(shell));
        put(buf, sb, x + 1, sb.y, &sname, st(TEXT).bg(shell));
        let file = match spec.kind {
            Kind::Local => "LocalForward",
            Kind::Remote => "RemoteForward",
            Kind::Socks => "DynamicForward",
        };
        put(
            buf,
            rf,
            rf.x + 1,
            yy + 4,
            &format!("~/.config/shellvisor/sessions.yaml · {file}"),
            st(DIM).bg(PANEL),
        );

        // key strip
        let ky = area.bottom() - 1;
        buf.set_style(
            Rect::new(area.x, ky, area.width, 1),
            Style::default().bg(BAR),
        );
        let mut x = area.x + 1;
        for (k, label) in [
            ("a", "Add forward"),
            ("x", "Close"),
            ("⏎", "Open in browser"),
            ("c", "Copy address"),
            ("tab", "Sessions"),
            ("esc", "Back"),
        ] {
            x = put_w(buf, area, x, ky, k, st(AMBER).bg(BAR)) + 1;
            x = put_w(buf, area, x, ky, label, st(DIM).bg(BAR)) + 3;
        }
        put(
            buf,
            area,
            x,
            ky,
            "· sv ssh forward · ctrl+] P",
            st(DIM).bg(BAR),
        );
        if let Some(n) = &self.note {
            let w = n.chars().count() as u16 + 2;
            put(
                buf,
                area,
                area.right().saturating_sub(w + 1),
                ky,
                &format!(" {n} "),
                st(INK).bg(AMBER),
            );
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 840K, 1.2M, 512B.
pub fn human(b: u64) -> String {
    match b {
        0..=1023 => format!("{b}B"),
        1024..=1_048_575 => format!("{}K", b / 1024),
        _ => format!("{:.1}M", b as f64 / 1_048_576.0),
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn fit_s(s: &str, w: u16) -> String {
    let n = s.chars().count();
    if n <= w as usize {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1) as usize).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        let l = Spec::parse("-L 5433:db.internal:5432").unwrap();
        assert_eq!(
            (l.kind, l.port, l.host.as_str(), l.to_port),
            (Kind::Local, 5433, "db.internal", 5432)
        );
        assert_eq!(l.to_arg(), "-L 5433:db.internal:5432");
        assert_eq!(
            Spec::parse("-L127.0.0.1:8080:localhost:80").unwrap().port,
            8080
        );
        let d = Spec::parse("-D 1080").unwrap();
        assert_eq!((d.kind, d.port), (Kind::Socks, 1080));
        assert_eq!(d.to_arg(), "-D 1080");
        let r = Spec::parse("-R 9000:localhost:3000").unwrap();
        assert_eq!(r.dest_text("prod"), "your localhost:3000");
        assert_eq!(r.listen_text("prod"), "prod:9000");
        assert!(Spec::parse("-X 1").is_none());
        assert_eq!(human(840 * 1024), "840K");
    }

    /// A forward relays and counts: a stand-in "ssh" that is just the tunnel.
    #[test]
    fn relays_and_counts() {
        // the far end: echoes once
        let echo = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let ep = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = echo.accept() {
                let mut b = [0u8; 4];
                if c.read_exact(&mut b).is_ok() {
                    let _ = c.write_all(&b);
                }
            }
        });
        let conns = Arc::new(AtomicUsize::new(0));
        let (bin, bout) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let lp = l.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (s, n, i, o) = (stop.clone(), conns.clone(), bin.clone(), bout.clone());
            std::thread::spawn(move || accept_loop(l, ("127.0.0.1".into(), ep), s, n, i, o));
        }
        let mut c = TcpStream::connect(("127.0.0.1", lp)).unwrap();
        c.write_all(b"ping").unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        drop(c);
        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, Ordering::Relaxed);
        assert_eq!(bout.load(Ordering::Relaxed), 4);
        assert_eq!(bin.load(Ordering::Relaxed), 4);
    }

    /// Board 20 from fixed data, compared with tests/snapshots.
    #[test]
    fn board_20_snapshots() {
        let spec = |a: &str| Spec::parse(a).unwrap();
        let row = |a: &str, st: Option<State>, c: usize, i: u64, o: u64, auto: bool| Row {
            spec: spec(a),
            state: st,
            conns: c,
            bytes_in: i,
            bytes_out: o,
            saved: true,
            auto,
        };
        let sess = |n: &str, addr: &str, tab: Option<usize>, saved: Vec<(&str, bool)>| SessInfo {
            name: n.into(),
            address: addr.into(),
            keepalive: 30,
            tab,
            saved: saved.into_iter().map(|(a, b)| (spec(a), b)).collect(),
        };
        let snap = Snap {
            sessions: vec![
                sess(
                    "db-bastion",
                    "aditya@bastion.internal",
                    Some(3),
                    vec![("-L 15432:db.internal:5432", true)],
                ),
                sess(
                    "pi-lab",
                    "pi@raspberrypi.local",
                    None,
                    vec![("-L 8081:localhost:8080", true)],
                ),
                sess(
                    "prod-web",
                    "deploy@10.0.4.12",
                    Some(2),
                    vec![
                        ("-L 5433:db.internal:5432", true),
                        ("-L 8080:localhost:80", true),
                        ("-R 9000:localhost:3000", true),
                        ("-D 1080", true),
                    ],
                ),
                sess("staging", "deploy@staging.example.com", None, vec![]),
            ],
            rows: vec![
                (
                    "db-bastion".into(),
                    row(
                        "-L 15432:db.internal:5432",
                        Some(State::Listening),
                        1,
                        48 * 1024,
                        2048,
                        true,
                    ),
                ),
                (
                    "pi-lab".into(),
                    row("-L 8081:localhost:8080", None, 0, 0, 0, true),
                ),
                (
                    "prod-web".into(),
                    row(
                        "-L 5433:db.internal:5432",
                        Some(State::Listening),
                        2,
                        840 * 1024,
                        12 * 1024,
                        true,
                    ),
                ),
                (
                    "prod-web".into(),
                    row(
                        "-L 8080:localhost:80",
                        Some(State::Listening),
                        0,
                        310 * 1024,
                        41 * 1024,
                        true,
                    ),
                ),
                (
                    "prod-web".into(),
                    row(
                        "-R 9000:localhost:3000",
                        Some(State::Listening),
                        1,
                        22 * 1024,
                        96 * 1024,
                        true,
                    ),
                ),
                (
                    "prod-web".into(),
                    row(
                        "-D 1080",
                        Some(State::Busy("ssh -D (pid 9112)".into())),
                        0,
                        0,
                        0,
                        true,
                    ),
                ),
            ],
        };
        let sc = FwdScreen::new(2);
        let mut failed = Vec::new();
        for (w, h) in [(160u16, 44u16), (100, 24)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            sc.render(area, &mut buf, &snap);
            if let Some(e) = crate::mcpscreen::snap::check(
                &format!("fwd20-{w}x{h}.txt"),
                &crate::mcpscreen::snap::text(&buf),
            ) {
                failed.push(e);
            }
        }
        assert!(
            failed.is_empty(),
            "board 20 drifted:\n{}",
            failed.join("\n")
        );
    }
}

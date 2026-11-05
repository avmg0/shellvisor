//! The MCP screens, drawn as design boards 17 (servers, details, tools, try
//! a tool) and 18 (live traffic, permissions, add a server): the same
//! panels in the same places, the same labels, colours and key strips.

use crate::mcp::{self, Call, CallResult, Health, Level, NewServer, Server, Transport};
use crate::palette::{Role, role};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Widget};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

const AMBER: Color = Color::Indexed(179);
const CYAN: Color = Color::Indexed(73);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const BLUE: Color = Color::Indexed(110);
const MAGENTA: Color = Color::Indexed(170);
const TEXT: Color = Color::Indexed(252);
const DIM: Color = Color::Indexed(245);
const BORDER: Color = Color::Indexed(240);
const PANEL: Color = Color::Indexed(235);
const BAR: Color = Color::Indexed(236);
const SELECT: Color = Color::Indexed(238);
const INK: Color = Color::Indexed(16);

#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Checking,
    Ok(Health),
    Failed(String, Vec<String>),
}

/// A column of the permissions grid: an agent (or you, in the project).
#[derive(Debug, Clone, PartialEq)]
pub struct Who {
    pub name: String,
    /// Its folder (worktree or project): settings and log folder.
    pub dir: PathBuf,
    pub color: Color,
}

/// What the app should do for the screen (run in the background).
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Check(Server),
    Call(Server, String, serde_json::Value),
    Stop(String, i32),
    Restart(Server, i32),
    Traffic,
    Add(NewServer, [bool; 4]),
    /// Answer the waiting approval: 'y', 'n' or 'a'.
    Approve(char),
    Copy(String),
    Save(String),
    Close,
}

pub struct TryForm {
    pub tool: String,
    pub fields: Vec<(String, String, bool, String)>,
    pub values: Vec<String>,
    pub field: usize,
    pub editing: bool,
    pub running: bool,
    pub result: Option<CallResult>,
}

pub struct AddForm {
    pub template: Option<usize>,
    pub name: String,
    pub http: bool,
    /// Command line (stdio) or URL (http).
    pub target: String,
    pub secret: String,
    pub to: [bool; 4],
    /// 0 templates, 1 name, 2 transport, 3 target, 4 secret, 5 add to, 6 button
    pub field: usize,
    pub to_sel: usize,
    pub busy: bool,
    pub status: Option<(String, bool)>,
}

impl Default for AddForm {
    fn default() -> Self {
        AddForm {
            template: None,
            name: String::new(),
            http: false,
            target: String::new(),
            secret: String::new(),
            to: [true, true, false, false],
            field: 0,
            to_sel: 0,
            busy: false,
            status: None,
        }
    }
}

impl AddForm {
    fn use_template(&mut self, i: usize) {
        let (name, tr, cmd, args, secret) = mcp::TEMPLATES[i];
        self.template = Some(i);
        self.name = name.into();
        self.http = tr == "http";
        self.target = if self.http {
            cmd.to_string()
        } else {
            std::iter::once(cmd)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ")
        };
        self.secret = secret.into();
    }

    fn server(&self) -> Result<NewServer, String> {
        let name = self.name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Err("a name: letters, digits, - and _".into());
        }
        let target = self.target.trim();
        if target.is_empty() {
            return Err(if self.http {
                "the server's URL"
            } else {
                "the command that starts it"
            }
            .into());
        }
        let transport = if self.http {
            if !target.starts_with("http") {
                return Err("the URL starts with https://".into());
            }
            Transport::Http {
                url: target.to_string(),
            }
        } else {
            let mut w = target.split_whitespace().map(String::from);
            Transport::Stdio {
                command: w.next().unwrap_or_default(),
                args: w.collect(),
            }
        };
        Ok(NewServer {
            name: name.to_string(),
            transport,
            secrets: self
                .secret
                .split([',', ' '])
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        })
    }
}

pub struct McpScreen {
    /// 0: board 17 (servers), 1: board 18 (traffic, permissions, add).
    pub page: u8,
    pub servers: Vec<Server>,
    pub checks: HashMap<String, Check>,
    pub running: BTreeMap<String, i32>,
    pub sel: usize,
    /// Board 17: the tools list has the keys (else the server list).
    pub in_tools: bool,
    /// Tools · Resources · Prompts · Traffic · Permissions · Log
    pub tab: usize,
    pub tool_sel: usize,
    pub filter: String,
    pub filtering: bool,
    pub try_form: Option<TryForm>,
    pub calls: Vec<Call>,
    pub paused: bool,
    pub call_sel: usize,
    pub errors_only: bool,
    /// Board 18 focus: 0 traffic, 1 permissions, 2 add.
    pub area: u8,
    pub who: Vec<Who>,
    pub perms: HashMap<(usize, String), Level>,
    pub perm_row: usize,
    pub perm_col: usize,
    pub add: AddForm,
    pub note: Option<String>,
    pub home: PathBuf,
    pub cwd: PathBuf,
}

pub const TABS: [&str; 6] = [
    "Tools",
    "Resources",
    "Prompts",
    "Traffic",
    "Permissions",
    "Log",
];

impl McpScreen {
    pub fn new(home: PathBuf, cwd: PathBuf, who: Vec<Who>) -> (McpScreen, Vec<Act>) {
        let servers = mcp::discover(&home, &cwd);
        let mut checks = HashMap::new();
        let mut acts = vec![Act::Traffic];
        for s in &servers {
            checks.insert(s.name.clone(), Check::Checking);
            acts.push(Act::Check(s.clone()));
        }
        let mut sc = McpScreen {
            page: 0,
            running: mcp::running(&servers),
            servers,
            checks,
            sel: 0,
            in_tools: false,
            tab: 0,
            tool_sel: 0,
            filter: String::new(),
            filtering: false,
            try_form: None,
            calls: Vec::new(),
            paused: false,
            call_sel: 0,
            errors_only: false,
            area: 0,
            who,
            perms: HashMap::new(),
            perm_row: 0,
            perm_col: 0,
            add: AddForm::default(),
            note: None,
            home,
            cwd,
        };
        sc.load_perms();
        (sc, acts)
    }

    /// Log folders to read traffic from: (Claude Code key, who).
    pub fn log_keys(&self) -> Vec<(String, String)> {
        self.who
            .iter()
            .map(|w| (mcp::claude_key(&w.dir), w.name.clone()))
            .collect()
    }

    pub fn load_perms(&mut self) {
        self.perms.clear();
        let rows = self.perm_rows();
        for (i, w) in self.who.iter().enumerate() {
            let v: serde_json::Value = std::fs::read_to_string(mcp::settings_path(&w.dir))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(serde_json::Value::Null);
            for (rule, _, _) in &rows {
                self.perms.insert((i, rule.clone()), mcp::level(&v, rule));
            }
        }
    }

    /// The servers the filter shows.
    fn shown(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.servers.len())
            .filter(|i| {
                f.is_empty() || {
                    let s = &self.servers[*i];
                    s.name.to_lowercase().contains(&f)
                        || match self.checks.get(&s.name) {
                            Some(Check::Ok(h)) => {
                                h.tools.iter().any(|t| t.name.to_lowercase().contains(&f))
                            }
                            _ => false,
                        }
                }
            })
            .collect()
    }

    fn current(&self) -> Option<&Server> {
        let shown = self.shown();
        shown.get(self.sel).map(|i| &self.servers[*i])
    }

    fn health(&self, name: &str) -> Option<&Health> {
        match self.checks.get(name) {
            Some(Check::Ok(h)) => Some(h),
            _ => None,
        }
    }

    /// Permission rows: (rule, label, is a server row).
    pub fn perm_rows(&self) -> Vec<(String, String, bool)> {
        let mut v = Vec::new();
        for s in &self.servers {
            v.push((mcp::rule(&s.name, None), s.name.clone(), true));
            if let Some(h) = self.health(&s.name) {
                for t in &h.tools {
                    v.push((
                        mcp::rule(&s.name, Some(&t.name)),
                        format!("  {}", t.name),
                        false,
                    ));
                }
            }
        }
        v
    }

    // ------------------------------------------------------------ results

    pub fn checked(&mut self, name: String, r: Result<Health, (String, Vec<String>)>) {
        self.checks.insert(
            name,
            match r {
                Ok(h) => Check::Ok(h),
                Err((e, log)) => Check::Failed(e, log),
            },
        );
        self.load_perms();
    }

    pub fn called(&mut self, r: CallResult) {
        if let Some(t) = self.try_form.as_mut() {
            t.running = false;
            t.result = Some(r);
        }
    }

    pub fn traffic(&mut self, calls: Vec<Call>) {
        if !self.paused {
            self.calls = calls;
        }
        self.running = mcp::running(&self.servers);
    }

    pub fn added(&mut self, r: Result<Vec<String>, String>) -> Vec<Act> {
        self.add.busy = false;
        match r {
            Ok(to) => {
                self.add.status = Some((format!("✔ added to {}", to.join(", ")), true));
                let name = self.add.name.clone();
                self.servers = mcp::discover(&self.home, &self.cwd);
                if let Some(i) = self.servers.iter().position(|s| s.name == name) {
                    self.checks.insert(name, Check::Checking);
                    return vec![Act::Check(self.servers[i].clone())];
                }
                Vec::new()
            }
            Err(e) => {
                self.add.status = Some((format!("✖ {e}"), false));
                Vec::new()
            }
        }
    }

    // ------------------------------------------------------------ keys

    pub fn key(&mut self, key: &[u8], waiting: bool) -> Vec<Act> {
        self.note = None;
        // two quick escs arrive as one alt+esc: treat them as two presses
        if key == b"\x1b\x1b" {
            let mut acts = self.key(b"\x1b", waiting);
            if !acts.iter().any(|a| matches!(a, Act::Close)) {
                acts.extend(self.key(b"\x1b", waiting));
            }
            return acts;
        }
        if self.page == 0 {
            self.key17(key)
        } else {
            self.key18(key, waiting)
        }
    }

    fn key17(&mut self, key: &[u8]) -> Vec<Act> {
        // the try-a-tool form takes typing while open
        if let Some(t) = self.try_form.as_mut() {
            if t.editing {
                match key {
                    b"\x1b" => {
                        self.try_form = None;
                    }
                    b"\t" | b"\x1b[B" => t.field = (t.field + 1) % t.fields.len().max(1),
                    b"\x1b[A" => {
                        t.field = (t.field + t.fields.len().max(1) - 1) % t.fields.len().max(1)
                    }
                    [0x7f] | [0x08] => {
                        if let Some(v) = t.values.get_mut(t.field) {
                            v.pop();
                        }
                    }
                    b"\r" => return self.run_try(),
                    k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                        if let Some(v) = t.values.get_mut(t.field) {
                            v.push_str(&String::from_utf8_lossy(k));
                        }
                    }
                    _ => {}
                }
                return Vec::new();
            }
        }
        if self.filtering {
            match key {
                b"\x1b" => {
                    self.filtering = false;
                    self.filter.clear();
                }
                b"\r" => self.filtering = false,
                [0x7f] | [0x08] => {
                    self.filter.pop();
                }
                k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                    self.filter.push_str(&String::from_utf8_lossy(k));
                    self.sel = 0;
                }
                _ => {}
            }
            return Vec::new();
        }
        let n = self.shown().len();
        let tools_n = self
            .current()
            .and_then(|s| self.health(&s.name))
            .map(|h| h.tools.len())
            .unwrap_or(0);
        match key {
            b"\x1b" | b"q" => {
                if self.try_form.is_some() {
                    self.try_form = None;
                } else if self.in_tools {
                    self.in_tools = false;
                } else {
                    return vec![Act::Close];
                }
            }
            b"\x1b[A" | b"k" => {
                if self.in_tools {
                    self.tool_sel = self.tool_sel.saturating_sub(1);
                } else {
                    self.sel = self.sel.saturating_sub(1);
                    self.tool_sel = 0;
                    self.try_form = None;
                }
            }
            b"\x1b[B" | b"j" => {
                if self.in_tools {
                    self.tool_sel = (self.tool_sel + 1).min(tools_n.saturating_sub(1));
                } else {
                    self.sel = (self.sel + 1).min(n.saturating_sub(1));
                    self.tool_sel = 0;
                    self.try_form = None;
                }
            }
            b"\r" | b"\x1b[C" => {
                if self.try_form.is_some() {
                    return self.run_try();
                }
                self.in_tools = true;
            }
            b"\x1b[D" => self.in_tools = false,
            b"\t" => self.tab = (self.tab + 1) % TABS.len(),
            b"\x1b[Z" => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
            b"t" => self.open_try(),
            b"/" => {
                self.filtering = true;
                self.filter.clear();
            }
            b"l" => self.tab = 5,
            b"a" => {
                self.page = 1;
                self.area = 2;
            }
            b"p" => {
                self.page = 1;
                self.area = 1;
            }
            b"]" | b"2" => self.page = 1,
            b"s" => {
                if let Some(s) = self.current().cloned() {
                    if let Some(pid) = self.running.get(&s.name) {
                        return vec![Act::Stop(s.name.clone(), *pid)];
                    }
                    self.checks.insert(s.name.clone(), Check::Checking);
                    return vec![Act::Check(s)];
                }
            }
            b"r" => {
                if let Some(s) = self.current().cloned() {
                    let pid = self.running.get(&s.name).copied().unwrap_or(0);
                    self.checks.insert(s.name.clone(), Check::Checking);
                    return vec![Act::Restart(s, pid)];
                }
            }
            b"c" => {
                if let Some(r) = self.try_form.as_ref().and_then(|t| t.result.as_ref()) {
                    return vec![Act::Copy(r.text.clone())];
                }
            }
            b"S" => {
                if let Some(r) = self.try_form.as_ref().and_then(|t| t.result.as_ref()) {
                    return vec![Act::Save(r.text.clone())];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn open_try(&mut self) {
        let Some(s) = self.current() else { return };
        let Some(h) = self.health(&s.name) else {
            self.note = Some("the server isn't answering yet — s checks it".into());
            return;
        };
        let Some(tool) = h.tools.get(self.tool_sel) else {
            return;
        };
        let fields = mcp::fields(&tool.schema);
        let values = vec![String::new(); fields.len()];
        self.try_form = Some(TryForm {
            tool: tool.name.clone(),
            fields,
            values,
            field: 0,
            editing: true,
            running: false,
            result: None,
        });
    }

    fn run_try(&mut self) -> Vec<Act> {
        let Some(s) = self.current().cloned() else {
            return Vec::new();
        };
        let Some(t) = self.try_form.as_mut() else {
            return Vec::new();
        };
        match mcp::args_from(&t.fields, &t.values) {
            Ok(args) => {
                t.running = true;
                t.editing = false;
                t.result = None;
                vec![Act::Call(s, t.tool.clone(), args)]
            }
            Err(e) => {
                self.note = Some(e);
                Vec::new()
            }
        }
    }

    fn shown_calls(&self) -> Vec<&Call> {
        self.calls
            .iter()
            .filter(|c| !self.errors_only || c.ok == Some(false))
            .collect()
    }

    fn key18(&mut self, key: &[u8], waiting: bool) -> Vec<Act> {
        if waiting && self.area == 0 {
            match key {
                b"y" => return vec![Act::Approve('y')],
                b"n" => return vec![Act::Approve('n')],
                b"a" => return vec![Act::Approve('a')],
                _ => {}
            }
        }
        // typing into the add form
        if self.area == 2 && matches!(self.add.field, 1 | 3 | 4) {
            let f = &mut self.add;
            let text = match f.field {
                1 => &mut f.name,
                3 => &mut f.target,
                _ => &mut f.secret,
            };
            match key {
                [0x7f] | [0x08] => {
                    text.pop();
                    return Vec::new();
                }
                k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                    text.push_str(&String::from_utf8_lossy(k));
                    return Vec::new();
                }
                _ => {}
            }
        }
        match key {
            b"\x1b" => self.page = 0,
            b"\t" => self.area = (self.area + 1) % 3,
            b"\x1b[Z" => self.area = (self.area + 2) % 3,
            b"[" | b"1" if !(self.area == 2 && matches!(self.add.field, 1 | 3 | 4)) => {
                self.page = 0
            }
            _ => match self.area {
                0 => {
                    let n = self.shown_calls().len();
                    match key {
                        b" " => self.paused = !self.paused,
                        b"e" => self.errors_only = !self.errors_only,
                        b"\x1b[A" | b"k" => self.call_sel = self.call_sel.saturating_sub(1),
                        b"\x1b[B" | b"j" => {
                            self.call_sel = (self.call_sel + 1).min(n.saturating_sub(1))
                        }
                        b"\r" => {
                            if let Some(c) = self.shown_calls().get(self.call_sel) {
                                self.note = Some(format!(
                                    "{} · {} · {} — {}",
                                    c.agent,
                                    c.server,
                                    c.tool,
                                    if c.args.is_empty() {
                                        "(no arguments)"
                                    } else {
                                        &c.args
                                    }
                                ));
                            }
                        }
                        b"p" => self.area = 1,
                        b"a" => self.area = 2,
                        _ => {}
                    }
                }
                1 => {
                    let rows = self.perm_rows();
                    match key {
                        b"\x1b[A" | b"k" => self.perm_row = self.perm_row.saturating_sub(1),
                        b"\x1b[B" | b"j" => {
                            self.perm_row = (self.perm_row + 1).min(rows.len().saturating_sub(1))
                        }
                        b"\x1b[D" | b"h" => self.perm_col = self.perm_col.saturating_sub(1),
                        b"\x1b[C" | b"l" => {
                            self.perm_col =
                                (self.perm_col + 1).min(self.who.len().saturating_sub(1))
                        }
                        b" " | b"\r" => {
                            if let (Some((rule, _, _)), Some(w)) =
                                (rows.get(self.perm_row), self.who.get(self.perm_col))
                            {
                                let cur = self
                                    .perms
                                    .get(&(self.perm_col, rule.clone()))
                                    .copied()
                                    .unwrap_or(Level::Default);
                                let next = cur.next();
                                match mcp::set_level(&mcp::settings_path(&w.dir), rule, next) {
                                    Ok(()) => {
                                        self.perms.insert((self.perm_col, rule.clone()), next);
                                        self.note = Some(format!(
                                            "{} · {} → {}",
                                            w.name,
                                            rule,
                                            next.label()
                                        ));
                                    }
                                    Err(e) => self.note = Some(format!("couldn't save: {e}")),
                                }
                            }
                        }
                        b"a" => self.area = 2,
                        _ => {}
                    }
                }
                _ => {
                    let f = &mut self.add;
                    match key {
                        b"\x1b[A" => f.field = f.field.saturating_sub(1),
                        b"\x1b[B" => f.field = (f.field + 1).min(6),
                        b"\x1b[C" | b"\x1b[D" => {
                            let right = key == b"\x1b[C";
                            match f.field {
                                0 => {
                                    let n = mcp::TEMPLATES.len();
                                    let i = match (f.template, right) {
                                        (None, true) => 0,
                                        (None, false) => n - 1,
                                        (Some(i), true) => (i + 1) % n,
                                        (Some(i), false) => (i + n - 1) % n,
                                    };
                                    f.use_template(i);
                                }
                                2 => f.http = !f.http,
                                5 => {
                                    f.to_sel = if right {
                                        (f.to_sel + 1) % 4
                                    } else {
                                        (f.to_sel + 3) % 4
                                    }
                                }
                                _ => {}
                            }
                        }
                        b" " if f.field == 5 => f.to[f.to_sel] = !f.to[f.to_sel],
                        b" " if f.field == 2 => f.http = !f.http,
                        b"\r" => {
                            if f.field < 6 && f.field != 5 {
                                f.field += 1;
                                return Vec::new();
                            }
                            if !f.to.iter().any(|x| *x) {
                                f.status = Some(("✖ tick at least one agent".into(), false));
                                return Vec::new();
                            }
                            match f.server() {
                                Ok(s) => {
                                    f.busy = true;
                                    f.status = Some(("checking the server…".into(), true));
                                    return vec![Act::Add(s, f.to)];
                                }
                                Err(e) => f.status = Some((format!("✖ {e}"), false)),
                            }
                        }
                        _ => {}
                    }
                }
            },
        }
        Vec::new()
    }

    // ------------------------------------------------------------ drawing

    pub fn render(&self, area: Rect, buf: &mut Buffer, waiting: Option<&str>) {
        if area.width < 80 || area.height < 24 {
            put(
                buf,
                area,
                area.x + 1,
                area.y + 1,
                "make the window bigger for the MCP screen (80 × 24)",
                st(DIM),
            );
            return;
        }
        if self.page == 0 {
            self.render17(area, buf);
        } else {
            self.render18(area, buf, waiting);
        }
    }

    fn header(&self, area: Rect, buf: &mut Buffer, what: &str, pill: Vec<(String, Style)>) {
        let r = Rect::new(area.x, area.y, area.width, 1);
        buf.set_style(r, Style::default().bg(BAR));
        put(
            buf,
            area,
            area.x + 1,
            area.y,
            "▍shellvisor",
            st(AMBER).bg(BAR).add_modifier(Modifier::BOLD),
        );
        put(buf, area, area.x + 14, area.y, what, st(DIM).bg(BAR));
        let clock = crate::osinfo::clock();
        let cx = area
            .right()
            .saturating_sub(clock.chars().count() as u16 + 2);
        put(buf, area, cx, area.y, &clock, st(TEXT).bg(BAR));
        let w: usize = pill.iter().map(|(t, _)| t.chars().count()).sum::<usize>() + 2;
        let mut x = cx.saturating_sub(w as u16 + 2);
        put(
            buf,
            area,
            x,
            area.y,
            " ",
            Style::default().bg(role(Role::Badge)),
        );
        x += 1;
        for (t, s) in pill {
            let n = t.chars().count() as u16;
            put(buf, area, x, area.y, &t, s.bg(role(Role::Badge)));
            x += n;
        }
        put(
            buf,
            area,
            x,
            area.y,
            " ",
            Style::default().bg(role(Role::Badge)),
        );
    }

    fn keys(&self, area: Rect, buf: &mut Buffer, keys: &[(&str, &str)], tail: &str) {
        let y = area.bottom() - 1;
        buf.set_style(
            Rect::new(area.x, y, area.width, 1),
            Style::default().bg(BAR),
        );
        let mut x = area.x + 1;
        for (k, label) in keys {
            put(buf, area, x, y, k, st(AMBER).bg(BAR));
            x += k.chars().count() as u16 + 1;
            put(buf, area, x, y, label, st(DIM).bg(BAR));
            x += label.chars().count() as u16 + 3;
        }
        put(buf, area, x, y, tail, st(DIM).bg(BAR));
        if let Some(n) = &self.note {
            let w = n.chars().count() as u16 + 2;
            let nx = area.right().saturating_sub(w + 1);
            put(buf, area, nx, y, &format!(" {n} "), st(INK).bg(AMBER));
        }
    }

    fn state_of(&self, s: &Server) -> (&'static str, Color, &'static str) {
        match self.checks.get(&s.name) {
            Some(Check::Failed(..)) => ("✖", RED, "failing"),
            Some(Check::Checking) if !self.running.contains_key(&s.name) => {
                ("◌", AMBER, "checking")
            }
            _ if self.running.contains_key(&s.name) => ("●", GREEN, "running"),
            _ => ("○", DIM, "stopped"),
        }
    }

    fn render17(&self, area: Rect, buf: &mut Buffer) {
        let (mut run, mut stop, mut fail) = (0, 0, 0);
        for s in &self.servers {
            match self.state_of(s).2 {
                "running" => run += 1,
                "failing" => fail += 1,
                _ => stop += 1,
            }
        }
        self.header(
            area,
            buf,
            "MCP · servers your agents can use",
            vec![
                ("●".into(), st(GREEN)),
                (format!(" {run} running · "), st(TEXT)),
                ("○".into(), st(DIM)),
                (format!(" {stop} stopped · "), st(TEXT)),
                ("✖".into(), st(RED)),
                (format!(" {fail} failing"), st(TEXT)),
            ],
        );
        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let lw = ratio(body.width, 430, 1280).max(34);
        let left = Rect::new(body.x + 1, body.y, lw, body.height);
        let rx = left.right() + 1;
        let right = Rect::new(rx, body.y, body.right().saturating_sub(rx + 1), body.height);

        // ---- SERVERS
        let li = boxed(buf, left, BORDER, PANEL, None);
        put(
            buf,
            li,
            li.x + 1,
            li.y,
            "SERVERS",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        let from = [
            "from Claude Code · Codex · Gemini · .mcp.json",
            "from Claude · Codex · Gemini · .mcp.json",
            "Claude · Codex · Gemini · .mcp.json",
        ]
        .into_iter()
        .find(|t| t.chars().count() as u16 + 10 <= li.width)
        .unwrap_or("");
        put_right(buf, li, li.y, from, st(DIM).bg(PANEL));
        let fr = Rect::new(li.x + 1, li.y + 1, li.width.saturating_sub(2), 3);
        let fi = boxed(
            buf,
            fr,
            if self.filtering {
                AMBER
            } else {
                role(Role::Stroke)
            },
            PANEL,
            None,
        );
        if self.filtering || !self.filter.is_empty() {
            put(
                buf,
                fi,
                fi.x + 1,
                fi.y,
                &format!("/ {}▏", self.filter),
                st(TEXT).bg(PANEL),
            );
        } else {
            put(
                buf,
                fi,
                fi.x + 1,
                fi.y,
                "/ filter servers and tools",
                st(DIM).bg(PANEL),
            );
        }
        let shown = self.shown();
        if self.servers.is_empty() {
            let y = fr.bottom() + 1;
            put(
                buf,
                li,
                li.x + 1,
                y,
                "No MCP servers configured yet.",
                st(DIM).bg(PANEL),
            );
            put(
                buf,
                li,
                li.x + 1,
                y + 1,
                "a adds one for every agent at once.",
                st(DIM).bg(PANEL),
            );
        }
        let card_h = 5u16;
        let fits = ((li.bottom().saturating_sub(fr.bottom())) / card_h) as usize;
        let first = self.sel.saturating_sub(fits.saturating_sub(1));
        let (by_server, by_tool) = mcp::usage(&self.calls);
        for (k, &i) in shown.iter().enumerate().skip(first).take(fits) {
            let s = &self.servers[i];
            let y = fr.bottom() + (k - first) as u16 * card_h;
            let selected = k == self.sel;
            let ci = boxed(
                buf,
                Rect::new(li.x + 1, y, li.width.saturating_sub(2), card_h),
                if selected { AMBER } else { role(Role::Stroke) },
                role(Role::Card),
                None,
            );
            let cb = role(Role::Card);
            let (icon, col, state) = self.state_of(s);
            let mut x = ci.x + 1;
            x = put_w(buf, ci, x, ci.y, &format!("{icon} "), st(col).bg(cb));
            x = put_w(
                buf,
                ci,
                x,
                ci.y,
                &s.name,
                st(TEXT).bg(cb).add_modifier(Modifier::BOLD),
            );
            // keep a space before the state on the right
            let state_x = ci.right().saturating_sub(state.chars().count() as u16 + 1);
            let tr = format!(" · {}", s.transport_name());
            if x + (tr.chars().count() as u16) < state_x {
                put(buf, ci, x, ci.y, &tr, st(DIM).bg(cb));
            }
            put_right(buf, ci, ci.y, state, st(col).bg(cb));
            let mut where_ = s.sources.join(" · ");
            if state == "stopped" && matches!(s.transport, Transport::Stdio { .. }) {
                where_.push_str(" · starts when an agent calls it");
            }
            if let Transport::Http { url } = &s.transport {
                let host = url
                    .split("//")
                    .nth(1)
                    .unwrap_or(url)
                    .split('/')
                    .next()
                    .unwrap_or("");
                where_.push_str(&format!(" · {host}"));
            }
            put(buf, ci, ci.x + 1, ci.y + 1, &where_, st(DIM).bg(cb));
            let (left_txt, lc) = match self.checks.get(&s.name) {
                Some(Check::Ok(h)) => {
                    let mut t = format!(
                        "{} tool{}",
                        h.tools.len(),
                        if h.tools.len() == 1 { "" } else { "s" }
                    );
                    if !h.prompts.is_empty() {
                        t.push_str(&format!(
                            " · {} prompt{}",
                            h.prompts.len(),
                            if h.prompts.len() == 1 { "" } else { "s" }
                        ));
                    }
                    (t, TEXT)
                }
                Some(Check::Failed(e, _)) => {
                    let missing: Vec<String> = s
                        .secrets()
                        .into_iter()
                        .filter(|(_, ok)| !ok)
                        .map(|(k, _)| k)
                        .collect();
                    if missing.is_empty() {
                        (e.clone(), TEXT)
                    } else {
                        (format!("{} missing", missing.join(", ")), TEXT)
                    }
                }
                _ => ("…".into(), DIM),
            };
            let u = by_server.get(&s.name);
            let usage = match u {
                Some(u) if u.calls > 0 => {
                    format!("{} calls · {}", u.calls, ms(u.ms / u.calls as u64))
                }
                _ => match self.checks.get(&s.name) {
                    Some(Check::Failed(e, _)) if e.len() < 24 => e.clone(),
                    _ => "not used today".into(),
                },
            };
            let uw = usage.chars().count() as u16;
            put_n(
                buf,
                ci.x + 1,
                ci.y + 2,
                &left_txt,
                st(lc).bg(cb),
                ci.width.saturating_sub(uw + 3),
            );
            put_right(buf, ci, ci.y + 2, &usage, st(DIM).bg(cb));
        }

        // ---- the selected server
        let Some(s) = self.current() else {
            boxed(buf, right, BORDER, PANEL, None);
            return;
        };
        let try_h = ratio(right.height, 210, 800).max(10);
        let det_h = 7u16;
        let det = Rect::new(right.x, right.y, right.width, det_h);
        let tools_r = Rect::new(
            right.x,
            det.bottom(),
            right.width,
            right.height.saturating_sub(det_h + try_h),
        );
        let try_r = Rect::new(right.x, tools_r.bottom(), right.width, try_h);
        let shell = role(Role::Shell);
        let di = boxed(
            buf,
            det,
            AMBER,
            shell,
            Some((&s.name, st(AMBER).add_modifier(Modifier::BOLD))),
        );
        let health = self.health(&s.name);
        let colw = 40u16.min(di.width / 2);
        let sep_x = di.right().saturating_sub(colw + 1);
        let kv = |buf: &mut Buffer, row: u16, k: &str, parts: Vec<(String, Style)>| {
            put(buf, di, di.x + 1, di.y + row, k, st(DIM).bg(shell));
            let mut x = di.x + 16;
            for (t, s) in parts {
                if x >= sep_x {
                    break;
                }
                let room = sep_x.saturating_sub(x + 1);
                let n = (t.chars().count() as u16).min(room);
                put_n(buf, x, di.y + row, &t, s.bg(shell), room);
                x += n;
            }
        };
        kv(buf, 0, "command", vec![(s.target(), st(TEXT))]);
        let proto = health
            .map(|h| format!(" · protocol {}", h.protocol))
            .unwrap_or_default();
        kv(
            buf,
            1,
            "transport",
            vec![(format!("{}{proto}", s.transport_name()), st(TEXT))],
        );
        kv(
            buf,
            2,
            "configured in",
            vec![
                (s.files.join(", "), st(TEXT)),
                (format!(" ({})", s.sources.join(", ")), st(DIM)),
            ],
        );
        let secrets = s.secrets();
        let mut sp: Vec<(String, Style)> = Vec::new();
        if secrets.is_empty() {
            sp.push(("none needed".into(), st(DIM)));
        }
        for (k, ok) in &secrets {
            sp.push((format!("{k} "), st(TEXT)));
            sp.push((
                if *ok { "✔ set " } else { "✖ not set " }.into(),
                st(if *ok { GREEN } else { RED }),
            ));
        }
        if !secrets.is_empty() {
            sp.push(("· value never shown".into(), st(DIM)));
        }
        kv(buf, 3, "secrets", sp);
        let mut users: Vec<(String, Style)> = Vec::new();
        for w in &self.who {
            if self
                .calls
                .iter()
                .any(|c| c.server == s.name && c.agent == w.name)
            {
                users.push((format!("● {}  ", w.name), st(w.color)));
            }
        }
        if users.is_empty() {
            users.push(("no calls seen yet".into(), st(DIM)));
        }
        kv(buf, 4, "used by", users);
        for r in 0..di.height {
            put(buf, di, sep_x, di.y + r, "│", st(BORDER).bg(shell));
        }
        let hx = sep_x + 2;
        let u = by_server.get(&s.name).cloned().unwrap_or_default();
        match self.checks.get(&s.name) {
            Some(Check::Ok(h)) => {
                let x = put_w(
                    buf,
                    di,
                    hx,
                    di.y,
                    "Health ",
                    st(TEXT).bg(shell).add_modifier(Modifier::BOLD),
                );
                put(
                    buf,
                    di,
                    x,
                    di.y,
                    &format!("✔ handshake {} ms", h.ms),
                    st(GREEN).bg(shell),
                );
                put(
                    buf,
                    di,
                    hx,
                    di.y + 1,
                    &format!(
                        "{} tools · {} resources · {} prompts",
                        h.tools.len(),
                        h.resources.len(),
                        h.prompts.len()
                    ),
                    st(DIM).bg(shell),
                );
            }
            Some(Check::Failed(e, _)) => {
                let x = put_w(
                    buf,
                    di,
                    hx,
                    di.y,
                    "Health ",
                    st(TEXT).bg(shell).add_modifier(Modifier::BOLD),
                );
                put(buf, di, x, di.y, &format!("✖ {e}"), st(RED).bg(shell));
                put(
                    buf,
                    di,
                    hx,
                    di.y + 1,
                    "l shows its log · r tries again",
                    st(DIM).bg(shell),
                );
            }
            _ => {
                let x = put_w(
                    buf,
                    di,
                    hx,
                    di.y,
                    "Health ",
                    st(TEXT).bg(shell).add_modifier(Modifier::BOLD),
                );
                put(buf, di, x, di.y, "◌ checking…", st(AMBER).bg(shell));
            }
        }
        let avg = if u.calls > 0 {
            ms(u.ms / u.calls as u64)
        } else {
            "—".into()
        };
        put(
            buf,
            di,
            hx,
            di.y + 2,
            &format!("{} calls today · {} failed · avg {avg}", u.calls, u.failed),
            st(DIM).bg(shell),
        );
        let x = put_w(
            buf,
            di,
            hx,
            di.y + 3,
            "context sent back ",
            st(TEXT).bg(shell),
        );
        put(
            buf,
            di,
            x,
            di.y + 3,
            &format!("≈ {} tokens", tokens(u.bytes)),
            st(AMBER).bg(shell),
        );
        let run = match self.running.get(&s.name) {
            Some(pid) => format!("running · pid {pid}"),
            None if matches!(s.transport, Transport::Http { .. }) => "remote · always on".into(),
            None => "not running · starts when an agent calls it".into(),
        };
        put(buf, di, hx, di.y + 4, &run, st(DIM).bg(shell));

        // ---- tabs + tools
        let ti = boxed(
            buf,
            tools_r,
            if self.in_tools { AMBER } else { BORDER },
            PANEL,
            None,
        );
        let mut x = ti.x + 1;
        for (i, name) in TABS.iter().enumerate() {
            let label = match (i, health) {
                (0, Some(h)) => format!("{name} ({})", h.tools.len()),
                (1, Some(h)) if !h.resources.is_empty() => {
                    format!("{name} ({})", h.resources.len())
                }
                (2, Some(h)) if !h.prompts.is_empty() => format!("{name} ({})", h.prompts.len()),
                _ => name.to_string(),
            };
            let s2 = if i == self.tab {
                st(TEXT)
                    .bg(PANEL)
                    .add_modifier(Modifier::UNDERLINED)
                    .underline_color(AMBER)
            } else {
                st(DIM).bg(PANEL)
            };
            x = put_w(buf, ti, x, ti.y, &label, s2) + 3;
        }
        let hint = "tab: next · ⏎ into the list";
        if x + hint.chars().count() as u16 + 2 <= ti.x + ti.width {
            put_right(buf, ti, ti.y, hint, st(DIM).bg(PANEL));
        }
        let lr = Rect::new(ti.x, ti.y + 2, ti.width, ti.height.saturating_sub(2));
        match self.tab {
            0 => self.tools_tab(lr, buf, s, health, &by_tool),
            1 | 2 => {
                let items: Vec<String> = match (self.tab, health) {
                    (1, Some(h)) => h.resources.clone(),
                    (2, Some(h)) => h.prompts.clone(),
                    _ => Vec::new(),
                };
                if items.is_empty() {
                    put(
                        buf,
                        lr,
                        lr.x + 1,
                        lr.y,
                        &format!("no {}", TABS[self.tab].to_lowercase()),
                        st(DIM).bg(PANEL),
                    );
                }
                for (i, it) in items.iter().enumerate().take(lr.height as usize) {
                    put(buf, lr, lr.x + 1, lr.y + i as u16, it, st(TEXT).bg(PANEL));
                }
            }
            3 => {
                let calls: Vec<&Call> = self.calls.iter().filter(|c| c.server == s.name).collect();
                if calls.is_empty() {
                    put(
                        buf,
                        lr,
                        lr.x + 1,
                        lr.y,
                        "no calls in today's agent logs",
                        st(DIM).bg(PANEL),
                    );
                }
                for (i, c) in calls.iter().enumerate().take(lr.height as usize) {
                    let y = lr.y + i as u16;
                    let ag = self
                        .who
                        .iter()
                        .find(|w| w.name == c.agent)
                        .map(|w| w.color)
                        .unwrap_or(TEXT);
                    let mut x = lr.x + 1;
                    x = put_w(
                        buf,
                        lr,
                        x,
                        y,
                        &format!("{}  ", hms(c.at)),
                        st(DIM).bg(PANEL),
                    );
                    x = put_w(
                        buf,
                        lr,
                        x,
                        y,
                        &format!("● {:<8} ", c.agent),
                        st(ag).bg(PANEL),
                    );
                    x = put_w(
                        buf,
                        lr,
                        x,
                        y,
                        &format!("{:<24} ", c.tool),
                        st(TEXT).bg(PANEL),
                    );
                    put(buf, lr, x, y, &c.args, st(DIM).bg(PANEL));
                }
            }
            4 => {
                put(
                    buf,
                    lr,
                    lr.x + 1,
                    lr.y,
                    "per-agent permissions for this server · p opens the grid",
                    st(DIM).bg(PANEL),
                );
                let rows: Vec<(String, String, bool)> = self
                    .perm_rows()
                    .into_iter()
                    .filter(|(r, _, _)| {
                        r == &mcp::rule(&s.name, None)
                            || r.starts_with(&format!("mcp__{}__", s.name))
                    })
                    .collect();
                for (i, (rule, label, _)) in rows
                    .iter()
                    .enumerate()
                    .take(lr.height.saturating_sub(1) as usize)
                {
                    let y = lr.y + 1 + i as u16;
                    put(buf, lr, lr.x + 1, y, label, st(TEXT).bg(PANEL));
                    for (j, w) in self.who.iter().enumerate() {
                        let lv = self
                            .perms
                            .get(&(j, rule.clone()))
                            .copied()
                            .unwrap_or(Level::Default);
                        put(
                            buf,
                            lr,
                            lr.x + 40 + j as u16 * 10,
                            y,
                            lv.label(),
                            st(level_color(lv)).bg(PANEL),
                        );
                        let _ = w;
                    }
                }
            }
            _ => {
                let log: Vec<String> = match self.checks.get(&s.name) {
                    Some(Check::Ok(h)) => h.stderr.clone(),
                    Some(Check::Failed(_, l)) => l.clone(),
                    _ => Vec::new(),
                };
                if log.is_empty() {
                    put(
                        buf,
                        lr,
                        lr.x + 1,
                        lr.y,
                        "the server wrote nothing to its log (stderr) during the check",
                        st(DIM).bg(PANEL),
                    );
                }
                let h = lr.height as usize;
                for (i, l) in log.iter().rev().take(h).rev().enumerate() {
                    put(buf, lr, lr.x + 1, lr.y + i as u16, l, st(TEXT).bg(PANEL));
                }
            }
        }

        // ---- try a tool
        let ri = boxed(
            buf,
            try_r,
            if self.try_form.as_ref().is_some_and(|t| t.editing) {
                AMBER
            } else {
                BORDER
            },
            PANEL,
            None,
        );
        let half = ri.width / 2;
        let lft = Rect::new(ri.x, ri.y, half.saturating_sub(1), ri.height);
        let rgt = Rect::new(
            ri.x + half + 1,
            ri.y,
            ri.width.saturating_sub(half + 1),
            ri.height,
        );
        for r in 0..ri.height {
            put(buf, ri, ri.x + half, ri.y + r, "│", st(BORDER).bg(PANEL));
        }
        put(
            buf,
            lft,
            lft.x + 1,
            lft.y,
            "Try a tool",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        if lft.width >= 46 {
            put_right(
                buf,
                lft,
                lft.y,
                "t · runs as you, not as an agent",
                st(DIM).bg(PANEL),
            );
        }
        match &self.try_form {
            None => {
                let tool = health
                    .and_then(|h| h.tools.get(self.tool_sel))
                    .map(|t| t.name.clone());
                let msg = match tool {
                    Some(t) => format!("t fills in {t}'s inputs here and runs it"),
                    None => "select a tool above (⏎ into the list), then t".into(),
                };
                put(buf, lft, lft.x + 1, lft.y + 2, &msg, st(DIM).bg(PANEL));
            }
            Some(t) => {
                let field_box =
                    |buf: &mut Buffer, y: u16, label: &str, val: &str, active: bool, ty: &str| {
                        put(
                            buf,
                            lft,
                            lft.x + 1,
                            y,
                            &label.chars().take(10).collect::<String>(),
                            st(DIM).bg(PANEL),
                        );
                        let fx = lft.x + 12;
                        let fw = lft.right().saturating_sub(fx + 1);
                        let fill = role(Role::Card);
                        buf.set_style(Rect::new(fx, y, fw, 1), Style::default().bg(fill));
                        let border = if active { AMBER } else { role(Role::Stroke) };
                        put(buf, lft, fx, y, "▕", st(border).bg(fill));
                        let shown = if val.is_empty() && !active {
                            ty.to_string()
                        } else {
                            format!("{val}{}", if active { "▏" } else { "" })
                        };
                        put_n(
                            buf,
                            fx + 2,
                            y,
                            &shown,
                            st(if val.is_empty() && !active { DIM } else { TEXT }).bg(fill),
                            fw.saturating_sub(3),
                        );
                        put(
                            buf,
                            lft,
                            fx + fw.saturating_sub(1),
                            y,
                            "▏",
                            st(border).bg(fill),
                        );
                    };
                field_box(buf, lft.y + 1, "tool", &format!("{} ▾", t.tool), false, "");
                let room = lft.height.saturating_sub(4) as usize;
                for (i, ((name, ty, req, _), val)) in
                    t.fields.iter().zip(&t.values).enumerate().take(room)
                {
                    let label = if *req {
                        format!("{name}*")
                    } else {
                        name.clone()
                    };
                    field_box(
                        buf,
                        lft.y + 2 + i as u16,
                        &label,
                        val,
                        t.editing && i == t.field,
                        ty,
                    );
                }
                if t.fields.is_empty() {
                    put(
                        buf,
                        lft,
                        lft.x + 12,
                        lft.y + 2,
                        "no inputs",
                        st(DIM).bg(PANEL),
                    );
                }
                let by = lft.bottom().saturating_sub(1);
                let x = put_w(
                    buf,
                    lft,
                    lft.x + 1,
                    by,
                    if t.running {
                        " … running "
                    } else {
                        " ⏎ Run "
                    },
                    st(INK).bg(AMBER).add_modifier(Modifier::BOLD),
                );
                put(
                    buf,
                    lft,
                    x + 2,
                    by,
                    {
                        let room = lft.right().saturating_sub(x + 3) as usize;
                        [
                            "fields from the tool's input schema · tab next · esc close",
                            "fields from the tool's input schema · tab next",
                            "fields from the tool's input schema",
                            "tab next · esc close",
                        ]
                        .into_iter()
                        .find(|t| t.chars().count() <= room)
                        .unwrap_or("")
                    },
                    st(DIM).bg(PANEL),
                );
            }
        }
        put(
            buf,
            rgt,
            rgt.x + 1,
            rgt.y,
            "Result",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        if let Some(r) = self.try_form.as_ref().and_then(|t| t.result.as_ref()) {
            let (mark, c) = if r.ok { ("✔", GREEN) } else { ("✖", RED) };
            let meta = format!(" · {} · ≈ {} tokens", size(r.bytes), tokens(r.bytes));
            let w = meta.chars().count() as u16;
            let mx = rgt.right().saturating_sub(w + 1);
            put(buf, rgt, mx, rgt.y, &meta, st(DIM).bg(PANEL));
            let m = format!("{mark} {}", ms(r.ms));
            put(
                buf,
                rgt,
                mx.saturating_sub(m.chars().count() as u16),
                rgt.y,
                &m,
                st(c).bg(PANEL),
            );
            let lines = json_lines(&r.text);
            let room = rgt.height.saturating_sub(3) as usize;
            for (i, spans) in lines.iter().take(room).enumerate() {
                let mut x = rgt.x + 1;
                for (t, c) in spans {
                    x = put_w(buf, rgt, x, rgt.y + 1 + i as u16, t, st(*c).bg(PANEL));
                }
            }
            put(
                buf,
                rgt,
                rgt.x + 1,
                rgt.bottom().saturating_sub(1),
                "c copy · S save to a file · ⏎ again",
                st(DIM).bg(PANEL),
            );
        } else if self.try_form.as_ref().is_some_and(|t| t.running) {
            put(
                buf,
                rgt,
                rgt.x + 1,
                rgt.y + 1,
                "◌ calling the tool…",
                st(AMBER).bg(PANEL),
            );
        } else {
            put(
                buf,
                rgt,
                rgt.x + 1,
                rgt.y + 1,
                "the answer shows here, with its size and token cost",
                st(DIM).bg(PANEL),
            );
        }

        self.keys(
            area,
            buf,
            &[
                ("⏎", "Details"),
                ("t", "Try a tool"),
                ("s", "Start / stop"),
                ("r", "Restart"),
                ("a", "Add server"),
                ("p", "Permissions"),
                ("l", "Server log"),
                ("/", "Filter"),
            ],
            "· sv mcp · ctrl+] M",
        );
    }

    fn tools_tab(
        &self,
        lr: Rect,
        buf: &mut Buffer,
        s: &Server,
        health: Option<&Health>,
        by_tool: &BTreeMap<(String, String), mcp::Usage>,
    ) {
        let Some(h) = health else {
            let msg = match self.checks.get(&s.name) {
                Some(Check::Failed(e, _)) => format!("✖ {e}"),
                _ => "◌ asking the server for its tools…".into(),
            };
            put(buf, lr, lr.x + 1, lr.y, &msg, st(DIM).bg(PANEL));
            return;
        };
        let ag: Vec<&Who> = self.who.iter().take(2).collect();
        let w = lr.width.saturating_sub(2);
        let name_w = (w / 4).clamp(12, 26);
        let pw = (w / 7).clamp(6, 18);
        let num_w: u16 = if w >= 90 { 8 } else { 6 };
        let desc_w = w.saturating_sub(name_w + pw + num_w * 2 + 4);
        let cols = [name_w, desc_w, num_w, num_w, pw];
        let nw = num_w as usize;
        let xs: Vec<u16> = {
            let mut x = lr.x + 1;
            cols.iter()
                .map(|w| {
                    let s = x;
                    x += w + 1;
                    s
                })
                .collect()
        };
        let head_y = lr.y.saturating_sub(1);
        put_n(buf, xs[0], head_y, "tool", st(DIM).bg(PANEL), cols[0]);
        put_n(
            buf,
            xs[1],
            head_y,
            "what it does",
            st(DIM).bg(PANEL),
            cols[1],
        );
        put_n(
            buf,
            xs[2],
            head_y,
            &format!("{:>nw$}", "calls"),
            st(DIM).bg(PANEL),
            num_w,
        );
        put_n(
            buf,
            xs[3],
            head_y,
            &format!("{:>nw$}", "avg"),
            st(DIM).bg(PANEL),
            num_w,
        );
        let names: Vec<&str> = ag.iter().map(|w| w.name.as_str()).collect();
        put_n(
            buf,
            xs[4],
            head_y,
            &names.join(" · "),
            st(DIM).bg(PANEL),
            cols[4],
        );
        let room = lr.height as usize;
        let first = self.tool_sel.saturating_sub(room.saturating_sub(1));
        for (i, t) in h.tools.iter().enumerate().skip(first).take(room) {
            let y = lr.y + (i - first) as u16;
            let selected = self.in_tools && i == self.tool_sel
                || (!self.in_tools && i == self.tool_sel && self.try_form.is_some());
            let bg = if selected { SELECT } else { PANEL };
            buf.set_style(Rect::new(lr.x, y, lr.width, 1), Style::default().bg(bg));
            put_n(
                buf,
                xs[0],
                y,
                &t.name,
                st(TEXT).bg(bg).add_modifier(Modifier::BOLD),
                cols[0],
            );
            put_n(buf, xs[1], y, &t.desc, st(DIM).bg(bg), cols[1]);
            let u = by_tool.get(&(s.name.clone(), t.name.clone()));
            let calls = u.map(|u| u.calls).unwrap_or(0);
            let avg = u
                .filter(|u| u.calls > 0)
                .map(|u| ms(u.ms / u.calls as u64))
                .unwrap_or_else(|| "—".into());
            put_n(
                buf,
                xs[2],
                y,
                &format!("{calls:>nw$}"),
                st(TEXT).bg(bg),
                num_w,
            );
            put_n(buf, xs[3], y, &format!("{avg:>nw$}"), st(DIM).bg(bg), num_w);
            let mut x = xs[4];
            for (j, _) in ag.iter().enumerate() {
                let rule = mcp::rule(&s.name, Some(&t.name));
                let lv = self
                    .perms
                    .get(&(j, rule))
                    .copied()
                    .unwrap_or(Level::Default);
                let lv = if lv == Level::Default {
                    self.perms
                        .get(&(j, mcp::rule(&s.name, None)))
                        .copied()
                        .unwrap_or(Level::Default)
                } else {
                    lv
                };
                if j > 0 {
                    x = put_w(buf, lr, x, y, " · ", st(DIM).bg(bg));
                }
                x = put_w(buf, lr, x, y, lv.label(), st(level_color(lv)).bg(bg));
            }
        }
    }

    /// Rows the add form needs at this width (the chips wrap).
    fn add_need(&self, width: u16) -> u16 {
        let mut x = 13u16;
        let mut rows = 1u16;
        for t in mcp::TEMPLATES {
            let w = t.0.chars().count() as u16 + 2;
            if 1 + x + w >= width {
                rows += 1;
                x = 13;
            }
            x += w + 1;
        }
        // title, chips, gap, 5 fields, gap, previews, gap, button
        1 + rows + 1 + 5 + 1 + 6 + 1 + 1
    }

    /// Draw the add form inside `ai`; returns each field's row.
    fn add_form(&self, ai: Rect, buf: &mut Buffer, shell: Color) -> [u16; 7] {
        let f = &self.add;
        put(
            buf,
            ai,
            ai.x + 1,
            ai.y,
            "Add a server",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        put_right(buf, ai, ai.y, "once, for every agent", st(DIM).bg(PANEL));
        let mut x = put_w(
            buf,
            ai,
            ai.x + 1,
            ai.y + 1,
            "ready-made: ",
            st(DIM).bg(PANEL),
        );
        let mut cy = ai.y + 1;
        for (i, t) in mcp::TEMPLATES.iter().enumerate() {
            let on = f.template == Some(i);
            let s2 = if on {
                st(INK).bg(AMBER)
            } else {
                st(TEXT).bg(role(Role::Card))
            };
            let w = t.0.chars().count() as u16 + 2;
            if x + w >= ai.right() {
                cy += 1;
                x = ai.x + 13;
            }
            x = put_w(buf, ai, x, cy, &format!(" {} ", t.0), s2) + 1;
        }
        if self.area == 2 && f.field == 0 {
            put(buf, ai, x + 1, cy, "←→ pick", st(DIM).bg(PANEL));
        }
        let active = |n: usize| self.area == 2 && f.field == n;
        let row = |buf: &mut Buffer, y: u16, label: &str, val: &str, n: usize, ok: bool| {
            put(buf, ai, ai.x + 1, y, label, st(DIM).bg(PANEL));
            let fx = ai.x + 12;
            let fw = ai.right().saturating_sub(fx + 1);
            let fill = role(Role::Card);
            buf.set_style(Rect::new(fx, y, fw, 1), Style::default().bg(fill));
            let border = if active(n) { AMBER } else { role(Role::Stroke) };
            put(buf, ai, fx, y, "▕", st(border).bg(fill));
            let text = if active(n) {
                format!("{val}▏")
            } else {
                val.to_string()
            };
            put_n(
                buf,
                fx + 2,
                y,
                &text,
                st(if ok { TEXT } else { DIM }).bg(fill),
                fw.saturating_sub(3),
            );
            put(
                buf,
                ai,
                fx + fw.saturating_sub(1),
                y,
                "▏",
                st(border).bg(fill),
            );
        };
        let y0 = cy + 2;
        row(
            buf,
            y0,
            "name",
            if f.name.is_empty() { "sentry" } else { &f.name },
            1,
            !f.name.is_empty(),
        );
        // transport: two choices
        put(buf, ai, ai.x + 1, y0 + 1, "transport", st(DIM).bg(PANEL));
        let ts = |on: bool| {
            if on {
                st(AMBER).bg(role(Role::Card))
            } else {
                st(DIM).bg(PANEL)
            }
        };
        let mut tx = ai.x + 12;
        tx = put_w(buf, ai, tx, y0 + 1, " http ", ts(f.http)) + 1;
        tx = put_w(buf, ai, tx, y0 + 1, " stdio ", ts(!f.http)) + 1;
        if active(2) {
            put(buf, ai, tx, y0 + 1, "←→ change", st(DIM).bg(PANEL));
        }
        let (tl, ph) = if f.http {
            ("url", "https://mcp.example.com/mcp")
        } else {
            ("command", "npx -y @scope/server-name")
        };
        row(
            buf,
            y0 + 2,
            tl,
            if f.target.is_empty() { ph } else { &f.target },
            3,
            !f.target.is_empty(),
        );
        let sec_set = !f.secret.is_empty()
            && f.secret
                .split([',', ' '])
                .filter(|k| !k.is_empty())
                .all(|k| std::env::var(k).is_ok());
        row(
            buf,
            y0 + 3,
            "secrets",
            if f.secret.is_empty() {
                "env var names, e.g. SENTRY_TOKEN (values stay in your env)"
            } else {
                &f.secret
            },
            4,
            !f.secret.is_empty(),
        );
        if !f.secret.is_empty() && !active(4) {
            let mark = if sec_set { "✔ set" } else { "✖ not set" };
            put_right(
                buf,
                ai,
                y0 + 3,
                &format!("{mark} "),
                st(if sec_set { GREEN } else { RED }).bg(role(Role::Card)),
            );
        }
        put(buf, ai, ai.x + 1, y0 + 4, "add to", st(DIM).bg(PANEL));
        let mut cx = ai.x + 12;
        for (i, name) in ["Claude Code", "Codex", "Gemini", "this project"]
            .iter()
            .enumerate()
        {
            let on = f.to[i];
            let cur = active(5) && f.to_sel == i;
            let s2 = if cur {
                st(INK).bg(AMBER)
            } else {
                st(if on { GREEN } else { DIM }).bg(PANEL)
            };
            cx = put_w(buf, ai, cx, y0 + 4, if on { "☑" } else { "☐" }, s2);
            cx = put_w(
                buf,
                ai,
                cx,
                y0 + 4,
                &format!(" {name}  "),
                if cur {
                    st(INK).bg(AMBER)
                } else {
                    st(TEXT).bg(PANEL)
                },
            );
        }
        // previews of what gets written
        let py = y0 + 6;
        let pv_h = ai.bottom().saturating_sub(py + 2).min(6);
        if pv_h >= 3 {
            let pw = ai.width.saturating_sub(3) / 2;
            let preview = f.server().ok();
            let l = boxed(buf, Rect::new(ai.x + 1, py, pw, pv_h), BORDER, shell, None);
            let r = boxed(
                buf,
                Rect::new(ai.x + 2 + pw, py, pw, pv_h),
                BORDER,
                shell,
                None,
            );
            put(buf, l, l.x + 1, l.y, "~/.claude.json", st(DIM).bg(shell));
            put(
                buf,
                r,
                r.x + 1,
                r.y,
                "~/.codex/config.toml",
                st(DIM).bg(shell),
            );
            match preview {
                Some(s) => {
                    let body = serde_json::to_string_pretty(&s.json(false)).unwrap_or_default();
                    let j = format!("\"{}\": {}", s.name, body);
                    for (i, spans) in json_lines(&j)
                        .iter()
                        .take(l.height.saturating_sub(1) as usize)
                        .enumerate()
                    {
                        let mut x = l.x + 1;
                        for (t, c) in spans {
                            x = put_w(buf, l, x, l.y + 1 + i as u16, t, st(*c).bg(shell));
                        }
                    }
                    for (i, line) in s
                        .toml()
                        .lines()
                        .take(r.height.saturating_sub(1) as usize)
                        .enumerate()
                    {
                        let c = if line.starts_with('[') { AMBER } else { TEXT };
                        if let Some((k, v)) = line.split_once(" = ") {
                            let x = put_w(
                                buf,
                                r,
                                r.x + 1,
                                r.y + 1 + i as u16,
                                &format!("{k} = "),
                                st(TEXT).bg(shell),
                            );
                            put(buf, r, x, r.y + 1 + i as u16, v, st(GREEN).bg(shell));
                        } else {
                            put(buf, r, r.x + 1, r.y + 1 + i as u16, line, st(c).bg(shell));
                        }
                    }
                }
                None => {
                    put(
                        buf,
                        l,
                        l.x + 1,
                        l.y + 1,
                        "pick a ready-made server",
                        st(DIM).bg(shell),
                    );
                    put(
                        buf,
                        r,
                        r.x + 1,
                        r.y + 1,
                        "or fill in the form",
                        st(DIM).bg(shell),
                    );
                }
            }
        }
        let by = ai.bottom().saturating_sub(1);
        let btn = if f.busy {
            " … checking "
        } else {
            " ⏎ Add & check "
        };
        let bs = if active(6) {
            st(INK)
                .bg(AMBER)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            st(INK).bg(AMBER).add_modifier(Modifier::BOLD)
        };
        let x = put_w(buf, ai, ai.x + 1, by, btn, bs);
        let (msg, c) = match &f.status {
            Some((m, ok)) => (
                m.as_str(),
                if m.starts_with('✖') {
                    RED
                } else if *ok {
                    GREEN
                } else {
                    AMBER
                },
            ),
            None => (
                "runs the handshake first · nothing is written if it fails",
                DIM,
            ),
        };
        if x + 2 + msg.chars().count() as u16 <= ai.right() {
            put(buf, ai, x + 2, by, msg, st(c).bg(PANEL));
        } else {
            put(
                buf,
                ai,
                ai.x + 1,
                by.saturating_sub(1),
                msg,
                st(c).bg(PANEL),
            );
        }

        [
            cy,
            y0,
            y0 + 1,
            y0 + 2,
            y0 + 3,
            y0 + 4,
            ai.bottom().saturating_sub(1),
        ]
    }

    fn render18(&self, area: Rect, buf: &mut Buffer, waiting: Option<&str>) {
        let pill = match waiting {
            Some(_) => vec![(
                "⚑ 1 waiting · alt+a".to_string(),
                st(INK).bg(AMBER).add_modifier(Modifier::BOLD),
            )],
            None => vec![(format!("{} calls today", self.calls.len()), st(TEXT))],
        };
        self.header(
            area,
            buf,
            "MCP · traffic · permissions · add a server",
            pill,
        );
        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let lw = ratio(body.width.saturating_sub(3), 125, 225);
        let left = Rect::new(body.x + 1, body.y, lw, body.height);
        let rx = left.right() + 1;
        let right = Rect::new(rx, body.y, body.right().saturating_sub(rx + 1), body.height);
        let stats_h = ratio(body.height, 170, 800).max(8);
        let traffic_r = Rect::new(
            left.x,
            left.y,
            left.width,
            left.height.saturating_sub(stats_h),
        );
        let stats_r = Rect::new(left.x, traffic_r.bottom(), left.width, stats_h);
        let perm_h = ratio(body.height, 300, 800).max(12);
        let perm_r = Rect::new(right.x, right.y, right.width, perm_h);
        let add_r = Rect::new(
            right.x,
            perm_r.bottom(),
            right.width,
            right.height.saturating_sub(perm_h),
        );
        let shell = role(Role::Shell);

        // ---- LIVE TRAFFIC
        let ti = boxed(
            buf,
            traffic_r,
            if self.area == 0 { AMBER } else { BORDER },
            shell,
            Some((
                "LIVE TRAFFIC · every tool call agents make",
                st(AMBER).add_modifier(Modifier::BOLD),
            )),
        );
        let chip = |buf: &mut Buffer, x: u16, y: u16, t: &str, on: bool| -> u16 {
            let s = if on {
                st(INK).bg(AMBER)
            } else {
                st(DIM).bg(role(Role::Card))
            };
            put_w(buf, ti, x, y, &format!(" {t} "), s) + 1
        };
        let mut x = ti.x + 1;
        x = chip(buf, x, ti.y, "all agents ▾", false);
        x = chip(buf, x, ti.y, "all servers ▾", false);
        x = chip(buf, x, ti.y, "errors only", self.errors_only);
        let hint = if self.paused {
            "▮▮ paused · space resumes"
        } else {
            "space pause · ⏎ full request / result"
        };
        if x + hint.chars().count() as u16 + 2 < ti.right() {
            put_right(
                buf,
                ti,
                ti.y,
                hint,
                st(if self.paused { AMBER } else { DIM }).bg(shell),
            );
        }
        // columns: time, agent, server · tool, arguments, result, ms, mark;
        // narrower windows drop the least useful ones (0 = hidden, ARGS = the rest)
        const ARGS: u16 = u16::MAX;
        let inner = ti.width.saturating_sub(2);
        let cw: [u16; 7] = if inner >= 90 {
            [10, 9, 26, ARGS, 9, 7, 2]
        } else if inner >= 66 {
            [8, 9, 18, ARGS, 8, 6, 2]
        } else {
            [0, 8, 14, ARGS, 0, 0, 2]
        };
        let fixed: u16 = cw
            .iter()
            .filter(|w| **w != ARGS && **w > 0)
            .map(|w| w + 1)
            .sum::<u16>()
            + 1;
        let args_w = inner.saturating_sub(fixed);
        let xs: Vec<u16> = {
            let mut x = ti.x + 1;
            cw.iter()
                .map(|w| {
                    let s = x;
                    match *w {
                        0 => {}
                        ARGS => x += args_w + 1,
                        w => x += w + 1,
                    }
                    s
                })
                .collect()
        };
        let cw: [u16; 7] = cw.map(|w| if w == ARGS { args_w } else { w });
        let hy = ti.y + 2;
        for (i, h) in [
            "time",
            "agent",
            "server · tool",
            "arguments",
            "result",
            "ms",
            "",
        ]
        .iter()
        .enumerate()
        {
            if cw[i] > 0 {
                let w = cw[i] as usize;
                let h = if i == 4 || i == 5 {
                    format!("{h:>w$}")
                } else {
                    h.to_string()
                };
                put_n(buf, xs[i], hy, &h, st(DIM).bg(shell), cw[i]);
            }
        }
        let banner_h = if waiting.is_some() { 3 } else { 0 };
        let rows_room = ti.bottom().saturating_sub(hy + 1 + banner_h) as usize;
        let calls = self.shown_calls();
        if calls.is_empty() {
            put(
                buf,
                ti,
                ti.x + 1,
                hy + 1,
                "No MCP calls in today's agent logs yet (Claude Code sessions in this project and its agents' worktrees).",
                st(DIM).bg(shell),
            );
        }
        let first = self.call_sel.saturating_sub(rows_room.saturating_sub(1));
        for (i, c) in calls.iter().enumerate().skip(first).take(rows_room) {
            let y = hy + 1 + (i - first) as u16;
            let sel = self.area == 0 && i == self.call_sel;
            let bg = if c.ok.is_none() {
                role(Role::Warn)
            } else if sel {
                SELECT
            } else {
                shell
            };
            buf.set_style(Rect::new(ti.x, y, ti.width, 1), Style::default().bg(bg));
            let ag = self
                .who
                .iter()
                .find(|w| w.name == c.agent)
                .map(|w| w.color)
                .unwrap_or(TEXT);
            if cw[0] > 0 {
                put_n(buf, xs[0], y, &hms(c.at), st(DIM).bg(bg), cw[0]);
            }
            put_n(
                buf,
                xs[1],
                y,
                &format!("● {}", c.agent),
                st(ag).bg(bg),
                cw[1],
            );
            put_n(
                buf,
                xs[2],
                y,
                &format!("{} · {}", c.server, c.tool),
                st(TEXT).bg(bg),
                cw[2],
            );
            put_n(buf, xs[3], y, &c.args, st(DIM).bg(bg), args_w);
            let res = match (c.ok, c.bytes) {
                (Some(false), _) => "failed".to_string(),
                (_, Some(b)) => size(b),
                _ => "—".into(),
            };
            if cw[4] > 0 {
                let rw = cw[4] as usize;
                put_n(
                    buf,
                    xs[4],
                    y,
                    &format!("{res:>rw$}"),
                    st(TEXT).bg(bg),
                    cw[4],
                );
            }
            let m = c.ms.map(ms).unwrap_or_else(|| "…".into());
            if cw[5] > 0 {
                let mw = cw[5] as usize;
                put_n(buf, xs[5], y, &format!("{m:>mw$}"), st(DIM).bg(bg), cw[5]);
            }
            let (mk, mc) = match c.ok {
                Some(true) => ("✔", GREEN),
                Some(false) => ("✖", RED),
                None => ("⚑", AMBER),
            };
            put(buf, ti, xs[6], y, mk, st(mc).bg(bg));
        }
        if let Some(w) = waiting {
            let br = Rect::new(ti.x, ti.bottom().saturating_sub(3), ti.width, 3);
            let bi = boxed(buf, br, AMBER, role(Role::Warn), None);
            let wb = role(Role::Warn);
            let x = put_w(buf, bi, bi.x + 1, bi.y, "⚑ ", st(AMBER).bg(wb));
            put_n(
                buf,
                x,
                bi.y,
                w,
                st(TEXT).bg(wb),
                bi.width.saturating_sub(30),
            );
            let keys = [
                ("y", GREEN, " allow · "),
                ("n", RED, " deny · "),
                ("a", AMBER, " always "),
            ];
            let w: u16 = keys
                .iter()
                .map(|(k, _, t)| (k.len() + t.chars().count()) as u16)
                .sum();
            let mut x = bi.right().saturating_sub(w + 1);
            for (k, c, t) in keys {
                x = put_w(buf, bi, x, bi.y, k, st(c).bg(wb));
                x = put_w(buf, bi, x, bi.y, t, st(TEXT).bg(wb));
            }
        }

        // ---- calls per minute · context by server
        let si = boxed(buf, stats_r, BORDER, PANEL, None);
        let half = si.width / 2;
        let a = Rect::new(si.x, si.y, half.saturating_sub(1), si.height);
        let b = Rect::new(
            si.x + half + 1,
            si.y,
            si.width.saturating_sub(half + 1),
            si.height,
        );
        put(
            buf,
            a,
            a.x + 1,
            a.y,
            if a.width >= 20 {
                "Calls per minute"
            } else {
                "Calls/min"
            },
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        if a.width >= 28 {
            put_right(buf, a, a.y, "last hour", st(DIM).bg(PANEL));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut per = vec![0usize; 60];
        for c in &self.calls {
            let ago = (now - c.at) / 60;
            if (0..60).contains(&ago) {
                per[59 - ago as usize] += 1;
            }
        }
        let bh = a.height.saturating_sub(3).max(1);
        let bw = a.width.saturating_sub(2) as usize;
        let max = per.iter().copied().max().unwrap_or(0).max(1);
        const LV: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        for col in 0..bw {
            let v = per[col * 60 / bw.max(1)];
            let eighths = (v * bh as usize * 8).div_ceil(max);
            for r in 0..bh {
                let from_bottom = (bh - 1 - r) as usize;
                let cell = eighths.saturating_sub(from_bottom * 8);
                let ch = if cell >= 8 {
                    "█"
                } else if cell == 0 {
                    if from_bottom == 0 { "▁" } else { " " }
                } else {
                    LV[cell - 1]
                };
                let c = if v == 0 { role(Role::Track) } else { CYAN };
                put(
                    buf,
                    a,
                    a.x + 1 + col as u16,
                    a.y + 1 + r,
                    ch,
                    st(c).bg(PANEL),
                );
            }
        }
        let failed = self.calls.iter().filter(|c| c.ok == Some(false)).count();
        put(
            buf,
            a,
            a.x + 1,
            a.bottom().saturating_sub(1),
            &format!("{} calls · {failed} failed", self.calls.len()),
            st(DIM).bg(PANEL),
        );
        for r in 0..si.height {
            put(buf, si, si.x + half, si.y + r, "│", st(BORDER).bg(PANEL));
        }
        put(
            buf,
            b,
            b.x + 1,
            b.y,
            if b.width >= 30 {
                "Context sent back to agents"
            } else {
                "Context sent back"
            },
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        if b.width >= 39 {
            put_right(buf, b, b.y, "by server", st(DIM).bg(PANEL));
        }
        let (by_server, _) = mcp::usage(&self.calls);
        let mut v: Vec<(&String, &mcp::Usage)> = by_server.iter().collect();
        v.sort_by_key(|(_, u)| std::cmp::Reverse(u.bytes));
        let top = v.first().map(|(_, u)| u.bytes).unwrap_or(0).max(1);
        let colors = [AMBER, MAGENTA, CYAN, GREEN, BLUE];
        if v.is_empty() {
            put(buf, b, b.x + 1, b.y + 2, "nothing yet", st(DIM).bg(PANEL));
        }
        for (i, (name, u)) in v
            .iter()
            .enumerate()
            .take(b.height.saturating_sub(2) as usize)
        {
            let y = b.y + 2 + i as u16;
            put_n(buf, b.x + 1, y, name, st(DIM).bg(PANEL), 12);
            let tw = b.width.saturating_sub(24) as usize;
            let fill = (u.bytes * tw / top).max(1);
            put(
                buf,
                b,
                b.x + 14,
                y,
                &"━".repeat(fill),
                st(colors[i % colors.len()]).bg(PANEL),
            );
            put(
                buf,
                b,
                b.x + 14 + fill as u16,
                y,
                &"━".repeat(tw.saturating_sub(fill)),
                st(role(Role::Track)).bg(PANEL),
            );
            put_right(buf, b, y, &tokens(u.bytes), st(TEXT).bg(PANEL));
        }

        // ---- Permissions
        let pi = boxed(
            buf,
            perm_r,
            if self.area == 1 { AMBER } else { BORDER },
            PANEL,
            None,
        );
        put(
            buf,
            pi,
            pi.x + 1,
            pi.y,
            "Permissions",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        let hint = if pi.width >= 60 {
            "←→↑↓ move · space cycles allow / ask / never"
        } else {
            "space: allow / ask / never"
        };
        if pi.width >= 40 {
            put_right(buf, pi, pi.y, hint, st(DIM).bg(PANEL));
        }
        let colw = 9u16;
        let name_w = pi.width.saturating_sub(colw * self.who.len() as u16 + 2);
        for (j, w) in self.who.iter().enumerate() {
            let x = pi.x + 1 + name_w + j as u16 * colw;
            put_n(
                buf,
                x,
                pi.y + 1,
                &center(&w.name, colw as usize),
                st(w.color).bg(PANEL),
                colw,
            );
        }
        put(
            buf,
            pi,
            pi.x + 1,
            pi.y + 1,
            "server · tool",
            st(DIM).bg(PANEL),
        );
        let rows = self.perm_rows();
        let room = pi.height.saturating_sub(3) as usize;
        let first = self.perm_row.saturating_sub(room.saturating_sub(1));
        if rows.is_empty() {
            put(
                buf,
                pi,
                pi.x + 1,
                pi.y + 2,
                "no servers yet",
                st(DIM).bg(PANEL),
            );
        }
        for (i, (rule, label, server)) in rows.iter().enumerate().skip(first).take(room) {
            let y = pi.y + 2 + (i - first) as u16;
            let sel_row = self.area == 1 && i == self.perm_row;
            let bg = if sel_row { SELECT } else { PANEL };
            buf.set_style(Rect::new(pi.x, y, pi.width, 1), Style::default().bg(bg));
            let s2 = if *server {
                st(TEXT).bg(bg).add_modifier(Modifier::BOLD)
            } else {
                st(TEXT).bg(bg)
            };
            put_n(buf, pi.x + 1, y, label, s2, name_w);
            for j in 0..self.who.len() {
                let lv = self
                    .perms
                    .get(&(j, rule.clone()))
                    .copied()
                    .unwrap_or(Level::Default);
                let x = pi.x + 1 + name_w + j as u16 * colw;
                let cell = center(lv.label(), colw as usize);
                let s3 = if sel_row && j == self.perm_col {
                    st(INK).bg(level_color(lv)).add_modifier(Modifier::BOLD)
                } else {
                    st(level_color(lv)).bg(bg)
                };
                put_n(buf, x, y, &cell, s3, colw);
            }
        }
        put(
            buf,
            pi,
            pi.x + 1,
            pi.bottom().saturating_sub(1),
            if pi.width >= 62 {
                "written to each agent's own settings · ⚑ asks show in alt+a"
            } else {
                "written to each agent's own settings"
            },
            st(DIM).bg(PANEL),
        );

        // ---- Add a server
        let ai = boxed(
            buf,
            add_r,
            if self.area == 2 { AMBER } else { BORDER },
            PANEL,
            None,
        );
        let need = self.add_need(ai.width);
        if ai.height >= need {
            self.add_form(ai, buf, shell);
        } else {
            // too short: draw it whole off-screen, show a window around the active field
            let v = Rect::new(ai.x, 0, ai.width, need);
            let mut tmp = Buffer::empty(Rect::new(0, 0, ai.right(), need));
            let rows = self.add_form(v, &mut tmp, shell);
            let at = rows[self.add.field.min(6)];
            let off = at
                .saturating_sub(ai.height / 2)
                .min(need.saturating_sub(ai.height));
            for dy in 0..ai.height {
                for x in ai.x..ai.right() {
                    buf[(x, ai.y + dy)] = tmp[(x, off + dy)].clone();
                }
            }
            let edge = ai.right();
            if off > 0 {
                put_n(buf, edge, ai.y, "▲", st(DIM).bg(PANEL), 1);
            }
            if off + ai.height < need {
                put_n(buf, edge, ai.bottom() - 1, "▼", st(DIM).bg(PANEL), 1);
            }
        }

        self.keys(
            area,
            buf,
            &[
                ("space", "Pause traffic"),
                ("⏎", "Call details"),
                ("p", "Permissions"),
                ("a", "Add server"),
                ("alt+a", "Approvals"),
                ("esc", "Back to servers"),
            ],
            "· traffic is read from the agents' logs · secrets are never shown",
        );
    }
}

// ------------------------------------------------------------------ drawing helpers

/// `v * num / den` without overflowing u16 (160 * 430 already does).
fn ratio(v: u16, num: u32, den: u32) -> u16 {
    (v as u32 * num / den) as u16
}

fn st(c: Color) -> Style {
    Style::default().fg(c)
}

fn level_color(l: Level) -> Color {
    match l {
        Level::Allow => GREEN,
        Level::Ask => AMBER,
        Level::Never => RED,
        Level::Default => DIM,
    }
}

fn boxed(
    buf: &mut Buffer,
    r: Rect,
    border: Color,
    bg: Color,
    title: Option<(&str, Style)>,
) -> Rect {
    if r.width < 3 || r.height < 2 {
        return Rect::new(r.x, r.y, 0, 0);
    }
    let mut b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(bg));
    if let Some((t, s)) = title {
        b = b.title(ratatui::text::Span::styled(format!(" {t} "), s.bg(bg)));
    }
    let inner = b.inner(r);
    b.render(r, buf);
    inner
}

/// Write within `area`, clipped; returns nothing.
fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, t: &str, s: Style) {
    if y >= area.y && y < area.bottom() && x >= area.x && x < area.right() {
        buf.set_stringn(x, y, t, (area.right() - x) as usize, s);
    }
}

/// Write within `area`; returns the x after the text.
fn put_w(buf: &mut Buffer, area: Rect, x: u16, y: u16, t: &str, s: Style) -> u16 {
    if y >= area.y && y < area.bottom() && x >= area.x && x < area.right() {
        let (nx, _) = buf.set_stringn(x, y, t, (area.right() - x) as usize, s);
        nx
    } else {
        x + t.chars().count() as u16
    }
}

fn put_n(buf: &mut Buffer, x: u16, y: u16, t: &str, s: Style, w: u16) {
    if w > 0 && x < buf.area.right() && y < buf.area.bottom() {
        buf.set_stringn(x, y, t, w as usize, s);
    }
}

fn put_right(buf: &mut Buffer, area: Rect, y: u16, t: &str, s: Style) {
    let w = t.chars().count() as u16;
    let x = area.right().saturating_sub(w + 1).max(area.x);
    put(buf, area, x, y, t, s);
}

fn center(t: &str, w: usize) -> String {
    let n = t.chars().count();
    if n >= w {
        return t.chars().take(w).collect();
    }
    let l = (w - n) / 2;
    format!("{}{}{}", " ".repeat(l), t, " ".repeat(w - n - l))
}

fn ms(m: u64) -> String {
    if m >= 1000 {
        format!("{:.1} s", m as f64 / 1000.0)
    } else {
        format!("{m} ms")
    }
}

fn size(b: usize) -> String {
    crate::sysmon::human(b as u64)
}

/// Rough tokens for a result (≈ 4 bytes a token): "540", "61k".
fn tokens(bytes: usize) -> String {
    let t = bytes / 4;
    if t >= 1000 {
        format!("{}k", t / 1000)
    } else {
        t.to_string()
    }
}

/// Local HH:MM:SS for a unix time.
fn hms(at: i64) -> String {
    let t = at as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// JSON text, coloured like the boards: keys cyan, strings green, numbers
/// amber, punctuation dim.
pub fn json_lines(text: &str) -> Vec<Vec<(String, Color)>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut spans: Vec<(String, Color)> = Vec::new();
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '"' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '"' {
                    if chars[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                let s: String = chars[i..(j + 1).min(chars.len())].iter().collect();
                let rest: String = chars[(j + 1).min(chars.len())..].iter().collect();
                let is_key = rest.trim_start().starts_with(':');
                spans.push((s, if is_key { CYAN } else { GREEN }));
                i = j + 1;
            } else if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
            {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_ascii_digit() || ".eE+-".contains(chars[j])) {
                    j += 1;
                }
                spans.push((chars[i..j].iter().collect(), AMBER));
                i = j;
            } else if let Some(w) = ["true", "false", "null"].iter().find_map(|k| {
                let rest: String = chars[i..].iter().take(k.len()).collect();
                (rest == *k).then_some(k.len())
            }) {
                spans.push((chars[i..i + w].iter().collect(), AMBER));
                i += w;
            } else {
                let dim = "{}[],:".contains(c);
                spans.push((c.to_string(), if dim { DIM } else { TEXT }));
                i += 1;
            }
        }
        out.push(spans);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_colours() {
        let l = json_lines("{\"total\": 1, \"name\": \"x\", \"ok\": true}");
        let spans = &l[0];
        assert!(spans.iter().any(|(t, c)| t == "\"total\"" && *c == CYAN));
        assert!(spans.iter().any(|(t, c)| t == "1" && *c == AMBER));
        assert!(spans.iter().any(|(t, c)| t == "\"x\"" && *c == GREEN));
        assert!(spans.iter().any(|(t, c)| t == "true" && *c == AMBER));
        assert_eq!(center("ab", 6), "  ab  ");
        assert_eq!(tokens(244_000), "61k");
        let mut f = AddForm::default();
        f.use_template(1);
        assert_eq!(f.name, "github");
        assert!(
            f.target
                .starts_with("npx -y @modelcontextprotocol/server-github")
        );
        let s = f.server().unwrap();
        assert_eq!(s.secrets, vec!["GITHUB_PERSONAL_ACCESS_TOKEN"]);
        f.use_template(5);
        assert!(f.http && f.server().is_ok());
    }
}

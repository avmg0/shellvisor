//! MCP servers (design boards 17 and 18): every server the agent CLIs are
//! configured with, whether it's running, what it offers (tools, prompts,
//! resources), trying a tool, the calls agents make, per-agent permissions,
//! and adding a server once for every agent.
//!
//! Shellvisor speaks MCP itself for the health check and "try a tool":
//! JSON-RPC over a server's stdin/stdout (stdio servers) or HTTP POST via
//! `curl` (streamable HTTP servers). The calls agents make are read from
//! Claude Code's session logs. Permissions are Claude Code's own
//! (`.claude/settings.local.json` in the project or the agent's worktree),
//! so the agent enforces them. Secret values are never shown or logged.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

pub const PROTOCOL: &str = "2025-06-18";

#[derive(Debug, Clone, PartialEq)]
pub enum Transport {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub name: String,
    pub transport: Transport,
    /// Environment the config sets: (name, value as written).
    pub env: Vec<(String, String)>,
    /// HTTP headers the config sets (Authorization…), values as written.
    pub headers: Vec<(String, String)>,
    /// "Claude Code", "Codex", "Gemini CLI", "this project · .mcp.json"
    pub sources: Vec<String>,
    /// Where it's written ("~/.claude.json"…).
    pub files: Vec<String>,
}

impl Server {
    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            Transport::Stdio { .. } => "stdio",
            Transport::Http { .. } => "http",
        }
    }

    /// "npx -y @modelcontextprotocol/server-github" or the URL.
    pub fn target(&self) -> String {
        match &self.transport {
            Transport::Stdio { command, args } => {
                let mut s = command.clone();
                for a in args {
                    s.push(' ');
                    s.push_str(a);
                }
                s
            }
            Transport::Http { url } => url.clone(),
        }
    }

    /// Secrets the config refers to, and whether each is available. A
    /// value written in the config counts as set; `${VAR}` needs VAR set.
    pub fn secrets(&self) -> Vec<(String, bool)> {
        let mut v = Vec::new();
        for (k, val) in self.env.iter().chain(self.headers.iter()) {
            let refs = env_refs(val);
            if refs.is_empty() {
                if looks_secret(k) || !val.is_empty() {
                    v.push((k.clone(), !val.trim().is_empty()));
                }
            } else {
                for r in refs {
                    let set = std::env::var(&r).is_ok_and(|x| !x.is_empty());
                    v.push((r, set));
                }
            }
        }
        v.sort();
        v.dedup();
        v
    }

    /// The word that identifies its process in `ps` ("server-github").
    fn process_key(&self) -> Option<String> {
        let Transport::Stdio { command, args } = &self.transport else {
            return None;
        };
        let runners = [
            "npx", "uvx", "node", "python", "python3", "bunx", "deno", "pipx", "docker",
        ];
        let base = command.rsplit('/').next().unwrap_or(command);
        if !runners.contains(&base) {
            return Some(base.to_string());
        }
        args.iter()
            .rev()
            .find(|a| {
                !a.starts_with('-') && !a.starts_with('/') && !a.contains('=') && !a.is_empty()
            })
            .or_else(|| args.iter().find(|a| !a.starts_with('-')))
            .map(|a| {
                a.rsplit('/')
                    .next()
                    .unwrap_or(a)
                    .trim_end_matches("@latest")
                    .to_string()
            })
    }
}

fn looks_secret(k: &str) -> bool {
    let k = k.to_ascii_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD", "AUTH"]
        .iter()
        .any(|w| k.contains(w))
}

/// `${VAR}`, `$VAR`, `${env:VAR}` references in a config value.
fn env_refs(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            let (start, braced) = if b.get(i + 1) == Some(&b'{') {
                (i + 2, true)
            } else {
                (i + 1, false)
            };
            let mut j = start;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || (braced && b[j] == b':'))
            {
                j += 1;
            }
            if j > start {
                let name = &v[start..j];
                let name = name.strip_prefix("env:").unwrap_or(name);
                out.push(name.to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

/// Expand `${VAR}` / `$VAR` from the environment.
fn expand(v: &str) -> String {
    let mut s = v.to_string();
    for r in env_refs(v) {
        let val = std::env::var(&r).unwrap_or_default();
        s = s
            .replace(&format!("${{env:{r}}}"), &val)
            .replace(&format!("${{{r}}}"), &val)
            .replace(&format!("${r}"), &val);
    }
    s
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn str_map(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(|m| m.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// A parsed config entry: transport, env, headers.
type Entry = (Transport, Vec<(String, String)>, Vec<(String, String)>);

/// One server entry from a JSON config (Claude Code / .mcp.json / Gemini).
fn from_json(name: &str, v: &Value) -> Option<Entry> {
    let env = str_map(v.get("env"));
    let headers = str_map(v.get("headers"));
    if let Some(url) = v
        .get("url")
        .or_else(|| v.get("httpUrl"))
        .or_else(|| v.get("serverUrl"))
        .and_then(|u| u.as_str())
    {
        return Some((
            Transport::Http {
                url: url.to_string(),
            },
            env,
            headers,
        ));
    }
    let command = v.get("command")?.as_str()?.to_string();
    let _ = name;
    Some((
        Transport::Stdio {
            command,
            args: str_list(v.get("args")),
        },
        env,
        headers,
    ))
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Every configured server, merged by name.
pub fn discover(home: &Path, cwd: &Path) -> Vec<Server> {
    let mut list: Vec<Server> = Vec::new();
    let mut add = |name: &str,
                   t: Transport,
                   env: Vec<(String, String)>,
                   headers: Vec<(String, String)>,
                   source: &str,
                   file: &str| {
        if let Some(s) = list.iter_mut().find(|s| s.name == name) {
            if !s.sources.iter().any(|x| x == source) {
                s.sources.push(source.to_string());
            }
            if !s.files.iter().any(|x| x == file) {
                s.files.push(file.to_string());
            }
            for (k, v) in env {
                if !s.env.iter().any(|(x, _)| *x == k) {
                    s.env.push((k, v));
                }
            }
            return;
        }
        list.push(Server {
            name: name.to_string(),
            transport: t,
            env,
            headers,
            sources: vec![source.to_string()],
            files: vec![file.to_string()],
        });
    };
    let mut json_servers = |obj: Option<&Value>, source: &str, file: &str| {
        if let Some(o) = obj.and_then(|o| o.as_object()) {
            for (name, v) in o {
                if v.get("disabled").and_then(|d| d.as_bool()) == Some(true) {
                    continue;
                }
                if let Some((t, env, headers)) = from_json(name, v) {
                    add(name, t, env, headers, source, file);
                }
            }
        }
    };
    let cwd_s = cwd.to_string_lossy().to_string();
    if let Some(j) = read_json(&home.join(".claude.json")) {
        json_servers(j.get("mcpServers"), "Claude Code", "~/.claude.json");
        json_servers(
            j.get("projects")
                .and_then(|p| p.get(&cwd_s))
                .and_then(|p| p.get("mcpServers")),
            "Claude Code",
            "~/.claude.json",
        );
    }
    if let Some(j) = read_json(&cwd.join(".mcp.json")) {
        json_servers(j.get("mcpServers"), "this project · .mcp.json", ".mcp.json");
    }
    if let Some(j) = read_json(&home.join(".gemini/settings.json")) {
        json_servers(j.get("mcpServers"), "Gemini CLI", "~/.gemini/settings.json");
    }
    if let Ok(t) = std::fs::read_to_string(home.join(".codex/config.toml")) {
        if let Ok(v) = toml::from_str::<toml::Table>(&t) {
            if let Some(m) = v.get("mcp_servers").and_then(|m| m.as_table()) {
                for (name, s) in m {
                    let Some(s) = s.as_table() else { continue };
                    let env: Vec<(String, String)> = s
                        .get("env")
                        .and_then(|e| e.as_table())
                        .map(|e| {
                            e.iter()
                                .map(|(k, v)| {
                                    (k.clone(), v.as_str().unwrap_or_default().to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let t = if let Some(url) = s.get("url").and_then(|u| u.as_str()) {
                        Transport::Http {
                            url: url.to_string(),
                        }
                    } else if let Some(c) = s.get("command").and_then(|c| c.as_str()) {
                        Transport::Stdio {
                            command: c.to_string(),
                            args: s
                                .get("args")
                                .and_then(|a| a.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|x| x.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        }
                    } else {
                        continue;
                    };
                    add(name, t, env, Vec::new(), "Codex", "~/.codex/config.toml");
                }
            }
        }
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

// ------------------------------------------------------------------ running

/// Running server processes: server name → pid (matched in `ps`).
pub fn running(servers: &[Server]) -> BTreeMap<String, i32> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,args="])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    running_in(servers, &out)
}

pub fn running_in(servers: &[Server], ps: &str) -> BTreeMap<String, i32> {
    let me = std::process::id() as i32;
    let mut m = BTreeMap::new();
    for s in servers {
        let Some(key) = s.process_key() else { continue };
        for line in ps.lines() {
            let line = line.trim();
            let Some((pid, args)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid.parse::<i32>() else {
                continue;
            };
            if pid == me || args.contains("shellvisor") || args.starts_with("ps ") {
                continue;
            }
            if args.split_whitespace().any(|w| {
                w.rsplit('/')
                    .next()
                    .unwrap_or(w)
                    .trim_end_matches("@latest")
                    == key
            }) {
                m.insert(s.name.clone(), pid);
                break;
            }
        }
    }
    m
}

// ------------------------------------------------------------------ protocol

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub desc: String,
    pub schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub protocol: String,
    pub server_name: String,
    pub tools: Vec<Tool>,
    pub prompts: Vec<String>,
    pub resources: Vec<String>,
    pub ms: u64,
    pub stderr: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallResult {
    pub ok: bool,
    pub text: String,
    pub ms: u64,
    pub bytes: usize,
}

/// A connection to a server for a few requests.
enum Conn {
    Stdio {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        lines: mpsc::Receiver<String>,
        stderr: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
        session: Option<String>,
    },
}

const TIMEOUT: Duration = Duration::from_secs(20);

impl Conn {
    fn open(s: &Server) -> Result<Conn, String> {
        match &s.transport {
            Transport::Stdio { command, args } => {
                let mut c = Command::new(expand(command));
                c.args(args.iter().map(|a| expand(a)));
                for (k, v) in &s.env {
                    c.env(k, expand(v));
                }
                c.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let mut child = c
                    .spawn()
                    .map_err(|e| format!("can't start `{command}`: {e}"))?;
                let stdin = child.stdin.take().ok_or("no stdin")?;
                let out = child.stdout.take().ok_or("no stdout")?;
                let err = child.stderr.take().ok_or("no stderr")?;
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    for l in BufReader::new(out).lines().map_while(Result::ok) {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                });
                let stderr = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let st = stderr.clone();
                std::thread::spawn(move || {
                    for l in BufReader::new(err).lines().map_while(Result::ok) {
                        let mut v = st.lock().unwrap();
                        v.push(l);
                        if v.len() > 200 {
                            v.remove(0);
                        }
                    }
                });
                Ok(Conn::Stdio {
                    child,
                    stdin,
                    lines: rx,
                    stderr,
                })
            }
            Transport::Http { url } => Ok(Conn::Http {
                url: expand(url),
                headers: s
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), expand(v)))
                    .collect(),
                session: None,
            }),
        }
    }

    fn stderr(&self) -> Vec<String> {
        match self {
            Conn::Stdio { stderr, .. } => stderr.lock().unwrap().clone(),
            Conn::Http { .. } => Vec::new(),
        }
    }

    fn send(&mut self, msg: &Value) -> Result<Option<Value>, String> {
        match self {
            Conn::Stdio {
                stdin,
                lines,
                child,
                ..
            } => {
                let line = format!("{msg}\n");
                stdin
                    .write_all(line.as_bytes())
                    .map_err(|e| format!("the server closed: {e}"))?;
                stdin.flush().map_err(|e| e.to_string())?;
                let Some(id) = msg.get("id").cloned() else {
                    return Ok(None); // a notification
                };
                let end = Instant::now() + TIMEOUT;
                loop {
                    let left = end.saturating_duration_since(Instant::now());
                    match lines.recv_timeout(left) {
                        Ok(l) => {
                            let Ok(v) = serde_json::from_str::<Value>(&l) else {
                                continue;
                            };
                            if v.get("id") == Some(&id) {
                                return Ok(Some(v));
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            return Err("no answer in 20 s".into());
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let code = child.try_wait().ok().flatten().and_then(|s| s.code());
                            return Err(match code {
                                Some(c) => format!("the server exited ({c})"),
                                None => "the server closed its output".into(),
                            });
                        }
                    }
                }
            }
            Conn::Http {
                url,
                headers,
                session,
            } => {
                let mut c = Command::new("curl");
                c.args(["-sS", "-i", "-X", "POST", "--max-time", "20"]);
                c.args(["-H", "Content-Type: application/json"]);
                c.args(["-H", "Accept: application/json, text/event-stream"]);
                c.args(["-H", &format!("MCP-Protocol-Version: {PROTOCOL}")]);
                if let Some(sid) = session.as_ref() {
                    c.args(["-H", &format!("Mcp-Session-Id: {sid}")]);
                }
                for (k, v) in headers.iter() {
                    c.args(["-H", &format!("{k}: {v}")]);
                }
                c.args(["--data-binary", &msg.to_string(), url.as_str()]);
                let out = c.output().map_err(|e| format!("curl: {e}"))?;
                if !out.status.success() {
                    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
                }
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                let (head, body) = split_http(&text);
                let status = head.lines().next().unwrap_or("").to_string();
                for l in head.lines() {
                    if let Some((k, v)) = l.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("mcp-session-id") {
                            *session = Some(v.trim().to_string());
                        }
                    }
                }
                let code: u16 = status
                    .split_whitespace()
                    .nth(1)
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                if code >= 400 {
                    return Err(match code {
                        401 | 403 => format!("{code}: not authorised — check the server's token"),
                        _ => format!("HTTP {code}"),
                    });
                }
                if msg.get("id").is_none() {
                    return Ok(None);
                }
                let id = msg.get("id").cloned();
                // JSON, or server-sent events with JSON in "data:" lines
                if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
                    return Ok(Some(v));
                }
                for l in body.lines() {
                    if let Some(d) = l.strip_prefix("data:") {
                        if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                            if v.get("id").cloned() == id {
                                return Ok(Some(v));
                            }
                        }
                    }
                }
                Err("the server's answer wasn't JSON".into())
            }
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let v = self.send(&msg)?.ok_or("no answer")?;
        if let Some(e) = v.get("error") {
            return Err(e
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("error")
                .to_string());
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    fn initialize(&mut self) -> Result<Value, String> {
        let r = self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "shellvisor", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(r)
    }

    fn close(self) {
        if let Conn::Stdio {
            mut child, stdin, ..
        } = self
        {
            drop(stdin);
            let end = Instant::now() + Duration::from_millis(300);
            while Instant::now() < end {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn split_http(t: &str) -> (&str, &str) {
    // curl -i may print "HTTP/1.1 100 Continue\r\n\r\n" first
    let mut rest = t;
    loop {
        let Some(i) = rest
            .find("\r\n\r\n")
            .map(|i| (i, 4))
            .or_else(|| rest.find("\n\n").map(|i| (i, 2)))
        else {
            return (rest, "");
        };
        let (head, body) = (&rest[..i.0], &rest[i.0 + i.1..]);
        if head.contains(" 100 ") && body.starts_with("HTTP/") {
            rest = body;
            continue;
        }
        return (head, body);
    }
}

/// Start (or reach) a server, handshake, and list what it offers.
pub fn check(s: &Server) -> Result<Health, (String, Vec<String>)> {
    let start = Instant::now();
    let mut c = Conn::open(s).map_err(|e| (e, Vec::new()))?;
    let init = match c.initialize() {
        Ok(v) => v,
        Err(e) => {
            let err = c.stderr();
            c.close();
            return Err((e, err));
        }
    };
    let ms = start.elapsed().as_millis() as u64;
    let caps = init.get("capabilities").cloned().unwrap_or(Value::Null);
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..10u64 {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        match c.request(10 + page, "tools/list", params) {
            Ok(r) => {
                for t in r
                    .get("tools")
                    .and_then(|t| t.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    tools.push(Tool {
                        name: t
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        desc: t
                            .get("description")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_string(),
                        schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
                    });
                }
                cursor = r
                    .get("nextCursor")
                    .and_then(|c| c.as_str())
                    .map(String::from);
                if cursor.is_none() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let names = |r: Result<Value, String>, key: &str, field: &str| -> Vec<String> {
        r.ok()
            .and_then(|r| r.get(key).and_then(|x| x.as_array()).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|x| x.get(field).and_then(|n| n.as_str()).map(String::from))
            .collect()
    };
    let prompts = if caps.get("prompts").is_some() {
        names(c.request(30, "prompts/list", json!({})), "prompts", "name")
    } else {
        Vec::new()
    };
    let resources = if caps.get("resources").is_some() {
        names(
            c.request(31, "resources/list", json!({})),
            "resources",
            "uri",
        )
    } else {
        Vec::new()
    };
    let stderr = c.stderr();
    c.close();
    Ok(Health {
        protocol: init
            .get("protocolVersion")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string(),
        server_name: init
            .get("serverInfo")
            .and_then(|i| i.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        tools,
        prompts,
        resources,
        ms,
        stderr,
    })
}

/// Call one tool (as you, not as an agent).
pub fn call(s: &Server, tool: &str, args: Value) -> CallResult {
    let start = Instant::now();
    let mut c = match Conn::open(s) {
        Ok(c) => c,
        Err(e) => {
            return CallResult {
                ok: false,
                text: e,
                ms: 0,
                bytes: 0,
            };
        }
    };
    let res = c.initialize().and_then(|_| {
        let t = Instant::now();
        c.request(2, "tools/call", json!({"name": tool, "arguments": args}))
            .map(|r| (r, t.elapsed()))
    });
    let err = c.stderr();
    c.close();
    match res {
        Ok((r, took)) => {
            let is_err = r.get("isError").and_then(|e| e.as_bool()).unwrap_or(false);
            let mut text = String::new();
            for item in r
                .get("content")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default()
            {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        text.push_str(item.get("text").and_then(|t| t.as_str()).unwrap_or(""));
                        text.push('\n');
                    }
                    Some(other) => text.push_str(&format!("[{other}]\n")),
                    None => {}
                }
            }
            if text.is_empty() {
                if let Some(sc) = r.get("structuredContent") {
                    text = serde_json::to_string_pretty(sc).unwrap_or_default();
                }
            }
            let text = pretty_if_json(text.trim_end());
            CallResult {
                ok: !is_err,
                bytes: text.len(),
                text,
                ms: took.as_millis() as u64,
            }
        }
        Err(e) => {
            let mut text = e;
            if let Some(l) = err.last() {
                text.push_str(&format!(" · {l}"));
            }
            CallResult {
                ok: false,
                text,
                ms: start.elapsed().as_millis() as u64,
                bytes: 0,
            }
        }
    }
}

fn pretty_if_json(t: &str) -> String {
    match serde_json::from_str::<Value>(t) {
        Ok(v) if v.is_object() || v.is_array() => {
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| t.to_string())
        }
        _ => t.to_string(),
    }
}

/// A tool's arguments as form fields: (name, type, required, description).
pub fn fields(schema: &Value) -> Vec<(String, String, bool, String)> {
    let req: Vec<String> = str_list(schema.get("required"));
    let mut v: Vec<(String, String, bool, String)> = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, p)| {
                    let ty = p
                        .get("type")
                        .and_then(|t| {
                            t.as_str().map(String::from).or_else(|| {
                                t.as_array()
                                    .and_then(|a| a.first())
                                    .and_then(|x| x.as_str())
                                    .map(String::from)
                            })
                        })
                        .unwrap_or_else(|| "string".into());
                    let d = p
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    (k.clone(), ty, req.contains(k), d)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(_, _, r, _)| !*r);
    v
}

/// Form values → JSON arguments, typed by the schema.
pub fn args_from(
    fields: &[(String, String, bool, String)],
    values: &[String],
) -> Result<Value, String> {
    let mut m = serde_json::Map::new();
    for ((name, ty, req, _), val) in fields.iter().zip(values) {
        let val = val.trim();
        if val.is_empty() {
            if *req {
                return Err(format!("{name} is required"));
            }
            continue;
        }
        let v = match ty.as_str() {
            "integer" => Value::from(
                val.parse::<i64>()
                    .map_err(|_| format!("{name}: a whole number"))?,
            ),
            "number" => Value::from(
                val.parse::<f64>()
                    .map_err(|_| format!("{name}: a number"))?,
            ),
            "boolean" => Value::from(matches!(val, "true" | "yes" | "1" | "on")),
            "array" | "object" => serde_json::from_str(val).map_err(|_| format!("{name}: JSON"))?,
            _ => Value::from(val),
        };
        m.insert(name.clone(), v);
    }
    Ok(Value::Object(m))
}

// ------------------------------------------------------------------ traffic

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Unix seconds.
    pub at: i64,
    pub agent: String,
    pub server: String,
    pub tool: String,
    pub args: String,
    /// Result size in bytes (None: no answer yet).
    pub bytes: Option<usize>,
    pub ms: Option<u64>,
    /// None: waiting; Some(true) ok; Some(false) failed.
    pub ok: Option<bool>,
}

/// Claude Code's folder name for a project path (`/a/b.c` → `-a-b-c`).
pub fn claude_key(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
fn parse_ts(s: &str) -> Option<i64> {
    parse_ts_ms(s).map(|ms| ms.div_euclid(1000))
}

/// Unix milliseconds for an ISO timestamp.
fn parse_ts_ms(s: &str) -> Option<i64> {
    // 2026-11-05T22:34:51.123Z (UTC)
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next()?.parse().ok()?;
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let m2 = (mo + 9) % 12;
    let doy = (153 * m2 + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60) * 1000 + (sec * 1000.0).round() as i64)
}

fn summary(v: &Value) -> String {
    match v {
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k} {}", s.chars().take(60).collect::<String>())
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// MCP tool calls in Claude Code session logs changed since `since`, for
/// the given project folders: (log folder key, who it is).
pub fn traffic(home: &Path, who: &[(String, String)], since: SystemTime) -> Vec<Call> {
    let mut calls: Vec<Call> = Vec::new();
    for (key, agent) in who {
        let dir = home.join(".claude/projects").join(key);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            if e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|m| m < since)
            {
                continue;
            }
            if let Ok(t) = std::fs::read_to_string(&p) {
                calls.extend(parse_log(&t, agent));
            }
        }
    }
    calls.sort_by_key(|c| std::cmp::Reverse(c.at));
    calls.truncate(500);
    calls
}

/// Tool uses named `mcp__<server>__<tool>` and their results, from one log.
pub fn parse_log(text: &str, agent: &str) -> Vec<Call> {
    let mut calls: Vec<(String, Call, i64)> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at_ms = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(parse_ts_ms)
            .unwrap_or(0);
        let at = at_ms.div_euclid(1000);
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let Some(rest) = name.strip_prefix("mcp__") else {
                        continue;
                    };
                    let Some((server, tool)) = rest.split_once("__") else {
                        continue;
                    };
                    let id = item
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    calls.push((
                        id,
                        Call {
                            at,
                            agent: agent.to_string(),
                            server: server.to_string(),
                            tool: tool.to_string(),
                            args: summary(item.get("input").unwrap_or(&Value::Null)),
                            bytes: None,
                            ms: None,
                            ok: None,
                        },
                        at_ms,
                    ));
                }
                Some("tool_result") => {
                    let id = item
                        .get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("");
                    if let Some((_, c, start)) = calls.iter_mut().find(|(i, _, _)| i == id) {
                        let size = match item.get("content") {
                            Some(Value::String(s)) => s.len(),
                            Some(Value::Array(a)) => a
                                .iter()
                                .map(|x| {
                                    x.get("text")
                                        .and_then(|t| t.as_str())
                                        .map(str::len)
                                        .unwrap_or(0)
                                })
                                .sum(),
                            _ => 0,
                        };
                        c.bytes = Some(size);
                        c.ms = Some((at_ms - *start).max(0) as u64);
                        c.ok = Some(
                            !item
                                .get("is_error")
                                .and_then(|e| e.as_bool())
                                .unwrap_or(false),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    calls.into_iter().map(|(_, c, _)| c).collect()
}

/// Per server: (calls, failed, avg ms, result bytes); per tool: (calls, avg ms).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub calls: usize,
    pub failed: usize,
    pub ms: u64,
    pub bytes: usize,
}

pub fn usage(calls: &[Call]) -> (BTreeMap<String, Usage>, BTreeMap<(String, String), Usage>) {
    let mut by_server: BTreeMap<String, Usage> = BTreeMap::new();
    let mut by_tool: BTreeMap<(String, String), Usage> = BTreeMap::new();
    for c in calls {
        for u in [
            by_server.entry(c.server.clone()).or_default(),
            by_tool
                .entry((c.server.clone(), c.tool.clone()))
                .or_default(),
        ] {
            u.calls += 1;
            if c.ok == Some(false) {
                u.failed += 1;
            }
            u.ms += c.ms.unwrap_or(0);
            u.bytes += c.bytes.unwrap_or(0);
        }
    }
    (by_server, by_tool)
}

// ------------------------------------------------------------------ permissions

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Default,
    Allow,
    Ask,
    Never,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Default => "—",
            Level::Allow => "allow",
            Level::Ask => "ask",
            Level::Never => "never",
        }
    }

    pub fn next(self) -> Level {
        match self {
            Level::Default => Level::Allow,
            Level::Allow => Level::Ask,
            Level::Ask => Level::Never,
            Level::Never => Level::Default,
        }
    }
}

/// Claude Code's rule name: `mcp__server` (all its tools) or `mcp__server__tool`.
pub fn rule(server: &str, tool: Option<&str>) -> String {
    match tool {
        Some(t) => format!("mcp__{server}__{t}"),
        None => format!("mcp__{server}"),
    }
}

pub fn settings_path(dir: &Path) -> PathBuf {
    dir.join(".claude/settings.local.json")
}

/// The level a settings file gives a rule.
pub fn level(settings: &Value, rule: &str) -> Level {
    let has = |k: &str| {
        settings
            .get("permissions")
            .and_then(|p| p.get(k))
            .and_then(|a| a.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(rule)))
    };
    if has("deny") {
        Level::Never
    } else if has("ask") {
        Level::Ask
    } else if has("allow") {
        Level::Allow
    } else {
        Level::Default
    }
}

/// Set a rule's level in a settings file (other settings kept).
pub fn set_level(path: &Path, rule: &str, lv: Level) -> std::io::Result<()> {
    let mut v: Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !v.is_object() {
        v = json!({});
    }
    let perms = v
        .as_object_mut()
        .unwrap()
        .entry("permissions")
        .or_insert_with(|| json!({}));
    if !perms.is_object() {
        *perms = json!({});
    }
    let po = perms.as_object_mut().unwrap();
    for k in ["allow", "ask", "deny"] {
        if let Some(a) = po.get_mut(k).and_then(|a| a.as_array_mut()) {
            a.retain(|x| x.as_str() != Some(rule));
        }
    }
    let key = match lv {
        Level::Allow => Some("allow"),
        Level::Ask => Some("ask"),
        Level::Never => Some("deny"),
        Level::Default => None,
    };
    if let Some(k) = key {
        let a = po.entry(k).or_insert_with(|| json!([]));
        if let Some(a) = a.as_array_mut() {
            a.push(Value::from(rule));
        }
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n",
    )
}

// ------------------------------------------------------------------ add

/// Ready-made servers: (name, transport, command or URL, args, secret).
pub const TEMPLATES: &[(&str, &str, &str, &[&str], &str)] = &[
    (
        "filesystem",
        "stdio",
        "npx",
        &["-y", "@modelcontextprotocol/server-filesystem", "."],
        "",
    ),
    (
        "github",
        "stdio",
        "npx",
        &["-y", "@modelcontextprotocol/server-github"],
        "GITHUB_PERSONAL_ACCESS_TOKEN",
    ),
    (
        "postgres",
        "stdio",
        "npx",
        &[
            "-y",
            "@modelcontextprotocol/server-postgres",
            "postgresql://localhost/mydb",
        ],
        "",
    ),
    (
        "playwright",
        "stdio",
        "npx",
        &["@playwright/mcp@latest"],
        "",
    ),
    ("fetch", "stdio", "uvx", &["mcp-server-fetch"], ""),
    ("sentry", "http", "https://mcp.sentry.dev/mcp", &[], ""),
];

/// What to write: name, transport, the server, secrets by name.
#[derive(Debug, Clone, PartialEq)]
pub struct NewServer {
    pub name: String,
    pub transport: Transport,
    /// Env var names it needs; written as `${NAME}` so values stay in your env.
    pub secrets: Vec<String>,
}

impl NewServer {
    pub fn as_server(&self) -> Server {
        Server {
            name: self.name.clone(),
            transport: self.transport.clone(),
            env: self
                .secrets
                .iter()
                .map(|k| (k.clone(), format!("${{{k}}}")))
                .collect(),
            headers: Vec::new(),
            sources: Vec::new(),
            files: Vec::new(),
        }
    }

    /// The entry for Claude Code / .mcp.json / Gemini.
    pub fn json(&self, gemini: bool) -> Value {
        let env: serde_json::Map<String, Value> = self
            .secrets
            .iter()
            .map(|k| (k.clone(), Value::from(format!("${{{k}}}"))))
            .collect();
        match &self.transport {
            Transport::Stdio { command, args } => {
                let mut o = json!({"command": command, "args": args});
                if !gemini {
                    o["type"] = Value::from("stdio");
                }
                if !env.is_empty() {
                    o["env"] = Value::Object(env);
                }
                o
            }
            Transport::Http { url } => {
                if gemini {
                    json!({"httpUrl": url})
                } else {
                    json!({"type": "http", "url": url})
                }
            }
        }
    }

    /// The `[mcp_servers.<name>]` table for Codex.
    pub fn toml(&self) -> String {
        let q = |s: &str| format!("{:?}", s);
        let mut t = format!("[mcp_servers.{}]\n", self.name);
        match &self.transport {
            Transport::Stdio { command, args } => {
                t.push_str(&format!("command = {}\n", q(command)));
                let a: Vec<String> = args.iter().map(|x| q(x)).collect();
                t.push_str(&format!("args = [{}]\n", a.join(", ")));
                if !self.secrets.is_empty() {
                    let e: Vec<String> = self
                        .secrets
                        .iter()
                        .map(|k| format!("{k} = {}", q(&format!("${{{k}}}"))))
                        .collect();
                    t.push_str(&format!("env = {{ {} }}\n", e.join(", ")));
                }
            }
            Transport::Http { url } => t.push_str(&format!("url = {}\n", q(url))),
        }
        t
    }
}

fn add_json(path: &Path, nested: Option<&str>, name: &str, entry: Value) -> std::io::Result<()> {
    let mut v: Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if !v.is_object() {
        v = json!({});
    }
    let mut obj = v.as_object_mut().unwrap();
    if let Some(n) = nested {
        let e = obj.entry(n).or_insert_with(|| json!({}));
        obj = e
            .as_object_mut()
            .ok_or_else(|| std::io::Error::other("unexpected layout"))?;
    }
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    servers
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("mcpServers isn't an object"))?
        .insert(name.to_string(), entry);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // write next to it, then move over: never a half-written config
    let tmp = path.with_extension("shellvisor-tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n",
    )?;
    std::fs::rename(tmp, path)
}

/// Add a server to the chosen agents: [Claude Code, Codex, Gemini, this project].
pub fn add(home: &Path, cwd: &Path, s: &NewServer, to: [bool; 4]) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    if to[0] {
        add_json(&home.join(".claude.json"), None, &s.name, s.json(false))
            .map_err(|e| format!("~/.claude.json: {e}"))?;
        done.push("Claude Code".to_string());
    }
    if to[1] {
        let p = home.join(".codex/config.toml");
        let mut t = std::fs::read_to_string(&p).unwrap_or_default();
        if t.contains(&format!("[mcp_servers.{}]", s.name)) {
            return Err(format!("Codex already has a server called {}", s.name));
        }
        if !t.is_empty() && !t.ends_with('\n') {
            t.push('\n');
        }
        if !t.is_empty() {
            t.push('\n');
        }
        t.push_str(&s.toml());
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&p, t).map_err(|e| format!("~/.codex/config.toml: {e}"))?;
        done.push("Codex".to_string());
    }
    if to[2] {
        add_json(
            &home.join(".gemini/settings.json"),
            None,
            &s.name,
            s.json(true),
        )
        .map_err(|e| format!("~/.gemini/settings.json: {e}"))?;
        done.push("Gemini CLI".to_string());
    }
    if to[3] {
        add_json(&cwd.join(".mcp.json"), None, &s.name, s.json(false))
            .map_err(|e| format!(".mcp.json: {e}"))?;
        done.push("this project".to_string());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sv-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn discovers_and_merges_configs() {
        let home = tmp("home");
        let cwd = home.join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"github":{"command":"npx","args":["-y","@modelcontextprotocol/server-github"],"env":{"GITHUB_TOKEN":"${GH_TEST_UNSET_X}"}}},
                "projects":{"__CWD__":{"mcpServers":{"fetch":{"command":"uvx","args":["mcp-server-fetch"]}}}}}"#
                .replace("__CWD__", &cwd.to_string_lossy()),
        )
        .unwrap();
        std::fs::write(cwd.join(".mcp.json"), r#"{"mcpServers":{"pg":{"command":"npx","args":["-y","@modelcontextprotocol/server-postgres","postgresql://x"]}}}"#).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/config.toml"),
            "[mcp_servers.github]\ncommand = \"npx\"\nargs = [\"-y\", \"@modelcontextprotocol/server-github\"]\n\n[mcp_servers.linear]\nurl = \"https://mcp.linear.app/mcp\"\n",
        )
        .unwrap();
        let s = discover(&home, &cwd);
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["fetch", "github", "linear", "pg"]);
        let gh = &s[1];
        assert_eq!(gh.sources, vec!["Claude Code", "Codex"]);
        assert_eq!(gh.secrets(), vec![("GH_TEST_UNSET_X".to_string(), false)]);
        assert_eq!(gh.process_key().as_deref(), Some("server-github"));
        assert_eq!(s[2].transport_name(), "http");
        assert_eq!(s[0].process_key().as_deref(), Some("mcp-server-fetch"));
        let ps = "  101 node /x/node_modules/.bin/mcp-server-github\n  202 /usr/bin/python3 -m x\n  303 uv tool run mcp-server-fetch\n";
        let r = running_in(&s, ps);
        assert_eq!(r.get("fetch"), Some(&303));
        assert_eq!(r.get("github"), None, "server-github vs mcp-server-github");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn traffic_from_claude_logs() {
        let log = r#"{"type":"assistant","timestamp":"2026-11-05T22:34:46.000Z","message":{"content":[{"type":"tool_use","id":"t1","name":"mcp__github__get_file_contents","input":{"path":"src/remote.rs"}}]}}
{"type":"user","timestamp":"2026-11-05T22:34:46.280Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"0123456789"}]}}
{"type":"assistant","timestamp":"2026-11-05T22:34:50.000Z","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{}},{"type":"tool_use","id":"t3","name":"mcp__linear__list_issues","input":{"team":"SHV"}}]}}
{"type":"user","timestamp":"2026-11-05T22:34:52.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":[{"type":"text","text":"401"}],"is_error":true}]}}
{"type":"assistant","timestamp":"2026-11-05T22:35:00.000Z","message":{"content":[{"type":"tool_use","id":"t4","name":"mcp__github__push_files","input":{"files":3}}]}}
"#;
        let c = parse_log(log, "api");
        assert_eq!(c.len(), 3);
        assert_eq!(
            (c[0].server.as_str(), c[0].tool.as_str()),
            ("github", "get_file_contents")
        );
        assert_eq!((c[0].bytes, c[0].ok), (Some(10), Some(true)));
        assert_eq!(c[0].args, "path src/remote.rs");
        assert_eq!(c[1].ok, Some(false));
        assert_eq!(c[1].ms, Some(2000));
        assert_eq!(c[2].ok, None, "still waiting");
        let (s, t) = usage(&c);
        assert_eq!(s["github"].calls, 2);
        assert_eq!(s["linear"].failed, 1);
        assert_eq!(
            t[&("github".to_string(), "push_files".to_string())].calls,
            1
        );
        assert_eq!(parse_ts("1970-01-02T00:00:01Z"), Some(86401));
        assert_eq!(claude_key(Path::new("/Users/a/my.app")), "-Users-a-my-app");
    }

    #[test]
    fn permissions_round_trip() {
        let d = tmp("perm");
        let p = settings_path(&d);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            r#"{"permissions":{"allow":["Bash(ls:*)"]},"model":"x"}"#,
        )
        .unwrap();
        let r = rule("github", Some("push_files"));
        set_level(&p, &r, Level::Ask).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(level(&v, &r), Level::Ask);
        assert_eq!(v["model"], "x");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        set_level(&p, &r, Level::Never).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(level(&v, &r), Level::Never);
        assert_eq!(v["permissions"]["ask"].as_array().unwrap().len(), 0);
        set_level(&p, &r, Level::Default).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(level(&v, &r), Level::Default);
        assert_eq!(Level::Default.next(), Level::Allow);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn adding_writes_each_format() {
        let home = tmp("add");
        let cwd = home.join("p");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"numStartups":3,"mcpServers":{"a":{"command":"x"}}}"#,
        )
        .unwrap();
        let s = NewServer {
            name: "github".into(),
            transport: Transport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            },
            secrets: vec!["GITHUB_PERSONAL_ACCESS_TOKEN".into()],
        };
        let done = add(&home, &cwd, &s, [true, true, true, true]).unwrap();
        assert_eq!(done.len(), 4);
        let c: Value =
            serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(c["numStartups"], 3, "the rest of the file kept");
        assert_eq!(
            c["mcpServers"]["github"]["env"]["GITHUB_PERSONAL_ACCESS_TOKEN"],
            "${GITHUB_PERSONAL_ACCESS_TOKEN}"
        );
        let t = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(
            t.contains("[mcp_servers.github]")
                && t.contains("args = [\"-y\", \"@modelcontextprotocol/server-github\"]"),
            "{t}"
        );
        let back = discover(&home, &cwd);
        let gh = back.iter().find(|x| x.name == "github").unwrap();
        assert_eq!(gh.sources.len(), 4, "{:?}", gh.sources);
        assert!(
            add(&home, &cwd, &s, [false, true, false, false]).is_err(),
            "no duplicate Codex table"
        );
        let h = NewServer {
            name: "sentry".into(),
            transport: Transport::Http {
                url: "https://mcp.sentry.dev/mcp".into(),
            },
            secrets: vec![],
        };
        assert_eq!(h.json(true)["httpUrl"], "https://mcp.sentry.dev/mcp");
        assert!(h.toml().contains("url = \"https://mcp.sentry.dev/mcp\""));
        let f = fields(
            &json!({"properties":{"q":{"type":"string"},"n":{"type":"integer"}},"required":["q"]}),
        );
        assert_eq!(f[0].0, "q");
        assert_eq!(
            args_from(&f, &["x".into(), "3".into()]).unwrap(),
            json!({"q":"x","n":3})
        );
        assert!(args_from(&f, &["".into(), "".into()]).is_err());
        assert_eq!(
            env_refs("Bearer ${API_KEY} $OTHER ${env:Z}"),
            vec!["API_KEY", "OTHER", "Z"]
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn speaks_mcp_over_stdio() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let d = tmp("srv");
        let script = d.join("server.py");
        std::fs::write(&script, FAKE_SERVER).unwrap();
        let s = Server {
            name: "fake".into(),
            transport: Transport::Stdio {
                command: "python3".into(),
                args: vec![script.display().to_string()],
            },
            env: vec![],
            headers: vec![],
            sources: vec![],
            files: vec![],
        };
        let h = check(&s).unwrap();
        assert_eq!(h.protocol, PROTOCOL);
        assert_eq!(h.tools.len(), 2);
        assert_eq!(h.tools[0].name, "echo");
        assert_eq!(h.prompts, vec!["summarise"]);
        let r = call(&s, "echo", json!({"text": "hello"}));
        assert!(r.ok, "{r:?}");
        assert_eq!(r.text, "HELLO");
        let r = call(&s, "fail", json!({}));
        assert!(!r.ok);
        let _ = std::fs::remove_dir_all(d);
    }

    /// A tiny MCP server for tests (also used by the end-to-end test).
    pub const FAKE_SERVER: &str = r#"import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth = m.get("method")
    if meth == "initialize":
        r = {"protocolVersion": m["params"]["protocolVersion"], "capabilities": {"tools": {}, "prompts": {}}, "serverInfo": {"name": "fake", "version": "1"}}
    elif meth == "tools/list":
        r = {"tools": [{"name": "echo", "description": "Say it back, loud", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}, {"name": "fail", "description": "Always fails", "inputSchema": {"type": "object", "properties": {}}}]}
    elif meth == "prompts/list":
        r = {"prompts": [{"name": "summarise"}]}
    elif meth == "tools/call":
        a = m["params"]
        if a["name"] == "echo":
            r = {"content": [{"type": "text", "text": a["arguments"]["text"].upper()}]}
        else:
            r = {"content": [{"type": "text", "text": "nope"}], "isError": True}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "no such method"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;
}

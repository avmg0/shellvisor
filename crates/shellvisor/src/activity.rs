//! Downloads and other long jobs, spotted from the command line.
//!
//! When a shell starts a command, its text is matched against a small table
//! (git clone, npm install, pip, uv, brew, docker pull, cargo, curl…). While
//! it runs, the pane's bottom rows give its progress, speed and time left
//! (from what the tool prints: git, curl, wget, pip, tqdm, brew…; the time
//! left is worked out from the progress when the tool doesn't say). No hooks
//! into the tools themselves — it works with any version.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// A recognised long-running command.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    /// "npm install", "git clone", "curl"…
    pub label: String,
    /// The whole command line.
    pub cmd: String,
    pub progress: Option<u8>,
    /// "3.0 MB/s", as the tool printed it.
    pub speed: Option<String>,
    /// Seconds left.
    pub eta: Option<u64>,
    /// "256 / 412 MB", as far as the tool says.
    pub amount: Option<String>,
    /// The tool's latest progress line ("Receiving objects 62% (148221/239067)").
    pub phase: Option<String>,
    /// Where it downloads from: "github.com", "registry.npmjs.org"…
    pub src: String,
    /// What it makes: the cloned folder, node_modules, the downloaded file.
    pub target: Option<PathBuf>,
    pub started: Instant,
    /// Where the progress was first seen (for working out the time left).
    base: Option<(Instant, u8)>,
    /// Paused from the activity list (SIGSTOP).
    pub paused: bool,
}

impl Activity {
    fn new(label: String) -> Activity {
        Activity {
            label,
            paused: false,
            cmd: String::new(),
            progress: None,
            speed: None,
            eta: None,
            amount: None,
            phase: None,
            src: String::new(),
            target: None,
            started: Instant::now(),
            base: None,
        }
    }

    /// Take a new reading of the screen. True if anything shown changed.
    pub fn update(&mut self, r: Reading) -> bool {
        let before = (
            self.progress,
            self.speed.clone(),
            self.eta,
            self.amount.clone(),
            self.phase.clone(),
        );
        if r.amount.is_some() {
            self.amount = r.amount;
        }
        if r.phase.is_some() {
            self.phase = r.phase;
        }
        if let Some(p) = r.progress {
            // a new phase (git: receiving, then resolving) starts over
            match self.base {
                Some((_, b)) if p >= b && self.progress.is_none_or(|old| p + 5 >= old) => {}
                _ => self.base = Some((Instant::now(), p)),
            }
            self.progress = Some(p);
        }
        if r.speed.is_some() {
            self.speed = r.speed;
        }
        self.eta = r.eta.or_else(|| {
            let (t0, p0) = self.base?;
            let p = self.progress?;
            let secs = t0.elapsed().as_secs_f64();
            if p <= p0 || p >= 100 || secs < 2.0 {
                return None;
            }
            Some((secs * (100 - p) as f64 / (p - p0) as f64).round() as u64)
        });
        before
            != (
                self.progress,
                self.speed.clone(),
                self.eta,
                self.amount.clone(),
                self.phase.clone(),
            )
    }
}

/// What the bottom of a pane says about a download.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub progress: Option<u8>,
    pub speed: Option<String>,
    pub eta: Option<u64>,
    pub amount: Option<String>,
    pub phase: Option<String>,
}

/// (program, sub-commands that count — empty means any)
const TABLE: &[(&str, &[&str])] = &[
    ("git", &["clone", "fetch", "pull", "lfs"]),
    ("npm", &["install", "i", "ci", "update", "add"]),
    ("pnpm", &["install", "i", "add", "update"]),
    ("yarn", &["install", "add", ""]),
    ("bun", &["install", "i", "add"]),
    ("pip", &["install", "download"]),
    ("pip3", &["install", "download"]),
    ("uv", &["sync", "pip", "add", "install", "tool"]),
    ("poetry", &["install", "add", "update"]),
    ("conda", &["install", "create", "update"]),
    (
        "brew",
        &["install", "upgrade", "update", "fetch", "reinstall"],
    ),
    ("apt", &["install", "upgrade", "update"]),
    ("apt-get", &["install", "upgrade", "update"]),
    ("docker", &["pull", "build", "push"]),
    ("podman", &["pull", "build", "push"]),
    ("cargo", &["install", "build", "fetch", "update"]),
    ("go", &["get", "install", "mod"]),
    ("gem", &["install", "update"]),
    ("bundle", &["install", "update"]),
    ("curl", &[]),
    ("wget", &[]),
    ("rsync", &[]),
    ("scp", &[]),
    ("huggingface-cli", &["download"]),
    ("ollama", &["pull"]),
];

/// Words that may come before the real program.
const PREFIXES: &[&str] = &["sudo", "time", "nice", "env", "command", "exec", "nohup"];

/// Is this command a download/install we should show as activity?
pub fn classify(cmd: &str) -> Option<Activity> {
    // Look at each part of a pipeline / && chain; the first match wins.
    for part in cmd.split(['|', ';', '&']) {
        let mut words = part
            .split_whitespace()
            .skip_while(|w| PREFIXES.contains(w) || w.contains('=') && !w.starts_with('-'));
        let Some(prog) = words.next() else { continue };
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        let Some((_, subs)) = TABLE.iter().find(|(p, _)| *p == prog) else {
            continue;
        };
        if subs.is_empty() {
            return Some(Activity::new(prog.to_string()));
        }
        let sub = words.find(|w| !w.starts_with('-')).unwrap_or("");
        if subs.contains(&sub) {
            let label = if sub.is_empty() {
                prog.to_string()
            } else {
                format!("{prog} {sub}")
            };
            return Some(Activity::new(label));
        }
    }
    None
}

/// A download/install started from `cwd`: what it is and what it makes.
pub fn start(cmd: &str, cwd: &Path) -> Option<Activity> {
    let mut a = classify(cmd)?;
    a.cmd = cmd.to_string();
    a.target = target(cmd, cwd);
    a.src = source(cmd, &a.label);
    Some(a)
}

/// Where a download comes from: the URL's host, else the tool's registry.
pub fn source(cmd: &str, label: &str) -> String {
    for w in cmd.split_whitespace() {
        let w = w.trim_matches(['\'', '"']);
        if let Some((_, rest)) = w.split_once("://") {
            let host = rest.split(['/', ':', '?']).next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or(host);
            if !host.is_empty() {
                return host.to_string();
            }
        }
        // git@github.com:me/repo.git
        if let Some((user, rest)) = w.split_once('@') {
            if !user.contains('/') && rest.contains(':') {
                return rest.split(':').next().unwrap_or("").to_string();
            }
        }
    }
    let prog = label.split_whitespace().next().unwrap_or("");
    match prog {
        "npm" | "pnpm" | "yarn" | "bun" => "registry.npmjs.org",
        "pip" | "pip3" | "uv" | "poetry" => "pypi.org",
        "conda" => "conda.anaconda.org",
        "brew" => "ghcr.io",
        "docker" | "podman" => "docker.io",
        "cargo" => "crates.io",
        "go" => "proxy.golang.org",
        "gem" | "bundle" => "rubygems.org",
        "ollama" => "ollama.com",
        "huggingface-cli" => "huggingface.co",
        "apt" | "apt-get" => "apt mirrors",
        _ => "",
    }
    .to_string()
}

/// "3.00 MiB/s" → bytes per second (for adding speeds up).
pub fn rate_bytes(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches("/s");
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic())?);
    let v: f64 = num.trim().parse().ok()?;
    let mul = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1e3,
        "KiB" => 1024.0,
        "MB" => 1e6,
        "MiB" => 1048576.0,
        "GB" => 1e9,
        "GiB" => 1073741824.0,
        _ => return None,
    };
    Some(v * mul)
}

const BYTE_UNITS: [&str; 12] = [
    "B", "kB", "KB", "KiB", "MB", "MiB", "GB", "GiB", "k", "K", "M", "G",
];

/// An amount like "12.3/30.0 MB" → "12.3 / 30.0 MB"; "1.20 MiB |" → "1.20 MiB".
fn amount(row: &str) -> Option<String> {
    let toks: Vec<&str> = row
        .split(|c: char| c.is_whitespace() || c == ',' || c == '|' || c == '(' || c == ')')
        .filter(|t| !t.is_empty())
        .collect();
    let num_unit = |t: &str| -> Option<(String, String)> {
        let i = t.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let (n, u) = t.split_at(i);
        (!n.is_empty() && BYTE_UNITS.contains(&u)).then(|| (n.to_string(), u.to_string()))
    };
    for (i, t) in toks.iter().enumerate() {
        // "12.3/30.0" "MB" or "1.2G/3G"
        if let Some((a, b)) = t.split_once('/') {
            if b == "s" || b.starts_with("s") {
                continue;
            }
            let (an, au) = match num_unit(a) {
                Some(x) => x,
                None if a.parse::<f64>().is_ok() => (a.to_string(), String::new()),
                None => continue,
            };
            let (bn, bu) = match num_unit(b) {
                Some(x) => x,
                None if b.parse::<f64>().is_ok() => match toks.get(i + 1) {
                    Some(u) if BYTE_UNITS.contains(u) => (b.to_string(), u.to_string()),
                    _ => continue,
                },
                None => continue,
            };
            return Some(if au.is_empty() || au == bu {
                format!("{an} / {bn} {bu}")
            } else {
                format!("{an} {au} / {bn} {bu}")
            });
        }
    }
    // a single amount not followed by /s
    for (i, t) in toks.iter().enumerate() {
        if t.parse::<f64>().is_ok() {
            if let Some(u) = toks.get(i + 1) {
                if BYTE_UNITS.contains(u) && u.len() > 1 {
                    return Some(format!("{t} {u}"));
                }
            }
        }
    }
    None
}

/// A screen row as a tidy one-line phase description.
fn phase(row: &str) -> String {
    let t: String = row
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| "│|━─#=>[]".contains(c) || c.is_whitespace())
        .to_string();
    t.chars().take(120).collect()
}

/// Flags that take a value, per program (so the value isn't read as an argument).
fn takes_value(prog: &str, flag: &str) -> bool {
    if flag.contains('=') {
        return false;
    }
    match prog {
        "git" => matches!(
            flag,
            "-b" | "--branch"
                | "--depth"
                | "-o"
                | "--origin"
                | "-c"
                | "--config"
                | "--reference"
                | "--filter"
                | "-j"
                | "--jobs"
                | "--template"
                | "-u"
                | "--upload-pack"
                | "--shallow-since"
                | "--shallow-exclude"
                | "--separate-git-dir"
        ),
        "curl" => matches!(
            flag,
            "-o" | "--output"
                | "-H"
                | "--header"
                | "-d"
                | "--data"
                | "-X"
                | "--request"
                | "-u"
                | "--user"
                | "-A"
                | "--user-agent"
                | "-e"
                | "--referer"
                | "-m"
                | "--max-time"
                | "--retry"
                | "-x"
                | "--proxy"
                | "-b"
                | "--cookie"
                | "-c"
                | "--cookie-jar"
                | "-w"
                | "--write-out"
                | "-T"
                | "--upload-file"
                | "-r"
                | "--range"
                | "-C"
        ),
        "wget" => matches!(
            flag,
            "-O" | "--output-document"
                | "-P"
                | "--directory-prefix"
                | "-t"
                | "--tries"
                | "-T"
                | "--timeout"
                | "-U"
                | "--user-agent"
                | "--header"
        ),
        "huggingface-cli" => matches!(flag, "--local-dir" | "--revision" | "--repo-type"),
        _ => false,
    }
}

/// The last part of a URL's path, without a query: ".../b.tgz?x=1" → "b.tgz".
fn url_name(url: &str) -> Option<String> {
    let u = url.split(['?', '#']).next().unwrap_or(url);
    let u = u.trim_end_matches('/');
    let name = u.rsplit(['/', ':']).next()?;
    (!name.is_empty() && !name.contains('@')).then(|| name.to_string())
}

/// What a download/install makes, if Shellvisor can tell.
pub fn target(cmd: &str, cwd: &Path) -> Option<PathBuf> {
    let mut cwd = cwd.to_path_buf();
    for part in cmd.split(['|', ';', '&']) {
        let words: Vec<&str> = part
            .split_whitespace()
            .skip_while(|w| PREFIXES.contains(w) || w.contains('=') && !w.starts_with('-'))
            .map(|w| w.trim_matches(['\'', '"']))
            .collect();
        let Some(prog) = words.first() else { continue };
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        if prog == "cd" {
            if let Some(d) = words.get(1) {
                cwd = expand(d, &cwd);
            }
            continue;
        }
        if classify(part).is_none() {
            continue;
        }
        // arguments that aren't flags (or a flag's value), with the flags kept apart
        let mut args = Vec::new();
        let mut flags: Vec<(&str, Option<&str>)> = Vec::new();
        let mut i = 1;
        while i < words.len() {
            let w = words[i];
            if w.starts_with('-') && w.len() > 1 {
                if takes_value(prog, w) {
                    flags.push((w, words.get(i + 1).copied()));
                    i += 1;
                } else {
                    flags.push((w, None));
                }
            } else {
                args.push(w);
            }
            i += 1;
        }
        let flag = |names: &[&str]| {
            flags
                .iter()
                .find(|(f, _)| names.contains(f))
                .map(|(_, v)| *v)
        };
        let sub = args.first().copied().unwrap_or("");
        return match prog {
            "git" if sub == "clone" => {
                let url = args.get(1)?;
                let dir = match args.get(2) {
                    Some(d) => d.to_string(),
                    None => url_name(url)?.trim_end_matches(".git").to_string(),
                };
                Some(expand(&dir, &cwd))
            }
            "git" => Some(cwd),
            "npm" | "pnpm" | "yarn" | "bun" => Some(cwd.join("node_modules")),
            "cargo" if sub == "build" || sub == "fetch" => Some(cwd.join("target")),
            "uv" if sub == "sync" => Some(cwd.join(".venv")),
            "curl" => {
                if let Some(Some(f)) = flag(&["-o", "--output"]) {
                    return Some(expand(f, &cwd));
                }
                let named = flags.iter().any(|(f, _)| {
                    *f == "-O"
                        || *f == "--remote-name"
                        || (f.starts_with('-') && !f.starts_with("--") && f.contains('O'))
                });
                if named {
                    let url = args.iter().rev().find(|a| a.contains("://"))?;
                    return Some(cwd.join(url_name(url)?));
                }
                None
            }
            "wget" => {
                if let Some(Some(f)) = flag(&["-O", "--output-document"]) {
                    return (f != "-").then(|| expand(f, &cwd));
                }
                let url = args.iter().rev().find(|a| a.contains("://"))?;
                let dir = match flag(&["-P", "--directory-prefix"]) {
                    Some(Some(d)) => expand(d, &cwd),
                    _ => cwd,
                };
                Some(dir.join(url_name(url)?))
            }
            "huggingface-cli" => match flag(&["--local-dir"]) {
                Some(Some(d)) => Some(expand(d, &cwd)),
                _ => None,
            },
            _ => None,
        };
    }
    None
}

fn expand(p: &str, cwd: &Path) -> PathBuf {
    let p = match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(p)),
        None => PathBuf::from(p),
    };
    if p.is_absolute() { p } else { cwd.join(p) }
}

/// Progress, speed and time left from the bottom rows of a pane.
pub fn read_screen<'a>(rows: impl DoubleEndedIterator<Item = &'a str>) -> Reading {
    let mut r = Reading::default();
    for row in rows.rev().filter(|r| !r.trim().is_empty()).take(4) {
        if let Some(c) = curl_row(row) {
            return c;
        }
        if r.phase.is_none() {
            r.phase = Some(phase(row)).filter(|p| !p.is_empty());
        }
        if r.progress.is_none() {
            r.progress = last_percent(row);
            if r.progress.is_some() {
                // the line with the % says best what is going on
                r.phase = Some(phase(row));
            }
        }
        if r.amount.is_none() {
            r.amount = amount(row);
        }
        if r.speed.is_none() {
            r.speed = speed(row);
        }
        if r.eta.is_none() {
            r.eta = eta(row);
        }
    }
    r
}

/// curl's meter: "42  100M   42 42.1M  0  0  3245k  0  0:00:30 0:00:12 0:00:18 3300k"
fn curl_row(row: &str) -> Option<Reading> {
    let t: Vec<&str> = row.split_whitespace().collect();
    if t.len() != 12 || !t[8..11].iter().all(|x| x.contains(':')) {
        return None;
    }
    let progress = t[0].parse::<u8>().ok().filter(|p| *p <= 100);
    let eta = duration(t[10]);
    let sp = t[11];
    let speed = (sp != "0").then(|| human_rate(sp));
    Some(Reading {
        progress,
        speed,
        eta,
        amount: Some(format!("{} / {}", t[3], t[1])),
        phase: Some(format!("{}% of {}", t[0], t[1])),
    })
}

/// "3300k" (curl's bytes per second) → "3.3 MB/s"
fn human_rate(s: &str) -> String {
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
    let unit = match unit {
        "" => "B",
        "k" => "kB",
        "M" => "MB",
        "G" => "GB",
        u => u,
    };
    let v: f64 = num.parse().unwrap_or(0.0);
    if unit == "kB" && v >= 1000.0 {
        format!("{:.1} MB/s", v / 1000.0)
    } else {
        format!("{num} {unit}/s")
    }
}

/// The last "<number> <bytes unit>/s" on a row: "3.00 MiB/s", "1.2MB/s".
fn speed(row: &str) -> Option<String> {
    let mut found = None;
    let mut rest = row;
    let mut offset = 0;
    while let Some(i) = rest.find("/s") {
        let at = offset + i;
        let before = &row[..at];
        let unit_len = before
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphabetic())
            .count();
        let unit = &before[before.len() - unit_len..];
        let num_part = before[..before.len() - unit_len].trim_end();
        let num_len = num_part
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .count();
        let num = &num_part[num_part.len() - num_len..];
        let bytes = matches!(
            unit,
            "B" | "kB" | "KB" | "KiB" | "MB" | "MiB" | "GB" | "GiB" | "k" | "K" | "M" | "G"
        );
        if bytes && !num.is_empty() && num.chars().any(|c| c.is_ascii_digit()) {
            let unit = match unit {
                "k" | "K" => "kB",
                "M" => "MB",
                "G" => "GB",
                u => u,
            };
            found = Some(format!("{num} {unit}/s"));
        }
        offset = at + 2;
        rest = &row[offset..];
    }
    found
}

/// The time left on a row: "eta 0:00:06", "ETA 18s", "[00:12<00:18", "12s left".
fn eta(row: &str) -> Option<u64> {
    let lower = row.to_ascii_lowercase();
    if let Some(i) = lower.rfind("eta") {
        let after = lower[i + 3..].trim_start_matches([' ', ':']);
        let tok: String = after.chars().take_while(|c| !c.is_whitespace()).collect();
        if let Some(d) = duration(&tok) {
            return Some(d);
        }
    }
    // tqdm: [elapsed<remaining, rate]
    if let Some(i) = row.rfind('<') {
        let tok: String = row[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == ':')
            .collect();
        if row[..i].ends_with(|c: char| c.is_ascii_digit()) {
            if let Some(d) = duration(&tok) {
                return Some(d);
            }
        }
    }
    if let Some(i) = lower.find(" left") {
        let tok = lower[..i].split_whitespace().last().unwrap_or("");
        return duration(tok);
    }
    None
}

/// "0:00:06", "00:18", "18s", "1m2s", "2h", "1m" → seconds.
fn duration(s: &str) -> Option<u64> {
    let s = s.trim_end_matches([',', ']', ')']);
    if s.is_empty() || s.starts_with("--") {
        return None;
    }
    if s.contains(':') {
        let mut total = 0u64;
        for part in s.split(':') {
            total = total * 60 + part.parse::<u64>().ok()?;
        }
        return Some(total);
    }
    let mut total = 0u64;
    let mut num = String::new();
    let mut any = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: u64 = num.parse().ok()?;
            num.clear();
            total += n * match c {
                'h' => 3600,
                'm' => 60,
                's' => 1,
                _ => return None,
            };
            any = true;
        }
    }
    (any && num.is_empty()).then_some(total)
}

/// The command a coding agent is running right now, from its screen:
/// Claude Code's "Bash(npm install)", a "$ npm install" line, "Running …".
pub fn agent_command<'a>(rows: impl DoubleEndedIterator<Item = &'a str>) -> Option<String> {
    for row in rows.rev().filter(|r| !r.trim().is_empty()).take(15) {
        let t = row.trim_matches(|c: char| c.is_whitespace() || "│┃|▌⎿⏺●•*>".contains(c));
        let cmd = if let Some(i) = t.find("Bash(") {
            let inner = &t[i + 5..];
            inner.rsplit_once(')').map(|(a, _)| a).unwrap_or(inner)
        } else if let Some(c) = t.strip_prefix("$ ") {
            c
        } else if let Some(c) = t.strip_prefix("Running ") {
            c
        } else {
            continue;
        };
        if classify(cmd).is_some() {
            return Some(cmd.trim().to_string());
        }
    }
    None
}

fn last_percent(s: &str) -> Option<u8> {
    let b = s.as_bytes();
    let mut found = None;
    for (i, &c) in b.iter().enumerate() {
        if c != b'%' {
            continue;
        }
        // digits (maybe with a decimal part) right before the %
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.') {
            j -= 1;
        }
        if j == i || (j > 0 && b[j - 1].is_ascii_alphabetic()) {
            continue;
        }
        let num = &s[j..i];
        let whole = num.split('.').next().unwrap_or("");
        if let Ok(n) = whole.parse::<u32>() {
            if n <= 100 {
                found = Some(n as u8);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(c: &str) -> Option<String> {
        classify(c).map(|a| a.label)
    }

    #[test]
    fn spots_downloads() {
        assert_eq!(
            label("git clone https://x/y.git").as_deref(),
            Some("git clone")
        );
        assert_eq!(label("npm i -D vite").as_deref(), Some("npm i"));
        assert_eq!(
            label("sudo apt-get install -y jq").as_deref(),
            Some("apt-get install")
        );
        assert_eq!(
            label("pip install --upgrade pip").as_deref(),
            Some("pip install")
        );
        assert_eq!(label("uv sync").as_deref(), Some("uv sync"));
        assert_eq!(label("curl -LO https://a/b.tgz").as_deref(), Some("curl"));
        assert_eq!(label("/usr/bin/wget x").as_deref(), Some("wget"));
        assert_eq!(label("cd app && npm ci").as_deref(), Some("npm ci"));
        assert_eq!(
            label("RUST_LOG=1 cargo build --release").as_deref(),
            Some("cargo build")
        );
        assert_eq!(label("yarn").as_deref(), Some("yarn"));
        assert_eq!(label("docker pull nginx").as_deref(), Some("docker pull"));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(label("git status"), None);
        assert_eq!(label("npm run dev"), None);
        assert_eq!(label("ls -la"), None);
        assert_eq!(label("cargo test"), None);
        assert_eq!(label(""), None);
    }

    #[test]
    fn reads_progress() {
        let rows = [
            "Cloning into 'y'...",
            "Receiving objects:  45% (450/1000), 1.2 MiB | 3 MiB/s",
            "",
        ];
        assert_eq!(read_screen(rows.iter().copied()).progress, Some(45));
        let rows = ["  42.7% ━━━━━━━ 12/30 MB", "Resolving deltas: 100% (3/3)"];
        assert_eq!(read_screen(rows.iter().copied()).progress, Some(100));
        assert_eq!(
            read_screen(["no numbers here", "x86%y"].iter().copied()).progress,
            None
        );
        assert_eq!(read_screen(["150% done"].iter().copied()).progress, None);
        assert_eq!(
            read_screen(["a 10% b 20%"].iter().copied()).progress,
            Some(20)
        );
    }

    #[test]
    fn targets() {
        let cwd = Path::new("/w");
        let t = |c: &str| target(c, cwd);
        assert_eq!(
            t("git clone https://github.com/avmg0/shellvisor.git"),
            Some(PathBuf::from("/w/shellvisor"))
        );
        assert_eq!(
            t("git clone --depth 1 -b main git@github.com:a/b.git dest"),
            Some(PathBuf::from("/w/dest"))
        );
        assert_eq!(
            t("cd app && npm ci"),
            Some(PathBuf::from("/w/app/node_modules"))
        );
        assert_eq!(
            t("curl -LO https://x.org/f/b.tgz?sig=1"),
            Some(PathBuf::from("/w/b.tgz"))
        );
        assert_eq!(
            t("curl -o out.zip https://x/y"),
            Some(PathBuf::from("/w/out.zip"))
        );
        assert_eq!(t("curl https://x/y"), None, "to the terminal");
        assert_eq!(
            t("wget -P dl https://x/a.iso"),
            Some(PathBuf::from("/w/dl/a.iso"))
        );
        assert_eq!(t("cargo build --release"), Some(PathBuf::from("/w/target")));
        assert_eq!(t("brew install jq"), None);
        let a = start("git clone https://x/y.git", cwd).unwrap();
        assert_eq!(a.label, "git clone");
        assert_eq!(a.target, Some(PathBuf::from("/w/y")));
    }

    #[test]
    fn reads_speed_and_time_left() {
        let r = read_screen(
            ["Receiving objects:  45% (450/1000), 1.20 MiB | 3.00 MiB/s"]
                .iter()
                .copied(),
        );
        assert_eq!(r.progress, Some(45));
        assert_eq!(r.speed.as_deref(), Some("3.00 MiB/s"));
        let r = read_screen(
            ["   ━━━━━━━━━━━━━━━━ 12.3/30.0 MB 3.1 MB/s eta 0:00:06"]
                .iter()
                .copied(),
        );
        assert_eq!((r.speed.as_deref(), r.eta), (Some("3.1 MB/s"), Some(6)));
        let r = read_screen(
            ["a.iso   42%[=====>      ]  12.3M  3.21MB/s    eta 18s"]
                .iter()
                .copied(),
        );
        assert_eq!((r.progress, r.eta), (Some(42), Some(18)));
        assert_eq!(r.speed.as_deref(), Some("3.21 MB/s"));
        let r = read_screen(
            [" 42%|████      | 1.2G/3G [00:12<00:18, 98.3MB/s]"]
                .iter()
                .copied(),
        );
        assert_eq!((r.progress, r.eta), (Some(42), Some(18)));
        assert_eq!(r.speed.as_deref(), Some("98.3 MB/s"));
        let r = read_screen(
            [
                "  % Total    % Received % Xferd  Average Speed   Time    Time     Time  Current",
                " 42  100M   42 42.1M    0     0  3245k      0  0:00:30  0:00:12  0:00:18 3300k",
            ]
            .iter()
            .copied(),
        );
        assert_eq!((r.progress, r.eta), (Some(42), Some(18)));
        assert_eq!(r.speed.as_deref(), Some("3.3 MB/s"));
        let r = read_screen(["12.3it/s 40%"].iter().copied());
        assert_eq!((r.progress, r.speed), (Some(40), None), "not bytes");
    }

    #[test]
    fn amounts_sources_phases() {
        let r = read_screen(
            ["   ━━━━━━━━━━━━━━━━ 12.3/30.0 MB 3.1 MB/s eta 0:00:06"]
                .iter()
                .copied(),
        );
        assert_eq!(r.amount.as_deref(), Some("12.3 / 30.0 MB"));
        let r = read_screen(
            ["Receiving objects:  45% (450/1000), 1.20 MiB | 3.00 MiB/s"]
                .iter()
                .copied(),
        );
        assert_eq!(r.amount.as_deref(), Some("1.20 MiB"));
        assert_eq!(
            r.phase.as_deref(),
            Some("Receiving objects: 45% (450/1000), 1.20 MiB | 3.00 MiB/s")
        );
        let r = read_screen(
            [" 42%|████ | 1.2G/3G [00:12<00:18, 98.3MB/s]"]
                .iter()
                .copied(),
        );
        assert_eq!(r.amount.as_deref(), Some("1.2 / 3 G"));
        assert_eq!(
            source("git clone https://github.com/a/b", "git clone"),
            "github.com"
        );
        assert_eq!(
            source("git clone git@gitlab.com:a/b.git", "git clone"),
            "gitlab.com"
        );
        assert_eq!(source("npm install", "npm install"), "registry.npmjs.org");
        assert_eq!(rate_bytes("3.00 MiB/s"), Some(3.0 * 1048576.0));
        assert_eq!(rate_bytes("510 kB/s"), Some(510e3));
    }

    #[test]
    fn works_out_time_left() {
        let mut a = Activity::new("brew install".into());
        a.update(Reading {
            progress: Some(10),
            ..Default::default()
        });
        a.base = Some((Instant::now() - std::time::Duration::from_secs(10), 10));
        a.update(Reading {
            progress: Some(30),
            ..Default::default()
        });
        assert_eq!(a.eta, Some(35), "20 % in 10 s: 70 % more in 35 s");
        a.update(Reading {
            progress: Some(3),
            ..Default::default()
        });
        assert_eq!(a.eta, None, "a new phase starts over");
    }

    #[test]
    fn spots_an_agents_command() {
        let rows = [
            "⏺ I'll install the dependencies.",
            "⏺ Bash(npm install express)",
            "  ⎿  Running…",
        ];
        assert_eq!(
            agent_command(rows.iter().copied()).as_deref(),
            Some("npm install express")
        );
        assert_eq!(
            agent_command(["$ git clone https://x/y.git"].iter().copied()).as_deref(),
            Some("git clone https://x/y.git")
        );
        assert_eq!(agent_command(["⏺ Bash(ls -la)"].iter().copied()), None);
    }
}

/// A download or install from an earlier session (`<state>/downloads.jsonl`).
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    /// Unix seconds it finished.
    pub at: u64,
    pub cmd: String,
    pub src: String,
    pub ok: bool,
    pub code: Option<i32>,
    pub amount: Option<String>,
    pub took: u64,
    pub cwd: String,
}

/// Remember a finished download (keeps the newest 200).
pub fn remember(state: &std::path::Path, p: &Past) {
    let line = serde_json::json!({
        "at": p.at, "cmd": p.cmd, "src": p.src, "ok": p.ok, "code": p.code,
        "amount": p.amount, "took": p.took, "cwd": p.cwd,
    })
    .to_string();
    let path = state.join("downloads.jsonl");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default();
    lines.push(line);
    let skip = lines.len().saturating_sub(200);
    let _ = std::fs::create_dir_all(state);
    let _ = std::fs::write(&path, lines[skip..].join("\n") + "\n");
}

/// Earlier downloads, newest first.
pub fn history(state: &std::path::Path) -> Vec<Past> {
    let Ok(t) = std::fs::read_to_string(state.join("downloads.jsonl")) else {
        return Vec::new();
    };
    t.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .map(|v| Past {
            at: v["at"].as_u64().unwrap_or(0),
            cmd: v["cmd"].as_str().unwrap_or("").to_string(),
            src: v["src"].as_str().unwrap_or("").to_string(),
            ok: v["ok"].as_bool().unwrap_or(true),
            code: v["code"].as_i64().map(|c| c as i32),
            amount: v["amount"].as_str().map(String::from),
            took: v["took"].as_u64().unwrap_or(0),
            cwd: v["cwd"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn downloads_are_remembered() {
        let d = std::env::temp_dir().join(format!("sv-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (i, c) in ["git clone a", "npm install"].iter().enumerate() {
            remember(
                &d,
                &Past {
                    at: i as u64,
                    cmd: c.to_string(),
                    src: "x".into(),
                    ok: i == 0,
                    code: Some(i as i32),
                    amount: None,
                    took: 3,
                    cwd: "/w".into(),
                },
            );
        }
        let h = history(&d);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].cmd, "npm install");
        assert!(!h[0].ok && h[1].ok);
        let _ = std::fs::remove_dir_all(&d);
    }
}

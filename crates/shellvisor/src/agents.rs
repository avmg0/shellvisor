//! Coding agents: CLIs like Claude Code, Codex, Gemini CLI or Aider, each run
//! in its own shell (in its own git worktree), shown side by side.
//!
//! Shellvisor doesn't talk to the agents through private APIs. It runs their
//! normal interactive CLI and reads the screen: whether the agent is working,
//! idle, or asking for approval (each CLI's prompt is recognised and answered
//! with that CLI's own keys). Which files an agent changed comes from git.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cli {
    Claude,
    Codex,
    Gemini,
    Aider,
    /// Any other command.
    Generic,
}

/// The keys that answer an approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub allow: &'static str,
    pub always: &'static str,
    pub deny: &'static str,
}

impl Cli {
    pub const KNOWN: [Cli; 4] = [Cli::Claude, Cli::Codex, Cli::Gemini, Cli::Aider];

    pub fn name(self) -> &'static str {
        match self {
            Cli::Claude => "claude",
            Cli::Codex => "codex",
            Cli::Gemini => "gemini",
            Cli::Aider => "aider",
            Cli::Generic => "command",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Cli::Claude => "Claude Code",
            Cli::Codex => "Codex",
            Cli::Gemini => "Gemini CLI",
            Cli::Aider => "Aider",
            Cli::Generic => "Custom",
        }
    }

    pub fn parse(s: &str) -> Option<Cli> {
        Some(match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Cli::Claude,
            "codex" => Cli::Codex,
            "gemini" | "gemini-cli" => Cli::Gemini,
            "aider" => Cli::Aider,
            "command" | "custom" | "generic" => Cli::Generic,
            _ => return None,
        })
    }

    /// Which CLI is this command line?
    pub fn of_command(cmd: &str) -> Cli {
        let first = cmd.split_whitespace().next().unwrap_or("");
        let prog = first.rsplit('/').next().unwrap_or(first);
        Cli::parse(prog)
            .filter(|c| *c != Cli::Generic)
            .unwrap_or(Cli::Generic)
    }

    /// The command line to start it with a first instruction (shell-quoted).
    pub fn launch(self, prompt: &str, custom: &str) -> String {
        let q = crate::app::shell_quote;
        let p = prompt.trim();
        match (self, p.is_empty()) {
            (Cli::Generic, _) => custom.trim().to_string(),
            (c, true) => c.name().to_string(),
            (Cli::Claude, false) => format!("claude {}", q(p)),
            (Cli::Codex, false) => format!("codex {}", q(p)),
            (Cli::Gemini, false) => format!("gemini -i {}", q(p)),
            (Cli::Aider, false) => format!("aider --message {}", q(p)),
        }
    }

    pub fn keys(self) -> Keys {
        match self {
            // "1. Yes  2. Yes, and don't ask again  3. No (esc)"
            Cli::Claude | Cli::Gemini => Keys {
                allow: "1",
                always: "2",
                deny: "\x1b",
            },
            // "(y) yes · (a) always · (n) no"
            Cli::Codex | Cli::Aider => Keys {
                allow: "y",
                always: "a",
                deny: "n",
            },
            Cli::Generic => Keys {
                allow: "y\r",
                always: "y\r",
                deny: "n\r",
            },
        }
    }
}

/// CLIs installed on this machine (found on PATH).
pub fn detect() -> Vec<(Cli, PathBuf)> {
    Cli::KNOWN
        .iter()
        .filter_map(|c| crate::shell::find_in_path(c.name()).map(|p| (*c, p)))
        .collect()
}

// ------------------------------------------------------------------ approvals

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Read,
    Edit,
    Test,
    Lint,
    Install,
    Network,
    Push,
    Delete,
    Command,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Edit => "edit",
            Action::Test => "test",
            Action::Lint => "lint",
            Action::Install => "install",
            Action::Network => "network",
            Action::Push => "push",
            Action::Delete => "delete",
            Action::Command => "command",
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        [
            Action::Read,
            Action::Edit,
            Action::Test,
            Action::Lint,
            Action::Install,
            Action::Network,
            Action::Push,
            Action::Delete,
            Action::Command,
        ]
        .into_iter()
        .find(|a| a.name() == s)
    }
}

/// What kind of thing is being asked for? (From the prompt's text.)
pub fn classify(text: &str) -> Action {
    let t = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["git push", "push --", "npm publish", "cargo publish"]) {
        Action::Push
    } else if has(&["rm -", "rm ", "rmdir", "delete", "unlink", "git clean"]) {
        Action::Delete
    } else if has(&[
        "npm install",
        "npm i ",
        "pnpm add",
        "yarn add",
        "pip install",
        "uv add",
        "uv pip",
        "brew install",
        "cargo add",
        "cargo install",
        "apt install",
        "apt-get install",
        "gem install",
        "go get",
        "install ",
        "dependency",
        "dependencies",
    ]) {
        Action::Install
    } else if has(&["curl ", "wget ", "fetch", "http://", "https://", "ssh "]) {
        Action::Network
    } else if has(&[
        "pytest",
        "cargo test",
        "npm test",
        "npm run test",
        "go test",
        "jest",
        "vitest",
        "rspec",
        "make test",
    ]) {
        Action::Test
    } else if has(&[
        "eslint", "ruff", "clippy", "lint", "prettier", "black ", "fmt", "mypy", "tsc",
    ]) {
        Action::Lint
    } else if has(&[
        "edit",
        "write",
        "create",
        "make this",
        "apply",
        "patch",
        "update file",
        "overwrite",
    ]) {
        Action::Edit
    } else if has(&["read", "cat ", "ls ", "grep", "find ", "view", "search"]) {
        Action::Read
    } else {
        Action::Command
    }
}

/// An agent waiting for a yes/no.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    /// The question, short ("Bash: npm install pytest-asyncio").
    pub what: String,
    pub action: Action,
    pub since: Instant,
}

/// Look at an agent's bottom screen rows for an approval prompt.
pub fn detect_approval(cli: Cli, rows: &[String]) -> Option<Approval> {
    let lines: Vec<&str> = rows
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(16)..];
    let joined = tail.join("\n").to_lowercase();
    let asking = match cli {
        Cli::Claude => {
            joined.contains("do you want to")
                && (joined.contains("1. yes") || joined.contains("❯ 1"))
        }
        Cli::Gemini => {
            joined.contains("allow") && (joined.contains("1. yes") || joined.contains("allow once"))
        }
        Cli::Codex => {
            (joined.contains("allow")
                || joined.contains("approve")
                || joined.contains("would you like to run"))
                && (joined.contains("(y)")
                    || joined.contains("yes (y)")
                    || joined.contains("[y/n]"))
        }
        Cli::Aider => joined.contains("(y)es/(n)o"),
        Cli::Generic => {
            let last = tail.last().map(|s| s.to_lowercase()).unwrap_or_default();
            last.ends_with("(y/n)")
                || last.ends_with("[y/n]")
                || last.ends_with("[y/n]:")
                || last.ends_with("(y/n)?")
                || last.ends_with("(y/n):")
                || last.ends_with("? [y/n]")
                || last.contains("(y/n)") && last.ends_with(['?', ':', ' '])
        }
    };
    if !asking {
        return None;
    }
    // The most telling line: the command or file being asked about, not the
    // question or the answer options.
    let strip = |l: &str| l.trim_matches(|c: char| "│╭╮╰╯─● ".contains(c)).to_string();
    let is_noise = |l: &str| {
        let l = l.to_lowercase();
        l.is_empty()
            || l.starts_with(|c: char| c.is_ascii_digit()) && l.get(1..2) == Some(".")
            || l.starts_with('❯')
            || l.starts_with("do you want")
            || l.starts_with("would you like")
            || l.contains("(y)")
            || l.contains("(n)")
            || l.contains("(esc)")
            || !l.chars().any(char::is_alphanumeric)
    };
    let cleaned: Vec<String> = tail.iter().map(|l| strip(l)).collect();
    let what: String = cleaned
        .iter()
        .rev()
        .find(|l| !is_noise(l))
        .map(|l| {
            let l = l.trim_start_matches("$ ");
            // drop a trailing "(y/n)" / "[y/N]"
            let cut = l.rfind(['(', '[']).filter(|i| {
                let t = l[*i..].to_lowercase();
                t.contains("y/n") || t.contains("y)") || t.contains("yes")
            });
            cut.map(|i| l[..i].trim_end()).unwrap_or(l).to_string()
        })
        .unwrap_or_default()
        .chars()
        .take(80)
        .collect();
    Some(Approval {
        action: classify(&tail.join(" ")),
        what,
        since: Instant::now(),
    })
}

// ------------------------------------------------------------------ status

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Starting,
    Working,
    Idle,
    NeedsApproval(Approval),
    /// The CLI exited (back at the shell prompt), with its exit code.
    Exited(Option<i32>),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::Starting => "starting".into(),
            Status::Working => "◌ working".into(),
            Status::Idle => "● idle".into(),
            Status::NeedsApproval(a) => format!("⚑ approve {}", a.action.name()),
            Status::Exited(Some(0)) | Status::Exited(None) => "✔ done".into(),
            Status::Exited(Some(c)) => format!("✖ exit {c}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

/// Everything Shellvisor tracks about one agent (its terminal is a normal pane).
#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub cli: Cli,
    /// 256-colour index for its column.
    pub color: u8,
    pub repo: Option<PathBuf>,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    /// Commit the agent started from (for the diff).
    pub base: Option<String>,
    pub started: Instant,
    pub last_output: Instant,
    pub status: Status,
    pub files: Vec<FileChange>,
    /// Action kinds answered "always" for this agent.
    pub always: Vec<Action>,
    /// Shellvisor launched the CLI (vs. attached to one already running).
    pub launched: bool,
    /// Tokens used so far, from the CLI's own session log (Claude Code, Codex).
    pub tokens: Option<u64>,
    pub started_at: SystemTime,
    /// The agent that spawned this one (None: started by you).
    pub parent: Option<String>,
    /// 1 for agents you start, parent's + 1 for sub-agents.
    pub depth: u8,
    /// Stopped with SIGSTOP (sv agent pause, or the budget cap).
    pub paused: bool,
    /// The stopped job's process group: once stopped, the shell takes the
    /// terminal back, so it is no longer the foreground job.
    pub paused_pg: Option<i32>,
    /// It has worked since its last task (so "idle" now means "finished").
    pub worked: bool,
    /// The task it was given last.
    pub task: String,
    /// The CLI's own helper agents seen in its log (e.g. Claude Code's Task tool).
    pub internal: Vec<String>,
    /// The team (team.yaml) it belongs to; sub-agents inherit it.
    pub team: Option<String>,
    /// Status last reported as events (started, done, failed…).
    pub reported: &'static str,
    /// Every status change, for the timeline.
    pub history: Vec<(Instant, &'static str)>,
    /// Approvals answered by themselves for a while (review board): until
    /// when (None: until its task is done), and which kinds.
    pub approve_for: Option<(Option<Instant>, Vec<Action>)>,
}

/// How big an agent tree may grow (config `agents.limits`, or team.yaml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_depth: u8,
    pub max_agents: usize,
    pub max_children: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 3,
            max_agents: 10,
            max_children: 3,
        }
    }
}

/// May `parent` spawn one more agent? `Err` carries the reason, for the parent.
pub fn check_spawn(
    limits: Limits,
    active: &[(String, Option<String>, u8)],
    parent: Option<&str>,
) -> Result<u8, String> {
    if active.len() >= limits.max_agents {
        return Err(format!(
            "refused: {} agents are already running (max_agents {})",
            active.len(),
            limits.max_agents
        ));
    }
    let Some(p) = parent else { return Ok(1) };
    let Some((_, _, depth)) = active.iter().find(|(n, _, _)| n == p) else {
        return Err(format!("refused: no running agent called {p}"));
    };
    let depth = depth + 1;
    if depth > limits.max_depth {
        return Err(format!(
            "refused: a sub-agent of {p} would be at depth {depth} (max_depth {})",
            limits.max_depth
        ));
    }
    let children = active
        .iter()
        .filter(|(_, par, _)| par.as_deref() == Some(p))
        .count();
    if children >= limits.max_children {
        return Err(format!(
            "refused: {p} already has {children} sub-agents (max_children {})",
            limits.max_children
        ));
    }
    Ok(depth)
}

/// Names under `root` in the tree (children, grandchildren…), not `root` itself.
pub fn descendants(all: &[(String, Option<String>)], root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut frontier = vec![root.to_string()];
    while let Some(n) = frontier.pop() {
        for (c, p) in all {
            if p.as_deref() == Some(n.as_str()) && !out.contains(c) {
                out.push(c.clone());
                frontier.push(c.clone());
            }
        }
    }
    out
}

/// The CLI's own helper agents in a Claude Code transcript (Task tool calls).
pub fn internal_agents(transcript: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in transcript.lines() {
        if !line.contains("\"tool_use\"") {
            continue;
        }
        let Ok(j) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(items) = j.pointer("/message/content").and_then(|c| c.as_array()) else {
            continue;
        };
        for it in items {
            if it.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = it.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name != "Task" && name != "Agent" {
                continue;
            }
            let input = it.get("input");
            let kind = input
                .and_then(|i| i.get("subagent_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("helper");
            let what = input
                .and_then(|i| i.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push(if what.is_empty() {
                kind.to_string()
            } else {
                format!("{kind}: {what}")
            });
        }
    }
    out
}

pub const COLORS: [u8; 6] = [73, 176, 111, 179, 108, 167];

/// Quiet this long → idle.
pub const IDLE_AFTER: Duration = Duration::from_secs(3);

impl Agent {
    pub fn new(name: String, cli: Cli, index: usize) -> Agent {
        Agent {
            name,
            cli,
            color: COLORS[index % COLORS.len()],
            repo: None,
            worktree: None,
            branch: None,
            base: None,
            started: Instant::now(),
            last_output: Instant::now(),
            status: Status::Starting,
            files: Vec::new(),
            always: Vec::new(),
            launched: true,
            tokens: None,
            started_at: SystemTime::now(),
            parent: None,
            depth: 1,
            paused: false,
            paused_pg: None,
            approve_for: None,
            worked: false,
            task: String::new(),
            internal: Vec::new(),
            team: None,
            reported: "",
            history: Vec::new(),
        }
    }

    pub fn approval(&self) -> Option<&Approval> {
        match &self.status {
            Status::NeedsApproval(a) => Some(a),
            _ => None,
        }
    }

    /// "3 files +118 −6"
    pub fn files_summary(&self) -> String {
        if self.files.is_empty() {
            return String::new();
        }
        let add: u64 = self.files.iter().map(|f| f.added).sum();
        let del: u64 = self.files.iter().map(|f| f.removed).sum();
        let n = self.files.len();
        format!("{n} file{} +{add} −{del}", if n == 1 { "" } else { "s" })
    }
}

// ------------------------------------------------------------------ usage

/// Tokens used so far, read from the CLI's own session log when it keeps one
/// we can read: Claude Code (`~/.claude/projects/<folder>/<session>.jsonl`,
/// `usage` on each reply) and Codex (`~/.codex/sessions/…/rollout-*.jsonl`,
/// `total_tokens`). Others: None — shown as nothing rather than a guess.
/// Also returns the CLI's own helper agents seen in the same log.
pub fn usage(cli: Cli, dir: &Path, since: SystemTime) -> (Option<u64>, Vec<String>) {
    usage_inner(cli, dir, since).unwrap_or((None, Vec::new()))
}

fn usage_inner(cli: Cli, dir: &Path, since: SystemTime) -> Option<(Option<u64>, Vec<String>)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    // logs written just before we noticed the agent still count
    let since = since.checked_sub(Duration::from_secs(5)).unwrap_or(since);
    match cli {
        Cli::Claude => {
            let key: String = dir
                .to_string_lossy()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            let file = newest_jsonl(&home.join(".claude/projects").join(key), since, 0)?;
            let text = std::fs::read_to_string(file).ok()?;
            Some((Some(sum_usage(&text)), internal_agents(&text)))
        }
        Cli::Codex => {
            let file = newest_jsonl(&home.join(".codex/sessions"), since, 4)?;
            let text = std::fs::read_to_string(file).ok()?;
            if !text.contains(&*dir.to_string_lossy()) {
                return None; // another folder's session
            }
            Some((max_number(&text, "\"total_tokens\":"), Vec::new()))
        }
        _ => None,
    }
}

fn newest_jsonl(dir: &Path, since: SystemTime, depth: u32) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, lvl)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() && lvl < depth {
                stack.push((p, lvl + 1));
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                if let Ok(t) = m.modified() {
                    if t >= since && best.as_ref().is_none_or(|(bt, _)| t > *bt) {
                        best = Some((t, p));
                    }
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Claude Code transcripts: every reply carries `message.usage`.
pub fn sum_usage(text: &str) -> u64 {
    let mut total = 0;
    for line in text.lines() {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(u) = j.pointer("/message/usage") else {
            continue;
        };
        for k in [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ] {
            total += u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        }
    }
    total
}

fn max_number(text: &str, key: &str) -> Option<u64> {
    text.match_indices(key)
        .filter_map(|(i, _)| {
            let rest = text[i + key.len()..].trim_start();
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .ok()
        })
        .max()
}

/// "61K tok"
pub fn fmt_tokens(t: u64) -> String {
    match t {
        0..1000 => format!("{t} tok"),
        1000..1_000_000 => format!("{}K tok", t / 1000),
        _ => format!("{:.1}M tok", t as f64 / 1e6),
    }
}

/// Files changed by two or more agents (path → agent names).
pub fn overlaps<'a>(agents: impl Iterator<Item = &'a Agent>) -> Vec<(String, Vec<String>)> {
    let mut by: Vec<(String, Vec<String>)> = Vec::new();
    for a in agents {
        for f in &a.files {
            match by.iter_mut().find(|(p, _)| *p == f.path) {
                Some((_, names)) => {
                    if !names.contains(&a.name) {
                        names.push(a.name.clone())
                    }
                }
                None => by.push((f.path.clone(), vec![a.name.clone()])),
            }
        }
    }
    by.retain(|(_, n)| n.len() > 1);
    by.sort();
    by
}

// ------------------------------------------------------------------ git

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Is `name` usable as an agent / branch name?
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !name.starts_with('-')
}

pub struct Worktree {
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

/// `git worktree add <repo>/.shellvisor/worktrees/<name> -b agent/<name>`,
/// with `.shellvisor/` kept out of `git status` via `.git/info/exclude`.
pub fn create_worktree(cwd: &Path, name: &str) -> Result<Worktree, String> {
    if !valid_name(name) {
        return Err(format!(
            "`{name}` can't be a branch name — use letters, digits, - and _"
        ));
    }
    let repo = PathBuf::from(
        git(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|_| "not inside a git repository".to_string())?,
    );
    let base = git(&repo, &["rev-parse", "HEAD"])
        .map_err(|_| "the repository has no commits yet".to_string())?;
    let path = repo.join(".shellvisor").join("worktrees").join(name);
    let branch = format!("agent/{name}");
    if path.exists() {
        return Err(format!(
            "{} already exists — pick another name",
            path.display()
        ));
    }
    // keep our folder out of the user's status
    if let Ok(common) = git(&repo, &["rev-parse", "--git-common-dir"]) {
        let common = if Path::new(&common).is_absolute() {
            PathBuf::from(common)
        } else {
            repo.join(common)
        };
        let exclude = common.join("info").join("exclude");
        let text = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !text.lines().any(|l| l.trim() == "/.shellvisor/") {
            let _ = std::fs::create_dir_all(exclude.parent().unwrap_or(&common));
            let _ = std::fs::write(
                &exclude,
                format!(
                    "{text}{}/.shellvisor/\n",
                    if text.is_empty() || text.ends_with('\n') {
                        ""
                    } else {
                        "\n"
                    }
                ),
            );
        }
    }
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            &path.to_string_lossy(),
            "HEAD",
        ],
    )?;
    Ok(Worktree {
        repo,
        path,
        branch,
        base,
    })
}

/// Files an agent changed since `base` (committed or not), plus new files.
pub fn changes(dir: &Path, base: &str) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = git(dir, &["diff", "--numstat", base])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let a = f.next()?.parse().unwrap_or(0);
            let d = f.next()?.parse().unwrap_or(0);
            let path = f.next()?.to_string();
            Some(FileChange {
                path,
                added: a,
                removed: d,
            })
        })
        .collect();
    for p in git(dir, &["ls-files", "--others", "--exclude-standard"])
        .unwrap_or_default()
        .lines()
    {
        if !p.is_empty() && !out.iter().any(|f| f.path == p) {
            let lines = std::fs::read_to_string(dir.join(p))
                .map(|t| t.lines().count() as u64)
                .unwrap_or(0);
            out.push(FileChange {
                path: p.to_string(),
                added: lines,
                removed: 0,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The repository top and current commit for a folder (for agents without a worktree).
pub fn repo_head(dir: &Path) -> Option<(PathBuf, String)> {
    let top = git(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let head = git(dir, &["rev-parse", "HEAD"]).ok()?;
    Some((PathBuf::from(top), head))
}

#[cfg(test)]
mod tests {
    #[test]
    fn spawn_limits_and_descendants() {
        let l = super::Limits::default();
        let a = |n: &str, p: Option<&str>, d: u8| (n.to_string(), p.map(String::from), d);
        let active = vec![
            a("lead", None, 1),
            a("api", Some("lead"), 2),
            a("ws", Some("api"), 3),
        ];
        assert_eq!(super::check_spawn(l, &active, None), Ok(1));
        assert_eq!(super::check_spawn(l, &active, Some("lead")), Ok(2));
        let deep = super::check_spawn(l, &active, Some("ws")).unwrap_err();
        assert!(deep.contains("depth 4 (max_depth 3)"), "{deep}");
        assert!(
            super::check_spawn(l, &active, Some("ghost"))
                .unwrap_err()
                .contains("no running agent")
        );
        let full = vec![
            a("api", None, 1),
            a("c1", Some("api"), 2),
            a("c2", Some("api"), 2),
            a("c3", Some("api"), 2),
        ];
        assert!(
            super::check_spawn(l, &full, Some("api"))
                .unwrap_err()
                .contains("3 sub-agents (max_children 3)")
        );
        let many: Vec<_> = (0..10).map(|i| a(&format!("a{i}"), None, 1)).collect();
        assert!(
            super::check_spawn(l, &many, None)
                .unwrap_err()
                .contains("max_agents 10")
        );
        let tree: Vec<(String, Option<String>)> = active
            .iter()
            .map(|(n, p, _)| (n.clone(), p.clone()))
            .collect();
        assert_eq!(super::descendants(&tree, "lead"), vec!["api", "ws"]);
        assert!(super::descendants(&tree, "ws").is_empty());
    }

    #[test]
    fn claude_helper_agents() {
        let t = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task","input":{"subagent_type":"Explore","description":"find the price feed"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert_eq!(
            super::internal_agents(t),
            vec!["Explore: find the price feed"]
        );
    }

    #[test]
    fn usage_from_session_logs() {
        let t = r#"{"type":"user","message":{"content":"hi"}}
{"type":"assistant","message":{"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}}
{"type":"assistant","message":{"usage":{"input_tokens":3,"output_tokens":2}}}"#;
        assert_eq!(super::sum_usage(t), 120);
        assert_eq!(
            super::max_number(
                r#"{"total_tokens": 50}{"total_tokens":1234}"#,
                "\"total_tokens\":"
            ),
            Some(1234)
        );
        assert_eq!(super::fmt_tokens(61_234), "61K tok");
        // a real Claude Code log, found by its folder
        let home = std::env::temp_dir().join(format!("sv-usage-{}", std::process::id()));
        let dir = std::path::Path::new("/work/my.app");
        let logdir = home.join(".claude/projects/-work-my-app");
        std::fs::create_dir_all(&logdir).unwrap();
        std::fs::write(logdir.join("s1.jsonl"), t).unwrap();
        let old = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        let got = super::usage(super::Cli::Claude, dir, std::time::SystemTime::now()).0;
        match old {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(got, Some(120));
    }

    use super::*;

    fn rows(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn claude_permission_prompt() {
        let screen = rows(
            "╭──────────────────────────────╮\n│ Bash command                  │\n│                               │\n│   npm install pytest-asyncio  │\n│   Install the test dependency │\n│                               │\n│ Do you want to proceed?       │\n│ ❯ 1. Yes                      │\n│   2. Yes, and don't ask again │\n│   3. No, and tell Claude what to do differently (esc) │\n╰──────────────────────────────╯",
        );
        let a = detect_approval(Cli::Claude, &screen).expect("prompt");
        assert_eq!(a.action, Action::Install);
        assert!(
            a.what.contains("Install the test dependency") || a.what.contains("npm install"),
            "{}",
            a.what
        );
        assert_eq!(Cli::Claude.keys().always, "2");
        assert!(detect_approval(Cli::Claude, &rows("> working on it\n✻ Thinking…")).is_none());
    }

    #[test]
    fn other_clis() {
        let g = rows(
            "Allow execution of: 'pytest -q'?\n● 1. Yes, allow once\n  2. Yes, allow always\n  3. No (esc)",
        );
        assert_eq!(
            detect_approval(Cli::Gemini, &g).unwrap().action,
            Action::Test
        );
        let c = rows(
            "Would you like to run the following command?\n  $ git push origin agent/api\nYes (y)   Always (a)   No (n)",
        );
        let a = detect_approval(Cli::Codex, &c).unwrap();
        assert_eq!(a.action, Action::Push);
        assert!(a.what.contains("git push"), "{}", a.what);
        let ai = rows(
            "Add src/core/alerts.py to the chat? (Y)es/(N)o/(A)ll/(S)kip all/(D)on't ask again [Yes]:",
        );
        assert!(detect_approval(Cli::Aider, &ai).is_some());
        let generic = rows("Removing build/\nDelete 12 files? (y/n)");
        let a = detect_approval(Cli::Generic, &generic).unwrap();
        assert_eq!(a.action, Action::Delete);
        assert!(detect_approval(Cli::Generic, &rows("all good")).is_none());
    }

    #[test]
    fn launch_lines_and_names() {
        assert_eq!(
            Cli::Claude.launch("fix the tests", ""),
            "claude 'fix the tests'"
        );
        assert_eq!(
            Cli::Gemini.launch("it's broken", ""),
            "gemini -i 'it'\\''s broken'"
        );
        assert_eq!(Cli::Codex.launch("", ""), "codex");
        assert_eq!(
            Cli::Generic.launch("ignored", " ./my-agent --go "),
            "./my-agent --go"
        );
        assert_eq!(
            Cli::of_command("/usr/local/bin/claude --resume"),
            Cli::Claude
        );
        assert_eq!(Cli::of_command("python agent.py"), Cli::Generic);
        assert!(valid_name("api-2") && !valid_name("a b") && !valid_name("-x") && !valid_name(""));
    }

    #[test]
    fn overlap_warnings() {
        let mut a = Agent::new("api".into(), Cli::Claude, 0);
        a.files = vec![FileChange {
            path: "src/settings.py".into(),
            added: 3,
            removed: 1,
        }];
        let mut b = Agent::new("ui".into(), Cli::Codex, 1);
        b.files = vec![
            FileChange {
                path: "src/settings.py".into(),
                added: 1,
                removed: 0,
            },
            FileChange {
                path: "ui/x.tsx".into(),
                added: 9,
                removed: 2,
            },
        ];
        assert_eq!(
            overlaps([&a, &b].into_iter()),
            vec![(
                "src/settings.py".to_string(),
                vec!["api".to_string(), "ui".to_string()]
            )]
        );
        assert_eq!(b.files_summary(), "2 files +10 −2");
    }

    #[test]
    fn worktree_and_changes() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("sv-agents-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let g = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(args)
                .output()
                .unwrap();
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "one\n").unwrap();
        g(&["add", "."]);
        g(&[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]);
        let wt = create_worktree(&d, "api").expect("worktree");
        assert_eq!(wt.branch, "agent/api");
        assert!(wt.path.join("a.txt").exists());
        std::fs::write(wt.path.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(wt.path.join("new.py"), "x\ny\n").unwrap();
        let ch = changes(&wt.path, &wt.base);
        assert_eq!(
            ch,
            vec![
                FileChange {
                    path: "a.txt".into(),
                    added: 1,
                    removed: 0
                },
                FileChange {
                    path: "new.py".into(),
                    added: 2,
                    removed: 0
                },
            ]
        );
        // the main checkout doesn't see .shellvisor/
        let st = Command::new("git")
            .arg("-C")
            .arg(&d)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&st.stdout).trim().is_empty(),
            "{}",
            String::from_utf8_lossy(&st.stdout)
        );
        assert!(create_worktree(&d, "api").is_err());
        assert!(create_worktree(&d, "bad name").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! Command history with exit codes and durations, one JSON object per line.
//! Written to `<state>/history.jsonl`; read later by `sv history`.

use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    pub ts: u64,
    pub cwd: &'a str,
    pub cmd: &'a str,
    pub exit: Option<i32>,
    pub duration_ms: u64,
    pub shell: &'a str,
}

pub struct History {
    path: PathBuf,
}

impl History {
    pub fn new(state_dir: &Path) -> Self {
        History {
            path: state_dir.join("history.jsonl"),
        }
    }

    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Best effort: history must never break the shell.
    pub fn append(&self, e: &Entry) {
        let Ok(line) = serde_json::to_string(e) else {
            return;
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// The last `n` distinct commands (newest first) with their folder and exit code.
pub fn recent(state_dir: &Path, n: usize) -> Vec<(String, String, Option<i32>)> {
    let Ok(text) = std::fs::read_to_string(state_dir.join("history.jsonl")) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String, Option<i32>)> = Vec::new();
    for line in text.lines().rev().take(2000) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let cmd = v.get("cmd").and_then(|c| c.as_str()).unwrap_or("").trim();
        if cmd.is_empty() || out.iter().any(|(c, _, _)| c == cmd) {
            continue;
        }
        let cwd = v.get("cwd").and_then(|c| c.as_str()).unwrap_or("");
        let exit = v.get("exit").and_then(|e| e.as_i64()).map(|e| e as i32);
        out.push((cmd.to_string(), cwd.to_string(), exit));
        if out.len() >= n {
            break;
        }
    }
    out
}

/// How often and how recently palette entries were used (`palette-use.json`).
pub fn palette_use(state_dir: &Path) -> std::collections::HashMap<String, (u64, u64)> {
    std::fs::read_to_string(state_dir.join("palette-use.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|o| {
            o.into_iter()
                .filter_map(|(k, v)| {
                    let a = v.as_array()?;
                    Some((k, (a.first()?.as_u64()?, a.get(1)?.as_u64()?)))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Count one more use of a palette entry (keeps the 200 most recent).
pub fn palette_used(state_dir: &Path, label: &str) {
    let mut m = palette_use(state_dir);
    let e = m.entry(label.to_string()).or_insert((0, 0));
    e.0 += 1;
    e.1 = History::now();
    let mut v: Vec<(String, (u64, u64))> = m.into_iter().collect();
    v.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
    v.truncate(200);
    let o: serde_json::Map<String, serde_json::Value> = v
        .into_iter()
        .map(|(k, (c, t))| (k, serde_json::json!([c, t])))
        .collect();
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(
        state_dir.join("palette-use.json"),
        serde_json::Value::Object(o).to_string(),
    );
}

/// Ranking bonus for an entry used `count` times, last at `last` (unix secs):
/// more uses and more recent use rank higher.
pub fn frecency(count: u64, last: u64, now: u64) -> i64 {
    let uses = ((count as f64 + 1.0).ln() * 12.0) as i64;
    let age = now.saturating_sub(last);
    let recent = if age < 3600 {
        25
    } else if age < 86_400 {
        15
    } else if age < 7 * 86_400 {
        6
    } else {
        0
    };
    (uses + recent).min(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_and_frecency() {
        let d = std::env::temp_dir().join(format!("sv-hist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let h = History::new(&d);
        for c in ["ls", "make test", "ls", "git push"] {
            h.append(&Entry {
                ts: 1,
                cwd: "/w",
                cmd: c,
                exit: Some(0),
                duration_ms: 1,
                shell: "zsh",
            });
        }
        let r = recent(&d, 10);
        let cmds: Vec<&str> = r.iter().map(|(c, _, _)| c.as_str()).collect();
        assert_eq!(cmds, ["git push", "ls", "make test"]);
        palette_used(&d, "Split side by side");
        palette_used(&d, "Split side by side");
        let u = palette_use(&d);
        assert_eq!(u["Split side by side"].0, 2);
        let now = History::now();
        assert!(frecency(5, now, now) > frecency(1, now - 30 * 86_400, now));
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! Downloads and other long jobs, spotted from the command line.
//!
//! When a shell starts a command, its text is matched against a small table
//! (git clone, npm install, pip, uv, brew, docker pull, cargo, curl…). While
//! it runs, the last percentage printed on the pane's bottom rows becomes its
//! progress. No hooks into the tools themselves — it works with any version.

/// A recognised long-running command.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    /// "npm install", "git clone", "curl"…
    pub label: String,
    pub progress: Option<u8>,
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
            return Some(Activity {
                label: prog.to_string(),
                progress: None,
            });
        }
        let sub = words.find(|w| !w.starts_with('-')).unwrap_or("");
        if subs.contains(&sub) {
            let label = if sub.is_empty() {
                prog.to_string()
            } else {
                format!("{prog} {sub}")
            };
            return Some(Activity {
                label,
                progress: None,
            });
        }
    }
    None
}

/// The most recent percentage on screen: the last "NN%" on the bottom-most
/// line that has one. `rows` are the visible rows, top to bottom.
pub fn progress<'a>(rows: impl DoubleEndedIterator<Item = &'a str>) -> Option<u8> {
    for row in rows.rev().filter(|r| !r.trim().is_empty()).take(4) {
        if let Some(p) = last_percent(row) {
            return Some(p);
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
        assert_eq!(progress(rows.iter().copied()), Some(45));
        let rows = ["  42.7% ━━━━━━━ 12/30 MB", "Resolving deltas: 100% (3/3)"];
        assert_eq!(progress(rows.iter().copied()), Some(100));
        assert_eq!(progress(["no numbers here", "x86%y"].iter().copied()), None);
        assert_eq!(progress(["150% done"].iter().copied()), None);
        assert_eq!(progress(["a 10% b 20%"].iter().copied()), Some(20));
    }
}

//! `sv connect`: what agents can use on this machine — agent CLIs (installed?
//! logged in?), MCP servers, ssh hosts. Only presence is reported; no key,
//! token or file content is ever printed.

use crate::agents::Cli;
use crate::data::value::{Data, Record, Value};
use std::path::{Path, PathBuf};

fn s(v: impl Into<String>) -> Value {
    Value::Str(v.into())
}

fn row(kind: &str, name: &str, status: &str, detail: String) -> Record {
    vec![
        ("kind".into(), s(kind)),
        ("name".into(), s(name)),
        ("status".into(), s(status)),
        ("detail".into(), s(detail)),
    ]
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn env_set(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|n| std::env::var(n).is_ok_and(|v| !v.is_empty()))
        .map(|n| n.to_string())
}

/// How each CLI keeps its login (files checked for existence only).
fn login(cli: Cli, home: &Path) -> (bool, String) {
    let file = |rel: &str| home.join(rel).exists().then(|| format!("~/{rel}"));
    let (files, envs): (&[&str], &[&str]) = match cli {
        Cli::Claude => (
            &[".claude/.credentials.json"],
            &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        ),
        Cli::Codex => (&[".codex/auth.json"], &["OPENAI_API_KEY"]),
        Cli::Gemini => (
            &[".gemini/oauth_creds.json"],
            &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        ),
        Cli::Aider => (
            &[".aider.conf.yml"],
            &["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "OPENROUTER_API_KEY"],
        ),
        Cli::Generic => (&[], &[]),
    };
    if let Some(f) = files.iter().find_map(|f| file(f)) {
        return (true, format!("credentials in {f}"));
    }
    if let Some(e) = env_set(envs) {
        return (true, format!("${e} is set"));
    }
    // Claude Code on macOS keeps its login in the Keychain; its config file
    // records the signed-in account.
    if cli == Cli::Claude {
        if let Ok(t) = std::fs::read_to_string(home.join(".claude.json")) {
            if t.contains("\"oauthAccount\"") {
                return (true, "signed in (account in ~/.claude.json)".into());
            }
        }
    }
    (false, "not logged in — run it once to sign in".into())
}

fn version(path: &Path) -> String {
    let out = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    out.ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// MCP server names from the agent CLIs' config files (names only).
pub fn mcp_servers(home: &Path, cwd: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, from: String| {
        if !out.iter().any(|(n, f)| n == name && *f == from) {
            out.push((name.to_string(), from));
        }
    };
    let json_keys =
        |path: &Path, pick: &dyn Fn(&serde_json::Value) -> Vec<String>| -> Vec<String> {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|j| pick(&j))
                .unwrap_or_default()
        };
    let keys = |v: Option<&serde_json::Value>| -> Vec<String> {
        v.and_then(|m| m.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    // Claude Code: global and per-project servers in ~/.claude.json, and .mcp.json in the project
    let cwd_s = cwd.to_string_lossy().to_string();
    for n in json_keys(&home.join(".claude.json"), &|j| {
        let mut v = keys(j.get("mcpServers"));
        v.extend(keys(
            j.get("projects")
                .and_then(|p| p.get(&cwd_s))
                .and_then(|p| p.get("mcpServers")),
        ));
        v
    }) {
        add(&n, "claude (~/.claude.json)".into());
    }
    for n in json_keys(&cwd.join(".mcp.json"), &|j| keys(j.get("mcpServers"))) {
        add(&n, "project .mcp.json".into());
    }
    for n in json_keys(&home.join(".gemini/settings.json"), &|j| {
        keys(j.get("mcpServers"))
    }) {
        add(&n, "gemini (~/.gemini/settings.json)".into());
    }
    if let Ok(t) = std::fs::read_to_string(home.join(".codex/config.toml")) {
        if let Ok(v) = toml::from_str::<toml::Table>(&t) {
            if let Some(m) = v.get("mcp_servers").and_then(|m| m.as_table()) {
                for n in m.keys() {
                    add(n, "codex (~/.codex/config.toml)".into());
                }
            }
        }
    }
    out
}

/// `Host` names from an ssh config (no wildcards).
pub fn ssh_hosts(config: &str) -> Vec<String> {
    let mut v = Vec::new();
    for line in config.lines() {
        let l = line.trim();
        let Some(rest) = l.strip_prefix("Host ").or_else(|| l.strip_prefix("host ")) else {
            continue;
        };
        for h in rest.split_whitespace() {
            if !h.contains(['*', '?', '!']) && !v.iter().any(|x: &String| x == h) {
                v.push(h.to_string());
            }
        }
    }
    v
}

pub fn table() -> Data {
    let home = home();
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut rows = Vec::new();
    for cli in Cli::KNOWN {
        match crate::shell::find_in_path(cli.name()) {
            Some(path) => {
                let (ok, how) = login(cli, &home);
                let v = version(&path);
                rows.push(row(
                    "agent cli",
                    cli.name(),
                    if ok { "ready" } else { "not logged in" },
                    if v.is_empty() {
                        how
                    } else {
                        format!("{v} · {how}")
                    },
                ));
            }
            None => rows.push(row("agent cli", cli.name(), "not installed", String::new())),
        }
    }
    for (name, from) in mcp_servers(&home, &cwd) {
        rows.push(row("mcp server", &name, "configured", from));
    }
    let ssh = std::fs::read_to_string(home.join(".ssh/config")).unwrap_or_default();
    for h in ssh_hosts(&ssh) {
        rows.push(row("ssh host", &h, "configured", "~/.ssh/config".into()));
    }
    Data::Table(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_hosts() {
        let cfg = "Host prod prod-db\n  HostName 10.0.0.1\nHost *\n  ForwardAgent no\nhost gh\nMatch all\n";
        assert_eq!(ssh_hosts(cfg), vec!["prod", "prod-db", "gh"]);
    }

    #[test]
    fn mcp_and_logins_from_files() {
        let d = std::env::temp_dir().join(format!("sv-connect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let proj = d.join("proj");
        std::fs::create_dir_all(proj.join("x")).unwrap();
        std::fs::create_dir_all(d.join(".codex")).unwrap();
        std::fs::write(
            d.join(".claude.json"),
            format!(
                "{{\"oauthAccount\":{{}},\"mcpServers\":{{\"github\":{{}}}},\"projects\":{{\"{}\":{{\"mcpServers\":{{\"db\":{{}}}}}}}}}}",
                proj.display()
            ),
        )
        .unwrap();
        std::fs::write(
            proj.join(".mcp.json"),
            "{\"mcpServers\":{\"linear\":{\"command\":\"secret-token\"}}}",
        )
        .unwrap();
        std::fs::write(
            d.join(".codex/config.toml"),
            "[mcp_servers.fs]\ncommand = \"x\"\n",
        )
        .unwrap();
        let names: Vec<String> = mcp_servers(&d, &proj).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["github", "db", "linear", "fs"]);
        let (ok, how) = login(Cli::Claude, &d);
        assert!(ok && how.contains("signed in"), "{how}");
        std::fs::write(d.join(".codex/auth.json"), "{\"token\":\"sk-SECRET\"}").unwrap();
        let (ok, how) = login(Cli::Codex, &d);
        assert!(ok && !how.contains("SECRET"), "{how}");
        let _ = std::fs::remove_dir_all(d);
    }
}

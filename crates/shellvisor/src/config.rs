//! `config.yaml` — user settings. Every field is optional.
//!
//! ```yaml
//! shell: zsh              # or bash, or a full path; default: $SHELL
//! scrollback_lines: 5000  # per shell tab
//! shell_integration: true # report folder / command / exit code to Shellvisor
//! start_view: full       # full (tabs, status bar) or basic (just the shell)
//! restore: ask            # last session's tabs on start: ask, always or never
//! mouse: true             # mouse wheel scrolls history; hold Option (iTerm2) or
//!                         # turn off View → Allow Mouse Reporting (Terminal.app) to select text
//! low_power: auto         # slower sampling and animations: auto (on battery), on, off
//! agents:
//!   worktree: true        # each new agent works in its own git worktree + branch
//!   approvals:            # answer agents' permission prompts for you: auto | ask
//!     test: auto          # read edit test lint install network push delete command
//!   limits:               # agent trees (sv agent spawn): depth, total, sub-agents each
//!     max_depth: 3
//!     max_agents: 10
//!     max_children: 3
//! keys:                   # change or turn off shortcuts (see `F1` for all actions)
//!   new_tab: alt+n        # one key…
//!   next_layout: [f7]     # …or a list
//!   rename_tab: none      # frees alt+r for your shell
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shell: Option<String>,
    pub scrollback_lines: usize,
    pub shell_integration: bool,
    pub mouse: bool,
    pub start_view: StartView,
    pub restore: Restore,
    pub low_power: LowPower,
    pub agents: AgentsConfig,
    pub keys: HashMap<String, KeyList>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Restore {
    #[default]
    Ask,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LowPower {
    /// On while running on battery.
    #[default]
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    pub worktree: bool,
    /// action kind → "auto" or "ask"
    pub approvals: HashMap<String, String>,
    pub limits: LimitsConfig,
    /// Command that turns a goal into a plan (prompt on stdin, JSON out).
    /// Default: `claude -p`, `codex exec -` or `gemini`, whichever is installed.
    pub planner: Option<String>,
}

/// `agents.limits` — how big agent trees may grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_depth: u8,
    pub max_agents: usize,
    pub max_children: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        let d = crate::agents::Limits::default();
        LimitsConfig {
            max_depth: d.max_depth,
            max_agents: d.max_agents,
            max_children: d.max_children,
        }
    }
}

impl LimitsConfig {
    pub fn get(self) -> crate::agents::Limits {
        crate::agents::Limits {
            max_depth: self.max_depth.max(1),
            max_agents: self.max_agents.max(1),
            max_children: self.max_children,
        }
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            worktree: true,
            approvals: HashMap::new(),
            limits: LimitsConfig::default(),
            planner: None,
        }
    }
}

impl AgentsConfig {
    /// Action kinds answered automatically, plus warnings for typos.
    pub fn auto_actions(&self) -> (Vec<crate::agents::Action>, Vec<String>) {
        let mut auto = Vec::new();
        let mut warn = Vec::new();
        for (k, v) in &self.approvals {
            match (crate::agents::Action::parse(k), v.as_str()) {
                (Some(a), "auto") => auto.push(a),
                (Some(_), "ask") => {}
                (Some(_), other) => warn.push(format!("agents.approvals.{k}: `{other}` — use auto or ask")),
                (None, _) => warn.push(format!(
                    "agents.approvals: unknown action `{k}` (read edit test lint install network push delete command)"
                )),
            }
        }
        (auto, warn)
    }
}

/// `alt+n` or `[alt+n, f10]`
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            KeyList::One(s) => vec![s.clone()],
            KeyList::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StartView {
    #[default]
    Full,
    Basic,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            shell: None,
            scrollback_lines: 5000,
            shell_integration: true,
            mouse: true,
            start_view: StartView::Full,
            restore: Restore::Ask,
            low_power: LowPower::Auto,
            agents: AgentsConfig::default(),
            keys: HashMap::new(),
        }
    }
}

impl Config {
    /// Key overrides in the shape the keymap wants.
    pub fn key_overrides(&self) -> HashMap<String, Vec<String>> {
        self.keys
            .iter()
            .map(|(k, v)| (k.clone(), v.to_vec()))
            .collect()
    }

    /// Parse YAML text. An empty file means "all defaults".
    pub fn parse(text: &str) -> Result<Config, String> {
        if text.trim().is_empty() {
            return Ok(Config::default());
        }
        let mut cfg: Config = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
        cfg.scrollback_lines = cfg.scrollback_lines.min(1_000_000);
        Ok(cfg)
    }

    /// Load from disk. A missing file is fine; a broken one falls back to
    /// defaults and returns a warning for the user.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match Config::parse(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (
                    Config::default(),
                    Some(format!("{}: {e} — using defaults", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (
                Config::default(),
                Some(format!("{}: {e} — using defaults", path.display())),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::parse("  \n").unwrap(), Config::default());
    }

    #[test]
    fn partial_fields() {
        let c = Config::parse("shell: bash\n").unwrap();
        assert_eq!(c.shell.as_deref(), Some("bash"));
        assert_eq!(c.scrollback_lines, 5000);
        assert!(c.shell_integration);
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        // Later releases add keys (layouts, keymap…); older binaries must not choke.
        let c = Config::parse("scrollback_lines: 100\nlayouts: {}\n").unwrap();
        assert_eq!(c.scrollback_lines, 100);
    }

    #[test]
    fn start_view() {
        assert_eq!(
            Config::parse("start_view: basic").unwrap().start_view,
            StartView::Basic
        );
        assert_eq!(Config::default().start_view, StartView::Full);
        assert!(Config::parse("start_view: sideways").is_err());
    }

    #[test]
    fn restore_and_keys() {
        let c =
            Config::parse("restore: never\nkeys:\n  new_tab: alt+n\n  next_layout: [f7, alt+y]\n")
                .unwrap();
        assert_eq!(c.restore, Restore::Never);
        let o = c.key_overrides();
        assert_eq!(o["new_tab"], vec!["alt+n"]);
        assert_eq!(o["next_layout"], vec!["f7", "alt+y"]);
        assert_eq!(Config::default().restore, Restore::Ask);
        assert_eq!(
            Config::parse("low_power: on").unwrap().low_power,
            LowPower::On
        );
        assert_eq!(Config::default().low_power, LowPower::Auto);
    }

    #[test]
    fn agent_settings() {
        let c = Config::parse("agents:\n  worktree: false\n  approvals:\n    test: auto\n    lint: ask\n    rm: auto\n").unwrap();
        assert!(!c.agents.worktree);
        let (auto, warn) = c.agents.auto_actions();
        assert_eq!(auto, vec![crate::agents::Action::Test]);
        assert_eq!(warn.len(), 1);
        assert!(Config::default().agents.worktree);
    }

    #[test]
    fn bad_yaml_is_an_error() {
        assert!(Config::parse("scrollback_lines: lots\n").is_err());
    }

    #[test]
    fn missing_file_is_silent() {
        let (c, warn) = Config::load(Path::new("/definitely/not/here.yaml"));
        assert_eq!(c, Config::default());
        assert!(warn.is_none());
    }
}

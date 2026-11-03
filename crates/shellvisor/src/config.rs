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

//! Where Shellvisor keeps its files.
//!
//! Every location can be overridden with an environment variable, which the
//! tests use to stay out of the real home directory.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

fn overridable(var: &str, default: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(default)
}

/// `~/.config/shellvisor/config.yaml`
pub fn config_file() -> PathBuf {
    overridable("SHELLVISOR_CONFIG", || {
        xdg("XDG_CONFIG_HOME", ".config").join("shellvisor/config.yaml")
    })
}

/// `~/.local/state/shellvisor` — history, sessions, agent logs.
pub fn state_dir() -> PathBuf {
    overridable("SHELLVISOR_STATE_DIR", || {
        xdg("XDG_STATE_HOME", ".local/state").join("shellvisor")
    })
}

/// `~/.cache/shellvisor` — shell integration files and the `sv` link.
pub fn cache_dir() -> PathBuf {
    overridable("SHELLVISOR_CACHE_DIR", || {
        xdg("XDG_CACHE_HOME", ".cache").join("shellvisor")
    })
}

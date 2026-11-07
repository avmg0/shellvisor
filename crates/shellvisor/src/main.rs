//! Shellvisor — a multi-pane workspace around your real zsh or bash.
//!
//! One binary, two names:
//!   `shellvisor [--shell zsh|bash|PATH]`  start Shellvisor
//!   `sv <command>`                       control it from inside your shell

/// Product name, as shown to users.
pub const NAME: &str = "Shellvisor";
/// Lower-case name: binary, config and data folders.
pub const NAME_LOWER: &str = "shellvisor";

mod activity;
mod agents;
mod app;
mod autostart;
mod board;
mod bus;
mod cli;
mod config;
mod connect;
mod data;
mod files;
mod flow;
mod flowrun;
mod forwards;
mod gitstat;
mod highlight;
mod history;
mod infopane;
mod keys;
mod layouted;
mod loading;
mod mcp;
mod mcpscreen;
mod mouse;
mod osinfo;
mod palette;
mod paths;
mod remote;
mod review;
mod serial;
mod session;
mod sessionlog;
mod sessions;
mod settings;
mod shell;
mod sysmon;
mod tableview;
mod team;
mod term;
mod termcolors;
mod ui;
mod workspace;

use std::path::{Path, PathBuf};
use sv_proto::{ENV_INSIDE, ENV_PID, ENV_SHELL, ENV_VERSION};

const USAGE: &str = "\
shellvisor — your shell, plus a workspace around it

usage:
  shellvisor [options]
  shellvisor sv <command>      (same as running `sv <command>`)

options:
  -s, --shell <zsh|bash|PATH>  shell to run (default: config, then $SHELL)
      --no-integration         don't add Shellvisor's shell hooks
  -h, --help                   this help
  -V, --version                print the version

inside Shellvisor:
  F1                    all keys
  ctrl+]                leader key (then one letter) — works in every terminal
  alt+t / alt+\\ / alt+-  new tab / split side by side / split stacked
  alt+`                 full view ⇄ basic view (also: sv on / sv off)
  ctrl+q                quit
";

struct Args {
    shell: Option<String>,
    integration: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        shell: None,
        integration: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-s" | "--shell" => {
                out.shell = Some(it.next().ok_or("--shell needs a value")?.clone());
            }
            s if s.starts_with("--shell=") => out.shell = Some(s["--shell=".len()..].to_string()),
            "--no-integration" => out.integration = false,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(out)
}

/// Make `sv` callable inside the shell: `<cache>/bin/sv` → this executable.
fn install_sv_link(cache: &Path) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = cache.join("bin");
    std::fs::create_dir_all(&bin).ok()?;
    let link = bin.join("sv");
    if std::fs::read_link(&link).ok().as_deref() != Some(exe.as_path()) {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link).ok()?;
    }
    Some(bin)
}

fn main() {
    let argv0 = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if argv0 == "sv" {
        std::process::exit(cli::run(&args));
    }
    match args.first().map(String::as_str) {
        Some("sv") => std::process::exit(cli::run(&args[1..])),
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return;
        }
        Some("-V" | "--version") => {
            println!("shellvisor {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }

    let opts = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("shellvisor: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    if cli::inside() {
        eprintln!(
            "shellvisor: already running in this terminal — use `sv off` / `sv on` to switch views"
        );
        std::process::exit(1);
    }

    let (cfg, warning) = config::Config::load(&paths::config_file());
    if let Some(w) = warning {
        eprintln!("shellvisor: {w}");
    }

    let (keymap, mut warnings) = keys::Keymap::new(&cfg.key_overrides());
    let (auto_approve, agent_warnings) = cfg.agents.auto_actions();
    warnings.extend(agent_warnings);
    for w in &warnings {
        eprintln!("shellvisor: {w}");
    }

    let cache = paths::cache_dir();
    let choice = opts.shell.as_deref().or(cfg.shell.as_deref());
    let launch = match shell::build(choice, opts.integration && cfg.shell_integration, &cache) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("shellvisor: {e}");
            std::process::exit(1);
        }
    };

    let mut env = vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "Shellvisor".into()),
        (
            "SHELLVISOR_OUTER_TERM".into(),
            std::env::var("TERM_PROGRAM").unwrap_or_default(),
        ),
        (
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        (ENV_INSIDE.into(), "1".into()),
        (ENV_PID.into(), std::process::id().to_string()),
        (ENV_VERSION.into(), env!("CARGO_PKG_VERSION").into()),
        (ENV_SHELL.into(), launch.name.clone()),
        (
            "SHELLVISOR_HOOKS".into(),
            if launch.integrated { "on" } else { "off" }.into(),
        ),
    ];
    if launch.name == "bash" && cfg!(target_os = "macos") {
        // macOS's bash 3.2 otherwise greets every new pane with "The default
        // interactive shell is now zsh…"
        env.push(("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()));
    }
    // the shell hooks read the settings screen's choices from here
    let ui_env = settings::ui_env_path();
    let _ = settings::Settings::load().save();
    env.push(("SHELLVISOR_UI".into(), ui_env.display().to_string()));
    let bus_path = bus::socket_path(&paths::state_dir());
    env.push((bus::ENV_SOCK.into(), bus_path.display().to_string()));
    if let Some(bin) = install_sv_link(&cache) {
        let path = std::env::var("PATH").unwrap_or_default();
        env.push(("PATH".into(), format!("{}:{path}", bin.display())));
    }

    let result = app::run(app::Options {
        launch,
        extra_env: env,
        scrollback: cfg.scrollback_lines,
        mouse: cfg.mouse,
        history: history::History::new(&paths::state_dir()),
        start_view: match cfg.start_view {
            config::StartView::Full => app::View::Full,
            config::StartView::Basic => app::View::Basic,
        },
        keymap,
        restore: cfg.restore,
        low_power: cfg.low_power,
        light: {
            // the settings screen's theme wins over config.yaml
            let ui = settings::Settings::load();
            match ui.theme.as_str() {
                "light" => true,
                "dark" => false,
                "auto" => config::Theme::Auto.is_light(),
                _ => cfg.theme.is_light(),
            }
        },
        agent_worktree: cfg.agents.worktree,
        agent_limits: cfg.agents.limits.get(),
        planner: cfg.agents.planner.clone(),
        bus_path,
        auto_approve,
        session_path: paths::state_dir().join("session.yaml"),
        warnings,
    });
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("shellvisor: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args() {
        let a = parse_args(&s(&["--shell", "bash"])).unwrap();
        assert_eq!(a.shell.as_deref(), Some("bash"));
        assert!(a.integration);
        let a = parse_args(&s(&["--shell=zsh", "--no-integration"])).unwrap();
        assert_eq!(a.shell.as_deref(), Some("zsh"));
        assert!(!a.integration);
        assert!(parse_args(&s(&["--shell"])).is_err());
        assert!(parse_args(&s(&["--bogus"])).is_err());
    }
}

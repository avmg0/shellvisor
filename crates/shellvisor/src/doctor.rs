//! `sv doctor`: is this terminal set up for Shellvisor? Checks what can be
//! read (version on PATH and running, colour depth, UTF-8, shell hooks,
//! mouse, autostart) and, with a key press, Option-as-Alt. Each check says
//! how to fix what is off. Reads nothing private; changes nothing.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Warn,
    Bad,
    Info,
}

impl Mark {
    fn symbol(self) -> &'static str {
        match self {
            Mark::Ok => "\x1b[32m✔\x1b[0m",
            Mark::Warn => "\x1b[33m⚠\x1b[0m",
            Mark::Bad => "\x1b[31m✖\x1b[0m",
            Mark::Info => "\x1b[2m·\x1b[0m",
        }
    }
}

pub struct Check {
    pub mark: Mark,
    pub what: String,
    pub fix: Option<String>,
}

fn check(mark: Mark, what: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        mark,
        what: what.into(),
        fix: fix.map(str::to_string),
    }
}

/// Every `sv` on PATH and the version it reports.
fn copies_on_path() -> Vec<(PathBuf, String)> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for dir in std::env::split_paths(&path) {
        let p = dir.join("sv");
        if !p.is_file() {
            continue;
        }
        let real = std::fs::canonicalize(&p).unwrap_or(p.clone());
        if seen.contains(&real) {
            continue;
        }
        seen.push(real);
        let v = std::process::Command::new(&p)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        out.push((p, v.trim_start_matches("sv ").to_string()));
    }
    out
}

/// The checks that only read the environment (no key press).
pub fn passive() -> Vec<Check> {
    let mut v = Vec::new();
    let me = env!("CARGO_PKG_VERSION");
    let env = |k: &str| std::env::var(k).unwrap_or_default();

    // versions: this one, the running one, other copies on PATH
    let inside = crate::cli::inside();
    let running = env(sv_proto::ENV_VERSION);
    if inside && !running.is_empty() && running != me {
        v.push(check(
            Mark::Warn,
            format!("Shellvisor running here is {running}, but sv on PATH is {me}"),
            Some("quit it (ctrl+q) and start it again to use the new version"),
        ));
    } else if inside {
        v.push(check(
            Mark::Ok,
            format!("Shellvisor {me} is running in this terminal"),
            None,
        ));
    } else {
        v.push(check(
            Mark::Info,
            format!("Shellvisor {me} is installed, not running in this terminal"),
            Some("start it with: shellvisor (or sv on)"),
        ));
    }
    let copies = copies_on_path();
    let cache = crate::paths::cache_dir();
    let stale: Vec<String> = copies
        .iter()
        .filter(|(p, ver)| !ver.is_empty() && ver != me && !p.starts_with(&cache))
        .map(|(p, ver)| format!("{} ({ver})", p.display()))
        .collect();
    if !stale.is_empty() {
        v.push(check(
            Mark::Warn,
            format!("other sv copies on PATH: {}", stale.join(", ")),
            Some("remove the old copy so the new one is used everywhere"),
        ));
    }

    // terminal
    let outer = env("SHELLVISOR_OUTER_TERM");
    let term = if inside && !outer.is_empty() {
        outer
    } else {
        env("TERM_PROGRAM")
    };
    let name = match term.as_str() {
        "Apple_Terminal" => "Terminal.app".to_string(),
        "iTerm.app" => "iTerm2".to_string(),
        "" => env("TERM"),
        t => t.to_string(),
    };
    v.push(check(Mark::Info, format!("terminal: {name}"), None));
    // inside Shellvisor, COLORTERM is ours: judge the terminal around it
    let truecolor = if inside {
        match term.as_str() {
            "Apple_Terminal" => crate::palette::macos_major().is_some_and(|v| v >= 26),
            "iTerm.app" | "ghostty" | "WezTerm" | "vscode" | "kitty" | "Hyper" | "Tabby" => true,
            _ => crate::palette::truecolor_terminal(),
        }
    } else {
        crate::palette::truecolor_terminal()
    };
    if truecolor {
        v.push(check(Mark::Ok, "24-bit colour", None));
    } else {
        v.push(check(
            Mark::Warn,
            "256 colours only: the design's colours are approximated",
            Some(if term == "Apple_Terminal" {
                "Terminal.app shows 24-bit colour from macOS 26; or use iTerm2, Ghostty, WezTerm or Kitty"
            } else {
                "set COLORTERM=truecolor if your terminal supports it"
            }),
        ));
    }
    let locale = [env("LC_ALL"), env("LC_CTYPE"), env("LANG")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    if locale.to_uppercase().contains("UTF-8") || locale.to_uppercase().contains("UTF8") {
        v.push(check(Mark::Ok, format!("UTF-8 ({locale})"), None));
    } else {
        v.push(check(
            Mark::Bad,
            format!(
                "locale isn't UTF-8 ({}): frames and icons may show as ?",
                if locale.is_empty() { "unset" } else { &locale }
            ),
            Some("export LANG=en_US.UTF-8 in your shell's rc file"),
        ));
    }

    // shell hooks
    if inside {
        let hooks = env("SHELLVISOR_HOOKS");
        if hooks == "1" || hooks == "on" || hooks.starts_with("zsh") || hooks.starts_with("bash") {
            v.push(check(
                Mark::Ok,
                format!("shell hooks on ({})", env(sv_proto::ENV_SHELL)),
                None,
            ));
        } else {
            v.push(check(
                Mark::Warn,
                format!("shell hooks: {}", if hooks.is_empty() { "off" } else { &hooks }),
                Some("tab states, the folder sync and `ls` tables need zsh or bash with hooks (shell_integration: true)"),
            ));
        }
    } else {
        let shell = env("SHELL");
        let base = shell.rsplit('/').next().unwrap_or("");
        let m = if base == "zsh" || base == "bash" {
            Mark::Ok
        } else {
            Mark::Warn
        };
        v.push(check(
            m,
            format!(
                "login shell: {}",
                if shell.is_empty() { "unknown" } else { &shell }
            ),
            (m == Mark::Warn)
                .then_some("hooks work with zsh and bash; other shells run without them"),
        ));
    }

    // settings
    let s = crate::settings::Settings::load();
    if s.mouse == "terminal" {
        v.push(check(
            Mark::Info,
            "mouse: the terminal's own (settings → Mouse); clicking splits and the right-click menu are off",
            None,
        ));
    } else {
        v.push(check(
            Mark::Ok,
            "mouse: Shellvisor's (click, drag, wheel, right-click menu)",
            None,
        ));
    }

    // autostart
    let home = PathBuf::from(env("HOME"));
    let shell = env("SHELL");
    let base = shell.rsplit('/').next().unwrap_or("zsh");
    let on = crate::autostart::rc_files(&home, base)
        .iter()
        .any(|f| std::fs::read_to_string(f).is_ok_and(|t| crate::autostart::installed(&t)));
    v.push(check(
        Mark::Info,
        if on {
            "autostart: on (new terminal windows start Shellvisor)"
        } else {
            "autostart: off"
        },
        (!on).then_some("sv autostart on starts it in every new terminal window"),
    ));
    if cfg!(target_os = "macos") {
        v.push(check(
            Mark::Info,
            "F-keys on a Mac need fn unless System Settings → Keyboard → \"Use F1, F2, etc. keys as standard function keys\" is on",
            Some("every F-key also has a ctrl+] version (ctrl+] ? lists them)"),
        ));
    }
    v
}

/// Ask for alt+1 and look at what arrives: ESC 1 (good), ¡ (Option types
/// characters), or nothing within a few seconds.
pub fn option_as_alt() -> Option<Check> {
    use std::os::fd::AsRawFd;
    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();
    if unsafe { libc::isatty(fd) } != 1 {
        return None;
    }
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
        return None;
    }
    let mut raw = old;
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 1;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
    print!("  press alt+1 (Option+1 on a Mac) — or wait 6 s to skip … ");
    let _ = std::io::stdout().flush();
    let start = Instant::now();
    let mut got = Vec::new();
    let mut lock = stdin.lock();
    while start.elapsed() < Duration::from_secs(6) {
        let mut b = [0u8; 16];
        if let Ok(n) = lock.read(&mut b) {
            got.extend_from_slice(&b[..n]);
        }
        if !got.is_empty() && start.elapsed() > Duration::from_millis(50) {
            // the whole sequence comes at once
            std::thread::sleep(Duration::from_millis(30));
            if let Ok(n) = lock.read(&mut b) {
                got.extend_from_slice(&b[..n]);
            }
            break;
        }
    }
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
    println!("\r\x1b[K");
    let fix = crate::keys::option_fix_hint();
    Some(if got.is_empty() {
        check(Mark::Info, "Option-as-Alt: skipped (no key)", None)
    } else if got.len() == 2 && got[0] == 0x1b && got[1] == b'1' {
        check(
            Mark::Ok,
            "Option-as-Alt: alt shortcuts reach Shellvisor",
            None,
        )
    } else if crate::keys::is_option_char(&got) {
        check(
            Mark::Bad,
            format!(
                "Option-as-Alt: the terminal typed {} instead of alt+1",
                String::from_utf8_lossy(&got)
            ),
            Some(fix),
        )
    } else {
        check(
            Mark::Warn,
            format!("Option-as-Alt: got {} — not alt+1", got.escape_ascii()),
            Some("press Option (Alt) and 1 together; the leader key ctrl+] always works"),
        )
    })
}

pub fn run(args: &[String]) -> i32 {
    let keys = !args.iter().any(|a| a == "--no-keys");
    println!("\x1b[1mShellvisor doctor\x1b[0m");
    let mut all = passive();
    if keys {
        if let Some(c) = option_as_alt() {
            all.push(c);
        }
    }
    let mut bad = 0;
    for c in &all {
        println!("  {} {}", c.mark.symbol(), c.what);
        if let Some(f) = &c.fix {
            if c.mark != Mark::Ok {
                println!("      \x1b[2m→ {f}\x1b[0m");
            }
        }
        if c.mark == Mark::Bad {
            bad += 1;
        }
    }
    let warn = all.iter().filter(|c| c.mark == Mark::Warn).count();
    println!();
    match (bad, warn) {
        (0, 0) => println!("all good"),
        (b, w) => println!("{b} to fix, {w} to look at"),
    }
    i32::from(bad > 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn passive_checks_run_anywhere() {
        let v = super::passive();
        assert!(v.iter().any(|c| c.what.contains("terminal:")));
        assert!(
            v.iter()
                .any(|c| c.what.contains("UTF-8") || c.what.contains("locale"))
        );
    }
}

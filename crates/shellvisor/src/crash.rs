//! Crash reports, kept on this computer.
//!
//! A panic writes `state/crashes/crash-YYYYMMDD-HHMMSS.txt`: the version, OS,
//! the terminal around Shellvisor, the shell, the panic message, where it
//! happened and a backtrace. Your home folder and user name are replaced
//! by `~` and `<user>`. Nothing is sent anywhere: `sv bug` shows the report
//! and opens a new GitHub issue in the browser with it filled in, for you
//! to read, edit and submit (or not).

use std::path::{Path, PathBuf};

const ISSUES: &str = "https://github.com/avmg0/shellvisor/issues/new";
/// Keep this many reports; older ones are removed.
const KEEP: usize = 10;

pub fn crash_dir() -> PathBuf {
    crate::paths::state_dir().join("crashes")
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn uname() -> String {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return std::env::consts::OS.to_string();
    }
    let s = |f: &[libc::c_char]| {
        let b: Vec<u8> = f
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&b).into_owned()
    };
    format!("{} {} {}", s(&u.sysname), s(&u.release), s(&u.machine))
}

fn macos_version() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    Some(format!(
        "macOS {}",
        String::from_utf8_lossy(&out.stdout).trim()
    ))
}

/// Facts about this setup, without anything personal.
pub fn environment() -> Vec<(&'static str, String)> {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let mut terminal = var("SHELLVISOR_OUTER_TERM");
    if terminal.is_empty() {
        terminal = var("TERM_PROGRAM");
    }
    if terminal.is_empty() {
        terminal = "unknown".into();
    }
    let shell = {
        let s = var("SHELLVISOR_SHELL");
        if s.is_empty() {
            Path::new(&var("SHELL"))
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            s
        }
    };
    let mut v = vec![
        ("version", env!("CARGO_PKG_VERSION").to_string()),
        ("os", macos_version().unwrap_or_else(uname)),
        ("arch", std::env::consts::ARCH.to_string()),
        ("terminal", terminal),
        ("TERM", var("TERM")),
        ("shell", shell),
    ];
    if cfg!(target_os = "macos") {
        v.push(("kernel", uname()));
    }
    v
}

/// Replace the home folder and user name so a report can be shared.
pub fn scrub(text: &str) -> String {
    let mut t = text.to_string();
    if let Ok(home) = std::env::var("HOME") {
        if home.len() > 1 {
            t = t.replace(&home, "~");
        }
    }
    for k in ["USER", "LOGNAME"] {
        if let Ok(u) = std::env::var(k) {
            if u.len() >= 3 {
                t = t.replace(&u, "<user>");
            }
        }
    }
    t
}

/// The text of a report for `info`.
fn report(message: &str, location: &str, thread: &str, backtrace: &str) -> String {
    let mut s = String::from("Shellvisor crash report\n\n");
    for (k, v) in environment() {
        s.push_str(&format!("{k:<9} {v}\n"));
    }
    s.push_str(&format!(
        "thread    {thread}\nat        {location}\n\npanic: {message}\n\nbacktrace:\n{backtrace}\n"
    ));
    scrub(&s)
}

fn keep_newest(dir: &Path) {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    while v.len() > KEEP {
        let _ = std::fs::remove_file(v.remove(0));
    }
}

/// Write a report; returns its path.
pub fn write(message: &str, location: &str, thread: &str, backtrace: &str) -> Option<PathBuf> {
    let dir = crash_dir();
    std::fs::create_dir_all(&dir).ok()?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let path = dir.join(format!("crash-{}.txt", stamp()));
    std::fs::write(&path, report(message, location, thread, backtrace)).ok()?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    keep_newest(&dir);
    Some(path)
}

/// Install the panic hook. Call first in `main`; a hook set later (the
/// one that restores the terminal) runs before this one.
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let bt = std::backtrace::Backtrace::force_capture().to_string();
        eprintln!("\nshellvisor: crashed: {message} ({location})");
        match write(&message, &location, &thread, &bt) {
            Some(p) => eprintln!(
                "A report was saved to {}\nNothing was sent. `sv bug` opens a GitHub issue with it filled in, for you to check first.",
                scrub(&p.display().to_string())
            ),
            None => eprintln!("(couldn't save a crash report)"),
        }
    }));
}

/// The newest report, if any.
pub fn latest() -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(crash_dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    v.sort();
    v.pop()
}

pub fn urlencode(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                o.push(b as char)
            }
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

/// The body of a new issue: what happened (for you to fill in), this setup,
/// and the newest crash report when there is one.
pub fn issue_body(crash: Option<&str>) -> String {
    let mut s = String::from(
        "### What happened\n\n<!-- what you did, what you expected, what you saw -->\n\n### Setup\n\n",
    );
    for (k, v) in environment() {
        s.push_str(&format!("- {k}: {v}\n"));
    }
    if let Some(c) = crash {
        // Browsers and GitHub cap the URL length; keep the top of the report.
        let mut c = c.to_string();
        if c.len() > 5000 {
            let mut cut = 5000;
            while !c.is_char_boundary(cut) {
                cut -= 1;
            }
            c.truncate(cut);
            c.push_str("\n… (cut; the full report is in the file)");
        }
        s.push_str(&format!(
            "\n### Crash report\n\n<details><summary>report</summary>\n\n```\n{c}\n```\n</details>\n"
        ));
    }
    scrub(&s)
}

fn open(url: &str) -> bool {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `sv bug [--print] [--no-crash]`
pub fn bug(args: &[String]) -> i32 {
    let print_only = args.iter().any(|a| a == "--print");
    let crash = if args.iter().any(|a| a == "--no-crash") {
        None
    } else {
        latest().and_then(|p| std::fs::read_to_string(p).ok())
    };
    let body = issue_body(crash.as_deref());
    let title = if crash.is_some() { "Crash: " } else { "" };
    println!("{body}");
    if print_only {
        return 0;
    }
    let url = format!(
        "{ISSUES}?title={}&body={}",
        urlencode(title),
        urlencode(&body)
    );
    if open(&url) {
        println!(
            "Opened a new issue in your browser with the text above. Nothing is sent until you submit it there."
        );
    } else {
        println!("Couldn't open a browser. Copy the text above into a new issue at {ISSUES}");
    }
    0
}

/// `sv crashes`: list the saved reports.
pub fn list() -> i32 {
    let mut v: Vec<PathBuf> = std::fs::read_dir(crash_dir())
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    if v.is_empty() {
        println!("no crash reports");
    }
    for p in v.iter().rev() {
        println!("{}", scrub(&p.display().to_string()));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_and_body() {
        assert_eq!(urlencode("a b&c/é"), "a%20b%26c%2F%C3%A9");
        let b = issue_body(Some("panic: boom"));
        assert!(b.contains("### Setup") && b.contains("version") && b.contains("panic: boom"));
        let long = "x".repeat(9000);
        assert!(issue_body(Some(&long)).len() < 6000);
        assert!(!issue_body(None).contains("Crash report"));
    }

    #[test]
    fn report_has_the_facts() {
        let r = report("boom", "src/app.rs:1", "main", "0: frame");
        for want in [
            "version",
            "terminal",
            "panic: boom",
            "src/app.rs:1",
            "backtrace",
        ] {
            assert!(r.contains(want), "{want} in {r}");
        }
    }
}

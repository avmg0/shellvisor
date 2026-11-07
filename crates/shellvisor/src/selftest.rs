//! `sv selftest`: start each shell on this machine the way Shellvisor does
//! (with the hooks and your own dotfiles), in a hidden terminal, and check
//! what Shellvisor relies on: the prompt shows up (and how fast), the folder
//! is followed, exit codes and command text arrive. Nothing is changed;
//! nothing is drawn but the report.

use crate::term::Pane;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use sv_proto::ShellEvent;

struct Run {
    pane: Pane,
    rx: mpsc::Receiver<Vec<u8>>,
    events: Vec<ShellEvent>,
    text: String,
}

impl Run {
    /// Read output for up to `t`, until `done` says so.
    fn until(&mut self, t: Duration, done: impl Fn(&[ShellEvent], &str) -> bool) -> bool {
        let end = Instant::now() + t;
        while Instant::now() < end {
            if done(&self.events, &self.text) {
                return true;
            }
            if let Ok(bytes) = self.rx.recv_timeout(Duration::from_millis(50)) {
                let fx = self.pane.feed(&bytes);
                if !fx.replies.is_empty() {
                    let _ = self.pane.write(&fx.replies);
                }
                self.events.extend(fx.events);
                self.text.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        done(&self.events, &self.text)
    }

    fn send(&mut self, s: &str) {
        self.events.clear();
        let _ = self.pane.write(s.as_bytes());
    }
}

fn version_of(program: &std::path::Path) -> String {
    std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
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

fn line(ok: bool, what: &str) {
    let mark = if ok {
        "\x1b[32m✔\x1b[0m"
    } else {
        "\x1b[31m✖\x1b[0m"
    };
    println!("    {mark} {what}");
}

/// Check one shell; returns how many checks failed.
fn check_shell(choice: &str) -> usize {
    let launch = match crate::shell::build(Some(choice), true, &crate::paths::cache_dir()) {
        Ok(l) => l,
        Err(e) => {
            println!("  {choice}: {e}");
            return 1;
        }
    };
    println!(
        "\n  \x1b[1m{}\x1b[0m  \x1b[2m{} · {}\x1b[0m",
        launch.name,
        launch.program.display(),
        version_of(&launch.program)
    );
    if !launch.integrated {
        line(false, "no hooks for this shell (zsh and bash only)");
        return 1;
    }
    let mut env: Vec<(String, String)> = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("TERM_PROGRAM".into(), "Shellvisor".into()),
        (sv_proto::ENV_INSIDE.into(), "1".into()),
        (
            sv_proto::ENV_VERSION.into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
        (sv_proto::ENV_SHELL.into(), launch.name.clone()),
        ("SHELLVISOR_HOOKS".into(), "on".into()),
        (
            "SHELLVISOR_UI".into(),
            crate::settings::ui_env_path().display().to_string(),
        ),
    ];
    if launch.name == "bash" && cfg!(target_os = "macos") {
        env.push(("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()));
    }
    let home = std::env::var("HOME").map(std::path::PathBuf::from).ok();
    let spawned = match Pane::spawn(&launch, &env, home.as_deref(), 30, 120, 500) {
        Ok(s) => s,
        Err(e) => {
            line(false, &format!("couldn't start it: {e}"));
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel();
    let mut reader = spawned.reader;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = std::io::Read::read(&mut reader, &mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut run = Run {
        pane: spawned.pane,
        rx,
        events: Vec::new(),
        text: String::new(),
    };
    let mut failed = 0;
    let mut check = |ok: bool, what: &str| {
        line(ok, what);
        if !ok {
            failed += 1;
        }
    };

    // 1. the prompt, and how long your dotfiles take
    let t0 = Instant::now();
    let prompt = run.until(Duration::from_secs(15), |e, _| {
        e.iter().any(|x| matches!(x, ShellEvent::PromptShown))
    });
    let ms = t0.elapsed().as_millis();
    if !prompt {
        check(
            false,
            "no prompt from the hooks within 15 s — your rc files may stop them; try: sv doctor",
        );
        let mut child = spawned.child;
        let _ = child.kill();
        return failed;
    }
    let slow = ms > 1000;
    check(
        !slow || ms < 3000,
        &format!(
            "prompt in {ms} ms{}",
            if slow {
                " — every new tab and split waits this long for your rc files"
            } else {
                ""
            }
        ),
    );
    // 2. the folder is followed
    let tmp = std::env::temp_dir();
    run.send(&format!("cd {}\r", tmp.display()));
    let cwd = run.until(Duration::from_secs(5), |e, _| {
        e.iter().any(|x| matches!(x, ShellEvent::Cwd(_)))
            && e.iter().any(|x| matches!(x, ShellEvent::PromptShown))
    });
    check(cwd, "cd is followed (tab name, files pane)");
    // 3. exit codes
    run.send("false\r");
    let code1 = run.until(Duration::from_secs(5), |e, _| {
        e.contains(&ShellEvent::CommandFinished(Some(1)))
    });
    run.send("true\r");
    let code0 = run.until(Duration::from_secs(5), |e, _| {
        e.contains(&ShellEvent::CommandFinished(Some(0)))
    });
    check(
        code1 && code0,
        "exit codes (✔ / ✖ on tabs, the failed-command card)",
    );
    // 4. the command line itself
    run.send("echo sv-selftest-$((40+2))\r");
    let text = run.until(Duration::from_secs(5), |e, t| {
        t.contains("sv-selftest-42")
            && e.iter()
                .any(|x| matches!(x, ShellEvent::CommandText(c) if c.contains("sv-selftest")))
    });
    check(
        text,
        "the command you ran is known (tab labels, history, downloads)",
    );
    // 5. a running command is seen as running
    run.send("sleep 0.4; echo slept\r");
    let started = run.until(Duration::from_secs(5), |e, _| {
        e.iter().any(|x| matches!(x, ShellEvent::CommandStarted))
    });
    let finished = run.until(Duration::from_secs(5), |e, t| {
        t.contains("slept")
            && e.iter()
                .any(|x| matches!(x, ShellEvent::CommandFinished(_)))
    });
    check(
        started && finished,
        "running and finished (spinner, notifications)",
    );
    run.send("exit\r");
    let mut child = spawned.child;
    let _ = run.until(Duration::from_millis(500), |_, _| false);
    let _ = child.kill();
    failed
}

pub fn run(args: &[String]) -> i32 {
    println!(
        "\x1b[1mShellvisor selftest\x1b[0m — your shells, with the hooks and your own rc files"
    );
    let mut shells: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    if shells.is_empty() {
        // macOS's own bash 3.2, your login shell, and the other of zsh / bash
        let mut want = vec!["/bin/bash".to_string()];
        if let Ok(s) = std::env::var("SHELL") {
            want.push(s);
        }
        want.push("zsh".into());
        want.push("bash".into());
        for w in want {
            let p = if w.contains('/') {
                std::path::PathBuf::from(&w)
            } else {
                match crate::shell::find_in_path(&w) {
                    Some(p) => p,
                    None => continue,
                }
            };
            let real = std::fs::canonicalize(&p).unwrap_or(p.clone());
            if p.exists()
                && !shells
                    .iter()
                    .any(|s| std::fs::canonicalize(s).unwrap_or_default() == real)
            {
                shells.push(p.display().to_string());
            }
        }
    }
    let mut failed = 0;
    for s in &shells {
        failed += check_shell(s);
    }
    println!();
    if failed == 0 {
        println!("all good");
    } else {
        println!("{failed} check(s) failed — paste this output into an issue");
    }
    i32::from(failed > 0)
}

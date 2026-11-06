//! End-to-end tests: run the real `shellvisor` binary inside a pseudo-terminal
//! with a real bash / zsh, type into it, and read the screen back.

use portable_pty::{CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(15);
const NAME: &str = "Shellvisor";
const NAME_LOWER: &str = "shellvisor";

struct Session {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    rx: Receiver<Vec<u8>>,
    screen: vt100::Parser,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    dir: PathBuf,
    /// Keep the scratch folder on drop (to restart in the same state).
    keep: bool,
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sv-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home")).unwrap();
    std::fs::create_dir_all(d.join("work")).unwrap();
    // Predictable prompts, while still going through the user-dotfile path.
    std::fs::write(d.join("home/.bash_profile"), "PS1='PROMPT> '\n").unwrap();
    std::fs::write(d.join("home/.zshrc"), "PROMPT='PROMPT> '\nunsetopt BEEP\n").unwrap();
    // Debian/Ubuntu's /etc/zsh/zshrc runs compinit, which stops to ask about
    // "insecure directories" on CI runners
    std::fs::write(d.join("home/.zshenv"), "skip_global_compinit=1\n").unwrap();
    d
}

impl Session {
    /// Start in basic view (looks like a plain terminal).
    fn start(shell: &str, tag: &str) -> Session {
        Self::start_with(shell, tag, "start_view: basic\n")
    }

    /// Start in full view (tab bar, borders, status bar).
    fn start_full(tag: &str) -> Session {
        Self::start_with("bash", tag, "start_view: full\n")
    }

    fn start_with(shell: &str, tag: &str, config: &str) -> Session {
        let dir = scratch(tag);
        std::fs::write(dir.join("config.yaml"), config).unwrap();
        Self::launch(shell, dir)
    }

    /// Start again in an existing scratch folder (same state, same config).
    fn launch(shell: &str, dir: PathBuf) -> Session {
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_shellvisor"));
        cmd.args(["--shell", shell]);
        cmd.cwd(dir.join("work"));
        for k in [
            "SHELLVISOR",
            "SHELLVISOR_PID",
            "ZDOTDIR",
            "PROMPT_COMMAND",
            "BASH_ENV",
            "ENV",
        ] {
            cmd.env_remove(k);
        }
        cmd.env("HOME", dir.join("home"));
        cmd.env("TERM", "xterm-256color");
        cmd.env("SHELL", "/bin/sh");
        cmd.env("SHELLVISOR_STATE_DIR", dir.join("state"));
        // the first-run tour only where a test asks for it
        if !dir.join("tour").exists() {
            cmd.env("SHELLVISOR_TOUR", "0");
        }
        // stand-in programs (ssh, agent CLIs) in home/bin come first
        if dir.join("home/bin").is_dir() {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.join("home/bin").display()));
        }
        let fake_ssh = dir.join("home/bin/ssh");
        if fake_ssh.exists() {
            cmd.env("SHELLVISOR_SSH", &fake_ssh);
        }
        cmd.env("SHELLVISOR_CACHE_DIR", dir.join("cache"));
        cmd.env("SHELLVISOR_CONFIG", dir.join("config.yaml"));
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Session {
            writer,
            master: pair.master,
            rx,
            screen: vt100::Parser::new(24, 100, 0),
            child,
            dir,
            keep: false,
        }
    }

    fn pump(&mut self, for_: Duration) {
        let end = Instant::now() + for_;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(b) => self.screen.process(&b),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
    }

    fn text(&self) -> String {
        self.screen.screen().contents()
    }

    fn wait_for(&mut self, needle: &str) {
        let end = Instant::now() + WAIT;
        while Instant::now() < end {
            if self.text().contains(needle) {
                return;
            }
            self.pump(Duration::from_millis(100));
        }
        panic!("timed out waiting for {needle:?}; screen:\n{}", self.text());
    }

    fn count(&self, needle: &str) -> usize {
        self.text().matches(needle).count()
    }

    fn send(&mut self, s: &str) {
        self.writer.write_all(s.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// Type a command and wait for the next prompt.
    fn run(&mut self, cmd: &str) {
        let prompts = self.count("PROMPT>");
        self.send(&format!("{cmd}\r"));
        let end = Instant::now() + WAIT;
        while self.count("PROMPT>") <= prompts {
            assert!(
                Instant::now() < end,
                "no prompt after {cmd:?}; screen:\n{}",
                self.text()
            );
            self.pump(Duration::from_millis(100));
        }
    }

    fn wait_exit(&mut self) -> u32 {
        let end = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            assert!(
                Instant::now() < end,
                "shellvisor did not exit; screen:\n{}",
                self.text()
            );
            self.pump(Duration::from_millis(100));
        }
    }

    fn history(&self) -> String {
        std::fs::read_to_string(self.dir.join("state/history.jsonl")).unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if !self.keep {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn lifecycle(shell: &str) {
    let mut s = Session::start(shell, shell);
    s.wait_for("PROMPT>");

    // Ordinary commands work and render.
    s.run("echo hel''lo-from-${0##*/}");
    s.wait_for(&format!(
        "hello-from-{}",
        if shell == "zsh" { "zsh" } else { "bash" }
    ));

    // Hooks report the command, exit code and duration.
    s.run("cd /tmp && false");
    s.run("sleep 0.3");
    let h = s.history();
    assert!(h.contains(r#""cmd":"cd /tmp && false""#), "history:\n{h}");
    assert!(h.contains(r#""exit":1"#), "history:\n{h}");
    assert!(
        h.contains(r#""cwd":"/tmp""#),
        "cwd after cd should be /tmp:\n{h}"
    );
    let sleep_line = h.lines().find(|l| l.contains("sleep 0.3")).unwrap();
    let ms: u64 = sleep_line
        .split(r#""duration_ms":"#)
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(ms >= 250, "duration {ms}ms");

    // `sv` is on PATH and knows it is inside Shellvisor.
    s.run("sv status");
    s.wait_for("running (pid");
    s.wait_for(&format!("shell: {shell}"));
    s.wait_for("hooks: on");

    // Our private escape sequences never reach the screen.
    assert!(!s.text().contains("7770"), "{}", s.text());

    // `sv exit` leaves Shellvisor.
    s.send("sv exit\r");
    assert_eq!(s.wait_exit(), 0);
}

#[test]
fn bash_lifecycle() {
    if have("bash") {
        lifecycle("bash");
    }
}

#[test]
fn zsh_lifecycle() {
    if have("zsh") {
        lifecycle("zsh");
    }
}

#[test]
fn ctrl_q_quits() {
    let mut s = Session::start("bash", "ctrlq");
    s.wait_for("PROMPT>");
    s.send("\x11");
    assert_eq!(s.wait_exit(), 0);
}

#[test]
fn shell_exit_code_is_returned() {
    let mut s = Session::start("bash", "exitcode");
    s.wait_for("PROMPT>");
    s.send("exit 7\r");
    assert_eq!(s.wait_exit(), 7);
}

#[test]
fn resize_reaches_the_shell() {
    let mut s = Session::start("bash", "resize");
    s.wait_for("PROMPT>");
    s.master
        .resize(PtySize {
            rows: 30,
            cols: 90,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    s.screen.screen_mut().set_size(30, 90);
    s.pump(Duration::from_millis(300));
    s.run("stty size");
    s.wait_for("30 90");
}

#[test]
fn cursor_position_queries_are_answered() {
    // Programs like fzf and some prompts ask the terminal where the cursor is.
    let mut s = Session::start("bash", "dsr");
    s.wait_for("PROMPT>");
    // Like real programs (fzf etc.), turn echo off *before* asking.
    s.run(r#"stty -echo; printf '\033[6n'; IFS= read -rd R -t 3 r; stty echo; echo "got-${r#*[}-end""#);
    // Wait for the *output* line (the typed command also contains "got-").
    let end = Instant::now() + WAIT;
    let line = loop {
        if let Some(l) = s.text().lines().find(|l| l.starts_with("got-")) {
            break l.to_string();
        }
        assert!(Instant::now() < end, "no reply; screen:\n{}", s.text());
        s.pump(Duration::from_millis(50));
    };
    assert!(line.contains(';') && line.ends_with("-end"), "{line}");
}

#[test]
fn full_screen_apps_use_alternate_screen_and_come_back() {
    let mut s = Session::start("bash", "altscreen");
    s.wait_for("PROMPT>");
    s.run("echo before-app");
    s.send("printf '\\033[?1049h\\033[HFULLSCREEN-APP'; sleep 1; printf '\\033[?1049l'\r");
    // While the app runs, the shell's screen is hidden…
    s.wait_for("FULLSCREEN-APP");
    assert!(
        s.text()
            .lines()
            .next()
            .unwrap()
            .starts_with("FULLSCREEN-APP")
    );
    assert!(!s.text().contains("before-app"));
    // …and it comes back intact afterwards.
    s.wait_for("before-app");
    assert!(!s.text().lines().any(|l| l.starts_with("FULLSCREEN-APP")));
}

#[test]
fn vim_round_trip() {
    if !have("vim") {
        return;
    }
    let mut s = Session::start("bash", "vim");
    s.wait_for("PROMPT>");
    s.send("vim -u NONE -N note.txt\r");
    s.wait_for("note.txt");
    s.send("iShellvisor vim test\x1b:wq\r");
    s.wait_for("PROMPT>");
    s.run("cat note.txt");
    s.wait_for("Shellvisor vim test");
    let saved = std::fs::read_to_string(s.dir.join("work/note.txt")).unwrap();
    assert_eq!(saved.trim(), "Shellvisor vim test");
}

#[test]
fn already_inside_is_refused() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_shellvisor"))
        .env("SHELLVISOR", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already running"));
}

#[test]
fn sv_outside_shellvisor() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_shellvisor"))
        .args(["sv", "status"])
        .env_remove("SHELLVISOR")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("not running"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_shellvisor"))
        .args(["sv", "exit"])
        .env_remove("SHELLVISOR")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

// ------------------------------------------------------------------ P1a

impl Session {
    fn line(&self, row: usize) -> String {
        self.text().lines().nth(row).unwrap_or_default().to_string()
    }

    /// Wait until `pred(screen)` holds.
    fn wait_until(&mut self, what: &str, pred: impl Fn(&str) -> bool) {
        let end = Instant::now() + WAIT;
        while !pred(&self.text()) {
            assert!(
                Instant::now() < end,
                "timed out waiting for {what}; screen:\n{}",
                self.text()
            );
            self.pump(Duration::from_millis(50));
        }
    }
}

fn top(t: &str) -> &str {
    // row 0 is the header, row 1 the tab strip
    t.lines().nth(1).unwrap_or("")
}

#[test]
fn full_view_has_tab_bar_border_and_status_bar() {
    let mut s = Session::start_full("full");
    s.wait_for("PROMPT>");
    // the boards' chrome: header, tab strip, panes, key strip
    assert!(
        s.line(0).contains(&format!("▍{}", NAME_LOWER)),
        "header: {:?}",
        s.line(0)
    );
    assert!(
        s.line(0).contains("CPU") && s.line(0).contains("MEM"),
        "header: {:?}",
        s.line(0)
    );
    let tabs = s.line(1);
    assert!(
        tabs.contains("1 bash") && tabs.contains("+ new"),
        "tab strip: {tabs:?}"
    );
    assert!(
        s.line(2).starts_with('╭') && s.line(2).contains("SHELL · bash"),
        "border: {:?}",
        s.line(2)
    );
    s.wait_for("F1 Help");
    s.wait_for("F2 Files");
}

#[test]
fn tabs_open_switch_and_close() {
    let mut s = Session::start_full("tabs");
    s.wait_for("PROMPT>");
    s.send("echo first-tab\r");
    s.wait_for("first-tab");
    s.send("\x1bt"); // alt+t
    s.wait_until("second tab", |t| top(t).contains("2 bash"));
    s.wait_until("fresh prompt", |t| {
        !t.contains("first-tab") && t.contains("PROMPT>")
    });
    s.send("\x1b1"); // alt+1
    s.wait_for("first-tab");
    s.send("\x1b2");
    s.wait_until("tab 2 again", |t| !t.contains("first-tab"));
    s.send("exit\r"); // closing the shell closes its tab
    s.wait_until("one tab left", |t| !top(t).contains("2 bash"));
    s.wait_for("first-tab");
}

#[test]
fn split_side_by_side_and_type_into_each() {
    let mut s = Session::start_full("split");
    s.wait_for("PROMPT>");
    s.send("\x1b\\"); // alt+\ → split right
    s.wait_until("two panes", |t| {
        t.lines().nth(2).unwrap_or("").matches('╭').count() == 2
    });
    s.send("echo RIGHT-SIDE\r");
    s.wait_for("RIGHT-SIDE");
    // The right pane starts at column 50 of 100.
    let row = s
        .text()
        .lines()
        .find(|l| l.contains("RIGHT-SIDE") && !l.contains("echo"))
        .unwrap()
        .to_string();
    assert!(row.find("RIGHT-SIDE").unwrap() > 45, "{row:?}");
    s.send("\x1b[1;3D"); // alt+left
    s.send("echo LEFT-SIDE\r");
    s.wait_until("left output", |t| {
        t.lines().any(|l| l.starts_with("│LEFT-SIDE"))
    });
    // The right shell really is 48 columns wide (100/2 minus borders).
    s.send("\x1b[1;3C");
    s.send("stty size\r");
    s.wait_until("stty", |t| t.contains("│19 48 "));
}

#[test]
fn sv_off_and_on_switch_views_without_losing_the_shell() {
    let mut s = Session::start_full("toggle");
    s.wait_for("PROMPT>");
    let mark = format!("▍{}", NAME_LOWER);
    s.send("export MARK=still-here\r");
    s.send("sv off\r");
    s.wait_until("basic view", |t| !t.contains(&mark) && !t.contains('╭'));
    s.send("sv on\r");
    s.wait_for(&mark);
    s.send("\x1b`"); // alt+` → basic
    s.wait_until("basic again", |t| !t.contains(&mark));
    s.send("echo $MARK\r");
    s.wait_for("still-here");
}

#[test]
fn closing_a_busy_tab_asks_first() {
    let mut s = Session::start_full("confirm");
    s.wait_for("PROMPT>");
    s.send("\x1bt");
    s.wait_until("tab 2", |t| top(t).contains("2 bash"));
    s.wait_for("PROMPT>");
    s.send("sleep 300\r");
    s.wait_for("sleep 300");
    s.pump(Duration::from_millis(300));
    s.send("\x1bw"); // alt+w
    s.wait_for("is still running");
    s.send("n");
    s.wait_until("dialog gone", |t| !t.contains("is still running"));
    assert!(
        s.line(1).contains("2 "),
        "tab must survive 'n': {:?}",
        s.line(1)
    );
    // board 12: "Keep in background" takes it off the bar, alt+2 brings it back
    s.send("\x1bw");
    s.wait_for("Keep in background");
    s.send("k");
    s.wait_until("background chip", |t| top(t).contains("◌ 1 in background"));
    s.send("\x1b2");
    s.wait_until("back on the bar", |t| top(t).contains("2 bash · sleep"));
    s.send("\x1bw");
    s.wait_for("is still running");
    s.send("y");
    s.wait_until("tab closed", |t| !top(t).contains("sleep"));
}

#[test]
fn leader_key_and_help() {
    let mut s = Session::start_full("leader");
    s.wait_for("PROMPT>");
    s.send("\x1d"); // ctrl+]
    s.wait_for("ctrl+] →");
    s.send("?");
    let help = format!("{} keys", NAME);
    s.wait_for(&help);
    s.send("x"); // any key closes help…
    s.wait_until("help closed", |t| !t.contains(&help));
    s.send("\x1d-"); // leader + '-' → split stacked
    s.wait_until("stacked split", |t| {
        t.lines().filter(|l| l.starts_with('╭')).count() == 2
    });
}

#[test]
fn rename_tab() {
    let mut s = Session::start_full("rename");
    s.wait_for("PROMPT>");
    s.send("\x1br"); // alt+r
    s.wait_for("Rename tab:");
    s.send("api server\r");
    s.wait_until("renamed", |t| top(t).contains("api server"));
}

#[test]
fn quit_with_running_command_asks() {
    let mut s = Session::start_full("quit");
    s.wait_for("PROMPT>");
    s.send("sleep 300\r");
    s.pump(Duration::from_millis(400));
    s.send("\x11");
    s.wait_for(&format!("Quit {}?", NAME));
    s.send("\x11"); // ctrl+q again = yes
    assert_eq!(s.wait_exit(), 0);
}

// ------------------------------------------------------------------ P1b

#[test]
fn session_is_restored_after_quitting() {
    let mut s = Session::start_with("bash", "restore", "start_view: full\n");
    s.wait_for("PROMPT>");
    s.send("\x1bt"); // second tab…
    s.wait_until("tab 2", |t| top(t).contains("2 bash"));
    s.wait_for("PROMPT>");
    s.send("cd /usr\r"); // …in /usr
    s.wait_until("cd", |t| top(t).contains("2 bash · usr"));
    s.send("\x1b\\"); // split it
    s.wait_until("split", |t| top(t).contains("⊞2"));
    s.send("\x1br"); // and name tab 1 after switching to it
    s.wait_for("Rename tab:");
    s.send("\x1b"); // (cancel)
    s.wait_until("rename cancelled", |t| !t.contains("Rename tab:"));
    s.send("\x1b1"); // go to tab 1…
    s.wait_until("tab 1 active", |t| !t.contains("/usr"));
    s.send("\x1br"); // …and rename it
    s.wait_for("Rename tab:");
    s.send("main\r");
    s.wait_until("renamed", |t| top(t).contains("main"));
    s.pump(Duration::from_millis(1500)); // let the session save
    let saved = std::fs::read_to_string(s.dir.join("state/session.yaml")).unwrap();
    assert!(
        saved.contains("/usr") && saved.contains("side_by_side") && saved.contains("main"),
        "{saved}"
    );
    s.send("\x11");
    assert_eq!(s.wait_exit(), 0);
    s.keep = true;
    let dir = s.dir.clone();
    drop(s);

    let mut s = Session::launch("bash", dir);
    s.wait_for("Restore last session");
    s.wait_for("Restore 2 tabs");
    s.wait_until("one row per tab", |t| {
        t.contains("▣ 1 · main") && t.contains("▣ 2 · /usr")
    });
    s.send("y");
    s.wait_until("two tabs back", |t| {
        top(t).contains("1 bash · main") && top(t).contains("2 bash · usr ⊞2")
    });
    s.send("\x1b2");
    s.wait_until("split restored", |t| {
        t.lines().nth(2).unwrap_or("").matches('╭').count() == 2
    });
    s.send("pwd\r");
    s.wait_for("││/usr");
}

#[test]
fn declining_restore_starts_fresh_and_never_is_silent() {
    let mut s = Session::start_with("bash", "norestore", "start_view: full\n");
    s.wait_for("PROMPT>");
    s.send("\x1bt");
    s.wait_until("tab 2", |t| top(t).contains("2 bash"));
    s.pump(Duration::from_millis(1500));
    s.send("\x11");
    s.wait_exit();
    s.keep = true;
    let dir = s.dir.clone();
    drop(s);

    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("Restore last session");
    s.send("n");
    s.wait_until("one fresh tab", |t| {
        !t.contains("Restore") && !top(t).contains("2 bash")
    });
    s.send("\x11");
    s.wait_exit();
    s.keep = true;
    drop(s);

    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    let mut s = Session::launch("bash", dir);
    s.wait_for("PROMPT>");
    s.pump(Duration::from_millis(500));
    assert!(!s.text().contains("Restore"), "{}", s.text());
}

#[test]
fn f7_rearranges_splits() {
    let mut s = Session::start_full("layout");
    s.wait_for("PROMPT>");
    s.send("\x1b\\"); // split right
    s.wait_until("2", |t| top(t).contains("⊞2"));
    s.send("\x1b-"); // split the right one down
    s.wait_until("3", |t| top(t).contains("⊞3"));
    s.send("\x1b[18~"); // F7 → columns
    s.wait_for("layout: columns");
    s.wait_until("three columns", |t| {
        t.lines().nth(2).unwrap_or("").matches('╭').count() == 3
    });
    s.send("\x1b[18~"); // → rows
    s.wait_for("layout: rows");
    s.wait_until("three rows", |t| {
        t.lines().filter(|l| l.starts_with('╭')).count() == 3
    });
}

#[test]
fn custom_keys_from_config() {
    let mut s = Session::start_with(
        "bash",
        "keys",
        "start_view: full\nkeys:\n  new_tab: alt+n\n  rename_tab: none\n",
    );
    s.wait_for("PROMPT>");
    s.send("\x1bn"); // alt+n is now "new tab"
    s.wait_until("tab 2", |t| top(t).contains("2 bash"));
    s.send("\x1br"); // alt+r goes to the shell instead of renaming
    s.pump(Duration::from_millis(400));
    assert!(!s.text().contains("Rename tab:"));
    s.send("\x1b[11~"); // F1 lists the custom key
    s.wait_for("keys");
    s.wait_until("alt+n in help", |t| {
        t.contains("alt+n") && t.contains("new tab in the same folder")
    });
}

#[test]
fn tab_menu() {
    let mut s = Session::start_full("menu");
    s.wait_for("PROMPT>");
    s.send("\x1bm"); // alt+m
    s.wait_for("Tab menu");
    s.wait_for("Split right");
    s.send("\x1b[B\r"); // down a row: "Split right", enter
    s.wait_until("split", |t| top(t).contains("⊞2"));
    s.send("\x1bm");
    s.wait_for("Duplicate here");
    s.send("7"); // Pin / unpin
    s.wait_for("tab pinned");
    s.wait_until("icon-only", |t| top(t).contains("⚑ 1"));
    s.send("\x1bm");
    s.wait_for("Mute alerts");
    s.send("\x1b"); // esc closes
    s.wait_until("menu closed", |t| !t.contains("Duplicate here"));
}

#[test]
fn option_typing_characters_opens_the_alt_help_once() {
    let mut s = Session::start_full("optionhelp");
    s.wait_for("PROMPT>");
    s.send("¡"); // what macOS Option+1 types when Option isn't Alt
    s.wait_for("Alt keys aren't reaching Shellvisor");
    s.send("t");
    s.wait_for("press alt+1 now");
    s.send("\x1b1");
    s.wait_for("alt works now");
    s.send("\x1b");
    s.wait_until("closed", |t| !t.contains("aren't reaching"));
    s.send("¡"); // only once
    s.pump(Duration::from_millis(300));
    assert!(!s.text().contains("aren't reaching"));
}

// ------------------------------------------------------------------ P2 panes

#[test]
fn files_pane_opens_inserts_paths_and_closes() {
    let mut s = Session::start_full("filespane");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    std::fs::write(work.join("alpha.txt"), "hello-alpha\n").unwrap();
    std::fs::create_dir_all(work.join("beta")).unwrap();
    s.send("\x1bOQ"); // F2
    s.wait_until("files pane", |t| {
        t.contains("▸ beta/") && t.contains("alpha.txt") && t.contains("Tree ")
    });
    s.send("j"); // beta/ → alpha.txt
    s.send("i"); // insert path at the prompt (and focus the shell)
    s.wait_for("PROMPT> alpha.txt");
    s.send("\x01cat \r"); // ctrl+a, prefix `cat `
    s.wait_for("hello-alpha");
    // F2 from the shell focuses the pane; F2 on the pane closes it.
    s.send("\x1bOQ");
    s.pump(Duration::from_millis(200));
    s.send("\x1bOQ");
    s.wait_until("files pane closed", |t| !t.contains("▸ beta/"));
}

#[test]
fn files_pane_follows_cd_and_can_cd_the_shell() {
    let mut s = Session::start_full("filesfollow");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    std::fs::create_dir_all(work.join("sub/deeper")).unwrap();
    std::fs::write(work.join("sub/inner.txt"), "x").unwrap();
    s.send("\x1bOQ");
    s.wait_for("▸ sub/");
    // The pane follows the shell's folder.
    s.send("\x1b[D"); // alt+left would be focus; plain ← goes to the pane: harmless
    s.send("\x1b\x1b[D"); // alt+left: back to the shell
    s.pump(Duration::from_millis(200));
    s.run("cd sub");
    s.wait_until("pane followed", |t| {
        t.contains("inner.txt") && t.contains("▸ deeper/")
    });
    // `c` on a folder cds the shell there.
    s.send("\x1b\x1b[C"); // alt+right: to the pane
    s.pump(Duration::from_millis(200));
    s.send("c"); // deeper/ is the first row
    s.pump(Duration::from_millis(500));
    s.send("pwd\r");
    s.wait_for("sub/deeper");
}

#[test]
fn find_view_searches_names() {
    let mut s = Session::start_full("findview");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    std::fs::create_dir_all(work.join("src/core")).unwrap();
    std::fs::write(work.join("src/core/pricing.py"), "x = 1\n").unwrap();
    s.run("sv files find");
    s.wait_for(" name ");
    s.send("pric");
    s.wait_for("src/core/pricing.py");
}

#[test]
fn dev_layout_shows_system_stats_and_is_restored() {
    let mut s = Session::start_full("devlayout");
    s.wait_for("PROMPT>");
    s.run("sv layout dev");
    s.wait_until("dev layout", |t| {
        t.contains("CPU") && t.contains("Mem") && t.contains("Tree ") && t.contains("Temp")
    });
    s.pump(Duration::from_millis(1500));
    let saved = std::fs::read_to_string(s.dir.join("state/session.yaml")).unwrap();
    assert!(
        saved.contains("tool: files") && saved.contains("tool: system"),
        "{saved}"
    );
    s.send("\x11");
    assert_eq!(s.wait_exit(), 0);
    s.keep = true;
    let dir = s.dir.clone();
    drop(s);

    let mut s = Session::launch("bash", dir);
    s.wait_for("Restore last session");
    s.send("y");
    s.wait_until("panes back", |t| t.contains("CPU") && t.contains("Tree "));
    // F3 on the shell focuses the system pane; F5 zooms it; `m` sorts by memory.
    s.send("\x1bOR");
    s.pump(Duration::from_millis(200));
    s.send("\x1b[15~");
    s.wait_for("PID");
    s.send("m");
    s.wait_for("MEM%▾");
}

#[test]
fn downloads_show_a_loading_circle_and_a_toast() {
    let mut s = Session::start_full("activity");
    s.wait_for("PROMPT>");
    // A stand-in `curl` that prints progress like the real one.
    s.run("curl() { printf 'got 42%%\\n'; sleep 2; }");
    s.send("curl https://example.invalid/file\r");
    s.wait_until("progress bar in the pane title", |t| {
        let title = t.lines().nth(2).unwrap_or("");
        title.contains(" curl ━") && title.contains(" 42%")
    });
    s.wait_for("✔ curl finished");
}

#[test]
fn activity_list_speed_files_mark_and_toast_actions() {
    let mut s = Session::start_full("activity2");
    s.wait_for("PROMPT>");
    // a stand-in `git clone`: makes the folder, prints git's progress line
    s.run(
        "git() { mkdir -p repo; echo hi > repo/inside.txt; \
         printf 'Receiving objects:  42%% (42/100), 1.20 MiB | 3.00 MiB/s\\n'; sleep 4; }",
    );
    s.send("git clone https://example.invalid/me/repo.git\r");
    s.wait_until("bar in the title, the header badge", |t| {
        let title = t.lines().nth(2).unwrap_or("");
        let header = t.lines().next().unwrap_or("");
        title.contains("git clone ━")
            && title.contains(" 42%")
            && header.contains("1 download · 42%")
    });
    // the files pane marks the folder being filled
    s.send("\x1bOQ"); // F2
    s.wait_until("loading mark on repo/", |t| {
        t.lines().any(|l| l.contains("repo/") && l.contains("42%"))
    });
    // the activity list
    s.send("\x1d");
    s.send("j");
    s.wait_until("activity list", |t| {
        t.contains("ACTIVITY · downloads & installs")
            && t.contains("git clone example.invalid/me/repo.git")
            && t.contains("3.00 MiB/s")
            && t.contains("Receiving objects")
    });
    s.send("\x1b");
    s.wait_for("✔ git clone finished · 1.20 MiB in");
    s.wait_for("ctrl+] c cd into repo");
    // ctrl+] c: cd into the clone
    s.send("\x1d");
    s.send("c");
    s.pump(Duration::from_millis(500));
    s.run("pwd");
    s.wait_until("in the clone", |t| t.contains("work/repo"));
    // the list keeps it; o opens it in the files pane
    s.send("\x1d");
    s.send("j");
    s.wait_until("finished card", |t| {
        t.contains("✔") && t.contains("done in") && t.contains("c cd into it · o open in files")
    });
    s.send("o");
    s.wait_until("files pane shows the clone", |t| t.contains("inside.txt"));
}

#[test]
fn downloads_pause_cancel_and_history() {
    let dir = scratch("dlpause");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    // a stand-in wget that prints its progress slowly
    std::fs::create_dir_all(dir.join("home/bin")).unwrap();
    let fake = dir.join("home/bin/wget");
    std::fs::write(
        &fake,
        "#!/bin/sh\ni=1\nwhile [ $i -le 200 ]; do echo \"big.iso  $i%  1.0M/s\"; i=$((i+1)); sleep 0.25; done\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send("wget https://example.invalid/big.iso\r");
    s.wait_until("downloading", |t| {
        t.lines().next().unwrap_or("").contains("1 download")
    });
    s.send("\x1d");
    s.send("j");
    s.wait_for("ACTIVITY · downloads & installs");
    // p pauses: the output stops
    s.send("p");
    s.wait_for("paused · p resumes · x cancels");
    s.pump(Duration::from_millis(600));
    s.send("\x1b");
    s.pump(Duration::from_millis(300));
    let before = s.text();
    s.pump(Duration::from_millis(1200));
    let pct = |t: &str| {
        t.lines()
            .filter_map(|l| l.split("big.iso  ").nth(1))
            .filter_map(|r| r.split('%').next()?.trim().parse::<u32>().ok())
            .max()
            .unwrap_or(0)
    };
    assert_eq!(pct(&before), pct(&s.text()), "nothing printed while paused");
    // p again resumes it
    s.send("\x1d");
    s.send("j");
    s.wait_for("ACTIVITY · downloads & installs");
    s.send("p");
    s.wait_for("resumed");
    s.send("\x1b");
    let at = pct(&s.text());
    s.wait_until("printing again", |t| pct(t) > at + 2);
    // x cancels it
    s.send("\x1d");
    s.send("j");
    s.wait_for("ACTIVITY · downloads & installs");
    s.send("x");
    s.wait_for("cancelled");
    s.send("\x1b");
    s.wait_until("cancelled with ctrl+C's code", |t| t.contains("✖ 130"));
    let hist = std::fs::read_to_string(dir.join("state/downloads.jsonl")).unwrap_or_default();
    assert!(hist.contains("wget"), "remembered: {hist}");
    s.keep = true;
    drop(s);
    // a new session lists it under "earlier"
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send("\x1d");
    s.send("j");
    s.wait_until("earlier downloads", |t| {
        t.contains("earlier ·") && t.contains("wget example.invalid/big.iso")
    });
}

#[test]
fn first_run_tour_once() {
    let dir = scratch("tour");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    std::fs::write(dir.join("tour"), "").unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_until("tour card 1", |t| {
        t.contains("Welcome to Shellvisor") && t.contains("1 of 4")
    });
    s.send("x");
    s.wait_until("card 2", |t| {
        t.contains("command palette") && t.contains("2 of 4")
    });
    s.send("\x1b[D");
    s.wait_for("1 of 4");
    s.send("\x1b");
    s.wait_until("tour closed", |t| {
        !t.contains("1 of 4") && t.contains("PROMPT>")
    });
    assert_eq!(
        std::fs::read_to_string(dir.join("state/tour.done"))
            .unwrap()
            .trim(),
        "1"
    );
    s.keep = true;
    drop(s);
    // not again on the next start; sv tour brings it back
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.pump(Duration::from_millis(400));
    assert!(!s.text().contains("Welcome to Shellvisor"));
    s.send("sv tour\r");
    s.wait_for("Welcome to Shellvisor");
    for _ in 0..4 {
        s.send(" ");
        s.pump(Duration::from_millis(150));
    }
    s.wait_until("tour done", |t| !t.contains("of 4"));
}

const FAKE_SSH_TUNNEL: &str = r#"#!/usr/bin/env python3
import sys, socket, threading, time
a = sys.argv[1:]
def pipe(x, y):
    try:
        while True:
            d = x.recv(65536)
            if not d:
                break
            y.sendall(d)
    except Exception:
        pass
    try:
        y.shutdown(socket.SHUT_WR)
    except Exception:
        pass
if '-N' in a:
    spec = None
    for i, x in enumerate(a):
        if x in ('-L', '-D', '-R'):
            spec = (x, a[i + 1])
    if spec and spec[0] == '-L':
        bind, p2, h, p = spec[1].split(':')
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((bind, int(p2)))
        s.listen(5)
        while True:
            c, _ = s.accept()
            f = socket.create_connection((h, int(p)))
            threading.Thread(target=pipe, args=(c, f), daemon=True).start()
            threading.Thread(target=pipe, args=(f, c), daemon=True).start()
    time.sleep(3600)
else:
    print('fake-ssh ' + ' '.join(a), flush=True)
    time.sleep(3600)
"#;

#[test]
fn port_forwards_board_20() {
    use std::io::{Read, Write};
    let dir = scratch("fwd");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("home/bin")).unwrap();
    let fake = dir.join("home/bin/ssh");
    std::fs::write(&fake, FAKE_SSH_TUNNEL).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // the service on the far side: echoes what it gets
    let echo = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ep = echo.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in echo.incoming().flatten() {
            std::thread::spawn(move || {
                let mut c = c;
                let mut b = [0u8; 64];
                while let Ok(n) = c.read(&mut b) {
                    if n == 0 || c.write_all(&b[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let free = || {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let up = free();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.run(&format!(
        "sv ssh save lab me@lab.example -L {up}:127.0.0.1:{ep}"
    ));
    s.wait_for("saved lab");
    // opening the session starts its forward (Shellvisor's own ssh -N)
    s.send("sv ssh lab\r");
    s.wait_for("fake-ssh -o ServerAliveInterval=30 me@lab.example");
    s.send("\x1d");
    s.send("P");
    s.wait_until("board 20", |t| {
        t.contains("ssh · port forwards")
            && t.contains("lab · FORWARDS")
            && t.contains(&format!("localhost:{up}"))
            && t.contains("● listening")
            && t.contains("connected")
    });
    // a connection through it: relayed and counted
    let mut c = None;
    for _ in 0..50 {
        if let Ok(mut x) = std::net::TcpStream::connect(("127.0.0.1", up)) {
            x.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            if x.write_all(b"ping").is_ok() {
                let mut b = [0u8; 4];
                if x.read_exact(&mut b).is_ok() && &b == b"ping" {
                    c = Some(x);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(c.is_some(), "the forward relays to the far side");
    s.wait_until("counted", |t| t.contains("4B / 4B"));
    drop(c);
    // a busy port: shown, and moved to the next free one
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let busy = held.local_addr().unwrap().port();
    s.send("a");
    s.wait_for("Add a forward");
    s.send("\r"); // type: Local
    s.send(&format!("localhost:{busy}"));
    s.send("\r");
    s.send(&format!("127.0.0.1:{ep}"));
    s.send("\r");
    s.send("\x1b[B");
    s.send("\x1b[B");
    s.send("\r");
    s.wait_until("port busy", |t| {
        t.contains("✖ port busy") && t.contains(&format!("{busy} is used by"))
    });
    s.send("j");
    s.send("p");
    s.wait_until("moved", |t| t.contains("moved to port"));
    let saved = std::fs::read_to_string(dir.join("home/.config/shellvisor/sessions.yaml"))
        .or_else(|_| std::fs::read_to_string(dir.join("sessions.yaml")))
        .unwrap_or_default();
    assert!(
        saved.contains(&format!("-L {up}:127.0.0.1:{ep}")),
        "{saved}"
    );
    drop(held);
    s.send("\x1b");
    s.wait_until("closed", |t| !t.contains("lab · FORWARDS"));
}

// ------------------------------------------------------------------ P3 data

#[test]
fn sv_data_pipelines_in_bash() {
    let mut s = Session::start_full("svdata");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    std::fs::write(work.join("big.bin"), vec![0u8; 3_000_000]).unwrap();
    std::fs::write(work.join("small.txt"), "hi").unwrap();
    std::fs::write(work.join("people.csv"), "name,age\nAda,36\nLin,29\nBo,41\n").unwrap();
    s.run("sv ls | sv where size gt 1mb | sv select name size");
    s.wait_for("│ 0 │ big.bin │ 3.0 MB │");
    assert!(!s.text().contains("small.txt"), "{}", s.text());
    s.run("sv 'open people.csv | where age ge 30 | sort-by age -r | get name | to csv'");
    s.wait_for("Bo");
    s.run("sv open people.csv | sv avg age | cat");
    s.wait_for("35.33");
}

#[test]
fn f4_explores_the_last_table() {
    let mut s = Session::start_full("tableview");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    for (n, size) in [("alpha.txt", 10), ("beta.txt", 3000), ("gamma.txt", 200)] {
        std::fs::write(work.join(n), vec![b'x'; size]).unwrap();
    }
    s.run("sv ls");
    s.wait_for("F4 explores this table");
    s.send("\x1bOS"); // F4
    s.wait_for("3 of 3 rows");
    s.send("l"); // type column
    s.send("l"); // size column
    s.send("S"); // sort descending
    s.wait_for("sorted by size ↓");
    s.send("c"); // copy as CSV (no clipboard tool here → OSC 52)
    s.wait_for("copied 4 lines as CSV");
    s.send("h");
    s.send("h"); // back to name
    s.send("\r"); // insert the selected cell (beta.txt, the biggest) at the prompt
    s.wait_for("PROMPT> beta.txt");
}

#[test]
fn command_bar_runs_a_pipeline_into_a_table() {
    let mut s = Session::start_full("cmdbar");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    std::fs::write(work.join("keep-me.log"), "x").unwrap();
    std::fs::write(work.join("other.txt"), "x").unwrap();
    s.send("\x1b;"); // alt+;
    s.wait_for("sv ▸");
    s.send("ls | where name =~ keep");
    s.send("\r");
    s.wait_for("1 of 1 rows");
    assert!(!s.text().contains("other.txt"), "{}", s.text());
    s.send("q");
    s.wait_until("table closed", |t| !t.contains("1 of 1 rows"));
    // a scalar result is shown as a notice
    s.send("\x1b;");
    s.send("ls | count\r");
    s.wait_for("ls | count = 2");
}

#[test]
fn sv_flow_meters_a_pipeline_and_the_flow_pane_shows_it() {
    let mut s = Session::start_full("flow");
    s.wait_for("PROMPT>");
    s.send("\x1b[19~"); // F8: flow pane
    s.wait_for("Nothing running");
    s.send("\x1b\x1b[D"); // alt+left: back to the shell
    s.pump(Duration::from_millis(200));
    // live job: two processes joined by a pipe
    s.send("sleep 3 | cat\r");
    s.wait_until("job in the flow pane", |t| {
        // pipe links come from /proc: Linux only
        t.contains("sleep 3") && (t.contains("│ pipe") || !cfg!(target_os = "linux"))
    });
    s.wait_for("Nothing running");
    // metered pipeline
    s.run("sv flow 'seq 1 20000 | grep 7 | wc -l'");
    s.wait_for("flow · 3 stages");
    s.wait_until("stage meters in the pane", |t| {
        t.contains("finished") && t.contains("20.0k → 6878 lines · 34% kept")
    });
    // a saved pipeline runs by name
    s.run("sv flow save sevens 'seq 1 2000 | grep 7 | wc -l'");
    s.wait_for("saved sevens");
    s.run("sv flow list");
    s.wait_for("sevens           seq 1 2000");
    s.send("sv flow sevens\r");
    s.wait_until("ran by name", |t| t.contains("2000 → 542 lines"));
}

// ------------------------------------------------------------------ P4 agents

fn git_repo(dir: &std::path::Path) {
    let g = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
    };
    g(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# demo\n").unwrap();
    g(&["add", "."]);
    g(&[
        "-c",
        "user.email=a@b",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
}

#[test]
fn agent_runs_in_a_worktree_asks_for_approval_and_takes_messages() {
    let mut s = Session::start_full("agent");
    s.wait_for("PROMPT>");
    let work = s.dir.join("work");
    git_repo(&work);
    let fake = s.dir.join("fake-agent.sh");
    std::fs::write(
        &fake,
        "#!/bin/bash\necho \"fake agent: $*\"\necho 'x = 1' > app.py\necho more >> README.md\nsleep 0.3\nprintf 'Install the test dependency? (y/n) '\nread ans\necho \"you said: $ans\"\nread -p '> ' msg\necho \"got message: $msg\"\n",
    )
    .unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(&fake)
        .status()
        .unwrap();
    s.send(&format!(
        "sv agent new --name api --cmd '{} go'\r",
        fake.display()
    ));
    s.wait_until("agent column", |t| {
        top(t).contains("2 agent · api") && t.contains("● api")
    });
    s.wait_for("fake agent: go");
    s.wait_for("agent/api"); // its own branch
    s.wait_for("⚑"); // asks for approval
    s.send("\x1ba"); // alt+a: the approvals list
    s.wait_for("waiting for approval");
    s.send("y");
    s.wait_for("you said: y");
    s.wait_for("2 files +2"); // git sees its changes (in its worktree)
    assert!(work.join(".shellvisor/worktrees/api/app.py").exists());
    assert!(
        !work.join("app.py").exists(),
        "the main checkout is untouched"
    );
    s.send("hello\r"); // typing goes to the agent
    s.wait_for("got message: hello");
    s.wait_for("agent api finished");
    // back in the first tab: the agent's work shows up in files and in sv agents
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("\x1bOQ"); // F2 files (main checkout)
    s.wait_until("badge", |t| t.contains("README.md") && t.contains("◆ api"));
    s.send("\x1b\x1b[D"); // alt+left: to the shell
    s.pump(Duration::from_millis(200));
    s.send("sv agents | sv select name status branch files\r");
    s.wait_until("agents table", |t| {
        t.contains("agent/api") && t.contains("done")
    });
    s.send("sv connect | sv get kind | sv uniq\r");
    s.wait_for("agent cli");
    // board 21: review its work before merging
    s.send("clear\r");
    s.pump(Duration::from_millis(200));
    s.send("sv agent diff api\r");
    s.wait_until("review board", |t| {
        t.contains("review · ● api · agent/api → main")
            && t.contains("FILES")
            && t.contains("app.py")
            && t.contains("README.md")
            && t.contains("not committed yet")
            && t.contains("Approve for api")
            && t.contains("m Merge into main")
    });
    // the diff of the selected file, then the next file
    s.wait_until("a diff", |t| t.contains("+x = 1") || t.contains("+more"));
    s.send("v");
    s.wait_until("side by side", |t| t.contains("side by side"));
    // approve for a while: on, then off
    s.send("A");
    s.send("\r");
    s.wait_until("approving", |t| t.contains("● on") && t.contains("ends"));
    s.send("A");
    s.send("\r");
    s.wait_until("asks again", |t| t.contains("to turn on"));
    s.send("\x1b");
    s.send("\x1b");
    s.wait_until("review closed", |t| !t.contains("Approve for api"));
}

#[test]
fn agents_spawn_sub_agents_within_limits_wait_and_stop_as_a_tree() {
    let mut s = Session::start_with(
        "bash",
        "team",
        "start_view: full\nagents:\n  limits:\n    max_children: 1\n",
    );
    s.wait_for("PROMPT>");
    let helper = s.dir.join("helper.sh");
    std::fs::write(
        &helper,
        "#!/bin/bash\necho \"helper working on: $*\"\nsleep 0.5\necho 'helper finished'\nread -p '> ' msg\necho \"helper got: $msg\"\nread -p '> ' msg\necho \"helper again: $msg\"\nread -p '> ' msg\n",
    )
    .unwrap();
    let lead = s.dir.join("lead.sh");
    std::fs::write(
        &lead,
        format!(
            "#!/bin/bash\necho \"lead as $SHELLVISOR_AGENT\"\nsv agent spawn --name helper --cmd '{h} tests'\n\
             sv agent spawn --name extra --cmd '{h} more' 2>&1 || echo \"second spawn exit $?\"\n\
             sv agent wait helper\necho 'lead saw helper finish'\nread -p '> ' msg\n",
            h = helper.display()
        ),
    )
    .unwrap();
    for f in [&helper, &lead] {
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(f)
            .status()
            .unwrap();
    }
    s.send(&format!(
        "sv agent new --name lead --cmd '{}'\r",
        lead.display()
    ));
    s.wait_for("lead as lead");
    s.wait_for("helper working on: tests");
    s.wait_for("● helper ← lead");
    s.wait_for("refused: lead already has 1 sub-agents");
    s.wait_for("second spawn exit 3");
    s.wait_for("lead saw helper finish");
    // from the plain shell in the first tab
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent tree\r");
    s.wait_until("tree", |t| {
        t.contains("lead  command") && t.contains("└─ helper  command")
    });
    s.send("sv agent send helper 'run the slow ones'\r");
    s.send("\x1b2");
    s.wait_for("helper got: run the slow ones");
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent log\r");
    s.wait_until("bus log", |t| {
        t.contains("tried to spawn extra") && t.contains("you → helper: run the slow ones")
    });
    s.send("clear; sv agent pause helper\r");
    s.wait_for("paused helper");
    s.send("\x1b2");
    s.wait_for("⏸ paused");
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent resume helper; sv agent send helper still-there\r");
    s.wait_for("resumed helper");
    s.send("\x1b2");
    s.wait_for("helper again: still-there");
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent pause helper; sv agent stop lead --tree\r");
    s.wait_for("paused helper");
    s.wait_for("stopped lead, helper");
    s.send("clear; sv agent wait nobody; echo \"wait exit $?\"\r");
    s.wait_for("no agent called nobody");
    s.wait_for("wait exit 1");
}

#[test]
fn a_team_yaml_runs_end_to_end_with_rules_and_a_budget_cap() {
    let mut s = Session::start_full("teamrun");
    s.wait_for("PROMPT>");
    let d = s.dir.clone();
    let script = |name: &str, body: &str| {
        let p = d.join(name);
        std::fs::write(&p, format!("#!/bin/bash\n{body}")).unwrap();
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(&p)
            .status()
            .unwrap();
    };
    script(
        "lead.sh",
        "echo \"lead task: $SHELLVISOR_TASK\"\nsv agent spawn --name api 'build the alerts endpoint'\n\
         sv agent spawn --name db 'x' 2>&1 || echo \"db refused $?\"\nread -p '> ' x\n",
    );
    script(
        "api.sh",
        "echo \"api task: $SHELLVISOR_TASK\"\necho 'api built it'\nread -p '> ' msg\necho \"api got: $msg\"\nread -p '> ' x\n",
    );
    script(
        "tests.sh",
        "echo 'tests running'\nsv agent emit failed '2 tests failed in test_alerts.py'\nsv agent usage 6000\nread -p '> ' x\n",
    );
    let team = format!(
        "team:\n  name: price-alerts\n  budget_tokens: 5000\n  max_agents: 5\n  max_depth: 3\n\
         agents:\n  - id: lead\n    cmd: {d}/lead.sh\n    role: orchestrator\n    can_spawn: [api]\n\
         \x20 - id: api\n    cmd: {d}/api.sh\n    max_children: 1\n    approve: [install]\n\
         \x20 - id: tests\n    cmd: {d}/tests.sh\n    start: after:api\n\
         on:\n  - event: tests.failed\n    send: api\n    text: \"fix failing tests: {{log}}\"\n",
        d = d.display()
    );
    std::fs::write(d.join("team.yaml"), &team).unwrap();
    std::fs::write(
        d.join("bad.yaml"),
        team.replace("budget_tokens: 5000", "budget_usd: 5.0"),
    )
    .unwrap();
    s.send(&format!("cd {}; sv agent run bad.yaml 'go'\r", d.display()));
    s.wait_for("Shellvisor sees tokens, not prices");
    s.send("clear; sv agent run team.yaml 'add price alerts' --dry-run\r");
    s.wait_until("plan", |t| {
        t.contains("api") && t.contains("started by lead") && t.contains("starts when api is done")
    });
    s.send("clear; sv agent run team.yaml 'add price alerts'\r");
    s.wait_for("lead task: add price alerts");
    s.wait_for("api task: build the");
    s.wait_for("db refused 3");
    s.wait_for("tests running"); // started once api was done
    s.wait_for("api got: fix failing tests");
    s.wait_for("⏸ paused"); // over budget: the whole team pauses
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent tree\r");
    s.wait_until("tree", |t| {
        t.contains("team price-alerts · 6K tok of 5K · ⏸ over budget") && t.contains("└─ api")
    });
    s.send("clear; sv agent log | tail -5; sv agent budget 20000\r");
    s.wait_for("reached its budget");
    s.wait_until("resumed", |t| t.contains("resumed") && t.contains("tests"));
    s.send("clear; sv agent stop --all\r");
    s.wait_for("stopped");
    s.send("clear; sleep 1; sv agent log | tail -3\r");
    s.wait_for("team price-alerts finished");
}

#[test]
fn auto_plan_dispatches_stages_then_review_and_merges_from_the_tree() {
    let dir = scratch("plan");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git_repo(&work);
    for (k, v) in [("user.email", "a@b"), ("user.name", "t")] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["config", k, v])
            .status()
            .unwrap();
    }
    let worker = dir.join("worker.sh");
    std::fs::write(
        &worker,
        "#!/bin/bash\necho \"worker $SHELLVISOR_AGENT: ${SHELLVISOR_TASK:0:30}\"\n\
         echo \"$SHELLVISOR_AGENT\" > \"$SHELLVISOR_AGENT.txt\"\ngit add -A >/dev/null 2>&1\n\
         git commit -qm \"$SHELLVISOR_AGENT work\" >/dev/null 2>&1\necho \"$SHELLVISOR_AGENT finished\"\nread -p '> ' x\n",
    )
    .unwrap();
    let planner = dir.join("planner.sh");
    std::fs::write(
        &planner,
        format!(
            "#!/bin/bash\ngrep -q 'ONLY a JSON object' || exit 3\nsleep 0.3\necho 'Plan:'\n\
             echo '{{\"tasks\": [{{\"id\": \"api\", \"title\": \"Add POST /alerts\", \"scope\": \"src/api/\", \"agent\": \"{w}\"}}, \
             {{\"id\": \"ui\", \"title\": \"Alert form\", \"scope\": \"src/ui/\", \"agent\": \"{w}\"}}, \
             {{\"id\": \"tests\", \"title\": \"Tests for alerts\", \"deps\": [\"api\", \"ui\"], \"agent\": \"{w}\"}}]}}'\n",
            w = worker.display()
        ),
    )
    .unwrap();
    for f in [&worker, &planner] {
        std::process::Command::new("chmod")
            .arg("+x")
            .arg(f)
            .status()
            .unwrap();
    }
    std::fs::write(
        dir.join("config.yaml"),
        format!(
            "start_view: full\nagents:\n  planner: {}\n",
            planner.display()
        ),
    )
    .unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send(&format!(
        "cd {}; sv agent plan 'add price alerts'\r",
        work.display()
    ));
    s.wait_for("agent board");
    s.wait_for("Proposed plan · 3 tasks · 2 stages");
    s.wait_for("stage 1 · 2 in parallel");
    s.send("e\x15Build the API\r"); // edit the first task's title
    s.wait_for("Build the API");
    s.send("\r"); // dispatch
    s.wait_for("Running · 3 tasks");
    s.wait_until("stage 1 done, tests and review ran", |t| {
        t.contains("tests") && t.matches("done").count() >= 4
    });
    s.send("\t"); // tree
    s.wait_until("tree", |t| {
        t.contains("◆ review") && t.contains("branch") && t.contains("agents")
    });
    s.send("\t"); // timeline
    s.wait_for("now");
    s.send("\t"); // log
    s.wait_until("log", |t| {
        t.contains("team plan-") && t.contains("started: api, ui")
    });
    s.send("\t\t"); // plan → tree
    s.wait_for("◆ review");
    s.send("m"); // merge the first agent's branch (api)
    s.wait_for("merge agent/api into your branch? y/n");
    s.send("y");
    s.wait_for("merged agent/api");
    assert!(
        work.join("api.txt").exists(),
        "api's work is in the main checkout"
    );
    s.send("\x1b"); // close the board
    s.pump(Duration::from_millis(300));
    s.send("\x1b1");
    s.pump(Duration::from_millis(300));
    s.send("clear; sv agent log | grep -c 'tests.done'\r");
    s.wait_for("1");
}

#[test]
fn layout_editor_builds_saves_and_reopens_a_folder_layout() {
    let mut s = Session::start_with("bash", "layout", "start_view: full\nrestore: never\n");
    s.wait_for("PROMPT>");
    s.send("\x1be"); // alt+e
    s.wait_until("editor screen", |t| {
        t.contains("Edit layout") && t.contains("EDITING") && t.contains("1 dev")
    });
    s.send("\\"); // add Files (the palette's pick) to the right
    s.wait_for("new");
    s.send("]]-"); // System stats, below it
    s.wait_until("system pane added", |t| {
        t.contains("System stats") && t.contains("save as:")
    });
    s.send(">>f"); // resize, open it in this folder
    s.send("n");
    s.send("code\r");
    s.send("\r"); // save & use
    s.wait_for("saved layout code");
    s.wait_until("applied", |t| {
        t.contains("F2 · FILES") && t.contains("F3 · SYSTEM")
    });
    let saved = std::fs::read_to_string(s.dir.join("layouts/code.yaml")).unwrap();
    assert!(
        saved.contains("tool: files") && saved.contains("tool: system"),
        "{saved}"
    );
    let prefs = std::fs::read_to_string(s.dir.join("state/layouts.yaml")).unwrap();
    assert!(prefs.contains("code"), "{prefs}");
    s.pump(Duration::from_millis(300));
    s.send("\x11");
    s.wait_exit();
    s.keep = true;
    let dir = s.dir.clone();
    drop(s);
    // starting in that folder again opens the layout
    let mut s = Session::launch("bash", dir);
    s.wait_until("folder layout", |t| {
        t.contains("F2 · FILES") && t.contains("F3 · SYSTEM")
    });
    s.wait_for("PROMPT>");
    // and by name from any shell
    s.send("sv layout nope\r");
    s.wait_for("no saved layout called nope");
}

#[test]
fn settings_colour_the_prompt_folder_and_ls_prints_a_table() {
    for shell in ["bash", "zsh"] {
        if !have(shell) {
            continue;
        }
        let dir = scratch(&format!("settings-{shell}"));
        std::fs::write(dir.join("home/.bash_profile"), "PS1='\\u \\W > '\n").unwrap();
        std::fs::write(
            dir.join("home/.zshrc"),
            "PROMPT='%n %1~ > '\nunsetopt BEEP\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config.yaml"),
            "start_view: full\nrestore: never\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("work/src")).unwrap();
        std::fs::write(dir.join("work/README.md"), "# hi\n").unwrap();
        let mut s = Session::launch(shell, dir.clone());
        s.wait_for(" work > ");
        // ls: a nushell-style table; with other flags, the real ls
        s.send("ls\r");
        s.wait_until("ls table", |t| {
            t.contains("╭───┬") && t.contains("│ # │ name") && t.contains("README.md")
        });
        s.send("clear; ls -l\r");
        s.wait_for("total");
        // settings screen: colour the prompt's folder
        s.send("\x1bs");
        s.wait_for("Folder colour in the prompt");
        s.send("\x1b[C"); // cyan
        s.wait_until("preview", |t| t.contains("% ls"));
        s.send("\x1b");
        s.pump(Duration::from_millis(200));
        s.send("\r");
        s.pump(Duration::from_millis(800));
        let saved = std::fs::read_to_string(dir.join("state/settings.yaml")).unwrap();
        assert!(saved.contains("prompt_folder: cyan"), "{saved}");
        // the folder name in the newest prompt is drawn in cyan (colour 6)
        let screen = s.screen.screen();
        let (rows, cols) = screen.size();
        let mut found = false;
        for r in (0..rows).rev() {
            let line: String = (0..cols)
                .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
                .collect();
            if let Some(i) = line.find(" work > ") {
                let col = line[..i + 1].chars().count() as u16;
                let cell = screen.cell(r, col).unwrap();
                found = cell.fgcolor() == vt100::Color::Idx(6);
                break;
            }
        }
        assert!(found, "{shell}: prompt folder not coloured:\n{}", s.text());
        // what you type is blue (12); the output is not
        let colour_of = |s: &Session, needle: &str| -> Option<vt100::Color> {
            let screen = s.screen.screen();
            let (rows, cols) = screen.size();
            for r in (0..rows).rev() {
                let line: String = (0..cols)
                    .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
                    .collect();
                if let Some(i) = line.find(needle) {
                    let col = line[..i].chars().count() as u16;
                    return screen.cell(r, col).map(|x| x.fgcolor());
                }
            }
            None
        };
        s.send("clear\r");
        s.pump(Duration::from_millis(300));
        s.send("echo typed-$((40+2))");
        s.wait_for("echo typed-");
        s.pump(Duration::from_millis(200));
        assert_eq!(
            colour_of(&s, "echo typed-"),
            Some(vt100::Color::Idx(12)),
            "{shell}: typed command colour:\n{}",
            s.text()
        );
        s.send("\r");
        s.wait_for("typed-42");
        s.pump(Duration::from_millis(300));
        let out = colour_of(&s, "typed-42");
        assert!(
            out.is_some() && out != Some(vt100::Color::Idx(12)),
            "{shell}: output must not keep the typed colour: {out:?}\n{}",
            s.text()
        );
        // turning the table off gives the plain ls back
        let mut st = std::fs::read_to_string(dir.join("state/settings.yaml")).unwrap();
        st = st.replace("ls_table: true", "ls_table: false");
        std::fs::write(dir.join("state/settings.yaml"), st).unwrap();
        std::fs::write(dir.join("state/ui.env"), "37\n0\n0\n").unwrap();
        s.send("clear\r");
        s.pump(Duration::from_millis(300));
        s.send("ls\r");
        s.pump(Duration::from_millis(500));
        assert!(
            !s.text().contains("╭───┬") && s.text().contains("README.md"),
            "{shell}:\n{}",
            s.text()
        );
    }
}

#[test]
fn mouse_selects_copies_and_right_click_menu() {
    let mut s = Session::start_full("select");
    s.wait_for("PROMPT>");
    s.run("echo 'pick-me and https://example.org/page more'");
    s.wait_for("pick-me and https");
    s.pump(Duration::from_millis(300));
    // screen position (1-based, as the mouse reports it) of some text
    let find = |s: &Session, needle: &str| -> (u16, u16) {
        let screen = s.screen.screen();
        let (rows, cols) = screen.size();
        for r in (0..rows).rev() {
            let line: String = (0..cols)
                .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
                .collect();
            if let Some(i) = line.find(needle) {
                if !line.contains("echo") {
                    return (line[..i].chars().count() as u16 + 1, r + 1);
                }
            }
        }
        panic!("{needle} not on screen:\n{}", s.text());
    };
    let (x, y) = find(&s, "pick-me");
    // drag over "pick-me" and release: copied
    s.send(&format!("\x1b[<0;{x};{y}M"));
    s.send(&format!("\x1b[<32;{};{y}M", x + 3));
    s.send(&format!("\x1b[<0;{};{y}m", x + 6));
    s.wait_for("copied 7 characters");
    // the selection is highlighted (not the default background)
    let cell = s.screen.screen().cell(y - 1, x - 1).unwrap().bgcolor();
    assert_ne!(cell, vt100::Color::Default, "selected text is highlighted");
    // double-click a word
    let (wx, wy) = find(&s, "more");
    for _ in 0..2 {
        s.send(&format!("\x1b[<0;{wx};{wy}M"));
        s.send(&format!("\x1b[<0;{wx};{wy}m"));
    }
    s.wait_for("copied 4 characters");
    s.pump(Duration::from_millis(600)); // a new click sequence
    // triple-click: the whole line
    for _ in 0..3 {
        s.send(&format!("\x1b[<0;{wx};{wy}M"));
        s.send(&format!("\x1b[<0;{wx};{wy}m"));
    }
    s.wait_for("copied 41 characters");
    // right-click on the link: the menu, with Open link
    let (lx, ly) = find(&s, "example.org");
    s.send(&format!("\x1b[<2;{lx};{ly}M"));
    s.wait_until("context menu", |t| {
        t.contains("Copy")
            && t.contains("Paste")
            && t.contains("Open link")
            && t.contains("Split stacked")
    });
    s.send("\x1b");
    s.wait_until("menu closed", |t| !t.contains("Split stacked"));
    // typing still goes to the shell
    s.run("echo still-typing");
    s.wait_for("still-typing");
}

#[test]
fn sv_resize_moves_the_border_and_sets_a_share() {
    let mut s = Session::start_full("resize");
    s.wait_for("PROMPT>");
    // the column of the second pane's top-left corner on the top border row
    let divider = |s: &Session| -> usize {
        let line = s.text().lines().nth(2).unwrap_or("").to_string();
        line.chars()
            .skip(1)
            .position(|c| c == '╭')
            .map(|i| i + 1)
            .unwrap_or(0)
    };
    s.send("\x1b\\"); // alt+\ : split side by side, focus goes to the new (right) pane
    s.wait_until("two panes", |t| {
        t.lines().nth(2).unwrap_or("").matches('╭').count() == 2
    });
    s.wait_until("prompt in the new pane", |t| {
        t.matches("PROMPT>").count() >= 2
    });
    let half = divider(&s);
    s.run("sv resize 30%");
    s.wait_until("right pane at 30%", |t| {
        let line = t.lines().nth(2).unwrap_or("").to_string();
        let d = line
            .chars()
            .skip(1)
            .position(|c| c == '╭')
            .map(|i| i + 1)
            .unwrap_or(0);
        d > half + 10
    });
    let thirty = divider(&s);
    s.run("sv resize left 2");
    s.wait_until("right pane grew by 10%", |t| {
        let line = t.lines().nth(2).unwrap_or("").to_string();
        let d = line
            .chars()
            .skip(1)
            .position(|c| c == '╭')
            .map(|i| i + 1)
            .unwrap_or(0);
        d + 5 < thirty
    });
    s.run("sv resize sideways");
    s.wait_for("sv resize left|right|up|down");
}

#[test]
fn mouse_setting_gives_the_mouse_back_to_the_terminal() {
    let mut s = Session::start_full("mousemode");
    s.wait_for("PROMPT>");
    // Shellvisor asks the terminal for clicks and drags
    s.pump(Duration::from_millis(300));
    assert_eq!(
        s.screen.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::ButtonMotion
    );
    // settings → Mouse: terminal
    s.send("\x1bs");
    s.wait_for("Folder colour in the prompt");
    for _ in 0..12 {
        s.send("\x1b[B");
    }
    s.send("\r");
    s.wait_for("terminal · its own selection");
    s.send("\x1b");
    s.pump(Duration::from_millis(400));
    assert_eq!(
        s.screen.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None,
        "the terminal has the mouse again"
    );
    let saved = std::fs::read_to_string(s.dir.join("state/settings.yaml")).unwrap();
    assert!(saved.contains("mouse: terminal"), "{saved}");
}

#[test]
fn find_clear_hints_and_option_click() {
    if !have("zsh") {
        return;
    }
    let dir = scratch("findclear");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    let mut s = Session::launch("zsh", dir.clone());
    s.wait_for("PROMPT>");
    s.send("echo needle-old; seq 1 120\r");
    s.wait_until("output done, needle scrolled off", |t| {
        t.contains("120") && t.contains("PROMPT>") && !t.contains("needle-old")
    });
    // ctrl+] / : find in the pane's scrollback
    s.send("\x1d");
    s.send("/");
    s.wait_for("type to search");
    s.send("needle");
    s.wait_until("found and scrolled to", |t| {
        t.contains("1 of 2") && t.contains("needle-old")
    });
    // ctrl+r: a regular expression; ⏎ then n / N step through the matches
    s.send("\x12");
    s.send("\x15");
    s.send("^1[0-2]0$");
    s.wait_until("regex matches", |t| {
        t.contains(".*") && t.contains("1 of 3")
    });
    s.send("\r");
    s.wait_for("2 of 3");
    s.send("n");
    s.wait_for("3 of 3");
    s.send("N");
    s.wait_for("2 of 3");
    s.send("/");
    s.send("(");
    s.wait_for("✖");
    s.send("\x1b");
    s.wait_until("find closed", |t| !t.contains("of 3") && !t.contains("✖"));
    // ctrl+] k : clear the pane and its scrollback
    s.send("\x1d");
    s.send("k");
    s.pump(Duration::from_millis(500));
    s.send("\x1d");
    s.send("/");
    s.send("needle");
    s.wait_for("no matches");
    s.send("\x1b");
    s.wait_until("find closed", |t| !t.contains("no matches"));
    // an unclosed quote: the hint on the pane's border
    s.send("echo \"abc\r");
    s.wait_for("unclosed quote · type it, or ctrl+C to cancel");
    s.send("\x03");
    s.wait_until("hint gone", |t| !t.contains("unclosed quote"));
    // less: how to quit
    std::fs::write(dir.join("work/notes.txt"), "hello\n").unwrap();
    s.send("less notes.txt\r");
    s.wait_for("q quits");
    s.send("q");
    s.wait_until("less closed", |t| !t.contains("q quits"));
    // option-click moves the cursor on the line being typed
    s.send("clear\r");
    s.pump(Duration::from_millis(300));
    s.send("echo abcdef");
    s.wait_for("echo abcdef");
    let screen = s.screen.screen();
    let (rows, cols) = screen.size();
    let mut pos = None;
    for r in 0..rows {
        let line: String = (0..cols)
            .map(|c| screen.cell(r, c).map(|x| x.contents()).unwrap_or_default())
            .collect();
        if let Some(i) = line.find("abcdef") {
            pos = Some((line[..i].chars().count() as u16 + 1, r + 1));
        }
    }
    let (x, y) = pos.unwrap();
    s.send(&format!("\x1b[<8;{x};{y}M"));
    s.send(&format!("\x1b[<8;{x};{y}m"));
    s.pump(Duration::from_millis(300));
    s.send("X\r");
    s.wait_for("Xabcdef");
}

#[test]
fn saved_sessions_reconnect_and_logging() {
    let dir = scratch("putty");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    // a stand-in ssh that "drops" the connection (exit 255)
    std::fs::create_dir_all(dir.join("home/bin")).unwrap();
    let fake = dir.join("home/bin/ssh");
    std::fs::write(&fake, "#!/bin/sh\necho \"fake-ssh $*\"\nexit 255\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("home/.bash_profile"),
        "PS1='PROMPT> '\nexport PATH=\"$HOME/bin:$PATH\"\n",
    )
    .unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.run("sv ssh save prod deploy@10.0.0.5 -p 2222");
    s.wait_for("saved prod");
    s.send("sv ssh prod\r");
    s.wait_for("fake-ssh -o ServerAliveInterval=30 -p 2222 deploy@10.0.0.5");
    assert!(
        s.text().lines().nth(1).unwrap_or("").contains("prod"),
        "tab named prod"
    );
    s.wait_for("the ssh connection closed");
    s.send("\x1d");
    s.send("R");
    s.wait_until("reconnected", |t| t.matches("fake-ssh -o").count() >= 2);
    // logging a pane to a file: text only
    s.run("sv log start session.log");
    s.wait_for("logging this pane to");
    s.run("printf '\\033[1;32mgreen-text\\033[0m\\n'");
    s.wait_for("green-text");
    s.run("sv log stop");
    s.wait_for("stopped logging");
    let log = std::fs::read_to_string(dir.join("home/session.log"))
        .or_else(|_| std::fs::read_to_string(dir.join("work/session.log")))
        .unwrap_or_default();
    assert!(
        log.contains("green-text") && !log.contains('\x1b'),
        "{log:?}"
    );
    let line = log
        .lines()
        .find(|l| l.ends_with("] green-text"))
        .unwrap_or_default();
    assert!(
        line.starts_with('[') && line.contains("] green-text"),
        "each line has its time: {line:?}"
    );
    // the log viewer: a table with time and text
    s.send("sv log view session.log\r");
    s.wait_until("log viewer", |t| {
        t.contains("log · session.log") && t.contains("green-text")
    });
    s.send("q");
    s.wait_until("viewer closed", |t| !t.contains("log · session.log"));
    // the saved-sessions picker
    s.run("sv ssh");
    s.wait_until("picker", |t| {
        t.contains("saved sessions") && t.contains("deploy@10.0.0.5:2222")
    });
    s.send("\x1b");
    s.wait_until("picker closed", |t| !t.contains("saved sessions"));
    // every host at once: one tab, a split each, typing goes to all
    s.run("sv ssh save stage deploy@10.0.0.6");
    s.wait_for("saved stage");
    s.send("sv ssh all prod stage\r");
    s.wait_until("two hosts", |t| {
        t.contains("2 hosts")
            && t.lines().nth(2).unwrap_or("").matches("⇶ broad").count() == 2
            && t.contains("-p 2222 de")
            && t.contains("ServerAliveInterval=30 deploy@10.")
    });
    s.pump(Duration::from_millis(500));
    s.send("echo to-every-host\r");
    s.wait_until("typed into both", |t| {
        t.matches("to-every-host").count() >= 4
    });
}

#[test]
fn palette_broadcast_and_failed_command_card() {
    let dir = scratch("palette");
    std::fs::write(dir.join("config.yaml"), "start_view: full\n").unwrap();
    // a stand-in for Claude Code, for "ask an agent why"
    std::fs::create_dir_all(dir.join("home/bin")).unwrap();
    let fake = dir.join("home/bin/claude");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho \"fake claude: $*\" | head -c 300\necho\nsleep 30\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = Session::launch("bash", dir);
    s.wait_for("PROMPT>");
    // the command palette runs an action by name
    s.send("\x1d");
    s.send(" ");
    s.wait_for("command palette");
    s.send("split side");
    s.wait_for("Split side by side");
    s.send("\r");
    s.wait_until("two panes", |t| {
        t.lines().nth(2).unwrap_or("").matches('╭').count() == 2
    });
    s.wait_until("prompt in the new pane", |t| {
        t.matches("PROMPT>").count() >= 2
    });
    // broadcast: one command, both shells
    s.send("\x1d");
    s.send("B");
    s.wait_for("broadcast on");
    s.send("echo cast-$((6*7))\r");
    s.wait_until("ran in both", |t| t.matches("cast-42").count() >= 2);
    s.send("\x1d");
    s.send("B");
    s.wait_for("broadcast off");
    // a failed command: the card, copy, run again
    s.run("ls /no-such-dir-here");
    s.wait_until("card", |t| {
        t.contains("✖ exit") && t.contains("ls /no-such-dir-here") && t.contains("run again")
    });
    s.send("\x1d");
    s.send("c");
    s.wait_for("copied the command and its last lines");
    s.send("\x1d");
    s.send("r");
    s.wait_until("ran again", |t| {
        t.matches("No such file").count() >= 2 || t.matches("cannot access").count() >= 2
    });
    // ctrl+] ? : an agent is asked why it failed
    s.pump(Duration::from_millis(800));
    s.wait_until("card again", |t| {
        t.contains("✖ exit") && t.contains("run again")
    });
    s.send("\x1d");
    s.send("?");
    s.wait_until("agent asked", |t| {
        t.contains("● why") && t.contains("This command failed")
    });
    // the palette now lists the recent command
    s.send("\x1d");
    s.send(" ");
    s.wait_for("command palette");
    s.send("no-such-dir");
    s.wait_for("Run: ls /no-such-dir-here");
    s.send("\x1b");
}

#[test]
fn serial_console_talks_to_a_device() {
    if !have("python3") {
        return;
    }
    let dir = scratch("serial");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    // a fake device: a pty that answers in capitals
    let name = dir.join("devname");
    let script = format!(
        "import pty,os,select,tty\nm,s=pty.openpty()\ntty.setraw(s)\nopen({:?},'w').write(os.ttyname(s))\n\
         while True:\n  r,_,_=select.select([m],[],[],30)\n  if not r: break\n  d=os.read(m,1024)\n  os.write(m,d.upper())\n",
        name.display().to_string()
    );
    let mut dev = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !name.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let path = std::fs::read_to_string(&name).unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send(&format!("sv serial {path} 9600\r"));
    s.wait_for("connected to");
    assert!(
        s.text().lines().nth(1).unwrap_or("").contains("serial ·"),
        "own tab"
    );
    s.send("hello");
    s.wait_for("HELLO");
    s.send("\r~.");
    s.wait_for("disconnected from");
    let _ = dev.kill();
    let _ = dev.wait();
}

#[test]
fn remote_files_over_the_ssh_session() {
    let dir = scratch("remote");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    // a stand-in ssh: "box" alone is the interactive session; a remote
    // command runs here (so listing, cat > and cat -- work on local files)
    std::fs::create_dir_all(dir.join("home/bin")).unwrap();
    let fake = dir.join("home/bin/ssh");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor a; do last=$a; done\nif [ \"$last\" = box ]; then echo connected-to-box; sleep 60; else sh -c \"$last\"; fi\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("home/.bash_profile"),
        "PS1='PROMPT> '\nexport PATH=\"$HOME/bin:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("home/remote-file.txt"), "from the server\n").unwrap();
    std::fs::write(dir.join("work/up.txt"), "to the server\n").unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send("ssh -p 2222 box\r");
    s.wait_for("connected-to-box");
    s.send("\x1d");
    s.send("F");
    s.wait_until("remote listing", |t| {
        t.contains("box:") && t.contains("remote-file.txt")
    });
    // move to remote-file.txt (folders first, then files by name) and download it
    s.send("j");
    for _ in 0..12 {
        s.send("d");
        s.pump(Duration::from_millis(400));
        if dir.join("work/remote-file.txt").exists() {
            break;
        }
        s.send("j");
    }
    assert!(
        dir.join("work/remote-file.txt").exists(),
        "downloaded:\n{}",
        s.text()
    );
    // drop a local file on the pane: uploaded
    let up = dir.join("work/up.txt");
    s.send(&format!("\x1b[200~{}\x1b[201~", up.display()));
    s.wait_for("uploaded up.txt");
    assert_eq!(
        std::fs::read_to_string(dir.join("home/up.txt")).unwrap(),
        "to the server\n"
    );
}

const FAKE_MCP: &str = r#"import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth = m.get("method")
    if meth == "initialize":
        r = {"protocolVersion": m["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}
    elif meth == "tools/list":
        r = {"tools": [{"name": "echo", "description": "Say it back, loud", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}}]}
    elif meth == "tools/call":
        r = {"content": [{"type": "text", "text": m["params"]["arguments"]["text"].upper()}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "no"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}), flush=True)
"#;

#[test]
fn mcp_screens_check_try_traffic_permissions_and_add() {
    if !have("python3") {
        return;
    }
    let dir = scratch("mcp");
    std::fs::write(
        dir.join("config.yaml"),
        "start_view: full\nrestore: never\n",
    )
    .unwrap();
    let server = dir.join("server.py");
    std::fs::write(&server, FAKE_MCP).unwrap();
    std::fs::write(
        dir.join("home/.claude.json"),
        format!(
            r#"{{"mcpServers":{{"fake":{{"command":"python3","args":["{}"]}},"broken":{{"command":"no-such-mcp-cmd-here"}}}}}}"#,
            server.display()
        ),
    )
    .unwrap();
    // one call in a Claude Code log for this project
    let work = dir.join("work");
    let key: String = work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let logdir = dir.join("home/.claude/projects").join(key);
    std::fs::create_dir_all(&logdir).unwrap();
    std::fs::write(
        logdir.join("s.jsonl"),
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-05T10:00:00.000Z\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"mcp__fake__echo\",\"input\":{\"text\":\"from-the-log\"}}]}}\n\
         {\"type\":\"user\",\"timestamp\":\"2026-11-05T10:00:00.250Z\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"FROM-THE-LOG\"}]}}\n",
    )
    .unwrap();
    // and one in a Codex log started in the same folder
    let cdx = dir.join("home/.codex/sessions/2026/11/05");
    std::fs::create_dir_all(&cdx).unwrap();
    std::fs::write(
        cdx.join("rollout-1.jsonl"),
        format!(
            "{{\"timestamp\":\"2026-11-05T09:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{}\"}}}}\n\
             {{\"timestamp\":\"2026-11-05T09:00:01.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"mcp_tool_call_begin\",\"call_id\":\"c1\",\"invocation\":{{\"server\":\"fake\",\"tool\":\"echo\",\"arguments\":{{\"text\":\"from-codex\"}}}}}}}}\n",
            work.display()
        ),
    )
    .unwrap();
    // the echo tool's description was different last time
    std::fs::create_dir_all(dir.join("state")).unwrap();
    std::fs::write(
        dir.join("state/mcp-tools.json"),
        r#"{"fake":{"echo":{"desc":"Say it back"}}}"#,
    )
    .unwrap();
    let mut s = Session::launch("bash", dir.clone());
    s.wait_for("PROMPT>");
    s.send("sv mcp\r");
    s.wait_until("board 17", |t| {
        t.contains("SERVERS")
            && t.contains("MCP · servers your agents can use")
            && t.contains("failing")
    });
    // broken first (alphabetical), then fake: select it, see its health and tools
    s.send("j");
    s.wait_until("fake checked", |t| {
        t.contains("✔ handshake") && t.contains("echo") && t.contains("Say it back")
    });
    s.wait_for("1 description changed");
    s.send("d");
    s.wait_until("the change", |t| {
        t.contains("before") && t.contains("got it")
    });
    s.send("d");
    s.wait_until("change seen", |t| t.contains("tool list ≈"));
    let seen = std::fs::read_to_string(dir.join("state/mcp-tools.json")).unwrap();
    assert!(!seen.contains("before"), "{seen}");
    s.send("\r");
    s.send("t");
    s.wait_for("Try a tool");
    s.send("hello-mcp");
    s.send("\r");
    s.wait_for("HELLO-MCP");
    // board 18: the call from the log
    s.send("\x1b");
    s.send("\x1b");
    s.pump(Duration::from_millis(200));
    s.send("2");
    s.wait_until("board 18", |t| {
        t.contains("LIVE TRAFFIC")
            && t.contains("fake · echo")
            && t.contains("from-the-log")
            && t.contains("from-codex")
    });
    // ⏎ on a call: board 19, the call in full, the server and its log
    s.send("\r");
    s.wait_until("board 19", |t| {
        t.contains("CALL · you → fake · echo")
            && t.contains("Request")
            && t.contains("from-the-log\"")
            && t.contains("FROM-THE-LOG")
            && t.contains("Server log")
            && t.contains("on a crash")
    });
    s.send("\x1b[D"); // the older call (from Codex)
    s.wait_for("from-codex");
    s.send("\x1b");
    s.wait_until("back to board 18", |t| {
        t.contains("LIVE TRAFFIC") && !t.contains("Server log")
    });
    // permissions: allow the whole fake server for you (Claude Code's settings)
    s.send("\t");
    s.pump(Duration::from_millis(200));
    s.send("j");
    s.send(" ");
    s.wait_for("you · mcp__fake → allow");
    let settings = std::fs::read_to_string(work.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("\"mcp__fake\""), "{settings}");
    // add a server for Claude Code and Codex (checked first)
    s.send("\t");
    s.send("\x1b[B"); // name
    s.send("fake2");
    s.send("\x1b[B"); // transport (stdio)
    s.send("\x1b[B"); // command
    s.send(&format!("python3 {}", server.display()));
    s.send("\x1b[B"); // secrets
    s.send("\x1b[B"); // add to
    s.send("\x1b[B"); // button
    s.send("\r");
    s.wait_for("✔ added to Claude Code, Codex");
    let codex = std::fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
    assert!(codex.contains("[mcp_servers.fake2]"), "{codex}");
    let claude = std::fs::read_to_string(dir.join("home/.claude.json")).unwrap();
    assert!(
        claude.contains("\"fake2\"") && claude.contains("\"fake\""),
        "{claude}"
    );
    s.send("\x1b");
    s.wait_for("SERVERS");
    s.pump(Duration::from_millis(300));
    s.send("\x1b");
    s.wait_until("closed", |t| {
        t.contains("PROMPT>") && !t.contains("SERVERS")
    });
}

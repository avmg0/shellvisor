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
    t.lines().next().unwrap_or("")
}

#[test]
fn full_view_has_tab_bar_border_and_status_bar() {
    let mut s = Session::start_full("full");
    s.wait_for("PROMPT>");
    let first = s.line(0);
    assert!(first.contains("1 bash"), "tab bar: {first:?}");
    assert!(s.line(1).starts_with('╭'), "border: {:?}", s.line(1));
    s.wait_for(&format!("▍{}", NAME_LOWER));
    s.wait_for("F1 help");
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
        t.lines().nth(1).unwrap_or("").matches('╭').count() == 2
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
    s.wait_until("stty", |t| t.contains("│20 48 "));
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
        s.line(0).contains("2 "),
        "tab must survive 'n': {:?}",
        s.line(0)
    );
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
    s.wait_for("2 tabs: main, /usr");
    s.send("y");
    s.wait_until("two tabs back", |t| {
        top(t).contains("1 bash · main") && top(t).contains("2 bash · usr ⊞2")
    });
    s.send("\x1b2");
    s.wait_until("split restored", |t| {
        t.lines().nth(1).unwrap_or("").matches('╭').count() == 2
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
        t.lines().nth(1).unwrap_or("").matches('╭').count() == 3
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
    s.wait_for("Split side by side");
    s.send("\x1b[B\x1b[B\r"); // down twice to "Split side by side", enter
    s.wait_until("split", |t| top(t).contains("⊞2"));
    s.send("\x1bm");
    s.wait_for("Rename tab");
    s.send("\x1b"); // esc closes
    s.wait_until("menu closed", |t| !t.contains("Rename tab"));
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
        t.contains("▸ beta/") && t.contains("alpha.txt") && t.contains(" tree ")
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
        t.contains("CPU") && t.contains("MEM") && t.contains(" tree ") && t.contains("TEMP")
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
    s.wait_until("panes back", |t| t.contains("CPU") && t.contains(" tree "));
    // F3 on the shell focuses the system pane; F5 zooms it; `m` sorts by memory.
    s.send("\x1bOR");
    s.pump(Duration::from_millis(200));
    s.send("\x1b[15~");
    s.wait_for("PID");
    s.send("m");
    s.wait_for("MEM▾");
}

#[test]
fn downloads_show_a_loading_circle_and_a_toast() {
    let mut s = Session::start_full("activity");
    s.wait_for("PROMPT>");
    // A stand-in `curl` that prints progress like the real one.
    s.run("curl() { printf 'got 42%%\\n'; sleep 2; }");
    s.send("curl https://example.invalid/file\r");
    s.wait_until("progress in the pane title", |t| {
        t.lines().nth(1).unwrap_or("").contains("curl 42%")
    });
    s.wait_for("✔ curl finished");
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
    s.wait_for("big.bin  3.0 MB");
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
        t.contains("sleep 3") && t.contains("│ pipe")
    });
    s.wait_for("Nothing running");
    // metered pipeline
    s.run("sv flow 'seq 1 20000 | grep 7 | wc -l'");
    s.wait_for("flow · 3 stages");
    s.wait_until("stage meters in the pane", |t| {
        t.contains("finished") && t.contains("20.0k → 6878 lines · 34% kept")
    });
}

//! Remote files: browse the server an ssh session in this tab is connected
//! to, like an SFTP pane. Enter / Backspace move around, `d` downloads the
//! selected file into the local folder, and dropping files from Finder on
//! the pane (the terminal pastes their paths) uploads them here.
//!
//! It runs its own `ssh` with the session's options (port, key, jump host,
//! user) in batch mode, sharing one connection between requests
//! (ControlMaster), so key or agent logins work without asking; a server
//! that wants a password gets a clear message instead of a hang.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::cell::Cell;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const CYAN: Color = Color::Indexed(73);
const RED: Color = Color::Indexed(167);
const GREEN: Color = Color::Indexed(108);
const SEL_BG: Color = Color::Indexed(237);

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    pub when: String,
}

/// Background work, run on a thread.
#[derive(Debug, Clone)]
pub enum Job {
    List {
        args: Vec<String>,
        dir: String,
    },
    Upload {
        args: Vec<String>,
        local: PathBuf,
        dir: String,
    },
    Download {
        args: Vec<String>,
        dir: String,
        name: String,
        to: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub enum Done {
    /// (absolute dir, entries) or why not.
    Listed(Result<(String, Vec<Entry>), String>),
    /// What happened, as a line for the status row; true = refresh after.
    Moved(Result<String, String>, bool),
}

/// The options of an `ssh …` command line that matter for connecting
/// again (no forwards, no remote command), plus the destination.
pub fn ssh_target(cmd: &str) -> Option<Vec<String>> {
    let words = shell_words(cmd);
    let mut it = words.iter().skip_while(|w| *w != "ssh").skip(1);
    let mut out = Vec::new();
    let with_value = [
        "-p", "-i", "-l", "-o", "-J", "-F", "-c", "-m", "-b", "-E", "-I", "-S", "-B",
    ];
    let drop_with_value = ["-L", "-R", "-D", "-W", "-w", "-e", "-Q", "-O"];
    while let Some(w) = it.next() {
        if with_value.contains(&w.as_str()) {
            out.push(w.clone());
            out.push(it.next()?.clone());
        } else if drop_with_value.contains(&w.as_str()) {
            it.next();
        } else if w.starts_with('-') {
            // -A -C -4 -6 -v…: keep; -t -T -N -f -n -M -G: not for us
            if !matches!(
                w.as_str(),
                "-t" | "-tt" | "-T" | "-N" | "-f" | "-n" | "-M" | "-G"
            ) {
                out.push(w.clone());
            }
        } else {
            out.push(w.clone());
            return Some(out); // the destination; what follows is a remote command
        }
    }
    None
}

/// Split a command line the way a shell would (quotes, backslashes).
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut any = false;
    let mut q: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (q, c) {
            (Some(x), c) if c == x => q = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                q = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Paths in what the terminal pasted when files were dropped on it
/// ("/Users/a/My\ File.txt /tmp/b.png", or one per line, maybe quoted).
pub fn dropped_paths(text: &str) -> Vec<PathBuf> {
    shell_words(text)
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect()
}

fn q(s: &str) -> String {
    crate::app::shell_quote(s)
}

fn ssh(args: &[String], remote: &str) -> Command {
    let cm = crate::paths::cache_dir().join("ssh-%C");
    // SHELLVISOR_SSH: another ssh program (a non-standard install, tests)
    let prog = std::env::var("SHELLVISOR_SSH").unwrap_or_else(|_| "ssh".into());
    let mut c = Command::new(prog);
    c.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ControlMaster=auto",
        "-o",
        &format!("ControlPath={}", cm.display()),
        "-o",
        "ControlPersist=600",
    ]);
    c.args(args);
    c.arg(remote);
    c.stdin(Stdio::null());
    c
}

fn explain(stderr: &[u8]) -> String {
    let e = String::from_utf8_lossy(stderr);
    let e = e.trim();
    if e.contains("Permission denied") || e.contains("password") {
        "this server wants a password: remote files need a key or ssh-agent login (ssh-copy-id helps)".into()
    } else if e.is_empty() {
        "couldn't reach the server".into()
    } else {
        e.lines().last().unwrap_or(e).to_string()
    }
}

/// `ls -lAp` lines → entries ("total" and . / .. skipped).
pub fn parse_ls(text: &str) -> Vec<Entry> {
    let mut v = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 9 || line.starts_with("total") {
            continue;
        }
        let perms = f[0];
        let size = f[4].parse().unwrap_or(0);
        let when = format!("{} {} {}", f[5], f[6], f[7]);
        // the name is everything after the 8th field (it may have spaces)
        let mut idx = 0;
        let mut rest = line;
        while idx < 8 {
            rest = rest.trim_start();
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            rest = &rest[end..];
            idx += 1;
        }
        let mut name = rest.trim_start().to_string();
        let link = perms.starts_with('l');
        if link {
            if let Some((n, _)) = name.split_once(" -> ") {
                name = n.to_string();
            }
        }
        let dir = perms.starts_with('d') || name.ends_with('/');
        let name = name.trim_end_matches('/').to_string();
        if name == "." || name == ".." || name.is_empty() {
            continue;
        }
        v.push(Entry {
            name,
            dir,
            link,
            size,
            when,
        });
    }
    v.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    v
}

pub fn run(job: Job) -> Done {
    match job {
        Job::List { args, dir } => {
            let cd = if dir.is_empty() {
                "cd".to_string()
            } else {
                format!("cd -- {}", q(&dir))
            };
            let remote = format!("{cd} && pwd && LC_ALL=C ls -lAp");
            match ssh(&args, &remote).output() {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let mut lines = text.lines();
                    let abs = lines.next().unwrap_or("/").to_string();
                    let rest: Vec<&str> = lines.collect();
                    Done::Listed(Ok((abs, parse_ls(&rest.join("\n")))))
                }
                Ok(o) => Done::Listed(Err(explain(&o.stderr))),
                Err(e) => Done::Listed(Err(format!("ssh: {e}"))),
            }
        }
        Job::Upload { args, local, dir } => {
            let name = local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if local.is_dir() {
                return Done::Moved(Err(format!("{name} is a folder — drop files")), false);
            }
            let file = match std::fs::File::open(&local) {
                Ok(f) => f,
                Err(e) => return Done::Moved(Err(format!("{name}: {e}")), false),
            };
            let target = format!("{}/{}", dir.trim_end_matches('/'), name);
            let mut c = ssh(&args, &format!("cat > {}", q(&target)));
            c.stdin(Stdio::from(file));
            match c.output() {
                Ok(o) if o.status.success() => Done::Moved(Ok(format!("↑ uploaded {name}")), true),
                Ok(o) => Done::Moved(Err(format!("{name}: {}", explain(&o.stderr))), false),
                Err(e) => Done::Moved(Err(format!("ssh: {e}")), false),
            }
        }
        Job::Download {
            args,
            dir,
            name,
            to,
        } => {
            let src = format!("{}/{}", dir.trim_end_matches('/'), name);
            let mut dest = to.join(&name);
            let mut n = 1;
            while dest.exists() {
                dest = to.join(format!("{name}.{n}"));
                n += 1;
            }
            let file = match std::fs::File::create(&dest) {
                Ok(f) => f,
                Err(e) => return Done::Moved(Err(format!("{}: {e}", dest.display())), false),
            };
            let mut c = ssh(&args, &format!("cat -- {}", q(&src)));
            c.stdout(Stdio::from(file));
            match c.status() {
                Ok(s) if s.success() => Done::Moved(
                    Ok(format!("↓ downloaded {name} to {}", dest.display())),
                    false,
                ),
                _ => {
                    let _ = std::fs::remove_file(&dest);
                    Done::Moved(Err(format!("couldn't download {name}")), false)
                }
            }
        }
    }
}

pub struct RemotePane {
    pub args: Vec<String>,
    /// "deploy@10.0.0.5"
    pub host: String,
    /// The remote folder shown ("" = home, until the first answer).
    pub dir: String,
    pub entries: Option<Result<Vec<Entry>, String>>,
    pub sel: usize,
    scroll: Cell<usize>,
    /// What's going on / what just happened.
    pub status: Option<(String, bool)>,
    /// Downloads go here.
    pub local: PathBuf,
}

impl RemotePane {
    pub fn new(args: Vec<String>, local: PathBuf) -> (Self, Job) {
        let host = args.last().cloned().unwrap_or_default();
        let p = RemotePane {
            args: args.clone(),
            host,
            dir: String::new(),
            entries: None,
            sel: 0,
            scroll: Cell::new(0),
            status: Some(("connecting…".into(), true)),
            local,
        };
        (
            p,
            Job::List {
                args,
                dir: String::new(),
            },
        )
    }

    pub fn title(&self) -> String {
        format!(
            "⇄ {}:{}",
            self.host,
            if self.dir.is_empty() { "~" } else { &self.dir }
        )
    }

    fn list(&self, dir: String) -> Job {
        Job::List {
            args: self.args.clone(),
            dir,
        }
    }

    pub fn done(&mut self, d: Done) -> Vec<Job> {
        match d {
            Done::Listed(Ok((abs, entries))) => {
                if abs != self.dir {
                    self.sel = 0;
                    self.scroll.set(0);
                }
                self.dir = abs;
                self.sel = self.sel.min(entries.len().saturating_sub(1));
                self.entries = Some(Ok(entries));
                if self.status.as_ref().is_some_and(|(_, busy)| *busy) {
                    self.status = None;
                }
                Vec::new()
            }
            Done::Listed(Err(e)) => {
                self.entries = Some(Err(e));
                self.status = None;
                Vec::new()
            }
            Done::Moved(r, refresh) => {
                self.status = Some(match r {
                    Ok(m) => (m, false),
                    Err(e) => (format!("✖ {e}"), false),
                });
                if refresh {
                    vec![self.list(self.dir.clone())]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn current(&self) -> Option<&Entry> {
        match &self.entries {
            Some(Ok(e)) => e.get(self.sel),
            _ => None,
        }
    }

    fn parent(&self) -> String {
        let d = self.dir.trim_end_matches('/');
        match d.rfind('/') {
            Some(0) | None => "/".into(),
            Some(i) => d[..i].to_string(),
        }
    }

    /// A key; returns jobs to run.
    pub fn key(&mut self, key: &[u8]) -> Vec<Job> {
        let n = match &self.entries {
            Some(Ok(e)) => e.len(),
            _ => 0,
        };
        if key.starts_with(b"\x1b[200~") {
            let text = String::from_utf8_lossy(key);
            let text = text
                .trim_start_matches("\x1b[200~")
                .trim_end_matches("\x1b[201~");
            let files = dropped_paths(text);
            if files.is_empty() {
                self.status = Some(("drop files from Finder here to upload them".into(), false));
                return Vec::new();
            }
            self.status = Some((format!("↑ uploading {} file(s)…", files.len()), true));
            return files
                .into_iter()
                .map(|local| Job::Upload {
                    args: self.args.clone(),
                    local,
                    dir: self.dir.clone(),
                })
                .collect();
        }
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => self.sel = self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" | b"j" => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            b"\x1b[5~" => self.sel = self.sel.saturating_sub(10),
            b"\x1b[6~" => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            b"\r" | b"\x1b[C" | b"l" => {
                if let Some(e) = self.current() {
                    if e.dir || e.link {
                        let d = format!("{}/{}", self.dir.trim_end_matches('/'), e.name);
                        self.status = Some(("…".into(), true));
                        return vec![self.list(d)];
                    }
                    self.status = Some(("d downloads this file".into(), false));
                }
            }
            [0x7f] | [0x08] | b"\x1b[D" | b"h" => {
                self.status = Some(("…".into(), true));
                return vec![self.list(self.parent())];
            }
            b"~" => return vec![self.list(String::new())],
            b"r" => {
                self.status = Some(("…".into(), true));
                return vec![self.list(self.dir.clone())];
            }
            b"d" => {
                if let Some(e) = self.current().filter(|e| !e.dir).cloned() {
                    self.status = Some((format!("↓ downloading {}…", e.name), true));
                    return vec![Job::Download {
                        args: self.args.clone(),
                        dir: self.dir.clone(),
                        name: e.name,
                        to: self.local.clone(),
                    }];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, folder: Style, focused: bool) {
        if area.height < 3 || area.width < 16 {
            return;
        }
        let put = |buf: &mut Buffer, x: u16, y: u16, s: &str, st: Style| {
            if y < area.bottom() && x < area.right() {
                buf.set_stringn(x, y, s, (area.right() - x) as usize, st);
            }
        };
        put(
            buf,
            area.x + 1,
            area.y,
            &format!(
                "{}:{}",
                self.host,
                if self.dir.is_empty() { "~" } else { &self.dir }
            ),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        let list_top = area.y + 1;
        let list_h = area.height.saturating_sub(3) as usize;
        match &self.entries {
            None => put(
                buf,
                area.x + 1,
                list_top,
                "connecting…",
                Style::default().fg(DIM),
            ),
            Some(Err(e)) => {
                put(
                    buf,
                    area.x + 1,
                    list_top,
                    "✖ couldn't list this folder",
                    Style::default().fg(RED),
                );
                put(buf, area.x + 1, list_top + 1, e, Style::default().fg(DIM));
            }
            Some(Ok(entries)) => {
                if entries.is_empty() {
                    put(
                        buf,
                        area.x + 1,
                        list_top,
                        "empty folder",
                        Style::default().fg(DIM),
                    );
                }
                let mut top = self.scroll.get();
                if self.sel < top {
                    top = self.sel;
                } else if self.sel >= top + list_h {
                    top = self.sel + 1 - list_h;
                }
                self.scroll.set(top);
                let w = area.width as usize;
                for (i, e) in entries.iter().enumerate().skip(top).take(list_h) {
                    let y = list_top + (i - top) as u16;
                    let selected = i == self.sel;
                    let base = if selected {
                        Style::default().bg(SEL_BG).add_modifier(if focused {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        })
                    } else {
                        Style::default()
                    };
                    if selected {
                        buf.set_style(Rect::new(area.x, y, area.width, 1), base);
                    }
                    let (icon, st) = if e.dir {
                        ("▸ ", base.patch(folder))
                    } else if e.link {
                        ("↪ ", base.fg(Color::Indexed(170)))
                    } else {
                        ("  ", base)
                    };
                    let name = format!(" {icon}{}{}", e.name, if e.dir { "/" } else { "" });
                    put(buf, area.x, y, &name, st);
                    if w > 40 {
                        let right = if e.dir {
                            e.when.clone()
                        } else {
                            format!("{:>9}  {}", crate::sysmon::human(e.size), e.when)
                        };
                        let rx = area
                            .right()
                            .saturating_sub(right.chars().count() as u16 + 1);
                        put(buf, rx, y, &right, base.fg(FAINT));
                    }
                }
            }
        }
        if let Some((s, busy)) = &self.status {
            let st = if s.starts_with('✖') {
                Style::default().fg(RED)
            } else if *busy {
                Style::default().fg(AMBER)
            } else {
                Style::default().fg(GREEN)
            };
            put(buf, area.x + 1, area.bottom().saturating_sub(2), s, st);
        }
        put(
            buf,
            area.x,
            area.bottom().saturating_sub(1),
            "⏎ open · ⌫ up · d download · r refresh · drop files here to upload",
            Style::default().fg(FAINT),
        );
        let _ = CYAN;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_options_kept() {
        assert_eq!(
            ssh_target(
                "ssh -o ServerAliveInterval=30 -p 2222 -L 8080:localhost:80 deploy@10.0.0.5 htop"
            )
            .unwrap(),
            vec![
                "-o",
                "ServerAliveInterval=30",
                "-p",
                "2222",
                "deploy@10.0.0.5"
            ]
        );
        assert_eq!(
            ssh_target("ssh -t -i '~/My Keys/k' box").unwrap(),
            vec!["-i", "~/My Keys/k", "box"]
        );
        assert_eq!(ssh_target("ls"), None);
        assert_eq!(ssh_target("ssh -p"), None);
    }

    #[test]
    fn listing_and_drops() {
        let ls = "total 8\n\
                  drwxr-xr-x  2 me staff   64 Nov  5 10:00 src/\n\
                  -rw-r--r--  1 me staff 1234 Nov  5 09:00 My File.txt\n\
                  lrwxr-xr-x  1 me staff    4 Nov  5 08:00 cur -> /tmp\n";
        let e = parse_ls(ls);
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].name.as_str(), e[0].dir), ("src", true));
        assert_eq!(e[1].name, "cur");
        assert!(e[1].link);
        assert_eq!((e[2].name.as_str(), e[2].size), ("My File.txt", 1234));
        assert_eq!(
            dropped_paths("/Users/a/My\\ File.txt '/tmp/b c.png' relative"),
            vec![
                PathBuf::from("/Users/a/My File.txt"),
                PathBuf::from("/tmp/b c.png")
            ]
        );
        let (mut p, _) = RemotePane::new(vec!["box".into()], PathBuf::from("/tmp"));
        p.done(Done::Listed(Ok(("/home/me/src".into(), e))));
        assert_eq!(p.parent(), "/home/me");
        assert_eq!(p.title(), "⇄ box:/home/me/src");
    }
}

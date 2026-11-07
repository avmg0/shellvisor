//! Agent replay: step through what an agent did after the fact.
//!
//! While an agent runs, Shellvisor writes `state/replay/<agent>-<stamp>.jsonl`
//! (mode 0600, in a 0700 folder): one frame per line —
//!
//! - `status`  — started, working, approval, done, failed … with its screen
//! - `screen`  — what its terminal showed, when it changed (at most every 2 s)
//! - `files`   — the files it had changed so far (+added −removed)
//! - `approval`— what it asked and how it was answered (you, auto, policy)
//! - `note`    — messages to it, its task
//!
//! `sv agent replay [NAME|FILE]` opens a viewer: the timeline on the left,
//! the screen at that moment on the right, files and approvals under it.
//! Recordings stay on this computer; the newest 40 are kept
//! (`agents.replay: false` in config.yaml turns recording off).

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Frames per recording, and bytes, before it stops adding screens.
const MAX_FRAMES: usize = 4000;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const KEEP: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    /// Seconds since the agent started.
    pub t: f64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screen: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<(String, u64, u64)>,
}

pub fn dir() -> PathBuf {
    crate::paths::state_dir().join("replay")
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

/// Writes one agent's frames as it runs.
pub struct Recorder {
    pub path: PathBuf,
    started: Instant,
    frames: usize,
    bytes: u64,
    last_screen: Vec<String>,
    last_screen_at: Option<Instant>,
    last_status: String,
    last_files: Vec<(String, u64, u64)>,
    last_ask: String,
    full: bool,
}

impl Recorder {
    pub fn new(agent: &str, header: &str) -> Option<Recorder> {
        let d = dir();
        std::fs::create_dir_all(&d).ok()?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
        prune(&d);
        let path = d.join(format!("{agent}-{}.jsonl", stamp()));
        let mut r = Recorder {
            path,
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            last_screen: Vec::new(),
            last_screen_at: None,
            last_status: String::new(),
            last_files: Vec::new(),
            last_ask: String::new(),
            full: false,
        };
        r.push(Frame {
            t: 0.0,
            kind: "note".into(),
            text: header.to_string(),
            screen: Vec::new(),
            files: Vec::new(),
        });
        let _ = std::fs::set_permissions(&r.path, std::fs::Permissions::from_mode(0o600));
        Some(r)
    }

    fn t(&self) -> f64 {
        (self.started.elapsed().as_secs_f64() * 10.0).round() / 10.0
    }

    fn push(&mut self, f: Frame) {
        if self.full && f.kind == "screen" {
            return;
        }
        let Ok(line) = serde_json::to_string(&f) else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        if writeln!(file, "{line}").is_ok() {
            self.frames += 1;
            self.bytes += line.len() as u64 + 1;
            if self.frames >= MAX_FRAMES || self.bytes >= MAX_BYTES {
                self.full = true;
            }
        }
    }

    /// The screen, trimmed (trailing blank lines and spaces).
    fn trim(rows: &[String]) -> Vec<String> {
        let mut v: Vec<String> = rows.iter().map(|r| r.trim_end().to_string()).collect();
        while v.last().is_some_and(|l| l.is_empty()) {
            v.pop();
        }
        v
    }

    /// Called often; writes what changed since the last call.
    pub fn observe(
        &mut self,
        status: &str,
        rows: &[String],
        files: &[(String, u64, u64)],
        asking: Option<&str>,
    ) {
        let screen = Self::trim(rows);
        let t = self.t();
        if status != self.last_status && !status.is_empty() {
            self.last_status = status.to_string();
            self.last_screen = screen.clone();
            self.last_screen_at = Some(Instant::now());
            self.push(Frame {
                t,
                kind: "status".into(),
                text: status.to_string(),
                screen: screen.clone(),
                files: Vec::new(),
            });
        }
        let ask = asking.unwrap_or("").to_string();
        if ask != self.last_ask {
            if !ask.is_empty() {
                self.push(Frame {
                    t,
                    kind: "approval".into(),
                    text: format!("asks: {ask}"),
                    screen: screen.clone(),
                    files: Vec::new(),
                });
            }
            self.last_ask = ask;
        }
        if files != self.last_files.as_slice() {
            self.last_files = files.to_vec();
            self.push(Frame {
                t,
                kind: "files".into(),
                text: format!("{} file(s) changed", files.len()),
                screen: Vec::new(),
                files: files.to_vec(),
            });
        }
        let due = self
            .last_screen_at
            .is_none_or(|at| at.elapsed().as_secs_f64() >= 2.0);
        if due && screen != self.last_screen && !screen.is_empty() {
            self.last_screen = screen.clone();
            self.last_screen_at = Some(Instant::now());
            self.push(Frame {
                t,
                kind: "screen".into(),
                text: String::new(),
                screen,
                files: Vec::new(),
            });
        }
    }

    /// An answered approval, a message, a pause…
    pub fn note(&mut self, kind: &str, text: &str) {
        let t = self.t();
        self.push(Frame {
            t,
            kind: kind.into(),
            text: text.to_string(),
            screen: Vec::new(),
            files: Vec::new(),
        });
    }
}

fn prune(d: &Path) {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(d)
        .map(|r| {
            r.flatten()
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    while v.len() >= KEEP {
        let _ = std::fs::remove_file(v.remove(0).1);
    }
}

/// Recordings, newest first: (path, agent name).
pub fn list() -> Vec<(PathBuf, String)> {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir())
        .map(|r| {
            r.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter()
        .rev()
        .map(|(_, p)| {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // name-YYYYMMDD-HHMMSS
            let name = stem.rsplitn(3, '-').nth(2).unwrap_or(&stem).to_string();
            (p, name)
        })
        .collect()
}

pub fn load(path: &Path) -> Vec<Frame> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The screen and files in effect at frame `i`.
pub fn state_at(frames: &[Frame], i: usize) -> (Vec<String>, Vec<(String, u64, u64)>) {
    let upto = &frames[..=i.min(frames.len().saturating_sub(1))];
    let screen = upto
        .iter()
        .rev()
        .find(|f| !f.screen.is_empty())
        .map(|f| f.screen.clone())
        .unwrap_or_default();
    let files = upto
        .iter()
        .rev()
        .find(|f| f.kind == "files")
        .map(|f| f.files.clone())
        .unwrap_or_default();
    (screen, files)
}

pub fn fmt_t(t: f64) -> String {
    let s = t as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// One line for the timeline list.
pub fn label(f: &Frame) -> String {
    match f.kind.as_str() {
        "screen" => {
            let last = f
                .screen
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            format!("· {last}")
        }
        "status" => format!("● {}", f.text),
        "files" => {
            let add: u64 = f.files.iter().map(|x| x.1).sum();
            let del: u64 = f.files.iter().map(|x| x.2).sum();
            format!("✎ {} file(s) +{add} −{del}", f.files.len())
        }
        "approval" => format!("⚑ {}", f.text),
        _ => format!("» {}", f.text),
    }
}

// ------------------------------------------------------------------ viewer

/// `sv agent replay [NAME | FILE | --list]`
pub fn run(args: &[String]) -> i32 {
    let all = list();
    if args.iter().any(|a| a == "--list" || a == "-l") {
        if all.is_empty() {
            println!("no replays yet — they are recorded while agents run");
        }
        for (p, name) in &all {
            let frames = load(p);
            let end = frames.last().map(|f| f.t).unwrap_or(0.0);
            println!(
                "{name:<14} {:>7}  {:>5} frames  {}",
                fmt_t(end),
                frames.len(),
                p.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        return 0;
    }
    let want = args.iter().find(|a| !a.starts_with('-'));
    let path = match want {
        Some(w) if Path::new(w).is_file() => PathBuf::from(w),
        Some(w) => match all.iter().find(|(_, n)| n == w) {
            Some((p, _)) => p.clone(),
            None => {
                eprintln!("sv agent replay: no replay of `{w}` — sv agent replay --list");
                return 1;
            }
        },
        None => match all.first() {
            Some((p, _)) => p.clone(),
            None => {
                eprintln!("sv agent replay: no replays yet — they are recorded while agents run");
                return 1;
            }
        },
    };
    let frames = load(&path);
    if frames.is_empty() {
        eprintln!("sv agent replay: {} is empty", path.display());
        return 1;
    }
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match view(&title, &frames) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sv agent replay: {e}");
            1
        }
    }
}

fn view(title: &str, frames: &[Frame]) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent};
    use crossterm::{execute, terminal};
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;

    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    let restore = || {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
    };
    let mut term = match Terminal::new(CrosstermBackend::new(std::io::stdout())) {
        Ok(t) => t,
        Err(e) => {
            restore();
            return Err(e);
        }
    };
    let mut v = Viewer::new(title, frames);
    let res = (|| -> std::io::Result<()> {
        loop {
            term.draw(|f| {
                let area = f.area();
                v.draw(area, f.buffer_mut());
            })?;
            let wait = if v.playing {
                std::time::Duration::from_millis(600)
            } else {
                std::time::Duration::from_secs(3600)
            };
            if event::poll(wait)? {
                if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                    let k = match code {
                        KeyCode::Char(c) => c.to_string(),
                        KeyCode::Left => "left".into(),
                        KeyCode::Right => "right".into(),
                        KeyCode::Up => "up".into(),
                        KeyCode::Down => "down".into(),
                        KeyCode::Home => "home".into(),
                        KeyCode::End => "end".into(),
                        KeyCode::Esc => "q".into(),
                        _ => String::new(),
                    };
                    if !v.key(&k) {
                        return Ok(());
                    }
                }
            } else if v.playing {
                v.key("tick");
            }
        }
    })();
    restore();
    res
}

pub struct Viewer<'a> {
    title: String,
    frames: &'a [Frame],
    pub sel: usize,
    pub playing: bool,
}

impl<'a> Viewer<'a> {
    pub fn new(title: &str, frames: &'a [Frame]) -> Viewer<'a> {
        Viewer {
            title: title.to_string(),
            frames,
            sel: 0,
            playing: false,
        }
    }

    /// false: quit.
    pub fn key(&mut self, k: &str) -> bool {
        let last = self.frames.len().saturating_sub(1);
        match k {
            "q" => return false,
            "right" | "l" | "j" | "down" => self.sel = (self.sel + 1).min(last),
            "left" | "h" | "k" | "up" => self.sel = self.sel.saturating_sub(1),
            "home" | "g" => self.sel = 0,
            "end" | "G" => self.sel = last,
            " " | "p" => self.playing = !self.playing,
            "tick" => {
                if self.sel >= last {
                    self.playing = false;
                } else {
                    self.sel += 1;
                }
            }
            // next / previous status change, approval or file change
            "]" | "n" => {
                if let Some(i) = (self.sel + 1..=last).find(|&i| self.frames[i].kind != "screen") {
                    self.sel = i;
                }
            }
            "[" | "N" => {
                if let Some(i) = (0..self.sel)
                    .rev()
                    .find(|&i| self.frames[i].kind != "screen")
                {
                    self.sel = i;
                }
            }
            _ => {}
        }
        true
    }

    pub fn draw(&self, r: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::style::{Color, Modifier, Style};
        let amber = Style::default().fg(Color::Indexed(179));
        let dim = Style::default().fg(Color::Indexed(245));
        let text = Style::default().fg(Color::Indexed(252));
        if r.width < 40 || r.height < 10 {
            buf.set_string(r.x, r.y, "replay: make the window bigger", text);
            return;
        }
        let f = &self.frames[self.sel];
        let end = self.frames.last().map(|f| f.t).unwrap_or(0.0);
        buf.set_stringn(
            r.x + 1,
            r.y,
            format!("◆ replay · {}", self.title),
            r.width as usize - 2,
            amber.add_modifier(Modifier::BOLD),
        );
        let pos = format!(
            "{} / {}  ·  frame {} of {}{}",
            fmt_t(f.t),
            fmt_t(end),
            self.sel + 1,
            self.frames.len(),
            if self.playing { "  ▶ playing" } else { "" }
        );
        buf.set_string(
            r.right().saturating_sub(pos.chars().count() as u16 + 1),
            r.y,
            &pos,
            dim,
        );
        // scrubber
        let bw = r.width.saturating_sub(2) as usize;
        let at = if end > 0.0 {
            ((f.t / end) * (bw.saturating_sub(1)) as f64) as usize
        } else {
            0
        };
        let mut bar: Vec<char> = vec!['─'; bw];
        for fr in self.frames {
            if fr.kind != "screen" && end > 0.0 {
                let i = ((fr.t / end) * (bw.saturating_sub(1)) as f64) as usize;
                bar[i.min(bw - 1)] = match fr.kind.as_str() {
                    "approval" => '⚑',
                    "files" => '✎',
                    "status" => '●',
                    _ => '·',
                };
            }
        }
        bar[at.min(bw - 1)] = '█';
        buf.set_string(r.x + 1, r.y + 1, bar.iter().collect::<String>(), dim);
        // timeline (left)
        let lw = (r.width / 3).clamp(24, 50);
        let top = r.y + 3;
        let h = r.height.saturating_sub(5) as usize;
        let first = self
            .sel
            .saturating_sub(h / 2)
            .min(self.frames.len().saturating_sub(h));
        for (row, i) in (first..self.frames.len()).take(h).enumerate() {
            let fr = &self.frames[i];
            let line = format!("{:>6} {}", fmt_t(fr.t), label(fr));
            let st = if i == self.sel {
                Style::default()
                    .bg(Color::Indexed(237))
                    .fg(Color::Indexed(252))
            } else if fr.kind == "screen" {
                dim
            } else if fr.kind == "approval" {
                amber
            } else {
                text
            };
            buf.set_stringn(r.x + 1, top + row as u16, line, lw as usize - 1, st);
        }
        for row in 0..h as u16 {
            buf.set_string(
                r.x + lw,
                top + row,
                "│",
                Style::default().fg(Color::Indexed(238)),
            );
        }
        // screen at this moment (right)
        let (screen, files) = state_at(self.frames, self.sel);
        let sx = r.x + lw + 1;
        let sw = r.right().saturating_sub(sx + 1) as usize;
        let files_h = if files.is_empty() {
            0
        } else {
            files.len().min(6) + 1
        };
        let sh = h.saturating_sub(files_h + 1);
        buf.set_stringn(sx, top, "screen", sw, amber);
        let start = screen.len().saturating_sub(sh.saturating_sub(1));
        for (row, l) in screen[start..].iter().enumerate() {
            buf.set_stringn(sx, top + 1 + row as u16, l, sw, text);
        }
        if files_h > 0 {
            let fy = top + 1 + sh as u16;
            buf.set_stringn(sx, fy, "files changed", sw, amber);
            for (k, (p, a, d)) in files.iter().take(6).enumerate() {
                buf.set_stringn(sx, fy + 1 + k as u16, format!("{p}  +{a} −{d}"), sw, text);
            }
        }
        buf.set_stringn(
            r.x + 1,
            r.bottom() - 1,
            "←→ step · ] [ next / previous event · space play · g G start / end · q quit",
            r.width as usize - 2,
            dim,
        );
        if !f.text.is_empty() && f.kind != "screen" {
            let msg = label(f);
            buf.set_stringn(sx, r.bottom() - 2, msg, sw, amber);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_and_stepping() {
        let tmp = std::env::temp_dir().join(format!("sv-replay-{}", std::process::id()));
        let path = tmp.join("api-1.jsonl");
        std::fs::create_dir_all(&tmp).unwrap();
        let mut r = Recorder {
            path: path.clone(),
            started: Instant::now(),
            frames: 0,
            bytes: 0,
            last_screen: Vec::new(),
            last_screen_at: None,
            last_status: String::new(),
            last_files: Vec::new(),
            last_ask: String::new(),
            full: false,
        };
        let rows = |s: &str| s.lines().map(String::from).collect::<Vec<_>>();
        r.observe("working", &rows("$ claude\n› Reading src\n\n"), &[], None);
        r.observe("working", &rows("$ claude\n› Reading src\n"), &[], None); // same: nothing
        r.observe(
            "approval",
            &rows("Run pytest? (y/n)"),
            &[("src/a.py".into(), 3, 1)],
            Some("run pytest"),
        );
        r.note("answer", "you allowed: run pytest");
        r.observe(
            "done",
            &rows("12 passed"),
            &[("src/a.py".into(), 3, 1)],
            None,
        );
        let f = load(&path);
        let kinds: Vec<&str> = f.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["status", "status", "approval", "files", "answer", "status"]
        );
        assert_eq!(f[0].screen, ["$ claude", "› Reading src"]);
        let (screen, files) = state_at(&f, 4);
        assert_eq!(screen, ["Run pytest? (y/n)"]);
        assert_eq!(files, [("src/a.py".to_string(), 3, 1)]);
        let mut v = Viewer::new("api", &f);
        v.key("]");
        assert_eq!(v.sel, 1);
        v.key("G");
        assert_eq!(v.sel, 5);
        v.key("[");
        assert_eq!(v.sel, 4);
        assert!(!v.key("q"));
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 100, 24));
        v.draw(buf.area, &mut buf);
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("replay · api") && text.contains("you allowed"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

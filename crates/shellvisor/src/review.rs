//! Board 21: review an agent's work before it is merged — the files it
//! changed, its commits and some checks, a coloured diff (unified, side by
//! side or the whole file), merge / squash / message buttons, and "approve for
//! a while" (its approval prompts answered by themselves for 15 minutes, an
//! hour or the rest of its task).
//!
//! Opened with `sv agent diff NAME`, `ctrl+] D` on an agent's column, or `d`
//! on the agent board.

use crate::agents::Action;
use crate::mcpscreen::{
    AMBER, BAR, BORDER, CYAN, DIM, GREEN, INK, PANEL, RED, SELECT, TEXT, boxed, fit, hms, put,
    put_n, put_right, put_w, ratio, st,
};
use crate::palette::{Role, role};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One changed file: git's letter (M A D R), path, lines added / removed,
/// and whether the change is committed on the agent's branch yet.
#[derive(Debug, Clone, PartialEq)]
pub struct RFile {
    pub status: char,
    pub path: String,
    pub added: u64,
    pub removed: u64,
    pub committed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DKind {
    Hunk,
    Ctx,
    Add,
    Del,
}

/// A line of a unified diff with its old and new line numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct DLine {
    pub kind: DKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

/// A check shown under the commits: icon, its colour, then text parts.
pub type Check = (&'static str, Color, Vec<(String, Style)>);

/// What the review asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewAct {
    Close,
    Merge,
    Squash,
    Message(String),
    OpenFiles(PathBuf),
    Edit(PathBuf, Option<usize>),
    /// Remove the worktree and delete the branch.
    Discard,
    /// Turn approving-by-itself on (how long: None = this task; which
    /// kinds), or off.
    Approve(Option<(Option<Duration>, Vec<Action>)>),
}

pub const DURATIONS: [(&str, Option<u64>); 3] = [
    ("15 min", Some(15 * 60)),
    ("1 hour", Some(3600)),
    ("this task", None),
];

/// The kinds of request in each row of the approve box.
pub const CLASSES: [(&str, &[Action]); 3] = [
    (
        "read, edit, test, lint",
        &[Action::Read, Action::Edit, Action::Test, Action::Lint],
    ),
    ("install", &[Action::Install]),
    (
        "network · push · delete",
        &[Action::Network, Action::Push, Action::Delete],
    ),
];

pub struct Review {
    pub agent: String,
    pub color: Color,
    /// "✔ done", "⠋ working"… and whether it's a good state.
    pub status: String,
    pub status_color: Color,
    pub branch: String,
    /// Your branch (merges go into it).
    pub into: String,
    pub repo: PathBuf,
    pub worktree: PathBuf,
    /// Where the agent's branch left yours.
    pub base: String,
    pub tokens: Option<u64>,
    pub files: Vec<RFile>,
    pub commits: Vec<(String, String)>,
    pub checks: Vec<Check>,
    pub sel: usize,
    /// 0 unified, 1 side by side, 2 whole file.
    pub view: u8,
    pub scroll: usize,
    pub diff: Vec<DLine>,
    /// The keys go to the approve box.
    pub focus_approve: bool,
    pub dur: usize,
    pub classes: [bool; 3],
    pub class_sel: usize,
    /// Approving by itself now: until when (text), set by the app.
    pub approving: Option<String>,
    /// Typing a message to the agent.
    pub typing: Option<String>,
    /// `x` pressed once: press again to throw the branch away.
    pub confirm_x: bool,
    pub note: Option<String>,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Parse `git diff` output into lines with numbers (headers dropped).
pub fn parse_diff(text: &str) -> Vec<DLine> {
    let mut out = Vec::new();
    let (mut o, mut n) = (0u32, 0u32);
    let mut in_hunk = false;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("@@ ") {
            // @@ -14,9 +14,40 @@ context
            let mut it = rest.split_whitespace();
            let old = it.next().unwrap_or("-0");
            let new = it.next().unwrap_or("+0");
            let num = |s: &str| {
                s[1..]
                    .split(',')
                    .next()
                    .and_then(|x| x.parse::<u32>().ok())
                    .unwrap_or(0)
            };
            o = num(old);
            n = num(new);
            in_hunk = true;
            out.push(DLine {
                kind: DKind::Hunk,
                old: None,
                new: None,
                text: l.to_string(),
            });
            continue;
        }
        if !in_hunk || l.starts_with('\\') {
            continue;
        }
        let (kind, text) = match l.chars().next() {
            Some('+') => (DKind::Add, &l[1..]),
            Some('-') => (DKind::Del, &l[1..]),
            Some(' ') => (DKind::Ctx, &l[1..]),
            None => (DKind::Ctx, ""),
            _ => {
                in_hunk = false;
                continue;
            }
        };
        let (old, new) = match kind {
            DKind::Add => {
                n += 1;
                (None, Some(n - 1))
            }
            DKind::Del => {
                o += 1;
                (Some(o - 1), None)
            }
            _ => {
                o += 1;
                n += 1;
                (Some(o - 1), Some(n - 1))
            }
        };
        out.push(DLine {
            kind,
            old,
            new,
            text: text.replace('\t', "    "),
        });
    }
    out
}

/// Side by side rows: (left, right); a run of removed lines is paired with
/// the added lines that follow it.
pub fn side_by_side(d: &[DLine]) -> Vec<(Option<&DLine>, Option<&DLine>)> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < d.len() {
        match d[i].kind {
            DKind::Del => {
                let ds: Vec<&DLine> = d[i..].iter().take_while(|l| l.kind == DKind::Del).collect();
                let j = i + ds.len();
                let ad: Vec<&DLine> = d[j..].iter().take_while(|l| l.kind == DKind::Add).collect();
                for k in 0..ds.len().max(ad.len()) {
                    rows.push((ds.get(k).copied(), ad.get(k).copied()));
                }
                i = j + ad.len();
            }
            DKind::Add => {
                rows.push((None, Some(&d[i])));
                i += 1;
            }
            _ => {
                rows.push((Some(&d[i]), Some(&d[i])));
                i += 1;
            }
        }
    }
    rows
}

impl Review {
    /// Read the agent's branch: files, commits, the first file's diff.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: &str,
        color: Color,
        status: (String, Color),
        repo: PathBuf,
        worktree: PathBuf,
        branch: String,
        start: Option<String>,
        tokens: Option<u64>,
    ) -> Review {
        let into = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "HEAD".into());
        let base = git(&repo, &["merge-base", "HEAD", &branch])
            .map(|s| s.trim().to_string())
            .or(start)
            .unwrap_or_else(|| "HEAD".into());
        let mut r = Review {
            agent: agent.to_string(),
            color,
            status: status.0,
            status_color: status.1,
            branch,
            into,
            repo,
            worktree,
            base,
            tokens,
            files: Vec::new(),
            commits: Vec::new(),
            checks: Vec::new(),
            sel: 0,
            view: 0,
            scroll: 0,
            diff: Vec::new(),
            focus_approve: false,
            dur: 1,
            classes: [true, true, false],
            class_sel: 0,
            approving: None,
            typing: None,
            confirm_x: false,
            note: None,
        };
        r.reload();
        // start on the biggest change
        if let Some(i) = (0..r.files.len()).max_by_key(|&i| r.files[i].added + r.files[i].removed) {
            if i != r.sel {
                r.sel = i;
                r.load_diff();
            }
        }
        r
    }

    pub fn reload(&mut self) {
        let wt = self.worktree.clone();
        let uncommitted: Vec<String> =
            git(&wt, &["status", "--porcelain", "--untracked-files=all"])
                .unwrap_or_default()
                .lines()
                .filter(|l| l.len() > 3)
                .map(|l| {
                    let p = &l[3..];
                    p.rsplit(" -> ")
                        .next()
                        .unwrap_or(p)
                        .trim_matches('"')
                        .to_string()
                })
                .collect();
        let mut files: Vec<RFile> = Vec::new();
        let status = git(&wt, &["diff", "--name-status", &self.base]).unwrap_or_default();
        let nums = git(&wt, &["diff", "--numstat", &self.base]).unwrap_or_default();
        for l in status.lines() {
            let mut it = l.split('\t');
            let st = it.next().unwrap_or("M").chars().next().unwrap_or('M');
            let path = it.next_back().unwrap_or("").to_string();
            if path.is_empty() {
                continue;
            }
            let (a, d) = nums
                .lines()
                .find(|n| n.split('\t').next_back() == Some(path.as_str()))
                .map(|n| {
                    let mut f = n.split('\t');
                    (
                        f.next().and_then(|x| x.parse().ok()).unwrap_or(0),
                        f.next().and_then(|x| x.parse().ok()).unwrap_or(0),
                    )
                })
                .unwrap_or((0, 0));
            files.push(RFile {
                status: st,
                committed: !uncommitted.contains(&path),
                path,
                added: a,
                removed: d,
            });
        }
        // new files git doesn't know yet
        for p in git(&wt, &["ls-files", "--others", "--exclude-standard"])
            .unwrap_or_default()
            .lines()
        {
            let lines = std::fs::read_to_string(wt.join(p))
                .map(|t| t.lines().count() as u64)
                .unwrap_or(0);
            files.push(RFile {
                status: 'A',
                path: p.to_string(),
                added: lines,
                removed: 0,
                committed: false,
            });
        }
        self.files = files;
        self.commits = git(
            &self.repo,
            &[
                "log",
                "--format=%h%x09%s",
                &format!("{}..{}", self.base, self.branch),
            ],
        )
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(h, s)| (h.to_string(), s.to_string()))
        .collect();
        self.sel = self.sel.min(self.files.len().saturating_sub(1));
        self.load_diff();
    }

    pub fn load_diff(&mut self) {
        self.scroll = 0;
        let Some(f) = self.files.get(self.sel) else {
            self.diff.clear();
            return;
        };
        let ctx = if self.view == 2 { "-U100000" } else { "-U3" };
        let text = if f.status == 'A' && !f.committed && !self.tracked(&f.path) {
            // untracked: against nothing (git exits 1 when there's a difference)
            std::process::Command::new("git")
                .arg("-C")
                .arg(&self.worktree)
                .args(["diff", "--no-index", ctx, "--", "/dev/null", &f.path])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        } else {
            git(&self.worktree, &["diff", ctx, &self.base, "--", &f.path]).unwrap_or_default()
        };
        self.diff = parse_diff(&text);
    }

    fn tracked(&self, p: &str) -> bool {
        git(&self.worktree, &["ls-files", "--error-unmatch", p]).is_some()
    }

    fn hunks(&self) -> Vec<usize> {
        let rows = if self.view == 1 {
            side_by_side(&self.diff)
                .iter()
                .enumerate()
                .filter(|(_, (l, _))| l.is_some_and(|l| l.kind == DKind::Hunk))
                .map(|(i, _)| i)
                .collect()
        } else {
            self.diff
                .iter()
                .enumerate()
                .filter(|(_, l)| l.kind == DKind::Hunk)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        // in the whole file there's one hunk: step through the changes instead
        if rows.len() <= 1 && self.view != 1 {
            let mut v = Vec::new();
            let mut prev = DKind::Ctx;
            for (i, l) in self.diff.iter().enumerate() {
                if matches!(l.kind, DKind::Add | DKind::Del)
                    && !matches!(prev, DKind::Add | DKind::Del)
                {
                    v.push(i.saturating_sub(2));
                }
                prev = l.kind;
            }
            return v;
        }
        rows
    }

    /// The new-file line at the top of the diff view (for `e`).
    fn line_here(&self) -> Option<usize> {
        self.diff
            .iter()
            .skip(self.scroll)
            .find_map(|l| l.new)
            .map(|n| n as usize)
    }

    pub fn key(&mut self, key: &[u8]) -> Option<ReviewAct> {
        self.note = None;
        // two quick escs arrive as one alt+esc: two presses
        if key == b"\x1b\x1b" {
            return self.key(b"\x1b").or_else(|| self.key(b"\x1b"));
        }
        if let Some(t) = self.typing.as_mut() {
            match key {
                b"\x1b" => self.typing = None,
                b"\r" => {
                    let t = std::mem::take(t);
                    self.typing = None;
                    if !t.trim().is_empty() {
                        return Some(ReviewAct::Message(t));
                    }
                }
                [0x7f] | [0x08] => {
                    t.pop();
                }
                k if k.first().is_some_and(|b| *b >= 0x20) && !k.starts_with(b"\x1b") => {
                    t.push_str(&String::from_utf8_lossy(k));
                }
                _ => {}
            }
            return None;
        }
        if self.confirm_x {
            self.confirm_x = false;
            if key == b"x" {
                return Some(ReviewAct::Discard);
            }
            self.note = Some("kept".into());
            return None;
        }
        if self.focus_approve {
            match key {
                b"\x1b" | b"\t" => self.focus_approve = false,
                b"\x1b[D" | b"h" => self.dur = self.dur.saturating_sub(1),
                b"\x1b[C" | b"l" => self.dur = (self.dur + 1).min(DURATIONS.len() - 1),
                b"\x1b[A" | b"k" => self.class_sel = self.class_sel.saturating_sub(1),
                b"\x1b[B" | b"j" => self.class_sel = (self.class_sel + 1).min(CLASSES.len() - 1),
                b" " => self.classes[self.class_sel] = !self.classes[self.class_sel],
                b"\r" | b"A" => return Some(self.approve_act()),
                _ => {}
            }
            return None;
        }
        let page = 10usize;
        match key {
            b"\x1b" | b"q" => return Some(ReviewAct::Close),
            b"\x1b[A" | b"k" => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.load_diff();
                }
            }
            b"\x1b[B" | b"j" => {
                if self.sel + 1 < self.files.len() {
                    self.sel += 1;
                    self.load_diff();
                }
            }
            b"]f" | b"]" => {
                if !self.files.is_empty() {
                    self.sel = (self.sel + 1) % self.files.len();
                    self.load_diff();
                }
            }
            b"[" => {
                if !self.files.is_empty() {
                    self.sel = (self.sel + self.files.len() - 1) % self.files.len();
                    self.load_diff();
                }
            }
            b"n" | b"N" => {
                let h = self.hunks();
                let next = if key == b"n" {
                    h.iter().find(|&&i| i > self.scroll).copied()
                } else {
                    h.iter().rev().find(|&&i| i < self.scroll).copied()
                };
                match next {
                    Some(i) => self.scroll = i,
                    None => {
                        self.note = Some(
                            if key == b"n" {
                                "last change"
                            } else {
                                "first change"
                            }
                            .into(),
                        )
                    }
                }
            }
            b"\x1b[6~" | b" " => self.scroll += page,
            b"\x1b[5~" => self.scroll = self.scroll.saturating_sub(page),
            b"J" => self.scroll += 1,
            b"K" => self.scroll = self.scroll.saturating_sub(1),
            b"v" => {
                self.view = (self.view + 1) % 3;
                self.load_diff();
            }
            b"m" => return Some(ReviewAct::Merge),
            b"S" => return Some(ReviewAct::Squash),
            b"t" => self.typing = Some(String::new()),
            b"o" => {
                if let Some(f) = self.files.get(self.sel) {
                    return Some(ReviewAct::OpenFiles(self.worktree.join(&f.path)));
                }
            }
            b"e" => {
                if let Some(f) = self.files.get(self.sel) {
                    return Some(ReviewAct::Edit(
                        self.worktree.join(&f.path),
                        self.line_here(),
                    ));
                }
            }
            b"x" => {
                self.confirm_x = true;
            }
            b"A" | b"\t" => self.focus_approve = true,
            b"r" => {
                self.reload();
                self.note = Some("read again".into());
            }
            _ => {}
        }
        None
    }

    fn approve_act(&mut self) -> ReviewAct {
        if self.approving.is_some() {
            return ReviewAct::Approve(None);
        }
        let kinds: Vec<Action> = CLASSES
            .iter()
            .zip(self.classes)
            .filter(|(_, on)| *on)
            .flat_map(|((_, a), _)| a.iter().copied())
            .collect();
        if kinds.is_empty() {
            self.note = Some("tick at least one kind (space)".into());
            return ReviewAct::Approve(None);
        }
        let d = DURATIONS[self.dur].1.map(Duration::from_secs);
        ReviewAct::Approve(Some((d, kinds)))
    }

    // ------------------------------------------------------------ drawing

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let shell = role(Role::Shell);
        // header
        let hr = Rect::new(area.x, area.y, area.width, 1);
        buf.set_style(hr, Style::default().bg(BAR));
        let x = put_w(
            buf,
            hr,
            area.x + 1,
            area.y,
            "▍shellvisor",
            st(AMBER).bg(BAR).add_modifier(Modifier::BOLD),
        );
        let x = put_w(buf, hr, x + 2, area.y, "review · ", st(DIM).bg(BAR));
        let x = put_w(
            buf,
            hr,
            x,
            area.y,
            &format!("● {}", self.agent),
            st(self.color).bg(BAR),
        );
        put(
            buf,
            hr,
            x,
            area.y,
            &format!(" · {} → {}", self.branch, self.into),
            st(DIM).bg(BAR),
        );
        let (add, del): (u64, u64) = self
            .files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.added, d + f.removed));
        let mut pill: Vec<(String, Style)> = vec![
            (self.status.clone(), st(self.status_color).bg(BAR)),
            (
                format!(
                    " · {} file{} · ",
                    self.files.len(),
                    if self.files.len() == 1 { "" } else { "s" }
                ),
                st(TEXT).bg(BAR),
            ),
            (format!("+{add}"), st(GREEN).bg(BAR)),
            (" ".into(), st(TEXT).bg(BAR)),
            (format!("−{del}"), st(RED).bg(BAR)),
        ];
        if let Some(t) = self.tokens {
            pill.push((format!(" · {} tokens", short_num(t)), st(TEXT).bg(BAR)));
        }
        let clock = hms(now_secs());
        let clock: String = clock.chars().take(8).collect();
        let pw: u16 = pill
            .iter()
            .map(|(t, _)| t.chars().count() as u16)
            .sum::<u16>()
            + 2;
        let cx = area
            .right()
            .saturating_sub(clock.chars().count() as u16 + 2);
        put(buf, hr, cx, area.y, &clock, st(TEXT).bg(BAR));
        let mut px = cx.saturating_sub(pw + 2);
        px = put_w(buf, hr, px, area.y, " ", st(TEXT).bg(BAR));
        for (t, s) in &pill {
            px = put_w(buf, hr, px, area.y, t, *s);
        }

        let body = Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        );
        let lw = ratio(body.width, 300, 1280).max(30);
        let left = Rect::new(body.x + 1, body.y, lw, body.height);
        let rx = left.right() + 1;
        let right = Rect::new(rx, body.y, body.right().saturating_sub(rx + 1), body.height);
        let ap_h = 9u16.min(left.height / 2);
        let files_r = Rect::new(left.x, left.y, left.width, left.height.saturating_sub(ap_h));
        let ap_r = Rect::new(left.x, files_r.bottom(), left.width, ap_h);

        // ---- files, commits, checks
        let fi = boxed(buf, files_r, BORDER, PANEL, None);
        put(
            buf,
            fi,
            fi.x + 1,
            fi.y,
            "FILES",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        let nc = self.commits.len();
        put_right(
            buf,
            fi,
            fi.y,
            &format!("{nc} commit{}", if nc == 1 { "" } else { "s" }),
            st(DIM).bg(PANEL),
        );
        let mut y = fi.y + 1;
        let checks_h = self.checks.len() as u16 * 2 + 1;
        let commits_h = (nc.min(4) as u16).max(1) + 1;
        // a short window: no blank lines or rules between the parts
        let tight = fi.height < self.files.len().min(6) as u16 + checks_h + commits_h + 6;
        let gaps = if tight { 0 } else { 4 };
        let room = fi.bottom().saturating_sub(y + checks_h + commits_h + gaps) as usize;
        let first = self.sel.saturating_sub(room.saturating_sub(1));
        if self.files.is_empty() {
            put(buf, fi, fi.x + 1, y, "no changes yet", st(DIM).bg(PANEL));
            y += 1;
        }
        for (i, f) in self.files.iter().enumerate().skip(first).take(room.max(1)) {
            let bg = if i == self.sel && !self.focus_approve {
                SELECT
            } else {
                PANEL
            };
            buf.set_style(Rect::new(fi.x, y, fi.width, 1), Style::default().bg(bg));
            let sc = match f.status {
                'A' => GREEN,
                'D' => RED,
                _ => AMBER,
            };
            put_w(buf, fi, fi.x + 1, y, &f.status.to_string(), st(sc).bg(bg));
            let nums = if f.removed > 0 {
                format!("+{} −{}", f.added, f.removed)
            } else {
                format!("+{}", f.added)
            };
            let nw = nums.chars().count() as u16;
            let pw = fi.width.saturating_sub(nw + 5);
            let mark = if f.committed { "" } else { "*" };
            put_n(
                buf,
                fi.x + 3,
                y,
                &format!("{}{mark}", tail(&f.path, pw as usize)),
                st(TEXT).bg(bg),
                pw,
            );
            let nx = fi.right().saturating_sub(nw + 1);
            let x = put_w(buf, fi, nx, y, &format!("+{}", f.added), st(GREEN).bg(bg));
            if f.removed > 0 {
                put(
                    buf,
                    fi,
                    x + 1,
                    y,
                    &format!("−{}", f.removed),
                    st(RED).bg(bg),
                );
            }
            y += 1;
        }
        let rule = "─".repeat(fi.width.saturating_sub(2) as usize);
        if !tight {
            y += 1;
            put(
                buf,
                fi,
                fi.x + 1,
                y.min(fi.bottom().saturating_sub(1)),
                &rule,
                st(BORDER).bg(PANEL),
            );
            y += 1;
        }
        put(
            buf,
            fi,
            fi.x + 1,
            y,
            "Commits",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        y += 1;
        if self.commits.is_empty() {
            put(buf, fi, fi.x + 1, y, "none yet", st(DIM).bg(PANEL));
            y += 1;
        }
        for (i, (h, s)) in self.commits.iter().take(4).enumerate() {
            let x = put_w(buf, fi, fi.x + 1, y, h, st(AMBER).bg(PANEL));
            put(
                buf,
                fi,
                x + 1,
                y,
                s,
                st(if i == 0 { TEXT } else { DIM }).bg(PANEL),
            );
            y += 1;
        }
        if nc > 4 {
            put(
                buf,
                fi,
                fi.x + 1,
                y,
                &format!("+{} more", nc - 4),
                st(DIM).bg(PANEL),
            );
            y += 1;
        }
        if !tight {
            y += 1;
            put(buf, fi, fi.x + 1, y, &rule, st(BORDER).bg(PANEL));
            y += 1;
        }
        put(
            buf,
            fi,
            fi.x + 1,
            y,
            "Checks",
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        y += 1;
        for (icon, c, parts) in &self.checks {
            if y >= fi.bottom() {
                break;
            }
            let mut x = put_w(buf, fi, fi.x + 1, y, icon, st(*c).bg(PANEL)) + 1;
            // wrap onto a second line when long
            for (t, s) in parts {
                for word in t.split_inclusive(' ') {
                    let w = word.chars().count() as u16;
                    if x + w > fi.right() && x > fi.x + 3 {
                        y += 1;
                        x = fi.x + 1;
                    }
                    x = put_w(buf, fi, x, y, word, s.bg(PANEL));
                }
            }
            y += 1;
        }

        // ---- approve for a while
        let ai = boxed(buf, ap_r, AMBER, PANEL, None);
        put(
            buf,
            ai,
            ai.x + 1,
            ai.y,
            &format!("Approve for {}", self.agent),
            st(TEXT).bg(PANEL).add_modifier(Modifier::BOLD),
        );
        put(
            buf,
            ai,
            ai.x + 1,
            ai.y + 1,
            "answer its approvals by itself for",
            st(DIM).bg(PANEL),
        );
        let mut x = ai.x + 1;
        for (i, (label, _)) in DURATIONS.iter().enumerate() {
            let s = if i == self.dur {
                st(INK).bg(AMBER)
            } else {
                st(TEXT).bg(role(Role::Card))
            };
            x = put_w(buf, ai, x, ai.y + 2, &format!(" {label} "), s) + 1;
        }
        for (i, (label, _)) in CLASSES.iter().enumerate() {
            let on = self.classes[i];
            let cur = self.focus_approve && i == self.class_sel;
            let y = ai.y + 3 + i as u16;
            let bg = if cur { SELECT } else { PANEL };
            buf.set_style(Rect::new(ai.x, y, ai.width, 1), Style::default().bg(bg));
            let x = put_w(
                buf,
                ai,
                ai.x + 1,
                y,
                if on { "☑" } else { "☐" },
                st(if on { GREEN } else { DIM }).bg(bg),
            );
            put(
                buf,
                ai,
                x + 2,
                y,
                label,
                st(if on { TEXT } else { DIM }).bg(bg),
            );
        }
        let fy = ai.y + 3 + CLASSES.len() as u16;
        let w = ai.width.saturating_sub(2);
        let (choices, col): (Vec<String>, Color) = match &self.approving {
            Some(until) => (
                vec![
                    format!("● on · {until} · A turns it off"),
                    format!("● on · {until} · A off"),
                    "● on · A off".into(),
                ],
                GREEN,
            ),
            None => {
                let ends = match DURATIONS[self.dur].1 {
                    Some(s) => format!(
                        "ends {}",
                        hms(now_secs() + s as i64)
                            .chars()
                            .take(5)
                            .collect::<String>()
                    ),
                    None => "ends when it's done".into(),
                };
                (
                    vec![
                        format!("{ends} · its own worktree · A to turn on"),
                        format!("{ends} · A to turn on"),
                        "A to turn on".into(),
                    ],
                    DIM,
                )
            }
        };
        let refs: Vec<&str> = choices.iter().map(String::as_str).collect();
        let text = fit(&refs, w);
        put(buf, ai, ai.x + 1, fy, text, st(DIM).bg(PANEL));
        if let Some(rest) = text.strip_prefix("● on") {
            put(buf, ai, ai.x + 1, fy, "● on", st(col).bg(PANEL));
            let _ = rest;
        }
        if let Some(i) = text.find(" A ") {
            let x = ai.x + 1 + text[..i].chars().count() as u16 + 1;
            put(buf, ai, x, fy, "A", st(AMBER).bg(PANEL));
        }

        // ---- the diff
        let title = self
            .files
            .get(self.sel)
            .map(|f| f.path.clone())
            .unwrap_or_else(|| "no changes".into());
        let di = boxed(
            buf,
            right,
            BORDER,
            shell,
            Some((&title, st(TEXT).bg(shell).add_modifier(Modifier::BOLD))),
        );
        let mut x = di.x + 1;
        for (i, label) in ["unified", "side by side", "whole file"].iter().enumerate() {
            let s = if i as u8 == self.view {
                st(AMBER).bg(role(Role::Card))
            } else {
                st(DIM).bg(role(Role::Card))
            };
            x = put_w(buf, di, x, di.y, &format!(" {label} "), s) + 1;
        }
        let hint = fit(
            &["n / N next change · ]f next file", "n / N · ]f", ""],
            di.right().saturating_sub(x + 2),
        );
        put_right(buf, di, di.y, hint, st(DIM).bg(shell));
        let bottom_h = 1u16;
        let dr = Rect::new(
            di.x,
            di.y + 1,
            di.width,
            di.height.saturating_sub(1 + bottom_h + 1),
        );
        let dd = boxed(buf, dr, BORDER, PANEL, None);
        let rows = dd.height as usize;
        let numw = 4u16;
        if self.diff.is_empty() {
            put(
                buf,
                dd,
                dd.x + 1,
                dd.y,
                if self.files.is_empty() {
                    "nothing to show"
                } else {
                    "no text changes (binary, or only a mode change)"
                },
                st(DIM).bg(PANEL),
            );
        }
        if self.view == 1 {
            let pairs = side_by_side(&self.diff);
            let half = dd.width / 2;
            for (r, (l, rr)) in pairs.iter().skip(self.scroll).take(rows).enumerate() {
                let y = dd.y + r as u16;
                for (side, line) in [(0u16, l), (1u16, rr)] {
                    let sx = dd.x + side * half;
                    let w = if side == 0 { half } else { dd.width - half };
                    let Some(line) = line else {
                        buf.set_style(
                            Rect::new(sx, y, w, 1),
                            Style::default().bg(role(Role::Track)),
                        );
                        continue;
                    };
                    if line.kind == DKind::Hunk {
                        put_n(
                            buf,
                            sx + 1,
                            y,
                            &line.text,
                            st(DIM).bg(PANEL),
                            w.saturating_sub(1),
                        );
                        continue;
                    }
                    let (bg, fg) = match (line.kind, side) {
                        (DKind::Del, _) => (role(Role::DelBg), RED),
                        (DKind::Add, _) => (role(Role::AddBg), GREEN),
                        _ => (PANEL, TEXT),
                    };
                    buf.set_style(Rect::new(sx, y, w, 1), Style::default().bg(bg));
                    let n = if side == 0 { line.old } else { line.new };
                    let ns = n.map(|n| format!("{n:>4}")).unwrap_or_default();
                    put_n(buf, sx, y, &ns, st(DIM).bg(bg), numw);
                    put_n(
                        buf,
                        sx + numw + 1,
                        y,
                        &line.text,
                        st(fg).bg(bg),
                        w.saturating_sub(numw + 2),
                    );
                }
            }
        } else {
            for (r, line) in self.diff.iter().skip(self.scroll).take(rows).enumerate() {
                let y = dd.y + r as u16;
                match line.kind {
                    DKind::Hunk => {
                        put_n(
                            buf,
                            dd.x + 1,
                            y,
                            &line.text,
                            st(DIM).bg(PANEL),
                            dd.width.saturating_sub(1),
                        );
                    }
                    k => {
                        let (bg, fg, mark) = match k {
                            DKind::Add => (role(Role::AddBg), GREEN, "+"),
                            DKind::Del => (role(Role::DelBg), RED, "-"),
                            _ => (PANEL, TEXT, " "),
                        };
                        let n = line
                            .new
                            .or(line.old)
                            .map(|n| format!("{n:>4}"))
                            .unwrap_or_default();
                        if k != DKind::Ctx {
                            buf.set_style(
                                Rect::new(dd.x, y, dd.width.saturating_sub(1), 1),
                                Style::default().bg(bg),
                            );
                        }
                        put_n(buf, dd.x, y, &n, st(DIM).bg(bg), numw);
                        put_n(buf, dd.x + numw + 1, y, mark, st(fg).bg(bg), 1);
                        put_n(
                            buf,
                            dd.x + numw + 2,
                            y,
                            &line.text,
                            st(if k == DKind::Ctx { TEXT } else { fg }).bg(bg),
                            dd.width.saturating_sub(numw + 3),
                        );
                    }
                }
            }
        }
        // the buttons under the diff
        let by = di.bottom().saturating_sub(1);
        if let Some(t) = &self.typing {
            let x = put_w(
                buf,
                di,
                di.x + 1,
                by,
                &format!(" message {}: ", self.agent),
                st(INK).bg(AMBER).add_modifier(Modifier::BOLD),
            );
            put(
                buf,
                di,
                x + 1,
                by,
                &format!("{t}▏ · ⏎ send · esc cancel"),
                st(TEXT).bg(shell),
            );
        } else if self.confirm_x {
            put(
                buf,
                di,
                di.x + 1,
                by,
                &format!(
                    " x again throws away {} and its worktree · any other key keeps it ",
                    self.branch
                ),
                st(INK).bg(RED).add_modifier(Modifier::BOLD),
            );
        } else {
            let x = put_w(
                buf,
                di,
                di.x + 1,
                by,
                &format!(" m Merge into {} ", self.into),
                st(INK).bg(AMBER).add_modifier(Modifier::BOLD),
            ) + 1;
            let x = put_w(buf, di, x, by, " S Squash ", st(TEXT).bg(role(Role::Card))) + 1;
            let x = put_w(
                buf,
                di,
                x,
                by,
                &format!(" t Message {} ", self.agent),
                st(TEXT).bg(role(Role::Card)),
            ) + 1;
            put(
                buf,
                di,
                x + 1,
                by,
                fit(
                    &[
                        "o open in files · e open in $EDITOR at this line · x throw away the branch",
                        "o files · e $EDITOR · x throw away",
                        "o · e · x",
                    ],
                    di.right().saturating_sub(x + 2),
                ),
                st(DIM).bg(shell),
            );
        }

        // key strip
        let ky = area.bottom() - 1;
        buf.set_style(
            Rect::new(area.x, ky, area.width, 1),
            Style::default().bg(BAR),
        );
        let mut x = area.x + 1;
        for (k, label) in [
            ("↑↓", "Files"),
            ("n", "Next change"),
            ("v", "Side by side"),
            ("m", "Merge"),
            ("t", "Message"),
            ("A", "Approve for a while"),
            ("esc", "Back"),
        ] {
            x = put_w(buf, area, x, ky, k, st(AMBER).bg(BAR)) + 1;
            x = put_w(buf, area, x, ky, label, st(DIM).bg(BAR)) + 3;
        }
        put(
            buf,
            area,
            x,
            ky,
            "· ctrl+] D on an agent column · sv agent diff NAME",
            st(DIM).bg(BAR),
        );
        if let Some(n) = &self.note {
            let w = n.chars().count() as u16 + 2;
            put(
                buf,
                area,
                area.right().saturating_sub(w + 1),
                ky,
                &format!(" {n} "),
                st(INK).bg(AMBER),
            );
        }
        let _ = CYAN;
    }
}

/// The newest test summary in an agent's output: (passed?, what ran, counts).
pub fn test_summary(lines: &[String]) -> Option<(bool, String, String)> {
    let re_rust =
        regex_lite::Regex::new(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed").ok()?;
    let re_py = regex_lite::Regex::new(r"=+ (?:(\d+) failed, )?(\d+) passed").ok()?;
    let re_js = regex_lite::Regex::new(r"Tests:\s+(?:(\d+) failed, )?(\d+) passed").ok()?;
    let re_go = regex_lite::Regex::new(r"^(ok|FAIL)\s+\S+\s+[\d.]+s").ok()?;
    for (i, l) in lines.iter().enumerate().rev() {
        let what = |default: &str| -> String {
            // the command that ran, a few lines above
            lines[i.saturating_sub(60)..i]
                .iter()
                .rev()
                .find_map(|x| {
                    [
                        "cargo test",
                        "pytest",
                        "npm test",
                        "npm run test",
                        "pnpm test",
                        "yarn test",
                        "go test",
                        "jest",
                        "vitest",
                    ]
                    .iter()
                    .find(|c| x.contains(*c))
                    .map(|c| c.to_string())
                })
                .unwrap_or_else(|| default.to_string())
        };
        if let Some(c) = re_rust.captures(l) {
            let ok = &c[1] == "ok" && &c[3] == "0";
            return Some((
                ok,
                what("cargo test"),
                format!("{} passed, {} failed", &c[2], &c[3]).replace(", 0 failed", ""),
            ));
        }
        if let Some(c) = re_js.captures(l) {
            let failed = c.get(1).map(|m| m.as_str()).unwrap_or("0");
            return Some((
                failed == "0",
                what("tests"),
                format!(
                    "{} passed{}",
                    &c[2],
                    if failed == "0" {
                        String::new()
                    } else {
                        format!(", {failed} failed")
                    }
                ),
            ));
        }
        if let Some(c) = re_py.captures(l) {
            let failed = c.get(1).map(|m| m.as_str()).unwrap_or("0");
            return Some((
                failed == "0",
                what("pytest"),
                format!(
                    "{} passed{}",
                    &c[2],
                    if failed == "0" {
                        String::new()
                    } else {
                        format!(", {failed} failed")
                    }
                ),
            ));
        }
        if let Some(c) = re_go.captures(l) {
            return Some((
                &c[1] == "ok",
                what("go test"),
                if &c[1] == "ok" {
                    "passed".into()
                } else {
                    "failed".into()
                },
            ));
        }
    }
    None
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn short_num(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// The end of a path that fits in `w` (…/routes/alerts.rs).
fn tail(p: &str, w: usize) -> String {
    let n = p.chars().count();
    if n <= w {
        return p.to_string();
    }
    let keep: String = p.chars().skip(n + 1 - w.max(2)).collect();
    format!("…{keep}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lines_and_side_by_side() {
        let d = parse_diff(
            "diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n@@ -14,4 +14,5 @@ fn routes\n a\n-b\n+B\n+C\n c\n\\ No newline at end of file\n",
        );
        assert_eq!(d.len(), 6);
        assert_eq!(d[0].kind, DKind::Hunk);
        assert_eq!((d[1].old, d[1].new), (Some(14), Some(14)));
        assert_eq!((d[2].kind, d[2].old), (DKind::Del, Some(15)));
        assert_eq!((d[3].kind, d[3].new), (DKind::Add, Some(15)));
        assert_eq!((d[5].old, d[5].new), (Some(16), Some(17)));
        let s = side_by_side(&d);
        // hunk, a, b|B, -|C, c
        assert_eq!(s.len(), 5);
        assert_eq!(s[2].0.unwrap().text, "b");
        assert_eq!(s[2].1.unwrap().text, "B");
        assert!(s[3].0.is_none() && s[3].1.unwrap().text == "C");
        assert_eq!(tail("src/routes/alerts.rs", 10), "…alerts.rs");
        let out: Vec<String> = [
            "$ cargo test",
            "running 48 tests",
            "test result: ok. 48 passed; 0 failed; 0 ignored",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            test_summary(&out),
            Some((true, "cargo test".into(), "48 passed".into()))
        );
        let py = vec!["===== 2 failed, 10 passed in 0.3s =====".to_string()];
        assert_eq!(test_summary(&py).map(|t| t.0), Some(false));
    }

    /// Board 21 from fixed data, compared with tests/snapshots.
    #[test]
    fn board_21_snapshots() {
        use ratatui::style::Style;
        let diff = parse_diff(
            "@@ -14,9 +14,20 @@ use crate::models::Alert;\n pub fn routes() -> Router {\n     Router::new()\n         .route(\"/alerts\", get(list_alerts))\n-        .route(\"/alerts/:id\", get(get_alert))\n+        .route(\"/alerts\", post(create_alert))\n+        .route(\"/alerts/:id\", get(get_alert))\n }\n \n+/// POST /alerts: validate, store, return 201 with the new alert.\n+async fn create_alert(\n+    State(db): State<Db>,\n+    Json(new): Json<NewAlert>,\n+) -> Result<(StatusCode, Json<Alert>), ApiError> {\n+    new.validate()?;\n+    let alert = db.insert_alert(&new).await?;\n+    Ok((StatusCode::CREATED, Json(alert)))\n+}\n",
        );
        let f = |s: char, p: &str, a: u64, d: u64| RFile {
            status: s,
            path: p.into(),
            added: a,
            removed: d,
            committed: true,
        };
        let r = Review {
            agent: "api".into(),
            color: CYAN,
            status: "✔ done".into(),
            status_color: GREEN,
            branch: "agent/api".into(),
            into: "main".into(),
            repo: PathBuf::from("/w"),
            worktree: PathBuf::from("/w/.shellvisor/worktrees/api"),
            base: "abc".into(),
            tokens: Some(41_000),
            files: vec![
                f('M', "src/routes/alerts.rs", 48, 6),
                f('A', "src/models/alert.rs", 27, 0),
                f('M', "src/main.rs", 3, 1),
                f('M', "tests/alerts.rs", 8, 5),
            ],
            commits: vec![
                ("a41c9e2".into(), "add POST /alerts".into()),
                ("7be0d13".into(), "tests for alert validation".into()),
            ],
            checks: vec![
                (
                    "✔",
                    GREEN,
                    vec![
                        ("cargo test".into(), Style::default().fg(TEXT)),
                        (" · 48 passed".into(), Style::default().fg(DIM)),
                    ],
                ),
                (
                    "✔",
                    GREEN,
                    vec![
                        ("no overlap with ".into(), Style::default().fg(TEXT)),
                        ("● review".into(), Style::default().fg(Color::Indexed(170))),
                    ],
                ),
                (
                    "⚑",
                    AMBER,
                    vec![
                        ("touches ".into(), Style::default().fg(TEXT)),
                        ("src/main.rs".into(), Style::default().fg(TEXT)),
                        (" · you edited it 1h ago".into(), Style::default().fg(DIM)),
                    ],
                ),
            ],
            sel: 0,
            view: 0,
            scroll: 0,
            diff,
            focus_approve: false,
            dur: 1,
            classes: [true, true, false],
            class_sel: 0,
            approving: None,
            typing: None,
            confirm_x: false,
            note: None,
        };
        let mut failed = Vec::new();
        for (w, h) in [(160u16, 44u16), (100, 24)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            r.render(area, &mut buf);
            if let Some(e) = crate::mcpscreen::snap::check(
                &format!("review21-{w}x{h}.txt"),
                &crate::mcpscreen::snap::text(&buf),
            ) {
                failed.push(e);
            }
        }
        assert!(
            failed.is_empty(),
            "board 21 drifted:\n{}",
            failed.join("\n")
        );
    }
}

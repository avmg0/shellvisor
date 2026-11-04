//! The files pane: Tree, List, Git changes and Find views, with a preview.
//!
//! Folders are read lazily (only what's expanded) and re-read only when
//! their modification time changes. Slow work — git status, building the
//! file index, searching contents, diffs — is described as a [`Job`] that
//! the app runs on a short-lived thread; the result comes back as [`Done`].
//! This module never spawns anything itself, so it is easy to test.

use crate::gitstat::{self, GitMark, GitStatus};
use crate::highlight;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(240);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);
const SEL_BG: Color = Color::Indexed(237);
const INK: Color = Color::Indexed(16);

/// Most entries read from one folder.
const MAX_DIR: usize = 5000;
/// Most files indexed for Find.
const MAX_INDEX: usize = 20_000;
const MAX_HITS: usize = 500;
const PREVIEW_BYTES: usize = 64 * 1024;
pub const POLL: Duration = Duration::from_secs(3);

/// Folders never walked when there's no .gitignore to tell us.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cache",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".gradle",
    "Pods",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Tree,
    List,
    Git,
    Find,
}

impl View {
    pub const ALL: [View; 4] = [View::Tree, View::List, View::Git, View::Find];
    pub fn name(self) -> &'static str {
        match self {
            View::Tree => "tree",
            View::List => "list",
            View::Git => "git",
            View::Find => "find",
        }
    }
    pub fn parse(s: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.name() == s)
    }
}

/// Slow work for the app to run off the UI thread.
#[derive(Debug, Clone)]
pub enum Job {
    Git {
        root: PathBuf,
    },
    Index {
        root: PathBuf,
        seq: u64,
    },
    Grep {
        root: PathBuf,
        query: String,
        seq: u64,
        cancel: Arc<AtomicU64>,
    },
    Diff {
        root: PathBuf,
        file: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub enum Done {
    Git {
        root: PathBuf,
        status: Option<GitStatus>,
    },
    Index {
        root: PathBuf,
        seq: u64,
        files: Vec<String>,
        truncated: bool,
    },
    Grep {
        seq: u64,
        hits: Vec<Hit>,
        truncated: bool,
    },
    Diff {
        file: PathBuf,
        text: String,
    },
}

/// Something the files pane wants done to the shell next to it.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// Type this path at the prompt.
    Insert(PathBuf),
    /// `cd` there.
    Cd(PathBuf),
    /// Open in $EDITOR (at a line, for search hits).
    Edit(PathBuf, Option<usize>),
    Notice(String),
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub jobs: Vec<Job>,
    pub acts: Vec<Act>,
    pub redraw: bool,
}

impl Outcome {
    fn redraw() -> Self {
        Outcome {
            redraw: true,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Relative to the search root.
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
struct Item {
    name: String,
    path: PathBuf,
    dir: bool,
    link: bool,
    size: u64,
    mtime: Option<SystemTime>,
}

#[derive(Debug)]
struct DirCache {
    mtime: Option<SystemTime>,
    items: Vec<Item>,
    more: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Entry { idx: (PathBuf, usize), depth: u16 },
    Up,
    More(usize, u16),
    Change(PathBuf, GitMark),
    Name(usize),
    Hit(usize),
    Note(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Editing {
    Filter,
    Query,
}

struct Preview {
    path: PathBuf,
    lines: Vec<Line<'static>>,
    note: String,
    /// First line shown.
    top: usize,
}

#[derive(Default)]
struct Find {
    query: String,
    contents: bool,
    index: Option<Vec<String>>,
    index_root: PathBuf,
    indexing: bool,
    index_truncated: bool,
    /// Matching index entries for the query (names mode).
    names: Vec<usize>,
    hits: Vec<Hit>,
    searched: String,
    searching: bool,
    truncated: bool,
    seq: u64,
    cancel: Arc<AtomicU64>,
}

pub struct FilesPane {
    pub root: PathBuf,
    pub view: View,
    pub hidden: bool,
    pub follow: bool,
    /// The shell folder we last followed (so browsing away sticks until the shell moves).
    pub seen_cwd: PathBuf,
    pub preview: bool,
    expanded: HashSet<PathBuf>,
    cache: HashMap<PathBuf, DirCache>,
    rows: Vec<Row>,
    pub sel: usize,
    scroll: Cell<usize>,
    /// Rows shown last frame (for paging).
    page: Cell<usize>,
    filter: String,
    editing: Option<Editing>,
    pub git: Option<GitStatus>,
    git_asked: Option<PathBuf>,
    /// The folder the last git answer was for.
    git_for: Option<PathBuf>,
    find: Find,
    prev: Option<Preview>,
    last_poll: Instant,
}

impl FilesPane {
    pub fn new(root: PathBuf) -> (Self, Vec<Job>) {
        let mut p = FilesPane {
            root: PathBuf::new(),
            view: View::Tree,
            hidden: false,
            follow: true,
            seen_cwd: PathBuf::new(),
            preview: false,
            expanded: HashSet::new(),
            cache: HashMap::new(),
            rows: Vec::new(),
            sel: 0,
            scroll: Cell::new(0),
            page: Cell::new(10),
            filter: String::new(),
            editing: None,
            git: None,
            git_asked: None,
            git_for: None,
            find: Find::default(),
            prev: None,
            last_poll: Instant::now(),
        };
        let jobs = p.set_root(root);
        (p, jobs)
    }

    // ------------------------------------------------------------ model

    /// Show another folder (the shell moved, or the user went up).
    pub fn set_root(&mut self, root: PathBuf) -> Vec<Job> {
        if root == self.root {
            return Vec::new();
        }
        self.root = root;
        self.cache.retain(|k, _| k.starts_with(&self.root));
        self.sel = 0;
        self.scroll.set(0);
        self.filter.clear();
        self.prev = None;
        self.load(&self.root.clone());
        let mut jobs = vec![self.git_job()];
        if self.view == View::Find {
            self.find = Find::default();
            jobs.extend(self.ensure_index());
        }
        self.rebuild();
        jobs
    }

    fn git_job(&mut self) -> Job {
        self.git_asked = Some(self.root.clone());
        Job::Git {
            root: self.root.clone(),
        }
    }

    /// Re-read changed folders and ask for a fresh git status.
    pub fn refresh(&mut self) -> Vec<Job> {
        self.reload_changed(true);
        self.rebuild();
        vec![self.git_job()]
    }

    /// Cheap check while visible: re-read folders whose mtime changed.
    pub fn poll(&mut self) -> Outcome {
        self.last_poll = Instant::now();
        if self.reload_changed(false) {
            self.rebuild();
            Outcome {
                jobs: vec![self.git_job()],
                redraw: true,
                ..Default::default()
            }
        } else {
            Outcome::default()
        }
    }

    pub fn poll_due_in(&self) -> Duration {
        POLL.saturating_sub(self.last_poll.elapsed())
    }

    fn reload_changed(&mut self, force: bool) -> bool {
        let dirs: Vec<PathBuf> = self.cache.keys().cloned().collect();
        let mut changed = false;
        for d in dirs {
            let m = std::fs::metadata(&d).and_then(|m| m.modified()).ok();
            let stale = self.cache.get(&d).map(|c| c.mtime != m).unwrap_or(true);
            if force || stale {
                if m.is_none() && !d.exists() {
                    self.cache.remove(&d);
                    self.expanded.remove(&d);
                } else {
                    self.load(&d);
                }
                changed |= stale;
            }
        }
        changed
    }

    fn load(&mut self, dir: &Path) {
        let mtime = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        let mut items = Vec::new();
        let mut more = 0;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if items.len() >= MAX_DIR {
                    more += 1;
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                let ft = e.file_type().ok();
                let link = ft.map(|t| t.is_symlink()).unwrap_or(false);
                // follow symlinks for dir-ness and size
                let meta = std::fs::metadata(e.path()).ok();
                let dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                items.push(Item {
                    name,
                    path: e.path(),
                    dir,
                    link,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    mtime: meta.and_then(|m| m.modified().ok()),
                });
            }
        }
        items.sort_by(|a, b| {
            b.dir
                .cmp(&a.dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.cache
            .insert(dir.to_path_buf(), DirCache { mtime, items, more });
    }

    fn visible_item(&self, it: &Item) -> bool {
        (self.hidden || !it.name.starts_with('.'))
            && (self.filter.is_empty()
                || it.dir && self.expanded.contains(&it.path)
                || matches(&self.filter, &it.name))
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        match self.view {
            View::Tree => {
                let root = self.root.clone();
                self.tree_rows(&root, 0, &mut rows);
                if rows.is_empty() {
                    rows.push(Row::Note(if self.filter.is_empty() {
                        "empty folder".into()
                    } else {
                        "nothing matches".into()
                    }));
                }
            }
            View::List => {
                if self.root.parent().is_some() {
                    rows.push(Row::Up);
                }
                if let Some(c) = self.cache.get(&self.root) {
                    for (i, it) in c.items.iter().enumerate() {
                        if self.visible_item(it) {
                            rows.push(Row::Entry {
                                idx: (self.root.clone(), i),
                                depth: 0,
                            });
                        }
                    }
                    if c.more > 0 {
                        rows.push(Row::More(c.more, 0));
                    }
                }
            }
            View::Git => match &self.git {
                _ if self.git_for.as_deref() != Some(&self.root) => {
                    rows.push(Row::Note("reading git status…".into()))
                }
                Some(g) => {
                    for (p, m) in g.entries() {
                        let rel = p.strip_prefix(&self.root).unwrap_or(p);
                        if self.filter.is_empty() || matches(&self.filter, &rel.to_string_lossy()) {
                            rows.push(Row::Change(p.to_path_buf(), m));
                        }
                    }
                    if rows.is_empty() {
                        rows.push(Row::Note("no changes — working tree clean".into()));
                    }
                }
                None => rows.push(Row::Note("not a git repository".into())),
            },
            View::Find => {
                let f = &self.find;
                if f.contents {
                    rows.extend((0..f.hits.len()).map(Row::Hit));
                    if f.hits.is_empty() {
                        rows.push(Row::Note(if f.searching {
                            "searching…".into()
                        } else if f.query.is_empty() {
                            "type text to search inside files, ⏎ to run".into()
                        } else if f.searched == f.query {
                            "no matches".into()
                        } else {
                            "⏎ to search".into()
                        }));
                    }
                } else {
                    rows.extend(f.names.iter().copied().map(Row::Name));
                    if rows.is_empty() {
                        rows.push(Row::Note(if f.indexing {
                            "indexing…".into()
                        } else if f.query.is_empty() {
                            "type part of a file name".into()
                        } else {
                            "no matches".into()
                        }));
                    }
                }
            }
        }
        self.rows = rows;
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    fn tree_rows(&self, dir: &Path, depth: u16, rows: &mut Vec<Row>) {
        let Some(c) = self.cache.get(dir) else { return };
        for (i, it) in c.items.iter().enumerate() {
            if !self.visible_item(it) {
                continue;
            }
            rows.push(Row::Entry {
                idx: (dir.to_path_buf(), i),
                depth,
            });
            if it.dir && self.expanded.contains(&it.path) {
                self.tree_rows(&it.path, depth + 1, rows);
            }
        }
        if c.more > 0 {
            rows.push(Row::More(c.more, depth));
        }
    }

    fn item(&self, idx: &(PathBuf, usize)) -> Option<&Item> {
        self.cache.get(&idx.0).and_then(|c| c.items.get(idx.1))
    }

    /// Path of the selected row, and whether it's a folder.
    pub fn selected(&self) -> Option<(PathBuf, bool, Option<usize>)> {
        match self.rows.get(self.sel)? {
            Row::Entry { idx, .. } => self.item(idx).map(|i| (i.path.clone(), i.dir, None)),
            Row::Up => self.root.parent().map(|p| (p.to_path_buf(), true, None)),
            Row::Change(p, _) => Some((p.clone(), p.is_dir(), None)),
            Row::Name(i) => {
                let rel = self.find.index.as_ref()?.get(*i)?;
                Some((self.find.index_root.join(rel), false, None))
            }
            Row::Hit(i) => {
                let h = self.find.hits.get(*i)?;
                Some((self.root.join(&h.path), false, Some(h.line)))
            }
            Row::More(..) | Row::Note(_) => None,
        }
    }

    // ------------------------------------------------------------ jobs

    pub fn done(&mut self, d: Done) -> Outcome {
        match d {
            Done::Git { root, status } => {
                if root != self.root {
                    return Outcome::default();
                }
                self.git = status;
                self.git_for = Some(root);
            }
            Done::Index {
                root,
                seq,
                files,
                truncated,
            } => {
                if root != self.root || seq != self.find.seq {
                    return Outcome::default();
                }
                self.find.index = Some(files);
                self.find.index_root = root;
                self.find.indexing = false;
                self.find.index_truncated = truncated;
                self.match_names();
            }
            Done::Grep {
                seq,
                hits,
                truncated,
            } => {
                if seq != self.find.seq {
                    return Outcome::default();
                }
                self.find.hits = hits;
                self.find.searching = false;
                self.find.truncated = truncated;
                self.sel = 0;
            }
            Done::Diff { file, text } => {
                if self.prev.as_ref().map(|p| &p.path) == Some(&file) {
                    let lines = text.lines().map(diff_line).collect::<Vec<_>>();
                    let note = if lines.is_empty() {
                        "no diff (untracked or binary)".into()
                    } else {
                        String::new()
                    };
                    self.prev = Some(Preview {
                        path: file,
                        lines,
                        note,
                        top: 0,
                    });
                }
                return Outcome::redraw();
            }
        }
        self.rebuild();
        Outcome::redraw()
    }

    fn ensure_index(&mut self) -> Vec<Job> {
        if self.find.index.is_some() && self.find.index_root == self.root || self.find.indexing {
            return Vec::new();
        }
        self.find.indexing = true;
        self.find.seq += 1;
        vec![Job::Index {
            root: self.root.clone(),
            seq: self.find.seq,
        }]
    }

    fn match_names(&mut self) {
        let Some(index) = &self.find.index else {
            return;
        };
        let q = self.find.query.trim();
        if q.is_empty() {
            self.find.names.clear();
            return;
        }
        let mut scored: Vec<(bool, usize, usize)> = index
            .iter()
            .enumerate()
            .filter(|(_, p)| q.split_whitespace().all(|w| matches(w, p)))
            .map(|(i, p)| {
                let base = p.rsplit('/').next().unwrap_or(p);
                let first = q.split_whitespace().next().unwrap_or("");
                (!matches(first, base), p.len(), i)
            })
            .collect();
        scored.sort();
        self.find.names = scored
            .into_iter()
            .take(MAX_HITS)
            .map(|(_, _, i)| i)
            .collect();
        self.sel = 0;
    }

    fn run_grep(&mut self) -> Vec<Job> {
        let q = self.find.query.trim().to_string();
        if q.is_empty() {
            return Vec::new();
        }
        self.find.seq += 1;
        self.find.cancel.store(self.find.seq, Ordering::Relaxed);
        self.find.searching = true;
        self.find.searched = q.clone();
        self.find.hits.clear();
        vec![Job::Grep {
            root: self.root.clone(),
            query: q,
            seq: self.find.seq,
            cancel: self.find.cancel.clone(),
        }]
    }

    // ------------------------------------------------------------ input

    pub fn set_view(&mut self, v: View) -> Vec<Job> {
        if self.view == v {
            return Vec::new();
        }
        let mut jobs = Vec::new();
        if self.view == View::Find {
            // free the index — it can be big
            self.find.cancel.store(u64::MAX, Ordering::Relaxed);
            let seq = self.find.seq;
            self.find = Find {
                seq,
                ..Default::default()
            };
        }
        self.view = v;
        self.sel = 0;
        self.scroll.set(0);
        self.filter.clear();
        self.editing = None;
        self.prev = None;
        match v {
            View::Find => {
                self.editing = Some(Editing::Query);
                jobs.extend(self.ensure_index());
            }
            View::Git => jobs.push(self.git_job()),
            View::List | View::Tree => {}
        }
        self.rebuild();
        jobs
    }

    pub fn key(&mut self, key: &[u8]) -> Outcome {
        if let Some(ed) = self.editing {
            return self.edit_key(ed, key);
        }
        let mut out = Outcome::redraw();
        let n = self.rows.len();
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => self.sel = self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" | b"j" => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            b"\x1b[5~" => self.sel = self.sel.saturating_sub(self.page.get()),
            b"\x1b[6~" => self.sel = (self.sel + self.page.get()).min(n.saturating_sub(1)),
            b"g" | b"\x1b[H" | b"\x1bOH" => self.sel = 0,
            b"G" | b"\x1b[F" | b"\x1bOF" => self.sel = n.saturating_sub(1),
            b"\r" => return self.open(),
            b"\x1b[C" | b"\x1bOC" | b"l" => return self.right(),
            b"\x1b[D" | b"\x1bOD" | b"h" => return self.left(),
            [0x7f] | [0x08] | b"-" => return self.up(),
            b"." => {
                self.hidden = !self.hidden;
                self.rebuild();
            }
            b"/" => {
                if self.view == View::Find {
                    self.editing = Some(Editing::Query);
                } else {
                    self.editing = Some(Editing::Filter);
                }
            }
            b"\x1b" => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.rebuild();
                } else if self.preview {
                    self.preview = false;
                    self.prev = None;
                } else if self.view != View::Tree {
                    out.jobs = self.set_view(View::Tree);
                } else {
                    out.redraw = false;
                }
            }
            b"\t" => {
                let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
                out.jobs = self.set_view(View::ALL[(i + 1) % 4]);
            }
            b"\x1b[Z" => {
                let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
                out.jobs = self.set_view(View::ALL[(i + 3) % 4]);
            }
            [d @ b'1'..=b'4'] => out.jobs = self.set_view(View::ALL[(d - b'1') as usize]),
            b"t" => out.jobs = self.set_view(View::Tree),
            b"s" => out.jobs = self.set_view(View::Git),
            b"p" | b" " => {
                self.preview = !self.preview;
                if !self.preview {
                    self.prev = None;
                }
            }
            b"J" => self.scroll_preview(3),
            b"K" => self.scroll_preview(-3),
            b"r" => out.jobs = self.refresh(),
            b"f" => {
                self.follow = !self.follow;
                self.seen_cwd = PathBuf::new(); // snap back to the shell now
                out.acts.push(Act::Notice(format!(
                    "files: {} the shell's folder",
                    if self.follow {
                        "following"
                    } else {
                        "not following"
                    }
                )));
            }
            b"i" => {
                if let Some((p, _, _)) = self.selected() {
                    out.acts.push(Act::Insert(p));
                }
            }
            b"c" => {
                if let Some((p, dir, _)) = self.selected() {
                    let d = if dir {
                        p
                    } else {
                        p.parent().map(Path::to_path_buf).unwrap_or(p)
                    };
                    out.acts.push(Act::Cd(d));
                }
            }
            b"e" => {
                if let Some((p, dir, line)) = self.selected() {
                    if !dir {
                        out.acts.push(Act::Edit(p, line));
                    }
                }
            }
            _ => out.redraw = false,
        }
        out.jobs.extend(self.update_preview());
        out
    }

    fn edit_key(&mut self, ed: Editing, key: &[u8]) -> Outcome {
        let mut out = Outcome::redraw();
        let text = match ed {
            Editing::Filter => &mut self.filter,
            Editing::Query => &mut self.find.query,
        };
        let mut changed = false;
        match key {
            b"\x1b" => {
                if ed == Editing::Filter {
                    self.filter.clear();
                    self.rebuild();
                }
                self.editing = None;
                return out;
            }
            b"\r" => {
                if ed == Editing::Query
                    && self.find.contents
                    && self.find.query.trim() != self.find.searched
                {
                    out.jobs = self.run_grep();
                    self.rebuild();
                    return out;
                }
                if ed == Editing::Filter {
                    self.editing = None;
                    return out;
                }
                return self.open();
            }
            b"\t" if ed == Editing::Query => {
                self.find.contents = !self.find.contents;
                self.sel = 0;
                if !self.find.contents {
                    self.match_names();
                }
                self.rebuild();
                return out;
            }
            b"\x1b[A" | b"\x1bOA" => self.sel = self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" => {
                self.sel = (self.sel + 1).min(self.rows.len().saturating_sub(1))
            }
            [0x7f] | [0x08] => {
                text.pop();
                changed = true;
            }
            [0x15] => {
                text.clear();
                changed = true;
            }
            k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                if text.chars().count() < 80 {
                    text.push_str(&String::from_utf8_lossy(k));
                    changed = true;
                }
            }
            _ => out.redraw = false,
        }
        if changed {
            self.sel = 0;
            if ed == Editing::Query && !self.find.contents {
                self.match_names();
            }
            self.rebuild();
        }
        out.jobs.extend(self.update_preview());
        out
    }

    fn open(&mut self) -> Outcome {
        let mut out = Outcome::redraw();
        let Some((p, dir, line)) = self.selected() else {
            return out;
        };
        if dir {
            match self.view {
                View::Tree => self.toggle(&p),
                _ => {
                    if self.view != View::List {
                        out.jobs.extend(self.set_view(View::List));
                    }
                    out.jobs.extend(self.set_root(p));
                }
            }
        } else {
            out.acts.push(Act::Edit(p, line));
        }
        out
    }

    fn right(&mut self) -> Outcome {
        let mut out = Outcome::redraw();
        let Some((p, dir, _)) = self.selected() else {
            return out;
        };
        if dir {
            if self.view == View::Tree {
                if !self.expanded.contains(&p) {
                    self.toggle(&p);
                } else {
                    self.sel = (self.sel + 1).min(self.rows.len().saturating_sub(1));
                }
            } else {
                return self.open();
            }
        } else {
            self.preview = true;
            out.jobs.extend(self.update_preview());
        }
        out
    }

    fn left(&mut self) -> Outcome {
        let out = Outcome::redraw();
        if self.view != View::Tree {
            return if self.view == View::List {
                self.up()
            } else {
                out
            };
        }
        let Some(Row::Entry { idx, depth }) = self.rows.get(self.sel).cloned() else {
            return out;
        };
        let Some(it) = self.item(&idx).cloned() else {
            return out;
        };
        if it.dir && self.expanded.contains(&it.path) {
            self.toggle(&it.path);
        } else if depth > 0 {
            // jump to the parent folder's row
            if let Some(i) = self.rows[..self.sel].iter().rposition(
                |r| matches!(r, Row::Entry { idx: pi, .. } if self.item(pi).map(|x| x.path.as_path()) == Some(idx.0.as_path())),
            ) {
                self.sel = i;
            }
        }
        out
    }

    fn up(&mut self) -> Outcome {
        let mut out = Outcome::redraw();
        if let Some(parent) = self.root.parent().map(Path::to_path_buf) {
            let from = self.root.clone();
            out.jobs = self.set_root(parent);
            // keep the folder we came from selected
            if let Some(i) = self.rows.iter().position(
                |r| matches!(r, Row::Entry { idx, .. } if self.item(idx).map(|x| &x.path) == Some(&from)),
            ) {
                self.sel = i;
            }
        }
        out
    }

    fn toggle(&mut self, dir: &Path) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_path_buf());
            if !self.cache.contains_key(dir) {
                self.load(dir);
            }
        } else {
            // forget what's inside so memory follows what's on screen
            let gone: Vec<PathBuf> = self
                .cache
                .keys()
                .filter(|k| k.starts_with(dir) && *k != dir)
                .cloned()
                .collect();
            for k in gone {
                self.cache.remove(&k);
                self.expanded.remove(&k);
            }
        }
        self.rebuild();
    }

    fn scroll_preview(&mut self, by: isize) {
        if let Some(p) = &mut self.prev {
            p.top =
                (p.top as isize + by).clamp(0, p.lines.len().saturating_sub(1) as isize) as usize;
        }
    }

    /// Load the preview for the selected row if preview is on.
    fn update_preview(&mut self) -> Vec<Job> {
        if !self.preview {
            return Vec::new();
        }
        let Some((path, dir, line)) = self.selected() else {
            self.prev = None;
            return Vec::new();
        };
        if self.prev.as_ref().map(|p| &p.path) == Some(&path) {
            if let (Some(l), Some(p)) = (line, self.prev.as_mut()) {
                p.top = l.saturating_sub(4);
            }
            return Vec::new();
        }
        if self.view == View::Git && !dir {
            self.prev = Some(Preview {
                path: path.clone(),
                lines: Vec::new(),
                note: "diff…".into(),
                top: 0,
            });
            return vec![Job::Diff {
                root: self.root.clone(),
                file: path,
            }];
        }
        let mut p = load_preview(&path, dir, self.hidden);
        if let Some(l) = line {
            p.top = l.saturating_sub(4);
        }
        self.prev = Some(p);
        Vec::new()
    }

    pub fn wheel(&mut self, dir: isize) -> Outcome {
        let n = self.rows.len();
        self.sel = if dir > 0 {
            self.sel.saturating_sub(3)
        } else {
            (self.sel + 3).min(n.saturating_sub(1))
        };
        let jobs = self.update_preview();
        Outcome {
            jobs,
            redraw: true,
            ..Default::default()
        }
    }

    /// A click at (x, y) inside the pane.
    pub fn click(&mut self, x: u16, y: u16, width: u16) -> Outcome {
        if y == 0 {
            // view tabs: " tree  list  git  find "
            let mut col = 1u16;
            for v in View::ALL {
                let w = v.name().len() as u16 + 2;
                if x >= col && x < col + w {
                    let jobs = self.set_view(v);
                    return Outcome {
                        jobs,
                        redraw: true,
                        ..Default::default()
                    };
                }
                col += w + 1;
            }
            return Outcome::default();
        }
        let top = self.list_top();
        if y < top || (self.preview && width >= 60 && x >= width * 45 / 100) {
            return Outcome::default();
        }
        let i = self.scroll.get() + (y - top) as usize;
        if i < self.rows.len() {
            if i == self.sel {
                return self.open();
            }
            self.sel = i;
            let jobs = self.update_preview();
            return Outcome {
                jobs,
                redraw: true,
                ..Default::default()
            };
        }
        Outcome::default()
    }

    fn list_top(&self) -> u16 {
        if self.editing.is_some() || !self.filter.is_empty() || self.view == View::Find {
            2
        } else {
            1
        }
    }

    // ------------------------------------------------------------ drawing

    pub fn title(&self) -> String {
        let mut t = format!(
            "files · {}",
            crate::ui::short_path(&self.root.to_string_lossy())
        );
        if let Some(g) = &self.git {
            if g.root == self.root {
                t.push_str(" · ");
                t.push_str(&g.summary());
            }
        }
        t
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        if area.height < 2 || area.width < 8 {
            return;
        }
        let dim = Style::default().fg(DIM);
        // row 0: view switcher + flags
        let mut x = area.x + 1;
        for v in View::ALL {
            let st = if v == self.view {
                Style::default()
                    .bg(if focused { AMBER } else { FAINT })
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            let (nx, _) = buf.set_stringn(
                x,
                area.y,
                format!(" {} ", v.name()),
                area.right().saturating_sub(x) as usize,
                st,
            );
            x = nx + 1;
        }
        let mut flags = String::new();
        if self.hidden {
            flags.push_str(" .hidden");
        }
        if !self.follow {
            flags.push_str(" ⇣pinned");
        }
        let fw = flags.chars().count() as u16;
        if x + fw < area.right() {
            buf.set_string(
                area.right() - fw,
                area.y,
                &flags,
                Style::default().fg(FAINT),
            );
        }

        // row 1: filter / query
        let top = self.list_top();
        if top == 2 {
            let (label, text) = match (self.view, self.find.contents) {
                (View::Find, false) => (" name ", &self.find.query),
                (View::Find, true) => (" text ", &self.find.query),
                _ => (" filter ", &self.filter),
            };
            let st = Style::default().fg(CYAN).add_modifier(Modifier::BOLD);
            let (nx, _) = buf.set_stringn(area.x, area.y + 1, label, area.width as usize, st);
            let cursor = if self.editing.is_some() && focused {
                "▏"
            } else {
                ""
            };
            let (nx, _) = buf.set_stringn(
                nx,
                area.y + 1,
                format!("{text}{cursor}"),
                area.right().saturating_sub(nx) as usize,
                Style::default(),
            );
            let hint = match self.view {
                View::Find if self.find.indexing => "  indexing…".to_string(),
                View::Find => {
                    let n = if self.find.contents {
                        self.find.hits.len()
                    } else {
                        self.find.names.len()
                    };
                    let more = if self.find.truncated || self.find.index_truncated {
                        "+"
                    } else {
                        ""
                    };
                    format!(
                        "  {n}{more} · tab: {}",
                        if self.find.contents {
                            "names"
                        } else {
                            "contents"
                        }
                    )
                }
                _ => String::new(),
            };
            buf.set_stringn(
                nx,
                area.y + 1,
                hint,
                area.right().saturating_sub(nx) as usize,
                Style::default().fg(FAINT),
            );
        }

        // body: rows (+ preview)
        let body_h = area.height.saturating_sub(top + 1) as usize; // last row: hints
        let split = self.preview && area.width >= 60;
        let list_w = if split {
            area.width * 45 / 100
        } else {
            area.width
        };
        let body = Rect::new(area.x, area.y + top, list_w, body_h as u16);
        self.page.set(body_h.max(1));
        if self.preview && !split {
            self.render_preview(
                Rect::new(area.x, area.y + top, area.width, body_h as u16),
                buf,
            );
        } else {
            self.render_rows(body, buf, focused);
            if split {
                let px = area.x + list_w;
                for yy in 0..body_h as u16 {
                    buf.set_string(px, area.y + top + yy, "│", Style::default().fg(FAINT));
                }
                self.render_preview(
                    Rect::new(px + 2, area.y + top, area.width - list_w - 2, body_h as u16),
                    buf,
                );
            }
        }

        // hints
        let hints = if self.editing.is_some() {
            if self.view == View::Find {
                "⏎ open/search · tab names⇄text · ↑↓ · esc done"
            } else {
                "type to filter · ⏎ keep · esc clear"
            }
        } else {
            match self.view {
                View::Find => "/ edit · ⏎ open · i insert · p preview · esc back",
                _ => {
                    "⏎ open · i insert · c cd · e edit · p preview · / filter · . hidden · tab view"
                }
            }
        };
        buf.set_stringn(
            area.x,
            area.bottom() - 1,
            hints,
            area.width as usize,
            Style::default().fg(FAINT),
        );
    }

    fn render_rows(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        let h = area.height as usize;
        if h == 0 {
            return;
        }
        let mut top = self.scroll.get();
        if self.sel < top {
            top = self.sel;
        } else if self.sel >= top + h {
            top = self.sel + 1 - h;
        }
        top = top.min(self.rows.len().saturating_sub(h));
        self.scroll.set(top);
        let git = self
            .git
            .as_ref()
            .filter(|g| self.root.starts_with(&g.root) || g.root == self.root);
        for (row_i, row) in self.rows.iter().enumerate().skip(top).take(h) {
            let y = area.y + (row_i - top) as u16;
            let selected = row_i == self.sel;
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
            let w = area.width as usize;
            let (text, st, right): (String, Style, Option<(String, Style)>) = match row {
                Row::Entry { idx, depth } => {
                    let Some(it) = self.item(idx) else { continue };
                    let indent = if self.view == View::Tree {
                        "  ".repeat(*depth as usize)
                    } else {
                        String::new()
                    };
                    let icon = if it.dir {
                        if self.expanded.contains(&it.path) && self.view == View::Tree {
                            "▾ "
                        } else {
                            "▸ "
                        }
                    } else {
                        "  "
                    };
                    let mut name = it.name.clone();
                    if it.dir {
                        name.push('/');
                    }
                    if it.link {
                        name.push_str(" →");
                    }
                    let st = if it.dir { base.fg(CYAN) } else { base };
                    let mark = git.and_then(|g| g.mark(&it.path));
                    let mut right = String::new();
                    if self.view == View::List && w > 40 {
                        if !it.dir {
                            right.push_str(&crate::sysmon::human(it.size));
                        }
                        right.push_str(&format!(" {:>4}", ago(it.mtime)));
                    }
                    let right = match mark {
                        Some(m) => {
                            Some((format!("{right} {}", m.symbol()), base.fg(mark_color(m))))
                        }
                        None if !right.is_empty() => Some((format!("{right}  "), base.fg(FAINT))),
                        None => None,
                    };
                    (format!("{indent}{icon}{name}"), st, right)
                }
                Row::Up => ("  ../".into(), base.fg(CYAN), None),
                Row::More(n, depth) => (
                    format!("{}  … {n} more", "  ".repeat(*depth as usize)),
                    base.fg(FAINT),
                    None,
                ),
                Row::Note(t) => (format!("  {t}"), base.fg(DIM), None),
                Row::Change(p, m) => {
                    let rel = p
                        .strip_prefix(&self.root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .into_owned();
                    (
                        format!(" {}  {rel}", m.symbol()),
                        base.fg(mark_color(*m)),
                        None,
                    )
                }
                Row::Name(i) => {
                    let p = self
                        .find
                        .index
                        .as_ref()
                        .and_then(|x| x.get(*i))
                        .cloned()
                        .unwrap_or_default();
                    (format!("  {p}"), base, None)
                }
                Row::Hit(i) => {
                    let Some(h) = self.find.hits.get(*i) else {
                        continue;
                    };
                    let loc = format!("{}:{}", h.path, h.line);
                    let room = w.saturating_sub(loc.chars().count() + 4);
                    let snippet: String = h.text.trim().chars().take(room).collect();
                    (format!("  {loc}  {snippet}"), base, None)
                }
            };
            let right_w = right.as_ref().map(|(r, _)| r.chars().count()).unwrap_or(0);
            let name_room = w.saturating_sub(right_w + 1);
            buf.set_stringn(area.x, y, truncate(&text, name_room), name_room, st);
            if let Some((r, rs)) = right {
                if right_w < w {
                    buf.set_stringn(area.x + (w - right_w) as u16, y, r, right_w, rs);
                }
            }
        }
    }

    fn render_preview(&self, area: Rect, buf: &mut Buffer) {
        let Some(p) = &self.prev else {
            buf.set_stringn(
                area.x,
                area.y,
                "nothing to preview",
                area.width as usize,
                Style::default().fg(DIM),
            );
            return;
        };
        let mut y = area.y;
        let name = p
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        buf.set_stringn(
            area.x,
            y,
            format!("{name} {}", p.note),
            area.width as usize,
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        y += 1;
        let gutter = if p.lines.len() >= 1000 { 5 } else { 4 };
        for (i, line) in p.lines.iter().enumerate().skip(p.top) {
            if y >= area.bottom() {
                break;
            }
            buf.set_stringn(
                area.x,
                y,
                format!("{:>w$} ", i + 1, w = gutter - 1),
                gutter,
                Style::default().fg(FAINT),
            );
            buf.set_line(
                area.x + gutter as u16,
                y,
                line,
                area.width.saturating_sub(gutter as u16),
            );
            y += 1;
        }
    }
}

// ---------------------------------------------------------------- helpers

/// Case-insensitive unless the needle has capitals ("smart case").
pub fn matches(needle: &str, hay: &str) -> bool {
    if needle.chars().any(char::is_uppercase) {
        hay.contains(needle)
    } else {
        hay.to_lowercase().contains(needle)
    }
}

fn truncate(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}

fn mark_color(m: GitMark) -> Color {
    match m {
        GitMark::Modified | GitMark::Renamed => AMBER,
        GitMark::Added | GitMark::Untracked => GREEN,
        GitMark::Deleted | GitMark::Conflict => RED,
        GitMark::Changed => DIM,
    }
}

fn ago(t: Option<SystemTime>) -> String {
    let Some(d) = t.and_then(|t| t.elapsed().ok()) else {
        return String::new();
    };
    let s = d.as_secs();
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..31_536_000 => format!("{}d", s / 86_400),
        _ => format!("{}y", s / 31_536_000),
    }
}

fn diff_line(l: &str) -> Line<'static> {
    let c = if l.starts_with("+++") || l.starts_with("---") {
        DIM
    } else if l.starts_with('+') {
        GREEN
    } else if l.starts_with('-') {
        RED
    } else if l.starts_with("@@") {
        CYAN
    } else {
        Color::Reset
    };
    Line::styled(l.replace('\t', "    "), Style::default().fg(c))
}

fn load_preview(path: &Path, dir: bool, hidden: bool) -> Preview {
    let mut p = Preview {
        path: path.to_path_buf(),
        lines: Vec::new(),
        note: String::new(),
        top: 0,
    };
    if dir {
        match std::fs::read_dir(path) {
            Ok(rd) => {
                let mut names: Vec<String> = rd
                    .flatten()
                    .map(|e| {
                        let mut n = e.file_name().to_string_lossy().into_owned();
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            n.push('/');
                        }
                        n
                    })
                    .filter(|n| hidden || !n.starts_with('.'))
                    .take(MAX_DIR)
                    .collect();
                names.sort_by_key(|n| (!n.ends_with('/'), n.to_lowercase()));
                p.note = format!("· {} items", names.len());
                p.lines = names.into_iter().map(Line::raw).collect();
            }
            Err(e) => p.note = format!("· {e}"),
        }
        return p;
    }
    use std::io::Read;
    let mut buf = Vec::with_capacity(PREVIEW_BYTES.min(8192));
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut buf))
    {
        Ok(_) => {
            if buf.iter().take(8192).any(|&b| b == 0) {
                p.note = format!("· binary, {}", crate::sysmon::human(size));
                return p;
            }
            let text = String::from_utf8_lossy(&buf);
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default();
            p.lines = highlight::highlight(&text, &ext);
            p.note = if size as usize > PREVIEW_BYTES {
                format!("· {} (first 64K)", crate::sysmon::human(size))
            } else {
                format!("· {}", crate::sysmon::human(size))
            };
        }
        Err(e) => p.note = format!("· {e}"),
    }
    p
}

// ---------------------------------------------------------------- jobs

/// Run a job (on a worker thread).
pub fn run_job(job: Job) -> Done {
    match job {
        Job::Git { root } => {
            let status = gitstat::query(&root);
            Done::Git { root, status }
        }
        Job::Index { root, seq } => {
            let (files, truncated) = index(&root, MAX_INDEX);
            Done::Index {
                root,
                seq,
                files,
                truncated,
            }
        }
        Job::Grep {
            root,
            query,
            seq,
            cancel,
        } => {
            let (files, _) = index(&root, MAX_INDEX);
            let (hits, truncated) = grep(&root, &files, &query, &|| {
                cancel.load(Ordering::Relaxed) != seq
            });
            Done::Grep {
                seq,
                hits,
                truncated,
            }
        }
        Job::Diff { root, file } => {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["diff", "--no-color", "--no-ext-diff", "HEAD", "--"])
                .arg(&file)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            let mut text = out
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            if text.len() > PREVIEW_BYTES {
                let mut cut = PREVIEW_BYTES;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
            }
            Done::Diff { file, text }
        }
    }
}

/// Files under `root` (relative paths): git's list if it's a repo (respects
/// .gitignore), else a walk that skips the usual heavy folders.
pub fn index(root: &Path, max: usize) -> (Vec<String>, bool) {
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    if let Ok(o) = git {
        if o.status.success() {
            let mut v: Vec<String> = o
                .stdout
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            v.sort();
            v.dedup();
            let truncated = v.len() > max;
            v.truncate(max);
            return (v, truncated);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let started = Instant::now();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.') {
                    stack.push(e.path());
                }
            } else if let Ok(rel) = e.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
                if out.len() >= max || started.elapsed() > Duration::from_secs(5) {
                    out.sort();
                    return (out, true);
                }
            }
        }
    }
    out.sort();
    (out, false)
}

/// Search file contents for `query` (smart case). Stops at MAX_HITS or when
/// `cancelled()` says so.
pub fn grep(
    root: &Path,
    files: &[String],
    query: &str,
    cancelled: &dyn Fn() -> bool,
) -> (Vec<Hit>, bool) {
    let smart = query.chars().any(char::is_uppercase);
    let needle = if smart {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut hits = Vec::new();
    for rel in files {
        if cancelled() {
            break;
        }
        let path = root.join(rel);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > 2 * 1024 * 1024 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes.iter().take(1024).any(|&b| b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (n, line) in text.lines().enumerate() {
            let found = if smart {
                line.contains(&needle)
            } else {
                line.to_lowercase().contains(&needle)
            };
            if found {
                hits.push(Hit {
                    path: rel.clone(),
                    line: n + 1,
                    text: line.chars().take(200).collect(),
                });
                if hits.len() >= MAX_HITS {
                    return (hits, true);
                }
            }
        }
    }
    (hits, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sv-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/core")).unwrap();
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(
            d.join("src/core/pricing.py"),
            "def price():\n    return 42\n",
        )
        .unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {}\n// TODO: alerts\n").unwrap();
        std::fs::write(d.join("README.md"), "# Demo\n").unwrap();
        std::fs::write(d.join(".env"), "SECRET=1\n").unwrap();
        d
    }

    fn names(p: &FilesPane) -> Vec<String> {
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, true);
        (0..20)
            .map(|y| {
                (0..60)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn screen(p: &FilesPane) -> String {
        names(p).join("\n")
    }

    #[test]
    fn tree_expand_collapse_and_hidden() {
        let d = tmp("tree");
        let (mut p, jobs) = FilesPane::new(d.clone());
        assert!(matches!(jobs[0], Job::Git { .. }));
        let s = screen(&p);
        assert!(
            s.contains("▸ docs/") && s.contains("▸ src/") && s.contains("README.md"),
            "{s}"
        );
        assert!(!s.contains(".env"));
        // dirs first: docs, src, README
        p.key(b"j"); // src
        p.key(b"\r"); // expand
        let s = screen(&p);
        assert!(
            s.contains("▾ src/") && s.contains("    main.rs") && s.contains("  ▸ core/"),
            "{s}"
        );
        p.key(b"j"); // core
        p.key(b"l"); // expand core
        assert!(screen(&p).contains("pricing.py"));
        p.key(b"h"); // collapse core
        assert!(!screen(&p).contains("pricing.py"));
        p.key(b"h"); // core is collapsed → jump to parent (src)
        assert_eq!(p.selected().unwrap().0, d.join("src"));
        p.key(b".");
        assert!(screen(&p).contains(".env"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn filter_and_actions() {
        let d = tmp("filter");
        let (mut p, _) = FilesPane::new(d.clone());
        p.key(b"/");
        for k in ["r", "e", "a", "d"] {
            p.key(k.as_bytes());
        }
        let s = screen(&p);
        assert!(
            s.contains("filter read") && s.contains("README.md") && !s.contains("docs/"),
            "{s}"
        );
        p.key(b"\r"); // keep filter, leave editing
        let out = p.key(b"i");
        assert_eq!(out.acts, vec![Act::Insert(d.join("README.md"))]);
        let out = p.key(b"c");
        assert_eq!(out.acts, vec![Act::Cd(d.clone())]);
        let out = p.key(b"\r");
        assert_eq!(out.acts, vec![Act::Edit(d.join("README.md"), None)]);
        p.key(b"\x1b"); // clear filter
        assert!(screen(&p).contains("docs/"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn list_view_navigates_and_preview_highlights() {
        let d = tmp("list");
        let (mut p, _) = FilesPane::new(d.clone());
        p.key(b"2");
        assert_eq!(p.view, View::List);
        let s = screen(&p);
        assert!(s.contains("../") && s.contains("README.md"), "{s}");
        // ../, docs, src, README → go into src
        p.key(b"j");
        p.key(b"j");
        p.key(b"\r");
        assert_eq!(p.root, d.join("src"));
        p.key(b"j");
        p.key(b"j"); // ../, core, main.rs
        p.key(b"p");
        let s = screen(&p);
        assert!(
            s.contains("main.rs ·") && s.contains("1 fn main() {}"),
            "{s}"
        );
        p.key(b"\x7f"); // up again
        assert_eq!(p.root, d);
        assert_eq!(
            p.selected().unwrap().0,
            d.join("src"),
            "keeps the folder we came from selected"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn notices_new_files() {
        let d = tmp("poll");
        let (mut p, _) = FilesPane::new(d.clone());
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(d.join("new.txt"), "x").unwrap();
        // mtime granularity can be coarse; force like the app does after a command
        p.refresh();
        assert!(screen(&p).contains("new.txt"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn find_names_and_contents() {
        let d = tmp("find");
        let (mut p, _) = FilesPane::new(d.clone());
        let jobs = p.set_view(View::Find);
        let Job::Index { root, seq } = jobs[0].clone() else {
            panic!()
        };
        let done = run_job(Job::Index { root, seq });
        p.done(done);
        for k in ["p", "r", "i", "c"] {
            p.key(k.as_bytes());
        }
        let s = screen(&p);
        assert!(s.contains("src/core/pricing.py"), "{s}");
        let out = p.key(b"\r");
        assert_eq!(
            out.acts,
            vec![Act::Edit(d.join("src/core/pricing.py"), None)]
        );
        // contents
        p.key(b"\t");
        for _ in 0..4 {
            p.key(b"\x7f");
        }
        for k in ["t", "o", "d", "o"] {
            p.key(k.as_bytes());
        }
        let out = p.key(b"\r");
        let job = out.jobs.into_iter().next().expect("grep job");
        p.done(run_job(job));
        let s = screen(&p);
        assert!(
            s.contains("src/main.rs:2") && s.contains("TODO: alerts"),
            "{s}"
        );
        let out = p.key(b"\r");
        assert_eq!(out.acts, vec![Act::Edit(d.join("src/main.rs"), Some(2))]);
        // leaving Find frees the index
        p.key(b"\x1b");
        p.key(b"\x1b");
        assert_eq!(p.view, View::Tree);
        assert!(p.find.index.is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn git_view_and_badges() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let d = tmp("git");
        let git = |a: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(a)
                .output()
                .unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["-c", "user.email=a@b", "-c", "user.name=t", "add", "."]);
        git(&[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "x",
        ]);
        std::fs::write(d.join("src/main.rs"), "fn main() { changed }\n").unwrap();
        let (mut p, jobs) = FilesPane::new(d.clone());
        for j in jobs {
            p.done(run_job(j));
        }
        let s = screen(&p);
        assert!(
            s.lines().any(|l| l.contains("src/") && l.ends_with('•')),
            "{s}"
        );
        assert!(p.title().contains("main · 1 changed"), "{}", p.title());
        let jobs = p.set_view(View::Git);
        for j in jobs {
            p.done(run_job(j));
        }
        let s = screen(&p);
        assert!(s.contains("M  src/main.rs"), "{s}");
        let out = p.key(b"p");
        let diff = out.jobs.into_iter().next().expect("diff job");
        p.done(run_job(diff));
        let s = screen(&p);
        assert!(s.contains("+fn main() { changed }"), "{s}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn smart_case() {
        assert!(matches("read", "README.md"));
        assert!(!matches("Read", "README.md"));
        assert!(matches("READ", "README.md"));
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}

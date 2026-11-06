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
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);
const SEL_BG: Color = Color::Indexed(237);
const INK: Color = Color::Indexed(16);
const TEXT: Color = Color::Indexed(252);
const BLUE: Color = Color::Indexed(110);
const MAGENTA: Color = Color::Indexed(170);

/// Most entries read from one folder.
const MAX_DIR: usize = 5000;
/// Most files indexed for Find.
const MAX_INDEX: usize = 20_000;
/// Size view: stop counting after this many files (and say so).
const MAX_SIZE_WALK: u64 = 400_000;
/// Recent view: newest files kept.
const MAX_RECENT: usize = 300;
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
    /// Parent folder | this folder | preview.
    Columns,
    /// Names in a grid, like a file manager's icon view.
    Grid,
    Git,
    Find,
    /// What takes the space: folders and files by size, as bars.
    Sizes,
    /// Files changed most recently, anywhere under the folder.
    Recent,
}

impl View {
    /// In the boards' order: 1 Tree … 8 Recent (the number keys pick them).
    pub const ALL: [View; 8] = [
        View::Tree,
        View::Columns,
        View::List,
        View::Grid,
        View::Sizes,
        View::Git,
        View::Find,
        View::Recent,
    ];
    /// As the header shows it.
    pub fn label(self) -> &'static str {
        match self {
            View::Tree => "Tree",
            View::List => "List",
            View::Columns => "Columns",
            View::Grid => "Grid",
            View::Git => "Git",
            View::Find => "Find",
            View::Sizes => "Size",
            View::Recent => "Recent",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            View::Tree => "tree",
            View::List => "list",
            View::Columns => "columns",
            View::Grid => "grid",
            View::Git => "git",
            View::Find => "find",
            View::Sizes => "size",
            View::Recent => "recent",
        }
    }
    /// Shows the folder's own entries (navigable with ⏎ / ← like List).
    fn flat(self) -> bool {
        matches!(self, View::List | View::Columns | View::Grid)
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
    Sizes {
        root: PathBuf,
        hidden: bool,
    },
    Recent {
        root: PathBuf,
    },
}

/// Recent view: (path relative to the folder, changed, bytes).
pub type RecentFile = (String, SystemTime, u64);

/// One entry of the size view.
#[derive(Debug, Clone, PartialEq)]
pub struct Sized {
    pub name: String,
    pub dir: bool,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // one at a time, off the UI thread
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
    Sizes {
        root: PathBuf,
        entries: Vec<Sized>,
        /// Stopped counting early (huge folder).
        partial: bool,
    },
    Recent {
        root: PathBuf,
        files: Vec<(String, SystemTime, u64)>,
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
    /// Put this on the clipboard.
    Copy(String),
    /// Type `mv -i <paths> ` at the prompt for you to finish.
    Move(Vec<PathBuf>),
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
    Entry {
        idx: (PathBuf, usize),
        depth: u16,
    },
    Up,
    More(usize, u16),
    Change(PathBuf, GitMark),
    Name(usize),
    Hit(usize),
    Size(usize),
    Recent(usize),
    Note(String),
    /// A group title ("STAGED", "CHANGES"): not selectable.
    Heading(String),
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
    /// Files coding agents changed: path → (agent name, colour).
    pub agent_marks: HashMap<PathBuf, (String, u8)>,
    last_poll: Instant,
    /// Size view: (folder, entries, partial) — None while counting.
    sizes: Option<(PathBuf, Vec<Sized>, bool)>,
    /// Recent view: (folder, files newest first) — None while looking.
    recent: Option<(PathBuf, Vec<RecentFile>)>,
    /// Grid: cells per row at the last draw.
    grid_cols: Cell<usize>,
    /// How folder names look (settings screen: colour, bold).
    pub folder_style: Style,
    /// What downloads are filling right now: path → progress (a spinner on it).
    pub loading: HashMap<PathBuf, Option<u8>>,
    /// The pane was wide enough at the last draw to show a preview beside the list.
    wide: Cell<bool>,
    /// Git and Find turn the preview on by themselves when wide; what it was before.
    preview_before: Option<bool>,
    /// The preview was turned on by a layout, not by you (see `show_preview`).
    pub preview_soft: bool,
    /// List view: sort column and the files marked with space.
    sort: ListSort,
    marked: Vec<PathBuf>,
    /// Grid cards' second line, worked out once per entry: "6 items", "512×512".
    grid_info: RefCell<HashMap<PathBuf, String>>,
    /// Rows above the list body drawn last frame (List's column header).
    header: Cell<u16>,
    /// Size view: `x` asked to delete this (waiting for y).
    confirm_delete: Option<PathBuf>,
}

/// List view sort column (`s` cycles; `S` flips the order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListSort {
    #[default]
    Name,
    Size,
    Modified,
    Type,
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
            agent_marks: HashMap::new(),
            last_poll: Instant::now(),
            sizes: None,
            recent: None,
            grid_cols: Cell::new(1),
            folder_style: Style::default().fg(CYAN),
            loading: HashMap::new(),
            wide: Cell::new(false),
            preview_before: None,
            preview_soft: false,
            sort: ListSort::Name,
            marked: Vec::new(),
            header: Cell::new(0),
            grid_info: RefCell::new(HashMap::new()),
            confirm_delete: None,
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
        jobs.extend(self.view_job());
        self.rebuild();
        jobs.extend(self.update_preview());
        jobs
    }

    /// Show a path: a folder's contents, or a file selected in its folder.
    /// Stops following the shell, so it stays put.
    pub fn reveal(&mut self, path: &Path) -> Vec<Job> {
        self.follow = false;
        if path.is_dir() {
            let jobs = self.set_root(path.to_path_buf());
            if jobs.is_empty() {
                return self.refresh();
            }
            return jobs;
        }
        let Some(dir) = path.parent() else {
            return Vec::new();
        };
        let mut jobs = Vec::new();
        if !matches!(self.view, View::Tree | View::List) {
            jobs.extend(self.set_view(View::Tree));
        }
        if dir == self.root {
            jobs.extend(self.refresh());
        } else {
            jobs.extend(self.set_root(dir.to_path_buf()));
        }
        if let Some(i) = self.rows.iter().position(|r| match r {
            Row::Entry { idx, .. } => self.item(idx).is_some_and(|it| it.path == path),
            _ => false,
        }) {
            self.sel = i;
        }
        jobs
    }

    /// A download filling this path (or something under a closed folder).
    fn loading_at(&self, path: &Path, dir: bool) -> Option<Option<u8>> {
        if let Some(p) = self.loading.get(path) {
            return Some(*p);
        }
        if dir && !(self.view == View::Tree && self.expanded.contains(path)) {
            return self
                .loading
                .iter()
                .find(|(k, _)| k.starts_with(path))
                .map(|(_, v)| *v);
        }
        None
    }

    /// The background work the size and recent views need for this folder.
    fn view_job(&mut self) -> Option<Job> {
        match self.view {
            View::Sizes if self.sizes.as_ref().is_none_or(|(r, _, _)| *r != self.root) => {
                self.sizes = None;
                Some(Job::Sizes {
                    root: self.root.clone(),
                    hidden: self.hidden,
                })
            }
            View::Recent if self.recent.as_ref().is_none_or(|(r, _)| *r != self.root) => {
                self.recent = None;
                Some(Job::Recent {
                    root: self.root.clone(),
                })
            }
            _ => None,
        }
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
        self.grid_info.borrow_mut().clear();
        self.sizes = None;
        self.recent = None;
        let mut jobs = vec![self.git_job()];
        jobs.extend(self.view_job());
        self.rebuild();
        jobs
    }

    /// Turn the preview on. `soft`: only where it fits beside or under the
    /// list (the dev layout's PREVIEW box), never in place of it.
    pub fn show_preview(&mut self, soft: bool) -> Vec<Job> {
        self.preview = true;
        self.preview_soft = soft;
        self.update_preview()
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
            View::List | View::Columns | View::Grid => {
                if self.root.parent().is_some() && self.view != View::Grid {
                    rows.push(Row::Up);
                }
                if let Some(c) = self.cache.get(&self.root) {
                    let mut idx: Vec<usize> = (0..c.items.len())
                        .filter(|i| self.visible_item(&c.items[*i]))
                        .collect();
                    if self.view == View::List {
                        let it = |i: &usize| &c.items[*i];
                        match self.sort {
                            ListSort::Name => {}
                            ListSort::Size => idx.sort_by_key(|i| std::cmp::Reverse(it(i).size)),
                            ListSort::Modified => {
                                idx.sort_by_key(|i| std::cmp::Reverse(it(i).mtime))
                            }
                            ListSort::Type => idx.sort_by_key(|i| (!it(i).dir, kind(it(i)))),
                        }
                    }
                    for i in idx {
                        rows.push(Row::Entry {
                            idx: (self.root.clone(), i),
                            depth: 0,
                        });
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
                    // board 10: STAGED, CHANGES, UNTRACKED, then BY AGENT
                    let shown: Vec<(PathBuf, GitMark, bool)> = g
                        .entries()
                        .into_iter()
                        .filter(|(p, _)| {
                            let rel = p.strip_prefix(&self.root).unwrap_or(p);
                            self.filter.is_empty() || matches(&self.filter, &rel.to_string_lossy())
                        })
                        .map(|(p, m)| (p.to_path_buf(), m, g.is_staged(p)))
                        .collect();
                    type Pick = fn(&(PathBuf, GitMark, bool)) -> bool;
                    let groups: [(&str, Pick); 3] = [
                        ("STAGED", |e| e.2),
                        ("CHANGES", |e| !e.2 && e.1 != GitMark::Untracked),
                        ("UNTRACKED", |e| !e.2 && e.1 == GitMark::Untracked),
                    ];
                    for (name, pick) in groups {
                        let mut first = true;
                        for e in shown.iter().filter(|e| pick(e)) {
                            if first {
                                rows.push(Row::Heading(name.into()));
                                first = false;
                            }
                            rows.push(Row::Change(e.0.clone(), e.1));
                        }
                    }
                    let mut by: Vec<(String, usize)> = Vec::new();
                    for (p, (who, _)) in &self.agent_marks {
                        if p.starts_with(&self.root) {
                            match by.iter_mut().find(|(w, _)| w == who) {
                                Some((_, n)) => *n += 1,
                                None => by.push((who.clone(), 1)),
                            }
                        }
                    }
                    by.sort();
                    if !by.is_empty() {
                        rows.push(Row::Heading("BY AGENT".into()));
                        for (who, n) in by {
                            rows.push(Row::Note(format!(
                                "● {who}  {n} file{}  agent/{who}",
                                if n == 1 { "" } else { "s" }
                            )));
                        }
                    }
                    if rows.is_empty() {
                        rows.push(Row::Note("no changes — working tree clean".into()));
                    }
                }
                None => rows.push(Row::Note("not a git repository".into())),
            },
            View::Sizes => match &self.sizes {
                Some((r, e, _)) if *r == self.root => {
                    if self.root.parent().is_some() {
                        rows.push(Row::Up);
                    }
                    for (i, it) in e.iter().enumerate() {
                        if self.filter.is_empty() || matches(&self.filter, &it.name) {
                            rows.push(Row::Size(i));
                        }
                    }
                    if e.is_empty() {
                        rows.push(Row::Note("empty folder".into()));
                    }
                }
                _ => rows.push(Row::Note("counting sizes…".into())),
            },
            View::Recent => match &self.recent {
                Some((r, f)) if *r == self.root => {
                    for (i, (p, _, _)) in f.iter().enumerate() {
                        if self.filter.is_empty() || matches(&self.filter, p) {
                            rows.push(Row::Recent(i));
                        }
                    }
                    if rows.is_empty() {
                        rows.push(Row::Note("no files".into()));
                    }
                }
                _ => rows.push(Row::Note("looking for recent changes…".into())),
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
        self.skip_headings(true);
    }

    /// Marked files (List view), else the one under the cursor.
    fn marked_or_selected(&self) -> Vec<PathBuf> {
        if !self.marked.is_empty() {
            return self.marked.clone();
        }
        match self.rows.get(self.sel) {
            Some(Row::Entry { .. }) => self.selected().map(|s| vec![s.0]).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Move off a group title, the way the cursor was going.
    fn skip_headings(&mut self, down: bool) {
        let n = self.rows.len();
        let heading = |r: Option<&Row>| matches!(r, Some(Row::Heading(_)));
        if !heading(self.rows.get(self.sel)) {
            return;
        }
        let mut i = self.sel;
        if down {
            while i + 1 < n && heading(self.rows.get(i)) {
                i += 1;
            }
            if heading(self.rows.get(i)) {
                return self.skip_from_bottom();
            }
        } else {
            while i > 0 && heading(self.rows.get(i)) {
                i -= 1;
            }
            if heading(self.rows.get(i)) {
                // the top row is a title: go to the first thing below it
                while i + 1 < n && heading(self.rows.get(i)) {
                    i += 1;
                }
            }
        }
        self.sel = i;
    }

    fn skip_from_bottom(&mut self) {
        while self.sel > 0 && matches!(self.rows.get(self.sel), Some(Row::Heading(_))) {
            self.sel -= 1;
        }
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
            Row::Size(i) => {
                let (_, e, _) = self.sizes.as_ref()?;
                let it = e.get(*i)?;
                Some((self.root.join(&it.name), it.dir, None))
            }
            Row::Recent(i) => {
                let (_, f) = self.recent.as_ref()?;
                Some((self.root.join(&f.get(*i)?.0), false, None))
            }
            Row::More(..) | Row::Note(_) | Row::Heading(_) => None,
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
            Done::Sizes {
                root,
                entries,
                partial,
            } => {
                if root != self.root {
                    return Outcome::default();
                }
                self.sizes = Some((root, entries, partial));
            }
            Done::Recent { root, files } => {
                if root != self.root {
                    return Outcome::default();
                }
                self.recent = Some((root, files));
            }
            Done::Diff { file, text } => {
                if self.prev.as_ref().map(|p| &p.path) == Some(&file) {
                    // board 10: hunks only, with +N −M in the title
                    let body: Vec<&str> =
                        text.lines().skip_while(|l| !l.starts_with("@@")).collect();
                    let add = body.iter().filter(|l| l.starts_with('+')).count();
                    let del = body.iter().filter(|l| l.starts_with('-')).count();
                    let lines = body.into_iter().map(diff_line).collect::<Vec<_>>();
                    let note = if lines.is_empty() {
                        "no diff (untracked or binary)".into()
                    } else {
                        format!(" +{add} −{del}")
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
        Outcome {
            jobs: self.update_preview(),
            redraw: true,
            ..Default::default()
        }
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
        // Every word must appear in the path, as a substring or loosely in
        // order ("alrt" finds "alerts"). Exact substrings, then name hits, rank first.
        let words: Vec<&str> = q.split_whitespace().collect();
        let mut scored: Vec<(u8, usize, usize)> = index
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let exact = words.iter().all(|w| matches(w, p));
                if !exact && !words.iter().all(|w| subsequence(w, p)) {
                    return None;
                }
                let base = p.rsplit('/').next().unwrap_or(p);
                let in_name = matches(words[0], base) || subsequence(words[0], base);
                let tier = match (exact, in_name) {
                    (true, true) => 0,
                    (true, false) => 1,
                    (false, true) => 2,
                    (false, false) => 3,
                };
                Some((tier, p.len(), i))
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
        if let Some(before) = self.preview_before.take() {
            self.preview = before;
        }
        if matches!(v, View::Git | View::Find) && self.wide.get() && !self.preview {
            self.preview_before = Some(self.preview);
            self.preview = true;
        }
        self.view = v;
        self.sel = 0;
        self.scroll.set(0);
        self.filter.clear();
        self.editing = None;
        self.prev = None;
        self.marked.clear();
        self.confirm_delete = None;
        match v {
            View::Find => {
                self.editing = Some(Editing::Query);
                jobs.extend(self.ensure_index());
            }
            View::Git => jobs.push(self.git_job()),
            View::Sizes | View::Recent => jobs.extend(self.view_job()),
            _ => {}
        }
        self.rebuild();
        jobs
    }

    pub fn key(&mut self, key: &[u8]) -> Outcome {
        if let Some(ed) = self.editing {
            return self.edit_key(ed, key);
        }
        if let Some(path) = self.confirm_delete.take() {
            let mut out = Outcome::redraw();
            if key == b"y" || key == b"Y" {
                match to_trash(&path) {
                    Ok(dest) => {
                        out.acts.push(Act::Notice(format!(
                            "moved {} to the Trash ({})",
                            path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            crate::ui::short_path(&dest.to_string_lossy())
                        )));
                        out.jobs = self.refresh();
                    }
                    Err(e) => out
                        .acts
                        .push(Act::Notice(format!("couldn't move it to the Trash: {e}"))),
                }
            }
            return out;
        }
        if self.view == View::Sizes && key == b"x" {
            if let Some((p, _, _)) = self
                .selected()
                .filter(|_| matches!(self.rows.get(self.sel), Some(Row::Size(_))))
            {
                self.confirm_delete = Some(p);
            }
            return Outcome::redraw();
        }
        let mut out = Outcome::redraw();
        let n = self.rows.len();
        if self.view == View::Grid {
            // arrows move in two dimensions; ⌫ goes up a folder
            let cols = self.grid_cols.get().max(1);
            let moved = match key {
                b"\x1b[A" | b"\x1bOA" | b"k" => Some(self.sel.saturating_sub(cols)),
                b"\x1b[B" | b"\x1bOB" | b"j" => Some((self.sel + cols).min(n.saturating_sub(1))),
                b"\x1b[D" | b"\x1bOD" | b"h" => Some(self.sel.saturating_sub(1)),
                b"\x1b[C" | b"\x1bOC" | b"l" => Some((self.sel + 1).min(n.saturating_sub(1))),
                _ => None,
            };
            if let Some(s) = moved {
                self.sel = s;
                out.jobs.extend(self.update_preview());
                return out;
            }
        }
        if self.view == View::List {
            match key {
                b" " => {
                    if let Some((p, _, _)) = self
                        .selected()
                        .filter(|_| matches!(self.rows.get(self.sel), Some(Row::Entry { .. })))
                    {
                        match self.marked.iter().position(|m| *m == p) {
                            Some(i) => {
                                self.marked.remove(i);
                            }
                            None => self.marked.push(p),
                        }
                    }
                    self.sel = (self.sel + 1).min(n.saturating_sub(1));
                    return out;
                }
                b"s" => {
                    self.sort = match self.sort {
                        ListSort::Name => ListSort::Size,
                        ListSort::Size => ListSort::Modified,
                        ListSort::Modified => ListSort::Type,
                        ListSort::Type => ListSort::Name,
                    };
                    let keep = self.selected().map(|s| s.0);
                    self.rebuild();
                    if let Some(k) = keep {
                        if let Some(i) = (0..self.rows.len()).find(|i| {
                            let save = self.sel;
                            self.sel = *i;
                            let hit = self.selected().map(|s| s.0) == Some(k.clone());
                            self.sel = save;
                            hit
                        }) {
                            self.sel = i;
                        }
                    }
                    return out;
                }
                b"y" | b"m" => {
                    let paths = self.marked_or_selected();
                    if paths.is_empty() {
                        return out;
                    }
                    if key == b"y" {
                        let text = paths
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("\n");
                        out.acts.push(Act::Copy(text));
                        out.acts.push(Act::Notice(format!(
                            "copied {} path{}",
                            paths.len(),
                            if paths.len() == 1 { "" } else { "s" }
                        )));
                    } else {
                        // the shell finishes it: you type where to
                        out.acts.push(Act::Move(paths));
                    }
                    self.marked.clear();
                    return out;
                }
                _ => {}
            }
        }
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => {
                self.sel = self.sel.saturating_sub(1);
                self.skip_headings(false);
            }
            b"\x1b[B" | b"\x1bOB" | b"j" => {
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
                self.skip_headings(true);
            }
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
                let n = View::ALL.len();
                let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
                out.jobs = self.set_view(View::ALL[(i + 1) % n]);
            }
            b"\x1b[Z" => {
                let n = View::ALL.len();
                let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
                out.jobs = self.set_view(View::ALL[(i + n - 1) % n]);
            }
            [d @ b'1'..=b'8'] => out.jobs = self.set_view(View::ALL[(d - b'1') as usize]),
            b"t" => out.jobs = self.set_view(View::Tree),
            b"s" => out.jobs = self.set_view(View::Git),
            b"p" | b" " => {
                self.preview = !self.preview || self.preview_soft;
                self.preview_soft = false;
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
                v => {
                    if !v.flat() && v != View::Sizes {
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
            return if self.view.flat() || self.view == View::Sizes {
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
            // view tabs, laid out as drawn
            let mut col = 0u16;
            for (v, text) in self.tab_cells(width) {
                let w = text.chars().count() as u16;
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
        let top = self.list_top() + self.header.get();
        if y < top || (self.preview && width >= 50 && x >= width * 45 / 100) {
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

    /// The header's view tabs for a pane this wide: every name if they fit,
    /// else numbers with the current one named, else just the current one.
    fn tab_cells(&self, width: u16) -> Vec<(View, String)> {
        let full: Vec<(View, String)> = View::ALL
            .iter()
            .enumerate()
            .map(|(i, v)| (*v, format!(" {} {} ", i + 1, v.label())))
            .collect();
        let need: usize = full.iter().map(|(_, t)| t.chars().count() + 1).sum();
        if need + 10 <= width as usize {
            return full;
        }
        let short: Vec<(View, String)> = View::ALL
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if *v == self.view {
                    (*v, format!(" {} {} ", i + 1, v.label()))
                } else {
                    (*v, format!("{}", i + 1))
                }
            })
            .collect();
        let need: usize = short.iter().map(|(_, t)| t.chars().count() + 1).sum();
        if need + 6 <= width as usize {
            return short;
        }
        let i = View::ALL.iter().position(|v| *v == self.view).unwrap_or(0);
        vec![(
            self.view,
            format!(" {}/{} {} ", i + 1, View::ALL.len(), self.view.label()),
        )]
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
        self.header.set(0);
        // row 0: "1 Tree  2 Columns  3 List …" (board 9), the current one in amber
        let tabs = self.tab_cells(area.width);
        let mut x = area.x;
        for (v, text) in &tabs {
            let st = if *v == self.view {
                Style::default()
                    .bg(if focused { AMBER } else { FAINT })
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            let (nx, _) =
                buf.set_stringn(x, area.y, text, area.right().saturating_sub(x) as usize, st);
            x = nx + 1;
        }
        let mut flags = String::new();
        if let (View::Sizes, Some((r, e, partial))) = (self.view, &self.sizes) {
            if *r == self.root {
                let total: u64 = e.iter().map(|x| x.bytes).sum();
                flags.push_str(&format!(
                    " {}{}",
                    crate::sysmon::human(total),
                    if *partial { "+ (stopped counting)" } else { "" }
                ));
            }
        }
        if let Some(p) = self.loading.get(&self.root) {
            flags.push_str(&format!(" {}", crate::loading::frame()));
            if let Some(p) = p {
                flags.push_str(&format!(" {p}%"));
            }
        }
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
                (View::Find, _) => ("❯ ", &self.find.query),
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
                    let total = self.find.index.as_ref().map(|i| i.len()).unwrap_or(0);
                    format!(
                        "  {} · {n}{more} / {total} · tab switches",
                        if self.find.contents {
                            "names | [contents]"
                        } else {
                            "[names] | contents"
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
        self.wide.set(area.width >= 70);
        // tall and narrow (the dev layout's file column): PREVIEW under the list (board 1)
        if self.preview && area.width < 50 && body_h >= 18 {
            let list_h = (body_h * 11 / 20) as u16;
            let body = Rect::new(area.x, area.y + top, area.width, list_h);
            self.page.set((list_h as usize).max(1));
            match self.view {
                View::Grid => self.render_grid(body, buf, focused),
                _ => self.render_rows(body, buf, focused),
            }
            let ry = area.y + top + list_h;
            let rule = format!("─ PREVIEW {}", "─".repeat(area.width as usize));
            buf.set_stringn(
                area.x,
                ry,
                &rule,
                area.width as usize,
                Style::default().fg(FAINT),
            );
            buf.set_stringn(
                area.x + 2,
                ry,
                "PREVIEW",
                7,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            );
            self.render_preview(
                Rect::new(area.x, ry + 1, area.width, body_h as u16 - list_h - 1),
                buf,
            );
            self.render_hints(area, buf);
            return;
        }
        let split = self.preview && area.width >= 50;
        if self.preview && self.preview_soft && !split {
            // a layout's preview that doesn't fit: just the list
            let body = Rect::new(area.x, area.y + top, area.width, body_h as u16);
            self.page.set(body_h.max(1));
            match self.view {
                View::Grid => self.render_grid(body, buf, focused),
                _ => self.render_rows(body, buf, focused),
            }
            self.render_hints(area, buf);
            return;
        }
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
            match self.view {
                View::Grid => self.render_grid(body, buf, focused),
                View::Columns if list_w >= 40 => {
                    // parent folder on the left, this one (selected inside) on the right
                    let pw = list_w * 2 / 5;
                    self.render_parent(Rect::new(body.x, body.y, pw, body.height), buf);
                    for yy in 0..body.height {
                        buf.set_string(body.x + pw, body.y + yy, "│", Style::default().fg(FAINT));
                    }
                    self.render_rows(
                        Rect::new(body.x + pw + 1, body.y, list_w - pw - 1, body.height),
                        buf,
                        focused,
                    );
                }
                View::Sizes if body.height >= 14 && list_w >= 30 => {
                    let mh = (body.height / 3).clamp(4, 7);
                    self.render_treemap(Rect::new(body.x, body.y, body.width, mh), buf);
                    self.header.set(mh + 1);
                    self.render_rows(
                        Rect::new(body.x, body.y + mh + 1, body.width, body.height - mh - 1),
                        buf,
                        focused,
                    );
                }
                View::List if list_w >= 44 => {
                    self.render_list_header(body, buf);
                    self.render_rows(
                        Rect::new(
                            body.x,
                            body.y + 1,
                            body.width,
                            body.height.saturating_sub(1),
                        ),
                        buf,
                        focused,
                    );
                }
                _ => self.render_rows(body, buf, focused),
            }
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

        self.render_hints(area, buf);
    }

    /// Size view: the biggest entries as blocks sized by what they hold.
    fn render_treemap(&self, area: Rect, buf: &mut Buffer) {
        let Some((r, e, _)) = &self.sizes else { return };
        if *r != self.root || e.is_empty() {
            return;
        }
        let top: Vec<(usize, &Sized)> = e
            .iter()
            .enumerate()
            .take(6)
            .filter(|(_, x)| x.bytes > 0)
            .collect();
        let mut rect = area;
        let mut left: u64 = top.iter().map(|(_, x)| x.bytes).sum::<u64>().max(1);
        for (k, (i, it)) in top.iter().enumerate() {
            if rect.width < 2 || rect.height < 1 {
                break;
            }
            let last = k + 1 == top.len();
            let share = it.bytes as f64 / left as f64;
            // alternate: across, then down, so blocks stay roughly square
            let cell = if last {
                rect
            } else if k % 2 == 0 {
                // leave room for the rest, however small they are
                let max = rect.width.saturating_sub(10).max(1);
                let w = ((rect.width as f64 * share).round() as u16).clamp(1, max);
                let c = Rect::new(rect.x, rect.y, w, rect.height);
                rect = Rect::new(rect.x + w, rect.y, rect.width - w, rect.height);
                c
            } else {
                let max = rect.height.saturating_sub(2).max(1);
                let h = ((rect.height as f64 * share).round() as u16).clamp(1, max);
                let c = Rect::new(rect.x, rect.y, rect.width, h);
                rect = Rect::new(rect.x, rect.y + h, rect.width, rect.height - h);
                c
            };
            left = left.saturating_sub(it.bytes).max(1);
            let bg = rank_tint(*i);
            // a one-column gap on the right and a row below, like separate tiles
            let tile = Rect::new(
                cell.x,
                cell.y,
                cell.width
                    .saturating_sub(u16::from(cell.right() < area.right()))
                    .max(1),
                cell.height
                    .saturating_sub(u16::from(cell.bottom() < area.bottom()))
                    .max(1),
            );
            buf.set_style(tile, Style::default().bg(bg));
            let w = tile.width.saturating_sub(1) as usize;
            let name = format!("{}{}", it.name, if it.dir { "/" } else { "" });
            let size = human_space(it.bytes);
            if tile.height >= 2 {
                buf.set_stringn(
                    tile.x + 1,
                    tile.y,
                    truncate(&name, w),
                    w,
                    Style::default().bg(bg).fg(TEXT),
                );
                buf.set_stringn(
                    tile.x + 1,
                    tile.y + 1,
                    &size,
                    w,
                    Style::default().bg(bg).fg(DIM),
                );
            } else {
                let t = format!("{name} {size}");
                buf.set_stringn(
                    tile.x + 1,
                    tile.y,
                    truncate(&t, w),
                    w,
                    Style::default().bg(bg).fg(TEXT),
                );
            }
        }
    }

    /// NAME · SIZE · MODIFIED · GIT · TYPE, the sorted one amber with ▾.
    fn render_list_header(&self, area: Rect, buf: &mut Buffer) {
        let w = area.width as usize;
        let cols_w = 6 + 1 + 12 + 1 + 3 + 1 + 4;
        let room = w.saturating_sub(cols_w + 1);
        let d = Style::default().fg(DIM);
        let on = Style::default().fg(AMBER);
        let pick = |k: ListSort| if self.sort == k { on } else { d };
        let arrow = |k: ListSort| if self.sort == k { "▾" } else { "" };
        buf.set_stringn(
            area.x + 2,
            area.y,
            format!("NAME{}", arrow(ListSort::Name)),
            room,
            pick(ListSort::Name),
        );
        let x = area.x + room as u16 + 1;
        buf.set_stringn(
            x,
            area.y,
            format!("{:>6}", format!("SIZE{}", arrow(ListSort::Size))),
            6,
            pick(ListSort::Size),
        );
        buf.set_stringn(
            x + 7,
            area.y,
            format!("MODIFIED{}", arrow(ListSort::Modified)),
            12,
            pick(ListSort::Modified),
        );
        buf.set_stringn(x + 20, area.y, "GIT", 3, d);
        buf.set_stringn(
            x + 24,
            area.y,
            format!("TYPE{}", arrow(ListSort::Type)),
            5,
            pick(ListSort::Type),
        );
        self.header.set(1);
    }

    fn render_hints(&self, area: Rect, buf: &mut Buffer) {
        if let Some(p) = &self.confirm_delete {
            let size = self
                .sizes
                .as_ref()
                .and_then(|(_, e, _)| {
                    let n = p.file_name()?.to_string_lossy().into_owned();
                    e.iter()
                        .find(|x| x.name == n)
                        .map(|x| crate::sysmon::human(x.bytes))
                })
                .unwrap_or_default();
            let t = format!(
                "delete {}{} ({size})? y moves it to the Trash · any other key keeps it",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                if p.is_dir() { "/" } else { "" }
            );
            buf.set_stringn(
                area.x,
                area.bottom() - 1,
                &t,
                area.width as usize,
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
            );
            return;
        }
        let hints = if self.editing.is_some() {
            if self.view == View::Find {
                "⏎ open/search · tab names⇄text · ↑↓ · esc done"
            } else {
                "type to filter · ⏎ keep · esc clear"
            }
        } else {
            match self.view {
                View::Find => "/ edit · ⏎ open · i insert · p preview · esc back",
                View::List if !self.marked.is_empty() => {
                    let bytes: u64 = self
                        .marked
                        .iter()
                        .filter_map(|p| std::fs::metadata(p).ok())
                        .map(|m| m.len())
                        .sum();
                    let t = format!(
                        "space select · s sort · {} selected ({}) → y copy paths · m move",
                        self.marked.len(),
                        crate::sysmon::human(bytes)
                    );
                    buf.set_stringn(
                        area.x,
                        area.bottom() - 1,
                        &t,
                        area.width as usize,
                        Style::default().fg(FAINT),
                    );
                    return;
                }
                View::List => {
                    "space select · s sort · y copy path · m move · ⏎ open · p preview · tab view"
                }
                View::Grid => "←↑↓→ move · ⏎ open · ⌫ up · i insert · p preview · tab view",
                View::Sizes => "⏎ drill in · x delete (asks first) · ⌫ up · r recount · tab view",
                View::Recent => {
                    "who changed what, newest first · ⏎ open · i insert · p preview · r refresh"
                }
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
                    let st = if it.dir {
                        base.patch(self.folder_style)
                    } else {
                        base
                    };
                    let mark = git.and_then(|g| g.mark(&it.path));
                    if self.view == View::List && w >= 44 {
                        // board 9: NAME · SIZE · MODIFIED · GIT · TYPE columns
                        let marked = self.marked.contains(&it.path);
                        let size = if it.dir {
                            "—".to_string()
                        } else {
                            crate::sysmon::human(it.size)
                        };
                        let modified = format!("{size:>6} {:<12} ", ago_long(it.mtime));
                        let gitc = format!("{:<3} ", mark.map(|m| m.symbol()).unwrap_or(""));
                        let typ = format!("{:<4}", kind(it));
                        let cols_w = 6 + 1 + 12 + 1 + 3 + 1 + 4;
                        let lead = if marked { "▣ " } else { "  " };
                        let name_st = if marked { st.fg(AMBER) } else { st };
                        let gcol = mark.map(mark_color).unwrap_or(FAINT);
                        let room = w.saturating_sub(cols_w + 1);
                        buf.set_stringn(
                            area.x,
                            y,
                            truncate(&format!("{lead}{name}"), room),
                            room,
                            name_st,
                        );
                        let cx = area.x + room as u16 + 1;
                        let (nx, _) = buf.set_stringn(cx, y, &modified, 20, base.fg(DIM));
                        let (nx, _) = buf.set_stringn(nx, y, &gitc, 4, base.fg(gcol));
                        buf.set_stringn(
                            nx,
                            y,
                            &typ,
                            area.right().saturating_sub(nx) as usize,
                            base.fg(DIM),
                        );
                        continue;
                    }
                    let mut right = String::new();
                    if self.view == View::List && w > 40 {
                        if !it.dir {
                            right.push_str(&crate::sysmon::human(it.size));
                        }
                        right.push_str(&format!(" {:>4}", ago(it.mtime)));
                    } else if self.view == View::Tree && w >= 40 && !it.dir {
                        right.push_str(&format!("{:>6}", crate::sysmon::human(it.size)));
                    }
                    let agent = self.agent_marks.get(&it.path);
                    let loading = self.loading_at(&it.path, it.dir);
                    let right = match (agent, mark) {
                        // a download is filling it: "◐ 42%"
                        _ if loading.is_some() => Some((
                            format!(
                                "{} {} ",
                                crate::loading::frame(),
                                loading
                                    .flatten()
                                    .map(|p| format!("{p:>3}%"))
                                    .unwrap_or_else(|| "writing…".into())
                            ),
                            base.fg(AMBER),
                        )),
                        // a coding agent changed it (on its branch): "◆ api M"
                        (Some((who, c)), m) => Some((
                            format!("◆ {who}{right} {}", m.map(|m| m.symbol()).unwrap_or(" ")),
                            base.fg(Color::Indexed(*c)),
                        )),
                        (None, Some(m)) => {
                            Some((format!("{right} {}", m.symbol()), base.fg(mark_color(m))))
                        }
                        (None, None) if !right.is_empty() => {
                            Some((format!("{right}  "), base.fg(FAINT)))
                        }
                        (None, None) => None,
                    };
                    (format!("{indent}{icon}{name}"), st, right)
                }
                Row::Up => ("  ../".into(), base.patch(self.folder_style), None),
                Row::More(n, depth) => (
                    format!("{}  … {n} more", "  ".repeat(*depth as usize)),
                    base.fg(FAINT),
                    None,
                ),
                Row::Note(t) => (format!("  {t}"), base.fg(DIM), None),
                Row::Heading(t) => (t.clone(), Style::default().fg(DIM), None),
                Row::Change(p, m) => {
                    let rel = p
                        .strip_prefix(&self.root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .into_owned();
                    let who = self
                        .agent_marks
                        .get(p)
                        .map(|(w, c)| (format!("◆ {w} "), base.fg(Color::Indexed(*c))));
                    (
                        format!("  {}  {rel}", m.symbol()),
                        base.fg(mark_color(*m)),
                        who,
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
                Row::Size(i) if w >= 30 => {
                    // board 10: name · size · a bar coloured by rank
                    let Some((_, e, _)) = &self.sizes else {
                        continue;
                    };
                    let Some(it) = e.get(*i) else { continue };
                    let biggest = e.first().map(|x| x.bytes).unwrap_or(1).max(1);
                    let bar_w = (w / 3).clamp(6, 30);
                    let filled = (((it.bytes as f64 / biggest as f64) * bar_w as f64).round()
                        as usize)
                        .max(1);
                    let col = rank_color(*i);
                    let size = format!("{:>8}  ", human_space(it.bytes));
                    let room = w.saturating_sub(bar_w + size.chars().count() + 1);
                    let name = if it.dir {
                        format!("{}/", it.name)
                    } else {
                        it.name.clone()
                    };
                    let nst = if it.dir { base.fg(BLUE) } else { base };
                    buf.set_stringn(area.x, y, truncate(&name, room), room, nst);
                    let x = area.x + room as u16 + 1;
                    let (x, _) = buf.set_stringn(x, y, &size, w, base.fg(TEXT));
                    let (x, _) =
                        buf.set_stringn(x, y, "━".repeat(filled.min(bar_w)), bar_w, base.fg(col));
                    buf.set_stringn(
                        x,
                        y,
                        "━".repeat(bar_w - filled.min(bar_w)),
                        bar_w,
                        base.fg(Color::Indexed(237)),
                    );
                    continue;
                }
                Row::Size(i) => {
                    let Some((_, e, _)) = &self.sizes else {
                        continue;
                    };
                    let Some(it) = e.get(*i) else { continue };
                    let total: u64 = e.iter().map(|x| x.bytes).sum::<u64>().max(1);
                    let biggest = e.first().map(|x| x.bytes).unwrap_or(1).max(1);
                    let size = crate::sysmon::human(it.bytes);
                    let pct = it.bytes * 100 / total;
                    let bar_w = (w / 3).clamp(4, 24);
                    let filled =
                        ((it.bytes as f64 / biggest as f64) * bar_w as f64).round() as usize;
                    let bar = format!(
                        "{}{}",
                        "█".repeat(filled),
                        "░".repeat(bar_w - filled.min(bar_w))
                    );
                    let name = if it.dir {
                        format!("▸ {}/", it.name)
                    } else {
                        format!("  {}", it.name)
                    };
                    let col = if pct >= 25 {
                        AMBER
                    } else if it.dir {
                        CYAN
                    } else {
                        DIM
                    };
                    (
                        name,
                        if it.dir {
                            base.patch(self.folder_style)
                        } else {
                            base
                        },
                        Some((format!("{bar} {size:>6} {pct:>3}% "), base.fg(col))),
                    )
                }
                Row::Recent(i) if w >= 44 => {
                    // board 10: time · who · path · what happened
                    let Some((_, f)) = &self.recent else { continue };
                    let Some((p, t, _)) = f.get(*i) else { continue };
                    let abs = self.root.join(p);
                    let secs = t
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let (who, wc) = match self.agent_marks.get(&abs) {
                        Some((n, c)) => (n.clone(), Color::Indexed(*c)),
                        None => ("you".to_string(), TEXT),
                    };
                    let (what, what_c) = match git.and_then(|g| g.mark(&abs)) {
                        Some(GitMark::Untracked) | Some(GitMark::Added) => ("created", GREEN),
                        Some(GitMark::Conflict) => ("conflict", RED),
                        Some(GitMark::Deleted) => ("deleted", RED),
                        Some(_) if who != "you" => ("edited", AMBER),
                        Some(_) => ("edited", AMBER),
                        None => ("saved", DIM),
                    };
                    let (x, _) = buf.set_stringn(
                        area.x,
                        y,
                        format!("{} ", crate::mcpscreen::hms(secs)),
                        w,
                        base.fg(DIM),
                    );
                    let (x, _) = buf.set_stringn(x, y, "● ", 2, base.fg(wc));
                    let who_w = 8usize;
                    let (_, _) =
                        buf.set_stringn(x, y, format!("{:<who_w$}", who), who_w, base.fg(wc));
                    let px = x + who_w as u16;
                    let room = area.right().saturating_sub(px) as usize;
                    let what_w = what.len() + 1;
                    buf.set_stringn(
                        px,
                        y,
                        truncate(p, room.saturating_sub(what_w + 1)),
                        room.saturating_sub(what_w + 1),
                        base,
                    );
                    buf.set_stringn(
                        area.right() - what.len() as u16,
                        y,
                        what,
                        what.len(),
                        base.fg(what_c),
                    );
                    continue;
                }
                Row::Recent(i) => {
                    let Some((_, f)) = &self.recent else { continue };
                    let Some((p, t, size)) = f.get(*i) else {
                        continue;
                    };
                    let right = if w > 40 {
                        format!("{} {:>5} ", crate::sysmon::human(*size), ago(Some(*t)))
                    } else {
                        format!("{:>5} ", ago(Some(*t)))
                    };
                    let mark = git.and_then(|g| g.mark(&self.root.join(p)));
                    let col = mark.map(mark_color).unwrap_or(FAINT);
                    (format!("  {p}"), base, Some((right, base.fg(col))))
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

    /// Columns view: the parent folder, with this folder highlighted.
    fn render_parent(&self, area: Rect, buf: &mut Buffer) {
        let Some(parent) = self.root.parent() else {
            buf.set_stringn(
                area.x + 1,
                area.y,
                "/",
                area.width as usize,
                Style::default().fg(FAINT),
            );
            return;
        };
        let names: Vec<(String, bool)> = match std::fs::read_dir(parent) {
            Ok(rd) => {
                let mut v: Vec<(String, bool)> = rd
                    .flatten()
                    .map(|e| {
                        let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        (e.file_name().to_string_lossy().into_owned(), dir)
                    })
                    .filter(|(n, _)| self.hidden || !n.starts_with('.'))
                    .take(MAX_DIR)
                    .collect();
                v.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                });
                v
            }
            Err(_) => Vec::new(),
        };
        let me = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let h = area.height as usize;
        let at = names.iter().position(|(n, _)| *n == me).unwrap_or(0);
        let top = at.saturating_sub(h / 2).min(names.len().saturating_sub(h));
        for (k, (n, dir)) in names.iter().enumerate().skip(top).take(h) {
            let y = area.y + (k - top) as u16;
            let here = *n == me;
            let st = if here {
                Style::default()
                    .bg(FAINT)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else if *dir {
                self.folder_style
            } else {
                Style::default().fg(DIM)
            };
            let text = format!(" {}{}", n, if *dir { "/" } else { "" });
            if here {
                buf.set_style(Rect::new(area.x, y, area.width, 1), st);
            }
            buf.set_stringn(
                area.x,
                y,
                truncate(&text, area.width as usize),
                area.width as usize,
                st,
            );
        }
    }

    /// Grid view: a card per entry (board 9), coloured by type, row by row.
    fn render_grid(&self, area: Rect, buf: &mut Buffer, focused: bool) {
        let gap = 1u16;
        let card_w = 18u16.min(area.width.max(4));
        let cols = ((area.width + gap) / (card_w + gap)).max(1) as usize;
        // stretch the cards to use the width
        let card_w = ((area.width + gap) / cols as u16)
            .saturating_sub(gap)
            .max(4);
        let card_h = 4u16;
        self.grid_cols.set(cols);
        let h = (area.height / card_h) as usize;
        if h == 0 {
            return;
        }
        let rows_needed = self.rows.len().div_ceil(cols);
        let sel_row = self.sel / cols;
        let mut top = self.scroll.get();
        if sel_row < top {
            top = sel_row;
        } else if sel_row >= top + h {
            top = sel_row + 1 - h;
        }
        top = top.min(rows_needed.saturating_sub(h));
        self.scroll.set(top);
        self.page.set((h * cols).max(1));
        for (i, row) in self.rows.iter().enumerate().skip(top * cols).take(h * cols) {
            let gy = area.y + (i / cols - top) as u16 * card_h;
            let gx = area.x + (i % cols) as u16 * (card_w + gap);
            let (name, info, color) = match row {
                Row::Entry { idx, .. } => {
                    let Some(it) = self.item(idx) else { continue };
                    let (info, color) = card_info(it, &self.grid_info);
                    let icon = if it.dir {
                        "▸"
                    } else if color == MAGENTA {
                        "■"
                    } else {
                        "·"
                    };
                    (
                        format!("{icon} {}{}", it.name, if it.dir { "/" } else { "" }),
                        info,
                        color,
                    )
                }
                Row::More(n, _) => (format!("… {n} more"), String::new(), FAINT),
                Row::Note(t) => {
                    buf.set_stringn(gx, gy, t, area.width as usize, Style::default().fg(DIM));
                    continue;
                }
                _ => continue,
            };
            let sel = i == self.sel;
            let border = if sel && focused {
                Style::default().fg(AMBER)
            } else if sel {
                Style::default().fg(DIM)
            } else {
                Style::default().fg(Color::Indexed(238))
            };
            let w = card_w as usize;
            buf.set_stringn(gx, gy, format!("╭{}╮", "─".repeat(w - 2)), w, border);
            buf.set_stringn(gx, gy + 3, format!("╰{}╯", "─".repeat(w - 2)), w, border);
            for dy in 1..3 {
                buf.set_string(gx, gy + dy, "│", border);
                buf.set_string(gx + card_w - 1, gy + dy, "│", border);
            }
            if sel {
                buf.set_style(
                    Rect::new(gx + 1, gy + 1, card_w - 2, 2),
                    Style::default().bg(SEL_BG),
                );
            }
            let inner = w.saturating_sub(3);
            let mut st = Style::default().fg(color);
            if sel && focused {
                st = st.add_modifier(Modifier::BOLD);
            }
            buf.set_stringn(gx + 2, gy + 1, truncate(&name, inner), inner, st);
            buf.set_stringn(
                gx + 2,
                gy + 2,
                truncate(&info, inner),
                inner,
                Style::default().fg(DIM),
            );
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

/// Are `needle`'s characters in `hay`, in order (smart case)?
pub fn subsequence(needle: &str, hay: &str) -> bool {
    let fold = !needle.chars().any(char::is_uppercase);
    let mut it = hay
        .chars()
        .map(|c| if fold { c.to_ascii_lowercase() } else { c });
    needle.chars().all(|n| it.any(|h| h == n))
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

/// A grid card's second line and colour: folders blue with their item count,
/// images magenta with their size in pixels, data green, logs amber.
fn card_info(it: &Item, cache: &RefCell<HashMap<PathBuf, String>>) -> (String, Color) {
    let ext = kind(it);
    let color = if it.dir {
        BLUE
    } else {
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => MAGENTA,
            "csv" | "json" | "parquet" | "sqlite" | "db" | "tsv" | "xlsx" => GREEN,
            "log" => AMBER,
            _ if it.name.starts_with('.') => DIM,
            _ => TEXT,
        }
    };
    if let Some(s) = cache.borrow().get(&it.path) {
        return (s.clone(), color);
    }
    let info = if it.dir {
        let n = std::fs::read_dir(&it.path)
            .map(|r| r.take(10_000).count())
            .unwrap_or(0);
        format!("{n} item{}", if n == 1 { "" } else { "s" })
    } else if it.name.starts_with('.') && !it.name[1..].contains('.') {
        "hidden".into()
    } else if ext == "png" {
        png_size(&it.path)
            .map(|(w, h)| format!("{w}×{h}"))
            .unwrap_or_else(|| crate::sysmon::human(it.size))
    } else {
        crate::sysmon::human(it.size)
    };
    cache.borrow_mut().insert(it.path.clone(), info.clone());
    (info, color)
}

/// The size view's colours by rank: biggest red, then amber, blue, grey.
fn rank_color(i: usize) -> Color {
    [RED, AMBER, BLUE].get(i).copied().unwrap_or(DIM)
}

/// The same, as dark tile backgrounds for the treemap.
fn rank_tint(i: usize) -> Color {
    [
        Color::Rgb(0x4a, 0x24, 0x2a),
        Color::Rgb(0x3d, 0x30, 0x18),
        Color::Rgb(0x1e, 0x2c, 0x45),
    ]
    .get(i)
    .copied()
    .unwrap_or(Color::Rgb(0x1e, 0x25, 0x30))
}

/// "912 MB", "1.4 MB", "38 KB" for the size view.
fn human_space(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1000.0 && u < U.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 10.0 || u == 0 {
        format!("{v:.0} {}", U[u])
    } else {
        format!("{v:.1} {}", U[u])
    }
}

/// Move a file or folder into the Trash (macOS ~/.Trash, else the
/// freedesktop trash). A move, never a delete; fails across disks.
fn to_trash(p: &Path) -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let dir = if cfg!(target_os = "macos") {
        home.join(".Trash")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("Trash/files")
    };
    std::fs::create_dir_all(&dir)?;
    let name = p
        .file_name()
        .ok_or_else(|| std::io::Error::other("no name"))?;
    let mut dest = dir.join(name);
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{} {n}", name.to_string_lossy()));
        n += 1;
    }
    std::fs::rename(p, &dest)?;
    Ok(dest)
}

/// Width and height from a PNG's header.
fn png_size(p: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = [0u8; 24];
    std::fs::File::open(p).ok()?.read_exact(&mut head).ok()?;
    if &head[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(head[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(head[20..24].try_into().ok()?);
    Some((w, h))
}

/// "py", "rs", "dir" — the List view's TYPE column.
fn kind(it: &Item) -> String {
    if it.dir {
        return "dir".into();
    }
    match it.name.rsplit_once('.') {
        Some((a, e)) if !a.is_empty() => e.to_lowercase(),
        _ => "file".into(),
    }
}

/// "2 min ago", "1 h ago", "yesterday", "3 days ago".
fn ago_long(t: Option<SystemTime>) -> String {
    let Some(d) = t.and_then(|t| t.elapsed().ok()) else {
        return String::new();
    };
    let s = d.as_secs();
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        172_800..1_209_600 => format!("{} days ago", s / 86_400),
        1_209_600..5_184_000 => format!("{} weeks ago", s / 604_800),
        5_184_000..31_536_000 => format!("{} months ago", s / 2_592_000),
        _ => format!("{} years ago", s / 31_536_000),
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
        Job::Sizes { root, hidden } => {
            let (entries, partial) = sizes(&root, hidden);
            Done::Sizes {
                root,
                entries,
                partial,
            }
        }
        Job::Recent { root } => {
            let files = recent(&root, MAX_RECENT);
            Done::Recent { root, files }
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

/// The folder's entries with their total size (folders counted all the way
/// down, without following links), biggest first.
pub fn sizes(root: &Path, hidden: bool) -> (Vec<Sized>, bool) {
    fn walk(dir: &Path, budget: &mut u64) -> (u64, u64) {
        let (mut bytes, mut files) = (0, 0);
        let Ok(rd) = std::fs::read_dir(dir) else {
            return (0, 0);
        };
        for e in rd.flatten() {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let (b, f) = walk(&e.path(), budget);
                bytes += b;
                files += f;
            } else if ft.is_file() {
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                files += 1;
            }
        }
        (bytes, files)
    }
    let mut budget = MAX_SIZE_WALK;
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let (bytes, files, dir) = if ft.is_dir() {
                let (b, f) = walk(&e.path(), &mut budget);
                (b, f, true)
            } else if ft.is_file() {
                (e.metadata().map(|m| m.len()).unwrap_or(0), 1, false)
            } else {
                continue;
            };
            out.push(Sized {
                name,
                dir,
                bytes,
                files,
            });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    (out, budget == 0)
}

/// The most recently changed files under `root` (same list as Find: git's
/// files, or a walk that skips heavy folders), newest first.
pub fn recent(root: &Path, keep: usize) -> Vec<(String, SystemTime, u64)> {
    let (files, _) = index(root, MAX_INDEX);
    let mut out: Vec<(String, SystemTime, u64)> = files
        .into_iter()
        .filter_map(|rel| {
            let m = std::fs::metadata(root.join(&rel)).ok()?;
            Some((rel, m.modified().ok()?, m.len()))
        })
        .collect();
    out.sort_by_key(|f| std::cmp::Reverse(f.1));
    out.truncate(keep);
    out
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

    /// Run the pane's background jobs right here.
    fn settle(p: &mut FilesPane, jobs: Vec<Job>) {
        for j in jobs {
            let more = p.done(run_job(j)).jobs;
            settle(p, more);
        }
    }

    #[test]
    fn columns_grid_sizes_and_recent_views() {
        let d = tmp("views");
        std::fs::write(d.join("docs/big.bin"), vec![0u8; 50_000]).unwrap();
        let (mut p, jobs) = FilesPane::new(d.clone());
        settle(&mut p, jobs);
        // columns: the parent folder on the left, this one on the right
        let out = p.key(b"2");
        settle(&mut p, out.jobs);
        assert_eq!(p.view, View::Columns);
        let me = d.file_name().unwrap().to_string_lossy().to_string();
        let s = screen(&p);
        assert!(
            s.contains(&me[..12]) && s.contains("│") && s.contains("README.md"),
            "{s}"
        );
        // grid: arrows move across, ⏎ opens a folder
        let out = p.key(b"4");
        settle(&mut p, out.jobs);
        assert_eq!(p.view, View::Grid);
        let s = screen(&p);
        assert!(
            s.contains("▸ docs/")
                && s.contains("▸ src/")
                && s.contains("· README.md")
                && s.contains("2 items"),
            "{s}"
        );
        assert_eq!(
            s.lines()
                .filter(|l| l.contains("docs/") && l.contains("src/"))
                .count(),
            1,
            "same row: {s}"
        );
        p.key(b"\x1b[C"); // → src
        let out = p.key(b"\r");
        settle(&mut p, out.jobs);
        assert_eq!(p.root, d.join("src"));
        assert_eq!(p.view, View::Grid);
        p.key(b"\x7f");
        // sizes: biggest first, as bars
        let out = p.key(b"5");
        settle(&mut p, out.jobs);
        assert_eq!(p.view, View::Sizes);
        let s = screen(&p);
        let docs = s.lines().position(|l| l.starts_with("docs/")).unwrap();
        let src = s.lines().position(|l| l.starts_with("src/")).unwrap();
        assert!(docs < src, "biggest first: {s}");
        assert!(s.contains('━') && s.contains("49 KB"), "{s}");
        // into a folder and back
        p.sel = p
            .rows
            .iter()
            .position(|r| matches!(r, Row::Size(_)))
            .unwrap();
        let out = p.key(b"\r");
        settle(&mut p, out.jobs);
        assert_eq!(p.root, d.join("docs"));
        assert!(screen(&p).contains("big.bin"));
        let out = p.key(b"\x7f");
        settle(&mut p, out.jobs);
        // recent: newest first, anywhere below
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.join("src/core/new.py"), "x = 1\n").unwrap();
        let out = p.key(b"8");
        settle(&mut p, out.jobs);
        let s = screen(&p);
        let first = s
            .lines()
            .find(|l| l.contains(".py") || l.contains(".rs") || l.contains(".md"))
            .unwrap_or("");
        assert!(first.contains("src/core/new.py"), "{s}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn list_view_navigates_and_preview_highlights() {
        let d = tmp("list");
        let (mut p, _) = FilesPane::new(d.clone());
        p.key(b"3");
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
    fn list_columns_sort_select_and_size_delete_asks() {
        let d = tmp("listcols");
        std::fs::write(d.join("big.txt"), vec![b'x'; 9000]).unwrap();
        let (mut p, jobs) = FilesPane::new(d.clone());
        settle(&mut p, jobs);
        p.key(b"3");
        let s = screen(&p);
        assert!(
            s.contains("NAME▾") && s.contains("MODIFIED") && s.contains("TYPE"),
            "{s}"
        );
        // s: by size, biggest first
        p.key(b"s");
        let s = screen(&p);
        assert!(s.contains("SIZE▾"), "{s}");
        let big = s.lines().position(|l| l.contains("big.txt")).unwrap();
        let readme = s.lines().position(|l| l.contains("README.md")).unwrap();
        assert!(big < readme, "{s}");
        // space marks two, y copies both paths
        p.sel = p
            .rows
            .iter()
            .position(|r| matches!(r, Row::Entry { .. }))
            .unwrap();
        p.key(b" ");
        p.key(b" ");
        assert!(screen(&p).contains("2 selected"), "{}", screen(&p));
        let out = p.key(b"y");
        assert!(matches!(&out.acts[0], Act::Copy(t) if t.lines().count() == 2));
        // size view: x asks, anything but y keeps it
        let out = p.key(b"5");
        settle(&mut p, out.jobs);
        p.sel = p
            .rows
            .iter()
            .position(|r| matches!(r, Row::Size(_)))
            .unwrap();
        p.key(b"x");
        assert!(
            screen(&p).contains("moves it to the Trash"),
            "{}",
            screen(&p)
        );
        p.key(b"n");
        assert!(d.join("big.txt").exists() && d.join("docs").exists());
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
        assert!(subsequence("alrt", "src/core/alerts.py"));
        assert!(!subsequence("trla", "src/core/alerts.py"));
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}

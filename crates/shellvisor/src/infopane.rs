//! Board 4's other pane kinds: Preview (the file selected in the tab's files
//! pane), Agents (every agent at a glance) and Activity (downloads and
//! installs, running and finished). What they show lives in the app; this
//! holds where each one is scrolled to and the preview it last read.

use ratatui::text::Line;
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoKind {
    Preview,
    Agents,
    Activity,
}

impl InfoKind {
    pub fn parse(s: &str) -> Option<InfoKind> {
        Some(match s {
            "preview" => InfoKind::Preview,
            "agents" => InfoKind::Agents,
            "activity" => InfoKind::Activity,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            InfoKind::Preview => "preview",
            InfoKind::Agents => "agents",
            InfoKind::Activity => "activity",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            InfoKind::Preview => "PREVIEW",
            InfoKind::Agents => "AGENTS",
            InfoKind::Activity => "ACTIVITY",
        }
    }
}

/// A preview read from disk: (path, modified, lines, note).
pub type Cached = (PathBuf, Option<SystemTime>, Vec<Line<'static>>, String);

pub struct InfoPane {
    pub kind: InfoKind,
    /// The highlighted row (Agents, Activity) or first line shown (Preview).
    pub sel: usize,
    cache: RefCell<Option<Cached>>,
}

impl InfoPane {
    pub fn new(kind: InfoKind) -> Self {
        InfoPane {
            kind,
            sel: 0,
            cache: RefCell::new(None),
        }
    }

    pub fn title(&self) -> String {
        self.kind.name().to_string()
    }

    /// The preview of `path`, read again only when it changed on disk.
    pub fn preview(
        &self,
        path: &std::path::Path,
        dir: bool,
        hidden: bool,
    ) -> (Vec<Line<'static>>, String) {
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((p, t, lines, note)) = self.cache.borrow().as_ref() {
            if p == path && *t == mtime {
                return (lines.clone(), note.clone());
            }
        }
        let (lines, note) = crate::files::preview_of(path, dir, hidden);
        *self.cache.borrow_mut() = Some((path.to_path_buf(), mtime, lines.clone(), note.clone()));
        (lines, note)
    }

    /// ↑↓ / j k / page keys; `n` rows. True if it moved.
    pub fn nav(&mut self, key: &[u8], n: usize) -> bool {
        let last = n.saturating_sub(1);
        let before = self.sel;
        self.sel = match key {
            b"\x1b[A" | b"\x1bOA" | b"k" | b"K" => self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" | b"j" | b"J" => (self.sel + 1).min(last),
            b"\x1b[5~" => self.sel.saturating_sub(10),
            b"\x1b[6~" => (self.sel + 10).min(last),
            b"g" | b"\x1b[H" => 0,
            b"G" | b"\x1b[F" => last,
            _ => return false,
        };
        self.sel != before
    }
}

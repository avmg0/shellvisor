//! The layout editor (alt+e): reshape the current tab live — split with a
//! pane from the palette, close, resize, move between panes — then save it
//! by name, as the default for new sessions, or for this folder.
//!
//! Saved layouts are small YAML files in `~/.config/shellvisor/layouts/`
//! (the same shape as a tab in session.yaml, without folders: they open
//! wherever you are). Which one is the default, and which folders have their
//! own, lives in `~/.local/state/shellvisor/layouts.yaml`.

use crate::keys::Dir;
use crate::session::SavedNode;
use crate::workspace::Axis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a split can add.
pub const KINDS: [&str; 4] = ["shell", "files", "system", "flow"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveAs {
    /// Just by name.
    Name,
    /// …and used for new sessions.
    Default,
    /// …and used when Shellvisor starts in this folder (or below it).
    Folder,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Edit,
    /// Typing a name to save as.
    Name(String, SaveAs),
    /// Picking a saved layout to open: (names, selected).
    Open(Vec<String>, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    /// Index into KINDS: what a split adds.
    pub kind: usize,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EdAct {
    Close,
    Focus(Dir),
    Resize(Dir),
    Split(Axis, &'static str),
    ClosePane,
    Save(String, SaveAs),
    Open(String),
}

impl Default for Editor {
    fn default() -> Self {
        Editor {
            kind: 0,
            mode: Mode::Edit,
        }
    }
}

impl Editor {
    /// A key; returns what the app should do.
    pub fn key(&mut self, key: &[u8], saved: impl FnOnce() -> Vec<String>) -> Option<EdAct> {
        match &mut self.mode {
            Mode::Name(name, how) => {
                match key {
                    b"\r" => {
                        let n = name.trim().to_string();
                        let how = *how;
                        self.mode = Mode::Edit;
                        if valid(&n) {
                            return Some(EdAct::Save(n, how));
                        }
                    }
                    b"\x1b" => self.mode = Mode::Edit,
                    [0x7f] | [0x08] => {
                        name.pop();
                    }
                    k => {
                        if let Ok(s) = std::str::from_utf8(k) {
                            if s.chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                                && name.len() < 40
                            {
                                name.push_str(s);
                            }
                        }
                    }
                }
                None
            }
            Mode::Open(names, sel) => {
                match key {
                    b"\x1b[A" | b"\x1bOA" | b"k" => *sel = sel.saturating_sub(1),
                    b"\x1b[B" | b"\x1bOB" | b"j" => {
                        *sel = (*sel + 1).min(names.len().saturating_sub(1))
                    }
                    b"\r" => {
                        let pick = names.get(*sel).cloned();
                        self.mode = Mode::Edit;
                        return pick.map(EdAct::Open);
                    }
                    _ => self.mode = Mode::Edit,
                }
                None
            }
            Mode::Edit => Some(match key {
                b"\x1b[A" | b"\x1bOA" => EdAct::Focus(Dir::Up),
                b"\x1b[B" | b"\x1bOB" => EdAct::Focus(Dir::Down),
                b"\x1b[C" | b"\x1bOC" => EdAct::Focus(Dir::Right),
                b"\x1b[D" | b"\x1bOD" => EdAct::Focus(Dir::Left),
                b"\x1b[1;2A" | b"K" => EdAct::Resize(Dir::Up),
                b"\x1b[1;2B" | b"J" => EdAct::Resize(Dir::Down),
                b"\x1b[1;2C" | b"L" | b">" => EdAct::Resize(Dir::Right),
                b"\x1b[1;2D" | b"H" | b"<" => EdAct::Resize(Dir::Left),
                [d @ b'1'..=b'4'] => {
                    self.kind = (d - b'1') as usize;
                    return None;
                }
                b"\t" => {
                    self.kind = (self.kind + 1) % KINDS.len();
                    return None;
                }
                b"\\" | b"|" | b"v" => EdAct::Split(Axis::Horizontal, KINDS[self.kind]),
                b"-" | b"_" | b"h" => EdAct::Split(Axis::Vertical, KINDS[self.kind]),
                b"x" => EdAct::ClosePane,
                b"s" => {
                    self.mode = Mode::Name(String::new(), SaveAs::Name);
                    return None;
                }
                b"d" => {
                    self.mode = Mode::Name("default".into(), SaveAs::Default);
                    return None;
                }
                b"f" => {
                    self.mode = Mode::Name(String::new(), SaveAs::Folder);
                    return None;
                }
                b"o" => {
                    let names = saved();
                    if names.is_empty() {
                        return None;
                    }
                    self.mode = Mode::Open(names, 0);
                    return None;
                }
                b"\x1b" | b"q" | b"\r" | b"\x1be" => EdAct::Close,
                _ => return None,
            }),
        }
    }

    /// The editor's panel: lines to show.
    pub fn lines(&self, prefs: &Prefs, here: &Path) -> Vec<String> {
        let palette: Vec<String> = KINDS
            .iter()
            .enumerate()
            .map(|(i, k)| {
                if i == self.kind {
                    format!("[{} {k}]", i + 1)
                } else {
                    format!(" {} {k} ", i + 1)
                }
            })
            .collect();
        let mut v = vec![format!("add: {}", palette.join(""))];
        match &self.mode {
            Mode::Edit => {
                v.push("←↑↓→ pane · \\ split right · - split below · x close · < > J K resize · drag borders with the mouse".into());
                v.push("s save as… · d save as default · f save for this folder · o open saved · esc done".into());
                let mut facts = Vec::new();
                if let Some(d) = &prefs.default {
                    facts.push(format!("default: {d}"));
                }
                if let Some((dir, n)) = prefs.for_folder(here) {
                    facts.push(format!("{} → {n}", crate::ui::short_path(&dir)));
                }
                if !facts.is_empty() {
                    v.push(facts.join(" · "));
                }
            }
            Mode::Name(n, how) => {
                let what = match how {
                    SaveAs::Name => "save layout as",
                    SaveAs::Default => "save as the default layout, named",
                    SaveAs::Folder => "save for this folder, named",
                };
                v.push(format!("{what}: {n}▏"));
                v.push("letters, digits, - and _ · ⏎ save · esc cancel".into());
            }
            Mode::Open(names, sel) => {
                for (i, n) in names.iter().enumerate().take(8) {
                    v.push(format!("{} {n}", if i == *sel { "▸" } else { " " }));
                }
                v.push("↑↓ pick · ⏎ open in a new tab (or this one, if it has a single shell) · esc back".into());
            }
        }
        v
    }
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ------------------------------------------------------------------ storage

pub fn layouts_dir() -> PathBuf {
    crate::paths::config_file()
        .parent()
        .map(|p| p.join("layouts"))
        .unwrap_or_else(|| PathBuf::from("layouts"))
}

fn prefs_file() -> PathBuf {
    crate::paths::state_dir().join("layouts.yaml")
}

/// The default layout, and folders with their own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<String, String>,
}

impl Prefs {
    pub fn load() -> Prefs {
        Self::load_from(&prefs_file())
    }
    pub fn load_from(p: &Path) -> Prefs {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_yaml_ng::from_str(&t).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&prefs_file())
    }
    pub fn save_to(&self, p: &Path) -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            p,
            serde_yaml_ng::to_string(self).map_err(std::io::Error::other)?,
        )
    }

    /// The layout for `dir`: its own folder's, else the closest parent's.
    pub fn for_folder(&self, dir: &Path) -> Option<(String, String)> {
        self.folders
            .iter()
            .filter(|(f, _)| dir.starts_with(f.as_str()))
            .max_by_key(|(f, _)| f.len())
            .map(|(f, n)| (f.clone(), n.clone()))
    }

    /// What to open when Shellvisor starts in `dir`.
    pub fn start_layout(&self, dir: &Path) -> Option<String> {
        self.for_folder(dir)
            .map(|(_, n)| n)
            .or_else(|| self.default.clone())
    }
}

pub fn save_layout(name: &str, node: &SavedNode) -> std::io::Result<PathBuf> {
    save_layout_in(&layouts_dir(), name, node)
}

pub fn save_layout_in(dir: &Path, name: &str, node: &SavedNode) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = dir.join(format!("{name}.yaml"));
    let text = format!(
        "# Shellvisor layout — open with: sv layout {name}\n{}",
        serde_yaml_ng::to_string(node).map_err(std::io::Error::other)?
    );
    std::fs::write(&p, text)?;
    Ok(p)
}

pub fn load_layout(name: &str) -> Result<SavedNode, String> {
    load_layout_in(&layouts_dir(), name)
}

pub fn load_layout_in(dir: &Path, name: &str) -> Result<SavedNode, String> {
    let p = dir.join(format!("{name}.yaml"));
    let text = std::fs::read_to_string(&p).map_err(|_| format!("no saved layout called {name}"))?;
    serde_yaml_ng::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn saved_layouts() -> Vec<String> {
    saved_in(&layouts_dir())
}

pub fn saved_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_suffix(".yaml").map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SavedAxis;

    #[test]
    fn editor_keys() {
        let mut e = Editor::default();
        assert_eq!(
            e.key(b"\\", Vec::new),
            Some(EdAct::Split(Axis::Horizontal, "shell"))
        );
        assert_eq!(e.key(b"2", Vec::new), None);
        assert_eq!(
            e.key(b"-", Vec::new),
            Some(EdAct::Split(Axis::Vertical, "files"))
        );
        assert_eq!(e.key(b">", Vec::new), Some(EdAct::Resize(Dir::Right)));
        assert_eq!(e.key(b"s", Vec::new), None);
        for k in ["m", "y", " ", "/", "-", "1"] {
            e.key(k.as_bytes(), Vec::new);
        }
        assert_eq!(
            e.key(b"\r", Vec::new),
            Some(EdAct::Save("my-1".into(), SaveAs::Name))
        );
        assert_eq!(e.key(b"o", Vec::new), None, "nothing saved: stays in edit");
        assert_eq!(e.key(b"o", || vec!["a".into(), "b".into()]), None);
        e.key(b"j", Vec::new);
        assert_eq!(e.key(b"\r", Vec::new), Some(EdAct::Open("b".into())));
        assert_eq!(e.key(b"\x1b", Vec::new), Some(EdAct::Close));
    }

    #[test]
    fn save_load_and_folder_prefs() {
        let dir = std::env::temp_dir().join(format!("sv-layouts-{}", std::process::id()));
        let node = SavedNode::Split {
            split: SavedAxis::SideBySide,
            ratio: 0.3,
            a: Box::new(SavedNode::Pane {
                cwd: String::new(),
                tool: Some("files".into()),
            }),
            b: Box::new(SavedNode::Pane {
                cwd: String::new(),
                tool: None,
            }),
        };
        save_layout_in(&dir, "code", &node).unwrap();
        assert_eq!(load_layout_in(&dir, "code").unwrap(), node);
        assert_eq!(saved_in(&dir), vec!["code".to_string()]);
        assert!(
            load_layout_in(&dir, "nope")
                .unwrap_err()
                .contains("no saved layout")
        );
        let mut p = Prefs {
            default: Some("plain".into()),
            ..Default::default()
        };
        p.folders.insert("/work/app".into(), "code".into());
        p.folders.insert("/work".into(), "wide".into());
        assert_eq!(
            p.start_layout(Path::new("/work/app/src")),
            Some("code".into())
        );
        assert_eq!(
            p.start_layout(Path::new("/work/other")),
            Some("wide".into())
        );
        assert_eq!(p.start_layout(Path::new("/tmp")), Some("plain".into()));
        let f = dir.join("prefs.yaml");
        p.save_to(&f).unwrap();
        assert_eq!(Prefs::load_from(&f), p);
        let _ = std::fs::remove_dir_all(dir);
    }
}

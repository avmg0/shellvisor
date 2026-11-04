//! Remembering tabs, splits and folders between runs.
//!
//! Saved to `~/.local/state/shellvisor/session.yaml` whenever the layout or a
//! folder changes (so it survives closing the terminal window), and offered
//! back on the next start. Commands are never re-run — only folders, splits,
//! tab names and the view are restored.

use crate::workspace::{Axis, Node, PaneId};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    /// "full" or "basic"
    pub view: String,
    pub active: usize,
    pub tabs: Vec<SavedTab>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub zoomed: bool,
    /// Index of the focused split, in reading order.
    #[serde(default)]
    pub focus: usize,
    pub layout: SavedNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SavedNode {
    Split {
        split: SavedAxis,
        ratio: f32,
        a: Box<SavedNode>,
        b: Box<SavedNode>,
    },
    Pane {
        cwd: String,
        /// "files" or "system" for a tool pane; absent for a shell.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
}

/// What a saved leaf holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Leaf {
    pub cwd: String,
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedAxis {
    SideBySide,
    Stacked,
}

impl SavedNode {
    /// Capture a live tree, looking up each pane's folder.
    pub fn capture(node: &Node, leaf: &impl Fn(PaneId) -> Leaf) -> SavedNode {
        match node {
            Node::Leaf(id) => {
                let l = leaf(*id);
                SavedNode::Pane {
                    cwd: l.cwd,
                    tool: l.tool,
                }
            }
            Node::Split { axis, ratio, a, b } => SavedNode::Split {
                split: match axis {
                    Axis::Horizontal => SavedAxis::SideBySide,
                    Axis::Vertical => SavedAxis::Stacked,
                },
                ratio: *ratio,
                a: Box::new(SavedNode::capture(a, leaf)),
                b: Box::new(SavedNode::capture(b, leaf)),
            },
        }
    }

    /// Rebuild a live tree, creating a pane for each saved folder
    /// (in reading order) with `spawn`.
    pub fn rebuild<E>(
        &self,
        spawn: &mut impl FnMut(&Leaf) -> Result<PaneId, E>,
    ) -> Result<Node, E> {
        Ok(match self {
            SavedNode::Pane { cwd, tool } => Node::Leaf(spawn(&Leaf {
                cwd: cwd.clone(),
                tool: tool.clone(),
            })?),
            SavedNode::Split { split, ratio, a, b } => Node::Split {
                axis: match split {
                    SavedAxis::SideBySide => Axis::Horizontal,
                    SavedAxis::Stacked => Axis::Vertical,
                },
                ratio: ratio.clamp(0.1, 0.9),
                a: Box::new(a.rebuild(spawn)?),
                b: Box::new(b.rebuild(spawn)?),
            },
        })
    }

    pub fn folders(&self) -> Vec<&str> {
        match self {
            SavedNode::Pane { cwd, .. } if cwd.is_empty() => vec![],
            SavedNode::Pane { cwd, .. } => vec![cwd.as_str()],
            SavedNode::Split { a, b, .. } => {
                let mut v = a.folders();
                v.extend(b.folders());
                v
            }
        }
    }
}

impl Session {
    pub fn load(path: &Path) -> Option<Session> {
        let text = std::fs::read_to_string(path).ok()?;
        let s: Session = serde_yaml_ng::from_str(&text).ok()?;
        (s.version == VERSION && !s.tabs.is_empty()).then_some(s)
    }

    /// Write atomically (temp file + rename) so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_yaml_ng::to_string(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(
            &tmp,
            format!("# Shellvisor session — rewritten automatically\n{text}"),
        )?;
        std::fs::rename(&tmp, path)
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    /// "3 tabs: ~/api, ~/web, /tmp"
    pub fn summary(&self, short: impl Fn(&str) -> String) -> String {
        let folders: Vec<String> = self
            .tabs
            .iter()
            .map(|t| {
                t.name
                    .clone()
                    .unwrap_or_else(|| short(t.layout.folders().first().copied().unwrap_or("")))
            })
            .collect();
        let n = self.tabs.len();
        let shown: Vec<&str> = folders.iter().take(3).map(String::as_str).collect();
        let more = if n > 3 {
            format!(" +{}", n - 3)
        } else {
            String::new()
        };
        format!(
            "{n} tab{}: {}{more}",
            if n == 1 { "" } else { "s" },
            shown.join(", ")
        )
    }

    /// Is this just one fresh tab in `cwd` — i.e. nothing worth restoring?
    pub fn is_trivial(&self, cwd: &str) -> bool {
        self.tabs.len() == 1
            && self.tabs[0].name.is_none()
            && self.tabs[0].layout
                == SavedNode::Pane {
                    cwd: cwd.to_string(),
                    tool: None,
                }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> Node {
        let mut n = Node::Leaf(1);
        n.split(1, Axis::Horizontal, 2);
        n.split(2, Axis::Vertical, 3);
        n
    }

    #[test]
    fn capture_and_rebuild_round_trip() {
        let tree = sample_tree();
        let folder = |id: PaneId| Leaf {
            cwd: format!("/work/{id}"),
            tool: (id == 3).then(|| "files".to_string()),
        };
        let saved = SavedNode::capture(&tree, &folder);
        assert_eq!(saved.folders(), vec!["/work/1", "/work/2", "/work/3"]);

        let mut next = 10;
        let mut spawned = Vec::new();
        let rebuilt = saved
            .rebuild(&mut |l: &Leaf| -> Result<PaneId, ()> {
                spawned.push(format!(
                    "{}{}",
                    l.cwd,
                    l.tool
                        .as_deref()
                        .map(|t| format!(" {t}"))
                        .unwrap_or_default()
                ));
                next += 1;
                Ok(next)
            })
            .unwrap();
        assert_eq!(spawned, vec!["/work/1", "/work/2", "/work/3 files"]);
        assert_eq!(rebuilt.panes(), vec![11, 12, 13]);
        // same shape as the original
        let mut a = Vec::new();
        let mut b = Vec::new();
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        tree.layout(area, &mut a);
        rebuilt.layout(area, &mut b);
        let rects = |v: Vec<(PaneId, ratatui::layout::Rect)>| {
            v.into_iter().map(|(_, r)| r).collect::<Vec<_>>()
        };
        assert_eq!(rects(a), rects(b));
    }

    #[test]
    fn yaml_file_round_trip() {
        let s = Session {
            version: VERSION,
            view: "full".into(),
            active: 1,
            tabs: vec![
                SavedTab {
                    name: Some("api".into()),
                    zoomed: false,
                    focus: 2,
                    layout: SavedNode::capture(&sample_tree(), &|id| Leaf {
                        cwd: format!("/p/{id}"),
                        tool: (id == 2).then(|| "system".to_string()),
                    }),
                },
                SavedTab {
                    name: None,
                    zoomed: true,
                    focus: 0,
                    layout: SavedNode::Pane {
                        cwd: "/tmp".into(),
                        tool: None,
                    },
                },
            ],
        };
        let dir = std::env::temp_dir().join(format!("sv-session-{}", std::process::id()));
        let path = dir.join("session.yaml");
        s.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("side_by_side")
                && text.contains("stacked")
                && text.contains("tool: system"),
            "{text}"
        );
        assert_eq!(Session::load(&path), Some(s.clone()));
        assert_eq!(s.summary(|p| p.to_string()), "2 tabs: api, /tmp");
        Session::remove(&path);
        assert_eq!(Session::load(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn junk_and_old_versions_are_ignored() {
        let dir = std::env::temp_dir().join(format!("sv-session-junk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.yaml");
        std::fs::write(&path, "this: [is not a session").unwrap();
        assert_eq!(Session::load(&path), None);
        std::fs::write(&path, "version: 99\nview: full\nactive: 0\ntabs: []\n").unwrap();
        assert_eq!(Session::load(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn trivial_sessions_are_not_offered() {
        let s = Session {
            version: VERSION,
            view: "full".into(),
            active: 0,
            tabs: vec![SavedTab {
                name: None,
                zoomed: false,
                focus: 0,
                layout: SavedNode::Pane {
                    cwd: "/home/a".into(),
                    tool: None,
                },
            }],
        };
        assert!(s.is_trivial("/home/a"));
        assert!(!s.is_trivial("/elsewhere"));
    }
}

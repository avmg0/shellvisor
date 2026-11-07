//! The layout editor (alt+e): a screen of its own for shaping the current
//! tab — preset thumbnails to start from, a palette of pane kinds, a map of
//! the tab you edit (select, split, remove, resize with keys or the mouse,
//! undo), and a form to save it by name, as your default, or for this folder,
//! with the YAML that will be written.
//!
//! Edits happen on a model of the tab; nothing changes until you press ⏎, and
//! esc puts everything back. Panes you keep go on running throughout.
//!
//! Saved layouts are small YAML files in `~/.config/shellvisor/layouts/`
//! (the same shape as a tab in session.yaml, without folders: they open
//! wherever you are). Which one is the default, and which folders have their
//! own, lives in `~/.local/state/shellvisor/layouts.yaml`.

use crate::keys::Dir;
use crate::session::{SavedAxis, SavedNode};
use crate::workspace::{Axis, Node, PaneId, split_rect};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const CYAN: Color = Color::Indexed(73);
const GREEN: Color = Color::Indexed(108);
const BLUE: Color = Color::Indexed(110);
const RED: Color = Color::Indexed(167);
const SEL_BG: Color = Color::Indexed(237);
const TEXT: Color = Color::Indexed(252);
const INK: Color = Color::Indexed(16);

/// What a pane can be. `tool` is how it is saved (`None` = a shell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub name: &'static str,
    pub tool: Option<&'static str>,
    pub note: &'static str,
}

/// The palette.
pub const KINDS: [Kind; 9] = [
    Kind {
        name: "Shell",
        tool: None,
        note: "zsh / bash",
    },
    Kind {
        name: "Files",
        tool: Some("files"),
        note: "8 views",
    },
    Kind {
        name: "Git changes",
        tool: Some("git"),
        note: "files · git view",
    },
    Kind {
        name: "System stats",
        tool: Some("system"),
        note: "cpu mem disk net",
    },
    Kind {
        name: "Thermals",
        tool: Some("thermal"),
        note: "sensors · speed",
    },
    Kind {
        name: "Data flow",
        tool: Some("flow"),
        note: "jobs · sv flow",
    },
    Kind {
        name: "Preview",
        tool: Some("preview"),
        note: "the file picked in files",
    },
    Kind {
        name: "Agents",
        tool: Some("agents"),
        note: "state · tokens · changes",
    },
    Kind {
        name: "Activity",
        tool: Some("activity"),
        note: "downloads · installs",
    },
];

pub fn kind_of(tool: Option<&str>) -> Kind {
    KINDS
        .iter()
        .copied()
        .find(|k| k.tool == tool)
        .unwrap_or(KINDS[0])
}

/// One pane of the model: a pane that exists (kept running), or one to create.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub existing: Option<PaneId>,
    pub kind: Kind,
}

/// A layout to start from: built-in presets and your saved layouts.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub saved: bool,
    pub node: SavedNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Map,
    Name,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    /// The tab being edited: leaves are indexes into `slots`.
    pub tree: Node,
    pub slots: Vec<Slot>,
    pub sel: u32,
    /// Palette item that a split adds.
    pub kind: usize,
    pub field: Field,
    pub name: String,
    pub make_default: bool,
    pub for_folder: bool,
    pub folder: PathBuf,
    pub presets: Vec<Preset>,
    undo: Vec<(Node, Vec<Slot>, u32)>,
    /// Dragging a divider in the map: its path.
    drag: Option<Vec<bool>>,
    /// Board 4: a palette kind being dragged, and where it would land.
    carry: Option<(usize, Option<(u32, Zone)>)>,
    /// Where the map was drawn (for the mouse).
    map_area: std::cell::Cell<Rect>,
    /// Palette rows and preset thumbnails as drawn (for clicks).
    hits: std::cell::RefCell<Vec<(Rect, Hit)>>,
    pub message: Option<String>,
}

/// Where a dragged pane lands on another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// A split to its right.
    Right,
    /// A split below it.
    Below,
    /// Takes its place (changes its kind).
    Replace,
}

impl Zone {
    /// The part of a pane's box the zone covers.
    fn area(self, r: Rect) -> Rect {
        match self {
            Zone::Right => Rect::new(r.x + r.width / 2, r.y, r.width - r.width / 2, r.height),
            Zone::Below => Rect::new(r.x, r.y + r.height / 2, r.width, r.height - r.height / 2),
            Zone::Replace => r,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Kind(usize),
    Preset(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EdAct {
    /// Put the edited layout on the tab, and save it when it has a name.
    Apply,
    Cancel,
}

fn leaf(tool: Option<&str>) -> SavedNode {
    SavedNode::Pane {
        cwd: String::new(),
        tool: tool.map(String::from),
    }
}
fn sp(axis: SavedAxis, ratio: f32, a: SavedNode, b: SavedNode) -> SavedNode {
    SavedNode::Split {
        split: axis,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}

/// The built-in presets (the same shapes as `sv layout dev` …).
pub fn builtin_presets() -> Vec<Preset> {
    use SavedAxis::{SideBySide as H, Stacked as V};
    let p = |name: &str, node| Preset {
        name: name.into(),
        saved: false,
        node,
    };
    vec![
        p(
            "dev",
            sp(
                H,
                0.22,
                leaf(Some("files")),
                sp(
                    H,
                    0.62,
                    leaf(None),
                    sp(V, 0.6, leaf(Some("system")), leaf(Some("flow"))),
                ),
            ),
        ),
        p("monitor", sp(V, 0.62, leaf(Some("system")), leaf(None))),
        p("files", sp(H, 0.38, leaf(Some("files")), leaf(None))),
        p("data", sp(H, 0.5, leaf(None), leaf(Some("flow")))),
        p(
            "thermal",
            sp(
                H,
                0.6,
                leaf(None),
                sp(V, 0.5, leaf(Some("system")), leaf(Some("thermal"))),
            ),
        ),
        p("basic", leaf(None)),
    ]
}

impl Editor {
    /// Start from the tab as it is: `panes` in layout order with their kinds.
    pub fn new(
        root: &Node,
        kinds: &[(PaneId, Kind)],
        focus: PaneId,
        folder: PathBuf,
        presets: Vec<Preset>,
    ) -> Editor {
        let mut slots = Vec::new();
        let tree = Self::from_node(root, kinds, &mut slots);
        let sel = kinds.iter().position(|(id, _)| *id == focus).unwrap_or(0) as u32;
        let prefs = Prefs::load();
        Editor {
            tree,
            slots,
            sel,
            kind: 1,
            field: Field::Map,
            name: String::new(),
            make_default: false,
            for_folder: false,
            folder: folder.clone(),
            presets,
            undo: Vec::new(),
            drag: None,
            carry: None,
            map_area: std::cell::Cell::new(Rect::default()),
            hits: std::cell::RefCell::new(Vec::new()),
            message: prefs
                .for_folder(&folder)
                .map(|(_, n)| format!("this folder opens with “{n}”")),
        }
    }

    fn from_node(n: &Node, kinds: &[(PaneId, Kind)], slots: &mut Vec<Slot>) -> Node {
        match n {
            Node::Leaf(id) => {
                let kind = kinds
                    .iter()
                    .find(|(p, _)| p == id)
                    .map(|(_, k)| *k)
                    .unwrap_or(KINDS[0]);
                slots.push(Slot {
                    existing: Some(*id),
                    kind,
                });
                Node::Leaf(slots.len() as u32 - 1)
            }
            Node::Split { axis, ratio, a, b } => Node::Split {
                axis: *axis,
                ratio: *ratio,
                a: Box::new(Self::from_node(a, kinds, slots)),
                b: Box::new(Self::from_node(b, kinds, slots)),
            },
        }
    }

    fn checkpoint(&mut self) {
        self.undo
            .push((self.tree.clone(), self.slots.clone(), self.sel));
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
    }

    fn next_slot(&mut self, kind: Kind) -> u32 {
        self.slots.push(Slot {
            existing: None,
            kind,
        });
        self.slots.len() as u32 - 1
    }

    /// Replace the model with a preset, keeping running panes where a leaf of
    /// the same kind wants one (shells first, in order).
    pub fn load(&mut self, node: &SavedNode) {
        self.checkpoint();
        let mut pool: Vec<Slot> = self
            .slots
            .iter()
            .filter(|s| s.existing.is_some())
            .cloned()
            .collect();
        let mut slots = Vec::new();
        fn build(n: &SavedNode, pool: &mut Vec<Slot>, slots: &mut Vec<Slot>) -> Node {
            match n {
                SavedNode::Pane { tool, .. } => {
                    let kind = kind_of(tool.as_deref());
                    let slot = match pool.iter().position(|s| s.kind == kind) {
                        Some(i) => pool.remove(i),
                        None => Slot {
                            existing: None,
                            kind,
                        },
                    };
                    slots.push(slot);
                    Node::Leaf(slots.len() as u32 - 1)
                }
                SavedNode::Split { split, ratio, a, b } => Node::Split {
                    axis: if *split == SavedAxis::SideBySide {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    ratio: ratio.clamp(0.1, 0.9),
                    a: Box::new(build(a, pool, slots)),
                    b: Box::new(build(b, pool, slots)),
                },
            }
        }
        self.tree = build(node, &mut pool, &mut slots);
        self.slots = slots;
        self.sel = self
            .tree
            .panes()
            .into_iter()
            .find(|i| self.slots[*i as usize].kind == KINDS[0])
            .unwrap_or(0);
    }

    /// The model as it would be saved.
    pub fn saved(&self) -> SavedNode {
        fn go(n: &Node, slots: &[Slot]) -> SavedNode {
            match n {
                Node::Leaf(i) => leaf(slots[*i as usize].kind.tool),
                Node::Split { axis, ratio, a, b } => SavedNode::Split {
                    split: if *axis == Axis::Horizontal {
                        SavedAxis::SideBySide
                    } else {
                        SavedAxis::Stacked
                    },
                    ratio: (ratio * 100.0).round() / 100.0,
                    a: Box::new(go(a, slots)),
                    b: Box::new(go(b, slots)),
                },
            }
        }
        go(&self.tree, &self.slots)
    }

    /// Running panes the edited layout no longer has (closed on apply).
    pub fn dropped(&self) -> Vec<PaneId> {
        let kept: Vec<u32> = self.tree.panes();
        self.slots
            .iter()
            .enumerate()
            .filter(|(i, s)| s.existing.is_some() && !kept.contains(&(*i as u32)))
            .filter_map(|(_, s)| s.existing)
            .collect()
    }

    fn resize(&mut self, d: Dir) {
        self.checkpoint();
        if !self.tree.resize(self.sel, d, 0.05) {
            self.undo.pop();
        }
        snap(&mut self.tree);
    }

    pub fn key(&mut self, key: &[u8]) -> Option<EdAct> {
        self.message = None;
        if self.field == Field::Name {
            match key {
                b"\r" | b"\t" => self.field = Field::Map,
                b"\x1b" => {
                    self.field = Field::Map;
                    self.name.clear();
                }
                [0x7f] | [0x08] => {
                    self.name.pop();
                }
                k => {
                    if let Ok(s) = std::str::from_utf8(k) {
                        if s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                            && self.name.len() < 40
                        {
                            self.name.push_str(s);
                        }
                    }
                }
            }
            return None;
        }
        let rects = self.rects(Rect::new(0, 0, 200, 100));
        let mut go = |d: Dir| {
            if let Some(n) = crate::workspace::neighbor(&rects, self.sel, d) {
                self.sel = n;
            }
        };
        match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => go(Dir::Up),
            b"\x1b[B" | b"\x1bOB" | b"j" => go(Dir::Down),
            b"\x1b[C" | b"\x1bOC" | b"l" => go(Dir::Right),
            b"\x1b[D" | b"\x1bOD" | b"h" => go(Dir::Left),
            b"\x1b[1;2A" | b"K" => self.resize(Dir::Up),
            b"\x1b[1;2B" | b"J" => self.resize(Dir::Down),
            b"\x1b[1;2C" | b"L" | b">" => self.resize(Dir::Right),
            b"\x1b[1;2D" | b"H" | b"<" => self.resize(Dir::Left),
            b"\t" => {
                let panes = self.tree.panes();
                let i = panes.iter().position(|p| *p == self.sel).unwrap_or(0);
                self.sel = panes[(i + 1) % panes.len()];
            }
            b"[" => self.kind = (self.kind + KINDS.len() - 1) % KINDS.len(),
            b"]" => self.kind = (self.kind + 1) % KINDS.len(),
            b"\\" | b"|" | b"v" | b"-" | b"_" | b"s" => {
                let axis = if matches!(key, b"-" | b"_" | b"s") {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                };
                self.checkpoint();
                let new = self.next_slot(KINDS[self.kind]);
                self.tree.split(self.sel, axis, new);
                self.sel = new;
            }
            b"x" | b"\x1b[3~" => {
                if self.tree.panes().len() <= 1 {
                    self.message = Some("a tab keeps at least one pane".into());
                } else {
                    self.checkpoint();
                    let panes = self.tree.panes();
                    let i = panes.iter().position(|p| *p == self.sel).unwrap_or(0);
                    let tree = std::mem::replace(&mut self.tree, Node::Leaf(0));
                    self.tree = tree.remove(self.sel).unwrap_or(Node::Leaf(panes[0]));
                    let left = self.tree.panes();
                    self.sel = left[i.min(left.len() - 1)];
                }
            }
            b"c" => {
                // change the selected pane's kind to the palette's
                self.checkpoint();
                let k = KINDS[self.kind];
                let s = &mut self.slots[self.sel as usize];
                if s.kind != k {
                    let new = self.next_slot(k);
                    let sel = self.sel;
                    replace_leaf(&mut self.tree, sel, new);
                    self.sel = new;
                }
            }
            b"u" => match self.undo.pop() {
                Some((t, s, sel)) => {
                    self.tree = t;
                    self.slots = s;
                    self.sel = sel;
                }
                None => self.message = Some("nothing to undo".into()),
            },
            [d @ b'1'..=b'9'] => {
                if let Some(p) = self.presets.get((d - b'1') as usize).cloned() {
                    self.load(&p.node);
                    if p.saved {
                        self.name = p.name.clone();
                    }
                    self.message = Some(format!("“{}” loaded — ⏎ to use it", p.name));
                }
            }
            b"n" => self.field = Field::Name,
            b"d" => self.make_default = !self.make_default,
            b"f" => self.for_folder = !self.for_folder,
            b"\r" => {
                if (self.make_default || self.for_folder) && self.name.is_empty() {
                    self.message = Some("give it a name first (n)".into());
                    self.field = Field::Name;
                    return None;
                }
                return Some(EdAct::Apply);
            }
            b"\x1b" | b"q" | b"\x1be" => return Some(EdAct::Cancel),
            _ => {}
        }
        None
    }

    /// Mouse in the map: click selects a pane, drag moves a divider.
    pub fn mouse(&mut self, x: u16, y: u16, press: bool, motion: bool, release: bool) -> bool {
        let area = self.map_area.get();
        // board 4: a palette item carried onto a pane's drop zone
        if let Some((k, _)) = self.carry {
            let target = self.zone_at(x, y);
            if release {
                self.carry = None;
                if let Some((id, zone)) = target {
                    self.drop_kind(k, id, zone);
                }
            } else {
                self.carry = Some((k, target));
            }
            return true;
        }
        if let Some(path) = self.drag.clone() {
            if release {
                self.drag = None;
                snap(&mut self.tree);
            } else if motion {
                self.tree.drag_divider(area, &path, x, y);
            }
            return true;
        }
        if !press {
            return false;
        }
        let hit = self
            .hits
            .borrow()
            .iter()
            .find(|(r, _)| r.contains((x, y).into()))
            .map(|(_, h)| *h);
        match hit {
            Some(Hit::Kind(i)) => {
                self.kind = i;
                // holding the button down carries it to the map
                self.carry = Some((i, None));
                return true;
            }
            Some(Hit::Preset(i)) => {
                let key = [b'1' + i as u8];
                self.key(&key);
                return true;
            }
            None => {}
        }
        if !area.contains((x, y).into()) {
            return false;
        }
        if let Some(path) = self.tree.divider_at(area, x, y) {
            self.checkpoint();
            self.drag = Some(path);
            return true;
        }
        let rects = self.rects(area);
        if let Some((id, _)) = rects.iter().find(|(_, r)| r.contains((x, y).into())) {
            self.sel = *id;
            return true;
        }
        false
    }

    /// The pane under (x, y) on the map and which of its zones: its bottom
    /// third splits below, its right third splits right, the rest replaces it.
    fn zone_at(&self, x: u16, y: u16) -> Option<(u32, Zone)> {
        let area = self.map_area.get();
        if !area.contains((x, y).into()) {
            return None;
        }
        let rects = self.rects(area);
        let (id, r) = rects.iter().find(|(_, r)| r.contains((x, y).into()))?;
        let fx = (x - r.x) as f32 / r.width.max(1) as f32;
        let fy = (y - r.y) as f32 / r.height.max(1) as f32;
        let zone = if fy >= 0.66 {
            Zone::Below
        } else if fx >= 0.66 {
            Zone::Right
        } else {
            Zone::Replace
        };
        Some((*id, zone))
    }

    /// Put a palette kind on a pane: a split right or below it, or in its place.
    pub fn drop_kind(&mut self, k: usize, on: u32, zone: Zone) {
        let kind = KINDS[k];
        self.checkpoint();
        match zone {
            Zone::Replace => {
                if self.slots[on as usize].kind == kind {
                    self.message = Some(format!("it already is {}", kind.name));
                    return;
                }
                let new = self.next_slot(kind);
                replace_leaf(&mut self.tree, on, new);
                self.sel = new;
            }
            Zone::Right | Zone::Below => {
                let axis = if zone == Zone::Right {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let new = self.next_slot(kind);
                self.tree.split(on, axis, new);
                self.sel = new;
            }
        }
        self.kind = k;
        self.message = Some(format!(
            "{} added — ⏎ to use the layout, u undoes",
            kind.name
        ));
    }

    fn rects(&self, area: Rect) -> Vec<(u32, Rect)> {
        let mut v = Vec::new();
        self.tree.layout(area, &mut v);
        v
    }

    // -------------------------------------------------------------- drawing

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 40 || area.height < 16 {
            put(
                buf,
                area.x,
                area.y,
                area.width,
                "make the window bigger to edit the layout",
                Style::default().fg(DIM),
            );
            return;
        }
        let w = area.width;
        put(
            buf,
            area.x + 1,
            area.y,
            w,
            "Layouts",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        );
        put(
            buf,
            area.x + 9,
            area.y,
            w.saturating_sub(10),
            "· 1–9 start from one · F7 / alt+l cycles arrangements · saved in ~/.config/shellvisor/layouts/",
            Style::default().fg(FAINT),
        );
        let mut hits = Vec::new();
        // preset strip
        let n = self.presets.len().min(9) as u16;
        let tw = ((w - 2) / n.max(1)).clamp(10, 20);
        for (i, p) in self.presets.iter().take(9).enumerate() {
            let x = area.x + 1 + i as u16 * tw;
            if x + tw > area.right() {
                break;
            }
            let r = Rect::new(x, area.y + 2, tw - 2, 4);
            hits.push((Rect::new(x, area.y + 2, tw - 2, 6), Hit::Preset(i)));
            let frame = if p.saved { CYAN } else { FAINT };
            draw_box(buf, r, Style::default().fg(frame));
            thumbnail(
                buf,
                Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2),
                &p.node,
            );
            let label = format!("{} {}", i + 1, p.name);
            put(
                buf,
                x,
                area.y + 6,
                tw - 1,
                &label,
                Style::default().fg(if p.saved { CYAN } else { TEXT }),
            );
            put(
                buf,
                x,
                area.y + 7,
                tw - 1,
                if p.saved { "★ yours" } else { "built in" },
                Style::default().fg(FAINT),
            );
        }
        // three columns
        let top = area.y + 9;
        let h = area.bottom().saturating_sub(top + 2);
        for x in area.x..area.right() {
            put(
                buf,
                x,
                top - 1,
                1,
                "─",
                Style::default().fg(Color::Indexed(238)),
            );
        }
        let side = if w >= 110 { 36 } else { 0 };
        let pal_w = 22u16;
        let map = Rect::new(
            area.x + pal_w + 2,
            top,
            w.saturating_sub(pal_w + side + 4),
            h,
        );
        // palette
        put(
            buf,
            area.x + 1,
            top,
            pal_w,
            "Edit layout",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        );
        put(
            buf,
            area.x + 13,
            top,
            8,
            "alt+e",
            Style::default().fg(FAINT),
        );
        put(
            buf,
            area.x + 1,
            top + 1,
            pal_w,
            "add with \\ or -   [ ] pick",
            Style::default().fg(FAINT),
        );
        let used: Vec<Kind> = self
            .tree
            .panes()
            .iter()
            .map(|i| self.slots[*i as usize].kind)
            .collect();
        // two lines per kind when there is room, else one
        let roomy = h > 3 + KINDS.len() as u16 * 2;
        for (i, k) in KINDS.iter().enumerate() {
            let y = top + 3 + i as u16 * if roomy { 2 } else { 1 };
            if y >= top + h {
                break;
            }
            let on = i == self.kind;
            let st = if on {
                Style::default()
                    .fg(INK)
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            hits.push((
                Rect::new(area.x + 1, y, pal_w - 1, if roomy { 2 } else { 1 }),
                Hit::Kind(i),
            ));
            fill(
                buf,
                Rect::new(area.x + 1, y, pal_w - 1, 1),
                if on {
                    Style::default().bg(AMBER)
                } else {
                    Style::default()
                },
            );
            put(buf, area.x + 2, y, pal_w - 2, k.name, st);
            if used.contains(k) {
                let tag = "●";
                put(
                    buf,
                    area.x + pal_w - 2,
                    y,
                    1,
                    tag,
                    if on { st } else { Style::default().fg(GREEN) },
                );
            }
            if roomy {
                let carried = self.carry.is_some_and(|(c, _)| c == i);
                let note = if carried {
                    "dragging"
                } else if used.contains(k) {
                    "in use"
                } else {
                    k.note
                };
                put(
                    buf,
                    area.x + 2,
                    y + 1,
                    pal_w - 2,
                    note,
                    Style::default().fg(FAINT),
                );
            }
        }
        *self.hits.borrow_mut() = hits;
        // map
        self.render_map(map, buf);
        // save form + yaml
        if side > 0 {
            let sx = area.right() - side;
            let mut y = top;
            put(buf, sx, y, side, "save as", Style::default().fg(DIM));
            y += 1;
            let editing = self.field == Field::Name;
            let name_st = if editing {
                Style::default().fg(TEXT).bg(SEL_BG)
            } else {
                Style::default().fg(TEXT)
            };
            fill(
                buf,
                Rect::new(sx, y, side - 1, 1),
                Style::default().bg(SEL_BG),
            );
            let shown = if self.name.is_empty() && !editing {
                "(not saved — n to name it)".to_string()
            } else {
                format!("{}{}", self.name, if editing { "▏" } else { "" })
            };
            put(
                buf,
                sx + 1,
                y,
                side - 2,
                &shown,
                if self.name.is_empty() && !editing {
                    Style::default().fg(FAINT).bg(SEL_BG)
                } else {
                    name_st
                },
            );
            y += 2;
            let tick = |b: bool| if b { "▣" } else { "▢" };
            put(
                buf,
                sx,
                y,
                side,
                &format!("{} make this my default layout  d", tick(self.make_default)),
                Style::default().fg(if self.make_default { AMBER } else { DIM }),
            );
            y += 1;
            let folder = crate::ui::short_path(&self.folder.to_string_lossy());
            let line = format!(
                "{} open it when the folder is {folder}  f",
                tick(self.for_folder)
            );
            for l in wrap(&line, side as usize - 1).into_iter().take(2) {
                put(
                    buf,
                    sx,
                    y,
                    side,
                    &l,
                    Style::default().fg(if self.for_folder { AMBER } else { DIM }),
                );
                y += 1;
            }
            y += 1;
            let label = if self.name.is_empty() {
                " Use it ⏎ "
            } else {
                " Save & use ⏎ "
            };
            put(
                buf,
                sx,
                y,
                side,
                label,
                Style::default()
                    .fg(INK)
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD),
            );
            put(
                buf,
                sx + label.chars().count() as u16 + 1,
                y,
                side,
                " Cancel esc ",
                Style::default().fg(TEXT).bg(SEL_BG),
            );
            y += 2;
            put(
                buf,
                sx,
                y,
                side,
                "what gets saved",
                Style::default().fg(DIM),
            );
            y += 1;
            let yaml = serde_yaml_ng::to_string(&self.saved()).unwrap_or_default();
            for l in yaml.lines() {
                if y >= top + h {
                    break;
                }
                let (k, rest) = l.split_once(':').unwrap_or((l, ""));
                put(buf, sx, y, side, k, Style::default().fg(BLUE));
                let kw = k.chars().count() as u16;
                if !rest.is_empty() {
                    put(
                        buf,
                        sx + kw,
                        y,
                        side.saturating_sub(kw),
                        &format!(":{rest}"),
                        Style::default().fg(if rest.trim().parse::<f32>().is_ok() {
                            AMBER
                        } else {
                            GREEN
                        }),
                    );
                }
                y += 1;
            }
        }
        // narrow screens: the save form as one line
        if side == 0 {
            let tick = |b: bool| if b { "▣" } else { "▢" };
            let name = if self.field == Field::Name {
                format!("{}▏", self.name)
            } else if self.name.is_empty() {
                "— (n)".to_string()
            } else {
                self.name.clone()
            };
            let line = format!(
                "save as: {name} · {} default (d) · {} this folder (f) · ⏎ {} · esc cancel",
                tick(self.make_default),
                tick(self.for_folder),
                if self.name.is_empty() {
                    "use it"
                } else {
                    "save & use"
                }
            );
            put(
                buf,
                area.x + 1,
                area.bottom() - 2,
                w - 2,
                &line,
                Style::default().fg(if self.field == Field::Name {
                    AMBER
                } else {
                    DIM
                }),
            );
        }
        // footer
        let y = area.bottom() - 1;
        let hint = match (&self.message, self.field) {
            (Some(m), _) => m.clone(),
            (None, Field::Name) => "type a name · ⏎ done · esc clear".into(),
            (None, Field::Map) => "←↑↓→ pane · \\ split right · - split below · c change kind · x remove · < > J K resize (5%) · drag borders · drag a palette item onto a pane · u undo · ⏎ use · esc cancel".into(),
        };
        let st = if self.message.is_some() {
            Style::default().fg(AMBER)
        } else {
            Style::default().fg(FAINT)
        };
        put(buf, area.x + 1, y, w - 2, &hint, st);
    }

    fn render_map(&self, map: Rect, buf: &mut Buffer) {
        let dropped = self.dropped().len();
        put(
            buf,
            map.x,
            map.y,
            map.width,
            "EDITING",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        let note = if dropped > 0 {
            format!(
                "{dropped} running pane{} will close",
                if dropped == 1 { "" } else { "s" }
            )
        } else {
            "live panes keep running while you edit".to_string()
        };
        let nw = note.chars().count() as u16;
        put(
            buf,
            map.right().saturating_sub(nw),
            map.y,
            nw,
            &note,
            Style::default().fg(if dropped > 0 { RED } else { FAINT }),
        );
        let inner = Rect::new(map.x, map.y + 1, map.width, map.height.saturating_sub(1));
        self.map_area.set(inner);
        let rects = self.rects(inner);
        // each pane's share of the split it is in
        fn shares(n: &Node, out: &mut Vec<(u32, (f32, Axis))>) {
            if let Node::Split { axis, ratio, a, b } = n {
                if let Node::Leaf(i) = a.as_ref() {
                    out.push((*i, (*ratio, *axis)));
                }
                if let Node::Leaf(i) = b.as_ref() {
                    out.push((*i, (1.0 - *ratio, *axis)));
                }
                shares(a, out);
                shares(b, out);
            }
        }
        let mut shares_v = Vec::new();
        shares(&self.tree, &mut shares_v);
        let shares = shares_v;
        for (id, r) in &rects {
            let slot = &self.slots[*id as usize];
            let on = *id == self.sel;
            let col = if on {
                AMBER
            } else if slot.existing.is_none() {
                CYAN
            } else {
                Color::Indexed(240)
            };
            if on {
                fill(
                    buf,
                    Rect::new(
                        r.x + 1,
                        r.y + 1,
                        r.width.saturating_sub(2),
                        r.height.saturating_sub(2),
                    ),
                    Style::default().bg(Color::Indexed(235)),
                );
            }
            draw_box(buf, *r, Style::default().fg(col));
            let inn = Rect::new(
                r.x + 1,
                r.y + 1,
                r.width.saturating_sub(2),
                r.height.saturating_sub(2),
            );
            put(
                buf,
                inn.x,
                inn.y,
                inn.width,
                slot.kind.name,
                Style::default()
                    .fg(if on { AMBER } else { TEXT })
                    .add_modifier(Modifier::BOLD),
            );
            let state = if slot.existing.is_some() {
                "running"
            } else {
                "new"
            };
            if inn.height > 1 {
                put(
                    buf,
                    inn.x,
                    inn.y + 1,
                    inn.width,
                    state,
                    Style::default().fg(if slot.existing.is_some() { FAINT } else { CYAN }),
                );
            }
            if on && inn.height > 3 {
                put(
                    buf,
                    inn.x,
                    inn.y + inn.height / 2,
                    inn.width,
                    "selected",
                    Style::default().fg(DIM),
                );
            }
            if let (Some((share, axis)), true) = (
                shares.iter().find(|(i, _)| i == id).map(|(_, s)| *s),
                inn.height > 2,
            ) {
                let pct = format!(
                    "{:.0}% {}",
                    share * 100.0,
                    if axis == Axis::Horizontal {
                        "wide"
                    } else {
                        "tall"
                    }
                );
                put(
                    buf,
                    inn.x,
                    inn.bottom() - 1,
                    inn.width,
                    &pct,
                    Style::default().fg(FAINT),
                );
            }
        }
        draw_dividers(buf, &self.tree, inner);
        // the drop zone under a carried palette item (board 4)
        if let Some((k, Some((id, zone)))) = self.carry {
            if let Some((_, r)) = rects.iter().find(|(i, _)| *i == id) {
                let z = zone.area(*r);
                let st = Style::default().fg(CYAN);
                fill(
                    buf,
                    Rect::new(
                        z.x + 1,
                        z.y + 1,
                        z.width.saturating_sub(2),
                        z.height.saturating_sub(2),
                    ),
                    Style::default().bg(Color::Indexed(23)),
                );
                // a dashed outline
                for x in z.x..z.right() {
                    put(buf, x, z.y, 1, "╌", st);
                    put(buf, x, z.bottom().saturating_sub(1), 1, "╌", st);
                }
                for y in z.y..z.bottom() {
                    put(buf, z.x, y, 1, "╎", st);
                    put(buf, z.right().saturating_sub(1), y, 1, "╎", st);
                }
                let what = match zone {
                    Zone::Right => "split right",
                    Zone::Below => "split below",
                    Zone::Replace => "in its place",
                };
                let t = format!("drop “{}” here · {what}", KINDS[k].name);
                let tw = (t.chars().count() as u16).min(z.width.saturating_sub(2));
                put(
                    buf,
                    z.x + (z.width.saturating_sub(tw)) / 2,
                    z.y + z.height / 2,
                    tw,
                    &t,
                    Style::default()
                        .fg(CYAN)
                        .bg(Color::Indexed(23))
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

fn snap(n: &mut Node) {
    if let Node::Split { ratio, a, b, .. } = n {
        *ratio = ((*ratio * 20.0).round() / 20.0).clamp(0.1, 0.9);
        snap(a);
        snap(b);
    }
}

fn replace_leaf(n: &mut Node, from: u32, to: u32) {
    match n {
        Node::Leaf(id) if *id == from => *id = to,
        Node::Leaf(_) => {}
        Node::Split { a, b, .. } => {
            replace_leaf(a, from, to);
            replace_leaf(b, from, to);
        }
    }
}

fn draw_dividers(buf: &mut Buffer, n: &Node, area: Rect) {
    if let Node::Split { axis, ratio, a, b } = n {
        let (ra, rb) = split_rect(area, *axis, *ratio);
        let (x, y, ch) = match axis {
            Axis::Horizontal => (rb.x, area.y + area.height / 2, "⇔"),
            Axis::Vertical => (area.x + area.width / 2, rb.y, "⇕"),
        };
        put(buf, x, y, 1, ch, Style::default().fg(AMBER));
        draw_dividers(buf, a, ra);
        draw_dividers(buf, b, rb);
    }
}

/// A small picture of a layout: a letter per pane.
fn thumbnail(buf: &mut Buffer, area: Rect, n: &SavedNode) {
    match n {
        SavedNode::Pane { tool, .. } => {
            let k = kind_of(tool.as_deref());
            let (st, label) = if k.tool.is_none() {
                (Style::default().bg(Color::Indexed(94)).fg(AMBER), "shell")
            } else {
                (
                    Style::default().bg(Color::Indexed(236)).fg(DIM),
                    k.tool.unwrap_or(""),
                )
            };
            fill(
                buf,
                Rect::new(
                    area.x,
                    area.y,
                    area.width.saturating_sub(1).max(1),
                    area.height,
                ),
                st,
            );
            put(buf, area.x, area.y, area.width.saturating_sub(1), label, st);
        }
        SavedNode::Split { split, ratio, a, b } => {
            let axis = if *split == SavedAxis::SideBySide {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
            let (ra, rb) = split_rect(area, axis, *ratio);
            thumbnail(buf, ra, a);
            thumbnail(buf, rb, b);
        }
    }
}

fn draw_box(buf: &mut Buffer, r: Rect, st: Style) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    for x in r.x + 1..r.right() - 1 {
        put(buf, x, r.y, 1, "─", st);
        put(buf, x, r.bottom() - 1, 1, "─", st);
    }
    for y in r.y + 1..r.bottom() - 1 {
        put(buf, r.x, y, 1, "│", st);
        put(buf, r.right() - 1, y, 1, "│", st);
    }
    put(buf, r.x, r.y, 1, "╭", st);
    put(buf, r.right() - 1, r.y, 1, "╮", st);
    put(buf, r.x, r.bottom() - 1, 1, "╰", st);
    put(buf, r.right() - 1, r.bottom() - 1, 1, "╯", st);
}

fn put(buf: &mut Buffer, x: u16, y: u16, w: u16, s: &str, st: Style) {
    if w == 0 || y >= buf.area.bottom() || x >= buf.area.right() {
        return;
    }
    let w = w.min(buf.area.right() - x);
    buf.set_stringn(x, y, s, w as usize, st);
}

fn fill(buf: &mut Buffer, r: Rect, st: Style) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol(" ").set_style(st);
        }
    }
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
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

    fn sample() -> Editor {
        // files | shell, both running
        let root = Node::Split {
            axis: Axis::Horizontal,
            ratio: 0.3,
            a: Box::new(Node::Leaf(7)),
            b: Box::new(Node::Leaf(9)),
        };
        let kinds = [(7, kind_of(Some("files"))), (9, KINDS[0])];
        Editor::new(&root, &kinds, 9, PathBuf::from("/work"), builtin_presets())
    }

    #[test]
    fn edit_undo_and_what_gets_saved() {
        let mut e = sample();
        assert_eq!(
            e.slots[e.sel as usize].existing,
            Some(9),
            "starts on the focused pane"
        );
        e.key(b"]"); // Git changes
        e.key(b"]"); // System stats
        e.key(b"-"); // split the shell, system below
        assert_eq!(e.slots[e.sel as usize].kind.name, "System stats");
        assert_eq!(e.slots[e.sel as usize].existing, None);
        e.key(b"<");
        let yaml = serde_yaml_ng::to_string(&e.saved()).unwrap();
        assert!(
            yaml.contains("tool: files")
                && yaml.contains("tool: system")
                && yaml.contains("stacked"),
            "{yaml}"
        );
        e.key(b"\x1b[D"); // to the files pane
        assert_eq!(e.slots[e.sel as usize].existing, Some(7));
        e.key(b"x");
        assert_eq!(e.dropped(), vec![7], "the files pane would close");
        e.key(b"u");
        assert!(e.dropped().is_empty(), "undo brings it back");
        // a preset keeps the running shell and files pane
        e.key(b"1"); // dev
        let kept: Vec<Option<PaneId>> = e
            .tree
            .panes()
            .iter()
            .map(|i| e.slots[*i as usize].existing)
            .collect();
        assert!(
            kept.contains(&Some(9)) && kept.contains(&Some(7)),
            "{kept:?}"
        );
        assert_eq!(e.tree.panes().len(), 4);
        // saving for a folder needs a name
        e.key(b"f");
        assert_eq!(e.key(b"\r"), None);
        assert_eq!(e.field, Field::Name);
        for k in ["c", "o", "d", "e"] {
            e.key(k.as_bytes());
        }
        e.key(b"\r");
        assert_eq!(e.key(b"\r"), Some(EdAct::Apply));
        assert_eq!(e.name, "code");
        assert_eq!(e.key(b"\x1b"), Some(EdAct::Cancel));
    }

    #[test]
    fn mouse_selects_and_drags() {
        let mut e = sample();
        let area = Rect::new(0, 0, 160, 40);
        let mut buf = Buffer::empty(area);
        e.render(area, &mut buf);
        let text: String = (0..40)
            .map(|y| {
                (0..160)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        for want in [
            "Layouts",
            "1 dev",
            "Edit layout",
            "Thermals",
            "EDITING",
            "save as",
            "what gets saved",
            "side_by_side",
        ] {
            assert!(text.contains(want), "{want} missing:\n{text}");
        }
        let map = e.map_area.get();
        let rects = e.rects(map);
        let files = rects
            .iter()
            .find(|(i, _)| e.slots[*i as usize].existing == Some(7))
            .unwrap()
            .1;
        assert!(e.mouse(files.x + 2, files.y + 2, true, false, false));
        assert_eq!(e.slots[e.sel as usize].existing, Some(7));
        // drag the divider to the middle
        let dx = files.right() - 1;
        assert!(e.mouse(dx, files.y + 3, true, false, false));
        e.mouse(map.x + map.width / 2, files.y + 3, false, true, false);
        e.mouse(0, 0, false, false, true);
        let Node::Split { ratio, .. } = &e.tree else {
            panic!()
        };
        assert!((ratio - 0.5).abs() < 0.051, "{ratio}");
        // clicks on the palette and on a preset thumbnail
        let hits = e.hits.borrow().clone();
        let (thermal, _) = *hits.iter().find(|(_, h)| *h == Hit::Kind(4)).unwrap();
        assert!(e.mouse(thermal.x + 1, thermal.y, true, false, false));
        assert_eq!(KINDS[e.kind].name, "Thermals");
        // carried onto the bottom of the files pane: "drop “Thermals” here", then a split below
        let panes = e.tree.panes().len();
        let rects = e.rects(map);
        let files = rects
            .iter()
            .find(|(i, _)| e.slots[*i as usize].existing == Some(7))
            .unwrap()
            .1;
        let (dx, dy) = (files.x + 2, files.bottom() - 2);
        assert!(e.mouse(dx, dy, false, true, false));
        let mut buf = Buffer::empty(area);
        e.render(area, &mut buf);
        let text: String = (0..40)
            .map(|y| {
                (0..160)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(
            text.contains("drop “Thermals” here") && text.contains("dragging"),
            "{text}"
        );
        assert!(e.mouse(dx, dy, false, false, true));
        assert_eq!(e.tree.panes().len(), panes + 1);
        assert_eq!(e.slots[e.sel as usize].kind.name, "Thermals");
        // a click on the palette (press and release in place) only picks it
        let hits = e.hits.borrow().clone();
        let (shell, _) = *hits.iter().find(|(_, h)| *h == Hit::Kind(0)).unwrap();
        e.mouse(shell.x + 1, shell.y, true, false, false);
        e.mouse(shell.x + 1, shell.y, false, false, true);
        assert_eq!(e.tree.panes().len(), panes + 1);
        assert_eq!(KINDS[e.kind].name, "Shell");
        let (monitor, _) = *hits.iter().find(|(_, h)| *h == Hit::Preset(1)).unwrap();
        e.mouse(monitor.x + 1, monitor.y + 1, true, false, false);
        assert_eq!(e.tree.panes().len(), 2);
        assert!(e.message.as_deref().unwrap_or("").contains("monitor"));
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

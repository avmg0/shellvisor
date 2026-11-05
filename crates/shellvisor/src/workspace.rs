//! Tabs and splits.
//!
//! A tab holds a binary tree of panes: each split divides its area side by
//! side or stacked, at a ratio the user can adjust.

use crate::keys::Dir;
use ratatui::layout::Rect;

pub type PaneId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Children side by side (a │ b).
    Horizontal,
    /// Children stacked (a over b).
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// Share of the area given to `a`, 0.1..=0.9.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

const MIN_RATIO: f32 = 0.1;
const MAX_RATIO: f32 = 0.9;

impl Node {
    /// Pane rectangles for `area`, in reading order.
    pub fn layout(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, area)),
            Node::Split { axis, ratio, a, b } => {
                let (ra, rb) = split_rect(area, *axis, *ratio);
                a.layout(ra, out);
                b.layout(rb, out);
            }
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        self.collect(&mut v);
        v
    }

    fn collect(&self, v: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => v.push(*id),
            Node::Split { a, b, .. } => {
                a.collect(v);
                b.collect(v);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(x) => *x == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// Replace leaf `target` with a split of (target, new). Returns false if
    /// `target` isn't in this tree.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        self.split_at(target, axis, new, 0.5)
    }

    /// Like `split`, with `target` taking `ratio` of the space.
    pub fn split_at(&mut self, target: PaneId, axis: Axis, new: PaneId, ratio: f32) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    axis,
                    ratio,
                    a: Box::new(Node::Leaf(target)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.split_at(target, axis, new, ratio) || b.split_at(target, axis, new, ratio)
            }
        }
    }

    /// Remove `target`; its sibling takes its place. Returns None if the tree
    /// becomes empty.
    pub fn remove(self, target: PaneId) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == target => None,
            leaf @ Node::Leaf(_) => Some(leaf),
            Node::Split { axis, ratio, a, b } => match (a.remove(target), b.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    axis,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Grow/shrink `target` towards `dir` by `step` of the nearest enclosing
    /// split along that axis. Returns true if something changed.
    pub fn resize(&mut self, target: PaneId, dir: Dir, step: f32) -> bool {
        let want = match dir {
            Dir::Left | Dir::Right => Axis::Horizontal,
            Dir::Up | Dir::Down => Axis::Vertical,
        };
        self.resize_inner(target, want, dir, step)
    }

    /// Give `target` this share (0–1) of its nearest enclosing split (of
    /// `axis`, if given). Returns true if something changed.
    pub fn set_share(&mut self, target: PaneId, share: f32, axis: Option<Axis>) -> bool {
        let Node::Split {
            axis: ax,
            ratio,
            a,
            b,
        } = self
        else {
            return false;
        };
        let in_a = a.contains(target);
        if !in_a && !b.contains(target) {
            return false;
        }
        let deeper = if in_a {
            a.set_share(target, share, axis)
        } else {
            b.set_share(target, share, axis)
        };
        if deeper || axis.is_some_and(|w| w != *ax) {
            return deeper;
        }
        let want = if in_a { share } else { 1.0 - share };
        let new = want.clamp(MIN_RATIO, MAX_RATIO);
        let changed = (new - *ratio).abs() > f32::EPSILON;
        *ratio = new;
        changed
    }

    fn resize_inner(&mut self, target: PaneId, want: Axis, dir: Dir, step: f32) -> bool {
        let Node::Split { axis, ratio, a, b } = self else {
            return false;
        };
        let in_a = a.contains(target);
        if !in_a && !b.contains(target) {
            return false;
        }
        // Prefer the deepest matching split.
        let deeper = if in_a {
            a.resize_inner(target, want, dir, step)
        } else {
            b.resize_inner(target, want, dir, step)
        };
        if deeper || *axis != want {
            return deeper;
        }
        // Moving the divider right/down grows `a`.
        let delta = match dir {
            Dir::Right | Dir::Down => step,
            Dir::Left | Dir::Up => -step,
        };
        let new = (*ratio + delta).clamp(MIN_RATIO, MAX_RATIO);
        let changed = (new - *ratio).abs() > f32::EPSILON;
        *ratio = new;
        changed
    }
}

impl Node {
    /// The split whose divider is under (x, y), as a path of a/b turns
    /// (false = a, true = b). Pane frames meet at the divider, so either of
    /// the two touching border cells grabs it.
    pub fn divider_at(&self, area: Rect, x: u16, y: u16) -> Option<Vec<bool>> {
        let Node::Split { axis, ratio, a, b } = self else {
            return None;
        };
        if !area.contains((x, y).into()) {
            return None;
        }
        let (ra, rb) = split_rect(area, *axis, *ratio);
        let on = match axis {
            Axis::Horizontal => x + 1 == ra.right() || x == rb.x,
            Axis::Vertical => y + 1 == ra.bottom() || y == rb.y,
        };
        // a deeper divider that crosses this cell wins only off our line
        if on {
            return Some(Vec::new());
        }
        let (side, child, r) = if ra.contains((x, y).into()) {
            (false, a, ra)
        } else {
            (true, b, rb)
        };
        let mut path = child.divider_at(r, x, y)?;
        path.insert(0, side);
        Some(path)
    }

    /// Move the divider at `path` to (x, y). Returns true if it moved.
    pub fn drag_divider(&mut self, area: Rect, path: &[bool], x: u16, y: u16) -> bool {
        let Node::Split { axis, ratio, a, b } = self else {
            return false;
        };
        if let Some((first, rest)) = path.split_first() {
            let (ra, rb) = split_rect(area, *axis, *ratio);
            return if *first {
                b.drag_divider(rb, rest, x, y)
            } else {
                a.drag_divider(ra, rest, x, y)
            };
        }
        let new = match axis {
            Axis::Horizontal if area.width > 0 => {
                (x.saturating_sub(area.x) + 1) as f32 / area.width as f32
            }
            Axis::Vertical if area.height > 0 => {
                (y.saturating_sub(area.y) + 1) as f32 / area.height as f32
            }
            _ => return false,
        }
        .clamp(MIN_RATIO, MAX_RATIO);
        let changed = (new - *ratio).abs() > 0.001;
        *ratio = new;
        changed
    }
}

pub fn split_rect(area: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => {
            let wa = ((area.width as f32 * ratio).round() as u16)
                .clamp(1, area.width.saturating_sub(1).max(1));
            let a = Rect { width: wa, ..area };
            let b = Rect {
                x: area.x + wa,
                width: area.width.saturating_sub(wa),
                ..area
            };
            (a, b)
        }
        Axis::Vertical => {
            let ha = ((area.height as f32 * ratio).round() as u16)
                .clamp(1, area.height.saturating_sub(1).max(1));
            let a = Rect { height: ha, ..area };
            let b = Rect {
                y: area.y + ha,
                height: area.height.saturating_sub(ha),
                ..area
            };
            (a, b)
        }
    }
}

/// The pane next to `from` in direction `dir`: the closest one whose edge
/// touches and which overlaps most along the other axis.
pub fn neighbor(rects: &[(PaneId, Rect)], from: PaneId, dir: Dir) -> Option<PaneId> {
    let (_, r) = *rects.iter().find(|(id, _)| *id == from)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
    rects
        .iter()
        .filter(|(id, _)| *id != from)
        .filter_map(|(id, o)| {
            let (gap, ov) = match dir {
                Dir::Right if o.x >= r.right() => {
                    (o.x - r.right(), overlap(r.y, r.bottom(), o.y, o.bottom()))
                }
                Dir::Left if o.right() <= r.x => {
                    (r.x - o.right(), overlap(r.y, r.bottom(), o.y, o.bottom()))
                }
                Dir::Down if o.y >= r.bottom() => {
                    (o.y - r.bottom(), overlap(r.x, r.right(), o.x, o.right()))
                }
                Dir::Up if o.bottom() <= r.y => {
                    (r.y - o.bottom(), overlap(r.x, r.right(), o.x, o.right()))
                }
                _ => return None,
            };
            (ov > 0).then_some((gap, std::cmp::Reverse(ov), *id))
        })
        .min()
        .map(|(_, _, id)| id)
}

/// Ways to rearrange a tab's splits (F7 / alt+l cycles through them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// Equal columns side by side.
    Columns,
    /// Equal rows stacked.
    Rows,
    /// One big pane on the left, the rest stacked on the right.
    MainSide,
    /// As square a grid as possible.
    Grid,
}

impl Arrangement {
    pub fn next(self) -> Self {
        match self {
            Arrangement::Columns => Arrangement::Rows,
            Arrangement::Rows => Arrangement::MainSide,
            Arrangement::MainSide => Arrangement::Grid,
            Arrangement::Grid => Arrangement::Columns,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Arrangement::Columns => "columns",
            Arrangement::Rows => "rows",
            Arrangement::MainSide => "main + side",
            Arrangement::Grid => "grid",
        }
    }
}

/// Equal-sized chain of nodes along one axis.
fn chain(mut nodes: Vec<Node>, axis: Axis) -> Node {
    if nodes.len() == 1 {
        return nodes.pop().unwrap();
    }
    let n = nodes.len();
    let first = nodes.remove(0);
    Node::Split {
        axis,
        ratio: 1.0 / n as f32,
        a: Box::new(first),
        b: Box::new(chain(nodes, axis)),
    }
}

/// Build a fresh tree holding `panes` (in this order) in the given arrangement.
pub fn arrange(panes: &[PaneId], how: Arrangement) -> Node {
    let leaves = |ids: &[PaneId]| ids.iter().map(|id| Node::Leaf(*id)).collect::<Vec<_>>();
    if panes.len() <= 1 {
        return Node::Leaf(panes.first().copied().unwrap_or(0));
    }
    match how {
        Arrangement::Columns => chain(leaves(panes), Axis::Horizontal),
        Arrangement::Rows => chain(leaves(panes), Axis::Vertical),
        Arrangement::MainSide => Node::Split {
            axis: Axis::Horizontal,
            ratio: 0.6,
            a: Box::new(Node::Leaf(panes[0])),
            b: Box::new(chain(leaves(&panes[1..]), Axis::Vertical)),
        },
        Arrangement::Grid => {
            let cols = (panes.len() as f64).sqrt().ceil() as usize;
            let rows = panes
                .chunks(cols)
                .map(|row| chain(leaves(row), Axis::Horizontal))
                .collect();
            chain(rows, Axis::Vertical)
        }
    }
}

/// A tab: a tree of panes, which one is focused, and whether it's zoomed.
#[derive(Debug, Clone)]
pub struct Tab {
    pub root: Node,
    pub focus: PaneId,
    pub zoomed: bool,
    /// Name set by the user (alt+r); otherwise the label is automatic.
    pub name: Option<String>,
    /// Last arrangement applied with F7, so the next press moves on.
    pub arrangement: Option<Arrangement>,
}

impl Tab {
    pub fn new(pane: PaneId) -> Self {
        Tab {
            root: Node::Leaf(pane),
            focus: pane,
            zoomed: false,
            name: None,
            arrangement: None,
        }
    }

    /// Rearrange the splits into the next arrangement; returns its name.
    pub fn next_arrangement(&mut self) -> Option<Arrangement> {
        let panes = self.root.panes();
        if panes.len() < 2 {
            return None;
        }
        let next = self
            .arrangement
            .map(Arrangement::next)
            .unwrap_or(Arrangement::Columns);
        self.root = arrange(&panes, next);
        self.arrangement = Some(next);
        self.zoomed = false;
        Some(next)
    }

    /// Visible pane rectangles (only the focused one when zoomed).
    pub fn layout(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if self.zoomed {
            return vec![(self.focus, area)];
        }
        let mut v = Vec::new();
        self.root.layout(area, &mut v);
        v
    }

    pub fn next_pane(&mut self) {
        let panes = self.root.panes();
        if let Some(i) = panes.iter().position(|p| *p == self.focus) {
            self.focus = panes[(i + 1) % panes.len()];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(n: &Node) -> Vec<(PaneId, Rect)> {
        let mut v = Vec::new();
        n.layout(Rect::new(0, 0, 100, 40), &mut v);
        v
    }

    #[test]
    fn split_and_layout() {
        let mut n = Node::Leaf(1);
        assert!(n.split(1, Axis::Horizontal, 2));
        assert!(n.split(2, Axis::Vertical, 3));
        assert!(!n.split(9, Axis::Vertical, 4));
        let r = rects(&n);
        assert_eq!(r[0], (1, Rect::new(0, 0, 50, 40)));
        assert_eq!(r[1], (2, Rect::new(50, 0, 50, 20)));
        assert_eq!(r[2], (3, Rect::new(50, 20, 50, 20)));
        assert_eq!(n.panes(), vec![1, 2, 3]);
    }

    #[test]
    fn remove_collapses_to_sibling() {
        let mut n = Node::Leaf(1);
        n.split(1, Axis::Horizontal, 2);
        n.split(2, Axis::Vertical, 3);
        let n = n.remove(2).unwrap();
        assert_eq!(n.panes(), vec![1, 3]);
        let n = n.remove(1).unwrap();
        assert_eq!(n, Node::Leaf(3));
        assert_eq!(n.remove(3), None);
    }

    #[test]
    fn neighbors() {
        let mut n = Node::Leaf(1);
        n.split(1, Axis::Horizontal, 2);
        n.split(2, Axis::Vertical, 3);
        let r = rects(&n);
        assert_eq!(neighbor(&r, 1, Dir::Right), Some(2));
        assert_eq!(neighbor(&r, 2, Dir::Down), Some(3));
        assert_eq!(neighbor(&r, 3, Dir::Up), Some(2));
        assert_eq!(neighbor(&r, 3, Dir::Left), Some(1));
        assert_eq!(neighbor(&r, 1, Dir::Left), None);
    }

    #[test]
    fn resize_moves_the_right_divider() {
        let mut n = Node::Leaf(1);
        n.split(1, Axis::Horizontal, 2);
        n.split(2, Axis::Vertical, 3);
        assert!(n.resize(3, Dir::Left, 0.1)); // outer split: 1 shrinks
        assert_eq!(rects(&n)[0].1.width, 40);
        assert!(n.resize(3, Dir::Up, 0.1)); // inner split: 2 shrinks
        assert_eq!(rects(&n)[1].1.height, 16);
        for _ in 0..20 {
            n.resize(1, Dir::Left, 0.1);
        }
        assert_eq!(rects(&n)[0].1.width, 10, "clamped at 10%");
    }

    fn widths(n: &Node) -> Vec<(u16, u16, u16, u16)> {
        rects(n)
            .into_iter()
            .map(|(_, r)| (r.x, r.y, r.width, r.height))
            .collect()
    }

    #[test]
    fn arrangements() {
        let p = [1, 2, 3, 4];
        assert_eq!(
            widths(&arrange(&p, Arrangement::Columns)),
            vec![
                (0, 0, 25, 40),
                (25, 0, 25, 40),
                (50, 0, 25, 40),
                (75, 0, 25, 40)
            ]
        );
        assert_eq!(
            widths(&arrange(&p, Arrangement::Rows)),
            vec![
                (0, 0, 100, 10),
                (0, 10, 100, 10),
                (0, 20, 100, 10),
                (0, 30, 100, 10)
            ]
        );
        let ms = widths(&arrange(&p, Arrangement::MainSide));
        assert_eq!(ms[0], (0, 0, 60, 40));
        assert!(ms[1..].iter().all(|r| r.0 == 60 && r.2 == 40));
        assert_eq!(
            widths(&arrange(&p, Arrangement::Grid)),
            vec![
                (0, 0, 50, 20),
                (50, 0, 50, 20),
                (0, 20, 50, 20),
                (50, 20, 50, 20)
            ]
        );
        // 3 panes in a grid: 2 on top, 1 full-width below
        let g3 = widths(&arrange(&[1, 2, 3], Arrangement::Grid));
        assert_eq!(g3[2], (0, 20, 100, 20));
        assert_eq!(arrange(&[7], Arrangement::Grid), Node::Leaf(7));
        assert_eq!(arrange(&p, Arrangement::Grid).panes(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn cycling_arrangements_keeps_panes() {
        let mut t = Tab::new(1);
        assert_eq!(t.next_arrangement(), None, "one pane: nothing to arrange");
        t.root.split(1, Axis::Horizontal, 2);
        t.root.split(2, Axis::Vertical, 3);
        let seen: Vec<_> = (0..5).map(|_| t.next_arrangement().unwrap()).collect();
        assert_eq!(
            seen,
            [
                Arrangement::Columns,
                Arrangement::Rows,
                Arrangement::MainSide,
                Arrangement::Grid,
                Arrangement::Columns
            ]
        );
        assert_eq!(t.root.panes(), vec![1, 2, 3]);
    }

    #[test]
    fn zoom_shows_only_focus() {
        let mut t = Tab::new(1);
        t.root.split(1, Axis::Horizontal, 2);
        t.focus = 2;
        t.zoomed = true;
        assert_eq!(
            t.layout(Rect::new(0, 0, 80, 24)),
            vec![(2, Rect::new(0, 0, 80, 24))]
        );
        t.zoomed = false;
        t.next_pane();
        assert_eq!(t.focus, 1);
    }

    #[test]
    fn drag_a_divider() {
        let mut n = Node::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Split {
                axis: Axis::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(2)),
                b: Box::new(Node::Leaf(3)),
            }),
        };
        let area = Rect::new(0, 0, 100, 40);
        let mut rects = Vec::new();
        n.layout(area, &mut rects);
        let r1 = rects[0].1;
        assert_eq!(n.divider_at(area, r1.right() - 1, 10), Some(vec![]));
        assert_eq!(n.divider_at(area, 10, 10), None);
        let r2 = rects[1].1;
        assert_eq!(n.divider_at(area, 70, r2.bottom() - 1), Some(vec![true]));
        assert!(n.drag_divider(area, &[], 29, 10));
        let Node::Split { ratio, .. } = &n else {
            panic!()
        };
        assert!((ratio - 0.3).abs() < 0.01, "{ratio}");
        assert!(n.drag_divider(area, &[true], 70, 9));
        let Node::Split { b, .. } = &n else { panic!() };
        let Node::Split { ratio, .. } = b.as_ref() else {
            panic!()
        };
        assert!((*ratio - 0.25).abs() < 0.01, "{ratio}");
        assert!(n.drag_divider(area, &[], 0, 10));
        let Node::Split { ratio, .. } = &n else {
            panic!()
        };
        assert!((*ratio - 0.1).abs() < 0.01, "clamped to the minimum");
    }
}

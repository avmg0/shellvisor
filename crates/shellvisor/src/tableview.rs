//! The interactive table viewer (F4 after an `sv` command, or the command
//! bar): scroll, sort by any column, filter, copy as CSV / JSON / Markdown,
//! and type a cell at the prompt.
//!
//! Rows are kept once; sorting and filtering only reorder a list of row
//! numbers, and only the rows on screen are turned into text — so 100k rows
//! stay responsive.

use crate::data::format;
use crate::data::ops;
use crate::data::value::{Data, Record, Value, columns, get};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::cell::Cell;

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const CYAN: Color = Color::Indexed(73);
const RED: Color = Color::Indexed(167);
const SEL_BG: Color = Color::Indexed(237);
const INK: Color = Color::Indexed(16);
const MAX_COL: usize = 40;

#[derive(Debug, Clone, PartialEq)]
pub enum TableAct {
    Close,
    /// Copy this text; the name of the format for the notice.
    Copy(String, &'static str),
    /// Type this at the prompt.
    Insert(String),
}

pub struct TableView {
    pub title: String,
    rows: Vec<Record>,
    cols: Vec<String>,
    numeric: Vec<bool>,
    /// Row numbers shown, in order (after filter and sort).
    view: Vec<usize>,
    widths: Vec<usize>,
    pub sel: usize,
    /// Selected column.
    pub col: usize,
    /// First column on screen.
    left: Cell<usize>,
    top: Cell<usize>,
    page: Cell<usize>,
    sort: Option<(usize, bool)>,
    filter: String,
    editing: bool,
    error: Option<String>,
}

impl TableView {
    pub fn new(title: impl Into<String>, data: Data) -> Self {
        let rows = match data {
            Data::Value(Value::Record(r)) => r
                .into_iter()
                .map(|(k, v)| {
                    vec![
                        ("field".to_string(), Value::Str(k)),
                        ("value".to_string(), v),
                    ]
                })
                .collect(),
            d => d.into_rows(),
        };
        let cols = columns(&rows);
        let numeric = cols
            .iter()
            .map(|c| {
                rows.iter()
                    .take(500)
                    .filter_map(|r| get(r, c))
                    .filter(|v| !matches!(v, Value::Nothing))
                    .all(|v| {
                        matches!(
                            v,
                            Value::Int(_) | Value::Float(_) | Value::Size(_) | Value::Duration(_)
                        )
                    })
            })
            .collect();
        let mut t = TableView {
            title: title.into(),
            view: (0..rows.len()).collect(),
            rows,
            cols,
            numeric,
            widths: Vec::new(),
            sel: 0,
            col: 0,
            left: Cell::new(0),
            top: Cell::new(0),
            page: Cell::new(10),
            sort: None,
            filter: String::new(),
            editing: false,
            error: None,
        };
        t.measure();
        t
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.view.len()
    }

    fn cell(&self, row: usize, c: usize) -> String {
        get(&self.rows[row], &self.cols[c])
            .map(|v| {
                v.display()
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Column widths from the header and the first rows of the view.
    fn measure(&mut self) {
        self.widths = (0..self.cols.len())
            .map(|c| {
                let w = self
                    .view
                    .iter()
                    .take(1000)
                    .map(|&r| self.cell(r, c).chars().count())
                    .max()
                    .unwrap_or(0);
                w.max(self.cols[c].chars().count() + 2).min(MAX_COL)
            })
            .collect();
    }

    fn apply(&mut self) {
        self.error = None;
        let f = self.filter.trim().to_string();
        let mut view: Vec<usize> = if f.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            let words: Vec<String> = f.split_whitespace().map(String::from).collect();
            let looks_like_where = words.len() >= 2;
            match looks_like_where.then(|| ops::row_matcher(&words)) {
                Some(Ok(m)) => (0..self.rows.len()).filter(|&i| m(&self.rows[i])).collect(),
                other => {
                    if let Some(Err(e)) = other {
                        if words.len() >= 3 {
                            self.error = Some(e);
                        }
                    }
                    // plain text: any cell contains it
                    let needle = f.to_lowercase();
                    (0..self.rows.len())
                        .filter(|&i| {
                            self.rows[i]
                                .iter()
                                .any(|(_, v)| v.plain().to_lowercase().contains(&needle))
                        })
                        .collect()
                }
            }
        };
        if let Some((c, desc)) = self.sort {
            let col = &self.cols[c];
            let rows = &self.rows;
            view.sort_by(|&a, &b| {
                let o = get(&rows[a], col)
                    .unwrap_or(&Value::Nothing)
                    .compare(get(&rows[b], col).unwrap_or(&Value::Nothing));
                if desc { o.reverse() } else { o }
            });
        }
        self.view = view;
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
        self.measure();
    }

    /// The rows as shown (filtered, sorted).
    fn shown(&self) -> Data {
        Data::Table(self.view.iter().map(|&i| self.rows[i].clone()).collect())
    }

    pub fn key(&mut self, key: &[u8]) -> (bool, Option<TableAct>) {
        if self.editing {
            match key {
                b"\r" => self.editing = false,
                b"\x1b" => {
                    self.editing = false;
                    self.filter.clear();
                    self.apply();
                }
                [0x7f] | [0x08] => {
                    self.filter.pop();
                    self.apply();
                }
                [0x15] => {
                    self.filter.clear();
                    self.apply();
                }
                k if !k.is_empty() && k[0] >= 0x20 && k[0] != 0x7f && k[0] != 0x1b => {
                    self.filter.push_str(&String::from_utf8_lossy(k));
                    self.apply();
                }
                _ => return (false, None),
            }
            return (true, None);
        }
        let n = self.view.len();
        let ncols = self.cols.len();
        match key {
            b"q" | b"\x1b" => {
                if !self.filter.is_empty() && key == b"\x1b" {
                    self.filter.clear();
                    self.apply();
                } else {
                    return (true, Some(TableAct::Close));
                }
            }
            b"\x1b[A" | b"\x1bOA" | b"k" => self.sel = self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" | b"j" => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            b"\x1b[5~" => self.sel = self.sel.saturating_sub(self.page.get()),
            b"\x1b[6~" | b" " => self.sel = (self.sel + self.page.get()).min(n.saturating_sub(1)),
            b"g" | b"\x1b[H" | b"\x1bOH" => self.sel = 0,
            b"G" | b"\x1b[F" | b"\x1bOF" => self.sel = n.saturating_sub(1),
            b"\x1b[D" | b"\x1bOD" | b"h" => self.col = self.col.saturating_sub(1),
            b"\x1b[C" | b"\x1bOC" | b"l" => self.col = (self.col + 1).min(ncols.saturating_sub(1)),
            b"s" | b"S" => {
                if ncols > 0 {
                    // s: sort by this column, again to flip; S: descending first
                    self.sort = match self.sort {
                        Some((c, d)) if c == self.col && key == b"s" => Some((c, !d)),
                        _ => Some((self.col, key == b"S")),
                    };
                    self.apply();
                }
            }
            b"/" => self.editing = true,
            b"c" => {
                return (
                    true,
                    Some(TableAct::Copy(format::to_csv(&self.shown(), ','), "CSV")),
                );
            }
            b"J" => {
                return (
                    true,
                    Some(TableAct::Copy(format::to_json_text(&self.shown()), "JSON")),
                );
            }
            b"m" => {
                return (
                    true,
                    Some(TableAct::Copy(
                        format::to_markdown(&self.shown()),
                        "Markdown",
                    )),
                );
            }
            b"i" | b"\r" => {
                if let Some(&r) = self.view.get(self.sel) {
                    if let Some(v) = self.cols.get(self.col).and_then(|c| get(&self.rows[r], c)) {
                        return (true, Some(TableAct::Insert(v.plain())));
                    }
                }
            }
            _ => return (false, None),
        }
        (true, None)
    }

    pub fn wheel(&mut self, dir: isize) {
        let n = self.view.len();
        self.sel = if dir > 0 {
            self.sel.saturating_sub(3)
        } else {
            (self.sel + 3).min(n.saturating_sub(1))
        };
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 4 || area.width < 10 {
            return;
        }
        let w = area.width as usize;
        // keep the selected column on screen
        let mut left = self.left.get().min(self.col);
        loop {
            let used: usize = self.widths[left..=self.col.min(self.widths.len().saturating_sub(1))]
                .iter()
                .map(|x| x + 2)
                .sum();
            if used <= w.saturating_sub(8) || left >= self.col {
                break;
            }
            left += 1;
        }
        self.left.set(left);
        let idx_w = self.rows.len().max(1).to_string().len().max(1);
        let body_h = area.height as usize - 3; // header, footer, filter/status
        self.page.set(body_h.max(1));
        let mut top = self.top.get();
        if self.sel < top {
            top = self.sel;
        } else if self.sel >= top + body_h {
            top = self.sel + 1 - body_h;
        }
        self.top.set(top);

        // header
        let y = area.y;
        let mut x = area.x + idx_w as u16 + 1;
        buf.set_string(
            area.x,
            y,
            format!("{:>idx_w$}", "#"),
            Style::default().fg(FAINT),
        );
        let mut visible_cols = Vec::new();
        for c in left..self.cols.len() {
            let cw = self.widths[c];
            if x as usize + 1 >= area.right() as usize {
                break;
            }
            let mut name = self.cols[c].clone();
            if let Some((sc, desc)) = self.sort {
                if sc == c {
                    name.push(if desc { '↓' } else { '↑' });
                }
            }
            let st = if c == self.col {
                Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
            };
            let text = pad(&fit(&name, cw), cw, self.numeric[c]);
            let room = (area.right() - x - 1) as usize;
            buf.set_stringn(x + 1, y, &text, room, st);
            visible_cols.push((c, x + 1, cw));
            x += cw as u16 + 2;
        }
        // rows
        for (i, &r) in self.view.iter().enumerate().skip(top).take(body_h) {
            let y = area.y + 1 + (i - top) as u16;
            let selected = i == self.sel;
            if selected {
                buf.set_style(
                    Rect::new(area.x, y, area.width, 1),
                    Style::default().bg(SEL_BG),
                );
            }
            buf.set_string(
                area.x,
                y,
                format!("{:>idx_w$}", i),
                Style::default().fg(FAINT),
            );
            for &(c, cx, cw) in &visible_cols {
                if cx >= area.right() {
                    break;
                }
                let text = pad(&fit(&self.cell(r, c), cw), cw, self.numeric[c]);
                let mut st = if c == 0 {
                    Style::default().fg(CYAN)
                } else {
                    Style::default()
                };
                if selected && c == self.col {
                    st = st.add_modifier(Modifier::REVERSED);
                } else if selected {
                    st = st.add_modifier(Modifier::BOLD);
                }
                buf.set_stringn(cx, y, &text, (area.right() - cx) as usize, st);
            }
        }
        if self.view.is_empty() {
            buf.set_string(
                area.x + 2,
                area.y + 1,
                "no rows match",
                Style::default().fg(DIM),
            );
        }
        // filter / status line
        let y = area.bottom() - 2;
        let mut status = format!(" {} of {} rows", self.view.len(), self.rows.len());
        if let Some((c, d)) = self.sort {
            status.push_str(&format!(
                " · sorted by {} {}",
                self.cols[c],
                if d { "↓" } else { "↑" }
            ));
        }
        if self.editing || !self.filter.is_empty() {
            let cursor = if self.editing { "▏" } else { "" };
            let label = " filter ";
            buf.set_string(
                area.x,
                y,
                label,
                Style::default()
                    .bg(CYAN)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD),
            );
            let (nx, _) = buf.set_stringn(
                area.x + label.len() as u16 + 1,
                y,
                format!("{}{cursor}", self.filter),
                w / 2,
                Style::default(),
            );
            let msg = match &self.error {
                Some(e) => (format!("  {e}"), Style::default().fg(RED)),
                None => (status, Style::default().fg(DIM)),
            };
            buf.set_stringn(
                nx,
                y,
                msg.0,
                area.right().saturating_sub(nx) as usize,
                msg.1,
            );
        } else {
            buf.set_stringn(area.x, y, status, w, Style::default().fg(DIM));
        }
        let hints = if self.editing {
            " type text, or a condition like: size > 1mb · name has test · ⏎ keep · esc clear"
        } else {
            " ↑↓ ←→ move · s sort · / filter · c CSV · J JSON · m Markdown (copy) · ⏎ insert cell · q close"
        };
        buf.set_stringn(
            area.x,
            area.bottom() - 1,
            hints,
            w,
            Style::default().fg(FAINT),
        );
    }
}

fn fit(s: &str, w: usize) -> String {
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

fn pad(s: &str, w: usize, right: bool) -> String {
    let n = w.saturating_sub(s.chars().count());
    if right {
        format!("{}{s}", " ".repeat(n))
    } else {
        format!("{s}{}", " ".repeat(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(n: usize) -> TableView {
        let rows = (0..n)
            .map(|i| {
                vec![
                    ("name".to_string(), Value::Str(format!("file{i}"))),
                    ("size".to_string(), Value::Size((i as u64 * 7919) % 100_000)),
                ]
            })
            .collect();
        TableView::new("test", Data::Table(rows))
    }

    fn screen(t: &TableView) -> String {
        let area = Rect::new(0, 0, 70, 12);
        let mut buf = Buffer::empty(area);
        t.render(area, &mut buf);
        (0..12)
            .map(|y| {
                (0..70)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn sort_filter_copy_insert() {
        let mut t = table(50);
        assert!(screen(&t).contains("file0"));
        t.key(b"l"); // size column
        t.key(b"s"); // ascending
        t.key(b"s"); // descending
        let s = screen(&t);
        assert!(s.contains("size↓") && s.contains("sorted by size ↓"), "{s}");
        t.key(b"/");
        for k in ["s", "i", "z", "e", " ", ">", " ", "9", "0", "k", "b"] {
            t.key(k.as_bytes());
        }
        t.key(b"\r");
        assert!(t.len() < 50 && t.len() > 0, "{}", t.len());
        let s = screen(&t);
        assert!(s.contains(&format!("{} of 50 rows", t.len())), "{s}");
        let (_, act) = t.key(b"c");
        let Some(TableAct::Copy(csv, "CSV")) = act else {
            panic!()
        };
        assert_eq!(csv.lines().count(), t.len() + 1);
        t.key(b"h");
        let (_, act) = t.key(b"\r");
        assert!(matches!(act, Some(TableAct::Insert(s)) if s.starts_with("file")));
        // esc clears the filter first, then closes
        t.key(b"\x1b");
        assert_eq!(t.len(), 50);
        assert_eq!(t.key(b"\x1b").1, Some(TableAct::Close));
    }

    #[test]
    fn plain_text_filter_and_records() {
        let mut t = table(30);
        t.key(b"/");
        for k in ["f", "i", "l", "e", "2"] {
            t.key(k.as_bytes());
        }
        assert_eq!(t.len(), 11); // file2, file20..29
        let rec = TableView::new(
            "host",
            Data::Value(Value::Record(vec![(
                "os".into(),
                Value::Str("macOS".into()),
            )])),
        );
        assert!(screen(&rec).contains("macOS"));
    }

    #[test]
    fn hundred_thousand_rows_stay_fast() {
        let mut t = table(100_000);
        let start = std::time::Instant::now();
        t.key(b"l");
        t.key(b"S");
        t.key(b"/");
        for k in ["s", "i", "z", "e", " ", ">", " ", "5", "0", "k", "b"] {
            t.key(k.as_bytes());
        }
        t.key(b"G");
        let _ = screen(&t);
        let took = start.elapsed();
        // generous for debug builds on slow CI machines
        assert!(took.as_millis() < 20_000, "{took:?}");
    }
}

//! Settings changed from inside Shellvisor (the settings screen, `sv
//! settings`): colours for the folder in your prompt and for folders in
//! lists, whether `ls` prints a table, table borders, theme.
//!
//! Kept in `~/.local/state/shellvisor/settings.yaml`, apart from your own
//! config.yaml, which Shellvisor never rewrites. The shell hooks read a tiny
//! companion file (`ui.env`, three lines) at every prompt with the shell's
//! own `read`, so a change shows at the next prompt without starting a
//! process.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Colours offered: (name, 256-colour index). "none" leaves things as they are.
pub const COLORS: [(&str, u8); 9] = [
    ("none", 0),
    ("cyan", 37),
    ("blue", 39),
    ("green", 35),
    ("yellow", 178),
    ("orange", 208),
    ("magenta", 170),
    ("red", 167),
    ("grey", 245),
];

pub fn color_index(name: &str) -> Option<u8> {
    COLORS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, i)| *i)
        .filter(|i| *i != 0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Colour of the folder in the shell prompt ("none": your prompt as is).
    pub prompt_folder: String,
    pub prompt_bold: bool,
    /// Colour of folder names in the files pane and in `ls` / `sv` tables.
    pub list_folder: String,
    pub list_bold: bool,
    /// `ls` at the prompt prints a table (like nushell).
    pub ls_table: bool,
    /// Tables with rounded borders ("rounded") or none ("plain").
    pub table_borders: String,
    /// "auto", "dark" or "light"; empty: config.yaml decides.
    pub theme: String,
    /// Dark theme background: "design" (the boards' dark blue-grey and
    /// colours) or "terminal" (your terminal's own background).
    pub background: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            prompt_folder: "none".into(),
            prompt_bold: false,
            list_folder: "cyan".into(),
            list_bold: true,
            ls_table: true,
            table_borders: "rounded".into(),
            theme: String::new(),
            background: "design".into(),
        }
    }
}

pub fn path() -> PathBuf {
    crate::paths::state_dir().join("settings.yaml")
}

/// The file the shell hooks read (path in `$SHELLVISOR_UI`).
pub fn ui_env_path() -> PathBuf {
    crate::paths::state_dir().join("ui.env")
}

impl Settings {
    pub fn load() -> Settings {
        Self::load_from(&path())
    }

    pub fn load_from(p: &Path) -> Settings {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| serde_yaml_ng::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save, and refresh the file the shell hooks read.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path(), &ui_env_path())
    }

    pub fn save_to(&self, p: &Path, ui: &Path) -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            p,
            serde_yaml_ng::to_string(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::write(ui, self.ui_env())
    }

    /// Three lines for the hooks: prompt colour index (or -), bold 0/1, ls table 0/1.
    pub fn ui_env(&self) -> String {
        let c = color_index(&self.prompt_folder)
            .map(|i| i.to_string())
            .unwrap_or_else(|| "-".into());
        format!("{c}\n{}\n{}\n", self.prompt_bold as u8, self.ls_table as u8)
    }

    /// ANSI start for folder names in tables (None: no colour).
    pub fn folder_ansi(&self) -> Option<String> {
        color_index(&self.list_folder)
            .map(|i| format!("\x1b[{}38;5;{i}m", if self.list_bold { "1;" } else { "" }))
    }

    pub fn boxed(&self) -> bool {
        self.table_borders != "plain"
    }
}

// ------------------------------------------------------------------ screen

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const ROWS: [&str; 8] = [
    "Folder colour in the prompt",
    "Folder in the prompt bold",
    "Folder colour in lists",
    "Folders bold in lists",
    "ls prints a table",
    "Table borders",
    "Theme",
    "Dark background",
];

/// The settings screen (alt+s, `sv settings`): ↑↓ pick, ←→ change.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub sel: usize,
    pub s: Settings,
    /// "you@machine", for the prompt preview.
    pub who: String,
}

fn cycle(list: &[&str], cur: &str, by: isize) -> String {
    let i = list.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    list[(i + by).rem_euclid(list.len() as isize) as usize].to_string()
}

impl Screen {
    pub fn new() -> Screen {
        let user = std::env::var("USER").unwrap_or_else(|_| "you".into());
        Screen {
            sel: 0,
            s: Settings::load(),
            who: format!("{user}@{}", crate::osinfo::hostname_short()),
        }
    }

    /// A key. Returns (changed settings, close).
    pub fn key(&mut self, key: &[u8]) -> (bool, bool) {
        let by = match key {
            b"\x1b[A" | b"\x1bOA" | b"k" => {
                self.sel = self.sel.saturating_sub(1);
                return (false, false);
            }
            b"\x1b[B" | b"\x1bOB" | b"j" => {
                self.sel = (self.sel + 1).min(ROWS.len() - 1);
                return (false, false);
            }
            b"\x1b[C" | b"\x1bOC" | b"l" | b" " | b"\r" => 1,
            b"\x1b[D" | b"\x1bOD" | b"h" => -1,
            b"\x1b" | b"q" | b"\x1bs" => return (false, true),
            _ => return (false, false),
        };
        let names: Vec<&str> = COLORS.iter().map(|(n, _)| *n).collect();
        let s = &mut self.s;
        match self.sel {
            0 => s.prompt_folder = cycle(&names, &s.prompt_folder, by),
            1 => s.prompt_bold = !s.prompt_bold,
            2 => s.list_folder = cycle(&names, &s.list_folder, by),
            3 => s.list_bold = !s.list_bold,
            4 => s.ls_table = !s.ls_table,
            5 => s.table_borders = cycle(&["rounded", "plain"], &s.table_borders, by),
            7 => s.background = cycle(&["design", "terminal"], &s.background, by),
            _ => {
                let cur = if s.theme.is_empty() {
                    "config"
                } else {
                    s.theme.as_str()
                };
                let next = cycle(&["config", "auto", "dark", "light"], cur, by);
                s.theme = if next == "config" {
                    String::new()
                } else {
                    next
                };
            }
        }
        (true, false)
    }

    fn value(&self, row: usize) -> String {
        let s = &self.s;
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        match row {
            0 => s.prompt_folder.clone(),
            1 => on(s.prompt_bold),
            2 => s.list_folder.clone(),
            3 => on(s.list_bold),
            4 => on(s.ls_table),
            5 => s.table_borders.clone(),
            7 => {
                if s.background == "terminal" {
                    "your terminal's".into()
                } else {
                    "design (dark blue-grey)".into()
                }
            }
            _ => {
                if s.theme.is_empty() {
                    "from config.yaml".into()
                } else {
                    s.theme.clone()
                }
            }
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, user: &str, folder: &str) {
        let dim = Style::default().fg(Color::Indexed(245));
        let faint = Style::default().fg(Color::Indexed(243));
        let text = Style::default().fg(Color::Indexed(252));
        let amber = Color::Indexed(179);
        let put = |buf: &mut Buffer, x: u16, y: u16, s: &str, st: Style| {
            if y < area.bottom() && x < area.right() {
                buf.set_stringn(x, y, s, (area.right() - x) as usize, st);
            }
        };
        let (x0, mut y) = (area.x + 2, area.y + 1);
        put(buf, x0, y, "Settings", text.add_modifier(Modifier::BOLD));
        put(
            buf,
            x0 + 10,
            y,
            "saved as you change them · shells pick them up at the next prompt",
            faint,
        );
        y += 2;
        for (i, label) in ROWS.iter().enumerate() {
            let on = i == self.sel;
            let st = if on {
                Style::default()
                    .fg(Color::Indexed(16))
                    .bg(amber)
                    .add_modifier(Modifier::BOLD)
            } else {
                text
            };
            let line = format!(" {label:<30}");
            put(buf, x0, y, &line, st);
            let v = self.value(i);
            let vx = x0 + 32;
            let color = match i {
                0 | 2 => color_index(&v).map(Color::Indexed),
                _ => None,
            };
            let vst = match color {
                Some(c) => Style::default().fg(c).add_modifier(Modifier::BOLD),
                None => text,
            };
            put(
                buf,
                vx,
                y,
                if on { "◀ " } else { "  " },
                Style::default().fg(amber),
            );
            put(buf, vx + 2, y, &v, vst);
            if on {
                put(
                    buf,
                    vx + 3 + v.chars().count() as u16,
                    y,
                    "▶",
                    Style::default().fg(amber),
                );
            }
            y += 1;
        }
        y += 1;
        put(buf, x0, y, "Preview", dim);
        y += 1;
        // prompt preview
        let mut x = x0 + 1;
        let pre = format!("{user} ");
        put(buf, x, y, &pre, text);
        x += pre.chars().count() as u16;
        let mut fst = text;
        if let Some(c) = color_index(&self.s.prompt_folder) {
            fst = Style::default().fg(Color::Indexed(c));
            if self.s.prompt_bold {
                fst = fst.add_modifier(Modifier::BOLD);
            }
        }
        put(buf, x, y, folder, fst);
        x += folder.chars().count() as u16;
        put(buf, x, y, " % ls", text);
        y += 1;
        // ls preview
        let mut lst = text;
        if let Some(c) = color_index(&self.s.list_folder) {
            lst = Style::default().fg(Color::Indexed(c));
            if self.s.list_bold {
                lst = lst.add_modifier(Modifier::BOLD);
            }
        }
        let rows = [
            ("src", "dir", "  4.0 KB", "2 hours ago"),
            ("docs", "dir", "  4.0 KB", "a day ago"),
            ("README.md", "file", " 15.7 KB", "3 days ago"),
        ];
        if !self.s.ls_table {
            put(
                buf,
                x0 + 1,
                y,
                "src  docs  README.md      (your normal ls)",
                text,
            );
        } else if self.s.boxed() {
            let border = Style::default().fg(Color::Indexed(240));
            let green = Style::default()
                .fg(Color::Indexed(35))
                .add_modifier(Modifier::BOLD);
            put(
                buf,
                x0 + 1,
                y,
                "╭───┬───────────┬──────┬──────────┬─────────────╮",
                border,
            );
            y += 1;
            put(
                buf,
                x0 + 1,
                y,
                "│   │           │      │          │             │",
                border,
            );
            put(buf, x0 + 3, y, "#", green);
            put(buf, x0 + 7, y, "name", green);
            put(buf, x0 + 19, y, "type", green);
            put(buf, x0 + 30, y, "size", green);
            put(buf, x0 + 37, y, "modified", green);
            y += 1;
            put(
                buf,
                x0 + 1,
                y,
                "├───┼───────────┼──────┼──────────┼─────────────┤",
                border,
            );
            for (n, (name, kind, size, when)) in rows.iter().enumerate() {
                y += 1;
                put(
                    buf,
                    x0 + 1,
                    y,
                    "│   │           │      │          │             │",
                    border,
                );
                put(buf, x0 + 3, y, &n.to_string(), green);
                put(
                    buf,
                    x0 + 7,
                    y,
                    name,
                    if *kind == "dir" { lst } else { text },
                );
                put(buf, x0 + 19, y, kind, text);
                put(buf, x0 + 26, y, size, text);
                put(buf, x0 + 37, y, when, text);
            }
            y += 1;
            put(
                buf,
                x0 + 1,
                y,
                "╰───┴───────────┴──────┴──────────┴─────────────╯",
                border,
            );
        } else {
            let green = Style::default()
                .fg(Color::Indexed(179))
                .add_modifier(Modifier::BOLD);
            put(
                buf,
                x0 + 1,
                y,
                "#  name       type  size      modified",
                green,
            );
            for (n, (name, kind, size, when)) in rows.iter().enumerate() {
                y += 1;
                put(buf, x0 + 1, y, &n.to_string(), dim);
                put(
                    buf,
                    x0 + 4,
                    y,
                    name,
                    if *kind == "dir" { lst } else { text },
                );
                put(buf, x0 + 15, y, kind, text);
                put(buf, x0 + 20, y, size.trim(), text);
                put(buf, x0 + 30, y, when, text);
            }
        }
        let hint =
            "↑↓ setting · ←→ change · esc close  ·  `ls -l` and other flags always run the real ls";
        put(buf, x0, area.bottom().saturating_sub(1), hint, faint);
    }
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_and_hook_file() {
        let dir = std::env::temp_dir().join(format!("sv-settings-{}", std::process::id()));
        let (p, ui) = (dir.join("settings.yaml"), dir.join("ui.env"));
        let mut s = Settings::default();
        assert_eq!(
            s.ui_env(),
            "-\n0\n1\n",
            "the prompt is left alone by default"
        );
        s.prompt_folder = "orange".into();
        s.prompt_bold = true;
        s.ls_table = false;
        s.save_to(&p, &ui).unwrap();
        assert_eq!(Settings::load_from(&p), s);
        assert_eq!(std::fs::read_to_string(&ui).unwrap(), "208\n1\n0\n");
        assert_eq!(
            Settings::default().folder_ansi().unwrap(),
            "\x1b[1;38;5;37m"
        );
        s.list_folder = "none".into();
        assert_eq!(s.folder_ansi(), None);
        assert_eq!(
            Settings::load_from(&dir.join("missing.yaml")),
            Settings::default()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn screen_changes_values() {
        let mut sc = Screen {
            sel: 0,
            s: Settings::default(),
            who: String::new(),
        };
        assert_eq!(sc.key(b"\x1b[C"), (true, false));
        assert_eq!(sc.s.prompt_folder, "cyan");
        sc.key(b"\x1b[D");
        sc.key(b"\x1b[D");
        assert_eq!(sc.s.prompt_folder, "grey", "wraps around");
        sc.key(b"\x1b[B");
        sc.key(b"\r");
        assert!(sc.s.prompt_bold);
        for _ in 0..3 {
            sc.key(b"\x1b[B");
        }
        sc.key(b" ");
        assert!(!sc.s.ls_table);
        assert_eq!(sc.key(b"\x1b"), (false, true));
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        sc.render(area, &mut buf, "aditya@mac", "homebrew-tap");
        let t: String = (0..30)
            .map(|y| {
                (0..100)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(
            t.contains("Folder colour in the prompt")
                && t.contains("aditya@mac homebrew-tap % ls")
                && t.contains("your normal ls"),
            "{t}"
        );
    }
}

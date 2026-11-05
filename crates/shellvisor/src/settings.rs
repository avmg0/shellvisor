//! Settings changed from inside Shellvisor (the settings screen, `sv
//! settings`): colours for the folder in your prompt, for the commands you
//! type and for folders in lists, whether `ls` prints a table, table
//! borders, theme.
//!
//! Kept in `~/.local/state/shellvisor/settings.yaml`, apart from your own
//! config.yaml, which Shellvisor never rewrites. The shell hooks read a tiny
//! companion file (`ui.env`, four lines) at every prompt with the shell's
//! own `read`, so a change shows at the next prompt without starting a
//! process.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Colours offered: (name, colour index). "none" leaves things as they are.
/// They are the terminal's own 16 colours (its profile decides the exact
/// shade, so folders match a prompt drawn in the same colours), except
/// orange, which the 16 don't have.
pub const COLORS: [(&str, u8); 9] = [
    ("none", 0),
    ("cyan", 6),
    // bright blue: plain blue is too dark to read in many profiles
    ("blue", 12),
    ("green", 2),
    ("yellow", 3),
    ("orange", 208),
    ("magenta", 5),
    ("red", 1),
    ("grey", 8),
];

pub fn color_index(name: &str) -> Option<u8> {
    // "light blue" was its own choice before; it is blue now
    let name = if name == "light blue" { "blue" } else { name };
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
    /// Colour of the commands you type at the prompt ("none": your
    /// terminal's text colour); output stays in the normal colour.
    pub typed: String,
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
    /// Spinner for downloads and installs: circle, dots, arc, line, pulse, bounce.
    pub spinner: String,
    /// Downloads show a filling bar (or only the percentage).
    pub progress_bar: bool,
    /// Colour depth: "auto" (what the terminal says), "24-bit" or "256".
    pub colors: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            prompt_folder: "none".into(),
            prompt_bold: false,
            typed: "blue".into(),
            list_folder: "cyan".into(),
            list_bold: true,
            ls_table: true,
            table_borders: "rounded".into(),
            theme: String::new(),
            background: "design".into(),
            spinner: "ring".into(),
            progress_bar: true,
            colors: "auto".into(),
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

    /// Four lines for the hooks: prompt colour index (or -), bold 0/1,
    /// ls table 0/1, typed-command colour index (or -).
    pub fn ui_env(&self) -> String {
        let idx = |name: &str| {
            color_index(name)
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".into())
        };
        format!(
            "{}\n{}\n{}\n{}\n",
            idx(&self.prompt_folder),
            self.prompt_bold as u8,
            self.ls_table as u8,
            idx(&self.typed)
        )
    }

    /// ANSI start for folder names in tables (None: no colour).
    pub fn folder_ansi(&self) -> Option<String> {
        color_index(&self.list_folder)
            .map(|i| format!("\x1b[{}38;5;{i}m", if self.list_bold { "1;" } else { "" }))
    }

    /// Use 24-bit colour (the setting, or what the terminal says).
    pub fn true_color(&self) -> bool {
        match self.colors.as_str() {
            "24-bit" => true,
            "256" => false,
            _ => crate::palette::truecolor_terminal(),
        }
    }

    pub fn boxed(&self) -> bool {
        self.table_borders != "plain"
    }
}

// ------------------------------------------------------------------ screen

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

const ROWS: [&str; 12] = [
    "Folder colour in the prompt",
    "Folder in the prompt bold",
    "Commands you type",
    "Folder colour in lists",
    "Folders bold in lists",
    "ls prints a table",
    "Table borders",
    "Theme",
    "Dark background",
    "Loading spinner",
    "Download progress",
    "Colours",
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
            2 => s.typed = cycle(&names, &s.typed, by),
            3 => s.list_folder = cycle(&names, &s.list_folder, by),
            4 => s.list_bold = !s.list_bold,
            5 => s.ls_table = !s.ls_table,
            6 => s.table_borders = cycle(&["rounded", "plain"], &s.table_borders, by),
            8 => s.background = cycle(&["design", "terminal"], &s.background, by),
            9 => {
                let names: Vec<&str> = crate::loading::STYLES.iter().map(|(n, _)| *n).collect();
                s.spinner = cycle(&names, &s.spinner, by);
            }
            10 => s.progress_bar = !s.progress_bar,
            11 => s.colors = cycle(&["auto", "24-bit", "256"], &s.colors, by),
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
            2 => s.typed.clone(),
            3 => s.list_folder.clone(),
            4 => on(s.list_bold),
            5 => on(s.ls_table),
            6 => s.table_borders.clone(),
            9 => format!("{} {}", crate::loading::frame_of(&s.spinner), s.spinner),
            10 => if s.progress_bar {
                "bar"
            } else {
                "percent only"
            }
            .to_string(),
            11 => match s.colors.as_str() {
                "24-bit" | "256" => s.colors.clone(),
                _ if crate::palette::truecolor_terminal() => "auto · 24-bit".into(),
                _ => "auto · 256 (no 24-bit from this terminal)".into(),
            },
            8 => {
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
                0 | 2 | 3 => color_index(&v).map(Color::Indexed),
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
        put(buf, x, y, " % ", text);
        x += 3;
        let tst = color_index(&self.s.typed)
            .map(|c| Style::default().fg(Color::Indexed(c)))
            .unwrap_or(text);
        put(buf, x, y, "ls", tst);
        y += 1;
        // download preview
        let frame = crate::loading::frame_of(&self.s.spinner);
        let amber_st = Style::default().fg(amber);
        put(buf, x0 + 1, y, &format!("{frame} git clone"), amber_st);
        let mut dx = x0 + 13;
        if self.s.progress_bar {
            put(buf, dx, y, "▕", faint);
            put(buf, dx + 1, y, &crate::loading::bar(Some(45), 12), amber_st);
            put(buf, dx + 13, y, "▏", faint);
            dx += 15;
        }
        put(buf, dx, y, "45% · 3.0 MB/s · 12s left", text);
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
        if !self.s.true_color() && std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal")
        {
            put(
                buf,
                x0,
                area.bottom().saturating_sub(2),
                "Terminal.app before macOS 26 has 256 colours: run `sv terminal-colors` for the design's dark blue",
                Style::default().fg(amber),
            );
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
            "-\n0\n1\n12\n",
            "the prompt is left alone by default; typed commands in blue"
        );
        s.prompt_folder = "orange".into();
        s.prompt_bold = true;
        s.ls_table = false;
        s.typed = "none".into();
        s.save_to(&p, &ui).unwrap();
        assert_eq!(Settings::load_from(&p), s);
        assert_eq!(std::fs::read_to_string(&ui).unwrap(), "208\n1\n0\n-\n");
        assert_eq!(Settings::default().folder_ansi().unwrap(), "\x1b[1;38;5;6m");
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
        sc.key(b"\x1b[B");
        sc.key(b"\x1b[D");
        assert_eq!(sc.s.typed, "cyan");
        for _ in 0..3 {
            sc.key(b"\x1b[B");
        }
        sc.key(b" ");
        assert!(!sc.s.ls_table);
        for _ in 0..10 {
            sc.key(b"\x1b[B");
        }
        assert_eq!(sc.sel, 11, "stops at the last row");
        sc.key(b"\x1b[A");
        sc.key(b"\x1b[A");
        sc.key(b"\x1b[C");
        assert_eq!(sc.s.spinner, "braille");
        sc.key(b"\x1b[B");
        sc.key(b"\r");
        assert!(!sc.s.progress_bar);
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

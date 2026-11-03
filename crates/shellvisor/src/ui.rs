//! Drawing: tab bar, bordered panes, status bar and overlays.
//!
//! Colours use the 256-colour palette (not 24-bit) so they look the same in
//! Terminal.app, which has no true colour.

use crate::app::{App, Modal, PaneState, View};
use crate::keys;
use crate::term::TermView;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(240);
const BAR: Color = Color::Indexed(236);
const INK: Color = Color::Indexed(16);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.epoch.elapsed().as_millis() / 100) as usize % SPINNER.len()]
}

pub fn draw(f: &mut Frame, app: &App) {
    let (tabbar, _, status) = app.regions();
    let full = app.view == View::Full;
    let focused = app.focused();

    for (id, outer, inner) in app.visible() {
        let Some(p) = app.panes.get(&id) else {
            continue;
        };
        if full {
            let is_focus = id == focused;
            let border = Style::default().fg(if is_focus { AMBER } else { FAINT });
            let mut title = format!(" {} · {} ", p.shell, short_path(&p.cwd));
            if app.tabs[app.active].zoomed {
                title.push_str("· zoomed ");
            }
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(border)
                .title(Span::styled(
                    title,
                    if is_focus {
                        border.add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(DIM)
                    },
                ));
            f.render_widget(block, outer);
        }
        f.render_widget(TermView(p.pane.screen()), inner);
        if p.pane.scroll() > 0 {
            badge(
                f,
                inner,
                0,
                &format!(" ↑ {} lines · type or scroll to return ", p.pane.scroll()),
            );
        }
    }

    if let Some(r) = tabbar {
        draw_tabs(f, app, r);
    }
    if let Some(r) = status {
        draw_status(f, app, r);
    }

    if let Some((msg, _)) = &app.notice {
        let (_, content, _) = app.regions();
        badge(f, content, if full { 1 } else { 0 }, &format!(" {msg} "));
    }

    match &app.modal {
        Modal::Leader => {
            let row = status.unwrap_or(Rect::new(
                0,
                app.screen.height.saturating_sub(1),
                app.screen.width,
                1,
            ));
            f.render_widget(Clear, row);
            f.render_widget(
                Paragraph::new(keys::LEADER_HINT).style(Style::default().bg(AMBER).fg(INK)),
                row,
            );
        }
        Modal::Help => draw_help(f, app),
        Modal::Rename(name) => {
            let row = status.unwrap_or(Rect::new(
                0,
                app.screen.height.saturating_sub(1),
                app.screen.width,
                1,
            ));
            f.render_widget(Clear, row);
            let line = Line::from(vec![
                Span::styled(" Rename tab: ", Style::default().bg(AMBER).fg(INK)),
                Span::raw(format!(" {name}")),
                Span::styled("   ⏎ save · esc cancel", Style::default().fg(DIM)),
            ]);
            f.render_widget(Paragraph::new(line), row);
            let x = row.x + 14 + name.chars().count() as u16;
            f.set_cursor_position(Position::new(x.min(row.right().saturating_sub(1)), row.y));
            return;
        }
        Modal::Confirm(_, msg) => {
            let w = (msg.chars().count() as u16 + 6).min(app.screen.width);
            let area = centered(app.screen, w, 5);
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(AMBER))
                .title(Span::styled(" confirm ", Style::default().fg(AMBER)));
            f.render_widget(
                Paragraph::new(vec![
                    Line::raw(""),
                    Line::styled(
                        format!("  {msg}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
                .block(block),
                area,
            );
            return;
        }
        Modal::None => {}
    }

    if !matches!(app.modal, Modal::Help) {
        if let Some((_, _, inner)) = app.visible().into_iter().find(|(id, _, _)| *id == focused) {
            if let Some((x, y)) = app.panes.get(&focused).and_then(|p| p.pane.cursor()) {
                if x < inner.width && y < inner.height {
                    f.set_cursor_position(Position::new(inner.x + x, inner.y + y));
                }
            }
        }
    }
}

fn badge(f: &mut Frame, area: Rect, row: u16, text: &str) {
    let w = text.chars().count() as u16;
    if area.width > w && row < area.height {
        f.buffer_mut().set_string(
            area.x + area.width - w,
            area.y + row,
            text,
            Style::default().add_modifier(Modifier::REVERSED),
        );
    }
}

fn centered(r: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(r.width);
    let h = h.min(r.height);
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}

/// `/Users/a/PROJECTS/FinancialApp/src` → `~/…/FinancialApp/src`
pub fn short_path(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = if !home.is_empty() && (p == home || p.starts_with(&format!("{home}/"))) {
        format!("~{}", &p[home.len()..])
    } else {
        p.to_string()
    };
    let parts: Vec<&str> = p.split('/').collect();
    if parts.len() > 4 {
        format!("{}/…/{}", parts[0], parts[parts.len() - 2..].join("/"))
    } else {
        p
    }
}

fn basename(p: &str) -> &str {
    p.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
}

/// State icon + automatic label for a tab.
fn tab_label(app: &App, i: usize) -> (String, Option<(String, Color)>) {
    let tab = &app.tabs[i];
    let panes: Vec<&PaneState> = tab
        .root
        .panes()
        .iter()
        .filter_map(|id| app.panes.get(id))
        .collect();
    let main = app.panes.get(&tab.focus).or(panes.first().copied());
    let shell = main.map(|p| p.shell.as_str()).unwrap_or("?");
    let what = match (&tab.name, main) {
        (Some(n), _) => n.clone(),
        (None, Some(p)) if p.running && !p.command.is_empty() => {
            let c: String = p.command.chars().take(18).collect();
            c
        }
        (None, Some(p)) => basename(&short_path(&p.cwd)).to_string(),
        (None, None) => String::new(),
    };
    let split = if panes.len() > 1 {
        format!(" ⊞{}", panes.len())
    } else {
        String::new()
    };
    let icon = if panes.iter().any(|p| p.running) {
        Some((spinner(app).to_string(), AMBER))
    } else if let Some(code) = panes.iter().filter_map(|p| p.last_exit).find(|c| *c != 0) {
        Some((format!("✖{code}"), RED))
    } else if panes.iter().any(|p| p.bell) {
        Some(("!".to_string(), AMBER))
    } else if panes.iter().any(|p| p.unseen) {
        Some(("•".to_string(), CYAN))
    } else {
        None
    };
    (format!("{} {shell} · {what}{split}", i + 1), icon)
}

/// Tab index under screen column `x` (for mouse clicks).
pub fn tab_at(app: &App, x: u16) -> Option<usize> {
    let mut col = 0u16;
    for i in 0..app.tabs.len() {
        let w = tab_width(app, i);
        if x >= col && x < col + w {
            return Some(i);
        }
        col += w + 1;
    }
    None
}

fn tab_width(app: &App, i: usize) -> u16 {
    let (label, icon) = tab_label(app, i);
    let icon_w = icon.map(|(s, _)| s.chars().count() + 1).unwrap_or(0);
    (label.chars().count() + icon_w + 2) as u16
}

fn draw_tabs(f: &mut Frame, app: &App, r: Rect) {
    f.render_widget(Paragraph::new("").style(Style::default().bg(BAR)), r);
    let mut spans = Vec::new();
    for i in 0..app.tabs.len() {
        let (label, icon) = tab_label(app, i);
        let active = i == app.active;
        let base = if active {
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(BAR).fg(DIM)
        };
        spans.push(Span::styled(" ", base));
        if let Some((s, c)) = icon {
            spans.push(Span::styled(
                format!("{s} "),
                if active { base } else { base.fg(c) },
            ));
        }
        spans.push(Span::styled(format!("{label} "), base));
        spans.push(Span::styled(" ", Style::default().bg(BAR)));
    }
    spans.push(Span::styled("+ alt+t", Style::default().bg(BAR).fg(FAINT)));
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn draw_status(f: &mut Frame, app: &App, r: Rect) {
    let p = app.panes.get(&app.focused());
    let mut left = vec![
        Span::styled(
            format!(" ▍{} ", crate::NAME_LOWER),
            Style::default()
                .fg(AMBER)
                .bg(BAR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", p.map(|p| short_path(&p.cwd)).unwrap_or_default()),
            Style::default().fg(CYAN).bg(BAR),
        ),
    ];
    if let Some(p) = p {
        if p.running {
            let secs = p.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
            let cmd: String = p.command.chars().take(40).collect();
            left.push(Span::styled(
                format!(" {} {cmd} {} ", spinner(app), fmt_secs(secs)),
                Style::default().fg(AMBER).bg(BAR),
            ));
        } else if let Some(code) = p.last_exit {
            let (txt, c) = if code == 0 {
                ("✔".to_string(), GREEN)
            } else {
                (format!("✖ {code}"), RED)
            };
            left.push(Span::styled(
                format!(" {txt} "),
                Style::default().fg(c).bg(BAR),
            ));
        }
    }
    let right = format!(
        " mem {} · ctrl+] menu · F1 help · {} ",
        crate::sysinfo::self_memory(),
        crate::sysinfo::clock()
    );
    f.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(BAR)),
        r,
    );
    let w = right.chars().count() as u16;
    if r.width > w + 30 {
        f.buffer_mut().set_string(
            r.x + r.width - w,
            r.y,
            &right,
            Style::default().fg(DIM).bg(BAR),
        );
    }
}

fn fmt_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

const HELP: &[(&str, &str)] = &[
    ("", "Global — work everywhere, even inside vim"),
    ("F1", "this help"),
    ("alt+`", "full view ⇄ basic view (also: sv on / sv off)"),
    ("ctrl+q", "quit (asks if commands are running)"),
    (
        "ctrl+]",
        "leader key: then one letter (works in every terminal)",
    ),
    ("", "Tabs"),
    ("alt+t · F10", "new tab in the same folder"),
    ("alt+1…9", "go to tab"),
    ("alt+[ · alt+]", "previous / next tab"),
    ("alt+r · alt+w", "rename · close tab"),
    ("", "Splits"),
    ("alt+\\ · alt+-", "split side by side · stacked"),
    ("alt+arrows · F6", "move between splits · next split"),
    ("alt+shift+arrows", "resize split"),
    ("F5", "zoom split full-screen"),
    ("ctrl+] x", "close split"),
    ("", "Scrolling"),
    (
        "mouse wheel",
        "scroll back (Option-drag / iTerm2 to select text)",
    ),
    ("shift+PgUp/PgDn", "scroll back one page"),
];

fn draw_help(f: &mut Frame, app: &App) {
    let area = centered(app.screen, 72, HELP.len() as u16 + 6);
    f.render_widget(Clear, area);
    let mut lines = vec![Line::raw("")];
    for (k, v) in HELP {
        if k.is_empty() {
            lines.push(Line::styled(
                format!("  {v}"),
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<18}"), Style::default().fg(AMBER)),
                Span::raw(*v),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  any key closes", Style::default().fg(DIM)));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            format!(" {} keys ", crate::NAME),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/u".into());
        assert_eq!(short_path(&home), "~");
        assert_eq!(short_path(&format!("{home}/a/b")), "~/a/b");
        assert_eq!(short_path("/a/b/c/d/e"), "/…/d/e");
        assert_eq!(basename("/usr/lib/"), "lib");
        assert_eq!(basename("/"), "/");
        assert_eq!(fmt_secs(75), "1m15s");
    }
}

//! Drawing: tab bar, bordered panes, status bar and overlays.
//!
//! Colours use the 256-colour palette (not 24-bit) so they look the same in
//! Terminal.app, which has no true colour.

use crate::activity::Activity;
use crate::app::{App, MENU, Modal, PaneState, Tool, View};
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

/// The loading circle for downloads and installs.
const CIRCLE: [&str; 4] = ["◐", "◓", "◑", "◒"];

fn tick(app: &App) -> usize {
    let every = if app.low_power { 250 } else { 100 };
    (app.epoch.elapsed().as_millis() / every) as usize
}

fn spinner(app: &App) -> &'static str {
    SPINNER[tick(app) % SPINNER.len()]
}

fn circle(app: &App) -> &'static str {
    CIRCLE[(tick(app) / 2) % CIRCLE.len()]
}

/// "◐ npm install 42%"
fn activity_text(app: &App, a: &Activity) -> String {
    match a.progress {
        Some(p) => format!("{} {} {p}%", circle(app), a.label),
        None => format!("{} {}", circle(app), a.label),
    }
}

fn pane_block(title: String, is_focus: bool) -> Block<'static> {
    let border = Style::default().fg(if is_focus { AMBER } else { FAINT });
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Span::styled(
            title,
            if is_focus {
                border.add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ))
}

pub fn draw(f: &mut Frame, app: &App) {
    let (tabbar, _, status) = app.regions();
    let full = app.view == View::Full;
    let focused = app.input_target();
    let zoomed = app.tabs[app.active].zoomed;

    for (id, outer, inner) in app.visible() {
        let is_focus = id == focused;
        if let Some(t) = app.tools.get(&id) {
            if full {
                let mut title = format!(" {} ", t.title());
                if zoomed {
                    title.push_str("· zoomed ");
                }
                f.render_widget(pane_block(title, is_focus), outer);
            }
            match t {
                Tool::Files(fp) => fp.render(inner, f.buffer_mut(), is_focus),
                Tool::System(sp) => sp.render(inner, f.buffer_mut(), app.low_power),
            }
            continue;
        }
        let Some(p) = app.panes.get(&id) else {
            continue;
        };
        if full {
            let mut title = format!(" {} · {} ", p.shell, short_path(&p.cwd));
            if let Some(a) = &p.activity {
                title.push_str(&format!("· {} ", activity_text(app, a)));
            }
            if zoomed {
                title.push_str("· zoomed ");
            }
            f.render_widget(pane_block(title, is_focus), outer);
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
        Modal::Menu(sel) => draw_menu(f, app, *sel),
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
    let main = app
        .panes
        .get(&tab.focus)
        .or_else(|| app.paired_shell(i).and_then(|id| app.panes.get(&id)))
        .or(panes.first().copied());
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
    let n = tab.root.panes().len();
    let split = if n > 1 {
        format!(" ⊞{n}")
    } else {
        String::new()
    };
    let icon = if panes.iter().any(|p| p.activity.is_some()) {
        Some((circle(app).to_string(), AMBER))
    } else if panes.iter().any(|p| p.running) {
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
    let target = app.input_target();
    let tool = app.tools.get(&target);
    let p = app.panes.get(&target).or_else(|| {
        app.paired_shell(app.active)
            .and_then(|id| app.panes.get(&id))
    });
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
    if let Some(t) = tool {
        left.push(Span::styled(
            format!(" {} ", t.kind()),
            Style::default().fg(INK).bg(CYAN),
        ));
    }
    // Downloads anywhere (other tabs too) show here.
    let acts: Vec<&Activity> = app
        .panes
        .values()
        .filter_map(|p| p.activity.as_ref())
        .collect();
    if let Some(first) = acts.first() {
        let more = if acts.len() > 1 {
            format!(" +{}", acts.len() - 1)
        } else {
            String::new()
        };
        left.push(Span::styled(
            format!(" {}{more} ", activity_text(app, first)),
            Style::default().fg(AMBER).bg(BAR),
        ));
    } else if let Some(p) = p {
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
        " {}mem {} · ctrl+] menu · F1 help · {} ",
        if app.low_power {
            "◌ low-power · "
        } else {
            ""
        },
        crate::osinfo::self_memory(),
        crate::osinfo::clock()
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

pub fn fmt_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Section headings in the help screen, placed before these actions.
const SECTIONS: &[(&str, &str)] = &[
    ("help", "Global — work everywhere, even inside vim"),
    ("new_tab", "Tabs"),
    ("split_right", "Splits"),
    ("scroll_up", "Scrolling"),
    (
        "toggle_files",
        "Panes — in files: ⏎ open · i insert path · c cd · e edit · p preview · / filter · tab view",
    ),
];

/// Help rows built from the live keymap, so custom keys show up here.
pub fn help_rows(app: &App) -> Vec<(String, String, bool)> {
    let mut rows = Vec::new();
    for (name, _, _, desc) in keys::ACTIONS {
        if let Some((_, title)) = SECTIONS.iter().find(|(n, _)| n == name) {
            rows.push((String::new(), title.to_string(), true));
        }
        if desc.is_empty() {
            continue;
        }
        let shown = app.keymap().shown(name);
        let key = if shown.is_empty() {
            if *name == "close_pane" {
                "ctrl+] x".to_string()
            } else if name.starts_with("preset_") || *name == "low_power" {
                "alt+m menu".to_string()
            } else {
                "(off)".to_string()
            }
        } else {
            shown.to_string()
        };
        rows.push((key, desc.to_string(), false));
    }
    rows
}

fn draw_help(f: &mut Frame, app: &App) {
    let rows = help_rows(app);
    let area = centered(app.screen, 78, rows.len() as u16 + 5);
    f.render_widget(Clear, area);
    let mut lines = vec![Line::raw("")];
    for (k, v, heading) in rows {
        if heading {
            lines.push(Line::styled(
                format!("  {v}"),
                Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
            ));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<20}"), Style::default().fg(AMBER)),
                Span::raw(v),
            ]));
        }
    }
    lines.push(Line::styled(
        "  any key closes · change keys in ~/.config/shellvisor/config.yaml",
        Style::default().fg(DIM),
    ));
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

fn draw_menu(f: &mut Frame, app: &App, sel: usize) {
    let area = centered(app.screen, 34, MENU.len() as u16 + 4);
    f.render_widget(Clear, area);
    let mut lines = vec![Line::raw("")];
    for (i, (label, _)) in MENU.iter().enumerate() {
        let num = match i {
            0..9 => (i + 1).to_string(),
            9 => "0".into(),
            _ => " ".into(),
        };
        let text = format!(" {num}  {label:<26}");
        lines.push(if i == sel {
            Line::styled(
                text,
                Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Line::raw(text)
        });
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            format!(" tab {} ", app.active + 1),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(" ↑↓ ⏎ · esc ", Style::default().fg(DIM)));
    f.render_widget(Paragraph::new(lines).block(block), area);
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

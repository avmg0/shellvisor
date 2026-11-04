//! Drawing: tab bar, bordered panes, status bar and overlays.
//!
//! Colours use the 256-colour palette (not 24-bit) so they look the same in
//! Terminal.app, which has no true colour.

use crate::activity::Activity;
use crate::agents;
use crate::app::{App, MENU, Modal, NewAgentForm, PaneState, Tool, View};
use crate::keys;
use crate::term::TermView;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

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
                Tool::Flow(fp) => fp.render(inner, f.buffer_mut()),
            }
            continue;
        }
        let Some(p) = app.panes.get(&id) else {
            continue;
        };
        if let (true, Some(a)) = (full, p.agent.as_ref()) {
            let mut title = format!(" ● {} · {} ", a.name, a.cli.name());
            if let Some(b) = &a.branch {
                title.push_str(&format!("· {b} "));
            }
            let status = match &a.status {
                agents::Status::Working => format!("{} working", spinner(app)),
                s => s.label(),
            };
            title.push_str(&format!("· {status} "));
            let files = a.files_summary();
            if !files.is_empty() {
                title.push_str(&format!("· {files} "));
            }
            let mins = a.started.elapsed().as_secs() / 60;
            if mins > 0 {
                title.push_str(&format!("· {mins}m "));
            }
            let wants = a.approval().is_some();
            let color = Color::Indexed(a.color);
            let border = Style::default().fg(if wants {
                AMBER
            } else if is_focus {
                color
            } else {
                FAINT
            });
            let block = Block::bordered()
                .border_type(if wants {
                    BorderType::Double
                } else {
                    BorderType::Rounded
                })
                .border_style(border)
                .title(Span::styled(
                    title,
                    Style::default().fg(color).add_modifier(if is_focus {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ));
            let block = match a.approval() {
                Some(ap) => block.title_bottom(Span::styled(
                    format!(
                        " ⚑ {} · alt+a: y allow · n deny · a always ",
                        ap.what.chars().take(50).collect::<String>()
                    ),
                    Style::default().fg(INK).bg(AMBER),
                )),
                None => block,
            };
            f.render_widget(block, outer);
        } else if full {
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

    if let Some(t) = &app.table {
        let (_, content, _) = app.regions();
        f.render_widget(Clear, content);
        let block = pane_block(format!(" ▦ {} ", t.title), true);
        let inner = block.inner(content);
        f.render_widget(block, content);
        t.render(inner, f.buffer_mut());
    }

    if let Some((msg, _)) = &app.notice {
        let (_, content, _) = app.regions();
        badge(f, content, if full { 1 } else { 0 }, &format!(" {msg} "));
    }

    match &app.modal {
        Modal::NewAgent(form) => {
            draw_new_agent(f, app, form);
            return;
        }
        Modal::Approvals(sel) => {
            draw_approvals(f, app, *sel);
            return;
        }
        Modal::Bar(line) => {
            let row = status.unwrap_or(Rect::new(
                0,
                app.screen.height.saturating_sub(1),
                app.screen.width,
                1,
            ));
            f.render_widget(Clear, row);
            let label = " sv ▸ ";
            let hint = if line.is_empty() {
                "   e.g. ps | where cpu > 5 | sort-by cpu -r   ·   ⏎ table · alt+⏎ to shell · ↑ history · esc"
            } else {
                "   ⏎ table · alt+⏎ to shell · esc"
            };
            let line_w = line.chars().count() as u16;
            let spans = Line::from(vec![
                Span::styled(
                    label,
                    Style::default()
                        .bg(AMBER)
                        .fg(INK)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {line}")),
                Span::styled(hint, Style::default().fg(DIM)),
            ]);
            f.render_widget(Paragraph::new(spans).style(Style::default().bg(BAR)), row);
            let x = row.x + label.chars().count() as u16 + 1 + line_w;
            f.set_cursor_position(Position::new(x.min(row.right().saturating_sub(1)), row.y));
            return;
        }
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

    if !matches!(app.modal, Modal::Help) && app.table.is_none() {
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
    let agents_here: Vec<&agents::Agent> = panes.iter().filter_map(|p| p.agent.as_ref()).collect();
    if !agents_here.is_empty() {
        let n = agents_here.len();
        let icon = if agents_here.iter().any(|a| a.approval().is_some()) {
            Some(("⚑".to_string(), AMBER))
        } else if agents_here
            .iter()
            .any(|a| matches!(a.status, agents::Status::Working | agents::Status::Starting))
        {
            Some((spinner(app).to_string(), Color::Indexed(176)))
        } else {
            Some(("◆".to_string(), Color::Indexed(176)))
        };
        return (
            format!("{} ◆ {n} agent{}", i + 1, if n == 1 { "" } else { "s" }),
            icon,
        );
    }
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
    // Agents anywhere: count and approvals.
    let ids = app.agent_ids();
    if !ids.is_empty() {
        let waiting = app.pending_approvals().len();
        left.push(Span::styled(
            format!(
                " ◆ {} agent{} ",
                ids.len(),
                if ids.len() == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Indexed(176)).bg(BAR),
        ));
        if waiting > 0 {
            left.push(Span::styled(
                format!(" ⚑ {waiting} · alt+a "),
                Style::default()
                    .fg(INK)
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD),
            ));
        }
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
    } else if let Some(cmd) = &app.bar_running {
        left.push(Span::styled(
            format!(
                " {} sv {} ",
                spinner(app),
                cmd.chars().take(40).collect::<String>()
            ),
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
    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
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
    // Pane keys in the middle when there's room (from the live keymap).
    let mut hints: Vec<Span> = Vec::new();
    for (action, label) in [
        ("toggle_files", "files"),
        ("toggle_system", "system"),
        ("toggle_flow", "flow"),
        ("explore_table", "table"),
        ("command_bar", "sv bar"),
    ] {
        let k = app.keymap().shown(action);
        if let Some(first) = k.split(" · ").next().filter(|s| !s.is_empty()) {
            hints.push(Span::styled(
                first.to_uppercase().replace("ALT+", "alt+"),
                Style::default().fg(AMBER).bg(BAR),
            ));
            hints.push(Span::styled(
                format!(" {label}  "),
                Style::default().fg(DIM).bg(BAR),
            ));
        }
    }
    let hints_w: usize = hints.iter().map(|s| s.content.chars().count()).sum();
    if !hints.is_empty() && left_w + hints_w + w as usize + 4 <= r.width as usize {
        let x = r.x + left_w as u16 + 2;
        f.render_widget(
            Paragraph::new(Line::from(hints)),
            Rect::new(x, r.y, hints_w as u16, 1),
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
    (
        "explore_table",
        "Data — in a table: s sort · / filter · c CSV · J JSON · m Markdown · ⏎ insert cell",
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

fn help_line(k: &str, v: &str, heading: bool, w: usize) -> Line<'static> {
    let cut = |t: &str, n: usize| -> String {
        if t.chars().count() <= n {
            t.to_string()
        } else {
            let mut x: String = t.chars().take(n.saturating_sub(1)).collect();
            x.push('…');
            x
        }
    };
    if heading {
        // "Panes — in files: ⏎ open …": the title bold, the rest as a dim hint
        let (title, hint) = v.split_once(" — ").unwrap_or((v, ""));
        let title_w = title.chars().count() + 1;
        let mut spans = vec![Span::styled(
            format!(" {title}"),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        )];
        if !hint.is_empty() {
            spans.push(Span::styled(
                format!("  {}", cut(hint, w.saturating_sub(title_w + 3))),
                Style::default().fg(DIM),
            ));
        }
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled(format!("  {k:<18}"), Style::default().fg(AMBER)),
            Span::raw(cut(v, w.saturating_sub(21))),
        ])
    }
}

fn draw_help(f: &mut Frame, app: &App) {
    let rows = help_rows(app);
    let col_w: u16 = 76;
    let fits_one = rows.len() as u16 + 5 <= app.screen.height;
    let two = !fits_one && app.screen.width >= col_w * 2 + 3;
    // split two columns at the section heading nearest the middle
    let split = if two {
        let mid = rows.len() / 2;
        (0..rows.len())
            .filter(|i| rows[*i].2)
            .min_by_key(|i| i.abs_diff(mid))
            .unwrap_or(mid)
    } else {
        rows.len()
    };
    let to_lines = |part: &[(String, String, bool)]| -> Vec<Line<'static>> {
        part.iter()
            .map(|(k, v, h)| help_line(k, v, *h, col_w as usize - 2))
            .collect()
    };
    let left = to_lines(&rows[..split]);
    let right = to_lines(&rows[split..]);
    let body_h = left.len().max(right.len()) as u16;
    let w = if two { col_w * 2 + 3 } else { col_w + 2 };
    let area = centered(app.screen, w, body_h + 4);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            format!(" {} keys ", crate::NAME),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " any key closes · change keys in ~/.config/shellvisor/config.yaml ",
            Style::default().fg(DIM),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let inner = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let clip = |mut lines: Vec<Line<'static>>, h: u16| {
        if lines.len() > h as usize && h > 0 {
            lines.truncate(h as usize - 1);
            lines.push(Line::styled(
                "  … more — make the window taller",
                Style::default().fg(DIM),
            ));
        }
        lines
    };
    if two {
        let lw = col_w;
        f.render_widget(
            Paragraph::new(clip(left, inner.height)),
            Rect::new(inner.x, inner.y, lw, inner.height),
        );
        f.render_widget(
            Paragraph::new(clip(right, inner.height)),
            Rect::new(
                inner.x + lw + 1,
                inner.y,
                inner.width.saturating_sub(lw + 1),
                inner.height,
            ),
        );
    } else {
        let mut all = left;
        all.extend(right);
        f.render_widget(Paragraph::new(clip(all, inner.height)), inner);
    }
}

fn draw_new_agent(f: &mut Frame, app: &App, form: &NewAgentForm) {
    let area = centered(app.screen, 84, 17);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            " New agent ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " tab next field · ←→ choose · ⏎ start · esc cancel ",
            Style::default().fg(DIM),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let label = |i: usize, t: &str| {
        Span::styled(
            format!(" {t:<12}"),
            if form.field == i {
                Style::default()
                    .fg(INK)
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        )
    };
    let mut lines = vec![Line::raw("")];
    let mut clis = vec![label(0, "agent CLI"), Span::raw("  ")];
    for (i, c) in form.clis.iter().enumerate() {
        let on = i == form.cli;
        clis.push(Span::styled(
            format!(" {} ", c.label()),
            if on {
                Style::default()
                    .fg(INK)
                    .bg(CYAN)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            },
        ));
        clis.push(Span::raw(" "));
    }
    lines.push(Line::from(clis));
    lines.push(Line::raw(""));
    let cursor = |i: usize| if form.field == i { "▏" } else { "" };
    lines.push(Line::from(vec![
        label(1, "name"),
        Span::raw(format!("  {}{}", form.name, cursor(1))),
    ]));
    lines.push(Line::raw(""));
    let generic = form.cli() == agents::Cli::Generic;
    let (t, v) = if generic {
        ("command", &form.custom)
    } else {
        ("first task", &form.prompt)
    };
    let room = inner.width.saturating_sub(18) as usize;
    let shown: String = {
        let n = v.chars().count();
        if n > room {
            v.chars().skip(n - room).collect()
        } else {
            v.clone()
        }
    };
    lines.push(Line::from(vec![
        label(2, t),
        Span::raw(format!("  {shown}{}", cursor(2))),
    ]));
    if v.is_empty() {
        lines.push(Line::styled(
            if generic {
                "               e.g. ./my-agent.sh --task tests"
            } else {
                "               optional — you can also type to the agent once it starts"
            },
            Style::default().fg(FAINT),
        ));
    } else {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(vec![
        label(3, "worktree"),
        Span::raw(format!(
            "  [{}] own git worktree on branch agent/{}",
            if form.worktree { "x" } else { " " },
            if form.name.is_empty() {
                "<name>"
            } else {
                &form.name
            }
        )),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "  runs in a new shell in {} · type to it in its column",
            form.folder
        ),
        Style::default().fg(DIM),
    ));
    if let Some(e) = &form.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(format!("  {e}"), Style::default().fg(RED)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_approvals(f: &mut Frame, app: &App, sel: usize) {
    let ids = app.pending_approvals();
    let area = centered(app.screen, 96, ids.len() as u16 * 2 + 5);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            format!(" ⚑ {} waiting for approval ", ids.len()),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " y allow · a always (this kind, this agent) · n deny · ⏎ go to agent · esc ",
            Style::default().fg(DIM),
        ));
    let mut lines = vec![Line::raw("")];
    for (i, id) in ids.iter().enumerate() {
        let Some(a) = app.panes.get(id).and_then(|p| p.agent.as_ref()) else {
            continue;
        };
        let Some(ap) = a.approval() else { continue };
        let on = i == sel.min(ids.len().saturating_sub(1));
        let base = if on {
            Style::default().bg(Color::Indexed(237))
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(if on { " ▸ " } else { "   " }, base.fg(AMBER)),
            Span::styled(
                format!("{:<12}", a.name),
                base.fg(Color::Indexed(a.color))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<9}", ap.action.name()), base.fg(AMBER)),
            Span::styled(
                format!("{:<70}", ap.what.chars().take(70).collect::<String>()),
                base,
            ),
        ]));
        lines.push(Line::styled(
            format!(
                "               {} · waiting {}s",
                a.cli.label(),
                ap.since.elapsed().as_secs()
            ),
            Style::default().fg(FAINT),
        ));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
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

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
const FAINT: Color = Color::Indexed(243);
const BAR: Color = Color::Indexed(236);
const INK: Color = Color::Indexed(16);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);
const TEXT: Color = Color::Indexed(252);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn tick(app: &App) -> usize {
    let every = if app.low_power { 250 } else { 100 };
    (app.epoch.elapsed().as_millis() / every) as usize
}

fn spinner(app: &App) -> &'static str {
    SPINNER[tick(app) % SPINNER.len()]
}

/// The loading spinner for downloads and installs (style from settings).
fn circle(_app: &App) -> &'static str {
    crate::loading::frame()
}

/// A tool's colour on the boards: git amber, pip cyan, docker magenta.
fn tool_fg(label: &str) -> Color {
    match label.split_whitespace().next().unwrap_or("") {
        "pip" | "pip3" | "uv" | "poetry" | "conda" => CYAN,
        "docker" | "podman" => Color::Indexed(170),
        _ => AMBER,
    }
}

/// The boards' thin progress bar: "━━━━╸━━━━" with the filled part in
/// `color` on a dark track. No % yet: a short cyan piece slides along.
pub fn thin_bar(progress: Option<u8>, width: usize, color: Color) -> Vec<Span<'static>> {
    let track = Style::default().fg(crate::palette::role(crate::palette::Role::Track));
    let fill = Style::default().fg(color);
    let mut v = Vec::new();
    match progress {
        Some(p) => {
            let halves = p.min(100) as usize * width * 2 / 100;
            let full = halves / 2;
            let half = halves % 2 == 1;
            v.push(Span::styled("━".repeat(full), fill));
            if half {
                v.push(Span::styled("╸", fill));
            }
            let used = full + usize::from(half);
            v.push(Span::styled("━".repeat(width.saturating_sub(used)), track));
        }
        None => {
            let (a, b) = crate::loading::slide(width);
            v.push(Span::styled("━".repeat(a), track));
            v.push(Span::styled("━".repeat(b - a), Style::default().fg(CYAN)));
            v.push(Span::styled("━".repeat(width - b), track));
        }
    }
    v
}

/// "ETA 38s"
fn eta_text(a: &Activity) -> Option<String> {
    a.eta.map(|e| format!("ETA {}", crate::loading::eta(e)))
}

/// Spans for a download in a pane title: "◜ npm install ━━━━╸━━━ 42% · ETA 6s".
fn activity_spans(app: &App, a: &Activity) -> Vec<Span<'static>> {
    let mut v = vec![
        Span::styled(circle(app).to_string(), Style::default().fg(AMBER)),
        Span::styled(format!(" {}", a.label), Style::default().fg(DIM)),
    ];
    if crate::loading::bar_on() {
        v.push(Span::raw(" "));
        v.extend(thin_bar(a.progress, 10, tool_fg(&a.label)));
    }
    if let Some(p) = a.progress {
        v.push(Span::styled(format!(" {p}%"), Style::default().fg(TEXT)));
    }
    if let Some(e) = eta_text(a) {
        v.push(Span::styled(format!(" · {e}"), Style::default().fg(DIM)));
    }
    v
}

/// A pane frame with a title made of spans (the first in the frame's style).
fn pane_block_line(first: String, rest: Vec<Span<'static>>, is_focus: bool) -> Block<'static> {
    let border = Style::default().fg(if is_focus { AMBER } else { FAINT });
    let head = if is_focus {
        border.add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    let mut spans = vec![Span::styled(first, head)];
    spans.extend(rest);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(spans))
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
    // basic view looks exactly like your plain terminal: no palette there
    let design = !app.light && app.ui.background != "terminal" && app.view == View::Full;
    crate::palette::set_mode(design, app.truecolor);
    draw_all(f, app);
    if !app.light && !design {
        return;
    }
    // shells keep their programs' own colours; only overlays cover them
    let covered = app.board.is_some()
        || app.table.is_some()
        || app.editor.is_some()
        || app.settings.is_some()
        || app.acts_list.is_some()
        || app.mcp.is_some()
        || app.review.is_some()
        || app.fwd.is_some()
        || !matches!(app.modal, Modal::None);
    let mut areas = crate::palette::Areas::default();
    if !covered {
        for (id, outer, inner) in app.visible() {
            if app.tools.contains_key(&id) {
                areas.tool_panes.push(outer);
            } else {
                areas.shells.push(inner);
                areas.shell_panes.push(outer);
            }
        }
    }
    if app.light {
        light_theme(f.buffer_mut(), &areas.shells);
    } else {
        crate::palette::apply(f.buffer_mut(), &areas, app.truecolor);
    }
}

/// Shellvisor's palette, for a light terminal background.
pub fn light_color(c: Color) -> Color {
    match c {
        Color::Indexed(n) => Color::Indexed(match n {
            250..=255 => 235, // light text → dark text
            245..=249 => 241, // dim
            240..=244 => 246, // faint
            235..=239 => 254, // dark fills (bars, selection) → light fills
            179 => 130,       // amber
            73 => 31,         // cyan
            108 => 28,        // green
            167 => 160,       // red
            n => n,
        }),
        c => c,
    }
}

fn light_theme(buf: &mut ratatui::buffer::Buffer, skip: &[Rect]) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if skip
                .iter()
                .any(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
            {
                continue;
            }
            let cell = &mut buf[(x, y)];
            cell.fg = light_color(cell.fg);
            cell.bg = light_color(cell.bg);
        }
    }
}

fn draw_all(f: &mut Frame, app: &App) {
    let (tabbar, _, status) = app.regions();
    let full = app.view == View::Full;
    let focused = app.input_target();
    let zoomed = app.tabs[app.active].zoomed;

    // board 14: basic view says so for 3 s, then gets out of the way
    if !full
        && app
            .basic_since
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3))
    {
        let hint = format!(" basic · sv on or {} ", key_label(app, "toggle_view"));
        let w = hint.chars().count() as u16;
        if app.screen.width > w + 2 {
            f.buffer_mut().set_string(
                app.screen.width - w - 1,
                0,
                &hint,
                Style::default().fg(DIM).bg(Color::Indexed(235)),
            );
        }
    }
    if let Some((orch, bus)) = app.agents_regions(app.regions().1) {
        draw_orchestrator(f, app, orch);
        if let Some(b) = bus {
            draw_bus(f, app, b);
        }
    }
    for (id, outer, inner) in app.visible() {
        let is_focus = id == focused;
        if let Some(t) = app.tools.get(&id) {
            if full {
                // the boards' titles: "F2 · FILES", "F3 · SYSTEM", "F8 · FLOW"
                let (action, name) = match t {
                    Tool::Files(_) => ("toggle_files", "FILES"),
                    Tool::System(sp) if sp.thermal => ("toggle_system", "THERMAL"),
                    Tool::System(_) => ("toggle_system", "SYSTEM"),
                    Tool::Flow(_) => ("toggle_flow", "FLOW"),
                    Tool::Remote(_) => ("", "REMOTE"),
                    Tool::Info(i) => ("info", i.kind.title()),
                    Tool::Svc(sv) => ("info", sv.kind.title()),
                };
                let key = key_label(app, action);
                let mut title = if action == "info" {
                    format!(" {name} ")
                } else if key.is_empty() {
                    format!(" {name} · {} ", t.title())
                } else {
                    format!(" {key} · {name} ")
                };
                if zoomed {
                    title.push_str("· zoomed ");
                }
                f.render_widget(pane_block(title, is_focus), outer);
            }
            match t {
                Tool::Files(fp) => fp.render(inner, f.buffer_mut(), is_focus),
                Tool::System(sp) => sp.render(inner, f.buffer_mut(), app.low_power),
                Tool::Flow(fp) => fp.render(inner, f.buffer_mut()),
                Tool::Remote(r) => {
                    let folder = match crate::settings::color_index(&app.ui.list_folder) {
                        Some(c) => Style::default().fg(Color::Indexed(c)),
                        None => Style::default(),
                    };
                    r.render(inner, f.buffer_mut(), folder, is_focus)
                }
                Tool::Info(i) => draw_info(f, app, id, i, inner, is_focus),
                Tool::Svc(sv) => {
                    let pids = if sv.kind == crate::svc::Kind::Ports {
                        app.shell_pids()
                    } else {
                        Default::default()
                    };
                    sv.render(inner, f.buffer_mut(), is_focus, &pids)
                }
            }
            continue;
        }
        let Some(p) = app.panes.get(&id) else {
            continue;
        };
        if let (true, Some(a)) = (full, p.agent.as_ref()) {
            let rows = inner.y > outer.y + 1;
            let mut title = match &a.parent {
                Some(parent) => format!(" ● {} ← {parent} · {} ", a.name, a.cli.name()),
                None => format!(" ● {} · {} ", a.name, a.cli.name()),
            };
            if let (Some(b), None) = (&a.branch, &a.parent) {
                title.push_str(&format!("· {b} "));
            }
            let status = match &a.status {
                _ if a.paused => "⏸ paused".to_string(),
                agents::Status::Working => format!("{} working", spinner(app)),
                s => s.label(),
            };
            title.push_str(&format!("· {status} "));
            if let Some(act) = &p.agent_act {
                let pct = act.progress.map(|p| format!(" {p}%")).unwrap_or_default();
                title.push_str(&format!("· {} {}{pct} ", circle(app), act.label));
            }
            let files = a.files_summary();
            if !files.is_empty() {
                title.push_str(&format!("· {files} "));
            }
            if let Some(t) = a.tokens {
                title.push_str(&format!("· {} ", agents::fmt_tokens(t)));
            }
            if !a.internal.is_empty() {
                title.push_str(&format!(
                    "· +{} helper{} ",
                    a.internal.len(),
                    if a.internal.len() == 1 { "" } else { "s" }
                ));
            }
            let mins = a.started.elapsed().as_secs() / 60;
            if mins > 0 {
                title.push_str(&format!("· {mins}m "));
            }
            if rows {
                // board 5: the facts are inside the column, the frame stays quiet
                title = String::new();
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
            if rows {
                draw_agent_rows(f, app, a, p, inner, is_focus);
            }
        } else if full {
            let shells_here = app.tabs[app.active]
                .root
                .panes()
                .iter()
                .filter(|i| app.panes.get(i).is_some_and(|q| q.agent.is_none()))
                .count();
            let title = if shells_here > 1 {
                format!(" SHELL · {} · {} ", p.shell, basename(&short_path(&p.cwd)))
            } else {
                format!(" SHELL · {} ", p.shell)
            };
            let mut rest = Vec::new();
            if let Some(a) = &p.activity {
                rest.push(Span::styled("· ", Style::default().fg(DIM)));
                rest.extend(activity_spans(app, a));
                rest.push(Span::raw(" "));
            }
            if zoomed {
                rest.push(Span::styled("· zoomed ", Style::default().fg(DIM)));
            }
            if p.log.is_some() {
                rest.push(Span::styled("· ⏺ logging ", Style::default().fg(RED)));
            }
            if app.broadcast.contains(&id) {
                rest.push(Span::styled(
                    " ⇶ broadcast ",
                    Style::default()
                        .fg(INK)
                        .bg(AMBER)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            let mut block = pane_block_line(title, rest, is_focus);
            if is_focus && outer.width > 50 {
                block = block.title(
                    Line::from(Span::styled(
                        format!(" {}×{} · focused ", inner.width, inner.height),
                        Style::default().fg(DIM),
                    ))
                    .right_aligned(),
                );
            }
            if let Some(h) = stuck_hint(p) {
                block = block.title_bottom(Span::styled(
                    format!(" {h} "),
                    Style::default().fg(INK).bg(AMBER),
                ));
            }
            f.render_widget(block, outer);
        }
        f.render_widget(TermView(p.pane.screen()), inner);
        // find matches: the current one amber, the others tinted
        if let Some(fd) = app.find.as_ref().filter(|f| f.pane == id) {
            let sb = p.pane.screen().scrollback_lines();
            let top = sb - p.pane.scroll();
            let rows = inner.height as usize;
            let buf = f.buffer_mut();
            for (i, &(line, col, len)) in fd.matches.iter().enumerate() {
                if line < top || line >= top + rows {
                    continue;
                }
                let y = inner.y + (line - top) as u16;
                for c in col..(col + len).min(inner.width) {
                    let cell = &mut buf[(inner.x + c, y)];
                    if i == fd.cur {
                        cell.set_bg(AMBER).set_fg(INK);
                    } else {
                        cell.set_bg(Color::Indexed(94));
                    }
                }
            }
        }
        // text selected with the mouse
        if let Some(sel) = app
            .sel
            .filter(|s| s.pane == id && s.scroll == p.pane.scroll())
        {
            let bg = crate::palette::role(crate::palette::Role::Selection);
            let buf = f.buffer_mut();
            for row in 0..inner.height {
                for col in 0..inner.width {
                    if sel.contains(row, col) {
                        buf[(inner.x + col, inner.y + row)].set_bg(bg);
                    }
                }
            }
        }
        if p.pane.scroll() > 0 {
            badge(
                f,
                inner,
                0,
                &format!(" ↑ {} lines · type or scroll to return ", p.pane.scroll()),
            );
        }
    }

    // focus layout's rail: the other panes, one key away (board 3)
    if full && app.tabs[app.active].focus_mode {
        let (_, content, _) = app.regions();
        let area = app.pane_area(content);
        if area.width < content.width {
            let rail = Rect::new(
                area.right(),
                content.y,
                content.right() - area.right(),
                content.height,
            );
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(FAINT));
            let inner = block.inner(rail);
            f.render_widget(block, rail);
            let mut y = inner.y + 1;
            for (action, name) in [
                ("toggle_files", "files"),
                ("toggle_system", "system"),
                ("toggle_flow", "flow"),
            ] {
                let label = format!("{} {name}", key_label(app, action));
                let chars: Vec<char> = label.chars().collect();
                if y + chars.len() as u16 > inner.bottom() {
                    break;
                }
                for (i, c) in chars.iter().enumerate() {
                    f.buffer_mut().set_string(
                        inner.x + inner.width / 2,
                        y + i as u16,
                        c.to_string(),
                        Style::default().fg(DIM),
                    );
                }
                y += chars.len() as u16 + 2;
            }
        }
    }
    if let Some(r) = tabbar {
        if r.height >= 2 {
            draw_header(f, app, Rect::new(r.x, r.y, r.width, 1));
            draw_tabs(f, app, Rect::new(r.x, r.y + 1, r.width, 1));
        } else {
            draw_tabs(f, app, r);
        }
    }
    if let Some(r) = status {
        draw_keys(f, app, r);
    }

    if let Some(t) = &app.table {
        let (_, content, _) = app.regions();
        f.render_widget(Clear, content);
        let block = pane_block(format!(" ▦ {} ", t.title), true);
        let inner = block.inner(content);
        f.render_widget(block, content);
        t.render(inner, f.buffer_mut());
    }

    if let Some(b) = &app.board {
        let (_, content, _) = app.regions();
        f.render_widget(Clear, content);
        let block = pane_block(" ◆ agent board ".to_string(), true);
        let inner = block.inner(content);
        f.render_widget(block, content);
        b.render(inner, f.buffer_mut(), &app.board_snapshot());
    }

    if let Some(sc) = &app.settings {
        let (_, content, _) = app.regions();
        let w = content.width.min(96);
        let h = content.height.min(33);
        let r = Rect::new(
            content.x + (content.width - w) / 2,
            content.y + (content.height - h) / 2,
            w,
            h,
        );
        f.render_widget(Clear, r);
        let block = pane_block(" ⚙ settings ".to_string(), true);
        let inner = block.inner(r);
        f.render_widget(block, r);
        let folder = app
            .focused_cwd()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "~".into());
        sc.render(inner, f.buffer_mut(), &sc.who, &folder);
    }

    if let Some(sel) = app.acts_list {
        draw_activity(f, app, sel);
    }

    if let Some(ed) = &app.editor {
        let (_, content, _) = app.regions();
        f.render_widget(Clear, content);
        let block = pane_block(" ▦ layout editor ".to_string(), true);
        let inner = block.inner(content);
        f.render_widget(block, content);
        ed.render(inner, f.buffer_mut());
    }

    if let Some((msg, _)) = &app.notice {
        let (_, content, _) = app.regions();
        badge(f, content, if full { 1 } else { 0 }, &format!(" {msg} "));
    }
    if let Some((d, _)) = &app.toast_act {
        draw_finish_toast(f, app, d);
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
            // a download just finished: its actions come first
            let offer = app
                .toast_act
                .as_ref()
                .filter(|(d, at)| {
                    at.elapsed() < crate::app::TOAST_ACT && d.ok && d.target.is_some()
                })
                .map(|(d, _)| {
                    let name = d
                        .target
                        .as_ref()
                        .and_then(|t| t.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if d.target.as_ref().is_some_and(|t| t.is_dir()) {
                        format!(" ctrl+] → c cd into {name} · o open {name} in files ·")
                    } else {
                        format!(" ctrl+] → o show {name} in files ·")
                    }
                });
            let offer = offer.or_else(|| {
                app.fail
                    .as_ref()
                    .map(|_| " ctrl+] → c copy the error · r run it again ·".to_string())
            });
            let hint = match offer {
                Some(o) => format!("{o}{}", keys::LEADER_HINT.trim_start_matches(" ctrl+] →")),
                None => keys::LEADER_HINT.to_string(),
            };
            f.render_widget(
                Paragraph::new(hint).style(Style::default().bg(AMBER).fg(INK)),
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
        Modal::CloseTab { tab, detail, sel } => {
            draw_close_tab(f, app, *tab, detail, *sel);
            return;
        }
        Modal::OptionHelp {
            sent,
            testing,
            works,
        } => {
            draw_option_help(f, app, sent, *testing, *works);
            return;
        }
        Modal::Tell(text) => {
            let row = status.unwrap_or(Rect::new(
                0,
                app.screen.height.saturating_sub(1),
                app.screen.width,
                1,
            ));
            f.render_widget(Clear, row);
            let line = Line::from(vec![
                Span::styled(" Tell all agents: ", Style::default().bg(AMBER).fg(INK)),
                Span::raw(format!(" {text}")),
                Span::styled("   ⏎ send · esc cancel", Style::default().fg(DIM)),
            ]);
            f.render_widget(Paragraph::new(line), row);
            let x = row.x + 19 + text.chars().count() as u16;
            f.set_cursor_position(Position::new(x.min(row.right().saturating_sub(1)), row.y));
            return;
        }
        Modal::Restore {
            saved,
            checked,
            sel,
        } => {
            draw_restore(f, app, saved, checked, *sel);
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

    if let Some(m) = &app.ctx {
        draw_context_menu(f, app, m);
    }
    if let Some((sel, list)) = &app.sessions {
        draw_sessions(f, app, *sel, list);
    }
    if let Some((card, _)) = &app.fail {
        if app.toast_act.is_none() {
            draw_fail_card(f, app, card);
        }
    }
    if let Some(i) = app.tour {
        draw_tour(f, app, i);
    }
    if let Some(pal) = &app.palette {
        draw_palette(f, app, pal);
    }
    if let Some(fs) = &app.fwd {
        let area = app.screen;
        f.render_widget(Clear, area);
        fs.render(area, f.buffer_mut(), &app.fwd_snap());
    }
    if let Some(r) = &app.review {
        // the whole window, like board 21
        let area = app.screen;
        f.render_widget(Clear, area);
        r.render(area, f.buffer_mut());
    }
    if let Some(m) = &app.mcp {
        // the whole window, like the design boards (own header and key strip)
        let r = app.screen;
        f.render_widget(Clear, r);
        let waiting = app.mcp_waiting();
        m.render(r, f.buffer_mut(), waiting.as_deref());
    }
    if let Some(fd) = &app.find {
        let row = status.unwrap_or(Rect::new(
            0,
            app.screen.height.saturating_sub(1),
            app.screen.width,
            1,
        ));
        f.render_widget(Clear, row);
        let count = if fd.query.is_empty() {
            "type to search this pane's scrollback".to_string()
        } else if let Some(e) = &fd.error {
            format!("✖ {e}")
        } else if fd.matches.is_empty() {
            "no matches".to_string()
        } else {
            format!("{} of {}", fd.matches.len() - fd.cur, fd.matches.len())
        };
        let line = Line::from(vec![
            Span::styled(
                " find ",
                Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if fd.regex { " .* " } else { "" },
                Style::default()
                    .fg(INK)
                    .bg(CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}{} ", fd.query, if fd.typing { "▏" } else { "" }),
                Style::default().fg(TEXT).bg(BAR),
            ),
            Span::styled(
                format!(" {count} "),
                Style::default()
                    .fg(if fd.error.is_some() { RED } else { AMBER })
                    .bg(BAR),
            ),
            Span::styled(
                if fd.typing {
                    " ⏎ done · ↓↑ older / newer · ctrl+r regex · esc close "
                } else {
                    " n older · N newer · / edit · ctrl+r regex · esc close "
                },
                Style::default().fg(DIM).bg(BAR),
            ),
        ]);
        f.render_widget(Paragraph::new(line).style(Style::default().bg(BAR)), row);
    }
    let overlay = app.ctx.is_some()
        || app.tour.is_some()
        || app.mcp.is_some()
        || app.review.is_some()
        || app.fwd.is_some()
        || app.sessions.is_some()
        || app.palette.is_some()
        || app.table.is_some()
        || app.board.is_some()
        || app.settings.is_some()
        || app.acts_list.is_some()
        || app.editor.is_some();
    if !matches!(app.modal, Modal::Help) && !overlay {
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
    // too long for the screen: cut it rather than not showing it
    let text: String = if text.chars().count() >= area.width as usize {
        let keep = (area.width as usize).saturating_sub(2);
        format!(
            "{}… ",
            text.chars()
                .take(keep.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        text.to_string()
    };
    let text = text.as_str();
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

/// One card of the first-run tour.
fn draw_tour(f: &mut Frame, app: &App, i: usize) {
    use crate::palette::{Role, role};
    let Some((title, text, keys)) = crate::app::TOUR.get(i) else {
        return;
    };
    let (_, content, _) = app.regions();
    let h = text.len() as u16 + 7;
    let w = 72u16.min(content.width.saturating_sub(2));
    if w < 40 || content.height < h {
        return;
    }
    let r = centered(content, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .style(Style::default().bg(role(Role::Card)))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {} of {} ", i + 1, crate::app::TOUR.len()),
                Style::default().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = vec![Line::raw("")];
    for t in text.iter() {
        lines.push(Line::styled(format!("  {t}"), Style::default().fg(TEXT)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  try  ", Style::default().fg(DIM)),
        Span::styled(*keys, Style::default().fg(CYAN)),
    ]));
    let dots: String = (0..crate::app::TOUR.len())
        .map(|j| if j == i { "● " } else { "○ " })
        .collect();
    let last = i + 1 == crate::app::TOUR.len();
    lines.push(Line::from(vec![
        Span::styled(format!("  {dots}  "), Style::default().fg(AMBER)),
        Span::styled(
            if last {
                "any key: start · ← back"
            } else {
                "any key: next · ← back · esc skip"
            },
            Style::default().fg(DIM),
        ),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
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
        } else if panes.iter().any(|p| p.agent_act.is_some()) {
            Some((circle(app).to_string(), AMBER))
        } else if agents_here
            .iter()
            .any(|a| matches!(a.status, agents::Status::Working | agents::Status::Starting))
        {
            Some((spinner(app).to_string(), Color::Indexed(176)))
        } else {
            Some(("◆".to_string(), Color::Indexed(176)))
        };
        if tab.pinned {
            return (format!("{}", i + 1), icon);
        }
        // board 12: "◆ 5 agent · api"
        let label = match agents_here.as_slice() {
            [one] => format!("{} agent · {}", i + 1, one.name),
            _ => format!("{} agents · {n}", i + 1),
        };
        return (label, icon);
    }
    let ids = tab.root.panes();
    let just_done = app
        .done
        .iter()
        .find(|d| d.at.elapsed() < std::time::Duration::from_secs(5) && ids.contains(&d.pane));
    let muted = tab.muted;
    let download = panes
        .iter()
        .find_map(|p| p.activity.as_ref())
        .map(|a| a.progress);
    let icon = if let Some(pct) = download {
        // a download: the loading circle and how far it is
        Some(match pct {
            Some(p) => (format!("{} {p}%", circle(app)), AMBER),
            None => (circle(app).to_string(), AMBER),
        })
    } else if panes.iter().any(|p| waiting_input(p)) {
        Some(("?".to_string(), AMBER))
    } else if let (Some(d), false) = (just_done, muted) {
        Some(if d.ok {
            ("✔".to_string(), GREEN)
        } else {
            ("✖".to_string(), RED)
        })
    } else if panes.iter().any(|p| p.running) {
        Some((spinner(app).to_string(), AMBER))
    } else if let Some(code) = panes.iter().filter_map(|p| p.last_exit).find(|c| *c != 0) {
        Some((format!("✖ {code}"), RED))
    } else if !muted && panes.iter().any(|p| p.bell) {
        Some(("!".to_string(), AMBER))
    } else if !muted && panes.iter().any(|p| p.unseen) {
        Some(("•".to_string(), CYAN))
    } else {
        None
    };
    if tab.pinned {
        // icon-only: the pin, or the state when there is one
        return (format!("{}", i + 1), icon.or(Some(("⚑".to_string(), DIM))));
    }
    let cast = if ids.iter().any(|id| app.broadcast.contains(id)) {
        " ⇶"
    } else {
        ""
    };
    (format!("{} {shell} · {what}{split}{cast}", i + 1), icon)
}

/// Tab index under screen column `x` (for mouse clicks).
/// The tabs on the bar: all but the ones kept in the background.
fn shown_tabs(app: &App) -> Vec<usize> {
    (0..app.tabs.len())
        .filter(|i| !app.tabs[*i].background)
        .collect()
}

pub fn tab_at(app: &App, x: u16) -> Option<usize> {
    let mut col = 0u16;
    let compact = tabs_compact(app, app.screen.width);
    for i in shown_tabs(app) {
        let w = tab_width_in(app, i, compact);
        if x >= col && x < col + w {
            return Some(i);
        }
        col += w + 1;
    }
    None
}

/// What's after the tabs at column `x`: the background chip or "+ new".
pub fn strip_extra_at(app: &App, x: u16) -> Option<&'static str> {
    let compact = tabs_compact(app, app.screen.width);
    let shown = shown_tabs(app);
    let mut col: u16 = shown
        .iter()
        .map(|i| tab_width_in(app, *i, compact) + 1)
        .sum();
    let bg = app.tabs.len() - shown.len();
    if bg > 0 {
        let w = format!(" ◌ {bg} in background ").chars().count() as u16;
        if x >= col && x < col + w {
            return Some("background");
        }
        col += w + 1;
    }
    (x >= col && x < col + 8).then_some("new")
}

fn tab_width(app: &App, i: usize) -> u16 {
    let (label, icon) = tab_label(app, i);
    let icon_w = icon.map(|(s, _)| s.chars().count() + 1).unwrap_or(0);
    (label.chars().count() + icon_w + 2) as u16
}

/// The key a configurable action is on, as the boards write it ("F2", "alt+;", "^Q").
fn key_label(app: &App, action: &str) -> String {
    if action.is_empty() {
        return String::new();
    }
    let k = app.keymap().shown(action);
    let first = k.split(" · ").next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return String::new();
    }
    if let Some(rest) = first.strip_prefix("ctrl+") {
        if rest.chars().count() == 1 {
            return format!("^{}", rest.to_uppercase());
        }
    }
    if first.starts_with('f') && first[1..].chars().all(|c| c.is_ascii_digit()) {
        return first.to_uppercase();
    }
    first
}

/// The header row (boards 1, 3, 12): name, folder, shell and pid on the left;
/// downloads, agents, CPU, memory and the clock on the right.
fn draw_header(f: &mut Frame, app: &App, r: Rect) {
    use crate::palette::{Role, role};
    f.render_widget(Paragraph::new("").style(Style::default().bg(BAR)), r);
    let target = app.input_target();
    let p = app.panes.get(&target).or_else(|| {
        app.paired_shell(app.active)
            .and_then(|id| app.panes.get(&id))
    });
    let mut left = vec![Span::styled(
        format!(" ▍{}", crate::NAME_LOWER),
        Style::default()
            .fg(AMBER)
            .bg(BAR)
            .add_modifier(Modifier::BOLD),
    )];
    let on_agents = app.agents_tab() == Some(app.active);
    if on_agents {
        // board 5: "F9 · AGENTS  Manual Auto-plan  session … · 3 agents"
        let chip = role(Role::TabChip);
        left.push(Span::styled("  ", Style::default().bg(BAR)));
        left.push(Span::styled(
            format!(" {} · AGENTS ", key_label(app, "agents")),
            Style::default().bg(chip).fg(AMBER),
        ));
        left.push(Span::styled("  ", Style::default().bg(BAR)));
        left.push(Span::styled(
            " Manual ",
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
        ));
        left.push(Span::styled(
            " Auto-plan ",
            Style::default().bg(chip).fg(DIM),
        ));
        let n = app.agent_ids().len();
        let session = app
            .team_name()
            .map(|t| format!("  session \"{t}\" ·"))
            .unwrap_or_default();
        left.push(Span::styled(
            format!("{session}  {n} agent{}", if n == 1 { "" } else { "s" }),
            Style::default().fg(DIM).bg(BAR),
        ));
        if app.sync_scroll {
            left.push(Span::styled(
                "  ⇅ sync scroll",
                Style::default().fg(CYAN).bg(BAR),
            ));
        }
    } else if app.tabs[app.active].focus_mode {
        left.push(Span::styled(
            "  layout: focus",
            Style::default().fg(DIM).bg(BAR),
        ));
    } else if let Some(p) = p {
        left.push(Span::styled(
            format!("  {}", short_path(&p.cwd)),
            Style::default().fg(DIM).bg(BAR),
        ));
        let pid = p
            .pane
            .pid()
            .map(|n| format!(" · pid {n}"))
            .unwrap_or_default();
        left.push(Span::styled(
            format!("  {}{pid}", p.shell),
            Style::default().fg(DIM).bg(BAR),
        ));
    }
    // right: a downloads pill, agents, live, CPU, MEM, clock
    let mut right: Vec<Span> = Vec::new();
    let acts = app.activities();
    if !acts.is_empty() {
        let n = acts.len();
        let pcts: Vec<u32> = acts
            .iter()
            .filter_map(|(_, a, _)| a.progress.map(u32::from))
            .collect();
        // pushes go up: "1 upload"; a mix of both: "2 transfers"
        let ups = acts
            .iter()
            .filter(|(_, a, _)| a.label.ends_with(" push"))
            .count();
        let word = if ups == n {
            "upload"
        } else if ups > 0 {
            "transfer"
        } else {
            "download"
        };
        let mut t = format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        if !pcts.is_empty() {
            t.push_str(&format!(
                " · {}%",
                pcts.iter().sum::<u32>() / pcts.len() as u32
            ));
        }
        let rate: f64 = acts
            .iter()
            .filter_map(|(_, a, _)| a.speed.as_deref().and_then(crate::activity::rate_bytes))
            .sum();
        if rate > 0.0 {
            t.push_str(&format!(" · {}/s", crate::sysmon::human(rate as u64)));
        }
        let pill = Style::default().bg(role(Role::Badge)).fg(TEXT);
        right.push(Span::styled(" ", pill));
        right.push(Span::styled(circle(app).to_string(), pill.fg(AMBER)));
        right.push(Span::styled(format!(" {t} "), pill));
        right.push(Span::styled("  ", Style::default().bg(BAR)));
    }
    let ids = app.agent_ids();
    if !ids.is_empty() {
        right.push(Span::styled(
            format!(
                "◆ {} agent{}",
                ids.len(),
                if ids.len() == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Indexed(176)).bg(BAR),
        ));
        right.push(Span::styled("  ", Style::default().bg(BAR)));
        let waiting = app.pending_approvals().len();
        if waiting > 0 {
            right.push(Span::styled(
                format!(" ⚑ {waiting} · alt+a "),
                Style::default()
                    .fg(INK)
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD),
            ));
            right.push(Span::styled("  ", Style::default().bg(BAR)));
        }
    }
    // focus layout: the strip below has CPU and memory (board 3)
    if !app.tabs[app.active].focus_mode {
        if app.low_power {
            right.push(Span::styled(
                "◌ low power",
                Style::default().fg(AMBER).bg(BAR),
            ));
        } else {
            right.push(Span::styled("● live", Style::default().fg(GREEN).bg(BAR)));
        }
        right.push(Span::styled(
            format!("  CPU {:.0}%  MEM {:.0}%  ", app.usage.cpu, app.usage.mem),
            Style::default().fg(DIM).bg(BAR),
        ));
    }
    let clock = if app.low_power {
        crate::osinfo::clock()
    } else {
        crate::osinfo::clock_secs()
    };
    right.push(Span::styled(
        format!("{clock} "),
        Style::default().fg(TEXT).bg(BAR),
    ));
    let lw: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let mut rw: usize = right.iter().map(|s| s.content.chars().count()).sum();
    // narrow: drop the least useful first (live, CPU / MEM, agents), keep
    // downloads and the clock; then the pid, then the folder
    for drop in ["● live", "◌ low power", "  CPU", "◆ "] {
        if rw + lw + 2 <= r.width as usize {
            break;
        }
        if let Some(i) = right.iter().position(|s| s.content.starts_with(drop)) {
            rw -= right.remove(i).content.chars().count();
            if drop == "◆ " && i < right.len() && right[i].content == "  " {
                rw -= right.remove(i).content.chars().count();
            }
        }
    }
    let mut lw = lw;
    while lw + rw + 2 > r.width as usize && left.len() > 1 {
        lw -= left.pop().map(|s| s.content.chars().count()).unwrap_or(0);
    }
    f.render_widget(Paragraph::new(Line::from(left)), r);
    if rw < r.width as usize {
        f.render_widget(
            Paragraph::new(Line::from(right)),
            Rect::new(r.right() - rw as u16, r.y, rw as u16, 1),
        );
    }
}

/// The tab strip (board 12): the active tab amber, the others on chips with
/// their state first (◐ running, ✖ 1 failed, • new output, ◆ agent…).
fn draw_tabs(f: &mut Frame, app: &App, r: Rect) {
    use crate::palette::{Role, role};
    f.render_widget(Paragraph::new("").style(Style::default().bg(BAR)), r);
    let chip = role(Role::TabChip);
    let mut spans = Vec::new();
    let compact = tabs_compact(app, r.width);
    let shown = shown_tabs(app);
    for &i in &shown {
        let (label, icon) = tab_label(app, i);
        let label = if compact { (i + 1).to_string() } else { label };
        let active = i == app.active;
        let base = if active {
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(chip).fg(TEXT)
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
    let bg = app.tabs.len() - shown.len();
    if bg > 0 {
        spans.push(Span::styled(
            format!(" ◌ {bg} in background "),
            Style::default().bg(chip).fg(DIM),
        ));
        spans.push(Span::styled(" ", Style::default().bg(BAR)));
    }
    spans.push(Span::styled(" + new ▾", Style::default().bg(BAR).fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), r);
    let n = app.tabs.len();
    let right = format!(
        "{n} tab{} · {} ",
        if n == 1 { "" } else { "s" },
        crate::osinfo::self_memory()
    );
    let w = right.chars().count() as u16;
    let used: u16 = shown
        .iter()
        .map(|i| tab_width_in(app, *i, compact) + 1)
        .sum::<u16>()
        + 9
        + if bg > 0 { 20 } else { 0 };
    if used + w + 2 <= r.width {
        f.buffer_mut()
            .set_string(r.right() - w, r.y, &right, Style::default().fg(DIM).bg(BAR));
    }
}

/// Many tabs: the strip shows numbers only.
fn tabs_compact(app: &App, width: u16) -> bool {
    let full: u16 = shown_tabs(app)
        .into_iter()
        .map(|i| tab_width_in(app, i, false) + 1)
        .sum::<u16>()
        + 9;
    full + 14 > width
}

fn tab_width_in(app: &App, i: usize, compact: bool) -> u16 {
    if !compact {
        return tab_width(app, i);
    }
    let (_, icon) = tab_label(app, i);
    let icon_w = icon.map(|(s, _)| s.chars().count() + 1).unwrap_or(0);
    ((i + 1).to_string().len() + icon_w + 2) as u16
}

/// The key strip at the bottom: the keys for what has the focus (board 1
/// for shells, board 9 for files, board 2 for flow…).
fn draw_keys(f: &mut Frame, app: &App, r: Rect) {
    f.render_widget(Paragraph::new("").style(Style::default().bg(BAR)), r);
    if app.tabs[app.active].focus_mode {
        draw_focus_strip(f, app, r);
        return;
    }
    let target = app.input_target();
    let k = |a: &str, fallback: &str| {
        let s = key_label(app, a);
        if s.is_empty() {
            fallback.to_string()
        } else {
            s
        }
    };
    let keys: Vec<(String, &str)> = match app.tools.get(&target) {
        Some(Tool::Svc(sv)) => sv
            .kind
            .keys()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        Some(Tool::Info(i)) => match i.kind {
            crate::infopane::InfoKind::Preview => vec![
                ("↑↓".into(), "Scroll"),
                ("PgUp PgDn".into(), "Page"),
                (k("toggle_files", "F2"), "Pick a file"),
            ],
            crate::infopane::InfoKind::Agents => vec![
                ("↑↓".into(), "Agent"),
                ("⏎".into(), "Go to its column"),
                ("d".into(), "Review"),
                (k("agents", "F9"), "Agents tab"),
            ],
            crate::infopane::InfoKind::Activity => vec![
                ("↑↓".into(), "Download"),
                ("⏎ c".into(), "cd into it"),
                ("o".into(), "Open in files"),
            ],
        },
        Some(Tool::Files(_)) => vec![
            ("tab".into(), "Cycle views"),
            ("1-8".into(), "Views"),
            ("i c e".into(), "Insert · cd · edit"),
            ("/".into(), "Filter"),
            (".".into(), "Hidden files"),
            ("p".into(), "Preview on/off"),
            (k("zoom", "F5"), "Zoom"),
        ],
        Some(Tool::Flow(_)) => vec![
            ("s".into(), "Send pipeline to shell"),
            ("r".into(), "Run again with sv flow"),
            ("^C".into(), "Stop (in the shell)"),
            (k("zoom", "F5"), "Zoom"),
            (k("toggle_flow", "F8"), "Flow pane"),
        ],
        Some(Tool::System(_)) => vec![
            ("t".into(), "Thermal"),
            ("s".into(), "Sort CPU / memory"),
            (k("zoom", "F5"), "Zoom"),
            (k("toggle_system", "F3"), "Close"),
        ],
        Some(Tool::Remote(_)) => vec![
            ("⏎".into(), "Open"),
            ("⌫".into(), "Up"),
            ("d".into(), "Download"),
            ("drop files".into(), "Upload"),
        ],
        None if app.panes.get(&target).is_some_and(|p| p.agent.is_some()) => vec![
            (k("agents", "F9"), "Agents ⇄ Workspace"),
            (k("approvals", "alt+a"), "Approvals"),
            (k("zoom", "F5"), "Zoom agent"),
            ("^] D".into(), "Diff view"),
            ("^] T".into(), "Tell all"),
            ("^] Z".into(), "Pause all"),
            ("^] Y".into(), "Sync scroll"),
            (k("new_agent", "^] +"), "Spawn agent"),
            (k("agent_board", "alt+g"), "Plan · tree"),
            (k("quit", "^Q"), "Quit"),
        ],
        None => vec![
            (k("help", "F1"), "Help"),
            (k("toggle_files", "F2"), "Files"),
            (k("toggle_system", "F3"), "System"),
            (k("explore_table", "F4"), "Table"),
            (k("zoom", "F5"), "Zoom"),
            (k("next_pane", "F6"), "Next pane"),
            (k("next_layout", "F7"), "Rearrange"),
            (k("toggle_flow", "F8"), "Flow"),
            (k("command_bar", "alt+;"), "Command bar"),
            (k("quit", "^Q"), "Quit"),
        ],
    };
    let mut spans = vec![Span::styled(" ", Style::default().bg(BAR))];
    let mut w = 1usize;
    for (key, label) in &keys {
        let add = key.chars().count() + label.chars().count() + 4;
        if w + add > r.width as usize {
            break;
        }
        spans.push(Span::styled(
            key.clone(),
            Style::default().fg(AMBER).bg(BAR),
        ));
        spans.push(Span::styled(
            format!(" {label}   "),
            Style::default().fg(DIM).bg(BAR),
        ));
        w += add;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}

/// Focus layout's status strip (board 3): the folder, branch, the job, CPU,
/// memory and network, with F7 / F1 on the right.
fn draw_focus_strip(f: &mut Frame, app: &App, r: Rect) {
    let target = app.input_target();
    let p = app.panes.get(&target).or_else(|| {
        app.paired_shell(app.active)
            .and_then(|id| app.panes.get(&id))
    });
    let sep = Span::styled("│", Style::default().fg(Color::Indexed(240)).bg(BAR));
    let mut spans = vec![Span::styled(
        " SHELL ",
        Style::default()
            .fg(INK)
            .bg(AMBER)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(p) = p {
        spans.push(Span::styled(
            format!("  {}  ", short_path(&p.cwd)),
            Style::default().fg(CYAN).bg(BAR),
        ));
        spans.push(sep.clone());
        if let Some((branch, changed)) = app.branch_of(&p.cwd) {
            spans.push(Span::styled(
                format!("   {branch}"),
                Style::default().fg(Color::Indexed(176)).bg(BAR),
            ));
            if changed > 0 {
                spans.push(Span::styled(
                    format!(" +{changed}"),
                    Style::default().fg(AMBER).bg(BAR),
                ));
            }
            spans.push(Span::styled("  ", Style::default().bg(BAR)));
            spans.push(sep.clone());
        }
        let job = if let Some((_, a, _)) = app.activities().first() {
            let pct = a.progress.map(|x| format!(" {x}%")).unwrap_or_default();
            Some((format!("{} {}{pct}", circle(app), a.label), AMBER))
        } else if p.running {
            let secs = p.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
            // the command that is running: the last of a `a; b` / `a && b` chain
            let last = running_part(&p.command);
            let cmd: String = if last.chars().count() > 24 {
                last.chars().take(23).chain(['…']).collect()
            } else {
                last.to_string()
            };
            Some((format!("{cmd} ▶ {}", fmt_secs(secs)), GREEN))
        } else {
            None
        };
        if let Some((j, c)) = job {
            spans.push(Span::styled("  job: ", Style::default().fg(TEXT).bg(BAR)));
            spans.push(Span::styled(
                format!("{j}  "),
                Style::default().fg(c).bg(BAR),
            ));
            spans.push(sep.clone());
        }
    }
    spans.push(Span::styled(
        format!("  CPU {:.0}%  ", app.usage.cpu),
        Style::default().fg(TEXT).bg(BAR),
    ));
    spans.push(sep.clone());
    spans.push(Span::styled(
        format!("  MEM {:.0}%  ", app.usage.mem),
        Style::default().fg(TEXT).bg(BAR),
    ));
    spans.push(sep);
    f.render_widget(Paragraph::new(Line::from(spans)), r);
    let hint = format!(
        "{} layout · {} help ",
        key_label(app, "next_layout"),
        key_label(app, "help")
    );
    let w = hint.chars().count() as u16;
    if r.width > w + 60 {
        f.buffer_mut()
            .set_string(r.right() - w, r.y, &hint, Style::default().fg(DIM).bg(BAR));
    }
}

/// The part of `a; b && c` that is running now: the last one.
pub fn running_part(cmd: &str) -> &str {
    cmd.rsplit([';', '&'])
        .map(str::trim)
        .find(|c| !c.is_empty())
        .unwrap_or("")
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
    // board 8: CLI cards, two columns of fields, the first instruction,
    // approvals and limits, a detected CLI to attach, Cancel / Start agent
    let h = 33u16;
    let w = 100u16.min(app.screen.width.saturating_sub(2));
    if app.screen.height < h + 2 || w < 70 {
        return draw_new_agent_compact(f, app, form);
    }
    let area = centered(app.screen, w, h);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER));
    let r = block.inner(area);
    f.render_widget(block, area);
    let buf = f.buffer_mut();
    let x0 = r.x + 2;
    let iw = r.width.saturating_sub(4);
    let half = iw / 2;
    let d = Style::default().fg(DIM);
    let t = Style::default().fg(TEXT);
    let on = |i: usize| form.field == i;
    let lab = |i: usize| {
        if on(i) {
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
        } else {
            d
        }
    };
    let fieldbg = Style::default().bg(Color::Indexed(235)).fg(TEXT);
    let mut y = r.y + 1;
    buf.set_string(x0, y, "New agent", t.add_modifier(Modifier::BOLD));
    let k = format!("{} · tab next field", key_label(app, "new_agent"));
    buf.set_string(x0 + iw - k.chars().count() as u16, y, &k, d);
    y += 2;
    buf.set_string(x0, y, "agent CLI", lab(0));
    y += 1;
    // the cards
    let n = form.clis.len() as u16;
    let gap = 2u16;
    let cw = (iw + gap) / n - gap;
    for (i, c) in form.clis.iter().enumerate() {
        let cx = x0 + i as u16 * (cw + gap);
        let picked = i == form.cli;
        let border = if picked && on(0) {
            Style::default().fg(AMBER)
        } else if picked {
            Style::default().fg(DIM)
        } else {
            Style::default().fg(Color::Indexed(238))
        };
        let wv = cw as usize;
        buf.set_stringn(cx, y, format!("╭{}╮", "─".repeat(wv - 2)), wv, border);
        buf.set_stringn(cx, y + 3, format!("╰{}╯", "─".repeat(wv - 2)), wv, border);
        for dy in 1..3 {
            buf.set_string(cx, y + dy, "│", border);
            buf.set_string(cx + cw - 1, y + dy, "│", border);
        }
        if picked {
            buf.set_style(
                Rect::new(cx + 1, y + 1, cw - 2, 2),
                Style::default().bg(Color::Indexed(236)),
            );
        }
        let ok = form.installed.get(i).copied().unwrap_or(true);
        let name_st = if ok {
            t.add_modifier(Modifier::BOLD)
        } else {
            d
        };
        let sub = if *c == agents::Cli::Generic {
            "any command".to_string()
        } else if ok {
            c.name().to_string()
        } else {
            "not installed".to_string()
        };
        buf.set_stringn(cx + 2, y + 1, c.label(), wv - 3, name_st);
        buf.set_stringn(
            cx + 2,
            y + 2,
            &sub,
            wv - 3,
            if ok { d } else { Style::default().fg(FAINT) },
        );
    }
    y += 5;
    // name / folder
    let cursor = |i: usize| if on(i) { "▏" } else { "" };
    let boxed =
        |buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, w: u16, text: &str, focus: bool| {
            buf.set_style(Rect::new(x, y, w, 1), fieldbg);
            let shown: String = {
                let c = text.chars().count();
                let room = w.saturating_sub(3) as usize;
                if c > room {
                    text.chars().skip(c - room).collect()
                } else {
                    text.to_string()
                }
            };
            buf.set_stringn(
                x + 1,
                y,
                format!("{shown}{}", if focus { "▏" } else { "" }),
                (w - 1) as usize,
                fieldbg,
            );
            if focus {
                buf.set_style(Rect::new(x, y, 1, 1), Style::default().bg(AMBER));
            }
        };
    buf.set_string(x0, y, "name / role", lab(1));
    buf.set_string(x0 + half + 1, y, "folder", lab(2));
    y += 1;
    boxed(buf, x0, y, half - 1, &form.name, on(1));
    boxed(buf, x0 + half + 1, y, iw - half - 1, &form.folder, on(2));
    let _ = cursor;
    y += 2;
    // isolation / runs in
    let radio = |b: bool| if b { "◉ " } else { "○ " };
    buf.set_string(x0, y, "isolation", lab(3));
    buf.set_string(x0 + half + 1, y, "runs in", lab(4));
    y += 1;
    let branch = if form.name.is_empty() {
        "<name>".to_string()
    } else {
        form.name.clone()
    };
    let (rx, _) = buf.set_stringn(
        x0,
        y,
        radio(form.worktree),
        2,
        Style::default().fg(if form.worktree { AMBER } else { DIM }),
    );
    let (rx, _) = buf.set_stringn(rx, y, "own git worktree on branch ", half as usize, t);
    buf.set_stringn(
        rx,
        y,
        format!("agent/{branch}"),
        half as usize,
        Style::default().fg(CYAN),
    );
    let mut sx = x0 + half + 1;
    for (i, s) in form.shells.iter().enumerate() {
        let pick = i == form.shell;
        let (nx, _) = buf.set_stringn(
            sx,
            y,
            radio(pick),
            2,
            Style::default().fg(if pick { AMBER } else { DIM }),
        );
        let (nx, _) = buf.set_stringn(nx, y, format!("{s}  "), 10, if pick { t } else { d });
        sx = nx;
    }
    y += 1;
    buf.set_stringn(
        x0,
        y,
        radio(!form.worktree),
        2,
        Style::default().fg(if form.worktree { DIM } else { AMBER }),
    );
    buf.set_stringn(
        x0 + 2,
        y,
        "share the folder (read-only agents)",
        half as usize,
        if form.worktree { d } else { t },
    );
    buf.set_stringn(
        x0 + half + 1,
        y,
        format!("new shell in the agents tab, {}", key_label(app, "agents")),
        (iw - half - 1) as usize,
        d,
    );
    y += 2;
    // first instruction (or the command)
    let generic = form.cli() == agents::Cli::Generic;
    buf.set_string(
        x0,
        y,
        if generic {
            "command"
        } else {
            "first instruction"
        },
        lab(5),
    );
    y += 1;
    let text = if generic { &form.custom } else { &form.prompt };
    buf.set_style(Rect::new(x0, y, iw, 3), fieldbg);
    if on(5) {
        buf.set_style(Rect::new(x0, y, 1, 3), Style::default().bg(AMBER));
    }
    let lines = crate::board::wrap_text(text, (iw - 3) as usize);
    let tail = lines.len().saturating_sub(3);
    for (k, l) in lines.iter().skip(tail).enumerate() {
        buf.set_stringn(x0 + 1, y + k as u16, l, (iw - 2) as usize, fieldbg);
    }
    if text.is_empty() {
        let hint = if generic {
            "e.g. ./my-agent.sh --task tests"
        } else {
            "optional — you can also type to the agent once it starts"
        };
        buf.set_stringn(
            x0 + 1,
            y,
            hint,
            (iw - 2) as usize,
            Style::default().bg(Color::Indexed(235)).fg(FAINT),
        );
    }
    if on(5) {
        let last = lines.last().map(|l| l.chars().count()).unwrap_or(0) as u16;
        let ly = y + (lines.len().saturating_sub(tail + 1)) as u16;
        buf.set_string((x0 + 1 + last).min(x0 + iw - 1), ly, "▏", fieldbg.fg(AMBER));
    }
    y += 4;
    // approvals / limits
    buf.set_string(x0, y, "approvals", lab(6));
    buf.set_string(x0 + half + 1, y, "limits", lab(7));
    y += 1;
    buf.set_stringn(
        x0,
        y,
        radio(!form.full_auto),
        2,
        Style::default().fg(if form.full_auto { DIM } else { AMBER }),
    );
    buf.set_stringn(
        x0 + 2,
        y,
        "ask for installs, network, push",
        half as usize,
        if form.full_auto { d } else { t },
    );
    let (lx, _) = buf.set_stringn(x0 + half + 1, y, "stop after ", 12, t);
    boxed(buf, lx, y, 8, &form.stop_after, on(7));
    buf.set_stringn(
        lx + 9,
        y,
        if form.stop_after.trim().is_empty() {
            "min · empty: no limit"
        } else {
            "min, then pause"
        },
        24,
        d,
    );
    y += 1;
    buf.set_stringn(
        x0,
        y,
        radio(form.full_auto),
        2,
        Style::default().fg(if form.full_auto { AMBER } else { DIM }),
    );
    buf.set_stringn(
        x0 + 2,
        y,
        "full auto (it answers yes itself)",
        half as usize,
        if form.full_auto { t } else { d },
    );
    buf.set_stringn(
        x0 + half + 1,
        y,
        "tokens show in its column as it works",
        (iw - half - 1) as usize,
        d,
    );
    y += 2;
    // a ready-made team instead of one agent (sv agent templates)
    buf.set_string(x0, y, "team", lab(8));
    let (tname, tabout) = match form.team() {
        Some((n, a)) => (format!("◂ {n} ▸"), a.clone()),
        None => (
            "◂ just this agent ▸".to_string(),
            "or a ready-made team: the instruction becomes its goal".into(),
        ),
    };
    let (ax, _) = buf.set_stringn(
        x0 + 12,
        y,
        &tname,
        24,
        if on(8) {
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD)
        } else {
            t
        },
    );
    buf.set_stringn(
        ax + 2,
        y,
        &tabout,
        (x0 + iw).saturating_sub(ax + 2) as usize,
        d,
    );
    y += 2;
    // detected CLI → attach
    if let Some((_, what)) = &form.detected {
        let bst = Style::default().fg(Color::Indexed(238));
        buf.set_stringn(
            x0,
            y,
            format!("╭{}╮", "─".repeat(iw as usize - 2)),
            iw as usize,
            bst,
        );
        buf.set_stringn(
            x0,
            y + 2,
            format!("╰{}╯", "─".repeat(iw as usize - 2)),
            iw as usize,
            bst,
        );
        buf.set_string(x0, y + 1, "│", bst);
        buf.set_string(x0 + iw - 1, y + 1, "│", bst);
        let (nx, _) = buf.set_stringn(x0 + 2, y + 1, "◆ Detected ", 12, Style::default().fg(AMBER));
        buf.set_stringn(nx, y + 1, what, (iw - 30) as usize, t);
        let btn = " Attach as column ";
        let st = if on(9) {
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(AMBER)
        };
        buf.set_string(x0 + iw - 2 - btn.chars().count() as u16, y + 1, btn, st);
    }
    // buttons
    let by = r.bottom() - 2;
    let start = if form.team().is_some() {
        " Start team ⏎ "
    } else {
        " Start agent ⏎ "
    };
    let cancel = " esc Cancel ";
    let sx = x0 + iw - start.chars().count() as u16;
    buf.set_string(
        sx,
        by,
        start,
        Style::default()
            .bg(AMBER)
            .fg(INK)
            .add_modifier(Modifier::BOLD),
    );
    buf.set_string(
        sx - 2 - cancel.chars().count() as u16,
        by,
        cancel,
        Style::default().bg(Color::Indexed(236)).fg(TEXT),
    );
    if let Some(e) = &form.error {
        buf.set_stringn(
            x0,
            by,
            e,
            (iw as usize).saturating_sub(32),
            Style::default().fg(RED),
        );
    } else {
        buf.set_stringn(
            x0,
            by,
            "↑↓ tab field · ←→ choose · space toggle",
            44,
            Style::default().fg(FAINT),
        );
    }
}

/// The New agent form on a small screen: one field per line.
fn draw_new_agent_compact(f: &mut Frame, app: &App, form: &NewAgentForm) {
    let area = centered(
        app.screen,
        84.min(app.screen.width),
        20.min(app.screen.height),
    );
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            " New agent ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            if form.team().is_some() {
                " tab next field · ←→ choose · ⏎ Start team · esc cancel "
            } else {
                " tab next field · ←→ choose · ⏎ start · esc cancel "
            },
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
    let cursor = |i: usize| if form.field == i { "▏" } else { "" };
    let mut clis = vec![label(0, "agent CLI"), Span::raw("  ")];
    for (i, c) in form.clis.iter().enumerate() {
        let ok = form.installed.get(i).copied().unwrap_or(true);
        let st = if i == form.cli {
            Style::default()
                .fg(INK)
                .bg(CYAN)
                .add_modifier(Modifier::BOLD)
        } else if ok {
            Style::default().fg(TEXT)
        } else {
            Style::default().fg(FAINT)
        };
        clis.push(Span::styled(format!(" {} ", c.label()), st));
        clis.push(Span::raw(" "));
    }
    let generic = form.cli() == agents::Cli::Generic;
    let (tl, tv) = if generic {
        ("command", &form.custom)
    } else {
        ("first task", &form.prompt)
    };
    let room = inner.width.saturating_sub(18) as usize;
    let tail = |v: &str| -> String {
        let n = v.chars().count();
        if n > room {
            v.chars().skip(n - room).collect()
        } else {
            v.to_string()
        }
    };
    let mut lines = vec![
        Line::from(clis),
        Line::from(vec![
            label(1, "name"),
            Span::raw(format!("  {}{}", form.name, cursor(1))),
        ]),
        Line::from(vec![
            label(2, "folder"),
            Span::raw(format!("  {}{}", tail(&form.folder), cursor(2))),
        ]),
        Line::from(vec![
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
        ]),
        Line::from(vec![
            label(4, "runs in"),
            Span::raw(format!(
                "  {}",
                form.shells.get(form.shell).cloned().unwrap_or_default()
            )),
        ]),
        Line::from(vec![
            label(5, tl),
            Span::raw(format!("  {}{}", tail(tv), cursor(5))),
        ]),
    ];
    if tv.is_empty() {
        lines.push(Line::styled(
            if generic {
                "               e.g. ./my-agent.sh --task tests"
            } else {
                "               optional — you can also type to the agent once it starts"
            },
            Style::default().fg(FAINT),
        ));
    }
    lines.push(Line::from(vec![
        label(6, "approvals"),
        Span::raw(if form.full_auto {
            "  full auto"
        } else {
            "  ask for installs, network, push"
        }),
    ]));
    lines.push(Line::from(vec![
        label(7, "stop after"),
        Span::raw(format!(
            "  {}{} min",
            if form.stop_after.is_empty() {
                "—"
            } else {
                &form.stop_after
            },
            cursor(7)
        )),
    ]));
    lines.push(Line::from(vec![
        label(8, "team"),
        Span::raw(match form.team() {
            Some((n, a)) => format!("  ◂ {n} ▸ {a}"),
            None => "  ◂ just this agent ▸".to_string(),
        }),
    ]));
    if let Some((_, what)) = &form.detected {
        lines.push(Line::from(vec![
            label(9, "attach"),
            Span::raw(format!("  ⏎ {what}")),
        ]));
    }
    lines.push(Line::styled(
        format!(
            "  runs in a new shell in {} · type to it in its column",
            form.folder
        ),
        Style::default().fg(DIM),
    ));
    if let Some(e) = &form.error {
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
    use crate::app::MENU_COLS;
    // board 12: "Tab menu · alt+m or right-click", the tab's nine in a grid,
    // everything else below them
    let cell = 26usize;
    let w = ((cell * MENU_COLS) as u16 + 4).min(app.screen.width);
    let rows = MENU.len().div_ceil(MENU_COLS) as u16;
    let area = centered(app.screen, w, rows + 6);
    f.render_widget(Clear, area);
    let keys_for = |c: crate::keys::Cmd| -> String {
        crate::keys::ACTIONS
            .iter()
            .find(|a| a.1 == c)
            .map(|a| key_label(app, a.0))
            .unwrap_or_default()
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(" Tab menu", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" · {} or right-click", key_label(app, "tab_menu")),
            Style::default().fg(DIM),
        ),
    ])];
    lines.push(Line::raw(""));
    for r in 0..rows as usize {
        if r * MENU_COLS == 9 {
            lines.push(Line::styled(
                format!(" {}", "─".repeat(cell * MENU_COLS - 2)),
                Style::default().fg(Color::Indexed(238)),
            ));
        }
        let mut spans = vec![Span::raw(" ")];
        for c in 0..MENU_COLS {
            let i = r * MENU_COLS + c;
            let Some((label, cmd)) = MENU.get(i) else {
                break;
            };
            let keys = keys_for(*cmd);
            let num = if i < 9 {
                format!("{} ", i + 1)
            } else {
                "  ".into()
            };
            let close = *cmd == crate::keys::Cmd::CloseTab;
            let mut st = if close {
                Style::default().fg(RED)
            } else {
                Style::default().fg(TEXT)
            };
            let mut kst = Style::default().fg(DIM);
            if i == sel {
                st = Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD);
                kst = st;
            }
            let room = cell - 1;
            let body = format!("{num}{label}");
            let kw = keys.chars().count();
            let pad = room.saturating_sub(body.chars().count() + kw + 1);
            if body.chars().count() + kw + 1 > room {
                spans.push(Span::styled(
                    format!("{:<room$}", truncate_str(&body, room)),
                    st,
                ));
            } else {
                spans.push(Span::styled(body, st));
                spans.push(Span::styled(format!(" {keys}{}", " ".repeat(pad)), kst));
            }
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            format!(" tab {} ", app.active + 1),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " ←↑↓→ ⏎ · 1-9 · esc ",
            Style::default().fg(DIM),
        ));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Board 5: an agent column's facts above its terminal (name and state;
/// CLI, worktree, time and tokens; task) and its changes below it.
fn draw_agent_rows(
    f: &mut Frame,
    app: &App,
    a: &agents::Agent,
    p: &PaneState,
    inner: Rect,
    focus: bool,
) {
    let buf = f.buffer_mut();
    let w = inner.width as usize;
    let x = inner.x;
    let color = Color::Indexed(a.color);
    let line = |buf: &mut ratatui::buffer::Buffer,
                y: u16,
                l: Vec<(String, Style)>,
                r: Vec<(String, Style)>| {
        let rw: usize = r.iter().map(|(t, _)| t.chars().count()).sum();
        let mut cx = x;
        let lim = x + w.saturating_sub(rw + 1) as u16;
        for (t, st) in l {
            if cx >= lim {
                break;
            }
            let (nx, _) = buf.set_stringn(cx, y, &t, (lim - cx) as usize, st);
            cx = nx;
        }
        let mut rx = x + w.saturating_sub(rw) as u16;
        for (t, st) in r {
            let (nx, _) = buf.set_stringn(rx, y, &t, rw, st);
            rx = nx;
        }
    };
    let (state, sc) = match &a.status {
        _ if a.paused => ("⏸ paused".to_string(), DIM),
        agents::Status::Working => (format!("{} working", spinner(app)), AMBER),
        agents::Status::NeedsApproval(_) => ("⚑ approval".to_string(), AMBER),
        agents::Status::Idle if a.worked => ("✔ finished".to_string(), GREEN),
        agents::Status::Idle => ("● idle".to_string(), DIM),
        agents::Status::Starting => (format!("{} starting", spinner(app)), DIM),
        agents::Status::Exited(Some(0)) | agents::Status::Exited(None) => {
            ("✔ done".to_string(), GREEN)
        }
        agents::Status::Exited(Some(c)) => (format!("✖ exit {c}"), RED),
    };
    let d = Style::default().fg(DIM);
    let mut st = Style::default().fg(color);
    if focus {
        st = st.add_modifier(Modifier::BOLD);
    }
    let name = match &a.parent {
        Some(par) => format!("● {} ← {par}", a.name),
        None => format!("● {}", a.name),
    };
    line(
        buf,
        inner.y - 3,
        vec![(name, st)],
        vec![(state, Style::default().fg(sc))],
    );
    let place = match (&a.worktree, &a.branch) {
        (Some(_), Some(b)) => format!("{} · worktree {b}", a.cli.label().to_lowercase()),
        _ => format!("{} · shared folder", a.cli.label().to_lowercase()),
    };
    let secs = a.started.elapsed().as_secs();
    let mut facts = fmt_secs(secs);
    if let Some(t) = a.tokens {
        facts.push_str(&format!(
            " · {} tok",
            agents::fmt_tokens(t).trim_end_matches(" tok")
        ));
    }
    line(buf, inner.y - 2, vec![(place, d)], vec![(facts, d)]);
    match a.approval() {
        Some(ap) => line(
            buf,
            inner.y - 1,
            vec![
                ("⚑ ".into(), Style::default().fg(AMBER)),
                (ap.what.clone(), Style::default().fg(AMBER)),
            ],
            vec![],
        ),
        None => {
            let task = if a.task.is_empty() {
                "—".to_string()
            } else {
                a.task.clone()
            };
            line(
                buf,
                inner.y - 1,
                vec![("task ".into(), d), (task, Style::default().fg(TEXT))],
                vec![],
            )
        }
    }
    let files = a.files_summary();
    let files = if files.is_empty() {
        "no changes yet".to_string()
    } else {
        files
    };
    let act = p
        .agent_act
        .as_ref()
        .map(|x| format!("  {} {}", circle(app), x.label))
        .unwrap_or_default();
    line(
        buf,
        inner.bottom(),
        vec![(files, d), (act, Style::default().fg(AMBER))],
        vec![("type to message".into(), Style::default().fg(FAINT))],
    );
}

/// Board 5's left column: the goal, the task graph, what needs you
/// (approvals, files two agents touched), and the all-agents controls.
fn draw_orchestrator(f: &mut Frame, app: &App, r: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Indexed(238)))
        .title(Span::styled(
            " ORCHESTRATOR ",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width as usize;
    let d = Style::default().fg(DIM);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled("goal", d));
    let goal = app
        .team_goal()
        .unwrap_or_else(|| "none set · alt+g plans one from a goal".into());
    for l in crate::board::wrap_text(&goal, w).into_iter().take(3) {
        lines.push(Line::styled(l, Style::default().fg(TEXT)));
    }
    lines.push(Line::styled(
        "─".repeat(w),
        Style::default().fg(Color::Indexed(238)),
    ));
    lines.push(Line::styled("task graph", d));
    let ids = app.agent_ids();
    for (k, id) in ids.iter().enumerate() {
        let Some(a) = app.panes.get(id).and_then(|p| p.agent.as_ref()) else {
            continue;
        };
        let (icon, c) = match &a.status {
            _ if a.paused => ("⏸", DIM),
            agents::Status::NeedsApproval(_) => ("◆", AMBER),
            agents::Status::Working | agents::Status::Starting => (spinner(app), AMBER),
            agents::Status::Idle if a.worked => ("✔", GREEN),
            agents::Status::Exited(Some(0)) | agents::Status::Exited(None) => ("✔", GREEN),
            agents::Status::Exited(_) => ("✖", RED),
            agents::Status::Idle => ("○", DIM),
        };
        let indent = "  ".repeat(a.depth.saturating_sub(1) as usize);
        let task = if a.task.is_empty() {
            "(no task)".to_string()
        } else {
            a.task.clone()
        };
        let head = format!("{indent}{icon} {} ", k + 1);
        let room = w.saturating_sub(head.chars().count() + a.name.chars().count() + 1);
        let task: String = if task.chars().count() > room {
            task.chars()
                .take(room.saturating_sub(1))
                .chain(['…'])
                .collect()
        } else {
            task
        };
        lines.push(Line::from(vec![
            Span::styled(head, Style::default().fg(c)),
            Span::styled(format!("{task} "), Style::default().fg(TEXT)),
            Span::styled(a.name.clone(), Style::default().fg(Color::Indexed(a.color))),
        ]));
    }
    // what needs you
    let pending = app.pending_approvals();
    let first = pending
        .first()
        .and_then(|id| app.panes.get(id))
        .and_then(|p| p.agent.as_ref())
        .and_then(|a| a.approval().map(|ap| (a.name.clone(), ap)));
    if let Some((who, ap)) = first {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "⚑ Approval needed",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(vec![
            Span::styled(who, Style::default().fg(CYAN)),
            Span::styled(" wants to run:", Style::default().fg(TEXT)),
        ]));
        let what: String = ap.what.chars().take(w.saturating_sub(2)).collect();
        lines.push(Line::styled(
            format!(" {what}"),
            Style::default().fg(TEXT).bg(Color::Indexed(235)),
        ));
        lines.push(Line::from(vec![
            Span::styled(" y allow ", Style::default().bg(GREEN).fg(INK)),
            Span::raw(" "),
            Span::styled(
                " n deny ",
                Style::default().bg(Color::Indexed(237)).fg(TEXT),
            ),
            Span::raw(" "),
            Span::styled(
                " a always ",
                Style::default().bg(Color::Indexed(237)).fg(DIM),
            ),
        ]));
        lines.push(Line::styled(
            format!("{} answers", key_label(app, "approvals")),
            Style::default().fg(FAINT),
        ));
    }
    let overlap = app.agent_overlap();
    if let Some((file, who)) = overlap.first() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "⚠ File overlap",
            Style::default().fg(RED).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::styled(
            format!("{} both touched", who.join(" and ")),
            Style::default().fg(TEXT),
        ));
        lines.push(Line::styled(file.clone(), d));
    }
    let top = lines.len() as u16;
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(4),
        ),
    );
    // bottom: broadcast and the all-agents keys
    if inner.height >= top + 5 {
        let y = inner.bottom() - 4;
        let buf = f.buffer_mut();
        buf.set_stringn(inner.x, y, "broadcast to all agents", w, d);
        let boxed = format!(" {:<width$}", "^] T then type", width = w.saturating_sub(2));
        buf.set_stringn(
            inner.x,
            y + 1,
            &boxed,
            w,
            Style::default().fg(FAINT).bg(Color::Indexed(235)),
        );
        let (x, _) = buf.set_stringn(inner.x, y + 3, "^]Z", w, Style::default().fg(AMBER));
        let (x, _) = buf.set_stringn(x, y + 3, " pause all  ", w, d);
        let (x, _) = buf.set_stringn(x, y + 3, "^]U", w, Style::default().fg(AMBER));
        buf.set_stringn(x, y + 3, " resume", w, d);
    }
}

/// Board 5's MESSAGE BUS: who handed what to whom, newest last.
fn draw_bus(f: &mut Frame, app: &App, r: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Indexed(238)))
        .title(Span::styled(
            " MESSAGE BUS · handoffs ",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let n = inner.height as usize;
    let rows: Vec<&(i64, String)> = app.bus_log.iter().rev().take(n).collect();
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                "nothing yet · agents' messages, spawns and approvals show here (sv agent send)",
                Style::default().fg(DIM),
            )),
            inner,
        );
        return;
    }
    let colors: std::collections::HashMap<String, u8> = app
        .agent_ids()
        .iter()
        .filter_map(|id| app.panes.get(id).and_then(|p| p.agent.as_ref()))
        .map(|a| (a.name.clone(), a.color))
        .collect();
    let lines: Vec<Line> = rows
        .into_iter()
        .rev()
        .map(|(t, e)| {
            let mut spans = vec![Span::styled(
                format!("{} ", crate::mcpscreen::hms(*t)),
                Style::default().fg(DIM),
            )];
            // "a → b: text" with the names in their colours
            match e.split_once(": ") {
                Some((who, rest)) if who.contains(" → ") => {
                    for (k, part) in who.split(" → ").enumerate() {
                        if k > 0 {
                            spans.push(Span::styled(" → ", Style::default().fg(DIM)));
                        }
                        let c = colors
                            .get(part)
                            .map(|c| Color::Indexed(*c))
                            .unwrap_or(AMBER);
                        spans.push(Span::styled(part.to_string(), Style::default().fg(c)));
                    }
                    spans.push(Span::styled(format!("  {rest}"), Style::default().fg(TEXT)));
                }
                _ => spans.push(Span::styled(e.clone(), Style::default().fg(TEXT))),
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Board 4's Preview, Agents and Activity panes.
fn draw_info(
    f: &mut Frame,
    app: &App,
    id: crate::workspace::PaneId,
    p: &crate::infopane::InfoPane,
    r: Rect,
    focus: bool,
) {
    use crate::infopane::InfoKind as K;
    let buf = f.buffer_mut();
    let w = r.width as usize;
    let d = Style::default().fg(DIM);
    let sel_bg = Style::default().bg(Color::Indexed(237));
    let row = |buf: &mut ratatui::buffer::Buffer, y: u16, on: bool| {
        if on && focus {
            buf.set_style(Rect::new(r.x, y, r.width, 1), sel_bg);
        }
    };
    match p.kind {
        K::Preview => {
            let Some((path, dir, hidden)) = app.tab_files_selection(id) else {
                buf.set_stringn(r.x, r.y, "select a file in the files pane (F2)", w, d);
                return;
            };
            let (lines, note) = p.preview(&path, dir, hidden);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (x, _) = buf.set_stringn(
                r.x,
                r.y,
                &name,
                w,
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            );
            buf.set_stringn(
                x + 1,
                r.y,
                note.trim_start_matches("· "),
                w.saturating_sub(name.chars().count() + 1),
                d,
            );
            let top = p.sel.min(lines.len().saturating_sub(1));
            let gutter = if lines.len() >= 1000 { 5 } else { 4 };
            for (k, line) in lines
                .iter()
                .enumerate()
                .skip(top)
                .take(r.height.saturating_sub(1) as usize)
            {
                let y = r.y + 1 + (k - top) as u16;
                if !dir {
                    buf.set_stringn(
                        r.x,
                        y,
                        format!("{:>g$} ", k + 1, g = gutter - 1),
                        gutter,
                        Style::default().fg(FAINT),
                    );
                    buf.set_line(
                        r.x + gutter as u16,
                        y,
                        line,
                        r.width.saturating_sub(gutter as u16),
                    );
                } else {
                    buf.set_line(r.x, y, line, r.width);
                }
            }
        }
        K::Agents => {
            let ids = app.agent_ids();
            if ids.is_empty() {
                buf.set_stringn(r.x, r.y, "no agents yet · shift+F9 starts one", w, d);
                return;
            }
            let head = format!(
                "{:<14}{:<12}{:>8}  {}",
                "AGENT", "STATE", "TOKENS", "CHANGES"
            );
            buf.set_stringn(r.x, r.y, &head, w, d);
            for (k, aid) in ids
                .iter()
                .enumerate()
                .take(r.height.saturating_sub(1) as usize)
            {
                let y = r.y + 1 + k as u16;
                let Some(a) = app.panes.get(aid).and_then(|p| p.agent.as_ref()) else {
                    continue;
                };
                row(buf, y, k == p.sel);
                let base = if k == p.sel && focus {
                    sel_bg
                } else {
                    Style::default()
                };
                let (state, c) = match &a.status {
                    _ if a.paused => ("⏸ paused".to_string(), DIM),
                    agents::Status::Working => (format!("{} working", spinner(app)), AMBER),
                    agents::Status::NeedsApproval(_) => ("⚑ approval".to_string(), AMBER),
                    agents::Status::Idle if a.worked => ("✔ finished".to_string(), GREEN),
                    agents::Status::Idle => ("● idle".to_string(), DIM),
                    agents::Status::Starting => ("starting".to_string(), DIM),
                    agents::Status::Exited(Some(0)) | agents::Status::Exited(None) => {
                        ("✔ done".to_string(), GREEN)
                    }
                    agents::Status::Exited(Some(code)) => (format!("✖ exit {code}"), RED),
                };
                let indent = "  ".repeat(a.depth.saturating_sub(1) as usize);
                let name = format!("● {indent}{}", a.name);
                let (x, _) = buf.set_stringn(
                    r.x,
                    y,
                    format!("{name:<14}"),
                    14,
                    base.fg(Color::Indexed(a.color)),
                );
                let (x, _) = buf.set_stringn(x, y, format!("{state:<12}"), 12, base.fg(c));
                let tok = a
                    .tokens
                    .map(agents::fmt_tokens)
                    .unwrap_or_else(|| "—".into());
                let tok = tok.trim_end_matches(" tok").to_string();
                let (x, _) = buf.set_stringn(x, y, format!("{tok:>8}  "), 10, base.fg(TEXT));
                let files = a.files_summary();
                buf.set_stringn(
                    x,
                    y,
                    if files.is_empty() {
                        "—".to_string()
                    } else {
                        files
                    },
                    w.saturating_sub(34),
                    base.fg(DIM),
                );
            }
        }
        K::Activity => {
            let running = app.activities();
            let done = app.finished();
            if running.is_empty() && done.is_empty() {
                buf.set_stringn(r.x, r.y, "no downloads or installs yet", w, d);
                return;
            }
            let mut y = r.y;
            let mut k = 0usize;
            for (_, act, who) in &running {
                if y >= r.bottom() {
                    break;
                }
                row(buf, y, k == p.sel);
                let base = if k == p.sel && focus {
                    sel_bg
                } else {
                    Style::default()
                };
                let pct = act.progress.map(|p| format!(" {p}%")).unwrap_or_default();
                let by = who.map(|w| format!(" · {w}")).unwrap_or_default();
                let (x, _) =
                    buf.set_stringn(r.x, y, format!("{} ", circle(app)), 2, base.fg(AMBER));
                let (x, _) = buf.set_stringn(x, y, &act.label, w.saturating_sub(24), base.fg(TEXT));
                buf.set_stringn(x, y, format!("{pct}{by}"), 20, base.fg(AMBER));
                y += 1;
                k += 1;
            }
            for dn in done {
                if y >= r.bottom() {
                    break;
                }
                row(buf, y, k == p.sel);
                let base = if k == p.sel && focus {
                    sel_bg
                } else {
                    Style::default()
                };
                let (icon, c) = if dn.ok {
                    ("✔ ", GREEN)
                } else {
                    ("✖ ", RED)
                };
                let (x, _) = buf.set_stringn(r.x, y, icon, 2, base.fg(c));
                let (x, _) = buf.set_stringn(x, y, &dn.label, w.saturating_sub(24), base.fg(TEXT));
                let extra = format!(
                    "  {}{}",
                    dn.amount
                        .clone()
                        .map(|a| format!("{a} · "))
                        .unwrap_or_default(),
                    fmt_secs(dn.took)
                );
                buf.set_stringn(x, y, extra, 22, base.fg(DIM));
                y += 1;
                k += 1;
            }
        }
    }
}

/// Board 12 "Restore last session?": a tick per saved tab and two buttons.
fn draw_restore(
    f: &mut Frame,
    app: &App,
    saved: &crate::session::Session,
    checked: &[bool],
    sel: usize,
) {
    let n = saved.tabs.len();
    let h = (n as u16 + 9).min(app.screen.height);
    let w = 66u16.min(app.screen.width);
    let area = centered(app.screen, w, h);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::styled(
        " Restore last session?",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let room = inner.width.saturating_sub(8) as usize;
    for (i, t) in saved.tabs.iter().enumerate() {
        let on = checked.get(i).copied().unwrap_or(true);
        let folder = t
            .layout
            .folders()
            .first()
            .map(|f| short_path(f))
            .unwrap_or_default();
        let what = match &t.name {
            Some(n) => format!("{} · {n} · {folder}", i + 1),
            None => format!("{} · {folder}", i + 1),
        };
        let remote = t.layout.has_tool("remote");
        let what = if remote && !on {
            format!("{what} (not restored)")
        } else {
            what
        };
        let mut st = if on {
            Style::default().fg(TEXT)
        } else {
            Style::default().fg(DIM)
        };
        if i == sel {
            st = st.bg(Color::Indexed(237)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(
                if on { " ▣ " } else { " ▢ " },
                Style::default().fg(if on { AMBER } else { DIM }),
            ),
            Span::styled(truncate_str(&what, room), st),
        ]));
    }
    let picked = checked.iter().filter(|c| **c).count();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        if saved
            .tabs
            .iter()
            .any(|t| t.scrollback.iter().any(|s| !s.is_empty()))
        {
            " Restores folder, shell, splits, names and scrollback. Commands are not re-run."
        } else {
            " Restores folder, shell, splits and names. Commands are not re-run."
        },
        Style::default().fg(DIM),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!(
                " ⏎ Restore {picked} tab{} ",
                if picked == 1 { "" } else { "s" }
            ),
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            " n Start fresh ",
            Style::default()
                .bg(crate::palette::role(crate::palette::Role::TabChip))
                .fg(TEXT),
        ),
    ]));
    lines.push(Line::styled(
        " ↑↓ · space tick · a all",
        Style::default().fg(FAINT),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Board 4b: Option is typing characters instead of acting as Alt.
fn draw_option_help(f: &mut Frame, app: &App, sent: &str, testing: bool, works: Option<bool>) {
    let w = 64u16.min(app.screen.width);
    let area = centered(app.screen, w, 17.min(app.screen.height));
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let term = match std::env::var("TERM_PROGRAM").as_deref().unwrap_or("") {
        "Apple_Terminal" => "Terminal.app".to_string(),
        "iTerm.app" => "iTerm2".to_string(),
        "" => "your terminal".to_string(),
        o => o.to_string(),
    };
    let d = Style::default().fg(DIM);
    let t = Style::default().fg(TEXT);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                " ⚑ Alt keys aren't reaching Shellvisor",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  shown once", d),
        ]),
        Line::from(vec![
            Span::styled(" You pressed an alt shortcut but the terminal sent ", t),
            Span::styled(sent.to_string(), Style::default().fg(AMBER)),
            Span::styled(".", t),
        ]),
        Line::styled(
            " Option is typing special characters instead of acting as Alt.",
            t,
        ),
        Line::raw(""),
        Line::from(vec![Span::styled(" detected: ", d), Span::styled(term, t)]),
        Line::styled(
            format!(" {}", crate::keys::option_fix_hint()),
            Style::default().fg(TEXT).bg(Color::Indexed(235)),
        ),
        Line::raw(""),
        Line::styled(" iTerm2: Profiles → Keys → Left Option key → Esc+", d),
        Line::styled(" Ghostty: macos-option-as-alt = true", d),
        Line::styled(" WezTerm / Kitty: works by default", d),
        Line::raw(""),
    ];
    match (testing, works) {
        (true, _) => lines.push(Line::styled(
            " press alt+1 now…",
            Style::default().fg(AMBER),
        )),
        (false, Some(true)) => {
            lines.push(Line::styled(" ✔ alt works now", Style::default().fg(GREEN)))
        }
        (false, Some(false)) => lines.push(Line::styled(
            " ✖ still a character — change the setting, then Test again",
            Style::default().fg(RED),
        )),
        (false, None) => lines.push(Line::raw("")),
    }
    let chip = crate::palette::role(crate::palette::Role::TabChip);
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            " t Test again ",
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(" l Use leader key ", Style::default().bg(chip).fg(TEXT)),
        Span::raw("  "),
        Span::styled(" esc Later ", Style::default().bg(chip).fg(DIM)),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Board 12 "Close tab 2?": what is running, three buttons.
fn draw_close_tab(f: &mut Frame, app: &App, tab: usize, detail: &str, sel: usize) {
    use crate::palette::{Role, role};
    let w = 62u16.min(app.screen.width);
    let area = centered(app.screen, w, 10);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let chip = role(Role::TabChip);
    let btn = |i: usize, label: &str, danger: bool| -> Span<'static> {
        let st = match (i == sel, danger) {
            (true, true) => Style::default()
                .bg(RED)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD),
            (false, true) => Style::default().bg(chip).fg(RED),
            (false, false) => Style::default().bg(chip).fg(TEXT),
        };
        Span::styled(format!(" {label} "), st)
    };
    let lines = vec![
        Line::styled(
            format!(" Close tab {}?", tab + 1),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(format!(" {} ", spinner(app)), Style::default().fg(AMBER)),
            Span::styled(detail.to_string(), Style::default().fg(TEXT)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw(" "),
            btn(0, "s Stop & close", true),
            Span::raw("  "),
            btn(1, "k Keep in background", false),
            Span::raw("  "),
            btn(2, "esc Cancel", false),
        ]),
        Line::raw(""),
        Line::styled(
            " \"Keep in background\" keeps it running off the tab bar;",
            Style::default().fg(DIM),
        ),
        Line::styled(
            " the ◌ chip there brings it back. Cancel keeps the tab.",
            Style::default().fg(DIM),
        ),
        Line::styled(" ←→ pick · ⏎", Style::default().fg(FAINT)),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// Cut to `w` characters with an ellipsis.
fn truncate_str(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    s.chars().take(w.saturating_sub(1)).chain(['…']).collect()
}

/// The boards' toast on finish: "✔ git clone finished · 412 MB in 38s",
/// then what you can do with it.
fn draw_finish_toast(f: &mut Frame, app: &App, d: &crate::app::Finished) {
    let (_, content, _) = app.regions();
    let took = fmt_secs(d.took);
    let (icon, color, line1) = if d.ok {
        let what = match &d.amount {
            Some(a) => format!(" · {a} in {took}"),
            None => format!(" · {took}"),
        };
        ("✔", GREEN, format!(" {} finished{what}", d.label))
    } else {
        (
            "✖",
            RED,
            format!(
                " {} failed · exit {} after {took}",
                d.label,
                d.code.unwrap_or(1)
            ),
        )
    };
    let name = d
        .target
        .as_ref()
        .and_then(|t| t.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let line2 = match &d.target {
        Some(t) if d.ok && t.is_dir() => Some(format!("c cd into {name} · o open in files pane")),
        Some(_) if d.ok => Some(format!("o show {name} in the files pane")),
        _ => None,
    };
    let w = (line1.chars().count() + 1).max(line2.as_ref().map(|l| l.chars().count()).unwrap_or(0))
        as u16
        + 4;
    let h = if line2.is_some() { 4 } else { 3 };
    if content.width < w + 2 || content.height < h + 2 {
        return;
    }
    let r = Rect::new(content.right() - w - 1, content.bottom() - h - 1, w, h);
    use crate::palette::{Role, role};
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(role(Role::Stroke)))
        .style(Style::default().bg(role(Role::Bar)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {icon}"), Style::default().fg(color)),
        Span::styled(line1, Style::default().fg(TEXT)),
    ])];
    if let Some(l) = line2 {
        lines.push(Line::styled(format!(" {l}"), Style::default().fg(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The activity list as on the boards: one card per download with the
/// spinner, the command and where it comes from, a thin bar, the tool's own
/// progress line, and amount / speed / time left on the right.
fn draw_activity(f: &mut Frame, app: &App, sel: usize) {
    use crate::palette::{Role, role};
    let (_, content, _) = app.regions();
    if content.width < 40 || content.height < 10 {
        return;
    }
    f.render_widget(Clear, content);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(AMBER))
        .title(Span::styled(
            " ACTIVITY · downloads & installs ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(content);
    f.render_widget(block, content);
    let buf = f.buffer_mut();
    let dim = Style::default().fg(DIM);
    let text = Style::default().fg(TEXT);
    let put = |buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, s: &str, st: Style, w: u16| {
        if y < inner.bottom() && x < inner.right() {
            let w = w.min(inner.right() - x);
            buf.set_stringn(x, y, s, w as usize, st);
        }
    };
    put(
        buf,
        inner.x + 1,
        inner.y,
        "Spotted from shell commands (git, npm, pnpm, pip, uv, brew, docker, cargo, curl, wget…) — progress, speed and time left read from their output.",
        dim,
        inner.width - 2,
    );
    struct Card {
        state: u8, // 0 running, 1 done, 2 failed
        cmd: String,
        src: String,
        progress: Option<u8>,
        color: Color,
        phase: String,
        amount: String,
        rate: String,
        eta: String,
    }
    let tidy = |c: &str| {
        c.replace("https://", "")
            .replace("http://", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut cards: Vec<Card> = Vec::new();
    for (_, a, agent) in app.activities() {
        let mut phase = a.phase.clone().unwrap_or_else(|| "starting…".into());
        if let Some(n) = agent {
            phase = format!("agent {n} · {phase}");
        }
        if a.paused {
            phase = format!("▮▮ paused · p resumes · x cancels · {phase}");
        }
        cards.push(Card {
            state: if a.paused { 3 } else { 0 },
            cmd: tidy(&a.cmd),
            src: a.src.clone(),
            progress: a.progress,
            color: tool_fg(&a.label),
            phase,
            amount: a.amount.clone().unwrap_or_default(),
            rate: a.speed.clone().unwrap_or_else(|| "—".into()),
            eta: eta_text(a)
                .unwrap_or_else(|| format!("{} so far", fmt_secs(a.started.elapsed().as_secs()))),
        });
    }
    let running = cards.len();
    for (j, d) in app.done.iter().enumerate() {
        let selected = running + j == sel;
        let mut phase = if d.ok {
            format!("done in {}", fmt_secs(d.took))
        } else {
            format!(
                "failed · exit {} after {}",
                d.code.unwrap_or(1),
                fmt_secs(d.took)
            )
        };
        if selected && d.ok {
            match &d.target {
                Some(t) if t.is_dir() => phase.push_str(" · c cd into it · o open in files"),
                Some(_) => phase.push_str(" · o show in files"),
                None => {}
            }
        }
        cards.push(Card {
            state: if d.ok { 1 } else { 2 },
            cmd: tidy(&d.cmd),
            src: d.src.clone(),
            progress: Some(100),
            color: if d.ok { GREEN } else { RED },
            phase,
            amount: d.amount.clone().unwrap_or_default(),
            rate: String::new(),
            eta: String::new(),
        });
    }
    let now = crate::history::History::now();
    for p in app.past.iter().take(crate::app::PAST_SHOWN) {
        let when = ago(now.saturating_sub(p.at));
        let phase = if p.ok {
            format!("earlier · {when} · took {} · r run again", fmt_secs(p.took))
        } else {
            format!(
                "earlier · {when} · failed, exit {} · r run again",
                p.code.unwrap_or(1)
            )
        };
        cards.push(Card {
            state: if p.ok { 4 } else { 5 },
            cmd: tidy(&p.cmd),
            src: p.src.clone(),
            progress: Some(100),
            color: role(Role::Track),
            phase,
            amount: p.amount.clone().unwrap_or_default(),
            rate: String::new(),
            eta: String::new(),
        });
    }
    let top = inner.y + 2;
    let footer = inner.bottom().saturating_sub(1);
    let fits = ((footer.saturating_sub(top)) / 5).max(1) as usize;
    if cards.is_empty() {
        put(
            buf,
            inner.x + 2,
            top + 1,
            "Nothing downloaded or installed yet in this session.",
            dim,
            inner.width,
        );
    }
    let first = sel
        .saturating_sub(fits - 1)
        .min(cards.len().saturating_sub(fits));
    let mut y = top;
    for (i, c) in cards.iter().enumerate().skip(first).take(fits) {
        let r = Rect::new(inner.x + 1, y, inner.width - 2, 5);
        let card = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if i == sel { AMBER } else { role(Role::Stroke) }))
            .style(Style::default().bg(role(Role::Card)));
        let ci = card.inner(r);
        ratatui::widgets::Widget::render(card, r, buf);
        // icon
        let (icon, ist) = match c.state {
            0 => (circle(app).to_string(), Style::default().fg(AMBER)),
            1 => ("✔".to_string(), Style::default().fg(GREEN)),
            3 => ("▮▮".to_string(), Style::default().fg(AMBER)),
            4 => ("✔".to_string(), dim),
            _ => ("✖".to_string(), Style::default().fg(RED)),
        };
        put(buf, ci.x + 2, ci.y + 1, &icon, ist, 4);
        // right column
        let rw = 18u16.min(ci.width / 3);
        let rx = ci.right().saturating_sub(rw + 1);
        for (k, (t, st)) in [
            (&c.amount, text),
            (&c.rate, Style::default().fg(AMBER)),
            (&c.eta, dim),
        ]
        .iter()
        .enumerate()
        {
            let n = t.chars().count() as u16;
            put(buf, rx + rw.saturating_sub(n), ci.y + k as u16, t, *st, rw);
        }
        // middle
        let mx = ci.x + 6;
        let mw = rx.saturating_sub(mx + 2);
        let src_w = c.src.chars().count() as u16;
        put(
            buf,
            mx,
            ci.y,
            &c.cmd,
            text.add_modifier(Modifier::BOLD),
            mw.saturating_sub(src_w + 2),
        );
        put(buf, mx + mw.saturating_sub(src_w), ci.y, &c.src, dim, src_w);
        let mut bx = mx;
        for sp in thin_bar(c.progress, mw as usize, c.color) {
            let n = sp.content.chars().count() as u16;
            put(buf, bx, ci.y + 1, &sp.content, sp.style, n);
            bx += n;
        }
        put(buf, mx, ci.y + 2, &c.phase, dim, mw);
        y += 5;
    }
    put(
        buf,
        inner.x + 1,
        footer,
        "↑↓ select · p pause / resume · x cancel (finished: clear) · r run again · c cd · o open · esc close",
        Style::default().fg(FAINT),
        inner.width - 2,
    );
}

/// "5 min ago", "3 h ago", "2 days ago".
fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => {
            let d = secs / 86_400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
    }
}

/// A hint for a shell that is waiting on something you may not know how
/// to leave: an unclosed quote, vim, less, a REPL…
pub fn stuck_hint(p: &PaneState) -> Option<&'static str> {
    let screen = p.pane.screen();
    if !p.running {
        // zsh's continuation prompt says what it waits for ("quote> ")
        let (row, _) = screen.cursor_position();
        let cols = screen.size().1;
        let line = screen.contents_between(row, 0, row, cols);
        let (head, _) = line.split_once("> ")?;
        let words: Vec<&str> = head.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        if words.iter().all(|w| w.ends_with("quote")) {
            return Some("unclosed quote · type it, or ctrl+C to cancel");
        }
        if words.contains(&"heredoc") {
            return Some("here-document · type its end word, or ctrl+C");
        }
        let cont = [
            "pipe",
            "cmdand",
            "cmdor",
            "for",
            "while",
            "until",
            "if",
            "then",
            "else",
            "elif",
            "function",
            "cursh",
            "subsh",
            "cmdsubst",
            "case",
            "select",
            "foreach",
            "repeat",
            "braceparam",
            "math",
            "mathsubst",
            "quote",
            "dquote",
            "bquote",
        ];
        if words.iter().all(|w| cont.contains(w)) {
            return Some("command not finished · keep typing, or ctrl+C to cancel");
        }
        return None;
    }
    let cmd = p.command.trim();
    let first = cmd
        .split_whitespace()
        .find(|w| !w.contains('=') && *w != "sudo")
        .unwrap_or("");
    let prog = first.rsplit('/').next().unwrap_or(first);
    Some(match prog {
        "vi" | "vim" | "nvim" | "view" | "vimdiff" => "vim · Esc then :q! quits · :wq saves",
        "less" | "more" | "man" | "most" => "q quits · / searches",
        "nano" | "pico" => "ctrl+X quits",
        "emacs" if cmd.contains("-nw") => "ctrl+X ctrl+C quits",
        "top" | "htop" | "btop" | "watch" => "q quits",
        "python" | "python3" | "node" | "irb" | "ghci" | "lua"
            if cmd.split_whitespace().count() == 1 =>
        {
            "ctrl+D quits"
        }
        "ssh" | "mosh" => "type exit to leave · Enter ~ . if it hangs",
        "tail" if cmd.contains(" -f") || cmd.contains(" -F") => "ctrl+C stops",
        _ => return None,
    })
}

/// A running command asking for a password or a yes / no (board 12's "?").
pub fn waiting_input(p: &PaneState) -> bool {
    if !p.running {
        return false;
    }
    let screen = p.pane.screen();
    let (row, _) = screen.cursor_position();
    let cols = screen.size().1;
    let line = screen.contents_between(row, 0, row, cols).to_lowercase();
    let line = line.trim_end();
    line.contains("password")
        || line.contains("passphrase")
        || [
            "[y/n]",
            "(y/n)",
            "[y/n]:",
            "(y/n):",
            "(yes/no)",
            "(yes/no)?",
            "[yes/no]",
            "y/n?",
            "continue?",
        ]
        .iter()
        .any(|e| line.ends_with(e))
}

/// The saved-sessions picker (like PuTTY's Saved Sessions list).
fn draw_sessions(f: &mut Frame, app: &App, sel: usize, list: &[(String, String, String)]) {
    let (_, content, _) = app.regions();
    let h = (list.len() as u16 + 5).min(content.height);
    let r = centered(content, 72, h);
    f.render_widget(Clear, r);
    let block = pane_block(" ⇄ connect · saved sessions ".to_string(), true);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let rows = inner.height.saturating_sub(2) as usize;
    let first = sel.saturating_sub(rows.saturating_sub(1));
    let mut lines: Vec<Line> = list
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, (name, addr, _))| {
            let st = if i == sel {
                Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            let w = inner.width as usize;
            let mark = if app.session_marks.contains(&i) {
                "☑"
            } else {
                " "
            };
            let text = format!("{mark} {name:<22} {addr}");
            Line::styled(format!("{text:<w$}"), st)
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        if app.session_marks.is_empty() {
            " ⏎ open · space mark several (typing goes to all) · a all · esc close"
        } else {
            " ⏎ open the marked ones in one tab, typing to all · space mark · esc close"
        },
        Style::default().fg(FAINT),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The failed-command card: exit code, the command, its last lines, and
/// what you can do (copy the error, run it again).
fn draw_fail_card(f: &mut Frame, app: &App, c: &crate::app::FailCard) {
    use crate::palette::{Role, role};
    // only over the pane the command ran in, when it's on screen
    let Some((_, _, content)) = app.visible().into_iter().find(|(id, _, _)| *id == c.pane) else {
        return;
    };
    let w = content.width.saturating_sub(2).min(76);
    let h = c.lines.len() as u16 + 4;
    if w < 30 || content.height < h + 1 {
        return;
    }
    let r = Rect::new(content.right() - w, content.bottom() - h, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(role(Role::ErrBorder)))
        .style(Style::default().bg(role(Role::ErrBg)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let iw = inner.width as usize;
    let cmd: String = c.cmd.chars().take(iw.saturating_sub(24)).collect();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" ✖ exit {} ", c.code),
            Style::default().fg(RED).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("· {cmd} "), Style::default().fg(TEXT)),
        Span::styled(format!("· {}", fmt_secs(c.took)), Style::default().fg(DIM)),
    ])];
    for l in &c.lines {
        let l: String = l.chars().take(iw.saturating_sub(2)).collect();
        lines.push(Line::styled(format!(" {l}"), Style::default().fg(DIM)));
    }
    lines.push(Line::styled(
        if iw >= 50 {
            " ctrl+] c copy the error · r run it again · ? ask an agent why"
        } else {
            " ctrl+] c copy · r run again · ? why"
        },
        Style::default().fg(AMBER),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The command palette: type to filter every action, setting, session,
/// layout and tab.
fn draw_palette(f: &mut Frame, app: &App, pal: &crate::app::Palette) {
    let (_, content, _) = app.regions();
    let w = content.width.saturating_sub(4).min(84);
    let h = content.height.saturating_sub(4).min(22);
    if w < 30 || h < 6 {
        return;
    }
    let r = Rect::new(content.x + (content.width - w) / 2, content.y + 1, w, h);
    f.render_widget(Clear, r);
    let block = pane_block(" ⌘ command palette ".to_string(), true);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let iw = inner.width as usize;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                " ❯ ",
                Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{}▏", pal.query), Style::default().fg(TEXT)),
            if pal.query.is_empty() {
                Span::styled(
                    "type an action, setting, session, layout or tab",
                    Style::default().fg(FAINT),
                )
            } else {
                Span::raw("")
            },
        ]),
        Line::raw(""),
    ];
    let m = pal.matches();
    let rows = inner.height.saturating_sub(3) as usize;
    let first = pal.sel.saturating_sub(rows.saturating_sub(1));
    for (k, &i) in m.iter().enumerate().skip(first).take(rows) {
        let (label, detail, _) = &pal.items[i];
        let dw = detail.chars().count();
        let lw = iw.saturating_sub(dw + 4);
        let label: String = label.chars().take(lw).collect();
        let text = format!(" {label:<lw$}  {detail} ");
        let st = if k == pal.sel {
            Style::default()
                .bg(AMBER)
                .fg(INK)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT)
        };
        lines.push(Line::styled(text, st));
    }
    if m.is_empty() {
        lines.push(Line::styled(" nothing matches", Style::default().fg(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
    let foot = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
    f.render_widget(
        Paragraph::new(Line::styled(
            format!(
                " ↑↓ choose · ⏎ run · esc close · {} entries",
                pal.items.len()
            ),
            Style::default().fg(FAINT),
        )),
        foot,
    );
}

/// The right-click menu: Copy, Paste, Select all, Open link, splits.
fn draw_context_menu(f: &mut Frame, app: &App, m: &crate::app::ContextMenu) {
    use crate::palette::{Role, role};
    let r = app.context_rect(m);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(role(Role::Stroke)))
        .style(Style::default().bg(role(Role::Bar)));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let has_sel = app.selection_text().is_some();
    let has_link = app.context_link(m).is_some();
    let pane = app.panes.get(&m.pane);
    let logging = pane.is_some_and(|p| p.log.is_some());
    let can_reconnect = pane.is_some_and(|p| p.reconnect.is_some());
    let lines: Vec<Line> = crate::app::CONTEXT_ITEMS
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let enabled = match i {
                0 => has_sel,
                3 => has_link,
                8 => can_reconnect,
                _ => true,
            };
            let item = if i == 6 && logging {
                &"Stop logging"
            } else {
                item
            };
            let st = if i == m.sel {
                Style::default()
                    .bg(AMBER)
                    .fg(INK)
                    .add_modifier(Modifier::BOLD)
            } else if enabled {
                Style::default().fg(TEXT)
            } else {
                Style::default().fg(FAINT)
            };
            let w = inner.width as usize;
            Line::styled(format!(" {:<w$}", item, w = w.saturating_sub(1)), st)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_palette_keeps_contrast() {
        assert_eq!(light_color(Color::Indexed(252)), Color::Indexed(235));
        assert_eq!(light_color(Color::Indexed(237)), Color::Indexed(254));
        assert_eq!(light_color(Color::Indexed(179)), Color::Indexed(130));
        assert_eq!(light_color(Color::Indexed(16)), Color::Indexed(16));
        assert_eq!(light_color(Color::Reset), Color::Reset);
    }

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

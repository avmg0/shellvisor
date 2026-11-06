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
                };
                let key = key_label(app, action);
                let mut title = if key.is_empty() {
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
            }
            continue;
        }
        let Some(p) = app.panes.get(&id) else {
            continue;
        };
        if let (true, Some(a)) = (full, p.agent.as_ref()) {
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
        return (format!("{} agents · {n}", i + 1), icon);
    }
    let ids = tab.root.panes();
    let just_done = app
        .done
        .iter()
        .find(|d| d.at.elapsed() < std::time::Duration::from_secs(5) && ids.contains(&d.pane));
    let icon = if panes.iter().any(|p| p.activity.is_some()) {
        Some((circle(app).to_string(), AMBER))
    } else if let Some(d) = just_done {
        Some(if d.ok {
            ("✔".to_string(), GREEN)
        } else {
            ("✖".to_string(), RED)
        })
    } else if panes.iter().any(|p| p.running) {
        Some((spinner(app).to_string(), AMBER))
    } else if let Some(code) = panes.iter().filter_map(|p| p.last_exit).find(|c| *c != 0) {
        Some((format!("✖ {code}"), RED))
    } else if panes.iter().any(|p| p.bell) {
        Some(("!".to_string(), AMBER))
    } else if panes.iter().any(|p| p.unseen) {
        Some(("•".to_string(), CYAN))
    } else {
        None
    };
    let cast = if ids.iter().any(|id| app.broadcast.contains(id)) {
        " ⇶"
    } else {
        ""
    };
    (format!("{} {shell} · {what}{split}{cast}", i + 1), icon)
}

/// Tab index under screen column `x` (for mouse clicks).
pub fn tab_at(app: &App, x: u16) -> Option<usize> {
    let mut col = 0u16;
    let compact = tabs_compact(app, app.screen.width);
    for i in 0..app.tabs.len() {
        let w = tab_width_in(app, i, compact);
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
    if app.tabs[app.active].focus_mode {
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
        let mut t = format!("{n} download{}", if n == 1 { "" } else { "s" });
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
    for i in 0..app.tabs.len() {
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
    spans.push(Span::styled(" + new ▾", Style::default().bg(BAR).fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), r);
    let n = app.tabs.len();
    let right = format!(
        "{n} tab{} · {} ",
        if n == 1 { "" } else { "s" },
        crate::osinfo::self_memory()
    );
    let w = right.chars().count() as u16;
    let used: u16 = (0..n)
        .map(|i| tab_width_in(app, i, compact) + 1)
        .sum::<u16>()
        + 9;
    if used + w + 2 <= r.width {
        f.buffer_mut()
            .set_string(r.right() - w, r.y, &right, Style::default().fg(DIM).bg(BAR));
    }
}

/// Many tabs: the strip shows numbers only.
fn tabs_compact(app: &App, width: u16) -> bool {
    let full: u16 = (0..app.tabs.len())
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
            (k("agent_board", "alt+g"), "Agent board"),
            ("^] D".into(), "Review"),
            (k("zoom", "F5"), "Zoom agent"),
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
        Some(t) if d.ok && t.is_dir() => {
            Some(format!("ctrl+] c cd into {name} · o open in files pane"))
        }
        Some(_) if d.ok => Some(format!("ctrl+] o show {name} in the files pane")),
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

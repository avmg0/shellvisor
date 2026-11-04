//! The Flow pane: what the shell next to it is running right now.
//!
//! * While `sv flow '…'` runs, its per-stage meters (bytes, lines, kept %,
//!   rate, the slowest stage, stderr, last output lines).
//! * Otherwise the live foreground job: every process in it with CPU,
//!   memory and — on Linux — read/write rates and which ones are piped
//!   together.
//!
//! Sampled once a second while visible (every 3 s in low-power mode).

use crate::flowrun::FlowStats;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);
const BLUE: Color = Color::Indexed(110);
const MAGENTA: Color = Color::Indexed(170);
const TEXT: Color = Color::Indexed(252);

pub const INTERVAL: Duration = Duration::from_secs(1);
pub const INTERVAL_LOW_POWER: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub cpu: f32,
    pub rss_kb: u64,
    pub args: String,
}

impl Proc {
    #[cfg(test)]
    pub fn name(&self) -> &str {
        let first = self.args.split_whitespace().next().unwrap_or("");
        first.rsplit('/').next().unwrap_or(first)
    }
}

/// Parse `ps -A -o pid=,ppid=,pgid=,pcpu=,rss=,args=` (same on macOS and Linux).
pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let pgid = it.next()?.parse().ok()?;
            let cpu = it.next()?.parse().ok()?;
            let rss_kb = it.next()?.parse().ok()?;
            let args = it.collect::<Vec<_>>().join(" ");
            Some(Proc {
                pid,
                ppid,
                pgid,
                cpu,
                rss_kb,
                args,
            })
        })
        .collect()
}

fn ps_all() -> Vec<Proc> {
    std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,pcpu=,rss=,args="])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Linux: bytes read / written so far (all I/O, pipes included).
fn proc_io(pid: i32) -> Option<(u64, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    Some((get("rchar:")?, get("wchar:")?))
}

/// Linux: the pipe on fd 0 / fd 1, if any.
fn pipe_of(pid: i32, fd: u8) -> Option<String> {
    let l = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok()?;
    let s = l.to_string_lossy();
    s.starts_with("pipe:").then(|| s.into_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobProc {
    pub proc: Proc,
    /// Bytes per second (Linux only).
    pub read_rate: Option<u64>,
    pub write_rate: Option<u64>,
    /// Reads from the previous process's output.
    pub piped_from_prev: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlowAct {
    /// Put this command line at the prompt.
    Type(String),
}

#[derive(Default)]
pub struct FlowPane {
    pub flow: Option<(PathBuf, FlowStats)>,
    pub job: Vec<JobProc>,
    /// Command the shell is running (from the shell hooks).
    pub job_cmd: String,
    job_started: Option<Instant>,
    prev_io: HashMap<i32, (u64, u64, Instant)>,
    last: Option<Instant>,
    /// Output rate of each `sv flow` stage, once a second (last 30 s).
    hist: Vec<std::collections::VecDeque<u64>>,
    /// The stats the history was last taken from: (file, bytes out per stage, when).
    hist_at: Option<(PathBuf, Vec<u64>, Instant)>,
}

/// Seconds of history the throughput chart shows.
const HIST: usize = 30;

impl FlowPane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn due_in(&self, low_power: bool) -> Duration {
        let every = if low_power {
            INTERVAL_LOW_POWER
        } else {
            INTERVAL
        };
        self.last
            .map(|t| every.saturating_sub(t.elapsed()))
            .unwrap_or(Duration::ZERO)
    }

    /// Read the job and/or the `sv flow` stats file.
    pub fn sample(
        &mut self,
        pgid: Option<i32>,
        flow: Option<&Path>,
        cmd: &str,
        started: Option<Instant>,
    ) {
        self.last = Some(Instant::now());
        self.job_cmd = cmd.to_string();
        self.job_started = started;
        if let Some(p) = flow {
            if let Some(s) = std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<FlowStats>(&t).ok())
            {
                self.record(p, &s);
                self.flow = Some((p.to_path_buf(), s));
            }
        }
        let Some(pgid) = pgid else {
            self.job.clear();
            self.prev_io.clear();
            return;
        };
        let mut procs: Vec<Proc> = ps_all().into_iter().filter(|p| p.pgid == pgid).collect();
        procs.sort_by_key(|p| p.pid);
        let now = Instant::now();
        let mut prev_out: Option<String> = None;
        let mut job = Vec::new();
        let mut seen = HashMap::new();
        for p in procs {
            let (mut rr, mut wr) = (None, None);
            if let Some((r, w)) = proc_io(p.pid) {
                if let Some((pr, pw, at)) = self.prev_io.get(&p.pid) {
                    let dt = now.duration_since(*at).as_secs_f64().max(0.1);
                    rr = Some(((r.saturating_sub(*pr)) as f64 / dt) as u64);
                    wr = Some(((w.saturating_sub(*pw)) as f64 / dt) as u64);
                }
                seen.insert(p.pid, (r, w, now));
            }
            let stdin = pipe_of(p.pid, 0);
            let piped = stdin.is_some() && stdin == prev_out;
            prev_out = pipe_of(p.pid, 1);
            job.push(JobProc {
                proc: p,
                read_rate: rr,
                write_rate: wr,
                piped_from_prev: piped,
            });
        }
        self.prev_io = seen;
        self.job = job;
    }

    /// Add a second of output rates to the history (a new run starts it over).
    fn record(&mut self, path: &Path, s: &FlowStats) {
        let now = Instant::now();
        let bytes: Vec<u64> = s.stages.iter().map(|x| x.bytes_out).collect();
        let fresh = match &self.hist_at {
            Some((p, b, _)) => {
                p != path || b.len() != bytes.len() || bytes.iter().zip(b).any(|(n, o)| n < o)
            }
            None => true,
        };
        if fresh {
            self.hist = vec![Default::default(); bytes.len()];
            // the first reading: rates since the start
            let secs = (s.elapsed_ms as f64 / 1000.0).max(0.25);
            for (h, b) in self.hist.iter_mut().zip(&bytes) {
                h.push_back((*b as f64 / secs) as u64);
            }
            self.hist_at = Some((path.to_path_buf(), bytes, now));
            return;
        }
        let Some((_, prev, at)) = &self.hist_at else {
            return;
        };
        if s.done && prev == &bytes {
            return; // finished: the chart stops where it ended
        }
        let dt = now.duration_since(*at).as_secs_f64().max(0.1);
        for (i, h) in self.hist.iter_mut().enumerate() {
            h.push_back(((bytes[i].saturating_sub(prev[i])) as f64 / dt) as u64);
            while h.len() > HIST {
                h.pop_front();
            }
        }
        self.hist_at = Some((path.to_path_buf(), bytes, now));
    }

    /// Is there an `sv flow` to show (running, or the last one)?
    fn showing_flow(&self) -> bool {
        match &self.flow {
            Some((_, s)) => !s.done || self.job.is_empty(),
            None => false,
        }
    }

    pub fn key(&mut self, key: &[u8]) -> Option<FlowAct> {
        let (_, s) = self.flow.as_ref()?;
        match key {
            b"s" => Some(FlowAct::Type(s.cmd.clone())),
            b"r" => Some(FlowAct::Type(format!("sv flow {}", quote(&s.cmd)))),
            _ => None,
        }
    }

    pub fn title(&self) -> String {
        if self.showing_flow() {
            let (_, s) = self.flow.as_ref().unwrap();
            format!("flow · {}", if s.done { "finished" } else { "running" })
        } else if !self.job.is_empty() {
            format!("flow · job of {}", self.job.len())
        } else {
            "flow · idle".into()
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height < 3 || area.width < 20 {
            return;
        }
        if self.showing_flow() {
            self.render_flow(area, buf);
        } else if !self.job.is_empty() {
            self.render_job(area, buf);
        } else {
            let lines = [
                (
                    "Nothing running in the shell next to this pane.",
                    Style::default().fg(DIM),
                ),
                ("", Style::default()),
                (
                    "Run a command to see its processes, CPU, memory and pipes —",
                    Style::default(),
                ),
                ("or meter a pipeline stage by stage:", Style::default()),
                ("", Style::default()),
                (
                    "  sv flow 'cat big.log | grep ERROR | sort | uniq -c'",
                    Style::default().fg(CYAN),
                ),
            ];
            for (i, (t, st)) in lines.iter().enumerate() {
                if i as u16 >= area.height {
                    break;
                }
                buf.set_stringn(
                    area.x + 1,
                    area.y + i as u16,
                    t,
                    area.width as usize - 1,
                    *st,
                );
            }
        }
    }

    fn render_flow(&self, area: Rect, buf: &mut Buffer) {
        let (_, s) = self.flow.as_ref().unwrap();
        if self.board_fits(s, area) {
            self.render_board(s, area, buf);
            return;
        }
        let w = area.width as usize;
        let mut y = area.y;
        let state = if s.done {
            ("finished", GREEN)
        } else {
            ("running", AMBER)
        };
        let (nx, _) = buf.set_stringn(
            area.x,
            y,
            format!(" {} ", s.cmd),
            w.saturating_sub(20),
            Style::default().add_modifier(Modifier::BOLD),
        );
        buf.set_stringn(
            nx,
            y,
            format!(" {:.1}s · {}", s.elapsed_ms as f64 / 1000.0, state.0),
            area.right().saturating_sub(nx) as usize,
            Style::default().fg(state.1),
        );
        y += 2;
        let slow = s.bottleneck();
        let secs = (s.elapsed_ms as f64 / 1000.0).max(0.001);
        let max_out = s
            .stages
            .iter()
            .map(|x| x.bytes_out)
            .max()
            .unwrap_or(1)
            .max(1);
        if w < 86 {
            y = self.render_flow_compact(s, area, buf, y);
            self.render_flow_tail(s, area, buf, y);
            return;
        }
        let cmd_w = w.saturating_sub(76).clamp(8, 30);
        let head = format!(
            " #  {:<cmd_w$} {:<10} {:>9} {:>9} {:>14} {:>5} {:>9}",
            "stage", "state", "in", "out", "lines in→out", "kept", "rate/s"
        );
        buf.set_stringn(
            area.x,
            y,
            head,
            w,
            Style::default().fg(DIM).add_modifier(Modifier::UNDERLINED),
        );
        y += 1;
        for (i, st) in s.stages.iter().enumerate() {
            if y >= area.bottom().saturating_sub(1) {
                break;
            }
            let cmd: String = st.cmd.chars().take(cmd_w).collect();
            let inb = if i == 0 {
                "—".to_string()
            } else {
                size(st.bytes_in)
            };
            let lines_io = if i == 0 {
                format!("→ {}", count(st.lines_out))
            } else {
                format!("{} → {}", count(st.lines_in), count(st.lines_out))
            };
            let kept = if i > 0 && st.lines_in > 0 {
                format!("{:.0}%", st.lines_out as f64 / st.lines_in as f64 * 100.0)
            } else {
                String::new()
            };
            let state = match st.exit {
                _ if slow == Some(i) => "◀ slowest".to_string(),
                Some(0) => "✔ exit 0".to_string(),
                Some(c) => format!("✖ exit {c}"),
                None if s.done => "✔ done".to_string(),
                None if i > 0 && st.lines_out == 0 && st.lines_in > 0 => "◆ holding".to_string(),
                None => "▶ running".to_string(),
            };
            let row = format!(
                " {:<2} {cmd:<cmd_w$} {state:<10} {inb:>9} {:>9} {lines_io:>14} {kept:>5} {:>9}",
                i + 1,
                size(st.bytes_out),
                size((st.bytes_out as f64 / secs) as u64)
            );
            let failed = matches!(st.exit, Some(c) if c != 0);
            let style = if failed {
                Style::default().fg(RED)
            } else {
                Style::default()
            };
            let (nx, _) = buf.set_stringn(area.x, y, &row, w, style);
            // relative throughput bar, then the bottleneck mark
            let bar_room = area.right().saturating_sub(nx + 2) as usize;
            if bar_room >= 4 {
                let n = ((st.bytes_out as f64 / max_out as f64) * bar_room.min(12) as f64).round()
                    as usize;
                buf.set_stringn(
                    nx + 1,
                    y,
                    "▮".repeat(n.max(1)),
                    bar_room,
                    Style::default().fg(if slow == Some(i) { AMBER } else { CYAN }),
                );
            }
            y += 1;
        }
        self.render_flow_tail(s, area, buf, y);
    }

    /// Room for the boards' layout: a card per stage side by side, then the
    /// throughput chart, output and stderr.
    fn board_fits(&self, s: &FlowStats, area: Rect) -> bool {
        let n = s.stages.len().max(1) as u16;
        area.height >= 26 && area.width >= 90 && (area.width.saturating_sub(6 * (n - 1))) / n >= 18
    }

    fn render_board(&self, s: &FlowStats, area: Rect, buf: &mut Buffer) {
        use crate::palette::{Role, role};
        use ratatui::widgets::{Block, BorderType, Widget};
        let text = Style::default().fg(TEXT);
        let dim = Style::default().fg(DIM);
        let put = |buf: &mut Buffer, x: u16, y: u16, t: &str, st: Style, w: u16| {
            if y < area.bottom() && x < area.right() && w > 0 {
                buf.set_stringn(x, y, t, w.min(area.right() - x) as usize, st);
            }
        };
        let boxed = |buf: &mut Buffer, r: Rect, border: Color, bg: Color| -> Rect {
            let b = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border))
                .style(Style::default().bg(bg));
            let inner = b.inner(r);
            b.render(r, buf);
            inner
        };
        let secs = (s.elapsed_ms as f64 / 1000.0).max(0.001);
        let n = s.stages.len();
        let slow = s.bottleneck();
        let rate_now = |i: usize| -> u64 {
            match self.hist.get(i).and_then(|h| h.back()) {
                Some(r) if !s.done => *r,
                _ => (s.stages[i].bytes_out as f64 / secs) as u64,
            }
        };
        // ---- header: tabs, then the command in a box
        let x0 = area.x + 1;
        put(
            buf,
            x0,
            area.y + 1,
            "sv flow",
            Style::default()
                .fg(AMBER)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            7,
        );
        put(buf, x0 + 10, area.y + 1, "Live job", dim, 8);
        let bx = x0 + 21;
        let cmd_box = Rect::new(bx, area.y, area.right().saturating_sub(bx + 1), 3);
        let ci = boxed(buf, cmd_box, role(Role::Stroke), Color::Reset);
        let hint = "s send to shell · r run again";
        let (state, sc) = if s.done {
            (format!("✔ finished {secs:.1}s"), CYAN)
        } else {
            (format!("● running {secs:.1}s"), GREEN)
        };
        let right_w = (hint.chars().count() + state.chars().count() + 3) as u16;
        put(buf, ci.x + 1, ci.y, "❯", Style::default().fg(AMBER), 1);
        put(
            buf,
            ci.x + 3,
            ci.y,
            &format!("sv flow {}", display_quote(&s.cmd)),
            text,
            ci.width.saturating_sub(right_w + 4),
        );
        let hx = ci.right().saturating_sub(right_w);
        put(buf, hx, ci.y, &state, Style::default().fg(sc), right_w);
        put(
            buf,
            hx + state.chars().count() as u16 + 3,
            ci.y,
            hint,
            dim,
            hint.chars().count() as u16,
        );

        // ---- stage cards
        let gap = 6u16;
        let cy = area.y + 4;
        let cw = (area.width.saturating_sub(2) - gap * (n as u16 - 1)) / n as u16;
        let ch = 12u16;
        for (i, st) in s.stages.iter().enumerate() {
            let x = x0 + i as u16 * (cw + gap);
            let failed = matches!(st.exit, Some(c) if c != 0);
            let waiting = !s.done && i > 0 && st.bytes_in == 0 && st.exit.is_none();
            let buffering =
                !s.done && i > 0 && st.bytes_in > 0 && st.bytes_out == 0 && st.exit.is_none();
            let (status, stc) = match st.exit {
                Some(0) => ("✔ exit 0".to_string(), CYAN),
                Some(c) => (format!("✖ exit {c}"), RED),
                None if s.done => ("✔ done".to_string(), CYAN),
                None if waiting => ("… waiting".to_string(), DIM),
                None if buffering => ("◆ buffering".to_string(), AMBER),
                None => ("▶ running".to_string(), GREEN),
            };
            let hot = slow == Some(i) || buffering;
            let border = if failed {
                RED
            } else if hot {
                AMBER
            } else {
                role(Role::Stroke)
            };
            let ki = boxed(buf, Rect::new(x, cy, cw, ch), border, role(Role::Card));
            let iw = ki.width.saturating_sub(2);
            let ix = ki.x + 1;
            put(
                buf,
                ix,
                ki.y,
                &format!(" {} ", i + 1),
                Style::default().bg(role(Role::Badge)).fg(TEXT),
                4,
            );
            let sw = status.chars().count() as u16;
            put(
                buf,
                ix + iw.saturating_sub(sw),
                ki.y,
                &status,
                Style::default().fg(stc),
                sw,
            );
            let (cmd, args) = st.cmd.trim().split_once(' ').unwrap_or((st.cmd.trim(), ""));
            put(
                buf,
                ix,
                ki.y + 1,
                cmd,
                text.add_modifier(Modifier::BOLD),
                iw,
            );
            put(buf, ix, ki.y + 2, args.trim(), dim, iw);
            put(
                buf,
                ki.x,
                ki.y + 3,
                &"─".repeat(ki.width as usize),
                Style::default().fg(role(Role::Stroke)),
                ki.width,
            );
            let kept = if i == 0 {
                "100%".to_string()
            } else if buffering {
                "—".into()
            } else if waiting || st.lines_in == 0 {
                String::new()
            } else {
                pct(st.lines_out, st.lines_in)
            };
            let rows = [
                (
                    "in",
                    if i == 0 {
                        "—".to_string()
                    } else {
                        format!("{} · {} ln", size(st.bytes_in), count(st.lines_in))
                    },
                    text,
                ),
                (
                    "out",
                    format!("{} · {} ln", size(st.bytes_out), count(st.lines_out)),
                    text,
                ),
                (
                    "rate",
                    format!("{}/s", size(rate_now(i))),
                    Style::default().fg(AMBER),
                ),
                ("kept", kept, text),
            ];
            for (k, (label, v, vst)) in rows.iter().enumerate() {
                let y = ki.y + 4 + k as u16;
                put(buf, ix, y, label, dim, 5);
                put(buf, ix + 5, y, v, *vst, iw.saturating_sub(5));
            }
            put(
                buf,
                ix,
                ki.y + 8,
                &spark(self.hist.get(i), (iw as usize).min(12)),
                Style::default().fg(CYAN),
                iw,
            );
            let sample = if st.sample.is_empty() && buffering {
                "(holds until input ends)".to_string()
            } else {
                st.sample.clone()
            };
            if !sample.is_empty() {
                put(
                    buf,
                    ix,
                    ki.y + 9,
                    &format!("▸ {sample}"),
                    dim.add_modifier(Modifier::ITALIC),
                    iw,
                );
            }
            // the arrow to the next stage, with what this one keeps
            if i + 1 < n {
                let ax = x + cw + 1;
                put(buf, ax + 1, cy + 3, "━━▶", Style::default().fg(BLUE), 3);
                let k = if i == 0 {
                    "100%".to_string()
                } else if st.lines_in > 0 {
                    pct(st.lines_out, st.lines_in)
                } else {
                    String::new()
                };
                let kw = k.chars().count() as u16;
                put(
                    buf,
                    ax + (gap - 1).saturating_sub(kw) / 2,
                    cy + 4,
                    &k,
                    dim,
                    kw,
                );
            }
        }

        // ---- bottom: throughput chart | output over stderr
        let by = cy + ch + 1;
        let foot = area.bottom() - 1;
        let bh = foot.saturating_sub(by + 1);
        let lw = (area.width.saturating_sub(4)) * 13 / 23;
        let left = Rect::new(x0, by, lw, bh);
        let li = boxed(buf, left, role(Role::Stroke), Color::Reset);
        let liw = li.width.saturating_sub(2);
        put(
            buf,
            li.x + 1,
            li.y,
            "Throughput between stages",
            text.add_modifier(Modifier::BOLD),
            liw,
        );
        let note = "bytes/s · last 30s";
        put(
            buf,
            li.x + 1 + liw.saturating_sub(note.chars().count() as u16),
            li.y,
            note,
            dim,
            liw,
        );
        let lanes = [CYAN, BLUE, MAGENTA, AMBER];
        let label_w = 18u16;
        let now_w = 11u16;
        let bars_w = liw.saturating_sub(label_w + now_w + 2) as usize;
        let room = li.height.saturating_sub(3);
        let lane_h: u16 = if room >= 3 * (n as u16 - 1).max(1) {
            3
        } else {
            1
        };
        for i in 0..n.saturating_sub(1) {
            let y = li.y + 2 + i as u16 * lane_h;
            if y + lane_h > li.bottom().saturating_sub(1) {
                break;
            }
            let col = lanes[i % lanes.len()];
            let first = |c: &str| c.split_whitespace().next().unwrap_or("").to_string();
            let label = format!(
                "{} {} → {} {}",
                i + 1,
                first(&s.stages[i].cmd),
                i + 2,
                first(&s.stages[i + 1].cmd)
            );
            let ly = y + lane_h.saturating_sub(2).min(1);
            put(buf, li.x + 1, ly, &label, dim, label_w);
            let (top, bottom) = lane_bars(self.hist.get(i), bars_w, lane_h >= 2);
            let bx = li.x + 1 + label_w + 1;
            if lane_h >= 2 {
                put(
                    buf,
                    bx,
                    ly - 1,
                    &top,
                    Style::default().fg(col),
                    bars_w as u16,
                );
            }
            put(
                buf,
                bx,
                ly,
                &bottom,
                Style::default().fg(col),
                bars_w as u16,
            );
            let now = format!("{}/s", size(rate_now(i)));
            let nw = now.chars().count() as u16;
            put(buf, li.right().saturating_sub(nw + 1), ly, &now, text, nw);
        }
        // why it's slow, and the biggest filter
        let fy = li.bottom().saturating_sub(1);
        let mut fx = li.x + 1;
        if let Some(i) = slow {
            let first = |c: &str| c.split_whitespace().next().unwrap_or("").to_string();
            let me = first(&s.stages[i].cmd);
            put(buf, fx, fy, "◀ slowest", Style::default().fg(AMBER), 9);
            fx += 10;
            let why = if i > 0 {
                let prev = first(&s.stages[i - 1].cmd);
                let st = &s.stages[i];
                let holds = !s.done && st.bytes_in > 0 && st.bytes_out == 0;
                format!(
                    "{me} — {prev} waits to push into it{}",
                    if holds {
                        " (it buffers until input ends)"
                    } else {
                        ""
                    }
                )
            } else {
                format!("{me} — the others wait for its output")
            };
            put(buf, fx, fy, &why, text, liw.saturating_sub(10));
            fx += why.chars().count() as u16 + 3;
        }
        if let Some((i, st)) = s
            .stages
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, x)| {
                x.lines_in > 0 && x.lines_out < x.lines_in && (s.done || x.lines_out > 0)
            })
            .min_by(|a, b| {
                (a.1.lines_out as f64 / a.1.lines_in as f64)
                    .total_cmp(&(b.1.lines_out as f64 / b.1.lines_in as f64))
            })
        {
            let first = st.cmd.split_whitespace().next().unwrap_or("");
            let t = format!("{first} keeps {} of lines", pct(st.lines_out, st.lines_in));
            let _ = i;
            put(buf, fx, fy, &t, dim, li.right().saturating_sub(fx + 1));
        }

        let rx = left.right() + 2;
        let rw = area.right().saturating_sub(rx + 1);
        let out_h = (bh * 5 / 8).max(4);
        let out = Rect::new(rx, by, rw, out_h);
        let oi = boxed(buf, out, role(Role::Stroke), Color::Reset);
        let oiw = oi.width.saturating_sub(2);
        put(
            buf,
            oi.x + 1,
            oi.y,
            "Output",
            text.add_modifier(Modifier::BOLD),
            oiw,
        );
        let src = format!("stage {n} · stdout");
        put(
            buf,
            oi.x + 1 + oiw.saturating_sub(src.chars().count() as u16),
            oi.y,
            &src,
            dim,
            oiw,
        );
        let room = oi.height.saturating_sub(2) as usize;
        let mut y = oi.y + 1;
        for t in s.tail.iter().rev().take(room).rev() {
            put(buf, oi.x + 1, y, t, text, oiw);
            y += 1;
        }
        if !s.done && y < oi.bottom() {
            let holder = s
                .stages
                .iter()
                .skip(1)
                .find(|x| x.bytes_in > 0 && x.bytes_out == 0)
                .map(|x| x.cmd.split_whitespace().next().unwrap_or("").to_string());
            if let Some(h) = holder {
                put(
                    buf,
                    oi.x + 1,
                    y,
                    &format!("— waiting for {h} to flush…"),
                    dim,
                    oiw,
                );
            }
        }
        let err = Rect::new(rx, by + out_h, rw, bh.saturating_sub(out_h));
        if err.height >= 3 {
            let ei = boxed(buf, err, role(Role::ErrBorder), role(Role::ErrBg));
            let eiw = ei.width.saturating_sub(2);
            put(
                buf,
                ei.x + 1,
                ei.y,
                "stderr",
                Style::default().fg(RED).add_modifier(Modifier::BOLD),
                6,
            );
            let tag = "all stages, tagged";
            put(
                buf,
                ei.x + 1 + eiw.saturating_sub(tag.chars().count() as u16),
                ei.y,
                tag,
                dim,
                eiw,
            );
            let mut lines: Vec<(String, String, usize)> = Vec::new();
            for (i, st) in s.stages.iter().enumerate() {
                let first = st.cmd.split_whitespace().next().unwrap_or("");
                for e in &st.stderr {
                    let tag = format!("[{} {first}]", i + 1);
                    match lines.last_mut() {
                        Some((t, m, k)) if *t == tag && m == e => *k += 1,
                        _ => lines.push((tag, e.clone(), 1)),
                    }
                }
            }
            let room = ei.height.saturating_sub(1) as usize;
            if lines.is_empty() {
                put(buf, ei.x + 1, ei.y + 1, "nothing on stderr", dim, eiw);
            }
            for (y, (t, m, k)) in (ei.y + 1..).zip(lines.iter().rev().take(room).rev()) {
                let tw = t.chars().count() as u16;
                put(buf, ei.x + 1, y, t, dim, tw);
                put(
                    buf,
                    ei.x + 2 + tw,
                    y,
                    m,
                    Style::default().fg(RED),
                    eiw.saturating_sub(tw + 1),
                );
                if *k > 1 {
                    let c = format!(" ×{k}");
                    let mx = (ei.x + 2 + tw + m.chars().count() as u16)
                        .min(ei.right().saturating_sub(5));
                    put(buf, mx, y, &c, dim, 5);
                }
            }
        }

        // ---- keys
        let mut x = x0;
        for (k, label) in [
            ("s", "Send pipeline to shell"),
            ("r", "Run again with sv flow"),
            ("^C", "Stop (in the shell)"),
            ("F5", "Zoom"),
            ("F8", "Flow pane"),
        ] {
            put(buf, x, foot, k, Style::default().fg(AMBER), 2);
            x += k.chars().count() as u16 + 1;
            put(buf, x, foot, label, dim, label.chars().count() as u16);
            x += label.chars().count() as u16 + 3;
        }
        put(
            buf,
            x,
            foot,
            "summary also printed in the shell when it ends",
            Style::default().fg(FAINT),
            area.right().saturating_sub(x),
        );
    }

    /// Two lines per stage, for narrow panes.
    fn render_flow_compact(&self, s: &FlowStats, area: Rect, buf: &mut Buffer, mut y: u16) -> u16 {
        let w = area.width as usize;
        let slow = s.bottleneck();
        let secs = (s.elapsed_ms as f64 / 1000.0).max(0.001);
        let max_out = s
            .stages
            .iter()
            .map(|x| x.bytes_out)
            .max()
            .unwrap_or(1)
            .max(1);
        for (i, st) in s.stages.iter().enumerate() {
            if y + 1 >= area.bottom().saturating_sub(1) {
                break;
            }
            let failed = matches!(st.exit, Some(c) if c != 0);
            let n = ((st.bytes_out as f64 / max_out as f64) * 8.0)
                .round()
                .max(1.0) as usize;
            let mark = if slow == Some(i) {
                " ◀ slowest".to_string()
            } else if failed {
                format!(" ✖ {}", st.exit.unwrap_or(1))
            } else {
                String::new()
            };
            let right = format!("{}{mark}", "▮".repeat(n));
            let cmd_w = w.saturating_sub(right.chars().count() + 5);
            let cmd: String = st.cmd.chars().take(cmd_w).collect();
            buf.set_stringn(
                area.x,
                y,
                format!(" {} {cmd}", i + 1),
                w,
                if failed {
                    Style::default().fg(RED)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            );
            let rx = area
                .right()
                .saturating_sub(right.chars().count() as u16 + 1);
            buf.set_stringn(
                rx,
                y,
                &right,
                right.chars().count(),
                Style::default().fg(if slow == Some(i) { AMBER } else { CYAN }),
            );
            y += 1;
            let flow = if i == 0 {
                format!("→ {} lines", count(st.lines_out))
            } else if st.lines_in > 0 {
                format!(
                    "{} → {} lines · {:.0}% kept",
                    count(st.lines_in),
                    count(st.lines_out),
                    st.lines_out as f64 / st.lines_in as f64 * 100.0
                )
            } else {
                format!("→ {} lines", count(st.lines_out))
            };
            let detail = format!(
                "    {flow} · {} · {}/s",
                size(st.bytes_out),
                size((st.bytes_out as f64 / secs) as u64)
            );
            buf.set_stringn(area.x, y, detail, w, Style::default().fg(DIM));
            y += 1;
        }
        y
    }

    fn render_flow_tail(&self, s: &FlowStats, area: Rect, buf: &mut Buffer, mut y: u16) {
        let w = area.width as usize;
        // stderr, then output tail
        let errs: Vec<(usize, &String)> = s
            .stages
            .iter()
            .enumerate()
            .flat_map(|(i, st)| st.stderr.iter().map(move |e| (i, e)))
            .collect();
        if !errs.is_empty() && y + 1 < area.bottom() {
            y += 1;
            for (i, e) in errs.iter().rev().take(3).rev() {
                if y >= area.bottom().saturating_sub(1) {
                    break;
                }
                buf.set_stringn(
                    area.x,
                    y,
                    format!(" ! {} {e}", i + 1),
                    w,
                    Style::default().fg(RED),
                );
                y += 1;
            }
        }
        if !s.tail.is_empty() && y + 2 < area.bottom() {
            y += 1;
            buf.set_stringn(area.x, y, " output ›", w, Style::default().fg(DIM));
            y += 1;
            let room = (area.bottom().saturating_sub(1) - y) as usize;
            for t in s.tail.iter().rev().take(room).rev() {
                buf.set_stringn(area.x + 2, y, t, w - 2, Style::default().fg(FAINT));
                y += 1;
            }
        }
        buf.set_stringn(
            area.x,
            area.bottom() - 1,
            " s send to shell · r run again with sv flow",
            w,
            Style::default().fg(FAINT),
        );
    }

    fn render_job(&self, area: Rect, buf: &mut Buffer) {
        let w = area.width as usize;
        let mut y = area.y;
        let secs = self.job_started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let title = if self.job_cmd.is_empty() {
            "running".to_string()
        } else {
            self.job_cmd.clone()
        };
        let (nx, _) = buf.set_stringn(
            area.x,
            y,
            format!(" {title} "),
            w.saturating_sub(12),
            Style::default().add_modifier(Modifier::BOLD),
        );
        buf.set_stringn(
            nx,
            y,
            format!(" {}s", secs),
            area.right().saturating_sub(nx) as usize,
            Style::default().fg(AMBER),
        );
        y += 2;
        let rates = self.job.iter().any(|j| j.read_rate.is_some());
        let head = if rates {
            format!(
                " {:>7} {:>6} {:>9} {:>9} {:>9}  command",
                "pid", "cpu", "mem", "read/s", "write/s"
            )
        } else {
            format!(" {:>7} {:>6} {:>9}  command", "pid", "cpu", "mem")
        };
        buf.set_stringn(
            area.x,
            y,
            head,
            w,
            Style::default().fg(DIM).add_modifier(Modifier::UNDERLINED),
        );
        y += 1;
        let busiest = self
            .job
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.proc.cpu.total_cmp(&b.1.proc.cpu))
            .filter(|(_, j)| j.proc.cpu >= 5.0)
            .map(|(i, _)| i);
        for (i, j) in self.job.iter().enumerate() {
            if y >= area.bottom().saturating_sub(1) {
                break;
            }
            if j.piped_from_prev && y + 1 < area.bottom() {
                buf.set_stringn(
                    area.x + 9,
                    y,
                    "│ pipe",
                    w.saturating_sub(9),
                    Style::default().fg(FAINT),
                );
                y += 1;
            }
            let p = &j.proc;
            let mut row = format!(" {:>7} {:>5.1}% {:>9}", p.pid, p.cpu, size(p.rss_kb * 1024));
            if rates {
                row.push_str(&format!(
                    " {:>9} {:>9}",
                    j.read_rate.map(size).unwrap_or_default(),
                    j.write_rate.map(size).unwrap_or_default()
                ));
            }
            row.push_str(&format!("  {}", p.args));
            let (nx, _) = buf.set_stringn(area.x, y, &row, w, Style::default());
            if busiest == Some(i) && nx + 12 < area.right() {
                buf.set_string(nx + 1, y, "◀ busiest", Style::default().fg(AMBER));
            }
            y += 1;
        }
        buf.set_stringn(
            area.x,
            area.bottom() - 1,
            " meter a pipeline stage by stage: sv flow 'a | b | c'",
            w,
            Style::default().fg(FAINT),
        );
    }
}

/// "0.6%", "25%"
fn pct(part: u64, of: u64) -> String {
    let p = part as f64 / of.max(1) as f64 * 100.0;
    if p < 10.0 && p > 0.0 && p.fract() != 0.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A sparkline of the last `w` rates: "▃▆█▇▆▅▄▂▁".
fn spark(h: Option<&std::collections::VecDeque<u64>>, w: usize) -> String {
    let Some(h) = h else {
        return "▁".repeat(w);
    };
    let take: Vec<u64> = h.iter().rev().take(w).rev().copied().collect();
    let max = take.iter().copied().max().unwrap_or(0).max(1);
    let mut s: String = "▁".repeat(w - take.len());
    for v in take {
        s.push(LEVELS[((v as f64 / max as f64) * 7.0).round() as usize]);
    }
    s
}

/// The throughput chart for one lane, `w` cells wide over the last 30 s:
/// (top row, bottom row) — two rows give 16 heights.
fn lane_bars(h: Option<&std::collections::VecDeque<u64>>, w: usize, two: bool) -> (String, String) {
    let vals: Vec<u64> = h.map(|h| h.iter().copied().collect()).unwrap_or_default();
    let max = vals.iter().copied().max().unwrap_or(0).max(1);
    let (mut top, mut bottom) = (String::new(), String::new());
    for c in 0..w {
        // the last 30 samples spread over the width, newest on the right
        let idx = (c * HIST / w.max(1)) as isize - (HIST as isize - vals.len() as isize);
        let v = if idx >= 0 {
            vals.get(idx as usize).copied().unwrap_or(0)
        } else {
            0
        };
        let levels = if two { 16.0 } else { 8.0 };
        let l = ((v as f64 / max as f64) * levels).round() as usize;
        let l = l.max(1);
        if two {
            if l > 8 {
                bottom.push('█');
                top.push(LEVELS[l - 9]);
            } else {
                bottom.push(LEVELS[l - 1]);
                top.push(' ');
            }
        } else {
            bottom.push(LEVELS[l.min(8) - 1]);
        }
    }
    (top, bottom)
}

/// How the command reads best on screen: in single quotes, or double
/// quotes when it has single quotes inside.
fn display_quote(c: &str) -> String {
    if c.contains('\'') && !c.contains('"') {
        format!("\"{c}\"")
    } else {
        quote(c)
    }
}

fn size(b: u64) -> String {
    crate::data::value::fmt_size(b)
}

fn count(n: u64) -> String {
    match n {
        0..10_000 => n.to_string(),
        10_000..1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flowrun::StageStats;

    fn screen(p: &FlowPane, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn parses_ps() {
        let p = parse_ps(
            "  10     1    10   0.0  1234 /bin/zsh -l\n  11    10    11  12.5  2048 /usr/bin/grep -r foo bar\nbad line\n",
        );
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].name(), "grep");
        assert_eq!(p[1].args, "/usr/bin/grep -r foo bar");
        assert_eq!(p[1].cpu, 12.5);
    }

    #[test]
    fn idle_job_and_flow_views() {
        let mut p = FlowPane::new();
        assert!(screen(&p, 80, 10).contains("sv flow 'cat big.log"));
        p.job = vec![
            JobProc {
                proc: Proc {
                    pid: 20,
                    ppid: 1,
                    pgid: 20,
                    cpu: 50.0,
                    rss_kb: 2048,
                    args: "cat big".into(),
                },
                read_rate: Some(1_000_000),
                write_rate: Some(1_000_000),
                piped_from_prev: false,
            },
            JobProc {
                proc: Proc {
                    pid: 21,
                    ppid: 1,
                    pgid: 20,
                    cpu: 1.0,
                    rss_kb: 1024,
                    args: "grep x".into(),
                },
                read_rate: None,
                write_rate: None,
                piped_from_prev: true,
            },
        ];
        p.job_cmd = "cat big | grep x".into();
        let s = screen(&p, 90, 10);
        assert!(
            s.contains("cat big | grep x") && s.contains("│ pipe") && s.contains("◀ busiest"),
            "{s}"
        );
        assert!(s.contains("1.0 MB"), "{s}");
        let mut stats = FlowStats {
            cmd: "a | b".into(),
            elapsed_ms: 2000,
            done: true,
            ..Default::default()
        };
        stats.stages.push(StageStats {
            cmd: "a".into(),
            lines_out: 100,
            bytes_out: 1000,
            ..Default::default()
        });
        stats.stages.push(StageStats {
            cmd: "b".into(),
            lines_in: 100,
            lines_out: 25,
            bytes_in: 1000,
            bytes_out: 250,
            push_wait_ms: 1500,
            stderr: vec!["b: warning".into()],
            exit: Some(0),
            ..Default::default()
        });
        stats.tail = vec!["last line".into()];
        p.job.clear();
        p.flow = Some((PathBuf::from("/x"), stats));
        let s = screen(&p, 100, 14);
        assert!(s.contains("a | b") && s.contains("finished"), "{s}");
        assert!(
            s.contains("100 → 25") && s.contains("25%") && s.contains("◀ slowest"),
            "{s}"
        );
        assert!(
            s.contains("! 2 b: warning") && s.contains("last line"),
            "{s}"
        );
        assert_eq!(p.key(b"r"), Some(FlowAct::Type("sv flow 'a | b'".into())));
        // a big pane: the boards' layout
        let s = screen(&p, 140, 34);
        assert!(
            s.contains("sv flow 'a | b'") && s.contains("✔ finished") && s.contains("━━▶"),
            "{s}"
        );
        assert!(
            s.contains("Throughput between stages") && s.contains("1 a → 2 b"),
            "{s}"
        );
        assert!(
            s.contains("[2 b] b: warning") && s.contains("b keeps 25% of lines"),
            "{s}"
        );
        assert!(s.contains("Send pipeline to shell"), "{s}");
        assert_eq!(p.title(), "flow · finished");
    }

    #[test]
    fn live_job_of_a_real_process_group() {
        // a child in its own process group, so we can find it by pgid
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        let mut p = FlowPane::new();
        p.sample(Some(pgid), None, "sleep 5", Some(Instant::now()));
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(p.job.len(), 1);
        assert_eq!(p.job[0].proc.name(), "sleep");
    }
}

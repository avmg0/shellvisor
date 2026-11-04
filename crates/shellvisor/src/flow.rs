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
}

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

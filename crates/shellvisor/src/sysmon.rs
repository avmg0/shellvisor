//! The system pane: CPU, memory, swap, disk, network, I/O, load,
//! temperatures and the busiest processes.
//!
//! Sampling happens only while the pane is on screen (every 2 s, or 5 s in
//! low-power mode). When it's hidden nothing is read at all.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(240);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const CYAN: Color = Color::Indexed(73);
const BAR_BG: Color = Color::Indexed(237);

pub const INTERVAL: Duration = Duration::from_secs(2);
pub const INTERVAL_LOW_POWER: Duration = Duration::from_secs(5);
const HISTORY: usize = 120;
const TOP: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Mem,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
}

/// One temperature group: "cpu 52°C".
#[derive(Debug, Clone, PartialEq)]
pub struct Temp {
    pub group: &'static str,
    pub celsius: f32,
}

/// Everything shown, as plain numbers (so drawing is testable without sysinfo).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub cpu: f32,
    pub cores: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load: (f64, f64, f64),
    /// (mount, used, total)
    pub disks: Vec<(String, u64, u64)>,
    /// bytes per second
    pub net_rx: u64,
    pub net_tx: u64,
    pub io_read: u64,
    pub io_write: u64,
    pub temps: Vec<Temp>,
    pub procs: Vec<Proc>,
    pub self_cpu: f32,
    pub self_mem: u64,
    pub nprocs: usize,
}

pub struct SysPane {
    sys: Option<System>,
    disks: Option<Disks>,
    nets: Option<Networks>,
    comps: Option<Components>,
    pub snap: Snapshot,
    pub history: VecDeque<f32>,
    pub sort: Sort,
    pub sel: usize,
    last: Option<Instant>,
    disks_at: Option<Instant>,
    me: Option<Pid>,
}

impl Default for SysPane {
    fn default() -> Self {
        Self::new()
    }
}

impl SysPane {
    pub fn new() -> Self {
        SysPane {
            sys: None,
            disks: None,
            nets: None,
            comps: None,
            snap: Snapshot::default(),
            history: VecDeque::with_capacity(HISTORY),
            sort: Sort::Cpu,
            sel: 0,
            last: None,
            disks_at: None,
            me: sysinfo::get_current_pid().ok(),
        }
    }

    /// When the next sample is due (None = due now).
    pub fn due_in(&self, low_power: bool) -> Duration {
        let every = if low_power {
            INTERVAL_LOW_POWER
        } else {
            INTERVAL
        };
        match self.last {
            None => Duration::ZERO,
            Some(t) => every.saturating_sub(t.elapsed()),
        }
    }

    /// Read everything once. Cheap-ish (a few ms); only called while visible.
    pub fn sample(&mut self) {
        let first = self.sys.is_none();
        let elapsed = self
            .last
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(1.0)
            .max(0.2);
        self.last = Some(Instant::now());
        if self.sys.is_none() {
            self.sys = System::new().ok();
        }
        let Some(sys) = self.sys.as_mut() else {
            return; // not supported here; the pane says "reading…"
        };
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage()
                .without_tasks(),
        );
        let mut s = Snapshot {
            cpu: sys.global_cpu_usage(),
            cores: sys.cpus().iter().map(|c| c.usage()).collect(),
            mem_used: sys.used_memory(),
            mem_total: sys.total_memory(),
            swap_used: sys.used_swap(),
            swap_total: sys.total_swap(),
            nprocs: sys.processes().len(),
            ..Default::default()
        };
        if let Ok(l) = System::load_average() {
            s.load = (l.one, l.five, l.fifteen);
        }
        let ncpu = s.cores.len().max(1) as f32;
        let (mut rd, mut wr) = (0u64, 0u64);
        let mut procs: Vec<Proc> = Vec::with_capacity(sys.processes().len());
        for (pid, p) in sys.processes() {
            let du = p.disk_usage();
            rd += du.read_bytes;
            wr += du.written_bytes;
            // Show CPU as share of the whole machine, like Activity Monitor's
            // "% CPU" divided by the core count — keeps the column ≤ 100.
            let row = Proc {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu: p.cpu_usage() / ncpu,
                mem: p.memory(),
            };
            if Some(*pid) == self.me {
                s.self_cpu = row.cpu;
                s.self_mem = row.mem;
            }
            procs.push(row);
        }
        if !first {
            s.io_read = (rd as f64 / elapsed) as u64;
            s.io_write = (wr as f64 / elapsed) as u64;
        }
        sort_procs(&mut procs, self.sort);
        procs.truncate(TOP);
        s.procs = procs;

        if self.nets.is_none() {
            self.nets = Networks::new_with_refreshed_list().ok();
        } else if let Some(n) = self.nets.as_mut() {
            n.refresh(true);
        }
        if let (false, Some(nets)) = (first, &self.nets) {
            let (rx, tx) = nets
                .iter()
                .filter(|(name, _)| !name.to_string_lossy().starts_with("lo"))
                .fold((0u64, 0u64), |(r, t), (_, d)| {
                    (r + d.received(), t + d.transmitted())
                });
            s.net_rx = (rx as f64 / elapsed) as u64;
            s.net_tx = (tx as f64 / elapsed) as u64;
        }

        // Disks change rarely: re-list every 30 s.
        if self
            .disks_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(30))
        {
            self.disks = Disks::new_with_refreshed_list().ok();
            self.disks_at = Some(Instant::now());
        }
        if let Some(d) = &self.disks {
            s.disks = pick_disks(
                d.iter()
                    .map(|d| {
                        (
                            d.mount_point().display().to_string(),
                            d.total_space().saturating_sub(d.available_space()),
                            d.total_space(),
                        )
                    })
                    .collect(),
            );
        }

        if self.comps.is_none() {
            self.comps = Components::new_with_refreshed_list().ok();
        } else if let Some(c) = self.comps.as_mut() {
            c.refresh(false);
        }
        if let Some(comps) = &self.comps {
            s.temps = group_temps(
                comps
                    .iter()
                    .filter_map(|c| c.temperature().map(|t| (c.label().to_string(), t))),
            );
        }

        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(s.cpu);
        self.snap = s;
        self.sel = self.sel.min(self.snap.procs.len().saturating_sub(1));
    }

    /// Time since the last sample (zero if asleep).
    pub fn idle_for(&self) -> Duration {
        self.last.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// Forget samples (when hidden for long) so memory stays small.
    pub fn sleep(&mut self) {
        self.snap.procs = Vec::new();
        self.sys = None;
        self.last = None;
    }

    /// Handle a key. Returns true if something changed.
    pub fn key(&mut self, key: &[u8]) -> bool {
        let n = self.snap.procs.len();
        match key {
            b"c" => self.set_sort(Sort::Cpu),
            b"m" => self.set_sort(Sort::Mem),
            b"\x1b[A" | b"\x1bOA" | b"k" => self.sel = self.sel.saturating_sub(1),
            b"\x1b[B" | b"\x1bOB" | b"j" => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            b"g" | b"\x1b[H" => self.sel = 0,
            b"G" | b"\x1b[F" => self.sel = n.saturating_sub(1),
            _ => return false,
        }
        true
    }

    pub fn wheel(&mut self, dir: isize) {
        let n = self.snap.procs.len();
        self.sel = if dir > 0 {
            self.sel.saturating_sub(3)
        } else {
            (self.sel + 3).min(n.saturating_sub(1))
        };
    }

    fn set_sort(&mut self, s: Sort) {
        self.sort = s;
        sort_procs(&mut self.snap.procs, s);
        self.sel = 0;
    }

    pub fn title(&self) -> String {
        format!(
            "system · cpu {:.0}% · mem {}",
            self.snap.cpu,
            pct(self.snap.mem_used, self.snap.mem_total)
        )
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, low_power: bool) {
        let s = &self.snap;
        let w = area.width as usize;
        let mut y = area.y;
        let bottom = area.y + area.height;
        let line = |buf: &mut Buffer, y: &mut u16, parts: &[(&str, Style)]| {
            if *y >= bottom {
                return;
            }
            let mut x = area.x;
            for (t, st) in parts {
                if x >= area.x + area.width {
                    break;
                }
                let (nx, _) = buf.set_stringn(x, *y, t, (area.x + area.width - x) as usize, *st);
                x = nx;
            }
            *y += 1;
        };
        let label = Style::default().fg(DIM);
        let val = Style::default().add_modifier(Modifier::BOLD);
        if self.last.is_none() {
            line(buf, &mut y, &[("  reading…", label)]);
            return;
        }

        // CPU + sparkline
        let spark_w = w.saturating_sub(24).min(HISTORY);
        let spark = sparkline(self.history.iter().copied(), spark_w);
        line(
            buf,
            &mut y,
            &[
                ("CPU  ", label),
                (&format!("{:>3.0}% ", s.cpu), val.fg(heat(s.cpu))),
                (&spark, Style::default().fg(AMBER)),
                (
                    &format!("  load {:.1} {:.1} {:.1}", s.load.0, s.load.1, s.load.2),
                    label,
                ),
            ],
        );
        // per-core grid: "▕████░░▏" cells
        if !s.cores.is_empty() {
            let cell = 10usize;
            let per_row = (w / cell).max(1);
            let rows = s.cores.len().div_ceil(per_row);
            let max_rows = if area.height > 18 { 4 } else { 1 };
            for r in 0..rows.min(max_rows) {
                let mut parts: Vec<(String, Style)> = Vec::new();
                for (i, c) in s.cores.iter().enumerate().skip(r * per_row).take(per_row) {
                    parts.push((format!("{i:>2}"), Style::default().fg(FAINT)));
                    parts.push((bar(*c as f64 / 100.0, 5), Style::default().fg(heat(*c))));
                    parts.push((format!("{:>2.0} ", c.min(99.0)), label));
                }
                let refs: Vec<(&str, Style)> =
                    parts.iter().map(|(t, s)| (t.as_str(), *s)).collect();
                line(buf, &mut y, &refs);
            }
        }
        let mem_frac = frac(s.mem_used, s.mem_total);
        let bw = w.saturating_sub(34).clamp(6, 30);
        line(
            buf,
            &mut y,
            &[
                ("MEM  ", label),
                (
                    &bar(mem_frac, bw),
                    Style::default().fg(heat((mem_frac * 100.0) as f32)),
                ),
                (&format!(" {} / {}", gb(s.mem_used), gb(s.mem_total)), val),
                (
                    &format!("   swap {} / {}", gb(s.swap_used), gb(s.swap_total)),
                    label,
                ),
            ],
        );
        for (mount, used, total) in s.disks.iter().take(2) {
            let f = frac(*used, *total);
            line(
                buf,
                &mut y,
                &[
                    ("DISK ", label),
                    (&bar(f, bw), Style::default().fg(heat((f * 100.0) as f32))),
                    (&format!(" {} / {}", gb(*used), gb(*total)), val),
                    (&format!("   {mount}"), label),
                ],
            );
        }
        line(
            buf,
            &mut y,
            &[
                ("NET  ", label),
                (
                    &format!("↓ {}/s  ↑ {}/s", human(s.net_rx), human(s.net_tx)),
                    val.fg(CYAN),
                ),
                ("     IO  ", label),
                (
                    &format!("r {}/s  w {}/s", human(s.io_read), human(s.io_write)),
                    val.fg(CYAN),
                ),
            ],
        );
        let temps = if s.temps.is_empty() {
            "no sensors readable here".to_string()
        } else {
            s.temps
                .iter()
                .map(|t| format!("{} {:.0}°C", t.group, t.celsius))
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let hottest = s.temps.iter().map(|t| t.celsius).fold(0.0f32, f32::max);
        line(
            buf,
            &mut y,
            &[
                ("TEMP ", label),
                (
                    &temps,
                    Style::default().fg(if hottest >= 90.0 {
                        RED
                    } else if hottest >= 75.0 {
                        AMBER
                    } else {
                        GREEN
                    }),
                ),
            ],
        );
        line(
            buf,
            &mut y,
            &[
                ("SELF ", label),
                (
                    &format!(
                        "{} {} · {:.1}% cpu",
                        crate::NAME_LOWER,
                        human(s.self_mem),
                        s.self_cpu
                    ),
                    Style::default().fg(DIM),
                ),
                (
                    if low_power { "   ◌ low-power" } else { "" },
                    Style::default().fg(AMBER),
                ),
            ],
        );

        // process table
        if y + 2 > bottom {
            return;
        }
        let name_w = w.saturating_sub(23).max(8);
        let header = format!(
            "{:>7} {:<name_w$} {:>5} {:>8}",
            "PID",
            format!("NAME ({} procs)", s.nprocs),
            if self.sort == Sort::Cpu {
                "CPU▾"
            } else {
                "CPU"
            },
            if self.sort == Sort::Mem {
                "MEM▾"
            } else {
                "MEM"
            },
        );
        line(
            buf,
            &mut y,
            &[(
                &header,
                Style::default().fg(DIM).add_modifier(Modifier::UNDERLINED),
            )],
        );
        let room = (bottom - y) as usize;
        let start = self.sel.saturating_sub(room.saturating_sub(1));
        for (i, p) in s.procs.iter().enumerate().skip(start).take(room) {
            let name: String = p.name.chars().take(name_w).collect();
            let text = format!(
                "{:>7} {:<name_w$} {:>5.1} {:>8}",
                p.pid,
                name,
                p.cpu,
                human(p.mem)
            );
            let st = if i == self.sel {
                Style::default().bg(BAR_BG).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            line(buf, &mut y, &[(&text, st)]);
        }
        let _ = line;
    }
}

fn sort_procs(v: &mut [Proc], s: Sort) {
    match s {
        Sort::Cpu => v.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem))),
        Sort::Mem => v.sort_by_key(|p| std::cmp::Reverse(p.mem)),
    }
}

/// Real disks only, biggest first, without APFS duplicates (same numbers,
/// different mount points) and system volumes.
pub fn pick_disks(mut v: Vec<(String, u64, u64)>) -> Vec<(String, u64, u64)> {
    v.retain(|(m, _, t)| {
        *t > 0
            && *t < (1u64 << 49) // > 512 TB: a network/virtual mount, not a disk
            && !m.starts_with("/System/Volumes/") // macOS: Data is the same container as /
            && !m.starts_with("/boot")
            && !m.starts_with("/snap/")
            && !m.starts_with("/dev")
            && !m.starts_with("/proc")
            && !m.starts_with("/sys")
            && !m.starts_with("/run")
    });
    v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.len().cmp(&b.0.len())));
    let mut out: Vec<(String, u64, u64)> = Vec::new();
    for d in v {
        if !out.iter().any(|o| o.2 == d.2) {
            out.push(d);
        }
    }
    out
}

/// Group sensor readings into "cpu / gpu / battery / disk / other", hottest of each.
pub fn group_temps(readings: impl Iterator<Item = (String, f32)>) -> Vec<Temp> {
    const GROUPS: &[(&str, &[&str])] = &[
        (
            "cpu",
            &[
                "cpu", "core", "package", "tdie", "tctl", "pacc", "eacc", "soc", "k10temp",
                "coretemp", "x86_pkg",
            ],
        ),
        ("gpu", &["gpu", "amdgpu", "nouveau"]),
        ("battery", &["batt"]),
        ("disk", &["nvme", "ssd", "nand", "disk", "drive"]),
    ];
    let mut out: Vec<Temp> = Vec::new();
    for (label, t) in readings {
        if !(1.0..150.0).contains(&t) {
            continue; // unplugged sensor / garbage
        }
        let l = label.to_ascii_lowercase();
        let group = GROUPS
            .iter()
            .find(|(_, keys)| keys.iter().any(|k| l.contains(k)))
            .map(|(g, _)| *g)
            .unwrap_or("other");
        match out.iter_mut().find(|x| x.group == group) {
            Some(x) => x.celsius = x.celsius.max(t),
            None => out.push(Temp { group, celsius: t }),
        }
    }
    let order = |g: &str| {
        ["cpu", "gpu", "battery", "disk", "other"]
            .iter()
            .position(|x| *x == g)
    };
    out.sort_by_key(|t| order(t.group));
    out
}

pub fn sparkline(
    values: impl DoubleEndedIterator<Item = f32> + ExactSizeIterator,
    width: usize,
) -> String {
    const TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let n = values.len();
    let skip = n.saturating_sub(width);
    let mut s: String = values
        .skip(skip)
        .map(|v| TICKS[((v.clamp(0.0, 100.0) / 100.0 * 7.0).round() as usize).min(7)])
        .collect();
    let have = s.chars().count();
    if have < width {
        s = " ".repeat(width - have) + &s;
    }
    s
}

pub fn bar(frac: f64, width: usize) -> String {
    let f = frac.clamp(0.0, 1.0);
    let full = (f * width as f64).round() as usize;
    format!(
        "{}{}",
        "█".repeat(full),
        "░".repeat(width - full.min(width))
    )
}

fn frac(a: u64, b: u64) -> f64 {
    if b == 0 { 0.0 } else { a as f64 / b as f64 }
}

fn pct(a: u64, b: u64) -> String {
    format!("{:.0}%", frac(a, b) * 100.0)
}

fn heat(p: f32) -> Color {
    if p >= 85.0 {
        RED
    } else if p >= 60.0 {
        AMBER
    } else {
        GREEN
    }
}

fn gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / 1_073_741_824.0)
}

/// 1536 → "1.5K", 3_500_000 → "3.3M"
pub fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human(512), "512B");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(human(3 * 1024 * 1024), "3.0M");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(2.0, 3), "███");
        assert_eq!(sparkline([0.0, 50.0, 100.0].into_iter(), 5), "  ▁▅█");
        assert_eq!(
            sparkline([10.0, 20.0, 30.0].into_iter(), 2).chars().count(),
            2
        );
    }

    #[test]
    fn temperature_groups() {
        let t = group_temps(
            [
                ("PMU tdie1".to_string(), 51.0),
                ("pACC MTR Temp Sensor2".to_string(), 58.5),
                ("GPU MTR Temp Sensor1".to_string(), 44.0),
                ("gas gauge battery".to_string(), 31.0),
                ("NAND CH0 temp".to_string(), 36.0),
                ("acpitz".to_string(), 40.0),
                ("broken".to_string(), -127.0),
            ]
            .into_iter(),
        );
        let g: Vec<(&str, f32)> = t.iter().map(|t| (t.group, t.celsius)).collect();
        assert_eq!(
            g,
            vec![
                ("cpu", 58.5),
                ("gpu", 44.0),
                ("battery", 31.0),
                ("disk", 36.0),
                ("other", 40.0)
            ]
        );
    }

    #[test]
    fn disks_without_duplicates() {
        let d = pick_disks(vec![
            ("/".into(), 10, 500),
            ("/System/Volumes/Data".into(), 200, 500),
            ("/Volumes/USB".into(), 1, 64),
            ("/boot/efi".into(), 1, 1),
            ("/home".into(), 5, 500),
        ]);
        assert_eq!(
            d,
            vec![
                ("/".to_string(), 10, 500),
                ("/Volumes/USB".to_string(), 1, 64)
            ]
        );
    }

    #[test]
    fn real_sample_and_render() {
        let mut p = SysPane::new();
        p.sample();
        p.sample();
        assert!(p.snap.mem_total > 0);
        assert!(!p.snap.cores.is_empty());
        assert!(p.snap.self_mem > 0, "finds itself in the process list");
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf, false);
        let text: String = (0..24)
            .map(|y| {
                (0..80)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        for want in ["CPU", "MEM", "NET", "TEMP", "SELF", "PID"] {
            assert!(text.contains(want), "{want} missing:\n{text}");
        }
        assert!(p.key(b"m"));
        assert_eq!(p.sort, Sort::Mem);
        assert!(!p.key(b"z"));
    }
}

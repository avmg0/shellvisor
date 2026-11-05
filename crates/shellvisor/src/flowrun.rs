//! `sv flow 'cat big.log | grep ERROR | sort | uniq -c'` — run a pipeline
//! with a meter between every stage.
//!
//! Each stage runs as `sh -c <stage>`; Shellvisor-owned copier threads move
//! the data from one stage to the next, counting bytes and lines and timing
//! how long each side waits. The stage everyone waits for is the bottleneck.
//! Inside Shellvisor the numbers are written to a small JSON file four times a
//! second for the Flow pane; at the end a summary is printed.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StageStats {
    pub cmd: String,
    pub pid: u32,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub lines_in: u64,
    pub lines_out: u64,
    /// Time the stage before it spent blocked writing into it (it was slow to read).
    pub push_wait_ms: u64,
    /// Time the next copier spent waiting for its output (it was slow to write).
    pub pull_wait_ms: u64,
    pub stderr: Vec<String>,
    pub exit: Option<i32>,
    /// The latest whole line it wrote (shown on its card).
    #[serde(default)]
    pub sample: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FlowStats {
    pub cmd: String,
    pub elapsed_ms: u64,
    pub done: bool,
    pub stages: Vec<StageStats>,
    /// Last lines of the final output.
    pub tail: Vec<String>,
}

impl FlowStats {
    /// The stage holding the pipeline back, if one clearly is.
    pub fn bottleneck(&self) -> Option<usize> {
        let n = self.stages.len();
        if n < 2 || self.elapsed_ms < 200 {
            return None;
        }
        // A slow stage makes the one before it wait to push, and the ones after
        // it wait to pull. The first stage can only show up as the latter.
        let score = |i: usize| -> u64 {
            let s = &self.stages[i];
            if i == 0 {
                self.stages.get(1).map(|_| s.pull_wait_ms).unwrap_or(0) / 2
            } else {
                s.push_wait_ms
            }
        };
        let (i, best) = (0..n).map(|i| (i, score(i))).max_by_key(|x| x.1)?;
        (best * 10 > self.elapsed_ms).then_some(i)
    }
}

/// Split a pipeline into stage command strings at `|` (outside quotes).
pub fn split_stages(line: &str) -> Result<Vec<String>, String> {
    let mut stages = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), '\\') | (None, '\\') => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, '|') => {
                if chars.peek() == Some(&'|') {
                    return Err(
                        "`||` isn't supported in sv flow — use one pipeline of `|` stages".into(),
                    );
                }
                stages.push(std::mem::take(&mut cur).trim().to_string());
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    stages.push(cur.trim().to_string());
    if stages.iter().any(String::is_empty) {
        return Err("empty stage (a `|` with nothing next to it)".into());
    }
    Ok(stages)
}

type Shared = Arc<Mutex<FlowStats>>;

/// The last complete line in a chunk (up to 160 characters).
fn last_line(b: &[u8]) -> Option<String> {
    let end = b.iter().rposition(|&c| c == b'\n')?;
    let start = b[..end]
        .iter()
        .rposition(|&c| c == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let l = String::from_utf8_lossy(&b[start..end]);
    let l = l.trim_end_matches('\r').trim();
    (!l.is_empty()).then(|| l.chars().take(160).collect())
}

fn count_lines(b: &[u8]) -> u64 {
    b.iter().filter(|&&c| c == b'\n').count() as u64
}

/// Move bytes from `from` to `to`, metering stage `src` (output) and `dst`
/// (input). `dst = None` means the final output (our stdout).
fn copier(
    mut from: impl Read,
    mut to: Box<dyn Write + Send>,
    stats: Shared,
    src: usize,
    dst: Option<usize>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut tail = String::new();
    loop {
        let t = Instant::now();
        let n = match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let pulled = t.elapsed();
        let lines = count_lines(&buf[..n]);
        let sample = last_line(&buf[..n]);
        let t = Instant::now();
        let ok = to
            .write_all(&buf[..n])
            .and_then(|_| if dst.is_none() { to.flush() } else { Ok(()) })
            .is_ok();
        let pushed = t.elapsed();
        {
            let mut s = stats.lock().unwrap();
            let st = &mut s.stages[src];
            st.bytes_out += n as u64;
            st.lines_out += lines;
            if let Some(l) = sample {
                st.sample = l;
            }
            st.pull_wait_ms += pulled.as_millis() as u64;
            match dst {
                Some(d) => {
                    let d = &mut s.stages[d];
                    d.bytes_in += n as u64;
                    d.lines_in += lines;
                    d.push_wait_ms += pushed.as_millis() as u64;
                }
                None => {
                    tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if tail.len() > 4096 {
                        let mut cut = tail.len() - 2048;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail.drain(..cut);
                    }
                    s.tail = tail
                        .lines()
                        .rev()
                        .take(5)
                        .map(String::from)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                }
            }
        }
        if !ok {
            break; // the next stage went away (e.g. `head`)
        }
    }
    // dropping `to` closes the next stage's stdin
}

fn stderr_reader(from: impl Read, stats: Shared, i: usize) {
    for line in std::io::BufReader::new(from).lines().map_while(Result::ok) {
        eprintln!("{line}");
        let mut s = stats.lock().unwrap();
        let e = &mut s.stages[i].stderr;
        e.push(line.chars().take(200).collect());
        if e.len() > 3 {
            e.remove(0);
        }
    }
}

fn write_stats(path: &PathBuf, s: &FlowStats) {
    if let Ok(text) = serde_json::to_string(s) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn human(b: u64) -> String {
    crate::data::value::fmt_size(b)
}

fn lines(n: u64) -> String {
    match n {
        0..10_000 => n.to_string(),
        10_000..1_000_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// One line per stage for the end-of-run summary (and tests).
pub fn summary(s: &FlowStats) -> String {
    let slow = s.bottleneck();
    let secs = (s.elapsed_ms as f64 / 1000.0).max(0.001);
    let mut out = format!(
        "flow · {} stage{} · {:.1}s\n",
        s.stages.len(),
        if s.stages.len() == 1 { "" } else { "s" },
        s.elapsed_ms as f64 / 1000.0
    );
    let w = s
        .stages
        .iter()
        .map(|x| x.cmd.chars().count())
        .max()
        .unwrap_or(0)
        .min(36);
    for (i, st) in s.stages.iter().enumerate() {
        let cmd: String = st.cmd.chars().take(w).collect();
        let kept = if i > 0 && st.lines_in > 0 {
            format!(
                "  {:.0}% kept",
                st.lines_out as f64 / st.lines_in as f64 * 100.0
            )
        } else {
            String::new()
        };
        let exit = match st.exit {
            Some(0) | None => String::new(),
            Some(c) => format!("  ✖ exit {c}"),
        };
        out.push_str(&format!(
            "  {}  {cmd:<w$}  {:>6} lines  {:>9}  {:>9}/s{kept}{}{exit}\n",
            i + 1,
            lines(st.lines_out),
            human(st.bytes_out),
            human((st.bytes_out as f64 / secs) as u64),
            if slow == Some(i) { "  ◀ slowest" } else { "" },
        ));
    }
    out
}

/// Saved pipelines: `~/.config/shellvisor/flows.yaml` (name → pipeline).
fn flows_path() -> std::path::PathBuf {
    crate::paths::config_file()
        .parent()
        .map(|d| d.join("flows.yaml"))
        .unwrap_or_else(|| std::path::PathBuf::from("flows.yaml"))
}

pub fn saved() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(flows_path())
        .ok()
        .and_then(|t| serde_yaml_ng::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_all(m: &std::collections::BTreeMap<String, String>) -> std::io::Result<()> {
    let p = flows_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_yaml_ng::to_string(m).unwrap_or_default())
}

/// `sv flow save NAME '<pipeline>'`, `sv flow list`, `sv flow rm NAME`, or
/// `sv flow NAME` to run a saved one. Returns None for a plain pipeline.
fn saved_cmd(args: &[String]) -> Option<Result<String, i32>> {
    match args.first().map(String::as_str) {
        Some("save") => {
            let (Some(name), true) = (args.get(1), args.len() > 2) else {
                eprintln!("usage: sv flow save NAME 'cmd1 | cmd2'");
                return Some(Err(2));
            };
            let line = args[2..].join(" ");
            if let Err(e) = split_stages(&line) {
                eprintln!("sv flow: {e}");
                return Some(Err(2));
            }
            let mut m = saved();
            m.insert(name.clone(), line.clone());
            match save_all(&m) {
                Ok(()) => {
                    println!("saved {name}: {line} · sv flow {name} runs it");
                    Some(Err(0))
                }
                Err(e) => {
                    eprintln!("sv flow: {e}");
                    Some(Err(1))
                }
            }
        }
        Some("list" | "ls") if args.len() == 1 => {
            let m = saved();
            if m.is_empty() {
                println!("no saved pipelines · sv flow save NAME 'cmd1 | cmd2'");
            }
            for (n, l) in &m {
                println!("{n:<16} {l}");
            }
            Some(Err(0))
        }
        Some("rm" | "remove") => {
            let Some(name) = args.get(1) else {
                eprintln!("usage: sv flow rm NAME");
                return Some(Err(2));
            };
            let mut m = saved();
            if m.remove(name).is_none() {
                eprintln!("sv flow: no saved pipeline `{name}`");
                return Some(Err(1));
            }
            let _ = save_all(&m);
            println!("removed {name}");
            Some(Err(0))
        }
        Some(name) if args.len() == 1 && !name.contains(' ') => saved().get(name).cloned().map(Ok),
        _ => None,
    }
}

/// `sv flow '<pipeline>'`.
pub fn run(args: &[String]) -> i32 {
    let line = match saved_cmd(args) {
        Some(Ok(l)) => l,
        Some(Err(code)) => return code,
        None => args.join(" "),
    };
    if line.trim().is_empty() {
        eprintln!("usage: sv flow 'cmd1 | cmd2 | cmd3'   (quote the pipeline)");
        return 2;
    }
    let stages = match split_stages(&line) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sv flow: {e}");
            return 2;
        }
    };
    let n = stages.len();
    let stats: Shared = Arc::new(Mutex::new(FlowStats {
        cmd: line.clone(),
        stages: stages
            .iter()
            .map(|c| StageStats {
                cmd: c.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }));

    // Start every stage.
    let mut children = Vec::new();
    for (i, cmd) in stages.iter().enumerate() {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c.stdin(if i == 0 {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        match c.spawn() {
            Ok(child) => {
                stats.lock().unwrap().stages[i].pid = child.id();
                children.push(child);
            }
            Err(e) => {
                eprintln!("sv flow: cannot start `{cmd}`: {e}");
                for mut ch in children {
                    let _ = ch.kill();
                }
                return 1;
            }
        }
    }

    let mut threads = Vec::new();
    // Wire stage i → i+1, and the last stage → our stdout.
    let mut stdins: Vec<Option<std::process::ChildStdin>> =
        children.iter_mut().map(|c| c.stdin.take()).collect();
    for i in 0..n {
        let out = children[i].stdout.take().expect("piped stdout");
        let to: Box<dyn Write + Send> = if i + 1 < n {
            Box::new(stdins[i + 1].take().expect("piped stdin"))
        } else {
            Box::new(std::io::stdout())
        };
        let st = stats.clone();
        let dst = (i + 1 < n).then_some(i + 1);
        threads.push(std::thread::spawn(move || copier(out, to, st, i, dst)));
        let err = children[i].stderr.take().expect("piped stderr");
        let st = stats.clone();
        threads.push(std::thread::spawn(move || stderr_reader(err, st, i)));
    }

    // Live stats for the Flow pane.
    let started = Instant::now();
    let path = crate::cli::inside().then(|| {
        let dir = crate::paths::state_dir().join("flow");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("{}.json", std::process::id()))
    });
    if let Some(p) = &path {
        write_stats(p, &stats.lock().unwrap());
        let seq = sv_proto::flow_sequence(&p.display().to_string());
        if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            let _ = tty.write_all(seq.as_bytes());
        }
    }
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ticker = path.clone().map(|p| {
        let st = stats.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(250));
                let mut s = st.lock().unwrap().clone();
                s.elapsed_ms = started.elapsed().as_millis() as u64;
                write_stats(&p, &s);
            }
        })
    });

    let mut codes = Vec::new();
    for mut c in children {
        codes.push(c.wait().ok().and_then(|s| s.code()).unwrap_or(1));
    }
    for t in threads {
        let _ = t.join();
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(t) = ticker {
        let _ = t.join();
    }
    let final_stats = {
        let mut s = stats.lock().unwrap();
        s.elapsed_ms = started.elapsed().as_millis() as u64;
        s.done = true;
        for (i, c) in codes.iter().enumerate() {
            s.stages[i].exit = Some(*c);
        }
        s.clone()
    };
    if let Some(p) = &path {
        write_stats(p, &final_stats);
    }
    // Summary on the terminal (stderr), not into a redirect of stdout.
    let _ = std::io::stdout().flush();
    let text = summary(&final_stats);
    match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        Ok(mut tty) => {
            let _ = tty.write_all(format!("\x1b[38;5;244m{text}\x1b[0m").as_bytes());
        }
        Err(_) => eprint!("{text}"),
    }
    *codes.last().unwrap_or(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_split_outside_quotes() {
        assert_eq!(
            split_stages("cat 'a|b' | grep \"x|y\" | wc -l").unwrap(),
            vec!["cat 'a|b'", "grep \"x|y\"", "wc -l"]
        );
        assert!(split_stages("a || b").is_err());
        assert!(split_stages("a |").is_err());
    }

    #[test]
    fn bottleneck_and_summary() {
        let mut s = FlowStats {
            cmd: "a | b | c".into(),
            elapsed_ms: 10_000,
            ..Default::default()
        };
        for (cmd, push, out) in [("a", 0, 1000), ("b", 6000, 100), ("c", 0, 100)] {
            s.stages.push(StageStats {
                cmd: cmd.into(),
                push_wait_ms: push,
                lines_in: 1000,
                lines_out: out,
                bytes_out: out * 10,
                ..Default::default()
            });
        }
        assert_eq!(s.bottleneck(), Some(1));
        let text = summary(&s);
        assert!(text.contains("flow · 3 stages · 10.0s"), "{text}");
        assert!(
            text.lines().nth(2).unwrap().contains("10% kept  ◀ slowest"),
            "{text}"
        );
        s.stages[1].push_wait_ms = 10;
        assert_eq!(s.bottleneck(), None);
    }
}

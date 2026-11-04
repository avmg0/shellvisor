//! Where tables come from: files, processes, the system, git, history…

use super::format;
use super::value::{Data, Record, Value};
use std::path::Path;
use std::process::Command;

pub const SOURCES: &[(&str, &str)] = &[
    ("ls", "files in a folder: ls [path…] [-a]"),
    ("ps", "running processes"),
    ("host", "this machine: os, cpu, memory, uptime…"),
    ("df", "disks and free space"),
    ("ports", "TCP ports that are listening, and who listens"),
    ("env", "environment variables"),
    ("history", "commands run in Shellvisor: history [n]"),
    (
        "open",
        "read a file: .json .jsonl .csv .tsv .yaml .toml, else lines",
    ),
    ("git-log", "recent commits: git-log [n]"),
    ("git-status", "changed files in this repository"),
];

pub fn is_source(name: &str) -> bool {
    SOURCES.iter().any(|(n, _)| *n == name)
}

fn s(v: impl Into<String>) -> Value {
    Value::Str(v.into())
}

pub fn source(cmd: &str, args: &[String]) -> Result<Data, String> {
    match cmd {
        "ls" => ls(args),
        "ps" => Ok(ps()),
        "host" => Ok(host()),
        "df" => Ok(df()),
        "ports" => Ok(ports()),
        "env" => {
            let mut v: Vec<(String, String)> = std::env::vars().collect();
            v.sort();
            Ok(Data::Table(
                v.into_iter()
                    .map(|(k, val)| vec![("name".into(), s(k)), ("value".into(), s(val))])
                    .collect(),
            ))
        }
        "history" => history(args),
        "open" => open(args),
        "git-log" => git_log(args),
        "git-status" => git_status(),
        other => Err(format!("unknown source `{other}`")),
    }
}

fn mtime(m: &std::fs::Metadata) -> Value {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| Value::Date(d.as_secs() as i64))
        .unwrap_or(Value::Nothing)
}

fn file_row(name: String, path: &Path) -> Option<Record> {
    let lm = std::fs::symlink_metadata(path).ok()?;
    let m = std::fs::metadata(path).unwrap_or(lm.clone());
    let kind = if lm.file_type().is_symlink() {
        "link"
    } else if m.is_dir() {
        "dir"
    } else {
        "file"
    };
    Some(vec![
        ("name".into(), s(name)),
        ("type".into(), s(kind)),
        ("size".into(), Value::Size(m.len())),
        ("modified".into(), mtime(&m)),
    ])
}

fn ls(args: &[String]) -> Result<Data, String> {
    let all = args
        .iter()
        .any(|a| a == "-a" || a == "--all" || a == "-la" || a == "-al");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let paths: Vec<String> = if paths.is_empty() {
        vec![".".into()]
    } else {
        paths.into_iter().cloned().collect()
    };
    let mut rows = Vec::new();
    let several = paths.len() > 1;
    for p in &paths {
        let path = Path::new(p);
        let meta = std::fs::metadata(path).map_err(|e| format!("ls: {p}: {e}"))?;
        if meta.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(path)
                .map_err(|e| format!("ls: {p}: {e}"))?
                .flatten()
                .collect();
            entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                if !all && name.starts_with('.') {
                    continue;
                }
                let shown = if several || p != "." {
                    format!("{}/{name}", p.trim_end_matches('/'))
                } else {
                    name
                };
                if let Some(r) = file_row(shown, &e.path()) {
                    rows.push(r);
                }
            }
        } else if let Some(r) = file_row(p.clone(), path) {
            rows.push(r);
        }
    }
    Ok(Data::Table(rows))
}

fn ps() -> Data {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let Ok(mut sys) = System::new() else {
        return Data::Table(Vec::new());
    };
    let kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .without_tasks();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    let mut procs: Vec<_> = sys.processes().iter().collect();
    procs.sort_by_key(|(pid, _)| **pid);
    Data::Table(
        procs
            .into_iter()
            .map(|(pid, p)| {
                vec![
                    ("pid".into(), Value::Int(pid.as_u32() as i64)),
                    (
                        "ppid".into(),
                        p.parent()
                            .map(|x| Value::Int(x.as_u32() as i64))
                            .unwrap_or(Value::Nothing),
                    ),
                    ("name".into(), s(p.name().to_string_lossy())),
                    (
                        "cpu".into(),
                        Value::Float((p.cpu_usage() as f64 * 10.0).round() / 10.0),
                    ),
                    ("mem".into(), Value::Size(p.memory())),
                    (
                        "status".into(),
                        s(format!("{:?}", p.status()).to_lowercase()),
                    ),
                ]
            })
            .collect(),
    )
}

fn host() -> Data {
    use sysinfo::System;
    let mut r: Record = Vec::new();
    r.push((
        "host".into(),
        System::host_name().map(s).unwrap_or(Value::Nothing),
    ));
    r.push((
        "os".into(),
        s(format!(
            "{} {}",
            System::name().unwrap_or_default(),
            System::os_version().unwrap_or_default()
        )
        .trim()
        .to_string()),
    ));
    r.push((
        "kernel".into(),
        System::kernel_version().map(s).unwrap_or(Value::Nothing),
    ));
    r.push(("arch".into(), s(System::cpu_arch())));
    if let Ok(up) = System::uptime() {
        r.push(("uptime".into(), Value::Duration(up as i64 * 1000)));
    }
    if let Ok(mut sys) = System::new() {
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        std::thread::sleep(std::time::Duration::from_millis(200));
        sys.refresh_cpu_usage();
        r.push(("cpus".into(), Value::Int(sys.cpus().len() as i64)));
        if let Some(b) = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|b| !b.is_empty())
        {
            r.push(("cpu".into(), s(b)));
        }
        r.push((
            "cpu_use".into(),
            Value::Float((sys.global_cpu_usage() as f64 * 10.0).round() / 10.0),
        ));
        r.push(("mem_used".into(), Value::Size(sys.used_memory())));
        r.push(("mem_total".into(), Value::Size(sys.total_memory())));
        r.push(("swap_used".into(), Value::Size(sys.used_swap())));
        r.push(("swap_total".into(), Value::Size(sys.total_swap())));
    }
    if let Ok(l) = System::load_average() {
        r.push((
            "load".into(),
            s(format!("{:.2} {:.2} {:.2}", l.one, l.five, l.fifteen)),
        ));
    }
    Data::Value(Value::Record(r))
}

fn df() -> Data {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else {
        return Data::Table(Vec::new());
    };
    Data::Table(
        disks
            .iter()
            .filter(|d| d.total_space() > 0)
            .map(|d| {
                let total = d.total_space();
                let free = d.available_space();
                let used = total.saturating_sub(free);
                vec![
                    ("mount".into(), s(d.mount_point().display().to_string())),
                    ("device".into(), s(d.name().to_string_lossy())),
                    ("fs".into(), s(d.file_system().to_string_lossy())),
                    ("used".into(), Value::Size(used)),
                    ("free".into(), Value::Size(free)),
                    ("total".into(), Value::Size(total)),
                    (
                        "use%".into(),
                        Value::Float((used as f64 / total as f64 * 1000.0).round() / 10.0),
                    ),
                ]
            })
            .collect(),
    )
}

fn port_row(proto: &str, addr: &str, port: i64, pid: Option<i64>, name: Option<String>) -> Record {
    vec![
        ("proto".into(), s(proto)),
        ("port".into(), Value::Int(port)),
        ("address".into(), s(addr)),
        ("pid".into(), pid.map(Value::Int).unwrap_or(Value::Nothing)),
        ("process".into(), name.map(s).unwrap_or(Value::Nothing)),
    ]
}

fn ports() -> Data {
    // lsof works on macOS and most Linux boxes; /proc is the Linux fallback.
    if let Ok(out) = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-FpcPn"])
        .stderr(std::process::Stdio::null())
        .output()
    {
        if out.status.success() || !out.stdout.is_empty() {
            return Data::Table(parse_lsof(&String::from_utf8_lossy(&out.stdout)));
        }
    }
    Data::Table(proc_net_ports())
}

/// `lsof -F pcPn` field output: p<pid> c<command> P<proto> n<addr:port>.
pub fn parse_lsof(text: &str) -> Vec<Record> {
    let (mut pid, mut cmd, mut proto) = (None, None, String::from("TCP"));
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        let (tag, val) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = val.parse::<i64>().ok(),
            "c" => cmd = Some(val.to_string()),
            "P" => proto = val.to_string(),
            "n" => {
                let Some((addr, port)) = val.rsplit_once(':') else {
                    continue;
                };
                let Ok(port) = port.parse::<i64>() else {
                    continue;
                };
                if seen.insert((pid, port, addr.to_string())) {
                    rows.push(port_row(
                        &proto.to_lowercase(),
                        addr.trim_matches(['[', ']']),
                        port,
                        pid,
                        cmd.clone(),
                    ));
                }
            }
            _ => {}
        }
    }
    rows.sort_by_key(|r| match &r[1].1 {
        Value::Int(p) => *p,
        _ => 0,
    });
    rows
}

fn proc_net_ports() -> Vec<Record> {
    let mut owners = std::collections::HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i64>() else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else {
                continue;
            };
            let name = std::fs::read_to_string(e.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_string();
            for fd in fds.flatten() {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    let l = link.to_string_lossy();
                    if let Some(inode) =
                        l.strip_prefix("socket:[").and_then(|x| x.strip_suffix(']'))
                    {
                        owners.insert(inode.to_string(), (pid, name.clone()));
                    }
                }
            }
        }
    }
    let mut rows = Vec::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 || f[3] != "0A" {
                continue; // 0A = LISTEN
            }
            let Some((hex_addr, hex_port)) = f[1].split_once(':') else {
                continue;
            };
            let port = i64::from_str_radix(hex_port, 16).unwrap_or(0);
            let addr = if v6 {
                if hex_addr.chars().all(|c| c == '0') {
                    "::".to_string()
                } else {
                    "::?".to_string()
                }
            } else {
                let n = u32::from_str_radix(hex_addr, 16).unwrap_or(0);
                std::net::Ipv4Addr::from(n.to_le_bytes()).to_string()
            };
            let owner = owners.get(f[9]);
            rows.push(port_row(
                if v6 { "tcp6" } else { "tcp" },
                &addr,
                port,
                owner.map(|o| o.0),
                owner.map(|o| o.1.clone()),
            ));
        }
    }
    rows.sort_by_key(|r| match &r[1].1 {
        Value::Int(p) => *p,
        _ => 0,
    });
    rows
}

fn history(args: &[String]) -> Result<Data, String> {
    let n: usize = match args.first() {
        Some(a) => a
            .parse()
            .map_err(|_| format!("history: `{a}` is not a number"))?,
        None => 100,
    };
    let path = crate::paths::state_dir().join("history.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut rows = Vec::new();
    for l in &lines[start..] {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let get = |k: &str| j.get(k);
        rows.push(vec![
            (
                "command".into(),
                s(get("cmd").and_then(|v| v.as_str()).unwrap_or_default()),
            ),
            (
                "exit".into(),
                get("exit")
                    .and_then(|v| v.as_i64())
                    .map(Value::Int)
                    .unwrap_or(Value::Nothing),
            ),
            (
                "duration".into(),
                get("duration_ms")
                    .and_then(|v| v.as_i64())
                    .map(Value::Duration)
                    .unwrap_or(Value::Nothing),
            ),
            (
                "time".into(),
                get("ts")
                    .and_then(|v| v.as_i64())
                    .map(Value::Date)
                    .unwrap_or(Value::Nothing),
            ),
            (
                "cwd".into(),
                s(get("cwd").and_then(|v| v.as_str()).unwrap_or_default()),
            ),
        ]);
    }
    Ok(Data::Table(rows))
}

fn open(args: &[String]) -> Result<Data, String> {
    let raw = args.iter().any(|a| a == "--raw");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("open: which file? e.g. `open package.json`")?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("open: {path}: {e}"))?;
    if raw {
        return Ok(Data::Value(Value::Str(text)));
    }
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let r = match ext.as_str() {
        "json" | "jsonl" | "ndjson" | "geojson" => format::parse_json(&text),
        "csv" => format::parse_csv(&text, ','),
        "tsv" | "tab" => format::parse_csv(&text, '\t'),
        "yaml" | "yml" => format::parse_yaml(&text),
        "toml" => format::parse_toml(&text),
        _ => Ok(format::parse_lines(&text)),
    };
    r.map_err(|e| format!("open: {path}: {e}"))
}

fn git(args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_log(args: &[String]) -> Result<Data, String> {
    let n = args.first().map(String::as_str).unwrap_or("20");
    n.parse::<usize>()
        .map_err(|_| format!("git-log: `{n}` is not a number"))?;
    let text = git(&["log", "-n", n, "--format=%h%x1f%an%x1f%at%x1f%s"])?;
    Ok(Data::Table(
        text.lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\x1f').collect();
                (f.len() == 4).then(|| {
                    vec![
                        ("commit".into(), s(f[0])),
                        ("author".into(), s(f[1])),
                        (
                            "date".into(),
                            f[2].parse().map(Value::Date).unwrap_or(Value::Nothing),
                        ),
                        ("message".into(), s(f[3])),
                    ]
                })
            })
            .collect(),
    ))
}

fn git_status() -> Result<Data, String> {
    let text = git(&["status", "--porcelain=v1", "-z"])?;
    let mut rows = Vec::new();
    let mut fields = text.split('\0').filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let (xy, path) = f.split_at(3);
        let xy = xy.trim_end();
        let status = match xy.trim() {
            "??" => "untracked",
            x if x.contains('U') || x == "AA" || x == "DD" => "conflict",
            x if x.starts_with('R') => {
                fields.next();
                "renamed"
            }
            x if x.starts_with('A') => "added",
            x if x.contains('D') => "deleted",
            _ => "modified",
        };
        rows.push(vec![
            ("status".into(), s(status)),
            (
                "staged".into(),
                Value::Bool(!xy.starts_with(' ') && !xy.starts_with('?')),
            ),
            ("path".into(), s(path)),
        ]);
    }
    Ok(Data::Table(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_lists_a_folder() {
        let d = std::env::temp_dir().join(format!("sv-data-ls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.txt"), "hello").unwrap();
        std::fs::write(d.join(".hidden"), "x").unwrap();
        let Data::Table(rows) = ls(&[d.display().to_string()]).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
        assert!(rows[0][0].1.plain().ends_with("/a.txt"));
        assert_eq!(rows[0][2].1, Value::Size(5));
        assert_eq!(rows[1][1].1, Value::Str("dir".into()));
        let Data::Table(rows) = ls(&[d.display().to_string(), "-a".into()]).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 3);
        assert!(ls(&["/definitely/missing".into()]).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn lsof_fields() {
        let text =
            "p123\ncnode\nPTCP\nn*:3000\nn[::1]:3000\np77\ncpostgres\nPTCP\nn127.0.0.1:5432\n";
        let rows = parse_lsof(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][1].1, Value::Int(3000));
        assert_eq!(rows[0][4].1, Value::Str("node".into()));
        assert_eq!(rows[2][2].1, Value::Str("127.0.0.1".into()));
    }

    #[test]
    fn system_sources_have_rows() {
        assert!(matches!(ps(), Data::Table(r) if !r.is_empty()));
        let Data::Value(Value::Record(r)) = host() else {
            panic!()
        };
        assert!(r.iter().any(|(k, _)| k == "mem_total"));
        assert!(matches!(source("env", &[]).unwrap(), Data::Table(r) if !r.is_empty()));
    }
}

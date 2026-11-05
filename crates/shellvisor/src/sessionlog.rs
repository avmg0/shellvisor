//! Session logging, like PuTTY's "printable output" log: everything a pane
//! shows goes to a text file, with colours, cursor movement and other escape
//! sequences taken out (`sv log start [file]`, `sv log stop`, right-click →
//! Log to file).

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum State {
    #[default]
    Text,
    Esc,
    /// CSI: until a final byte.
    Csi,
    /// OSC / DCS / APC…: until BEL or ESC \.
    Str,
    StrEsc,
    /// ESC ( B and the like: one more byte.
    Charset,
    /// A carriage return not yet known to start "\r\n".
    Cr,
}

pub struct Log {
    pub path: PathBuf,
    file: std::fs::File,
    state: State,
    /// The line being built (a lone \r starts it over, like the screen does).
    line: Vec<u8>,
    /// Start each line with the local time, `[22:14:03] `.
    pub stamps: bool,
}

impl Log {
    pub fn open(path: &Path) -> std::io::Result<Log> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Log {
            path: path.to_path_buf(),
            file,
            state: State::Text,
            line: Vec::new(),
            stamps: true,
        })
    }

    /// Write what a pane printed, as text.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut out: Vec<u8> = Vec::new();
        for &b in bytes {
            self.state = match self.state {
                State::Text | State::Cr => {
                    if self.state == State::Cr && b != b'\n' {
                        // a lone \r: the line is redrawn (progress bars)
                        self.line.clear();
                    }
                    match b {
                        0x1b => State::Esc,
                        b'\r' => State::Cr,
                        b'\n' => {
                            if self.stamps {
                                out.extend_from_slice(stamp_now().as_bytes());
                            }
                            out.append(&mut self.line);
                            out.push(b'\n');
                            State::Text
                        }
                        0x08 => {
                            self.line.pop();
                            State::Text
                        }
                        b'\t' => {
                            self.line.push(b);
                            State::Text
                        }
                        0..=0x1f | 0x7f => State::Text,
                        _ => {
                            self.line.push(b);
                            State::Text
                        }
                    }
                }
                State::Esc => match b {
                    b'[' => State::Csi,
                    b']' | b'P' | b'_' | b'^' | b'X' => State::Str,
                    b'(' | b')' | b'*' | b'+' | b'#' | b'%' => State::Charset,
                    _ => State::Text,
                },
                State::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        State::Text
                    } else {
                        State::Csi
                    }
                }
                State::Str => match b {
                    0x07 => State::Text,
                    0x1b => State::StrEsc,
                    _ => State::Str,
                },
                State::StrEsc => {
                    if b == b'\\' {
                        State::Text
                    } else {
                        State::Str
                    }
                }
                State::Charset => State::Text,
            };
        }
        if !out.is_empty() {
            let _ = self.file.write_all(&out);
        }
    }

    /// Write out a last unfinished line.
    pub fn finish(&mut self) {
        if !self.line.is_empty() {
            let mut l = if self.stamps {
                stamp_now().into_bytes()
            } else {
                Vec::new()
            };
            l.append(&mut self.line);
            l.push(b'\n');
            let _ = self.file.write_all(&l);
        }
        let _ = self.file.flush();
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        self.finish();
    }
}

/// ~/shellvisor-logs/2026-11-05_221403-<name>.log
pub fn default_path(name: &str) -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let stamp = chrono_like_stamp();
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    home.join("shellvisor-logs")
        .join(format!("{stamp}-{safe}.log"))
}

/// `[HH:MM:SS] ` in local time.
fn stamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!("[{:02}:{:02}:{:02}] ", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The folder logs go to by default.
pub fn log_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("shellvisor-logs")
}

/// Logs in the default folder, newest first: (path, bytes, modified).
pub fn list() -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut v: Vec<_> = std::fs::read_dir(log_dir())
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some((e.path(), m.len(), m.modified().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(_, _, t)| std::cmp::Reverse(*t));
    v
}

/// A log as rows for the table viewer: line number, time (if stamped), text.
pub fn rows(text: &str) -> Vec<crate::data::value::Record> {
    use crate::data::value::Value;
    text.lines()
        .enumerate()
        .map(|(i, l)| {
            let (time, rest) = match l.strip_prefix('[') {
                Some(r)
                    if r.len() >= 10
                        && r.as_bytes()[8] == b']'
                        && r[..8].bytes().all(|b| b.is_ascii_digit() || b == b':') =>
                {
                    (
                        r[..8].to_string(),
                        r[9..].strip_prefix(' ').unwrap_or(&r[9..]),
                    )
                }
                _ => (String::new(), l),
            };
            vec![
                ("line".to_string(), Value::Int(i as i64 + 1)),
                ("time".to_string(), Value::Str(time)),
                ("text".to_string(), Value::Str(rest.to_string())),
            ]
        })
        .collect()
}

/// Local time as YYYY-MM-DD_HHMMSS (via `date`, which knows the time zone).
fn chrono_like_stamp() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d_%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            secs.to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_only() {
        let p = std::env::temp_dir().join(format!("sv-log-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut l = Log::open(&p).unwrap();
        l.stamps = false;
        l.feed(b"\x1b]7770;prompt\x07\x1b[1;32mhello\x1b[0m world\r\n");
        l.feed(b"Receiving 10%\rReceiving 50%\rReceiving 100%\r\n");
        l.feed(b"tab\there\x1b(B ok\r\nab\x08c\r\nlast");
        l.finish();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t, "hello world\nReceiving 100%\ntab\there ok\nac\nlast\n");
        let _ = std::fs::remove_file(&p);
        // with times
        let mut l = Log::open(&p).unwrap();
        l.feed(b"one\r\ntwo\r\n");
        drop(l);
        let t = std::fs::read_to_string(&p).unwrap();
        let last: Vec<&str> = t.lines().rev().take(2).collect();
        assert!(
            last[0].starts_with('[') && last[0].ends_with("] two"),
            "{t}"
        );
        let r = rows(last[0]);
        assert_eq!(r[0][2].1, crate::data::value::Value::Str("two".into()));
        assert_eq!(
            rows("plain")[0][1].1,
            crate::data::value::Value::Str(String::new())
        );
        let d = default_path("zsh · work");
        assert!(
            d.to_string_lossy().ends_with("-zsh---work.log"),
            "{}",
            d.display()
        );
    }
}

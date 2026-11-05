//! Saved connections, like PuTTY's "Saved Sessions": `sv ssh save NAME
//! [user@]host [-p port] [-i key] [-L/-R/-D forward]… [-- remote command]`,
//! `sv ssh NAME` to open one in a new tab, `sv ssh` to list them together
//! with the hosts in ~/.ssh/config, `sv ssh rm NAME`.
//!
//! Kept in ~/.config/shellvisor/sessions.yaml. Connections use the system
//! `ssh` (your keys, agent and config apply); Shellvisor only remembers the
//! options and adds a keepalive so idle sessions don't drop.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub name: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Private key file (-i).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Port forwards as ssh takes them: "-L 8080:localhost:80", "-D 1080"…
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<String>,
    /// Seconds between keepalives (0: none). Default 30.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keepalive: Option<u32>,
    /// Run this on the server instead of a login shell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// A connection in the picker: (name, address or where it's from, command line).
pub type Entry = (String, String, String);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    sessions: Vec<Session>,
}

fn path() -> PathBuf {
    crate::paths::config_file()
        .parent()
        .map(|d| d.join("sessions.yaml"))
        .unwrap_or_else(|| PathBuf::from("sessions.yaml"))
}

pub fn load() -> Vec<Session> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_yaml_ng::from_str::<File>(&t).ok())
        .map(|f| f.sessions)
        .unwrap_or_default()
}

fn save_all(list: &[Session]) -> std::io::Result<()> {
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let text = serde_yaml_ng::to_string(&File {
        sessions: list.to_vec(),
    })
    .map_err(std::io::Error::other)?;
    std::fs::write(p, text)
}

fn q(s: &str) -> String {
    crate::app::shell_quote(s)
}

impl Session {
    /// The ssh command line that opens it.
    pub fn command_line(&self) -> String {
        let mut c = vec!["ssh".to_string()];
        let ka = self.keepalive.unwrap_or(30);
        if ka > 0 {
            c.push(format!("-o ServerAliveInterval={ka}"));
        }
        if let Some(p) = self.port {
            c.push(format!("-p {p}"));
        }
        if let Some(i) = &self.identity {
            c.push(format!("-i {}", q(i)));
        }
        for f in &self.forwards {
            c.push(f.clone());
        }
        let target = match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        c.push(q(&target));
        if let Some(cmd) = &self.command {
            c.push("-t".into());
            c.push(q(cmd));
        }
        c.join(" ")
    }

    /// "user@host:port"
    pub fn address(&self) -> String {
        let mut s = match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s
    }
}

/// Parse `NAME [user@]host [-p port] [-i key] [-L|-R|-D spec]… [-k secs] [-- cmd]`.
pub fn parse_save(args: &[String]) -> Result<Session, String> {
    let mut it = args.iter().peekable();
    let name = it.next().ok_or("a name")?.clone();
    if name.starts_with('-') || name.contains('/') {
        return Err(format!("`{name}` isn't a good name"));
    }
    let target = it.next().ok_or("a host ([user@]host)")?;
    let (user, host) = match target.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, target.clone()),
    };
    let (host, mut port) = match host.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()),
        _ => (host, None),
    };
    let mut s = Session {
        name,
        host,
        user,
        ..Default::default()
    };
    while let Some(a) = it.next() {
        let mut val = |flag: &str| -> Result<String, String> {
            it.next().cloned().ok_or(format!("{flag} needs a value"))
        };
        match a.as_str() {
            "-p" => port = Some(val("-p")?.parse().map_err(|_| "-p: a port number")?),
            "-i" => s.identity = Some(val("-i")?),
            "-L" | "-R" | "-D" => {
                let v = val(a)?;
                s.forwards.push(format!("{a} {v}"));
            }
            "-k" => s.keepalive = Some(val("-k")?.parse().map_err(|_| "-k: seconds")?),
            "--" => {
                let rest: Vec<&str> = it.by_ref().map(String::as_str).collect();
                if !rest.is_empty() {
                    s.command = Some(rest.join(" "));
                }
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    s.port = port;
    Ok(s)
}

/// Everything you can connect to: saved sessions, then ~/.ssh/config hosts
/// not already saved. (name, address or source, command line)
pub fn entries() -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = load()
        .into_iter()
        .map(|s| (s.name.clone(), s.address(), s.command_line()))
        .collect();
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let cfg = std::fs::read_to_string(home.join(".ssh/config")).unwrap_or_default();
    for h in crate::connect::ssh_hosts(&cfg) {
        if !v.iter().any(|(n, _, _)| *n == h) {
            let cmd = format!("ssh -o ServerAliveInterval=30 {}", q(&h));
            v.push((h, "~/.ssh/config".into(), cmd));
        }
    }
    v
}

/// `sv ssh …` outside the request path: list, save, remove. Returns the
/// command line to open for `sv ssh NAME` (Some) or an exit code (Err).
pub fn cli(args: &[String]) -> Result<Option<(String, String)>, i32> {
    match args.first().map(String::as_str) {
        None | Some("list") => {
            let e = entries();
            if e.is_empty() {
                println!(
                    "no saved sessions — sv ssh save NAME [user@]host [-p port] [-i key] [-L spec]"
                );
            }
            for (name, addr, _) in e {
                println!("{name:<20} {addr}");
            }
            Err(0)
        }
        Some("save") => match parse_save(&args[1..]) {
            Ok(s) => {
                let mut list = load();
                list.retain(|x| x.name != s.name);
                let line = s.command_line();
                let name = s.name.clone();
                list.push(s);
                list.sort_by(|a, b| a.name.cmp(&b.name));
                match save_all(&list) {
                    Ok(()) => {
                        println!("saved {name}: {line}");
                        Err(0)
                    }
                    Err(e) => {
                        eprintln!("sv ssh: {e}");
                        Err(1)
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "sv ssh save: {e}\nsv ssh save NAME [user@]host[:port] [-p port] [-i key] [-L|-R|-D spec]… [-k keepalive-secs] [-- remote command]"
                );
                Err(2)
            }
        },
        Some("rm") | Some("remove") => {
            let Some(name) = args.get(1) else {
                eprintln!("sv ssh rm NAME");
                return Err(2);
            };
            let mut list = load();
            let n = list.len();
            list.retain(|x| &x.name != name);
            if list.len() == n {
                eprintln!("sv ssh: no saved session `{name}`");
                return Err(1);
            }
            match save_all(&list) {
                Ok(()) => {
                    println!("removed {name}");
                    Err(0)
                }
                Err(e) => {
                    eprintln!("sv ssh: {e}");
                    Err(1)
                }
            }
        }
        Some(name) => match entries().into_iter().find(|(n, _, _)| n == name) {
            Some((n, _, cmd)) => Ok(Some((n, cmd))),
            None => {
                eprintln!("sv ssh: no session `{name}` (sv ssh lists them)");
                Err(1)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn save_and_command_line() {
        let s = parse_save(&a(&[
            "prod",
            "deploy@10.0.0.5",
            "-p",
            "2222",
            "-i",
            "~/.ssh/prod",
            "-L",
            "8080:localhost:80",
            "-D",
            "1080",
        ]))
        .unwrap();
        assert_eq!(s.address(), "deploy@10.0.0.5:2222");
        assert_eq!(
            s.command_line(),
            "ssh -o ServerAliveInterval=30 -p 2222 -i '~/.ssh/prod' -L 8080:localhost:80 -D 1080 deploy@10.0.0.5"
        );
        let s = parse_save(&a(&["db", "db.example.com:2200", "-k", "0", "--", "htop"])).unwrap();
        assert_eq!(s.port, Some(2200));
        assert_eq!(s.command_line(), "ssh -p 2200 db.example.com -t htop");
        assert!(parse_save(&a(&["x"])).is_err());
        assert!(parse_save(&a(&["x", "h", "-z"])).is_err());
        let y = serde_yaml_ng::to_string(&File {
            sessions: vec![s.clone()],
        })
        .unwrap();
        let back: File = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.sessions[0], s);
    }
}

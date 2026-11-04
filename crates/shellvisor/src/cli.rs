//! The `sv` command.
//!
//! `sv` is the same binary as `shellvisor`, invoked under another name (or as
//! `shellvisor sv …`). Inside Shellvisor it talks to its parent by printing an
//! escape sequence to the terminal; outside it can start Shellvisor.

use std::io::Write;
use sv_proto::{ENV_INSIDE, ENV_PID, ENV_SHELL, ENV_VERSION, Mode, request_sequence};

const HELP: &str = "\
sv — control Shellvisor from your shell

usage:
  sv on        start Shellvisor here (or show the full view if already inside)
  sv off       switch to basic view (just your shell; everything keeps running)
  sv exit      leave Shellvisor
  sv files [tree|list|git|find]   open the files pane (in this tab)
  sv sys       open the system pane (CPU, memory, disk, network, temperatures)
  sv layout dev|monitor|files|data|agents   lay this tab out as a preset
  sv flow 'a | b | c'             run a pipeline with a meter on every stage
  sv flowpane  open the Flow pane (what the shell is running, live)
  sv power on|off|auto            low-power mode (auto = on battery)
  sv agent new [claude|codex|gemini|aider] [--name N] [--here] ['first task']
  sv agent new --cmd '<command>' [--name N]   any other agent CLI
  sv agent attach   make the agent running in this shell an agent column
  sv agent spawn|send|wait|tree|logs|pause|resume|stop|log   agent teams (sv agent for details)
  sv agents    the agents as a table · sv connect   CLIs, logins, MCP servers, ssh hosts
  sv data      list the data commands (ls, ps, open, where, sort-by, to csv…)
  sv status    is Shellvisor running in this terminal?
  sv version   print the version
  sv help      this help
";

pub fn inside() -> bool {
    std::env::var_os(ENV_INSIDE).is_some_and(|v| v == "1")
}

fn send(mode: Mode) -> i32 {
    send_seq(mode.sequence())
}

fn send_seq(seq: String) -> i32 {
    // Write to the controlling terminal so it works even with stdout redirected.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .and_then(|mut tty| tty.write_all(seq.as_bytes()));
    if written.is_err() {
        let mut out = std::io::stdout();
        if out
            .write_all(seq.as_bytes())
            .and_then(|_| out.flush())
            .is_err()
        {
            eprintln!("sv: could not reach the terminal");
            return 1;
        }
    }
    0
}

fn not_running(cmd: &str) -> i32 {
    eprintln!("sv {cmd}: Shellvisor is not running in this terminal — start it with `sv on`");
    1
}

pub fn run(args: &[String]) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "on" => {
            if inside() {
                send(Mode::On)
            } else {
                start_shellvisor()
            }
        }
        "off" => {
            if inside() {
                send(Mode::Off)
            } else {
                not_running("off")
            }
        }
        "exit" | "quit" => {
            if inside() {
                send(Mode::Exit)
            } else {
                not_running("exit")
            }
        }
        "flow" => crate::flowrun::run(&args[1..]),
        "agent" => agent(&args[1..]),
        "files" | "sys" | "system" | "flowpane" | "layout" | "power" => {
            if !inside() {
                return not_running(cmd);
            }
            let arg = args.get(1).map(String::as_str);
            let words: Vec<&str> = match (cmd, arg) {
                ("files", None) => vec!["files"],
                ("files", Some(v @ ("tree" | "list" | "git" | "find"))) => vec!["files", v],
                ("sys" | "system", None) => vec!["system"],
                ("flowpane", None) => vec!["flow"],
                ("layout", Some(l @ ("dev" | "monitor" | "files" | "data" | "agents"))) => {
                    vec!["layout", l]
                }
                ("power", Some(m @ ("on" | "off" | "auto"))) => vec!["power", m],
                _ => {
                    eprintln!("sv {cmd}: unexpected argument — see `sv help`");
                    return 2;
                }
            };
            send_seq(request_sequence(&words))
        }
        "status" => {
            if inside() {
                let get = |k| std::env::var(k).unwrap_or_else(|_| "?".into());
                println!(
                    "Shellvisor {} · running (pid {}) · view: basic · shell: {} · hooks: {}",
                    get(ENV_VERSION),
                    get(ENV_PID),
                    get(ENV_SHELL),
                    get("SHELLVISOR_HOOKS")
                );
            } else {
                println!("Shellvisor is not running in this terminal (start it with `sv on`)");
            }
            0
        }
        "version" | "--version" | "-V" => {
            println!("sv {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "help" | "--help" | "-h" if args.get(1).map(String::as_str) == Some("data") => {
            print!("{}", crate::data::help());
            0
        }
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            0
        }
        "data" => {
            print!("{}", crate::data::help());
            0
        }
        other if crate::data::is_command(other) || other.contains('|') || other.contains(' ') => {
            crate::data::run(args)
        }
        other => {
            eprintln!("sv: unknown command `{other}`\n\n{HELP}");
            2
        }
    }
}

fn agent(args: &[String]) -> i32 {
    if !inside() {
        return not_running("agent");
    }
    let usage = AGENT_USAGE;
    match args.first().map(String::as_str) {
        Some(
            op
            @ ("spawn" | "send" | "wait" | "tree" | "logs" | "pause" | "resume" | "stop" | "log"),
        ) => agent_bus(op, &args[1..]),
        Some("attach") => send_seq(request_sequence(&["agent", "attach"])),
        Some("new") => {
            let mut cli = String::new();
            let (mut name, mut cmd, mut here, mut prompt) =
                (String::new(), String::new(), false, Vec::new());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--name" | "-n" => name = it.next().cloned().unwrap_or_default(),
                    "--cmd" | "-c" => cmd = it.next().cloned().unwrap_or_default(),
                    "--here" => here = true,
                    x if cli.is_empty()
                        && prompt.is_empty()
                        && crate::agents::Cli::parse(x).is_some() =>
                    {
                        cli = x.to_string()
                    }
                    x => prompt.push(x.to_string()),
                }
            }
            if !cmd.is_empty() {
                cli = "command".into();
            }
            if cli.is_empty() {
                cli = match crate::agents::detect().first() {
                    Some((c, _)) => c.name().to_string(),
                    None => {
                        eprintln!(
                            "sv agent: no agent CLI found (claude, codex, gemini, aider) — use --cmd \"<command>\""
                        );
                        return 1;
                    }
                };
            }
            if !name.is_empty() && !crate::agents::valid_name(&name) {
                eprintln!("sv agent: name `{name}` — use letters, digits, - and _");
                return 2;
            }
            let b = |s: &str| sv_proto::b64(s.as_bytes());
            let words = [
                "agent".to_string(),
                "new".into(),
                cli,
                if name.is_empty() { "-".into() } else { name },
                if here {
                    "here".into()
                } else {
                    "worktree".into()
                },
                b(&prompt.join(" ")),
                b(&cmd),
                b(&std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()),
            ];
            let refs: Vec<&str> = words.iter().map(String::as_str).collect();
            send_seq(request_sequence(&refs))
        }
        _ => {
            eprintln!("{usage}");
            2
        }
    }
}

const AGENT_USAGE: &str = "\
usage: sv agent new [claude|codex|gemini|aider] [--name N] [--here] [\"first task\"]
       sv agent new --cmd \"<command>\" [--name N]
       sv agent attach
       sv agent spawn [cli] [--name N] [--cmd C] [--top] [--here] \"task\"
                                       start an agent (a sub-agent when an agent runs it)
       sv agent send <name> \"text\"     type a message into an agent
       sv agent wait <name>… [--timeout S]  until each has finished its task
       sv agent tree [--json]           every agent, by parent
       sv agent logs <name> [-f]        what the agent's screen shows
       sv agent pause|resume|stop <name> [--tree]
       sv agent log                     what agents did to each other";

/// `sv agent …` commands that need an answer: over the agent bus.
fn agent_bus(op: &str, args: &[String]) -> i32 {
    use serde_json::{Value, json};
    use std::time::Duration;
    let fail = |e: String| {
        eprintln!("sv agent {op}: {e}");
        if e.starts_with("refused") { 3 } else { 1 }
    };
    let flag = |f: &str| args.iter().any(|a| a == f);
    let plain: Vec<&String> = {
        let mut out = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--name" | "-n" | "--cmd" | "-c" | "--timeout" | "--folder" | "--cli" => {
                    it.next();
                }
                x if x.starts_with('-') && x.len() > 1 => {}
                _ => out.push(a),
            }
        }
        out
    };
    let opt = |f: &[&str]| {
        args.iter()
            .position(|a| f.contains(&a.as_str()))
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let need_name = || plain.first().map(|s| s.to_string());
    let req = match op {
        "spawn" => {
            let mut words: Vec<String> = plain.iter().map(|s| s.to_string()).collect();
            let mut cli = opt(&["--cli"]);
            if cli.is_empty() && words.len() > 1 && crate::agents::Cli::parse(&words[0]).is_some() {
                cli = words.remove(0);
            }
            let name = opt(&["--name", "-n"]);
            if !name.is_empty() && !crate::agents::valid_name(&name) {
                return fail(format!("name `{name}` — use letters, digits, - and _"));
            }
            let folder = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let mut r = json!({
                "op": "spawn", "cli": cli, "name": name, "cmd": opt(&["--cmd", "-c"]),
                "task": words.join(" "), "top": flag("--top"), "folder": folder,
            });
            if flag("--here") {
                r["worktree"] = Value::Bool(false);
            }
            r
        }
        "send" => {
            let Some(to) = need_name() else {
                return fail("which agent? sv agent send <name> \"text\"".into());
            };
            let text = plain[1..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return fail("nothing to send".into());
            }
            json!({"op": "send", "to": to, "text": text})
        }
        "tree" | "log" => json!({"op": op}),
        _ => {
            let Some(name) = need_name() else {
                return fail(format!("which agent? sv agent {op} <name>"));
            };
            json!({"op": op, "name": name, "tree": flag("--tree")})
        }
    };
    let quick = Some(Duration::from_secs(10));
    match op {
        "spawn" => match crate::bus::call(req, quick) {
            Ok(v) => {
                println!("{}", v["name"].as_str().unwrap_or(""));
                0
            }
            Err(e) => fail(e),
        },
        "wait" => {
            let secs: f64 = opt(&["--timeout"]).parse().unwrap_or(0.0);
            let timeout = (secs > 0.0).then(|| Duration::from_secs_f64(secs));
            let mut code = 0;
            for name in &plain {
                let r = json!({"op": "wait", "name": name});
                match crate::bus::call(r, timeout) {
                    Ok(v) => {
                        let a = &v["agent"];
                        if flag("--json") {
                            println!("{a}");
                        } else {
                            println!("{}", agent_line(a));
                        }
                        if a["status"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("exited "))
                        {
                            code = 1;
                        }
                    }
                    Err(e) => code = fail(format!("{name}: {e}")),
                }
            }
            code
        }
        "tree" => match crate::bus::call(req, quick) {
            Ok(v) => {
                let list = v["agents"].as_array().cloned().unwrap_or_default();
                if flag("--json") {
                    for a in &list {
                        println!("{a}");
                    }
                } else if list.is_empty() {
                    println!("no agents — start one with `sv agent new` or shift+F9");
                } else {
                    print!("{}", agent_tree(&list));
                }
                0
            }
            Err(e) => fail(e),
        },
        "logs" => {
            let mut last: Vec<String> = Vec::new();
            loop {
                let lines = match crate::bus::call(req.clone(), quick) {
                    Ok(v) => v["lines"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|l| l.as_str().map(String::from))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    Err(e) => return if last.is_empty() { fail(e) } else { 0 },
                };
                let mut lines = lines;
                while lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                for l in new_lines(&last, &lines) {
                    println!("{l}");
                }
                if !flag("-f") && !flag("--follow") {
                    return 0;
                }
                last = lines;
                std::thread::sleep(Duration::from_millis(700));
            }
        }
        "log" => match crate::bus::call(req, quick) {
            Ok(v) => {
                for e in v["events"].as_array().cloned().unwrap_or_default() {
                    let t = e["time"].as_str().unwrap_or("");
                    let t = t.get(11..19).unwrap_or(t);
                    println!("{t}  {}", e["event"].as_str().unwrap_or(""));
                }
                0
            }
            Err(e) => fail(e),
        },
        _ => match crate::bus::call(req, quick) {
            Ok(v) => {
                let names: Vec<&str> = v["agents"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|n| n.as_str()).collect())
                    .unwrap_or_default();
                if names.is_empty() {
                    return fail("that agent has no running program to signal".into());
                }
                let verb = match op {
                    "pause" => "paused",
                    "resume" => "resumed",
                    _ => "stopped",
                };
                println!("{verb} {}", names.join(", "));
                0
            }
            Err(e) => fail(e),
        },
    }
}

/// `api  claude  done  3 files +40 -2  12K tok`
fn agent_line(a: &serde_json::Value) -> String {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let mut out = format!("{}  {}  {}", s("name"), s("cli"), s("status"));
    let files = a["files"].as_array().map_or(0, |f| f.len());
    if files > 0 {
        out += &format!(
            "  {files} file{} +{} -{}",
            if files == 1 { "" } else { "s" },
            a["added"].as_u64().unwrap_or(0),
            a["removed"].as_u64().unwrap_or(0)
        );
    }
    if let Some(t) = a["tokens"].as_u64() {
        out += &format!("  {}", crate::agents::fmt_tokens(t));
    }
    let task = s("task");
    if !task.is_empty() {
        let short: String = task.chars().take(50).collect();
        out += &format!(
            "  — {short}{}",
            if task.chars().count() > 50 { "…" } else { "" }
        );
    }
    out
}

/// Agents indented under their parents; the CLI's own helpers as leaves.
fn agent_tree(list: &[serde_json::Value]) -> String {
    fn walk(list: &[serde_json::Value], parent: Option<&str>, indent: &str, out: &mut String) {
        let kids: Vec<&serde_json::Value> = list
            .iter()
            .filter(|a| a["parent"].as_str() == parent)
            .collect();
        for (i, a) in kids.iter().enumerate() {
            let last = i + 1 == kids.len();
            let (branch, next) = match parent {
                None => ("", String::new()),
                Some(_) => (
                    if last { "└─ " } else { "├─ " },
                    format!("{indent}{}", if last { "   " } else { "│  " }),
                ),
            };
            out.push_str(&format!("{indent}{branch}{}\n", agent_line(a)));
            let name = a["name"].as_str().unwrap_or("");
            walk(list, Some(name), &next, out);
            for h in a["helpers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| h.as_str())
            {
                out.push_str(&format!(
                    "{next}·  {h} (inside {})\n",
                    a["cli"].as_str().unwrap_or("")
                ));
            }
        }
    }
    let mut out = String::new();
    walk(list, None, "", &mut out);
    out
}

/// Lines of `now` that weren't already shown at the end of `before`.
fn new_lines<'a>(before: &[String], now: &'a [String]) -> &'a [String] {
    if before.is_empty() {
        return now;
    }
    // longest suffix of `before` that is a prefix of `now`
    for k in (1..=before.len().min(now.len())).rev() {
        if before[before.len() - k..] == now[..k] {
            return &now[k..];
        }
    }
    if before == now { &[] } else { now }
}

/// Replace this process with Shellvisor, in the current folder.
fn start_shellvisor() -> i32 {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sv on: cannot find the shellvisor binary: {e}");
            return 1;
        }
    };
    let err = std::process::Command::new(exe).arg0("shellvisor").exec();
    eprintln!("sv on: could not start Shellvisor: {err}");
    1
}

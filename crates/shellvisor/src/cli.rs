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
  sv files [tree|list|columns|grid|git|find|size|recent]   the files pane
  sv sys       open the system pane (CPU, memory, disk, network, temperatures)
  sv layout [name]   a preset (dev monitor files data agents) or a saved layout · no name: list them
  sv flow 'a | b | c'             run a pipeline with a meter on every stage
  sv flowpane  open the Flow pane (what the shell is running, live)
  sv power on|off|auto            low-power mode (auto = on battery)
  sv theme light|dark|auto        colours of Shellvisor's own frames and panes
  sv autostart on|off|status      start Shellvisor when a terminal opens (zsh, bash)
  sv settings     folder colours (prompt, lists), ls as a table, table borders, theme (also alt+s)
  sv agent new [claude|codex|gemini|aider] [--name N] [--here] ['first task']
  sv agent new --cmd '<command>' [--name N]   any other agent CLI
  sv agent attach   make the agent running in this shell an agent column
  sv agent run team.yaml 'goal'   start a team of agents (team.yaml)
  sv agent plan 'goal'            plan a team from a goal · sv agent board   tree, timeline, log (alt+g)
  sv agent spawn|send|wait|tree|logs|pause|resume|stop|log|emit|budget   (sv agent for details)
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
        "autostart" => crate::autostart::run(&args[1..]),
        "settings" => {
            if !inside() {
                return not_running("settings");
            }
            send_seq(request_sequence(&["settings"]))
        }
        "agent" => agent(&args[1..]),
        "files" | "sys" | "system" | "flowpane" | "layout" | "power" | "theme" => {
            if !inside() {
                return not_running(cmd);
            }
            let arg = args.get(1).map(String::as_str);
            let words: Vec<&str> = match (cmd, arg) {
                ("files", None) => vec!["files"],
                ("files", Some(v)) if crate::files::View::parse(v).is_some() => vec!["files", v],
                ("theme", Some(t @ ("light" | "dark" | "auto"))) => vec!["theme", t],
                ("sys" | "system", None) => vec!["system"],
                ("flowpane", None) => vec!["flow"],
                ("layout", None) => {
                    println!("presets: dev monitor files data agents");
                    let saved = crate::layouted::saved_layouts();
                    if saved.is_empty() {
                        println!("saved:   none yet — alt+e opens the layout editor");
                    } else {
                        println!("saved:   {}", saved.join(" "));
                    }
                    return 0;
                }
                ("layout", Some(l))
                    if l.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
                {
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
            op @ ("spawn" | "send" | "wait" | "tree" | "logs" | "pause" | "resume" | "stop" | "log"
            | "emit" | "usage" | "budget"),
        ) => agent_bus(op, &args[1..]),
        Some("run") => agent_run(&args[1..]),
        Some("board") => {
            let view = args.get(1).map(String::as_str).unwrap_or("tree");
            if crate::board::View::parse(view).is_none() {
                eprintln!("sv agent board: plan, tree, timeline or log");
                return 2;
            }
            send_seq(request_sequence(&["agent", "board", view]))
        }
        Some("plan") => {
            let goal = args[1..].join(" ");
            if goal.trim().is_empty() {
                eprintln!("sv agent plan: what should the team do? sv agent plan \"goal\"");
                return 2;
            }
            let b = |s: &str| sv_proto::b64(s.as_bytes());
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let (g, c) = (b(&goal), b(&cwd));
            send_seq(request_sequence(&["agent", "plan", &g, &c]))
        }
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
       sv agent log                     what agents did to each other
       sv agent run [team.yaml] \"goal\" [--dry-run]   start a team (--init: a starter file)
       sv agent emit <event> [\"details\"]  tell the team something happened (e.g. failed)
       sv agent usage <tokens>          report tokens used (agents that don't log them)
       sv agent budget [tokens]         the team's budget: show, or raise and resume
       sv agent pause|resume|stop --all
       sv agent plan \"goal\"            a planner proposes tasks; edit them, then dispatch
       sv agent board [plan|tree|timeline|log]   the agent board (alt+g)";

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
        "emit" => {
            let Some(event) = need_name() else {
                return fail("which event? sv agent emit <event> [\"details\"]".into());
            };
            let text = plain[1..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            json!({"op": "emit", "event": event, "text": text})
        }
        "usage" | "budget" => {
            let n = plain.first().map(|s| parse_tokens(s));
            if let Some(None) = n {
                return fail(format!(
                    "`{}` — a number of tokens, like 250000 or 2M",
                    plain[0]
                ));
            }
            if op == "usage" && n.is_none() {
                return fail("how many tokens? sv agent usage <tokens>".into());
            }
            json!({"op": op, "tokens": n.flatten(), "name": opt(&["--name", "-n"])})
        }
        "pause" | "resume" | "stop" if flag("--all") => json!({"op": op, "all": true}),
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
        "emit" | "usage" => match crate::bus::call(req, quick) {
            Ok(_) => 0,
            Err(e) => fail(e),
        },
        "budget" => match crate::bus::call(req, quick) {
            Ok(v) => {
                println!("{}", team_line(&v));
                if let Some(r) = v["resumed"].as_array().filter(|r| !r.is_empty()) {
                    let names: Vec<&str> = r.iter().filter_map(|n| n.as_str()).collect();
                    println!("resumed {}", names.join(", "));
                }
                0
            }
            Err(e) => fail(e),
        },
        "tree" => match crate::bus::call(req, quick) {
            Ok(v) => {
                let list = v["agents"].as_array().cloned().unwrap_or_default();
                if !v["team"].is_null() && !flag("--json") {
                    println!("{}", team_line(&v["team"]));
                }
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

/// `250000`, `250K`, `2M`, `1.5m`
fn parse_tokens(s: &str) -> Option<u64> {
    let t = s.trim().to_lowercase().replace(['_', ','], "");
    let t = t.trim_end_matches("tok").trim_end_matches("tokens").trim();
    let (num, mult) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1e3),
        'm' => (&t[..t.len() - 1], 1e6),
        _ => (t, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then(|| (v * mult).round() as u64)
}

/// `team price-alerts · 120K tok of 2M · goal: …`
fn team_line(t: &serde_json::Value) -> String {
    let used = t["tokens"].as_u64().unwrap_or(0);
    let mut s = format!(
        "team {} · {}",
        t["name"].as_str().unwrap_or(""),
        crate::agents::fmt_tokens(used)
    );
    if let Some(b) = t["budget_tokens"].as_u64() {
        s += &format!(
            " of {}",
            crate::agents::fmt_tokens(b).trim_end_matches(" tok")
        );
    }
    if t["capped"].as_bool() == Some(true) {
        s += " · ⏸ over budget";
    }
    if let Some(g) = t["goal"].as_str().filter(|g| !g.is_empty()) {
        s += &format!(" · {}", g.chars().take(60).collect::<String>());
    }
    s
}

/// `sv agent run [team.yaml] "goal" [--dry-run]` · `sv agent run --init`
fn agent_run(args: &[String]) -> i32 {
    use crate::team::{self, Start};
    let default = std::path::Path::new(".shellvisor/team.yaml");
    if args.iter().any(|a| a == "--init") {
        if default.exists() {
            eprintln!("sv agent run: {} already exists", default.display());
            return 1;
        }
        let _ = std::fs::create_dir_all(".shellvisor");
        return match std::fs::write(default, team::STARTER) {
            Ok(()) => {
                println!(
                    "wrote {} — edit it, then: sv agent run \"what the team should do\"",
                    default.display()
                );
                0
            }
            Err(e) => {
                eprintln!("sv agent run: {e}");
                1
            }
        };
    }
    let dry = args.iter().any(|a| a == "--dry-run" || a == "-n");
    let words: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with("--") && *a != "-n")
        .collect();
    let (file, goal) = match words.first() {
        Some(f) if f.ends_with(".yaml") || f.ends_with(".yml") => {
            (std::path::PathBuf::from(f.as_str()), &words[1..])
        }
        _ if default.exists() => (default.to_path_buf(), &words[..]),
        _ => (std::path::PathBuf::from("team.yaml"), &words[..]),
    };
    let goal = goal
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "sv agent run: {}: {e}\n(sv agent run --init writes a starter .shellvisor/team.yaml)",
                file.display()
            );
            return 2;
        }
    };
    let t = match team::parse(&text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sv agent run: {}: {e}", file.display());
            return 2;
        }
    };
    // Things worth knowing, not errors.
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let servers: Vec<String> = crate::connect::mcp_servers(&home, &cwd)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    let installed: Vec<&str> = crate::agents::detect()
        .into_iter()
        .map(|(c, _)| c.name())
        .collect();
    for m in &t.agents {
        if !m.cli.is_empty() && !installed.contains(&m.cli.as_str()) {
            eprintln!(
                "sv agent run: note: {} uses {}, which isn't installed here",
                m.id, m.cli
            );
        }
        for x in m.mcp.iter().filter(|x| !servers.contains(x)) {
            eprintln!(
                "sv agent run: note: {} wants MCP server {x}, which no agent CLI here has configured",
                m.id
            );
        }
    }
    if goal.trim().is_empty() {
        eprintln!(
            "sv agent run: what should the team do? sv agent run {} \"goal\"",
            file.display()
        );
        return 2;
    }
    if dry {
        println!("team {} — {}", t.name, goal);
        for m in &t.agents {
            // a command: its program name (and arguments), not the whole path
            let how = if m.cmd.is_empty() {
                m.cli.clone()
            } else {
                let mut words = m.cmd.split_whitespace();
                let prog = words.next().unwrap_or("");
                let prog = prog.rsplit('/').next().unwrap_or(prog);
                std::iter::once(prog)
                    .chain(words)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let when = match &m.start {
                Start::Now => "starts now".to_string(),
                Start::After(d) => format!(
                    "starts when {} {} done",
                    d.join(" and "),
                    if d.len() == 1 { "is" } else { "are" }
                ),
                Start::Spawned => {
                    let by: Vec<&str> = t
                        .agents
                        .iter()
                        .filter(|o| o.can_spawn.contains(&m.id))
                        .map(|o| o.id.as_str())
                        .collect();
                    if by.is_empty() {
                        "starts when spawned".into()
                    } else {
                        format!("started by {}", by.join(" or "))
                    }
                }
            };
            let role = if m.orchestrator {
                " · orchestrator"
            } else {
                ""
            };
            println!("  {}  {how}{role} · {when}", m.id);
        }
        for r in &t.rules {
            println!("  on {}.{} → send {}: {}", r.agent, r.event, r.send, r.text);
        }
        if let Some(b) = t.budget_tokens {
            println!(
                "  budget {} — every agent pauses past it",
                crate::agents::fmt_tokens(b)
            );
        }
        return 0;
    }
    let req = serde_json::json!({
        "op": "run",
        "team": t,
        "goal": goal,
        "folder": cwd.display().to_string(),
    });
    match crate::bus::call(req, Some(std::time::Duration::from_secs(20))) {
        Ok(v) => {
            let started: Vec<&str> = v["started"]
                .as_array()
                .map(|a| a.iter().filter_map(|n| n.as_str()).collect())
                .unwrap_or_default();
            println!(
                "team {} started: {} · F9 to watch · sv agent tree",
                t.name,
                started.join(", ")
            );
            0
        }
        Err(e) => {
            eprintln!("sv agent run: {e}");
            1
        }
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

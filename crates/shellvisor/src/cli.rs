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
  sv pane ports|docker|logs [files…]|http [url]   service panes (also preview, agents, activity)
  sv power on|off|auto            low-power mode (auto = on battery)
  sv theme light|dark|auto        colours of Shellvisor's own frames and panes
  sv autostart on|off|status      start Shellvisor when a terminal opens (zsh, bash)
  sv resize left|right|up|down [n] · sv resize 30% [width|height]   resize this pane
  sv ssh [NAME] · sv ssh save NAME user@host [-p port] [-i key] [-L spec] · sv ssh rm NAME
                                  saved connections (like PuTTY), opened in a new tab
  sv mcp                          MCP servers: status, tools, try a tool, traffic, permissions, add (also ctrl+] M)
  sv serial [device] [baud]       serial console (routers, boards); no device: list ports · ~. quits
  sv log start [file] · sv log stop   save everything this pane shows to a text file
  sv terminal-colors [profile|undo]  Terminal.app: the design's background (profile: import the Shellvisor profile)
  sv settings     folder colours (prompt, lists), ls as a table, table borders, theme (also alt+s)
  sv activity     downloads & installs: progress, speed, time left, cd / open (also ctrl+] j)
  sv agent new [claude|codex|gemini|aider] [--name N] [--here] ['first task']
  sv agent new --cmd '<command>' [--name N]   any other agent CLI
  sv agent attach   make the agent running in this shell an agent column
  sv agent run team.yaml 'goal'   start a team of agents (team.yaml)
  sv agent plan 'goal'            plan a team from a goal · sv agent board   tree, timeline, log (alt+g)
  sv agent run --template NAME 'goal'   a ready-made team (sv agent templates)
  sv agent queue · sv agent replay NAME   merge finished agents one by one · step through what one did
  sv agent spawn|send|wait|tree|logs|pause|resume|stop|log|emit|budget   (sv agent for details)
  sv agents    the agents as a table · sv connect   CLIs, logins, MCP servers, ssh hosts
  sv data      list the data commands (ls, ps, open, where, sort-by, to csv…)
  sv status    is Shellvisor running in this terminal?
  sv doctor    check this terminal: colours, UTF-8, hooks, Option-as-Alt, old copies (--no-keys)
  sv bug       open a new GitHub issue with your setup and the last crash report filled in (--print: just show it)
  sv crashes   list the crash reports saved on this computer (never sent anywhere)
  sv selftest  start your shells with the hooks and your rc files, check prompt, cd, exit codes
  sv version   print the version
  sv help      this help
";

pub fn inside() -> bool {
    std::env::var_os(ENV_INSIDE).is_some_and(|v| v == "1")
}

fn send(mode: Mode) -> i32 {
    send_seq(mode.sequence())
}

/// `sv log list`: the saved logs, newest first.
fn log_list() -> i32 {
    use crate::data::value::Value;
    let logs = crate::sessionlog::list();
    if logs.is_empty() {
        println!(
            "no logs in {} yet · sv log start",
            crate::sessionlog::log_dir().display()
        );
        return 0;
    }
    let rows: Vec<crate::data::value::Record> = logs
        .iter()
        .enumerate()
        .map(|(i, (p, size, _))| {
            vec![
                ("#".to_string(), Value::Int(i as i64 + 1)),
                (
                    "name".to_string(),
                    Value::Str(
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ),
                ),
                ("size".to_string(), Value::Size(*size)),
            ]
        })
        .collect();
    let mut st = crate::data::format::Style::text();
    st.width = crate::data::term_width();
    print!("{}", crate::data::format::render_table(&rows, &st));
    println!("sv log view N opens one");
    0
}

/// A log by number from `sv log list`, a file, or (none) the newest.
fn log_pick(arg: Option<&str>) -> Option<std::path::PathBuf> {
    let logs = crate::sessionlog::list();
    let p = match arg {
        None => logs.first().map(|(p, _, _)| p.clone()),
        Some(n) if n.parse::<usize>().is_ok_and(|n| n >= 1) => logs
            .get(n.parse::<usize>().unwrap() - 1)
            .map(|(p, _, _)| p.clone()),
        Some(f) => {
            let p = std::path::Path::new(f);
            let p = if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            };
            p.exists().then_some(p)
        }
    };
    if p.is_none() {
        eprintln!("sv log view: no such log · sv log list");
    }
    p
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
        "resize" => {
            if !inside() {
                return not_running("resize");
            }
            let rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            let ok = match rest.as_slice() {
                [p] | [p, _] if p.ends_with('%') => {
                    let v = p.trim_end_matches('%').parse::<f32>();
                    let axis_ok = rest.get(1).is_none_or(|a| *a == "width" || *a == "height");
                    v.is_ok_and(|v| (10.0..=90.0).contains(&v)) && axis_ok
                }
                [d] => resize_dir(d).is_some(),
                [d, n] => resize_dir(d).is_some() && n.parse::<usize>().is_ok_and(|n| n >= 1),
                _ => false,
            };
            if !ok {
                eprintln!(
                    "sv resize left|right|up|down [steps]   move this pane's border (5% a step)\n\
                     sv resize 30% [width|height]           this pane's share of its split (10–90%)"
                );
                return 2;
            }
            let mut req = vec!["resize"];
            req.extend(rest);
            send_seq(request_sequence(&req))
        }
        "autostart" => crate::autostart::run(&args[1..]),
        "doctor" => crate::doctor::run(&args[1..]),
        "selftest" => crate::selftest::run(&args[1..]),
        "bug" => crate::crash::bug(&args[1..]),
        "crashes" => crate::crash::list(),
        // for tests of the crash report
        "__panic" => panic!("test panic from sv __panic"),
        "mcp" => {
            if !inside() {
                // outside Shellvisor: list what's configured
                let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
                let cwd = std::env::current_dir().unwrap_or_default();
                let list = crate::mcp::discover(&home, &cwd);
                if list.is_empty() {
                    println!("no MCP servers configured (Claude Code, Codex, Gemini, .mcp.json)");
                }
                for s in list {
                    println!(
                        "{:<18} {:<6} {}  ({})",
                        s.name,
                        s.transport_name(),
                        s.target(),
                        s.sources.join(", ")
                    );
                }
                return 0;
            }
            send_seq(request_sequence(&["mcp"]))
        }
        "serial" => {
            let rest: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            let (here, rest) = match rest.split_first() {
                Some((&"--here", r)) => (true, r.to_vec()),
                _ => (false, rest),
            };
            match rest.as_slice() {
                [] => {
                    let ports = crate::serial::ports();
                    if ports.is_empty() {
                        println!("no serial ports found (plug in the USB adapter or board)");
                    }
                    for p in ports {
                        println!("{p}");
                    }
                    println!("open one: sv serial /dev/… [baud, default 115200] · ~. quits");
                    0
                }
                [dev] | [dev, _] => {
                    let baud: u32 = match rest.get(1).map(|b| b.parse()) {
                        None => 115200,
                        Some(Ok(b)) => b,
                        Some(Err(_)) => {
                            eprintln!("sv serial: the baud rate is a number (9600, 115200…)");
                            return 2;
                        }
                    };
                    if crate::serial::speed(baud).is_none() {
                        eprintln!(
                            "sv serial: {baud} isn't a baud rate this system knows (try 9600 or 115200)"
                        );
                        return 2;
                    }
                    if inside() && !here {
                        // its own tab, named after the port
                        send_seq(request_sequence(&["serial", dev, &baud.to_string()]))
                    } else {
                        crate::serial::console(dev, baud)
                    }
                }
                _ => {
                    eprintln!("sv serial [device] [baud]");
                    2
                }
            }
        }
        "ssh" => {
            let rest = &args[1..];
            // inside Shellvisor, plain `sv ssh` opens the picker
            if rest.is_empty() && inside() {
                return send_seq(request_sequence(&["ssh"]));
            }
            // sv ssh forward [NAME]: the port forwards screen
            if matches!(
                rest.first().map(String::as_str),
                Some("forward" | "forwards" | "fwd")
            ) {
                if !inside() {
                    return not_running("ssh forward");
                }
                let mut req = vec!["ssh-forwards"];
                if let Some(n) = rest.get(1) {
                    req.push(n);
                }
                return send_seq(request_sequence(&req));
            }
            // sv ssh all [NAME…]: every (or the named) saved session in one
            // tab, typing going to all of them
            if rest.first().map(String::as_str) == Some("all") {
                let entries = crate::sessions::entries();
                let pick: Vec<(String, String)> = if rest.len() > 1 {
                    let mut v = Vec::new();
                    for want in &rest[1..] {
                        match entries.iter().find(|(n, _, _)| n == want) {
                            Some((n, _, c)) => v.push((n.clone(), c.clone())),
                            None => {
                                eprintln!("sv ssh all: no session `{want}` (sv ssh lists them)");
                                return 1;
                            }
                        }
                    }
                    v
                } else {
                    entries
                        .iter()
                        .map(|(n, _, c)| (n.clone(), c.clone()))
                        .collect()
                };
                if pick.len() < 2 {
                    eprintln!("sv ssh all: needs two or more saved sessions");
                    return 2;
                }
                if !inside() {
                    return not_running("ssh all");
                }
                let mut req: Vec<&str> = vec!["ssh-all"];
                for (n, c) in &pick {
                    req.push(n);
                    req.push(c);
                }
                return send_seq(request_sequence(&req));
            }
            match crate::sessions::cli(rest) {
                Err(code) => code,
                Ok(Some((name, cmd))) => {
                    if inside() {
                        send_seq(request_sequence(&["ssh", &name, &cmd]))
                    } else {
                        // outside: just run it here
                        std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&cmd)
                            .status()
                            .map(|s| s.code().unwrap_or(1))
                            .unwrap_or(1)
                    }
                }
                Ok(None) => 0,
            }
        }
        "log" => {
            match args.get(1).map(String::as_str) {
                Some("list" | "ls") => return log_list(),
                Some("view" | "show") => {
                    let Some(path) = log_pick(args.get(2).map(String::as_str)) else {
                        return 1;
                    };
                    if !inside() {
                        // outside Shellvisor: the pager, at the end
                        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".into());
                        return std::process::Command::new(&pager)
                            .args(if pager == "less" { vec!["+G"] } else { vec![] })
                            .arg(&path)
                            .status()
                            .map(|s| s.code().unwrap_or(1))
                            .unwrap_or(1);
                    }
                    return send_seq(request_sequence(&[
                        "log",
                        "view",
                        &path.display().to_string(),
                    ]));
                }
                _ => {}
            }
            if !inside() {
                return not_running("log");
            }
            let plain = args.iter().any(|a| a == "--plain");
            let args: Vec<String> = args.iter().filter(|a| *a != "--plain").cloned().collect();
            let rest: Vec<String> = match args.get(1).map(String::as_str) {
                None | Some("status") => vec![],
                Some("stop") => vec!["stop".into()],
                Some("start") if plain => match args.get(2) {
                    Some(f) => {
                        let p = std::path::Path::new(f);
                        let abs = if p.is_absolute() {
                            p.to_path_buf()
                        } else {
                            std::env::current_dir().unwrap_or_default().join(p)
                        };
                        vec!["start-plain".into(), abs.display().to_string()]
                    }
                    None => vec!["start-plain".into()],
                },
                Some("start") => match args.get(2) {
                    Some(f) => {
                        let p = std::path::Path::new(f);
                        let abs = if p.is_absolute() {
                            p.to_path_buf()
                        } else {
                            std::env::current_dir().unwrap_or_default().join(p)
                        };
                        vec!["start".into(), abs.display().to_string()]
                    }
                    None => vec!["start".into()],
                },
                Some(other) => {
                    eprintln!(
                        "sv log: `{other}` — start [file] [--plain], stop, status, list or view [file|N]"
                    );
                    return 2;
                }
            };
            let mut req = vec!["log"];
            req.extend(rest.iter().map(String::as_str));
            send_seq(request_sequence(&req))
        }
        "terminal-colors" | "terminal-colours" => crate::termcolors::run(&args[1..]),
        "tour" => {
            if !inside() {
                return not_running("tour");
            }
            send_seq(request_sequence(&["tour"]))
        }
        "activity" | "downloads" => {
            if !inside() {
                return not_running("activity");
            }
            send_seq(request_sequence(&["activity"]))
        }
        "settings" => {
            if !inside() {
                return not_running("settings");
            }
            send_seq(request_sequence(&["settings"]))
        }
        "agent" => agent(&args[1..]),
        "files" | "sys" | "system" | "flowpane" | "layout" | "power" | "theme" | "pane" => {
            if !inside() {
                return not_running(cmd);
            }
            let arg = args.get(1).map(String::as_str);
            let mut extra: Vec<String> = Vec::new();
            let words: Vec<&str> = match (cmd, arg) {
                ("files", None) => vec!["files"],
                ("files", Some(v)) if crate::files::View::parse(v).is_some() => vec!["files", v],
                ("theme", Some(t @ ("light" | "dark" | "auto"))) => vec!["theme", t],
                ("sys" | "system", None) => vec!["system"],
                ("flowpane", None) => vec!["flow"],
                (
                    "pane",
                    Some(
                        k @ ("files" | "git" | "system" | "thermal" | "flow" | "preview" | "agents"
                        | "activity" | "ports" | "docker" | "logs" | "http"),
                    ),
                ) => {
                    // sv pane logs a.log b.log · sv pane http URL: files made absolute
                    let cwd = std::env::current_dir().unwrap_or_default();
                    for a in &args[2..] {
                        let v = if k == "logs" {
                            cwd.join(a).display().to_string()
                        } else {
                            a.clone()
                        };
                        extra.push(v);
                    }
                    let mut w = vec!["pane", k];
                    w.extend(extra.iter().map(String::as_str));
                    w
                }
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
    // these work in a plain terminal too: they only read and write files
    let offline = match args.first().map(String::as_str) {
        Some("templates" | "template" | "replay" | "queue") => true,
        Some("run") => args
            .iter()
            .any(|a| a == "--init" || a == "--dry-run" || a == "-n"),
        _ => false,
    };
    if !inside() && !offline {
        return not_running("agent");
    }
    let usage = AGENT_USAGE;
    match args.first().map(String::as_str) {
        Some(
            op @ ("spawn" | "send" | "wait" | "tree" | "logs" | "pause" | "resume" | "stop" | "log"
            | "emit" | "usage" | "budget"),
        ) => agent_bus(op, &args[1..]),
        Some("run") => agent_run(&args[1..]),
        Some("templates") => agent_templates(),
        Some("replay") => crate::replay::run(&args[1..]),
        Some("queue") => crate::mergeq::run(&args[1..]),
        Some("template") => agent_template(&args[1..]),
        Some("diff" | "review") => {
            let Some(name) = args.get(1) else {
                eprintln!("sv agent diff NAME: which agent's work to review");
                return 2;
            };
            if !inside() {
                return not_running("agent diff");
            }
            send_seq(request_sequence(&["agent-review", name]))
        }
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
       sv agent run --template NAME [--cli codex | --cmd '…'] \"goal\"   a ready-made team
       sv agent templates · sv agent template show|save|rm NAME [team.yaml]
       sv agent queue [NAME…] [--test 'cmd'] [--dry-run]   rebase, test and merge finished agents one by one
       sv agent replay [NAME | --list]   step through what an agent did (screen, files, approvals)
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

/// `sv agent templates`
fn agent_templates() -> i32 {
    for (name, about, _, mine) in crate::team::templates() {
        println!("{name:<10} {about}{}", if mine { "  (yours)" } else { "" });
    }
    println!(
        "\nsv agent run --template NAME \"goal\"  ·  --cli codex to use another CLI  ·  your own: sv agent template save NAME [team.yaml]"
    );
    0
}

/// `sv agent template show|save|rm NAME [file]`
fn agent_template(args: &[String]) -> i32 {
    let fail = |m: String| {
        eprintln!("sv agent template: {m}");
        2
    };
    let (Some(op), Some(name)) = (args.first(), args.get(1)) else {
        return fail("show|save|rm NAME [team.yaml]".into());
    };
    if !crate::agents::valid_name(name) {
        return fail(format!("name `{name}`: letters, digits, - and _"));
    }
    let path = crate::team::templates_dir().join(format!("{name}.yaml"));
    match op.as_str() {
        "show" => match crate::team::templates().into_iter().find(|t| &t.0 == name) {
            Some((_, _, y, _)) => {
                print!("{y}");
                0
            }
            None => fail(format!("no template `{name}`")),
        },
        "save" => {
            let src = args
                .get(2)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(".shellvisor/team.yaml"));
            let text = match std::fs::read_to_string(&src) {
                Ok(t) => t,
                Err(e) => return fail(format!("{}: {e}", src.display())),
            };
            // checked with the CLI placeholder filled, so `{cli}` may stay in
            if let Err(e) = crate::team::parse(&crate::team::fill_template(&text, "claude", None)) {
                return fail(format!("{}: {e}", src.display()));
            }
            let _ = std::fs::create_dir_all(crate::team::templates_dir());
            match std::fs::write(&path, text) {
                Ok(()) => {
                    println!("saved as template {name} — sv agent run --template {name} \"goal\"");
                    0
                }
                Err(e) => fail(e.to_string()),
            }
        }
        "rm" => match std::fs::remove_file(&path) {
            Ok(()) => {
                println!("removed template {name}");
                0
            }
            Err(_) => fail(format!(
                "no saved template `{name}` (built-in ones can't be removed)"
            )),
        },
        _ => fail(format!("`{op}` — show, save or rm")),
    }
}

/// `sv agent run [team.yaml] "goal" [--dry-run]` · `sv agent run --init`
fn agent_run(args: &[String]) -> i32 {
    use crate::team::{self, Start};
    let default = std::path::Path::new(".shellvisor/team.yaml");
    // --template NAME [--cli claude|codex|…] [--cmd 'command']
    let val = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let valued = ["--template", "-t", "--cli", "--cmd"];
    let template_name = val("--template").or_else(|| val("-t"));
    let tcli = val("--cli").unwrap_or_else(|| "claude".into());
    let tcmd = val("--cmd");
    let from_template = match &template_name {
        Some(n) => match team::template(n, &tcli, tcmd.as_deref()) {
            Ok(y) => Some(y),
            Err(e) => {
                eprintln!("sv agent run: {e}");
                return 2;
            }
        },
        None => None,
    };
    let args: Vec<String> = {
        let mut out = Vec::new();
        let mut skip = false;
        for a in args {
            if skip {
                skip = false;
                continue;
            }
            if valued.contains(&a.as_str()) {
                skip = true;
                continue;
            }
            out.push(a.clone());
        }
        out
    };
    if args.iter().any(|a| a == "--init") {
        if default.exists() {
            eprintln!("sv agent run: {} already exists", default.display());
            return 1;
        }
        let _ = std::fs::create_dir_all(".shellvisor");
        let text = from_template
            .clone()
            .unwrap_or_else(|| team::STARTER.to_string());
        return match std::fs::write(default, text) {
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
        _ if from_template.is_some() => (
            std::path::PathBuf::from(format!(
                "template {}",
                template_name.as_deref().unwrap_or("")
            )),
            &words[..],
        ),
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
    let text = match from_template
        .clone()
        .map(Ok)
        .unwrap_or_else(|| std::fs::read_to_string(&file))
    {
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

/// `left`, `right`, `up`, `down` for `sv resize`.
pub fn resize_dir(s: &str) -> Option<crate::keys::Dir> {
    use crate::keys::Dir;
    Some(match s {
        "left" | "l" => Dir::Left,
        "right" | "r" => Dir::Right,
        "up" | "u" => Dir::Up,
        "down" | "d" => Dir::Down,
        _ => return None,
    })
}

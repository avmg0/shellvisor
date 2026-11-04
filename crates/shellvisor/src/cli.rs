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
    let usage = "usage: sv agent new [claude|codex|gemini|aider] [--name N] [--here] [\"first task\"]\n       sv agent new --cmd \"<command>\" [--name N]\n       sv agent attach";
    match args.first().map(String::as_str) {
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

//! `sv` structured data commands — tables you can filter and sort, nushell-style,
//! from inside bash or zsh.
//!
//! ```sh
//! sv ls | sv where size gt 1mb | sv sort-by size -r      # stage by stage
//! sv 'ls | where size gt 1mb | sort-by size -r'           # or one quoted pipeline
//! sv open package.json | sv get dependencies
//! ps aux | sv from ssv | sv where %cpu gt 5 | sv to csv
//! ```
//!
//! Between `sv` stages rows travel as JSON lines (so `jq` works too). At the
//! end of a pipe, on the terminal, they are drawn as a table.

pub mod format;
pub mod ops;
pub mod sources;
pub mod value;

use std::io::{IsTerminal, Read, Write};
use value::Data;

const FORMATS_IN: &str = "json, jsonl, csv, tsv, yaml, toml, ssv (space-separated columns), lines";
const FORMATS_OUT: &str = "json, jsonl, csv, tsv, md, yaml, text";

/// Every data command name (for `sv` dispatch and help).
pub fn is_command(name: &str) -> bool {
    sources::is_source(name) || ops::is_filter(name) || name == "from" || name == "to"
}

pub fn help() -> String {
    let mut s = String::from("data commands — tables you can filter and sort\n\nsources:\n");
    for (n, d) in sources::SOURCES {
        s.push_str(&format!("  {n:<12} {d}\n"));
    }
    s.push_str("\nfilters (take a table from the previous stage):\n");
    for (n, d) in ops::FILTERS {
        s.push_str(&format!("  {n:<12} {d}\n"));
    }
    s.push_str(&format!(
        "\nformats:\n  from <fmt>   parse piped text: {FORMATS_IN}\n  to <fmt>     write: {FORMATS_OUT}\n"
    ));
    s.push_str(
        "\nwhere: eq ne gt lt ge le (or == != > < >= <=), has (=~), not-has, starts, ends,\n\
         \x20      like (*.rs), in (a,b,c), within (dates: modified within 2d); join with and / or\n\
         values: 10mb 2kib · 90s 5m 2d · 2026-01-31 · true · text\n\
         \nexamples:\n\
         \x20 sv ls | sv where size gt 1mb | sv sort-by size -r\n\
         \x20 sv 'ps | where cpu gt 5 | sort-by cpu -r | first 10'\n\
         \x20 sv open data.csv | sv group-by country\n\
         \x20 ps aux | sv from ssv | sv where user eq root | sv to csv\n",
    );
    s
}

/// What flows between stages inside one `sv` process.
enum Input {
    None,
    Text(String),
    Data(Data),
}

fn parse_from(fmt: &str, text: &str) -> Result<Data, String> {
    match fmt {
        "json" | "jsonl" | "ndjson" => format::parse_json(text),
        "csv" => format::parse_csv(text, ','),
        "tsv" => format::parse_csv(text, '\t'),
        "yaml" | "yml" => format::parse_yaml(text),
        "toml" => format::parse_toml(text),
        "ssv" => Ok(format::parse_ssv(text)),
        "lines" | "text" => Ok(format::parse_lines(text)),
        other => Err(format!("unknown format `{other}` — try {FORMATS_IN}")),
    }
}

fn render_to(fmt: &str, d: &Data) -> Result<String, String> {
    Ok(match fmt {
        "json" => format::to_json_text(d) + "\n",
        "jsonl" | "ndjson" => {
            let mut buf = Vec::new();
            format::write_jsonl(d, &mut buf).map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&buf).into_owned()
        }
        "csv" => format::to_csv(d, ','),
        "tsv" => format::to_csv(d, '\t'),
        "md" | "markdown" => format::to_markdown(d),
        "yaml" | "yml" => format::to_yaml(d),
        "text" | "table" => format::render_text(
            d,
            &format::Style {
                color: false,
                width: usize::MAX / 4,
                max_rows: usize::MAX,
            },
        ),
        other => return Err(format!("unknown format `{other}` — try {FORMATS_OUT}")),
    })
}

/// Terminal width for tables.
pub fn term_width() -> usize {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        for fd in [1, 2] {
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                return ws.ws_col as usize;
            }
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}

/// Run a pipeline of stages on optional piped-in text. Returns the final
/// data, or text if the last stage was `to <format>`.
pub fn run_stages(
    stages: &[Vec<String>],
    stdin: Option<String>,
) -> Result<Result<Data, String>, String> {
    let mut input = match stdin {
        Some(t) => Input::Text(t),
        None => Input::None,
    };
    for (i, stage) in stages.iter().enumerate() {
        let (cmd, args) = (stage[0].as_str(), &stage[1..]);
        let last = i + 1 == stages.len();
        input = match cmd {
            c if sources::is_source(c) => {
                Input::Data(sources::source(c, args).map_err(|e| prefix(c, e))?)
            }
            "from" => {
                let fmt = args.first().ok_or("from: which format? e.g. `from csv`")?;
                let text = match input {
                    Input::Text(t) => t,
                    Input::Data(d) => render_to("jsonl", &d)?,
                    Input::None => return Err(format!("from {fmt}: nothing piped in")),
                };
                Input::Data(parse_from(fmt, &text)?)
            }
            "to" => {
                let fmt = args.first().ok_or("to: which format? e.g. `to csv`")?;
                if !last {
                    return Err(format!("to {fmt}: must be the last stage"));
                }
                let d = into_data(input)?;
                return Ok(Err(render_to(fmt, &d)?));
            }
            c if ops::is_filter(c) => {
                let d = into_data(input).map_err(|e| format!("{c}: {e}"))?;
                Input::Data(ops::filter(c, args, d)?)
            }
            other => {
                return Err(format!(
                    "unknown command `{other}` — `sv help data` lists the data commands"
                ));
            }
        };
    }
    into_data(input).map(Ok)
}

fn prefix(cmd: &str, e: String) -> String {
    if e.starts_with(cmd) {
        e
    } else {
        format!("{cmd}: {e}")
    }
}

fn into_data(i: Input) -> Result<Data, String> {
    match i {
        Input::Data(d) => Ok(d),
        Input::Text(t) => format::parse_auto(&t),
        Input::None => {
            Err("needs input — pipe a table in (e.g. `sv ls | sv …`) or start with a source".into())
        }
    }
}

/// `sv <data command…>` from the shell.
pub fn run(args: &[String]) -> i32 {
    let stages =
        if args.len() == 1 && args[0].contains('|') || args.len() == 1 && args[0].contains(' ') {
            match ops::split_pipeline(&args[0]) {
                Ok(s) => s,
                Err(e) => return fail(&e),
            }
        } else {
            vec![args.to_vec()]
        };
    let starts_with_source = stages
        .first()
        .and_then(|s| s.first())
        .is_some_and(|c| sources::is_source(c));
    let stdin = if !starts_with_source && !std::io::stdin().is_terminal() {
        let mut t = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut t) {
            return fail(&format!("reading input: {e}"));
        }
        Some(t)
    } else {
        None
    };
    let out = match run_stages(&stages, stdin) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    let mut stdout = std::io::stdout().lock();
    let res = match out {
        Err(text) => stdout.write_all(text.as_bytes()),
        Ok(d) if std::io::stdout().is_terminal() => {
            let st = format::Style {
                color: true,
                width: term_width(),
                max_rows: 1000,
            };
            stdout.write_all(format::render_text(&d, &st).as_bytes())
        }
        Ok(d) => format::write_jsonl(&d, &mut stdout),
    };
    match res.and_then(|_| stdout.flush()) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => fail(&e.to_string()),
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("sv: {msg}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use value::Value;

    fn run_line(line: &str, stdin: Option<&str>) -> Result<Result<Data, String>, String> {
        run_stages(&ops::split_pipeline(line).unwrap(), stdin.map(String::from))
    }

    #[test]
    fn stages_chain_and_formats() {
        let csv = "name,n\na,3\nb,1\nc,2\n";
        let out = run_line("from csv | where n ge 2 | sort-by n | to csv", Some(csv)).unwrap();
        assert_eq!(out, Err("name,n\nc,2\na,3\n".into()));
        let out = run_line("from csv | get name | to json", Some(csv)).unwrap();
        assert_eq!(out, Err("[\n  \"a\",\n  \"b\",\n  \"c\"\n]\n".into()));
        let out = run_line("from csv | sum n", Some(csv)).unwrap();
        assert_eq!(out, Ok(Data::Value(Value::Int(6))));
        // JSON lines from a previous sv stage are picked up automatically
        let out = run_line("count", Some("{\"a\":1}\n{\"a\":2}\n")).unwrap();
        assert_eq!(out, Ok(Data::Value(Value::Int(2))));
        // plain text becomes lines
        let out = run_line("where line has b | count", Some("abc\nxyz\nbbb\n")).unwrap();
        assert_eq!(out, Ok(Data::Value(Value::Int(2))));
    }

    #[test]
    fn errors_are_friendly() {
        assert!(run_line("count", None).unwrap_err().contains("needs input"));
        assert!(
            run_line("frobnicate", None)
                .unwrap_err()
                .contains("unknown command")
        );
        assert!(
            run_line("env | to csv | count", None)
                .unwrap_err()
                .contains("last stage")
        );
        assert!(
            run_line("from xml", Some("<a/>"))
                .unwrap_err()
                .contains("unknown format")
        );
    }

    #[test]
    fn help_lists_everything() {
        let h = help();
        for name in ["ls", "git-log", "where", "group-by", "from <fmt>", "within"] {
            assert!(h.contains(name), "{name}");
        }
    }
}

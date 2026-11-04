//! Reading and writing data: JSON / JSON lines, CSV / TSV, YAML, TOML,
//! space-separated columns, plain lines — and drawing tables as text.

use super::value::{Data, Record, Value, columns, parse_date};

// ------------------------------------------------------------------ JSON

pub fn to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Nothing => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or(J::Null),
        Value::Size(s) => J::from(*s),
        Value::Duration(d) => J::from(*d),
        Value::Date(d) => J::String(super::value::iso_date(*d)),
        Value::Str(s) => J::String(s.clone()),
        Value::List(l) => J::Array(l.iter().map(to_json).collect()),
        Value::Record(r) => J::Object(r.iter().map(|(k, v)| (k.clone(), to_json(v))).collect()),
    }
}

pub fn record_json(r: &Record) -> serde_json::Value {
    serde_json::Value::Object(r.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
}

/// JSON → value, using the column name to bring back sizes and durations
/// (JSON has only plain numbers) and ISO strings to bring back dates.
pub fn from_json(j: &serde_json::Value, key: Option<&str>) -> Value {
    use serde_json::Value as J;
    match j {
        J::Null => Value::Nothing,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                match key.map(hint) {
                    Some(Hint::Size) if i >= 0 => Value::Size(i as u64),
                    Some(Hint::Duration) => Value::Duration(i),
                    _ => Value::Int(i),
                }
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => {
            if s.len() >= 19 && s.as_bytes().get(10) == Some(&b'T') {
                if let Some(d) = parse_date(s) {
                    return Value::Date(d);
                }
            }
            Value::Str(s.clone())
        }
        J::Array(a) => Value::List(a.iter().map(|x| from_json(x, None)).collect()),
        J::Object(o) => Value::Record(
            o.iter()
                .map(|(k, v)| (k.clone(), from_json(v, Some(k))))
                .collect(),
        ),
    }
}

enum Hint {
    Size,
    Duration,
    None,
}

fn hint(key: &str) -> Hint {
    let k = key.to_ascii_lowercase();
    const SIZE: &[&str] = &[
        "size",
        "mem",
        "memory",
        "bytes",
        "used",
        "total",
        "free",
        "avail",
        "available",
        "rss",
        "vsz",
    ];
    const DUR: &[&str] = &["duration", "elapsed", "uptime", "took"];
    if SIZE.contains(&k.as_str()) || k.ends_with("_bytes") || k.ends_with("_size") {
        Hint::Size
    } else if DUR.contains(&k.as_str()) || k.ends_with("_ms") {
        Hint::Duration
    } else {
        Hint::None
    }
}

/// JSON (one document) or JSON lines.
pub fn parse_json(text: &str) -> Result<Data, String> {
    let t = text.trim_start();
    if t.starts_with('[') {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(t) {
            return Ok(json_data(&j));
        }
    }
    // JSON lines (a single object is the 1-line case)
    let mut rows = Vec::new();
    let mut scalars = Vec::new();
    for (n, line) in t.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let j: serde_json::Value = match serde_json::from_str(line) {
            Ok(j) => j,
            Err(e) => {
                // maybe one pretty-printed document
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(t) {
                    return Ok(json_data(&j));
                }
                return Err(format!("line {}: {e}", n + 1));
            }
        };
        match from_json(&j, None) {
            Value::Record(r) => rows.push(r),
            v => scalars.push(v),
        }
    }
    if rows.is_empty() && !scalars.is_empty() {
        return Ok(Data::Value(if scalars.len() == 1 {
            scalars.pop().unwrap()
        } else {
            Value::List(scalars)
        }));
    }
    rows.extend(scalars.into_iter().map(|v| vec![("value".to_string(), v)]));
    Ok(Data::Table(rows))
}

fn json_data(j: &serde_json::Value) -> Data {
    match from_json(j, None) {
        Value::List(items) if items.iter().all(|v| matches!(v, Value::Record(_))) => Data::Table(
            items
                .into_iter()
                .filter_map(|v| match v {
                    Value::Record(r) => Some(r),
                    _ => None,
                })
                .collect(),
        ),
        v => Data::Value(v),
    }
}

/// JSON lines: one object (or value) per line — the format between `sv` stages.
pub fn write_jsonl(d: &Data, out: &mut impl std::io::Write) -> std::io::Result<()> {
    match d {
        Data::Table(rows) => {
            for r in rows {
                serde_json::to_writer(&mut *out, &record_json(r))?;
                out.write_all(b"\n")?;
            }
        }
        Data::Value(Value::List(items)) => {
            for v in items {
                serde_json::to_writer(&mut *out, &to_json(v))?;
                out.write_all(b"\n")?;
            }
        }
        Data::Value(v) => {
            serde_json::to_writer(&mut *out, &to_json(v))?;
            out.write_all(b"\n")?;
        }
    }
    Ok(())
}

pub fn to_json_text(d: &Data) -> String {
    let j = match d {
        Data::Table(rows) => serde_json::Value::Array(rows.iter().map(record_json).collect()),
        Data::Value(v) => to_json(v),
    };
    serde_json::to_string_pretty(&j).unwrap_or_default()
}

// ------------------------------------------------------------------ YAML / TOML

pub fn parse_yaml(text: &str) -> Result<Data, String> {
    let j: serde_json::Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    Ok(json_data(&j))
}

pub fn parse_toml(text: &str) -> Result<Data, String> {
    let j: serde_json::Value = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(json_data(&j))
}

pub fn to_yaml(d: &Data) -> String {
    let j = match d {
        Data::Table(rows) => serde_json::Value::Array(rows.iter().map(record_json).collect()),
        Data::Value(v) => to_json(v),
    };
    serde_yaml_ng::to_string(&j).unwrap_or_default()
}

// ------------------------------------------------------------------ CSV

/// RFC 4180 CSV (or TSV with `sep = '\t'`). The first row is the header.
pub fn parse_csv(text: &str, sep: char) -> Result<Data, String> {
    let mut records = split_csv(text, sep);
    if records.is_empty() {
        return Ok(Data::Table(Vec::new()));
    }
    let header = records.remove(0);
    let rows = records
        .into_iter()
        .filter(|r| !(r.len() == 1 && r[0].is_empty()))
        .map(|r| {
            header
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let cell = r.get(i).map(String::as_str).unwrap_or("");
                    (h.clone(), cell_value(cell))
                })
                .collect()
        })
        .collect();
    Ok(Data::Table(rows))
}

/// A cell read from text: numbers become numbers, ISO dates dates, the rest strings.
pub fn cell_value(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Nothing;
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::Int(i);
    }
    if t.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') {
        if let Ok(f) = t.parse::<f64>() {
            return Value::Float(f);
        }
    }
    if t.len() >= 10 {
        if let Some(d) = parse_date(t) {
            return Value::Date(d);
        }
    }
    Value::Str(s.to_string())
}

fn split_csv(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == sep {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut cell));
            out.push(std::mem::take(&mut row));
        } else {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        out.push(row);
    }
    out
}

pub fn to_csv(d: &Data, sep: char) -> String {
    let rows = d.clone().into_rows();
    let cols = columns(&rows);
    let esc = |s: &str| {
        if s.contains([sep, '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let mut out = cols
        .iter()
        .map(|c| esc(c))
        .collect::<Vec<_>>()
        .join(&sep.to_string());
    out.push('\n');
    for r in &rows {
        let line: Vec<String> = cols
            .iter()
            .map(|c| {
                esc(&super::value::get(r, c)
                    .map(Value::plain)
                    .unwrap_or_default())
            })
            .collect();
        out.push_str(&line.join(&sep.to_string()));
        out.push('\n');
    }
    out
}

// ------------------------------------------------------------------ text in

/// One row per line: `{line: "..."}`.
pub fn parse_lines(text: &str) -> Data {
    Data::Table(
        text.lines()
            .map(|l| vec![("line".to_string(), Value::Str(l.to_string()))])
            .collect(),
    )
}

/// Whitespace-separated columns with a header row (`ps aux`, `df -h`,
/// `docker ps` …). The last column takes the rest of the line. Tools whose
/// column names contain spaces (`CONTAINER ID`) separate columns by 2+
/// spaces; that is detected when every line splits the same way.
pub fn parse_ssv(text: &str) -> Data {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let Some((head, body)) = lines.split_first() else {
        return Data::Table(Vec::new());
    };
    let wide_split = |l: &str| -> Vec<String> {
        l.split("  ")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    let head_wide = wide_split(head);
    let wide = head_wide.len() > 1
        && head_wide.len() < head.split_whitespace().count()
        && body.iter().all(|l| wide_split(l).len() == head_wide.len());
    let header: Vec<String> = if wide {
        head_wide
    } else {
        head.split_whitespace().map(String::from).collect()
    };
    let n = header.len().max(1);
    let narrow_split = |l: &str| -> Vec<String> {
        let mut v = Vec::new();
        let mut rest = l.trim_start();
        while !rest.is_empty() {
            if v.len() == n - 1 {
                v.push(rest.trim_end().to_string());
                break;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            v.push(rest[..end].to_string());
            rest = rest[end..].trim_start();
        }
        v
    };
    let rows = body
        .iter()
        .map(|l| {
            let cells = if wide { wide_split(l) } else { narrow_split(l) };
            header
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    (
                        h.to_lowercase(),
                        cells
                            .get(i)
                            .map(|c| cell_value(c))
                            .unwrap_or(Value::Nothing),
                    )
                })
                .collect()
        })
        .collect();
    Data::Table(rows)
}

/// Guess the format of piped-in text: JSON / JSON lines (also bare numbers,
/// one per line) if every line parses, else one row per line.
pub fn parse_auto(text: &str) -> Result<Data, String> {
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        return parse_json(text);
    }
    match parse_json(text) {
        Ok(d) if !t.is_empty() => Ok(d),
        _ => Ok(parse_lines(text)),
    }
}

// ------------------------------------------------------------------ text out

pub struct Style {
    pub color: bool,
    pub width: usize,
    /// Show at most this many rows (the rest are summarised).
    pub max_rows: usize,
    /// Rounded borders around and between cells, like nushell.
    pub boxed: bool,
    /// ANSI start for folder names (rows whose `type` is dir).
    pub folder: Option<String>,
}

impl Style {
    /// Plain text, no colour or borders, every row.
    pub fn text() -> Style {
        Style {
            color: false,
            width: usize::MAX / 4,
            max_rows: usize::MAX,
            boxed: false,
            folder: None,
        }
    }
}

const BOLD_AMBER: &str = "\x1b[1;38;5;179m";
const DIM: &str = "\x1b[38;5;244m";
const CYAN: &str = "\x1b[38;5;73m";
const RESET: &str = "\x1b[0m";
const BOLD_GREEN: &str = "\x1b[1;38;5;35m";
const BORDER: &str = "\x1b[38;5;240m";
const LINK: &str = "\x1b[38;5;170m";

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn width_of(s: &str) -> usize {
    s.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD)
}

fn fit(s: &str, w: usize) -> String {
    if width_of(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = if is_wide(c) { 2 } else { 1 };
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

fn pad(s: &str, w: usize, right: bool) -> String {
    let n = w.saturating_sub(width_of(s));
    if right {
        format!("{}{s}", " ".repeat(n))
    } else {
        format!("{s}{}", " ".repeat(n))
    }
}

/// Aligned text table (`#` index column like nushell), a key/value block for
/// a single record, or the bare value.
pub fn render_text(d: &Data, st: &Style) -> String {
    match d {
        Data::Value(Value::Record(r)) => render_record(r, st),
        Data::Value(Value::List(items)) if items.iter().all(|v| !matches!(v, Value::Record(_))) => {
            let mut out = String::new();
            for v in items.iter().take(st.max_rows) {
                out.push_str(&clean(&v.display()));
                out.push('\n');
            }
            if items.len() > st.max_rows {
                out.push_str(&format!("… {} more\n", items.len() - st.max_rows));
            }
            out
        }
        Data::Value(v) => format!("{}\n", v.display()),
        Data::Table(rows) => render_table(rows, st),
    }
}

fn render_record(r: &Record, st: &Style) -> String {
    let kw = r
        .iter()
        .map(|(k, _)| width_of(k))
        .max()
        .unwrap_or(0)
        .min(30);
    let mut out = String::new();
    for (k, v) in r {
        let val = fit(&clean(&v.display()), st.width.saturating_sub(kw + 3).max(8));
        if st.color {
            out.push_str(&format!(
                "{BOLD_AMBER}{}{RESET}  {val}\n",
                pad(&fit(k, kw), kw, false)
            ));
        } else {
            out.push_str(&format!("{}  {val}\n", pad(&fit(k, kw), kw, false)));
        }
    }
    out
}

pub fn render_table(rows: &[Record], st: &Style) -> String {
    if st.boxed && !rows.is_empty() {
        return render_boxed(rows, st);
    }
    if rows.is_empty() {
        return if st.color {
            format!("{DIM}(empty){RESET}\n")
        } else {
            "(empty)\n".into()
        };
    }
    let cols = columns(rows);
    let shown = &rows[..rows.len().min(st.max_rows)];
    let cells: Vec<Vec<String>> = shown
        .iter()
        .map(|r| {
            cols.iter()
                .map(|c| {
                    super::value::get(r, c)
                        .map(|v| clean(&v.display()))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    let numeric: Vec<bool> = cols
        .iter()
        .map(|c| {
            shown
                .iter()
                .filter_map(|r| super::value::get(r, c))
                .filter(|v| !matches!(v, Value::Nothing))
                .all(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Size(_) | Value::Duration(_)
                    )
                })
        })
        .collect();
    let idx_w = (shown.len().saturating_sub(1)).to_string().len().max(1);
    let mut w: Vec<usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            cells
                .iter()
                .map(|r| width_of(&r[i]))
                .max()
                .unwrap_or(0)
                .max(width_of(c))
        })
        .collect();
    // Shrink the widest columns until the table fits.
    let budget = st.width.saturating_sub(idx_w + 2);
    let total = |w: &[usize]| w.iter().sum::<usize>() + 2 * w.len().saturating_sub(1);
    while total(&w) > budget {
        let (i, &m) = w.iter().enumerate().max_by_key(|(_, x)| **x).unwrap();
        if m <= 6 {
            break;
        }
        w[i] = m - 1;
    }
    let mut out = String::new();
    let sep = "  ";
    let mut head = format!("{} ", pad("#", idx_w, true));
    for (i, c) in cols.iter().enumerate() {
        head.push(' ');
        head.push_str(&pad(&fit(c, w[i]), w[i], numeric[i]));
        if i + 1 < cols.len() {
            head.push(' ');
        }
    }
    let head = head.trim_end().to_string();
    if st.color {
        out.push_str(&format!("{BOLD_AMBER}{head}{RESET}\n"));
    } else {
        out.push_str(&head);
        out.push('\n');
    }
    for (n, r) in cells.iter().enumerate() {
        let mut line = String::new();
        let idx = pad(&n.to_string(), idx_w, true);
        if st.color {
            line.push_str(&format!("{DIM}{idx}{RESET} "));
        } else {
            line.push_str(&format!("{idx} "));
        }
        for (i, cell) in r.iter().enumerate() {
            line.push(' ');
            let text = pad(&fit(cell, w[i]), w[i], numeric[i]);
            if st.color && i == 0 {
                let c = name_color(&shown[n], st);
                line.push_str(&format!("{c}{text}{RESET}"));
            } else {
                line.push_str(&text);
            }
            if i + 1 < r.len() {
                line.push_str(&sep[1..]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if rows.len() > shown.len() {
        let n = rows.len() - shown.len();
        let more = format!(
            "… {n} more row{} ({} total)",
            if n == 1 { "" } else { "s" },
            rows.len()
        );
        if st.color {
            out.push_str(&format!("{DIM}{more}{RESET}\n"));
        } else {
            out.push_str(&more);
            out.push('\n');
        }
    }
    out
}

/// Colour for the first cell: folders in the chosen colour, links magenta,
/// other files plain; tables without a `type` column keep cyan names.
fn name_color(r: &Record, st: &Style) -> String {
    match super::value::get(r, "type").map(|v| v.plain()) {
        Some(t) if t == "dir" => st.folder.clone().unwrap_or_default(),
        Some(t) if t == "link" => LINK.into(),
        Some(_) => String::new(),
        None => CYAN.into(),
    }
}

/// A table with rounded borders, header and index in green (nushell's look).
fn render_boxed(rows: &[Record], st: &Style) -> String {
    let cols = columns(rows);
    let shown = &rows[..rows.len().min(st.max_rows)];
    let cells: Vec<Vec<String>> = shown
        .iter()
        .map(|r| {
            cols.iter()
                .map(|c| {
                    super::value::get(r, c)
                        .map(|v| clean(&v.display()))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    let numeric: Vec<bool> = cols
        .iter()
        .map(|c| {
            shown
                .iter()
                .filter_map(|r| super::value::get(r, c))
                .filter(|v| !matches!(v, Value::Nothing))
                .all(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Size(_) | Value::Duration(_)
                    )
                })
        })
        .collect();
    let idx_w = shown.len().saturating_sub(1).to_string().len().max(1);
    let mut w: Vec<usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            cells
                .iter()
                .map(|r| width_of(&r[i]))
                .max()
                .unwrap_or(0)
                .max(width_of(c))
        })
        .collect();
    // borders: "│ " before each column and " │" at the end
    let budget = st.width.saturating_sub(idx_w + 4 + 3 * w.len());
    while w.iter().sum::<usize>() > budget {
        let (i, &m) = w.iter().enumerate().max_by_key(|(_, x)| **x).unwrap();
        if m <= 6 {
            break;
        }
        w[i] = m - 1;
    }
    let (b, g, r, dim) = if st.color {
        (BORDER, BOLD_GREEN, RESET, DIM)
    } else {
        ("", "", "", "")
    };
    let rule = |l: &str, m: &str, rt: &str| {
        let mut s = format!("{b}{l}{}", "─".repeat(idx_w + 2));
        for x in &w {
            s.push_str(m);
            s.push_str(&"─".repeat(x + 2));
        }
        format!("{s}{rt}{r}\n")
    };
    let mut out = rule("╭", "┬", "╮");
    let mut head = format!("{b}│{r} {g}{}{r} ", pad("#", idx_w, true));
    for (i, c) in cols.iter().enumerate() {
        head.push_str(&format!(
            "{b}│{r} {g}{}{r} ",
            pad(&fit(c, w[i]), w[i], numeric[i])
        ));
    }
    out.push_str(&format!("{head}{b}│{r}\n"));
    out.push_str(&rule("├", "┼", "┤"));
    for (n, row) in cells.iter().enumerate() {
        let mut line = format!("{b}│{r} {g}{}{r} ", pad(&n.to_string(), idx_w, true));
        for (i, cell) in row.iter().enumerate() {
            let text = pad(&fit(cell, w[i]), w[i], numeric[i]);
            let c = if st.color && i == 0 {
                name_color(&shown[n], st)
            } else {
                String::new()
            };
            let end = if c.is_empty() { "" } else { r };
            line.push_str(&format!("{b}│{r} {c}{text}{end} "));
        }
        out.push_str(&format!("{line}{b}│{r}\n"));
    }
    out.push_str(&rule("╰", "┴", "╯"));
    if rows.len() > shown.len() {
        let n = rows.len() - shown.len();
        out.push_str(&format!(
            "{dim}… {n} more row{} ({} total){r}\n",
            if n == 1 { "" } else { "s" },
            rows.len()
        ));
    }
    out
}

/// GitHub-flavoured markdown table.
pub fn to_markdown(d: &Data) -> String {
    let rows = d.clone().into_rows();
    let cols = columns(&rows);
    let esc = |s: String| s.replace('|', "\\|").replace('\n', " ");
    let mut out = format!(
        "| {} |\n|{}|\n",
        cols.join(" | "),
        cols.iter().map(|_| "---").collect::<Vec<_>>().join("|")
    );
    for r in &rows {
        let cells: Vec<String> = cols
            .iter()
            .map(|c| {
                esc(super::value::get(r, c)
                    .map(Value::display)
                    .unwrap_or_default())
            })
            .collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Style {
        Style {
            color: false,
            width: 80,
            max_rows: 1000,
            boxed: false,
            folder: None,
        }
    }

    #[test]
    fn csv_round_trip_with_quotes() {
        let text = "name,size,note\nCargo.toml,1200,\"has, comma\"\n\"q\"\"uote\",5,\n";
        let d = parse_csv(text, ',').unwrap();
        let Data::Table(rows) = &d else { panic!() };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].1, Value::Int(1200));
        assert_eq!(rows[0][2].1, Value::Str("has, comma".into()));
        assert_eq!(rows[1][0].1, Value::Str("q\"uote".into()));
        assert_eq!(rows[1][2].1, Value::Nothing);
        let back = to_csv(&d, ',');
        assert_eq!(parse_csv(&back, ',').unwrap(), d);
    }

    #[test]
    fn json_lines_keep_types_through_names() {
        let rows = vec![vec![
            ("name".to_string(), Value::Str("a".into())),
            ("size".to_string(), Value::Size(2048)),
            ("modified".to_string(), Value::Date(1_767_225_600)),
            ("took_ms".to_string(), Value::Duration(1500)),
        ]];
        let d = Data::Table(rows);
        let mut buf = Vec::new();
        write_jsonl(&d, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.contains("\"modified\":\"2026-01-01T00:00:00Z\""),
            "{text}"
        );
        assert_eq!(parse_json(&text).unwrap(), d);
    }

    #[test]
    fn json_documents_and_scalars() {
        let d = parse_json("[{\"a\":1},{\"a\":2}]").unwrap();
        assert!(matches!(&d, Data::Table(r) if r.len() == 2));
        let d = parse_json("{\n  \"a\": {\"b\": [1, 2]}\n}\n").unwrap();
        assert!(matches!(d, Data::Value(Value::Record(_))) || matches!(d, Data::Table(_)));
        assert_eq!(
            parse_json("3\n4\n").unwrap(),
            Data::Value(Value::List(vec![Value::Int(3), Value::Int(4)]))
        );
        assert!(parse_json("{nope").is_err());
    }

    #[test]
    fn yaml_and_toml() {
        let d = parse_yaml("- name: a\n  n: 1\n- name: b\n  n: 2\n").unwrap();
        assert!(matches!(&d, Data::Table(r) if r.len() == 2));
        let d = parse_toml("[package]\nname = \"sv\"\nversion = \"0.1\"\n").unwrap();
        let Data::Value(Value::Record(r)) = d else {
            panic!()
        };
        assert_eq!(r[0].0, "package");
    }

    #[test]
    fn ssv_columns() {
        let d = parse_ssv(
            "USER  PID %CPU COMMAND\nroot    1  0.0 /sbin/init splash\nme     42  1.5 vim a b\n",
        );
        let Data::Table(rows) = d else { panic!() };
        assert_eq!(rows[1][1], ("pid".to_string(), Value::Int(42)));
        assert_eq!(
            rows[1][3],
            ("command".to_string(), Value::Str("vim a b".into()))
        );
        let d = parse_ssv("CONTAINER ID   IMAGE     STATUS\nabc123         nginx     Up 2 hours\n");
        let Data::Table(rows) = d else { panic!() };
        assert_eq!(rows[0][0].0, "container id");
        assert_eq!(rows[0][2].1, Value::Str("Up 2 hours".into()));
    }

    #[test]
    fn text_table() {
        let rows = vec![
            vec![
                ("name".to_string(), Value::Str("Cargo.toml".into())),
                ("size".to_string(), Value::Size(1200)),
            ],
            vec![
                ("name".to_string(), Value::Str("src".into())),
                ("size".to_string(), Value::Size(64)),
            ],
        ];
        let t = render_table(&rows, &plain());
        assert_eq!(
            t,
            "#  name          size\n0  Cargo.toml  1.2 kB\n1  src           64 B\n"
        );
        let narrow = render_table(
            &rows,
            &Style {
                color: false,
                width: 18,
                max_rows: 1,
                boxed: false,
                folder: None,
            },
        );
        // nushell-style borders, folders coloured
        let boxed = render_table(
            &rows,
            &Style {
                color: false,
                width: 80,
                max_rows: 10,
                boxed: true,
                folder: None,
            },
        );
        assert!(boxed.starts_with("╭───┬"), "{boxed}");
        assert!(boxed.contains("│ # │ name       │   size │"), "{boxed}");
        assert!(boxed.contains("│ 0 │ Cargo.toml │ 1.2 kB │"), "{boxed}");
        assert!(boxed.trim_end().ends_with("╯"), "{boxed}");
        assert!(
            narrow.lines().take(2).all(|l| width_of(l) <= 18),
            "{narrow}"
        );
        assert!(narrow.contains("Cargo.…"), "{narrow}");
        assert!(narrow.contains("… 1 more row (2 total)"));
        let rec = render_text(&Data::Value(Value::Record(rows[0].clone())), &plain());
        assert_eq!(rec, "name  Cargo.toml\nsize  1.2 kB\n");
        assert!(
            to_markdown(&Data::Table(rows))
                .starts_with("| name | size |\n|---|---|\n| Cargo.toml | 1.2 kB |")
        );
    }
}

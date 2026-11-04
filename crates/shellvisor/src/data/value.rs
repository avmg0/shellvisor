//! The value model for `sv` data commands.
//!
//! A table is a list of rows; a row is an *ordered* list of (column, value).
//! Values carry a little type — sizes, durations and dates — so that
//! `where size gt 1mb` or `sort-by modified` do the right thing.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nothing,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Bytes.
    Size(u64),
    /// Milliseconds.
    Duration(i64),
    /// Seconds since the Unix epoch (UTC).
    Date(i64),
    Str(String),
    List(Vec<Value>),
    Record(Record),
}

pub type Record = Vec<(String, Value)>;

/// What a stage produces.
#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Table(Vec<Record>),
    Value(Value),
}

impl Data {
    /// View any data as rows (a scalar becomes one row with a `value` column,
    /// a list one row per item).
    pub fn into_rows(self) -> Vec<Record> {
        match self {
            Data::Table(t) => t,
            Data::Value(Value::List(items)) => items.into_iter().map(item_row).collect(),
            Data::Value(Value::Record(r)) => vec![r],
            Data::Value(Value::Nothing) => Vec::new(),
            Data::Value(v) => vec![vec![("value".into(), v)]],
        }
    }
}

fn item_row(v: Value) -> Record {
    match v {
        Value::Record(r) => r,
        v => vec![("value".into(), v)],
    }
}

/// Columns in order of first appearance.
pub fn columns(rows: &[Record]) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for r in rows.iter().take(10_000) {
        for (k, _) in r {
            if !cols.iter().any(|c| c == k) {
                cols.push(k.clone());
            }
        }
    }
    cols
}

pub fn get<'a>(row: &'a Record, col: &str) -> Option<&'a Value> {
    if let Some((_, v)) = row.iter().find(|(k, _)| k == col) {
        return Some(v);
    }
    // a.b → nested record field
    let (head, rest) = col.split_once('.')?;
    match get(row, head)? {
        Value::Record(r) => get(r, rest),
        _ => None,
    }
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nothing => "nothing",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Size(_) => "size",
            Value::Duration(_) => "duration",
            Value::Date(_) => "date",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Record(_) => "record",
        }
    }

    /// A number for maths and comparisons (sizes in bytes, durations in ms,
    /// dates in seconds).
    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            Value::Int(i) => *i as f64,
            Value::Float(f) => *f,
            Value::Size(s) => *s as f64,
            Value::Duration(d) => *d as f64,
            Value::Date(d) => *d as f64,
            Value::Bool(b) => *b as i64 as f64,
            Value::Str(s) => s.trim().parse::<f64>().ok()?,
            _ => return None,
        })
    }

    /// Plain text (no units for numbers, ISO dates) — used for matching and CSV.
    pub fn plain(&self) -> String {
        match self {
            Value::Nothing => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Size(s) => s.to_string(),
            Value::Duration(d) => d.to_string(),
            Value::Date(d) => iso_date(*d),
            Value::Str(s) => s.clone(),
            Value::List(l) => l.iter().map(Value::plain).collect::<Vec<_>>().join(", "),
            Value::Record(r) => r
                .iter()
                .map(|(k, v)| format!("{k}: {}", v.plain()))
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// For people: "1.2 MB", "3 days ago", "1m 20s".
    pub fn display(&self) -> String {
        match self {
            Value::Size(s) => fmt_size(*s),
            Value::Duration(d) => fmt_duration(*d),
            Value::Date(d) => fmt_date_relative(*d, now()),
            Value::List(l) => format!("[{} items]", l.len()),
            Value::Record(r) => format!("{{{} fields}}", r.len()),
            v => v.plain(),
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Value::Int(_) | Value::Float(_) | Value::Size(_) | Value::Duration(_) | Value::Date(_)
        )
    }

    /// Order two values; numbers of any kind compare as numbers, strings
    /// case-insensitively, nothing sorts first.
    pub fn compare(&self, other: &Value) -> Ordering {
        use Value::*;
        match (self, other) {
            (Nothing, Nothing) => Ordering::Equal,
            (Nothing, _) => Ordering::Less,
            (_, Nothing) => Ordering::Greater,
            (Str(a), Str(b)) => natural(a, b),
            (a, b) if a.is_numeric() || b.is_numeric() => match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => natural(&a.plain(), &b.plain()),
            },
            (Bool(a), Bool(b)) => a.cmp(b),
            (a, b) => natural(&a.plain(), &b.plain()),
        }
    }

    /// Read a literal typed by the user: `12`, `1.5`, `true`, `10mb`,
    /// `2s`, `2026-01-31`, anything else is a string.
    pub fn parse_literal(s: &str) -> Value {
        let t = s.trim();
        match t {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            "null" | "nothing" => return Value::Nothing,
            _ => {}
        }
        if let Ok(i) = t.parse::<i64>() {
            return Value::Int(i);
        }
        if let Ok(f) = t.parse::<f64>() {
            if t.trim_start_matches('-')
                .starts_with(|c: char| c.is_ascii_digit() || c == '.')
            {
                return Value::Float(f);
            }
        }
        if let Some(b) = parse_size(t) {
            return Value::Size(b);
        }
        if let Some(ms) = parse_duration(t) {
            return Value::Duration(ms);
        }
        if let Some(d) = parse_date(t) {
            return Value::Date(d);
        }
        Value::Str(unquote(t).to_string())
    }
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// "file10" after "file9"; case-insensitive.
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (nt, mt) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = nt.len().cmp(&mt.len()).then_with(|| nt.cmp(mt));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

// ------------------------------------------------------------------ units

pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" | "k" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mult).round() as u64)
}

pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let ms: f64 = match unit.trim() {
        "ms" => 1.0,
        "s" | "sec" | "secs" => 1e3,
        "m" | "min" | "mins" => 60e3,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600e3,
        "d" | "day" | "days" => 86_400e3,
        "w" | "wk" | "week" | "weeks" => 604_800e3,
        _ => return None,
    };
    Some((n * ms).round() as i64)
}

pub fn fmt_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Decimal units, like Finder: 1.2 kB, 3.4 MB.
pub fn fmt_size(b: u64) -> String {
    const U: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if b < 1000 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

pub fn fmt_duration(ms: i64) -> String {
    let neg = if ms < 0 { "-" } else { "" };
    let ms = ms.unsigned_abs();
    let s = ms / 1000;
    let out = if ms < 1000 {
        format!("{ms}ms")
    } else if s < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    };
    format!("{neg}{out}")
}

// ------------------------------------------------------------------ dates

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The local UTC offset in seconds at `t`.
fn local_offset(t: i64) -> i64 {
    unsafe {
        let tt = t as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

/// `2026-01-31`, `2026-01-31 10:00`, `2026-01-31T10:00:00Z`, `…+02:00`.
/// Without an offset the time is local.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = 0i64;
    let mut rest = &s[10..];
    if let Some(r) = rest.strip_prefix(['T', ' ']) {
        let hh: i64 = r.get(0..2)?.parse().ok()?;
        if r.as_bytes().get(2) != Some(&b':') {
            return None;
        }
        let mm: i64 = r.get(3..5)?.parse().ok()?;
        secs = hh * 3600 + mm * 60;
        rest = &r[5..];
        if let Some(r) = rest.strip_prefix(':') {
            let ss: i64 = r.get(0..2)?.parse().ok()?;
            secs += ss;
            rest = &r[2..];
            if let Some(r) = rest.strip_prefix('.') {
                rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
            }
        }
    }
    let base = days_from_civil(y, m, d) * 86_400 + secs;
    let offset = match rest {
        "" => return Some(base - local_offset(base)),
        "Z" | "z" => 0,
        o if (o.starts_with('+') || o.starts_with('-')) && o.len() >= 3 => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            let digits: String = o[1..].chars().filter(char::is_ascii_digit).collect();
            let hh: i64 = digits.get(0..2)?.parse().ok()?;
            let mm: i64 = digits.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
            sign * (hh * 3600 + mm * 60)
        }
        _ => return None,
    };
    Some(base - offset)
}

/// UTC ISO-8601, e.g. `2026-11-04T10:23:00Z`.
pub fn iso_date(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

/// Local "2026-11-04 10:23".
pub fn local_date(t: i64) -> String {
    let l = t + local_offset(t);
    let (y, m, d) = civil_from_days(l.div_euclid(86_400));
    let s = l.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        s / 3600,
        (s % 3600) / 60
    )
}

/// "just now", "5 min ago", "3 days ago", or a date for older things.
pub fn fmt_date_relative(t: i64, now: i64) -> String {
    let age = now - t;
    if age < -60 {
        return local_date(t);
    }
    match age {
        ..60 => "just now".into(),
        60..3600 => format!("{} min ago", age / 60),
        3600..86_400 => plural(age / 3600, "hour"),
        86_400..2_592_000 => plural(age / 86_400, "day"),
        _ => local_date(t)[..10].to_string(),
    }
}

fn plural(n: i64, unit: &str) -> String {
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(Value::parse_literal("12"), Value::Int(12));
        assert_eq!(Value::parse_literal("1.5"), Value::Float(1.5));
        assert_eq!(Value::parse_literal("true"), Value::Bool(true));
        assert_eq!(Value::parse_literal("10mb"), Value::Size(10_000_000));
        assert_eq!(Value::parse_literal("2KiB"), Value::Size(2048));
        assert_eq!(Value::parse_literal("90s"), Value::Duration(90_000));
        assert_eq!(Value::parse_literal("2d"), Value::Duration(172_800_000));
        assert_eq!(
            Value::parse_literal("2026-01-01T00:00:00Z"),
            Value::Date(1_767_225_600)
        );
        assert_eq!(
            Value::parse_literal("'hi there'"),
            Value::Str("hi there".into())
        );
        assert_eq!(
            Value::parse_literal("main.rs"),
            Value::Str("main.rs".into())
        );
        assert_eq!(Value::parse_literal("1e3x"), Value::Str("1e3x".into()));
        assert_eq!(Value::parse_literal("nan"), Value::Str("nan".into()));
    }

    #[test]
    fn dates_round_trip() {
        for t in [0i64, 1_767_225_600, 1_793_000_123, -86_400] {
            assert_eq!(parse_date(&iso_date(t)), Some(t), "{}", iso_date(t));
        }
        assert_eq!(parse_date("2026-01-01T02:00:00+02:00"), Some(1_767_225_600));
        assert_eq!(parse_date("not a date"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        let now = 1_800_000_000;
        assert_eq!(fmt_date_relative(now - 10, now), "just now");
        assert_eq!(fmt_date_relative(now - 7200, now), "2 hours ago");
        assert_eq!(fmt_date_relative(now - 86_400, now), "1 day ago");
    }

    #[test]
    fn ordering() {
        assert_eq!(natural("file9", "file10"), Ordering::Less);
        assert_eq!(natural("B", "a"), Ordering::Greater);
        assert_eq!(Value::Size(10).compare(&Value::Int(9)), Ordering::Greater);
        assert_eq!(Value::Nothing.compare(&Value::Int(0)), Ordering::Less);
        assert_eq!(
            Value::Str("2".into()).compare(&Value::Int(10)),
            Ordering::Less
        );
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_size(999), "999 B");
        assert_eq!(fmt_size(1_234_567), "1.2 MB");
        assert_eq!(fmt_duration(450), "450ms");
        assert_eq!(fmt_duration(80_000), "1m 20s");
        assert_eq!(fmt_float(2.50), "2.5");
        assert_eq!(fmt_float(3.0), "3");
    }

    #[test]
    fn nested_get_and_rows() {
        let row: Record = vec![("a".into(), Value::Record(vec![("b".into(), Value::Int(1))]))];
        assert_eq!(get(&row, "a.b"), Some(&Value::Int(1)));
        assert_eq!(get(&row, "a.c"), None);
        let rows = Data::Value(Value::List(vec![Value::Int(1), Value::Int(2)])).into_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(columns(&rows), vec!["value"]);
    }
}

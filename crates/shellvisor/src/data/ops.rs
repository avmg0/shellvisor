//! Pipeline parsing and the table filters (`where`, `sort-by`, `select` …).

use super::value::{Data, Record, Value, columns, get, now};
use std::cmp::Ordering;

/// Split `ls | where size gt 1mb | sort-by size -r` into stages of words,
/// shell-style: '…' and "…" quote, \ escapes, `|` outside quotes separates.
/// A leading `sv` on a stage is dropped, so `sv ls | sv where …` also works.
pub fn split_pipeline(line: &str) -> Result<Vec<Vec<String>>, String> {
    let mut stages = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                    in_word = true;
                }
            }
            (None, '|') => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                stages.push(std::mem::take(&mut words));
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if in_word {
        words.push(word);
    }
    stages.push(words);
    for s in &mut stages {
        if s.first().map(String::as_str) == Some("sv") {
            s.remove(0);
        }
        if s.is_empty() {
            return Err("empty stage in the pipeline (a `|` with nothing next to it)".into());
        }
    }
    Ok(stages)
}

// ------------------------------------------------------------------ where

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Has,
    NotHas,
    Starts,
    Ends,
    Like,
    In,
    Within,
}

fn op(word: &str) -> Option<Op> {
    Some(match word {
        "eq" | "==" | "=" | "is" => Op::Eq,
        "ne" | "!=" => Op::Ne,
        "gt" | ">" => Op::Gt,
        "lt" | "<" => Op::Lt,
        "ge" | ">=" => Op::Ge,
        "le" | "<=" => Op::Le,
        "has" | "contains" | "=~" => Op::Has,
        "not-has" | "!~" => Op::NotHas,
        "starts" | "starts-with" => Op::Starts,
        "ends" | "ends-with" => Op::Ends,
        "like" => Op::Like,
        "in" => Op::In,
        "within" => Op::Within,
        _ => return None,
    })
}

#[derive(Debug, Clone)]
struct Cond {
    not: bool,
    col: String,
    test: Option<(Op, Value, String)>,
}

/// `where` words → OR-groups of AND-ed conditions.
fn parse_where(words: &[String]) -> Result<Vec<Vec<Cond>>, String> {
    if words.is_empty() {
        return Err("where: what to keep? e.g. `where size gt 1mb`, `where name has test`".into());
    }
    // Allow the whole condition as one quoted word: where 'size > 1mb'
    let words: Vec<String> = if words.len() == 1 && words[0].contains(' ') {
        words[0].split_whitespace().map(String::from).collect()
    } else {
        words.to_vec()
    };
    let mut groups = vec![Vec::new()];
    let mut i = 0;
    while i < words.len() {
        let mut not = false;
        if words[i] == "not" {
            not = true;
            i += 1;
        }
        let col = words
            .get(i)
            .ok_or("where: a column name is missing")?
            .clone();
        i += 1;
        let test = match words.get(i).and_then(|w| op(w)) {
            Some(o) => {
                let raw = words
                    .get(i + 1)
                    .ok_or_else(|| format!("where: `{}` needs a value after it", words[i]))?
                    .clone();
                i += 2;
                Some((o, Value::parse_literal(&raw), raw))
            }
            None => {
                if let Some(w) = words.get(i) {
                    if w != "and" && w != "or" {
                        return Err(format!(
                            "where: unknown comparison `{w}` — use eq ne gt lt ge le has starts ends like in within (or == != > < >= <= =~)"
                        ));
                    }
                }
                None
            }
        };
        groups.last_mut().unwrap().push(Cond { not, col, test });
        match words.get(i).map(String::as_str) {
            Some("and") => i += 1,
            Some("or") => {
                groups.push(Vec::new());
                i += 1;
            }
            Some(w) => return Err(format!("where: expected `and` / `or`, found `{w}`")),
            None => {}
        }
    }
    Ok(groups)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Nothing => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Size(s) => *s != 0,
        Value::Duration(d) => *d != 0,
        Value::Str(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Record(r) => !r.is_empty(),
        Value::Date(_) => true,
    }
}

fn glob(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

fn test(v: &Value, op: Op, lit: &Value, raw: &str) -> bool {
    let lower = |x: &str| x.to_lowercase();
    match op {
        Op::Eq => match (v, lit) {
            (Value::Str(a), _) => {
                a == raw || (lit.is_numeric() && v.compare(lit) == Ordering::Equal)
            }
            _ => v.compare(lit) == Ordering::Equal,
        },
        Op::Ne => !test(v, Op::Eq, lit, raw),
        Op::Gt => !matches!(v, Value::Nothing) && v.compare(lit) == Ordering::Greater,
        Op::Lt => !matches!(v, Value::Nothing) && v.compare(lit) == Ordering::Less,
        Op::Ge => !matches!(v, Value::Nothing) && v.compare(lit) != Ordering::Less,
        Op::Le => !matches!(v, Value::Nothing) && v.compare(lit) != Ordering::Greater,
        Op::Has => contains_anchored(&lower(&v.plain()), &lower(raw)),
        Op::NotHas => !contains_anchored(&lower(&v.plain()), &lower(raw)),
        Op::Starts => lower(&v.plain()).starts_with(&lower(raw)),
        Op::Ends => lower(&v.plain()).ends_with(&lower(raw)),
        Op::Like => glob(raw, &v.plain()),
        Op::In => raw.split(',').any(|x| {
            let x = x.trim();
            v.plain() == x || v.compare(&Value::parse_literal(x)) == Ordering::Equal
        }),
        Op::Within => match (v, lit) {
            (Value::Date(d), Value::Duration(ms)) => now() - d <= ms / 1000,
            _ => false,
        },
    }
}

/// A row test from `where`-style words, for the table viewer's filter.
pub fn row_matcher(words: &[String]) -> Result<impl Fn(&Record) -> bool, String> {
    let groups = parse_where(words)?;
    Ok(move |r: &Record| keep(r, &groups))
}

/// Substring test where a leading `^` / trailing `$` pin the start / end
/// (`name =~ py$`).
fn contains_anchored(hay: &str, pat: &str) -> bool {
    match (pat.strip_prefix('^'), pat.strip_suffix('$')) {
        (Some(p), _) if p.ends_with('$') && p.len() > 1 => hay == &p[..p.len() - 1],
        (Some(p), _) => hay.starts_with(p),
        (None, Some(p)) => hay.ends_with(p),
        (None, None) => hay.contains(pat),
    }
}

fn keep(row: &Record, groups: &[Vec<Cond>]) -> bool {
    groups.iter().any(|g| {
        g.iter().all(|c| {
            let v = get(row, &c.col).unwrap_or(&Value::Nothing);
            let r = match &c.test {
                Some((o, lit, raw)) => test(v, *o, lit, raw),
                None => truthy(v),
            };
            r != c.not
        })
    })
}

// ------------------------------------------------------------------ filters

fn rows_of(d: Data) -> Vec<Record> {
    d.into_rows()
}

fn need_col<'a>(cmd: &str, args: &'a [String]) -> Result<&'a str, String> {
    args.first()
        .map(String::as_str)
        .ok_or_else(|| format!("{cmd}: which column? e.g. `{cmd} size`"))
}

fn count_arg(cmd: &str, args: &[String], default: usize) -> Result<usize, String> {
    match args.first() {
        None => Ok(default),
        Some(n) => n
            .parse()
            .map_err(|_| format!("{cmd}: `{n}` is not a number")),
    }
}

/// The values a column-wise aggregate works on.
fn values_for(cmd: &str, d: Data, args: &[String]) -> Result<Vec<Value>, String> {
    match d {
        Data::Value(Value::List(l)) => Ok(l),
        d => {
            let rows = rows_of(d);
            let col = match args.first() {
                Some(c) => c.clone(),
                None => {
                    let cols = columns(&rows);
                    if cols.len() == 1 {
                        cols[0].clone()
                    } else {
                        return Err(format!("{cmd}: which column? e.g. `{cmd} size`"));
                    }
                }
            };
            Ok(rows.iter().filter_map(|r| get(r, &col).cloned()).collect())
        }
    }
}

fn sum(values: &[Value]) -> Value {
    let nums: Vec<&Value> = values
        .iter()
        .filter(|v| v.as_f64().is_some() && !matches!(v, Value::Str(_)))
        .collect();
    if nums.iter().all(|v| matches!(v, Value::Size(_))) && !nums.is_empty() {
        return Value::Size(nums.iter().map(|v| v.as_f64().unwrap() as u64).sum());
    }
    if nums.iter().all(|v| matches!(v, Value::Duration(_))) && !nums.is_empty() {
        return Value::Duration(nums.iter().map(|v| v.as_f64().unwrap() as i64).sum());
    }
    if nums.iter().all(|v| matches!(v, Value::Int(_))) {
        return Value::Int(nums.iter().map(|v| v.as_f64().unwrap() as i64).sum());
    }
    Value::Float(nums.iter().map(|v| v.as_f64().unwrap()).sum())
}

/// Same unit as the inputs (avg of sizes is a size).
fn like_kind(sample: Option<&Value>, x: f64) -> Value {
    match sample {
        Some(Value::Size(_)) => Value::Size(x.round() as u64),
        Some(Value::Duration(_)) => Value::Duration(x.round() as i64),
        Some(Value::Date(_)) => Value::Date(x.round() as i64),
        _ => Value::Float(x),
    }
}

pub const FILTERS: &[(&str, &str)] = &[
    (
        "where",
        "keep rows that match: where size gt 1mb and name has log",
    ),
    ("select", "keep these columns: select name size"),
    ("reject", "drop these columns: reject type"),
    (
        "sort-by",
        "sort by columns (-r to reverse): sort-by size -r",
    ),
    ("reverse", "reverse the row order"),
    ("first", "first n rows (default 1): first 10"),
    ("last", "last n rows (default 1): last 5"),
    ("skip", "skip n rows: skip 2"),
    (
        "uniq",
        "unique rows, or unique values of a column: uniq type",
    ),
    ("get", "one column as a list: get name"),
    ("group-by", "count rows per value: group-by type"),
    ("count", "number of rows"),
    ("sum", "sum of a column: sum size"),
    ("avg", "average of a column"),
    ("min", "smallest value of a column"),
    ("max", "largest value of a column"),
    ("columns", "the column names"),
    ("describe", "each column and its type"),
];

pub fn is_filter(name: &str) -> bool {
    FILTERS.iter().any(|(n, _)| *n == name) || name == "length" || name == "sort"
}

/// Run one filter on its input.
pub fn filter(cmd: &str, args: &[String], input: Data) -> Result<Data, String> {
    Ok(match cmd {
        "where" => {
            let groups = parse_where(args)?;
            match input {
                Data::Value(Value::List(l)) => Data::Value(Value::List(
                    l.into_iter()
                        .filter(|v| keep(&vec![("value".into(), v.clone())], &groups))
                        .collect(),
                )),
                d => Data::Table(
                    rows_of(d)
                        .into_iter()
                        .filter(|r| keep(r, &groups))
                        .collect(),
                ),
            }
        }
        "select" => {
            if args.is_empty() {
                return Err("select: which columns? e.g. `select name size`".into());
            }
            Data::Table(
                rows_of(input)
                    .into_iter()
                    .map(|r| {
                        args.iter()
                            .map(|c| (c.clone(), get(&r, c).cloned().unwrap_or(Value::Nothing)))
                            .collect()
                    })
                    .collect(),
            )
        }
        "reject" => Data::Table(
            rows_of(input)
                .into_iter()
                .map(|r| r.into_iter().filter(|(k, _)| !args.contains(k)).collect())
                .collect(),
        ),
        "sort-by" | "sort" => {
            let reverse = args.iter().any(|a| a == "-r" || a == "--reverse");
            let cols: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
            match input {
                Data::Value(Value::List(mut l)) => {
                    l.sort_by(|a, b| a.compare(b));
                    if reverse {
                        l.reverse();
                    }
                    Data::Value(Value::List(l))
                }
                d => {
                    let mut rows = rows_of(d);
                    if cols.is_empty() && cmd == "sort-by" {
                        return Err("sort-by: which column? e.g. `sort-by size`".into());
                    }
                    let first_col = columns(&rows).into_iter().next();
                    let cols: Vec<String> = if cols.is_empty() {
                        first_col.into_iter().collect()
                    } else {
                        cols.into_iter().cloned().collect()
                    };
                    rows.sort_by(|a, b| {
                        for c in &cols {
                            let o = get(a, c)
                                .unwrap_or(&Value::Nothing)
                                .compare(get(b, c).unwrap_or(&Value::Nothing));
                            if o != Ordering::Equal {
                                return o;
                            }
                        }
                        Ordering::Equal
                    });
                    if reverse {
                        rows.reverse();
                    }
                    Data::Table(rows)
                }
            }
        }
        "reverse" => match input {
            Data::Value(Value::List(mut l)) => {
                l.reverse();
                Data::Value(Value::List(l))
            }
            d => {
                let mut r = rows_of(d);
                r.reverse();
                Data::Table(r)
            }
        },
        "first" | "last" | "skip" => {
            let n = count_arg(cmd, args, 1)?;
            let pick = |len: usize| match cmd {
                "first" => 0..n.min(len),
                "last" => len.saturating_sub(n)..len,
                _ => n.min(len)..len,
            };
            match input {
                Data::Value(Value::List(l)) => {
                    let r = pick(l.len());
                    Data::Value(Value::List(l[r].to_vec()))
                }
                d => {
                    let rows = rows_of(d);
                    let r = pick(rows.len());
                    Data::Table(rows[r].to_vec())
                }
            }
        }
        "uniq" => {
            let mut seen = std::collections::HashSet::new();
            match (input, args.first()) {
                (Data::Value(Value::List(l)), _) => Data::Value(Value::List(
                    l.into_iter().filter(|v| seen.insert(v.plain())).collect(),
                )),
                (d, Some(col)) => Data::Value(Value::List(
                    rows_of(d)
                        .iter()
                        .filter_map(|r| get(r, col).cloned())
                        .filter(|v| seen.insert(v.plain()))
                        .collect(),
                )),
                (d, None) => Data::Table(
                    rows_of(d)
                        .into_iter()
                        .filter(|r| seen.insert(format!("{r:?}")))
                        .collect(),
                ),
            }
        }
        "get" => {
            let col = need_col(cmd, args)?;
            match input {
                Data::Value(Value::Record(r)) => {
                    Data::Value(get(&r, col).cloned().unwrap_or(Value::Nothing))
                }
                d => {
                    let rows = rows_of(d);
                    if rows.iter().all(|r| get(r, col).is_none()) && !rows.is_empty() {
                        return Err(format!(
                            "get: no column `{col}` (columns: {})",
                            columns(&rows).join(", ")
                        ));
                    }
                    Data::Value(Value::List(
                        rows.iter()
                            .map(|r| get(r, col).cloned().unwrap_or(Value::Nothing))
                            .collect(),
                    ))
                }
            }
        }
        "group-by" => {
            let col = need_col(cmd, args)?;
            let mut groups: Vec<(Value, i64)> = Vec::new();
            let mut index: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for r in rows_of(input) {
                let v = get(&r, col).cloned().unwrap_or(Value::Nothing);
                let key = v.plain();
                match index.get(&key) {
                    Some(&i) => groups[i].1 += 1,
                    None => {
                        index.insert(key, groups.len());
                        groups.push((v, 1));
                    }
                }
            }
            groups.sort_by_key(|g| std::cmp::Reverse(g.1));
            Data::Table(
                groups
                    .into_iter()
                    .map(|(v, n)| vec![(col.to_string(), v), ("count".to_string(), Value::Int(n))])
                    .collect(),
            )
        }
        "count" | "length" => Data::Value(Value::Int(match input {
            Data::Value(Value::List(l)) => l.len(),
            d => rows_of(d).len(),
        } as i64)),
        "sum" => Data::Value(sum(&values_for(cmd, input, args)?)),
        "avg" => {
            let v = values_for(cmd, input, args)?;
            let nums: Vec<f64> = v.iter().filter_map(Value::as_f64).collect();
            if nums.is_empty() {
                Data::Value(Value::Nothing)
            } else {
                Data::Value(like_kind(
                    v.first(),
                    nums.iter().sum::<f64>() / nums.len() as f64,
                ))
            }
        }
        "min" | "max" => {
            let v = values_for(cmd, input, args)?;
            let it = v.into_iter().filter(|x| !matches!(x, Value::Nothing));
            Data::Value(
                if cmd == "min" {
                    it.min_by(|a, b| a.compare(b))
                } else {
                    it.max_by(|a, b| a.compare(b))
                }
                .unwrap_or(Value::Nothing),
            )
        }
        "columns" => {
            let rows = rows_of(input);
            Data::Value(Value::List(
                columns(&rows).into_iter().map(Value::Str).collect(),
            ))
        }
        "describe" => {
            let rows = rows_of(input);
            Data::Table(
                columns(&rows)
                    .into_iter()
                    .map(|c| {
                        let t = rows
                            .iter()
                            .filter_map(|r| get(r, &c))
                            .find(|v| !matches!(v, Value::Nothing))
                            .map(Value::type_name)
                            .unwrap_or("nothing");
                        vec![
                            ("column".to_string(), Value::Str(c)),
                            ("type".to_string(), Value::Str(t.into())),
                        ]
                    })
                    .collect(),
            )
        }
        other => return Err(format!("unknown command `{other}`")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Data {
        let row = |n: &str, t: &str, s: u64, age: i64| {
            vec![
                ("name".to_string(), Value::Str(n.into())),
                ("type".to_string(), Value::Str(t.into())),
                ("size".to_string(), Value::Size(s)),
                ("modified".to_string(), Value::Date(now() - age)),
            ]
        };
        Data::Table(vec![
            row("Cargo.toml", "file", 1_200, 60),
            row("src", "dir", 4_096, 3_600),
            row("big.log", "file", 5_000_000, 10 * 86_400),
            row("notes.md", "file", 300, 120),
        ])
    }

    fn run(line: &str) -> Data {
        let mut d = files();
        for stage in split_pipeline(line).unwrap() {
            d = filter(&stage[0], &stage[1..], d).unwrap();
        }
        d
    }

    fn names(d: &Data) -> Vec<String> {
        d.clone()
            .into_rows()
            .iter()
            .map(|r| get(r, "name").unwrap().plain())
            .collect()
    }

    #[test]
    fn pipeline_words() {
        assert_eq!(
            split_pipeline("sv ls | where name has 'a b' | sort-by \"size\" -r").unwrap(),
            vec![
                vec!["ls"],
                vec!["where", "name", "has", "a b"],
                vec!["sort-by", "size", "-r"]
            ]
        );
        assert_eq!(
            split_pipeline("where x eq a\\|b").unwrap(),
            vec![vec!["where", "x", "eq", "a|b"]]
        );
        assert!(split_pipeline("ls |").is_err());
        assert!(split_pipeline("ls 'open").is_err());
    }

    #[test]
    fn where_conditions() {
        assert_eq!(
            names(&run("where size gt 1kb")),
            vec!["Cargo.toml", "src", "big.log"]
        );
        assert_eq!(names(&run("where size > 1mb")), vec!["big.log"]);
        assert_eq!(names(&run("where 'size >= 4kb'")), vec!["src", "big.log"]);
        assert_eq!(
            names(&run("where type eq file and size lt 1kb")),
            vec!["notes.md"]
        );
        assert_eq!(
            names(&run("where name ends .md or name like *.log")),
            vec!["big.log", "notes.md"]
        );
        assert_eq!(
            names(&run("where modified within 1d")),
            vec!["Cargo.toml", "src", "notes.md"]
        );
        assert_eq!(
            names(&run("where not type eq dir and name =~ O")),
            vec!["Cargo.toml", "big.log", "notes.md"]
        );
        assert_eq!(names(&run("where type in dir,nothing")), vec!["src"]);
        assert_eq!(names(&run("where name =~ md$")), vec!["notes.md"]);
        assert_eq!(names(&run("where name =~ ^b")), vec!["big.log"]);
        assert_eq!(names(&run("where name =~ ^src$")), vec!["src"]);
        assert!(
            filter(
                "where",
                &["size".into(), "bigger".into(), "1".into()],
                files()
            )
            .is_err()
        );
    }

    #[test]
    fn shaping() {
        assert_eq!(
            names(&run("sort-by size -r | first 2")),
            vec!["big.log", "src"]
        );
        assert_eq!(
            names(&run("sort-by name")),
            vec!["big.log", "Cargo.toml", "notes.md", "src"]
        );
        assert_eq!(names(&run("last 1")), vec!["notes.md"]);
        assert_eq!(names(&run("skip 3")), vec!["notes.md"]);
        let d = run("select name size | first");
        assert_eq!(columns(&d.into_rows()), vec!["name", "size"]);
        let d = run("reject modified type");
        assert_eq!(columns(&d.into_rows()), vec!["name", "size"]);
    }

    #[test]
    fn aggregates() {
        assert_eq!(run("count"), Data::Value(Value::Int(4)));
        assert_eq!(run("sum size"), Data::Value(Value::Size(5_005_596)));
        assert_eq!(
            run("where type eq file | get size | max"),
            Data::Value(Value::Size(5_000_000))
        );
        assert_eq!(
            run("get name | min"),
            Data::Value(Value::Str("big.log".into()))
        );
        assert_eq!(run("avg size"), Data::Value(Value::Size(1_251_399)));
        let g = run("group-by type");
        assert_eq!(
            g,
            Data::Table(vec![
                vec![
                    ("type".into(), Value::Str("file".into())),
                    ("count".into(), Value::Int(3))
                ],
                vec![
                    ("type".into(), Value::Str("dir".into())),
                    ("count".into(), Value::Int(1))
                ],
            ])
        );
        assert_eq!(
            run("uniq type"),
            Data::Value(Value::List(vec![
                Value::Str("file".into()),
                Value::Str("dir".into())
            ]))
        );
        assert!(filter("get", &["nope".into()], files()).is_err());
        let d = run("describe");
        assert_eq!(d.into_rows()[3][1].1, Value::Str("date".into()));
    }

    #[test]
    fn globbing() {
        assert!(glob("*.RS", "main.rs"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a*z", "abc"));
    }
}

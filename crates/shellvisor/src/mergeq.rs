//! Merge queue: bring finished agents' branches in one at a time.
//!
//! `sv agent queue [NAME…] [--test 'cmd'] [--dry-run] [--no-commit]`
//!
//! For each agent, in order (the names given, else the order they started):
//! 1. its uncommitted work is committed on its branch ("NAME: work left
//!    uncommitted"), unless `--no-commit`, which skips that agent instead;
//! 2. its branch is rebased onto your current branch, in its worktree;
//! 3. the tests run in its worktree (`--test`, `agents.test` in config.yaml,
//!    or found: cargo test, npm test, pytest, go test, make test);
//! 4. your branch is fast-forwarded to it.
//!
//! It stops at the first conflict (the rebase is undone and the files named)
//! or failing test (the last lines shown), so what is merged always passed.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub status: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let mut e = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if e.is_empty() {
            e = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Err(e)
    }
}

/// The main checkout a worktree belongs to.
pub fn main_checkout(worktree: &Path) -> Option<PathBuf> {
    let common = git(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()?;
    Path::new(&common).parent().map(Path::to_path_buf)
}

/// The test command for a folder: config, else what the project uses.
pub fn find_tests(dir: &Path) -> Option<String> {
    let has = |f: &str| dir.join(f).exists();
    if has("Cargo.toml") {
        return Some("cargo test --quiet".into());
    }
    if has("package.json") {
        let pkg = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&pkg).unwrap_or_default();
        if v["scripts"]["test"].is_string() {
            return Some("npm test --silent".into());
        }
    }
    if has("go.mod") {
        return Some("go test ./...".into());
    }
    if has("pytest.ini") || has("pyproject.toml") || has("setup.cfg") || has("tests") {
        return Some("python3 -m pytest -q".into());
    }
    if has("Makefile")
        && std::fs::read_to_string(dir.join("Makefile"))
            .unwrap_or_default()
            .lines()
            .any(|l| l.starts_with("test:"))
    {
        return Some("make test".into());
    }
    None
}

/// Agents from state/agents.json that have a branch, in start order.
pub fn from_state() -> Vec<Item> {
    let path = crate::paths::state_dir().join("agents.json");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let rows: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            let branch = r["branch"].as_str().filter(|b| !b.is_empty())?;
            Some(Item {
                name: r["name"].as_str()?.to_string(),
                branch: branch.to_string(),
                worktree: PathBuf::from(
                    r["worktree"]
                        .as_str()
                        .filter(|w| !w.is_empty())
                        .or(r["folder"].as_str())?,
                ),
                status: r["status"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect()
}

fn finished(status: &str) -> bool {
    let s = status.trim_start_matches(|c: char| !c.is_alphanumeric());
    s.starts_with("done") || s.starts_with("idle") || s.starts_with("exited 0") || s == "exited"
}

pub enum Outcome {
    Merged(String),
    Skipped(String),
    Stopped(String),
}

/// Run one agent through the queue.
pub fn step(
    it: &Item,
    target_dir: &Path,
    target: &str,
    test: Option<&str>,
    commit: bool,
    dry: bool,
    say: &mut dyn FnMut(&str),
) -> Outcome {
    let wt = &it.worktree;
    if !wt.is_dir() {
        return Outcome::Skipped(format!(
            "{}: its worktree {} is gone",
            it.name,
            wt.display()
        ));
    }
    // 1. uncommitted work
    let dirty = git(wt, &["status", "--porcelain"]).unwrap_or_default();
    if !dirty.is_empty() {
        if !commit {
            return Outcome::Skipped(format!("{}: has uncommitted work (--no-commit)", it.name));
        }
        say(&format!(
            "  commit what {} left uncommitted ({} file(s))",
            it.name,
            dirty.lines().count()
        ));
        if !dry {
            if let Err(e) = git(wt, &["add", "-A"]).and_then(|_| {
                git(
                    wt,
                    &[
                        "commit",
                        "-q",
                        "-m",
                        &format!("{}: work left uncommitted", it.name),
                    ],
                )
            }) {
                return Outcome::Stopped(format!("{}: couldn't commit its work: {e}", it.name));
            }
        }
    }
    let ahead = git(wt, &["rev-list", "--count", &format!("{target}..HEAD")])
        .ok()
        .and_then(|n| n.parse::<u32>().ok())
        .unwrap_or(0);
    if ahead == 0 && dirty.is_empty() {
        return Outcome::Skipped(format!("{}: nothing new on {}", it.name, it.branch));
    }
    // 2. rebase onto the target
    say(&format!("  rebase {} onto {target}", it.branch));
    if !dry {
        if let Err(e) = git(wt, &["rebase", "-q", target]) {
            let files = git(wt, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(wt, &["rebase", "--abort"]);
            let files: Vec<&str> = files.lines().collect();
            return Outcome::Stopped(if files.is_empty() {
                format!(
                    "{}: rebase failed: {}",
                    it.name,
                    e.lines().last().unwrap_or("")
                )
            } else {
                format!(
                    "{}: conflicts with {target} in {} — the rebase was undone; tell {} to rebase, or fix it in {}",
                    it.name,
                    files.join(", "),
                    it.name,
                    wt.display()
                )
            });
        }
    }
    // 3. tests
    match test {
        Some(cmd) => {
            say(&format!("  test: {cmd}"));
            if !dry {
                let out = Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(wt)
                    .output();
                match out {
                    Ok(o) if o.status.success() => say("  tests pass"),
                    Ok(o) => {
                        let mut all = String::from_utf8_lossy(&o.stdout).into_owned();
                        all.push_str(&String::from_utf8_lossy(&o.stderr));
                        let lines: Vec<&str> =
                            all.lines().filter(|l| !l.trim().is_empty()).collect();
                        let tail = lines[lines.len().saturating_sub(8)..].join("\n    ");
                        return Outcome::Stopped(format!(
                            "{}: tests fail after the rebase (exit {}):\n    {tail}",
                            it.name,
                            o.status.code().unwrap_or(-1)
                        ));
                    }
                    Err(e) => {
                        return Outcome::Stopped(format!("{}: couldn't run {cmd}: {e}", it.name));
                    }
                }
            }
        }
        None => say("  no tests found (--test 'cmd' or agents.test in config.yaml)"),
    }
    // 4. fast-forward
    say(&format!("  fast-forward {target} to {}", it.branch));
    if !dry {
        if let Err(e) = git(target_dir, &["merge", "-q", "--ff-only", &it.branch]) {
            return Outcome::Stopped(format!("{}: couldn't fast-forward {target}: {e}", it.name));
        }
    }
    Outcome::Merged(format!("{} ({ahead} commit(s))", it.name))
}

/// `sv agent queue …`
pub fn run(args: &[String]) -> i32 {
    let val = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let flag = |k: &str| args.iter().any(|a| a == k);
    let dry = flag("--dry-run") || flag("-n");
    let commit = !flag("--no-commit");
    let mut skip_next = false;
    let names: Vec<String> = args
        .iter()
        .filter(|a| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if *a == "--test" {
                skip_next = true;
                return false;
            }
            !a.starts_with('-')
        })
        .cloned()
        .collect();
    let all = from_state();
    let items: Vec<Item> = if names.is_empty() {
        all.into_iter().filter(|i| finished(&i.status)).collect()
    } else {
        let mut v = Vec::new();
        for n in &names {
            match all.iter().find(|i| &i.name == n) {
                Some(i) => v.push(i.clone()),
                None => {
                    eprintln!("sv agent queue: no agent {n} with a branch");
                    return 2;
                }
            }
        }
        v
    };
    if items.is_empty() {
        println!("nothing to merge: no finished agent with a branch (sv agent tree)");
        return 0;
    }
    let Some(target_dir) = main_checkout(&items[0].worktree) else {
        eprintln!(
            "sv agent queue: {} isn't in a git repository",
            items[0].worktree.display()
        );
        return 2;
    };
    let target = match git(&target_dir, &["branch", "--show-current"]) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            eprintln!("sv agent queue: {} isn't on a branch", target_dir.display());
            return 2;
        }
    };
    if !git(
        &target_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .unwrap_or_default()
    .is_empty()
    {
        eprintln!(
            "sv agent queue: {} has uncommitted changes — commit or stash them first",
            target_dir.display()
        );
        return 2;
    }
    let config_test = crate::config::Config::load(&crate::paths::config_file())
        .0
        .agents
        .test;
    let test = val("--test")
        .or(config_test)
        .or_else(|| find_tests(&target_dir));
    println!(
        "merge queue{} → {target} in {} · {} agent(s): {}",
        if dry { " (dry run)" } else { "" },
        target_dir.display(),
        items.len(),
        items
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut merged = Vec::new();
    let mut skipped = Vec::new();
    for it in &items {
        println!("▸ {} ({})", it.name, it.branch);
        let mut say = |s: &str| println!("{s}");
        match step(
            it,
            &target_dir,
            &target,
            test.as_deref(),
            commit,
            dry,
            &mut say,
        ) {
            Outcome::Merged(m) => {
                println!("  ✔ merged {m}");
                merged.push(it.name.clone());
            }
            Outcome::Skipped(m) => {
                println!("  – {m}");
                skipped.push(it.name.clone());
            }
            Outcome::Stopped(m) => {
                println!("  ✖ {m}");
                println!(
                    "\nstopped · merged: {} · not reached: {}",
                    if merged.is_empty() {
                        "none".into()
                    } else {
                        merged.join(", ")
                    },
                    items
                        .iter()
                        .skip_while(|i| i.name != it.name)
                        .map(|i| i.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return 1;
            }
        }
    }
    println!(
        "\ndone · merged: {}{}",
        if merged.is_empty() {
            "none".into()
        } else {
            merged.join(", ")
        },
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" · skipped: {}", skipped.join(", "))
        }
    );
    0
}

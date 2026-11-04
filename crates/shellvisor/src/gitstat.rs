//! `git status` for the files pane, via the git CLI (no libgit2).
//!
//! Runs off the UI thread, only when needed (folder change, a command
//! finished, manual refresh), with `GIT_OPTIONAL_LOCKS=0` so it never fights
//! the user's own git commands for the index lock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflict,
    /// A folder that contains changes.
    Changed,
}

impl GitMark {
    pub fn symbol(self) -> &'static str {
        match self {
            GitMark::Modified => "M",
            GitMark::Added => "A",
            GitMark::Deleted => "D",
            GitMark::Renamed => "R",
            GitMark::Untracked => "?",
            GitMark::Conflict => "!",
            GitMark::Changed => "•",
        }
    }
}

/// Status of the files under one folder (`root`), keyed by absolute paths
/// built from `root` — so symlinked paths like macOS's /tmp still match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitStatus {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changed: usize,
    marks: HashMap<PathBuf, GitMark>,
    dirs: HashSet<PathBuf>,
    /// Untracked folders git reports as a whole ("?? build/").
    new_dirs: HashSet<PathBuf>,
}

impl GitStatus {
    pub fn mark(&self, path: &Path) -> Option<GitMark> {
        if let Some(m) = self.marks.get(path) {
            return Some(*m);
        }
        if !self.new_dirs.is_empty() && path.ancestors().skip(1).any(|a| self.new_dirs.contains(a))
        {
            return Some(GitMark::Untracked);
        }
        self.dirs.contains(path).then_some(GitMark::Changed)
    }

    /// Changed files under the root, sorted by path.
    pub fn entries(&self) -> Vec<(&Path, GitMark)> {
        let mut v: Vec<(&Path, GitMark)> =
            self.marks.iter().map(|(p, m)| (p.as_path(), *m)).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v
    }

    /// "main ↑2 ↓1 · 3 changed"
    pub fn summary(&self) -> String {
        let mut s = self.branch.clone().unwrap_or_else(|| "detached".into());
        if self.ahead > 0 {
            s.push_str(&format!(" ↑{}", self.ahead));
        }
        if self.behind > 0 {
            s.push_str(&format!(" ↓{}", self.behind));
        }
        if self.changed > 0 {
            s.push_str(&format!(" · {} changed", self.changed));
        }
        s
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Status for `root`, or None if it isn't inside a git repository.
pub fn query(root: &Path) -> Option<GitStatus> {
    // Where `root` sits inside the repo ("" at the top, "src/" below it).
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let prefix = String::from_utf8_lossy(&prefix)
        .trim_end_matches('\n')
        .to_string();
    let out = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "-b",
            "--untracked-files=normal",
        ],
    )?;
    Some(parse(root, &prefix, &out))
}

/// Parse `git status --porcelain=v1 -z -b` output. Paths in it are relative
/// to the repository top; `prefix` is `root`'s position inside the repo.
pub fn parse(root: &Path, prefix: &str, out: &[u8]) -> GitStatus {
    let mut st = GitStatus {
        root: root.to_path_buf(),
        ..Default::default()
    };
    let mut fields = out.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        let line = String::from_utf8_lossy(f);
        if let Some(branch) = line.strip_prefix("## ") {
            parse_branch(branch, &mut st);
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let (xy, path) = line.split_at(3);
        let mut chars = xy.chars();
        let (x, y) = (chars.next().unwrap_or(' '), chars.next().unwrap_or(' '));
        if x == 'R' || x == 'C' {
            fields.next(); // the original name of a rename/copy
        }
        let mark = match (x, y) {
            ('?', '?') => GitMark::Untracked,
            ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => GitMark::Conflict,
            ('R', _) | ('C', _) => GitMark::Renamed,
            ('A', _) => GitMark::Added,
            ('D', _) | (_, 'D') => GitMark::Deleted,
            _ => GitMark::Modified,
        };
        st.changed += 1;
        let is_dir = path.ends_with('/');
        let path = path.trim_end_matches('/');
        if is_dir && mark == GitMark::Untracked && prefix.starts_with(&format!("{path}/")) {
            st.new_dirs.insert(root.to_path_buf()); // we're inside a new folder
            continue;
        }
        let Some(rel) = path.strip_prefix(prefix) else {
            continue; // outside the folder we're showing
        };
        let abs = root.join(rel);
        let mut dir = abs.parent();
        while let Some(d) = dir {
            if !d.starts_with(root) || d == root {
                break;
            }
            st.dirs.insert(d.to_path_buf());
            dir = d.parent();
        }
        if is_dir && mark == GitMark::Untracked {
            st.new_dirs.insert(abs.clone());
        }
        st.marks.insert(abs, mark);
    }
    st
}

fn parse_branch(s: &str, st: &mut GitStatus) {
    // "main...origin/main [ahead 2, behind 1]" | "No commits yet on main" | "HEAD (no branch)"
    if let Some(b) = s.strip_prefix("No commits yet on ") {
        st.branch = Some(b.to_string());
        return;
    }
    if s.starts_with("HEAD (no branch)") {
        return;
    }
    let (names, counts) = match s.split_once(" [") {
        Some((n, c)) => (n, Some(c.trim_end_matches(']'))),
        None => (s, None),
    };
    st.branch = Some(names.split("...").next().unwrap_or(names).to_string());
    for part in counts.unwrap_or("").split(", ") {
        if let Some(n) = part.strip_prefix("ahead ") {
            st.ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            st.behind = n.parse().unwrap_or(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain() {
        let out = b"## main...origin/main [ahead 2, behind 1]\0 M src/core/pricing.py\0A  src/core/alerts.py\0?? logs/app.log\0R  new.txt\0old.txt\0UU both.txt\0 D gone.txt\0";
        let root = Path::new("/p/app");
        let st = parse(root, "", out);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!((st.ahead, st.behind, st.changed), (2, 1, 6));
        assert_eq!(
            st.mark(&root.join("src/core/pricing.py")),
            Some(GitMark::Modified)
        );
        assert_eq!(
            st.mark(&root.join("src/core/alerts.py")),
            Some(GitMark::Added)
        );
        assert_eq!(
            st.mark(&root.join("logs/app.log")),
            Some(GitMark::Untracked)
        );
        assert_eq!(st.mark(&root.join("new.txt")), Some(GitMark::Renamed));
        assert_eq!(st.mark(&root.join("old.txt")), None);
        assert_eq!(st.mark(&root.join("both.txt")), Some(GitMark::Conflict));
        assert_eq!(st.mark(&root.join("gone.txt")), Some(GitMark::Deleted));
        assert_eq!(st.mark(&root.join("src")), Some(GitMark::Changed));
        assert_eq!(st.mark(&root.join("src/core")), Some(GitMark::Changed));
        assert_eq!(st.mark(&root.join("README.md")), None);
        assert_eq!(st.summary(), "main ↑2 ↓1 · 6 changed");
    }

    #[test]
    fn paths_are_relative_to_the_shown_folder() {
        // showing <repo>/src, git reports paths from the repo top
        let out = b"## dev\0 M src/a.rs\0 M docs/b.md\0";
        let st = parse(Path::new("/x/src"), "src/", out);
        assert_eq!(st.mark(Path::new("/x/src/a.rs")), Some(GitMark::Modified));
        assert_eq!(st.mark(Path::new("/x/docs/b.md")), None);
        assert_eq!(st.changed, 2);
        assert_eq!(st.summary(), "dev · 2 changed");
    }

    #[test]
    fn branch_variants() {
        let st = parse(Path::new("/r"), "", b"## No commits yet on main\0");
        assert_eq!(st.branch.as_deref(), Some("main"));
        let st = parse(Path::new("/r"), "", b"## HEAD (no branch)\0");
        assert_eq!(st.branch, None);
        assert_eq!(st.summary(), "detached");
    }

    #[test]
    fn real_repository() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("sv-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&d)
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(d.join("sub/new.txt"), "hi").unwrap();
        let st = query(&d).expect("is a repo");
        assert_eq!(st.mark(&d.join("sub/new.txt")), Some(GitMark::Untracked));
        assert_eq!(st.mark(&d.join("sub")), Some(GitMark::Untracked));
        let st = query(&d.join("sub")).unwrap();
        assert_eq!(st.mark(&d.join("sub/new.txt")), Some(GitMark::Untracked));
        assert!(query(Path::new("/")).is_none() || std::path::Path::new("/.git").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

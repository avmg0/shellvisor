//! The agent board (alt+g): four views over the agents —
//!
//! - **Plan**: type a goal, a planner CLI proposes a staged task graph, you
//!   edit / reassign / drop tasks and dispatch it (all at once, or a stage at
//!   a time). A review agent looks over the branches before you merge.
//! - **Tree**: who spawned whom, tokens rolled up per branch, pause / stop /
//!   merge a branch.
//! - **Timeline**: what every agent was doing over time.
//! - **Log**: the message bus — spawns, messages, events, refusals.
//!
//! The board holds only its own state; the app hands it a [`Snap`] of the
//! agents each time it draws or handles a key.

use crate::agents::{Cli, fmt_tokens, valid_name};
use crate::team::{Member, Start, Team, find_cycle};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;

const AMBER: Color = Color::Indexed(179);
const DIM: Color = Color::Indexed(245);
const FAINT: Color = Color::Indexed(243);
const CYAN: Color = Color::Indexed(73);
const GREEN: Color = Color::Indexed(108);
const RED: Color = Color::Indexed(167);
const SEL_BG: Color = Color::Indexed(237);
const TEXT: Color = Color::Indexed(252);

// ------------------------------------------------------------------ plan

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub scope: String,
    /// CLI name (claude, codex…) or a command.
    pub agent: String,
    pub deps: Vec<String>,
    pub est: Option<u64>,
    pub stage: u32,
}

/// What the planner CLI is asked (on its standard input).
pub fn planner_prompt(goal: &str, agents: &[String]) -> String {
    format!(
        "You are planning work for a team of coding agents in this repository.\n\
         Goal: {goal}\n\n\
         Look at the repository as much as you need, then reply with ONLY a JSON object, no other text:\n\
         {{\"tasks\": [{{\"id\": \"short-name\", \"title\": \"what to do, in one or two sentences\", \
         \"scope\": \"the files or folders it should touch\", \"deps\": [\"ids that must finish first\"], \
         \"estimate_tokens\": 50000, \"agent\": \"{first}\"}}]}}\n\n\
         Use 2 to 6 tasks. Tasks without deps run at the same time, each in its own git worktree, \
         so keep their scopes apart. Ids: lowercase letters, digits and dashes. \
         Agents available: {list}.\n",
        first = agents.first().map(String::as_str).unwrap_or("claude"),
        list = if agents.is_empty() {
            "claude".into()
        } else {
            agents.join(", ")
        },
    )
}

/// Read the planner's reply: the JSON object in it (prose and ``` fences
/// around it are ignored). Ids are made safe, deps checked, stages computed.
pub fn parse_plan(text: &str, default_agent: &str) -> Result<Vec<Task>, String> {
    let start = text
        .find('{')
        .ok_or("the planner's reply has no JSON in it")?;
    let end = text
        .rfind('}')
        .ok_or("the planner's reply has no JSON in it")?;
    if end < start {
        return Err("the planner's reply has no JSON in it".into());
    }
    let v: Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| format!("the planner's JSON doesn't read: {e}"))?;
    let list = v["tasks"]
        .as_array()
        .or(v.as_array())
        .ok_or("the planner's JSON has no \"tasks\" list")?;
    if list.is_empty() {
        return Err("the planner proposed no tasks".into());
    }
    let mut tasks: Vec<Task> = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let s = |k: &str| t[k].as_str().unwrap_or("").trim().to_string();
        let mut id = slug(&s("id"));
        if id.is_empty() {
            id = format!("t{}", i + 1);
        }
        while tasks.iter().any(|x| x.id == id) {
            id.push('2');
        }
        let title = Some(s("title"))
            .filter(|x| !x.is_empty())
            .or_else(|| Some(s("task")).filter(|x| !x.is_empty()))
            .ok_or(format!("task {id} has no title"))?;
        let deps = t["deps"]
            .as_array()
            .or(t["depends_on"].as_array())
            .map(|a| a.iter().filter_map(|d| d.as_str()).map(slug).collect())
            .unwrap_or_default();
        let agent = Some(s("agent"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| default_agent.into());
        tasks.push(Task {
            id,
            title,
            scope: s("scope"),
            agent,
            deps,
            est: t["estimate_tokens"].as_u64(),
            stage: 0,
        });
    }
    // deps on tasks that don't exist are dropped rather than failing the plan
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    for t in &mut tasks {
        t.deps.retain(|d| ids.contains(d) && *d != t.id);
    }
    restage(&mut tasks)?;
    Ok(tasks)
}

fn slug(s: &str) -> String {
    let out: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(24)
        .collect();
    if valid_name(&out) { out } else { String::new() }
}

/// Stage = 1 + the latest stage among its deps; tasks sorted by stage.
pub fn restage(tasks: &mut [Task]) -> Result<(), String> {
    let graph: Vec<(String, Vec<String>)> = tasks
        .iter()
        .map(|t| (t.id.clone(), t.deps.clone()))
        .collect();
    if let Some(c) = find_cycle(&graph) {
        return Err(format!(
            "the plan goes round in a circle: {}",
            c.join(" → ")
        ));
    }
    fn stage_of(id: &str, g: &[(String, Vec<String>)]) -> u32 {
        let deps = g
            .iter()
            .find(|(n, _)| n == id)
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        1 + deps.iter().map(|d| stage_of(d, g)).max().unwrap_or(0)
    }
    for t in tasks.iter_mut() {
        t.stage = stage_of(&t.id, &graph);
    }
    tasks.sort_by_key(|t| t.stage);
    Ok(())
}

/// Drop a task; tasks after it no longer wait for it.
pub fn drop_task(tasks: &mut Vec<Task>, id: &str) {
    tasks.retain(|t| t.id != id);
    for t in tasks.iter_mut() {
        t.deps.retain(|d| d != id);
    }
    let _ = restage(tasks);
}

/// Paths named by two tasks of the same stage (they would edit the same files
/// in different worktrees at once).
pub fn overlap_risk(tasks: &[Task]) -> Vec<(String, Vec<String>)> {
    let paths = |s: &str| -> Vec<String> {
        s.split([',', ' ', ';'])
            .map(|p| p.trim().trim_end_matches('/').to_string())
            .filter(|p| !p.is_empty() && p.contains(['/', '.']))
            .collect()
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (i, a) in tasks.iter().enumerate() {
        for b in &tasks[i + 1..] {
            if a.stage != b.stage {
                continue;
            }
            for pa in paths(&a.scope) {
                for pb in paths(&b.scope) {
                    let shared = if pa.starts_with(&pb) {
                        pb.clone()
                    } else if pb.starts_with(&pa) {
                        pa.clone()
                    } else {
                        continue;
                    };
                    match out.iter_mut().find(|(p, _)| *p == shared) {
                        Some((_, who)) => {
                            for n in [&a.id, &b.id] {
                                if !who.contains(n) {
                                    who.push(n.clone());
                                }
                            }
                        }
                        None => out.push((shared, vec![a.id.clone(), b.id.clone()])),
                    }
                }
            }
        }
    }
    out
}

/// The plan as a team: one member per task (own worktree), started when its
/// deps are done, then a review member once everything is done.
pub fn plan_to_team(
    name: &str,
    tasks: &[Task],
    goal: &str,
    reviewer: &str,
    budget: Option<u64>,
) -> Team {
    let how = |agent: &str| match Cli::parse(agent) {
        Some(c) => (c.name().to_string(), String::new()),
        None => (String::new(), agent.to_string()),
    };
    let mut agents: Vec<Member> = tasks
        .iter()
        .map(|t| {
            let (cli, cmd) = how(&t.agent);
            let mut task = t.title.trim_end_matches('.').to_string();
            if !t.scope.is_empty() {
                task += &format!(". Only touch: {}", t.scope);
            }
            task += &format!(". This is part of: {goal}. When you are done, say so and stop.");
            Member {
                id: t.id.clone(),
                cli,
                cmd,
                orchestrator: false,
                task,
                can_spawn: Vec::new(),
                worktree: Some(true),
                max_children: None,
                approve: Vec::new(),
                mcp: Vec::new(),
                start: if t.deps.is_empty() {
                    Start::Now
                } else {
                    Start::After(t.deps.clone())
                },
                stage: t.stage,
            }
        })
        .collect();
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    if !ids.is_empty() {
        let (cli, cmd) = how(reviewer);
        let branches: Vec<String> = ids.iter().map(|i| format!("agent/{i}")).collect();
        let review_id = if ids.iter().any(|i| i == "review") {
            "review-all"
        } else {
            "review"
        };
        agents.push(Member {
            id: review_id.into(),
            cli,
            cmd,
            orchestrator: true,
            task: format!(
                "Review the team's work for: {goal}. Each task was done on its own branch ({}). \
                 Compare each with `git diff HEAD...<branch>`, run the tests, and report what is wrong or missing, \
                 branch by branch. Don't merge anything — the person will merge from the agent board.",
                branches.join(", ")
            ),
            can_spawn: Vec::new(),
            worktree: Some(false),
            max_children: None,
            approve: Vec::new(),
            mcp: Vec::new(),
            start: Start::After(ids),
            stage: tasks.iter().map(|t| t.stage).max().unwrap_or(0) + 1,
        });
    }
    Team {
        name: name.into(),
        budget_tokens: budget,
        max_agents: None,
        max_depth: None,
        agents,
        rules: Vec::new(),
    }
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Plan,
    Tree,
    Timeline,
    Log,
}

impl View {
    pub fn parse(s: &str) -> Option<View> {
        Some(match s {
            "plan" => View::Plan,
            "tree" => View::Tree,
            "timeline" => View::Timeline,
            "log" => View::Log,
            _ => return None,
        })
    }
    fn next(self) -> View {
        match self {
            View::Plan => View::Tree,
            View::Tree => View::Timeline,
            View::Timeline => View::Log,
            View::Log => View::Plan,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Typing the goal.
    Goal,
    Planning(Instant),
    Ready,
    Failed(String),
    /// Running as team `name`.
    Dispatched(String),
}

const PARALLEL: [usize; 4] = [2, 3, 4, 6];
const BUDGETS: [Option<u64>; 5] = [
    None,
    Some(500_000),
    Some(1_000_000),
    Some(2_000_000),
    Some(5_000_000),
];

#[derive(Debug, Clone)]
pub struct Plan {
    pub goal: String,
    pub phase: Phase,
    pub tasks: Vec<Task>,
    pub sel: usize,
    /// Editing the selected task's title.
    pub edit: Option<String>,
    pub parallel: usize,
    pub budget: Option<u64>,
    /// Agent CLIs to offer when reassigning.
    pub clis: Vec<String>,
    /// Who plans (shown) and who reviews at the end.
    pub planner: String,
    pub reviewer: String,
    pub folder: PathBuf,
    pub step: bool,
    /// Bumped per planner run: late answers from an older run are ignored.
    pub run: u64,
}

pub struct Board {
    pub view: View,
    pub sel: usize,
    pub scroll: usize,
    pub plan: Plan,
    /// "stop api and its sub-agents? y/n"
    confirm: Option<(String, BoardAct)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoardAct {
    Close,
    /// Go to that agent's column.
    Open(String),
    /// pause / resume / stop, agent, with its sub-agents.
    Signal(&'static str, String),
    Merge(String),
    /// Board 21 for that agent.
    Review(String),
    /// Ask the planner (the goal is in the plan).
    Plan,
    Dispatch,
    NextStage,
}

/// What the board shows about one agent.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub parent: Option<String>,
    pub depth: u8,
    pub cli: String,
    pub status: String,
    pub tokens: Option<u64>,
    pub task: String,
    pub branch: Option<String>,
    pub files: usize,
    pub added: u64,
    pub removed: u64,
    pub helpers: Vec<String>,
    pub orchestrator: bool,
    pub color: u8,
    /// (seconds after its start, status) — every change.
    pub history: Vec<(f64, &'static str)>,
    /// Seconds since it started.
    pub age: f64,
    /// Last lines of its screen.
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TeamInfo {
    pub name: String,
    pub used: u64,
    pub budget: Option<u64>,
    pub capped: bool,
    pub step: Option<u32>,
    pub stages: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Snap {
    pub nodes: Vec<Node>,
    pub team: Option<TeamInfo>,
    pub log: Vec<(i64, String)>,
    pub max_depth: u8,
    pub max_agents: usize,
    pub max_children: usize,
}

impl Board {
    pub fn new(view: View, plan: Plan) -> Board {
        Board {
            view,
            sel: 0,
            scroll: 0,
            plan,
            confirm: None,
        }
    }

    /// Typing text (goal or a title): letters go there, not to commands.
    pub fn typing(&self) -> bool {
        self.view == View::Plan && (self.plan.phase == Phase::Goal || self.plan.edit.is_some())
    }

    /// Rows of the tree view: (node index or None for a helper, label indent, helper name).
    fn tree_rows(snap: &Snap) -> Vec<(usize, String)> {
        fn walk(s: &Snap, parent: Option<&str>, indent: &str, out: &mut Vec<(usize, String)>) {
            let kids: Vec<usize> = (0..s.nodes.len())
                .filter(|i| s.nodes[*i].parent.as_deref() == parent)
                .collect();
            for (k, i) in kids.iter().enumerate() {
                let last = k + 1 == kids.len();
                let (branch, next) = match parent {
                    None => (String::new(), String::new()),
                    Some(_) => (
                        format!("{indent}{}", if last { "└─ " } else { "├─ " }),
                        format!("{indent}{}", if last { "   " } else { "│  " }),
                    ),
                };
                out.push((*i, branch));
                walk(s, Some(&s.nodes[*i].name), &next, out);
            }
        }
        let mut out = Vec::new();
        walk(snap, None, "", &mut out);
        // agents whose parent is gone
        for i in 0..snap.nodes.len() {
            if !out.iter().any(|(j, _)| *j == i) {
                out.push((i, String::new()));
            }
        }
        out
    }

    fn selected_name(&self, snap: &Snap) -> Option<String> {
        let rows = Self::tree_rows(snap);
        rows.get(self.sel.min(rows.len().saturating_sub(1)))
            .map(|(i, _)| snap.nodes[*i].name.clone())
    }

    /// A key. Returns (redraw, action).
    pub fn key(&mut self, key: &[u8], snap: &Snap) -> (bool, Option<BoardAct>) {
        if let Some((_, act)) = self.confirm.take() {
            return match key {
                b"y" | b"Y" | b"\r" => (true, Some(act)),
                _ => (true, None),
            };
        }
        let up = matches!(key, b"\x1b[A" | b"\x1bOA");
        let down = matches!(key, b"\x1b[B" | b"\x1bOB");
        if key == b"\t" {
            self.view = self.view.next();
            self.sel = 0;
            self.scroll = 0;
            return (true, None);
        }
        if self.view == View::Plan {
            return self.plan_key(key, up, down);
        }
        let n = match self.view {
            View::Log => snap.log.len(),
            _ => Self::tree_rows(snap).len(),
        };
        match key {
            b"\x1b" | b"q" | [0x03] => return (true, Some(BoardAct::Close)),
            b"t" => {
                self.view = self.view.next();
                self.sel = 0;
            }
            _ if up || key == b"k" && self.view == View::Log => {
                if self.view == View::Log {
                    self.scroll = (self.scroll + 1).min(n.saturating_sub(1));
                } else {
                    self.sel = self.sel.saturating_sub(1);
                }
            }
            _ if down || key == b"j" && self.view == View::Log => {
                if self.view == View::Log {
                    self.scroll = self.scroll.saturating_sub(1);
                } else {
                    self.sel = (self.sel + 1).min(n.saturating_sub(1));
                }
            }
            b"\r" if self.view != View::Log => {
                if let Some(name) = self.selected_name(snap) {
                    return (true, Some(BoardAct::Open(name)));
                }
            }
            b"p" if self.view == View::Tree => {
                if let Some(name) = self.selected_name(snap) {
                    let paused = snap
                        .nodes
                        .iter()
                        .any(|n| n.name == name && n.status == "paused");
                    return (
                        true,
                        Some(BoardAct::Signal(
                            if paused { "resume" } else { "pause" },
                            name,
                        )),
                    );
                }
            }
            b"d" if self.view == View::Tree => {
                if let Some(name) = self.selected_name(snap) {
                    return (true, Some(BoardAct::Review(name)));
                }
            }
            b"k" if self.view == View::Tree => {
                if let Some(name) = self.selected_name(snap) {
                    self.confirm = Some((
                        format!("stop {name} and its sub-agents? y/n"),
                        BoardAct::Signal("stop", name),
                    ));
                }
            }
            b"M" | b"m" if self.view == View::Tree => {
                if let Some(name) = self.selected_name(snap) {
                    let branch = snap
                        .nodes
                        .iter()
                        .find(|n| n.name == name)
                        .and_then(|n| n.branch.clone());
                    match branch {
                        Some(b) => {
                            self.confirm = Some((
                                format!("merge {b} into your branch? y/n"),
                                BoardAct::Merge(name),
                            ))
                        }
                        None => return (false, None),
                    }
                }
            }
            _ => return (false, None),
        }
        (true, None)
    }

    fn plan_key(&mut self, key: &[u8], up: bool, down: bool) -> (bool, Option<BoardAct>) {
        let p = &mut self.plan;
        // editing a title
        if let Some(text) = p.edit.as_mut() {
            match key {
                b"\r" => {
                    let t = text.trim().to_string();
                    if !t.is_empty() {
                        if let Some(task) = p.tasks.get_mut(p.sel) {
                            task.title = t;
                        }
                    }
                    p.edit = None;
                }
                b"\x1b" => p.edit = None,
                [0x7f] | [0x08] => {
                    text.pop();
                }
                [0x15] => text.clear(),
                k => {
                    if let Ok(s) = std::str::from_utf8(k) {
                        if !s.chars().any(char::is_control) {
                            text.push_str(s);
                        }
                    }
                }
            }
            return (true, None);
        }
        match p.phase.clone() {
            Phase::Goal => match key {
                b"\r" if !p.goal.trim().is_empty() => return (true, Some(BoardAct::Plan)),
                b"\x1b" => {
                    if p.tasks.is_empty() {
                        return (true, Some(BoardAct::Close));
                    }
                    p.phase = Phase::Ready;
                }
                [0x03] => return (true, Some(BoardAct::Close)),
                [0x7f] | [0x08] => {
                    p.goal.pop();
                }
                [0x15] => p.goal.clear(),
                k => {
                    if let Ok(s) = std::str::from_utf8(k) {
                        if !s.chars().any(char::is_control) {
                            p.goal.push_str(s);
                        }
                    }
                }
            },
            Phase::Planning(_) => match key {
                b"\x1b" => {
                    p.run += 1; // ignore the answer
                    p.phase = Phase::Goal;
                }
                b"q" | [0x03] => return (true, Some(BoardAct::Close)),
                _ => return (false, None),
            },
            Phase::Failed(_) => match key {
                b"\r" | b"r" => return (true, Some(BoardAct::Plan)),
                b"e" | b"g" => p.phase = Phase::Goal,
                b"\x1b" | b"q" => return (true, Some(BoardAct::Close)),
                b"t" => self.view = self.view.next(),
                _ => return (false, None),
            },
            Phase::Ready => {
                let n = p.tasks.len();
                match key {
                    _ if up => p.sel = p.sel.saturating_sub(1),
                    _ if down => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
                    b"e" if n > 0 => p.edit = Some(p.tasks[p.sel.min(n - 1)].title.clone()),
                    b"a" if n > 0 && !p.clis.is_empty() => {
                        let t = &mut p.tasks[p.sel.min(n - 1)];
                        let at = p.clis.iter().position(|c| *c == t.agent);
                        t.agent = p.clis[at.map_or(0, |i| (i + 1) % p.clis.len())].clone();
                    }
                    b"x" if n > 0 => {
                        let id = p.tasks[p.sel.min(n - 1)].id.clone();
                        drop_task(&mut p.tasks, &id);
                        p.sel = p.sel.min(p.tasks.len().saturating_sub(1));
                    }
                    b"+" | b"=" => {
                        let i = PARALLEL.iter().position(|x| *x == p.parallel).unwrap_or(1);
                        p.parallel = PARALLEL[(i + 1).min(PARALLEL.len() - 1)];
                    }
                    b"-" => {
                        let i = PARALLEL.iter().position(|x| *x == p.parallel).unwrap_or(1);
                        p.parallel = PARALLEL[i.saturating_sub(1)];
                    }
                    b"b" => {
                        let i = BUDGETS.iter().position(|x| *x == p.budget).unwrap_or(0);
                        p.budget = BUDGETS[(i + 1) % BUDGETS.len()];
                    }
                    b"g" => p.phase = Phase::Goal,
                    b"r" => return (true, Some(BoardAct::Plan)),
                    b"\r" if n > 0 => {
                        p.step = false;
                        return (true, Some(BoardAct::Dispatch));
                    }
                    b"s" if n > 0 => {
                        p.step = true;
                        return (true, Some(BoardAct::Dispatch));
                    }
                    b"t" => self.view = self.view.next(),
                    b"\x1b" | b"q" | [0x03] => return (true, Some(BoardAct::Close)),
                    _ => return (false, None),
                }
            }
            Phase::Dispatched(_) => match key {
                b"\r" | b"n" if p.step => return (true, Some(BoardAct::NextStage)),
                b"g" => {
                    p.phase = Phase::Goal;
                    p.goal.clear();
                    p.tasks.clear();
                }
                b"t" => self.view = self.view.next(),
                b"\x1b" | b"q" | [0x03] => return (true, Some(BoardAct::Close)),
                _ => return (false, None),
            },
        }
        (true, None)
    }

    // -------------------------------------------------------------- drawing

    pub fn render(&self, area: Rect, buf: &mut Buffer, snap: &Snap) {
        if area.height < 6 || area.width < 30 {
            return;
        }
        let w = area.width;
        // header: views + summary
        let mut x = area.x;
        for (v, label) in [
            (View::Plan, "Plan"),
            (View::Tree, "Tree"),
            (View::Timeline, "Timeline"),
            (View::Log, "Log"),
        ] {
            let st = if v == self.view {
                Style::default()
                    .fg(Color::Indexed(16))
                    .bg(AMBER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(DIM)
            };
            let s = format!(" {label} ");
            put(buf, x, area.y, w.saturating_sub(x - area.x), &s, st);
            x += s.chars().count() as u16 + 1;
        }
        let depth = snap.nodes.iter().map(|n| n.depth).max().unwrap_or(0);
        let mut sum = format!(
            "{} agents · depth {depth} of {}",
            snap.nodes.len(),
            snap.max_depth
        );
        if let Some(t) = &snap.team {
            sum += &format!(" · team {} {}", t.name, fmt_tokens(t.used));
            if let Some(b) = t.budget {
                sum += &format!(" of {}", fmt_tokens(b).trim_end_matches(" tok"));
            }
            if t.capped {
                sum += " ⏸ over budget";
            }
        }
        let sw = sum.chars().count() as u16;
        if x + sw + 1 < area.right() {
            put(
                buf,
                area.right() - sw - 1,
                area.y,
                sw + 1,
                &sum,
                Style::default().fg(if snap.team.as_ref().is_some_and(|t| t.capped) {
                    AMBER
                } else {
                    DIM
                }),
            );
        }
        let body = Rect::new(area.x, area.y + 2, w, area.height.saturating_sub(4));
        match self.view {
            View::Plan => self.draw_plan(body, buf, snap),
            View::Tree => self.draw_tree(body, buf, snap),
            View::Timeline => self.draw_timeline(body, buf, snap),
            View::Log => self.draw_log(body, buf, snap),
        }
        // footer
        let y = area.bottom() - 1;
        let hint = if let Some((q, _)) = &self.confirm {
            put(
                buf,
                area.x,
                y,
                w,
                &format!(" {q} "),
                Style::default().fg(Color::Indexed(16)).bg(AMBER),
            );
            return;
        } else {
            match self.view {
                View::Plan => match &self.plan.phase {
                    _ if self.plan.edit.is_some() => "type the new title · ⏎ save · esc cancel",
                    Phase::Goal => "type the goal · ⏎ plan it · tab views · esc close",
                    Phase::Planning(_) => "esc cancel · tab views",
                    Phase::Failed(_) => "⏎ try again · e edit goal · esc close",
                    Phase::Ready => {
                        "↑↓ task · e edit · a reassign · x drop · +/- at once · b budget · ⏎ dispatch · s step by step · r re-plan · g goal"
                    }
                    Phase::Dispatched(_) if self.plan.step => {
                        "⏎ start the next stage · tab tree / timeline / log · g new goal · esc close"
                    }
                    Phase::Dispatched(_) => "tab tree / timeline / log · g new goal · esc close",
                },
                View::Tree => {
                    "↑↓ agent · ⏎ go to it · p pause/resume branch · k stop branch · d review · m merge its branch · tab view · esc close"
                }
                View::Timeline => {
                    "↑↓ agent · ⏎ go to it · ━ working · ─ waiting · ⚑ approval · ┄ paused · tab view · esc close"
                }
                View::Log => "↑↓ scroll · tab view · esc close",
            }
        };
        put(
            buf,
            area.x,
            y,
            w,
            &format!(" {hint}"),
            Style::default().fg(FAINT),
        );
    }

    fn draw_plan(&self, r: Rect, buf: &mut Buffer, snap: &Snap) {
        let p = &self.plan;
        let mut y = r.y;
        put(
            buf,
            r.x + 1,
            y,
            6,
            "Goal",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        put(
            buf,
            r.x + 8,
            y,
            r.width.saturating_sub(9),
            &format!("folder {}  ·  planner {}", short_path(&p.folder), p.planner),
            Style::default().fg(FAINT),
        );
        y += 1;
        let goal = if p.phase == Phase::Goal {
            format!("{}▏", p.goal)
        } else {
            p.goal.clone()
        };
        for line in wrap(
            if goal.is_empty() { "▏" } else { &goal },
            r.width.saturating_sub(4) as usize,
        )
        .into_iter()
        .take(3)
        {
            put(
                buf,
                r.x + 3,
                y,
                r.width - 4,
                &line,
                Style::default().fg(TEXT),
            );
            y += 1;
        }
        y += 1;
        match &p.phase {
            Phase::Goal if p.tasks.is_empty() => {
                put(
                    buf,
                    r.x + 1,
                    y,
                    r.width - 2,
                    "Say what the team should do. The planner reads the repository and proposes tasks in stages;",
                    Style::default().fg(DIM),
                );
                put(
                    buf,
                    r.x + 1,
                    y + 1,
                    r.width - 2,
                    "you can edit, reassign or drop any of them before anything starts.",
                    Style::default().fg(DIM),
                );
                return;
            }
            Phase::Planning(at) => {
                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let secs = at.elapsed().as_secs();
                let f = frames[(at.elapsed().as_millis() / 100) as usize % frames.len()];
                put(
                    buf,
                    r.x + 1,
                    y,
                    r.width - 2,
                    &format!("{f} {} is planning… {secs}s", p.planner),
                    Style::default().fg(CYAN),
                );
                return;
            }
            Phase::Failed(e) => {
                for (i, line) in wrap(&format!("✖ {e}"), r.width.saturating_sub(4) as usize)
                    .into_iter()
                    .take(4)
                    .enumerate()
                {
                    put(
                        buf,
                        r.x + 1,
                        y + i as u16,
                        r.width - 2,
                        &line,
                        Style::default().fg(RED),
                    );
                }
                return;
            }
            _ => {}
        }
        let stages = p.tasks.iter().map(|t| t.stage).max().unwrap_or(0);
        let live = matches!(p.phase, Phase::Dispatched(_));
        let head = if live {
            format!(
                "Running · {} tasks · {} stages{}",
                p.tasks.len(),
                stages,
                if p.step { " · step by step" } else { "" }
            )
        } else {
            format!(
                "Proposed plan · {} tasks · {} stages",
                p.tasks.len(),
                stages
            )
        };
        put(
            buf,
            r.x + 1,
            y,
            r.width - 2,
            &head,
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        y += 1;
        let side = if r.width >= 100 { 38 } else { 0 };
        let lw = r.width.saturating_sub(side + 2);
        let mut stage = 0;
        let bottom = r.bottom();
        for (i, t) in p.tasks.iter().enumerate() {
            if y + 2 >= bottom {
                put(
                    buf,
                    r.x + 1,
                    y,
                    lw,
                    &format!("… {} more", p.tasks.len() - i),
                    Style::default().fg(FAINT),
                );
                break;
            }
            if t.stage != stage {
                stage = t.stage;
                let n = p.tasks.iter().filter(|x| x.stage == stage).count();
                let label = format!(
                    "stage {stage}{}",
                    if n > 1 {
                        format!(" · {n} in parallel")
                    } else {
                        String::new()
                    }
                );
                put(buf, r.x + 1, y, lw, &label, Style::default().fg(DIM));
                y += 1;
            }
            let selected = !live && i == p.sel;
            let bg = if selected {
                Style::default().bg(SEL_BG)
            } else {
                Style::default()
            };
            fill(buf, Rect::new(r.x + 1, y, lw, 1), bg);
            let status = if live {
                snap.nodes
                    .iter()
                    .find(|n| n.name == t.id)
                    .map(|n| n.status.clone())
                    .unwrap_or_else(|| "waiting".into())
            } else {
                String::new()
            };
            let title = match (&p.edit, selected) {
                (Some(e), true) => format!("{e}▏"),
                _ => t.title.clone(),
            };
            let mark = if selected { "▸" } else { " " };
            let left = format!("{mark} {:<10} ● {:<8} ", t.id, short_agent(&t.agent));
            put(
                buf,
                r.x + 1,
                y,
                lw,
                &left,
                bg.fg(TEXT).add_modifier(Modifier::BOLD),
            );
            let lx = r.x + 1 + left.chars().count() as u16;
            let right = if live {
                status.clone()
            } else {
                t.est
                    .map(|e| format!("~{}", fmt_tokens(e)))
                    .unwrap_or_default()
            };
            let rw = right.chars().count() as u16;
            put(
                buf,
                lx,
                y,
                (r.x + 1 + lw).saturating_sub(lx + rw + 1),
                &title,
                bg.fg(TEXT),
            );
            let rcol = match status.as_str() {
                "working" | "starting" => CYAN,
                "done" | "exited" => GREEN,
                s if s.starts_with("needs") || s == "paused" => AMBER,
                s if s.starts_with("exited ") => RED,
                _ => FAINT,
            };
            put(
                buf,
                (r.x + 1 + lw).saturating_sub(rw),
                y,
                rw,
                &right,
                bg.fg(rcol),
            );
            y += 1;
            let mut sub = Vec::new();
            if !t.scope.is_empty() {
                sub.push(format!("scope {}", t.scope));
            }
            if !t.deps.is_empty() {
                sub.push(format!("after {}", t.deps.join(", ")));
            }
            if !sub.is_empty() {
                put(
                    buf,
                    r.x + 15,
                    y,
                    lw.saturating_sub(14),
                    &sub.join(" · "),
                    Style::default().fg(FAINT),
                );
                y += 1;
            }
        }
        if y + 1 < bottom {
            let review = snap
                .nodes
                .iter()
                .find(|n| n.orchestrator && live)
                .map(|n| format!("review   ● {:<8} {}", short_agent(&n.cli), n.status));
            let line = review.unwrap_or_else(|| {
                format!(
                    "then {} reviews every branch → you merge from Tree (m)",
                    short_agent(&p.reviewer)
                )
            });
            put(buf, r.x + 1, y, lw, &line, Style::default().fg(FAINT));
        }
        if side == 0 {
            return;
        }
        // settings column
        let sx = r.right() - side;
        let mut sy = r.y + 4;
        let row = |buf: &mut Buffer, sy: &mut u16, k: &str, v: String, col: Color| {
            put(buf, sx, *sy, 14, k, Style::default().fg(DIM));
            put(buf, sx + 14, *sy, side - 15, &v, Style::default().fg(col));
            *sy += 1;
        };
        let est: u64 = p.tasks.iter().filter_map(|t| t.est).sum();
        put(
            buf,
            sx,
            sy,
            side,
            "Dispatch settings",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        sy += 1;
        let par: Vec<String> = PARALLEL
            .iter()
            .map(|n| {
                if *n == p.parallel {
                    format!("[{n}]")
                } else {
                    n.to_string()
                }
            })
            .collect();
        row(buf, &mut sy, "at once  +/-", par.join(" "), TEXT);
        row(
            buf,
            &mut sy,
            "budget   b",
            p.budget
                .map(|b| format!("{} · pause all at it", fmt_tokens(b)))
                .unwrap_or("none".into()),
            TEXT,
        );
        if est > 0 {
            row(
                buf,
                &mut sy,
                "estimate",
                format!("~{}", fmt_tokens(est)),
                TEXT,
            );
        }
        let agents: Vec<String> = {
            let mut a: Vec<String> = p.tasks.iter().map(|t| short_agent(&t.agent)).collect();
            a.dedup();
            a
        };
        row(buf, &mut sy, "agents", agents.join(" · "), TEXT);
        row(
            buf,
            &mut sy,
            "isolation",
            "worktree + branch each".into(),
            TEXT,
        );
        row(
            buf,
            &mut sy,
            "when done",
            format!("{} reviews → you merge", short_agent(&p.reviewer)),
            TEXT,
        );
        let risk = overlap_risk(&p.tasks);
        if !risk.is_empty() {
            sy += 1;
            put(
                buf,
                sx,
                sy,
                side,
                "overlap risk",
                Style::default().fg(AMBER),
            );
            sy += 1;
            for (path, who) in risk.iter().take(3) {
                put(
                    buf,
                    sx,
                    sy,
                    side,
                    &format!("{path} ({})", who.join(", ")),
                    Style::default().fg(AMBER),
                );
                sy += 1;
            }
        }
        if let Some(t) = snap.team.as_ref().filter(|_| live) {
            sy += 1;
            row(buf, &mut sy, "used", fmt_tokens(t.used), TEXT);
            if let Some(s) = t.step {
                row(
                    buf,
                    &mut sy,
                    "released",
                    format!("stage {s} of {}", t.stages),
                    TEXT,
                );
            }
        }
    }

    fn draw_tree(&self, r: Rect, buf: &mut Buffer, snap: &Snap) {
        let rows = Self::tree_rows(snap);
        if rows.is_empty() {
            put(
                buf,
                r.x + 1,
                r.y,
                r.width - 2,
                "no agents yet — shift+F9 for one, or Plan (tab) for a team",
                Style::default().fg(DIM),
            );
            return;
        }
        let sel = self.sel.min(rows.len() - 1);
        let side = if r.width >= 110 { r.width / 2 - 4 } else { 0 };
        let lw = r.width - side - 1;
        let totals_h = 3 + rows
            .iter()
            .filter(|(i, _)| snap.nodes[*i].parent.is_none())
            .count() as u16;
        let list_h = r.height.saturating_sub(totals_h + 1).max(3);
        let top = sel.saturating_sub(list_h as usize - 1);
        let mut y = r.y;
        for (k, (i, branch)) in rows.iter().enumerate().skip(top) {
            let mut extra: Vec<String> = Vec::new();
            let n = &snap.nodes[*i];
            if y >= r.y + list_h {
                break;
            }
            let bg = if k == sel {
                Style::default().bg(SEL_BG)
            } else {
                Style::default()
            };
            fill(buf, Rect::new(r.x, y, lw, 1), bg);
            let icon = if n.orchestrator {
                "◆"
            } else if n.parent.is_some() {
                "◇"
            } else {
                "●"
            };
            let left = format!(" {branch}{icon} ");
            put(buf, r.x, y, lw, &left, bg.fg(Color::Indexed(n.color)));
            let lx = r.x + left.chars().count() as u16;
            let name = format!("{:<12} {:<8}", n.name, short_agent(&n.cli));
            put(
                buf,
                lx,
                y,
                lw.saturating_sub(lx - r.x),
                &name,
                bg.fg(TEXT).add_modifier(Modifier::BOLD),
            );
            let tok = n.tokens.map(fmt_tokens).unwrap_or_default();
            let right = format!(
                "{:<15} {:>9} {:>4}",
                n.status,
                tok,
                if n.files > 0 {
                    format!("{}f", n.files)
                } else {
                    String::new()
                }
            );
            let rw = right.chars().count() as u16;
            if lx + 22 + rw < r.x + lw {
                put(
                    buf,
                    r.x + lw - rw - 1,
                    y,
                    rw,
                    &right,
                    bg.fg(status_color(&n.status)),
                );
            }
            y += 1;
            for h in &n.helpers {
                extra.push(h.clone());
            }
            for h in extra {
                if y >= r.y + list_h {
                    break;
                }
                let pad: String = " ".repeat(branch.chars().count() + 3);
                put(
                    buf,
                    r.x,
                    y,
                    lw,
                    &format!("{pad}┄ {h} (inside {})", short_agent(&n.cli)),
                    Style::default().fg(FAINT),
                );
                y += 1;
            }
        }
        // branch totals
        let mut ty = r.y + list_h + 1;
        put(
            buf,
            r.x + 1,
            ty,
            lw,
            &format!(
                "{:<14}{:>7}{:>11}{:>7}  status",
                "branch", "agents", "tokens", "files"
            ),
            Style::default().fg(DIM),
        );
        ty += 1;
        let all: Vec<(String, Option<String>)> = snap
            .nodes
            .iter()
            .map(|n| (n.name.clone(), n.parent.clone()))
            .collect();
        for root in snap.nodes.iter().filter(|n| n.parent.is_none()) {
            if ty >= r.bottom() {
                break;
            }
            let mut names = vec![root.name.clone()];
            names.extend(crate::agents::descendants(&all, &root.name));
            let members: Vec<&Node> = snap
                .nodes
                .iter()
                .filter(|n| names.contains(&n.name))
                .collect();
            let tokens: u64 = members.iter().filter_map(|n| n.tokens).sum();
            let files: usize = members.iter().map(|n| n.files).sum();
            let running = members
                .iter()
                .filter(|n| matches!(n.status.as_str(), "working" | "starting"))
                .count();
            let status = if running > 0 {
                format!("{running} running")
            } else {
                root.status.clone()
            };
            put(
                buf,
                r.x + 1,
                ty,
                lw,
                &format!(
                    "{:<14}{:>7}{:>11}{:>7}  {status}",
                    root.name,
                    members.len(),
                    if tokens > 0 {
                        fmt_tokens(tokens)
                    } else {
                        "—".into()
                    },
                    files
                ),
                Style::default().fg(TEXT),
            );
            ty += 1;
        }
        let limits = format!(
            "limits: depth ≤ {} · agents ≤ {} · sub-agents each ≤ {}",
            snap.max_depth, snap.max_agents, snap.max_children
        );
        put(
            buf,
            r.x + 1,
            r.bottom().saturating_sub(1),
            lw,
            &limits,
            Style::default().fg(FAINT),
        );
        if side == 0 {
            return;
        }
        // details of the selected agent
        let n = &snap.nodes[rows[sel].0];
        let sx = r.x + lw + 1;
        for yy in r.y..r.bottom() {
            put(buf, sx - 1, yy, 1, "│", Style::default().fg(FAINT));
        }
        let mut path = vec![n.name.clone()];
        let mut cur = n.parent.clone();
        while let Some(p) = cur {
            cur = snap
                .nodes
                .iter()
                .find(|x| x.name == p)
                .and_then(|x| x.parent.clone());
            path.insert(0, p);
        }
        let mut y = r.y;
        put(
            buf,
            sx + 1,
            y,
            side - 2,
            &path.join(" › "),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        );
        y += 1;
        let mut facts = vec![n.status.clone()];
        if let Some(t) = n.tokens {
            facts.push(fmt_tokens(t));
        }
        facts.push(format!("{}m", (n.age / 60.0) as u64));
        put(
            buf,
            sx + 1,
            y,
            side - 2,
            &facts.join(" · "),
            Style::default().fg(DIM),
        );
        y += 1;
        if let Some(b) = &n.branch {
            put(
                buf,
                sx + 1,
                y,
                side - 2,
                &format!("branch {b}"),
                Style::default().fg(DIM),
            );
            y += 1;
        }
        if n.files > 0 {
            put(
                buf,
                sx + 1,
                y,
                side - 2,
                &format!("{} files +{} −{}", n.files, n.added, n.removed),
                Style::default().fg(DIM),
            );
            y += 1;
        }
        if let Some(p) = &n.parent {
            put(
                buf,
                sx + 1,
                y,
                side - 2,
                &format!("spawned by {p} via sv agent spawn"),
                Style::default().fg(DIM),
            );
            y += 1;
        }
        y += 1;
        if !n.task.is_empty() {
            put(buf, sx + 1, y, side - 2, "task", Style::default().fg(DIM));
            y += 1;
            for line in wrap(&n.task, side.saturating_sub(3) as usize)
                .into_iter()
                .take(5)
            {
                put(buf, sx + 1, y, side - 2, &line, Style::default().fg(TEXT));
                y += 1;
            }
            y += 1;
        }
        let room = r.bottom().saturating_sub(y) as usize;
        for line in n.tail.iter().skip(n.tail.len().saturating_sub(room)) {
            put(buf, sx + 1, y, side - 2, line, Style::default().fg(DIM));
            y += 1;
        }
    }

    fn draw_timeline(&self, r: Rect, buf: &mut Buffer, snap: &Snap) {
        let rows = Self::tree_rows(snap);
        if rows.is_empty() {
            put(
                buf,
                r.x + 1,
                r.y,
                r.width - 2,
                "no agents yet",
                Style::default().fg(DIM),
            );
            return;
        }
        let sel = self.sel.min(rows.len() - 1);
        let label_w: u16 = 18;
        let bar_w = r.width.saturating_sub(label_w + 2).max(10) as f64;
        let span = snap.nodes.iter().map(|n| n.age).fold(30.0, f64::max);
        let mut y = r.y;
        for (k, (i, _)) in rows.iter().enumerate() {
            if y + 2 >= r.bottom() {
                break;
            }
            let n = &snap.nodes[*i];
            let st = if k == sel {
                Style::default().bg(SEL_BG)
            } else {
                Style::default()
            };
            let indent = " ".repeat(n.depth.saturating_sub(1) as usize);
            put(
                buf,
                r.x,
                y,
                label_w,
                &format!(" {indent}{}", n.name),
                st.fg(Color::Indexed(n.color)).add_modifier(Modifier::BOLD),
            );
            // node start, in bar coordinates
            let start = span - n.age;
            for c in 0..bar_w as u16 {
                let t = (c as f64 + 0.5) / bar_w * span; // seconds from the left edge
                if t < start {
                    continue;
                }
                let since = t - start;
                let state = n
                    .history
                    .iter()
                    .rev()
                    .find(|(at, _)| *at <= since)
                    .map(|(_, s)| *s)
                    .unwrap_or("starting");
                let (ch, col) = match state {
                    "working" => ("━", Color::Indexed(n.color)),
                    "approval" => ("⚑", AMBER),
                    "paused" => ("┄", FAINT),
                    "done" | "idle" => ("─", FAINT),
                    "exited" => (
                        if since - n.history.last().map_or(0.0, |h| h.0) < span / bar_w {
                            "✔"
                        } else {
                            " "
                        },
                        GREEN,
                    ),
                    "failed" => (
                        if since - n.history.last().map_or(0.0, |h| h.0) < span / bar_w {
                            "✖"
                        } else {
                            " "
                        },
                        RED,
                    ),
                    _ => ("·", FAINT),
                };
                put(buf, r.x + label_w + 1 + c, y, 1, ch, st.fg(col));
            }
            y += 1;
        }
        // time axis
        let ay = r.bottom() - 1;
        let mins = span / 60.0;
        let marks = 4;
        for m in 0..=marks {
            let x = r.x + label_w + 1 + ((m as f64 / marks as f64) * (bar_w - 1.0)) as u16;
            let ago = mins * (1.0 - m as f64 / marks as f64);
            let label = if m == marks {
                "now".to_string()
            } else if ago >= 1.0 {
                format!("−{}m", ago.round() as u64)
            } else {
                format!("−{}s", (ago * 60.0) as u64)
            };
            put(
                buf,
                x.min(r.right().saturating_sub(label.len() as u16)),
                ay,
                6,
                &label,
                Style::default().fg(FAINT),
            );
        }
    }

    fn draw_log(&self, r: Rect, buf: &mut Buffer, snap: &Snap) {
        if snap.log.is_empty() {
            put(
                buf,
                r.x + 1,
                r.y,
                r.width - 2,
                "nothing yet — spawns, messages, events and refusals show here (also: sv agent log)",
                Style::default().fg(DIM),
            );
            return;
        }
        let h = r.height as usize;
        let end = snap
            .log
            .len()
            .saturating_sub(self.scroll.min(snap.log.len().saturating_sub(1)));
        let start = end.saturating_sub(h);
        for (k, (t, text)) in snap.log[start..end].iter().enumerate() {
            let time = crate::data::value::iso_date(*t);
            let time = time.get(11..19).unwrap_or(&time).to_string();
            let col = if text.contains("refused") || text.contains("failed") {
                RED
            } else if text.contains("budget") || text.contains("pause") {
                AMBER
            } else if text.contains(" → ") {
                CYAN
            } else {
                TEXT
            };
            put(
                buf,
                r.x + 1,
                r.y + k as u16,
                9,
                &time,
                Style::default().fg(FAINT),
            );
            put(
                buf,
                r.x + 11,
                r.y + k as u16,
                r.width.saturating_sub(12),
                text,
                Style::default().fg(col),
            );
        }
    }
}

fn status_color(s: &str) -> Color {
    match s {
        "working" | "starting" => CYAN,
        "done" => GREEN,
        "exited" => GREEN,
        "paused" | "needs approval" => AMBER,
        s if s.starts_with("exited ") => RED,
        _ => DIM,
    }
}

fn short_agent(a: &str) -> String {
    if Cli::parse(a).is_some() {
        return a.to_string();
    }
    // a command: its program name
    let prog = a.split_whitespace().next().unwrap_or(a);
    prog.rsplit('/').next().unwrap_or(prog).to_string()
}

fn short_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, w: u16, s: &str, style: Style) {
    if w == 0 || y >= buf.area.bottom() || x >= buf.area.right() {
        return;
    }
    let w = w.min(buf.area.right() - x);
    buf.set_stringn(x, y, s, w as usize, style);
}

fn fill(buf: &mut Buffer, r: Rect, style: Style) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

fn wrap(s: &str, w: usize) -> Vec<String> {
    let w = w.max(10);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = "Here is the plan:\n```json\n{\"tasks\": [\
        {\"id\": \"API\", \"title\": \"Add POST /alerts\", \"scope\": \"src/api/\", \"deps\": [], \"estimate_tokens\": 60000},\
        {\"id\": \"ui\", \"title\": \"Alert form\", \"scope\": \"src/ui/\", \"agent\": \"codex\"},\
        {\"id\": \"push\", \"title\": \"Websocket push\", \"scope\": \"src/api/ws.py\", \"deps\": [\"ghost\"]},\
        {\"id\": \"tests\", \"title\": \"Tests\", \"deps\": [\"api\", \"ui\"]}\
        ]}\n```\nGood luck.";

    #[test]
    fn a_planner_reply_becomes_a_staged_plan() {
        let t = parse_plan(REPLY, "claude").unwrap();
        let ids: Vec<&str> = t.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["api", "ui", "push", "tests"]);
        assert_eq!(t[0].agent, "claude");
        assert_eq!(t[1].agent, "codex");
        assert!(t[2].deps.is_empty(), "unknown deps are dropped");
        assert_eq!(t[3].stage, 2);
        assert_eq!(t[0].est, Some(60000));
        let risk = overlap_risk(&t);
        assert_eq!(
            risk,
            vec![(
                "src/api".to_string(),
                vec!["api".to_string(), "push".to_string()]
            )]
        );
        assert!(
            parse_plan("no idea", "claude")
                .unwrap_err()
                .contains("no JSON")
        );
        let cyc = "{\"tasks\":[{\"id\":\"a\",\"title\":\"x\",\"deps\":[\"b\"]},{\"id\":\"b\",\"title\":\"y\",\"deps\":[\"a\"]}]}";
        assert!(parse_plan(cyc, "claude").unwrap_err().contains("circle"));
    }

    #[test]
    fn dropping_and_dispatching() {
        let mut t = parse_plan(REPLY, "claude").unwrap();
        drop_task(&mut t, "ui");
        let tests = t.iter().find(|x| x.id == "tests").unwrap();
        assert_eq!(tests.deps, vec!["api".to_string()]);
        let team = plan_to_team("plan-1", &t, "price alerts", "claude", Some(1_000_000));
        assert_eq!(team.agents.len(), 4); // 3 tasks + review
        let api = team.member("api").unwrap();
        assert_eq!(api.start, Start::Now);
        assert!(api.task.contains("Only touch: src/api/"));
        assert_eq!(
            team.member("tests").unwrap().start,
            Start::After(vec!["api".into()])
        );
        let review = team.member("review").unwrap();
        assert_eq!(
            review.start,
            Start::After(vec!["api".into(), "push".into(), "tests".into()])
        );
        assert!(review.task.contains("agent/api"));
        assert_eq!(review.worktree, Some(false));
        let cmd = plan_to_team("p", &t, "g", "/x/fake.sh go", None);
        assert_eq!(cmd.member("review").unwrap().cmd, "/x/fake.sh go");
        assert!(planner_prompt("g", &["claude".into()]).contains("ONLY a JSON object"));
    }

    #[test]
    fn plan_keys_edit_reassign_drop() {
        let tasks = parse_plan(REPLY, "claude").unwrap();
        let plan = Plan {
            goal: "g".into(),
            phase: Phase::Ready,
            tasks,
            sel: 0,
            edit: None,
            parallel: 3,
            budget: None,
            clis: vec!["claude".into(), "codex".into()],
            planner: "claude".into(),
            reviewer: "claude".into(),
            folder: PathBuf::from("/tmp"),
            step: false,
            run: 0,
        };
        let mut b = Board::new(View::Plan, plan);
        let snap = Snap::default();
        b.key(b"a", &snap);
        assert_eq!(b.plan.tasks[0].agent, "codex");
        b.key(b"e", &snap);
        assert!(b.typing());
        for k in [&[0x15u8][..], b"N", b"e", b"w"] {
            b.key(k, &snap);
        }
        b.key(b"\r", &snap);
        assert_eq!(b.plan.tasks[0].title, "New");
        b.key(b"+", &snap);
        assert_eq!(b.plan.parallel, 4);
        b.key(b"b", &snap);
        assert_eq!(b.plan.budget, Some(500_000));
        b.key(b"\x1b[B", &snap);
        b.key(b"x", &snap);
        assert!(!b.plan.tasks.iter().any(|t| t.id == "ui"));
        assert_eq!(b.key(b"s", &snap).1, Some(BoardAct::Dispatch));
        assert!(b.plan.step);
    }
}

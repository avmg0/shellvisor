//! `team.yaml`: a team of agents started together with `sv agent run`.
//!
//! ```yaml
//! team:
//!   name: price-alerts
//!   budget_tokens: 2000000   # pause every agent of the team past this
//!   max_agents: 10
//!   max_depth: 3             # lead › agent › sub-agent
//! agents:
//!   - id: lead
//!     cli: claude
//!     role: orchestrator     # gets the goal, and is told who it can start
//!     can_spawn: [api, tests]
//!   - id: api
//!     cli: claude
//!     worktree: true
//!     max_children: 3        # may start its own sub-agents
//!     approve: [install, network]
//!   - id: tests
//!     cmd: ./run-test-agent.sh
//!     start: after:api
//! on:
//!   - event: tests.failed
//!     send: api
//!     text: "fix failing tests: {log}"
//! ```
//!
//! Parsing and checking happen in `sv` (so mistakes are reported where you
//! typed the command); the checked [`Team`] travels to Shellvisor as JSON.

use crate::agents::{Action, Cli, valid_name};
use serde::{Deserialize, Serialize};

/// Events an agent produces by itself. Agents (or scripts) can add their own
/// with `sv agent emit <event>`.
pub const EVENTS: &[&str] = &["started", "done", "failed", "exited", "needs_approval"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    team: RawTeam,
    agents: Vec<RawAgent>,
    #[serde(default)]
    on: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTeam {
    name: String,
    budget_tokens: Option<u64>,
    /// Not supported — kept only to explain why.
    budget_usd: Option<serde_yaml_ng::Value>,
    max_agents: Option<usize>,
    max_depth: Option<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAgent {
    id: String,
    cli: Option<String>,
    cmd: Option<String>,
    role: Option<String>,
    task: Option<String>,
    #[serde(default)]
    can_spawn: Vec<String>,
    worktree: Option<bool>,
    max_children: Option<usize>,
    #[serde(default)]
    approve: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    mcp: Vec<String>,
    start: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    event: String,
    send: String,
    text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub budget_tokens: Option<u64>,
    pub max_agents: Option<usize>,
    pub max_depth: Option<u8>,
    pub agents: Vec<Member>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    /// CLI name (claude, codex, gemini, aider); empty with `cmd`.
    pub cli: String,
    pub cmd: String,
    pub orchestrator: bool,
    pub task: String,
    pub can_spawn: Vec<String>,
    pub worktree: Option<bool>,
    pub max_children: Option<usize>,
    pub approve: Vec<String>,
    /// Actions refused for it without asking.
    pub deny: Vec<String>,
    pub mcp: Vec<String>,
    pub start: Start,
    /// Stage in an auto-plan (1, 2, …); 0 in team.yaml. Step-by-step
    /// dispatch starts one stage at a time.
    #[serde(default)]
    pub stage: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Start {
    /// When the team starts.
    Now,
    /// When another member has finished.
    After(Vec<String>),
    /// Only when an agent (or you) runs `sv agent spawn --name <id>`.
    Spawned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Member id, or `*` for any agent of the team.
    pub agent: String,
    pub event: String,
    pub send: String,
    pub text: String,
}

impl Member {
    /// How many sub-agents it may start (None: not allowed to start any).
    pub fn children_allowed(&self) -> Option<usize> {
        match (self.max_children, self.can_spawn.len()) {
            (None, 0) => None,
            (m, n) => Some(m.unwrap_or(0).max(n)),
        }
    }

    pub fn may_spawn(&self, name: &str) -> Result<(), String> {
        if self.can_spawn.iter().any(|n| n == name) {
            return Ok(());
        }
        match (self.can_spawn.is_empty(), self.max_children) {
            (_, Some(_)) => Ok(()),
            (true, None) => Err(format!(
                "refused: {} may not start agents (give it can_spawn or max_children in team.yaml)",
                self.id
            )),
            (false, None) => Err(format!(
                "refused: {} may only start {} (can_spawn in team.yaml)",
                self.id,
                self.can_spawn.join(", ")
            )),
        }
    }
}

impl Team {
    pub fn member(&self, id: &str) -> Option<&Member> {
        self.agents.iter().find(|m| m.id == id)
    }

    /// What the orchestrator is told along with the goal (one line: it is
    /// typed into the agent's prompt).
    pub fn brief(&self, me: &Member, goal: &str) -> String {
        let mut s = goal.trim().to_string();
        if me.orchestrator {
            s += &format!(
                " — You lead the agent team `{}` inside Shellvisor.",
                self.name
            );
            if !me.can_spawn.is_empty() {
                let who: Vec<String> = me
                    .can_spawn
                    .iter()
                    .map(|id| match self.member(id) {
                        Some(m) if !m.task.is_empty() => format!("{id} ({})", m.task),
                        _ => id.clone(),
                    })
                    .collect();
                s += &format!(
                    " Hand work to your team with `sv agent spawn --name <id> \"<task>\"` (you can start: {}),",
                    who.join(", ")
                );
                s += " `sv agent wait <id>` to wait for one, `sv agent send <id> \"…\"` to message one, `sv agent tree` to see everyone.";
            }
            if let Some(b) = self.budget_tokens {
                s += &format!(" The whole team has a budget of {} tokens.", b);
            }
        }
        s
    }
}

/// Parse and check a team file. Errors say which agent / rule and what to write.
pub fn parse(text: &str) -> Result<Team, String> {
    let raw: RawFile =
        serde_yaml_ng::from_str(text).map_err(|e| e.to_string().replace(", budget_usd", ""))?;
    let t = raw.team;
    if t.budget_usd.is_some() {
        return Err(budget_usd_error());
    }
    if !valid_name(&t.name) {
        return Err(format!(
            "team.name `{}` — use letters, digits, - and _",
            t.name
        ));
    }
    if t.budget_tokens == Some(0) || t.max_agents == Some(0) || t.max_depth == Some(0) {
        return Err("team: budget_tokens, max_agents and max_depth must be more than 0".into());
    }
    if raw.agents.is_empty() {
        return Err("agents: the team has no agents".into());
    }
    let ids: Vec<String> = raw.agents.iter().map(|a| a.id.clone()).collect();
    let mut agents = Vec::new();
    for (i, a) in raw.agents.iter().enumerate() {
        let at = format!("agents[{i}] ({})", a.id);
        if !valid_name(&a.id) {
            return Err(format!("{at}: id — use letters, digits, - and _"));
        }
        if ids.iter().filter(|x| **x == a.id).count() > 1 {
            return Err(format!("{at}: there are two agents called {}", a.id));
        }
        let (cli, cmd) = match (a.cli.as_deref(), a.cmd.as_deref()) {
            (Some(_), Some(_)) => return Err(format!("{at}: give either cli or cmd, not both")),
            (None, None) => {
                return Err(format!(
                    "{at}: needs cli: (claude, codex, gemini, aider) or cmd: \"<command>\""
                ));
            }
            (Some(c), None) => match Cli::parse(c) {
                Some(cli) => (cli.name().to_string(), String::new()),
                None => {
                    return Err(format!(
                        "{at}: unknown cli `{c}` — claude, codex, gemini, aider, or cmd: \"<command>\""
                    ));
                }
            },
            (None, Some(c)) if c.trim().is_empty() => return Err(format!("{at}: cmd is empty")),
            (None, Some(c)) => (String::new(), c.trim().to_string()),
        };
        let orchestrator = match a.role.as_deref() {
            None | Some("worker") => false,
            Some("orchestrator") => true,
            Some(r) => return Err(format!("{at}: role `{r}` — orchestrator or worker")),
        };
        for n in &a.can_spawn {
            if !valid_name(n) {
                return Err(format!(
                    "{at}: can_spawn `{n}` — use letters, digits, - and _"
                ));
            }
            if *n == a.id {
                return Err(format!("{at}: an agent can't spawn itself"));
            }
        }
        for (field, list) in [("approve", &a.approve), ("deny", &a.deny)] {
            for x in list {
                if Action::parse(x).is_none() {
                    return Err(format!(
                        "{at}: {field} `{x}` — one of read edit test lint install network push delete command"
                    ));
                }
            }
        }
        let start = match a.start.as_deref().map(str::trim) {
            None => None,
            Some("now") => Some(Start::Now),
            Some("spawned" | "on_spawn") => Some(Start::Spawned),
            Some(s) => match s.strip_prefix("after:").map(str::trim) {
                Some(list) => {
                    let deps: Vec<String> = list
                        .split(',')
                        .map(|d| d.trim().to_string())
                        .filter(|d| !d.is_empty())
                        .collect();
                    if deps.is_empty() {
                        return Err(format!("{at}: start after: — after which agent?"));
                    }
                    for dep in &deps {
                        if !ids.contains(dep) || *dep == a.id {
                            return Err(format!(
                                "{at}: start after:{dep} — no other agent called {dep}"
                            ));
                        }
                    }
                    Some(Start::After(deps))
                }
                None => {
                    return Err(format!(
                        "{at}: start `{s}` — now, after:<id>[,<id>…] or spawned"
                    ));
                }
            },
        };
        agents.push((
            Member {
                id: a.id.clone(),
                cli,
                cmd,
                orchestrator,
                task: a.task.clone().unwrap_or_default().trim().to_string(),
                can_spawn: a.can_spawn.clone(),
                worktree: a.worktree,
                max_children: a.max_children,
                approve: a.approve.clone(),
                deny: a.deny.clone(),
                mcp: a.mcp.clone(),
                start: Start::Now,
                stage: 0,
            },
            start,
        ));
    }
    if agents.iter().filter(|(m, _)| m.orchestrator).count() > 1 {
        return Err("agents: only one agent can be the orchestrator".into());
    }
    // Default start: listed in someone's can_spawn → when spawned; else now.
    let spawnable: Vec<String> = agents
        .iter()
        .flat_map(|(m, _)| m.can_spawn.clone())
        .collect();
    let members: Vec<Member> = agents
        .into_iter()
        .map(|(mut m, start)| {
            m.start = start.unwrap_or(if !m.orchestrator && spawnable.contains(&m.id) {
                Start::Spawned
            } else {
                Start::Now
            });
            m
        })
        .collect();
    // after: chains must not go round in a circle
    let graph: Vec<(String, Vec<String>)> = members
        .iter()
        .map(|m| {
            let deps = match &m.start {
                Start::After(d) => d.clone(),
                _ => Vec::new(),
            };
            (m.id.clone(), deps)
        })
        .collect();
    if let Some(path) = find_cycle(&graph) {
        return Err(format!(
            "agents: start after: goes round in a circle ({})",
            path.join(" → ")
        ));
    }
    if !members.iter().any(|m| m.start == Start::Now) {
        return Err(
            "agents: nothing starts — give one agent start: now (or role: orchestrator)".into(),
        );
    }
    let mut rules = Vec::new();
    for (i, r) in raw.on.iter().enumerate() {
        let at = format!("on[{i}]");
        let (agent, event) = r.event.split_once('.').ok_or_else(|| {
            format!(
                "{at}: event `{}` — write <agent>.<event>, e.g. tests.failed",
                r.event
            )
        })?;
        if agent != "*" && !ids.iter().any(|x| x == agent) {
            return Err(format!("{at}: event {} — no agent called {agent}", r.event));
        }
        if !valid_name(event) {
            return Err(format!(
                "{at}: event `{event}` — {} or your own word (sv agent emit)",
                EVENTS.join(", ")
            ));
        }
        if !ids.contains(&r.send) {
            return Err(format!("{at}: send: no agent called {}", r.send));
        }
        let text = r
            .text
            .clone()
            .unwrap_or_else(|| "{agent} {event}: {log}".into());
        rules.push(Rule {
            agent: agent.into(),
            event: event.into(),
            send: r.send.clone(),
            text,
        });
    }
    Ok(Team {
        name: t.name,
        budget_tokens: t.budget_tokens,
        max_agents: t.max_agents,
        max_depth: t.max_depth,
        agents: members,
        rules,
    })
}

/// A dependency cycle, as the ids around it (first repeated at the end).
pub fn find_cycle(graph: &[(String, Vec<String>)]) -> Option<Vec<String>> {
    fn visit(
        g: &[(String, Vec<String>)],
        id: &str,
        path: &mut Vec<String>,
        done: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if let Some(i) = path.iter().position(|p| p == id) {
            let mut c = path[i..].to_vec();
            c.push(id.to_string());
            return Some(c);
        }
        if done.iter().any(|d| d == id) {
            return None;
        }
        path.push(id.to_string());
        if let Some((_, deps)) = g.iter().find(|(n, _)| n == id) {
            for d in deps {
                if let Some(c) = visit(g, d, path, done) {
                    return Some(c);
                }
            }
        }
        path.pop();
        done.push(id.to_string());
        None
    }
    let mut done = Vec::new();
    for (id, _) in graph {
        if let Some(c) = visit(graph, id, &mut Vec::new(), &mut done) {
            return Some(c);
        }
    }
    None
}

fn budget_usd_error() -> String {
    "team.budget_usd: Shellvisor sees tokens, not prices (they change, and agent CLIs only log tokens) — use budget_tokens: 2000000".into()
}

/// Fill a rule's text: {agent} {event} {log} {goal}.
pub fn fill(text: &str, agent: &str, event: &str, log: &str, goal: &str) -> String {
    text.replace("{agent}", agent)
        .replace("{event}", event)
        .replace("{log}", log)
        .replace("{goal}", goal)
}

/// A starter file for `sv agent run --init`.
pub const STARTER: &str = "\
# Run with: sv agent run .shellvisor/team.yaml \"what the team should do\"
team:
  name: my-team
  budget_tokens: 2000000   # every agent of the team pauses past this
  max_agents: 6
  max_depth: 3             # lead › agent › sub-agent
agents:
  - id: lead
    cli: claude
    role: orchestrator     # gets the goal and starts the others
    can_spawn: [api, tests]
  - id: api
    cli: claude
    worktree: true
    max_children: 2        # may start sub-agents of its own
    approve: [test, lint]  # answered for it without asking
    deny: [delete]         # refused for it without asking
  - id: tests
    cli: claude
    task: write and run tests for what api changed
    start: after:api
on:
  - event: tests.failed    # sv agent emit failed \"…\" from the tests agent
    send: api
    text: \"fix the failing tests: {log}\"
";

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = "
team:
  name: price-alerts
  budget_tokens: 2000000
  max_agents: 10
  max_depth: 3
agents:
  - id: lead
    cli: claude
    role: orchestrator
    can_spawn: [api, ui, tests]
  - id: api
    cli: claude
    worktree: true
    max_children: 3
    approve: [install, network]
    mcp: [github, postgres]
  - id: tests
    cli: gemini
    start: after:api
on:
  - event: tests.failed
    send: api
    text: \"fix failing tests: {log}\"
";

    #[test]
    fn the_design_board_team_parses() {
        let t = parse(BOARD).unwrap();
        assert_eq!(t.name, "price-alerts");
        assert_eq!(t.budget_tokens, Some(2_000_000));
        let lead = t.member("lead").unwrap();
        assert!(lead.orchestrator);
        assert_eq!(lead.start, Start::Now);
        assert_eq!(lead.children_allowed(), Some(3));
        assert!(lead.may_spawn("ui").is_ok()); // a sub-agent that isn't a member
        assert!(
            lead.may_spawn("db")
                .unwrap_err()
                .contains("may only start api, ui, tests")
        );
        let api = t.member("api").unwrap();
        assert_eq!(api.start, Start::Spawned);
        assert!(api.may_spawn("anything").is_ok());
        let tests = t.member("tests").unwrap();
        assert_eq!(tests.start, Start::After(vec!["api".into()]));
        assert!(
            tests
                .may_spawn("x")
                .unwrap_err()
                .contains("may not start agents")
        );
        assert_eq!(t.rules[0].agent, "tests");
        assert_eq!(t.rules[0].event, "failed");
        let b = t.brief(lead, "add price alerts");
        assert!(b.starts_with("add price alerts — You lead"));
        assert!(b.contains("you can start: api, ui, tests"));
        assert!(!b.contains('\n'));
        assert_eq!(
            fill("fix: {log}", "tests", "failed", "2 failed", ""),
            "fix: 2 failed"
        );
        parse(STARTER).unwrap();
    }

    #[test]
    fn mistakes_get_clear_errors() {
        let bad = |from: &str, to: &str, want: &str| {
            let text = BOARD.replacen(from, to, 1);
            let e = parse(&text).unwrap_err();
            assert!(e.contains(want), "{to:?} → {e}");
        };
        bad(
            "budget_tokens: 2000000",
            "budget_usd: 5.0",
            "tokens, not prices",
        );
        bad("cli: gemini", "cli: gemeni", "unknown cli `gemeni`");
        bad(
            "cli: gemini",
            "cli: gemini\n    cmd: x",
            "either cli or cmd",
        );
        bad(
            "start: after:api",
            "start: after:db",
            "no other agent called db",
        );
        bad("start: after:api", "start: later", "now, after:<id>");
        bad(
            "approve: [install, network]",
            "approve: [install, nuke]",
            "approve `nuke`",
        );
        bad("event: tests.failed", "event: failed", "<agent>.<event>");
        bad(
            "event: tests.failed",
            "event: db.failed",
            "no agent called db",
        );
        bad("send: api", "send: db", "send: no agent called db");
        bad("role: orchestrator", "role: boss", "role `boss`");
        bad("id: tests", "id: api", "two agents called api");
        bad(
            "max_depth: 3",
            "max_depth: 3\n  colour: red",
            "unknown field `colour`",
        );
        bad(
            "  - id: api\n    cli: claude",
            "  - id: api\n    cli: claude\n    start: after:tests",
            "circle",
        );
    }
}

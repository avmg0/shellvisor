# Design boards vs. what is built

Checked 2026-11-07 against `main` after the board-matching pass on top of
1.0.3 (window frame, system and flow panes, file views, tabs, agents tab,
New agent form, Option-as-Alt dialog, basic-view hint). One row per thing a
board shows. ✅ built · ◐ built differently or in part · ✗ not built.

Totals: **21 boards · 140 items · 129 ✅ built · 7 ◐ partly or differently · 4 ✗ not built**.

## 1 · Default workspace (`sv layout dev`)

| Item | | Notes |
|---|---|---|
| Files │ shell │ system over flow, four panes | ✅ | ratios 22 / 62 / 60 |
| Header: folder, shell, downloads %, agents count, CPU / MEM | ✅ | two-row header: folder, shell, pid; downloads, agents, approvals, live, CPU, MEM, clock |
| Files pane follows the shell's folder (⇅ synced) | ✅ | `f` stops following |
| Preview with syntax colours, `i` / `c` / `e` | ✅ | |
| Shell with git prompt, commands, output | ✅ | your real shell |
| System: CPU per core, memory, swap, disk, temps, net, I/O, processes | ✅ | |
| Thermal state "nominal", fan rpm | ◐ | thermal view (`t`) shows sensors and CPU speed limit; fans not read |
| Flow pane under system | ✅ | |

## 2 · Flow (`sv flow`, F8)

| Item | | Notes |
|---|---|---|
| Per-stage in / out / rate / kept %, ◀ slowest | ✅ | |
| Output tail, stderr per stage | ✅ | |
| `s` send to shell, `r` run again | ✅ | |
| Live job view | ✅ | pipe links on Linux only |
| Throughput graph per stage (bytes/s, last 30 s) | ✅ | v1.0.1, with the stage cards and arrows as on the board |
| Summary printed in the shell at the end | ✅ | |

## 3 · Focus mode

| Item | | Notes |
|---|---|---|
| Shell full screen, apps like vim render normally | ✅ | basic view |
| Toast when a long command finishes | ✅ | |
| Bottom status strip (folder, branch, job, CPU spark, net) | ✅ | `sv layout focus`: SHELL chip, folder, branch, job, CPU, MEM; a rail of the other panes |

## 4 · Layouts

| Item | | Notes |
|---|---|---|
| Preset thumbnails (dev, monitor, files, data, basic, yours) | ✅ | in the layout editor; plus a thermal preset; agents layout is F9 |
| `F7` / `alt+l` cycles arrangements | ✅ | |
| Editor: palette, map, split, remove, resize, undo, esc cancels | ✅ | v1.0.1 |
| Save as name / default / for this folder, YAML preview | ✅ | v1.0.1; saved in `layouts/`, not config.yaml |
| Drag a pane from the palette onto a drop zone | ✅ | a dashed cyan zone says where it lands (split left / right / above / below, or replace) |
| Preview / Agents / Activity as pane kinds | ✗ | not panes in Shellvisor |

## 4b · Keymap & Mac setup

| Item | | Notes |
|---|---|---|
| Keymap, editable in config.yaml, F1 lists it | ✅ | |
| Leader `ctrl+]` with hint bar | ✅ | |
| Option-as-Alt detection with setup steps | ✅ | a dialog the first time, with the fix for your terminal, Test again, Use leader key, Later |
| Shell keys left alone (ctrl+r, ctrl+t, alt+b …) | ✅ | |

## 5 · Agents side by side (F9)

| Item | | Notes |
|---|---|---|
| One column per agent, real CLIs | ✅ | |
| Status, files changed, tokens, time per column | ✅ | tokens, not dollars |
| Approvals list with allow / deny / always | ✅ | `alt+a` |
| File-overlap warning | ✅ | |
| Orchestrator with task graph | ✅ | agent board (`alt+g`) Plan |
| Message bus / handoffs log | ✅ | agent board Log, `sv agent log` |
| Pause all / resume / kill | ✅ | `ctrl+] Z` / `ctrl+] U` and the orchestrator column; `sv agent pause|resume|stop --all` |
| Broadcast a message to all agents | ✅ | `ctrl+] T` |
| `d` diff view, `s` sync scroll | ◐ | diff view is `ctrl+] D` (typing in a column goes to the agent); no sync scroll |
| `1-9` focus agent, `F5` zoom | ✅ | |

## 6 · Downloads & loading states

| Item | | Notes |
|---|---|---|
| Detected from git, npm, pip, uv, brew, docker, cargo, curl … | ✅ | |
| Loading circle + % in tab, pane title, status bar | ✅ | v1.0.1 adds a filling progress bar, speed and time left |
| ✔ / ✖ then a toast with duration | ✅ | |
| Activity list with ETA and rate per download | ✅ | v1.0.1; `ctrl+] j`, `sv activity`, click the status bar |
| Spinner style setting | ✅ | v1.0.1; circle, dots, arc, line, pulse, bounce; bar or percent |
| Toast actions (`c` cd into it, `o` open in files) | ◐ | v1.0.1; `ctrl+] c` / `ctrl+] o` while the toast shows (plain `c` would go to your shell); plain `c` / `o` in the activity list |
| Loading state in agent column / file tree node | ✅ | v1.0.1; agents' installs spotted on their screen |

## 7 · Agents, Auto-plan

| Item | | Notes |
|---|---|---|
| Goal → proposed plan in stages | ✅ | `alt+g` Plan, `sv agent plan` |
| Edit / reassign / drop tasks, re-plan | ✅ | |
| Agents at once, budget cap | ✅ | budget in tokens |
| Estimate, overlap risk | ✅ | tokens, not dollars |
| Dispatch all / step by step | ✅ | |
| Review before merge | ✅ | review agent; you merge (Tree `m`) |
| Approval policy per dispatch (auto / ask / never) | ✗ | uses `agents.approvals` from config |

## 8 · Start an agent manually / attach

| Item | | Notes |
|---|---|---|
| New agent form: CLI, name, first instruction | ✅ | `shift+F9` |
| Own worktree or share the folder | ✅ | |
| Runs in zsh / bash choice | ✅ | |
| Approvals choice in the form | ✅ | ask, or full auto |
| Limits: budget, stop after N minutes | ✅ | stop after N minutes pauses it; token budget via team.yaml (no dollar costs) |
| Detect an agent already running and attach | ✅ | offered in the form as Attach as column |

## 9 · File views — tree, columns, list, grid

| Item | | Notes |
|---|---|---|
| Tree with git badges, filter, agent badges | ✅ | |
| Indent guides | ✗ | |
| Columns (parent │ folder │ preview) | ✅ | v1.0 P6 |
| Columns: type to jump | ✗ | `/` filters |
| List with size and modified | ✅ | |
| List: sortable columns, multi-select, copy / move | ✅ | `s` sorts, `space` marks, `y` copies paths, `m` types `mv -i` at the prompt |
| Grid coloured by type | ✅ | icons by type |
| Grid: image size and dimensions, `+/−` density | ◐ | cards with item count, PNG dimensions or size; no density keys |
| View numbers 1 Tree 2 Columns 3 List 4 Grid 5 Size 6 Git 7 Find 8 Recent | ✅ | |

## 10 · File views — size, git, find, recent

| Item | | Notes |
|---|---|---|
| Size map: biggest first, drill in, background scan | ✅ | bars |
| Size: `x` delete (asks first) | ✅ | moves it to the Trash after `y` |
| Git: changed files + diff preview | ✅ | |
| Git: staged / changes / untracked groups, by agent | ✅ | |
| Find: fuzzy names and text in files, open at line | ✅ | |
| Recent: newest changes anywhere below | ✅ | |
| Recent: who changed it (you / agents / downloads), `u` undo agent edit | ◐ | who (you or the agent) and what happened; no undo |

## 11 · Performance budget

| Item | | Notes |
|---|---|---|
| Binary ≤ 8 MB | ✅ | 2.3 MB |
| Idle memory ≤ 15 MB | ✅ | 5.1 MB |
| Idle CPU < 0.5 % | ✅ | 0 % |
| Startup < 100 ms | ✅ | 7 ms first frame |
| Redraw on change, hidden panes sleep, capped scrollback | ✅ | |
| Low-power mode, auto on battery | ✅ | |

## 12 · Shell tabs, splits, close, restore

| Item | | Notes |
|---|---|---|
| Label: number · shell · what it's doing | ✅ | |
| States: running, ✖ exit, • new output, ! bell, ◆ agent, ⚑ approval | ✅ | |
| ✔ for 5 s after a long command | ✅ | tab mark and toast |
| ? waiting for input, pinned tabs | ✅ | |
| Compact bar with many tabs | ✅ | numbers only when they don't fit |
| Splits, alt+arrows move, F5 zoom | ✅ | |
| Drag the divider or alt+shift+arrows to resize | ✅ | |
| Tab menu: rename, split, close | ✅ | |
| Tab menu: duplicate, change shell, move to window, pin, mute | ✅ | the board's grid; Move to new window explains why a terminal can't |
| Close dialog for a running command | ✅ | |
| "Keep in background" | ✅ | off the bar, a ◌ chip brings it back |
| Restore last session | ✅ | |

## 13 · sv data commands, table viewer, command bar

| Item | | Notes |
|---|---|---|
| Sources, filters, from / to formats | ✅ | |
| Table at the end of a pipe, F4 viewer (sort, filter, copy, insert) | ✅ | |
| Command bar with nu-style operators | ✅ | |
| Tables look like nushell (borders, folder colour) | ✅ | v1.0.1; `ls` itself prints a table |

## 14 · sv on / sv off

| Item | | Notes |
|---|---|---|
| sv on / off / exit / status, alt+` | ✅ | |
| sv autostart | ✅ | |
| Basic view sleeps panes | ✅ | |
| "basic · sv on" hint that fades | ✅ | 3 s |

## 15 · Agent tree & sub-agents

| Item | | Notes |
|---|---|---|
| Tree of who spawned whom, tokens rolled up per branch | ✅ | `alt+g` Tree |
| Selected agent: task, branch, output | ✅ | |
| Timeline view | ✅ | |
| Limits shown, refused spawns explained | ✅ | |
| `p` pause / `k` stop a branch | ✅ | |
| Fold branches, `c` promote to column, `+` spawn child | ✅ | ← / → fold and open, `c` asks then promotes, `+` types the child's task |
| Message box to the selected agent | ✅ | `i` types into it; spawned by · via · worktree and the instruction above it |

## 16 · Agent scripts & connections

| Item | | Notes |
|---|---|---|
| team.yaml (team, agents, on: rules) | ✅ | `budget_tokens` |
| `sv agent run / spawn / send / wait / tree / logs / pause / stop` | ✅ | |
| Sub-agents from inside an agent, limits | ✅ | |
| Connections: CLIs, logins, MCP servers, ssh hosts | ✅ | `sv connect` |
| API keys "in Keychain" | ◐ | presence from env and CLI logins; Keychain not read |

## 17 · MCP servers (`sv mcp`, ctrl+] M)

| Item | | Notes |
|---|---|---|
| Header with running / stopped / failing pill and clock | ✅ | |
| SERVERS list from Claude Code, Codex, Gemini, .mcp.json | ✅ | merged by name, filter `/` |
| Server cards: state, where configured, tools, usage | ✅ | |
| Detail box: command, transport, configured in, secrets, used by | ✅ | secret values never shown |
| Health: handshake ms, tools / resources / prompts, calls, context, pid | ✅ | real MCP handshake |
| Tabs: Tools, Resources, Prompts, Traffic, Permissions, Log | ✅ | |
| Tools table with calls, avg and per-agent permission | ✅ | columns scale with width; ⚠ changed descriptions; tool-list tokens |
| Try a tool (fields from the input schema) and Result | ✅ | `c` copy, `s` save |
| Key strip | ✅ | |

## 18 · MCP traffic, permissions, add a server

| Item | | Notes |
|---|---|---|
| Live traffic table, chips, pause, errors only | ✅ | from Claude Code, Codex and Gemini CLI logs |
| Approval banner `y` / `n` / `a` | ✅ | |
| Calls per minute, context sent back by server | ✅ | |
| Permissions grid allow / ask / never | ✅ | `.claude/settings.local.json` per agent |
| Add a server: templates, form, JSON / TOML previews, Add & check | ✅ | scrolls when the window is short |

## 19 · MCP call in full, the server, its log

| Item | | Notes |
|---|---|---|
| Call: when, agent, time, result, permission rule, log line | ✅ | first-byte time isn't in the logs |
| Request and result as pretty JSON, more lines, ←→ calls | ✅ | |
| Server: state, process (cpu, memory, up), command, started by | ✅ | |
| s Stop / r Restart / k Kill, on a crash restart / leave stopped | ✅ | a crash re-runs the handshake check |
| Server log: stderr / protocol / errors only, follow, search | ✅ | Claude Code's per-server log + calls + checks |
| Description-change banner, d shows the change | ✅ | |

## 20 · ssh port forwards

| Item | | Notes |
|---|---|---|
| Sessions: connected / not open, tab, keepalive, forwards, traffic | ✅ | |
| Forwards table: type, listen on, goes to, state, conns, in / out | ✅ | counted by Shellvisor's relay |
| Busy port: who holds it, p picks another | ✅ | lsof + ps |
| Add a forward: L / R / D, save, open on login, what it means, same as | ✅ | own ssh -N, no reconnect |

## 21 · Agent review

| Item | | Notes |
|---|---|---|
| Files with +/−, commits, checks | ✅ | tests from the agent's output, overlap, files you changed |
| Diff: unified / side by side / whole file, n / N, ]f | ✅ | |
| m Merge, S Squash, t Message, o files, e $EDITOR, x throw away | ✅ | x asks twice |
| Approve for a while: 15 min / 1 hour / this task, kinds | ✅ | |
| Opened from an agent column | ◐ | ctrl+] D (typing in a column goes to the agent), d on the agent board |

## Not on any board, built anyway

Thermal view, light theme, settings screen, prompt folder colour, Homebrew /
install script / signed releases, `sv agent emit|usage|budget`.

## Suggested next (biggest gaps first)

1. Agents tab: sync scroll across columns; Recent `u` to undo an agent's edit to one file.
2. File views: indent guides in Tree, type-to-jump in Columns, `+/−` density in Grid.

## What a terminal can't do (noted, not faked)

- Move a tab to a new window: a program inside a terminal can't open windows; the tab menu says so.
- Fan speed: needs root on macOS.
- Keychain lookups for API keys: Shellvisor checks the environment and the CLIs' own logins instead.
- Dollar costs: the CLIs report tokens, not prices; budgets are in tokens.

# Shellvisor

**Your real shell, plus a workspace around it — inside the terminal you already use.**

Shellvisor is a full-screen terminal app (like `tmux`, `htop` or `vim`) that runs your own zsh or bash in tabs and splits, and — as it grows — adds panes for files, live system stats, a data-flow view of your pipelines, and coding agents working side by side. Every command, alias and full-screen program you use today keeps working unchanged.

> **Status: early development (P0–P4 of 7 done).**
> Working today: tabs and splits of real zsh/bash shells, a tab bar that shows what each tab is doing (running ⠋, downloading ◐, failed ✖, new output •), a status bar, split layouts, your tabs restored after a restart, custom keys, and a one-key switch to *basic view*, which looks exactly like a plain terminal. Plus a **files pane** (tree, list, columns, grid, git changes, find, sizes, recent, preview) that follows your shell, a **system pane** (CPU, memory, disk, network, I/O, temperatures, processes), download/install progress for git, npm, pip, uv, brew, docker, cargo, curl and friends, **`sv` data commands** (nushell-style tables in bash/zsh) with an interactive table viewer, a **Flow pane** that shows the running job and meters pipelines stage by stage, and **coding agents side by side** (Claude Code, Codex, Gemini CLI, Aider or any command), each in its own git worktree, with one place to answer their approval prompts. Agents can start sub-agents, run as a team from `team.yaml`, or be planned from a goal on the agent board (`alt+g`). See [docs/ROADMAP.md](docs/ROADMAP.md).

## Why

- **One binary, tiny footprint.** Written in Rust. Budget: ≤ 8 MB binary, ≤ 15 MB memory, < 0.5 % CPU when idle. The current build is ~1.8 MB and uses ~5.6 MB of memory with three tabs plus the files and system panes open.
- **Nothing to relearn.** Your `.zshrc` / `.bash_profile` load as usual; vim, less, fzf, ssh and git all behave as before.
- **Off is one command away.** `sv off` hides the panels, `sv exit` leaves completely.

## Install

macOS or Linux, with zsh or bash.

```sh
brew install avmg0/tap/shellvisor                     # Homebrew (macOS, Linux)
curl -fsSL https://raw.githubusercontent.com/avmg0/shellvisor/main/scripts/install.sh | sh   # or: prebuilt binary into ~/.local/bin, checksum-verified
cargo install --git https://github.com/avmg0/shellvisor shellvisor                           # or: build it (Rust 1.85+)
```

Then run `shellvisor`. Inside it, `sv` is always available; Homebrew and the install script also put `sv` next to `shellvisor` so `sv on` works from a plain terminal (after `cargo install`, link it once: `ln -s "$(command -v shellvisor)" ~/.cargo/bin/sv`).

`sv autostart on` starts Shellvisor in every new terminal window (it adds a short, marked block to `~/.zshrc` or `~/.bashrc`; it skips tmux, SSH sessions and editor terminals, and quitting Shellvisor leaves you in your normal shell). `sv autostart off` removes it.

Release archives are checksummed and carry a signed build-provenance attestation — see [docs/RELEASING.md](docs/RELEASING.md).

## Use

```sh
shellvisor                 # start with your login shell
shellvisor --shell bash    # or pick: zsh, bash, or a full path
```

| Key / command | What it does |
|---|---|
| `F1` | all keys |
| `alt+t` · `F10` | new tab (same folder) |
| `alt+1…9` · `alt+[` / `alt+]` | go to tab · previous / next |
| `alt+r` · `alt+w` | rename · close tab (asks if something is still running) |
| `alt+\` · `alt+-` | split side by side · stacked |
| `alt+arrows` · `F6` | move between splits |
| `alt+shift+arrows` · `F5` | resize · zoom a split |
| `F7` · `alt+l` | rearrange splits: columns → rows → main + side → grid |
| `F2` · `sv files` | files pane: open · focus · close |
| `F3` · `sv sys` | system pane: open · focus · close |
| `F8` · `sv flowpane` | flow pane: the job running in the shell, or `sv flow` meters |
| `F4` | explore the last `sv` table: sort, filter, copy |
| `alt+;` | command bar: `ls \| where size > 1mb \| sort-by size -r` → table |
| `F9` · `shift+F9` | agents tab · new agent |
| `alt+a` | agents waiting for approval: `y` allow · `n` deny · `a` always |
| `sv layout dev` | files + shell + system + flow (also `monitor`, `files`, `data`, `agents`; or `alt+m`) |
| `alt+e` | layout editor: start from a preset or a saved layout, add shell / files / git / system / thermals / flow panes, remove, resize (keys or mouse), undo; nothing changes until ⏎, esc puts it back. Save by name, as the default, or for this folder (`sv layout <name>` opens a saved one) |
| `alt+s` · `sv settings` | settings: colour of the folder in your prompt, of the commands you type and of folders in lists, `ls` as a table, table borders, theme |
| `alt+m` | tab menu |
| ``alt+` `` · `sv off` / `sv on` | basic view ⇄ full view |
| `ctrl+]` then a letter | leader key — every shortcut, in every terminal (`ctrl+] ?` lists them) |
| mouse | click a tab or split to focus it · drag the border between splits to resize · wheel scrolls back |
| `ctrl+q` · `sv exit` | quit Shellvisor |
| `sv status` | is Shellvisor running here? which shell? |
| `sv power on\|off\|auto` | low-power mode (auto: on while on battery) |
| `sv theme light\|dark\|auto` | Shellvisor's own colours for a light or dark terminal |

### Files pane

Eight views — `1` tree · `2` list · `3` columns (parent | folder | preview) · `4` grid · `5` git changes · `6` find · `7` size (what takes the space, as bars) · `8` recent (newest changed files anywhere below); `tab` cycles. It follows the folder your shell is in, and refreshes after every command.

| Key | |
|---|---|
| `↑↓` `j k` · `⏎` | move · open folder / open file in `$EDITOR` |
| `→ ←` `l h` · `⌫` | expand · collapse · go up a folder |
| `i` · `c` · `e` | type the path at your prompt · `cd` there · edit |
| `p` / `space` · `J K` | preview (with colours; diffs in the git view) · scroll it |
| `/` · `.` · `r` · `f` | filter · hidden files · refresh · stop/start following the shell |

In **find**, type part of a name; `tab` switches to searching inside files (`⏎` runs it). Inside git repos, `.gitignore` is respected.

### System pane

CPU (history and per core), memory, swap, disks, network, disk I/O, load, temperatures (whatever sensors the machine exposes), Shellvisor's own usage, and the busiest processes (`c` / `m` sort by CPU / memory). It reads nothing while it's hidden, and refreshes every 2 s (5 s in low-power mode).

`t` switches to the **thermal view**: every sensor with its critical temperature, a temperature history per group (cpu, gpu, battery, disk), and whether the CPU is being slowed by heat — the clock against its top speed on Linux, macOS's CPU speed limit on a Mac. When the machine gets hot enough to slow down, Shellvisor tells you once (at most every 10 minutes).

**Mac users:** `alt` shortcuts need your terminal to send Option as Alt — Terminal.app: *Settings → Profiles → Keyboard → Use Option as Meta key*; iTerm2: *Profiles → Keys → Left Option key → Esc+*. Shellvisor tells you if it's off. The leader key `ctrl+]` always works.

**Selecting text:** Shellvisor uses the mouse wheel for scrollback. To select text with the mouse, hold **Option** (iTerm2) or turn off *View → Allow Mouse Reporting* (Terminal.app). Or set `mouse: false` in the config.

### `ls` as a table

Inside Shellvisor, `ls` (and `ls <folder>`, `ls -a`) prints a table like nushell's — name, type, size, modified — with folders in your folder colour, and `F4` explores it. `ls -l` and any other flag, `ls` in a pipe or script, and `command ls` are the real ls. Turn it off in settings (`alt+s`).

### Settings

`alt+s` (or `sv settings`) opens a settings screen with a live preview:

- the **colour of the folder in your prompt** (and bold) — Shellvisor's hooks colour `%1~` / `%~` (zsh) or `\W` / `\w` (bash) in whatever prompt you have, no dotfile editing; prompts drawn by tools like starship are left as they are
- the **colour of the commands you type** — light blue by default (the design's `#82aaff`), so your commands stand apart from their output, which stays in the normal text colour; `none` turns it off. With zsh-syntax-highlighting its colours win
- the **colour of folders in lists** — the files pane and `ls` / `sv` tables
- `ls` as a table on/off, table borders (rounded or plain), theme
- **dark background**: *design* (default) paints Shellvisor in the design's palette — a very dark blue-grey page `#0b0e12`, panels `#11151b`, shells on `#0e1217`, borders `#262c36`, text `#d6dbe3`, amber / cyan / green / red accents — or *terminal* to keep your terminal's own background. Programs inside shells keep their own colours; basic view always looks like your plain terminal. Terminals without 24-bit colour (e.g. older Terminal.app) get the nearest 256-colour shades

Choices are saved in `~/.local/state/shellvisor/settings.yaml` (your config.yaml is never rewritten) and shells pick them up at the next prompt.

## Data commands

`sv` also turns command output into tables you can filter and sort — nushell-style, from inside bash or zsh:

```sh
sv ls | sv where size gt 1mb | sv sort-by size -r     # stage by stage…
sv 'ps | where cpu gt 5 | sort-by cpu -r | first 10'  # …or one quoted pipeline
sv open package.json | sv get dependencies
sv open sales.csv | sv group-by country
ps aux | sv from ssv | sv where user eq root | sv to csv
```

Sources: `ls ps host df ports env history open git-log git-status`. Filters: `where select reject sort-by reverse first last skip uniq get group-by count sum avg min max columns describe`. Formats: `from json|csv|tsv|yaml|toml|ssv|lines`, `to json|jsonl|csv|tsv|md|yaml|text`. `sv data` lists them all.

On the terminal you get an aligned table; between `sv` stages rows travel as JSON lines, so `jq` works on them too.

Inside Shellvisor, **`F4`** opens the last table full-screen: `←→` pick a column, `s`/`S` sort, `/` filter (text, or a condition like `size > 1mb`), `c` / `J` / `m` copy as CSV / JSON / Markdown, `⏎` types the cell at your prompt. It stays quick with 100k rows. **`alt+;`** opens a command bar for the same pipelines without the `sv` prefix — the result opens in the table viewer, or `alt+⏎` puts it at your prompt.

### Flow

```sh
sv flow 'cat access.log | grep " 500 " | cut -d" " -f7 | sort | uniq -c | sort -rn'
```

runs the pipeline with a meter between every stage — lines and bytes in and out, % kept, rate, the stage holding everything up (◀ slowest), each stage's errors and the last lines of output — and prints a summary at the end. The **Flow pane** (`F8`, or `sv layout data`) shows it live; when nothing is metered it shows the job your shell is running right now: its processes, CPU, memory, read/write rates and pipes (rates and pipes on Linux).

## Agents

```sh
sv agent new claude --name api 'add a POST /alerts endpoint'   # or codex, gemini, aider
sv agent new --cmd 'ollama run llama3' --name local            # any other agent CLI
sv agents                                                      # all agents as a table
sv connect                                                     # installed CLIs, logins, MCP servers, ssh hosts
```

Each agent gets a column in the agents tab (`F9`; `shift+F9` opens the New agent form). In a git repository it works in its own worktree and branch (`.shellvisor/worktrees/<name>`, branch `agent/<name>`), so agents never edit the same checkout — or yours. The column title shows its status (working ⠋, waiting for you, ⚑ needs approval, done), the files it changed with +/− lines, and tokens used (Claude Code and Codex, read from their own session logs). Type in a column to message that agent. When two agents change the same file you get a warning, and the files pane marks files agents have touched (◆ name).

### Agents that start agents

Every shell inside Shellvisor can reach it over a private socket, so an agent (or you, or a script) can run a team:

```sh
sv agent spawn --name tests 'write tests for the alerts endpoint'   # from inside an agent: a sub-agent
sv agent send tests 'cover the 429 case too'                         # type a message into it
sv agent wait tests                                                  # until it has finished its task
sv agent tree                                                        # every agent, under its parent
sv agent logs tests -f                                               # follow what its screen shows
sv agent pause|resume|stop api --tree                                # an agent and all its sub-agents
sv agent log                                                         # who spawned and messaged whom
```

A sub-agent works in its parent's folder (and worktree) and inherits the approvals its parent set to "always". Trees are limited — by default 3 levels deep, 10 agents running, 3 sub-agents per agent (`agents.limits` in config.yaml); a spawn over a limit is refused with the reason, which the asking agent sees. When Claude Code runs its own helper agents (its Task tool), the column title shows them as `+N helpers` and `sv agent tree` lists them under it.

### Teams: team.yaml

A team file says who is on the team, who may start whom, and what happens when something goes wrong. `sv agent run --init` writes a starter `.shellvisor/team.yaml`:

```yaml
team:
  name: price-alerts
  budget_tokens: 2000000   # past this, every agent of the team pauses
  max_agents: 10
  max_depth: 3             # lead › agent › sub-agent
agents:
  - id: lead
    cli: claude
    role: orchestrator     # gets the goal, and is told who it can start
    can_spawn: [api, tests]
  - id: api
    cli: claude
    worktree: true
    max_children: 3        # may start sub-agents of its own
    approve: [install, network]   # answered "always" for it
    mcp: [github]          # checked: is this MCP server set up?
  - id: tests
    cmd: ./my-test-agent.sh   # any command; it gets the task in $SHELLVISOR_TASK
    start: after:api       # now (default) · after:<id> · spawned
on:
  - event: tests.failed    # started, done, failed, exited, needs_approval, or your own
    send: api
    text: "fix failing tests: {log}"   # {log} {agent} {event} {goal}
```

```sh
sv agent run team.yaml 'add price alerts' --dry-run   # check it and show the plan
sv agent run team.yaml 'add price alerts'             # start it
sv agent emit failed '2 tests failed'                 # from an agent: fire tests.failed
sv agent budget                                       # tokens used of the budget
sv agent budget 3M                                    # raise it — resumes what the cap paused
sv agent stop --all
```

Mistakes are reported before anything starts, with the agent and field (`agents[2] (tests): start after:db — no other agent called db`). Members listed in someone's `can_spawn` start when that agent spawns them; an agent may only start the names in its `can_spawn`, or any sub-agent if it has `max_children`. The budget counts tokens, not money — Shellvisor reads them from Claude Code and Codex session logs, and other agents can report theirs with `sv agent usage <tokens>`.

### Agent board (alt+g)

Four views over everything agents are doing (`tab` switches; `sv agent board tree|timeline|log` opens one):

- **Plan** — type a goal (or run `sv agent plan 'add price alerts'`). A planner reads the repository and proposes tasks in stages, with scopes, estimates in tokens and overlap risks. Edit a title (`e`), reassign the agent CLI (`a`), drop a task (`x`), set how many run at once (`+`/`-`) and a token budget (`b`), re-plan (`r`). `⏎` dispatches everything; `s` runs one stage at a time (`⏎` releases the next). Each task gets its own worktree and branch; once all are done a reviewer agent compares the branches and reports.
- **Tree** — who spawned whom, status, tokens and files, totals per branch, and the selected agent's task and latest output. `p` pauses or resumes a branch, `k` stops it, `m` merges an agent's branch into your checkout, `⏎` jumps to its column.
- **Timeline** — every agent over time: working, waiting, approval, paused, done.
- **Log** — spawns, messages, events, refusals and budget stops.

The planner is `claude -p`, `codex exec -` or `gemini`, whichever is installed; set `agents.planner` in config.yaml to use another command (it gets the prompt on stdin and prints JSON).

When agents ask for permission, `alt+a` lists every waiting prompt; `y` / `n` / `a` answers it — Shellvisor types the right key for that CLI. Already running an agent in a normal tab? `sv agent attach` turns that split into an agent column.

## Configuration

`~/.config/shellvisor/config.yaml` — every key is optional:

```yaml
shell: zsh              # zsh, bash or a full path (default: $SHELL)
scrollback_lines: 5000  # lines of history kept per shell
shell_integration: true # report folder / command / exit code to Shellvisor
mouse: true             # wheel scrolls history
start_view: full        # full (tabs, status bar) or basic (just the shell)
restore: ask            # reopen last session's tabs: ask, always or never
low_power: auto         # slower refresh + animations: auto (on battery), on, off
theme: auto             # Shellvisor's own colours: auto (from $COLORFGBG), dark, light — also: sv theme
agents:
  worktree: true        # each new agent works in its own git worktree + branch
  approvals:            # answer agents' permission prompts for you: auto | ask
    test: auto          # read edit test lint install network push delete command
  limits:               # how big agent trees may grow
    max_depth: 3        # sub-agents of sub-agents …
    max_agents: 10      # running at once
    max_children: 3     # sub-agents per agent
  planner: claude -p    # turns a goal into a task plan (alt+g → Plan)
keys:                   # change or turn off any shortcut (F1 lists them all)
  new_tab: alt+n
  rename_tab: none      # frees alt+r for your shell
```

Keys are written like `alt+t`, `ctrl+]`, `f7`, `alt+shift+left`, `shift+pageup`. Shellvisor refuses keys without `ctrl`/`alt` (they would stop you typing) and tells you about clashes.

## Sessions

Shellvisor remembers your tabs, splits, tab names, folders and files/system panes in `~/.local/state/shellvisor/session.yaml` as you work, so it survives closing the terminal window. Next time it asks whether to reopen them. Commands are never re-run.

## How the shell integration works

Shellvisor adds small hooks to your shell *without editing your dotfiles*:

- **zsh** starts with a temporary `ZDOTDIR` whose files source your real `.zshenv`, `.zprofile`, `.zshrc` and `.zlogin`, then add `precmd`/`preexec` hooks.
- **bash** starts with `--rcfile`, which runs your normal login files, then adds a `PROMPT_COMMAND` and a `DEBUG` trap. Works with macOS's bash 3.2 and newer bash. *Note: this replaces an existing `DEBUG` trap.*
- Other shells run fine, just without the hooks.

The hooks print invisible escape sequences (`OSC 7` for the folder, Shellvisor's own `OSC 7770` for the rest) that Shellvisor reads and removes. Command history with exit codes and durations is kept in `~/.local/state/shellvisor/history.jsonl`.

## Development

```sh
cargo test --all                                       # unit + end-to-end tests (real bash/zsh in a PTY)
cargo build --release
cargo run --release --example bench -- --check         # performance budget
```

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — how it is built
- [docs/ROADMAP.md](docs/ROADMAP.md) — phases P0 → P6

## License

Apache License 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

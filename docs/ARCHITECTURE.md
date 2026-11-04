# Shellvisor — Architecture

Status: v0.7 · 2026-11-04 · P0 – P4 implemented (see Decisions log at the end)
Wireframes: `docs/wireframes/` (to be exported) · Phases: [ROADMAP.md](ROADMAP.md)

---

## 1. What Shellvisor is

A full-screen terminal UI that runs **inside the user's existing terminal app** (Terminal.app, iTerm2, Ghostty, WezTerm, Kitty) — like `tmux`, `htop` or `vim` — and splits the window into panes:

- **Shell** — real zsh or bash, many tabs and splits
- **Files** — 8 view styles (tree, columns, list, grid, size map, git, find, recent)
- **System** — CPU, memory, disk, network, processes, thermals
- **Data flow** — the shell's live job and a stage-by-stage pipeline runner
- **Agents** — multiple coding agents (and their sub-agents) side by side, manual or auto-planned
- **Activity** — downloads, installs and long-running commands

Plus `sv`, a set of structured (nushell-style) commands that work inside the shell.

### Goals
1. Every existing shell command, alias, script and full-screen app keeps working unchanged.
2. Tiny footprint: Shellvisor itself ≤ 15 MB RAM idle, < 0.5 % CPU idle, ≤ 8 MB binary (agents' own memory excluded).
3. One binary, no runtime dependencies, macOS (Apple Silicon) first, Linux second.
4. Can be turned off and on at any moment without losing running work (`sv off` / `sv on`).

### Non-goals (for now)
- Being a terminal emulator app (we run *inside* one).
- Replacing zsh/bash with a new shell language.
- Windows support.
- Remote/collaborative sessions (beyond running agents over ssh).

---

## 2. Process model

```
 Terminal.app / iTerm2 / Ghostty            (the user's terminal)
 └── shellvisor                              (ONE process: UI + state, ~10–15 MB)
     ├── PTY ─ zsh   (tab 1)               ┐
     ├── PTY ─ bash  (tab 2, split a)      │ real shells, one PTY each
     ├── PTY ─ zsh   (tab 2, split b)      ┘
     ├── PTY ─ claude (agent "api", hidden tab)   ┐ agent CLIs run in their own
     │        └── PTY ─ claude (sub-agent "ws-push") │ PTYs; their memory is theirs
     ├── PTY ─ codex  (agent "ui")                ┘
     └── short-lived helpers: git, du-walk threads, pipeline stages (only on demand)

 sv <cmd>   ← same binary, invoked by the user or an agent inside any shell
            talks to its parent Shellvisor via (a) OSC escape on the tty or
            (b) a private unix socket — never the network
```

- **No daemon, no background service.** Everything lives as long as the `shellvisor` process. Quitting ends it all (with a warning if commands or agents are still running).
- **Multi-call binary:** `shellvisor` starts the UI; `sv …` (symlink or argv[0] check) runs a CLI subcommand.

---

## 3. Runtime model (threads & events)

Chosen for minimal memory and predictable latency — **std threads + channels, no async runtime**.

| Thread | Job | Wakes on |
|---|---|---|
| **app** (main) | owns all state, handles events, decides redraws, renders with ratatui | channel messages |
| **input** | reads terminal input (crossterm), forwards key/mouse/resize events | stdin |
| **pty-io** | reads shell output (P0: one small blocking reader thread per PTY; a single `polling` reactor for all PTYs once tabs/agents multiply them) | PTY readable |
| **sampler** | system stats + thermals + process list; sleeps when no visible pane needs it | timer (2 s / 5 s low-power) |
| **fs-watch** | `notify` (FSEvents on macOS) for the expanded folders only | file events |
| **workers (≤2)** | on-demand jobs: git status/diff, size scan, find, pipeline relays | job queue; exit when idle |
| **bus** | agent/CLI unix socket: one thread accepting, one short thread per request (`sv agent spawn/wait/tree…`) | socket |

```
 input ─┐
 pty-io ─┼──▶ crossbeam channel ──▶ app thread ──▶ state update ──▶ dirty regions ──▶ render (≤ 60 fps, coalesced)
 sampler ─┤
 fs-watch ┤
 workers ─┤
 bus ─────┘
```

Rules that keep it light (from the performance budget board):
- Redraw **only on change**; frames are coalesced, capped at 60 fps, idle = 0 frames.
- **Hidden panes sleep**: sampler, fs-watch and parsers unsubscribe when nothing visible needs them.
- Scrollback capped (5,000 lines/tab, configurable); agent output kept in 2,000-line ring buffers, full logs streamed to disk.
- File tree loads folders lazily; size map and find only run when asked and are cancellable.

---

## 4. Code layout (single Cargo workspace)

Start as **one binary crate with clear modules** plus one small protocol crate. Split into more crates only if compile times hurt.

```
shellvisor/
├── Cargo.toml                 # workspace
├── crates/
│   ├── shellvisor/             # the binary (UI + everything below as modules)
│   │   └── src/
│   │       ├── main.rs        # argv[0] dispatch: shellvisor | sv
│   │       ├── app/           # App state, event loop, focus, commands, toasts
│   │       ├── config/        # config.yaml load/watch/defaults, keymap, layouts
│   │       ├── ui/            # ratatui widgets, theme, layout tree, editor, status bar
│   │       ├── input/         # key router: global → leader → pane → PTY passthrough
│   │       ├── term/          # PTY spawn, vt100 screen, scrollback, OSC extraction
│   │       ├── shell/         # zsh/bash launch + embedded integration scripts
│   │       ├── tabs/          # tabs, splits, session save/restore
│   │       ├── files/         # 8 views, lazy tree, preview, git (CLI), size, find
│   │       ├── sys/           # sysinfo stats, processes, thermals (macOS IOKit/SMC)
│   │       ├── flow/          # live-job inspector, pipeline runner
│   │       ├── activity/      # download/install detection, progress parsers
│   │       ├── data/          # `sv` tables: types, sources, filters, command-bar parser
│   │       ├── agents/        # registry, tree, limits, approvals, worktrees, adapters
│   │       └── cli/           # `sv` subcommands (on/off/exit, agent, connect, data…)
│   └── sv-proto/              # OSC 7770 + socket message types (serde), shared by app & cli
├── shell/                     # integration scripts (embedded with include_str!)
│   ├── zsh/  zshenv zprofile zshrc zlogin  (installed as dotfiles at runtime)
│   └── bash/ shellvisor.bash
├── docs/                      # ARCHITECTURE.md, ROADMAP.md, decisions/
├── tests/                     # integration tests with real PTYs
└── prototype/                 # the earlier Python/Textual prototype — reference only
```

### Main dependencies (all chosen for size and maturity)

| Need | Crate | Note |
|---|---|---|
| Drawing | `ratatui` + `crossterm` | immediate-mode, diffed output |
| PTYs | `portable-pty` | spawn shells and agent CLIs |
| Terminal emulation | `vt100` | fallback if gaps: `alacritty_terminal` (bigger) |
| System stats | `sysinfo` | default features off; `system`, `disk`, `network`, `component` only |
| File watching | — | mtime check every 3 s while visible + after each command (no watcher threads) |
| Find | `git ls-files` / bounded walk + plain substring scan | respects .gitignore in repos |
| Config | `serde` + `serde_yaml_ng` | maintained fork of serde_yaml |
| Channels | `std::sync::mpsc` (P0), `polling` later | no async runtime |
| Keychain | `security-framework` | API keys for agents |
| Git | **git CLI** on demand | avoids libgit2 size |

Release profile: `opt-level = "z"` or `"s"` (measure), `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

---

## 5. Core building blocks

### 5.1 Terminal pane (`term/`)
- Each shell/agent = **PTY + `vt100::Parser` + scrollback ring**.
- Output path: `pty bytes → OSC extractor → vt100 parser → dirty rows → render`.
- The OSC extractor strips Shellvisor's own sequences (see 5.3) before the emulator sees them and turns them into events.
- Full-screen apps (vim, htop, less) use the alternate screen; Shellvisor just renders whatever vt100 holds.
- Mouse: passed through when the app inside requested mouse mode; otherwise used for focus/selection.

### 5.2 Shell integration (`shell/`) — *already built and tested in the prototype*
- **zsh:** start with `ZDOTDIR=<embedded dir>`; forwarder files source the user's real `.zshenv/.zprofile/.zshrc/.zlogin`, then add `precmd`/`preexec` hooks; `ZDOTDIR` is handed back.
- **bash:** `bash --rcfile shellvisor.bash -i`; emulates login startup (`/etc/profile`, `~/.bash_profile` | `~/.bash_login` | `~/.profile`), then `PROMPT_COMMAND` + `DEBUG` trap. Works with macOS bash 3.2. Caveat: replaces an existing DEBUG trap.
- Other shells run without hooks; cwd falls back to polling the process.

### 5.3 OSC protocol (`sv-proto`)

| Sequence | Sender | Meaning |
|---|---|---|
| `OSC 7 ; file://host/path` | shell hook | current folder |
| `OSC 7770 ; prompt` | shell hook | prompt shown |
| `OSC 7770 ; start` | shell hook | command started |
| `OSC 7770 ; end ; <exit>` | shell hook | command finished + exit code |

The common `OSC 133` marks are deliberately ignored: VTE's `vte.sh` (sourced by Ubuntu's `/etc/bash.bashrc`), kitty, iTerm2 and prompt frameworks print them too, and mixing them with ours made Shellvisor think commands were still running. In bash, `__sv_precmd` runs first and `__sv_ready` last in `PROMPT_COMMAND` (as array entries on bash 5.1+), so other `PROMPT_COMMAND` entries can't trip the DEBUG trap as a "command".
| `OSC 7770 ; cmd ; <base64>` | shell hook | command text |
| `OSC 7770 ; mode ; on\|off\|exit` | `sv on/off/exit` | switch view (planned) |
| `OSC 7770 ; table ; <id>` | `sv` data cmds | "render this table inline" marker (planned) |

These drive: file pane following `cd`, tab states (running / ✔ / ✖ exit code), finish toasts, `sv history`, download detection, and the Flow pane.

### 5.4 Layout tree (`ui/`, `config/`)
- A layout is a tree: `Row | Col` nodes with child sizes (%), leaves are pane kinds (`shell`, `files.tree`, `stats`, `flow`, `thermals`, `agents`, `activity`, `git`, `preview`).
- Presets (Dev, Monitor, Files, Agents, Focus, Basic) + user layouts live in `config.yaml`; the editor (alt+e) edits the same tree live.
- **Basic view** = a layout with a single shell leaf and no chrome; everything else is suspended, not killed.

### 5.5 Input routing (`input/`)
```
key ─▶ global keys (ctrl+q, alt+`, F1, F5…)   — always win, even inside vim
    ─▶ leader key (ctrl+] then letter)
    ─▶ focused Shellvisor pane (files, agents, tables…)
    ─▶ PTY of the focused shell / agent       — everything else
```
- Detects Option-as-Alt misconfiguration on macOS (e.g. `alt+1` arriving as `¡`) and shows the setup helper once.
- All bindings overridable in `config.yaml`.

### 5.6 Structured data — `sv` (`data/`, `cli/`)
- Value model: `Table = Vec<Row>`, `Row = ordered map`, cell types: string, int, float, size, duration, date, bool, list, record.
- Between `sv` stages: **JSON lines** over the pipe (detected because stdout isn't a tty).
- At the end of a pipe (stdout is Shellvisor's tty): emits a table marker → Shellvisor renders an interactive table inline (sort / filter / copy).
- Piped into a normal command: plain aligned text, or `--json` / `--csv`.
- Command bar (alt+;) uses its own parser with nu-style operators (`> < == =~ and or`); in shell pipes the same filters use word operators (`gt lt eq ne has`).

### 5.7 Agents (`agents/`)
```
             ┌──────────── Shellvisor agent runtime ────────────┐
 team.yaml ─▶│ registry ── tree (parent/child) ── limits       │
 Auto-plan ─▶│    │            │                    │          │
 sv agent  ─▶│ spawner ─▶ PTY + adapter ─▶ normalized events ─▶ UI columns / tree / timeline
             │    │         (claude / codex / gemini / generic) │
             │ approvals queue · budget ledger · worktree mgr   │
             │ message bus (unix socket, a JSON line each way)  │
             └──────────────────────────────────────────────────┘
```
- **Agent** = an agent CLI in its own PTY (hidden tab) + an **adapter** that turns its output into events: `Thinking`, `ToolCall`, `FileEdit{path,+,-}`, `Command`, `NeedsApproval`, `Usage{tokens,cost}`, `SpawnRequest`, `Done`. CLIs with structured output modes are parsed; others fall back to a generic text adapter.
- **Sub-agents:** any agent can run `sv agent spawn …` in its shell → bus → runtime checks limits (depth ≤ 3, agents ≤ 10, children ≤ 3 by default) → spawns the child in the parent's folder/worktree with the parent's "always" approvals (`$SHELLVISOR_AGENT` tells the bus who is asking). Over-limit requests are refused with a reason (exit 3 for the caller). `wait` replies when the agent has exited, or has worked and gone idle. Pause/resume/stop signal the agent's process group (SIGSTOP; SIGCONT + `fg`; SIGTERM), `--tree` includes descendants. Agents' *internal* sub-agents (Claude Code's Task helpers, read from its session log) are listed under the agent.
- **Isolation:** one `git worktree` + branch per top-level agent; children share their parent's.
- **Approvals:** policy from team.yaml (auto / ask / never per action class: read, test, lint, install, network, push, delete).
- **Teams:** `team.yaml` is parsed and checked by `sv` (team.rs), sent over the bus as JSON; the app keeps one running team: members start now, after another is done, or when spawned; status changes become events (started, done, failed, exited, needs_approval; `sv agent emit` adds others) that drive `on:` rules and `after:` starts.
- **Auto-plan:** board.rs asks the planner CLI (prompt on stdin) for `{"tasks":[…]}`, stages tasks by deps, and turns the edited plan into a team (one member per task with `after:` deps, plus a review member). `TeamRun.parallel` caps busy members; step mode releases stages one at a time.
- **Board:** an overlay drawn from a snapshot of the agents (status history for the timeline, last screen lines for details); merges run `git merge --no-ff agent/<name>` on a short thread.
- **Budget:** tokens per agent (CLI session logs, or `sv agent usage`), summed per team; pause-all at the cap, resume when raised. No money: prices change and the logs only have tokens.
- **Auto-plan:** an orchestrator agent proposes a staged task graph → user edits → dispatch.
- **Connections:** detects installed agent CLIs and login state, configured MCP servers, ssh hosts; API keys in macOS Keychain, never displayed or logged.

### 5.8 System & thermals (`sys/`)
- `sysinfo` for CPU/mem/disk/net/processes; sampled only while visible (2 s, 5 s in low-power), process threads skipped, everything dropped after 30 s hidden.
- Thermals v1: `sysinfo` components (Apple Silicon sensors via IOKit HID, Linux hwmon) grouped into cpu / gpu / battery / disk / other, hottest of each; "no sensors readable here" rather than a guess. Later: macOS thermal state (`NSProcessInfo.thermalState`) and fans.
- Shellvisor reports its own RSS/CPU ("self") so the budget is always visible.

---

## 6. Files on disk

| Path | Purpose |
|---|---|
| `~/.config/shellvisor/config.yaml` | settings, keymap overrides, layouts, theme |
| `~/.local/state/shellvisor/session.yaml` | last session's tabs/splits/folders (restore) |
| `~/.local/state/shellvisor/history.jsonl` | command history with exit code, duration, tab |
| `~/.local/state/shellvisor/agents/<run>/` | full agent logs, usage ledger |
| `<project>/.shellvisor/team.yaml` | agent team script |
| `~/.local/state/shellvisor/run/bus-<pid>.sock` | agent bus (folder 0700, removed on exit; path in `$SHELLVISOR_SOCK`) |

---

## 7. Security & safety
- No network listener; the bus is a unix socket readable only by the user.
- API keys only in Keychain; redacted from logs and agent output panes.
- Agents work in worktrees; anything destructive outside them is denied by default.
- Destructive UI actions (kill process, delete files, stop agent tree, close busy tab) always confirm.
- Shell integration never changes the user's dotfiles; `sv autostart on` adds a single, clearly marked line and `sv autostart off` removes it.

---

## 8. Performance budget & how it is enforced

| Metric | Target |
|---|---|
| Binary | ≤ 8 MB |
| RSS idle (1 shell, 4 panes) | ≤ 15 MB |
| RSS basic view | ≈ 6–8 MB |
| CPU idle | < 0.5 % (low-power < 0.1 %) |
| Startup | < 100 ms |
| Per extra tab | +2–4 MB |
| Per agent column | ~+1 MB (agent CLI excluded) |

Enforced by: a `bench/` harness that launches Shellvisor in a pseudo-terminal, idles 30 s and records RSS/CPU; CI fails if binary size or idle RSS regress past the budget.

---

## 9. Testing
- **Unit:** layout tree math, keymap routing, OSC extractor, `sv` parser and filters, limits/approvals logic.
- **Snapshot:** ratatui `TestBackend` renders of each pane compared to golden text frames (the wireframes become the spec).
- **Integration:** real zsh and bash (incl. macOS bash 3.2) in PTYs — cd tracking, exit codes, vim/htop passthrough, sv on/off.
- **Agents:** fake agent CLI that replays recorded transcripts, so adapters and tree logic are tested without spending tokens.
- **Budget:** see §8.

---

## 10. Open questions
1. ~~File views for first release~~ → Tree, List, Git, Find in P2; the other four in P6.
2. Leader key: `ctrl+]` (current pick) or `ctrl+space`?
3. ~~YAML crate choice~~ → `serde_yaml_ng` (decided in P0).
4. Thermals on Apple Silicon: which sensors are readable without elevated permissions?
5. Agent adapters: which CLIs expose structured output reliably enough to parse sub-agents and usage?
6. Session restore: offer scrollback restore at all (privacy vs convenience)?

---

## 11. Decisions log

| Date | Decision | Why |
|---|---|---|
| 2026-10-04 | **Input is forwarded byte-for-byte** in basic view; Shellvisor only picks out its own keys (`ctrl+q`, `alt+\``, `shift+PgUp/PgDn`) and, when it owns the mouse, wheel events. A key decoder is added in P1 for pane-local keys. | Zero translation loss: every key, paste, IME and mouse sequence reaches the shell exactly as the real terminal sent it. |
| 2026-10-04 | **Input modes are mirrored** to the real terminal (application cursor/keypad, bracketed paste, mouse mode + encoding). | The real terminal encodes keys and mouse events, so it must be in the mode the program inside asked for. |
| 2026-10-04 | **Shellvisor answers terminal queries itself** (cursor position `CSI 6n`, status `CSI 5n`, device attributes `CSI c` / `CSI > c`); passes bell, window title and OSC 52 clipboard through. | Programs inside talk to Shellvisor's emulator, not the real terminal. |
| 2026-10-04 | **Mouse wheel = scrollback** by default (Shellvisor enables SGR mouse reporting when the program inside doesn't use the mouse). In full-screen apps without mouse support the wheel becomes arrow keys, like real terminals do. Config `mouse: false` turns it off. | On macOS, `shift+PgUp` is taken by Terminal.app/iTerm2, so the wheel is the only reliable scroll gesture. Trade-off: text selection needs Option (iTerm2) or turning off mouse reporting (Terminal.app). |
| 2026-10-04 | **Never query the real terminal's cursor position** (ratatui `Terminal::clear`/`autoresize` are avoided; the screen is cleared with a plain escape and the ratatui terminal is rebuilt on resize). | Saves a round-trip at startup and works with terminals that don't answer. |
| 2026-10-04 | `sv` is linked as `<cache>/bin/sv` and that folder is prepended to `PATH` inside Shellvisor. | `sv` works in every shell tab with no install step. |
| 2026-11-04 | **Renamed ShellView → Shellvisor** (binary `shellvisor`, command stays `sv`, folders `~/.config/shellvisor` etc., env `SHELLVISOR*`). | Distinct, searchable name; "supervisor" for your shells and agents; keeps the `sv` initials. |
| 2026-11-04 | **Full view always captures the mouse** (SGR). Clicks focus tabs/splits; wheel scrolls; programs that asked for the mouse get events re-encoded in *their* protocol (X10/UTF-8/SGR) with pane-local coordinates. | Splits make raw passthrough impossible — coordinates must be translated. |
| 2026-11-04 | **Keys are tokenized** (CSI/SS3/alt+key/UTF-8/mouse/bracketed paste as single tokens); shortcuts match whole tokens; everything else is forwarded byte-for-byte. A paste is never interpreted. | Shortcuts work even when typed fast, without losing fidelity for the shell. |
| 2026-11-04 | **Only the focused pane may set the window title**; bell and OSC 52 clipboard pass through from any pane; a bell in a hidden tab marks the tab `!`. | Background tabs must not hijack the title. |
| 2026-11-04 | **Quit, close tab and close split ask first when a command is still running** (`y`/Enter/ctrl+q = yes). | Protects running work; ctrl+q twice still always quits. |
| 2026-11-04 | Colours use the **256-colour palette**, not 24-bit. | Terminal.app has no true colour. |
| 2026-11-04 | **Session file** `~/.local/state/shellvisor/session.yaml` (tabs, split tree with ratios, folders, names, focus, zoom). Written atomically ≤ 1 s after a change, and on quit/SIGHUP; deleted when the last shell exits. Not overwritten while the "restore?" question is open. | Survives closing the terminal window; never restores half a file; never re-runs commands. |
| 2026-11-04 | **Keys are configured as text** (`alt+t`, `ctrl+]`, `f7`, `alt+shift+left`) and the defaults use the same parser. `none` unbinds; keys without ctrl/alt are refused; clashes and unknown actions are reported. Help (F1) is generated from the live keymap. | One source of truth; lets users free shortcuts their shell needs (e.g. zsh's alt+t / alt+l). |
| 2026-11-04 | **F7 rearranges shell splits** (columns → rows → main + side → grid) instead of switching to presets with placeholder panes. | Placeholder panes are clutter until files/stats/agents panes exist. |
| 2026-11-04 | **Tool panes** (files, system) live in the same split tree as shells (`App.tools` beside `App.panes`, same `PaneId` space). Keys go to the focused pane after global shortcuts; a tool acts on its tab's *paired shell* (the most recently focused one). A tab closes when its last shell exits. Basic view always shows a shell. | One layout model; tools never trap you without a prompt. |
| 2026-11-04 | **Slow work is a `Job`** (git status, file index, content search, diff) run on a short-lived thread; results come back through the main channel. The files pane itself does no I/O beyond reading a folder or a 64 KB preview. Stale results are dropped by folder/sequence number; searches can be cancelled. | UI never blocks; the pane logic stays testable without threads. |
| 2026-11-04 | **Git via the CLI** with `GIT_OPTIONAL_LOCKS=0` (`status --porcelain=v1 -z -b`, `ls-files`, `diff`). | No libgit2 in the binary; never fights the user's git for the index lock. |
| 2026-11-04 | **Activity from the command line, progress from the screen.** A table of programs + sub-commands (git clone, npm i, pip install, uv sync, brew, docker pull, cargo build, curl, wget…) marks a command as a download; the last `NN%` on the pane's bottom lines is its progress (re-read ≤ 4×/s). | Works with every version of every tool, no wrappers or hooks. |
| 2026-11-04 | **`sv` requests**: `OSC 7770;do;<word>[;<arg>]` (`files`, `system`, `layout dev`, `power auto`). | Same channel as `sv on/off`, works over ssh-less local shells with no socket. |
| 2026-11-04 | **`sv` data between stages is JSON lines** (no header line), so `jq` and friends work on it; types survive by convention — ISO-8601 strings become dates, numbers in columns named `size`, `mem`, `*_bytes`… become sizes, `duration`, `*_ms`… durations. At the end of a pipe on a terminal it is an aligned table. | No hidden schema to break other tools; good-enough round trip. |
| 2026-11-04 | **Tables inside Shellvisor are saved** (state/tables, newest 20, JSON lines) and announced with `OSC 7770;table;<base64 path>`; `F4` opens the viewer on them. Viewer sorts/filters a list of row numbers and formats only visible rows. | Scrollback keeps the plain text; the viewer scales to 100k rows. |
| 2026-11-04 | **Command bar runs `shellvisor sv '<pipeline>'` as a child** in the shell's folder and reads its JSON lines. | Same engine as the shell, correct working folder, nothing blocking the UI. |
| 2026-11-04 | **`sv flow` meters with copier threads** between `sh -c` stages: bytes/lines counted per stage, time blocked writing *into* a stage = it is slow (time waiting to read from the first stage counts for it). Stats JSON rewritten 4×/s (`OSC 7770;flow;<path>`); the Flow pane reads it once a second while visible. The live job comes from the PTY's foreground process group + `ps`; idle shells cost nothing. | Works for any commands, no tracing, no root. |
| 2026-11-04 | **Agents are ordinary panes watched from the outside.** The CLI runs in a shell pane in the agents tab (you see and type into the real tool). Status comes from the screen (approval phrases per CLI; quiet ≥ 3 s = waiting), files from `git diff --numstat` against the agent's start commit in its worktree, tokens from the CLI's own session log (Claude Code `~/.claude/projects/<folder>/*.jsonl`, Codex `~/.codex/sessions/**`). Polled every few seconds on a worker thread. | Works with every agent CLI and version, including unknown ones; no fragile output parsing. |
| 2026-11-04 | **One git worktree per agent** at `<repo>/.shellvisor/worktrees/<name>` on branch `agent/<name>`, excluded via `.git/info/exclude`. | Agents never collide in one checkout; merging stays a normal git operation. |
| 2026-11-04 | **Approvals are answered by typing the CLI's own key** (`1`/`2`/esc for Claude Code and Gemini CLI, `y`/`a`/`n` for Codex and Aider, `y`/`n` + Enter for other commands); the prompt text is classified into action classes for `agents.approvals: {class: auto}`. | No hooks into the CLIs; the CLI's own safety prompt stays the source of truth. |
| 2026-11-04 | **Low-power mode**: auto while on battery (checked once a minute — `/sys/class/power_supply` or `pmset -g ps`), or `sv power on/off`. Stats every 5 s, spinner at 4 fps, no folder polling. | Respects laptops without giving up the panes. |

### P0 measurements (Linux x86_64, release build, bash)

| Metric | Budget | Measured |
|---|---|---|
| Binary | ≤ 5 MB (P0) | 0.76 MB |
| First frame | ≤ 100 ms | 4 ms |
| Memory idle | ≤ 8 MB (P0) | 3.5 MB |
| CPU idle | ≤ 0.5 % | 0.00 % |

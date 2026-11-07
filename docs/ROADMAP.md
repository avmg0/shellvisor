# Shellvisor — Roadmap & Phases

Status: v1.0 · 2026-11-04 · **P0 – P6 done** · released as v1.0.0 · see [ARCHITECTURE.md](ARCHITECTURE.md)

Each phase ends with something you can **run and use daily**, and must stay inside the performance budget before the next phase starts. Sizes are relative (S ≈ a few days, M ≈ 1–2 weeks, L ≈ 2–4 weeks of focused work) — not promises.

| Phase | Name | Size | You can use it for… | Boards |
|---|---|---|---|---|
| **P0** ✅ | Foundations | M | Shellvisor as a plain, fast shell wrapper (basic view) | 11, 14 |
| **P1** ✅ | Workspace | L | tabs, splits, layouts, keymap — a tmux-like daily driver | 3, 4, 4b, 12, 14 |
| **P2** ✅ | Core panes | L | the default 4-pane workspace (files, stats, activity) | 1, 6, 9, 10 |
| **P3** ✅ | Data | M | data-flow view and `sv` structured commands | 2, 13 |
| **P4** ✅ | Agents I | L | run several coding agents side by side, manually | 5, 8, 16 |
| **P5** ✅ | Agents II | L | sub-agents, team.yaml, auto-plan | 7, 15, 16 |
| **P6** ✅ | Polish & ship | M | the rest of the file views, layout editor, packaging | 4, 9, 10, 11 |

---

## P0 · Foundations — ✅ done 2026-10-04
**Goal:** one Rust binary that runs your real zsh/bash full-screen with zero visible difference — the "basic view".

Deliverables
- Cargo workspace, CI (macOS arm64 + Linux), release profile tuned for size.
- Config loading (`config.yaml`) with defaults; pick the YAML crate.
- Event loop: input thread, PTY reactor, app thread, coalesced redraw.
- Terminal pane: PTY + vt100 + scrollback; resize; mouse passthrough; paste.
- Port the existing zsh/bash integration scripts (embedded) and the OSC extractor.
- Multi-call binary: `shellvisor` and `sv` (`sv status`, `sv exit`).
- Budget harness (`bench/`) + CI size/RSS checks.

Exit criteria
- vim, htop, less, ssh, fzf, git all behave exactly as in the bare terminal.
- cwd / command / exit code events fire for zsh, bash 5 and macOS bash 3.2.
- Binary ≤ 5 MB, RSS ≤ 8 MB idle, CPU ≈ 0 % idle, startup < 100 ms.

Risks: vt100 gaps with exotic apps → keep `alacritty_terminal` as plan B.

**Result:** all exit criteria met on Linux (bash 5.2, zsh 5.9, vim): 0.76 MB binary, 4 ms first frame, 3.5 MB RSS, 0 % idle CPU. 26 unit + 10 end-to-end PTY tests. macOS (bash 3.2, Terminal.app/iTerm2) is covered by CI and needs a hands-on check.

---

## P1 · Workspace — ✅ done 2026-11-04
**Goal:** the frame everything else plugs into.

**P1a done:** layout tree, tabs, splits (side by side / stacked), focus/resize/zoom, tab states (running spinner, ✖ exit, • new output, ! bell), rename, close-with-running confirmation, keymap router (global → leader `ctrl+]` → pane → PTY), F1 help, status bar (folder, running command + time, last exit, own memory, clock), Option-as-Alt detection hint, `sv on`/`sv off`/alt+` full ⇄ basic, mouse routing (click to focus tab/split, wheel scroll, apps that use the mouse get pane-local coordinates in their own protocol). Measured: 0.85 MB binary, 4.4 MB RSS with 3 tabs, 0 % idle CPU. 36 unit + 18 end-to-end tests.

**P1b done:** session save/restore (`session.yaml`, saved within 1 s of any change and on exit/SIGHUP; ask/always/never; commands never re-run), `F7`/`alt+l` split arrangements (columns, rows, main + side, grid), keymap overrides in config.yaml (text key specs, `none` to unbind, clash and typo warnings, F1 help built from the live keymap), tab menu (`alt+m`). Measured: 0.99 MB binary, 4.5 MB RSS with 3 tabs, 0 % idle CPU. 47 unit + 23 end-to-end tests.

*Change from plan:* the Dev/Monitor/Files/Agents presets with placeholder panes were dropped from P1 — empty panes are clutter. Those presets arrive with their panes (P2+); in P1, `F7` rearranges shell splits instead.

Deliverables
- Layout tree (row/col with sizes), pane framework (focus, borders, titles, zoom F5).
- Tabs (alt+t/F10, alt+1…9, rename, close-with-running dialog) and splits (alt+\ / alt+-).
- Tab states from shell events (running, ✔, ✖ exit, new output, waiting for input, bell).
- Keymap router: global → leader (ctrl+]) → pane → PTY; config overrides; F1 help.
- Mac key setup helper (Option-as-Alt detection).
- `sv on` / `sv off` / `sv exit` / alt+` with full ⇄ basic view.
- Status bar + toasts; session save/restore.
- Presets: Dev (empty pane placeholders), Focus, Basic.

Exit criteria: usable as a daily tmux-replacement; idle RSS ≤ 10 MB with 3 tabs.

---

## P2 · Core panes — ✅ done 2026-11-04
**Goal:** board 1 working for real.

**Result:** files pane (Tree, List, Git changes and Find views; lazy folders re-read only when their mtime changes; git badges from `git status`; filter, hidden toggle, preview with light syntax colours and git diffs; follows the shell's folder; insert path / cd / open in `$EDITOR` at the matching line), system pane (CPU history + per core, memory, swap, disks, network, disk I/O, load, temperatures grouped cpu/gpu/battery/disk, own usage, top processes sorted by CPU or memory; sampled only while visible, freed after 30 s hidden), activity (downloads and installs recognised from the command line, progress read from the pane's last lines, ◐ loading circle in pane title, tab and status bar, finish/fail toast), presets `dev` / `monitor` / `files` (`sv layout`, tab menu), `F2`/`F3` and `sv files`/`sv sys`, low-power mode (manual, or auto on battery: 5 s sampling, slower animations, no folder polling), tool panes saved in the session. Measured with 3 tabs + dev layout: 1.31 MB binary, 5.2 MB RSS, 0.2 % idle CPU. 70 unit + 28 end-to-end tests.

*Changes from plan:* agent badges in the file tree move to P4 (no agents yet). Thermals v1 reads the sensors `sysinfo` exposes (Apple Silicon via IOKit HID, Linux hwmon); macOS thermal *state* and fans are left for P6. No `notify`/`ignore` crates: folders are re-checked by mtime every 3 s while visible and after each command, and Find uses `git ls-files` (respects .gitignore) or a bounded walk — smaller binary, no watcher threads.

Deliverables
- **Files:** lazy tree with git + agent badges, filter, hidden toggle; List view; Git changes view; Find (names + contents); preview with light syntax colors; follows shell cwd; insert path / cd / open in $EDITOR.
- **System:** CPU (sparkline, per-core), memory, swap, disk, network, I/O, load, process list (visible-only sampling), "self" usage; **thermals v1** (thermal state + whatever sensors are readable).
- **Activity:** detect git clone / npm / pip / uv / brew / docker / cargo / curl from command text; progress parsing; loading circle in header, tab and pane; finish toasts.
- Presets: Dev, Monitor, Files.
- Low-power mode (manual + auto on battery).

Exit criteria: default workspace runs all day at ≤ 15 MB, < 0.5 % CPU idle.

---

## P3 · Data — ✅ done 2026-11-04
**Goal:** see and shape data flowing through commands.

**Result:** `sv` data commands — a value model with sizes, durations and dates; sources `ls ps host df ports env history open git-log git-status`; filters `where select reject sort-by reverse first last skip uniq get group-by count sum avg min max columns describe`; `from json|jsonl|csv|tsv|yaml|toml|ssv|lines`, `to json|jsonl|csv|tsv|md|yaml|text`; stage by stage (`sv ls | sv where …`, JSON lines between stages) or as one quoted pipeline. Table viewer (`F4`): sort, filter by text or condition, copy CSV/JSON/Markdown (pbcopy / wl-copy / xclip, else OSC 52), insert a cell; 100k rows tested. Command bar (`alt+;`) with nu-style operators, history, `alt+⏎` to the prompt. Flow pane (`F8`, `sv layout data`): the live foreground job (processes, CPU, memory; read/write rates and pipe links on Linux) and `sv flow '…'` meters per stage (bytes, lines, kept %, rate, slowest stage, stderr, output tail), "send to shell" / "run again". Measured with 3 tabs + dev layout: 1.76 MB binary, 5.6 MB RSS, 0.2 % idle CPU. 100 unit + 32 end-to-end tests.

*Changes from plan:* `sys` became `host` (`sv sys` already opens the system pane). The table viewer is an overlay opened with `F4` rather than drawn inline in the scrollback — the text table stays in the scrollback, the viewer works on the saved rows. Per-stage rates in the *live job* view need `/proc`, so on macOS that view shows CPU and memory only; `sv flow` meters work everywhere. The board 13 examples were not re-checked against the design canvas (set aside); the README examples are covered by tests instead.

Deliverables
- **Flow pane:** live foreground job (processes, cpu, rss, pipes); pipeline runner with per-stage bytes/lines/rate, filter ratio, bottleneck, per-stage stderr, output tail; "send to shell".
- **`sv` structured commands:** value model, JSON-lines between stages, plain-text/`--json`/`--csv` output; sources (ls, ps, sys, df, ports, env, history, open json/csv/toml/yaml, git-log, git-status); filters (where, select, reject, first, last, skip, uniq, get, sort-by, reverse, group-by, count, sum, avg, min, max, from, to).
- Inline interactive tables (sort, filter, copy as csv/json/md).
- Command bar (alt+;) with nu-style parser; sv pipelines shown in Flow with rows per stage.

Exit criteria: board 13 examples all work in zsh and bash; tables of 100k rows stay responsive.

---

## P4 · Agents I — side by side — ✅ done 2026-11-04
**Goal:** run several coding agents manually and watch them live.

**Result:** agents tab (`F9`) with one column per agent; New agent form (`shift+F9`) and `sv agent new [claude|codex|gemini|aider] [--name] ['task']` / `--cmd '<any command>'`; `sv agent attach` for an agent already running in a split. Each agent in its own git worktree + branch (`.shellvisor/worktrees/<name>`, `agent/<name>`, hidden from your git status). Column titles: status from the screen (starting, working, waiting for you, ⚑ needs approval, done/exited), files changed with +/− from git, tokens from Claude Code / Codex session logs, minutes running. Approvals list (`alt+a`) answers any agent's prompt with the key its CLI expects (allow / deny / always); `agents.approvals` in config.yaml auto-answers chosen action classes (read, edit, test, lint, install, network, push, delete, command). File-overlap warnings between agents; ◆ badges in the files pane; `sv agents` and `sv connect` (CLIs installed and logged in, MCP servers from Claude/Codex/Gemini configs, ssh hosts — presence only, never secrets). Measured: 1.91 MB binary; idle 5.9 MB RSS, 0.13 % CPU with 3 tabs + dev layout; three agents printing 10 lines/s each add 1.4 MB to Shellvisor (budget 3 MB) at 1.2 % CPU. 109 unit + 33 end-to-end tests (a scripted fake agent covers worktree, approval, messages, badges, tables).

*Changes from plan:* the agents view is `F9` (F8 became Flow in P3). Agents run in ordinary panes rather than hidden PTYs, so you see and type into the real CLI; status comes from the screen and git rather than per-CLI event parsers, which keeps every CLI working, including ones we don't know. Cost in money is not shown — prices change and the logs only have tokens. Keychain lookups were not needed: Claude Code's sign-in shows in `~/.claude.json`; other logins are files or environment variables (presence only).

Deliverables
- Agent runtime: spawn CLI in hidden PTY, adapters (Claude Code, Codex, Gemini CLI, Aider, generic), normalized events.
- Agents view (F8): columns with live output, status, tokens/cost, files touched; message an agent; Manual "New agent" dialog; attach an agent already running in a tab.
- Git worktree per agent; file-overlap warnings; file tree badges.
- Approvals queue (allow / deny / always) with per-action policy.
- Connections (`sv connect`): detect CLIs + login, MCP servers, Keychain keys, ssh hosts.
- Agents preset layout.

Exit criteria: 3 agents on one repo for an hour without UI lag; Shellvisor's own overhead ≤ +3 MB.

---

## P5 · Agents II — teams & sub-agents — ✅ done 2026-11-04
**Goal:** orchestration.

Deliverables
- Message bus (unix socket); `sv agent` CLI (run, spawn, send, wait, tree, logs -f, pause/resume, stop --tree, new).
- Agent tree with limits (depth/agents/children), inheritance, budget roll-up, branch actions; detection of CLIs' internal sub-agents where visible.
- `team.yaml` (team, agents, on-event rules) with validation and clear errors.
- Auto-plan: goal → staged task graph → edit/reassign/drop → dispatch / step-by-step / re-plan; orchestrator review before merge.
- Timeline view; message-bus log.

Exit criteria: the board 16 team.yaml runs end-to-end; over-limit spawns are refused with a reason; budget cap pauses everything.

**P5a done:** agent bus (unix socket in the state folder, 0700, `$SHELLVISOR_SOCK`; agents get `$SHELLVISOR_AGENT` so their requests say who asks). `sv agent spawn | send | wait | tree [--json] | logs [-f] | pause | resume | stop [--tree] | log`. Sub-agents share the parent's folder/worktree and its "always" approvals; limits (`agents.limits`: max_depth 3, max_agents 10, max_children 3) refuse with a reason (exit 3). Pause = SIGSTOP of the agent's job (resumed with SIGCONT + `fg`, since the shell takes the terminal back). Claude Code's own Task helpers appear as `+N helpers`. Column titles show `name ← parent` and ⏸ paused. 112 unit + 34 end-to-end tests (a fake lead agent spawns a helper, is refused a second, waits; tree, send, log, pause/resume, stop --tree from the shell).

**P5b done:** `team.yaml` — team (name, budget_tokens, max_agents, max_depth), agents (id, cli or cmd, role orchestrator/worker, task, can_spawn, worktree, max_children, approve, mcp, start now / after:<id> / spawned), `on:` rules (`<agent>.<event>` → send text with {log} {agent} {event} {goal}). Checked in `sv` before anything starts, with the agent and field in every error; `budget_usd` is refused with the reason (Shellvisor sees tokens, not prices); missing CLIs and unconfigured MCP servers are noted. `sv agent run [file] "goal" [--dry-run]` · `--init` starter file · `emit` · `usage` · `budget [tokens]` · `pause|resume|stop --all`. The orchestrator gets the goal plus who it can start; members keep their team settings when spawned; events started / done / failed / exited / needs_approval come from status changes. Over budget → every team agent pauses; raising the budget resumes them. Command agents get their task in `$SHELLVISOR_TASK`. 114 unit + 35 end-to-end tests (the board 16 team with scripted agents: validation error, dry run, lead spawns api and is refused db, tests starts after api, tests.failed reaches api, the budget pauses all, raising it resumes, stop --all ends the team).

**P5c done — P5 complete:** agent board (`alt+g`, leader `e`, `sv agent board`, `sv agent plan "goal"`). Plan: the planner CLI (claude -p / codex exec - / gemini, or `agents.planner`) gets the goal on stdin and returns a JSON task graph; tasks are staged by their deps, cycles refused, scopes checked for overlap; edit / reassign / drop, parallel limit, token budget, dispatch all or step by step, re-plan. Dispatch turns the plan into a team (one worktree + branch per task, `after:` deps, a review agent after everything) and the team scheduler keeps at most N busy. Tree: branches with tokens / files totals, details and live output of the selected agent, pause / stop / merge a branch. Timeline from each agent's status history. Log view of the bus. `team.yaml` `start: after:a,b` takes several agents. Measured: 2.34 MB binary. 117 unit + 36 end-to-end tests (a scripted planner and workers: plan, edit, dispatch with 2 stages, review, tree / timeline / log, merge into main).

*Changes from plan:* estimates and budgets are tokens, never dollars. The orchestrator's review runs as an agent that reports; merging stays your call (Tree, `m`).

---

## P6 · Polish & ship — ✅ done 2026-11-04
**P6a done:** the rest of the file views — Columns (parent folder | this folder | preview), Grid (names in cells, arrows in two directions), Size (folders and files by total size as bars, drill in and out, stops counting past 400k files and says so) and Recent (newest changed files anywhere below, from git's file list or a bounded walk). `1`–`8` / tab switch views; the view bar shortens to "view n/8" when narrow. 118 unit + 36 end-to-end tests.

**P6b done:** thermal view in the system pane (`t`): every sensor with its critical point, per-group history, CPU clock vs. top clock (Linux cpufreq) or macOS's `CPU_Speed_Limit` (read every 30 s, only in this view), and a one-time heat warning (≥ 95 °C, at a sensor's critical point, or slowed by heat) at most every 10 min. Themes: `theme: auto|dark|light` and `sv theme` — light remaps Shellvisor's own palette only (never your programs' colours inside shells); auto reads `$COLORFGBG`. Contrast: faint text raised from 240 to 243 (≈ 4.6:1 on a dark background); every status keeps a symbol as well as a colour (⠋ ✔ ✖ ⚑ ⏸ ▲). 120 unit + 36 end-to-end tests.

**P6c done:** layout editor (`alt+e`, leader `E`): reshape the tab live — palette (shell, files, system, flow) with `1`–`4`/tab, split right `\` / below `-`, close `x`, resize `< > J K`, move with the arrows; save by name (`s`), as the default for new sessions (`d`) or for this folder and below (`f`); open a saved one (`o`, or `sv layout <name>`; `sv layout` lists them). Layouts are YAML in `~/.config/shellvisor/layouts/` (no folders inside, so they open wherever you are); default and per-folder choices in `~/.local/state/shellvisor/layouts.yaml`, applied on a fresh start. Drag the border between any two splits with the mouse to resize. Long notices are cut to fit instead of not showing. 123 unit + 37 end-to-end tests.

*Changes from plan:* the editor works on the live tab (what you see is the layout) rather than a separate mini-map.

**P6d done — P6 complete:** packaging and start-up. Release workflow on `v*` tags: macOS arm64/x86_64 and Linux x86_64/arm64 archives, SHA256SUMS, signed build-provenance attestations (Sigstore), optional macOS codesign + notarization when the Apple secrets exist, GitHub release with notes. Homebrew formula template (`packaging/homebrew`, filled by `scripts/brew-formula.sh` for the `avmg0/homebrew-tap` tap), a checksum-verifying `scripts/install.sh`, and `cargo install --git`. `sv autostart on|off|status` adds/removes a marked block in `~/.zshrc` or `~/.bashrc` (+ `~/.bash_profile`) that starts Shellvisor only in an interactive terminal outside Shellvisor, tmux, screen, SSH and editor terminals, never `exec` (quitting leaves your shell). docs/RELEASING.md. 126 unit + 37 end-to-end tests.

*Not done here:* crates.io (the patched vt100 is a path dependency); the tap repository itself and the Apple signing secrets need your accounts.

Deliverables
- Remaining file views: Columns, Grid, Size map, Recent activity.
- Layout editor (alt+e) with palette, drag-resize, save/default/per-folder layouts.
- Full thermal view (history, throttling alerts, per-sensor list).
- Themes (dark/light), accessibility pass (contrast, no color-only signals).
- Packaging: Homebrew tap, `cargo install`, signed release binaries; `sv autostart`.
- User docs and `sv help`.

Exit criteria: v1.0 — all boards implemented, budget met, install in one command.

---

## v1.0.1 · planned
**Goal:** small additions and release fixes after 1.0.

Deliverables
- `sv resize left|right|up|down [steps]` (done) — move the focused pane's border from the command line (same as `alt+shift+arrows`, which need Option-as-Meta on macOS); `sv resize 30%` sets the focused pane's share of its split directly.
- Release workflow (done): build both macOS targets (arm64 and x86_64) on one Mac runner, so a release needs a single Mac machine (GitHub's Mac runners were short on capacity for 1.0.0).
- Update GitHub Actions to Node 24 versions (done): checkout v5, upload-artifact v6, download-artifact v7, attest-build-provenance v3, rust-cache v2.9.0.

- Layout editor as designed on board 4 (done): its own screen with preset thumbnails (built-in and saved, `1`–`9` or click), a palette (shell, files, git changes, system stats, thermals, data flow; `[ ]` or click), a map of the tab (select, split, change kind, remove, resize in 5 % steps with keys or by dragging, undo), and a save form (name, default, this folder) with the YAML that will be written. Edits apply on ⏎ only; esc cancels; running panes are kept, and a removed pane that is still running a command blocks the apply with a message.

- Settings screen (done): `alt+s` / `sv settings` — colour (and bold) of the folder in the shell prompt, applied by the hooks to zsh `%1~ %~ %c %. %/ %d` and bash `\W \w` in any prompt; colour of the commands you type (default blue; colour choices are the terminal's own 16 colours plus orange, so they match a prompt in the same colours and follow the profile; zsh `zle_highlight`, bash a colour at the end of PS1 reset before the command runs); colour of folders in lists (files pane, `ls`/`sv` tables); `ls` as a table; table borders; theme. Saved in state/settings.yaml; hooks read state/ui.env with the shell's own `read` at each prompt.
- `ls` as a nushell-style table (done): the hooks define `ls` to run `sv ls` at the terminal (cosmetic flags -a -A -G -F -h --color allowed); other flags, pipes and the setting off use the real ls. `sv` tables get rounded borders, green header and index, folders in the folder colour.

- Design palette (done): in full view, Shellvisor's frames, bars, panes and the shells' default background use the boards' colours (page #0b0e12, panels #11151b, shells #0e1217, bars #151a21, borders #262c36, text #d6dbe3, accents amber #e0a84a, cyan #6cc9d6, green #8fd18f, red #f07178). 24-bit where the terminal supports it, nearest 256-colour otherwise. Setting *Dark background: design / terminal*.

- Downloads & installs, board 6 (done): a filling progress bar (eighth-block precision; a sliding block when there's no %) beside the spinner in the tab, pane title and status bar; speed and time left read from what tools print (git, curl's meter, wget, pip/rich, tqdm, "eta"/"left") or worked out from the progress, restarting per phase; activity list (`ctrl+] j`, `sv activity`, click the status bar) with running and the last 20 finished; the target of a download (clone folder, node_modules, target/, .venv, curl -o/-O, wget -O/-P, hf --local-dir; `cd x &&` followed) gets the spinner in the files pane; finish toast offers `ctrl+] c` cd into it / `ctrl+] o` open in files for 12 s; agents' installs from their screen (`Bash(…)`, `$ …`, `Running …`) on their column; settings for spinner style (circle, dots, arc, line, pulse, bounce) and bar on/off.

- Downloads restyled to board 6 (done): the ring spinner (◴◷◶◵; braille and bar styles), thin line bars (━ on a dark track, a sliding cyan piece without a %), the header badge with count / average % / total speed, `job: …` in the status bar, activity list as cards (command, source host, bar in the tool's colour, the tool's progress line, amount / speed / ETA), two-line finish toast, ✔ / ✖ on the tab for 5 s.
- Flow pane as on board 2 (done) when it has room (≥ 90 × 26, cards ≥ 18 wide): the command box with state and keys, a card per stage with status, in / out / rate / kept, sparkline and latest line (new `sample` in the stats file), arrows with kept %, a throughput chart per lane over the last 30 s, the slowest stage and why, output and tagged stderr (repeats counted); the table stays for small panes.
- Colours (done): Terminal.app on macOS 26+ counts as 24-bit; a "Colours" setting (auto / 24-bit / 256); in 256 colours the page, panel and shell backgrounds are left to the terminal (256 colours have no dark blue); `sv terminal-colors [undo]` sets Terminal.app's profile to the design's background and text.

- Mouse selection and right-click menu (done): drag selects within a pane and copies on release (pbcopy / wl-copy / xclip, else OSC 52), double-click a word, triple-click a line, shift when the program uses the mouse; right-click menu Copy / Paste (bracketed when asked) / Select all / Open link / split right / split below. Shellvisor now asks the terminal for drags (1002) so the selection follows the mouse. Nothing the terminal does is taken away: fn-drag (Terminal.app), Option-drag (iTerm2) and `mouse: false` still work.

- Mouse setting (done): *shellvisor* (default) or *terminal*, which gives the terminal its own selection, right-click menu and ⌘-click back.

- Terminal features per pane (done): find in scrollback with highlights (`ctrl+] /`), clear pane + scrollback (`ctrl+] k`; ED 3 now clears scrollback), option-click moves the cursor, ctrl-click opens links, hints on the border when stuck (unclosed quote, unfinished command, vim, less, nano, top, REPLs, ssh).
- PuTTY-style (done): saved sessions (`sv ssh save|rm|NAME`, picker `ctrl+] S`, ~/.ssh/config hosts, keepalive), reconnect a dropped ssh (`ctrl+] R`), duplicate session, session logging to plain text (`sv log start|stop`), right-click pastes (setting) and middle-click paste.

- Command palette, broadcast typing, serial consoles, remote files, failed command card (done): `ctrl+] space` fuzzy-searches actions, settings, sessions, layouts, tabs and serial ports; `ctrl+] B` broadcasts keys to every shell in a tab; `sv serial [dev] [baud]` is a built-in serial console (termios raw, `~.` quits); `ctrl+] F` opens a remote files pane over the tab's ssh (batch-mode ssh with ControlMaster: list, download `d`, drop to upload); a card over the pane for failed commands with copy (`ctrl+] c`) and run again (`ctrl+] r`). The card is drawn only over the pane the command ran in.

- MCP servers, boards 17 and 18 (done): `ctrl+] M` / `sv mcp`. Board 17: every MCP server your agents can use, read from Claude Code (`~/.claude.json`, per-project), `.mcp.json`, Gemini (`~/.gemini/settings.json`) and Codex (`~/.codex/config.toml`), merged by name; running / stopped / failing from a real handshake (stdio or streamable HTTP, protocol 2025-06-18); a detail box with command, transport, where it is configured, secrets (set or missing, values never shown), who uses it and health; tools, resources, prompts; *Try a tool* with fields from the tool's input schema and a coloured JSON result with size and token cost. Board 18: live traffic read from the agents' Claude Code logs (args, result size, ms, ✔ / ✖, approvals waiting with y / n / a), calls per minute, context sent back by server, a permissions grid (allow / ask / never per server or tool, per agent, written to each agent's `.claude/settings.local.json`), and *Add a server* (ready-made templates, previews of the JSON and TOML that will be written, the handshake runs first and nothing is written if it fails). Both screens scale down to 100 × 24.

- Improvements to everything above (done): MCP — ⚠ when a tool's description changes (`d` shows old and new), tool-list cost in tokens, traffic from Codex and Gemini CLI logs too; find — regular expressions (`ctrl+r`) and `n` / `N`; session logs with a time on every line, `sv log list` / `sv log view`; `sv ssh all` and marking several sessions in the picker (a split each, broadcast on); palette ranks by use and lists recent commands; failed-command card `ctrl+] ?` asks an agent why; activity list `p` pause / `x` cancel, downloads remembered across sessions, `r` runs one again; `sv flow save | list | rm` and `sv flow NAME`; a first-run tour (`sv tour`); snapshot tests for boards 17 and 18 at 160 × 44 and 100 × 24.
- Boards 19–21 (done): an MCP call in full with the server's process, controls and log (`⏎` on a traffic row, `l` on a server); ssh port forwards per session (`ctrl+] P`, `sv ssh forward`: own `ssh -N` per forward plus a counting relay, busy ports explained and moved, saved forwards open with the session); agent review before merging (`sv agent diff NAME`, `ctrl+] D`, `d` on the board: files, commits, checks, coloured diff in three views, merge / squash / message / open / edit / throw away, approve for a while). Snapshot tests cover boards 17–21 at 160 × 44 and 100 × 24.

Exit criteria: `sv resize` covered by an end-to-end test; a `v1.0.1` tag releases with one macOS job; CI and Release runs show no deprecation warnings.

---

## v1.0.4 · board-matching pass — ✅ done 2026-11-07
**Goal:** every built screen looks like its board, adding what the boards show and taking nothing away.

Deliverables
- Window frame, boards 1, 3, 12 (done): two-row header (folder, shell, pid · downloads, agents, approvals, live, CPU, MEM, clock), tab strip in the boards' chip style, a key strip that follows the focused pane, `F2 · FILES` style pane titles, the shell's size in its title; `sv layout focus` with a side rail and a status strip.
- System and Flow panes, board 1 (done): CPU history chart, a cell per core, Mem / Swap / Disk / Temp gauges, Therm row with history, Net, I/O, PID / NAME / CPU% / MEM%; Live job / sv flow tabs, a card per process, the pipeline note.
- File views, boards 9 and 10 (done): 1–8 in the boards' order; List columns, sort, marks, copy and move; Grid cards; Size treemap and move-to-Trash; Git groups with the diff beside; Find and Git previews when wide; Recent who and what; PREVIEW box under the tree in the dev layout.
- Tabs, board 12 (done): `?`, download %, single-agent names, pin, mute, background tabs; the tab menu's grid; Stop & close / Keep in background / Cancel; Restore with a tick per tab.
- Agents, boards 5 and 8 (done): facts inside each column, orchestrator column, message bus, tell / pause / resume all; the New agent form with CLI cards, folder, shell, approvals, stop-after and Attach as column.
- Option-as-Alt dialog with Test again (board 4b) and the fading basic-view hint (board 14) (done).

- Agent tree, board 15 (done): ← / → fold and open a branch, `c` promotes a sub-agent to its own column, `+` spawns a child of the selected agent, `i` messages it; the details show spawned by · via · worktree, the instruction and its output.
- Layout editor drop zones, board 4 (done): drag a palette item onto a pane; a dashed zone shows where it lands. The editor's mouse (presets, panes, borders) now works from the terminal, not only in tests.

- The rest of the boards (done): sync scroll across agent columns (`ctrl+] Y`); an approval policy per dispatch in Plan (`p`: auto · ask · full auto, with `deny:` in team.yaml); Tree indent guides; Columns type-to-jump; Grid card sizes (`+` / `−`); Recent `u` undoes an agent's edit (asks, keeps a copy in the Trash); Preview, Agents and Activity as pane kinds (`sv pane …`, the layout editor); plain `c` / `o` on the finish toast at an empty prompt. A second tool pane now stacks under the first instead of narrowing the shell.


## v1.0.8 · after the boards — ✅ done 2026-11-07
- `sv doctor` (done): versions running and on PATH (old copies named), 24-bit colour of the terminal around Shellvisor, UTF-8, shell hooks, mouse, autostart, F-keys on a Mac, and an Option-as-Alt key test; each with its fix.
- One-command release (done): `scripts/release.sh X` sets the version, commits, pushes, tags, waits for the Release workflow, updates the Homebrew tap and upgrades.
- Desktop notifications (done): long commands (≥ 10 s) and agents (finished, failed, needs approval) when you aren't looking — focus reports from the terminal, or the split off screen; Notification Center / notify-send; a setting; muted tabs excluded.
- Scrollback restore (done, off by default): each shell's last 5000 lines saved every 2 min and on quit (state/scrollback, 0700) and shown again on restore under a marker line.

## v1.0.9 · try it, install it, see it
- `sv selftest` (done): starts your shells as Shellvisor does (with the hooks and your own rc files, macOS's bash 3.2 too) and checks prompt time, `cd`, exit codes, the command text and running / finished.
- Linux packages (done): a `.deb` for amd64 and arm64 on every release (with a checksum and build provenance), and an AUR `shellvisor-bin` PKGBUILD generated by the release script.
- README pictures (done): a short GIF and two screenshots, made from a demo account (`john@mbp`) with made-up project data.
- Still to do by hand: a pass in Terminal.app, iTerm2 and Ghostty with your own zsh setup (checklist in RELEASING.md).

---

## Cross-cutting, every phase
- Budget checks in CI (size, idle RSS, idle CPU).
- Snapshot tests for every new pane against the wireframes.
- Real-shell integration tests (zsh, bash 5, macOS bash 3.2).
- Update ARCHITECTURE.md decisions log when anything changes.

## Before P0 starts
- [x] Designs signed off.
- [x] YAML crate chosen (`serde_yaml_ng`). Still open: first file views, leader key.
- [x] Python prototype moved into `prototype/` (reference only).

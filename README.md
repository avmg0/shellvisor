# Shellvisor

**Your real shell, plus a workspace around it — inside the terminal you already use.**

Shellvisor is a full-screen terminal app (like `tmux`, `htop` or `vim`) that runs your own zsh or bash in tabs and splits, and — as it grows — adds panes for files, live system stats, a data-flow view of your pipelines, and coding agents working side by side. Every command, alias and full-screen program you use today keeps working unchanged.

> **Status: early development (P0–P2 of 7 done).**
> Working today: tabs and splits of real zsh/bash shells, a tab bar that shows what each tab is doing (running ⠋, downloading ◐, failed ✖, new output •), a status bar, split layouts, your tabs restored after a restart, custom keys, and a one-key switch to *basic view*, which looks exactly like a plain terminal. Plus a **files pane** (tree, list, git changes, find, preview) that follows your shell, a **system pane** (CPU, memory, disk, network, I/O, temperatures, processes), and download/install progress for git, npm, pip, uv, brew, docker, cargo, curl and friends. Data flow and agents arrive in P3–P5. See [docs/ROADMAP.md](docs/ROADMAP.md).

## Why

- **One binary, tiny footprint.** Written in Rust. Budget: ≤ 8 MB binary, ≤ 15 MB memory, < 0.5 % CPU when idle. The current build is ~1.3 MB and uses ~5.2 MB of memory with three tabs plus the files and system panes open.
- **Nothing to relearn.** Your `.zshrc` / `.bash_profile` load as usual; vim, less, fzf, ssh and git all behave as before.
- **Off is one command away.** `sv off` hides the panels, `sv exit` leaves completely.

## Install (from source)

Requires Rust 1.85+.

```sh
git clone https://github.com/avmg0/shellvisor
cd shellvisor
cargo install --path crates/shellvisor
```

This installs a single `shellvisor` binary. Inside Shellvisor, `sv` is available automatically. To use `sv on` from a plain terminal, link it once:

```sh
ln -s "$(command -v shellvisor)" ~/.cargo/bin/sv
```

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
| `sv layout dev` | shell + files + system (also `monitor`, `files`; or `alt+m`) |
| `alt+m` | tab menu |
| ``alt+` `` · `sv off` / `sv on` | basic view ⇄ full view |
| `ctrl+]` then a letter | leader key — every shortcut, in every terminal (`ctrl+] ?` lists them) |
| mouse | click a tab or split to focus it · wheel scrolls back |
| `ctrl+q` · `sv exit` | quit Shellvisor |
| `sv status` | is Shellvisor running here? which shell? |
| `sv power on\|off\|auto` | low-power mode (auto: on while on battery) |

### Files pane

Four views — `1` tree · `2` list · `3` git changes · `4` find (`tab` cycles). It follows the folder your shell is in, and refreshes after every command.

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

**Mac users:** `alt` shortcuts need your terminal to send Option as Alt — Terminal.app: *Settings → Profiles → Keyboard → Use Option as Meta key*; iTerm2: *Profiles → Keys → Left Option key → Esc+*. Shellvisor tells you if it's off. The leader key `ctrl+]` always works.

**Selecting text:** Shellvisor uses the mouse wheel for scrollback. To select text with the mouse, hold **Option** (iTerm2) or turn off *View → Allow Mouse Reporting* (Terminal.app). Or set `mouse: false` in the config.

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

The hooks print invisible escape sequences (`OSC 7`, `OSC 133`, `OSC 7770`) that Shellvisor reads and removes. Command history with exit codes and durations is kept in `~/.local/state/shellvisor/history.jsonl`.

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

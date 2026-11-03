# Shellvisor

**Your real shell, plus a workspace around it — inside the terminal you already use.**

Shellvisor is a full-screen terminal app (like `tmux`, `htop` or `vim`) that runs your own zsh or bash in tabs and splits, and — as it grows — adds panes for files, live system stats, a data-flow view of your pipelines, and coding agents working side by side. Every command, alias and full-screen program you use today keeps working unchanged.

> **Status: early development (phase P1 of 7).**
> Working today: tabs and splits of real zsh/bash shells, a tab bar that shows what each tab is doing (running ⠋, failed ✖, new output •), a status bar, and a one-key switch to *basic view*, which looks exactly like a plain terminal. Files, stats, data flow and agents panes arrive in P2–P5. See [docs/ROADMAP.md](docs/ROADMAP.md).

## Why

- **One binary, tiny footprint.** Written in Rust. Budget: ≤ 8 MB binary, ≤ 15 MB memory, < 0.5 % CPU when idle. The current build is ~0.9 MB and uses ~4.4 MB of memory with three tabs open.
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
| ``alt+` `` · `sv off` / `sv on` | basic view ⇄ full view |
| `ctrl+]` then a letter | leader key — every shortcut, in every terminal (`ctrl+] ?` lists them) |
| mouse | click a tab or split to focus it · wheel scrolls back |
| `ctrl+q` · `sv exit` | quit Shellvisor |
| `sv status` | is Shellvisor running here? which shell? |

**Mac users:** `alt` shortcuts need your terminal to send Option as Alt — Terminal.app: *Settings → Profiles → Keyboard → Use Option as Meta key*; iTerm2: *Profiles → Keys → Left Option key → Esc+*. Shellvisor tells you if it's off. The leader key `ctrl+]` always works.

**Selecting text:** Shellvisor uses the mouse wheel for scrollback. To select text with the mouse, hold **Option** (iTerm2) or turn off *View → Allow Mouse Reporting* (Terminal.app). Or set `mouse: false` in the config.

## Configuration

`~/.config/shellvisor/config.yaml` — every key is optional:

```yaml
shell: zsh              # zsh, bash or a full path (default: $SHELL)
scrollback_lines: 5000  # lines of history kept per shell
shell_integration: true # report folder / command / exit code to Shellvisor
mouse: true             # wheel scrolls history
start_view: full        # full (tabs, status bar) or basic (just the shell)
```

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

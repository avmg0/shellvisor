# Releasing Shellvisor

**One command:** `scripts/release.sh 1.0.8` sets the version, commits, pushes, tags,
waits for the Release workflow, updates the Homebrew tap (`../homebrew-tap`, or
`$SV_TAP`) and runs `brew upgrade` (`--no-brew` skips that). It stops at the first
problem: not on main, uncommitted changes, an existing tag, no tap checkout, or no
release after 40 minutes. The steps it runs, by hand:

1. Update `version` in the root `Cargo.toml` (`[workspace.package]`), run `cargo build` so `Cargo.lock` follows, and commit.
2. Tag and push: `git tag v1.0.0 && git push origin main v1.0.0`.
3. The **Release** workflow builds macOS (arm64 and x86_64, both on one Mac runner) and Linux (x86_64, arm64) binaries, packs each as `shellvisor-v1.0.0-<target>.tar.gz` with a `.sha256`, writes `SHA256SUMS`, attaches a signed build-provenance attestation to every archive (Sigstore, via GitHub), and publishes a GitHub release with generated notes.
4. Homebrew: in the tap repository `github.com/avmg0/homebrew-tap`, run
   `scripts/brew-formula.sh 1.0.0 ../homebrew-tap/Formula/shellvisor.rb` (from this repo) and commit there. It refuses to write anything until the release is published. Users install with `brew install avmg0/tap/shellvisor`.

5. Debian / Ubuntu: the Release workflow also builds `shellvisor_<version>_amd64.deb` and `_arm64.deb` (`scripts/package-deb.sh`) and lists them in `SHA256SUMS`.
6. AUR (`shellvisor-bin`): `scripts/aur-pkgbuild.sh <version> ../aur-shellvisor-bin` writes `PKGBUILD` and `.SRCINFO` from the release's checksums into a checkout of `ssh://aur@aur.archlinux.org/shellvisor-bin.git`; commit and push there. The first time, create the package by pushing to that URL from an AUR account with your SSH key added. `scripts/release.sh` does this step too when `../aur-shellvisor-bin` (or `$SV_AUR`) exists.

7. Benchmarks: the Release workflow measures the build (3 tabs, dev layout, ubuntu-latest) and attaches `bench.json`; `scripts/release.sh` adds it to `docs/bench/history.json`, redraws `docs/images/bench.svg` (`scripts/bench-history.py`) and pushes "Benchmarks X".

## Hands-on pass before a release

CI builds and tests on Linux and macOS, but not how it feels. On a Mac, in each of
Terminal.app, iTerm2 and Ghostty:

1. `sv selftest` — every shell ✔ (bash 3.2, your zsh with its plugins, your login shell).
2. `sv doctor` — no ✖; answer the Option-as-Alt key test.
3. Start `shellvisor`: the prompt looks like your normal one; `alt+t`, `alt+\`, `alt+-`, `F2`, `F3`, `F9` work; right-click opens the menu and stays open.
4. Run `sleep 12` and switch to another app: one notification arrives when it ends. Run it again while looking: none.
5. `vim`, `less`, `fzf` (or `ctrl+r`), `ssh` to some host, mouse scrolling, copy with the mouse.
6. Quit with `ctrl+q`, start again: the tabs come back.

## README pictures

`docs/images/` holds the GIF and screenshots. They are made in a throwaway demo
account (home `/home/johnsmith`, prompt `john@mbp`, git author John Smith
<john@example.com>) so no real name, user name or host name shows. Remake them the
same way, never from a personal machine's session.

## Checking a download

```sh
gh attestation verify shellvisor-v1.0.0-aarch64-apple-darwin.tar.gz --repo avmg0/shellvisor
shasum -a 256 -c SHA256SUMS --ignore-missing
```

## macOS signing (optional)

Without these secrets the macOS binaries are unsigned (Homebrew and the install script don't need a signature; a binary downloaded by a browser gets the Gatekeeper quarantine prompt). To sign and notarize, add repository secrets:

| Secret | What |
|---|---|
| `APPLE_CERT_P12` | Developer ID Application certificate + key, `.p12`, base64-encoded |
| `APPLE_CERT_PASSWORD` | the `.p12` password |
| `APPLE_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | for `notarytool` (an app-specific password) |

## crates.io

Not published there: Shellvisor depends on a patched copy of `vt100` (`crates/vt100`, see its `SHELLVISOR.md`), and crates.io doesn't allow path dependencies. `cargo install --git https://github.com/avmg0/shellvisor shellvisor` builds it from source instead.

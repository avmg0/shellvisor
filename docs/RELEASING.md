# Releasing Shellvisor

1. Update `version` in the root `Cargo.toml` (`[workspace.package]`), run `cargo build` so `Cargo.lock` follows, and commit.
2. Tag and push: `git tag v1.0.0 && git push origin main v1.0.0`.
3. The **Release** workflow builds macOS (arm64 and x86_64, both on one Mac runner) and Linux (x86_64, arm64) binaries, packs each as `shellvisor-v1.0.0-<target>.tar.gz` with a `.sha256`, writes `SHA256SUMS`, attaches a signed build-provenance attestation to every archive (Sigstore, via GitHub), and publishes a GitHub release with generated notes.
4. Homebrew: in the tap repository `github.com/avmg0/homebrew-tap`, run
   `scripts/brew-formula.sh 1.0.0 ../homebrew-tap/Formula/shellvisor.rb` (from this repo) and commit there. It refuses to write anything until the release is published. Users install with `brew install avmg0/tap/shellvisor`.

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

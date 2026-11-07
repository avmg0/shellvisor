#!/bin/sh
# One-command release: version, push, tag, wait for the build, Homebrew tap.
#
#   scripts/release.sh 1.0.8            bump to 1.0.8, release it, upgrade here
#   scripts/release.sh 1.0.8 --no-brew  release, but don't run brew upgrade
#
# Steps (each says what it does; it stops at the first problem):
#   1. checks: on main, nothing uncommitted, the tag doesn't exist yet
#   2. sets the version in Cargo.toml and Cargo.lock, commits "Version X"
#      (skipped when it already is X), with your git name and email
#   3. pushes main, then an annotated tag vX and pushes it
#   4. waits for the Release workflow's SHA256SUMS (up to 40 min)
#   5. writes the formula in ../homebrew-tap (or $SV_TAP), commits, pushes
#   6. brew update && brew upgrade avmg0/tap/shellvisor
set -eu
v="${1:?usage: $0 <version, e.g. 1.0.8> [--no-brew]}"
brew=1
[ "${2:-}" = "--no-brew" ] && brew=0
here=$(cd "$(dirname "$0")/.." && pwd)
tap="${SV_TAP:-$here/../homebrew-tap}"
cd "$here"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[31mrelease: %s\033[0m\n' "$*" >&2; exit 1; }

case "$v" in
  *[!0-9.]* | "" | .* | *.) die "version looks wrong: $v (like 1.0.8)" ;;
esac
[ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "not on main"
git diff --quiet && git diff --cached --quiet || die "uncommitted changes — commit or stash them first"
git rev-parse -q --verify "refs/tags/v$v" >/dev/null && die "tag v$v already exists"
[ -d "$tap/.git" ] || die "no tap checkout at $tap (set SV_TAP)"

cur=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
if [ "$cur" != "$v" ]; then
  say "version $cur → $v"
  sed -i.bak "s/^version = \"$cur\"$/version = \"$v\"/" Cargo.toml && rm -f Cargo.toml.bak
  # the two workspace crates in Cargo.lock
  awk -v cur="$cur" -v new="$v" '
    /^name = "(shellvisor|sv-proto)"$/ { hit = 1; print; next }
    hit && $0 == "version = \"" cur "\"" { print "version = \"" new "\""; hit = 0; next }
    { hit = 0; print }' Cargo.lock > Cargo.lock.new && mv Cargo.lock.new Cargo.lock
  grep -q "^version = \"$v\"" Cargo.toml || die "couldn't set the version in Cargo.toml"
  git add Cargo.toml Cargo.lock
  git commit -q -m "Version $v"
fi

say "push main"
git push -q origin main
say "tag v$v"
git tag -a "v$v" -m "v$v"
git push -q origin "v$v"

url="https://github.com/avmg0/shellvisor/releases/download/v$v/SHA256SUMS"
say "waiting for the Release workflow (github.com/avmg0/shellvisor/actions)"
i=0
until curl -fsSL -o /dev/null "$url" 2>/dev/null; do
  i=$((i + 1))
  [ "$i" -gt 120 ] && die "no release after 40 min — check the Release run, then: scripts/brew-formula.sh $v $tap/Formula/shellvisor.rb"
  printf '.'
  sleep 20
done
echo

say "Homebrew formula"
"$here/scripts/brew-formula.sh" "$v" "$tap/Formula/shellvisor.rb"
(cd "$tap" && git add Formula/shellvisor.rb && git commit -q -m "shellvisor $v" && git push -q)

if [ "$brew" = 1 ] && command -v brew >/dev/null; then
  say "brew upgrade"
  brew update >/dev/null && brew upgrade avmg0/tap/shellvisor
fi
say "released $v — quit Shellvisor (ctrl+q) and start it again; sv status shows the running version"

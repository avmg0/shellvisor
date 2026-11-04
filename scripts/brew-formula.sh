#!/bin/sh
# Write the Homebrew formula for a published release, checksums filled in.
#   scripts/brew-formula.sh 1.0.0 ../homebrew-tap/Formula/shellvisor.rb
# Writes nothing unless the release exists and every checksum was found.
set -eu
v="${1:?usage: $0 <version, e.g. 1.0.0> <output file>}"
out="${2:?usage: $0 <version> <output file, e.g. ../homebrew-tap/Formula/shellvisor.rb>}"
url="https://github.com/avmg0/shellvisor/releases/download/v$v/SHA256SUMS"
if ! sums=$(curl -fsSL "$url"); then
  echo "brew-formula: release v$v isn't published yet ($url not found)." >&2
  echo "Push the tag (git push origin v$v), wait for the Release workflow, then run this again." >&2
  exit 1
fi
sha() {
  s=$(printf '%s\n' "$sums" | awk -v f="shellvisor-v$v-$1.tar.gz" '$2 == f { print $1 }')
  [ -n "$s" ] || { echo "brew-formula: no checksum for $1 in SHA256SUMS" >&2; exit 1; }
  printf '%s' "$s"
}
a=$(sha aarch64-apple-darwin); b=$(sha x86_64-apple-darwin)
c=$(sha aarch64-unknown-linux-gnu); d=$(sha x86_64-unknown-linux-gnu)
tmp="$out.tmp.$$"
sed -e "s/VERSION/$v/" \
    -e "s/SHA_AARCH64_APPLE_DARWIN/$a/" -e "s/SHA_X86_64_APPLE_DARWIN/$b/" \
    -e "s/SHA_AARCH64_UNKNOWN_LINUX_GNU/$c/" -e "s/SHA_X86_64_UNKNOWN_LINUX_GNU/$d/" \
    "$(dirname "$0")/../packaging/homebrew/shellvisor.rb" > "$tmp"
mv "$tmp" "$out"
echo "wrote $out for v$v"

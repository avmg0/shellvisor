#!/bin/sh
# Print the Homebrew formula for a release, with its checksums filled in.
#   scripts/brew-formula.sh 1.0.0 > ../homebrew-tap/Formula/shellvisor.rb
set -eu
v="${1:?usage: $0 <version, e.g. 1.0.0>}"
sums=$(curl -fsSL "https://github.com/avmg0/shellvisor/releases/download/v$v/SHA256SUMS")
sha() { printf '%s\n' "$sums" | awk -v t="$1" '$2 ~ t".tar.gz$" { print $1 }'; }
sed -e "s/VERSION/$v/" \
    -e "s/SHA_AARCH64_APPLE_DARWIN/$(sha aarch64-apple-darwin)/" \
    -e "s/SHA_X86_64_APPLE_DARWIN/$(sha x86_64-apple-darwin)/" \
    -e "s/SHA_AARCH64_UNKNOWN_LINUX_GNU/$(sha aarch64-unknown-linux-gnu)/" \
    -e "s/SHA_X86_64_UNKNOWN_LINUX_GNU/$(sha x86_64-unknown-linux-gnu)/" \
    "$(dirname "$0")/../packaging/homebrew/shellvisor.rb"

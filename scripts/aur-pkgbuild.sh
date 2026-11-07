#!/bin/sh
# Write the AUR package files for a published release.
#   scripts/aur-pkgbuild.sh 1.0.8 ../aur-shellvisor-bin
# Fills packaging/aur/PKGBUILD with the version and the release's checksums
# and writes PKGBUILD and .SRCINFO into the AUR repository checkout
# (ssh://aur@aur.archlinux.org/shellvisor-bin.git). Commit and push there.
set -eu
v="${1:?usage: $0 <version> <aur checkout>}"
out="${2:?usage: $0 <version> <aur checkout>}"
here=$(cd "$(dirname "$0")/.." && pwd)
url="https://github.com/avmg0/shellvisor/releases/download/v$v/SHA256SUMS"
sums=$(curl -fsSL "$url") || { echo "aur-pkgbuild: v$v isn't published yet ($url)" >&2; exit 1; }
sha() {
  s=$(printf '%s\n' "$sums" | awk -v f="shellvisor-v$v-$1-unknown-linux-gnu.tar.gz" '$2 == f { print $1 }')
  [ -n "$s" ] || { echo "aur-pkgbuild: no checksum for $1" >&2; exit 1; }
  printf '%s' "$s"
}
a=$(sha x86_64); b=$(sha aarch64)
mkdir -p "$out"
sed -e "s/^pkgver=VERSION/pkgver=$v/" -e "s/SHA_X86_64/$a/" -e "s/SHA_AARCH64/$b/" \
    -e '/^# /d' "$here/packaging/aur/PKGBUILD" > "$out/PKGBUILD"
# .SRCINFO by hand (makepkg --printsrcinfo needs Arch)
base="https://github.com/avmg0/shellvisor/releases/download/v$v"
cat > "$out/.SRCINFO" <<SRC
pkgbase = shellvisor-bin
	pkgdesc = Multi-pane terminal workspace: your real shell plus files, system, data flow and coding agents
	pkgver = $v
	pkgrel = 1
	url = https://github.com/avmg0/shellvisor
	arch = x86_64
	arch = aarch64
	license = Apache-2.0
	depends = glibc
	provides = shellvisor
	conflicts = shellvisor
	source_x86_64 = $base/shellvisor-v$v-x86_64-unknown-linux-gnu.tar.gz
	sha256sums_x86_64 = $a
	source_aarch64 = $base/shellvisor-v$v-aarch64-unknown-linux-gnu.tar.gz
	sha256sums_aarch64 = $b

pkgname = shellvisor-bin
SRC
echo "wrote $out/PKGBUILD and $out/.SRCINFO for v$v"

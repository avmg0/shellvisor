#!/bin/sh
# Build a Debian / Ubuntu package from a release binary.
#   scripts/package-deb.sh <version> <path/to/shellvisor> <amd64|arm64> [outdir]
# Writes <outdir>/shellvisor_<version>_<arch>.deb (outdir: dist). Installs
# /usr/bin/shellvisor, /usr/bin/sv (a link to it) and the docs. Used by the
# Release workflow; works anywhere dpkg-deb is installed.
set -eu
v="${1:?usage: $0 <version> <binary> <amd64|arm64> [outdir]}"
bin="${2:?binary}"
arch="${3:?amd64 or arm64}"
out="${4:-dist}"
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
pkg="$root/shellvisor_${v}_$arch"
mkdir -p "$pkg/DEBIAN" "$pkg/usr/bin" "$pkg/usr/share/doc/shellvisor"
install -m 0755 "$bin" "$pkg/usr/bin/shellvisor"
ln -s shellvisor "$pkg/usr/bin/sv"
install -m 0644 "$here/README.md" "$here/NOTICE" "$pkg/usr/share/doc/shellvisor/"
install -m 0644 "$here/LICENSE" "$pkg/usr/share/doc/shellvisor/copyright"
size=$(du -sk "$pkg/usr" | cut -f1)
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: shellvisor
Version: $v
Section: utils
Priority: optional
Architecture: $arch
Depends: libc6 (>= 2.39), libgcc-s1
Installed-Size: $size
Maintainer: Shellvisor <15078558+avmg0@users.noreply.github.com>
Homepage: https://github.com/avmg0/shellvisor
Description: multi-pane terminal workspace around your real shell
 Shellvisor runs your own zsh or bash full-screen with tabs and splits,
 and adds panes for files, system stats, data flow and coding agents.
 It looks like a plain terminal in basic view; alt+\` shows the rest.
CONTROL
mkdir -p "$out"
dpkg-deb --build --root-owner-group "$pkg" "$out/shellvisor_${v}_$arch.deb" >/dev/null
echo "$out/shellvisor_${v}_$arch.deb"

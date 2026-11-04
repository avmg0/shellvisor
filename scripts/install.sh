#!/bin/sh
# Install the latest Shellvisor release into ~/.local/bin (or $SHELLVISOR_BIN).
#   curl -fsSL https://raw.githubusercontent.com/avmg0/shellvisor/main/scripts/install.sh | sh
# Checks the download against the release's SHA256SUMS before installing.
set -eu
repo=avmg0/shellvisor
bin="${SHELLVISOR_BIN:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) echo "shellvisor: no prebuilt binary for $(uname -s) $(uname -m) — build it: cargo install --git https://github.com/$repo shellvisor" >&2; exit 1 ;;
esac
tag="${SHELLVISOR_VERSION:-$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)}"
[ -n "$tag" ] || { echo "shellvisor: couldn't find the latest release" >&2; exit 1; }
name="shellvisor-$tag-$target"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "https://github.com/$repo/releases/download/$tag/$name.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "https://github.com/$repo/releases/download/$tag/SHA256SUMS"
want=$(awk -v f="$name.tar.gz" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
else got=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1); fi
[ -n "$want" ] && [ "$want" = "$got" ] || { echo "shellvisor: checksum mismatch — not installing" >&2; exit 1; }
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$bin"
install -m 0755 "$tmp/$name/shellvisor" "$bin/shellvisor"
ln -sf shellvisor "$bin/sv"
echo "installed $bin/shellvisor and $bin/sv ($tag)"
case ":$PATH:" in *":$bin:"*) ;; *) echo "add $bin to your PATH to run it" ;; esac
echo "start it with: shellvisor    (sv autostart on: start it in every new terminal)"

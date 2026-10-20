#!/bin/sh
# Formless installer for macOS/Linux: downloads the matching GitHub
# release (or builds from a source checkout when FORMLESS_FROM_SOURCE=1).
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
REPO="https://github.com/athiwatc/formless"
if [ "${FORMLESS_FROM_SOURCE:-0}" = "1" ]; then
  command -v cargo >/dev/null 2>&1 || { echo "error: cargo is required for a source install" >&2; exit 1; }
  cargo install --git "$REPO" --path crates/formless --locked --root "$PREFIX"
  echo "installed: $PREFIX/bin/formless"
  exit 0
fi
os="$(uname -s)"; arch="$(uname -m)"
case "$os/$arch" in
  Darwin/arm64) target="aarch64-apple-darwin" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) echo "error: unsupported platform $os/$arch; use FORMLESS_FROM_SOURCE=1" >&2; exit 1 ;;
esac
version="${FORMLESS_VERSION:-}"
if [ -z "$version" ]; then
  version="$(curl -fsSL https://api.github.com/repos/athiwatc/formless/releases/latest | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"
fi
[ -n "$version" ] || { echo "error: could not determine the latest release" >&2; exit 1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
asset="formless-$version-$target.tar.gz"
curl -fsSL "$REPO/releases/download/$version/$asset" -o "$tmp/$asset"
curl -fsSL "$REPO/releases/download/$version/$asset.sha256" -o "$tmp/$asset.sha256"
(cd "$tmp" && shasum -a 256 -c "$asset.sha256")
mkdir -p "$PREFIX/bin"
tar -xzf "$tmp/$asset" -C "$tmp"
cp "$tmp/formless-$version-$target/formless" "$PREFIX/bin/formless"
chmod +x "$PREFIX/bin/formless"
echo "installed: $PREFIX/bin/formless ($version)"

#!/bin/sh
# Formless installer for macOS/Linux: downloads the matching GitHub
# release (or builds from a source checkout when FORMLESS_FROM_SOURCE=1).
set -eu
PREFIX="${PREFIX:-$HOME/.local}"
REPO="https://github.com/athiwatc/formless"

# Shell completions under $PREFIX/share, where bash-completion, zsh (with
# $PREFIX/share/zsh/site-functions on fpath) and fish look for them.
install_completions() {
  bin="$PREFIX/bin/formless"
  mkdir -p "$PREFIX/share/bash-completion/completions" "$PREFIX/share/zsh/site-functions" \
    "$PREFIX/share/fish/vendor_completions.d"
  "$bin" completions bash > "$PREFIX/share/bash-completion/completions/formless" &&
    "$bin" completions zsh > "$PREFIX/share/zsh/site-functions/_formless" &&
    "$bin" completions fish > "$PREFIX/share/fish/vendor_completions.d/formless.fish" &&
    echo "completions: $PREFIX/share/{bash-completion,zsh/site-functions,fish/vendor_completions.d}" ||
    echo "note: could not install shell completions (formless completions <shell> prints them)" >&2
}

if [ "${FORMLESS_FROM_SOURCE:-0}" = "1" ]; then
  command -v cargo >/dev/null 2>&1 || { echo "error: cargo is required for a source install" >&2; exit 1; }
  cargo install --git "$REPO" --path crates/formless --locked --root "$PREFIX"
  echo "installed: $PREFIX/bin/formless"
  install_completions
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
if [ -f "$tmp/formless-$version-$target/formless.1" ]; then
  mkdir -p "$PREFIX/share/man/man1"
  cp "$tmp/formless-$version-$target/formless.1" "$PREFIX/share/man/man1/formless.1"
fi
echo "installed: $PREFIX/bin/formless ($version)"
install_completions

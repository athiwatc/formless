#!/bin/sh
# Formless installer for macOS/Linux: downloads the matching GitHub
# release, or builds from source when FORMLESS_FROM_SOURCE=1 (this
# checkout when the script runs from one, else the GitHub repository at
# FORMLESS_VERSION or its default branch).
#
#   FORMLESS_VERSION   a release tag, with or without the leading v
#   PREFIX             install root (default $HOME/.local)
set -eu
REPO="https://github.com/athiwatc/formless"
# Overridable for tests and mirrors.
RELEASES="${FORMLESS_RELEASES_URL:-$REPO/releases/download}"
LATEST_API="${FORMLESS_LATEST_URL:-https://api.github.com/repos/athiwatc/formless/releases/latest}"

die() { echo "error: $*" >&2; exit 1; }

if [ -z "${PREFIX:-}" ]; then
  [ -n "${HOME:-}" ] || die "HOME is not set; set PREFIX to the install root (e.g. PREFIX=/usr/local)"
  PREFIX="$HOME/.local"
fi

version="${FORMLESS_VERSION:-}"
# Tags and asset names carry the v: accept 0.2.0 as well as v0.2.0.
case "$version" in
  "" | v*) ;;
  *) version="v$version" ;;
esac

# Preflight, before anything is downloaded: where the binary goes must be
# writable.
mkdir -p "$PREFIX/bin" 2>/dev/null || die "cannot create $PREFIX/bin (set PREFIX to a folder you can write)"
probe="$PREFIX/bin/.formless-write-test.$$"
( : > "$probe" ) 2>/dev/null || die "$PREFIX/bin is not writable (set PREFIX to a folder you can write)"
rm -f "$probe"

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

# Copy $1 over $2 atomically: a temp file next to the target, then a
# rename, so an interrupted install never leaves a truncated binary and a
# running daemon keeps its old file.
install_file() {
  tmpfile="$(dirname "$2")/.$(basename "$2").new.$$"
  cp "$1" "$tmpfile" && chmod "$3" "$tmpfile" && mv -f "$tmpfile" "$2" || {
    rm -f "$tmpfile"
    die "could not write $2"
  }
}

if [ "${FORMLESS_FROM_SOURCE:-0}" = "1" ]; then
  command -v cargo >/dev/null 2>&1 || die "cargo is required for a source install"
  here="$(cd "$(dirname "$0")/.." 2>/dev/null && pwd || true)"
  if [ -n "$here" ] && [ -f "$here/crates/formless/Cargo.toml" ] && [ -z "${FORMLESS_VERSION:-}" ]; then
    echo "building this checkout: $here"
    cargo install --path "$here/crates/formless" --locked --root "$PREFIX"
  elif [ -n "$version" ]; then
    cargo install --git "$REPO" --tag "$version" --locked --root "$PREFIX" formless-bin
  else
    cargo install --git "$REPO" --locked --root "$PREFIX" formless-bin
  fi
  echo "installed: $PREFIX/bin/formless"
  install_completions
  exit 0
fi

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"
if command -v sha256sum >/dev/null 2>&1; then
  check_sum() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null 2>&1; then
  check_sum() { shasum -a 256 -c "$1"; }
else
  die "sha256sum or shasum is required to verify the download"
fi

os="$(uname -s)"; arch="$(uname -m)"
case "${FORMLESS_TARGET:-$os/$arch}" in
  Darwin/arm64) target="aarch64-apple-darwin" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
  */*) die "unsupported platform $os/$arch; use FORMLESS_FROM_SOURCE=1" ;;
  *) target="$FORMLESS_TARGET" ;;
esac

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
fetch() { curl -fsSL --retry 3 --retry-delay 2 "$1" -o "$2"; }

if [ -z "$version" ]; then
  # Keep curl's status: a rate-limited API (403) says so instead of
  # becoming "no release".
  code="$(curl -sSL --retry 3 --retry-delay 2 -o "$tmp/latest.json" -w '%{http_code}' "$LATEST_API" 2>"$tmp/curl.err" || true)"
  case "$code" in
    200) ;;
    403 | 429) die "GitHub refused the release lookup (HTTP $code, probably the unauthenticated rate limit); set FORMLESS_VERSION, e.g. FORMLESS_VERSION=v0.1.0" ;;
    000 | "") die "could not reach $LATEST_API: $(cat "$tmp/curl.err")" ;;
    *) die "the release lookup failed with HTTP $code" ;;
  esac
  version="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp/latest.json" | head -1)"
  [ -n "$version" ] || die "the latest release has no tag (set FORMLESS_VERSION)"
fi

asset="formless-$version-$target.tar.gz"
fetch "$RELEASES/$version/$asset" "$tmp/$asset" || die "could not download $asset (is $version a release?)"
fetch "$RELEASES/$version/$asset.sha256" "$tmp/$asset.sha256" || die "could not download $asset.sha256"
(cd "$tmp" && check_sum "$asset.sha256") || die "checksum mismatch for $asset; nothing was installed"
tar -xzf "$tmp/$asset" -C "$tmp"
install_file "$tmp/formless-$version-$target/formless" "$PREFIX/bin/formless" 755
if [ -f "$tmp/formless-$version-$target/formless.1" ]; then
  mkdir -p "$PREFIX/share/man/man1"
  install_file "$tmp/formless-$version-$target/formless.1" "$PREFIX/share/man/man1/formless.1" 644
fi
echo "installed: $PREFIX/bin/formless ($version)"
install_completions

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
install_completion() {
  mkdir -p "$(dirname "$2")" 2>/dev/null || return 1
  completion_tmp=$(mktemp "$(dirname "$2")/.formless-completion.XXXXXX") || return 1
  if "$bin" completions "$1" > "$completion_tmp" && [ -s "$completion_tmp" ] &&
      chmod 644 "$completion_tmp" && mv -f "$completion_tmp" "$2"; then
    return 0
  fi
  rm -f "$completion_tmp"
  return 1
}

install_completions() {
  bin="$PREFIX/bin/formless"
  completion_failed=""
  for completion_shell in bash zsh fish; do
    case "$completion_shell" in
      bash) completion_dest="$PREFIX/share/bash-completion/completions/formless" ;;
      zsh) completion_dest="$PREFIX/share/zsh/site-functions/_formless" ;;
      fish) completion_dest="$PREFIX/share/fish/vendor_completions.d/formless.fish" ;;
    esac
    if ! install_completion "$completion_shell" "$completion_dest"; then
      completion_failed="$completion_failed $completion_shell"
    fi
  done
  if [ -z "$completion_failed" ]; then
    echo "completions: $PREFIX/share/{bash-completion,zsh/site-functions,fish/vendor_completions.d}"
  else
    echo "note: could not install completions for:$completion_failed (formless completions <shell> prints them)" >&2
  fi
}

# Quote literal paths, including spaces, quotes, dollars and backslashes.
quote_path() {
  printf "'"
  printf '%s' "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

install_guidance() {
  installed_bin="$PREFIX/bin"
  case ":${PATH:-}:" in
    *":$installed_bin:"* | *":$installed_bin/:"*) ;;
    *)
      printf '\n%s is not on PATH. For this shell session, run:\n' "$installed_bin"
      user_shell="${SHELL:-sh}"
      case "${user_shell##*/}" in
        fish)
          # fish single quotes escape both backslashes and single quotes.
          quoted_bin="$(printf '%s' "$installed_bin" | sed -e 's/\\/\\\\/g' -e "s/'/\\\\'/g")"
          printf "  set -gx PATH '%s' \$PATH\n" "$quoted_bin"
          printf 'To keep it, add that line to %s/fish/config.fish.\n' "${XDG_CONFIG_HOME:-${HOME:-~}/.config}"
          ;;
        *)
          case "${user_shell##*/}" in
            bash | zsh | sh | dash | ksh) ;;
            *) printf 'Shell %s is not recognized; the following command is for a POSIX-compatible shell:\n' "$user_shell" ;;
          esac
          quoted_bin="$(quote_path "$installed_bin")"
          printf '  export PATH=%s:"$PATH"\n' "$quoted_bin"
          case "${user_shell##*/}" in
            bash) printf 'To keep it, add that line to %s/.bashrc (interactive shells) or %s/.bash_profile (login shells).\n' "${HOME:-~}" "${HOME:-~}" ;;
            zsh) printf 'To keep it, add that line to %s/.zshrc.\n' "${ZDOTDIR:-${HOME:-~}}" ;;
            sh | dash | ksh) printf 'To keep it, add that line to %s/.profile.\n' "${HOME:-~}" ;;
            *) printf 'Persist the equivalent PATH change in your shell startup configuration.\n' ;;
          esac
          ;;
      esac
      ;;
  esac
  guidance_shell="${SHELL:-sh}"
  case "${guidance_shell##*/}" in
    zsh)
      completion_dir="$PREFIX/share/zsh/site-functions"
      completion_known=false
      case ":${FPATH:-}:" in *":$completion_dir:"*) completion_known=true ;; esac
      if ! $completion_known && command -v zsh >/dev/null 2>&1; then
        # -f avoids loading user startup files just to inspect discovery paths.
        completion_paths=$(zsh -fc 'print -rl -- $fpath' 2>/dev/null || true)
        if printf '%s\n' "$completion_paths" | grep -Fqx "$completion_dir"; then completion_known=true; fi
      fi
      if ! $completion_known; then
        printf '\nFor zsh completions, add this to %s/.zshrc before compinit:\n' "${ZDOTDIR:-${HOME:-~}}"
        printf '  fpath=(%s $fpath)\n' "$(quote_path "$completion_dir")"
      fi
      ;;
    bash)
      if [ "$PREFIX" != "${HOME:-}/.local" ]; then
        printf '\nBash completions are in %s/share; bash-completion discovers XDG_DATA_HOME/XDG_DATA_DIRS. Include this share directory there to load them.\n' "$PREFIX"
      fi
      ;;
  esac
  printf '\nNext: run formless to start the setup wizard, or formless doctor to verify configuration.\n'
  printf 'Setup needs a provider API key or a local model endpoint.\n'
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

# A source install renders the manual; an archive install supplies its file.
install_man_page() (
  man_dir="$PREFIX/share/man/man1"
  man_page="$man_dir/formless.1"
  mkdir -p "$man_dir" && [ ! -d "$man_page" ] || exit 1
  man_tmp="$(mktemp "$man_dir/.formless-man.XXXXXX")" || exit 1
  trap 'rm -f "$man_tmp"' EXIT
  if [ "$#" -eq 0 ]; then
    "$PREFIX/bin/formless" man > "$man_tmp" || exit 1
  else
    cat "$1" > "$man_tmp" || exit 1
  fi
  [ -s "$man_tmp" ] && chmod 644 "$man_tmp" && mv -f "$man_tmp" "$man_page"
)

install_optional_man_page() {
  if ! install_man_page "$@" 2>/dev/null; then
    echo "note: could not install the optional man page in $PREFIX/share/man/man1" >&2
  fi
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
  printf 'installed: %s/bin/formless\n' "$PREFIX"
  install_optional_man_page
  install_completions
  install_guidance
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
  */*) die "unsupported platform $os/$arch; build from source with FORMLESS_FROM_SOURCE=1 (requires Rust/cargo and a C compiler/linker; see README.md source-build requirements)" ;;
  *) target="$FORMLESS_TARGET" ;;
esac

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
# Each attempt bounds connection establishment and the complete transfer,
# including redirects/body reads. Three retries retain transient-error recovery.
fetch() {
  if curl -fsSL --connect-timeout 10 --max-time 120 --retry 3 --retry-delay 2 "$1" -o "$2"; then
    return 0
  else
    fetch_status=$?
  fi
  [ "$fetch_status" -ne 28 ] || die "download timed out; retry or set FORMLESS_RELEASES_URL to a trusted release mirror"
  return "$fetch_status"
}

if [ -z "$version" ]; then
  # Keep curl's status: a rate-limited API (403) says so instead of
  # becoming "no release".
  lookup_status=0
  code="$(curl -sSL --connect-timeout 10 --max-time 120 --retry 3 --retry-delay 2 -o "$tmp/latest.json" -w '%{http_code}' "$LATEST_API" 2>"$tmp/curl.err")" || lookup_status=$?
  [ "$lookup_status" -ne 28 ] || die "release lookup timed out; retry, pin FORMLESS_VERSION, or set FORMLESS_LATEST_URL to a trusted mirror"
  [ "$lookup_status" -eq 0 ] || die "could not reach $LATEST_API: $(cat "$tmp/curl.err")"
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
  install_optional_man_page "$tmp/formless-$version-$target/formless.1"
fi
printf 'installed: %s/bin/formless (%s)\n' "$PREFIX" "$version"
install_completions
install_guidance

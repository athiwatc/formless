#!/usr/bin/env bash
# Fill the Homebrew formula, AUR PKGBUILD and winget manifests for a
# release:
#   scripts/render-packaging.sh VERSION ASSET_DIR OUT_DIR
# ASSET_DIR holds the release's *.sha256 files (and the Windows .zip, when
# present, whose listing must hold the path the manifest names). The
# source tarball's checksum is SRC_SHA, or fetched from GitHub. VERSION
# must be the Cargo.toml version (MANIFEST, default the workspace's).
# winget-releaser builds later versions' manifests from the last published
# one; the rendered ones are what the first submission uses (#841).
# Fails on a missing input or any placeholder left unfilled.
set -euo pipefail
version="${1:?version, e.g. 0.2.0}"
assets="${2:?directory with the release .sha256 files}"
out="${3:?output directory}"
root="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || {
  echo "error: '$version' is not a version like 0.2.0" >&2; exit 1; }
# The packages must describe the version the binaries report.
"$root/scripts/release-check.sh" "v$version" "${MANIFEST:-$root/Cargo.toml}" >/dev/null
mkdir -p "$out/homebrew" "$out/aur" "$out/winget"
sha_of() {
  local f="$assets/formless-v$version-$1.$2.sha256"
  [[ -s "$f" ]] || { echo "error: missing $f" >&2; exit 1; }
  local sum; sum=$(cut -d' ' -f1 < "$f")
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "error: $f holds no SHA-256" >&2; exit 1; }
  echo "$sum"
}
linux=$(sha_of x86_64-unknown-linux-gnu tar.gz)
windows=$(sha_of x86_64-pc-windows-msvc zip)
# The exe sits in a folder named after the archive (Compress-Archive -Path).
nested="formless-v$version-x86_64-pc-windows-msvc/formless.exe"
zip="$assets/formless-v$version-x86_64-pc-windows-msvc.zip"
zip_has_member() {
  local archive=$1 expected=$2 listing
  if command -v unzip >/dev/null; then
    listing=$(unzip -Z1 "$archive" | tr '\\' '/') || return 1
    grep -Fxq -- "$expected" <<<"$listing"
  elif command -v python3 >/dev/null; then
    python3 - "$archive" "$expected" <<'PY'
import sys
from zipfile import ZipFile

archive, expected = sys.argv[1:]
with ZipFile(archive) as files:
    sys.exit(not any(name.replace("\\", "/") == expected for name in files.namelist()))
PY
  else
    echo "error: python3 or unzip is required to inspect $archive" >&2
    exit 1
  fi
}
if [[ -f "$zip" ]] && ! zip_has_member "$zip" "$nested"; then
  echo "error: $zip does not hold $nested (the manifest's RelativeFilePath)" >&2
  exit 1
fi
src_url="https://github.com/athiwatc/formless/archive/refs/tags/v$version.tar.gz"
src_sha="${SRC_SHA:-}"
if [[ -z "$src_sha" ]]; then
  if command -v sha256sum >/dev/null; then
    hasher=(sha256sum)
  elif command -v shasum >/dev/null; then
    hasher=(shasum -a 256)
  else
    echo "error: sha256sum or shasum is required to verify the source archive (set SRC_SHA to skip)" >&2
    exit 1
  fi
  src_archive=$(mktemp)
  trap 'rm -f -- "$src_archive"' EXIT
  if ! curl -fsSL --connect-timeout 15 --max-time 120 --retry 3 --retry-all-errors \
      -o "$src_archive" "$src_url"; then
    echo "error: could not download $src_url to compute its checksum (set SRC_SHA to skip)" >&2
    exit 1
  fi
  src_sha=$("${hasher[@]}" < "$src_archive" | cut -d' ' -f1)
fi
[[ "$src_sha" =~ ^[0-9a-f]{64}$ ]] || { echo "error: no checksum for $src_url" >&2; exit 1; }
# Field-scoped edits: the url line and the formula's top-level sha256.
awk -v url="$src_url" -v sha="$src_sha" '
  !u && /^[[:space:]]*url "/ { sub(/url ".*"/, "url \"" url "\""); u = 1 }
  !s && /^[[:space:]]*sha256 "/ { sub(/sha256 ".*"/, "sha256 \"" sha "\""); s = 1 }
  { print }
' "$root/packaging/homebrew/formless.rb" > "$out/homebrew/formless.rb"
sed -e "s|@VERSION@|$version|g" -e "s|@SHA256_LINUX@|$linux|g" \
    "$root/packaging/aur/PKGBUILD.in" > "$out/aur/PKGBUILD"
for tpl in "$root"/packaging/winget/*.yaml.in; do
  sed -e "s|@VERSION@|$version|g" -e "s|@SHA256_WINDOWS@|$windows|g" \
    "$tpl" > "$out/winget/$(basename "${tpl%.in}")"
done
grep -q "RelativeFilePath: $nested" "$out/winget/athiwatc.formless.installer.yaml" \
  || { echo "error: the winget manifest does not name $nested" >&2; exit 1; }
# Nothing left unfilled, and each file names this version.
if grep -n '@[A-Z0-9_]*@' "$out/aur/PKGBUILD" "$out"/winget/*.yaml; then
  echo "error: unfilled placeholder" >&2; exit 1
fi
grep -q "v$version.tar.gz" "$out/homebrew/formless.rb" \
  || { echo "error: the formula does not name v$version" >&2; exit 1; }
grep -q "$src_sha" "$out/homebrew/formless.rb" \
  || { echo "error: the formula's sha256 was not replaced" >&2; exit 1; }
echo "rendered packaging for v$version into $out"

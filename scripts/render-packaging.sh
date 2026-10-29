#!/usr/bin/env bash
# Fill the Homebrew formula and AUR PKGBUILD for a release:
#   scripts/render-packaging.sh VERSION ASSET_DIR OUT_DIR
# ASSET_DIR holds the release's *.sha256 files. The source tarball's
# checksum is SRC_SHA, or fetched from GitHub. winget is not rendered
# here: winget-releaser builds each version's manifests from the last
# published one (packaging/winget/ is the template for the first, by hand).
# Fails on a missing input or any placeholder left unfilled.
set -euo pipefail
version="${1:?version, e.g. 0.2.0}"
assets="${2:?directory with the release .sha256 files}"
out="${3:?output directory}"
root="$(cd "$(dirname "$0")/.." && pwd)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || {
  echo "error: '$version' is not a version like 0.2.0" >&2; exit 1; }
mkdir -p "$out/homebrew" "$out/aur"
sha_of() {
  local f="$assets/formless-v$version-$1.$2.sha256"
  [[ -s "$f" ]] || { echo "error: missing $f" >&2; exit 1; }
  local sum; sum=$(cut -d' ' -f1 < "$f")
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || { echo "error: $f holds no SHA-256" >&2; exit 1; }
  echo "$sum"
}
linux=$(sha_of x86_64-unknown-linux-gnu tar.gz)
src_url="https://github.com/athiwatc/formless/archive/refs/tags/v$version.tar.gz"
src_sha="${SRC_SHA:-$(curl -fsSL "$src_url" | shasum -a 256 | cut -d' ' -f1)}"
[[ "$src_sha" =~ ^[0-9a-f]{64}$ ]] || { echo "error: no checksum for $src_url" >&2; exit 1; }
# Field-scoped edits: the url line and the formula's top-level sha256.
awk -v url="$src_url" -v sha="$src_sha" '
  !u && /^[[:space:]]*url "/ { sub(/url ".*"/, "url \"" url "\""); u = 1 }
  !s && /^[[:space:]]*sha256 "/ { sub(/sha256 ".*"/, "sha256 \"" sha "\""); s = 1 }
  { print }
' "$root/packaging/homebrew/formless.rb" > "$out/homebrew/formless.rb"
sed -e "s|@VERSION@|$version|g" -e "s|@SHA256_LINUX@|$linux|g" \
    "$root/packaging/aur/PKGBUILD.in" > "$out/aur/PKGBUILD"
# Nothing left unfilled, and each file names this version.
if grep -n '@[A-Z0-9_]*@' "$out/aur/PKGBUILD"; then
  echo "error: unfilled placeholder in PKGBUILD" >&2; exit 1
fi
grep -q "v$version.tar.gz" "$out/homebrew/formless.rb" \
  || { echo "error: the formula does not name v$version" >&2; exit 1; }
grep -q "$src_sha" "$out/homebrew/formless.rb" \
  || { echo "error: the formula's sha256 was not replaced" >&2; exit 1; }
echo "rendered packaging for v$version into $out"

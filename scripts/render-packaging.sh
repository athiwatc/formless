#!/usr/bin/env bash
# Fill the Homebrew formula, winget manifests and AUR PKGBUILD for a
# release: scripts/render-packaging.sh VERSION ASSET_DIR OUT_DIR
# ASSET_DIR holds the release's *.sha256 files; the source tarball's
# checksum is fetched from GitHub.
set -euo pipefail
version="${1:?version, e.g. 0.2.0}"
assets="${2:?directory with the release .sha256 files}"
out="${3:?output directory}"
root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$out/homebrew" "$out/winget" "$out/aur"
sha_of() { cut -d' ' -f1 < "$assets/formless-v$version-$1.$2.sha256"; }
linux=$(sha_of x86_64-unknown-linux-gnu tar.gz)
win=$(sha_of x86_64-pc-windows-msvc zip)
src_url="https://github.com/athiwatc/formless/archive/refs/tags/v$version.tar.gz"
src_sha="${SRC_SHA:-$(curl -fsSL "$src_url" | shasum -a 256 | cut -d' ' -f1)}"
sed -e "s|refs/tags/v[0-9.]*\.tar\.gz|refs/tags/v$version.tar.gz|" \
    -e "s|^  sha256 \".*\"|  sha256 \"$src_sha\"|" \
    "$root/packaging/homebrew/formless.rb" > "$out/homebrew/formless.rb"
for f in "$root"/packaging/winget/*.yaml; do
  sed -e "s|0\.1\.0|$version|g" \
      -e "s|InstallerSha256: .*|InstallerSha256: $(echo "$win" | tr a-z A-Z)|" \
      "$f" > "$out/winget/$(basename "$f")"
done
sed -e "s|@VERSION@|$version|g" -e "s|@SHA256_LINUX@|$linux|g" \
    "$root/packaging/aur/PKGBUILD.in" > "$out/aur/PKGBUILD"
echo "rendered packaging for v$version into $out"

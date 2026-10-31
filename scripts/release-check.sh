#!/bin/sh
# Release gate (#842): the tag must name the workspace version, so asset
# names, `formless --version` and `formless update` agree.
#   scripts/release-check.sh v0.2.0 [Cargo.toml]
set -eu
tag="${1:?usage: release-check.sh TAG [Cargo.toml]}"
manifest="${2:-$(dirname "$0")/../Cargo.toml}"
version="$(awk '/^\[workspace.package\]/{p=1; next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/, "", $0); sub(/^version=/, ""); print; exit}' "$manifest")"
[ -n "$version" ] || { echo "error: no [workspace.package] version in $manifest" >&2; exit 1; }
if [ "$tag" != "v$version" ]; then
  echo "error: tag $tag does not match Cargo.toml version $version (expected v$version); bump the version or fix the tag" >&2
  exit 1
fi
echo "tag $tag matches Cargo.toml $version"

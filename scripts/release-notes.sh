#!/bin/sh
# Release notes for a version from CHANGELOG.md (#842). Requires a
# nonempty `## X.Y.Z` or `## [X.Y.Z]` section.
#   scripts/release-notes.sh 0.2.0 [CHANGELOG.md]
set -eu
tag="${1:?usage: release-notes.sh VERSION [CHANGELOG.md]}"
version="${tag#v}"
changelog="${2:-$(dirname "$0")/../CHANGELOG.md}"
if ! awk -v v="$version" '
  /^##[[:blank:]]+/ {
    if (on) exit
    sub(/\r$/, "")
    on = ($2 == v || $2 == "[" v "]")
    next
  }
  on && (wrote || /[^[:space:]]/) { print; wrote = 1 }
  END { if (!wrote) exit 1 }
' "$changelog"; then
  echo "error: no nonempty CHANGELOG section for $version in $changelog" >&2
  exit 1
fi

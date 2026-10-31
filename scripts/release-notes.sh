#!/bin/sh
# Release notes for a version from CHANGELOG.md (#842): the body of its
# `## [X.Y.Z]` section. Prints nothing when there is none (the release
# then carries GitHub's generated notes alone).
#   scripts/release-notes.sh 0.2.0 [CHANGELOG.md]
set -eu
version="${1#v}"
changelog="${2:-$(dirname "$0")/../CHANGELOG.md}"
awk -v v="$version" '
  /^## \[/ { if (on) exit; on = (index($0, "## [" v "]") == 1); next }
  on { print }
' "$changelog" | sed -e '/./,$!d'

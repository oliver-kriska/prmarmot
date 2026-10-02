#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md, for the GitHub release page.
#
#   scripts/release-notes.sh 0.14.0          # the "## [0.14.0]" section, headings and all
#
# Exits 1 with a message when CHANGELOG.md has no section for that version or
# the section is empty, so a release never ships with a bare compare link as
# its notes (the in-app Update button on a direct install opens that page).
set -euo pipefail

VERSION=${1:?usage: scripts/release-notes.sh X.Y.Z}
VERSION=${VERSION#v}
CHANGELOG=${2:-CHANGELOG.md}

notes=$(awk -v version="$VERSION" '
  /^## \[/ {
    if (found) exit
    found = index($0, "## [" version "]") == 1
    next
  }
  found { print }
' "$CHANGELOG" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')

if [[ -z "${notes//[[:space:]]/}" ]]; then
  echo "release-notes: $CHANGELOG has no section for $VERSION (run: make bump V=$VERSION)" >&2
  exit 1
fi
printf '%s\n' "$notes"

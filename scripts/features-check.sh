#!/bin/sh
# Fail when a released version whose CHANGELOG.md section lists Features is
# never named in FEATURES.md, so a shipped feature can't be left off the list.
# The check is coarse on purpose: it proves the version appears, not that
# every feature in it does.
set -eu

cd "$(dirname "$0")/.."

missing=$(awk '
  /^## \[/ { version = $2; gsub(/[][]/, "", version); next }
  /^### .*Features/ && version ~ /^[0-9]/ { print version; version = "" }
' CHANGELOG.md | while read -r version; do
  grep -q "since v$version" FEATURES.md || echo "v$version"
done)

if [ -n "$missing" ]; then
  echo "FEATURES.md names no feature from these releases with Features in CHANGELOG.md:" >&2
  echo "$missing" >&2
  exit 1
fi
echo "FEATURES.md names every release that shipped features."

#!/usr/bin/env bash
# bump-version.sh — prepare the version change for a release commit.
#
# Usage: scripts/bump-version.sh X.Y.Z [--no-changelog]
#
# Sets the [package] version of the app (Cargo.toml) and the CLI
# (cli/Cargo.toml) together — a prmarmot-cli unit test pins them equal —
# refreshes only the workspace entries in Cargo.lock, and regenerates
# CHANGELOG.md for vX.Y.Z with git-cliff. It never commits, tags, or pushes:
# publishing is the tag-only flow in packaging/RELEASING.md and needs explicit
# approval first.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${1:-}"
CHANGELOG=1
[[ "${2:-}" == "--no-changelog" ]] && CHANGELOG=0

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "usage: $0 X.Y.Z [--no-changelog]   (exact MAJOR.MINOR.PATCH, no leading v)" >&2
  exit 2
}

package_version() {
  awk -F'"' '
    /^\[package\]/ { in_pkg = 1; next }
    /^\[/          { in_pkg = 0 }
    in_pkg && /^version[[:space:]]*=/ { print $2; exit }
  ' "$1"
}

set_package_version() {
  local file=$1 tmp
  tmp="$(mktemp "$file.XXXXXX")"
  awk -v version="$VERSION" '
    /^\[package\]/ { in_pkg = 1; print; next }
    /^\[/          { in_pkg = 0 }
    in_pkg && !done && /^version[[:space:]]*=/ { print "version = \"" version "\""; done = 1; next }
    { print }
  ' "$file" >"$tmp"
  # Rewrite in place so the manifest keeps its permissions.
  cat "$tmp" >"$file"
  rm -f "$tmp"
  [[ "$(package_version "$file")" == "$VERSION" ]] || {
    echo "error: could not set the version in $file" >&2
    exit 1
  }
}

CURRENT="$(package_version Cargo.toml)"
CLI_CURRENT="$(package_version cli/Cargo.toml)"
[[ -n "$CURRENT" && -n "$CLI_CURRENT" ]] || {
  echo "error: could not read the current versions" >&2
  exit 1
}
if [[ "$VERSION" == "$CURRENT" && "$VERSION" == "$CLI_CURRENT" ]]; then
  echo "error: the app and CLI are already at $VERSION" >&2
  exit 1
fi
if [[ "$(printf '%s\n%s\n' "$CURRENT" "$VERSION" | sort -V | tail -1)" != "$VERSION" ]]; then
  echo "error: $VERSION is lower than the current $CURRENT" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  echo "error: tag v$VERSION already exists; released versions are never reused" >&2
  exit 1
fi

set_package_version Cargo.toml
set_package_version cli/Cargo.toml
# Resolving rewrites only the changed workspace entries; nothing is upgraded.
cargo metadata --format-version 1 --offline >/dev/null
echo "Set the app and CLI to $VERSION (was $CURRENT) and refreshed Cargo.lock"

if [[ "$CHANGELOG" == 1 ]]; then
  if command -v git-cliff >/dev/null 2>&1; then
    git cliff --config cliff.toml --tag "v$VERSION" -o CHANGELOG.md
    echo "Regenerated CHANGELOG.md for v$VERSION"
  else
    echo "warning: git-cliff not found; CHANGELOG.md not regenerated (brew install git-cliff)" >&2
  fi
fi

cat <<EOF

Next (packaging/RELEASING.md):
  git fetch --tags && git tag -l v$VERSION   # must print nothing: tags created on GitHub only exist remotely
  make verify
  git add Cargo.toml cli/Cargo.toml Cargo.lock CHANGELOG.md
  git commit -m "chore(release): v$VERSION"
Then, only with explicit approval: push main, then push the v$VERSION tag.
EOF

#!/usr/bin/env bash

# Render the committed cask template into a checked-out homebrew-tap.

set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 VERSION SHA256 TAP_CHECKOUT" >&2
  exit 2
fi

version=$1
sha256=$2
tap=$3
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_cask="$root/packaging/homebrew/prmarmot.rb"
target_rel="Casks/prmarmot.rb"
target_cask="$tap/$target_rel"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
  echo "invalid version: $version" >&2
  exit 1
}
[[ "$sha256" =~ ^[0-9a-f]{64}$ ]] || {
  echo "invalid SHA-256: $sha256" >&2
  exit 1
}
[[ -d "$tap/.git" ]] || { echo "not a Git checkout: $tap" >&2; exit 1; }
[[ -f "$source_cask" ]] || { echo "missing cask template: $source_cask" >&2; exit 1; }

mkdir -p "$(dirname "$target_cask")"
sed \
  -e "s/^  version \"[^\"]*\"/  version \"$version\"/" \
  -e "s/^  sha256 \"[^\"]*\"/  sha256 \"$sha256\"/" \
  "$source_cask" >"$target_cask"

grep -Fq "  version \"$version\"" "$target_cask"
grep -Fq "  sha256 \"$sha256\"" "$target_cask"

if [[ -z "$(git -C "$tap" status --porcelain -- "$target_rel")" ]]; then
  echo "Homebrew cask is already current; nothing to commit"
  exit 0
fi

git -C "$tap" add "$target_rel"
git -C "$tap" commit -m "prmarmot $version"
git -C "$tap" push origin HEAD

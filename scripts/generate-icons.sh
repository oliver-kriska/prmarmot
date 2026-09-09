#!/usr/bin/env bash
# Regenerate committed branding assets from the SVG. macOS maintainer tool;
# normal builds consume the assets and do not need librsvg installed.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$ROOT/assets/branding"
command -v rsvg-convert >/dev/null || { echo 'Install librsvg to regenerate icons.' >&2; exit 1; }
command -v iconutil >/dev/null || { echo 'macOS iconutil is required.' >&2; exit 1; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/prboard-icons.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/prboard.iconset"
for size in 16 32 64 128 256 512 1024; do
  rsvg-convert --width "$size" --height "$size" "$ASSETS/icon.svg" \
    --output "$ASSETS/icon-$size.png"
done
for size in 16 32 128 256 512; do
  cp "$ASSETS/icon-$size.png" "$WORK/prboard.iconset/icon_${size}x${size}.png"
  cp "$ASSETS/icon-$((size * 2)).png" "$WORK/prboard.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$WORK/prboard.iconset" -o "$ASSETS/prboard.icns"
echo "Generated PNG sizes 16–1024 and prboard.icns from assets/branding/icon.svg"

#!/usr/bin/env bash
# Regenerate committed branding assets from the SVG. macOS maintainer tool;
# normal builds consume the assets and do not need librsvg installed.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ASSETS="$ROOT/assets/branding"
command -v rsvg-convert >/dev/null || { echo 'Install librsvg to regenerate icons.' >&2; exit 1; }
command -v iconutil >/dev/null || { echo 'macOS iconutil is required.' >&2; exit 1; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/prmarmot-icons.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/prmarmot.iconset"
rsvg-convert --width 192 --height 212 "$ASSETS/mascot.svg" \
  --output "$ASSETS/mascot.png"
for size in 16 32 64 128 256 512 1024; do
  source="$ASSETS/icon.svg"
  # Optical micro artwork omits details that disappear at menu/Finder sizes.
  if (( size <= 32 )); then
    source="$ASSETS/icon-small.svg"
  fi
  rsvg-convert --width "$size" --height "$size" "$source" \
    --output "$ASSETS/icon-$size.png"
done
for size in 16 32 128 256 512; do
  cp "$ASSETS/icon-$size.png" "$WORK/prmarmot.iconset/icon_${size}x${size}.png"
  cp "$ASSETS/icon-$((size * 2)).png" "$WORK/prmarmot.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$WORK/prmarmot.iconset" -o "$ASSETS/prmarmot.icns"
echo "Generated PNG sizes 16–1024 and prmarmot.icns from the branding SVG sources"

#!/usr/bin/env bash
# Build PR Marmot for Linux in a Debian container and smoke-test the GPUI app
# on X11 (Xvfb) with Mesa's software Vulkan driver (lavapipe).
#
#   scripts/linux-smoke.sh [OUT_DIR]        # default: target/linux-smoke
#
# Needs Docker (OrbStack works). The checkout is mounted read-only; the cargo
# registry and target dir live in Docker volumes (prmarmot-linux-*), so a
# second run only rebuilds what changed. Two stages:
#   build  rust:1-bookworm — the older glibc a .deb would target
#   run    debian:trixie   — Bookworm's Mesa 22 cannot present a Vulkan
#          swapchain on Xvfb (every window stays black, vkcube included, with
#          the old GPUI as well); Trixie's Mesa 25 can.
# Writes to OUT_DIR:
#   build.log, app-*.log    the build and each app run
#   board.png               the demo board, retention on (GPUI Fast default)
#   details.png             after Down ×3 and Space (Details opens)
#   board-noretain.png      the board with GPUI_VIEW_RETENTION=0
# and prints each picture's colour count; a black window has one colour.
# The native architecture of the Docker host is built (arm64 on Apple
# silicon). Wayland is not exercised.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/linux-smoke}"
BUILD_IMAGE="${PRMARMOT_LINUX_BUILD_IMAGE:-rust:1-bookworm}"
RUN_IMAGE="${PRMARMOT_LINUX_RUN_IMAGE:-debian:trixie}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

command -v docker >/dev/null || { echo "linux-smoke: docker not found" >&2; exit 1; }

echo "== build ($BUILD_IMAGE)"
docker run --rm \
  -v "$ROOT:/src:ro" \
  -v "$OUT:/out" \
  -v prmarmot-linux-target:/target \
  -v prmarmot-linux-registry:/usr/local/cargo/registry \
  -v prmarmot-linux-rustup:/usr/local/rustup \
  -e CARGO_TARGET_DIR=/target \
  "$BUILD_IMAGE" bash -euo pipefail -c '
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  build-essential pkg-config cmake clang \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-dev libx11-xcb-dev libxcb1-dev libxcb-xkb-dev libxi-dev libxrandr-dev libxcursor-dev \
  libfontconfig-dev libfreetype-dev libzstd-dev >/dev/null
cd /src
echo "   $(uname -m)"
cargo build --release --locked --bin prmarmot >/out/build.log 2>&1 || { tail -40 /out/build.log; exit 1; }
tail -1 /out/build.log
'

echo "== run ($RUN_IMAGE)"
docker run --rm \
  -v "$ROOT:/src:ro" \
  -v "$OUT:/out" \
  -v prmarmot-linux-target:/target:ro \
  "$RUN_IMAGE" bash -uo pipefail -c '
apt-get update -qq >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  procps python3 libxkbcommon-x11-0 libvulkan1 libfontconfig1 libx11-xcb1 mesa-vulkan-drivers \
  xvfb x11-utils xdotool imagemagick fonts-dejavu-core fonts-noto-core >/dev/null 2>&1
mkdir -p /tmp/xdg && chmod 700 /tmp/xdg
export XDG_RUNTIME_DIR=/tmp/xdg DISPLAY=:99
Xvfb :99 -screen 0 1920x1280x24 >/dev/null 2>&1 &
sleep 2
cd /src

window() { xwininfo -root -tree | grep "\"dev.oliverkriska.prmarmot\"" | awk "{print \$1}" | head -1; }
shoot() { # name, window
  import -window "$2" "/out/$1.png"
  echo "   $1.png: $(convert "/out/$1.png" -format %k info:) colours"
}
start() { # name, env...
  local name=$1; shift
  env "$@" PRMARMOT_DEMO_BINARY=/target/release/prmarmot \
    scripts/demo.sh --width 1700 --height 1150 >"/out/app-$name.log" 2>&1 &
  sleep 20
  WID=$(window)
  if [ -z "$WID" ]; then echo "   $name: no window"; tail -20 "/out/app-$name.log"; return 1; fi
}

start board || exit 1
shoot board "$WID"
xdotool windowfocus --sync "$WID" 2>/dev/null || true
xdotool key --delay 150 Down Down Down space
sleep 2
shoot details "$WID"
pkill -f /target/release/prmarmot; sleep 2

start board-noretain GPUI_VIEW_RETENTION=0 || exit 1
shoot board-noretain "$WID"
pkill -f /target/release/prmarmot
'
echo "linux-smoke: results in $OUT"

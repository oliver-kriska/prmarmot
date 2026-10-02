#!/usr/bin/env bash
# capture.sh — screenshot PR Marmot's main screens on fictional demo data, in
# light and dark, the same way every time.
#
#   scripts/capture.sh [--out DIR] [--light | --dark] [SCENE...]
#
# Scenes: board, review-queue, all-open, pr-details (default: all).
# Writes DIR/<scene>-<theme>.png (default DIR: target/captures). Uses the
# debug build, or PRMARMOT_DEMO_BINARY (e.g. target/release/prmarmot).
#
# Each scene is a fresh instance launched by scripts/demo.sh, so nothing a
# person has configured or signed in to is read, and nothing real is shown.
# The traps this avoids:
#   - GPUI draws nothing while its window is covered, so a capture would keep a
#     stale first frame. The window is brought to the front, and the scene
#     fails after 5 s if another window still lies over it.
#   - A key sent to "the focused window" can land anywhere; one once quit an
#     instance under a memory measurement. Keys here are posted to the
#     scene's own process (scripts/capture/post-event.swift), and nothing runs
#     while a memory-gate measurement is live.
#   - A frame taken mid-load or mid-transition: a scene is captured only after
#     the demo data has answered the board's query, and saved only once two
#     captures 0.6 s apart are identical.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/target/captures"
THEMES=(light dark)
WANTED=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="${2:?--out needs a directory}"; shift ;;
    --light) THEMES=(light) ;;
    --dark) THEMES=(dark) ;;
    -h | --help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    --*) printf 'capture: unknown option %s\n' "$1" >&2; exit 2 ;;
    *) WANTED+=("$1") ;;
  esac
  shift
done

# name | app arguments | keys once the board has settled.
# Down (125) moves the selection, Space (49) opens Details: six Downs select
# #414, the first layer of the demo's stack.
SCENES=(
  "board||"
  "review-queue|--review|"
  "all-open|--repo demo-labs/atlas --all-open|"
  "pr-details||key:125 key:125 key:125 key:125 key:125 key:125 key:49"
)

if [[ $(uname) != Darwin ]]; then
  echo "capture: macOS only (screencapture, CGWindowList)" >&2
  exit 1
fi
if pgrep -f "scripts/(measure|measure-footprint|spike-run)\.sh" >/dev/null; then
  echo "capture: a memory-gate measurement is running; capture nothing until it ends" >&2
  exit 1
fi
BINARY="${PRMARMOT_DEMO_BINARY:-$ROOT/target/debug/prmarmot}"
if [[ ! -x "$BINARY" ]]; then
  printf 'capture: no binary at %s (cargo build first)\n' "$BINARY" >&2
  exit 1
fi

TOOLS="$(mktemp -d "${TMPDIR:-/tmp}/prmarmot-capture.XXXXXX")"
APP_PID=""
cleanup() {
  if [[ -n $APP_PID ]]; then kill "$APP_PID" 2>/dev/null || true; fi
  rm -rf "$TOOLS"
}
trap cleanup EXIT INT TERM
swiftc -O "$ROOT/scripts/capture/window.swift" -o "$TOOLS/window"
swiftc -O "$ROOT/scripts/capture/post-event.swift" -o "$TOOLS/post-event"
mkdir -p "$OUT"
# A sleeping display captures as black.
caffeinate -u -t 2 &

wanted() {
  [[ ${#WANTED[@]} -eq 0 ]] && return 0
  local name
  for name in "${WANTED[@]}"; do [[ $name == "$1" ]] && return 0; done
  return 1
}

# Bring the instance's window to the front and print "id x y w h", or fail
# after 5 s if it is still covered.
front_window() {
  local tries=0 info
  while ((tries < 25)); do
    osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $APP_PID) to true" >/dev/null 2>&1 || true
    if info="$("$TOOLS/window" "$APP_PID" 2>/dev/null)"; then
      echo "$info"
      return 0
    fi
    sleep 0.2
    tries=$((tries + 1))
  done
  "$TOOLS/window" "$APP_PID" >/dev/null || true # says why
  return 1
}

# Capture until two frames 0.6 s apart are identical (15 s at most).
settled_capture() {
  local wid="$1" out="$2" a="$TOOLS/a.png" b="$TOOLS/b.png" tries=0
  screencapture -x -o -l"$wid" "$a"
  while ((tries < 25)); do
    sleep 0.6
    screencapture -x -o -l"$wid" "$b"
    if cmp -s "$a" "$b"; then
      mv "$b" "$out"
      return 0
    fi
    mv "$b" "$a"
    tries=$((tries + 1))
  done
  echo "capture: the window never stopped changing" >&2
  return 1
}

captured=0
for theme in "${THEMES[@]}"; do
  for scene in "${SCENES[@]}"; do
    IFS='|' read -r name app_args steps <<<"$scene"
    wanted "$name" || continue
    flags=()
    [[ $theme == dark ]] && flags+=(--dark)
    answered="$TOOLS/$name-$theme.answered"
    # shellcheck disable=SC2206 # app_args are fixed words from the table above
    args=($app_args)
    PRMARMOT_DEMO_BINARY="$BINARY" PRMARMOT_DEMO_ANSWERED="$answered" "$ROOT/scripts/demo.sh" ${flags[@]+"${flags[@]}"} \
      ${args[@]+"${args[@]}"} >"$TOOLS/$name-$theme.log" 2>&1 &
    launcher=$!
    APP_PID=""
    for _ in $(seq 50); do
      APP_PID="$(pgrep -P "$launcher" | head -1 || true)"
      [[ -n $APP_PID ]] && break
      sleep 0.2
    done
    if [[ -z $APP_PID ]]; then
      echo "capture: $name ($theme) did not start" >&2
      cat "$TOOLS/$name-$theme.log" >&2
      exit 1
    fi
    for _ in $(seq 75); do
      [[ -s $answered ]] && break
      sleep 0.2
    done
    if [[ ! -s $answered ]]; then
      echo "capture: $name ($theme): the board never asked for its pull requests" >&2
      cat "$TOOLS/$name-$theme.log" >&2
      exit 1
    fi
    read -r wid _ _ _ _ < <(front_window) || {
      echo "capture: $name ($theme): window covered or missing" >&2
      exit 1
    }
    settled_capture "$wid" "$TOOLS/before.png"
    if [[ -n $steps ]]; then
      # shellcheck disable=SC2086 # steps are fixed words from the table above
      "$TOOLS/post-event" "$APP_PID" $steps
      read -r wid _ _ _ _ < <(front_window) || {
        echo "capture: $name ($theme): window covered after input" >&2
        exit 1
      }
    fi
    settled_capture "$wid" "$OUT/$name-$theme.png"
    if [[ -n $steps ]] && cmp -s "$TOOLS/before.png" "$OUT/$name-$theme.png"; then
      echo "capture: $name ($theme): the input changed nothing on screen" >&2
      exit 1
    fi
    echo "$OUT/$name-$theme.png"
    captured=$((captured + 1))
    kill "$APP_PID" 2>/dev/null || true
    wait "$launcher" 2>/dev/null || true
    APP_PID=""
  done
done

if ((captured == 0)); then
  echo "capture: no scene matched ${WANTED[*]}" >&2
  exit 2
fi

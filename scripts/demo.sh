#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAKE_GH="$ROOT/scripts/demo/gh"

if [[ ${1:-} == "--self-test" ]]; then
  exec python3 "$FAKE_GH" --self-test
fi

FAIL_ONCE=false
if [[ ${1:-} == "--fail-once" ]]; then
  FAIL_ONCE=true
  shift
fi

# Preserve only the runner-specific binary setting. App behavior must come
# from the isolated demo config, never from the caller's real environment.
BINARY="${PRBOARD_DEMO_BINARY:-$ROOT/target/debug/prboard}"
unset PRBOARD_REPO PRBOARD_REFRESH_SECS PRBOARD_THEME
unset PRBOARD_ISSUE_PATTERN PRBOARD_ISSUE_URL_TEMPLATE
unset PRBOARD_DEFAULT_REVIEWERS

if [[ ! -x "$BINARY" ]]; then
  printf 'demo: binary is not executable: %s\n' "$BINARY" >&2
  printf 'demo: build it with: cargo build\n' >&2
  exit 1
fi

DEMO_HOME="$(mktemp -d "${TMPDIR:-/tmp}/prboard-demo.XXXXXX")"
trap 'rm -rf "$DEMO_HOME"' EXIT INT TERM
mkdir -p "$DEMO_HOME/prboard"
if $FAIL_ONCE; then
  touch "$DEMO_HOME/fail-next-graphql"
fi
cat >"$DEMO_HOME/prboard/config.toml" <<'TOML'
repo = "demo-labs/atlas"
repos = ["demo-labs/atlas", "demo-labs/mobile"]
pinned_repos = ["demo-labs/atlas", "demo-labs/mobile"]
refresh_secs = 300
theme = "light"
view = "authored"
default_reviewers = ["alex", "sam"]

[issue_link]
pattern = "DEMO-[0-9]+"
url_template = "https://example.com/issues/{id}"

[window]
width = 1440
height = 800
TOML

export XDG_CONFIG_HOME="$DEMO_HOME"
export PATH="$ROOT/scripts/demo:$PATH"
"$BINARY" "$@"

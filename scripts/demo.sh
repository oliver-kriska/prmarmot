#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAKE_GH="$ROOT/scripts/demo/gh"

if [[ ${1:-} == "--self-test" ]]; then
  exec python3 "$FAKE_GH" --self-test
fi

FAIL_ONCE=false
UPDATE_AVAILABLE=false
UPDATE_FAILED=false
while [[ ${1:-} == --* ]]; do
  case "$1" in
    --fail-once) FAIL_ONCE=true ;;
    --update-available) UPDATE_AVAILABLE=true ;;
    --update-failed) UPDATE_FAILED=true ;;
    *) break ;;
  esac
  shift
done

# Preserve only the runner-specific binary setting. App behavior must come
# from the isolated demo config, never from the caller's real environment.
BINARY="${PRMARMOT_DEMO_BINARY:-$ROOT/target/debug/prmarmot}"
unset PRMARMOT_REPO PRMARMOT_REFRESH_SECS PRMARMOT_THEME
unset PRMARMOT_ISSUE_PATTERN PRMARMOT_ISSUE_URL_TEMPLATE
unset PRMARMOT_DEFAULT_REVIEWERS

if [[ ! -x "$BINARY" ]]; then
  printf 'demo: binary is not executable: %s\n' "$BINARY" >&2
  printf 'demo: build it with: cargo build\n' >&2
  exit 1
fi

DEMO_HOME="$(mktemp -d "${TMPDIR:-/tmp}/prmarmot-demo.XXXXXX")"
trap 'rm -rf "$DEMO_HOME"' EXIT INT TERM
mkdir -p "$DEMO_HOME/prmarmot"
if $FAIL_ONCE; then
  touch "$DEMO_HOME/fail-next-graphql"
fi
cat >"$DEMO_HOME/prmarmot/config.toml" <<'TOML'
repo = "demo-labs/atlas"
scope = "all"
repos = ["demo-labs/atlas", "demo-labs/mobile"]
pinned_repos = ["demo-labs/atlas", "demo-labs/mobile"]
refresh_secs = 300
theme = "light"
view = "authored"
default_reviewers = ["alex", "sam"]
automatic_update_checks = true

[issue_link]
pattern = "DEMO-[0-9]+"
url_template = "https://example.com/issues/{id}"

[window]
width = 1440
height = 800
TOML

export XDG_CONFIG_HOME="$DEMO_HOME"
export XDG_STATE_HOME="$DEMO_HOME/state"
export PRMARMOT_DEMO_ATTENTION=1
if $UPDATE_AVAILABLE; then
  export PRMARMOT_DEMO_UPDATE_AVAILABLE=1
fi
if $UPDATE_FAILED; then
  mkdir -p "$XDG_STATE_HOME/prmarmot"
  cat >"$XDG_STATE_HOME/prmarmot/update-receipt.toml" <<'TOML'
completed_at_unix = 1789063200
upgrade_succeeded = false
reopen_succeeded = true
message = "Demo Homebrew upgrade failed; the previous app was reopened safely"
TOML
fi
export PATH="$ROOT/scripts/demo:$PATH"
"$BINARY" "$@"

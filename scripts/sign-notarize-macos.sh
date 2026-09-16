#!/usr/bin/env bash

# Developer-ID sign, notarize, staple, and Gatekeeper-check a staged app.
# This is intentionally fail-closed: there is no release-mode bypass.

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 PATH/TO/prmarmot.app" >&2
  exit 2
fi

app=$1
required=(
  MACOS_SIGN_IDENTITY
  MACOS_NOTARY_KEY_PATH
  MACOS_NOTARY_KEY_ID
  MACOS_NOTARY_ISSUER_ID
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    echo "missing required release environment variable: $name" >&2
    exit 1
  fi
done

[[ -d "$app" ]] || { echo "app bundle does not exist: $app" >&2; exit 1; }
for binary in prmarmot prmarmot-cli; do
  [[ -x "$app/Contents/MacOS/$binary" ]] || {
    echo "app bundle has no executable $binary binary: $app" >&2
    exit 1
  }
done
[[ -f "$MACOS_NOTARY_KEY_PATH" ]] || {
  echo "App Store Connect API key does not exist: $MACOS_NOTARY_KEY_PATH" >&2
  exit 1
}

# Inside-out without --deep: the nested CLI keeps its own identifier, then the
# bundle seals it.
echo "Developer ID signing $app/Contents/MacOS/prmarmot-cli"
codesign --force --timestamp --options runtime \
  --identifier dev.oliverkriska.prmarmot.cli \
  --sign "$MACOS_SIGN_IDENTITY" \
  "$app/Contents/MacOS/prmarmot-cli"
echo "Developer ID signing $app"
codesign --force --timestamp --options runtime \
  --identifier dev.oliverkriska.prmarmot \
  --sign "$MACOS_SIGN_IDENTITY" \
  "$app"
codesign --verify --deep --strict --verbose=2 "$app"

notary_archive="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/prmarmot-notary-$$.zip"
trap 'rm -f "$notary_archive"' EXIT
ditto -c -k --keepParent "$app" "$notary_archive"

echo "Submitting PR Marmot to Apple's notary service"
notary_result=$(
  xcrun notarytool submit "$notary_archive" \
    --key "$MACOS_NOTARY_KEY_PATH" \
    --key-id "$MACOS_NOTARY_KEY_ID" \
    --issuer "$MACOS_NOTARY_ISSUER_ID" \
    --wait \
    --output-format json
)

read -r submission_id submission_status < <(
  python3 -c '
import json
import sys

result = json.load(sys.stdin)
print(result.get("id", "unknown"), result.get("status", "unknown"))
' <<<"$notary_result"
)
echo "Apple notarization $submission_status (submission $submission_id)"
if [[ "$submission_status" != "Accepted" ]]; then
  echo "$notary_result" >&2
  exit 1
fi

xcrun stapler staple "$app"
xcrun stapler validate "$app"
codesign --verify --deep --strict --verbose=2 "$app"
spctl --assess --type execute --verbose=2 "$app"
echo "Developer ID signature, notarization ticket, and Gatekeeper assessment accepted"

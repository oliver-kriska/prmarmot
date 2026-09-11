#!/usr/bin/env bash

# Import release credentials into an ephemeral CI keychain. Secrets are read
# only from the environment and temporary files are removed by workflow cleanup.

set -euo pipefail
umask 077

required=(
  MACOS_SIGN_P12
  MACOS_SIGN_PASSWORD
  MACOS_NOTARY_KEY
  MACOS_NOTARY_KEY_ID
  MACOS_NOTARY_ISSUER_ID
  RUNNER_TEMP
  GITHUB_ENV
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    echo "missing required CI environment variable or repository secret: $name" >&2
    exit 1
  fi
done

keychain="$RUNNER_TEMP/prmarmot-signing.keychain-db"
certificate="$RUNNER_TEMP/prmarmot-signing.p12"
notary_key="$RUNNER_TEMP/AuthKey.p8"
keychain_password=$(openssl rand -hex 24)

printf '%s' "$MACOS_SIGN_P12" | openssl base64 -d -A >"$certificate"
if [[ "$MACOS_NOTARY_KEY" == *"BEGIN PRIVATE KEY"* ]]; then
  printf '%s' "$MACOS_NOTARY_KEY" >"$notary_key"
else
  printf '%s' "$MACOS_NOTARY_KEY" | openssl base64 -d -A >"$notary_key"
fi
grep -q 'BEGIN PRIVATE KEY' "$notary_key" || {
  echo "MACOS_NOTARY_KEY is neither raw nor base64-encoded .p8 content" >&2
  exit 1
}
chmod 600 "$certificate" "$notary_key"

security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$certificate" -k "$keychain" \
  -P "$MACOS_SIGN_PASSWORD" -T /usr/bin/codesign -T /usr/bin/security
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$keychain_password" "$keychain"
security list-keychains -d user -s "$keychain"
rm -f "$certificate"

identity=$(security find-identity -v -p codesigning "$keychain" \
  | awk '/"Developer ID Application:/{print $2; exit}')
[[ -n "$identity" ]] || {
  echo "Imported PKCS#12 contains no Developer ID Application identity" >&2
  exit 1
}

echo "Imported Developer ID identity $identity"
echo "MACOS_SIGN_IDENTITY=$identity" >>"$GITHUB_ENV"
echo "MACOS_NOTARY_KEY_PATH=$notary_key" >>"$GITHUB_ENV"

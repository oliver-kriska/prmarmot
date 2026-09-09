#!/usr/bin/env bash
# bundle-app.sh — assemble a local, ad-hoc-signed prboard.app and install it
# to ~/Applications so Spotlight can launch it.
#
# This is the LOCAL/dev bundle path (hand-rolled .app skeleton). The real
# release pipeline (Zed's cargo-bundle fork + Developer ID + notarization)
# is documented in packaging/RELEASING.md.
#
# Usage: scripts/bundle-app.sh [--stage-only]
#   --stage-only          stop after codesign; leave the .app in target/bundle
#                         (used by release packaging to tar it up)
#   PRBOARD_INSTALL_DIR   override the install destination (default ~/Applications)
# Requires: target/release/prboard (cargo build --release), stock macOS tools.
set -euo pipefail

STAGE_ONLY=0
[[ "${1:-}" == "--stage-only" ]] && STAGE_ONLY=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$REPO_ROOT/target/release/prboard"
STAGE="$REPO_ROOT/target/bundle"
APP="$STAGE/prboard.app"
INSTALL_DIR="${PRBOARD_INSTALL_DIR:-$HOME/Applications}"
BUNDLE_ID="dev.oliverkriska.prboard"

step() { printf '\n==> %s\n' "$*"; }

[[ -x "$BINARY" ]] || {
  echo "error: $BINARY not found or not executable — run 'cargo build --release' first" >&2
  exit 1
}

# --- version ---------------------------------------------------------------
# Parse the [package] version straight from Cargo.toml (read-only; cargo
# pkgid is only a fallback because it may refresh Cargo.lock).
VERSION="$(awk -F'"' '
  /^\[package\]/ { in_pkg = 1; next }
  /^\[/          { in_pkg = 0 }
  in_pkg && /^version[[:space:]]*=/ { print $2; exit }
' "$REPO_ROOT/Cargo.toml")"
if [[ -z "$VERSION" ]]; then
  VERSION="$(cd "$REPO_ROOT" && cargo pkgid 2>/dev/null | sed -n 's/.*[#@]\([0-9][0-9A-Za-z.+-]*\)$/\1/p')"
fi
[[ -n "$VERSION" ]] || { echo "error: could not determine package version" >&2; exit 1; }
step "prboard v$VERSION"

# --- stage skeleton --------------------------------------------------------
step "Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/prboard"

# --- icon -----------------------------------------------------------------
# Generated from assets/branding/icon.svg by scripts/generate-icons.sh.
# Use the committed asset so every local/CI build has the same icon, without
# rasterizer dependencies or a silent fallback to an unbranded application.
cp "$REPO_ROOT/assets/branding/prboard.icns" "$APP/Contents/Resources/prboard.icns"

# --- Info.plist ------------------------------------------------------------
step "Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleName</key>
	<string>prboard</string>
	<key>CFBundleDisplayName</key>
	<string>prboard</string>
	<key>CFBundleExecutable</key>
	<string>prboard</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>12.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>CFBundleIconFile</key>
	<string>prboard</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# --- ad-hoc codesign -------------------------------------------------------
step "Codesigning (ad-hoc)"
codesign --force --deep -s - "$APP"
codesign --verify --deep "$APP"

# --- install ---------------------------------------------------------------
if [[ "$STAGE_ONLY" == 1 ]]; then
  step "Staged (not installed): $APP (v$VERSION, ad-hoc signed)"
  exit 0
fi
step "Installing to $INSTALL_DIR/prboard.app"
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_DIR/prboard.app"
ditto "$APP" "$INSTALL_DIR/prboard.app"

# Nudge LaunchServices so Spotlight picks it up promptly (best-effort).
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[[ -x "$LSREGISTER" ]] && "$LSREGISTER" -f "$INSTALL_DIR/prboard.app" >/dev/null 2>&1 || true

step "Done: $INSTALL_DIR/prboard.app (v$VERSION, ad-hoc signed)"
echo "Launch with Spotlight ('prboard') or: open ~/Applications/prboard.app"

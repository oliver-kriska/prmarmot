#!/usr/bin/env bash
# bundle-app.sh — assemble a local, ad-hoc-signed prmarmot.app and install it
# to /Applications, the same place the Homebrew cask and install.sh use, so a
# machine only ever has one PR Marmot. An older copy left in ~/Applications by
# earlier installs is removed. The terminal/agent CLI ships inside the bundle
# (Contents/MacOS/prmarmot-cli) and an install links it onto PATH, with its
# shell completions (Contents/Resources/completions) for your login shell.
#
# This assembles the same app skeleton used in release CI. Local builds remain
# ad-hoc signed; release CI replaces that signature with Developer ID signing,
# notarizes, staples, and Gatekeeper-checks it (packaging/RELEASING.md).
#
# Usage: scripts/bundle-app.sh [--stage-only]
#   --stage-only          stop after codesign; leave the .app in target/bundle
#                         (used by release packaging to tar it up)
#   PRMARMOT_INSTALL_DIR  override the install destination (default /Applications,
#                         or ~/Applications when /Applications is not writable)
#   PRMARMOT_BIN_DIR      where to link prmarmot-cli (default ~/.local/bin; set it
#                         empty to skip the link)
# Requires: target/release/prmarmot and target/release/prmarmot-cli
# (cargo build --release --workspace), stock macOS tools.
set -euo pipefail

STAGE_ONLY=0
[[ "${1:-}" == "--stage-only" ]] && STAGE_ONLY=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$REPO_ROOT/target/release/prmarmot"
CLI_BINARY="$REPO_ROOT/target/release/prmarmot-cli"
STAGE="$REPO_ROOT/target/bundle"
APP="$STAGE/prmarmot.app"
BUNDLE_ID="dev.oliverkriska.prmarmot"
LEGACY_APP="$HOME/Applications/prmarmot.app"

step() { printf '\n==> %s\n' "$*"; }

# One location for every install path. A standard (non-admin) account cannot
# write /Applications, and neither can Homebrew there, so it keeps a per-user app.
if [[ -n "${PRMARMOT_INSTALL_DIR:-}" ]]; then
  INSTALL_DIR="$PRMARMOT_INSTALL_DIR"
elif [[ -w /Applications ]]; then
  INSTALL_DIR="/Applications"
else
  INSTALL_DIR="$HOME/Applications"
fi

for built in "$BINARY" "$CLI_BINARY"; do
  [[ -x "$built" ]] || {
    echo "error: $built not found or not executable — run 'cargo build --release --workspace' first" >&2
    exit 1
  }
done

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
step "PR Marmot v$VERSION"

# --- stage skeleton --------------------------------------------------------
step "Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/prmarmot"
cp "$CLI_BINARY" "$APP/Contents/MacOS/prmarmot-cli"
# Shell completions for the CLI; the Homebrew cask links them from here.
mkdir -p "$APP/Contents/Resources/completions"
cp "$REPO_ROOT"/cli/completions/prmarmot-cli.{bash,zsh,fish} "$APP/Contents/Resources/completions/"

# --- icon -----------------------------------------------------------------
# Generated from assets/branding/icon.svg by scripts/generate-icons.sh.
# Use the committed asset so every local/CI build has the same icon, without
# rasterizer dependencies or a silent fallback to an unbranded application.
cp "$REPO_ROOT/assets/branding/prmarmot.icns" "$APP/Contents/Resources/prmarmot.icns"

# --- Info.plist ------------------------------------------------------------
# The bundle stays prmarmot.app, but macOS should say "PR Marmot":
# - The menu bar reads CFBundleName.
# - Finder, Launchpad, Spotlight and the Dock read the localized
#   CFBundleDisplayName (en.lproj), and only when LSHasLocalizedDisplayName is
#   set and the plain CFBundleDisplayName equals the file name ("prmarmot").
#   Otherwise they show the file name.
step "Writing Info.plist"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>$BUNDLE_ID</string>
	<key>CFBundleName</key>
	<string>PR Marmot</string>
	<key>CFBundleDisplayName</key>
	<string>prmarmot</string>
	<key>LSHasLocalizedDisplayName</key>
	<true/>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>prmarmot</string>
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
	<string>prmarmot</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null
mkdir -p "$APP/Contents/Resources/en.lproj"
cat > "$APP/Contents/Resources/en.lproj/InfoPlist.strings" <<'STRINGS'
"CFBundleName" = "PR Marmot";
"CFBundleDisplayName" = "PR Marmot";
STRINGS
plutil -lint "$APP/Contents/Resources/en.lproj/InfoPlist.strings" >/dev/null

# --- ad-hoc codesign -------------------------------------------------------
step "Codesigning (ad-hoc)"
# Inside-out, like the release signing: nested code first, then the bundle.
codesign --force -s - --identifier "$BUNDLE_ID.cli" "$APP/Contents/MacOS/prmarmot-cli"
codesign --force -s - "$APP"
codesign --verify --deep --strict "$APP"

# --- install ---------------------------------------------------------------
if [[ "$STAGE_ONLY" == 1 ]]; then
  step "Staged (not installed): $APP (v$VERSION, ad-hoc signed)"
  exit 0
fi
# macOS can SIGKILL an app whose signed binary is swapped, and a running
# instance may be under a memory measurement.
if pgrep -f 'prmarmot.app/Contents/MacOS/prmarmot( |$)' >/dev/null; then
  echo "error: PR Marmot is running — quit it first" >&2
  exit 1
fi
if command -v brew >/dev/null 2>&1 && brew list --cask prmarmot >/dev/null 2>&1; then
  echo "note: this replaces the Homebrew-installed app with a local build;" \
    "'brew upgrade --cask prmarmot' or 'brew reinstall --cask prmarmot' puts a release back"
fi
step "Installing to $INSTALL_DIR/prmarmot.app"
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_DIR/prmarmot.app"
ditto "$APP" "$INSTALL_DIR/prmarmot.app"
INSTALLED_APP="$(cd "$INSTALL_DIR" && pwd -P)/prmarmot.app"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# The staging copy sits in an indexed folder; left behind, Spotlight and
# Launchpad list it as a second PR Marmot. Release packaging uses --stage-only.
[[ -x "$LSREGISTER" ]] && "$LSREGISTER" -u "$APP" >/dev/null 2>&1 || true
rm -rf "$APP"

# Earlier installs defaulted to ~/Applications. Two bundles with one identifier
# show up twice in Spotlight and Launchpad, so drop the older copy — only when it
# really is this app and is not the one just installed.
if [[ -d "$LEGACY_APP" && "$(cd "$LEGACY_APP" && pwd -P)" != "$INSTALLED_APP" ]] \
  && [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$LEGACY_APP/Contents/Info.plist" 2>/dev/null)" == "$BUNDLE_ID" ]]; then
  [[ -x "$LSREGISTER" ]] && "$LSREGISTER" -u "$LEGACY_APP" >/dev/null 2>&1 || true
  rm -rf "$LEGACY_APP"
  # Its CLI link would dangle; the link step below recreates it when wanted.
  if [[ -L "$HOME/.local/bin/prmarmot-cli" \
    && "$(readlink "$HOME/.local/bin/prmarmot-cli")" == "$LEGACY_APP/Contents/MacOS/prmarmot-cli" ]]; then
    rm -f "$HOME/.local/bin/prmarmot-cli"
  fi
  step "Removed the older copy at $LEGACY_APP"
fi

# Nudge LaunchServices so Spotlight picks it up promptly (best-effort).
[[ -x "$LSREGISTER" ]] && "$LSREGISTER" -f "$INSTALL_DIR/prmarmot.app" >/dev/null 2>&1 || true

# --- CLI link --------------------------------------------------------------
# A symlink into the bundle, so every app update updates the CLI too. Never
# replace a real file someone put there (e.g. a cargo-installed copy).
BIN_DIR="${PRMARMOT_BIN_DIR-$HOME/.local/bin}"
if [[ -n "$BIN_DIR" ]]; then
  LINK="$BIN_DIR/prmarmot-cli"
  if [[ -e "$LINK" && ! -L "$LINK" ]]; then
    echo "warning: $LINK exists and is not a symlink; leaving it (the CLI is at $INSTALL_DIR/prmarmot.app/Contents/MacOS/prmarmot-cli)" >&2
  else
    mkdir -p "$BIN_DIR"
    ln -sfn "$INSTALLED_APP/Contents/MacOS/prmarmot-cli" "$LINK"
    step "Linked $LINK"
    case ":$PATH:" in
      *":$BIN_DIR:"*) ;;
      *) echo "note: $BIN_DIR is not on your PATH — add it to use prmarmot-cli" ;;
    esac
  fi
fi

# --- shell completions -----------------------------------------------------
# For your login shell, the same way as the CLI: a symlink into the bundle, never
# over a real file. zsh needs fpath set in ~/.zshrc, so it gets the line to run.
if [[ -n "$BIN_DIR" ]]; then
  COMPLETIONS="$INSTALLED_APP/Contents/Resources/completions"
  case "${SHELL:-}" in
    */bash) SCRIPT=prmarmot-cli.bash
      LINK="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions/prmarmot-cli" ;;
    */fish) SCRIPT=prmarmot-cli.fish
      LINK="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions/prmarmot-cli.fish" ;;
    *) SCRIPT="" ;;
  esac
  if [[ -n "$SCRIPT" ]]; then
    if [[ -e "$LINK" && ! -L "$LINK" ]]; then
      echo "note: $LINK exists and is not a symlink; leaving it"
    else
      mkdir -p "$(dirname "$LINK")"
      ln -sfn "$COMPLETIONS/$SCRIPT" "$LINK"
      step "Linked $LINK (shell completions)"
    fi
  elif [[ "${SHELL:-}" == */zsh ]]; then
    echo "zsh completions: mkdir -p ~/.zfunc && prmarmot-cli completions zsh > ~/.zfunc/_prmarmot-cli"
    echo "  (with fpath=(~/.zfunc \$fpath) before compinit in ~/.zshrc)"
  fi
fi

step "Done: $INSTALL_DIR/prmarmot.app (v$VERSION, ad-hoc signed)"
echo "Launch with Spotlight ('PR Marmot') or: open '$INSTALLED_APP'"
echo "Terminal and agents: prmarmot-cli --help"

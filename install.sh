#!/bin/sh
# PR Marmot installer — designed to be curl-piped:
#
#   curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh | sh -s -- --from-source
#
# Default: download the latest signed and notarized Apple-silicon macOS app.
# Source builds remain locally ad-hoc signed. Either way the terminal/agent CLI
# is linked as ~/.local/bin/prmarmot-cli (--bin-dir to change; --bin-dir "" to skip).
set -eu

REPO="oliver-kriska/prmarmot"
FROM_SOURCE=0
DIR="${PRMARMOT_INSTALL_DIR:-}"
DEFAULT_REPO="${PRMARMOT_REPO:-}"
BIN_DIR="${PRMARMOT_BIN_DIR-$HOME/.local/bin}"

while [ $# -gt 0 ]; do
  case "$1" in
    --from-source) FROM_SOURCE=1 ;;
    --dir) DIR="${2:?--dir needs a path}"; shift ;;
    --repo) DEFAULT_REPO="${2:?--repo needs owner/name}"; shift ;;
    --bin-dir) [ $# -ge 2 ] || { echo "--bin-dir needs a path" >&2; exit 2; }; BIN_DIR="$2"; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

OS="$(uname -s)"
ARCH="$(uname -m)"
say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

config_path() {
  case "${XDG_CONFIG_HOME:-}" in
    /*) printf '%s/prmarmot/config.toml\n' "$XDG_CONFIG_HOME" ;;
    *) printf '%s/.config/prmarmot/config.toml\n' "$HOME" ;;
  esac
}

configure_repo() {
  CONFIG=$(config_path)
  [ ! -e "$CONFIG" ] || return 0
  # Leave the destination absent so the app can migrate the complete legacy
  # config directory on first launch, rather than hiding it with a fresh file.
  CONFIG_ROOT=${CONFIG%/prmarmot/config.toml}
  [ ! -d "$CONFIG_ROOT/prboard" ] || return 0

  CANDIDATE="$DEFAULT_REPO"
  if [ -z "$CANDIDATE" ] && command -v gh >/dev/null 2>&1; then
    CANDIDATE=$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null || true)
  fi
  if [ -z "$CANDIDATE" ] && [ -t 1 ] && [ -r /dev/tty ]; then
    printf 'Default GitHub repo for PR Marmot (owner/name, or Enter to skip): ' >/dev/tty
    IFS= read -r CANDIDATE </dev/tty || CANDIDATE=""
  fi
  [ -n "$CANDIDATE" ] || return 0

  command -v gh >/dev/null 2>&1 || {
    printf 'warning: cannot validate repo without the GitHub CLI; config not written\n' >&2
    return 0
  }
  CANONICAL=$(gh repo view "$CANDIDATE" --json nameWithOwner --jq .nameWithOwner 2>/dev/null || true)
  [ -n "$CANONICAL" ] || {
    printf 'warning: cannot access GitHub repo %s; config not written\n' "$CANDIDATE" >&2
    return 0
  }

  mkdir -p "${CONFIG%/*}"
  (umask 077 && printf 'repo = "%s"\n' "$CANONICAL" >"$CONFIG")
  say "Configured $CANONICAL in $CONFIG"
}

on_path() {
  case ":$PATH:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Symlink the CLI that ships inside the app bundle, so app updates update it.
# Never replaces a real file (e.g. a cargo-installed copy).
link_cli() {
  [ -n "$BIN_DIR" ] || return 0
  TARGET="$(cd "$1" 2>/dev/null && pwd)/prmarmot.app/Contents/MacOS/prmarmot-cli"
  [ -x "$TARGET" ] || {
    say "This release has no bundled prmarmot-cli yet; not linking it"
    return 0
  }
  LINK="$BIN_DIR/prmarmot-cli"
  if [ -e "$LINK" ] && [ ! -L "$LINK" ]; then
    printf 'warning: %s exists and is not a symlink; left as is (the CLI is at %s)\n' "$LINK" "$TARGET" >&2
    return 0
  fi
  mkdir -p "$BIN_DIR"
  ln -sfn "$TARGET" "$LINK"
  say "Linked $LINK"
  on_path "$BIN_DIR" || printf 'note: %s is not on your PATH — add it to use prmarmot-cli\n' "$BIN_DIR"
}

post_install_notes() {
  CONFIG=$(config_path)
  if ! command -v gh >/dev/null 2>&1; then
    cat <<'EOF'

PR Marmot needs the GitHub CLI. Install it, then authenticate:

    brew install gh
    gh auth login

EOF
  elif ! gh auth status >/dev/null 2>&1; then
    cat <<'EOF'

Authenticate the GitHub CLI before launching PR Marmot:

    gh auth login

EOF
  fi
  if [ ! -e "$CONFIG" ]; then
    cat <<EOF
PR Marmot starts in All repositories scope. Optional config: $CONFIG

EOF
  fi
}

install_release_macos() {
  [ "$ARCH" = "arm64" ] || die "prebuilt releases are Apple-silicon only for now; use --from-source"
  command -v shasum >/dev/null 2>&1 || die "shasum not found"
  command -v spctl >/dev/null 2>&1 || die "macOS Gatekeeper tool not found"
  DIR="${DIR:-$HOME/Applications}"
  say "Finding the latest stable release of $REPO"
  RELEASE=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest") \
    || die "could not query the latest GitHub release"
  URL=$(printf '%s\n' "$RELEASE" \
    | grep -o '"browser_download_url": *"[^"]*prmarmot-v[^"]*-macos-arm64\.tar\.gz"' \
    | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')
  [ -n "$URL" ] || die "no signed macOS arm64 release asset found; use --from-source"

  TMP=$(mktemp -d "${TMPDIR:-/tmp}/prmarmot-install.XXXXXX")
  trap 'rm -rf "$TMP"' EXIT
  ASSET=${URL##*/}
  say "Downloading $ASSET and published checksum"
  curl -fL --progress-bar "$URL" -o "$TMP/$ASSET" || die "release download failed"
  curl -fsSL "$URL.sha256" -o "$TMP/$ASSET.sha256" || die "checksum download failed"
  (cd "$TMP" && shasum -a 256 -c "$ASSET.sha256") || die "release checksum verification failed"

  tar xzf "$TMP/$ASSET" -C "$TMP" || die "could not extract release archive"
  [ -d "$TMP/prmarmot.app" ] || die "unexpected archive layout (no prmarmot.app)"
  [ -x "$TMP/prmarmot.app/Contents/MacOS/prmarmot" ] || die "archive has no prmarmot executable"
  codesign --verify --deep --strict "$TMP/prmarmot.app" || die "Developer ID signature verification failed"
  # Gatekeeper checks notarization without requiring Xcode command-line tools
  # on end-user machines. CI validates the stapled ticket before publication.
  spctl --assess --type execute --verbose=2 "$TMP/prmarmot.app" || die "Gatekeeper rejected the app"

  if pgrep -f 'prmarmot.app/Contents/MacOS/prmarmot( |$)' >/dev/null 2>&1; then
    die "PR Marmot is running; quit it before upgrading"
  fi
  say "Installing to $DIR/prmarmot.app"
  mkdir -p "$DIR"
  rm -rf "$DIR/prmarmot.app"
  mv "$TMP/prmarmot.app" "$DIR/prmarmot.app"
  LSREG="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  [ -x "$LSREG" ] && "$LSREG" -f "$DIR/prmarmot.app" >/dev/null 2>&1 || true
  link_cli "$DIR"
  configure_repo
  say "Done — launch 'PR Marmot' from Spotlight, or: open '$DIR/prmarmot.app'"
  post_install_notes
}

install_from_source() {
  command -v git >/dev/null 2>&1 || die "git is required for --from-source"
  command -v cargo >/dev/null 2>&1 || die "Rust (cargo) is required for --from-source — https://rustup.rs"
  TMP=$(mktemp -d "${TMPDIR:-/tmp}/prmarmot-src.XXXXXX")
  trap 'rm -rf "$TMP"' EXIT
  say "Cloning $REPO (main)"
  git clone --depth 1 "https://github.com/$REPO" "$TMP/prmarmot"
  say "Building release binaries (a few minutes; LTO)"
  (cd "$TMP/prmarmot" && cargo build --release --workspace)
  if [ "$OS" = "Darwin" ]; then
    DIR="${DIR:-$HOME/Applications}"
    # bundle-app.sh links the CLI itself.
    PRMARMOT_INSTALL_DIR="$DIR" PRMARMOT_BIN_DIR="$BIN_DIR" "$TMP/prmarmot/scripts/bundle-app.sh"
  else
    DIR="${DIR:-$HOME/.local/bin}"
    say "Installing prmarmot and prmarmot-cli to $DIR"
    mkdir -p "$DIR"
    install -m 755 "$TMP/prmarmot/target/release/prmarmot" "$DIR/prmarmot"
    install -m 755 "$TMP/prmarmot/target/release/prmarmot-cli" "$DIR/prmarmot-cli"
    if on_path "$DIR"; then say "Done"; else say "Done — make sure $DIR is on your PATH"; fi
  fi
  configure_repo
  post_install_notes
}

if [ "$FROM_SOURCE" = 1 ]; then
  install_from_source
elif [ "$OS" = "Darwin" ]; then
  install_release_macos
else
  die "no prebuilt Linux packages yet — rerun with --from-source"
fi

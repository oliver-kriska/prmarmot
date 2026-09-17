#!/usr/bin/env bash
# Build PRMarmotCore.xcframework from the `ffi/` crate.
#
# Own script rather than cargo-swift or cargo-xcframework: the whole job is
# three cargo builds, one lipo, one bindgen run and one xcodebuild call, and
# the parts that go wrong (deployment target drift between slices, a modulemap
# named anything but module.modulemap, a bindgen binary built against a
# different uniffi than the library) are exactly the parts a wrapper hides.
#
# Usage:
#   scripts/build-xcframework.sh [--out DIR] [--debug] [--device-only]
#
# Output: $OUT/PRMarmotCore.xcframework and, with --zip, a zip plus its
# Swift Package Manager checksum.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

# The .xcframework's file name, and the SwiftPM target that wraps it.
FRAMEWORK_NAME="PRMarmotCore"
# The modulemap's module name. It has to be exactly what the generated Swift
# imports (`#if canImport(prmarmot_ffiFFI)`), which uniffi derives from the
# crate name — not from anything we choose. Name it anything else and the
# framework builds, links, and then every call is a missing symbol.
MODULE_NAME="prmarmot_ffiFFI"
LIB_NAME="libprmarmot_ffi.a"
OUT="$ROOT/target/xcframework"
PROFILE="ios"
PROFILE_DIR="ios"
MAKE_ZIP=0
# 17.2 is the floor the iPad app targets; every slice must agree or the
# linker warns and Xcode picks a slice you did not mean.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.2}"

TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios)

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --debug) PROFILE="dev"; PROFILE_DIR="debug"; shift ;;
    --device-only) TARGETS=(aarch64-apple-ios); shift ;;
    --zip) MAKE_ZIP=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

command -v xcodebuild >/dev/null || { echo "xcodebuild is required" >&2; exit 1; }

say "Rust targets"
for target in "${TARGETS[@]}"; do
  rustup target add "$target" >/dev/null
done

say "Building prmarmot-ffi for: ${TARGETS[*]}"
for target in "${TARGETS[@]}"; do
  # -C debuginfo=0 on top of the release profile: the static library is
  # linked into an app that ships its own dSYM, and Rust debug info is the
  # single largest thing in an unstripped slice.
  RUSTFLAGS="${RUSTFLAGS:-} -C debuginfo=0" \
    cargo build --locked --profile "$PROFILE" -p prmarmot-ffi --target "$target"
done

say "Generating Swift bindings"
BINDINGS="$ROOT/target/xcframework-bindings"
rm -rf "$BINDINGS"
mkdir -p "$BINDINGS"
# The generator reads metadata out of a built library, so it must be the
# library this run produced, from the same uniffi version. Build it for the
# host, where it can be loaded — `panic = "abort"` makes the iOS profile
# unsuitable for a host cdylib, so the host build stays on `release`.
cargo build --locked --release -p prmarmot-ffi
HOST_LIB="$ROOT/target/release/libprmarmot_ffi.dylib"
[[ -f $HOST_LIB ]] || HOST_LIB="$ROOT/target/release/libprmarmot_ffi.a"

# Note: NOT `--xcframework`. That flag emits `framework module ...`, which is
# for an XCFramework built out of real .frameworks. This one is built out of a
# static library plus a headers directory (`xcodebuild -library -headers`), and
# Swift only finds the C symbols if the modulemap declares a plain `module`.
# With the wrong one everything builds and every generated type is "not in
# scope" in the consuming target.
cargo run --locked -p prmarmot-ffi --features prmarmot-ffi/bindgen \
  --bin uniffi-bindgen-swift -- \
  --swift-sources --headers --modulemap \
  --module-name "$MODULE_NAME" \
  --modulemap-filename module.modulemap \
  "$HOST_LIB" "$BINDINGS"

# Xcode only accepts a headers directory whose modulemap is called exactly
# `module.modulemap`; anything else builds here and fails in the app.
[[ -f "$BINDINGS/module.modulemap" ]] || {
  echo "expected $BINDINGS/module.modulemap" >&2; exit 1; }

# The headers directory that goes into the XCFramework holds the C header and
# the modulemap and nothing else. A stray .swift in there is compiled twice —
# once as a header-adjacent source and once as the Swift target — and the
# second copy cannot see the first one's symbols.
HEADERS="$ROOT/target/xcframework-headers"
rm -rf "$HEADERS"
mkdir -p "$HEADERS"
cp "$BINDINGS"/*.h "$BINDINGS/module.modulemap" "$HEADERS/"

say "Assembling slices"
SLICES="$ROOT/target/xcframework-slices"
rm -rf "$SLICES"
mkdir -p "$SLICES/ios-arm64" "$SLICES/ios-sim"

cp "$ROOT/target/aarch64-apple-ios/$PROFILE_DIR/$LIB_NAME" "$SLICES/ios-arm64/$LIB_NAME"

# Only the simulator targets this run asked for. Picking up a stale slice
# from a previous full build would ship an XCFramework whose simulator half
# is older than its device half.
SIM_LIBS=()
for target in "${TARGETS[@]}"; do
  case "$target" in
    *-ios-sim | x86_64-apple-ios) SIM_LIBS+=("$ROOT/target/$target/$PROFILE_DIR/$LIB_NAME") ;;
  esac
done
if [[ ${#SIM_LIBS[@]} -gt 0 ]]; then
  # One simulator slice for both architectures; an XCFramework may not carry
  # two entries with the same platform+variant.
  lipo -create "${SIM_LIBS[@]}" -output "$SLICES/ios-sim/$LIB_NAME"
fi

say "Building $FRAMEWORK_NAME.xcframework"
mkdir -p "$OUT"
rm -rf "${OUT:?}/$FRAMEWORK_NAME.xcframework"
ARGS=(-create-xcframework)
ARGS+=(-library "$SLICES/ios-arm64/$LIB_NAME" -headers "$HEADERS")
if [[ -f "$SLICES/ios-sim/$LIB_NAME" ]]; then
  ARGS+=(-library "$SLICES/ios-sim/$LIB_NAME" -headers "$HEADERS")
fi
ARGS+=(-output "$OUT/$FRAMEWORK_NAME.xcframework")
xcodebuild "${ARGS[@]}"

# The .swift files are sources for the app, not part of the framework. They go
# next to the xcframework and into the local Swift package that `apple/`
# builds, both of which are build output and both gitignored.
mkdir -p "$OUT/Sources" "$ROOT/apple/Sources/PRMarmotCore"
cp "$BINDINGS"/*.swift "$OUT/Sources/"
cp "$BINDINGS"/*.swift "$ROOT/apple/Sources/PRMarmotCore/"

# The smoke test runs against the same fixtures the Rust goldens use. Copying
# them in rather than committing a second set is the only way the two can be
# guaranteed to still be the same file a year from now.
mkdir -p "$ROOT/apple/Tests/SmokeTests/Fixtures"
cp "$ROOT"/core/tests/fixtures/*.json "$ROOT/apple/Tests/SmokeTests/Fixtures/"

say "Sizes"
for target in "${TARGETS[@]}"; do
  lib="$ROOT/target/$target/$PROFILE_DIR/$LIB_NAME"
  [[ -f $lib ]] && printf '  %-24s %s\n' "$target" "$(du -h "$lib" | cut -f1)"
done
printf '  %-24s %s\n' "xcframework" "$(du -sh "$OUT/$FRAMEWORK_NAME.xcframework" | cut -f1)"

if [[ $MAKE_ZIP -eq 1 ]]; then
  say "Zipping"
  (cd "$OUT" && rm -f "$FRAMEWORK_NAME.xcframework.zip" &&
     zip -qry "$FRAMEWORK_NAME.xcframework.zip" "$FRAMEWORK_NAME.xcframework")
  CHECKSUM=$(swift package compute-checksum "$OUT/$FRAMEWORK_NAME.xcframework.zip")
  echo "$CHECKSUM" > "$OUT/$FRAMEWORK_NAME.xcframework.zip.checksum"
  echo "  checksum $CHECKSUM"
fi

say "Done: $OUT/$FRAMEWORK_NAME.xcframework"
echo "  Swift sources: $OUT/Sources"

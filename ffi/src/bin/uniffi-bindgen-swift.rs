//! The Swift bindings generator, as a binary inside this crate so it always
//! matches the `uniffi` version the library was compiled with. A mismatch
//! between generator and runtime is the classic UniFFI footgun.
//!
//! Built only with `--features bindgen`, so the iOS staticlib does not link it.
//! `scripts/build-xcframework.sh` is the only caller.

fn main() {
    uniffi::uniffi_bindgen_swift()
}

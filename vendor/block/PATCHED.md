# block 0.1.6 (patched)

Upstream: <https://github.com/SSheldon/rust-block> (MIT, last release 2016,
unmaintained). Pulled in only on macOS by GPUI (`gpui-pre-macos`/`gpui-pre-apple`
via `cocoa` 0.26 and `core-video`); `gpui-pre` 0.3.5 still depends on it.

Wired in through `[patch.crates-io]` in the root `Cargo.toml`.

## The changes

`src/lib.rs` declared `enum Class { }` and used it as the type of the extern
static `_NSConcreteStackBlock`. rustc reports this as a future-incompatibility
("static of uninhabited type", rust-lang/rust#74840) that will become a hard
error. The patch replaces the empty enum with an opaque `#[repr(C)]` zero-sized
struct. It is only ever used behind `*const Class`, so the ABI is unchanged.

Bare `extern` blocks and `extern fn` pointer types are spelled `extern "C"`.
A bare `extern` already means the C ABI; rustc deprecates leaving it implicit,
and warnings in a path dependency are not capped the way registry ones are.

Everything else is the crates.io 0.1.6 source as published, minus the
`objc_test_utils` dev-dependency (upstream test helper, not shipped).

Drop this directory and the patch entry once GPUI stops depending on `block`.

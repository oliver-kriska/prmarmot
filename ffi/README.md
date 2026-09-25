# prmarmot-ffi

PR Marmot's core, as a Swift library. This crate is what lets the iPad app be a
*front end* rather than a second implementation: categorization, Note text,
section order, pickup age, the size band, the search grammar and the "changed
since you looked" rule all stay in `prmarmot-core`, pinned by golden tests
against the shell prototype, and the app calls into them.

Build it with `scripts/build-xcframework.sh`, which produces
`PRMarmotCore.xcframework` plus the generated Swift sources.

## The cycle rule — read this before writing a transport

UniFFI's foreign traits are reference-counted on **both** sides. `BoardClient`
holds your `GithubTransport` and `TokenSource` strongly, because Rust has to be
able to call them. If either of them holds the `BoardClient` strongly in return,
the two keep each other alive forever: ARC cannot see the Rust half of the
cycle, and Rust cannot see the Swift half, so nothing ever collects it. The
symptom is an app whose memory grows by one board's worth of rows every time
the user signs out and back in — the same class of bug that killed the earlier
GPUI prototype (`SOLUTIONS_EXTRACTED.md`).

The rule is one word long: **`weak`**.

```swift
final class URLSessionTransport: GithubTransport {
    // Never `let client: BoardClient`. The client owns this object.
    weak var client: BoardClient?

    func send(request: GraphqlRequest) async throws -> HttpResponse {
        var urlRequest = URLRequest(url: URL(string: request.url)!)
        urlRequest.httpMethod = "POST"
        urlRequest.httpBody = Data(request.body.utf8)
        urlRequest.setValue("Bearer \(request.token)", forHTTPHeaderField: "Authorization")
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.setValue("application/json", forHTTPHeaderField: "Accept")
        // GitHub answers a request with no User-Agent with a silent 403.
        urlRequest.setValue(request.userAgent, forHTTPHeaderField: "User-Agent")

        do {
            let (data, response) = try await URLSession.shared.data(for: urlRequest)
            let http = response as! HTTPURLResponse
            return HttpResponse(
                status: UInt16(http.statusCode),
                headers: http.allHeaderFields.compactMap { key, value in
                    guard let name = key as? String else { return nil }
                    return Header(name: name, value: "\(value)")
                },
                body: String(data: data, encoding: .utf8) ?? ""
            )
        } catch {
            // Only when there was no answer at all. Any answer, including 401,
            // 403 and 502, belongs in HttpResponse — Rust knows what they mean.
            throw FfiError.Network(message: error.localizedDescription)
        }
    }
}
```

`apple/Smoke/` asserts both halves: that the transport's `weak` reference goes
`nil` when the client is released, and that `BoardClient` releases the transport
(`ffi/tests/offline_board.rs` asserts the Rust half with reference counts).

## What crosses the boundary

| Direction | What |
|---|---|
| Swift → Rust | `GithubTransport.send`, `AuthTransport.postForm`, `TokenSource.token` — all `async`, all throwing `FfiError`. Nothing else. |
| Rust → Swift | `BoardClient` (`fetchBoard`, `loadMore`, `hasMore`, `viewerLogin`, `reset`), `DeviceFlow` (`start`, `poll`, `refresh`, `verificationUrl`, `clientIdIsPlaceholder`), `AttentionStore`, and the pure functions `layout`, `search`, `takeFilterChips`, `withFilter`, `shareGroup`, `waitingSecs`, `waitLabel`, `sizeBand`, `sizeBandLabel`, `sizeLinesAndFiles`, `unresolvedLabel`, `detailItems`, `attentionLine`, `snoozeChoiceLabel`, `cancelSnoozeLabel`, `groupLabel`, `defaultBoardSettings`, `tokenFromPat`, `tokenNeedsRefresh`, `tokenCanRefresh`, `backoffSecs`, `rateLimitedWaitSecs`, `reservePauseUntil`, `rateLimitReserve`, `coreVersion`, `coreBuild`. |

`reset` is safe to call while a fetch is in flight: a page that was already on
its way when the account changed is dropped rather than remembered, so a
`loadMore` after the reset can never continue from the old account's cursor. A
Swift method that throws something other than `FfiError` reaches Rust as
`FfiError.Network` with the error's description; it never aborts the process.

Record and field names mirror `cli/schema/board-v1.schema.json`
(`prmarmot-cli/board@1`), so that published schema doubles as the contract
documentation for this boundary. Three fields on `PullRequest` are derived
rather than stored — `stale`, `waitingSecs` and `waitLabel` are computed from
the `nowEpoch` passed into the fetch — and converting a `PullRequest` back into
a Rust row ignores them.

## Deliberate omissions

- **No HTTP.** `prmarmot-core`'s `http` feature is off here, so an iOS binary
  never links a second TLS stack. Swift's `URLSession` does the I/O and this
  crate hands it a finished request: the URL, the token, the JSON body and the
  User-Agent. Building the GraphQL body — including the typed `tracked: [ID!]!`
  array — and deciding what a 403 means stay in Rust, in
  `prmarmot_core::github::response`, tested once for every front end.
- **No files.** `AttentionStore` serializes to bytes and restores from bytes.
  Where they live is Swift's business.
- **No clock.** Every entry point that needs "now" takes Unix seconds.
  `core/clippy.toml` and `core/tests/no_clock.rs` enforce the other half.
  `AttentionStore.snooze` and `wakeDue` clamp a clock outside 1970–9999 to
  that range instead of falling back to the device's own clock, so an absurd
  clock can neither overflow a deadline nor write one the store cannot read
  back; the other entry points that take a clock throw `FfiError.Invalid` for
  an instant that does not exist.
- **No async runtime.** See below.

## Which build is this

`coreVersion()` is the crate's version and rarely moves. `coreBuild()` is what
tells two XCFrameworks apart: `ffi/build.rs` writes the short commit, whether
`core/`, `local/`, `ffi/` or `Cargo.lock` had uncommitted changes, and the
profile into the library when it compiles, and `description` puts them on one
line (`0.1.0 (1a2b3c4d5e6f-dirty, release)`) for an About screen or a bug
report. Built outside a git checkout, the commit reads `unknown` and `dirty` is
`nil`.

## The GitHub budget

`reservePauseUntil(rate, nowEpoch)` is the desktop's rule for leaving
`rateLimitReserve()` (50) points of the hourly budget to the person's own `gh`
and git, from the same core function the desktop calls: given the last board's
`RateLimit`, it returns the second to wait until (one to fifteen minutes away)
or `nil` to fetch. A reset of 0 is unknown and never pauses.

A request GitHub refused comes back as `FfiError.rateLimited(resetEpoch:
retryAfterSecs:)`: the budget's reset (also read from a 200 carrying a GraphQL
`RATE_LIMITED` error) when the budget is what ran out, and a secondary limit's
`retry-after`. Wait `rateLimitedWaitSecs(resetEpoch:retryAfterSecs:nowEpoch:)`
seconds — `retry-after` when GitHub sent it, otherwise the reset, between one
and fifteen minutes — as the desktop does.

## How blocking core meets async Swift

Core's `GithubTransport` is blocking by design — one request per refresh, run
off the UI thread, no runtime anywhere. UniFFI's foreign traits must be `async`,
because a Swift implementation that blocked would block whatever thread UniFFI
called it on, and `URLSession` is callback-based regardless.

`transport.rs` joins the two with a worker thread and two channels: the thread
runs core's own `fetch_board_scoped` unchanged, and an async loop on this side
ferries each request out to Swift and the answer back. No runtime, no
`block_on`, and no shared executor to deadlock. One thread per fetch, alive
only for the length of that fetch.

## Panics abort

The `ios` profile in the workspace `Cargo.toml` sets `panic = "abort"`, which
is a large part of why the linked library is under a megabyte. The consequence
is that a Rust panic ends the app instead of surfacing to Swift as a thrown
error. Core is written to return errors rather than panic and is heavily
tested, so this is a size trade rather than a robustness one — but if a panic
ever does reach a user, change `panic` to `"unwind"` in `[profile.ios]` and
UniFFI will turn it into a thrown `FfiError` again, at the cost of the bytes.

## Size

Measured on `aarch64-apple-ios`, `--profile ios`, as the fully linked
`libprmarmot_ffi.dylib` (the `.a` is an unstripped archive and says nothing
useful):

| Build | Linked bytes |
|---|---|
| `regex` | 1,638,520 |
| `regex-lite` (`small-regex`) | 830,264 |
| `regex-lite`, plus `prmarmot-local` for watch, snooze and `config.toml` | 1,288,004 |

The third row is what the app links today. Depending on `prmarmot-local` costs
447 KiB — almost all of it the `toml` crate — and buys the desktop's watch
bound, its snooze conditions, its state-file validation and its config format
rather than a Swift re-reading of any of them. A second implementation of those
rules would cost more than 447 KiB in bugs.

789 KiB is worth having on a phone and worth nothing on a desktop, so
`small-regex` is a `prmarmot-core` feature that only this crate turns on. The
two engines differ only in syntax PR Marmot does not use: `regex-lite` has no
Unicode character classes (`\p{...}`), and its `\w`, `\d` and `\b` are ASCII.
Because the issue-link pattern comes from a user's `config.toml`, the desktop
and the CLI stay on full `regex`, and CI runs the entire core suite under both.

## Regenerating the bindings

```sh
scripts/build-xcframework.sh              # device + both simulators
scripts/build-xcframework.sh --device-only --debug   # fast iteration
scripts/build-xcframework.sh --zip        # + zip and SwiftPM checksum
```

The generator is a binary *inside this crate* (`--features bindgen`), so it can
never be built against a different `uniffi` than the library it describes —
the classic UniFFI footgun. The generated modulemap must be named exactly
`module.modulemap`; anything else builds here and fails in Xcode.

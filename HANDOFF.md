# PR Marmot — Project Handoff

**Name:** PR Marmot (`prmarmot`). **Owner:** Oliver Kriška. **License:** MIT, public, open source. **Platforms:** macOS + Linux. **Created from research on:** 2026-07-24. The project was called `prboard` through v0.5.3; historical entries below retain that name where it records what existed then. **"PRFlow" / `~/Projects/pr_flow` throughout this file is Oliver's own unreleased 2025 Rust+GPUI prototype (the predecessor whose memory bug shaped the guardrails), not the commercial product PR Flow at prflow.app.**

> **Read this first.** It's the executive summary of a research spike for a new project: an open-source **GitHub PR-review dashboard in Rust**. It distills five detailed research files (linked at the bottom) into a recommendation, a risk list, a v1 scope cut, and a build roadmap. A fresh session should be able to start building from this file alone. The deeper docs are there when you need the evidence behind a claim.
>
> Everything is tagged **FACT** (verified, sourced), **ASSESSMENT** (my judgement — challenge it), or **OPEN QUESTION** (unverified — with a way to answer it). Oliver said explicitly: don't fabricate. Where a number couldn't be verified, it's an OPEN QUESTION, not a guess.

---

## Current update — 2026-09-25

**FACT: three fixes from the iPad's round-5 review, unreleased and
uncommitted until Oliver tests them.** The desktop's behaviour does not change.

- **The budget pause is core's.** `rate_limit::reserve_pause_until(remaining,
  reset, now)` (and `RateLimitInfo::pause_until`) is the rule `refresh()` and
  `load_more()` used to spell out in `src/state.rs`: fewer than 50 points left
  and a reset still ahead → wait until the reset, clamped to 60..900 s; an
  unknown or past reset never pauses. The desktop now calls it, and ffi exports
  it as `reserve_pause_until(RateLimit, now)` plus `rate_limit_reserve()`.
  Boundary tests in core and ffi. `cli/src/watch.rs` keeps its own backoff,
  which reacts to a failed fetch, not to the reserve.
- **`core_build()`** (ffi): `ffi/build.rs` embeds the short commit, whether
  `core/`, `local/`, `ffi/` or `Cargo.lock` was dirty, and the profile; outside
  a git checkout they read "unknown". No new dependency. `core_version()` stays.
- **The attention store's clock is clamped, not replaced.** `snooze` and
  `wake_due` used to fall back to `Utc::now()` for an unrepresentable clock and
  could overflow `now + 24 h`; they now clamp to 1970–9999 (so a deadline
  always writes and reads back) with checked arithmetic. A clamp rather than an
  error keeps the Swift signatures the iPad already calls.

## Current update — 2026-09-22

**FACT: the section order is the user's to set, in config.toml and in
Settings; unreleased.** Oliver: different people have different workflows,
e.g. someone who assigns reviewers cares most about what nobody was asked to
review. One order covers every view (each view shows the sections it has in
it), following his same-day ruling that a section has one name everywhere.

- **Core owns it:** `layout::SectionOrder`, a permutation of
  `ORDERABLE_SECTIONS` (approved, todo, action, available, await, done,
  draft — the JSON section keys). `from_keys` puts the named sections first
  and the rest in default order, and names every unknown or repeated key and
  `snoozed` (always last) as an ignored value. `layout_ordered` takes it;
  `layout` is the default order. `section_name` / `section_views` are the
  Settings list's words. Snoozed and stack sub-headers are never reordered,
  and nothing is hidden, so counts, the badge and notifications are
  unchanged.
- **File:** `section_order = ["available", "await"]` (`prmarmot_local::config::
  section_order`); bad entries join the ignored-values banner. Settings writes
  it only when it was changed there, so a hand-written partial list stays as
  written, and removes it on Reset to default.
- **Desktop:** Settings → Section order, up/down buttons per section and
  Reset to default; a save redraws the rows on screen without a fetch. The
  Settings scroll targets for invalid fields were counted before the header
  and Account existed; they point at the right fields now.
- **CLI** prints its views in the same order (`BoardView::sections`), and
  JSON consumers are told to find sections by `key`.
- **ffi** is additive: `layout_ordered(…, section_order)`,
  `parse_section_order`, `default_section_order`, `move_section`,
  `section_order_entries`, and `AppConfig.section_order` (default `[]`), so
  the iPad can offer the same list (its Settings control is the iPad
  worker's).
- ASSESSMENT — G0: no refresh-loop, repaint or cache change; not a re-run
  trigger.

**FACT: every header number is the open view's; unreleased.** Oliver asked
for the counts to reflect the tab that is open, and chose both rules
(2026-09-22):

- **"Need you" is the Dock badge's rule in every view:** your own PRs under
  Needs action plus Requested from you (`status::row_needs_you`, which no
  longer looks at the mode; your rows are the ones with blockers). On his
  data that took the Review queue from 18 to 0 (18 PRs under Available to
  review, none requested) and Involving me from 39 to 34 (5 other people's
  PRs under Needs action). My PRs and All open are unchanged.
  `needs_you_here(Review, Available)` is false now. The desktop badge calls
  `row_needs_you` too, so the header and badge can't drift apart; its count
  is unchanged.
- **"N watched/snoozed" counts this view's rows** you watch or snoozed
  (`HeaderCounts::followed`, ffi `followed` with default 0). The app-wide
  rotation (`tracked_loaded`/`tracked_total`, up to 50 per refresh) moved
  into the explanation: "Each refresh also checks 50 of the 64 PRs you watch
  or snoozed, in any view, taking turns."
- **All open's observe rule moved to local** for the iPad (its worker asked;
  retyping it in Swift would break parity): `AttentionState::observes(mode,
  row)` — every row outside All open; there only PRs with a snapshot, watched,
  Requested from you, or yours by the state's account. The desktop calls it;
  ffi exports `AttentionStore.observes` and `rows_to_observe`.

## Current update — 2026-09-21

**FACT: View 1 of the original spec, "All open PRs", exists now as a third
view, All open (`Mode::AllOpen`, key `3`, `prmarmot-cli all`); unreleased.**
It was decided v1 scope on 2026-07-24 and never built; Oliver asked for it
again on 2026-09-21. Design and measurements:
`.claude/research/2026-09-21-all-open-prs-view.md` (local-only).

- **One repository only.** `repo:X is:pr is:open sort:updated-desc`, 60 per
  page, Load more up to five pages (300 rows), `issueCount` for the header
  ("60 of 759 open"). With All repositories the tab is disabled and says why;
  core returns `GhError::NeedsRepository` rather than searching everything.
- **Filters go to GitHub.** FACT (zed-industries/zed, 2026-09-21): filtering
  the 60 loaded rows by `label:"area:editor"` showed 7 PRs while GitHub has 40.
  So in this view `label:` and `author:` chips are pushed into the search
  string (`core::search::RemoteFilter`, at most 8 of each, labels quoted,
  authors sent as `author:x author:app/x` because a bot's login only matches
  as `app/x`), debounced 400 ms, one request per change. Words, `is:stale`
  and anything past the cap stay local, and a line under the header says
  they only checked the loaded rows. The local grammar changed with it:
  several `author:` / `repo:` terms now match any one (GitHub's reading),
  labels and words still all; one test runs one chip set through both paths.
- **Sections:** Approved, Requested from you, Needs action, Available to
  review, Awaiting review, Drafts, anyone's PRs in each. Oliver chose sections over
  the first build's flat list after trying it on his team's repository. On
  2026-09-22 he ruled "we should be consistent with naming": one name per
  section in every view, so Involving me and All open dropped "Needs
  attention" / "In progress" for My PRs' names, and someone else's PR that
  nobody was asked to review and nobody has reviewed moved from the waiting
  section (where it waited on nobody) to the Review queue's Available to
  review (`layout::is_available_section`; the core category stays Await).
  Your own review counts there: `review_state` leaves it out and GitHub
  drops your request once you review, so a PR only you reviewed read as
  unreviewed until the pre-release review caught it; `my_review` keeps it
  under Awaiting review. The default order stays My PRs', his call over
  lifting Awaiting review above Needs action; since 2026-09-22 the person
  can change it (section order, above). "Requested
  from you" is a second alias running the Review queue's own
  `review-requested:` search (ids only, first 100), so it is the same set in
  both views, team requests included. Notes on other people's PRs drop the
  "alice's PR ·" prefix because the Author column says it; share text says
  "by alice · approved".
- **Calm:** "need you" counts Requested from you plus your own PRs under
  Needs action (`status::row_needs_you`: your rows are the ones with
  blockers; every view since 2026-09-22), never a teammate's conflict; the
  view never adds to the Dock
  badge, and changed markers and
  notifications only cover PRs you watch, your own, those requesting you,
  and ones another view already follows. Load more says how many PRs joined
  (`status::loaded_more_text`).
- **Every section header explains itself** (all three views, Oliver's ask:
  "what means In progress?"): `layout::section_explanation(mode, kind,
  all_repos)` is one sentence per section, written against `board.rs`'s
  derivation and tested there; the desktop shows it on hovering the title,
  ffi exports it, and CLI JSON carries it as each section's `explanation`.
- ASSESSMENT — G0: no repaint or refresh-timer change; the per-view cache
  holds three entries of at most 300/600 rows (bound written next to
  `MAX_CACHED_QUEUES`). Not a re-run trigger as written; a non-blocking soak
  with All open selected is Oliver's call.

**FACT: the iPad's snooze menu, size band, "unresolved" fact and Details panel
kinds now come from core.** The desktop's words moved into constants
(`local/src/attention_state.rs`: `SNOOZE_ONE_HOUR` … `SNOOZE_CANCEL`,
`waiting_on`) and functions (`core::detail::detail_items`, `attention_line`,
`core::cells::unresolved_label`) that `src/app.rs` and `src/table.rs` call, so
the desktop reads the same and the iPad stops keeping its own copies.
`detail_items` is `detail_lines` with a `DetailKind` per line, so a front end
can draw the labels as chips or mute the last line without matching words.

**FACT: two FFI defects from the iPad review are fixed.** `BoardClient::reset`
now bumps a generation, and a fetch that started before it no longer stores its
cursor afterwards (`ffi/tests/offline_board.rs` holds a response across a reset
and fails on the old code). An unexpected Swift error from a callback is now
`FfiError::Network` instead of a uniffi panic, which under the iOS profile's
`panic = "abort"` ended the app.

## Current update — 2026-09-20

**FACT: the memory gate G0 is passed.** Every "the memory gate is still not
satisfied" line further down is a dated record of an earlier state. A 37.4 h
soak of the Homebrew v0.9.1 build measured a mean physical footprint of
106.7 MB, a p99 of 159 MB, a slope of −0.77 MB/h and 0.01 % of one core
(`benchmarks/2026-09-20-memory-gate-v0.9.1.md`). Oliver replaced one threshold
against the measurement: "(max − min) ≤ 25 MB" became "p99 < 200 MB", since one
drawn frame holds ~23 MB per drawable and no build could meet the range. The
251 MB peak is published beside the p99. The gate is re-run on a shipped build
whenever the framework, the refresh loop or the repaint path changes; the
procedure is in CLAUDE.md.

## Current update — 2026-09-17

**FACT: the sentences two front ends must agree on now live in core.** Two new
modules were lifted out of the GPUI app without changing a word of it:
`core/src/detail.rs` owns the Details panel (`detail_lines`, `detail_text`,
`copy_items`, `size_text`, `review_state_words`) and `core/src/status.rs` owns
everything that describes the board rather than a pull request — the header's
count line and its explanation (`header_counts`), the toggles' tooltips, the
blue marker's, the clock words (`relative`, `human_duration`), the sync line and
the empty-queue line (`queue_empty_text`). `src/table.rs` and `src/app.rs` call
them and keep only thin wrappers, so the desktop's wording is unchanged and is
now pinned by core's own tests rather than by a GPUI test that needs Metal.

`header_counts` takes a `BadgeName` (`Dock` or `AppIcon`) because that is the
one word the two products cannot share: macOS has a Dock badge and iPadOS has
an app icon badge. Everything else in the paragraph is identical by
construction. `ffi/src/pure.rs` exports all of it, so the iPad draws the same
strings rather than a Swift retype of them.

**Why it matters:** two front ends that each write "1 needs you" by hand will
eventually write two different sentences, and the difference will be found by a
user rather than by a test. Anything a person reads that is not about one PR
belongs in `status.rs`; anything about one PR belongs in `detail.rs`.

**FACT: `scripts/demo/gh` now records the viewer's own review.** `latestReview`
was always an empty node list, so `reviewed_oid` was never set and no fixture
could produce the Note "new commits since your review" — the one thing
`review-again-when-changed` is built on. It now carries the viewer's standing
review pinned to the commit that was the head when it was left, which is what
GitHub returns. The generator also gained a `deep` scenario (six authored pages
of twelve rows) so a client can be driven past core's five-page cap and the
fifty-watch bound can be filled.

**FACT: PR Marmot signs in to GitHub on its own; `gh` is now optional.**
`core/src/github/device_flow.rs` implements GitHub's OAuth Device Flow as pure
functions over a new `AuthTransport` trait — nothing sleeps, nothing reads the
clock (`now_epoch` is a parameter), so the terminal, the desktop app and iOS can
each drive the poll loop with their own timer. `core/src/github/http.rs` is a
blocking `ureq` transport behind the **non-default `http` feature**
(`GithubTransport` + `AuthTransport` + a new `RestTransport` for `user/repos`),
which keeps the iOS build free of a second TLS stack because Swift supplies its
own `URLSession` transport over FFI. `local/src/auth.rs` stores the token: macOS
keychain (service `dev.prmarmot.auth`) by default, a `0600` file under
`state_root()` otherwise, selectable with `[auth] store`. `local/src/session.rs`
is the one place that decides which door a run uses, so the app and the CLI
cannot drift.

`[auth]` in the config file takes `host`, `client_id`, `mode`
(`auto | gh | device | token`) and `store`; precedence is flags
(`--host`, `--auth`) > `PRMARMOT_HOST` / `GH_HOST` / `PRMARMOT_AUTH` /
`PRMARMOT_CLIENT_ID` / `PRMARMOT_TOKEN` > file. `auto` prefers a token this
machine stored and falls back to `gh`, so every existing install keeps working
untouched. (Reversed 2026-09-18 at Oliver's decision: `auto` now takes the
GitHub CLI login first, because organizations' OAuth-app restrictions don't
apply to `gh` but do apply to PR Marmot's own app; see `local/src/session.rs`.
Refined the same day: a token chosen on purpose, `PRMARMOT_TOKEN` or a pasted
PAT, still comes before `gh`; only the device-flow token, the one those
restrictions apply to, moved behind it.) The CLI gained `auth login | status | logout` (with `--with-token`
reading a PAT from stdin) and the desktop gained an in-app sign-in screen
(`src/onboarding.rs`) with device flow, token paste and an Enterprise-host
field, plus Settings → **Disconnect**.

**FACT (measured 2026-09-17):** `ureq 3.4.2` with `rustls` and
`security-framework 3.7.0` both declare `rust-version = 1.85` and both compile
under Rust 1.85 with the committed `Cargo.lock`, so the MSRV job stays green;
`ureq` + `rustls` also compiles for `aarch64-apple-ios`, though iOS will not use
it. Acceptance was checked end to end: with `gh` absent from `PATH`,
`PRMARMOT_AUTH=token` renders both boards and the live rate-limit budget.

**FACT (2026-09-17):** sign-in is registered as a classic **OAuth App** named
"PR Marmot" under `oliver-kriska`, client ID `Ov23liJnPBmrUZRLYilH`, device flow
on, **no client secret generated** (never generate one: the device flow does not
need it, and a secret in a shipped binary is not a secret). Core accepts either
registration — **OAuth App (recommended) or GitHub App** — and tests both token
shapes; the recommendation is about reach, because an OAuth App token sees every
repository its owner can see while a GitHub App user token sees only the
accounts and orgs where the app was installed. The OAuth App's tokens carry no
`expires_in` and no `refresh_token`, so nothing ever asks for a refresh; a
GitHub App's do, and `needs_refresh`/`can_refresh` still apply. The client ID is
public by design; a GitHub Enterprise Server host needs its own registration,
and until it has one `is_placeholder_client_id` keeps the device flow from
starting and points at the token path.

**FACT: the search grammar and the board's whole data layer are now reusable
from Swift.** `core/src/search.rs` holds what `src/table.rs` used to parse —
`Qualifier`, `FilterTerm`, `filter_terms`, `matches_filter`, `take_filter_chips`,
`with_filter`, `StaleRule` — with only chip *drawing* left in the app.
`core/tests/golden/search.json` pins 29 queries × the parity fixtures at a fixed
clock, and a second test asserts the matrix still discriminates so it cannot rot
into a file where everything matches everything. `prmarmot-cli mine|review
--filter "<query>"` runs the same grammar in a terminal, which is both a real
feature and the proof the port kept its meaning.

`ffi/` (`prmarmot-ffi`) is the UniFFI boundary: records mirroring
`cli/schema/board-v1.schema.json`, two async foreign traits Swift implements
(`GithubTransport`, `TokenSource`), a `BoardClient` that owns pagination, an
`AttentionStore` that serializes to bytes, and the pure functions `layout`,
`search`, `shareGroup` and the pickup/size helpers. `scripts/build-xcframework.sh`
produces `PRMarmotCore.xcframework`; `.github/workflows/xcframework.yml` does it
on `macos-26` from an `ffi-v*` tag. Read `ffi/README.md` before writing a Swift
transport — the cycle rule there is not optional.

**FACT (measured 2026-09-17):** the linked iOS library
(`aarch64-apple-ios`, `--profile ios`, `libprmarmot_ffi.dylib`) is **830,264
bytes** with `regex-lite` against **1,638,520 bytes** with `regex` — 789 KiB for
a regex engine PR Marmot barely uses. `small-regex` is therefore a
`prmarmot-core` feature that only `ffi/` turns on; the desktop and the CLI keep
full `regex` because the issue-link pattern comes from a user's config, and CI
runs the entire core suite under both engines.

**FACT:** `scripts/build-xcframework.sh` takes **39 s** locally for all three
slices, and the seven-test Swift smoke test passes in the iPad Pro 13-inch (M5)
simulator against the same fixtures the Rust goldens use. `core/clippy.toml`
plus `core/tests/no_clock.rs` now make "core never reads the clock" an
enforced rule rather than a habit.

**ASSESSMENT:** two details cost real time and are worth knowing. The modulemap
module name must be the crate-derived `prmarmot_ffiFFI`, not a pretty one, or
every generated type is "not in scope"; and `uniffi-bindgen-swift --xcframework`
emits `framework module`, which is wrong for an XCFramework built from a static
library plus headers. Both are recorded in the script next to the code that
depends on them.

### Prior update — 2026-09-16

**FACT:** Review state now follows a **standing review** rule, deliberately
diverging from the shell prototype's `$rv`/`$mine`: a reviewer's standing
review is their latest, except that a later COMMENTED review does not replace
an earlier APPROVED or CHANGES_REQUESTED one (a later change request or a
dismissal still does). `core/src/board.rs` `standing_review` and
`scripts/prototype-jq/` implement the same rule, the goldens were regenerated,
and fixtures 113–116 / 206–210 pin it. Do not "fix the port" back to the
prototype. `latestReview` in the query still includes COMMENTED on purpose: it
is the review-again evidence (`reviewed_oid`/`reviewed_at`), and a
comment-only review must keep triggering "Review again". `BoardRow.my_review`
is now set in every mode.

### Prior update — 2026-09-11

**FACT:** The product identity is now **PR Marmot**: repository
`oliver-kriska/prmarmot`, binary/package `prmarmot`, core package
`prmarmot-core`, app `prmarmot.app`, bundle identifier
`dev.oliverkriska.prmarmot`, environment prefix `PRMARMOT_`, and cask
`oliver-kriska/tap/prmarmot`. The GitHub repository rename is complete and the
existing shared Homebrew tap is confirmed. Release credentials remain pending;
Scribe's GitHub secrets cannot be read back through the API.

**FACT:** macOS release preparation is arm64-only and fail-closed. The manual
candidate workflow publishes nothing; tag publication requires an exact package
version match and completes tests, Developer ID signing, notarization acceptance,
stapling, `codesign`/`spctl` verification, and checksum verification before the
release is created. The cask is updated afterward without replacing historical
assets. See `packaging/RELEASING.md`.

### Prior implementation update — 2026-09-08

**FACT:** At Oliver's request, the app now uses published gpui-component **0.6.0**
with gpui-pre/gpui-pre-platform **0.3.4**, platform bootstrap, component `Root`, and
the renamed virtualized `DataTable`. Historical 0.5.1 API/pin guidance below is
retained as the build record, not current setup instructions; use `Cargo.toml`
and README for today's versions.

**FACT:** Repo selection now discovers owned/collaborator/organization repos via
`user/repos` (1,000 repo/10-page ceiling), merged with configured/current entries.
Review queue includes a separately labeled no-reviewer-requested section; two
bounded search aliases share one GraphQL operation. Native GitHub stacks are
grouped by stack number and ordered by layer position within category, with
partial-stack counts. Golden prototype behavior remains covered unchanged.

**FACT:** The toolbar now filters loaded PRs and explicitly loads more results
(five pages per alias maximum). Refresh resets to page one. A bottom details
panel exposes loaded metadata without per-PR network calls; `/` focuses search,
Space toggles details, and Escape closes them.

**ASSESSMENT:** Search, clear ownership, and stack relationships improve this
dense dashboard more than decorative artwork. The design rationale and next UX
priorities are in `DESIGN.md`; limits and reviewer-vs-assignee semantics are in
README. **OPEN QUESTION:** The framework's overnight memory/idle-GPU gate remains
unvalidated on the upgraded runtime. These specifically requested changes do not
establish that the gate passed or unblock the remaining roadmap automatically.

## 1. What we're building

A dashboard of GitHub pull requests you keep open all day. A dense table — PR number/link, draft/ready, CI state, requested reviewers, completed reviews, unresolved-thread count, merge-conflict flag, bug label, linked issue, and a computed **"Note"** saying what to do next / what it's blocked on. **Three views:** (1) all open PRs in a repo with filters; (2) *my authored PRs*, triaged action → awaiting-review → drafts; (3) *my review queue*. **Multiple named tabs**, each a repo or group of repos. **Auto-refresh** (default 5 min). **Read-only + open-in-browser only** — no AI, no reviewer-assignment, no merging in v1. Installable via **Homebrew**.

There is a **working shell prototype** (`~/.claude/skills/pr-board/`) that already does the data + categorization + note logic in one GitHub GraphQL call. **PR Marmot v1 = port that prototype to Rust behind a refreshing UI.** The prototype is the behavioral spec; it's transcribed in full in the data-layer doc.

---

## 2. The recommendation (distilled)

> **PRODUCT DECISION (Oliver, 2026-07-24, same day as the research):** prboard is a **windowed native desktop app, not a TUI**. The research below originally recommended ratatui as the default; Oliver ruled the TUI out as a product form ("we want a desktop app with a dashboard, not a terminal"). The recommendation table and roadmap reflect the updated decision; the ratatui analysis in the UI-framework doc stands as evidence and as the record of what was traded away.

| Decision | Recommendation | Confidence |
|---|---|---|
| **UI framework** | **GPUI + longbridge/gpui-component** (windowed desktop app — product decision above). Use gpui-component, NOT Guise, for the component layer (Guise lacks the table). Gated by the 1-day memory spike (§5 Step 0): idle RSS must stay flat and bounded. If the gate fails there is **no pre-committed fallback** — revisit with Oliver (egui, Tauri, or accept-and-mitigate), since the TUI escape hatch is ruled out. | Decided (product); memory gate mandatory |
| **Data transport** | **Shell out to `gh api graphql`** (like the prototype and gh-dash), behind a `GithubTransport` trait so direct-HTTP is a later swap. | High |
| **Query** | Reproduce the prototype's single `search(type:ISSUE, first:60)` GraphQL query (~3 rate-limit points/repo). Never fan out per-PR REST like the previous app did. | High (FACT prototype) |
| **Refresh floor** | Default **5 min**; **hard minimum 30 s** (not the guessed 5 s); adaptive budget warning across tabs. | High (derived from GitHub's published limits) |
| **Auth** | Reuse `gh` (zero-config for anyone who's run `gh auth login`); OAuth Device Flow + PAT later, behind a `TokenSource` trait. | High |
| **Distribution** | macOS: **signed + notarized `.app` — hard-required, not just accepted**: Homebrew quarantines cask downloads, is removing `--no-quarantine`, and enforces Gatekeeper checks on casks from **2026-09-01**; a personal tap does NOT dodge quarantine (r2 packaging doc §3). Pipeline: **Zed's cargo-bundle fork** → codesign `--options runtime` → `notarytool submit --wait` → `stapler staple` in CI → hand-authored **cask** in own tap + direct download. **cargo-dist is formula/CLI-only — it cannot produce `.app`/cask** (axodotdev #850); the macOS pipeline is hand-rolled. Linux: **`.deb` preferred over raw tarball** — GPUI needs Vulkan/fontconfig/xkbcommon at runtime, and a package manager resolves those. | High (r2-verified) |
| **Name** | `prboard` is free on crates.io (the namespace that matters) but crowded on GitHub. Consider `prwall`/`pullboard` for distinctiveness. | FACT (checked) |

**Why GPUI + gpui-component, given the desktop-app decision:** among windowed options it is the only one that fits both hard requirements — the native "Guise-family" aesthetic Oliver wants AND a production-proven **virtualized data table** (gpui-component powers Longbridge Pro, a trading app with exactly our always-open-dense-table workload). Tauri reintroduces webview idle memory (the failure class Oliver is escaping), and egui/iced/Slint are weak precisely on heavy tables while looking less native. PRFlow's memory crash was primarily an **app-level unbounded-cache bug** that is now understood and avoidable (guardrails in §5) — but GPUI carries its own documented leak/overdraw history in Zed, which is why the memory spike below is a **mandatory gate, not a formality**.

**What this decision consciously accepts** (the costs the original ratatui recommendation avoided — accept them with eyes open): Apple Developer ID + notarization ($99/yr + CI signing pipeline) for a distributable `.app` — and per round 2 this is **strictly required**, since Homebrew enforces Gatekeeper on casks from 2026-09-01 and quarantine applies regardless of tap; pre-1.0 `gpui` API churn and long cold builds — though round 2 corrected a round-1 belief: the **published `gpui-component 0.5.1` depends on crates.io `gpui ^0.2.2`, no git pin needed** (pin `gpui = "=0.2.2"` + `gpui-component = "=0.5.1"`, stable Rust confirmed by the gpui README); Linux renderer churn (Blade→wgpu Feb 2026) plus runtime deps (Vulkan/fontconfig/xkbcommon). The idle-GPU worry is **milder than round 1 feared**: GPUI renders on-demand via a `WindowInvalidator`, so idle GPU is ~0 *as long as nothing animates continuously* — design rule: **no perpetual refresh-spinner animation**. None of these is disqualifying; all of them are real. The ratatui analysis remains in the UI doc as the record of the alternative.

---

## 3. The five biggest risks / open questions

1. **OPEN QUESTION — GPUI idle memory is unmeasured, and it is now the go/no-go gate.** No public number exists for a minimal gpui-component app's idle RSS over a day. **Answer it in the spike** (build the walking skeleton, leave running 12–24 h, sample RSS + energy). Pass = flat and bounded (<~150 MB proposed); fail = climbs. This re-tests exactly what stalled PRFlow.
2. **RISK — if the memory gate fails, there is no good windowed fallback.** The TUI escape hatch is ruled out by the product decision; the remaining windowed options are egui (weak on heavy tables, tool-UI look), Tauri (webview idle memory — the failure class being escaped), or accept-and-mitigate GPUI. A gate failure means going back to Oliver with numbers, not silently picking one.
3. **RISK — GPUI's Linux support is real but churning, and it's now on our path.** The Linux renderer was rewritten off Blade onto wgpu in Feb 2026 (`zed#46758`) to fix NVIDIA/Wayland freezes. A cross-platform GPUI app inherits that surface (Wayland/X11 feature flags, GPU drivers). Budget Linux testing time; NVIDIA+Wayland is the historical trouble spot.
4. **RISK — shared rate-limit budget, not the "5 s floor."** The token is shared with your own `gh`/git/CI usage. A too-fast refresh across many tabs can rate-limit your *actual work*, not just the dashboard. Mitigation: default 5 min, hard floor 30 s, show the live `rateLimit{}` budget, adaptive warning (math in the data-layer doc).
5. **RISK — Guise (the specific lib Oliver linked) is missing our core widget and is bus-factor-1.** Guise has Tabs/Select/Modal but **no Table, List, or virtual scrolling** — the 80% of prboard — and is an unversioned single-author git dependency. If you go GPUI, use **gpui-component** (mature, has the virtualized table), not Guise. Wanting Guise's *components* specifically is hard to justify.

**Round-2 additions (2026-07-24, later same day — details in the r2 research docs):**

6. **RISK — Homebrew's Gatekeeper enforcement makes notarization non-optional.** Casks are quarantined regardless of tap, `--no-quarantine` is being removed, and Gatekeeper-failing casks are blocked from 2026-09-01. The $99/yr + CI signing pipeline is a hard prerequisite for shipping the `.app` to colleagues at all.
7. **RISK — dependency-variant footgun:** the *published* `gpui-component 0.5.1` uses crates.io `gpui ^0.2.2` (no git), while the *git-main* variant pulls `gpui`/`gpui_platform` via git with different bootstrap code. Pin the crates.io pair (`gpui = "=0.2.2"`, `gpui-component = "=0.5.1"`) and do NOT copy git-main Getting-Started snippets.
8. **RISK — Linux is not a drop-in binary:** GPUI needs a working Vulkan stack + fontconfig/xkbcommon on the user's machine (NVIDIA+Wayland worst). Prefer a `.deb` so the package manager resolves deps; document the requirements.
9. **NOTE — the table is greenfield:** PRFlow hand-rolled its rows and never used the virtualized table. Use gpui-component's **`DataTable`** (delegate `TableDelegate`; only `render_td` required; virtual scroll, sort, selection + keyboard nav, fixed columns, per-row/cell colors) — NOT the simple stateless `Table`. Budget learning time for the imperative delegate API.

**Two things that contradict what Oliver currently believes** (he asked to be told):
- **The "5 s or something" refresh floor is wrong as a floor.** The real GraphQL cost is ~3 points/repo-refresh, and 5 s is *survivable* for one repo (43% of the hourly budget) but imprudent and bad across tabs. Replace with **30 s hard minimum / 60 s recommended / 5 min default** — with the math shown.
- **The `gh`-CLI dependency is the right call, not a limitation to design around.** It's what gh-dash (12.1k stars) does, it gives true zero-config onboarding, and calling the API directly buys little in v1. Keep `gh` for v1; put it behind a trait so direct-HTTP (seeded by `gh auth token`) is a clean later swap. (Also corrected: PRFlow is *on disk* at `~/Projects/pr_flow`, and GPUI reportedly builds on *stable* Rust now, not the nightly PRFlow needed.)

---

## 4. Proposed v1 scope cut

**In v1:**
- The three views (authored default, review queue, all-open) as modes within a tab.
- One GraphQL query, `gh api graphql` transport behind a trait.
- The full categorization + Note logic from the prototype (action/await/draft; todo/done).
- Multiple named tabs (config-driven), each one or more repos.
- Repo selection: pick a GitHub repo (via `gh repo list` + free-text) **or** point at a local folder (derive owner/repo from git remote). Zero-config first run infers the repo from `cwd`.
- Auto-refresh (default 5 min, floor 30 s), refresh indicator + "last updated" + live rate-limit budget, back-off on rate-limit.
- Filters (author/CI/review-state/draft/label), client-side, persisted per tab.
- Full mouse support + keyboard shortcuts (`?` help); open PR / linked issue in browser; copy PR URL.
- Bounded caches from line one (explicit `MAX_*` + FIFO — the #1 lesson from PRFlow).
- Config in TOML at `$XDG_CONFIG_HOME/prmarmot/config.toml`.
- Distribution: macOS signed + notarized `.app` via own brew tap + direct download; Linux tarball / shell installer / brew formula; CI with clippy `-D warnings` (clean first).

**Explicitly NOT in v1:** AI/summaries; reviewer assignment / dismiss / merge / submit-review (any write action beyond open-in-browser); custom keybindings; shell-out custom actions (gh-dash-style); self-updating binary; OAuth Device Flow; homebrew-core; Windows.

---

## 5. Build roadmap

**Step 0 — Spike (≈1 day, the memory gate — do this before building on top).**
Build ONE walking skeleton in **GPUI + gpui-component** (the framework is decided; the spike validates it): stable Rust, crates.io pins `gpui = "=0.2.2"` + `gpui-component = "=0.5.1"`, the **`DataTable`** widget (not the stateless `Table`), no continuously-animating spinner (it would defeat the idle-GPU half of the gate) — one tab, one repo, the real `gh api graphql` call, render the 60-row virtualized table + Note, auto-refresh 5 min, open-in-browser. The r2 build doc (`2026-07-24-gpui-desktop-build.md`) has the skeleton sketch — start from it. Then **measure idle RSS over 12–24 h** (sample `ps -o rss=` per minute; on mac also `leaks`/Instruments and `powermetrics` for idle GPU/energy — GPUI has a history of idle frame-presentation burn) + build time + binary size. **Gate rule:** proceed if idle RSS is flat and < ~150 MB over a day and idle GPU is ~0 when nothing changes; if it climbs, STOP and bring the numbers back to Oliver — the fallbacks (egui, Tauri, accept-and-mitigate) are all compromised and need a conscious re-decision. Mine `~/Projects/pr_flow` first — it already used gpui-component, so its table/state code is a reference for both patterns and pitfalls. (Original two-skeleton protocol: UI-framework doc §7 — the ratatui half is obsolete per the product decision.)

> **STEP-0 STATUS (2026-07-24, evening).** The skeleton is built and the overnight gate run is in progress (release build + RSS sampler via `scripts/spike-run.sh`; verdict via `scripts/measure-summary.sh`). Build-time corrections to the research are in `.claude/research/2026-07-24-step0-build-findings.md` — notably: the published gpui-component 0.5.1 has no `DataTable` (its `Table`+`TableState` IS the virtualized one), MSRV is effectively 1.88, macOS needs the Xcode Metal Toolchain, and cold builds are ~4 min, not 15–30.
>
> **Step 0.5 — first-feedback round (same evening, done):** Note column wins width over Title and both carry full-text hover tooltips (truncation was hiding compound notes — composition itself is correct and golden-tested); drag-resizable columns confirmed on (0.5.1 default); Requested "none" dimmed to "—" (redundant with the red note); **dark/light/system theme modes** (`PRBOARD_THEME`, `t` key cycles, System follows macOS appearance changes live via `observe_window_appearance`).
>
> **Queued for later steps, learned in the build:** an asset bundle (icons) only when tabs/filters need it (Step 3); a details row on select as a richer note-survival strategy (Step 2 candidate); "copy board as markdown" via delegate cell-text (post-v1); the SKILL.md example JSON is internally inconsistent with the prototype script — the script is the spec (parity tests pin to it).
>
> **Step 0.6 — second-feedback round (2026-07-24, late evening, done):** (a) **config file** `~/.config/prboard/config.toml` (repo/repos/refresh/theme/reviewers/issue_link; env > file precedence) so Spotlight launches work, plus `gh` resolved from Homebrew paths when PATH is the LaunchServices minimum; (b) **repo picker** — header Select fed from config `repos`, switching clears + refetches; (c) **Bug column → Labels** — neutral chips for all labels (`labels` now on `BoardRow`), 🐛 marks the bug label, `+n` overflow, tooltip; (d) **Reviewed-by is glyph-first** (the state glyph survives truncation — the DISMISSED case on #10710 was the proof) with a spelled-out tooltip; (e) stray last-empty-column filler removed; (f) **visual overhaul** per the Guise/Mantine-derived spec in `.claude/research/2026-07-24-visual-design.md` — `src/design.rs` token overrides (both modes, WCAG-AA-checked), status **dots replace emoji** in CI/Note cells (core note strings unchanged — UI strips the glyph language), one-line header, keycap footer, full-bleed `Size::Small` table, draft rows dimmed not tinted; (g) **packaging** — `scripts/bundle-app.sh` hand-rolls an ad-hoc-signed `~/Applications/prboard.app` (Spotlight-launchable, generated icon), Homebrew cask template + notarization runbook in `packaging/` (distribution blocked on Apple Developer enrollment; Homebrew enforces Gatekeeper from 2026-09-01). A tiny embedded `AssetSource` (4 Lucide SVGs) now exists for the Select chevron — the "asset bundle when needed" moment arrived early.
> Also landed same round: **custom transparent titlebar** (user-requested) — gpui-component's `TitleBar` wraps the header row, traffic lights overlay the app's own chrome (drag + double-click-zoom included).
>
> The Step-0 memory gate still decides Step 1+. Verification gotcha for future sessions: GPUI skips repainting occluded windows — screenshot checks MUST bring the window frontmost first or they capture a stale first frame (looks like a hung fetch; it isn't).
>
> **Discovered gaps from the 2026-07-24 retro — all three FIXED (Step 0.7, commit 64c5a51):**
> 1. ~~No timeout on the `gh` subprocess~~ → `output_with_timeout` watchdog in `gh_cli.rs` (60 s GraphQL / 30 s quick calls, SIGKILL + visible error; the hung-`gh` `syncing=true` deadlock is gone).
> 2. ~~"synced Xm ago" staleness~~ → 60 s `cx.notify()` ticker (one frame a minute, nothing animates — but note it now runs during the gate measurement, so the idle numbers already include it).
> 3. ~~In-app state doesn't persist~~ → `toml_edit`-based write-back of `repo`/`theme`/`view`/`[window]` on change and on quit; never writes into an unparseable config.
>
> **Step 0.8 — view switcher (2026-07-24, done, commit e018011, from a parallel review session):** the footer-only `v` toggle was undiscoverable by mouse and its label ambiguous. Replaced by a segmented **My PRs | Review queue** `TabBar` in the titlebar next to the repo picker; `1`/`2` select directly, `v` still toggles. All paths route through one `select_mode` → `AppState::set_mode` (epoch guard intact); review counts reworded to "N open · N pending · N reviewed · N drafts". Evidence + prioritized design sequence: `.claude/research/2026-07-24-ux-review-and-view-switcher.md`. The control is built to take **All open PRs** as a third peer segment when roadmap #1 lands.
>
> **Feature roadmap from the 2026-07-24 external review (ordered; all post-gate):**
> 1. **View 1 "All open PRs" is MISSING and is decided v1 scope** (UX spec §2 — three views; only authored+review exist, `v` toggles two). Needs a `Mode::AllOpen` in core: search `repo:X is:pr is:open`, flat newest-first, author column, and a Note semantics decision for other people's PRs (the prototype never defined one — the spec's "situational awareness" framing suggests state-descriptions, not imperatives). Must land before v1.
> 2. **"Changed since last look" indicators** — track `updatedAt` per PR between refreshes (field must be ADDED to the GraphQL query — currently not fetched; goldens unaffected, jq selects its own fields); changed rows get a subtle marker cleared on selection. The feature that turns a status table into a glanceable dashboard.
> 3. ~~**Group header rows with counts**~~ **DONE (Step 0.9)** — see below. The 0.5.1 caution resolved: overlay-from-`render_tr`, no fork.
> 4. **Filters + `/` quick-find** — fuzzy title/issue/author + CI-fail/conflict/draft toggles (author filter is the daily driver for View 1).
> 5. **Dock badge with action count** (r2 UX doc: feasible).
> 6. ~~**Single-click blue links**~~ **DONE (Step 0.9)** — PR # + issue tag open on click.
> 7. ~~**Queue-specific transient states**~~ **DONE (Step 0.9)** — mode-specific loading/updating/empty copy; the "Loading board… forever" worst case is gone.
> Deliberately NOT yet: notifications (needs #2 first; easy to make annoying), any write actions (v2 line), footer-chrome reduction behind a `?` help surface (only after interactions are discoverable).
>
> **Step 0.9 — external design feedback, pass 1 (2026-07-24, done).** ⚠️ **Process note (honest record):** these are post-gate roadmap items, built BEFORE a clean gate at Oliver's explicit direction ("abort gate, build now"). The live gate run was only a **daytime partial** (~3h18m, idle RSS 99→105 MB — under the 150 MB ceiling but a mild ~+1.8 MB/h drift, extrapolating to ~148 MB at 24 h) and was aborted to do this work. **The memory gate is still NOT satisfied — the overnight 12–24 h run must still pass before the framework is validated.** Restart it (`scripts/spike-run.sh`) after this settles. What landed (verified by screenshot in both themes + both queues, clippy clean, `cargo fmt`, 16 core unit + 4 golden-parity tests green):
> - **Queue-specific states (roadmap #7)** — `queue_sync_text`/`queue_loading_text` give each queue its own loading/updating/empty copy ("Loading your open PRs…" vs "Loading requested reviews…"; "You have no open PRs" vs "Nothing is waiting for your review"). A background refresh keeps the "synced Xm ago" anchor visible ("Updating your PRs… · synced 3m ago") so switching reads as navigation, not a re-run. Empty view is now mode-aware in the delegate's `render_empty`.
> - **Single-click links (roadmap #6)** — the blue `#number` opens the PR and a configured issue tag such as `PROJ-1234` opens its linked issue (uses `BoardRow.issue_url`, already in core) on a single click, with hover-underline + pointer. Row double-click still opens the PR.
> - **Bounded per-queue cache + place-keeping (roadmap #1-adjacent, `state.rs`)** — `AppState.cache: HashMap<Mode, {rows, last_synced}>` (rate-limit budget deliberately NOT cached — it is account-global; restoring a stale budget could false-trip the back-off). Switching restores the target queue's rows instantly and refreshes in the background — no empty flash. `MAX_CACHED_QUEUES = 2`, explicit per the PRFlow bounded-everything guardrail. `RootView.selections: HashMap<Mode, usize>` + `scroll_to_row`/`set_selected_row` restore each queue's selected row + scroll on switch-back, driven by the generation observer with a `pending_restore` flag so restore happens only on a switch, not on every background refresh.
> - **Group section headers (roadmap #3, the risky one)** — `DisplayRow::{Header,Pr}` interleaves a "NEEDS ACTION 14"-style band before each category run (built in `rebuild_display` from the already-sorted rows; empty groups emit no header). **0.5.1 finding that RESOLVES the HANDOFF #3 caution:** published gpui-component renders every cell inside a fixed-width `overflow_hidden` box (`table/state.rs render_cell`), so a full-width label CANNOT live in a cell. The label is drawn as an **absolute overlay from `render_tr`** (the row container is not clipped) while that header row's cells render empty — no fork, no multi-table rework. There is no per-row "non-selectable" hook in 0.5.1, so keyboard/mouse selection **bounces off headers**: `on_select_row` subscribes to `TableEvent::SelectRow` and, on landing a header, re-selects the nearest PR in the direction of travel (`set_selected_row` re-emits `SelectRow`, but the bounced-to row is a real PR, so it settles in one hop).
> - **Column reorg + draft badge (critique #3)** — human scanning order now `PR · Title · CI · … · Labels · Note` in both modes (Title moved up right after PR); the always-"ready" **Status column is gone**; draft is a compact chip in the PR cell (PR width bumped 84→100 px to fit "#NNNNN" + badge). Note stays last and widest — the visual-design doc's "Note is the product" decision is preserved; the feedback's order already keeps Note last, so no conflict.
> - **Core:** `Mode` now derives `Hash` (HashMap key). No logic change; goldens unaffected.
> - **NOT done this pass** (deferred, still queued): roadmap #1 All-open view, #2 change indicators (needs `updatedAt` in the query), #4 filters/quick-find, #5 dock badge; feedback #8 Space inspector, #9 footer trim. The scroll-*offset* restore is approximate (`scroll_to_row`, not pixel-exact) — 0.5.1 exposes no pixel scroll setter.
>
> **Step 0.10 — hardening + visual pass from the second design review (2026-07-24, done).** A focused behavioral review of Step 0.9 found real state-transition bugs (screenshots/core tests didn't exercise them). All fixed + a visual pass; verified by screenshot in both themes + both queues, `make check` green, `cargo clippy --all-targets` clean, plus 3 new display-model unit tests. **Gate still NOT run — do that next.**
> - **Truthful loading/empty/error states.** The body state now derives from `AppState` truth, not `generation > 0` (which also bumps on a switch to an unseen queue and on a repo change). New `BodyState::{Loaded,Loading,Paused,Failed}`: loaded ⇔ `last_synced.is_some()`. An unseen Review queue no longer flashes "Nothing is waiting…" while loading; a first-fetch failure shows "Couldn't load — … · press r to retry" in the body instead of a permanent "Loading…".
> - **Selection by PR identity (URL), not display index.** `RootView.selections` is now `HashMap<Mode, String>` (URL). The generation observer captures the selected PR's URL before `set_rows` and re-resolves it to a display index after (`display_index_of_url`) on BOTH a switch and a plain refresh — so an insert/remove/reorder can never silently reselect a different PR or land on a header (which would break Enter/`y`); if the PR is gone, selection clears.
> - **Link clicks stop propagation + open once.** Both PR-number and issue-tag links now `on_mouse_down(Left) → cx.stop_propagation()` and open only on `click_count() == 1` (mirrors gpui-component `Link`). A double-click no longer opens twice and bubbles to the row's PR-open.
> - **Repo switch clears view-local nav state.** The repo picker now clears `selections`/`pending_restore`/`last_selected` before `switch_repo`, so repo A's per-queue selections can't influence repo B (`switch_repo` already dropped the row cache).
> - **Back-off is no longer an infinite load.** `AppState::backoff_remaining()` is exposed; the header shows "paused · retry in Xm" and the body "Paused — retrying in Xm" when a switch lands inside a rate-limit back-off window, instead of a stuck "Loading…".
> - **Visual (accepted as the new baseline by the reviewer):** removed the action-row red background tint (the section header + red note dot + red phrase already signal urgency three times — the block tint caused alarm fatigue and buried individual CI-fail/conflict rows); merged **Requested + Reviewed by → one "Review" column** (`→ names — requested` / `✓ name — approved` / blank), recovering ~150px; blanked sparse empty cells (Labels/Review/Unres — dashes only where "none" is meaningful, i.e. CI); titlebar summary reduced to **"N open"** (section headers carry the breakdown, and it frees room for the future `/` field); reallocated the recovered width to Title/Note.
> - **Tests:** `src/table.rs` now has 3 pure display-model unit tests (section grouping + counts, empty-group suppression, URL-identity-resolves-across-reorder). They run under `cargo test` (compiles the GPUI binary), NOT the fast core-only `make check`/CI.
> - **Still deferred:** adaptive/percentage column widths + per-queue persisted column widths (0.5.1 emits `ColumnWidthsChanged` — the hook exists, not wired), and the post-gate features (All-open, change indicators, `/` quick-find, inspector, footer trim). The partial bottom row is normal virtualized scroll (reviewer confirmed), not an occlusion defect — no change.
>
> **Step 0.11 — calmer notes + responsive columns + manual-resize preservation (2026-07-24, done).** Implemented `.claude/research/2026-07-24-note-hierarchy-responsive-table-plan.md` end to end. Verified: `cargo fmt --check` clean, `cargo clippy --all-targets -- -D warnings` **exit 0**, `cargo test --workspace` green (21 core + 4 golden-parity + **18** GPUI-bin unit tests, 15 of them new), `git diff --check` clean, plus a real-data screenshot matrix against `EnaiaInc/enaia` (both themes × both queues × Compact 900 / Medium 1200 / Wide 1440). **Goldens/parity untouched — the note string is now *generated from* the structured blockers, byte-for-byte.**
> - **Core: structured `Blocker` twin of the note (`board.rs`).** New `Blocker` enum (`NoReviewers{suggested}`, `MergeConflict`, `CiFailing`, `ChangesRequested`, `UnresolvedComments(n)`) + `BoardRow.blockers`, populated by `authored_blockers()` in the prototype's canonical order. `authored_note()` now *joins* the per-blocker legacy fragments, so the human note and the structured list can never diverge. Core owns facts only — **severity/wording/color stay in the UI**. Parity goldens are serialized field-by-field and exclude `blockers`, so they're unchanged; a kitchen-sink test asserts both the ordered blockers and the exact legacy note.
> - **Phase 2 — exception-first Note (`table.rs`, pure + unit-tested).** `note_presentation(row) -> NotePresentation{tone, primary, remedy, context, tooltip}`. Presentation priority **differs from canonical order**: conflict > CI > changes > unresolved > no-reviewers. Only an *exceptional* blocker (conflict/CI/changes) colors the primary red (`Danger`); routine `assign …` / `resolve N` rows are amber-muted (`Warning`) — **the red wall is gone** (confirmed on real data: 15 "Needs action" rows render as a calm amber field with only the genuinely-broken rows red). Remaining blockers trail as muted context (`CI failing · 3 unresolved · reviewers missing`) so **nothing hides in the tooltip**; the tooltip is still the full stripped canonical note. New tones `Success`/`Routine`/`Muted` (Routine = review "you commented/requested changes" — a neutral grey dot, verified on #11152; Muted = drafts). 8 pure tests.
> - **Phase 3 — responsive columns (`table.rs` + `app.rs`).** `TableWidthClass::{Compact<1120, Medium<1360, Wide}` + a pure `columns_for(mode, class, viewport_width)`: bounded fixed metadata columns, Title/Note split the elastic remainder (Title ≈44%, **Note never narrower than Title**, both floored to 240/280px), auto-widths quantized to 16px. **Labels drop only in Compact** (verified: present at 1200/1440, gone at 900, Note still visible). Wired via `cx.observe_window_bounds` + `window.viewport_size()` (both confirmed present in gpui 0.2.2); relayout skips no-op rebuilds so a resize drag doesn't thrash. 7 layout tests (fit at 900/1100/1440, labels-only-in-compact, required-columns-never-drop, minimums + note-priority).
> - **Phase 4 — manual resize survives refresh (the real bug).** `refresh()` rebuilds `col_groups` from `delegate.column(i).width`, so a drag (which only moved the runtime `col_group`) was silently reset on the next fetch. Now `TableEvent::ColumnWidthsChanged` writes the widths **back into the delegate columns** and stores them in a bounded `HashMap<(Mode,TableWidthClass),Vec<Pixels>>` (2×3 = 6 max). A mismatched width count (mode-switch race) is rejected; auto-resize won't overwrite a manual layout within the same class; crossing classes applies that class's override or its responsive default. Cross-relaunch persistence is deliberately deferred (in-memory only).
> - **GPUI limitation discovered (record for future screenshot work):** the transparent-titlebar window is **not exposed to System Events accessibility** (`count of windows` = 0), so osascript/AX window *resize* is impossible — the screenshot matrix was captured by relaunching per width via the config `[window]` size, then killing each instance (the `columns_for` layout is identical whether hit via initial layout or the live `observe_window_bounds` path). Live drag-resize and drag-to-resize-column-then-`r` are therefore **test-verified, not hand-verified** this session. Also: screencapture returns an all-black frame while the display is asleep — `caffeinate -d`/`-u` to wake it first. **Gate still NOT run — the overnight 12–24 h memory measurement remains the blocker before Step 1+.**
> - **Follow-up (2026-07-25): the ellipsis was a mid-word hard clip, not a real "…".** First-review feedback: Title/Review values stopped mid-word with no ellipsis. Root cause (verified in gpui source + by screenshot): gpui's in-cell `.truncate()`/`text_ellipsis` is **inert inside gpui-component's virtualized table**. `virtual_list::measure_item` measures rows with `AvailableSpace::MinContent` (indefinite width) to size row height; that first pass shapes the *untruncated* line and populates the text-layout cache, and because `wrap_width.is_none()` for nowrap text the cache (`elements/text.rs`) returns that full size on every later definite-width pass — so `truncate_line` never runs. Confirmed inert even with `w_full`/`flex_1`/an explicit `w(px(150))`. **Fix:** elide the strings ourselves in `render_td` using gpui's *own* metrics — `WindowTextSystem::layout_line(...).width` to measure prefixes, `LineWrapper::truncate_line(..,"…",..)` to cut — then render a plain, already-fitting label (`measure_width`/`elide` in `table.rs`). Applied to Title (minus the issue-tag prefix), Review-requested names, and the Note tail (all `flex_shrink_0` now). **Gotcha that scaled with length:** cells render at `text_sm` = `rems(0.875)` (14px) because the table is `.small()`, *not* the ambient 13px — measuring at 13px clipped the appended "…" on long titles; `CELL_FONT_REM`/`cell_font_px` pin the measurement to the real cell size (keep in lockstep with `.small()`). Re-verified by screenshot matrix (900/1100/1440 × both themes × both queues): real three-dot "…" on every overflowing Title/Review/Note-tail, none on labels that fit, Note visible at 900, Labels only in Wide. `make check` + `cargo test -p prboard` (18) green.

**Step 1 — Walking skeleton (framework chosen).**
Data layer first, framework-independent: `GithubTransport` trait + `GhCliTransport`, the GraphQL query, serde structs, and the `Pr → BoardRow` derive (category, ci, reviewState, unresolved, note). Unit-test the categorization against the prototype's output. Then a static one-tab render of a `Vec<BoardRow>`.

**Step 2 — Interactivity.**
Auto-refresh loop (interval + floor + back-off + `rateLimit{}` budget), the three view modes, keyboard nav, open-in-browser, refresh/last-updated/stale/error/empty states, bounded caches.

**Step 3 — Tabs, filters, config.**
Multiple tabs, per-tab repos + persisted filters, TOML config + zero-config first run (cwd git remote), repo picker (GitHub select + local folder).

**Step 4 — v1 release.**
`.app` bundling with **Zed's cargo-bundle fork** (Info.plist + .icns), Apple Developer ID enrollment, codesign `--options runtime` + `notarytool submit --wait` + `stapler staple` on a macos-14 CI job, hand-authored **cask** in own tap for macOS, **`.deb` + tarball** for Linux, tag `v0.1.0`, install on a colleague's machine, README. Ship. (Full pipeline incl. required CI secrets: r2 packaging doc.)

**Step 5 (post-v1, optional):** direct-HTTP transport, OAuth Device Flow, action layer (assign reviewers, re-run checks), custom keybindings, in-app update check, AUR/deb.

**Guardrails to encode from day one** (all from the prior-art post-mortem):
- Bound every cache (`MAX_*` + FIFO); no unbounded `HashMap`.
- One GraphQL call, never per-PR REST.
- Clamp every wait `max(60).min(900)`; never freeze on a far-future reset.
- No runtime you don't use (maybe no full tokio — a thread+channel may suffice).
- Ship "last synced Xm ago" + visible rate-limit state.
- Keep transport, token-source, and issue-link behind traits/config (no hard-coded project prefix or tracker).

---

## 6. The research files (evidence behind all of the above)

- **`.claude/research/2026-07-24-prior-art.md`** — PRFlow post-mortem (the memory root cause: app-level unbounded caches + GPUI's own leak history), Agentrix lessons, gh-dash + the Rust-TUI landscape. **Start here after this handoff** — it's why the recommendation is what it is.
- **`.claude/research/2026-07-24-github-data-layer.md`** — the behavioral spec: verbatim GraphQL query + JSON shape + categorization + note rules, the transport trait, the real rate-limit math, the auth story. **This is what v1 must reproduce.**
- **`.claude/research/2026-07-24-ui-framework-evaluation.md`** — the full framework comparison (ratatui / GPUI+gpui-component / Guise / Tauri / egui / iced / Slint), the decision matrix, and the spike protocol.
- **`.claude/research/2026-07-24-product-ux-spec.md`** — layout, the three views, filters, tabs, states, keyboard model, config, repo selection, visual direction.
- **`.claude/research/2026-07-24-distribution.md`** — Homebrew via cargo-dist, macOS notarization (and why the CLI path avoids it), CI matrix, update story, name availability. *Partly superseded by the r2 packaging doc below.*

**Round 2 (same day, after the desktop-app decision):**

- **`.claude/research/2026-07-24-gpui-desktop-build.md`** — building with GPUI + gpui-component in practice: dependency pins, `DataTable` API, standalone-app architecture (async executor, gh subprocess, refresh timers), PRFlow code lessons, sharpened memory/energy measurement protocol, and the **skeleton sketch the spike starts from**.
- **`.claude/research/2026-07-24-desktop-packaging.md`** — the real macOS `.app` pipeline (Zed's cargo-bundle fork → codesign → notarytool → staple → cask), why notarization is hard-required (Homebrew 2026-09-01 enforcement), Linux `.deb`/tarball, CI secrets and matrix. Supersedes the CLI-path parts of the round-1 distribution doc.
- **`.claude/research/2026-07-24-desktop-ux-capabilities.md`** — feasibility map for tray/menubar, Dock badge, native notifications, launch-at-login, window-state persistence, app menu — with v1/v2 markers.

---

## 7. Key facts to not re-derive

- Prototype: `~/.claude/skills/pr-board/scripts/pr-board.sh` + `SKILL.md` (the spec).
- Previous app: `~/Projects/pr_flow` (Rust+GPUI, on disk) — read `SOLUTIONS_EXTRACTED.md` for the memory fix.
- Prior TUI Oliver shipped: `~/Projects/agentrix` (Rust+ratatui) and `vibenalytics` (Rust+ratatui, GitHub-Releases distribution).
- GraphQL: 5,000 points/hr; our query ≈ 3 points/repo-refresh; 500k-node cap; secondary 2,000 pts/min.
- gpui-component: crates.io, Apache-2.0, 12.2k stars. **r2 corrections:** published 0.5.1 depends on **crates.io `gpui ^0.2.2` (no git pin)** — pin `gpui = "=0.2.2"` + `gpui-component = "=0.5.1"`, stable Rust (gpui README). Use **`DataTable`** (delegate API: virtual scroll, sort, selection, fixed columns, per-row/cell colors), not the stateless `Table`.
- Guise: git-only, MIT, 120+ components but **no table/list/virtual-scroll**.
- crates.io `prboard` is free; GitHub `prboard` is taken by others (use `oliver-kriska/prboard` or rename).
- Distribution: cargo-dist v0.32.0 (2026-05), alive but **formula/CLI-only — no `.app`/cask** (axodotdev #850); `.app` = $99/yr Developer ID + notarization, **hard-required** (Homebrew quarantines casks regardless of tap, `--no-quarantine` going away, Gatekeeper enforcement on casks from 2026-09-01). **DECIDED 2026-07-24: the `.app` path** — pipeline: Zed cargo-bundle fork → codesign → notarytool → staple → cask in own tap; Linux `.deb` (Vulkan/fontconfig/xkbcommon runtime deps).
- DECISION 2026-07-24: windowed desktop app (GPUI + gpui-component), TUI ruled out as product form. Second research round (desktop build/packaging specifics) in `.claude/research/2026-07-24-gpui-desktop-*.md` if present.

# Roadmap — prboard → PR Marmot (`prmarmot`)

Working plan agreed 2026-09-11. Ordered by dependency, not by size. Checkboxes are the task list;
each phase ends with a shipped release. Evidence for the ordering is in
`.claude/research/2026-09-11-monetization-and-naming-analysis.md` (local only).

## Implementation status — 2026-09-15

- **v0.6.0 released 2026-09-15 04:54 UTC** under the new name: notarized + stapled `.app`, cask
  `oliver-kriska/homebrew-tap` live with matching sha256, so Phases 0, 1a and 1b are shipped (verified
  independently via `gh release view`, the tap's `Casks/prmarmot.rb`, and the hub's `spctl` check).
- **v0.7.0 released 2026-09-16** (prmarmot-cli bundled in the app and linked by the cask). Oliver's Mac
  now has the single cask install at `/Applications/prmarmot.app`; the old `~/Applications` source build
  was moved to the Trash, and `make install` / `install.sh` now target `/Applications` too, so the
  benchmark's subject A is simply that app. A `make install` replaces it with a local ad-hoc build until
  the next `brew upgrade` — reinstall the cask before a gate run.

### Earlier status — 2026-09-11 (local, not released)

- Local rename is implemented, including one-time config/state copying with
  originals preserved and no overwrite of an existing PR Marmot directory.
  Historical references and migration paths intentionally retain the old name.
- Signed candidate CI, tag-only release CI, and cask publication are prepared.
  They have not run against Apple or the live tap. Secrets, tap creation, clean
  installation/upgrade, and notification delivery/click acceptance remain open.
- In-app updates (1c) and growth features (2a–2e) are implemented locally and
  covered by the workspace tests. Their unchecked acceptance items below remain
  the release checklist. Current scope choices: My PRs stays authored-only across
  repositories; dock counts cover loaded queues, not an account-wide total;
  watched PRs use aligned binocular controls, not a heart.
- Oliver will rename the GitHub repository manually. No version bump, commit,
  push, release, secret transfer, or domain/handle registration was performed.
- Logo/icon design is owned by the separate PR Marmot logo thread; current
  production assets remain the existing PR glyph until a design is approved.

## Strategic roadmap — decided 2026-09-14

Oliver's product ladder (assessment behind it: `.claude/research/2026-09-14-swift-native-mac-ipad-assessment.md`):

| Rung | Product | Stack | Price | Role |
|---|---|---|---|---|
| 1 | **PR Marmot for Mac + Linux** | Rust, GPUI, `gh` login, local only | Free, MIT, forever | Acquisition engine, the OSS brand |
| 2 | **PR Marmot for iPad (+ iPhone)** | Swift/SwiftUI over `prmarmot-core` via UniFFI | Paid, App Store (free download + one-time unlock) | First revenue, no server |
| 3 | **PR Marmot Cloud** | Hosted watcher (Phoenix/Fly.io or Workers) + APNs | Subscription | The only way mobile alerts are real; second revenue |
| 4 | Mac App Store copy of rung 1 (optional) | Same OSS app, sandboxed, direct-HTTP transport | Paid (Maccy model) | Only if cheap once rung 2 exists |

Why in this order: rung 1 has zero stars today, so nothing above it is justified yet; rung 2 needs
the direct-HTTP transport that also improves rung 1; rung 3 is the only thing that makes iPad
notifications work (iOS cannot poll in the background, pushes need a server) and it is the
subscription that scales — but it needs paying rung-2 customers to justify hosting and token custody.

Gates (numbers are targets, ASSESSMENT — revise against reality):

| Gate | Passes when | Unlocks |
|---|---|---|
| G0 memory | on the shipped cask build, 12–24 h soak, hands off: **phys_footprint** (Activity Monitor "Memory", summed over every `prmarmot.app/` process) is the headline, RSS recorded alongside for continuity with the July `measurements/`; **pass = mean phys_footprint over hours 1→end < 150 MB AND "flat" = linear-fit slope ≤ 2 MB/h over hours 1→end AND (max − min) after hour 1 ≤ 25 MB AND idle CPU ≈ 0 %** — numbers fixed 2026-09-15 before any soak run | Launch (Phase 3) |
| G1 signal | ≥ 300 stars **or** ≥ 100 waitlist signals **or** ≥ 20 issues from strangers, ~8 weeks after launch. *Waitlist signal* (decided 2026-09-16) = unique participants (comments + 👍 reactions) on one pinned GitHub Discussion "iPad app + Cloud: notify me", counted via the API — no email form, no third-party service, no PII, CSP untouched; the site's "Join the waitlist" button links to that Discussion. Emails are collected only when the iPad TestFlight opens (Phase 5). | iPad app (Phase 5) |
| G2 demand | ≥ 100 paid iPad unlocks **or** ≥ 50 % of iPad reviews/waitlist ask for alerts | Cloud (Phase 6) |

Timeline from 2026-09-14 (solo, ASSESSMENT): Phases 0–2 → week 2–6 · Phase 3 launch → week 6–8 ·
Phase 4 core work in parallel → week 8–12 · G1 read → week 16 · Phase 5 iPad → week 16–26 ·
G2 → week 30+ · Phase 6 Cloud → week 30–40.

---

## Principles

- **Free / paid line (revised 2026-09-14):** the desktop app is free and open source, all of it,
  forever — everything in this file's Phases 0–3 ships free. Paid = the iPad/iPhone app (App Store)
  and, later, the Cloud watcher subscription. Nothing shipped free is ever clawed back.
- **Early users get Cloud for life.** Anyone who installs the desktop app before 1.0 gets the Cloud
  subscription free. (Offer codes cover subscriptions *and* non-consumable unlocks at 1,000,000 per
  app per quarter since 2026-03-26, so either product could carry the promise; Cloud is the one whose
  marginal cost the promise actually affects.) Say so on the website from day one; it turns "no users yet" into a launch story.
- **Growth first, billing never before signal.** No licence keys, no payment provider, no extension
  system until stars / downloads / issues show real usage.
- **The memory gate still applies.** Keep-it-open-all-day is the promise; re-run
  `scripts/measure.sh` on the current 0.6.0 runtime before the launch push (Phase 3).

---

## Phase 0 — Rename to `prmarmot` (wordmark: **PR Marmot**)

Do this before any launch traffic; the cost grows with every user. Naming rule: **"PR Marmot"** (two
words) everywhere a human reads it — website, README title, `CFBundleDisplayName`, menu bar, dock,
release notes; **`prmarmot`** (one lowercase word) everywhere a machine reads it — binary, crates,
GitHub repo, cask token, domain, config dir, env prefix `PRMARMOT_`, social handles. Never `PrMarmot`
or `PRMarmot` (the first reads as the word "Pr", the second runs into "PRM").

- [x] Collision pass 2026-09-11: `prmarmot` free on crates.io, npm, GitHub (0 repos, user free),
      prmarmot.dev / .app / .com all unregistered. (Bare "Marmot" = apparel brand, Nice 25, and an
      unrelated SQLite tool — neither touches `prmarmot`.)
- [ ] Trademark + handle due diligence for `prmarmot` (rerun the Opus agent as done for prmarmot:
      TMview exact + contains, YouTube/X/Bluesky/Mastodon/HN handles, App Store, Reddit/Product Hunt
      manual). Report to `.claude/research/2026-09-11-prmarmot-name-due-diligence.md`.
- [x] Domains: `prmarmot.dev` and `prmarmot.com` registered 2026-09-14 (Oliver). `.io` skipped.
- [ ] Still to lock down: crates.io `prmarmot` + `prmarmot-core` placeholders, GitHub org `prmarmot`,
      `@prmarmot` on X + Bluesky, Mastodon, npm `prmarmot` defensive.
- [ ] Rename the GitHub repo `oliver-kriska/prboard` → `oliver-kriska/prmarmot` (GitHub redirects old URLs).
- [x] Cargo: package names `prmarmot` / `prmarmot-core`, binary name, repository metadata.
      Bundling uses the existing shell script, not cargo-bundle metadata.
- [x] Bundle identifier `dev.oliverkriska.prboard` → `dev.oliverkriska.prmarmot`. Updated
      `scripts/bundle-app.sh` (`CFBundleName` = `prmarmot`, `CFBundleDisplayName` = `PR Marmot`,
      `$BUNDLE_ID`), icon asset names.
- [x] Config: `~/.config/prboard/` → `~/.config/prmarmot/`; on first run, if the new dir is missing and
      the old one exists, copy it once and log it. Env vars `PRBOARD_*` → `PRMARMOT_*`.
- [x] `install.sh`: `REPO`, config path, app name, curl URL in the header comment.
- [x] Homebrew: cask token `prmarmot`, `packaging/homebrew/prmarmot.rb`, tap repo name unchanged.
- [x] Docs: README, PRODUCT.md, DESIGN.md, HANDOFF.md, CLAUDE.md, CHANGELOG header, `cliff.toml`,
      release workflow asset names (`prmarmot-vX.Y.Z-macos-arm64.tar.gz`), `.github` templates.
- [ ] Logo: marmot standing upright on a rock (sentinel pose), whistle implied; must read at 16 px
      (menu/dock) and 512 px (App icon); replaces the current generated PR icon.
- [ ] Ship `v0.6.0` as the first release under the new name; leave a one-line note in the old README
      position pointing at the new name (the repo redirect covers links).

## Phase 1 — Distribution that installs in one line

Prerequisite for everything else. Homebrew enforces Gatekeeper on casks since 2026-09-01.

### 1a. Notarized `.app`
- [x] Enrol the IdeaX Developer ID; create the **Developer ID Application** certificate; export `.p12`.
      (v0.6.0 signed by "Developer ID Application: IdeaX, s.r.o", team AUL48LCR3Y — verified 2026-09-15.)
- [x] App Store Connect API key for `notarytool` (preferred over app-specific password in CI).
- [x] CI secrets per `packaging/RELEASING.md` §2; extend `.github/workflows/release-build.yml`:
      `codesign --options runtime --timestamp` → `notarytool submit --wait` → `stapler staple` →
      `spctl -a -vv` assertion. Replace the current ad-hoc `codesign -s -` step.
- [ ] Hardened-runtime entitlements audit: the app shells out to `gh`; confirm no sandbox entitlement
      is set (sandbox would block it) and that JIT/unsigned-memory entitlements are not needed.
- [ ] Verify on a clean machine (or a fresh user account): download via browser, open, no Gatekeeper
      (partial 2026-09-15: SEO hub downloaded the v0.6.0 asset to a scratch dir; `spctl` = accepted,
      source=Notarized Developer ID; `stapler validate` OK. Fresh-account Gatekeeper dialog check still open.)
      dialog.

### 1b. Homebrew tap + cask
- [x] Create `oliver-kriska/homebrew-tap` with `Casks/prmarmot.rb` (sha256, `url` to the release
      tarball, `app "prmarmot.app"`, `zap` for `~/.config/prmarmot`, `livecheck` on GitHub releases).
- [x] Leave `auto_updates` unset (false by default) so normal `brew upgrade` includes the cask.
- [x] Release workflow implementation: after publishing, update the tap version + sha256.
      Live execution remains unverified until credentials and the tap are configured.
- [x] README install section becomes `brew install --cask oliver-kriska/tap/prmarmot` first, curl second.

### 1c. In-app update that runs Homebrew
- [ ] Update check on launch and once per day: GitHub releases API through the existing `gh`
      transport (releases are published as latest since `de08430`); compare semver with the running
      version; never more than one request per day; bounded cache of the last result.
- [ ] Detect install channel: Homebrew if `$(brew --prefix)/Caskroom/prmarmot` exists (resolve `brew`
      from the same Homebrew paths used for `gh`); otherwise "direct download".
- [ ] Banner "vX.Y.Z available — Update" in the header. Homebrew channel: the app must **quit before
      the binary is replaced** (macOS may SIGKILL a running app whose signed binary is swapped), so
      spawn a detached helper `sh -c 'brew upgrade --cask prmarmot && open -a prmarmot'` and exit.
      Direct channel: the button opens the release page.
- [ ] Failure path: if `brew upgrade` fails the helper reopens the old app and the banner shows the
      error on next launch (write the helper's exit status to a state file).
- [ ] Preference: "Check for updates automatically" (default on) in the settings editor.

### 1d. Website `prmarmot.dev`
- [ ] Static site (plain HTML/CSS or Astro), hosted on Cloudflare Pages or GitHub Pages.
- [ ] Sections: hero screenshot/GIF · one-line install · "No account. No server. No GitHub App to
      approve." · what the Note does (3 examples) · comparison with github.com/pulls · Linux · changelog
      link · privacy (everything stays on your machine).
- [ ] "Pro is coming — everyone who installs before 1.0 gets it free for life" with an email
      capture (Buttondown or a GitHub Discussion) — this is the only monetization artefact for now.
- [ ] Privacy-respecting analytics (Plausible or Cloudflare Web Analytics) — install-page views are
      the only top-of-funnel number you will have.
- [ ] Hero media: use the showcase-video plan in `.claude/research/2026-09-10-desktop-showcase-video.md`
      (OpenScreen capture of `scripts/demo.sh` fictional data).

## Phase 2 — Growth features (all free, all in-window)

### 2a. Zero-config first run: everything involving me
- [ ] New default mode when no repo is configured: one GraphQL `search` with
      `is:pr is:open involves:@me` (authored view) and `review-requested:@me` (review queue), same
      60-result page + Load more limits as today. Still one request per refresh.
- [ ] Add a **Repo** column (owner/name, dimmed) in all-repos mode; stacks group per repo.
- [ ] Repo picker becomes a filter ("All repos" as the first entry), pinned repos still one click.
- [ ] Golden tests: categorization must be identical whether rows come from one repo or many.
- [ ] Onboarding screen when `gh` is missing or not logged in: detect, explain, one-click copy of
      `gh auth login`, "Retry" button. The zero-config claim fails without this.

### 2b. Changed since I last looked
- [ ] Snapshot per PR (updatedAt, category, Note, CI state, review state, head SHA) — bounded
      `MAX_SNAPSHOTS`, persisted to a state file so it survives restarts.
- [ ] Row indicator (dot or accent) when any of those changed since the last time the row was
      selected or the window was focused; clears on selection. Filter "changed".
- [ ] "New commits since your review" as a Note prefix in the review queue (head SHA moved after
      your last review) — this is the case reviewers ask for most.

### 2c. Dock badge
- [ ] `NSApp.dockTile.badgeLabel` via `objc2-app-kit` = count of Needs-action rows (authored) +
      review-requested rows; cleared when zero; macOS only, no-op elsewhere. Setting to turn off.

### 2d. Watch a PR → notifications
The "heart" feature. Free because it runs inside the open app; the future Pro line is a *background*
agent that does this while the app is closed.
- [ ] Per-row watch toggle: `w` key and a small icon in the row; watched PRs get a filled glyph.
      Persisted (repo + number) in the state file, `MAX_WATCHES` with FIFO eviction.
- [ ] On every refresh, diff the watched PRs' snapshot (2b) and fire one notification per PR per
      change, **semantic not event-based**: "Ready for you — CI passed and Alice approved",
      "Needs you — changes requested", "Merge conflict after main moved", "Unblocked — #531 below
      it merged", "Review again — 3 commits since your approval". Reuse the Note engine: notify on
      Note *transitions*, never on every refresh.
- [ ] Global option "also notify for every PR entering Needs action" (off by default).
- [ ] `notify-rust` (macOS via the bundle id → only works from the `.app`, not `cargo run`; Linux via
      D-Bus). Clicking the notification focuses the app and selects the row (verify on macOS; may
      need `UNUserNotificationCenter` through objc2 for click handling — spike first).
- [ ] No notifications on the first refresh after launch (baseline), and none while the window is
      focused on that row. Quiet by design.
- [ ] Settings: master toggle, sound on/off.

### 2e. Snooze / follow-up
- [ ] Row states: *snoozed until <time>*, *waiting on <person>*, *waiting for CI*, *review again when
      changed*. Snoozed rows move to a collapsed "Snoozed" group and come back automatically when the
      condition is met (uses the 2b snapshot diff). `s` key, small menu.
- [ ] Persisted in the same bounded state file; shown in the Details panel.

### 2f. Linux
- [ ] `cargo-deb` `.deb` (arm64 + x86_64) built in CI on Ubuntu; runtime deps declared
      (Vulkan loader, fontconfig, libxkbcommon, wayland/x11 libs). `.desktop` file + icon.
- [ ] `install.sh` gains a Linux branch (download `.deb`, `apt install ./…`).
- [ ] Homebrew on Linux: **casks are macOS-only**, so "brew on Linux" means a *formula* that builds
      from source (4+ min, needs Vulkan headers). Ship the `.deb` first; add a formula only if asked.
- [ ] Test matrix note: NVIDIA + Wayland is the known trouble spot for GPUI; document it.

## Phase 3 — Launch

Sequencing agreed with the SEO hub 2026-09-16: **launch posts go out only after** the benchmark is
published, the three `/compare/` pages, the waitlist Discussion link, and JSON-LD are live on
prmarmot.dev — launch traffic is the only large sample the site will get for months. GEO baseline ~4
weeks *after* the posts. Ownership: this session owns the plan + G0 criteria/verdict; the SEO hub owns
the protocol, measurement and handoff acceptance; site edits go to the site session; **the soak run
itself is operator-scheduled by Oliver** (his Mac unattended ~12 h, PR Flow trial installed by him).

- [ ] **G0 / memory benchmark** — `benchmarks/2026-09-memory/` per the hub protocol (subjects: cask build
      of the current release at `/Applications/`, PR Flow trial, one github.com/pulls Safari tab as
      marginal cost; `brew reinstall --cask prmarmot` first if a `make install` build replaced it).
      Runnable now (notarized release, cask live). Oliver
      picks the night; verdict recorded here + HANDOFF. Until published: no "lightweight / low memory /
      fast" wording anywhere. Before publishing: send PR Flow's developer the README + results (Oliver's
      call, recommended); read prflow.app/terms in full.
- [x] PRFlow name disambiguation added to public CLAUDE.md + HANDOFF.md (2026-09-16; uncommitted).
- [ ] Repo hygiene: GitHub **topics** (github, pull-requests, code-review, rust, gpui, macos, linux,
      developer-tools, cli, claude-code) — still empty 2026-09-16; social preview + homepage are set.
- [ ] Pinned GitHub Discussion "iPad app + Cloud: notify me" (= the waitlist, see G1); Discussions on,
      issue templates, a "what would you pay for" poll.
- [ ] README: hero GIF at the top, install one-liner (cask), the "no account, no server" line, the CLI
      + agent-skill section, comparison table (github.com/pulls · gh-dash · RepoBar · Gitify · PR Flow).
- [ ] Posts (after the gates above): Show HN, r/rust, r/github, Lobsters, X/LinkedIn; awesome-rust,
      awesome-github, awesome-claude-code (the CLI skill is the hook there). Lead with "PR triage for
      you and your agents".
- [ ] Adoption tracking without telemetry: weekly script appending stars, release downloads, traffic
      views/clones, Discussion participants to `measurements/adoption.csv`.
- [ ] Tag `v1.0.0` when Phase 1 + 2a/2b/2d + benchmark are stable; grandfathering cutoff is announced
      against this tag.

## Phase 4 — Core becomes app-store-ready (parallel with post-launch measuring)

Needed by every rung above 1; also improves rung 1 (onboarding without `gh`, Enterprise hosts).
Keep `prmarmot-core` the single source of categorization/Note; golden tests stay the contract.

- [ ] Direct HTTP `GithubTransport`: same GraphQL document, `reqwest`/`ureq` behind the trait, same
      bounded paging, `rateLimit{}` parsing, host configurable (github.com + GHES).
- [ ] `TokenSource` for OAuth: register a GitHub OAuth App; **Device Flow** first (no secret, no
      server — works on desktop). Store the token in the OS keychain (`keyring` crate), never in TOML.
- [ ] Desktop: transport picker — `gh` if present (zero-config), else OAuth device flow onboarding.
- [ ] UniFFI bindings for `prmarmot-core` (`uniffi` crate, proc-macro style): expose board rows,
      categories, Note/Blocker, snapshot diff, and a callback interface for the transport so Swift
      can supply `URLSession`. CI job builds an `XCFramework` (macOS arm64, iOS, iOS simulator) and
      publishes it as a Swift package `PRMarmotCore` from a tag.
- [ ] Contract tests: the Swift package runs the same golden fixtures through the bindings.
- [x] OAuth for iOS decided 2026-09-14 (research: `.claude/research/2026-09-14-ios-ipad-and-cloud-research.md`
      §1): **Device Flow, no token-exchange server, ever.** `gh` itself uses Device Flow; GitHub
      requires no client secret for Device Flow tokens *or* their refresh; PKCE (2025-07) still needs
      the secret and is absent on GHES. A Worker would only save one 8-char paste per ~6 months in
      exchange for uptime + GDPR surface. Always send a `User-Agent` header (silent 403 otherwise).
- [ ] Rate-limit note for mobile: GraphQL budget (5,000 pts/h) is shared with the user's `gh`/IDE/PATs;
      board query ≈ 1–2 pts, so the 30 s floor / 5 min default stand on iOS too.

## Phase 5 — PR Marmot for iPad (and iPhone) — first paid product (after G1)

Scope is the mockup: sidebar (My PRs / Review queue / Watched / Snoozed, repositories), detail
pane (Note, checks, reviews, threads, branch, Open in GitHub, Watch, Snooze, Copy, Share).
Foreground triage companion; **no alert promises** until Phase 6.

- [ ] SwiftUI app target (iPadOS + iOS, Universal Purchase), `PRMarmotCore` dependency, URLSession
      transport, OAuth per Phase 4, Keychain token storage, same refresh floor/rate-limit rules.
- [ ] Parity with desktop categories, stacks, Note, search, change indicators, watches, snooze —
      local state files mirror the desktop's bounded stores (no sync in v1).
- [ ] Platform wins that cost little: Home/Lock Screen widget ("N need you", timeline refresh),
      App Intents/Shortcuts ("what needs me"), keyboard shortcuts on iPad, Handoff to GitHub.
- [ ] Best-effort background refresh (`BGAppRefreshTask`) with local notifications — label it
      "best effort" in the UI and never in a screenshot: hours of latency, never after force-quit, in
      Low Power Mode, or with Background App Refresh off. iOS favours apps you open often, which is
      the opposite of the pitch. The reliable version is Phase 6.
- [ ] Pricing (revised 2026-09-14): free download, one-time non-consumable unlock at **€19.99–24.99**
      — €12.99 is below the category floor (Working Copy $35.99, Prompt 3 $49.99, Textastic $69.99;
      nobody charges up front). Free tier = one repo, read-only; unlock = all repos + watches + snooze.
      Standard IAP + Small Business Program 15 % stays right in the EU after the 2026-10-01 terms.
- [ ] Pitch discipline: do NOT lead with stacked PRs on mobile (GitHub Mobile shipped stacks
      2026-07-30). Lead with the Note and state-based alerts: GitHub Mobile pushes *events* (mention,
      review requested, approval, CI result), never *states* (mergeable, all-green, conflicted), and
      has no snooze. Nearest prior art with snooze (Pocket Trailer iOS) was abandoned in 2023.
- [ ] Book a free "Meet with App Review" consultation (Tue/Thu) before Phase 6 and ask whether
      guideline 5.1.1(v) ("tokens to social networks off of the device") applies to a GitHub token.
      Research reads no (scoped to social networks; Buffer/Hootsuite ship worse), but no precedent
      exists either way and the answer gates Cloud.
- [ ] TestFlight beta from the desktop waitlist first; App Store listing, privacy nutrition label
      ("data not collected"), review notes explaining OAuth.
- [ ] Website: iPad page, App Store badge; desktop README cross-links.

## Phase 6 — PR Marmot Cloud — subscription (after G2)

The hosted sentinel: computes Note transitions server-side and pushes to iPad/iPhone (APNs) and
optionally Mac. Opt-in; local stays the default and the free path.

- [ ] Backend (decided by research 2026-09-14, §5): **Phoenix on Fly.io + `rustler_precompiled`
      NIF around `prmarmot-core` + `pigeon` 2.1 for APNs.** Workers/WASM rejected: second language,
      a `chrono`/wasm-bindgen panic trap, and Durable Object hibernation risk swinging the 10k-user
      bill between $50 and $400. Cost is otherwise noise: ~$5–150/month from 100 to 10,000 users.
- [ ] Ingest (inverted from the original plan): **poll with the user's OAuth token first**, same
      board query, adaptive interval. GitHub App webhooks become a *Team-tier* upgrade — a personal
      App install does not reach repos in orgs the user doesn't own (org-admin approval), which would
      invert the "no App to approve" advantage. Polling is mandatory anyway: merge conflicts have no
      webhook event. Encrypted token storage (GitHub's own guidance for backends), per-user isolation,
      in-app revoke, delete-my-data endpoint, minimal scopes.
- [ ] Push: APNs with the semantic Note transitions (same strings as desktop 2d); notification
      actions (Open, Snooze); digest option.
- [ ] Billing: StoreKit subscription in the iPad app (target €3.99/mo or €29/yr). Offer codes for
      pre-1.0 desktop users ("Cloud for life") — Apple retired IAP promo codes 2026-03-26 and offer
      codes now cover non-consumables too, 1,000,000 per app per quarter, so the grandfathering promise
      could equally be the iPad unlock; keep "Cloud for life" as worded. Web billing via a merchant of
      record only if Mac-only customers ask.
- [ ] Desktop opt-in: the Mac app can register for Cloud pushes instead of local polling (still free
      app; the subscription is the server).
- [ ] This is also where any Team features would live (review SLAs, stale-PR digests, Slack) —
      out of scope until Cloud exists.

## Phase 7 — optional: Mac App Store copy of the OSS app

Only if a sandboxed GPUI build proves cheap (spike: Zed is not on MAS). Maccy model: Homebrew/GitHub
build free, App Store copy paid for convenience. Requires Phase 4 transport. Not a priority.

## Maybe / small

- [ ] GitHub Enterprise hosts on desktop (`GH_HOST` passthrough) — ship blind only with a tester.
- [ ] Launch at login (`auto-launch` / `SMAppService`).
- [ ] Menu-bar / tray item — verify-first: `tray-icon` vs GPUI's run loop unproven, Linux worse.
- [ ] Always-on-top compact board.

## Metrics to track weekly (no telemetry; `measurements/adoption.csv`)

stars · release downloads · tap installs (release asset counts) · website install-page views ·
waitlist emails · issues from strangers · (Phase 5+) App Store units, proceeds, ratings ·
(Phase 6+) subscribers, churn, push volume, hosting cost per subscriber.

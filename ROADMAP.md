# Roadmap — prboard → PR Marmot (`prmarmot`)

Working plan agreed 2026-09-11. Ordered by dependency, not by size. Checkboxes are the task list;
each phase ends with a shipped release. Evidence for the ordering is in
`.claude/research/2026-09-11-monetization-and-naming-analysis.md` (local only).

## Implementation status — 2026-09-11 (local, not released)

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

## Principles

- **Free / paid line:** everything you see while the window is open is free and open source.
  Anything the app does while you are *not* looking at it (a background agent, menu-bar presence,
  digests) is the future Pro layer. Nothing on this roadmap is Pro. Nothing shipped free is ever
  clawed back.
- **Early users get Pro for life.** Anyone who installs before 1.0 is grandfathered. Say so on the
  website from day one; it turns "no users yet" into a launch story.
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
- [ ] Lock down in this order (irreversible ones first): (1) `prmarmot.dev`, (2) crates.io `prmarmot`
      + `prmarmot-core` placeholders, (3) GitHub org `prmarmot`, (4) `prmarmot.com`, (5) `@prmarmot`
      on X + Bluesky, (6) Mastodon, (7) npm `prmarmot` defensive. Skip `.io`.
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
- [ ] Enrol the IdeaX Developer ID; create the **Developer ID Application** certificate; export `.p12`.
- [ ] App Store Connect API key for `notarytool` (preferred over app-specific password in CI).
- [ ] CI secrets per `packaging/RELEASING.md` §2; extend `.github/workflows/release-build.yml`:
      `codesign --options runtime --timestamp` → `notarytool submit --wait` → `stapler staple` →
      `spctl -a -vv` assertion. Replace the current ad-hoc `codesign -s -` step.
- [ ] Hardened-runtime entitlements audit: the app shells out to `gh`; confirm no sandbox entitlement
      is set (sandbox would block it) and that JIT/unsigned-memory entitlements are not needed.
- [ ] Verify on a clean machine (or a fresh user account): download via browser, open, no Gatekeeper
      dialog.

### 1b. Homebrew tap + cask
- [ ] Create `oliver-kriska/homebrew-tap` with `Casks/prmarmot.rb` (sha256, `url` to the release
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

- [ ] Re-run the memory gate on the current runtime (`scripts/measure.sh` 12–24 h) and record the
      verdict in HANDOFF; do not launch a keep-it-open app without it.
- [ ] README: hero GIF at the top, install one-liner, the "no account, no server" line, comparison
      table (github.com/pulls · gh-dash · Gitify · prmarmot).
- [ ] GitHub Discussions on, issue templates, a "what would you pay for" poll pinned.
- [ ] Posts: Show HN, r/rust, r/github, Lobsters, X/LinkedIn; awesome-lists (awesome-rust,
      awesome-github); gh-dash and Gitify comparison angle.
- [ ] Adoption tracking without telemetry: weekly script appending stars, release downloads, traffic
      views/clones, tap installs to `measurements/adoption.csv`.
- [ ] Tag `v1.0.0` when Phase 1 + 2a/2b/2d are stable; grandfathering cutoff is announced against
      this tag.

## Later / maybe

- [ ] GitHub Enterprise hosts (`GH_HOST` passthrough; `gh` already handles auth per host). Cheap,
      but ship blind only with a volunteer tester — no proof otherwise.
- [ ] Launch at login (`auto-launch` / `SMAppService`) — fits the keep-it-open promise.
- [ ] Menu-bar / tray item — verify-first: `tray-icon` vs GPUI's run loop is unproven, Linux worse.
- [ ] Always-on-top compact board.
- [ ] Direct HTTP transport + own OAuth (`TokenSource` trait) — needed for a Mac App Store build
      (sandbox blocks shelling out to `gh`) and for users without `gh`.
- [ ] **Pro layer** (only after signal): background agent that watches while the app is closed,
      menu-bar attention count, notification actions, custom attention rules, daily digest. Sold via a
      merchant of record (Lemon Squeezy / Paddle / Polar — not yet evaluated), one-time licence with
      a year of updates preferred over a subscription.

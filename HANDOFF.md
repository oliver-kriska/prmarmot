# prboard — Project Handoff

**Working name:** prboard (rename-friendly — see below). **Owner:** Oliver Kriška. **License:** MIT (intended), public, open source. **Platforms:** macOS + Linux. **Created from research on:** 2026-07-24.

> **Read this first.** It's the executive summary of a research spike for a new project: an open-source **GitHub PR-review dashboard in Rust**. It distills five detailed research files (linked at the bottom) into a recommendation, a risk list, a v1 scope cut, and a build roadmap. A fresh session should be able to start building from this file alone. The deeper docs are there when you need the evidence behind a claim.
>
> Everything is tagged **FACT** (verified, sourced), **ASSESSMENT** (my judgement — challenge it), or **OPEN QUESTION** (unverified — with a way to answer it). Oliver said explicitly: don't fabricate. Where a number couldn't be verified, it's an OPEN QUESTION, not a guess.

---

## Current update — 2026-09-08

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

**ASSESSMENT:** Search, clear ownership, and stack relationships improve this
dense dashboard more than decorative artwork. The design rationale and next UX
priorities are in `DESIGN.md`; limits and reviewer-vs-assignee semantics are in
README. **OPEN QUESTION:** The framework's overnight memory/idle-GPU gate remains
unvalidated on the upgraded runtime. These specifically requested changes do not
establish that the gate passed or unblock the remaining roadmap automatically.

## 1. What we're building

A dashboard of GitHub pull requests you keep open all day. A dense table — PR number/link, draft/ready, CI state, requested reviewers, completed reviews, unresolved-thread count, merge-conflict flag, bug label, linked issue, and a computed **"Note"** saying what to do next / what it's blocked on. **Three views:** (1) all open PRs in a repo with filters; (2) *my authored PRs*, triaged action → awaiting-review → drafts; (3) *my review queue*. **Multiple named tabs**, each a repo or group of repos. **Auto-refresh** (default 5 min). **Read-only + open-in-browser only** — no AI, no reviewer-assignment, no merging in v1. Installable via **Homebrew**.

There is a **working shell prototype** (`~/.claude/skills/pr-board/`) that already does the data + categorization + note logic in one GitHub GraphQL call. **prboard v1 = port that prototype to Rust behind a refreshing UI.** The prototype is the behavioral spec; it's transcribed in full in the data-layer doc.

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
- Config in TOML at `$XDG_CONFIG_HOME/prboard/config.toml`.
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

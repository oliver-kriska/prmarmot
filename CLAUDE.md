# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

PR Marmot is an open-source GitHub PR-review dashboard: a windowed native desktop app in Rust (GPUI + gpui-component), macOS + Linux, MIT, at `github.com/oliver-kriska/prmarmot`. Workspace: `core/` (`prmarmot-core` — categorization/Note/transport, no GPUI dep, golden-tested against the shell prototype) and the root `prmarmot` GPUI binary. The project was named `prboard` through v0.5.3; preserve explicit historical migration references, but all active identity uses PR Marmot / `prmarmot`.

**Where the build stands (2026-07-24, Steps 0–0.8 done):** the skeleton grew into a usable app — config file + full persistence (repo/theme/view/window written back via `toml_edit`), repo picker Select, **My PRs | Review queue** segmented switcher in a custom transparent titlebar (`1`/`2` direct, `v` toggles), Labels chips, glyph-first Reviewed-by, Mantine/open-color design system (`src/design.rs`), `gh` watchdog timeouts, 60 s sync-label tick, Spotlight-launchable `.app`, and a public v0.1.0 pre-release under the former name. The framework gate (overnight memory measurement) may still be pending — check `measurements/` and the git log before assuming it passed. Step 1+ features (HANDOFF roadmap: All-open view, change indicators, group headers, filters, dock badge) stay blocked on the gate.

**Read `HANDOFF.md` first.** It is the executive summary of the research plus the running build log (Step 0.x entries + the post-gate roadmap queue), written so a fresh session can start from it alone. Evidence behind every claim is in `.claude/research/*.md` (indexed in HANDOFF §6) — **local-only since 2026-07-24: `.claude/research/` and `.claude/scriptorium/` are gitignored and were purged from public git history; never recommit them.** Claims are tagged FACT / ASSESSMENT / OPEN QUESTION — keep that discipline; never fabricate a number where an OPEN QUESTION belongs.

## Decisions already made — do not re-litigate

- **Windowed desktop app, NOT a TUI** (Oliver's product decision, 2026-07-24). The ratatui analysis in the UI-framework doc is a record of the alternative, not an option.
- **UI: GPUI + gpui-component.** Upgraded at Oliver's request to published `gpui-component = "=0.6.1"` (with `gpui-base = "=0.6.1"`), with `gpui-pre` and `gpui-pre-platform` pinned to `=0.3.5` (aliased as `gpui` and `gpui_platform`); bump all four together, never one alone. Bootstrap via `gpui_platform::application()` and wrap the view in component `Root`. The virtualized widget is now **`DataTable` + `TableState` + `TableDelegate`**. The 0.5.1 instructions in historical research no longer apply to current code. Do not copy unpinned git-main snippets.
- **Data: shell out to `gh api graphql`** behind a `GithubTransport` trait; one request per refresh, authored search starts at 60, review mode two 60-result aliases (requested plus candidates filtered to zero pending reviewer requests). User-invoked Load more is capped at five pages per alias; refresh resets pagination. Show truncation; never fan out per-PR REST. Repository discovery separately uses paginated `user/repos`, bounded at 1,000 entries/10 pages, merged with configured/current repos.
- **Refresh: default 5 min, hard floor 30 s** (the "5 s floor" idea is wrong — math in the data-layer doc).
- **Auth: reuse `gh` CLI login**, behind a `TokenSource` trait; OAuth/PAT is post-v1.
- **v1 is read-only + open-in-browser.** No AI, no reviewer assignment, no merging, no Windows (full in/out list: HANDOFF §4).
- **Distribution:** macOS signed + notarized `.app` — hard-required (Homebrew enforces Gatekeeper on casks from 2026-09-01; quarantine applies regardless of tap). Pipeline: Zed's cargo-bundle fork → `codesign --options runtime` → `notarytool submit --wait` → `stapler staple` → hand-authored cask in own tap. cargo-dist cannot produce `.app`/cask. Linux: `.deb` preferred (Vulkan/fontconfig/xkbcommon runtime deps).

## The memory gate (blocks Step 1+)

The framework choice is validated by an overnight measurement, not assumed: **idle RSS flat and < ~150 MB over 12–24 h, ~0 idle GPU**. Tooling: `scripts/spike-run.sh <owner/repo>` (app + per-minute RSS sampler + caffeinate), or attach to an already-running instance with `scripts/measure.sh <pid>` + `caffeinate -i -w <pid>` (preferred — it measures the app as actually launched, e.g. from Spotlight); `scripts/measure-summary.sh` for the verdict; `leaks`/`sudo powermetrics --samplers gpu_power` for depth. **While a gate run is live: no keystroke automation against any PR Marmot window** (a focus race once quit the measured instance via its `q` handler) **and never reinstall/replace `~/Applications/prmarmot.app`** (macOS can SIGKILL a running app whose signed binary is swapped). **If the gate fails, STOP and bring the numbers to Oliver** — there is no pre-committed fallback framework. Do not start tabs/filters/config (Step 3) before the gate passes.

## Behavioral spec

- The shell prototype at `~/.claude/skills/pr-board/scripts/pr-board.sh` (+ its `SKILL.md`) is the spec for the GraphQL query, categorization (action/await/draft), and Note logic. It is transcribed in full in `.claude/research/2026-07-24-github-data-layer.md`. The Rust port lives in `core/src/board.rs` and is **pinned by golden tests**: `scripts/prototype-jq/` holds the prototype's verbatim jq, `scripts/gen-golden.sh` regenerates `core/tests/golden/`, and `core/tests/parity.rs` asserts field-for-field equality. If parity fails, fix the port, never the golden.
- Reference codebases on disk: `~/Projects/pr_flow` (previous GPUI attempt — read its `SOLUTIONS_EXTRACTED.md` for the unbounded-cache memory bug that killed it, and mine its gpui-component table/state code), `~/Projects/agentrix` and `~/Projects/vibenalytics` (shipped Rust TUIs, for patterns and distribution).

## Guardrails (from the PRFlow post-mortem — encode from line one)

- Bound every cache: explicit `MAX_*` + FIFO eviction; no unbounded `HashMap`.
- Clamp every rate-limit wait to `max(60).min(900)`; never sleep until a far-future reset.
- No perpetually animating spinner — GPUI renders on demand, and continuous animation defeats the idle-GPU property the framework was chosen for.
- Don't pull in a runtime you don't use (a thread + channel may suffice over full tokio).
- Always show "last synced Xm ago" + the live `rateLimit{}` budget.
- Keep transport, token source, and issue-link detection behind traits/config — no hard-coded issue prefixes.

## Building and testing

- **Toolchain:** latest stable Rust — verified with 1.97.1; dependency manifests require at least 1.92 across supported platforms (minimum toolchain not tested). macOS needs the Xcode **Metal Toolchain** (`xcodebuild -downloadComponent MetalToolchain`) or gpui's shader build fails.
- `cargo test -p prmarmot-core` — the fast spec suite (no GPUI compile); run after any change to `core/`.
- **`make check` is the quality gate = CI** — `cargo fmt --all --check` + `cargo clippy -p prmarmot-core --all-targets -- -D warnings` + `cargo test -p prmarmot-core`. All fast, no Metal. Keep it green; CI (`.github/workflows/ci.yml`) runs exactly this on push/PR to main and enforces `-D warnings`. `make fix` auto-fixes; `make lint-all` clippies the whole workspace incl. the GPUI binary (needs Metal). `make help` lists targets.
- **Git hooks:** `make hooks` sets `core.hooksPath=.githooks`. `pre-commit` runs `make check` but only when `.rs`/`Cargo.*` are staged (docs-only commits stay instant; bypass with `--no-verify` or `PRMARMOT_SKIP_HOOKS=1`). `pre-push` runs `make verify` — the full-workspace gate incl. the GPUI binary (`fmt --all --check` + `clippy --all-targets -D warnings` + `test --workspace`), the only automated check the app binary gets since CI is core-only (needs Metal, slower; bypass with `git push --no-verify` or `PRMARMOT_SKIP_HOOKS=1`). `commit-msg` warns (never blocks) when a subject isn't a Conventional Commit.
- **Changelog / "what's unreleased":** `cliff.toml` drives [git-cliff](https://git-cliff.org). Conventional-commit prefixes group into `CHANGELOG.md`; unconventional commits are kept under "Other" (not dropped). `.github/workflows/unreleased.yml` keeps a **draft "Unreleased" GitHub release** synced with everything on main not in the latest tag; `make unreleased` shows it locally; `make changelog` regenerates `CHANGELOG.md`. Real releases use the fail-closed tag-only flow in `packaging/RELEASING.md`; this workflow only manages the draft, so no conflict. `.github/dependabot.yml` files weekly cargo + actions bumps (prefix `deps` → 📦 Dependencies).
- `cargo build` for the dev binary; `cargo build --release` (LTO) for anything measured or shipped.
- Run: `./target/debug/prmarmot --repo owner/name` (`--review` for the incoming queue). Config: TOML at `~/.config/prmarmot/config.toml` (repo/repos/refresh/theme/reviewers/issue_link); precedence CLI > env > file > cwd detection (see README).
- Local .app: `scripts/bundle-app.sh` (after a release build) installs an ad-hoc-signed `~/Applications/prmarmot.app`; the fail-closed signed/notarized pipeline is `packaging/RELEASING.md`.
- Visuals: `src/design.rs` `refine_theme()` owns the palette (spec: `.claude/research/2026-07-24-visual-design.md`); it must be re-applied after every `Theme::change`/`sync_system_appearance` — `ThemePref::apply` does this. Status semantics render as themed dots + text, never raw emoji (core note strings keep their emoji; the UI strips them).
- **Screenshot verification gotcha:** GPUI does not repaint occluded windows. Bring the window frontmost (osascript `set frontmost … to true`) before `screencapture`, or you'll capture a stale first frame that looks like a hung fetch.
- `.claude/research/2026-07-24-step0-build-findings.md` records where reality corrected the research (no `DataTable` in 0.5.1, MSRV 1.88, Metal Toolchain, ~4 min cold build) — read it before trusting the r2 docs on those points.
- **Historical release:** the former `github.com/oliver-kriska/prboard` identity published a v0.1.0 pre-release (`prboard-v0.1.0-macos-arm64.tar.gz`, ad-hoc-signed app). Do not rewrite those historical assets. Current releases use `github.com/oliver-kriska/prmarmot`, `prmarmot-vX.Y.Z-macos-arm64.tar.gz`, and the signed/notarized tag-only workflow. Never install over `~/Applications/prmarmot.app` while an instance is running/measured.
- **Git hygiene:** history was rewritten on 2026-07-24 to purge `.claude/research` + `.claude/scriptorium` (they're gitignored now). A plugin hook blocks bare `git push --force` — use `--force-with-lease=<ref>:<expected-remote-sha>` with an explicit SHA (after a local history rewrite the tracking ref is rewritten too, so the implicit lease is stale). Note `gh release create` tags exist only on the remote until fetched; raw.githubusercontent.com caches ~5 min after a push.

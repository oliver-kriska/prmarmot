# Product

<!-- impeccable:product-schema 1 -->

> Updated through Impeccable init on 2026-09-09 using Oliver's confirmed requirements,
> README.md and the current implementation. Retained assumptions are explicitly marked.
> DESIGN.md owns visual recipes; this record distinguishes shipped behavior from goals.

## Platform

PR Marmot is a native desktop app for macOS and Linux, implemented in Rust with GPUI + gpui-component.
This is not a website, iOS app, or Android app. Impeccable's current platform enum does
not represent desktop GPUI; use native window inspection, not browser/DOM checks.

## Users

Software engineers and team leads who juggle many open PRs across one or more repos — Oliver first,
then other GitHub users. The app is an **always-open, glanceable board**: it
lives on a second display or Space next to the editor and terminal, is consulted in 2-second glances
between tasks, and must never demand attention it hasn't earned. **ASSUMED:** users are GitHub-fluent
(`gh` CLI already authenticated) and keyboard-first, with full mouse support as the equal path.

## Product Purpose

A native (GPUI) macOS + Linux dashboard of GitHub pull requests: one dense table — PR, status, CI,
labels, reviewers, title — whose spine is a computed **"Note" that says what to do next** ("no
reviewers — assign X", "merge conflict — rebase", "awaiting review"). Two views: authored (triaged
action → awaiting → drafts) and review queue. All-open is not implemented. The app is
**read-only + open-in-browser**; one GraphQL operation per refresh; auto-refresh default 5 min.
Success looks like: you know what needs you
without clicking anything, the data is never stale without saying so ("synced Xm ago" + API budget
always visible), and the app stays quiet at idle. Flat RSS and ~0 idle GPU remain measurement
goals, not verified guarantees on the upgraded runtime. **ASSUMED:** "solo-maintainer
open source" means polish budget goes to the one table, not to breadth.

## Positioning

The computed Note joins CI, reviews, conflicts, and unresolved discussion into the next
step for each PR. Queue sections distinguish requested reviews from optional work; native
stack metadata shows dependency order without inferring relationships from labels.

## Operating Context

- Reuse the user's authenticated GitHub CLI (`gh`); no separate account or token flow.
- Oliver currently works mainly across two repositories and prefers one-click pinned
  shortcuts over more filter tabs. Keep the searchable full repository picker.
- Support quick inspection, switching between authored/review queues, and opening GitHub
  for actions. Mouse and keyboard paths should agree.
- Settings edits reviewer hints, refresh, theme, and optional issue links without a restart.
  Environment-controlled fields remain visibly locked and preserve their file values.

## Capabilities and Constraints

- Two queues, configurable global reviewer suggestions, labels, native GitHub stacks,
  pinned repos (up to 12), local search over loaded rows, and a selection-following Details panel.
- Reviewer suggestions are hints, not assignment, ownership inference, or CODEOWNERS processing.
  Available reviews have no pending user/team reviewer request; this is not an assignee filter.
- Bounded discovery (1,000 repos) and explicit pagination (five pages per search alias).
  Refresh resets pagination. Never imply that a partial list contains every accessible PR.
- Read-only GitHub access: no merging, commenting, or reviewer assignment. No embedded browser.
- Default refresh 300 seconds, minimum 30 seconds. No continuous idle animation or unbounded caches.
- Prepared prebuilt distribution: signed and notarized Apple-silicon macOS via
  direct download and Homebrew. The first release still requires provisioned
  Apple/tap credentials and manual acceptance. Linux/Intel Mac remain source-only.

## Evidence on Hand

- `README.md`: install/configuration instructions and explicit supported-platform/data limits.
- `assets/screenshots/`: real rendered UI using fictional repositories, people, and PRs only.
- `scripts/demo.sh`: isolated offline fixtures for authored/review queues, labels, stacks,
  reviewer states, and settings. Do not publish screenshots of Oliver's real work data.
- Core golden tests pin categorization to the prototype; they do not prove UX or accessibility.
- `HANDOFF.md`: history and unresolved overnight memory/idle-GPU gate. No performance claims
  should be fabricated from screenshots, short sessions, or passing tests.

## Product Principles

1. Make the next useful step apparent without opening every PR.
2. Distinguish responsibility from optional work; never imply GitHub assignments that do not exist.
3. Preserve fast switching between frequent repositories without independent polling tabs.
4. Show freshness, truncation, and failure honestly, with a discoverable recovery path.
5. Keep the app read-only, native, and quiet when the user is not interacting.

## Brand Personality

**Calm · native · exact.** The tool should disappear into the task. Emotional
goal: quiet confidence — the board is trusted precisely because it never exaggerates. Voice in copy
is lowercase-terse ("synced 2m ago", "no reviewers — assign alice"): declarative, no exclamation,
no mascot. The single permitted emoji is 🐛 on the bug label (semantic, a product decision).

## Anti-references

- **Web-app-in-a-window**: Electron chrome, shadcn defaults (pure-black/white surfaces, 16 px text,
  6–8 px radii on everything), oversized empty states with illustrations. PR Marmot's benchmark is
  Finder/Mail density, not a SaaS dashboard.
- **Emoji-as-status dashboards** (the shell prototype's 🔴✅⚠️ language): platform-colored, unthemed,
  oversized at 13 px. Themed dots + text words replaced them; never regress.
- **The GitHub notifications firehose**: undifferentiated, unranked, anxiety-inducing. PR Marmot ranks
  (action first) and computes the next step; it must never feel like a backlog.
- **TUI aesthetics**: ruled out as a product form (Oliver, 2026-07-24). No box-drawing, no
  full-block selection bars, no terminal color slabs.
- **Anything that animates at idle** — a perpetual spinner would also break the framework's
  idle-GPU gate. This is a hard rule, not a taste.

## Design Principles

1. **The Note is the product.** Every visual decision serves "what do I do next"; nothing on the
   board may out-shout an action note, and an action note must survive truncation.
2. **The calm rule.** Bad states get colored text; good states get only a dot and recede. Color
   ranks urgency — if everything is red, nothing is.
3. **Native density, not web scale.** 13 px cells, 30 px rows, 4 px radii, system font, zebra over
   hairlines, full-bleed table. Finder is the yardstick.
4. **Idle means idle.** No continuous animation, ever. State changes may transition; nothing loops.
5. **Position is the primary signal.** Categories sort action → awaiting → drafts; tint and color
   only confirm what position already says.
6. **Never lie about freshness.** "synced Xm ago" and the live rate-limit budget are permanent UI;
   errors replace the sync status rather than hiding behind it.

## Accessibility & Inclusion

**ASSUMED: WCAG 2.x AA** as the floor — already the working standard: every text token in
`src/design.rs` is contrast-checked against its background (ratios documented in
`.claude/research/2026-07-24-visual-design.md`). No color-only signals: every status dot is paired
with a text word ("pass", "fail") or a glyph (✓ ± –). Reduced motion is satisfied by design (no
motion at idle). Full keyboard operation with a visible legend; tooltips carry the untruncated text.

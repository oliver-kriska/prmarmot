# Roadmap — PR Marmot

The public plan for PR Marmot (`prmarmot`): what exists, what comes next, and what has to be true
before anything paid gets built. Ordered by dependency, not by size. Checkboxes are the task list.

Two ways to influence it:

- **Vote on features** in [Discussions → Ideas](https://github.com/oliver-kriska/prmarmot/discussions/categories/ideas).
  One thread per feature; 👍 means "build this next". The ranked list below is the current order and
  upvotes reorder it.
- **The waitlist** for the iPad app and PR Marmot Cloud is one pinned thread:
  [iPad app + Cloud: notify me](https://github.com/oliver-kriska/prmarmot/discussions/8). React or
  comment there. That count is the signal that decides whether those get built (gate G1 below).

## Where things stand — 2026-09-16

- **v0.7.1** is the current release: Developer-ID-signed and notarized `.app`, Homebrew cask in
  `oliver-kriska/homebrew-tap`, and `prmarmot-cli` bundled in the app and linked by the cask. Homebrew,
  the install script and `make install` all use the same `/Applications/prmarmot.app`.
- Shipped: **My PRs | Review queue** for one repository or all repositories, pinned repos, filters,
  Load more, stack grouping, watch/snooze, "changed since you looked" markers with notifications and
  a dock badge, copy-a-group sharing, editable Settings, daily update checks, and the CLI (table,
  Markdown, stable JSON, a blocking `watch` event stream, and a built-in coding-agent skill).
- Platforms: macOS (Apple Silicon) binaries. Linux and Intel Macs build from source until the `.deb`
  in Phase 2 ships.

## The product ladder

| Rung | Product | How it is built | Price | Role |
|---|---|---|---|---|
| 1 | **PR Marmot for Mac + Linux** | Rust, GPUI, reuses your `gh` login, everything stays on your machine | Free, MIT, forever | The product; the open-source brand |
| 2 | **PR Marmot for iPad (+ iPhone)** | Native Swift over the same `prmarmot-core`, signed in with your GitHub account, no server | Paid, one-time unlock on the App Store | First paid product |
| 3 | **PR Marmot Cloud** | A hosted watcher that computes the same Note transitions while your laptop is closed and pushes to your phone | Subscription | The only way mobile alerts can be reliable |
| 4 | Mac App Store copy of rung 1 (optional) | The same open-source app, sandboxed | Paid for convenience | Only if it turns out cheap |

Why in this order: nothing above rung 1 is justified until rung 1 has users; rung 2 needs the direct
HTTP transport that also improves rung 1; rung 3 is the only thing that makes iPad notifications
real (iOS cannot poll in the background), and it needs paying rung-2 users to justify running servers
and holding tokens.

## Principles

- **The desktop app is free and open source, all of it, forever.** Everything in Phases 0–3 ships
  free. Paid products live above it (rungs 2–3). Nothing shipped free is ever clawed back.
- **No "free for life" or grandfathering promises.** The waitlist asks for a signal and promises
  nothing. Whether early users are rewarded is decided when a paid product exists, and announced then.
- **No billing before signal.** No licence keys, no payment provider, no extension system until
  stars, downloads and issues show real usage.
- **Read-only, no AI, calm.** v1 does not comment, approve or merge, and does not review diffs for
  you. It tells you what needs you and gets out of the way.
- **Keep-it-open-all-day is the promise**, so memory is measured, not assumed (gate G0).

## Gates

Numbers are targets, revised against reality, never moved to make a gate pass.

| Gate | Passes when | Unlocks |
|---|---|---|
| **G0 memory** | On the shipped cask build, 12–24 h unattended soak: mean physical footprint after hour 1 < 150 MB, linear-fit slope ≤ 2 MB/h, (max − min) after hour 1 ≤ 25 MB, idle CPU ≈ 0 %. Results are published in `benchmarks/`. Until then the README makes no "lightweight" or "low memory" claims. | Launch posts (Phase 3) |
| **G1 signal** | ≥ 300 stars **or** ≥ 100 waitlist signals **or** ≥ 20 issues from people who are not the maintainer, about 8 weeks after launch. A waitlist signal is one unique human who commented or 👍-reacted on the pinned waitlist thread (that thread only; the maintainer excluded; counted by `scripts/adoption-snapshot.sh`). No email form, no third-party service. | iPad app (Phase 5) |
| **G2 demand** | ≥ 100 paid iPad unlocks **or** ≥ 50 % of iPad reviews and waitlist comments asking for alerts. | Cloud (Phase 6) |

## Phase 0 — Rename to PR Marmot — done

The project was `prboard` through v0.5.3. **PR Marmot** (two words) everywhere a human reads it,
**`prmarmot`** everywhere a machine reads it. Config and state migrate once from the old directory;
historical release assets keep their old names.

- [x] Cargo packages, binary, bundle id `dev.oliverkriska.prmarmot`, config dir, `PRMARMOT_*` env, cask token, docs.
- [x] Domains `prmarmot.dev` and `prmarmot.com`.
- [ ] Logo: a marmot standing sentinel on a rock, whistle implied; must read at 16 px and 512 px.

## Phase 1 — Distribution that installs in one line

- [x] **1a. Notarized `.app`**: Developer ID signing, hardened runtime, `notarytool`, stapled, verified with `spctl`.
- [x] **1b. Homebrew tap + cask**: `brew install --cask oliver-kriska/tap/prmarmot`; the release workflow updates the cask.
- [ ] **1c. In-app update that runs Homebrew**: daily check via the existing transport; "Update" banner; on the
      Homebrew channel the app quits before the binary is replaced (a detached helper runs `brew upgrade` and
      relaunches); failure path reopens the old app and shows the error; "Check for updates automatically" preference.
- [ ] **1d. Website `prmarmot.dev`**: static; hero, one-line install, "No account. No server. No GitHub App to
      approve.", what the Note does, comparison with github.com/pulls, changelog, privacy; "Join the waitlist"
      links to the pinned Discussion; privacy-respecting analytics only.
- [ ] **Linux `.deb`** (arm64 + x86_64) built in CI with declared runtime deps (Vulkan loader, fontconfig,
      libxkbcommon, Wayland/X11); `install.sh` Linux branch. Homebrew casks are macOS-only, so a Linux formula
      (source build) only if asked. NVIDIA + Wayland is the known GPUI trouble spot; documented, not hidden.

## Phase 2 — Growth features — shipped in v0.6 / v0.7

- [x] All-repositories mode (`involves:@me` / `review-requested:@me`, still one request per refresh), Repo column, repo picker as a filter.
- [x] "Changed since you looked": bounded per-PR snapshots, row markers, `changed` filter, "new commits since your review".
- [x] Dock badge = rows that need you.
- [x] Watch a PR → notifications on Note *transitions* ("Ready for you — CI passed and Alice approved"), never on every refresh; quiet on first refresh and while you are looking at the row.
- [x] Snooze / follow-up with automatic return when the condition is met.
- [ ] Onboarding when `gh` is missing or not logged in: detect, explain, one-click copy of `gh auth login`, Retry.

## Phase 3 — Launch

Launch posts go out only after the memory benchmark is published (G0) and the website is live.

- [ ] **Memory benchmark** in `benchmarks/`: method, raw samples and the verdict, reproducible by anyone.
- [x] Repo hygiene: topics, social preview, homepage, Discussions with the pinned waitlist thread and the Ideas category.
- [ ] Issue templates that route bugs to Issues and requests to Ideas.
- [ ] README: hero GIF, install one-liner, the "no account, no server" line, the CLI + agent-skill section, and a
      comparison table (github.com/pulls · gh-dash · RepoBar · Gitify · PR Flow) with an honest platform row and an
      "AI review" row.
- [ ] Posts: Show HN, r/rust, r/github, Lobsters; awesome-rust, awesome-github, awesome-claude-code.
- [x] Adoption tracking without telemetry: `scripts/adoption-snapshot.sh` appends stars, downloads, traffic,
      stranger issues, waitlist signals and Ideas upvotes to `measurements/adoption.csv`.
- [ ] Tag `v1.0.0` when Phases 1–2 and the benchmark are stable.

## Next features — ranked by evidence (2026-09-16)

Ranked from what people ask for in the trackers of neighbouring tools, what GitHub shipped in 2026,
and what developers complain about publicly. The frame: GitHub solved "a filtered list of my PRs"
(github.com/pulls, GA 2026-07-09), so PR Marmot does not compete there. GitHub has **not** built a
per-viewer "what changed since you reviewed" (only an unread dot), any whose-move-is-it model, a PR
size band, an agent-vs-human split, or a notification policy. Those are the plan. Upvotes in Ideas
reorder this list.

| # | Feature | Where | Effort | Why |
|---|---|---|---|---|
| 1 | **Agent-authored PR lane** — detect bot/agent authors (`Bot` accounts + configurable patterns), own collapsed group, `--agent/--no-agent` CLI filter, "no human has looked yet" | core → app + CLI | S–M | The most-upvoted request in GitHub's community forum (1,834 👍); five named agents opened over 159,000 public PRs in August 2026; GitHub only folds agent PRs *into* `author:` with no exclude filter |
| 2 | **Re-review delta anchored to your last review** — "3 commits, 4 files, 1 thread reopened since you approved" | core → both | M | GitHub shows only an unread dot; the reviewed commit is already in the query |
| 3 | **Evidence behind the Note** — the failing check's name and run URL, who requested changes, which thread; same words in app, CLI, badge and notification | core → both | M | `gh run watch` log streaming has 76 👍; GitHub's merge-status panel is per-PR and still in preview |
| 4 | **Wait time on the current turn** + `stale` filter + sort by wait; review queue "waiting 16 h, nobody has looked"; zero reviewers vs team-requested-none-responded | core → both | S / M | GitHub's reminders are Slack-only and org-scoped; industry data puts first pickup of agent PRs above 16 hours |
| 5 | **Deterministic size band** Quick / Medium / Deep dive, "quick wins" sort; lockfile/generated discounts later | core → both | S / M | No AI, never minutes; GitHub has no per-row size signal |
| 6 | **Ownership edge cases** — a conflicted teammate PR waits on its author; an outdated thread comes back to you; your reply clears your turn, the author's reply returns it; a re-run check that passed clears "fix CI" | core rules + golden tests | S each | GitHub has no ownership model at all |
| 7 | **`prmarmot-cli watch --until ci-pass\|approved\|mergeable\|merged --timeout`** with outcome exit codes (0 met / 5 failed terminal / 6 timeout) | CLI | S | `gh pr wait` has 38 👍; agents hand-roll polling loops that burn the 5,000 req/h budget |
| 8 | **Merge-queue state** ("in merge queue, position 2") — today a queued PR reads as "approved, press merge" | core → both | S | One GraphQL field; fixes a wrong Note |
| 9 | **Multi-account** — two `gh` identities, one board, account badge per row | core → both | M | 981 👍 on GitHub Desktop and 354 👍 on the GitHub CLI, both open for years; cheap after the Phase 4 transport |
| 10 | **Notification policy** — digest at times you pick, catch-up on launch, silence when nothing needs you, per-repo mute | app | M | Off by default; reuses the transition engine |
| 11 | **`prmarmot-cli pr OWNER/NAME#N`** compact single-PR detail, same JSON, no patches | CLI | S | Agents keep asking for a compact per-PR summary |
| 12 | **Stack ordering inside the queue** — which layer to open first, "ancestor unmerged" as a blocker | core → app | S | GitHub ships stack *creation*; ordering in a queue is still nobody's |
| 13 | **`prmarmot-cli report --since`** standup Markdown (merged / opened / still blocked) | CLI | S | Nearly free on the existing formatter |
| 14 | Up to **three saved views** (capped) | app | L | Reserve; only if Ideas votes demand it |
| 15 | JSON Schema for the `board@1` output + shell completions | CLI | S | Hygiene, with a CLI release |

**Not on the list, on purpose:** filtered lists and saved-view systems (GitHub does that, free); stack
creation, rebasing, merging (GitHub, 2026-07-30); AI review, summaries or estimates (Copilot approves
PRs since 2026-09-01; PR Marmot stays no-AI); write actions such as comment/approve/merge (never the
top ask in any dashboard tracker; revisit only if Ideas votes overwhelm); an MCP server (independent
2026 evals show a CLI is cheaper and faster for agents at equal correctness); Slack/Jira integrations
and team dashboards; auto-closing stale PRs.

## Phase 4 — Core becomes app-ready (parallel with post-launch measuring)

Needed by every rung above 1; also improves rung 1 (onboarding without `gh`, Enterprise hosts).
`prmarmot-core` stays the single source of categorization and the Note; golden tests stay the contract.

- [ ] Direct HTTP `GithubTransport`: same GraphQL document, same bounded paging and rate-limit parsing, host configurable (github.com + GitHub Enterprise).
- [x] OAuth decided: **Device Flow, no token-exchange server, ever.** Tokens in the OS keychain, never in config files.
- [ ] Desktop transport picker: `gh` if present (zero-config), else Device Flow onboarding.
- [ ] UniFFI bindings for `prmarmot-core`; CI builds an XCFramework published as a Swift package from a tag.
- [ ] Contract tests: the Swift package runs the same golden fixtures through the bindings.

## Phase 5 — PR Marmot for iPad (and iPhone) — after G1

A foreground triage companion: sidebar (My PRs / Review queue / Watched / Snoozed), detail pane
(Note, checks, reviews, threads, Open in GitHub, Watch, Snooze, Share). No alert promises until Phase 6.

- [ ] SwiftUI app (iPadOS + iOS, Universal Purchase) over `PRMarmotCore`, URLSession transport, Device Flow, Keychain, same refresh floor and rate-limit rules.
- [ ] Parity with desktop: categories, stacks, Note, search, change markers, watches, snooze; local state only, no sync in v1.
- [ ] Platform wins: Home/Lock Screen widget ("N need you"), Shortcuts ("what needs me"), keyboard shortcuts on iPad, Handoff to GitHub.
- [ ] Best-effort background refresh with local notifications, labelled best-effort in the UI and never in a screenshot.
- [ ] Free download, one-time unlock; the free tier is one repo, read-only.
- [ ] TestFlight invitations go to the waitlist thread first; App Store privacy label "data not collected".

## Phase 6 — PR Marmot Cloud — after G2

The hosted sentinel: computes Note transitions server-side and pushes to iPad/iPhone, optionally Mac.
Opt-in; local stays the default and the free path.

- [ ] Ingest by **polling with the user's own OAuth token**, same board query, adaptive interval. GitHub App
      webhooks only as a later team option (a personal App install cannot reach org repos without admin
      approval, and merge conflicts have no webhook event anyway).
- [ ] Privacy commitments up front: encrypted token storage, per-user isolation, in-app revoke, delete-my-data
      endpoint, minimal scopes.
- [ ] Push: the same semantic Note transitions as the desktop; notification actions (Open, Snooze); digest option.
- [ ] Billing through the App Store subscription; web billing only if Mac-only customers ask.
- [ ] Desktop opt-in: the Mac app can receive Cloud pushes instead of polling locally (still a free app).

## Phase 7 — optional: Mac App Store copy

Only if a sandboxed GPUI build proves cheap. The Homebrew/GitHub build stays free; the App Store copy
is paid for convenience. Requires the Phase 4 transport. Not a priority.

## Maybe / small

- [ ] GitHub Enterprise hosts on desktop (`GH_HOST` passthrough) — only with a tester.
- [ ] Launch at login.
- [ ] Menu-bar / tray item — verify first that it fits GPUI's run loop; Linux is harder.
- [ ] Always-on-top compact board.

## Metrics tracked weekly (no telemetry)

`measurements/adoption.csv`: stars · release downloads · website install-page views · waitlist
signals · Ideas upvotes · issues from strangers · (Phase 5+) App Store units and ratings ·
(Phase 6+) subscribers and hosting cost per subscriber.

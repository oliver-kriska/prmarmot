# PR Marmot features

This is the complete list of what the PR Marmot desktop app and `prmarmot-cli` do. The README summarises it.
Each id (`F-board-3`) is stable and never reused, so other documents (the site, the iPad app's parity list) can point at one.

Each entry gives the version it first shipped in. `unreleased (0.9.1)` marks work that is on `main` but not yet
released. An entry that names no platform works on both macOS and Linux. Limits are the values in the code. New
features get the next free number in their area.

## Board and queues

- **F-board-1** Two queues, switched from the title bar, with `1` / `2`, or with `v`: **Involving me** (**My PRs**
  when one repository is selected) and **Review queue**. _since v0.1.0_
- **F-board-2** **Involving me** is the zero-config default: open PRs involving your GitHub login across all
  repositories. Your own PRs get actionable Notes, and other people's get status Notes ("alice's PR · approved").
  It never widens to PRs that don't involve you. _since v0.6.0_
- **F-board-3** **My PRs** in one repository lists the PRs you opened there. _since v0.1.0_
- **F-board-4** **Review queue** splits PRs that request your review (**Requested from you**) from other people's
  PRs with no pending user or team reviewer request (**Available to review**), an optional pool rather than an
  assignment. Your own PRs never appear, and it goes by review requests, not issue assignees. _since v0.1.0;
  Available to review since v0.3.0_
- **F-board-5** Sections always come in the same order, and empty ones are left out.
  - My PRs: Approved, Needs action, Awaiting review, Drafts.
  - Involving me: Approved, Needs attention, In progress, Drafts.
  - Review queue: Requested from you, Available to review, Reviewed, Drafts.

  Snoozed PRs follow in their own group. Within Needs action, approved PRs come first. _since v0.2.0; Approved since
  v0.5.1_
- **F-board-6** GitHub's native stacks are grouped inside each section and ordered by layer from the base up.
  - Each row carries a layer marker (`├─ 2/3`, `└─ 3/3`).
  - The stack header reads "3 layers", or "2 of 3 layers shown" when part of the stack is elsewhere.

  Grouping uses GitHub's stack data, never labels or guessed branch relationships. _since v0.3.0_
- **F-board-7** Columns fit the window in three width classes.
  - Below 1,120 px wide the Labels column is hidden; from 1,360 px it gets wider.
  - Note always gets at least as much room as Title.
  - A width you drag is kept for that queue and width class until you quit.
  - Columns can't be sorted or reordered.

  _since v0.2.0_
- **F-board-8** Ways to open and select a PR:
  - `↑` / `↓` selects a row.
  - `Enter`, `o`, or a double-click opens the PR in the browser.
  - A single click on the blue PR number opens the PR, and on a linked-issue id opens the issue.

  _since v0.1.0; single-click links since v0.2.0_
- **F-board-9** Right-click a row for **Open on GitHub**, **Show details**, copy actions (URL, number,
  `owner/name#N` reference, title, all details), **Watch** / **Unwatch**, **Snooze…**, and **Cancel snooze**. The
  binoculars icon in the PR cell toggles watch. _since v0.3.0; watch and snooze since v0.6.0_
- **F-board-10** **Details** (`Space` or the Details button) shows the selected PR's loaded data:
  - labels, Note, author, CI, and size;
  - reviewers and reviews (your own review in the Review queue);
  - when the wait for a reviewer started;
  - stack layer and base branch;
  - whether it is changed, watched, or snoozed.

  It makes no extra request, and `Esc` closes it. _since v0.3.0_
- **F-board-11** Switching queues keeps each queue's rows, selection, and scroll position, and refreshes it in the
  background. _since v0.2.0_
- **F-board-12** The header counts:
  - PRs this view has loaded.
  - How many of them need you: Needs action (Needs attention in Involving me) in My PRs, and Requested from you
    plus Available to review in the Review queue. Snoozed PRs never count.
  - How many watched or snoozed PRs were refreshed.

  Hover it for the explanation. While search or **Changed** filters rows, the toolbar shows "N of M loaded".
  _since v0.8.1_
- **F-board-13** Draft rows are dimmed. Statuses show as coloured dots with text, never emoji; the one exception is
  🐛 on a `bug` label. _since v0.1.0_

## Categorization and notes

- **F-note-1** Every PR is sorted into a category from its CI, reviews, review requests, unresolved threads,
  conflicts, and draft state: needs action, awaiting review, or draft for your PRs; requested, available, reviewed,
  or draft in the Review queue. The rules are pinned field for field against the original shell prototype by
  golden tests. _since v0.1.0_
- **F-note-2** Every row has a plain-language **Note** that leads with the exception: a merge conflict, then
  failing CI, then requested changes, then unresolved threads, then "no reviewers". Only a conflict, failing CI, or
  requested changes turns it red; routine steps stay muted. Hover the Note for the full text. _since v0.2.0_
- **F-note-3** In the Review queue, a Note starts with "new commits since your review" when the PR has moved past
  the commit you reviewed. _since v0.6.0_
- **F-note-4** When one of your non-draft PRs has no reviewer requested and no qualifying review, its Note
  suggests reviewers ("assign alice + bob"). The list comes from `[repo_reviewers]` for that repository, then for
  its owner, then from `default_reviewers`; an empty list suggests nobody. PR Marmot never assigns them. _since
  v0.1.0; per repository and owner since v0.7.0_
- **F-note-5** **Pickup age:** the Note ends with how long the PR has waited for a reviewer (`· 16h`, `· 3d`).
  It turns amber at `stale_after_days`, which defaults to 3.
  - A PR requested from you counts from your latest request, or your team's.
  - Your own PR counts from its oldest pending request.
  - An available PR, or yours with nobody requested, counts from when it opened.
  - A wait never starts before the last "ready for review".
  - Drafts and reviewed PRs show no age; in the Review queue, only your own review counts.
  - The age reads each PR's latest 10 review-request and ready-for-review events; a request older than those
    counts from when the PR opened.

  _since v0.8.0_
- **F-note-6** When only a team was asked and nobody has reviewed, the Note says "team requested, nobody
  responded". _since v0.8.0_
- **F-note-7** In the Review queue, the Note ends with a **size band**. **Small** is at most 100 changed lines
  and at most 10 files, **Large** is more than 400 lines or more than 30 files, and **Medium** is everything
  between. Changed lines are additions plus deletions as GitHub counts them. A PR GitHub gave no counts for shows no
  band. The band is never turned into a time estimate. _since v0.8.0_
- **F-note-8** Requested from you, Available to review, and Awaiting review list the longest wait first. In the
  Review queue, **Smallest first** sorts Requested from you and Available to review by size band, then changed lines,
  then wait, with unsized PRs last. It lasts until you quit. _since v0.8.0_
- **F-note-9** The Review column shows each reviewer's standing review. A later comment doesn't cancel an approval
  or a change request, but a later change request or a dismissal does. _since v0.8.0_
- **F-note-10** Reviews from the `github-actions` and `chatgpt-codex-connector` bots never count. _since v0.1.0_
- **F-note-11** A merge conflict stays marked while GitHub briefly reports mergeability as unknown after a push to
  the base branch, so the marker doesn't flicker between refreshes. _since v0.7.0_
- **F-note-12** Optional issue links: `[issue_link]` gives a regular expression matched in PR titles and a URL
  template with `{id}`. No tracker or prefix is built in. When links are set up, a leading `[ID]` is dropped from the
  title, as is a leading `WIP`. _since v0.1.0_
- **F-note-13** Labels show as chips, up to 20 per PR, with a `bug` label always first. **+n** opens a menu of the
  labels that didn't fit. _since v0.1.0_

## Search and filters

- **F-search-1** Search the loaded PRs with **Search**, `/`, `⌘F` / `Ctrl+F`, or **Edit → Find**.
  - Words match the PR number (`#123` too), repository, title, author, labels, linked issue, and Note.
  - Every word must match, quotes keep a phrase whole, and matching ignores case.
  - A count shows how many loaded PRs match. Search never makes a request.

  _since v0.3.0; `⌘F` and Edit → Find since v0.8.0_
- **F-search-2** `label:NAME`, `author:LOGIN`, and `repo:OWNER/NAME` keep PRs whose label, author, or repository is
  exactly that, ignoring case. Quote a value with spaces (`label:"help wanted"`). A term becomes a chip after a
  space or `Enter`; the box holds up to 8 chips, and all of them must match. _since v0.8.0_
- **F-search-3** `is:stale` keeps PRs that have waited `stale_after_days` or longer for a reviewer. `stale` is the
  only `is:` value. _since v0.8.0_
- **F-search-4** Clicking a label, author, or repository in the table or in Details adds it as a chip. _since
  v0.8.0_
- **F-search-5** A chip's × removes it, `Backspace` in an empty box removes the last chip, the × at the right
  clears the search, and an empty search closes when you leave it. _since v0.8.0_
- **F-search-6** The desktop search box and `prmarmot-cli --filter` share one grammar, pinned by a golden test, so
  a saved query means the same in both. _since v0.9.0_

## Change tracking

- **F-track-1** A blue marker flags a PR that changed since you last looked. It survives restarts until you select
  the PR. Hovering it says what changed:
  - new commits, or draft and ready-for-review changes;
  - CI, merge conflicts, and the review decision;
  - reviews and review requests, and unresolved threads;
  - "Updated on GitHub (a comment, edit, or label)", or "changed, then changed back".

  PR Marmot remembers the last state of the 1,000 most recently seen PRs. _since v0.6.0_
- **F-track-2** **Changed** filters the loaded rows to changed PRs and shows how many match the search. A
  selection restored at launch doesn't clear a marker. _since v0.6.0; the count since v0.8.0_
- **F-track-3** **Watch** (`w`) follows a PR even outside the current view. Up to 50 watches are kept; a 51st drops
  the oldest, and the footer says which. Watched and snoozed PRs refresh in the same request as the board, at most
  50 per refresh, taking turns when there are more. _since v0.6.0_
- **F-track-4** **Snooze** (`s`) moves a PR to a collapsed **Snoozed** group, where it adds no alerts or counts
  until it wakes. **Snoozed** shows or collapses the group.
  - **One hour**.
  - **Until tomorrow**, which is 24 hours from now.
  - **Waiting for CI** wakes when checks change to passing or failing.
  - **Review again when changed** wakes on a new commit, review, review request, review decision, unresolved-thread
    count, or draft change.
  - **Waiting on** the PR's author wakes when the author submits a newer review.

  A conditional snooze wakes only when PR Marmot sees the PR, and missing data never wakes one. Snoozing again
  replaces a snooze, and **Cancel snooze** ends it. Up to 200 snoozes are kept; a 201st drops the oldest. _since
  v0.6.0; the Snoozed count since v0.8.0_
- **F-track-5** Desktop notifications for these changes:
  - merge conflict, changes requested, review again after new commits;
  - CI passed (with "and is approved" when it is);
  - pull request changed;
  - a watched PR merged, closed, or became unavailable.

  By default only watched PRs notify. **Notify for every PR entering Needs action** adds your own PRs. Snoozed PRs,
  the row you have selected, and the first refresh after launch never notify. Clicking a notification selects the
  PR, or opens a watched PR that isn't loaded on GitHub. Notifications and their sound can be turned off, and they
  arrive only while the app runs. _since v0.6.0_
- **F-track-6** The Dock badge counts, across both views, your PRs that need action plus review requests. It can
  be turned off. _since v0.6.0 · macOS only_
- **F-track-7** Watches, snoozes, and change history are stored apart from preferences under
  `$XDG_STATE_HOME/prmarmot` (or `~/.local/state/prmarmot`), separately for each GitHub host and account. Writes are
  bounded, batched, and atomic. A corrupt file, or one written by a newer version, is kept and reported rather than
  overwritten. _since v0.6.0_

## Sharing

- **F-share-1** Copy a whole section from its header (hover and click **Copy**, or right-click). The copy respects
  the active search. _since v0.6.0_
  - **Copy list for Slack & docs** pastes titled links into Slack, Teams, Google Docs, email, Notion, Linear, and
    Jira.
  - **Copy Markdown for GitHub** is for GitHub, Discord, and editors.
  - **Copy as table** is for docs and slides.
  - **Copy URLs** gives one link per line.
- **F-share-2** `Y` copies the selected PR's section as a list. _since v0.6.0_
- **F-share-3** `y` copies the selected PR's URL, and the footer confirms it. _since v0.1.0_
- **F-share-4** In Details you can select text and copy it with `⌘C` / `Ctrl+C`. Its **Copy** menu copies the PR
  URL, number, `owner/name#N` reference, title, or all details. _since v0.5.3_

## Repositories and scope

- **F-repo-1** **All repositories** is the default scope and always first in the repository picker. _since v0.6.0_
- **F-repo-2** Choosing one repository makes a fresh request scoped to it. It never filters the partial
  all-repositories page and passes that off as complete. _since v0.1.0_
- **F-repo-3** The picker is searchable and fills from your accessible repositories. It lists up to 1,000
  repositories, read in 10 pages of 100, plus configured, pinned, and current ones. If discovery fails, those three
  still show, and **Repos** retries. Organization SSO and GitHub permissions decide what's visible. _since v0.3.0_
- **F-repo-4** **Pin** adds a repository shortcut to the toolbar, and **Pinned** removes it. Up to 12 pins are kept
  in order. Pins never fetch in the background. _since v0.4.0_
- **F-repo-5** A repository that doesn't exist, or that the account can't see, gets its own error instead of an
  empty list. _since v0.7.0_
- **F-repo-6** The starting scope comes from `--repo owner/name` or `--all-repos`, then `PRMARMOT_REPO` /
  `PRMARMOT_SCOPE`, then the config file, then the current checkout's GitHub remote. _since v0.1.0_

## Sign-in and hosts

- **F-auth-1** PR Marmot uses your GitHub CLI (`gh`) login when there is one. _since v0.1.0_ In `auto`, the
  default, a token you chose comes first (`PRMARMOT_TOKEN`, then a pasted personal access token), then the `gh`
  login, then a device-flow sign-in, then the sign-in screen. The GitHub CLI is exempt from organizations'
  OAuth-app restrictions, and PR Marmot's own app isn't. _unreleased (0.9.1)_
- **F-auth-2** **Sign in with GitHub** uses GitHub's device flow with PR Marmot's own registration, so there is
  nothing to set up. You type an eight-character code at github.com/login/device. It asks for `repo` and
  `read:org`, and there is no client secret and no server of ours. _since v0.9.0_
- **F-auth-3** **Use a token** takes a personal access token. A fine-grained one needs Pull requests: read and
  Metadata: read and covers one owner. A classic one with `repo` and `read:org` covers several organizations and
  shows CI. The token is never displayed. _since v0.9.0_
- **F-auth-4** A fine-grained token still loads the board. CI it may not read shows as "hidden", and one line under
  the header explains why. _unreleased (0.9.1)_
- **F-auth-5** GitHub Enterprise Server: set `[auth] host`, `PRMARMOT_HOST`, or `GH_HOST`. Sign in with a token, or
  with the device flow once `[auth] client_id` names that instance's own registration. _since v0.9.0_
- **F-auth-6** A stored token lives in the login keychain on macOS (service `dev.prmarmot.auth`) and in
  `$XDG_STATE_HOME/prmarmot/auth.json` with mode `0600` on Linux. `[auth] store = "file"` uses the file on macOS
  too. _since v0.9.0_
- **F-auth-7** Settings shows the host and the signed-in account. **Disconnect** removes a stored token from this
  machine; revoking the grant at GitHub is a separate step. _since v0.9.0_
- **F-auth-8** Settings and `prmarmot-cli auth status` name the sign-in in use, including `PRMARMOT_TOKEN`, and
  point out a stored token that goes unused while the GitHub CLI is signed in. _unreleased (0.9.1)_
- **F-auth-9** `[auth] mode` or `PRMARMOT_AUTH` picks `auto`, `gh`, `device`, or `token`. `PRMARMOT_TOKEN` supplies a
  token for one run and is never stored. It comes before a GitHub CLI login, so it suits CI. _since v0.9.0_

## Refresh, limits and rate budget

- **F-refresh-1** Refresh runs automatically every 5 minutes by default, and `r` refreshes now. The interval can't
  go below 30 seconds. _since v0.1.0_
- **F-refresh-2** The header shows "synced Xm ago" and the live GraphQL rate-limit budget, updated once a minute.
  _since v0.1.0_
- **F-refresh-3** Each refresh is one GraphQL request.
  - My PRs and Involving me return up to 60 PRs and cost about 4 points.
  - The Review queue returns up to 60 requested and 60 candidate PRs and costs about 8 points.
  - Nothing is fetched separately for each PR.

  _since v0.1.0_
- **F-refresh-4** When GitHub says there is more, a **partial results** notice appears. **Load more** fetches the
  next page, up to five pages for each search: 300 authored results or 600 review candidates. A refresh returns to
  page one, and a failed page keeps what's already loaded. _since v0.3.0_
- **F-refresh-5** Refreshes pause while fewer than 50 points remain, leaving the rest of the hourly budget to your
  own tools, and the header says so. A rate-limit wait is always between 60 seconds and 15 minutes, never until a
  far-off reset. _since v0.1.0_
- **F-refresh-6** Each PR's data is capped: 20 labels, 15 review requests, 60 reviews, 100 review threads (so the
  unresolved count stops at 100), and the latest commit's CI rollup. _since v0.1.0_
- **F-refresh-7** If the first load fails, **Retry** tries again. A watched PR you can no longer see is reported as
  unavailable, and the refresh still succeeds. _since v0.5.0; unavailable PRs since v0.7.0_
- **F-refresh-8** An idle window repaints at most once a minute, with no animated spinner, so the GPU stays idle.
  _since v0.9.0_

## Settings, theme, shortcuts

- **F-settings-1** **Settings** edits these and applies them without a restart:
  - reviewer suggestions, the refresh interval, and the theme;
  - notifications and their sound;
  - the Dock badge and automatic update checks.

  **Advanced** holds issue links, whether every PR entering Needs action notifies, and the config file's path.
  **Save** checks each field and puts any error beside it, and **Cancel** discards your edits. Reviewer names must
  be valid GitHub logins, and an issue URL must be `http(s)` and contain `{id}`. A field set by an environment
  variable is disabled and names that variable. Settings also shows the version. _since v0.5.0; notification,
  badge and update switches since v0.6.0_
- **F-settings-2** Every setting lives in an optional TOML file at `$XDG_CONFIG_HOME/prmarmot/config.toml` or
  `~/.config/prmarmot/config.toml`. The app writes changes back and keeps your comments. A file it can't parse is
  never overwritten, and the app starts with defaults. _since v0.1.0_
- **F-settings-3** Settings are applied in this order: command-line options, then environment variables
  (`PRMARMOT_REPO`, `PRMARMOT_SCOPE`, `PRMARMOT_REFRESH_SECS`, `PRMARMOT_THEME`, `PRMARMOT_DEFAULT_REVIEWERS`,
  `PRMARMOT_ISSUE_PATTERN` with `PRMARMOT_ISSUE_URL_TEMPLATE`), then the file. _since v0.1.0_
- **F-settings-4** The theme is system, light, or dark, and `t` cycles through them. System follows the OS
  appearance live. _since v0.1.0_
- **F-settings-5** The window opens at 1440 × 860 and can't be made smaller than 900 × 560. Across launches the app
  remembers the scope, the last repository, the theme, the starting view, the window size, and pins. _since v0.1.0_
- **F-settings-6** Keyboard shortcuts work from any dashboard control, but never while you type in search, the
  repository picker, or Settings. The footer opens the full list. _since v0.1.0; the list since v0.5.0_
- **F-settings-7** Quit from the app menu or with `⌘Q` / `Ctrl+Q`, which works everywhere, even in text fields.
  Plain `q` quits when no text field or dialog is active. _since v0.5.0_

## Updates and install

- **F-install-1** Prebuilt releases are signed with a Developer ID and notarized. The installer checks the
  published checksum, the signature, the notarization ticket, and Gatekeeper acceptance before installing. _since
  v0.6.0 · macOS on Apple silicon_
- **F-install-2** Homebrew: `brew install --cask oliver-kriska/tap/prmarmot`. The cask also links `prmarmot-cli` and
  its shell completions. _since v0.6.0; the CLI since v0.7.0, completions since v0.8.0 · macOS_
- **F-install-3** Every install path uses one `/Applications/prmarmot.app`, and the installer removes an older
  `~/Applications` copy. _since v0.7.1 · macOS_
- **F-install-4** `install.sh` has these options:
  - `--repo` sets the first repository;
  - `--dir` and `--bin-dir` choose where the app and the CLI go;
  - `--from-source` builds on your machine.

  It writes a config file only when it can resolve a repository you can reach, and never overwrites an existing
  one. _since v0.1.0_
- **F-install-5** The GitHub CLI is optional: the cask doesn't depend on it, and without it the installer tells you
  the app will ask you to sign in. _unreleased (0.9.1)_
- **F-install-6** On launch, and at most once a day after that, the app checks for the latest stable release and
  shows a banner when there is one.
  - For a direct install, the banner opens the release page.
  - For a Homebrew install, a helper process upgrades the cask after the app quits, reopens it, and reports any
    failure on the next launch.

  The check can be turned off. _since v0.6.0_ It asks through the GitHub CLI when that can answer, and otherwise
  asks GitHub directly without a token. _unreleased (0.9.1)_
- **F-install-7** The first PR Marmot launch copies the former `prboard` app's config and state once and leaves the
  originals alone. If the copy fails, the app stops with an error instead of starting empty. _since v0.6.0_
- **F-install-8** Linux and Intel Macs build from source, and `install.sh --from-source` puts the binaries in
  `~/.local/bin`. There are no prebuilt Linux packages yet. _since v0.1.0_

## Terminal and agent CLI

- **F-cli-1** `prmarmot-cli mine` and `prmarmot-cli review` print the app's My PRs and Review queue. They use the
  same query, categories, Notes, sections, stacks, config file, and sign-in. The CLI reads the app's watches and
  snoozes but never changes them, and never clears a changed marker. _since v0.7.0_
- **F-cli-2** It is bundled inside the app and linked by every installer. It has no GPUI dependency, so it builds
  without Metal (`cargo install --locked --git https://github.com/oliver-kriska/prmarmot prmarmot-cli`), with Rust
  1.85 or newer. _since v0.7.0_
- **F-cli-3** Scope and view options:
  - `--repo owner/name` or `--all-repos` choose the scope, and `--authored` keeps only PRs you opened.
  - `--changed` keeps changed PRs and says what changed, and `--watched` keeps watched PRs.
  - `--snoozed` expands the Snoozed group.
  - `--pages N` loads up to five pages.

  _since v0.7.0_
- **F-cli-4** `--stale` keeps PRs that have waited too long. `--sort smallest` or `--sort wait` orders the pickup
  sections, and Notes end with the wait and the size band. _since v0.8.0_
- **F-cli-5** `--filter "<query>"` runs the desktop search grammar over the loaded PRs. _since v0.9.0_
- **F-cli-6** Output formats: a width-aware table on a terminal, Markdown when piped, `--format markdown`, and
  `--json`. The table follows `COLUMNS`, and `--no-color`, `NO_COLOR`, or `TERM=dumb` turn colour off. _since v0.7.0_
- **F-cli-7** `--json` follows the versioned `prmarmot-cli/board@1` contract.
  - Sections have stable keys.
  - Each PR carries its categorization facts, pickup age, size, and attention state.
  - Within `@1`, fields are only added. A field's values can grow: `ci: "hidden"` arrives in 0.9.1.

  _since v0.7.0_
- **F-cli-8** JSON Schemas (draft 2020-12) for the board and for watch events live in `cli/schema/`. The CLI's tests
  check real output against them. _since v0.8.0_
- **F-cli-9** `prmarmot-cli watch [mine|review]` streams changes, one line each: text on a terminal, NDJSON
  (`prmarmot-cli/event@1`) when piped.
  - It polls at `refresh_secs`, one request per poll for the view's first page. `--interval` overrides it but never
    goes below 30 seconds.
  - Events: `ready`, `changed`, `added`, `removed`, `rate_limited`, and `error`.
  - A removed PR costs one small extra request to learn whether it merged or closed, for up to 50 removals per
    poll; the rest are reported as `unknown`.
  - `--events N` exits after N events.

  _since v0.7.0_
- **F-cli-10** `watch --pr owner/name#N`, or the PR's URL, follows one pull request in any repository. The stream
  ends when it merges, closes, or becomes inaccessible. _since v0.7.0_
- **F-cli-11** `--until` turns a `--pr` follow into a wait whose exit code is the answer. Several conditions can be
  given, and a merge always counts as met. `--timeout` (`90s`, `30m`, `1h30m`) gives up with exit 6.
  - `ci-pass`, `approved`, `mergeable`, and `merged`.
  - Exit 0 when a condition is met, 5 when none can hold any more.

  _since v0.8.0_
- **F-cli-12** Exit codes:
  - 0 ok
  - 1 GitHub, network, or file error
  - 2 usage error
  - 3 not signed in
  - 4 rate limited
  - 5 `--until` unmet
  - 6 `--timeout` reached

  Errors go to stderr, and stdout carries only the view or the events. _since v0.7.0; 5 and 6 since v0.8.0_
- **F-cli-13** `prmarmot-cli auth login`, `status`, and `logout` handle signing in from the terminal. `login` can
  use the device flow, `--with-token` on stdin, `--host` for Enterprise, or `--client-id`. _since v0.9.0_
- **F-cli-14** `prmarmot-cli skill` prints an embedded skill for coding agents.
  - `skill install` installs it for Claude Code; `--agent agents` targets `~/.agents/skills` (Codex, Copilot,
    Cursor, Gemini CLI, OpenCode, Amp), and `--agent all` does both.
  - It won't overwrite an edited copy without `--force`.

  _since v0.7.0_
- **F-cli-15** `prmarmot-cli completions bash|zsh|fish` prints a completion script for commands, flags, and flag
  values. The cask installs all three. `install.sh` and `make install` link the bash or fish script for your login
  shell and print the zsh line. _since v0.8.0_

## Platform notes

- **F-platform-1** Prebuilt, signed releases are for Apple-silicon Macs. Intel Macs and Linux build from source.
  There is no Windows build. _since v0.6.0_
- **F-platform-2** Linux needs a working Vulkan stack plus fontconfig, xkbcommon, and X11 or Wayland libraries.
  _since v0.1.0 · Linux_
- **F-platform-3** Shortcuts use `⌘` on macOS and `Ctrl` on Linux. _since v0.1.0_
- **F-platform-4** On macOS, the section list and table copy as rich text with a plain-text fallback. On Linux they
  copy as plain text. _since v0.6.0_
- **F-platform-5** On macOS, the system permission prompt appears only after you click **Enable notifications…** in
  the footer and confirm in its dialog. Until permission is granted, nothing is sent and nothing prompts. On Linux
  notifications go to the desktop notification service directly, with no permission step. _since v0.6.0_
- **F-platform-6** The Dock badge and the Homebrew upgrade helper exist only on macOS. On Linux an available update
  opens the release page. _since v0.6.0_
- **F-platform-7** A stored token uses the login keychain on macOS and a `0600` file on Linux (F-auth-6).
  _since v0.9.0_

## Not in the app, by design

- No AI and no summaries.
- Read-only: PR Marmot opens PRs and issues in the browser and copies links. It never merges, assigns reviewers,
  comments, submits or dismisses reviews, or changes anything on GitHub.
- No Windows build.
- No PR Marmot account and no server between you and GitHub, and the app sends no telemetry.
- No custom key bindings and no custom shell actions.

## Planned

What comes next, and in what order, is in [ROADMAP.md](ROADMAP.md), under "Next features". It isn't repeated here,
so the two can't disagree.

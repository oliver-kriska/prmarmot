<img src="assets/branding/icon-256.png" alt="PR Marmot logo" width="96" height="96">

# PR Marmot

**A native desktop dashboard for deciding what to do next on GitHub pull
requests.** PR Marmot turns CI, review requests, completed reviews, unresolved
threads, conflicts, labels, linked issues, and GitHub stacks into two focused
queues with a plain-language **Note** on every row.

- **Involving me** is the zero-config, all-repositories default. Your own PRs
  keep actionable Notes; other authors' PRs use ownership-aware status Notes.
  Selecting one repository changes this queue to **My PRs** with the existing
  authored behavior.
- **Review queue** separates PRs that request your review from PRs with no
  reviewer requested that are available for someone to pick up.
- **Read-only by design:** PR Marmot can open a PR or linked issue and copy its
  URL, but it never assigns reviewers, comments, merges, or changes GitHub.
- **Uses your existing GitHub CLI login:** API access goes through an
  authenticated [`gh`](https://cli.github.com) installation.

![My PRs view showing authored pull requests grouped by next action](assets/screenshots/my-prs.png)

*My PRs. All repositories, people, pull requests, labels, and statuses shown in
the screenshot are fictional.*

![Review queue showing requested and available reviews](assets/screenshots/review-queue.png)

*Review queue. The screenshot uses entirely fictional data. “Requested” means
your user was explicitly requested. “Available to review” means the PR has no
pending user or team review request; it is an optional pool, not an assignment.*

Native GitHub stacks are grouped and ordered by layer, so dependent PRs can be
reviewed from the base upward. A label such as `backend`, `security`, or
`frontend` supplies context at scan speed. Together, stack order and labels make
the available pool useful when choosing work that matches your area without
pretending GitHub assigned it to you.

## Install

### Requirements

PR Marmot checks the GitHub CLI after its window opens. If it is missing or not
authenticated, the setup screen provides installation guidance, a copyable
login command, and Retry. You can also prepare it first:

```sh
gh auth login
```

PR Marmot's prepared release target is **Apple-silicon macOS only**. Until the
first signed PR Marmot release is published, build from source. Linux and Intel
Mac users must build from source for now.

### Apple-silicon macOS

Run the installer from inside a GitHub checkout so it can detect that
repository, or pass the initial repository explicitly:

```sh
# Detect the repository from the current checkout
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh | sh

# Or configure a repository explicitly (recommended for scripted installs)
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh \
  | sh -s -- --repo owner/name
```

Once a signed release exists, the installer downloads the latest macOS arm64 release, installs
`~/Applications/prmarmot.app`, verifies the published checksum, Developer ID
signature, notarization ticket, and Gatekeeper acceptance, and creates the
config file only when it can resolve an accessible repository. It never
overwrites an existing config.

Homebrew will use the same signed artifact after the first cask is published:

```sh
brew install --cask oliver-kriska/tap/prmarmot
```

**Updating? Quit the installed app before running the installer again.**

The former app was named `prboard`. PR Marmot uses new app, binary, config, and
state names. Keep the old installation and data until the first PR Marmot
launch confirms the app's one-time storage migration; see
[`packaging/RELEASING.md`](packaging/RELEASING.md).

### Build from source

Requires Git, latest stable Rust (dependency minimum 1.92, not tested), and the
platform dependencies required by GPUI. macOS additionally needs full Xcode and its Metal Toolchain. Linux
needs a working Vulkan stack plus fontconfig, xkbcommon, and the normal X11 or
Wayland development/runtime libraries.

```sh
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh \
  | sh -s -- --from-source --repo owner/name
```

On macOS this installs `~/Applications/prmarmot.app`; on Linux it installs the
`prmarmot` binary to `~/.local/bin` by default. There are no prebuilt Linux
packages yet.

## First run

1. Launch **PR Marmot** from Spotlight/Finder on macOS, or run `prmarmot` on Linux.
2. If prompted, install/authenticate `gh`, copy `gh auth login`, complete it in
   Terminal, and click **Retry**. Credentials never appear in PR Marmot.
3. The default **All repositories** scope shows open PRs involving your resolved
   GitHub login. Pick a specific repository for its complete scoped result.
4. Switch between **Involving me** (or **My PRs** in one repo) and **Review
   queue** in the title bar.

Click **Settings** in the bottom-right corner to edit reviewer suggestions,
the refresh interval, and the theme directly—no file editing required.
Expand **Advanced** to configure issue links or copy the config-file path.
**Save** validates and applies changes without restarting; **Cancel** discards
your edits. Reviewer and issue-link changes reload the board, subject to the
GitHub rate-limit budget. Theme changes apply immediately on Save. Validation
errors appear beside the relevant fields and focus the first invalid input
(opening and scrolling Advanced when needed); a brief footer message confirms a
successful save.

![Editable Settings with reviewer suggestions, refresh interval, and theme](assets/screenshots/settings.png)

*Settings shown with fictional reviewer names. Environment-controlled fields
are disabled and labeled; saving other settings leaves their file values alone.*

## Configuration

The config file is:

- `$XDG_CONFIG_HOME/prmarmot/config.toml` when `XDG_CONFIG_HOME` is an absolute
  path;
- otherwise `~/.config/prmarmot/config.toml`.

Every setting is optional. With no scope or repository, the app opens all
repositories:

```toml
scope = "all"                            # all | repo; clean default is all
repo = "acme/widgets"                    # remembered specific repository
repos = ["acme/widgets", "acme/api"]    # extra repo-picker entries
pinned_repos = ["acme/widgets"]          # toolbar shortcuts, maximum 12
refresh_secs = 300                       # default 300; hard floor 30
theme = "system"                         # system | light | dark
view = "authored"                        # authored | review
notifications = true                    # transition alerts while app runs
notification_sound = true
notify_all_needs_action = false         # watched PRs still notify
dock_badge = true                       # macOS; no-op on Linux
automatic_update_checks = true          # latest stable release, at most daily

# Static, global suggestions used only in the authored-PR “no reviewers” note.
default_reviewers = ["alice", "bob"]

[issue_link]
pattern = "PROJ-[0-9]+"                  # regex matched in PR titles
url_template = "https://linear.app/acme/issue/{id}"

[window]
width = 1440
height = 860
```

### What `default_reviewers` does—and does not do

`default_reviewers` is a static global hint. When one of **your** non-draft PRs
has no pending reviewer request and no qualifying completed review, its Note
can say, for example, “assign alice + bob.” PR Marmot does **not** assign those people, validate that they are suitable
for the repository, read or implement `CODEOWNERS`, or infer GitHub ownership.
The same list is suggested for every repository. Leave it empty if a global
suggestion would be misleading.

Issue links are optional and tracker-agnostic. `{id}` in `url_template` is
replaced with the first identifier matched by `pattern`; no tracker or project
prefix is built in.

CLI options override environment variables, which override the config file:

- `PRMARMOT_REPO`
- `PRMARMOT_SCOPE` (`all` or `repo`)
- `PRMARMOT_REFRESH_SECS`
- `PRMARMOT_THEME`
- `PRMARMOT_DEFAULT_REVIEWERS` (comma-separated)
- `PRMARMOT_ISSUE_PATTERN` and `PRMARMOT_ISSUE_URL_TEMPLATE` (use together)

Use `--all-repos` or `--repo owner/name` to choose scope explicitly. The app
persists scope and the last specific repository independently, plus theme,
starting view, window size, pinned repos,
and saved Settings back to valid TOML while preserving comments and unrelated
settings. It refuses to overwrite an unparseable config. Manual file edits
require a restart; changes saved in Settings do not.

## Using PR Marmot

- **Shortcuts:** the footer opens the full keyboard reference. Character shortcuts
  work from dashboard controls, but never while typing in search, the repo picker,
  or Settings. Arrow keys, Enter, and Space belong to the focused control.
- **Queues:** click **Involving me** (or **My PRs**) / **Review queue**,
  press `1` / `2`, or press `v` to toggle.
- **Navigate:** `↑` / `↓` selects; `Enter` or `o` opens the PR; `y` copies its
  URL with a brief footer confirmation; double-clicking a row opens it.
- **Inspect:** press `Space` or click **Details**; `Esc` closes the panel.
  Select text in the panel and press `⌘C` (macOS) or `Ctrl+C` (Linux) to copy it.
  The **Copy** menu offers the PR number, title, URL, or all details without selecting text.
- **Changes:** a blue row marker survives restarts until you actually select the
  PR. **Changed** filters the loaded rows; a restored selection does not clear it.
- **Watch:** press `w` on a selected PR. Watches are FIFO-bounded at 50 and are
  refreshed through one batched GraphQL operation, including watched PRs outside
  the active search. Notifications are semantic transitions, suppressed for the
  first successful observation after launch and for the focused selected row.
- **Snooze:** press `s` for one hour, until tomorrow, waiting on a person, waiting
  for terminal CI, or review-again-when-changed. Snoozed rows move to a collapsed
  group and do not contribute attention alerts or counts until they wake.
- **Search loaded rows:** click **Search** or press `/`. Terms match PR number,
  repository, title, author, label, issue, and Note; multiple terms narrow
  together.
- **Refresh:** `r` refreshes immediately. Automatic refresh defaults to five
  minutes and the header shows the last sync time and GitHub API budget. If the
  initial load fails, click **Retry**; rate-limit pauses still wait for their budget.
- **Updates:** by default PR Marmot checks the latest stable GitHub release on
  launch and at most once per day. A header banner opens the verified release
  page for direct installs. Homebrew installs stage a detached helper and quit
  only after it starts; the helper upgrades the `prmarmot` cask after the app
  exits, reopens it, and reports failures on the next launch.
- **Repositories:** **All repositories** is always first in the searchable
  picker. Choosing a repository performs a fresh repository-scoped request—it
  never filters the truncated global page and pretends it is complete. **Pin**
  adds an in-app shortcut and **Pinned** removes it. Up to 12 pins persist in
  order. Pins do not fetch in the background.
- **Theme:** `t` cycles system → light → dark.
- **Quit:** use **PR Marmot → Quit PR Marmot**, `⌘Q` on macOS, or `Ctrl+Q` on Linux.
  The global shortcut works even in Settings and text inputs. Plain `q` also
  quits when no text input or dialog is active.

Mutable attention state is stored separately from preferences under
`$XDG_STATE_HOME/prmarmot` (or `~/.local/state/prmarmot`), namespaced by GitHub host
and account. Writes are bounded, coalesced, and atomic. A corrupt or newer state
file is preserved and reported in the footer rather than overwritten.

![Selected stacked pull request with its reviewer, label, and stack-layer details](assets/screenshots/pr-details.png)

*Select a row and open Details to inspect its loaded metadata, including the
stack layer and base branch. This example uses fictional data; opening the
panel makes no additional GitHub request.*

## Data and queue limits

Repository discovery runs at startup and **Repos** retries it. Discovery is
limited to 1,000 repositories (10 pages); configured, pinned, and current repos
remain available if discovery fails. GitHub permissions and organization SSO
determine what is visible.

Each initial involvement/authored search returns up to 60 PRs. In
all-repositories scope, candidates are limited to PRs involving your resolved
login; PR Marmot never broadens this to arbitrary other-authored public PRs.
Review queue uses one GraphQL operation with two search aliases: up to 60
explicitly requested PRs and 60 other-authored candidates, then keeps available
candidates only when they have no pending user or team reviewer request. Your
own PRs are excluded from the review queue. This is based on **review requests**,
not issue assignees.

A **partial results** notice means GitHub has another page. **Load more** can
advance each active search to a maximum of five pages: at most 300 authored
results or 600 review candidates before filtering and deduplication. An
available-candidate page can add no visible rows after filtering. Refreshing,
including automatic refresh, returns to page one; a failed page request keeps
the rows already loaded.

Search and Details operate on the loaded snapshot and make no per-PR request.
Reviewer, label, thread, and stack information is subject to the GraphQL
query's per-PR limits. Stack grouping uses GitHub's native stack metadata—not
labels or guessed branch relationships—and reports partial stacks when not all
layers appear in the same loaded section.

## Build and develop

The workspace contains the GPUI app and `core/`, a UI-independent crate for
GitHub transport, categorization, and Note logic. Current UI dependencies are
`gpui-component 0.6.0` and `gpui-pre` / `gpui-pre-platform 0.3.4`.

```sh
make check      # fmt check + core clippy -D warnings + core tests (same as CI)
make build      # debug GPUI app build
make verify     # full-workspace fmt, clippy, and tests
make hooks      # install repository git hooks
```

`make check` and CI intentionally compile only `prmarmot-core`, so they do not
need Metal. `make build`, `make verify`, and the pre-push hook compile the GPUI
binary and need the platform graphics toolchain. On macOS, if `metal` is
missing, run:

```sh
xcodebuild -downloadComponent MetalToolchain
```

Run without installing:

```sh
cargo run -- --repo owner/name
cargo run -- --repo owner/name --review
```

Build a local Mac app only after quitting any running installed instance:

```sh
make install
```

The categorization behavior is pinned against the original shell prototype by
golden tests in `core/tests/parity.rs`. Fix the port when parity fails; do not
change golden expectations to make a failing implementation pass.

### Reproduce the screenshots

```sh
cargo build
scripts/demo.sh             # fictional My PRs
scripts/demo.sh --review    # fictional Review queue
scripts/demo.sh --fail-once # exercise the initial error and Retry recovery
scripts/demo.sh --update-available # show a fictional stable update banner
scripts/demo.sh --update-failed    # show a fictional prior helper failure
scripts/demo.sh --self-test # validate the demo fixtures (Python 3 required)
```

The demo runs the real UI with a local, fail-closed `gh` stand-in. It requires
Python 3 but no GitHub login or API requests, uses a temporary config, and never
reads or modifies your normal PR Marmot settings. Links are synthetic; do not use
the demo to act on real PRs. Quit its window when finished.

### App icon and logo

The amber marmot standing watch on granite is the PR Marmot identity.
[`icon.svg`](assets/branding/icon.svg) supplies the app icon; a simplified
[`icon-small.svg`](assets/branding/icon-small.svg) preserves its silhouette
at 16 and 32 pixels. Transparent PNGs through 1024 pixels and the macOS
`prmarmot.icns` are included. The README and app bundle use these same assets.
Light/dark wordmarks and monochrome marks are available in the
[branding guide](assets/branding/README.md). Normal builds need no
icon-generation tools.

To regenerate the app icons on macOS with `rsvg-convert` (librsvg) installed:

```sh
scripts/generate-icons.sh
```

The upgraded GPUI runtime's overnight memory and idle-GPU measurement remains
pending; no resource-usage guarantee is claimed. Linux is source-only and has
not been verified in the macOS development environment.

## License

MIT — see [LICENSE](LICENSE).

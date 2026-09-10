<img src="assets/branding/icon-256.png" alt="prboard logo" width="96" height="96">

# prboard

**A native desktop dashboard for deciding what to do next on GitHub pull
requests.** prboard turns CI, review requests, completed reviews, unresolved
threads, conflicts, labels, linked issues, and GitHub stacks into two focused
queues with a plain-language **Note** on every row.

- **My PRs** puts approved PRs first, followed by needs action, awaiting review,
  and drafts. Approved PRs with action blockers lead needs action, with stack
  layers kept together in dependency order. Drafts stay in drafts.
- **Review queue** separates PRs that request your review from PRs with no
  reviewer requested that are available for someone to pick up.
- **Read-only by design:** prboard can open a PR or linked issue and copy its
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

Install and authenticate the GitHub CLI first:

```sh
gh auth login
```

prboard currently provides a prebuilt release for **Apple-silicon macOS only**.
Linux and Intel Mac users must build from source for now.

### Apple-silicon macOS

Run the installer from inside a GitHub checkout so it can detect that
repository, or pass the initial repository explicitly:

```sh
# Detect the repository from the current checkout
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prboard/main/install.sh | sh

# Or configure a repository explicitly (recommended for scripted installs)
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prboard/main/install.sh \
  | sh -s -- --repo owner/name
```

The installer downloads the latest macOS arm64 release, installs
`~/Applications/prboard.app`, and creates the config file only when it can
resolve an accessible repository. It never overwrites an existing config. Early
releases are ad-hoc signed on the destination Mac and are not yet notarized;
Homebrew installation is therefore not available yet.

**Updating? Quit the installed app before running the installer again.**

### Build from source

Requires Git, latest stable Rust (dependency minimum 1.92, not tested), and the
platform dependencies required by GPUI. macOS additionally needs full Xcode and its Metal Toolchain. Linux
needs a working Vulkan stack plus fontconfig, xkbcommon, and the normal X11 or
Wayland development/runtime libraries.

```sh
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prboard/main/install.sh \
  | sh -s -- --from-source --repo owner/name
```

On macOS this installs `~/Applications/prboard.app`; on Linux it installs the
`prboard` binary to `~/.local/bin` by default. There are no prebuilt Linux
packages yet.

## First run

1. Confirm `gh auth status` succeeds and that your account can access the
   repository.
2. Launch **prboard** from Spotlight/Finder on macOS, or run `prboard` on Linux.
3. Pick another accessible repository from the searchable repository control
   if needed. Discovery includes owned, collaborator, and organization repos
   visible to the current `gh` account.
4. Switch between **My PRs** and **Review queue** in the title bar.

Spotlight and Finder launches have no useful repository working directory, so
they require `repo` in the config file. If the installer could not create it,
create the file described below before launching. Running the binary directly
from a checkout can instead detect that checkout's GitHub remote.

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

- `$XDG_CONFIG_HOME/prboard/config.toml` when `XDG_CONFIG_HOME` is an absolute
  path;
- otherwise `~/.config/prboard/config.toml`.

Every setting is optional, but app-bundle launches need `repo`:

```toml
repo = "acme/widgets"                    # repository opened at startup
repos = ["acme/widgets", "acme/api"]    # extra repo-picker entries
pinned_repos = ["acme/widgets"]          # toolbar shortcuts, maximum 12
refresh_secs = 300                       # default 300; hard floor 30
theme = "system"                         # system | light | dark
view = "authored"                        # authored | review

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
can say, for example, “assign alice + bob.” prboard does **not** assign those people, validate that they are suitable
for the repository, read or implement `CODEOWNERS`, or infer GitHub ownership.
The same list is suggested for every repository. Leave it empty if a global
suggestion would be misleading.

Issue links are optional and tracker-agnostic. `{id}` in `url_template` is
replaced with the first identifier matched by `pattern`; no tracker or project
prefix is built in.

Environment variables override the config file, and `--repo` overrides both:

- `PRBOARD_REPO`
- `PRBOARD_REFRESH_SECS`
- `PRBOARD_THEME`
- `PRBOARD_DEFAULT_REVIEWERS` (comma-separated)
- `PRBOARD_ISSUE_PATTERN` and `PRBOARD_ISSUE_URL_TEMPLATE` (use together)

The app persists repository, theme, starting view, window size, pinned repos,
and saved Settings back to valid TOML while preserving comments and unrelated
settings. It refuses to overwrite an unparseable config. Manual file edits
require a restart; changes saved in Settings do not.

## Using prboard

- **Shortcuts:** the footer opens the full keyboard reference. Character shortcuts
  work from dashboard controls, but never while typing in search, the repo picker,
  or Settings. Arrow keys, Enter, and Space belong to the focused control.
- **Queues:** click **My PRs** / **Review queue**, press `1` / `2`, or press `v`
  to toggle.
- **Navigate:** `↑` / `↓` selects; `Enter` or `o` opens the PR; `y` copies its
  URL with a brief footer confirmation; double-clicking a row opens it.
- **Inspect:** press `Space` or click **Details**; `Esc` closes the panel.
  Select text in the panel and press `⌘C` (macOS) or `Ctrl+C` (Linux) to copy it.
  The **Copy** menu offers the PR number, title, URL, or all details without selecting text.
- **Search loaded rows:** click **Search** or press `/`. Terms match PR number,
  title, author, label, issue, and Note, and multiple terms narrow together.
- **Refresh:** `r` refreshes immediately. Automatic refresh defaults to five
  minutes and the header shows the last sync time and GitHub API budget. If the
  initial load fails, click **Retry**; rate-limit pauses still wait for their budget.
- **Repositories:** use the searchable picker; **Pin** adds an in-app shortcut
  and **Pinned** removes it. Up to 12 pins persist in order. Pins do not fetch in
  the background.
- **Theme:** `t` cycles system → light → dark.
- **Quit:** use **prboard → Quit prboard**, `⌘Q` on macOS, or `Ctrl+Q` on Linux.
  The global shortcut works even in Settings and text inputs. Plain `q` also
  quits when no text input or dialog is active.

![Selected stacked pull request with its reviewer, label, and stack-layer details](assets/screenshots/pr-details.png)

*Select a row and open Details to inspect its loaded metadata, including the
stack layer and base branch. This example uses fictional data; opening the
panel makes no additional GitHub request.*

## Data and queue limits

Repository discovery runs at startup and **Repos** retries it. Discovery is
limited to 1,000 repositories (10 pages); configured, pinned, and current repos
remain available if discovery fails. GitHub permissions and organization SSO
determine what is visible.

Each initial authored search returns up to 60 PRs. Review queue uses one GraphQL
operation with two search aliases: up to 60 explicitly requested PRs and 60
other-authored candidates, then keeps available candidates only when they have
no pending user or team reviewer request. Your own PRs are excluded from the
review queue. This is based on **review requests**, not issue assignees.

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

`make check` and CI intentionally compile only `prboard-core`, so they do not
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
scripts/demo.sh --self-test # validate the demo fixtures (Python 3 required)
```

The demo runs the real UI with a local, fail-closed `gh` stand-in. It requires
Python 3 but no GitHub login or API requests, uses a temporary config, and never
reads or modifies your normal prboard settings. Links are synthetic; do not use
the demo to act on real PRs. Quit its window when finished.

### App icon and logo

The vector source is [`assets/branding/icon.svg`](assets/branding/icon.svg).
Transparent PNGs at 16, 32, 64, 128, 256, 512, and 1024 pixels and the macOS
`prboard.icns` are included alongside it. The README and app bundle use these
same assets. Normal builds need no icon-generation tools.

To regenerate the assets on macOS with `rsvg-convert` (librsvg) installed:

```sh
scripts/generate-icons.sh
```

The upgraded GPUI runtime's overnight memory and idle-GPU measurement remains
pending; no resource-usage guarantee is claimed. Linux is source-only and has
not been verified in the macOS development environment.

## License

MIT — see [LICENSE](LICENSE).

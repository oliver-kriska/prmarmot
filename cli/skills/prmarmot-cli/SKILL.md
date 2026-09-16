---
name: prmarmot-cli
description: Read the user's GitHub pull-request dashboard with `prmarmot-cli` — their own PRs and what blocks them (CI, conflicts, reviews, unresolved comments, missing reviewers), their review queue, what changed since they last looked, and a blocking watch that waits for a PR event such as CI passing or a review arriving. Use it whenever the user asks "what's the status of my PRs", "what needs my attention", "what should I review", "anything waiting on me", "did CI pass", "tell me when this PR is approved/merged", or wants a PR summary for standup or a report. Prefer it over hand-written `gh pr list` or `gh api` loops, because it applies PR Marmot's categorization and costs one GraphQL request per view. Read-only: it cannot comment, approve, merge, watch, or snooze.
---

# prmarmot-cli

`prmarmot-cli` prints PR Marmot's two views, **My PRs** and the **Review queue**.
It uses the same GitHub query, categories, Notes, and sections as the desktop
app. Authentication comes from `gh`.

## Before the first call

Run `prmarmot-cli --version`. If the command is missing, tell the user. The PR
Marmot app installers (Homebrew cask, `install.sh`, `make install`) put it on
`PATH`; without the app it installs with
`cargo install --locked --git https://github.com/oliver-kriska/prmarmot prmarmot-cli`.

This skill is built into the binary. If a flag here is rejected, run
`prmarmot-cli skill` to read the copy that matches the installed version.
`prmarmot-cli skill install --force` refreshes the user-level copy, but ask the
user before running it.

## Snapshot: what is the state right now

Always pass `--json`; its shape is stable (`prmarmot-cli/board@1`). Pass a scope
explicitly, because the default comes from the user's config:

```sh
prmarmot-cli mine   --repo owner/name --json   # PRs the user authored in one repo
prmarmot-cli mine   --all-repos --authored --json   # PRs the user authored, any repo
prmarmot-cli mine   --all-repos --json         # every open PR involving the user
prmarmot-cli review --all-repos --json         # PRs waiting for the user's review
```

**Structure:**
- The top level has `sections[]`, in the app's display order.
- Each section has a stable `key`, a `label`, and `prs[]`.
- Section keys:

| Key | Meaning |
| --- | --- |
| `approved` | the user's PRs that are approved and waiting to merge |
| `action` | the user's PRs blocked on the user; see `blockers` |
| `await` | the user's PRs waiting on others |
| `todo` | review requested from the user |
| `available` | nobody requested yet |
| `done` | already reviewed |
| `draft` | drafts |
| `snoozed` | snoozed in the app; mention them only if asked |

**Each PR has:**
- **Identity:** `repo`, `number`, `url`, `title`, `author`.
- **State:** `ci` (`pass`, `fail`, `running`, `none`), `conflict`,
  `review_decision`, `requested_reviewers`, `reviews[]`, `my_review`,
  `unresolved_threads`, `labels`, `issue`, `stack`.
- **Reviews:** `reviews[]` has each other reviewer's standing review and
  `my_review` the user's own (`NONE` if none). A standing review is the latest,
  except that a later comment does not cancel an approval or change request.
- **`note`:** a one-line human summary.
- **Pickup age:** `waiting_since` is when the PR started waiting for a
  reviewer (its review request, or when it opened or became ready for
  review), null when it isn't waiting (drafts, already reviewed). `stale` is
  true once it has waited `filters.stale_after_days` (default 3) or longer.
  Sections that wait on a reviewer list the longest wait first.
- **`blockers[]`:** typed as `merge_conflict`, `ci_failing`,
  `changes_requested`, `unresolved_comments` (with `count`), or `no_reviewers`
  (with `suggested`).
- **`attention`:** `watched`, `snoozed`, `changed`, and `changes[]`, the
  phrases for what changed.

Fields are only added within `@1`, so ignore any you don't know. The full
JSON Schemas are `cli/schema/board-v1.schema.json` and
`cli/schema/event-v1.schema.json` in the PR Marmot repository.

Useful filters:

```sh
# What needs the user's action
prmarmot-cli mine --all-repos --json |
  jq -r '.sections[] | select(.key == "action") | .prs[] | "\(.repo)#\(.number)  \(.note)  \(.url)"'

# Reviews to do
prmarmot-cli review --all-repos --json |
  jq -r '.sections[] | select(.key == "todo") | .prs[] | "\(.repo)#\(.number) by \(.author)  \(.url)"'

# Reviews that have waited too long, oldest first
prmarmot-cli review --all-repos --stale --json |
  jq -r '.sections[] | select(.key == "todo" or .key == "available") | .prs[] | "\(.repo)#\(.number) waiting since \(.waiting_since)  \(.url)"'

# What changed since the user last looked in PR Marmot
prmarmot-cli mine --all-repos --changed --json |
  jq -r '.sections[].prs[] | "\(.repo)#\(.number): \(.attention.changes | join("; "))"'
```

Other flags:
- `--watched`: only PRs the user watches in the app.
- `--stale`: only PRs that have waited `stale_after_days` or longer for a
  reviewer. Good for "what's been sitting too long?".
- `--pages N` (1–5): load more results. Use it only when the output says
  `more_pages_available: true`.
- `--format markdown`: a ready-to-paste report with linked PR tables, useful
  when the user wants the dashboard itself rather than an answer.

## Watch: wait for something to happen

`watch` blocks. It prints one JSON line per event and exits after `--events N`
events. Run it as a background task, not a foreground command that can time
out:

```sh
# Wait for the next change to one of the user's PRs in a repo (checks every 60 s)
prmarmot-cli watch mine --repo owner/name --json --interval 60 --events 1

# Follow one PR (any repo, any author) until it merges or closes
prmarmot-cli watch --pr owner/name#123 --json --interval 60

# Wait for one PR's CI: exit 0 when it passes, 5 if it fails, 6 after 30 minutes
prmarmot-cli watch --pr owner/name#123 --json --until ci-pass --timeout 30m
```

The first line is `{"type":"ready",...}`. After that, each line is one of:
- `changed`: `kind` is `merge_conflict`, `changes_requested`, `review_again`,
  `ci_passed`, or `changed`. The line also has `title`, `body`, `changes[]`,
  and the full `pr`.
- `added`: a PR entered the view.
- `removed`: a PR left the view. `status` is `merged`, `closed`, `open`,
  `inaccessible`, or `unknown`. With `--pr` the watch ends here (exit 0) once
  the PR merges, closes, or becomes inaccessible (at once if it already has);
  with `--until`, an `until` line follows it.
- `rate_limited` or `error`: informational. The watch retries by itself; these
  don't count toward `--events`.
- `until`: the last line of a `--until` or `--timeout` wait (see below).

To wait for one specific PR, use `--pr owner/name#123` (a PR URL works too)
rather than filtering a view. Add `--events 1` to return on its first change
instead of when it closes. `--pr` can't be combined with `--repo`,
`--all-repos`, `--watched`, or `--authored`; a PR or repository that doesn't
exist or isn't visible to `gh` exits 1 before the first event.

### Wait for a condition

To wait for a state rather than for any change, add `--until` to `--pr`. The
exit code is the answer, so there is nothing to parse unless you want the
reason:

| `--until` | Met (exit 0) when | Unmet (exit 5) when |
| --- | --- | --- |
| `ci-pass` | the check rollup is green | CI fails. A PR with no checks never passes; bound the wait with `--timeout` |
| `approved` | GitHub's review decision is approved. Without branch protection the standing reviews decide: an approval, the user's own included, and no change requests | changes are requested |
| `mergeable` | approved, CI green, no conflict, not a draft, no unresolved threads, and GitHub has finished computing mergeability | CI fails or changes are requested |
| `merged` | the PR merged | the PR closed without merging |

A merge ends every wait as met (exit 0): whatever you were waiting for before
acting no longer matters. The `until` line then has `condition: "merged"`, and
`reasons[]` lists the asked-for conditions that were never seen (`not_seen`),
so you can tell "merged" from "approved". Every condition is unmet when the PR
closes without merging or becomes inaccessible. Requested changes don't end a
`ci-pass` wait.

- Repeat `--until` or comma-separate it (`--until ci-pass,approved`) to stop at
  whichever holds first. The wait is unmet only when none of them can hold.
- Conditions are checked on every poll, including the first, so a PR that
  already qualifies returns at once.
- `--timeout 30m` (also `90s`, `2h`, `1h30m`) gives up with exit 6. The last
  check lands on the deadline when the 30-second floor allows it.
- `--until` can't be combined with `--events`.

The last line is `{"type":"until",...}`:
- `outcome` is `met`, `unmet`, or `timeout`.
- `condition` names the condition that was met (`merged` after a merge).
- `reasons[]` lists each unmet condition as `condition`, `reason` (`ci_failed`,
  `changes_requested`, `closed`, `inaccessible`), and `text`. After a merge it
  lists the conditions never seen, with reason `not_seen`.
- `pr` is the PR as last seen.

With `--pr`, the `ready` line also carries `until[]` and `timeout_secs`.

```sh
# Wait up to 2 hours for an approval (a merge ends the wait too)
prmarmot-cli watch --pr https://github.com/owner/name/pull/123 --json --until approved --timeout 2h
```

Don't pipe the stream into `head` or a read loop: the pipe only closes when the
next event arrives.

Keep polling cheap:
- `--interval` can't go below 30 seconds, and the default is five minutes.
- Never loop snapshot commands to poll; use `watch`.

## Exit codes

| Code | Meaning | What to do |
| --- | --- | --- |
| `0` | ok | continue |
| `1` | GitHub or network error, or `--repo` not found / not accessible | read stderr; fix the repo name, or retry once later |
| `2` | bad arguments | read `prmarmot-cli --help` |
| `3` | `gh` missing or not signed in | ask the user to run `gh auth login`; never handle credentials yourself |
| `4` | rate limited | stop and report; don't retry in a loop |
| `5` | `watch --until`: the condition can no longer be met (CI failed, changes requested, closed without merging, inaccessible) | report the `reasons` from the `until` line; don't restart the wait |
| `6` | `watch --timeout` passed first | report that it is still pending; wait again only if the user wants |

Errors go to stderr, and stdout carries only data.

## Limits to respect

- **Read-only.** To comment, review, or merge, use `gh`, and only when the user
  asks. Watch, snooze, and "mark as seen" happen in the desktop app. Never claim
  you changed them.
- **`attention.changed`** means changed since the user last selected the PR in
  PR Marmot. The CLI never clears it.
- **Watch events** are measured from one check to the next, starting when the
  watch starts.
- **Truncation.** If `truncated` or `more_pages_available` is true, the list is
  incomplete. Say so rather than presenting it as everything.

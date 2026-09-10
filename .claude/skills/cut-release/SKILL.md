---
name: cut-release
description: Cut and publish a proper prboard release end-to-end — pick the next version from the unreleased commits, bump Cargo.toml, generate the CHANGELOG and GitHub release notes with git-cliff, tag, build + stage the ad-hoc-signed .app tarball, publish, and verify the curl install path. Use this whenever asked to release, publish, ship, cut a version, tag a version, bump the version, update the GitHub release, draft release notes, or rebuild the release asset — even if the request only says "ship it" or names a version. Do not improvise release steps from memory; this pipeline has a confirm gate and non-obvious traps (running-app SIGKILL, pre-release API visibility, CDN caching) that this skill encodes.
---

# Cut a prboard release

Target: `github.com/oliver-kriska/prboard`, asset `prboard-v<X>-macos-arm64.tar.gz`
(ad-hoc-signed `.app` until Apple Developer enrollment unblocks notarization —
see `packaging/RELEASING.md` for the eventual notarized pipeline).

The flow has two phases. **Phase A is fully reversible** — do it unattended.
**Phase B is irreversible** (a pushed tag, a published release) — only start it
after the user approves the version and notes at the gate. This split exists so a
wrong version or a broken build is caught before anything is public.

Work out of the session scratchpad (`$SCRATCHPAD`) for all artifacts so the
user's installed app is never touched.

## Traps this pipeline exists to avoid

- **Never install over `~/Applications/prboard.app`.** macOS can SIGKILL a
  running app whose signed binary is replaced in place — and an instance may be
  under an overnight memory measurement. Stage and tar instead
  (`bundle-app.sh --stage-only`); only the *user* relaunches their installed app.
- **Release only from an up-to-date `main`.** The tag must point at a pushed
  commit. If local `main` is ahead of / behind `origin/main`, reconcile first —
  never tag work that isn't on the remote.
- **Always mark each new release Latest** — publish with `--latest`, not
  `--prerelease`. Oliver's release policy applies even while the app is
  ad-hoc-signed; retain that signing disclosure in the release notes.
  Verify `/releases/latest` returns the newly published tag.
- **raw.githubusercontent.com caches ~5 min.** After pushing an `install.sh`
  change, a curl of the raw URL can serve the old script; test the local file
  first, re-verify the raw URL after the cache expires.
- A hook blocks bare `git push --force`; if history was rewritten, use
  `--force-with-lease=<ref>:<expected-remote-sha>` with an explicit SHA.

## Phase A — prepare (reversible)

### 1. Preflight

Confirm a clean tree and that local `main` equals `origin/main`
(`git status -sb`). Then run the full-workspace gate locally (you have the Metal
Toolchain; CI only covers core, so the app binary must be proven here):

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo build --release          # proves the shipped binary compiles
```

The only warning allowed is the dependency future-incompat notice.

### 2. Choose the next version — and propose it

Show what's queued and derive a version:

```sh
git describe --tags --abbrev=0        # current version, e.g. v0.1.0
make unreleased                       # the commits that will be in this release
```

Default heuristic while pre-1.0 (`0.x`): any `feat:` since the last tag → bump
**minor** (`0.1.0` → `0.2.0`); only `fix:` / `chore:` / deps → bump **patch**
(`0.1.0` → `0.1.1`); an explicit breaking change → the user's call (usually minor
while `< 1.0`). This is a proposal, not a rule — **tell the user the version you
picked and why, and let them confirm or override before you touch any files.**

### 3. Bump, generate notes, build the artifact

With the agreed `V` (no leading `v`, e.g. `0.2.0`):

```sh
# a) Bump the workspace-root [package] version in Cargo.toml to "$V".
#    (core/ is an internal crate; leave its version unless asked to sync.)
# b) Regenerate the committed changelog and refresh Cargo.lock:
git cliff --config cliff.toml --tag "v$V" -o CHANGELOG.md
cargo build --release                                   # updates Cargo.lock + builds

# c) Release-notes body from the same source, then append the install footer:
git cliff --config cliff.toml --unreleased --tag "v$V" --strip all > "$SCRATCHPAD/notes.md"
cat >> "$SCRATCHPAD/notes.md" <<'EOF'

## Install (macOS, Apple silicon)

    curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prboard/main/install.sh | sh

Use curl, not a browser download — the `.app` is ad-hoc-signed until notarization
lands; the installer strips download metadata, then signs and verifies it locally.
Apple-silicon only for now; other platforms build from source with `--from-source`.
EOF

# d) Stage (never installs) and tar the artifact:
scripts/bundle-app.sh --stage-only                      # -> target/bundle/prboard.app
COPYFILE_DISABLE=1 tar czf "$SCRATCHPAD/prboard-v$V-macos-arm64.tar.gz" -C target/bundle prboard.app
```

If this thread runs in a non-macOS orb, do not fake or cross-compile the `.app`.
After the gate is approved, pushing the release commit (which changes
`Cargo.toml`) automatically starts the hosted-Mac build at that exact commit.
Wait for it to succeed, then download its artifact:

```sh
gh run download <run-id> -n "prboard-v$V-macos-arm64" -D "$SCRATCHPAD"
shasum -a 256 -c "$SCRATCHPAD/prboard-v$V-macos-arm64.tar.gz.sha256"
```

The workflow can also be dispatched manually with an exact `ref` from GitHub's
UI or a token with Actions write permission. It only builds and uploads an
Actions artifact; it never creates a tag or release.

### 4. The gate — show the user and wait

Present: the chosen version, the `CHANGELOG.md` diff, the rendered
`$SCRATCHPAD/notes.md`, and the tarball path. **Get explicit approval before
Phase B.** If they want changes, adjust here — nothing is public yet.

## Phase B — publish (irreversible, only after approval)

```sh
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): v$V"        # cliff.toml skips this commit next cycle
git push origin main

gh release create "v$V" "$SCRATCHPAD/prboard-v$V-macos-arm64.tar.gz" \
  --latest --title "prboard v$V" --notes-file "$SCRATCHPAD/notes.md" --target main
```

`gh release create` makes the tag on the remote only — `git fetch --tags` if
local work needs it. To refresh an existing release's asset instead of cutting a
new one: `gh release upload "v$V" <tarball> --clobber`.

### 5. Verify end-to-end

Check `gh api repos/oliver-kriska/prboard/releases/latest --jq '.tag_name'`
returns `v$V`. GitHub Latest releases must not be drafts or pre-releases.

Run the exact README command into a scratch dir so the user's installed app is
untouched. A release is not done until this passes against the live release:

```sh
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prboard/main/install.sh \
  | sh -s -- --dir "$SCRATCHPAD/install-test" --repo oliver-kriska/prboard
codesign --verify --deep "$SCRATCHPAD/install-test/prboard.app"
```

### 6. After

The `unreleased.yml` workflow clears the draft "Unreleased" release on the
`release: released` event (HEAD now equals the tag, so there's nothing unreleased)
— confirm the draft is gone. If the user wants the new build, tell them to rerun
the curl installer and relaunch; never swap their running app for them.

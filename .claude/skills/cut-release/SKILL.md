---
name: cut-release
description: Cut and publish a PR Marmot release end-to-end — pick the next version from the unreleased commits, bump the app and CLI versions + Cargo.lock + CHANGELOG with `make bump`, commit, and push the vX.Y.Z tag that makes CI build, sign, notarize, publish, and update the Homebrew cask; then verify the release, the cask, and the curl installer. Use this whenever asked to release, publish, ship, cut a version, tag a version, bump the version, or check a release — even if the request only says "ship it" or names a version. Do not improvise release steps from memory; this pipeline has a confirm gate and non-obvious traps (tag-only publishing, running-app SIGKILL, signed-candidate runs after packaging changes, CDN caching) that this skill encodes.
---

# Cut a PR Marmot release

Target: `github.com/oliver-kriska/prmarmot`, asset
`prmarmot-v<X>-macos-arm64.tar.gz` (Developer-ID-signed, notarized, stapled
`prmarmot.app`, with `prmarmot-cli` inside), plus the cask in
`oliver-kriska/homebrew-tap`. `packaging/RELEASING.md` is the source of truth;
this skill is the operating procedure around it.

**CI publishes, not you.** Pushing an exact `vX.Y.Z` tag starts
`.github/workflows/release.yml`, which runs `make verify`, builds, signs,
notarizes, publishes the GitHub release as Latest, and updates the cask. There
is no manual `gh release create` step and no manual-dispatch publish path.

The flow has two phases. **Phase A is reversible** — do it unattended.
**Phase B is irreversible** (a pushed release commit and tag) — only start it
after the user approves the version at the gate.

## Traps

- **Never install over `~/Applications/prmarmot.app`.** macOS can SIGKILL a
  running app whose signed binary is replaced, and an instance may be under a
  memory measurement. Verify installs into a scratch `--dir`; only the user
  relaunches their own app. `install.sh` refuses while the app runs, even with
  `--dir` — say so rather than working around it.
- **Release only from an up-to-date `main`** (`git status -sb` clean, equal to
  `origin/main`). The tag must point at a pushed commit.
- **`git fetch --tags` first.** Tags made on GitHub exist only remotely until
  fetched; released versions are never reused or replaced — a broken release is
  fixed with a new patch version.
- **Signing or packaging changed since the last tag?** (`scripts/sign-notarize-macos.sh`,
  `scripts/bundle-app.sh`, `packaging/homebrew/prmarmot.rb`,
  `.github/workflows/release*.yml`) Run the publish-nothing candidate at the
  release commit and verify it (RELEASING.md "Safe signed candidate") before
  tagging. A failed notarization after the tag burns that version.
- **raw.githubusercontent.com caches ~5 min** after an `install.sh` change.
- A hook blocks bare `git push --force`; use
  `--force-with-lease=<ref>:<expected-remote-sha>` if history was rewritten.

## Phase A — prepare (reversible)

### 1. Preflight

```sh
git fetch --tags origin && git status -sb     # clean, main == origin/main
make verify                                  # full workspace gate incl. the GPUI app (needs Metal)
```

### 2. Propose the version

```sh
git describe --tags --abbrev=0               # current, e.g. v0.6.0
make unreleased                              # what this release contains
git diff --stat "$(git describe --tags --abbrev=0)" -- scripts/sign-notarize-macos.sh \
  scripts/bundle-app.sh packaging/homebrew .github/workflows/release.yml .github/workflows/release-build.yml
```

Pre-1.0 heuristic: any `feat:` → bump **minor**; only `fix:`/`chore:`/deps →
**patch**; a breaking change is the user's call. **Tell the user the version and
why, and wait for confirmation before touching files.** If the last command
printed anything, the candidate run in step 4 is required.

### 3. Bump (after the version is agreed)

```sh
make bump V=<X.Y.Z>     # root + cli/Cargo.toml versions, Cargo.lock, CHANGELOG.md; never commits
make verify
git diff --stat         # exactly Cargo.toml, cli/Cargo.toml, Cargo.lock, CHANGELOG.md
```

### 4. The gate — show the user and wait

Present the version, the `CHANGELOG.md` diff, and whether a signed candidate is
needed. If it is, the release commit must be pushed first so CI can check it
out; get approval for that push, then:

```sh
gh workflow run release-build.yml --ref main -f ref=<full-release-commit-sha>
gh run watch <run-id> --exit-status
```

and verify the downloaded candidate per RELEASING.md (including the
`prmarmot-cli` signature lines). **Get explicit approval for the tag.**

## Phase B — publish (irreversible, only after approval)

```sh
git add Cargo.toml cli/Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): v<X.Y.Z>"     # cliff.toml skips this commit next cycle
git push origin main
git tag v<X.Y.Z> && git push origin v<X.Y.Z>
gh run list --workflow release.yml --limit 1 # then: gh run watch <run-id> --exit-status
```

If the run fails **before** "Publish immutable GitHub release", nothing is
public: fix on main and cut a new patch version (the tag stays burned). If tap
publication fails **after** the release exists, do not rerun the workflow; run
`scripts/update-homebrew-cask.sh` from a trusted checkout.

## Verify end-to-end

```sh
gh api repos/oliver-kriska/prmarmot/releases/latest --jq .tag_name     # v<X.Y.Z>
gh api repos/oliver-kriska/homebrew-tap/contents/Casks/prmarmot.rb --jq .content | base64 -d | grep -E 'version|sha256'
```

With the user's app quit (ask; never quit it yourself), test the README
installer into scratch directories:

```sh
curl -fsSL https://raw.githubusercontent.com/oliver-kriska/prmarmot/main/install.sh \
  | sh -s -- --dir "$SCRATCHPAD/install-test" --bin-dir "$SCRATCHPAD/install-bin" --repo oliver-kriska/prmarmot
codesign --verify --deep --strict "$SCRATCHPAD/install-test/prmarmot.app"
spctl --assess --type execute "$SCRATCHPAD/install-test/prmarmot.app"
"$SCRATCHPAD/install-bin/prmarmot-cli" --version                       # prmarmot-cli <X.Y.Z>
```

`unreleased.yml` clears the draft "Unreleased" release on the tag push —
confirm it is gone. Tell the user to `brew upgrade --cask prmarmot` or rerun the
installer and relaunch; never swap their running app for them.

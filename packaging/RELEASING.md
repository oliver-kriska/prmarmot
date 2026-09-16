# Releasing PR Marmot on macOS

PR Marmot ships one Apple-silicon archive named
`prmarmot-v<VERSION>-macos-arm64.tar.gz`. The archive contains
`prmarmot.app`, whose bundle identifier is `dev.oliverkriska.prmarmot` and
`CFBundleName` is **PR Marmot**. Finder, the Dock, and Spotlight show **PR Marmot**
from `Contents/Resources/en.lproj/InfoPlist.strings`; the plain
`CFBundleDisplayName` stays `prmarmot` because macOS shows the localized name
only when that value matches the file name. The terminal and
agent CLI ships inside the same bundle as `Contents/MacOS/prmarmot-cli`
(identifier `dev.oliverkriska.prmarmot.cli`), signed before the app seals it;
the cask's `binary` stanza and `install.sh` link it onto `PATH`. Its shell
completions ship as `Contents/Resources/completions/prmarmot-cli.{bash,zsh,fish}`,
which the cask's `bash_completion`, `zsh_completion`, and `fish_completion`
stanzas link. Intel macOS and Linux release assets are intentionally out of scope.

The release pipeline is fail-closed: it will not publish an unsigned,
unnotarized, unstapled, or Gatekeeper-rejected app. Existing releases and their
assets are never replaced.

## One-time repository setup

`release.yml` refuses to publish from any repository other than
`oliver-kriska/prmarmot`. It needs these Actions repository secrets (set for
v0.6.0; rotate them the same way):

| Secret | Value |
|---|---|
| `MACOS_SIGN_P12` | Base64 of the exported Developer ID Application PKCS#12 file |
| `MACOS_SIGN_PASSWORD` | Password used when exporting that `.p12` |
| `MACOS_NOTARY_KEY` | Raw or base64 App Store Connect API `.p8` key |
| `MACOS_NOTARY_KEY_ID` | App Store Connect API key ID |
| `MACOS_NOTARY_ISSUER_ID` | App Store Connect issuer UUID |
| `HOMEBREW_TAP_GITHUB_TOKEN` | Fine-grained PAT with Contents read/write for **only** `oliver-kriska/homebrew-tap` |

Generate the base64 certificate locally without printing it:

```sh
base64 < DeveloperIDApplication.p12 | tr -d '\n' | pbcopy
```

Create the App Store Connect key with only the access needed for notarization.
Paste secrets through GitHub's encrypted secret UI or `gh secret set`; do not
put them in shell history, logs, repo files, or Actions variables. The Scribe
tap token may be scoped to a different repository and must not be assumed to
work for `oliver-kriska/homebrew-tap`.

Create the tap repository's `Casks/` directory if needed. The release workflow
renders `packaging/homebrew/prmarmot.rb` into `Casks/prmarmot.rb`, commits only
when content changes, and pushes after the GitHub release exists.

## Safe signed candidate (publishes nothing)

After any change to signing, bundling, or the release workflows since the last
tag (`scripts/sign-notarize-macos.sh`, `scripts/bundle-app.sh`,
`.github/workflows/release*.yml`), run the manual **Signed release candidate**
workflow at the exact release commit before tagging:

```sh
gh workflow run release-build.yml --ref main -f ref=<full-commit-sha>
```

It runs the full workspace gate, builds arm64, imports credentials into a
temporary keychain, Developer-ID-signs with hardened runtime and timestamp,
waits for notarization acceptance, staples and validates the ticket, runs
`codesign` and `spctl`, packages the tarball, verifies its checksum, and uploads
a seven-day Actions artifact. It never creates a tag, release, or tap commit.

Download that candidate to a scratch directory and verify it without replacing
the installed app:

```sh
V=<version>
gh run download <run-id> -n prmarmot-v$V-macos-arm64-signed-candidate -D /tmp/prmarmot-candidate
(cd /tmp/prmarmot-candidate && shasum -a 256 -c prmarmot-v$V-macos-arm64.tar.gz.sha256)
mkdir /tmp/prmarmot-app
tar xzf /tmp/prmarmot-candidate/prmarmot-v$V-macos-arm64.tar.gz -C /tmp/prmarmot-app
codesign --verify --deep --strict --verbose=2 /tmp/prmarmot-app/prmarmot.app
codesign --display --verbose=2 /tmp/prmarmot-app/prmarmot.app/Contents/MacOS/prmarmot-cli
xcrun stapler validate /tmp/prmarmot-app/prmarmot.app
spctl --assess --type execute --verbose=2 /tmp/prmarmot-app/prmarmot.app
/tmp/prmarmot-app/prmarmot.app/Contents/MacOS/prmarmot-cli --version
```

The CLI line must show `Identifier=dev.oliverkriska.prmarmot.cli`, `Authority=Developer ID
Application: …`, a timestamp, and `flags=0x10000(runtime)`.

Do not submit from local scripts during routine verification; candidate CI is
the controlled Apple submission path.

## Publishing a release

Publishing has no manual-dispatch path. It happens only when an exact
`vMAJOR.MINOR.PATCH` tag is pushed. The tag must equal the root package version
in `Cargo.toml` or the workflow stops. Obtain explicit human approval before
changing the version or pushing a tag.

Prepare the release commit on an up-to-date `main`:

```sh
make bump V=<approved-new-version>   # app + CLI versions, Cargo.lock, CHANGELOG.md; never commits
make verify
git add Cargo.toml cli/Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): v<approved-new-version>"
git push origin main
```

`make bump` keeps `cli/Cargo.toml` equal to the app version; a `prmarmot-cli`
unit test fails `make verify` if they differ. Then, still with approval:

```sh
git tag v<approved-new-version>
git push origin v<approved-new-version>
```

Released versions and their assets are never recreated or replaced: a broken
release is fixed by a new patch version. `git fetch --tags` first — tags that
exist only on GitHub are invisible to the bump script's local check.

`.github/workflows/release.yml` then:

1. rejects malformed/mismatched tags and any existing GitHub release;
2. runs `make verify` and a locked release build of the app and the CLI;
3. signs, notarizes, staples, and passes `codesign`, `stapler validate`, and
   Gatekeeper `spctl` checks;
4. creates and verifies the tarball checksum;
5. publishes the immutable GitHub release and checksum asset;
6. updates `oliver-kriska/homebrew-tap` using the verified checksum.

If the cask already has the version and checksum, the update script exits
successfully without a commit. If tap publication fails after the GitHub release
exists, do not rerun the release workflow (it refuses to overwrite assets);
repair or run the idempotent cask update separately from a trusted checkout.

## Release acceptance

After a release that changes signing or packaging (the v0.6.0 first signed
release; the first release that bundles `prmarmot-cli`), a person verifies both
a clean install and an upgrade on a second Apple-silicon Mac:

```sh
brew install --cask oliver-kriska/tap/prmarmot
brew upgrade --cask prmarmot
```

Confirm the app opens without a Gatekeeper warning, appears as **PR Marmot**,
finds the `prmarmot` binary and new config/state locations, can authenticate via
`gh`, and preserves/migrates prior prboard settings as implemented by the app.
Run `prmarmot-cli --version` and `prmarmot-cli mine --json` from a new terminal
(through the Homebrew link, not the bundle path) to confirm Gatekeeper accepts
the nested CLI too.
For the first release that ships completions, confirm the rendered
`Casks/prmarmot.rb` in the tap has the three completion stanzas, and that
`$(brew --prefix)/share/zsh/site-functions/_prmarmot-cli`,
`$(brew --prefix)/etc/bash_completion.d/prmarmot-cli`, and
`$(brew --prefix)/share/fish/vendor_completions.d/prmarmot-cli.fish` link into
the installed app.
Also test the curl installer. Remove the old `prboard.app` only after the new
app and storage migration are verified; automation intentionally does not
delete historical installations or user data.

The app shells out to `gh`; the cask declares that dependency, but each user
still needs `gh auth login` once.

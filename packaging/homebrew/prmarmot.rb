# Homebrew cask template for PR Marmot.
#
# This file lives in the app repo as a template only. The live copy belongs in
# the personal tap: oliver-kriska/homebrew-tap → Casks/prmarmot.rb.
# Install with: brew install --cask oliver-kriska/tap/prmarmot
#
# scripts/update-homebrew-cask.sh replaces version and sha256 after a GitHub
# release is published. Everything else stays fixed between releases.
#
# PRECONDITION: the .app inside the archive MUST be Developer-ID-signed, notarized,
# and stapled. Homebrew quarantines cask downloads, and from 2026-09-01 casks
# that fail Gatekeeper are unsupported — an ad-hoc-signed archive will not install
# cleanly for anyone.
cask "prmarmot" do
  version "0.5.3"                                    # <- FILL per release
  sha256 "REPLACE_WITH_SHA256_OF_RELEASE_ARCHIVE"    # <- FILL per release

  url "https://github.com/oliver-kriska/prmarmot/releases/download/v#{version}/prmarmot-v#{version}-macos-arm64.tar.gz"
  name "PR Marmot"
  desc "GitHub pull-request review dashboard"
  homepage "https://github.com/oliver-kriska/prmarmot"

  livecheck do
    url :url
    strategy :github_latest
  end

  # PR Marmot shells out to the GitHub CLI for all API access; without gh
  # (authenticated via `gh auth login`) the app cannot load any data.
  depends_on formula: "gh"
  depends_on arch: :arm64
  depends_on macos: :monterey

  app "prmarmot.app"
  # The terminal/agent CLI ships inside the signed bundle; Homebrew links it
  # onto PATH, so `brew upgrade` updates both.
  binary "#{appdir}/prmarmot.app/Contents/MacOS/prmarmot-cli"
  # Its shell completions ship in the bundle too. zsh loads a completion by the
  # file name `_<command>`, and the others are named for the command too.
  bash_completion "#{appdir}/prmarmot.app/Contents/Resources/completions/prmarmot-cli.bash",
                  target: "prmarmot-cli"
  fish_completion "#{appdir}/prmarmot.app/Contents/Resources/completions/prmarmot-cli.fish"
  zsh_completion "#{appdir}/prmarmot.app/Contents/Resources/completions/prmarmot-cli.zsh",
                 target: "_prmarmot-cli"

  zap trash: [
    "~/.config/prmarmot",
    "~/.local/state/prmarmot",
    "~/Library/Application Support/prmarmot",
    "~/Library/Caches/dev.oliverkriska.prmarmot",
    "~/Library/Saved Application State/dev.oliverkriska.prmarmot.savedState",
  ]
end

# PR Marmot developer tasks. `make check` mirrors CI exactly.
#
# Fast targets (fmt/lint/test) cover the GPUI-free crates — prmarmot-core,
# prmarmot-local, prmarmot-cli — need no GPU/Metal, and are what CI runs.
# The GPUI binary (build/run/release) compiles Metal shaders and needs the Xcode
# Metal Toolchain locally — see CLAUDE.md.

.PHONY: help fmt fmt-check lint lint-all verify test build release run cli install check ci fix \
        hooks changelog unreleased bump clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

## ---- Quality gate (fast, no Metal — matches CI) ------------------------------

fmt: ## Format the whole workspace
	cargo fmt

fmt-check: ## Check formatting (CI mode)
	cargo fmt --check

# The crates that build without GPUI/Metal.
FAST_CRATES := -p prmarmot-core -p prmarmot-local -p prmarmot-cli

lint: ## Clippy on core, local, and cli, warnings as errors (matches CI)
	cargo clippy $(FAST_CRATES) --all-targets -- -D warnings

test: ## Run the core spec + golden suite and the local/cli tests (matches CI)
	cargo test $(FAST_CRATES)

check: fmt-check lint test ## Full local gate — run before every commit/push

ci: check ## Alias: simulate CI locally

fix: ## Auto-fix formatting and the clippy lints that are auto-fixable
	cargo fmt
	cargo clippy $(FAST_CRATES) --fix --allow-dirty --allow-staged

## ---- GPUI binary (needs the Metal Toolchain locally) -------------------------

lint-all: ## Clippy on the ENTIRE workspace incl. the GPUI binary (Metal required)
	cargo clippy --all-targets -- -D warnings

verify: ## Full-workspace gate incl. the GPUI app (needs Metal) — what pre-push runs
	cargo fmt --all --check
	cargo clippy --all-targets -- -D warnings
	cargo test --workspace

build: ## Debug build of the app
	cargo build

release: ## Release build (LTO) — used for measurements and shipping
	cargo build --release

run: ## Run the debug app
	cargo run

cli: ## Build the terminal/agent CLI (no Metal) -> target/release/prmarmot-cli
	cargo build --release -p prmarmot-cli

install: ## Release-build and install ~/Applications/prmarmot.app + link ~/.local/bin/prmarmot-cli (refuses if running)
	@pgrep -f 'prmarmot.app/Contents/MacOS/prmarmot( |$$)' >/dev/null \
		&& { echo "prmarmot.app is running — quit it first (macOS SIGKILLs an app whose binary is swapped; this also protects a live memory-gate run)"; exit 1; } \
		|| true
	cargo build --release --workspace
	scripts/bundle-app.sh

## ---- Release notes / changelog -----------------------------------------------

changelog: ## Regenerate CHANGELOG.md from the commit history (git-cliff)
	git cliff --config cliff.toml -o CHANGELOG.md

unreleased: ## Print what's on main but not in the latest tag
	@git cliff --config cliff.toml --unreleased --strip all

bump: ## Set app + CLI version, Cargo.lock, CHANGELOG for a release commit: make bump V=X.Y.Z (no commit/tag)
	@test -n "$(V)" || { echo "usage: make bump V=X.Y.Z"; exit 2; }
	scripts/bump-version.sh $(V)

## ---- Setup -------------------------------------------------------------------

hooks: ## Install the repo git hooks (pre-commit + commit-msg + pre-push)
	git config core.hooksPath .githooks
	@echo "core.hooksPath -> .githooks (pre-commit, commit-msg, pre-push active)"

clean: ## Remove build artifacts
	cargo clean

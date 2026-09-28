# HARW — workspace targets
#
# `clippy-tests` is this repo's canonical verification command: it runs
# Clippy across the whole workspace including test targets with
# `-D warnings`, then runs the test suite. Do not call `cargo` directly
# outside of these targets.
#
# `gates` invokes `xtask gates` — the dependency-graph, privilege and warden
# structure gates, built on top of `WorkspaceGraph::load` over the root
# workspace (which, since PL-60, includes the DoD crates under
# `dod/crates/`). It does NOT
# hang off `check`: `check` is meant to stay a fast type check that can be
# run repeatedly on its own, and a gate that slows down every `check` gets
# bypassed in practice (e.g. by calling `cargo check` directly). It hangs
# off `clippy-tests`, the canonical verification command before a
# commit/PR — an extra run there is acceptable, and that is exactly where a
# red gate must not be missed anymore. `gates` stays callable on its own as
# well, because a gate you have to call separately does not get called — it
# also has to run in normal operation.

# Use the user-specific Rust toolchain even when `cargo` is not on PATH
# (e.g. in slim shells/GUI launches). On systems without a rustup fallback
# the normal PATH lookup still applies; `make CARGO=…` explicitly overrides
# both.
CARGO ?= $(if $(wildcard $(HOME)/.cargo/bin/cargo),$(HOME)/.cargo/bin/cargo,cargo)

# Install prefix. BINDIR defaults to ~/.local/bin (no root required);
# PREFIX=/some/prefix, if given, is honored as $(PREFIX)/bin unless BINDIR
# is set explicitly. BINDIR always wins over PREFIX.
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin

.PHONY: help clippy-tests clippy tests fmt check build install uninstall service gates \
	dod-build dod-install dod-enable dod-uninstall release release-publish

.DEFAULT_GOAL := help

## List available targets with a short description (default goal).
help:
	@echo "HARW — available targets:"
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z0-9_-]+:.*?## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "Variables: CARGO, PREFIX, BINDIR (default $(HOME)/.local/bin)"

clippy-tests: ## Canonical verification: clippy (incl. tests, -D warnings) + test suite + gates
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	$(CARGO) test --workspace --all-features
	$(MAKE) gates

gates: ## Dependency-graph, privilege and warden structure gates (xtask gates)
	$(CARGO) run -q -p xtask -- gates

clippy: ## Clippy only (incl. test targets), warnings as errors
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

tests: ## Test suite only
	$(CARGO) test --workspace --all-features

fmt: ## Format check without changing anything
	$(CARGO) fmt --all --check

check: ## Fast type check of the whole workspace
	$(CARGO) check --workspace --all-features

build: ## Release-build harw, killer and the agent runner
	$(CARGO) build --release --bin harw --bin killer
	$(CARGO) build --profile release-runner --bin harw-agent-runner

# HARW_HOME follows the same default the Rust side uses (harw-home::paths,
# `HARW_HOME` env override, else `~/.harw`) — kept in sync by hand since Make
# cannot call into that crate without invoking cargo. `?=` already leaves an
# inherited `HARW_HOME` environment variable untouched.
HARW_HOME ?= $(HOME)/.harw

# Host target triple, read from `rustc -vV` (not `cargo`) at install time —
# see `docs/adr/0001-agent-compiler.md`: this runs once per install,
# never inside an agent's build, so it does not fall under the
# subagents-never-build rule above.
HARW_HOST_TARGET = $(shell rustc -vV | sed -n 's/^host: //p')

# Workspace version, read straight from the root Cargo.toml's
# `[workspace.package]` table (the same value `harw-agent-compiler`'s
# `HARW_VERSION` embeds via `CARGO_PKG_VERSION`), so Make needs no cargo
# subprocess to know it.
HARW_VERSION = $(shell awk -F'"' '/^version = /{print $$2; exit}' Cargo.toml)

install: build ## Install harw, killer and the agent runner into BINDIR (default ~/.local/bin)
	install -Dm755 target/release/harw $(BINDIR)/harw
	install -Dm755 target/release/killer $(BINDIR)/killer
	install -Dm755 target/release-runner/harw-agent-runner $(BINDIR)/harw-agent-runner
	install -Dm755 target/release-runner/harw-agent-runner \
		"$(HARW_HOME)/bin/.runners/$(HARW_HOST_TARGET)/$(HARW_VERSION)/harw-agent-runner"
	@echo "Installed $(BINDIR)/harw, $(BINDIR)/killer and $(BINDIR)/harw-agent-runner"
	@echo "Runner copy: $(HARW_HOME)/bin/.runners/$(HARW_HOST_TARGET)/$(HARW_VERSION)/harw-agent-runner"
	@case ":$$PATH:" in \
		*":$(BINDIR):"*) ;; \
		*) echo "Hint: $(BINDIR) is not on your PATH. Add e.g. 'export PATH=\"$(BINDIR):\$$PATH\"' to your shell profile." ;; \
	esac
	$(BINDIR)/harw agent install-record --source-dir $(CURDIR) --bindir $(BINDIR)
	$(BINDIR)/harw agent auto-build-uia || true

# Release tarballs in the layout `harw update` and `install.sh --binary` read
# (scripts/package-release.sh). RELEASE_TARGET defaults to the host;
# another target needs its Rust target and linker installed (or run the
# same two builds under `cross`).
RELEASE_TARGET ?= $(HARW_HOST_TARGET)
RELEASE_TAG ?= v$(HARW_VERSION)
DIST ?= dist

# killer needs Linux procfs and pidfd; Android targets build harw only.
RELEASE_PACKAGES = -p harw-cli $(if $(findstring android,$(RELEASE_TARGET)),,-p harw-killer)

release: ## Build and package a release tarball + SHA256SUMS into $(DIST)/ (RELEASE_TARGET, RELEASE_TAG)
	$(CARGO) build --release --locked $(RELEASE_PACKAGES) --target $(RELEASE_TARGET)
	$(CARGO) build --profile release-runner --locked -p harw-agent-runner --target $(RELEASE_TARGET)
	scripts/package-release.sh $(RELEASE_TAG) $(RELEASE_TARGET) $(DIST)

release-publish: ## Create the GitHub release $(RELEASE_TAG) from $(DIST)/ (needs the gh CLI, logged in)
	@ls $(DIST)/*.tar.gz $(DIST)/SHA256SUMS >/dev/null
	gh release create $(RELEASE_TAG) $(DIST)/*.tar.gz $(DIST)/SHA256SUMS \
		--title "harw $(RELEASE_TAG)" --notes "See CHANGELOG.md for $(RELEASE_TAG)."

uninstall: ## Remove harw, killer and the agent runner from BINDIR, plus the regenerable agent-build cache
	rm -f $(BINDIR)/harw $(BINDIR)/killer $(BINDIR)/harw-agent-runner
	rm -rf "$${HARW_HOME:-$$HOME/.harw}/cache/agent-builds"
	rm -f "$${HARW_HOME:-$$HOME/.harw}/install.toml"
	@echo "Removed $(BINDIR)/harw, killer, harw-agent-runner and the agent-build cache (the rest of ~/.harw was left untouched)"

service: install ## Install and enable the user systemd services, incl. the gateway
	$(BINDIR)/harw service install
	@echo "Harw services installed and enabled."

dod-build: ## DoD: build the system package (host binaries + eBPF artifacts)
	$(MAKE) -C dod build

dod-install: ## DoD: install the system package onto the host (root; see docs/setup/dod.md)
	$(MAKE) -C dod install

dod-enable: ## DoD: explicitly validate config and enable observation
	$(MAKE) -C dod enable

dod-uninstall: ## DoD: remove only manifest-owned files (config/state/logs are preserved)
	$(MAKE) -C dod uninstall

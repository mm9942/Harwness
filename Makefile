# HARW — workspace targets
#
# `clippy-tests` is this repo's canonical verification command: it runs
# Clippy across the whole workspace including test targets with
# `-D warnings`, then runs the test suite. Do not call `cargo` directly
# outside of these targets.
#
# `gates` invokes `xtask gates` — the dependency-graph, privilege and warden
# structure gates, built on top of `WorkspaceGraph::load_many` over the
# product workspace and the separate `dod/` workspace. It does NOT
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
	dod-build dod-install dod-enable dod-uninstall

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

build: ## Release-build harw and killer
	$(CARGO) build --release --bin harw --bin killer

install: build ## Install harw and killer into BINDIR (default ~/.local/bin)
	install -Dm755 target/release/harw $(BINDIR)/harw
	install -Dm755 target/release/killer $(BINDIR)/killer
	@echo "Installed $(BINDIR)/harw and $(BINDIR)/killer"
	@case ":$$PATH:" in \
		*":$(BINDIR):"*) ;; \
		*) echo "Hint: $(BINDIR) is not on your PATH. Add e.g. 'export PATH=\"$(BINDIR):\$$PATH\"' to your shell profile." ;; \
	esac

uninstall: ## Remove harw and killer from BINDIR (never touches ~/.harw)
	rm -f $(BINDIR)/harw $(BINDIR)/killer
	@echo "Removed $(BINDIR)/harw and $(BINDIR)/killer (~/.harw was left untouched)"

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

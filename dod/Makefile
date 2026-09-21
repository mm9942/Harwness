SHELL := /bin/sh

# FHS installation knobs.  DESTDIR is intentionally empty by default: when
# set, every file operation is redirected below it and no host-side action is
# allowed (no users, tmpfiles, daemon-reload, enable, or start).
DESTDIR ?=
PREFIX ?= /usr/local
LIBEXECDIR ?= $(PREFIX)/libexec/harw-dod
LIBDIR ?= $(PREFIX)/lib/harw-dod
BPFDIR ?= $(LIBDIR)/bpf
SYSCONFDIR ?= /etc/harw-dod
LOCALSTATEDIR ?= /var
STATEDIR ?= $(LOCALSTATEDIR)/lib/harw-dod
LOGDIR ?= $(LOCALSTATEDIR)/log/harw-dod
RUNSTATEDIR ?= /run
RUNTIMEDIR ?= $(RUNSTATEDIR)/harw-dod
SYSTEMD_UNITDIR ?= $(PREFIX)/lib/systemd/system
SYSUSERSDIR ?= $(PREFIX)/lib/sysusers.d
TMPFILESDIR ?= $(PREFIX)/lib/tmpfiles.d

TARGET_DIR ?= $(CURDIR)/target
BINARY_DIR ?= $(TARGET_DIR)/release
BPF_ARTIFACT_DIR ?= $(CURDIR)/bpf

INSTALL_ENV = DESTDIR='$(DESTDIR)' PREFIX='$(PREFIX)' \
	LIBEXECDIR='$(LIBEXECDIR)' LIBDIR='$(LIBDIR)' BPFDIR='$(BPFDIR)' \
	SYSCONFDIR='$(SYSCONFDIR)' LOCALSTATEDIR='$(LOCALSTATEDIR)' \
	STATEDIR='$(STATEDIR)' LOGDIR='$(LOGDIR)' RUNSTATEDIR='$(RUNSTATEDIR)' \
	RUNTIMEDIR='$(RUNTIMEDIR)' SYSTEMD_UNITDIR='$(SYSTEMD_UNITDIR)' \
	SYSUSERSDIR='$(SYSUSERSDIR)' TMPFILESDIR='$(TMPFILESDIR)' \
	BINARY_DIR='$(BINARY_DIR)' BPF_ARTIFACT_DIR='$(BPF_ARTIFACT_DIR)'

# Privilege escalation for host installation.  Empty when already root or when
# DESTDIR staging is requested (packaging sandboxes must never invoke sudo).
PRIV_ESC := $(shell if [ "$$(id -u)" -ne 0 ] && [ -z "$(DESTDIR)" ]; then printf 'sudo -E'; fi)

.PHONY: help doctor check-config ensure-bpf-linker install-bpf-linker build-bpf build check fmt clippy test verify \
	install install-existing stage enable enable-warden disable restart status logs smoke uninstall stage-test

help:
	@printf '%s\n' \
		'DoD packaging targets:' \
		'  doctor       read-only host/toolchain prerequisite report' \
		'  check-config validate TOML and print the selected profile' \
		'  build-bpf    build externally pinned BPF ELF artifacts' \
		'  install-bpf-linker  optional Rust BPF linker setup' \
		'  build        build release host binaries and BPF artifacts' \
		'  check/fmt/clippy/test/verify  DoD workspace quality gates' \
		'  install      build unprivileged, then install and start DoD on the host' \
		'  install-existing  install or uninstall already-built artifacts (no Cargo)' \
		'  stage        DESTDIR-only install for package building' \
		'  enable       explicitly validate config, then enable observation' \
		'  enable-warden  create guard file and start the Warden socket/service' \
		'  disable      stop and disable observation services' \
		'  restart      validate config and restart observation' \
		'  status/logs   inspect observation services' \
		'  smoke        explicit live smoke check (SMOKE_CONFIRM=1)' \
		'  uninstall    remove only manifest-owned files; preserve data/config' \
		'  stage-test   static packaging/placeholder checks (no Cargo invocation)' \
		'' \
		'Install variables: DESTDIR PREFIX LIBEXECDIR LIBDIR BPFDIR SYSCONFDIR' \
		'LOCALSTATEDIR RUNSTATEDIR SYSTEMD_UNITDIR SYSUSERSDIR TMPFILESDIR.'

doctor:
	@exec scripts/doctor.sh

check-config:
	@exec scripts/check-config.sh $(if $(CONFIG),--config '$(CONFIG)',)

# Install the Rust BPF linker into the invoking user's Cargo bin directory
# when it is not already available.  This is deliberately a separate,
# explicit-capability step and never runs from the read-only `doctor` target.
ensure-bpf-linker:
	@set -eu; \
	if command -v bpf-linker >/dev/null 2>&1 || [ -x "$(HOME)/.cargo/bin/bpf-linker" ]; then \
		echo 'bpf-linker already installed'; \
	else \
		test "$$(id -u)" -ne 0 || { echo 'bpf-linker installation must run unprivileged' >&2; exit 2; }; \
		command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
		if command -v cargo-binstall >/dev/null 2>&1; then \
			cargo binstall --no-confirm bpf-linker; \
		else \
			echo 'cargo install bpf-linker is unsupported here because it requires a matching LLVM build.' >&2; \
			echo 'Install cargo-binstall, then rerun: make install-bpf-linker' >&2; \
			echo 'Or download the ARM64 release from:' >&2; \
			echo 'https://github.com/aya-rs/bpf-linker/releases/latest/download/bpf-linker-aarch64-unknown-linux-musl.tar.zst' >&2; \
			exit 2; \
		fi; \
	fi

install-bpf-linker: ensure-bpf-linker

# These recipes are deliberately explicit.  The installer never invokes any
# of them as root; missing artifacts produce an actionable error instead.
build-bpf:
	@BPF_ARTIFACT_DIR='$(BPF_ARTIFACT_DIR)' exec scripts/build-bpf.sh

build:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'build must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; \
	command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo build --manifest-path Cargo.toml --release --target-dir '$(TARGET_DIR)' \
		-p harw-sentinel -p harw-probe-bpf -p harw-probe-fs -p harw-warden; \
	$(MAKE) build-bpf

check:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'check must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo check --manifest-path Cargo.toml --workspace

fmt:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'fmt must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo fmt --manifest-path Cargo.toml --all -- --check

clippy:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'clippy must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo clippy --manifest-path Cargo.toml --workspace --all-targets -- -D warnings

test:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'test must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo test --manifest-path Cargo.toml --workspace

verify: check fmt clippy test
	@exec scripts/verify-package.sh

# Internal helper: escalate to install.sh when needed, but never mix sudo
# with DESTDIR staging environments.
_run_install = \
	if [ -n '$(DESTDIR)' ] && [ -n '$(PRIV_ESC)' ]; then \
		echo 'install: DESTDIR is set, but PRIV_ESC is not empty; refusing to mix staging and sudo' >&2; \
		exit 2; \
	fi; \
	$(INSTALL_ENV) $(PRIV_ESC) scripts/install.sh

# Always rebuild the release host binaries and pinned BPF artifacts before
# packaging.  This keeps install/stage from silently reusing stale files in
# target/release or bpf/ after source changes.
#
# After installing the files, enable and start the DoD observation target.
# The first sudo prompt covers both the file installation and the service
# activation.
install: build
	@set -eu; \
	$(_run_install); \
	$(PRIV_ESC) CONFIG='$(SYSCONFDIR)/config.toml' scripts/service.sh enable

install-existing:
	@set -eu; $(_run_install)

stage:
	@test -n '$(DESTDIR)' || { echo 'stage requires DESTDIR=/path/to/stage' >&2; exit 2; }
	@$(MAKE) --no-print-directory install

enable:
	@CONFIG='$(if $(CONFIG),$(CONFIG),$(SYSCONFDIR)/config.toml)' exec scripts/service.sh enable

enable-warden:
	@set -eu; \
	$(PRIV_ESC) install -d -m 0750 '$(SYSCONFDIR)'; \
	$(PRIV_ESC) touch '$(SYSCONFDIR)/warden.enable'; \
	$(PRIV_ESC) systemctl daemon-reload; \
	$(PRIV_ESC) systemctl enable --now harw-dod-warden.socket; \
	printf '%s\n' 'Warden guard file created and socket enabled/started.'

disable:
	@exec scripts/service.sh disable

restart:
	@CONFIG='$(if $(CONFIG),$(CONFIG),$(SYSCONFDIR)/config.toml)' exec scripts/service.sh restart

status:
	@CONFIG='$(if $(CONFIG),$(CONFIG),$(SYSCONFDIR)/config.toml)' exec scripts/service.sh status

logs:
	@exec scripts/service.sh logs

smoke:
	@exec scripts/service.sh smoke

uninstall:
	@set -eu; $(INSTALL_ENV) $(PRIV_ESC) scripts/install.sh uninstall

stage-test:
	@exec scripts/stage-test.sh

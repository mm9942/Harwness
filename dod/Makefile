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
# Runtime sockets live in the fixed /run/harw (deploy/tmpfiles.d/harw.conf);
# there is no runtime-directory knob.
SYSTEMD_UNITDIR ?= $(PREFIX)/lib/systemd/system
SYSUSERSDIR ?= $(PREFIX)/lib/sysusers.d
TMPFILESDIR ?= $(PREFIX)/lib/tmpfiles.d

# Since PL-60 the DoD crates are members of the root Cargo workspace (one
# Cargo.toml, one Cargo.lock at the repository root); there is no
# dod/Cargo.toml any more. Every Cargo call below therefore goes through the
# root manifest and selects the DoD packages with `-p` (resolver 2 unifies
# features only across the selected packages, so product-crate features do
# not leak into DoD builds). check/fmt/clippy/test share the root `target/`.
#
# `build` (the packaged, privileged release binaries) deliberately keeps its
# own target directory `dod/target`: install/install-existing copy from
# $(BINARY_DIR), and a shared root `target/release` could hold a binary last
# built by an unrelated `cargo build --release --workspace` with
# product-crate features unified in. A separate directory guarantees the
# installed binaries are exactly the package-scoped build below.
ROOT_MANIFEST ?= $(abspath $(CURDIR)/../Cargo.toml)
DOD_PACKAGES := $(foreach dir,$(sort $(wildcard $(CURDIR)/crates/*/Cargo.toml)),-p $(notdir $(patsubst %/Cargo.toml,%,$(dir))))

TARGET_DIR ?= $(CURDIR)/target
BINARY_DIR ?= $(TARGET_DIR)/release
BPF_ARTIFACT_DIR ?= $(CURDIR)/bpf

INSTALL_ENV = DESTDIR='$(DESTDIR)' PREFIX='$(PREFIX)' \
	LIBEXECDIR='$(LIBEXECDIR)' LIBDIR='$(LIBDIR)' BPFDIR='$(BPFDIR)' \
	SYSCONFDIR='$(SYSCONFDIR)' LOCALSTATEDIR='$(LOCALSTATEDIR)' \
	STATEDIR='$(STATEDIR)' LOGDIR='$(LOGDIR)' \
	SYSTEMD_UNITDIR='$(SYSTEMD_UNITDIR)' \
	SYSUSERSDIR='$(SYSUSERSDIR)' TMPFILESDIR='$(TMPFILESDIR)' \
	BINARY_DIR='$(BINARY_DIR)' BPF_ARTIFACT_DIR='$(BPF_ARTIFACT_DIR)'

# Privilege escalation for host installation.  Empty when already root or when
# DESTDIR staging is requested (packaging sandboxes must never invoke sudo).
PRIV_ESC := $(shell if [ "$$(id -u)" -ne 0 ] && [ -z "$(DESTDIR)" ]; then printf 'sudo -E'; fi)

.PHONY: help doctor check-config ensure-bpf-toolchain build-bpf build check fmt clippy test verify \
	install install-existing stage enable enable-warden disable restart status logs smoke uninstall stage-test \
	clean-ephemeral

help:
	@printf '%s\n' \
		'DoD packaging targets:' \
		'  doctor       read-only host/toolchain prerequisite report' \
		'  check-config validate TOML and print the selected profile' \
		'  ensure-bpf-toolchain  locate/install clang, bpftool, llvm-objdump' \
		'  build-bpf    build externally pinned BPF ELF artifacts' \
		'  build        build release host binaries and BPF artifacts' \
		'  check/fmt/clippy/test/verify  DoD crate quality gates (root workspace, -p)' \
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
		'  clean-ephemeral  list (default) or delete (APPLY=1) rotated telemetry older than' \
		'               RETENTION_DAYS (default 7); never touches spool, findings or audit data' \
		'' \
		'Install variables: DESTDIR PREFIX LIBEXECDIR LIBDIR BPFDIR SYSCONFDIR' \
		'LOCALSTATEDIR STATEDIR LOGDIR SYSTEMD_UNITDIR SYSUSERSDIR TMPFILESDIR.' \
		'Units, sysusers and tmpfiles are installed from ../deploy (canonical source).'

doctor:
	@exec scripts/doctor.sh

# Ephemeral cleanup (retention class `telemetry_rotated`): rotated telemetry
# files (`rotated-*.jsonl` and their `.blake3` digest sidecars, always removed
# as a pair) older than RETENTION_DAYS under $(LOGDIR). A DRY RUN by default:
# it only lists. APPLY=1 deletes. The finding spool, the findings export and
# audit data are security-relevant and opt-in; this target never touches them
# (deploy/tmpfiles.d/harw.conf deliberately sets no age on those directories).
RETENTION_DAYS ?= 7
APPLY ?=
clean-ephemeral:
	@dir='$(DESTDIR)$(LOGDIR)'; \
	if [ ! -d "$$dir" ]; then printf 'clean-ephemeral: %s does not exist, nothing to do\n' "$$dir"; exit 0; fi; \
	find "$$dir" -maxdepth 1 -type f -name 'rotated-*.jsonl' -mtime +$(RETENTION_DAYS) -print | while IFS= read -r f; do \
		if [ -n '$(APPLY)' ]; then \
			rm -f -- "$$f" "$$f.blake3" && printf 'removed %s (+ digest sidecar)\n' "$$f"; \
		else \
			printf 'would remove %s (+ digest sidecar)\n' "$$f"; \
		fi; \
	done

check-config:
	@exec scripts/check-config.sh $(if $(CONFIG),--config '$(CONFIG)',)

# Locate (and, if genuinely missing and explicitly allowed, install via
# apt-get) the standalone C BPF build domain's external tools: clang,
# bpftool, llvm-objdump. There is no Rust eBPF toolchain here (the DoD BPF
# programs are plain C, built by dod/bpf/Makefile with clang/bpftool/
# llvm-objdump directly) — no bpf-linker is involved anywhere in this repo.
#
# This is the first step of `install` (and of `build`, via `build-bpf`'s own
# dependency below) precisely because bpftool commonly lives in /usr/sbin,
# which is outside an unprivileged user's PATH; earlier a plain `command -v
# bpftool` in doctor/verify-toolchain silently reported it missing even when
# installed. Installing the missing apt package(s) is deliberately this dod
# Makefile's responsibility, not dod/bpf/Makefile's: that Makefile is a
# self-contained, standalone build domain with no dependency-install target
# by design, and stays that way (unchanged).
#
# Privilege escalation is used ONLY for `apt-get install`, and only when a
# tool is actually missing; the BPF build itself always runs unprivileged.
# Without PRIV_ESC/root, without apt-get, or with DESTDIR set (a staging
# build must never touch the host), a missing tool aborts with an actionable
# command instead of installing anything. Version pinning is untouched: the
# fail-closed comparison against bpf/toolchain.lock.toml still happens in
# bpf/scripts/verify-toolchain.sh, unchanged by this step.
ensure-bpf-toolchain:
	@PRIV_ESC='$(PRIV_ESC)' DESTDIR='$(DESTDIR)' exec scripts/ensure-bpf-toolchain.sh

# These recipes are deliberately explicit.  The installer never invokes any
# of them as root; missing artifacts produce an actionable error instead.
# CLANG/BPFTOOL/LLVM_OBJDUMP are resolved to absolute paths (searching PATH,
# then /usr/sbin, /sbin) so dod/bpf/Makefile's tools are found even when a
# package (e.g. bpftool) installs outside PATH; the standalone BPF Makefile
# itself is untouched, it just receives these as ordinary variable overrides.
build-bpf: ensure-bpf-toolchain
	@BPF_ARTIFACT_DIR='$(BPF_ARTIFACT_DIR)' \
		CLANG='$(shell scripts/ensure-bpf-toolchain.sh --print-var clang)' \
		BPFTOOL='$(shell scripts/ensure-bpf-toolchain.sh --print-var bpftool)' \
		LLVM_OBJDUMP='$(shell scripts/ensure-bpf-toolchain.sh --print-var llvm-objdump)' \
		exec scripts/build-bpf.sh

build:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'build must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; \
	command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo build --manifest-path '$(ROOT_MANIFEST)' --locked --release --target-dir '$(TARGET_DIR)' \
		-p harw-sentinel -p harw-probe-bpf -p harw-probe-fs -p harw-warden; \
	$(MAKE) build-bpf

check:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'check must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo check --manifest-path '$(ROOT_MANIFEST)' --locked $(DOD_PACKAGES)

fmt:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'fmt must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo fmt --manifest-path '$(ROOT_MANIFEST)' $(DOD_PACKAGES) -- --check

clippy:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'clippy must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo clippy --manifest-path '$(ROOT_MANIFEST)' --locked $(DOD_PACKAGES) --all-targets -- -D warnings

test:
	@set -eu; test "$$(id -u)" -ne 0 || { echo 'test must run unprivileged; do not invoke Cargo as root' >&2; exit 2; }; command -v cargo >/dev/null 2>&1 || { echo 'cargo is required; run make doctor first' >&2; exit 2; }; \
	cargo test --manifest-path '$(ROOT_MANIFEST)' --locked $(DOD_PACKAGES)

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
install: ensure-bpf-toolchain build
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
	$(PRIV_ESC) systemctl enable --now harw-warden.socket; \
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

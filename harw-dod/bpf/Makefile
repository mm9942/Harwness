# Standalone, non-Cargo build domain for the versioned DoD C BPF objects.
# It deliberately has no dependency-install target.  A missing or unpinned
# clang/bpftool is a hard error, not an invitation to download a toolchain.

BPF_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
OUT ?= $(BPF_DIR)/out
CLANG ?= clang
BPFTOOL ?= bpftool
LLVM_OBJDUMP ?= $(if $(shell command -v llvm-objdump 2>/dev/null),llvm-objdump,llvm-objdump-19)
TOOLCHAIN_LOCK ?= $(BPF_DIR)/toolchain.lock.toml
VMLINUX_BTF ?= /sys/kernel/btf/vmlinux
VMLINUX_H := $(OUT)/include/vmlinux.h
# Content key of the target BTF: sha256(VMLINUX_BTF), rewritten only when it
# changes (scripts/btf-stamp.sh). vmlinux.h, and through it every object,
# depends on it, so a changed BTF behind the same path, or a different
# VMLINUX_BTF for a reused OUT, rebuilds the header, all four objects and the
# manifest; write-manifest.sh refuses a stamp that does not match the BTF.
VMLINUX_BTF_STAMP := $(OUT)/vmlinux.btf.sha256
MANIFEST := $(OUT)/manifest.json

OBJECTS := $(OUT)/exec.bpf.o $(OUT)/exit.bpf.o $(OUT)/tcp_v4_connect.bpf.o $(OUT)/tcp_v6_connect.bpf.o
CFLAGS := -target bpfel -O2 -g -Wall -Werror -fdebug-prefix-map=$(BPF_DIR)=/usr/src/harw-dod-bpf -I$(OUT)/include -I$(BPF_DIR)/include

.PHONY: all verify-tools verify-objects check-manifest-writer check-btf-stamp clean help FORCE

all: verify-tools $(VMLINUX_H) $(OBJECTS) verify-objects $(MANIFEST)

help:
	@echo "Standalone DoD BPF build (never invokes Cargo)."
	@echo "Required tools and exact versions are in toolchain.lock.toml."
	@echo "Set OUT=... to stage objects; VMLINUX_BTF=... selects target BTF."
	@echo "check-manifest-writer: diff write-manifest.sh output against the golden manifest."
	@echo "check-btf-stamp: check that a changed BTF rebuilds header, objects and manifest (no clang needed)."

verify-tools:
	@$(BPF_DIR)/scripts/verify-toolchain.sh "$(TOOLCHAIN_LOCK)" "$(CLANG)" "$(BPFTOOL)" "$(LLVM_OBJDUMP)"

# Runs on every make (FORCE) but rewrites the stamp only on a content change;
# make then re-reads its mtime, so dependants rebuild only after a change.
# '+' runs it under `make -n`/`-q` too, so a dry run reports a BTF change
# truthfully instead of assuming one; it only refreshes this content key.
$(VMLINUX_BTF_STAMP): FORCE
	+@$(BPF_DIR)/scripts/btf-stamp.sh "$(VMLINUX_BTF)" "$@" || { \
		echo 'BPF build cannot continue without kernel BTF.' >&2; \
		echo 'Install/build a kernel with CONFIG_DEBUG_INFO_BTF=y, then reboot.' >&2; \
		exit 2; \
	}

FORCE:

$(VMLINUX_H): $(VMLINUX_BTF_STAMP) $(BPF_DIR)/scripts/generate-vmlinux-h.sh $(TOOLCHAIN_LOCK) | verify-tools
	@mkdir -p $(dir $@)
	@$(BPF_DIR)/scripts/generate-vmlinux-h.sh "$(BPFTOOL)" "$(VMLINUX_BTF)" "$@" || { \
		echo 'BPF build cannot continue without kernel BTF.' >&2; \
		echo 'Install/build a kernel with CONFIG_DEBUG_INFO_BTF=y, then reboot.' >&2; \
		exit 2; \
	}

$(OUT)/exec.bpf.o: $(BPF_DIR)/src/exec.bpf.c $(BPF_DIR)/include/harw_dod_bpf_helpers.h $(BPF_DIR)/include/harw_dod_wire_v1.h $(VMLINUX_H)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(OUT)/exit.bpf.o: $(BPF_DIR)/src/exit.bpf.c $(BPF_DIR)/include/harw_dod_bpf_helpers.h $(BPF_DIR)/include/harw_dod_wire_v1.h $(VMLINUX_H)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(OUT)/tcp_v4_connect.bpf.o: $(BPF_DIR)/src/tcp_v4_connect.bpf.c $(BPF_DIR)/include/harw_dod_bpf_helpers.h $(BPF_DIR)/include/harw_dod_wire_v1.h $(VMLINUX_H)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

$(OUT)/tcp_v6_connect.bpf.o: $(BPF_DIR)/src/tcp_v6_connect.bpf.c $(BPF_DIR)/include/harw_dod_bpf_helpers.h $(BPF_DIR)/include/harw_dod_wire_v1.h $(VMLINUX_H)
	@mkdir -p $(dir $@)
	$(CLANG) $(CFLAGS) -c $< -o $@

verify-objects: $(OBJECTS) $(BPF_DIR)/scripts/verify-object-contract.sh
	@$(BPF_DIR)/scripts/verify-object-contract.sh "$(LLVM_OBJDUMP)" "$(OUT)"

$(MANIFEST): verify-objects $(VMLINUX_BTF_STAMP) $(VMLINUX_H) $(BPF_DIR)/scripts/write-manifest.sh $(TOOLCHAIN_LOCK)
	@$(BPF_DIR)/scripts/write-manifest.sh "$(OUT)" "$(VMLINUX_BTF)" "$(TOOLCHAIN_LOCK)" "$@" "$(VMLINUX_BTF_STAMP)"

# Needs no BPF toolchain: re-runs write-manifest.sh on fixed dummy inputs and
# diffs the result against the golden manifest that harw-probe-bpf's parser
# test reads (crates/harw-probe-bpf/src/testdata/manifest.sample.toml).
check-manifest-writer:
	@$(BPF_DIR)/scripts/check-manifest-writer.sh

# Needs no BPF toolchain: drives this Makefile (a copy in a temporary
# directory) with fake clang/bpftool/llvm-objdump and checks that changing the
# fixture BTF content regenerates the stamp, vmlinux.h, all four objects and
# the manifest, that unchanged content rebuilds nothing, and that
# write-manifest.sh refuses a stale or missing stamp.
check-btf-stamp:
	@$(BPF_DIR)/scripts/check-btf-stamp.sh

clean:
	@case "$(OUT)" in "$(BPF_DIR)"/*) ;; *) echo "refusing to clean OUT outside $(BPF_DIR)" >&2; exit 2;; esac
	@rm -rf -- "$(OUT)"

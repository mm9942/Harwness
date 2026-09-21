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
MANIFEST := $(OUT)/manifest.json

OBJECTS := $(OUT)/exec.bpf.o $(OUT)/exit.bpf.o $(OUT)/tcp_v4_connect.bpf.o $(OUT)/tcp_v6_connect.bpf.o
CFLAGS := -target bpfel -O2 -g -Wall -Werror -fdebug-prefix-map=$(BPF_DIR)=/usr/src/harw-dod-bpf -I$(OUT)/include -I$(BPF_DIR)/include

.PHONY: all verify-tools verify-objects clean help

all: verify-tools $(VMLINUX_H) $(OBJECTS) verify-objects $(MANIFEST)

help:
	@echo "Standalone DoD BPF build (never invokes Cargo)."
	@echo "Required tools and exact versions are in toolchain.lock.toml."
	@echo "Set OUT=... to stage objects; VMLINUX_BTF=... selects target BTF."

verify-tools:
	@$(BPF_DIR)/scripts/verify-toolchain.sh "$(TOOLCHAIN_LOCK)" "$(CLANG)" "$(BPFTOOL)" "$(LLVM_OBJDUMP)"

$(VMLINUX_H): $(BPF_DIR)/scripts/generate-vmlinux-h.sh $(TOOLCHAIN_LOCK) | verify-tools
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

$(MANIFEST): verify-objects $(VMLINUX_H) $(BPF_DIR)/scripts/write-manifest.sh $(TOOLCHAIN_LOCK)
	@$(BPF_DIR)/scripts/write-manifest.sh "$(OUT)" "$(VMLINUX_BTF)" "$(TOOLCHAIN_LOCK)" "$@"

clean:
	@case "$(OUT)" in "$(BPF_DIR)"/*) ;; *) echo "refusing to clean OUT outside $(BPF_DIR)" >&2; exit 2;; esac
	@rm -rf -- "$(OUT)"

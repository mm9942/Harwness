#!/bin/sh
set -eu

if [ "$(id -u)" -eq 0 ]; then
	printf '%s\n' 'BPF builds must run unprivileged; prepare artifacts before using a root install.' >&2
	exit 2
fi

# The kernel-program source/toolchain is intentionally a separate build
# domain.  This hook never guesses a compiler or silently installs one.  A
# The checked-in builder is the default; an operator may override it with a
# reviewed, reproducible command when packaging on a different toolchain.
artifact_dir=${BPF_ARTIFACT_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../bpf" && pwd)}
repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)

# CLANG/BPFTOOL/LLVM_OBJDUMP, when set (the dod Makefile's build-bpf target
# resolves them via scripts/ensure-bpf-toolchain.sh --print-var so a tool
# such as bpftool in /usr/sbin is found without a PATH entry), are forwarded
# as ordinary make variable overrides to the default builder command only.
# An operator-supplied BPF_BUILD_COMMAND is used exactly as given.
tool_vars=''
[ -z "${CLANG:-}" ] || tool_vars="$tool_vars CLANG='$CLANG'"
[ -z "${BPFTOOL:-}" ] || tool_vars="$tool_vars BPFTOOL='$BPFTOOL'"
[ -z "${LLVM_OBJDUMP:-}" ] || tool_vars="$tool_vars LLVM_OBJDUMP='$LLVM_OBJDUMP'"

builder=${BPF_BUILD_COMMAND:-"make -C '$repo_dir/bpf' OUT='$artifact_dir'$tool_vars"}

mkdir -p "$artifact_dir"
sh -c "$builder"
for artifact in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o manifest.json; do
	[ -f "$artifact_dir/$artifact" ] || {
		printf 'BPF builder did not produce %s/%s\n' "$artifact_dir" "$artifact" >&2
		exit 2
	}
done

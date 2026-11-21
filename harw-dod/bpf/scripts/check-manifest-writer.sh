#!/bin/sh
# Re-runs write-manifest.sh on fixed dummy inputs and compares the result with
# the golden manifest that harw-probe-bpf's parser test reads
# (harw-dod/crates/harw-probe-bpf/src/testdata/manifest.sample.toml). Keeps the
# shell writer and the Rust parser (src/bpf_manifest.rs) from drifting apart.
#
# Usage: check-manifest-writer.sh          compare, exit 1 on a difference
#        check-manifest-writer.sh --update rewrite the golden file
#
# Needs only sh, sha256sum, awk, sed, grep, cmp, diff and mktemp: no clang,
# no bpftool, no kernel BTF. The BTF stamp is written with btf-stamp.sh
# before the dummy objects, as the Makefile orders it (write-manifest.sh
# refuses objects older than the stamp). The dummy object bytes are
# "harw-dod manifest writer fixture: <name>\n"; the Rust test rebuilds exactly
# these bytes to check the digests. `host_arch` comes from `uname -m` and is
# normalised to "x86_64" so the comparison is host-independent.
set -eu

mode=${1:-check}
case $mode in
    check | --update) ;;
    *) printf 'usage: %s [--update]\n' "$0" >&2; exit 2 ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
golden="$script_dir/../../crates/harw-probe-bpf/src/testdata/manifest.sample.toml"

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' HUP INT TERM

mkdir -p "$work/out"
printf 'harw-dod manifest writer fixture: vmlinux BTF\n' >"$work/vmlinux"
sh "$script_dir/btf-stamp.sh" "$work/vmlinux" "$work/out/vmlinux.btf.sha256"
for name in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o; do
    printf 'harw-dod manifest writer fixture: %s\n' "$name" >"$work/out/$name"
done
cat >"$work/toolchain.lock.toml" <<'EOF'
schema_version = 1
clang_version = "fixture clang version 18.1.3"
bpftool_version = "fixture bpftool v7.4.0"
llvm_objdump_version = "fixture LLVM version 19.1.1"
EOF

sh "$script_dir/write-manifest.sh" \
    "$work/out" "$work/vmlinux" "$work/toolchain.lock.toml" "$work/manifest.json" \
    "$work/out/vmlinux.btf.sha256"
sed 's/^host_arch = ".*"$/host_arch = "x86_64"/' "$work/manifest.json" >"$work/normalised.toml"

if [ "$mode" = --update ]; then
    cp "$work/normalised.toml" "$golden"
    printf 'updated %s\n' "$golden"
    exit 0
fi

if ! diff -u "$golden" "$work/normalised.toml"; then
    printf '%s\n' 'write-manifest.sh output differs from the golden manifest; fix the writer or, for an intended schema change, update the parser and run this script with --update.' >&2
    exit 1
fi
printf '%s\n' 'write-manifest.sh output matches the golden manifest.'

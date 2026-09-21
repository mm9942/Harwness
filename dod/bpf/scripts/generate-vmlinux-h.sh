#!/bin/sh
set -eu

bpftool=$1
btf=$2
output=$3
tmp="${output}.tmp"

if [ ! -r "$btf" ]; then
    echo "missing kernel BTF: $btf" >&2
    echo "running kernel must provide /sys/kernel/btf/vmlinux (CONFIG_DEBUG_INFO_BTF=y)" >&2
    echo "this kernel reports CONFIG_DEBUG_INFO_NONE=y; install or build a BTF-enabled kernel" >&2
    exit 2
fi
"$bpftool" btf dump file "$btf" format c >"$tmp"
if [ ! -s "$tmp" ]; then
    echo "bpftool produced an empty vmlinux.h from $btf" >&2
    exit 2
fi
mv -f "$tmp" "$output"

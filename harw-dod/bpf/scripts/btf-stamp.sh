#!/bin/sh
# Records the SHA-256 of the target BTF in a content stamp
# ($(OUT)/vmlinux.btf.sha256). The Makefile runs this on every invocation
# (the stamp depends on FORCE) and makes vmlinux.h, and through it all four
# objects, depend on the stamp; write-manifest.sh refuses to run when the
# stamp does not match the BTF it is about to record.
#
# The stamp is rewritten only when the digest changes, so its mtime moves
# exactly when the BTF content does: a different BTF at the same path (or a
# different VMLINUX_BTF for a reused OUT) regenerates the header, the objects
# and the manifest; an unchanged BTF rebuilds nothing.
#
# Usage: btf-stamp.sh <vmlinux BTF> <stamp>
#
# The stamp holds exactly one line: 64 lowercase hex digits.
set -eu

if [ "$#" -ne 2 ]; then
    printf 'usage: %s <vmlinux BTF> <stamp>\n' "$0" >&2
    exit 2
fi

btf=$1
stamp=$2
tmp="${stamp}.tmp"

if [ ! -r "$btf" ]; then
    echo "missing kernel BTF: $btf" >&2
    echo "running kernel must provide /sys/kernel/btf/vmlinux (CONFIG_DEBUG_INFO_BTF=y)" >&2
    echo "this kernel reports CONFIG_DEBUG_INFO_NONE=y; install or build a BTF-enabled kernel" >&2
    exit 2
fi

# Inside a pipeline sha256sum's own status is lost; an unreadable BTF shows
# up as an empty or malformed digest and fails here.
digest=$(sha256sum "$btf" | awk '{print $1}')
case $digest in
    '' | *[!0-9a-f]*)
        printf 'btf-stamp: no sha256 digest for %s\n' "$btf" >&2
        exit 2
        ;;
    *) : ;;
esac
if [ "${#digest}" -ne 64 ]; then
    printf 'btf-stamp: no sha256 digest for %s\n' "$btf" >&2
    exit 2
fi

mkdir -p "$(dirname -- "$stamp")"
printf '%s\n' "$digest" >"$tmp"
if cmp -s "$tmp" "$stamp"; then
    # Same content: keep the old stamp and its mtime, rebuild nothing.
    rm -f "$tmp"
else
    mv -f "$tmp" "$stamp"
fi

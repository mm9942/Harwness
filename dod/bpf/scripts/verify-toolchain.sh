#!/bin/sh
set -eu

lock=$1
clang=$2
bpftool=$3
objdump=$4

lock_value() {
    key=$1
    sed -n "s/^${key} = \"\(.*\)\"$/\1/p" "$lock"
}

require_exact() {
    name=$1
    expected=$2
    actual=$3
    if [ -z "$expected" ] || [ "$expected" = "UNVALIDATED" ]; then
        echo "DoD BPF toolchain lock has no validated ${name} version" >&2
        exit 2
    fi
    if [ "$actual" != "$expected" ]; then
        printf 'DoD BPF %s version differs from lock %s\n' "$name" "$lock" >&2
        printf '  expected: %s\n  actual:   %s\n' "$expected" "$actual" >&2
        printf '%s\n' 'Use the locked toolchain, or revalidate all BPF objects before updating the lock.' >&2
        exit 2
    fi
}

require_exact clang_version "$(lock_value clang_version)" "$("$clang" --version | sed -n '1p')"
require_exact bpftool_version "$(lock_value bpftool_version)" "$("$bpftool" version | sed -n '1p')"
require_exact llvm_objdump_version "$(lock_value llvm_objdump_version)" "$("$objdump" --version | sed -n '1p')"

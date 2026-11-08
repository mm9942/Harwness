#!/bin/sh
# Writes the schema-1 BPF object manifest (TOML despite the `.json` name)
# that harw-probe-bpf verifies at runtime before any load
# (harw-dod/crates/harw-probe-bpf/src/bpf_manifest.rs, PL-100 Phase 4,
# THREAT-MODEL B23).
#
# Usage: write-manifest.sh <object dir> <vmlinux BTF> <toolchain lock> <manifest>
#
# Every value is emitted on exactly one line. A digest is taken through
# command substitution, which strips sha256sum's trailing newline; a raw
# newline inside a TOML basic string makes the whole manifest unparseable and
# the probe then refuses to start on every host. The written file is checked
# for that shape before it replaces the previous manifest.
#
# The golden output for fixed dummy inputs is
# harw-dod/crates/harw-probe-bpf/src/testdata/manifest.sample.toml, which the
# probe's parser test reads; `make -C harw-dod/bpf check-manifest-writer`
# (scripts/check-manifest-writer.sh) re-runs this script and diffs against it.
set -eu

out=$1
btf=$2
lock=$3
manifest=$4
tmp="${manifest}.tmp"

fail() {
    printf 'write-manifest: %s\n' "$*" >&2
    rm -f "$tmp"
    exit 2
}

lock_value() {
    key=$1
    sed -n "s/^${key} = \"\(.*\)\"$/\1/p" "$lock"
}

# Prints the SHA-256 of a file as exactly 64 lowercase hex digits, without a
# trailing newline. Inside a pipeline sha256sum's own status is lost, so an
# unreadable file shows up as an empty or malformed digest and fails here.
sha256_of() {
    file=$1
    digest=$(sha256sum "$file" | awk '{print $1}')
    case $digest in
        '' | *[!0-9a-f]*) fail "no sha256 digest for $file" ;;
        *) : ;;
    esac
    [ "${#digest}" -eq 64 ] || fail "no sha256 digest for $file"
    printf '%s' "$digest"
}

btf_hash=$(sha256_of "$btf")
host_arch=$(uname -m)
clang_version=$(lock_value clang_version)
bpftool_version=$(lock_value bpftool_version)
[ -n "$host_arch" ] || fail 'uname -m printed nothing'
[ -n "$clang_version" ] || fail "clang_version missing in $lock"
[ -n "$bpftool_version" ] || fail "bpftool_version missing in $lock"

{
    echo "schema_version = 1"
    echo "wire_abi_version = 1"
    echo "bpf_target = \"bpfel\""
    echo "host_arch = \"$host_arch\""
    echo "clang_version = \"$clang_version\""
    echo "bpftool_version = \"$bpftool_version\""
    echo "vmlinux_btf_sha256 = \"$btf_hash\""
    for spec in \
        'exec.bpf.o|dod_sched_process_exec|sched:sched_process_exec' \
        'exit.bpf.o|dod_sched_process_exit|sched:sched_process_exit' \
        'tcp_v4_connect.bpf.o|dod_tcp_v4_connect|tcp_v4_connect' \
        'tcp_v6_connect.bpf.o|dod_tcp_v6_connect|tcp_v6_connect'; do
        name=${spec%%|*}
        rest=${spec#*|}
        program=${rest%%|*}
        attach=${rest#*|}
        hash=$(sha256_of "$out/$name")
        echo
        echo '[[objects]]'
        echo "name = \"$name\""
        echo "program = \"$program\""
        echo "attach = \"$attach\""
        echo "sha256 = \"$hash\""
        echo 'maps = ["EVENTS", "SCOPE_CGROUP_IDS", "LOSS_COUNTS", "SEQUENCE"]'
    done
} >"$tmp"

# Shape guard: every line is empty, an `[[objects]]` header or one complete
# `key = value` pair; both digest kinds are 64 lowercase hex digits on their
# own line, one BTF digest and one digest per object.
if grep -Evq '^$|^\[\[objects\]\]$|^[a-z0-9_]+ = [^ ].*$' "$tmp"; then
    fail 'generated manifest has a line that is not one complete key = value pair'
fi
grep -Eq '^vmlinux_btf_sha256 = "[0-9a-f]{64}"$' "$tmp" \
    || fail 'generated manifest has no well-formed vmlinux_btf_sha256'
[ "$(grep -Ec '^sha256 = "[0-9a-f]{64}"$' "$tmp" || true)" -eq 4 ] \
    || fail 'generated manifest does not have four well-formed object digests'

mv -f "$tmp" "$manifest"

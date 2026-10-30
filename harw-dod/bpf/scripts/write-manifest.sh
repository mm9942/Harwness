#!/bin/sh
set -eu

out=$1
btf=$2
lock=$3
manifest=$4
tmp="${manifest}.tmp"

lock_value() {
    key=$1
    sed -n "s/^${key} = \"\(.*\)\"$/\1/p" "$lock"
}

sha256sum "$btf" | awk '{print $1}' >"${tmp}.btf"
{
    echo "schema_version = 1"
    echo "wire_abi_version = 1"
    echo "bpf_target = \"bpfel\""
    echo "host_arch = \"$(uname -m)\""
    echo "clang_version = \"$(lock_value clang_version)\""
    echo "bpftool_version = \"$(lock_value bpftool_version)\""
    printf 'vmlinux_btf_sha256 = "'
    cat "${tmp}.btf"
    echo '"'
    for spec in \
        'exec.bpf.o|dod_sched_process_exec|sched:sched_process_exec' \
        'exit.bpf.o|dod_sched_process_exit|sched:sched_process_exit' \
        'tcp_v4_connect.bpf.o|dod_tcp_v4_connect|tcp_v4_connect' \
        'tcp_v6_connect.bpf.o|dod_tcp_v6_connect|tcp_v6_connect'; do
        name=${spec%%|*}
        rest=${spec#*|}
        program=${rest%%|*}
        attach=${rest#*|}
        hash=$(sha256sum "$out/$name" | awk '{print $1}')
        echo
        echo '[[objects]]'
        echo "name = \"$name\""
        echo "program = \"$program\""
        echo "attach = \"$attach\""
        echo "sha256 = \"$hash\""
        echo 'maps = ["EVENTS", "SCOPE_CGROUP_IDS", "LOSS_COUNTS", "SEQUENCE"]'
    done
} >"$tmp"
rm -f "${tmp}.btf"
mv -f "$tmp" "$manifest"

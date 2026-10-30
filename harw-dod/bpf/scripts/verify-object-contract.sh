#!/bin/sh
set -eu

objdump=$1
out=$2

check_object() {
    object=$1
    section=$2
    program=$3
    symbols=$("$objdump" -t "$object")
    sections=$("$objdump" -h "$object")

    printf '%s\n' "$sections" | grep -F -q "$section"
    printf '%s\n' "$symbols" | grep -E -q "[[:space:]]${program}$"
    for map in EVENTS SCOPE_CGROUP_IDS LOSS_COUNTS SEQUENCE; do
        printf '%s\n' "$symbols" | grep -E -q "[[:space:]]${map}$"
    done
}

check_object "$out/exec.bpf.o" "tracepoint/sched/sched_process_exec" dod_sched_process_exec
check_object "$out/exit.bpf.o" "tracepoint/sched/sched_process_exit" dod_sched_process_exit
check_object "$out/tcp_v4_connect.bpf.o" "fentry/tcp_v4_connect" dod_tcp_v4_connect
check_object "$out/tcp_v6_connect.bpf.o" "fentry/tcp_v6_connect" dod_tcp_v6_connect

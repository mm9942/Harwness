#!/bin/sh
set -u

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)

pass=0
warn=0
fail=0
check_file() {
    label=$1
    path=$2
    if [ -e "$path" ]; then
        printf 'OK   %-22s %s\n' "$label" "$path"
        pass=$((pass + 1))
    else
        printf 'MISS %-22s %s\n' "$label" "$path"
        fail=$((fail + 1))
    fi
}
check_cmd() {
    label=$1
    cmd=$2
    if command -v "$cmd" >/dev/null 2>&1; then
        printf 'OK   %-22s %s\n' "$label" "$(command -v "$cmd")"
        pass=$((pass + 1))
    else
        printf 'MISS %-22s %s\n' "$label" "$cmd"
        fail=$((fail + 1))
    fi
}
# bpftool (and the rest of the standalone C BPF toolchain) commonly installs
# to /usr/sbin, which is outside an unprivileged user's PATH; a plain
# `command -v bpftool` then falsely reports it missing. Reuse
# ensure-bpf-toolchain.sh's own read-only --check search instead of
# duplicating its PATH/`/usr/sbin`/`/sbin` lookup here. doctor stays
# read-only: --check never installs anything.
check_bpf_tool() {
    label=$1
    tool=$2
    line=$("$script_dir/ensure-bpf-toolchain.sh" --check 2>/dev/null | grep "^${tool}=")
    path=${line#"${tool}="}
    if [ -n "$path" ] && [ "$path" != 'MISSING' ]; then
        printf 'OK   %-22s %s\n' "$label" "$path"
        pass=$((pass + 1))
    else
        printf 'MISS %-22s %s\n' "$label" "$tool"
        fail=$((fail + 1))
    fi
}

printf '%s\n' 'HARW DoD doctor (read-only; no installation or package management)'
printf 'kernel=%s arch=%s\n' "$(uname -sr 2>/dev/null || printf unknown)" "$(uname -m 2>/dev/null || printf unknown)"
check_file 'cgroup-v2' /sys/fs/cgroup/cgroup.controllers
check_file 'kernel BTF' /sys/kernel/btf/vmlinux
if [ -d /sys/kernel/tracing ] || [ -d /sys/kernel/debug/tracing ]; then
    printf 'OK   %-22s tracefs directory\n' 'tracefs'
    pass=$((pass + 1))
else
    printf 'MISS %-22s /sys/kernel/tracing or debug/tracing\n' 'tracefs'
    fail=$((fail + 1))
fi
if grep -qw landlock /proc/filesystems 2>/dev/null || [ -d /sys/kernel/security/landlock ]; then
    printf 'OK   %-22s Landlock advertised\n' 'landlock'
    pass=$((pass + 1))
else
    printf 'MISS %-22s kernel Landlock support\n' 'landlock'
    fail=$((fail + 1))
fi
check_cmd systemd 'systemctl'
check_cmd systemd-sysusers 'systemd-sysusers'
check_cmd systemd-tmpfiles 'systemd-tmpfiles'
check_cmd python3 'python3'
check_cmd rustc 'rustc'
check_cmd cargo 'cargo'
check_cmd clang 'clang'
check_bpf_tool bpftool 'bpftool'
if command -v rustup >/dev/null 2>&1; then
    printf 'INFO %-22s %s\n' 'rustup toolchain' "$(rustup toolchain list 2>/dev/null | tr '\n' ';' || true)"
else
    printf 'WARN %-22s rustup not found; inspect pinned BPF toolchain manually\n' 'rustup toolchain'
    warn=$((warn + 1))
fi
if [ -r /sys/kernel/tracing/available_events ] || [ -r /sys/kernel/debug/tracing/available_events ]; then
    printf 'OK   %-22s tracepoint inventory readable\n' 'tracepoints'
    pass=$((pass + 1))
else
    printf 'WARN %-22s tracepoint inventory unreadable\n' 'tracepoints'
    warn=$((warn + 1))
fi
printf 'summary: %s ok, %s warnings, %s missing\n' "$pass" "$warn" "$fail"
if [ "$fail" -ne 0 ]; then
    printf '%s\n' 'No automatic remediation is performed. Install prerequisites explicitly, then rerun doctor.' >&2
    exit 1
fi

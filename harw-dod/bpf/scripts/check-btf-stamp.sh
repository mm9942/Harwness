#!/bin/sh
# Checks that BPF object rebuilds are bound to the content of the selected
# BTF (owner review on PR #168): reusing OUT after the BTF changed must
# regenerate vmlinux.h, all four objects and the manifest, and the manifest
# must never attest objects built for other BTF content.
#
# 1. btf-stamp.sh keeps the stamp and its mtime for unchanged BTF content and
#    rewrites it with the new digest when the content changes.
# 2. write-manifest.sh refuses a missing stamp, a stamp of other BTF content
#    and objects older than the stamp, and leaves the previous manifest in
#    place; with a matching stamp it records the current BTF digest.
# 3. A copy of the Makefile, driven with fake clang/bpftool/llvm-objdump:
#    an unchanged BTF rebuilds neither vmlinux.h nor an object (also in a
#    dry run); changed BTF content behind the same path, and a different
#    VMLINUX_BTF for the same OUT, regenerate vmlinux.h, all four objects and
#    a manifest with the new BTF digest; a missing BTF stops the build.
#
# Usage: check-btf-stamp.sh
#
# Needs sh, GNU make, sha256sum, awk, sed, grep, cmp, cp, find, touch and
# mktemp: no clang, no bpftool, no kernel BTF. Writes only below a temporary
# directory; the Makefile under test is a copy, so the checkout is untouched.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
bpf_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
objects='exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o'

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
trap 'exit 130' HUP INT TERM

fail() {
    printf 'check-btf-stamp: FAIL: %s\n' "$*" >&2
    exit 1
}

sha256_hex() {
    hash_file=$1
    sha256sum "$hash_file" | awk '{print $1}'
}

# Every file aged with `touch -t 200001010000` or `-t 200101010000` and not
# rewritten since stays older than this reference.
touch -t 200201010000 "$work/reference"

# --- 1. btf-stamp.sh --------------------------------------------------------
btf="$work/vmlinux"
stamp="$work/stamp/vmlinux.btf.sha256"
printf 'harw-dod btf stamp fixture: BTF v1\n' >"$btf"
sh "$script_dir/btf-stamp.sh" "$btf" "$stamp"
[ "$(cat "$stamp")" = "$(sha256_hex "$btf")" ] || fail 'the stamp does not hold the BTF digest'
[ "$(sed -n '$=' "$stamp")" = 1 ] || fail 'the stamp is not exactly one line'

touch -t 200001010000 "$stamp"
sh "$script_dir/btf-stamp.sh" "$btf" "$stamp"
[ "$stamp" -ot "$work/reference" ] || fail 'unchanged BTF content rewrote the stamp'

printf 'harw-dod btf stamp fixture: BTF v2\n' >"$btf"
touch -t 200001010000 "$btf"
sh "$script_dir/btf-stamp.sh" "$btf" "$stamp"
[ "$stamp" -nt "$work/reference" ] || fail 'changed BTF content did not rewrite the stamp'
[ "$(cat "$stamp")" = "$(sha256_hex "$btf")" ] || fail 'the rewritten stamp does not hold the new digest'
[ ! -e "$stamp.tmp" ] || fail 'btf-stamp.sh left a temporary file'

if sh "$script_dir/btf-stamp.sh" "$work/no-such-btf" "$stamp" 2>"$work/err"; then
    fail 'a missing BTF was stamped'
fi
grep -Fq 'missing kernel BTF' "$work/err" || fail "unexpected error for a missing BTF: $(cat "$work/err")"

# --- 2. write-manifest.sh refuses a stale or missing stamp -------------------
lock="$work/toolchain.lock.toml"
cat >"$lock" <<'EOF'
schema_version = 1
clang_version = "fixture clang version 18.1.3"
bpftool_version = "fixture bpftool v7.4.0"
llvm_objdump_version = "fixture LLVM version 19.1.1"
EOF
objs="$work/objs"
mkdir -p "$objs"
manifest="$work/manifest.json"

# Matching stamp, objects built after it: accepted.
printf 'harw-dod btf stamp fixture: BTF v1\n' >"$btf"
sh "$script_dir/btf-stamp.sh" "$btf" "$stamp"
for name in $objects; do
    printf 'harw-dod btf stamp fixture object: %s\n' "$name" >"$objs/$name"
done
sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" "$stamp" \
    || fail 'a matching stamp was refused'
grep -Fqx "vmlinux_btf_sha256 = \"$(sha256_hex "$btf")\"" "$manifest" \
    || fail 'the manifest does not record the BTF v1 digest'
cp "$manifest" "$work/manifest.v1"

# The BTF content changed, the stamp (and with it header and objects) not.
printf 'harw-dod btf stamp fixture: BTF v2\n' >"$btf"
if sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" "$stamp" 2>"$work/err"; then
    fail 'a stamp of other BTF content was accepted'
fi
grep -Fq 'does not match' "$work/err" || fail "unexpected refusal: $(cat "$work/err")"
cmp -s "$manifest" "$work/manifest.v1" || fail 'a refused run replaced the manifest'
[ ! -e "$manifest.tmp" ] || fail 'a refused run left a temporary manifest'

# No stamp at all, and the old four-argument call.
if sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" "$work/no-such-stamp" 2>"$work/err"; then
    fail 'a missing stamp was accepted'
fi
grep -Fq 'missing BTF stamp' "$work/err" || fail "unexpected refusal: $(cat "$work/err")"
status=0
sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" 2>"$work/err" || status=$?
[ "$status" -eq 2 ] || fail "the old four-argument call did not exit with status 2 (got $status)"
grep -Fq 'missing fifth argument <BTF stamp>' "$work/err" \
    || fail "the old four-argument call does not name the missing stamp: $(cat "$work/err")"
cmp -s "$manifest" "$work/manifest.v1" || fail 'a refused run replaced the manifest'

# Stamp updated for v2, but the objects predate it: stale objects.
sh "$script_dir/btf-stamp.sh" "$btf" "$stamp"
for name in $objects; do
    touch -t 200001010000 "$objs/$name"
done
if sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" "$stamp" 2>"$work/err"; then
    fail 'objects older than the stamp were attested'
fi
grep -Fq 'older than the BTF stamp' "$work/err" || fail "unexpected refusal: $(cat "$work/err")"
cmp -s "$manifest" "$work/manifest.v1" || fail 'a refused run replaced the manifest'

# Rebuilt objects: the manifest records the v2 digest.
for name in $objects; do
    printf 'harw-dod btf stamp fixture object (v2): %s\n' "$name" >"$objs/$name"
done
sh "$script_dir/write-manifest.sh" "$objs" "$btf" "$lock" "$manifest" "$stamp" \
    || fail 'rebuilt objects with a matching stamp were refused'
grep -Fqx "vmlinux_btf_sha256 = \"$(sha256_hex "$btf")\"" "$manifest" \
    || fail 'the manifest does not record the BTF v2 digest'

# --- 3. the Makefile rebuilds header, objects and manifest -------------------
tree="$work/tree"
mkdir -p "$tree"
cp -R "$bpf_dir/Makefile" "$bpf_dir/scripts" "$bpf_dir/src" "$bpf_dir/include" "$tree/"
rm -f "$tree/include/vmlinux.h"
chmod 0755 "$tree"/scripts/*.sh
# Sources, scripts and the lock older than every build output: only the
# stamp decides what is out of date.
find "$tree" -exec touch -t 200001010000 {} +
touch -t 200001010000 "$lock"

bin="$work/bin"
mkdir -p "$bin"
FAKE_CALLS="$work/calls.log"
export FAKE_CALLS

cat >"$bin/clang" <<'EOF'
#!/bin/sh
# Fake clang: the locked version line, or an "object" that records its source
# and the digest of the vmlinux.h it was built against.
set -eu
if [ "${1:-}" = --version ]; then
    printf '%s\n' 'fixture clang version 18.1.3'
    exit 0
fi
include='' src='' out=''
while [ "$#" -gt 0 ]; do
    case $1 in
        -I*) [ -n "$include" ] || include=${1#-I} ;;
        -c) shift; src=$1 ;;
        -o) shift; out=$1 ;;
        *) : ;;
    esac
    shift
done
if [ -z "$include" ] || [ -z "$src" ] || [ -z "$out" ]; then
    exit 2
fi
header=$(sha256sum "$include/vmlinux.h" | awk '{print $1}')
printf 'compile %s\n' "${src##*/}" >>"$FAKE_CALLS"
printf 'fake bpf object\nsource = %s\nvmlinux_h = %s\n' "${src##*/}" "$header" >"$out"
EOF

cat >"$bin/bpftool" <<'EOF'
#!/bin/sh
# Fake bpftool: the locked version line, or a "vmlinux.h" embedding the BTF.
set -eu
case ${1:-} in
    version)
        printf '%s\n' 'fixture bpftool v7.4.0'
        ;;
    btf)
        if [ "$#" -ne 6 ] || [ "$2" != dump ] || [ "$3" != file ] || [ "$5" != format ] || [ "$6" != c ]; then
            exit 2
        fi
        printf 'dump\n' >>"$FAKE_CALLS"
        printf '/* fake vmlinux.h */\n'
        cat -- "$4"
        ;;
    *)
        exit 2
        ;;
esac
EOF

cat >"$bin/llvm-objdump" <<'EOF'
#!/bin/sh
# Fake llvm-objdump: the locked version line, or exactly the sections and
# symbols verify-object-contract.sh expects.
set -eu
case ${1:-} in
    --version)
        printf '%s\n' 'fixture LLVM version 19.1.1'
        ;;
    -h)
        printf '%s\n' 'Sections:' \
            ' 1 tracepoint/sched/sched_process_exec' \
            ' 2 tracepoint/sched/sched_process_exit' \
            ' 3 fentry/tcp_v4_connect' \
            ' 4 fentry/tcp_v6_connect'
        ;;
    -t)
        printf '%s\n' 'SYMBOL TABLE:' \
            ' g F prog dod_sched_process_exec' \
            ' g F prog dod_sched_process_exit' \
            ' g F prog dod_tcp_v4_connect' \
            ' g F prog dod_tcp_v6_connect' \
            ' g O .maps EVENTS' \
            ' g O .maps SCOPE_CGROUP_IDS' \
            ' g O .maps LOSS_COUNTS' \
            ' g O .maps SEQUENCE'
        ;;
    *)
        exit 2
        ;;
esac
EOF
chmod 0755 "$bin/clang" "$bin/bpftool" "$bin/llvm-objdump"

# An outer make must not leak flags (e.g. -n, -s, variable overrides) into
# the make under test.
unset MAKEFLAGS GNUMAKEFLAGS MFLAGS MAKELEVEL MAKEOVERRIDES
make_cmd=${MAKE:-make}
out="$tree/out"
target_btf="$work/target-vmlinux"

# Runs the Makefile under test against $target_btf; extra arguments (e.g. -n)
# go to make.
run_make() {
    "$make_cmd" -f "$tree/Makefile" OUT="$out" VMLINUX_BTF="$target_btf" \
        TOOLCHAIN_LOCK="$lock" CLANG="$bin/clang" BPFTOOL="$bin/bpftool" \
        LLVM_OBJDUMP="$bin/llvm-objdump" "$@" >"$work/make.log" 2>&1 || {
        cat "$work/make.log" >&2
        fail "make $* failed"
    }
}

# How often a fake tool was called since $FAKE_CALLS was last emptied.
count_calls() {
    call_prefix=$1
    grep -c "^$call_prefix" "$FAKE_CALLS" || true
}

# How many compilations the last dry run would have run.
dry_compiles() {
    grep -c -- ' -c [^ ]*/src/[a-z0-9_]*\.bpf\.c -o ' "$work/make.log" || true
}

# Checks stamp, header, objects and manifest against the current BTF.
expect_built_from() {
    label=$1
    header_marker=$2
    btf_hash=$(sha256_hex "$target_btf")
    [ "$(cat "$out/vmlinux.btf.sha256")" = "$btf_hash" ] || fail "$label: the stamp does not hold the BTF digest"
    grep -Fqx "$header_marker" "$out/include/vmlinux.h" || fail "$label: vmlinux.h was not generated from this BTF"
    header_hash=$(sha256_hex "$out/include/vmlinux.h")
    grep -Fqx "vmlinux_btf_sha256 = \"$btf_hash\"" "$out/manifest.json" \
        || fail "$label: the manifest does not record the BTF digest"
    for name in $objects; do
        grep -Fqx "vmlinux_h = $header_hash" "$out/$name" || fail "$label: $name was not built from this vmlinux.h"
        grep -Fqx "sha256 = \"$(sha256_hex "$out/$name")\"" "$out/manifest.json" \
            || fail "$label: the manifest does not bind $name"
    done
}

# Ages every build output to 2001: newer than the sources (2000), older than
# the reference (2002) and than anything rewritten later.
age_outputs() {
    touch -t 200101010000 "$out/vmlinux.btf.sha256" "$out/include/vmlinux.h" "$out/manifest.json"
    for name in $objects; do
        touch -t 200101010000 "$out/$name"
    done
}

# First build.
printf 'harw-dod btf stamp fixture: target BTF v1\n' >"$target_btf"
: >"$FAKE_CALLS"
run_make
expect_built_from 'first build' 'harw-dod btf stamp fixture: target BTF v1'
[ "$(count_calls dump)" -eq 1 ] || fail 'the first build did not generate vmlinux.h exactly once'
[ "$(count_calls compile)" -eq 4 ] || fail 'the first build did not compile all four objects'
cp "$out/manifest.json" "$work/make-manifest.v1"
age_outputs

# Unchanged BTF content (its own mtime moves): nothing is rebuilt.
touch "$target_btf"
: >"$FAKE_CALLS"
run_make -n
[ "$(dry_compiles)" -eq 0 ] || fail 'a dry run without a BTF change lists compilations'
run_make
[ "$(count_calls dump)" -eq 0 ] || fail 'an unchanged BTF regenerated vmlinux.h'
[ "$(count_calls compile)" -eq 0 ] || fail 'an unchanged BTF recompiled objects'
[ "$out/vmlinux.btf.sha256" -ot "$work/reference" ] || fail 'an unchanged BTF rewrote the stamp'
[ "$out/include/vmlinux.h" -ot "$work/reference" ] || fail 'an unchanged BTF rewrote vmlinux.h'
cmp -s "$out/manifest.json" "$work/make-manifest.v1" || fail 'an unchanged BTF changed the manifest'

# Changed content behind the same path (its mtime even moves backwards).
printf 'harw-dod btf stamp fixture: target BTF v2\n' >"$target_btf"
touch -t 200001010000 "$target_btf"
run_make -n
[ "$(dry_compiles)" -eq 4 ] || fail 'a dry run after a BTF change does not list all four compilations'
: >"$FAKE_CALLS"
run_make
expect_built_from 'changed BTF' 'harw-dod btf stamp fixture: target BTF v2'
[ "$(count_calls dump)" -eq 1 ] || fail 'a BTF change did not regenerate vmlinux.h'
[ "$(count_calls compile)" -eq 4 ] || fail 'a BTF change did not recompile all four objects'
if cmp -s "$out/manifest.json" "$work/make-manifest.v1"; then
    fail 'the manifest did not change with the BTF'
fi
age_outputs

# Another VMLINUX_BTF for the same OUT.
target_btf="$work/other-vmlinux"
printf 'harw-dod btf stamp fixture: other target BTF\n' >"$target_btf"
touch -t 200001010000 "$target_btf"
: >"$FAKE_CALLS"
run_make
expect_built_from 'other BTF' 'harw-dod btf stamp fixture: other target BTF'
[ "$(count_calls compile)" -eq 4 ] || fail 'another VMLINUX_BTF did not recompile all four objects'

# A missing BTF stops the build before anything is regenerated.
cp "$out/manifest.json" "$work/make-manifest.other"
target_btf="$work/no-such-btf"
if "$make_cmd" -f "$tree/Makefile" OUT="$out" VMLINUX_BTF="$target_btf" \
    TOOLCHAIN_LOCK="$lock" CLANG="$bin/clang" BPFTOOL="$bin/bpftool" \
    LLVM_OBJDUMP="$bin/llvm-objdump" >"$work/make.log" 2>&1; then
    fail 'make succeeded without a BTF'
fi
grep -Fq 'missing kernel BTF' "$work/make.log" || fail "unexpected make failure: $(cat "$work/make.log")"
cmp -s "$out/manifest.json" "$work/make-manifest.other" || fail 'a build without BTF changed the manifest'

printf '%s\n' 'BTF stamp: header, objects and manifest follow the BTF content.'

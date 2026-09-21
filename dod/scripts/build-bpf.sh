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

builder=${BPF_BUILD_COMMAND:-"make -C '$repo_dir/bpf' OUT='$artifact_dir'"}

mkdir -p "$artifact_dir"
sh -c "$builder"
for artifact in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o manifest.json; do
	[ -f "$artifact_dir/$artifact" ] || {
		printf 'BPF builder did not produce %s/%s\n' "$artifact_dir" "$artifact" >&2
		exit 2
	}
done

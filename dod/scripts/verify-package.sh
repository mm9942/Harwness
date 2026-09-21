#!/bin/sh
set -eu

packaging=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)
manifest=$packaging/manifest

grep -q '^systemd/harw-dod.target$' "$manifest"
grep -q '^systemd/harw-dod-sentinel.service$' "$manifest"
grep -q '^systemd/harw-dod-bpf.service$' "$manifest"
grep -q '^systemd/harw-dod-warden.service$' "$manifest"
grep -q '^systemd/harw-dod-warden.socket$' "$manifest"
grep -q '^bpf/manifest.json$' "$manifest"

if grep -Eq '^WantedBy=|^RequiredBy=' "$packaging/systemd/harw-dod-warden.service" "$packaging/systemd/harw-dod-warden.socket"; then
	printf '%s\n' 'Warden units must not contain an [Install] activation edge' >&2
	exit 1
fi
if grep -Eq '^(Wants|Requires|After|Before|ExecStart)=.*warden' "$packaging/systemd/harw-dod.target"; then
	printf '%s\n' 'Observation target must not pull in Warden' >&2
	exit 1
fi
printf '%s\n' 'Static DoD package contract checks passed.'

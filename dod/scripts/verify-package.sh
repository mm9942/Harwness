#!/bin/sh
set -eu

packaging=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)
manifest=$packaging/manifest
# Canonical unit source (Crypto Masterplan v2 §22, H10).
units=$(CDPATH= cd -- "$(dirname "$0")/../../deploy/systemd" && pwd)

grep -q '^systemd/harw-dod.target$' "$manifest"
grep -q '^systemd/harw-sentinel.service$' "$manifest"
grep -q '^systemd/harw-probe-bpf.service$' "$manifest"
grep -q '^systemd/harw-probe-fs.service$' "$manifest"
grep -q '^systemd/harw-warden.service$' "$manifest"
grep -q '^systemd/harw-warden.socket$' "$manifest"
grep -q '^sysusers/harw.conf$' "$manifest"
grep -q '^tmpfiles/harw.conf$' "$manifest"
grep -q '^bpf/manifest.json$' "$manifest"

if grep -Eq '^WantedBy=|^RequiredBy=' "$units/harw-warden.service" "$units/harw-warden.socket"; then
	printf '%s\n' 'Warden units must not contain an [Install] activation edge' >&2
	exit 1
fi
if grep -Eq '^(Wants|Requires|After|Before|ExecStart)=.*warden' "$units/harw-dod.target"; then
	printf '%s\n' 'Observation target must not pull in Warden' >&2
	exit 1
fi
printf '%s\n' 'Static DoD package contract checks passed.'

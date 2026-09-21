#!/bin/sh
set -eu

packaging=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)

[ -f "$root/Makefile" ]
[ -x "$root/scripts/install.sh" ]
[ -x "$root/scripts/check-config.sh" ]
[ -x "$root/scripts/doctor.sh" ]
[ -x "$root/scripts/service.sh" ]
[ -f "$packaging/manifest" ]

# This target is intentionally static: it must remain safe on a checkout with
# no release artifacts and must not invoke Cargo or any compiler.
if grep -nE '(^|[;&[:space:]])cargo[[:space:]]+(build|check|fmt|clippy|test|run|install|metadata)([[:space:]]|$)' "$root/scripts"/*.sh; then
	printf '%s\n' 'Packaging helpers must not invoke cargo.' >&2
	exit 1
fi
if grep -nE '^ExecStart=.*warden|^WantedBy=.*warden|^RequiredBy=.*warden' "$packaging/systemd/harw-dod.target"; then
	printf '%s\n' 'Observation target unexpectedly activates Warden.' >&2
	exit 1
fi
if grep -q '^\[Install\]' "$packaging/systemd/harw-dod-warden.service" "$packaging/systemd/harw-dod-warden.socket"; then
	printf '%s\n' 'Warden units unexpectedly contain [Install].' >&2
	exit 1
fi
printf '%s\n' 'Static staging contract checks passed (no Cargo/build/install side effects).'

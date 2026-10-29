# Harw-DoD system installation

Harw-DoD (Detect · Orient · Defend) is the suite name; crates, binaries,
paths, groups and unit files keep their `harw-dod`/`harw-*` names.
See [`docs/setup/dod.md`](../docs/setup/dod.md) for a full walkthrough
(purpose, components, privilege model, and step-by-step installation). This
file documents the packaging Makefile itself.

This directory owns the system-wide DoD package. The default prefix is
`/usr/local`; `PREFIX=/usr` is suitable for a distribution package. The
package never installs into a user's HARW home tree.

The package contains root-owned service definitions and program/ELF files,
one system identity per binary (`harw-sentinel`, `harw-probe-fs`,
`harw-probe-bpf`, `harw-warden`), and FHS directories:

* programs: `/usr/local/libexec/harw-dod`
* eBPF objects and manifest: `/usr/local/lib/harw-dod/bpf`
* configuration: `/etc/harw-dod`
* state and telemetry: `/var/lib/harw-dod`, `/var/log/harw-dod`
* sentinel exports for harw: `/var/lib/harw-dod/export` (`findings.jsonl`,
  `health.json`; `2750 harw-sentinel:harw-dod-readers`)
* runtime sockets: `/run/harw/sentinel.sock`, `/run/harw/warden.sock`

The systemd units, the `sysusers.d` accounts and the `tmpfiles.d`
directories are **not** kept here: `install.sh` installs them from the one
canonical tree `../deploy/` (Crypto Masterplan v2 §22, H10), the same files
`harw install --print-systemd` prints. `packaging/` holds only the install
manifest and the example configuration.

`make -C dod install` installs files and performs no start or enable action.
It preserves an existing `/etc/harw-dod/config.toml`; only
`config.toml.example` is installed or replaced (mode `0640`).
`DESTDIR=/tmp/stage make -C dod stage` is pure staging: it does not create
accounts, call systemd, or touch host runtime state. A host install requires
root and calls only the already-installed `systemd-sysusers`,
`systemd-tmpfiles`, and `systemctl daemon-reload` helpers; it never invokes
Cargo or installs prerequisites. The sentinel and the eBPF probe receive
read-only membership in the dedicated `harw-dod-config` group; the separate
`harw-ipc` group is reserved for the sentinel socket path,
`harw-warden-clients` (empty by default) for the Warden socket, and
`harw-dod-readers` (empty by default) for reading the sentinel exports — add
a user with `usermod -aG harw-dod-readers <user>`. `harw-security-hub` is not
a member either; it reads `findings.jsonl` only after the drop-in described
in `docs/setup/dod.md` ("Letting `harw-security-hub` read the findings").

Before enabling observation, copy the example to `/etc/harw-dod/config.toml`
and add exactly one `active_profile`. `make -C dod enable` runs the explicit
configuration/profile check first and refuses to enable without that choice.
`scope = "host"` is never an implicit fallback.

The Warden service and its SOCK_SEQPACKET socket are shipped for a later
enforcement delivery, but have no `[Install]` section, no target dependency,
and an additional `/etc/harw-dod/warden.enable` condition. Normal install,
enable, restart, and observation target operations do not start or enable
either unit.

## Current binary integration boundary

The units in `../deploy/systemd/` pass the configuration explicitly:
`harw-sentinel --config /etc/harw-dod/config.toml --home <STATEDIR> --socket
/run/harw/sentinel.sock --findings-export <STATEDIR>/export/findings.jsonl
--health-export <STATEDIR>/export/health.json` and `harw-probe-bpf --config
/etc/harw-dod/config.toml`. With `--config`, a missing, untrusted or invalid
configuration is fail closed for both binaries, so enable only after
`/etc/harw-dod/config.toml` exists with an `active_profile` (`make -C dod
enable` checks this first). There are no separate `--state-dir
--telemetry-dir --runtime-dir` flags; `--home` is the sentinel's state
directory, and packaging never substitutes a personal home path.

The BPF artefacts are build outputs and are never checked in: `make build-bpf`
compiles `bpf/src/*.bpf.c` with the pinned toolchain and writes
`bpf/exec.bpf.o`, `bpf/exit.bpf.o`, `bpf/tcp_v4_connect.bpf.o`,
`bpf/tcp_v6_connect.bpf.o`, `bpf/manifest.json` and the generated
`bpf/include/vmlinux.h` (all ignored by `bpf/.gitignore`). `install` refuses to
run until they exist. The builder may override `BPF_ARTIFACT_DIR` or rename the
files together with the packaging variables when its pinned build layout is
finalized.

## Targets

`help`, `doctor`, `check-config`, `ensure-bpf-toolchain`, `build-bpf`,
`build`, `check`, `fmt`, `clippy`, `test`, `verify`, `install`, `stage`,
`enable`, `disable`, `restart`, `status`, `logs`, `smoke`, `uninstall`, and
`stage-test` are provided by the Makefile. `doctor` and `check-config` are
read-only. `smoke` is an explicit operator action and is never part of
ordinary tests. `stage-test` checks the packaging contract without building
or installing anything. Build/check Cargo recipes are explicit and guarded;
the installer never invokes them.

`ensure-bpf-toolchain` is the first step of both `install` and `build`
(via `build-bpf`). It locates `clang`, `bpftool`, and `llvm-objdump` —
searching `PATH`, then `/usr/sbin` and `/sbin`, since a package such as
Debian's `bpftool` commonly installs outside an unprivileged user's `PATH`
— and passes the resolved absolute paths to the standalone BPF build in
`bpf/`. Installing dependencies is explicitly this Makefile's
responsibility, not the BPF build domain's: when a tool is genuinely
missing, this target runs `apt-get install` for it, escalating privileges
only for that install and only when `PRIV_ESC` (root or `sudo`) is
available, `apt-get` exists, and `DESTDIR` is not set; otherwise it aborts
with the exact command to run by hand. It never checks tool *versions* —
that stays `bpf/scripts/verify-toolchain.sh`'s fail-closed job against
`bpf/toolchain.lock.toml`, unchanged.

`make -C dod uninstall` removes only files listed in `packaging/manifest`.
It leaves `config.toml`, state, logs, runtime data, and system accounts in
place so upgrades and rollbacks cannot silently discard operator evidence.

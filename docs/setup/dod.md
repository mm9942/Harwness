# DoD (Detect · Orient · Defend)

> Status: partially implemented · Last reviewed: 2026-09-24

DoD is Harwness's on-device observation-and-defense subsystem: it watches
what is happening on the host around agent activity (process starts,
network connections, file writes, authentication, resource pressure, …)
and — behind an explicit, disabled-by-default enforcement path — can act on
what it observes. It is a **separate, privileged, opt-in install**: a
set of crates under `dod/` (members of the root Cargo workspace since
PL-60, built with `-p` selections — one `Cargo.lock`, no nested workspace)
with its own Makefile, its own system accounts, and its own FHS layout. Nothing in `make install`/`make service`
(the ordinary Harwness install described in
[`docs/setup/install.md`](install.md)) touches DoD, and nothing in DoD
requires it.

This page explains what DoD is made of, how it is installed, and — as
honestly as the code allows — what currently works and what is still a
contract rather than a live capability.

## Purpose

An agent harness that can execute arbitrary commands needs a way to answer
"what did this actually do to the machine, and can I stop it". DoD is that
answer, split into two halves:

- **Observation** — a set of *sensors*, each reading exactly one host
  source (a `/proc` file, a sysfs directory, cgroup v2 accounting, netlink
  audit records, or a kernel event via eBPF) through exactly one
  capability, and turning it into a typed reading or security event.
- **Escalation and enforcement** — turning a sensor's findings into rule
  evaluations, and, if a signed authorization says so, into an enforcement
  action (freeze a cgroup, cut network egress, kill a process tree). This
  half is shipped but **disabled** in every default install; see
  [Warden](#the-warden-shipped-but-disabled).

## Components

The DoD domain (`dod/crates/`, part of the root workspace) has around 30
crates. Grouped by role, with what each
one is responsible for:

**Access and data vocabulary** (shared foundations, not sensors themselves)
- `harw-dod-cap` — the capability/read-scope vocabulary every sensor is
  built against (`Capability`, `ReadScope`, `SensorHandle`, …).
- `harw-dod-signals` — the reading/event vocabulary (`Sensor` trait,
  `SensorReading`, `SecurityEvent`, `HostSample`, …).
- `harw-dod-readfs` — the single filesystem seam unprivileged sensors read
  through; no sensor opens a path itself.
- `harw-dod-fixtures` — the shared fixture/test harness all sensor crates
  inherit from.

**Sensors** (each owns exactly one host source and one capability)
- `harw-dod-cpu` (`/proc/stat`), `harw-dod-memory` (`/proc/meminfo`),
  `harw-dod-thermal` (`/sys/class/thermal`), `harw-dod-gpu`
  (`/sys/class/drm`), `harw-dod-blockio` (block device stats in sysfs),
  `harw-dod-netcounters` (`/proc/net/dev`), `harw-dod-cgroup` (cgroup v2
  accounting), `harw-dod-listener` (open listening sockets with cgroup
  attribution), `harw-dod-workspace` (structural drift in this repo's own
  Cargo dependency graph), `harw-dod-scanreport` (reads third-party
  scanner reports; never invokes a scanner itself), `harw-dod-authlog`
  (login events via `auid`, backed by `harw-dod-netlink`),
  `harw-dod-fsmon` (fanotify-based file-write watcher, library half of
  `harw-probe-fs`), `harw-dod-procmon` (process start/exit via eBPF,
  library half of `harw-probe-bpf`).

**eBPF and translation infrastructure**
- `harw-dod-bpf` — the eBPF loading layer behind a trait, shared by
  `-procmon` and `-flow`.
- `harw-dod-flow` — translates raw eBPF connection events into
  `SecurityEvent`s (TCP connect observation).
- `harw-dod-netlink` — the netlink transport `-authlog` reads audit
  records through.

**Evaluation**
- `harw-dod-rules` — turns observations into `Finding`s: rules, baselines,
  severity.
- `harw-dod-sentinel` — the unprivileged collection point: holds sensors,
  polls them, tracks health, buffers what they produce. Contains no
  source-parsing logic itself.

**Escalation and enforcement**
- `harw-dod-escalate` — the one place a `Finding` becomes an authorized
  action.
- `harw-dod-warden-proto` — the wire protocol between escalation and the
  enforcer.
- `harw-dod-warden` — the enforcer (library half): freeze/release cgroups,
  cut network egress, kill process trees.
- `harw-dod-netpolicy` — a pure data type describing which egress rules
  should apply for a scope; applies nothing itself.

**Facade and running processes**
- `harw-dod` — the curated facade the rest of the workspace uses; a
  consumer that wants "observe the host" or "evaluate a finding" depends
  on this, not on individual sensor crates.
- `harw-sentinel` — the unprivileged binary: runs the sensors and the
  collection loop.
- `harw-probe-fs` — the privileged (`CAP_SYS_ADMIN`) fanotify probe binary;
  push-only, no receive path.
- `harw-probe-bpf` — the privileged (`CAP_BPF`, `CAP_PERFMON`) eBPF probe
  binary; push-only, no receive path.
- `harw-warden` — the enforcer binary, listening on a systemd-activated
  `SOCK_SEQPACKET` socket.

## Data flow

```
   unprivileged                      privileged (per-probe capability)
 ┌───────────────┐   /proc,/sys,    ┌───────────────┐
 │  harw-sentinel │◄── cgroup v2 ────│                │
 │ (poll loop,    │   netlink        │                │
 │  no source     │                  │                │
 │  parsing)      │◄── sentinel.sock ┤ harw-probe-fs  │  fanotify (CAP_SYS_ADMIN)
 │                │                  │ harw-probe-bpf │  eBPF (CAP_BPF, CAP_PERFMON)
 └───────┬────────┘                  └───────────────┘
         │ readings / SecurityEvents
         ▼
 ┌───────────────┐
 │ harw-dod-rules │  findings, baselines, severity
 └───────┬────────┘
         │ Finding
         ▼
 ┌────────────────┐   signed authorization required
 │ harw-dod-escalate│─────────────────────────────┐
 └────────────────┘                               │
                                                    ▼
                                          ┌───────────────┐
                                          │  harw-warden   │  freeze/release cgroup,
                                          │ (own user,     │  cut egress, kill tree
                                          │  socket-       │
                                          │  activated,    │
                                          │  DISABLED by   │
                                          │  default)      │
                                          └───────────────┘
```

`harw-sentinel` is the single unprivileged collection point. The two
privileged probes (`harw-probe-fs`, `harw-probe-bpf`) are **push-only** —
by design, a process holding `CAP_SYS_ADMIN` or `CAP_BPF`/`CAP_PERFMON`
never *accepts* connections, it only sends to the sentinel's socket. Each
probe holds exactly the one capability class its job requires and nothing
more.

## Privilege model

- **System accounts** (`deploy/sysusers.d/harw.conf`, installed via
  `systemd-sysusers`, no login shell, no home directory), one per binary:
  `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden`. The
  Warden never runs as `root`.
- **Groups:** `harw-dod-config` gives the sentinel and the eBPF probe
  read-only access to `/etc/harw-dod`; `harw-ipc` lets the sentinel create,
  and the two probes connect to, `/run/harw/sentinel.sock`;
  `harw-warden-clients` (empty by default) may connect to
  `/run/harw/warden.sock` — add the escalation client explicitly.
- **Capabilities, per binary, and nothing beyond them:**
  `CapabilityBoundingSet`/`AmbientCapabilities` are empty for
  `harw-sentinel` (no elevated capability at all); `CAP_BPF CAP_PERFMON`
  for `harw-probe-bpf` (the loader refuses to attach tracepoint/FEntry
  programs without both); `CAP_SYS_ADMIN` for `harw-probe-fs` (the only
  capability that permits `fanotify_mark` with `FAN_MARK_FILESYSTEM` and
  the `FAN_UNLIMITED_*` flags it needs); `CAP_DAC_OVERRIDE CAP_NET_ADMIN`
  for `harw-warden` (writing the root-owned `cgroup.freeze`/`cgroup.kill`
  files, and running `nft` for network isolation).
- **Root-owned files and FHS directories** (default prefix `/usr/local`;
  `PREFIX=/usr` for a distribution package):
  - programs: `/usr/local/libexec/harw-dod`
  - eBPF objects + manifest: `/usr/local/lib/harw-dod/bpf`
  - configuration: `/etc/harw-dod`
  - state/telemetry: `/var/lib/harw-dod`, `/var/log/harw-dod`
  - runtime sockets: `/run/harw/sentinel.sock`, `/run/harw/warden.sock`
    (`/run/harw` is created by `deploy/tmpfiles.d/harw.conf`; the
    infrastructure daemons use `/run/harw/infra/`)
- Each systemd unit additionally sets `NoNewPrivileges=yes`,
  `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`,
  `SystemCallFilter=@system-service` (plus the few syscalls the binary
  needs, e.g. `bpf`, `perf_event_open`, `fanotify_*`, Landlock), and
  scopes its writable paths to exactly what that process needs
  (`ReadWritePaths`).
- The package **never** installs into a user's `~/.harw` tree, and never
  runs as, or touches files owned by, a regular user account.

## How it integrates with harw

The main Harwness config carries a `[dod]` section
(`harw-config/src/dod_toml.rs`) that is escalation *policy*, evaluated on
the agent side — separate from DoD's own `/etc/harw-dod/config.toml`,
which is the system-level sensor/profile configuration DoD itself reads.
The `[dod]` section's defaults are deliberately restrictive
("secure/off"):

- `kill_requires_human` — must be `true`; not negotiable, rejected in
  every configuration layer if set to `false` (per the harness's
  automation decision: freeze/release may be automatic after a signed
  proof, kill never is without a human).
- `auto_freeze` — whether freeze/release transitions happen automatically
  after a signed proof (default `true`).
- `proof_key_dir` — where the keys for `SignedAuthorization` verification
  live; never inherited from an untrusted config layer.
- `allowed_cgroup_prefixes` — which cgroup path prefixes the escalation
  chain may address at all; an untrusted layer may only narrow this list,
  never widen it.

Enforcement itself lives entirely in the escalation/warden binaries; this
config section only describes the policy they must honor.

## Installation

DoD is built and installed from `dod/`, or via the root `Makefile`'s
delegation targets (`make dod-build`, `sudo make dod-install`, `make
dod-enable`, `make dod-uninstall` — thin wrappers around `make -C dod
<target>`; run `make -C dod help` or `dod/README.md` for the complete
target list).

1. **Prerequisites.** `make -C dod doctor` gives a read-only report of what
   is present. Building the eBPF artifacts needs `clang`, `bpftool`, and
   `llvm-objdump` at versions pinned in `dod/bpf/toolchain.lock.toml`
   (checked fail-closed by `dod/bpf/scripts/verify-toolchain.sh`), plus a
   kernel exposing BTF (`/sys/kernel/btf/vmlinux`). `make -C dod
   ensure-bpf-toolchain` locates these tools (searching `PATH`, then
   `/usr/sbin`, `/sbin`) and, only when one is genuinely missing and
   `apt-get`/root privileges are available, installs it — this is the
   first step of both `build` and `install`.

2. **Build.**
   ```sh
   make dod-build         # from the repo root, or: make -C dod build
   ```
   Builds the release host binaries (`harw-sentinel`, `harw-probe-bpf`,
   `harw-probe-fs`, `harw-warden`) and the pinned eBPF artifacts
   (`bpf/exec.bpf.o`, `bpf/exit.bpf.o`, `bpf/tcp_v4_connect.bpf.o`,
   `bpf/tcp_v6_connect.bpf.o`, `bpf/manifest.json` — build outputs, never
   checked in). Must run unprivileged; it refuses to run as root.

3. **Install.**
   ```sh
   sudo make dod-install   # from the repo root, or: sudo make -C dod install
   ```
   Rebuilds (to avoid installing stale artifacts), then installs files and
   creates the system accounts (`systemd-sysusers`), runtime
   directories (`systemd-tmpfiles`), and systemd units — all taken from
   the repository's `deploy/` tree (see
   [below](#systemd-units-and-the-deploy-tree)) — and reloads the
   systemd daemon. Unit files of the old layout (`harw-dod-sentinel.service`,
   `harw-dod-bpf.service`, `harw-dod-warden.*`, `harw-dod.conf`) are
   removed. It performs **no enable/start action** by itself beyond
   that — see step 5. For package building, use `DESTDIR=/path/to/stage
   make -C dod stage` instead: pure staging, no accounts, no systemd calls,
   no host state touched.

4. **Configure.** Copy the shipped example and choose exactly one profile:
   ```sh
   sudo cp /etc/harw-dod/config.toml.example /etc/harw-dod/config.toml
   sudo editor /etc/harw-dod/config.toml
   ```
   Add exactly one `active_profile = "…"` under `[profiles.<name>]`.
   `scope = "host"` (observe the whole machine) is never selected
   implicitly — you must write it explicitly if that's what you want.
   Shipped example profiles (`dod/packaging/config.example.toml`):
   - `selected-services` — `scope = "cgroups"` over named systemd service
     cgroups.
   - `user-workloads` — `scope = "cgroups"` over `/user.slice`.
   - `host` — `scope = "host"`, the entire machine.
   All three default to `sensors = ["exec", "tcp-connect"]` and an empty
   egress allowlist. `make -C dod install` preserves an existing
   `/etc/harw-dod/config.toml`; only `config.toml.example` is
   installed/replaced.

5. **Enable.**
   ```sh
   make dod-enable         # from the repo root, or: make -C dod enable
   ```
   Runs the explicit configuration/profile validation first and refuses to
   enable without a chosen `active_profile`. This starts and enables
   `harw-sentinel.service` and `harw-probe-bpf.service` under
   `harw-dod.target` — observation only, never the Warden.
   `harw-probe-fs.service` is installed but not part of the target: set its
   `--scope-root` in a drop-in, then `systemctl enable --now
   harw-probe-fs.service`.

### The Warden — shipped but disabled

`harw-warden.service` and its `SOCK_SEQPACKET` activation socket are
installed by `make dod-install` but have **no `[Install]` section**, no
dependency from `harw-dod.target`, and an additional
`ConditionPathExists=/etc/harw-dod/warden.enable` gate. Normal install,
enable, restart, and observation-target operations never start or enable
either unit. To opt into enforcement explicitly:

```sh
sudo make -C dod enable-warden
```

This creates `/etc/harw-dod/warden.enable`, reloads systemd, and enables
the socket. Even then, the actual authorization to freeze/kill/cut network
still runs through the escalation policy in harw's own `[dod]` config (see
[above](#how-it-integrates-with-harw)) — `kill_requires_human` cannot be
turned off.

## Current status and limitations (honest)

- **The observation half (sentinel + probes) is what's live.** The
  escalation/warden chain is implemented as a library and a socket-
  activated binary, but is disabled by every default install path, as
  described above.
- **Binary integration boundary.** Per `dod/README.md`, the shipped
  systemd unit for `harw-sentinel` currently invokes it with the legacy
  `--home`/socket-only CLI (`harw-sentinel --home <state-dir> --socket
  <path> --log info`) rather than the planned system contract
  (`--config --state-dir --telemetry-dir --runtime-dir`). `harw-probe-bpf`
  already takes `--config` on the shipped unit. `harw-sentinel`'s own
  module documentation confirms the CLI has no `--config` flag yet: the
  system configuration path is passed as `None` from `main`, and DoD falls
  back to a fixed default path
  (`harw_dod_config::SYSTEM_CONFIG_PATH`) with built-in defaults if
  nothing is found there, logging a warning rather than failing closed on
  a missing file (it *does* fail closed — refuses to start — on a
  present-but-untrusted or invalid configuration: wrong owner, group/world
  writable, a symlink, or a parse/schema error). In short: **the systemd
  units are an installable, correctly-scoped contract, not yet a claim
  that `--config`-driven service configuration is fully wired through
  every binary.** Verify current behavior against
  `dod/crates/harw-sentinel/src/main.rs` and
  `dod/crates/harw-probe-bpf/src/main.rs` before relying on a specific CLI
  surface.
- **eBPF artifacts are never checked into the repository**; they must be
  built locally (or by your packaging pipeline) with the pinned toolchain
  before `install` will proceed — `install` refuses to run until they
  exist.
- **`stage-test`** (`make -C dod stage-test`) checks the packaging
  contract statically, without building or installing anything — useful
  for CI that can't run eBPF builds.

## systemd units and the deploy tree

`deploy/` at the repository root is the **single** source of every Harwness
system unit (Crypto Masterplan v2 §22, H10); there is no second copy under
`dod/`:

| Path | Content |
|---|---|
| `deploy/systemd/harw-dod.target`, `harw-sentinel.service`, `harw-probe-bpf.service`, `harw-probe-fs.service`, `harw-warden.service`, `harw-warden.socket` | DoD (installed by `make -C dod install`) |
| `deploy/systemd/harw-infra.target`, `harw-control.{socket,service}`, `harw-auth-hub.{socket,service}`, `harw-netsec.{socket,service}`, `harw-security-hub.{socket,service}` | infrastructure daemons (not installed by the DoD package) |
| `deploy/sysusers.d/harw.conf` | all system accounts and groups |
| `deploy/tmpfiles.d/harw.conf` | `/run/harw`, `/run/harw/infra`, DoD state/log directories |

The same files are compiled into `harw` byte-identically (`harw-install`
parity tests fail on any drift). To inspect exactly what is shipped, with
the default install paths substituted:

```sh
harw install --print-systemd                      # every unit
harw install --print-systemd harw-warden.socket   # one unit
```

`dod/scripts/install.sh` substitutes the `@LIBEXECDIR@`, `@BPFDIR@`,
`@SYSCONFDIR@`, `@STATEDIR@` and `@LOGDIR@` placeholders from its own
variables and refuses to install a unit with an unresolved placeholder.
Runtime sockets live at fixed paths: `/run/harw/` for DoD,
`/run/harw/infra/{control,secure,network,security}.sock` for the
infrastructure daemons. Every infrastructure daemon is socket-activated
(`--systemd-socket`): `harw web` behind `harw-control.{socket,service}`,
`harw-auth-hub`, `harw-netsec` and `harw-security-hub`. Only systemd
creates sockets in `/run/harw/infra`, so the directory is `0755 root:root`;
access is set per socket unit (mode `0660` plus a client group such as
`harw-control-clients`).

## Uninstalling

```sh
sudo make dod-uninstall   # from the repo root, or: sudo make -C dod uninstall
```

Removes only the files listed in `dod/packaging/manifest` (binaries, eBPF
objects, systemd units, sysusers/tmpfiles config snippets, and the
`config.toml.example`). It deliberately leaves `/etc/harw-dod/config.toml`,
state, logs, runtime data, and the system accounts in place, so an upgrade or a rollback never silently discards
operator evidence or configuration. Remove those by hand if you want a
full teardown:

```sh
sudo systemctl disable --now harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service
sudo rm -rf /etc/harw-dod /var/lib/harw-dod /var/log/harw-dod
sudo rm -f /run/harw/sentinel.sock /run/harw/warden.sock
for u in harw-sentinel harw-probe-fs harw-probe-bpf harw-warden; do sudo userdel "$u"; done
sudo groupdel harw-dod-config; sudo groupdel harw-ipc; sudo groupdel harw-warden-clients
```

## Troubleshooting

- `make -C dod doctor` — read-only report of what prerequisites are
  present/missing.
- `make -C dod check-config` — validates `/etc/harw-dod/config.toml`
  (or `CONFIG=<path>`) and prints the selected profile without touching
  any service.
- `make -C dod status` / `make -C dod logs` — inspect the observation
  services (`CONFIG=<path>` to check a non-default config file first).
- `install refuses to run` — most commonly means the eBPF artifacts
  haven't been built yet (`make -C dod build-bpf`) or a pinned toolchain
  tool is missing/wrong-version (`make -C dod doctor`,
  `bpf/scripts/verify-toolchain.sh`).
- `enable refuses to run` — no `active_profile` selected in
  `/etc/harw-dod/config.toml`; see [Configure](#installation) above.
- `make -C dod smoke` — an explicit, operator-triggered live smoke check
  (`SMOKE_CONFIRM=1`); it is never part of ordinary install/enable/tests.

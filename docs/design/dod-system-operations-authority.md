# DoD System Operations, eBPF and Authority Core — Implementation Plan

> Status: partially implemented · Last reviewed: 2026-09-24

This document was originally written on 2026-09-18 as a forward-looking
implementation plan, not yet verified against a running host. As of this
review, substantial parts of it are implemented: `harw-authority` exists as
a crate in the product workspace, `dod/crates/harw-dod-config` exists,
`dod/Makefile` implements the FHS-layout install targets described in §5,
and the systemd units described in §5.2 exist under
`dod/packaging/systemd/` (`harw-dod.target`, `harw-dod-sentinel.service`,
`harw-dod-bpf.service`, `harw-dod-warden.service`,
`harw-dod-warden.socket`). What is confirmed still open: the
`permission_request!` macro described in §6.3 does not exist in
`harw-macros`. The rest of this document's many detailed acceptance
criteria (§8) have not been individually re-verified for this pass — treat
sections below as the original plan's intent, cross-check specifics against
the code before relying on them, and see `dod/Makefile`,
`dod/packaging/systemd/`, `harw-authority/src/lib.rs`, and
`dod/crates/harw-dod-config` for current ground truth.

## 1. Goal and binding decisions

DoD is meant to run on the local Linux machine as standalone system
software with real eBPF observation. Installation, configuration, programs
and service definitions belong to root. Dedicated system accounts hold only
the operational data each one needs. The personal HARW home tree is not an
installation target.

In parallel, the previous permission model is fully migrated onto a
ground-up `harw-authority` core. There is no compatibility facade for the
old permission construction. `harw-macros` simplifies declarations and
wiring; actual authorization stays in the authority core.

Binding decisions from the original review:

- A dedicated `dod/Makefile`, including installation of every DoD runtime
  component and systemd service file.
- System-wide systemd services on this machine; no user units and no file
  capabilities on personally-writable binaries.
- A production eBPF path including versioned sources and a reproducible ELF
  build.
- TOML profiles for various groups of cgroups as well as the whole host.
  Host observation happens only after an explicit profile choice.
- Observation first. The warden is installed but stays disabled, including
  its socket. Freeze, kill, and network isolation are not enabled.
- `doctor` checks prerequisites and explains missing installation steps.
  Regular build/install targets never auto-install toolchains or OS
  packages.
- Breaking changes to internal APIs are allowed; every affected caller is
  migrated.

Work is split into two separately acceptable deliveries: A = DoD system
operations; B = the authority rebuild. A does not depend on B's completion.
After B, both Cargo workspaces need re-checking. (Since PL-60 DoD is part of
the root workspace; "both workspaces" now means the product and DoD crates of
the one root workspace.)

## 2. Starting point and corrected assumptions (as of the original review)

DoD already had its own Cargo workspace under `dod/`. `harw-sentinel`
collects sensor values, processes external events, and calls rules.
`harw-probe-bpf` connects push-only over a Unix `SEQPACKET` socket.
`harw-dod-bpf/src/real.rs` already contained an aya-based `RealBpfLoader` at
that time — an earlier claim that no real loader existed was incorrect.
What was missing then: shipped kernel programs and demonstrated production
operation.

The loader at that time expected one program per ELF and a ring-buffer map
named `EVENTS`. `harw-warden` had a cgroup execution layer and
systemd-socket activation already; the network isolator was not
implemented, so successful observation was not proof of network
enforcement.

`PermissionSet::from_policy` was public, and `PermissionSet` was
deserializable — many main-workspace call sites actually only produced
requests or ceilings, others real root rights, and many test fixtures. The
claim that a new crate or private constructors alone would make the whole
system mathematically closed was rejected: a public policy compiler over
freely constructible inputs would still be an issuer of arbitrary rights.
Root issuance must rest on a verified trust source. Likewise, Rust types
don't protect against arbitrary malicious native code with the same OS
rights.

## 3. Delivery A: configuration and profiles

### 3.1 One system contract

A new, small DoD crate, `harw-dod-config` (**implemented** —
`dod/crates/harw-dod-config`): parsing, validation and resolution of system
configuration. No dependency on `harw-runtime`, the registry, or tool
crates. Every participating service uses the same contract.

Production source: `/etc/harw-dod/config.toml`. No automatic search in the
current repository, user home, or via `HARW_HOME`. An alternate `--config`
path serves explicit tests and administrative starts. Privileged starts
check ownership, write permissions, and the full path chain; symlink-based
or check-then-read-swapped configuration must not bypass the trust check.

Example schema:

```toml
schema_version = 1
mode = "observe"
# No implicit active selection. Add explicitly before enabling:
# active_profile = "selected-services"

[profiles.selected-services]
scope = "cgroups"
cgroup_paths = ["/system.slice/example-a.service", "/system.slice/example-b.service"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []

[profiles.user-workloads]
scope = "cgroups"
cgroup_paths = ["/user.slice"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
```

The examples are templates; `example-a.service` is not claimed to be a real
local unit. Operators can define further named profiles with multiple
cgroups. Exactly one profile is active; multiple scopes are combined
through its list.

Rules:

- Missing active profile: installation allowed, activation refused with a
  clear message.
- Unknown fields, profile names, sensors, invalid CIDRs, or conflicting
  host/cgroup specs: hard errors, no fallback.
- cgroup paths are relative to the cgroup-v2 mount root, start with `/`,
  contain no traversal, and are never bare string prefixes — `/a` does not
  cover `/ab`.
- `scope = "host"` must be chosen explicitly; an empty cgroup list never
  means host.
- Nonexistent selected cgroups prevent a full start. Cgroups that vanish or
  get replaced during operation cause visible degradation and locked
  capture for that scope, never expansion.
- Selection and capture concern process/flow data. Host-wide CPU/thermal
  measurements are separate baseline metrics, shown as such in status.
  Listener/workspace sensors must not covertly collect additional
  process/file data in restricted profiles — they stay off there until they
  reliably respect the same scope.
- An empty egress allowlist means every captured outbound TCP destination
  is outside the allowed set. The list evaluates events; it does not open a
  firewall.
- Profile switches happen via validated configuration and an ordered
  restart. No hot reload in the first delivery.

### 3.2 Shared runtime decision

Configuration is translated, before programs load, into an immutable
`ResolvedObservationProfile`: profile ID, scope, resolved cgroup
identities, sensor list, CIDRs, and a configuration digest. The sentinel
and the probe log the same digest; a mismatch prevents the full chain from
reporting ready.

The probe filters personally identifying event data before it reaches the
ring buffer; downstream checking is additional defense, not the mechanism —
filtering only in the sentinel does not satisfy the observation boundary.

## 4. Delivery A: eBPF and event chain

### 4.1 Build domain

A standalone `dod/bpf/` with its own toolchain file and lockfile, separate
from the host workspace. Rust/`no_std` and aya-eBPF; no nightly requirement
for normal host crates. An isolated build step produces the exec and TCP
objects. Toolchain, `aya-ebpf` and `bpf-linker` versions get validated
together on the target and pinned exactly during the first build work
package, rather than carried over from stale comments.

Host code keeps `unsafe_code = forbid`. If kernel access in the BPF source
crate needs unsafe, it lives exclusively in this separate build domain,
with small, documented access functions and verifier acceptance. This
exception must not spread to host crates.

Artifacts get a manifest with object name, SHA-256, ABI version,
architecture, and toolchain version. Installed files are root-owned and not
writable by service accounts. ELF and configuration checking must check the
actual bytes subsequently loaded — no re-opening unguarded after the hash
check.

### 4.2 Event semantics

**Exec:** successful process start via `sched_process_exec`. No unchecked
reading of raw tracepoint offsets across kernels — layout access is
generated/checked from validated kernel knowledge; an unsupported layout is
a start failure.

**TCP:** the first delivery captures outbound TCP connection attempts for
IPv4 and IPv6. UDP and full packet observation are not claimed as covered.
The existing socket-state tracepoint alone doesn't reliably attribute to
the current PID/UID/cgroup, so the flow path moves to a connect hook in the
calling process's context, with its ABI checked against the target kernel;
the loader gets explicit program selection and attach spec for this. When
unsupported, this sensor does not start — there is no fallback to a
presumed cause from softirq context.

Scope checking uses the causing task's cgroup, including the chosen
ancestor semantics. Identities stay stable; deleting/recreating a
same-named path must not silently reuse old IDs. Tests must cover sibling,
child, migration and recreation cases.

The shared BPF wire contract is an explicitly versioned byte format: type,
length, monotonic kernel time, process/cgroup identity, and a type-dependent
payload — no implicit Rust/C padding. Unknown versions and inconsistent
lengths are dropped and counted. Existing fixture formats are migrated
deliberately; ring-buffer data is not a persistent compatibility format.

Time conversion happens in the loader: monotonic kernel time plus a
measured mapping to real time — `ktime` is never interpreted as a Unix
epoch. Time jumps and suspend are tested and surfaced as uncertainty.

Exec metadata carries PID, PPID, UID, a bounded program path, and explicit
truncation info. Arguments and environment variables are not captured in
v1. A previously expected `argv_digest` is optional with status "not
collected" — the hash of an empty buffer must never appear as a real
argument proof. TCP carries destination address and port, no packet
payload.

### 4.3 Loader, landlock and operation

- Reuse the existing aya loader; check program name, type, maps and ABI
  instead of taking the first arbitrary program.
- Fully populate profile maps before activating hooks; partial failure
  detaches everything already attached.
- Determine required BPF/tracing capabilities at the actual attach path —
  `CAP_BPF` alone is not a proven guarantee; check `CAP_PERFMON`
  specifically; no automatic fallback to `CAP_SYS_ADMIN` or unrestricted
  root.
- Coordinate landlock start ordering with real accesses: configuration,
  objects, `/proc/self/status`, tracefs/BTF, cgroup resolution — today's
  restriction to object parent directories cannot be assumed sufficient.
- The sink stays push-only; the sentinel authenticates peer UID and allowed
  sensor IDs before accepting; bounded connections, message sizes, queue
  capacity and timeouts.
- Ring-buffer losses, IPC losses, and invalid events are counted; sensor
  heartbeat and last successful receipt distinguish a quiet source from a
  failed probe.
- A sentinel restart triggers a controlled probe restart and re-attach;
  stop removes all links; no unbounded blocking on send.
- CIDR evaluation comes from the same configuration in probe/rule engine —
  no accidental second evaluation against the previously fixed-empty
  sentinel scope.

## 5. Delivery A: FHS layout, services, Makefile

### 5.1 Install layout — implemented

Confirmed present in `dod/Makefile`:

| Path | Content and ownership |
|---|---|
| `/usr/local/libexec/harw-dod/` | programs, root:root, 0755 |
| `/usr/local/lib/harw-dod/bpf/` | objects and manifest, root:root, 0644 |
| `/usr/local/lib/systemd/system/` | system units, root:root, 0644 |
| `/etc/harw-dod/` | system configuration, root and a reading service group; not group-writable |
| `/var/lib/harw-dod/` | persistent evidence, sentinel-writable only |
| `/var/log/harw-dod/` | rotating telemetry, sentinel-writable only |
| `/run/harw-dod/` | volatile sockets/runtime data, narrow group permission |

Configuration file 0640, operating directories 0750, socket 0660. System
accounts `harw-dod` and `harw-dod-bpf`, no login, no personal home; a
shared IPC group only for the required socket access. The warden gets no
probe identity.

The sentinel gets separate options for state, telemetry and runtime
directories; the old shared `--home` must not redirect a system install
back into a personal HARW tree.

Install supports `PREFIX`, `LIBEXECDIR`, `LIBDIR`, `SYSCONFDIR`,
`LOCALSTATEDIR`, `SYSTEMD_UNITDIR` and `DESTDIR` — confirmed as knobs in
`dod/Makefile`. Default is a local admin install under `/usr/local`;
distributions can set `/usr`. Runtime/data paths stay FHS-compliant.

### 5.2 systemd contract — implemented

Units confirmed present under `dod/packaging/systemd/`: `harw-dod.target`,
`harw-dod-sentinel.service`, `harw-dod-bpf.service`,
`harw-dod-warden.service`, `harw-dod-warden.socket`. The observation target
is meant to pull up only the sentinel and BPF probe; the warden should
carry no activation edge from the observation target and ships neither
enabled nor started.

The sentinel runs without elevated capabilities. BPF runs under its own
system account with exactly the verified BPF/tracing capabilities in the
bounding and ambient sets. `NoNewPrivileges`, read-only system paths and
bounded write paths need to be tested against the kernel accesses actually
required — no blanket hardening option that hides the BPF source or its
scope.

Runtime/state/log directories are created via systemd directory management
or installed tmpfiles/sysusers rules. The probe gets no write access to
configuration, ELF files, or sentinel evidence. Restart limits prevent
infinite loops on invalid configuration. Readiness requires working IPC
receipt and successfully attached configured sensors, not just a live
process.

### 5.3 Targets — implemented (`dod/Makefile`)

Confirmed present: `help`, `doctor`, `check-config`, `build-bpf`, `build`,
`check`/`fmt`/`clippy`/`test`, `verify`, `install`, `enable`, `disable`,
`restart`, `status`/`logs`, `smoke`, `uninstall`.

`install` always includes service files. On a direct system install it
creates accounts/directories and runs `daemon-reload`; it does not start
observation without a chosen profile. With `DESTDIR`, it stages only — no
account creation, `daemon-reload`, activation, or runtime intervention.
Existing configuration is left unchanged; new examples are placed
separately.

Build runs unprivileged. System copy and service actions need explicit
admin rights. `install` with missing artifacts reports the needed build
instruction instead of running cargo as root. Dependencies and toolchains
are documented by `doctor`, not silently installed.

Upgrade: fully check artifacts in advance, stop services in order, install
the matching version, `daemon-reload`, restore the prior active observation
state after re-checking configuration. Keep the previous version for
rollback. An upgrade does not enable a previously disabled warden. Rollback
preserves evidence and configuration and checks their version
compatibility.

## 6. Delivery B: authority core, no facade

### 6.1 Ownership and type separation — implemented

A new, ground-up crate, `harw-authority`, in the product workspace,
consumable from DoD via a path dependency. **Confirmed present**
(`harw-authority/src/lib.rs`), with `PermissionRequest`, `PermissionSet`,
`AuthorityContext` and `AuthoritySnapshot` all defined there as planned.
Dependencies limited to necessary base types/serialization, no runtime,
core, tool, or registry edges.

The core owns `Permission`, `PermissionSet`, `SandboxSpec`, and the
workspace/network-scope types needed for identical scope evaluation.
`harw-sandbox` keeps OS backends like bwrap and uses the authority types.
No old re-exports to hide the move; imports and manifests are fully
migrated. Network semantics must not be implemented twice.

Central separation:

- `PermissionRequest`: freely constructible, serializable
  requests/ceilings; conveys no execution right by itself.
- `PermissionSet`: granted rights with private fields and private raw
  construction; no public `Deserialize`, `FromIterator`, rights-bearing
  `Default`, or unbounded builder.
- `AuthorityContext`: granted context with workspace, network scope, and
  provenance; immutable once issued.
- `AuthoritySnapshot`: serializable display/persistence data, explicitly
  not a grant — resumption requires re-evaluating policy.

Public derivation `parent.restrict(request)` returns only a subset of the
parent context. Mode/role/profile/contract ceilings become
`PermissionRequest`; `required_permissions()` returns requests — they must
never reach a sandbox as finished grants.

### 6.2 Root trust boundary

Root issuance stays an explicitly documented bootstrap operation. The
authority core reads and validates the trusted operator policy itself; a
freely constructible `TrustedPolicy` or a public
`Issuer::new(all_permissions)` is excluded. A passed-in request can only be
evaluated against that policy.

For DoD, the trust source is root-controlled system configuration. For
user-operated HARW, the existing operator/home policy stays its trust
source — root ownership is not newly mandated there. Both operating modes
are typed separately. Repository content may only supply additional
restrictions, never assert a trust class itself.

Issuance decisions bind the grant to workspace/operator context and log
provenance and a policy digest. A root bootstrap API is therefore not a
permission boundary against malicious code with the same OS rights — the
demonstrable protection is that unvalidated data and ordinary consumer APIs
cannot construct an arbitrary grant, and derivation from existing authority
never widens it. Stronger isolation would need separate processes and its
own extension.

Serialization and resumption are fully inventoried. Existing stored rights
are read as a snapshot/request and intersected against current root and
parent policy — no implicit trust in old stored permission sets. Missing
provenance or policy leads to rejection or an explicitly empty context,
never full rights.

### 6.3 Macros — partially open

Existing `tool`/`operation` macros use the new authority types and
generate guard calls. A planned addition, `permission_request!`, for
validated declarative permission lists (result: only a `PermissionRequest`,
unknown entries a compile error) — **checked: not present in
`harw-macros`** as of this review. Open.

No macro exception for private grant construction, no `__private` minting
export, no caller-crate name check as a supposed security boundary.
`harw-macros` keeps no production runtime dependency on the authority core;
generated absolute paths are resolved in consumers. Hand-written and
generated guards call the same core implementation.

### 6.4 Migration

Classify call sites by root issuance, reduction, requirement checking,
snapshot, and test fixture, then migrate together: runtime assembly, entry
tiers, mode switches, child admission, registry roles, definition tools,
job contracts, channel restrictions, and tool contexts.

Fixtures produce legitimate grants via an isolated test policy and the same
validator. Arbitrary bit sets for algebra tests stay internal tests of the
core — no public test-support feature that opens a production minting path
via Cargo feature unification.

The (at plan time) 7 permission variants get fully checked: all 128 parent
sets against all 128 requests, plus network/workspace bounding and
resumption. New permission variants must update the exhaustive mappings and
must not auto-unlock.

## 7. Work packages and order

Each package delivers code, relevant tests, and updated operational
documentation. The next dependent package only follows once its own
acceptance passes. No automatic commits of unrelated changes.

1. **Inventory and baseline:** capture working-copy changes; check both
   workspace manifests and existing gates; separate real build failures
   from task changes. Produce a local kernel/toolchain report.
2. **DoD configuration contract:** config crate, profiles, validation,
   shared resolution; negative cases tested first.
3. **DoD Makefile and staging:** targets, FHS directory layout, unit
   templates, sysusers/tmpfiles, install manifest, `DESTDIR` tests.
4. **BPF build and ABI:** pin toolchain, build sources and wire contract,
   migrate parsers/fixtures, automatically check ELF structure.
5. **Scope and loader:** kernel profiles, attach selection, correct
   task/cgroup attribution, time conversion, resource cleanup, loss
   metrics.
6. **Sentinel integration:** authenticated IPC, profile matching, CIDR
   rules, readiness, separate FHS data paths, restart.
7. **Local acceptance and installation:** check staging, run a system
   install with required approval, choose a profile explicitly, enable
   observation, run the smoke test, check restart/rollback — warden stays
   off.
8. **Authority type core:** type separation and private grant construction
   with compile-fail and algebra tests; explicit bootstrap trust sources.
9. **Consumer migration and macros:** switch every production
   construction, snapshot, fixture and generated guard; remove the old
   API.
10. **Overall acceptance:** check the product workspace and DoD; re-test
    the installed DoD version against the same live scenarios; document
    the effective guarantee and limits.

## 8. Test and acceptance matrix

### Without elevated rights

- TOML: multiple cgroups, host, unknown profile, missing selection, empty
  lists, traversal, invalid CIDRs, unknown sensors.
- Scope: `/a` vs. `/ab`, children on/off, duplicate paths, missing and
  recreated cgroups.
- ABI: both event types, IPv4/IPv6, port > 255, wrong version/length,
  truncated strings, no argv/packet data.
- Loader: wrong ELF, wrong map, multiple unexpected programs, missing
  rights, partial failure and full detach via test doubles.
- IPC: foreign UID, forged sensor ID, oversized message, queue overflow,
  connection drop and restart.
- Installation: `DESTDIR` contains every program/object/unit; no host side
  effects; existing configuration stays byte-identical; no user-home paths
  in units.
- Authority: direct construction and deserialization of a grant do not
  compile; requirements grant no rights; a snapshot cannot be used as a
  grant; every derivation stays a subset.
- Macros: valid declarations, unknown permissions, missing authority
  context, and identical guard behavior to hand-written code.

### Explicit live acceptance

- Two test cgroups generate exec and TCP connections to a local test
  server; only selected groups appear; the host profile captures both only
  after an explicit switch.
- Check IPv4 and IPv6; document missing IPv6 as an explicit failure/host
  prerequisite, not a silent skip.
- An allowed CIDR produces no egress violation; a disallowed destination
  produces a finding; no connection is blocked.
- Compare PID/UID/cgroup and timestamps against the test process; no
  attribution to the probe process or a random kernel worker.
- Rights revocation, a missing object, invalid config, and sentinel failure
  each produce a clear error/degradation status.
- Stop/restart leaves no BPF links; restart produces no duplicate events
  from double attachment.
- The warden and its socket stay inactive; freeze/kill files are not
  written and firewall rules are not changed.
- Run observation for at least ten minutes; check heartbeats, log
  rotation, queue/ring losses and memory use — a silent failure does not
  count as successful operation.

Canonical verification once the targets exist: `make -C dod verify`, then
for delivery B the existing root target `make clippy-tests`. Live
acceptance separately with `make -C dod smoke`. Baseline failures must be
reported separately; no green overall acceptance with missing required
checks.

## 9. Definition of done, and limits left deliberately in place

Delivery A is done when a fresh build produces installable host and BPF
artifacts, `make install` provides the complete FHS package including
services, and real, profile-bounded events demonstrably arrive at the
sentinel — every acceptance step must supply evidence, not just module
comments.

Delivery B is done when requests and grants are separated, root issuance
actually checks trusted policy, no public raw/serde/macro path produces
arbitrary grants, and every main and DoD consumer builds and is tested
against the new types.

Explicitly not part of this delivery: automatic cgroup escalation, network
blocking, full UDP coverage, packet capture, unrestricted isolation of
malicious native plugins, fanotify/audit-netlink expansion. Their absence
is shown visibly in status and operational documentation.

# `harw-dod`: Crate Decomposition, Responsibilities and Rights Matrix

> Status: implemented · Last reviewed: 2026-09-24

**Purpose:** the decomposition of the defense subsystem into small,
single-responsibility crates; their dependencies, error types, wiring, and
who is allowed to do what, how, and where.
**Related:** `harw-dod-charter.md`, `harw-dod-integration-and-dependencies.md`

The `dod/` Cargo workspace (`dod/Cargo.toml`) now has 32 members implementing
this decomposition. Illustrative code in this document uses representative
names; consult the crate itself for exact type and variant names, which have
evolved since this document was first written (e.g. `Capability` variants are
named per concrete source, such as `ReadSysfsThermal`, `ReadProcStat`, rather
than the generic `SysfsRead`/`ProcfsRead` sketched below).

---

## 0. The principle everything follows

One crate, one source, one permission.

This is not tidiness, it is the core of the security architecture. If every
crate needs exactly one permission, a binary's permission set is the union
over its dependency graph — and that union is computable (`harw-code-graph`
already reads `Cargo.toml`/`Cargo.lock`).

**This turns the privilege budget into a graph property that CI can check**
(implemented: `xtask/src/gate_privileges.rs`, `CRATE_PRIVILEGE` table and
`BinaryBudget`). A binary that acquires a permission it shouldn't have is not
a review miss, it's a failed build.

The converse is the actual reason for the fine granularity: a crate that
observes processes needs `CAP_BPF`; one that observes file events needs
`CAP_SYS_ADMIN`; one that reads temperatures needs nothing. Bundled together,
temperature reading would effectively run as root. Separated, it has a read
permission on `/sys`.

---

## 1. Invariants of the decomposition

**C1.** A crate has exactly one source and declares exactly one permission.
If something needs two, it is two crates. (One documented exception: see §5
on `harw-dod-authlog`.)

**C2.** The permission is proven at bind time, not assumed.
`SensorHandle<Unbound>::bind()` performs a real probe access; only
`SensorHandle<Bound>` exposes `poll`/`subscribe`.
Implemented: `dod/crates/harw-dod-cap/src/handle.rs`.

**C3.** Scope is a value, not a convention. A sensor reads exclusively
through a `ReadScope` it receives in its constructor, which can only shrink.
Implemented: `dod/crates/harw-dod-cap/src/scope.rs` (a semilattice with only
`intersection`, no `add`/union; symlinks are resolved before the scope check,
including a dedicated alias-root mechanism for sysfs class directories whose
entries are themselves symlinks into `/sys/devices/...`).

**C4.** Privileged processes are push-only. A probe process with `CAP_BPF` or
`CAP_SYS_ADMIN` has no command interface; it pushes events to the sentinel
and accepts nothing. A compromised sentinel cannot task a probe, because
there is nothing to task.

**C5.** A crate's public error type is narrow and content-free. A parse error
names position and length, never the parsed content — otherwise an
attacker-controlled log line travels the error path into telemetry.

**C6.** A permanently failing sensor degrades the system, never aborts it.
Failure is an operating state with a metric, not a crash.

**C7.** No sensor crate depends on another sensor crate. Sensors are
siblings, never a chain, so one's permission never bleeds into another's
dependency graph.

**C8.** A binary's privilege budget is the union of permissions across its
dependency graph, checked in CI against a declared ceiling. Implemented:
`xtask/src/gate_privileges.rs`.

---

## 2. Naming scheme

All crates of the subsystem carry the `harw-dod-` prefix, which makes the
dependency graph immediately legible. The facade itself is `harw-dod`.

`harw-context` and the `harw-observe*` telemetry crates are excluded: they
are infrastructure for every agent, not part of this subsystem (charter §4).

---

## 3. Foundation crates

Vocabulary leaves and access primitives; none of them read a source
themselves.

| Crate | Responsibility |
|---|---|
| `harw-dod-cap` | `Capability`, `CapabilityClass`, `ReadScope`, `SensorError`, `Permanence`, the `Sensor` typestate (`Unbound`/`Bound`, `SensorHandle`) |
| `harw-dod-signals` | `SecurityEvent`, `EventKind`, `HostSample`, `Finding<S>`, `Verdict`, the `Sensor` trait itself |
| `harw-dod-warden-proto` | wire types of the closed action set |
| `harw-dod-readfs` | typed reads under a `ReadScope` |
| `harw-dod-netlink` | netlink socket and framing, protocol-parameterized |
| `harw-dod-bpf` | aya loader scaffolding, map access, verifier error mapping |

### 3.1 `harw-dod-cap`, the centerpiece

```rust
/// What a crate needs, as a permission. Closed set, one variant per
/// concrete source (illustrative — see capability.rs for the current list).
pub enum Capability {
    ReadSysfsThermal,
    ReadProcStat,
    ReadProcMeminfo,
    ReadSysfsBlock,
    ReadProcNetDev,
    ReadSysfsDrm,
    ReadCgroupV2,
    ReadProcNet,
    ReadJournal,
    ReadAuditNetlink,
    // ... plus the privileged capabilities for bpf/fanotify/net-admin/cgroup-write.
}

pub enum CapabilityClass { Unprivileged, Netlink, FileWatch, Bpf }
```

```rust
/// The "where." Can only shrink.
pub struct ReadScope { /* semilattice of allowed roots, intersection only */ }

impl ReadScope {
    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self;
    pub fn intersection(&self, other: &Self) -> Self;
    /// Resolves symlinks, then checks scope, then opens.
    pub fn open(&self, path: &Path) -> Result<OwnedFd, SensorError>;
}
```

No `add`, no `extend`. A sensor gets its scope in its constructor and cannot
widen it. Symlink resolution must happen before the check, or the scope can
be bypassed via a link.

**The typestate `Sensor` handle** (`SensorHandle<Unbound>` /
`SensorHandle<Bound>`): `bind()` performs a real probe access
(`Capability::probe`) and only on success returns a bound handle whose
`poll`/`subscribe` methods become callable.

**The boundary error type**, `SensorError`, is narrow: it names a
capability, a scope violation, unavailability, or a parse failure by offset
and length — never parsed content (C5).

---

## 4. Sensor crates, stream A: measurements

All unprivileged, all read-only, none depending on another sensor crate
(C7). Each depends only on `harw-dod-cap`, `harw-dod-signals`,
`harw-dod-readfs`.

| Crate | Source | Delivers |
|---|---|---|
| `harw-dod-thermal` | `/sys/class/hwmon`, `/sys/class/thermal` | temperature, fan, voltage, throttle flags |
| `harw-dod-cpu` | `/proc/stat`, cpufreq, `/proc/pressure/cpu` | time classes per core incl. `steal`, frequency, PSI |
| `harw-dod-memory` | `/proc/meminfo`, `/proc/pressure/memory`, `/proc/vmstat` | usage, swap rates, PSI |
| `harw-dod-blockio` | `/proc/diskstats`, `/proc/pressure/io`, `statvfs` | latency, queue depth, fill levels, PSI |
| `harw-dod-netcounters` | `/proc/net/dev`, conntrack counters | per-interface counters |
| `harw-dod-gpu` | `/sys/class/drm`, hwmon; NVML behind a feature | utilization, VRAM, temperature, throttle reasons |
| `harw-dod-cgroup` | `cpu.stat`, `memory.current`, `io.stat`, `pids.current` | resources per cgroup, hierarchical |

**Why `netcounters`, `listener` and `flow` stay separate crates:** three
permissions, three questions. Per-interface counters are harmless and
unprivileged. Open sockets require inode-to-PID resolution and are
noticeably more sensitive. Connections with destinations and SNI need eBPF
and a privileged process. Merging them would be convenient and would lift
the most harmless measurement to the highest privilege.

---

## 5. Sensor crates, stream B: events

| Crate | Source | Permission | Process | Delivers |
|---|---|---|---|---|
| `harw-dod-listener` | `/proc/net/tcp{,6}`, `/proc/net/udp`, inode→PID | unprivileged | sentinel | open listeners with cgroup context |
| `harw-dod-authlog` | auditd `USER_AUTH`/`USER_LOGIN`, journald sshd unit, btmp | netlink or journal | sentinel | login attempts/successes with `auid`, source, method |
| `harw-dod-scanreport` | reports from rkhunter, ClamAV, Lynis, AIDE, smartctl | reports-read | sentinel | normalized scan findings |
| `harw-dod-workspace` | `Cargo.toml`/`Cargo.lock` via `harw-code-graph` | workspace-read | sentinel | structure and dependency drift |
| `harw-dod-procmon` | eBPF `sched_process_exec`/`exit`, procfs reconciliation | bpf | probe-bpf | exec, exit, parentage, cgroup |
| `harw-dod-flow` | eBPF/XDP flow aggregation, SNI, DNS names | bpf | probe-bpf | five-tuple, bytes, duration, SNI, cgroup |
| `harw-dod-fsmon` | fanotify `FAN_MODIFY`/`FAN_CLOSE_WRITE`, loginuid | file-watch | probe-fs | path, timestamp, actor, digest before/after |

**`harw-dod-authlog` needs two permissions**, the one documented exception to
C1: it has two backends of the same source (login events), `AuditBackend`
and `JournalBackend`, each with exactly one permission. The sentinel binds
whichever is available and degrades to the other.

**`harw-dod-scanreport` starts nothing.** A systemd timer runs the tools with
fixed arguments and writes to the reports directory; the crate only reads.

---

## 6. Processing and enforcement

| Crate | Responsibility | Process |
|---|---|---|
| `harw-dod-sentinel` | aggregation, poll loops, degradation, ring buffer, IPC receipt from probes | sentinel |
| `harw-dod-rules` | contract, threshold and deviation rules; pure functions with injected `now` | sentinel |
| `harw-dod-escalate` | the ladder, sole constructor of `Action<Authorized>` (`pub(crate)`), freeze leases, plan attachment | sentinel |
| `harw-dod-netpolicy` | `NetPlan` as a pure, testable plan | warden |
| `harw-dod-warden` | enforcement, closed action set, audit | warden |
| `harw-dod` | re-export facade, no logic | — |

`harw-dod-sentinel` contains **no** parsing logic — it holds sensors, calls
them, fans results into streams, and manages degradation.

---

## 7. Process topology

Four binaries, one privilege class each (implements C4/C8 at the operational
level), confirmed present: `harw-sentinel`, `harw-probe-bpf`,
`harw-probe-fs`, `harw-warden` (`dod/crates/{harw-sentinel,harw-probe-bpf,harw-probe-fs,harw-warden}`).

| Binary | Kernel rights | Contains | Landlock scope |
|---|---|---|---|
| `harw-sentinel` | none | stream-A sensors, listener, authlog, scanreport, workspace, rules, escalate | ro: `/proc`, `/sys`, reports dir, workspace manifests |
| `harw-probe-bpf` | `CAP_BPF`, `CAP_PERFMON` | procmon, flow | ro: `/sys/fs/bpf`, `/proc` |
| `harw-probe-fs` | `CAP_SYS_ADMIN` | fsmon | ro: watched paths plus `/proc` |
| `harw-warden` | `CAP_NET_ADMIN`, cgroup write | netpolicy, warden | rw: only `cgroup.freeze` files |

**Why `harw-probe-fs` stands alone.** fanotify with filesystem marking
requires `CAP_SYS_ADMIN`, which is effectively root; a process with that
permission must carry nothing else.

**Data flow direction.** Probes push, the sentinel receives and proposes,
the warden enforces:

```
probe-bpf ──push──┐
probe-fs  ──push──┼──> sentinel ──WardenRequest──> warden
                  │       │
sentinel-owned ───┘       └──> artifacts, telemetry, plan bridge
```

No arrow points from the sentinel to a probe — the probes have no receive
path, their socket is write-side only. A compromised sentinel gains nothing
from a probe's `CAP_SYS_ADMIN`.

**The warden receives**, and is the only process that does — its socket is
systemd-activated, checked with `SO_PEERCRED`, versioned framing, and the
action set as its only message vocabulary.

---

## 8. Rights matrix: who may do what, how, where

Four dimensions, each with its own enforcement mechanism.

| Dimension | Question | Carrier | Enforced by |
|---|---|---|---|
| **Who** | which crate, which process | crate identity, systemd unit | dependency graph + CI gate (C8) |
| **What** | which permission | `Capability` | proven at `bind()` (C2), kernel capability of the unit |
| **Where** | which paths, which netlink protocol, which cgroup | `ReadScope`, protocol parameters | type plus the process's landlock rule (C3) |
| **How** | read, subscribe, act | typestate | `SensorHandle<Bound>` for read/subscribe, `Action<Authorized>` for acting |

All four mechanisms are independent and deny-only. A sensor that cannot
prove its permission is not callable. One whose path is outside its scope
gets no file descriptor. A process denied a path by landlock cannot see it
regardless of its code. An action without authorization does not exist at
the type level.

| Role | may read | may subscribe | may act |
|---|---|---|---|
| stream-A sensor | its one scoped source | no | no |
| unprivileged event sensor | its one scoped source | yes, its own source | no |
| privileged event sensor | its kernel source | yes, its own source | no, no receive path |
| sentinel | nothing directly, only via sensors | yes, all bound | no, only propose |
| rules layer | passed-in references | no | no |
| triage agent | context per `ContextCeiling` | no | no, only `ProposedAction` |
| ladder | findings, baselines | no | construct `Action<Authorized>` |
| warden | its own configuration | no | the closed action set |
| human | everything via operator surfaces | yes | including the irreversible |

The line that carries the system: **no agent and no model sits in the "may
act" column.** The only route there is the ladder, and it decides on
severity of the finding, not on judgment.

---

## 9. Error handling and wiring

**Two levels per crate.** Inside, a rich, specific error type
(`ThermalError`, `AuthlogError`, ...) with everything useful for debugging.
At the trait boundary, mapping onto `SensorError`. The rich type stays
`pub` for testability but never appears in a trait signature.

Collapsing is mandatory, not a convenience: `AuthlogError::Truncated {
index }` becomes `SensorError::Parse { offset, len }`, and no field content
survives the transition (C5).

**Degradation as state.** The sentinel runs a small state machine per
sensor (`Bound` / `Retrying { since, attempts }` / `Degraded { since,
reason }`). `Permanence::Transient` leads to `Retrying` with backoff;
`Permanence::Permanent` leads directly to `Degraded`. A degraded sensor is
deregistered, a counter increments, and the rules layer learns via an
`EventKind::SensorDegraded` that it is missing a source — blindness is
itself a finding, never silent "nothing to see."

**Errors in the warden.** Its own, even narrower error type. Every error
path writes an audit entry before returning; a zero-counter invariant
(`warden_unaudited_actions`) checks exactly that.

---

## 10. Build order

Bottom-up, each stage independently useful and testable:

1. **Foundation.** `harw-dod-cap`, `harw-dod-signals`. Pure leaves, fully
   unit-testable.
2. **Primitives.** `harw-dod-readfs`, `harw-dod-netlink`, `harw-dod-bpf`.
   Testable against a fixture directory / loopback socket, no privileges.
3. **Unprivileged sensors, in parallel.** The stream-A crates plus
   `listener`, `authlog` (journal backend), `scanreport`, `workspace`. Each
   tested against a fixture directory with real `/proc`/`/sys` excerpts.
4. **Sentinel.** Aggregation, degradation state machine, ring buffer,
   metrics. First runnable, fully unprivileged binary.
5. **Rules.** `harw-dod-rules`. Pure functions, golden cases.
6. **Privileged probes.** `harw-dod-fsmon`, then `harw-dod-procmon` and
   `harw-dod-flow`. Needs a VM to test, hence built as late as possible.
7. **Ladder and warden.** `harw-dod-escalate`, `harw-dod-warden-proto`,
   `harw-dod-netpolicy`, `harw-dod-warden`.
8. **Facade and agents.** `harw-dod`, the security agent family, triage
   specializations, plan attachment.

All 8 stages are now present in the `dod/` workspace; stages 1–5 need no
kernel permission and no VM to develop or test.

---

## 11. Checks

- **Privilege gate.** CI computes, per binary, the union of `Capability`
  declarations across its dependency graph and compares it to the unit's
  declared ceiling. Implemented: `xtask/src/gate_privileges.rs`.
- **Isolation test.** No sensor crate may depend on another sensor crate
  (C7). Implemented via `dod/crates/harw-dod/tests/facade.rs` and workspace
  dependency-graph checks.
- **Scope test.** A fixture directory containing a symlink pointing outside
  the scope; `ReadScope::open` must return `OutsideScope`.
- **Error-redaction test.** For each sensor crate, a case with an
  attacker-controlled line whose content must not appear in any
  `SensorError` output.
- **Degradation test.** A sensor that permanently returns `Permanent` must
  push the sentinel into `Degraded` without terminating it, and produce a
  `SensorDegraded` event.
- **Push-only test.** The probe socket, written to, must yield nothing on
  read; a test checks the probe binary contains no receive loop (C4).
- **Fixture corpora.** Per sensor, a directory of real excerpts from
  different kernel versions, to guard parsers against format drift.

---

## 12. Open decisions

1. **IPC format, probe to sentinel.** Length-prefixed frames with a compact
   binary format rather than JSON, because `procmon`'s event rate can be
   high. Alternative: an eBPF ring buffer read directly by the sentinel —
   rejected, because it would hand the sentinel `CAP_BPF` and break the
   separation.
2. **Ceiling on the number of sensor crates.** Current cut stands at the
   crates listed above. Rule for any new source: its own source plus its
   own permission equals its own crate.
3. **`harw-dod-cgroup` vs. `harw-dod-warden`.** Reading and writing the
   cgroup hierarchy are separate permissions and separate crates. Open:
   whether the warden reuses the read crate for freezing or keeps its own
   minimal read routine.
4. **Landlock minimum version.** ABI probing is mandatory; open whether a
   kernel without landlock is a hard start failure or a documented
   degradation. Leaning: degradation with a loud metric.
5. **Fixture provenance.** Which kernel versions to excerpt, and how they
   are maintained.

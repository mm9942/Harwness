# `harw-dod`: Charter of the Facade

> Status: implemented · Last reviewed: 2026-09-24

**Purpose of this document:** why this facade exists, why it is named as it
is, what it may do and what it explicitly may not do.
**Related:** `harw-security-observability-plan.md`,
`harw-dod-integration-and-dependencies.md`

---

## 1. Why a facade at all

The security subsystem is split across roughly thirty crates under
`dod/crates/` (members of the root workspace since PL-60), at several layers: capability/data vocabulary
(`harw-dod-cap`, `harw-dod-signals`, `harw-dod-readfs`, `harw-dod-fixtures`),
thirteen `Sensor`-implementing crates (`harw-dod-cpu`, `-thermal`, `-memory`,
`-blockio`, `-netcounters`, `-gpu`, `-cgroup`, `-listener`, `-scanreport`,
`-workspace`, `-authlog`, `-fsmon`, `-procmon`), shared infrastructure that is
not itself a sensor (`harw-dod-bpf`, `harw-dod-netlink`), event translation
(`harw-dod-flow`), evaluation (`harw-dod-rules`, `harw-dod-sentinel`), and the
escalation/enforcement chain (`harw-dod-escalate`, `harw-dod-warden-proto`,
`harw-dod-warden`, `harw-dod-netpolicy`).

Without a facade, every consumer would need to know this layering: which
crate `Finding` comes from, which crate `WardenAction` comes from, which
schema version currently applies. `harw-dod` (`dod/crates/harw-dod`) is that
facade: a curated re-export surface, modeled on the same discipline as
`harw-lens`'s facade, with no logic of its own.

For defensive code this is more than convenience. A facade is a **reduction
surface**: it fixes what is reachable from the outside, and anything it does
not re-export does not exist for consumers. For a subsystem whose entire
point is to bound reachability, that is not a comfort feature — it is the
enforcement of the "structurally prevent, don't just police" principle at
the module level.

---

## 2. The name

`harw-dod` follows the pattern of `harw`: a short name, clear ownership, no
description of internals. The acronym has a fixed meaning:

> **DoD stands, in this project, for Detect, Orient, Defend.**

This is not wordplay for its own sake. The three syllables are the three
layers a finding passes through, in that order:

**Detect** is the thirteen `Sensor` crates plus `harw-dod-cap` /
`harw-dod-signals`: read, measure, produce events. No judgment, no
privileges beyond their declared read scope.

**Orient** is `harw-dod-rules` (plus `harw-dod-sentinel` as the aggregator):
classification. Contract violation vs. threshold vs. drift. This is where
baselines, history and verdicts live.

**Defend** is `harw-dod-escalate` plus `harw-dod-warden`: the ladder and the
enforcer. Reversible before irreversible, a closed action set, mandatory
audit.

The allusion to the OODA loop (Observe, Orient, Decide, Act) is intentional.
What matters is what is **missing** from the acronym:

> **The A for Act is deliberately not in the name.**

Irreversible action belongs to the human. Kill, rollback, revocation of
permissions never happen automatically. The acronym names exactly the three
steps the system may take on its own and omits the fourth.

**What the name does not mean.** No department, no authority, no power
center. The facade is a re-export module with no logic and no rights of its
own — the narrowest part of the system, not the most powerful one.

---

## 3. What the facade is

`harw-dod` (`dod/crates/harw-dod/src/lib.rs`) re-exports individual, named
items — no `pub use *`, no phase-shaped sub-modules (`detect`/`orient`/
`defend`). The admission test the module doc states for every re-export:

1. Is it a parameter or return type of an already re-exported
   type/function?
2. Is it a tool a caller needs to fill those parameters, without which they
   would have to reach into internals directly?
3. Would withholding this name silently make a decision that actually
   belongs to the caller?

**"Observe the host"** — capability vocabulary from `harw-dod-cap`
(`Capability`, `CapabilityClass`, `ReadScope`, `SensorHandle`, `Bound`,
`Unbound`, `SensorError`, `Permanence`), data vocabulary from
`harw-dod-signals` (`Sensor`, `SensorReading`, `SecurityEvent`, `EventKind`,
`Actor`, `AuthOutcome`, `DriftSeverity`, `HostSample`, `Severity`,
`Hardness`, `SecurityEvidence`), the aggregator from `harw-dod-sentinel`
(`Sentinel`, `SentinelConfig`, `SensorHealth`, `DegradeReason`,
`RetryPolicy`, `EvidenceBuffer`), and the nine unprivileged sensors
(`CpuSensor`, `ThermalSensor`, `MemorySensor`, `BlockioSensor`,
`NetCountersSensor`, `GpuSensor`, `ListenerSensor`, `ScanReportSensor`,
`WorkspaceDriftSensor`). Four privileged sensors/loaders (`AuthlogSensor`,
`FsMonSensor`, `ProcmonSensor`, the `BpfLoader` family, `harw-dod-flow`'s
`observe`) sit behind this crate's `privileged` Cargo feature as optional
path dependencies, so a consumer that writes plain `harw-dod` in its
`Cargo.toml` gets none of those privilege classes on its dependency edge
(checked by `dod/crates/harw-dod/tests/facade.rs`).

**"Assess a finding"** — from `harw-dod-rules`: `run_rules`,
`run_rules_checked`, `Rule`, `RuleContext`, the three shipped rules
(`EgressFlowRule`, `BaselineDeviationRule`, `StructureDriftRule`), `Finding`,
`FindingKind`, `RuleChecked`, `Triaged`, `Verdict`, `triage`, `Baseline`,
`PalaceStatus`.

That is the whole surface. There is no `Action`, `Authorized` or
`Action<Authorized>` reachable from `harw_dod` at all — the crate has a
`compile_fail` doctest that proves the import itself fails name resolution
(E0432), not just a privilege check.

---

## 4. What the facade explicitly is not

**Not a place for logic.** No `impl` with behavior, no helper that didn't
fit anywhere else. Facades that start doing things themselves become junk
drawers.

**No path to the warden.** `harw-dod-escalate`, `harw-dod-warden-proto`,
`harw-dod-warden` and `harw-dod-netpolicy` are **not** part of this facade,
not even behind a feature. Reasons: `Action::authorize` is `pub(crate)` in
`harw-dod-escalate` (the only place in the workspace that produces an
`AuthorizationProof`); the escalation functions take a
`harw_session_store::FreezeStore`, which would pull in a fourth crate;
`harw-dod-warden` has exactly one caller (the `harw-warden` binary, which
already names it directly); and `harw-dod-netpolicy` does not implement
`Sensor` at all — it plans `NetPlan` rules against `harw-sandbox`'s
`NetworkScope`/`EgressTarget` vocabulary and belongs, if anywhere, to a
future enforcement-facing facade with its own rationale.

**No roof over infrastructure.** `harw-context` and the `harw-observe*`
telemetry crates are not under this facade. Context and telemetry serve
every agent, not just the security path; folding them under the security
facade would make infrastructure changes look like security changes.

**No privileged process.** `harw-dod-warden` is its own binary with its own
dependency budget. The facade links its wire types (`harw-dod-warden-proto`)
where relevant elsewhere, never its implementation.

---

## 5. Intentions: why the system is built this way

**First: express impossibility, not check correctness.** A check can be
forgotten, a type cannot. `PermissionSet` can only narrow, `EgressSet` can
only narrow, `ContextBudgetSpec` can only narrow, and `Action<Authorized>`
can only be constructed in one place.

**Second: structurally preventing beats monitoring.** Much of what
classical security tools try to detect cannot happen here in the first
place: an agent with no egress target cannot reach one, a child without
write permission does not write. Monitoring is the answer for the rest, not
the first answer.

**Third: proposing is not committing.** The pattern repeats throughout: a
verdict carries `ProposedAction`, the ladder authorizes, the human decides
on the irreversible.

**Fourth: the verdict is agentic, the reflex is deterministic.** Packet
filters, rate limiting, seccomp and integrity checks stay in kernel and
rule-engine territory because they must be fast, incorruptible and
available under attack. A language model can be talked into things, a
packet filter cannot. What an agent is good for is the layer above:
correlating over time, triaging noise, explaining drift — and nowhere below
that.

**Fifth: because the desired state is known, deviation is decidable.** This
project knows which agent may produce which flow, which contract applies to
which path, which plan node is currently active. A contract violation is
therefore a hard finding here, not a heuristic, and may act, while a
statistical signal may only warn.

**Sixth: built for long operating life.** Pure Rust on the privileged path,
no kernel module, every third-party library behind its own trait, a capped
dependency budget. A defense system that breaks on every kernel update
isn't defending, it's keeping people busy.

---

## 6. What the system explicitly does not do

- **No behavioral profiling of people.** Change attribution yes, so drift
  is explainable and rollback possible. No model of whether a person is
  behaving "normally." For human actors, the ladder ends at reporting and
  logging.
- **No in-house malware detection.** Established tools run on a timer with
  fixed arguments; this system reads their reports. It builds the triage,
  not the scanner.
- **No kernel module.** All needed capabilities are reachable through
  stable interfaces.
- **No generic execution path.** There is no warden operation that takes
  text as a command. A new capability is a new named action with an
  admission rule and an audit name, or it does not exist.
- **No own state for plan and goal.** Findings live in `harw-knowledge`;
  plan effects go exclusively through `harw-plan-bridge`. The facade owns
  nothing.

---

## 7. Admission criteria for the facade

For every re-export:

1. The type belongs to one of the three phases (detect/orient/defend, with
   defend excluded per §4). What fits none of them probably doesn't belong
   in the subsystem's public surface.
2. The type is part of a contract, not an implementation detail.
3. The type does not open a path around an invariant — in particular, no
   constructor for `Action<Authorized>`, no way around the ladder, no
   direct warden client without a proof.
4. The re-export is versioned with the subsystem; a schema change is a
   facade change and is tracked as one.

A re-export that violates one of these is rejected even when convenient.
Convenience is the usual way facades lose their reduction effect.

# Harwness Security & Observability Subsystem

> Status: partially implemented · Last reviewed: 2026-09-24

**Scope:** host sensing, security triage, enforcement, telemetry, and the
system's self-observation of its own structure.
**Related:** `harw-dod-charter.md`, `harw-dod-crate-decomposition.md`,
`harw-dod-integration-and-dependencies.md`, `harw-dod-sensor-extensibility-plan.md`,
`harw-telemetry-integration-plan.md`, `docs/design/runtime-contracts.md`

This document originated as the build plan for the security subsystem
before its crates were split out into the `dod/` workspace under the
`harw-dod-*` naming scheme. Where this document once specified crates named
`harw-signals`, `harw-sensor`, `harw-warden-proto`, `harw-netpolicy`,
`harw-rules`, `harw-escalate`, the implemented crates are
`harw-dod-signals`, the `harw-dod-*` sensor family, `harw-dod-warden-proto`,
`harw-dod-netpolicy`, `harw-dod-rules`, `harw-dod-escalate` — see
`harw-dod-crate-decomposition.md` for the authoritative, current crate
landscape and rights matrix; this document keeps the normative invariants
and the agent/observability layers that sit above that decomposition.

---

## 1. Normative invariants

**S1.** No model-authored content ever reaches a privileged execution path.
The IPC interface to the enforcer has no operation that executes free text
or free arguments. **Implemented** — `harw-dod-warden`'s action set is
closed and typed (`harw-dod-warden-proto`).

**S2.** The enforcer's action set is closed: named, typed, validated,
audited operations. New actions come only from a code change in the
enforcer, never configuration, a plugin, or a definition. **Implemented.**

**S3.** Observation and enforcement live in different processes.
Observation runs unprivileged with targeted read groups; enforcement has no
model contact and no outbound network. **Implemented** — see the four-binary
process topology in `harw-dod-crate-decomposition.md` §7.

**S4.** The escalation level depends on the severity of the finding, not a
model's judgment. Deterministically decidable contract violations may act;
statistical anomalies may only warn. The same actor convention (`human:`,
`model:`, plus `warden`) applies throughout the security subsystem.
**Implemented.**

**S5.** Reversible before irreversible. Freeze before kill. cgroup before
PID. Every irreversible action requires human confirmation. **Implemented**
— `harw-dod-escalate`'s ladder.

**S6.** Security findings are operator-only visibility and never leave
local inference. An agent with security context has no network access; an
agent with network access has no security context. The two sets are
disjoint; their only meeting point is reviewed knowledge artifacts.
**Implemented** — enforced via the security agent family's `universe`
capability set and a disjointness test in `harw-agent-dsl` (see §7.1).

**S7.** Audit checkpoints are mirrored off-host. The path to alert a human
is out-of-band and works even when the monitored host doesn't. **Open** —
no off-host mirroring or out-of-band alerting mechanism was found in the
codebase as of this review; the audit chain itself
(`harw-secrets::audit::chain`) is implemented and periodically verifiable,
but mirroring/out-of-band delivery is not.

**S8.** Baselines carry provenance and validity. Promoting a normal state
to an established baseline is review-gated like any other promotion.
**Implemented.**

**S9.** What can be structurally prevented is prevented, not monitored.
Monitoring is the answer only for what must stay structurally open.
**Implemented as a design principle throughout** — see
`harw-dod-charter.md` §5.

**S10.** Telemetry fields are a contract. Field names are generated, not
typed. An undeclared field does not exist. **Implemented** — `metrics!`
macro in `harw-macros`.

**S11.** Change attribution yes, behavior scoring no. For human actors, the
ladder ends at reporting and logging; automatic intervention applies only
to deterministically decidable permission violations. **Implemented.**

---

## 2. Crate landscape

Superseded by `harw-dod-crate-decomposition.md`, which is the current,
maintained source for crate responsibilities, dependency layering, the
rights matrix, and process topology. What that document adds beyond the
original plan: a dedicated `harw-dod-sentinel` aggregator crate, split
capability/vocabulary primitives (`harw-dod-cap`, `harw-dod-readfs`,
`harw-dod-netlink`, `harw-dod-bpf`), and per-source sensor crates rather
than one monolithic `harw-sensor` crate.

Also landed, unchanged from the original plan's expectations: `harw-plan`,
`harw-plan-bridge`, `harw-research`, `harw-code-graph`, and the DSL's
organization layer (clans/cells) as described in §7.1 below.

---

## 3. Macros

All five macros are implemented in `harw-macros`, each with a trybuild
corpus.

### 3.1 `metrics!` (function-like)

```rust
metrics! {
    CPU_UTIL: gauge, unit = Percent, labels = [core], cardinality = 256;
    TURN_TOKENS: counter, unit = Tokens, labels = [role, model], cardinality = 64;
    FREEZE_ACTIVE: gauge, unit = Count, labels = [], cardinality = 1;
    WARDEN_UNAUDITED: counter, unit = Count, labels = [], cardinality = 1;
}
```

Generates `pub static` keys and a registration slice; compile errors on
duplicate names and on labels not declared via `field!`.

### 3.2 `#[traced]` (attribute on `impl` blocks)

Instruments all `pub` methods: async handled correctly via `Instrument`
rather than `enter`, field expressions lazy behind the activation check,
arguments passed through `Redact` rather than `Debug`.

### 3.3 `#[derive(SensorSource)]`

Generates `poll`, metric emission and capability declaration from an
annotated struct. **Status note:** as documented in
`harw-dod-sensor-extensibility-plan.md` §4, the shipped sensors do not
currently use this derive — its scaling/per-match/scope-relative-glob gaps
made hand-written sensors the practical choice. The macro itself exists and
compiles; it is simply unused by the current sensor set.

### 3.4 `warden_actions!` (function-like)

The most consequential of the five: declares the closed action set once
and generates the wire enum, the proposal enum, the tool JSON schema for
triage (`strict: true`), warden-side validation, the audit entry type per
action, and the skeleton of the admission matrix with a mandatory entry per
action. **Implemented** in `harw-macros`, consumed by
`harw-dod-warden-proto`/`harw-dod-escalate`/`harw-dod-warden`.

### 3.5 `#[derive(Redact)]`

Field-wise redaction strategy, default `Omitted`, so a forgotten annotation
fails safe.

```rust
#[derive(Redact)]
struct AuthAttempt {
    #[redact(plain)]  outcome: AuthOutcome,
    #[redact(digest)] remote: IpAddr,
    #[redact(omit)]   raw_line: String,
}
```

---

## 4. Observability and self-monitoring

### 4.1 The span hierarchy

```
goal (goal_statement)
└─ plan_revision (parent_revision chain)
   └─ plan_node (TaskId)
      └─ reconcile_step (AttachEvidence | Invalidate | InsertExplore | ...)
      └─ job (WorkId, LeaseToken)
         └─ session (SessionId)
            └─ turn (TurnId)
               ├─ context_assembly
               ├─ model_request (provider, model, role)
               └─ tool_call (ToolName)
                  └─ child_session (Handoff)
```

`TraceContext` (`harw-observe/src/trace.rs`) is persisted at three points
that cross process or time boundaries: `StoredJob`, `ChildLeaseRecord`,
`WardenRequest`.

### 4.2 Metrics unique to this system

- **Context and cost per plan node:** context bytes included, fragments
  omitted, history items dropped, STM evictions, signals scanned vs.
  selected, tokens in/out/cached, labeled by role and model.
- **Agent economy on two axes:** tokens per plan node, plus CPU-seconds,
  IO and memory per node via the cgroup axis.
- **Orchestration health:** active children per parent against the
  configured limit, depth against the max, expired leases, reconciliations
  on startup, spawns rejected by admission checks, zombie reaps.
- **Plan loop:** reconcile steps by kind, invalidations by condition,
  explore-insertions, evidence attached by kind, pending proposals, goal
  age, coverage as a gauge. The single most valuable metric here is the
  scope-violation rate from patch validation per time window — it rises
  when plans degrade, before a test goes red.
- **Structure self-image:** workspace member/edge/depth counts and
  structure-drift totals by change kind, from the workspace sensor
  (`harw-dod-workspace`).

### 4.3 Meta-invariants as zero-counters

For every runtime-violatable invariant, a counter whose expected value is
permanently zero. An alert on nonzero is an alert on a broken invariant,
not a threshold.

| Counter | Meaning when > 0 |
|---|---|
| `warden_unaudited_actions` | S2 violated: action without an audit entry |
| `warden_rejected_schema` | foreign protocol version on the socket |
| `escalate_inadmissible` | S4 violated: escalation level not admissible for the finding's severity |
| `sensor_capability_denied` | S3 violated: sensor with the wrong permissions |
| `security_context_egress` | S6 violated: security artifact reached network context |
| `goal_status_applied_by_model` | S4 precedent violated: a terminal goal transition applied by a model actor |
| `audit_chain_break` | S7: chain broken, suspected tampering |

`spawn_denied_by_matrix` and `goal_status_rejected_by_validation` are
deliberately not zero-counters — rejections are the mechanism working as
intended, and their rate is a health signal, not a break.

### 4.4 The feedback channel into the model catalog

`ObservedModelBehavior` is implemented (`harw-model-catalog/src/observed.rs`).
Six axes map onto measurable telemetry: tool schema reliability, long
context retention, delegation discipline, recovery after a tool error,
completion calibration, compaction resilience. The writer evaluates
windows and proposes an update; `harw-model-catalog::router::pick` can then
route by measured behavior rather than assumption.

---

## 5. Agent layer

### 5.1 Family and ceiling — updated against the implemented registry

`harwness.family.security@1` (`harw-registry-defaults/agents/families/security/security.toml`)
declares a `universe` capability set under the `security.*` prefix
(`security.sensor.read`, `security.scanreport.read`,
`security.verdict.propose`, `security.context.read`,
`security.context.curate`, `security.advisory.correlate`) — no
`NetworkAccess`, no write tools, no shell tools, checked by a disjointness
test against other families in `harw-agent-dsl`.

**Correction to the original roster.** This document originally proposed
four specializations named `security-triage`, `baseline-curator`,
`drift-explainer` and `incident-correlator`, with `incident-correlator` as
the security clan's leader. **The implemented roster differs:** the clan's
four worker specializations, one per event category actually wired to a
rule in `harw-dod-rules`, are:

| Role | Triages |
|---|---|
| `security-egress-triage` | network findings (`EgressFlow`/`ListenerOpened`), against `EgressFlowRule` |
| `security-baseline-triage` | baseline deviation (`HostSample` vs. `Baseline`), against `BaselineDeviationRule` |
| `security-structure-triage` | structure drift, against `StructureDriftRule` |
| `security-endpoint-triage` | process exec, file write and auth events bundled together — the three raw event kinds with no dedicated rule of their own, so no single one would have anything to triage in isolation |

The clan's **leader is `context-steward`**, not a separate
`incident-correlator` role (which does not exist in the current
registry) — `context-steward` plays the same consolidating role for
security that `analyst` plays for the research family. `intel-scout`
remains organizationally outside the clan, preserving S6 as a structural
statement, not only a ceiling statement.

All four triage roles forbid `fs.write`/`shell.exec` tools and inherit the
`harwness.security-verdict/v1` return contract from the family's defaults
rather than declaring their own.

### 5.2 Context policy

Representative shape (see the `security-*-triage` role definitions for the
exact, current values):

```toml
[budget]
tokens = 6000
reserved_for_output = 1500

[must_include]
items = ["finding.event", "finding.hardness", "contract.declared",
         "baseline.matching", "host.identity"]

[exclude]
items = ["full_parent_transcript", "sibling_transcripts",
         "other_hosts.findings", "secrets.any", "raw_sample_ring"]

[details]
mode = "references"
load_on_demand = true
```

Reference-based, capped by the recall limits described in
`harw-lens-plan.md`; a triage turn stays within a few thousand tokens,
which local inference depends on.

### 5.3 Intel-scout via the research contract

`intel-scout@1` deliberately sits outside the security family and uses the
existing `harw-research` return contract unchanged: a bound question per
observed package, a finding bundle back, matched against `Cargo.lock`
locally via `harw-code-graph`'s lockfile lookup after the bundle has been
reviewed as a knowledge artifact. Network and security context never touch:
the scout sees package names, never findings; the rule layer sees
artifacts, never the network. **Implemented.**

### 5.4 Injection boundary

Every value from a `SecurityEvent` is attacker-controlled: filenames,
process names, log lines, SNI, usernames, package names, advisory text.
Event data lives in a labeled data block, never the instruction part; the
verdict is a strict tool call against a versioned schema, with a
fence-tolerant fallback parser matching the `harw-research` pattern;
`rationale` is a stored field, never parsed or executed. **Implemented** —
see `harw-dod-integration-and-dependencies.md` §1.2 for the shared
mechanism (`TrustClass`, the two-block renderer).

### 5.5 Return contract

`harwness.security-verdict/v1` with required fields (`finding_id`,
`assessment`, `confidence`, `rationale`, `evidence`, `proposed_action`,
`state_revision_after`), evaluated via `harw-core-bridge`'s
`ChildReturnContract`. **Implemented.**

---

## 6. Test strategy

- **Macros:** trybuild compile-fail corpus for all five — `warden_actions!`
  without an admission rule fails, `metrics!` with an undeclared label
  fails, `#[traced]` on an `async fn` produces `Instrument` not `enter`.
- **Typestate:** one compile-fail case per skipped state (`Finding<Triaged>`
  from `Raw`, `Action<Authorized>` outside `harw-dod-escalate`,
  `SensorHandle<Bound>` without `bind`).
- **Adversarial fixtures:** log-line injection, filename injection, process
  argv injection, advisory-text injection, a fenced verdict parsed
  correctly and rejected with unknown fields, a ceiling-escalation attempt
  denied by the resolver, an unauthorized warden request without a proof.
- **State machines:** property tests over freeze/lease transitions,
  including reconciliation after a simulated restart.
- **Rule layer:** golden cases per rule, since rules are pure functions
  with an injected clock.
- **Plan attachment:** an integration case running a synthetic contract
  violation through the bridge, checking exactly one evidence attachment
  and one invalidation proposal result, no applied goal transition, no
  second apply path.

---

## 7. Decisions

### 7.1 Time library

Resolved by how the codebase evolved: `harw-plan` stayed on `time`;
`harw-plan-bridge` is the one documented seam that converts. The eight (now
more) security crates are uniformly `jiff`.

### 7.2 `EvidenceRef`: not content-addressed

`EvidenceRef` (in `harw-plan`) carries `kind`, `locator`, `attached_at`,
`actor`, with an additive optional `digest: Option<ContentDigest>` field
(`serde(default)`) for security evidence, rather than a move to
`harw-types`. **Implemented.**

### 7.3 IPC transport to the warden

Unix socket with peer-credential checking (`SO_PEERCRED`) and versioned
framing, systemd-activated. **Implemented** — see
`harw-dod-crate-decomposition.md` §7.

### 7.4 Cardinality policy

`SampleScope::Process` with an executable digest is potentially
high-cardinality. **Open** — the `Cardinality` marker mechanism exists
(`harw-telemetry-integration-plan.md` §3), but a specific policy for this
axis (aggregate vs. reduce to the cgroup axis) is not pinned down.

### 7.5 Multi-host

Deliberately out of scope: everything here is single-host with advisory
locks. `HostId` exists as a type; cross-host aggregation would need
different persistence.

### 7.6 Naming

`harw-sentinel` and `harw-warden` as separate binary crates, `harw-dod` as
the facade name — chosen over a "department"-style name because a
department implies a power center, and the opposite was built (see
`harw-dod-charter.md` §2).

---

## 8. What this system deliberately does not include

- No kernel module of its own — all capabilities go through stable
  interfaces (fanotify, auditd-netlink, landlock, eBPF via aya, cgroup v2,
  netlink).
- No in-house virus/rootkit detection logic — established tools run on a
  timer, the sentinel reads their reports.
- No replacement for deterministic reflexes — packet filters, rate
  limiting, integrity checks stay kernel/rule-engine territory; the agent
  replaces the judgment layer above them, not them.
- No plan state of its own — findings live in `harw-knowledge`, plan effect
  runs exclusively through `harw-plan-bridge`.
- No behavioral scoring of people — change attribution yes, profiling no
  (S11).

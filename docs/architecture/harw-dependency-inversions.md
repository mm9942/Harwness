# Harw dependency inversions (Migration Phase 2)

> **Status:** descriptive, R11. Derived from the ring assignment in
> `harw-workspace-inventory.md` and from the symbols that actually cross each edge.
> Plan reference: Eco-Doc §6, §14, §48, §57–§59.
> **Enforcement:** `xtask/arch-policy.toml` + `xtask` arch gate. Edges that are still
> open are listed there as explicit, temporary exceptions. This file explains why each
> one exists and how it goes away.

Fix vocabulary (Eco §48): **move type inward**, **adapter**, **split crate**,
**invert trait**, **remove dep**. A facade that only hides the edge does not count as a fix.

## 1. Ring inversions (the edge breaks the direction law)

Ring rule: F→F; I→F,I; C→F,I,C; J→F,I,J; D→F,I,D; A→anything.
Rings are the ones in `harw-workspace-inventory.md`. `arch-policy.toml` puts `harw-session-store` in I and
`harw-agent-runner`/`harw-lens-*` in A, so R2, R4 and R6 are ring-legal there. The coupling they describe remains.

| # | Edge | What crosses | Why it is an inversion | Proposed fix | Wave |
|---|---|---|---|---|---|
| R1 | `harw-agent-compiler` (C) → `harw-registry-defaults` (A) | `capability_catalog`, `embedded_agents`, `roster`, `agent_definition_tools` | The compiler reads the Harwness default registry, so it cannot be reused outside Harwness (Eco §11). | **Move type inward**: put the capability-catalog/roster *contract* in the compiler domain (or in `harw-catalog`, I). `registry-defaults` supplies the data through an **adapter**/input artifact (Eco §35). | later |
| R2 | `harw-agent-runner` (C) → `harw-core`, `harw-runtime`, `harw-tui`, `harw-registry-defaults`, `harwness-sdk` (A) | `child_backend::ChildBackend`, `EmbeddedAgent`, `run_tui`, `fixed_agent`, registry roster | The "runner" is actually a full Harwness composition binary that embeds the interactive app. | **Split crate**: `harw-agent-runner-core` (C: child protocol, artifact load, rights frame) and a composition binary (A). The core defines a runner-host trait (**invert trait**) that the runtime implements. | later |
| R3 | `harw-agent-runner` (C) → `harw-tool-job` (A) | `JobManager::start_piped`, `JobManagerConfig`, `LogQuery`, `NoopNotifier`, `procfs` | The compiler runtime launches child processes through a Harwness *tool* crate. | **Adapter**: point `job_child_backend.rs` at the `harw-job` facade (J) once it exists. It then needs only spawn, piped stdio, log and stop. | R11 W2+ (after the `harw-job` facade lands) |
| R4 | `harw-dod-escalate` (D) → `harw-session-store` (A) | `FreezeStore`, `SessionStoreError` | The DoD domain depends on Harwness session persistence. It also drags `harw-job-runtime`, `harw-observe` and `harw-macros` into the DoD closure. | **Invert trait**: `harw-dod-escalate` owns a `FreezeSink` trait. `harw-session-store` (or `harw-plan-bridge`) implements it. | later |
| R5 | `harw-config` (I) → `harw-agent-dsl` (C) | `parse`, `raw`, `layers`, `bind`, `lower_v*`, `diagnostics`, `roles` | Infrastructure (config loading) depends on the compiler front end, so all 13 dependents of `harw-config` pull in the DSL. | **Move out / adapter**: `harw-config` keeps raw TOML layers. Agent-definition parsing/lowering moves to a compiler-side loader that composition wires in. | later |
| R6 | `harw-lens-source` (I) → `harw-knowledge` (A) | `KnowledgeStore`, `KnowledgeIndex`, `KnowledgeArtifact`, `ArtifactKind`, `VisibilityScope`, `AgentId`, `PalaceNode` | The generic retrieval engine ingests a Harwness domain store directly. | **Adapter**: lens-source defines a `SourceDocuments` trait. The knowledge-source implementation moves into `harw-knowledge` or `harw-runtime`. | later |

## 2. Semantic inversions in the job layer (the ring allows the edge, Eco §14/§57/§58 forbid it)

These edges are allowed by ring (J→F/I) but put Harwness application semantics into the
lowest job layer. In the working tree they now live in `harw-job-core`, moved unchanged
from `harw-job-runtime`, and are listed under "Known inversions" in `harw-job-core/src/lib.rs`.

| # | Edge | What crosses | Why it is an inversion | Proposed fix | Wave |
|---|---|---|---|---|---|
| J1 | `harw-job-runtime`/`harw-job-core` → `harw-types` (`TenantId`, `WorkspaceId`, `ApprovalActor`) | `JobScope { tenant, workspace, submitter }`, `JobCancellation::cancelled_by` | Job core carries Harwness tenant/workspace/approval identity (Eco §57 "bad" example, verbatim). | **Move type inward + adapter**: generic `JobScopeId`/opaque authority token in job-core (already exists in `ids.rs`). A Harwness admission adapter (session-store/core) maps `TenantId`/`WorkspaceId`/`ApprovalActor`. The on-disk `StoredJob` JSON stays byte-compatible through the adapter. | **R11 W2** |
| J2 | `harw-job-runtime`/`harw-job-core` → `harw-observe` (`TraceContext`) | `StoredJob::trace: Option<TraceContext>` | Tracing is cross-cutting, not job identity (Eco §58). The edge pulls the observe crate (and today `harw-authority`, see X1) into the job core closure. | **Adapter**: core stores an opaque, versioned correlation/metadata slot (or no trace at all). The Harwness store adapter keeps `TraceContext` in its own record envelope. Alternative: move `TraceContext` into an F vocabulary crate. | **R11 W2** |
| J3 | `harw-job-runtime`/`harw-job-core` → `harw-types::WorkId` | job identity in `Job`, `Lease`, `LeaseToken`, `JobRuntimeError` | The job id is a Harwness knowledge-surfaces type (§6.1/§8.1). | **Move type inward**: a job-core id (`JobId`). Harwness keeps `WorkId` and converts at the adapter. `harw-job-runtime` keeps re-exporting `WorkId` during migration. | **R11 W2** |
| J4 | `JobKind::{Dream, Worker}` in job core | enum variants | Harwness feature names (dream consolidation, kanban lanes) in the generic model. | **Move type outward**: core has an opaque `kind: JobKindName` (validated string). `Dream`/`Worker` become Harwness constants. | **R11 W2** |
| J5 | `serde_json::Value` in `StoredJob::input` / `JobOutcome::Succeeded` | untyped payload | Eco §59: serialization format leaks into the core contract. | Use `JobSpecEnvelope` (versioned, typed; already in `harw-job-core::spec`). Keep `Value` only inside the Harwness adapter. | **R11 W2** |
| J6 | Two job models: governance `JobState` (Pending/Ready/Running/…) vs `harw-tool-job::JobState` (process job) vs new `LifecycleState` | parallel state machines | Duplicate semantics. `harw-tool-job` has its own `JobId`/`JobState`/`meta.json`. | Converge on `harw-job-core::LifecycleState` and map the other two onto it. See `job-extraction-map.md`. | **R11 W2** (mapping); removal later |
| J7 | `harw-session-store` (A) owns generic durable job mechanics (`job_store.rs`) | `JobStore`, fs4 locks, `records/locks/approvals` | Not a bad edge, but the owner is wrong (Eco §33): generic jobs depend on the existence of a Harwness session store. | **Split crate**: generic store → `harw-job-store`. session-store keeps a thin adapter with the same API and the same on-disk layout. | **R11 W2** |
| J8 | Generic process supervision lives in `harw-tool-job` (A) | procfs identity, pgrp kill, `meta.json` | Same wrong owner at the process level. `harw-agent-runner` (R3) and `harw-runtime` depend on the tool crate for plain supervision. | **Split crate**: see `job-extraction-map.md`. | **R11 W2** onward |

## 3. Other semantic inversions and cleanup

| # | Edge | Why | Proposed fix | Wave |
|---|---|---|---|---|
| X1 | `harw-observe` (I) → `harw-authority` (I) | **Unused**: the only reference is a doc comment (`harw-observe/src/routing.rs:578`). It still puts `harw-authority` (+ `blake3`/cc) into the closure of every observe consumer, including `harw-job-runtime`/`harw-job-core`, `harw-dod-signals` and so all DoD sensors. | **Remove dep**. | later (trivial; worth doing with W2 because it shrinks the job-core closure) |
| X2 | `harw-types` (F) → `tokio-util` (`CancelToken`) + Harwness identity types | A foundation carries runtime behaviour and application vocabulary (Eco §10). | **Split crate** (value types vs runtime/Harwness types), only where the graph improves. | later |
| X3 | `harw-dod-rules` (D) → `harw-research` (I, agent-research vocabulary) | Uses only `harw_research::Confidence`. DoD depends on Harwness sub-agent vocabulary for one enum. | **Move type inward** (`Confidence` to an F crate) or give DoD its own type plus a conversion. | later |
| X4 | DoD → Harwness-flavoured I crates: `harw-sentinel` → `harw-home`, `harw-completions`, `harw-observe-file`, `harw-sandbox`. `harw-dod-scanreport` → `harw-home`. `harw-dod-{flow,netpolicy}` → `harw-sandbox`. `harw-dod-cap`/`-rules`/`-sentinel` → `harw-authority`. | Ring-legal (D→I), but `harw-home` ("where does `harw` live") and `harw-sandbox` (bwrap) are Harwness-shaped. They count against the DoD TCB budgets. | Review each crate against the Warden/probe budgets. Keep them out of `harw-warden` (true today: its closure is completions, dod-warden(-proto), macros, types). | later |
| X5 | A → D, cross-domain: `harw-core-bridge` → `harw-dod-signals`. `harw-knowledge` → `harw-dod-{rules,signals}`. `harw-plan-bridge` → `harw-dod-{escalate,signals}` | Ring-legal (A composes everything), but `harw-knowledge` is a Harwness *domain* crate that depends on DoD. It also makes the root workspace build path-dependent on `dod/crates/*` before the merge. | Keep the edges in bridge/composition crates only. Move `harw-knowledge`'s DoD use (`harw_dod_rules::baseline::{Baseline, PalaceStatus}`, `harw_dod_signals::{Severity, Hardness}`) into `harw-plan-bridge` or a dedicated adapter. | later |
| X6 | `harw-macros` (F) dev-deps on ~all crates | trybuild/positive tests. Dev only, never in shipped closures. | None. Documented exception (`gate_privileges.rs` ignores dev-deps). | — |
| X7 | `harw-provider-http` (A) → `harw-core` | The provider transport depends on the agent engine. Placed in A for that reason. If providers are ever to be I, this edge must go. | **Invert trait** (core-owned provider trait moves to `harw-provider`). | later |

## 4. Wave summary

| Wave | Edges |
|---|---|
| R11 W2 (job extraction) | J1–J8, R3 (once facade exists). X1 is recommended alongside. |
| later | R1, R2, R4, R5, R6, X2–X5, X7 |

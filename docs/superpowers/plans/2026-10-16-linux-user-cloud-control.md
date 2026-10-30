# Linux User Workload and Cloud Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** make user-associated Linux workloads observable and safely governable through Harw's existing job and authority model, then connect node-local control to an authenticated, durable cloud control plane.

**Architecture:** local inventory and capacity services remain authoritative for their host; existing job, session, DoD and identity components keep their lifecycle responsibilities. Cloud services route and admit work without becoming a second kernel, session writer or unrestricted executor. Observe external applications first; add scoped controls only after identity, ownership and recovery tests pass.

**Tech Stack:** the repository-pinned Rust toolchain/edition, existing Tokio/serde/record-store patterns, Linux procfs/eBPF/pidfd/cgroup v2, systemd manager interfaces on supported hosts, existing session/node transport and OCI/K8s executors. No new model provider, kernel module or cloud provisioning API is introduced by default.

**Spec:** [Linux User Workload and Cloud Control](../../planning/linux-user-cloud-control/README.md), especially [host contract](../../planning/linux-user-cloud-control/03-host-process-contract.md), [cloud contract](../../planning/linux-user-cloud-control/04-cloud-control-contract.md), [Rust integration](../../planning/linux-user-cloud-control/05-rust-integration.md) and [verification matrix](../../planning/linux-user-cloud-control/06-verification-and-rollout.md).

## Global Constraints

- Baseline B is `6e34f2e32779da20eb6b8d49e8cdedd83d27fb0a`; N is a candidate source, never an implicitly approved merge.
- This document is a proposed implementation sequence. No checklist item below is reported executed by the design PR.
- Preserve `SECURITY.md`, including Full Access confirmation behavior and explicit deny/scope boundaries.
- The root workspace has one lockfile and includes DoD; never add untracked cloud stubs to simulate a successful clean build.
- Rust interfaces in the spec are proposed names; implementations must use existing IDs/error/future conventions without duplicating authority types.
- No unsafe code or new production panic/unwrap/expect paths. Keep privileged dependencies within the existing TCB/ring gates.
- Application workload commands use CommandPort; infrastructure exceptions remain explicit and bounded.
- Observed process identity is not control authority; a pidfd is not serializable durable identity or parental ownership.
- No unapproved deployment, service restart, real BPF load, cloud spend, branch deletion, PR merge or permission widening.
- Build/test work is centrally coordinated on one frozen integration SHA; parallel code writers do not run competing Cargo builds on the Pi.
- Every new feature is opt-in and unavailable rather than silently weakened when a required service/capability is absent.

## Review Focus

1. **Fork/thread/exec ambiguity:** one task exit or reused PID must not terminate the wrong process record; owned by W02/W03, H02-H04.
2. **Non-default profile and registry compatibility:** canonical six/seven-field device rows and bound home must survive new cloud adapters; owned by W01, C02.
3. **Multiple Harw instances:** process-local permits must not be mistaken for a node-wide resource ceiling; owned by W06, J01/J03.
4. **Disconnect and ambiguous effects:** client loss is not job cancellation, while expired authority cannot resume a mutation; owned by W07/W10/W11, J05-J06/C07-C13.
5. **Real-world partial enforcement:** an API request, a listener and a successful parse are not proof of restrictions or readiness; owned by W01/W08/W11/W14, C04/C10 and the real-engine matrix.

## Task and dependency graph

```text
W00 baseline repair
  +-> W01 canonical cloud adapters --------------------+
  +-> W02 lifecycle vocabulary -> W03 inventory -------+-> W05 scoped external control
  |                                  +-> W04 job correlation -> W06 host capacity
  +-> W07 durable-agent recovery                       |
  +-> W08 identity/profile/readiness <- W01             |
          +-> W09 session-owner directory              |
          +-> W10 placement/reservation <- W06 --------+
                  +-> W11 backend/artifact execution <- W07/W09
W12 inference capacity/privacy can proceed after W00 independently
W13 UI/client convergence <- W03/W04/W09 (and W05 for mutation)
W14 frozen integration + field acceptance <- all enabled slices
```

One work package may produce several small commits, but each reviewable slice ends in a failing test, minimal implementation, passing test and recorded commit. Do not build a giant implementation PR that combines every branch simply because this is a unified design.

## W00 — Restore a reproducible tracked baseline

**Files:** root `Cargo.toml`, `Cargo.lock`, the five `harw-cloud` manifests/source roots, `xtask/arch-policy.toml`; existing `harw/tests/{version_coherence,license_coherence}.rs` and deployment parity checks.

**Consumes:** B and the exact N source snapshot. **Produces:** a clean tracked base with honest placeholder/availability declarations, not a complete Cloud Home.

- [ ] **Red check:** on a clean B checkout, record the missing-member result from `cargo metadata --locked --no-deps --format-version 1`. Do not create temporary member packages.
- [ ] **Implement:** extract the legitimate cloud source restoration into a dedicated branch. Reconcile workspace inheritance, package versions, feature/dependency declarations and architecture classification. Exclude unrelated generated declarations, private notes, papers and UI changes.
- [ ] **Green checks:** run locked metadata, version/license coherence, touched-crate checks and architecture/dependency gates. Every missing service remains explicitly unavailable; no stub returns a fabricated success.
- [ ] **Commit:** `fix(workspace): restore tracked cloud members with coherent metadata`. Record source PR/head and exact extracted file set. Do not merge N wholesale.

**Closes:** LUC-01-LUC-05 only to the extent demonstrated. Remaining cloud behavior is W01/W08 onward.

## W01 — Canonical cloud registry, service operations and status

**Files:** `harw-cloud/harw-cloud-ops/src/{enrollments,cloudctl,status,lib}.rs`, `harw-cloud/harw-cloudctl/src/main.rs`, `harw-cloud/harw-cloud-gateway/src/{config,status}.rs`; add composed tests under `harw-cloud/harw-cloud-ops/tests/`.

**Consumes:** existing bound home/profile and DeviceRegistry; proposed typed service interfaces from Rust integration sections 6-7. **Produces:** correctly scoped operations and phased readiness, with no ambient default-profile fallback.

- [ ] **Red tests:** C02 with `node-a|device-a|tenant-a|operator|active|phone|approve` under a non-default profile; assert node/device fields and opt-in survive. C03 asserts device revoke does not call a service enable/disable verb. C04 supplies an unrelated TCP listener and expects reachable but not ready.
- [ ] **Implement:** replace duplicate parsing/path discovery with injected canonical registry access. Split device identity operations from service lifecycle. Reconcile executable/unit names with deployment assets. Use a bounded service/command port, not blocking unbounded `Command::output` in async handlers.
- [ ] **Green checks:** `cargo test -p harw-cloud-ops -p harw-cloud-gateway -p harw-cloudctl`; exercise real operation registration with fake native services, not the actual machine's systemd. Add absence/cancellation/output-budget tests.
- [ ] **Commit:** `fix(cloud): use canonical registry context and truthful control semantics`.

**Closes:** LUC-06-LUC-11/LUC-13 for the repaired operations. Pairing and real proxy serving remain separate tasks.

## W02 — Versioned lifecycle facts and identity contract

**Files:** add `harw-host-model/src/{lib,identity,observation,workload,query}.rs`; change `harw-dod/crates/harw-dod-bpf/src/{abi,contract,profile}.rs`, relevant `harw-dod/bpf/src/` programs and minimal signal/sink DTO code. Shared manifests/gates are edited only by the integrator.

**Consumes:** existing v1 ABI and process observations. **Produces:** the proposed ProcessInstanceRef, SourceCursor, CoverageState and validated lifecycle envelopes; no control authority.

- [ ] **Red tests:** H02-H04 and H06: fork without exec, program replacement, leader/thread exit, reused PID/new boot and newly created cgroup scope. Add explicit unknown-version/length/reserved-field rejection fixtures.
- [ ] **Implement:** version the new lifecycle record, retain birth/namespace/source evidence where supported, and distinguish task exit from process termination. Keep unsupported identity facts explicitly unknown. Extend push-only observation delivery without adding a probe command receiver or application/runtime dependency.
- [ ] **Green checks:** `cargo test -p harw-host-model -p harw-dod-bpf -p harw-dod-procmon`; run defined BPF ABI fixture checks and dependency gates. Real hook attachment is a separately authorized integration test, not an ordinary unit-test side effect.
- [ ] **Commit:** `feat(observe): version process lifecycle and coverage facts`.

**Closes:** LUC-14-LUC-17 only for supported source dimensions. Preserve existing v1 security consumers until a tested package migration.

## W03 — Node-local inventory with bootstrap and reconciliation

**Files:** add `harw-host-inventory/src/{lib,reducer,bootstrap,reconcile,store,service,proc_source,dod_source,unit_source}.rs` and `tests/{bootstrap,replay,coverage}.rs`.

**Consumes:** W02 model and existing bounded low-level procfs/DoD readers. **Produces:** HostInventoryPort, revisioned InventoryPage and a single node-local materialized inventory; query limits do not define inventory completeness.

- [ ] **Red tests:** H01/H05/H07/H08; create preexisting fixtures, more than 2,000 entries, a lost source interval, duplicate/out-of-order events and permission loss. Assert no invented jobs/exits and visible degraded coverage.
- [ ] **Implement:** subscribe-before-scan bootstrap, per-source watermarks, incarnation-safe pure reducer, bounded journaling/checkpointing and periodic/on-gap reconciliation. Add scoped pagination and terminal tombstones. No model/provider dependency.
- [ ] **Green checks:** `cargo test -p harw-host-inventory`; deterministic replay produces the same inventory revision/content for the same accepted observations. Memory/queue limits report degradation instead of silently dropping truth.
- [ ] **Commit:** `feat(host): reconcile user workload inventory from snapshots and events`.

**Closes:** LUC-18 and read-only portions of LUC-19/LUC-20/LUC-40. Ship this usable slice before external controls.

## W04 — Correlate existing jobs without duplicating lifecycle

**Files:** `harw-runtime/src/{job_wiring,agent_job_wiring}.rs`, `harw-tool-job/src/{model,event,manager}.rs`, existing coordinator event seams; add `harw-host-inventory/src/job_source.rs` and composition tests.

**Consumes:** existing WorkId/AttemptId/lease events and W03 inventory. **Produces:** job-attempt-to-process/engine correlation and one logical workload view.

- [ ] **Red tests:** J03/J09. One build has several compiler processes; a compiled agent has both logical and process projections; an external process with matching text must not join that job. Unknown exit status is not success.
- [ ] **Implement:** publish direct executor/launch identity from the composition layer, retain authoritative job results, and join only validated identities. Keep J/F layering intact; do not make the coordinator depend on the inventory application.
- [ ] **Green checks:** `cargo test -p harw-tool-job -p harw-runtime -p harw-host-inventory`; verify owner cancellation and notifications still use the existing job/agent delivery chain.
- [ ] **Commit:** `feat(host): correlate inventory with durable job attempts`.

**Closes:** correlation portions of LUC-20/LUC-23/LUC-39. Observed external work still has no Harw restart recipe.

## W05 — Scoped native control of external workloads

**Files:** add `harw-host-control/src/{lib,admission,target,systemd,effects}.rs`; add `harw-ops/src/host.rs`; integrate approved capabilities/operations at the existing registry/assembly seams. Do not expand Warden's dependency closure with the host application.

**Consumes:** W03 inventory, trusted user scope and existing authorization/confirmation policy. **Produces:** HostControlPort and admitted, identity-bound actions with read-back.

- [ ] **Red tests:** H09-H12/U02. Same-UID discovery is not a grant; a reused PID/changed unit invocation refuses the old action; a protected user bus/compositor is not automatically stopped; external handles never imply reaping ownership.
- [ ] **Implement:** separate lifecycle owner and control grant. Use retained live target handles, systemd/engine owner adapters and an action/effect journal. A missing required adapter returns unavailable rather than invoking an unrestricted shell.
- [ ] **Green checks:** `cargo test -p harw-host-control -p harw-ops`; `ask`/`auto` preserves required confirmation and `full` bypasses confirmation only, never explicit denial or identity checks. Test partial/unknown read-back.
- [ ] **Commit:** `feat(host): admit scoped workload actions through native owners`.

**Gate:** independent closure of relevant previously public security review blockers before enabling new external actuation. **Closes:** LUC-19-LUC-21, action portions of LUC-37/LUC-40.

## W06 — Shared node capacity and explicit executor resources

**Files:** `harw-host-control/src/{capacity,reservations,governor}.rs`, `harw-command/src/port.rs`, `harw-runtime/src/{job_wiring,assembly}.rs`; proposed `harw-config/src/host_control_toml.rs` plus existing configuration scope/default wiring.

**Consumes:** existing job admission, W03 pressure observations, W04 attempt identity and actual executor capability reports. **Produces:** NodeCapacityPort and explicit executor configuration.

- [ ] **Red tests:** J01/J02/J07/J08. Two independent clients with host capacity one cannot both launch; no cgroup root refuses a required memory cap; virtual-memory and resident-memory conditions are distinct; noisy pressure cannot oscillate controls.
- [ ] **Implement:** one host reservation owner, atomic starting/running accounting, lease/effect reconciliation, validated delegated root and dimension-specific enforcement. Preserve local StartSlot reservations as defense in depth. Limit automatic policy to declared own-job actions first.
- [ ] **Green checks:** `cargo test -p harw-host-control -p harw-command -p harw-runtime -p harw-config`; run an actual two-process reservation test. Use a delegated disposable cgroup for the optional kernel test, not the developer's whole user slice.
- [ ] **Commit:** `feat(jobs): share node reservations and report applied resource limits`.

**Closes:** LUC-22/LUC-23/LUC-25; provides measurable inputs for LUC-26.

## W07 — Durable agent restart and ambiguous effects

**Files:** `harw-runtime/src/agent_job_wiring.rs`, `harw-core/src/durable_job_runner.rs`, `harw-cli/src/job_worker.rs`; add a focused agent-recovery module and integration tests rather than further expanding the worker monolith.

**Consumes:** existing #131 recovery envelope, authority reissuer and job store; W04 correlation and W06 reservation records. **Produces:** explicit startup reconciliation and a reviewed agent-job rehydration path.

- [ ] **Red tests:** J05/J06: crash at Pending, ownership transfer, Ready, claimed execution and before terminal commit; deny fresh policy/IR/capability mismatch; drop an effect acknowledgement.
- [ ] **Implement:** restore only reconstructable work after fresh authority/IR checks. Mark unreconstructable records visibly terminal or blocked with an owner/action path. Reconcile effects before retry. Do not equate lease-fenced records with exactly-once external behavior.
- [ ] **Green checks:** `cargo test -p harw-core -p harw-runtime -p harw-cli`; cover one real child lifecycle through the command runtime, then restart with a changed policy and verify refusal.
- [ ] **Commit:** `fix(jobs): reconcile durable agent ownership across restart`.

**Closes:** LUC-24 and recovery parts of LUC-35. Keep #131 semantics instead of creating another agent scheduler.

## W08 — Enrollment transactions, profile mounts and authenticated readiness

**Files:** `harw-node-listener/src/{identity,revoke}.rs`, `harw-cloud/harw-cloud-enroll/src/{service,store}.rs`, proposed `harw-cloud/src/profiles.rs`, existing CLI/session listener and cloud status composition; tests per service.

**Consumes:** W01 canonical adapters, existing AuthHub/pinned peers and reviewed #90 profile model. **Produces:** EnrollmentPort, transactional registry updates, live revocation receipts and real profile/readiness checks.

- [ ] **Red tests:** C05/C06/C17: concurrent single-use enrollment, interrupted pin/registry commit, exact expiry boundary, revoked live/queued requests, and CloudHub denied worker/engine capabilities.
- [ ] **Implement:** one transaction boundary over canonical identity stores; typed expiry and policy-bound issuance; profile-specific service mounts; staged readiness requiring the expected authenticated service, not just a port. Document private security review for new protocol details.
- [ ] **Green checks:** `cargo test -p harw-node-listener -p harw-cloud-enroll -p harw-cloud`; perform composed listener tests and distinguish those from real deployment. No production pairing window is opened by tests.
- [ ] **Commit:** `feat(cloud): bind enrollment and readiness to canonical node policy`.

**Closes:** LUC-10-LUC-13 and profile/revocation portions of LUC-30/LUC-35.

## W09 — Canonical session-owner directory and safe attach

**Files:** add `harw-cloud/src/directory.rs`; modify `harw-cli/src/{session_serve,session_serve_remote,session_listener}.rs`, `harw-session-daemon/src/compose.rs`, `harw-session-host/src/{host,identity}.rs` and remote attach tests.

**Consumes:** W08 authenticated caller/profile, existing SessionHost/SessionPort/transcript/approval mechanisms. **Produces:** SessionDirectory and directory-routed same-owner attach with per-connection capabilities.

- [ ] **Red tests:** C07/C08: two devices one session one model execution; read-only attach cannot resume; old generation after hub restart refused; unavailable owner does not spawn a replacement writer.
- [ ] **Implement:** durable generation authority separate from the location cache. Preserve local/remote rights while converging via an explicit owner, not a shared storage path. Inject optional ToolHost services explicitly; absence still refuses tool execution.
- [ ] **Green checks:** `cargo test -p harw-cloud -p harw-session-host -p harw-session-daemon -p harw-session-remote -p harw-cli`; reconnect/replay and approval identity through actual transport composition, not only a fake port.
- [ ] **Commit:** `feat(cloud): route sessions through fenced owner directory`.

**Closes:** LUC-27/LUC-28 and directory parts of LUC-30/LUC-35. No cross-node live failover in v1.

## W10 — Placement with host-side reservation and refusal

**Files:** `harw-placement-model/src/{lib,offer}.rs` and proposed requirements types; `harw-cloud/src/{placement,dispatch}.rs`; node admission integration outside J.

**Consumes:** W06 NodeCapacityPort, W08 lifecycle/policy, W09 directory and existing RuntimeOffer producers. **Produces:** FleetJobPort returning the existing durable job handle after trusted admission.

- [ ] **Red tests:** C09/C13: stale or drained node, wrong tenant, incompatible target, unapproved rootful backend, missing required enforcement, concurrent reservation and no candidate. All must refuse before any backend start.
- [ ] **Implement:** hard filters, deterministic score/tie order, bounded reservation acquisition/release and a node-side revalidation step. Preserve provider pacing independently; no permit held during indefinite host queueing.
- [ ] **Green checks:** `cargo test -p harw-placement-model -p harw-cloud -p harw-host-control`; demonstrate that a high-scored forbidden candidate never runs and a lost reservation acknowledgement is reconciled.
- [ ] **Commit:** `feat(cloud): place admitted jobs with node-local capacity leases`.

**Closes:** LUC-29 and placement portions of LUC-35. Cost/latency estimators must report unknown rather than inventing measurements.

## W11 — Backend mounting, job-owned boot and verified artifacts

**Files:** existing `harw-job-executor-{oci,k8s}` configuration/offer/executor modules, `harw-cloud/src/{dispatch,artifacts}.rs`; add a job-owned worker-boot module and backend factory at composition; target service configuration/deployment tests.

**Consumes:** W07 recovery, W10 reservations, existing container policy/read-back and CommandPort. **Produces:** an actual selected remote executor, single-flight activation and scoped verified input/output artifacts.

- [ ] **Red tests:** C10-C12/J06: missing or overridden timeout, failed inspect, mismatched immutable instance, concurrent first demand, lost launch acknowledgement, wrong artifact digest/architecture/tenant.
- [ ] **Implement:** mount the existing executors rather than resurrecting old Podman implementations. Preserve create/read-back/verify/start. Key boot by immutable inputs and policy; journal uncertain effects. Verify artifact source/target/provenance before use.
- [ ] **Green checks:** `cargo test -p harw-job-executor-oci -p harw-job-executor-k8s -p harw-cloud`; separately run authorized real rootless-engine and actual Kubernetes execution cases. Record API-only coverage separately. Isolate #147's genuine missing timeout delta rather than merging its broad head.
- [ ] **Commit:** `feat(cloud): execute verified job backends and artifact contracts`.

**Closes:** LUC-31-LUC-33. No automatic provider machine purchases or live browser migration.

## W12 — Inference capacity, cache scope and context parity

**Files:** `infra/cloudflare/mias-lab/src/{index,adaptive}.js` and tests, existing provider budget/routing/wire modules, relevant context assembly tests.

**Consumes:** existing #129/#138 worker behavior and previous #143 provider-wire findings. **Produces:** measured end-to-end permit/retry/cache contracts and correct selected-evidence serialization.

- [ ] **Red tests:** C15/C16/C18: stream cancellation, coalesced subscribers, queue expiry, shared capacity responses, duplicate retries, disallowed cache sharing and missing data block on each provider schema.
- [ ] **Implement:** use bounded total retry budgets and terminal-path permit accounting; authorize cache sharing before lookup; preserve selected host evidence as data with existing trust classes. Do not confuse model quota with node CPU capacity.
- [ ] **Green checks:** `npm run check && npm test` in `infra/cloudflare/mias-lab`; run affected Rust provider/context tests from the root. Fake binding and wire golden tests precede any staged live validation; no automatic Worker deploy.
- [ ] **Commit:** `fix(inference): preserve scoped evidence and bounded request capacity`.

**Closes:** LUC-36 and relevant LUC-38/LUC-40. Record which previous audit findings were actually reproduced/fixed.

## W13 — Unified operator projections and clients

**Files:** `harw-ops/src/host.rs`, additive `harw-protocol/src/host_wire.rs`, proposed `harw-tui/src/app/host_glue.rs`, existing attached TUI/agent/jobs projections and session clients; Telegram integration only after its own policy review.

**Consumes:** W03/W04 inventory/job views, W09 authenticated owner routing, W05 actions where enabled; reconciled #144/#137/#81 direction. **Produces:** scoped readable views and canonical action submission on each supported client.

- [ ] **Red tests:** U01-U06, plus C07 for reconnect and missing capability. Assert unknown provenance remains unknown, failure is visible when collapsed, stale selection cannot target a replacement and a slow client cannot block the collector.
- [ ] **Implement:** derive UI state from shared ports, with coverage/revision/cursor and explicit ownership distinctions. Preserve 40-59-column portrait behavior and larger chat space. Use deterministic Mini Live from existing public state; no new always-on model observer is required.
- [ ] **Green checks:** `cargo test -p harw-protocol -p harw-ops -p harw-tui -p harw-session-remote`; test local/remote authorization parity through real dispatch. Inspect actual callable tool lists and missing-service errors.
- [ ] **Commit:** `feat(ui): project host workloads with scope and lifecycle provenance`.

**Closes:** LUC-13/LUC-39 and presentation/privacy portions of LUC-40.

## W14 — Frozen integration, real-host acceptance and rollback

**Files:** test/fixture modules listed above, release/deployment documentation and a new run-specific validation report under this dossier. Do not amend historical reports to say new tests passed.

**Consumes:** all enabled work-package commits. **Produces:** one immutable evidence bundle and a conservative release decision.

- [ ] **Preflight:** refresh dev/PR heads, document extracted versus superseded branches and verify a clean worktree. Freeze the integration SHA before running gates.
- [ ] **Run:** the complete build matrix and H/J/C/U tests from the verification document, recording exclusions as not run. Privileged/engine checks use disposable authorized hosts. Independently review the external-control authority boundary.
- [ ] **Measure:** repeat the Pi/browser/build comparison with fixed compiler flags, caches, swap, storage, power and thermal conditions. Capture action journals, PSI, memory, I/O, workload completion and UI latency; no causal performance claim from the CPU sparkline alone.
- [ ] **Rollback drill:** disable new admission, preserve external applications, reconcile owned effects, and verify independent native administration after Harw/cloud/model failure.
- [ ] **Commit:** `test(system): record frozen host-cloud acceptance and rollback evidence`. Keep release/draft status blocked for any enabled capability without its proof.

**Closes:** LUC-04/LUC-26 plus the deployment evidence for all enabled rows. A design PR or merged implementation without these results is not acceptance.

## Conservative initial configuration contract

These are **proposed defaults**, not currently accepted config keys or tuned performance claims. W06/W08 must define them in the real strict configuration schema and include generated defaults/discovery:

| Setting | Proposed initial value / rule |
|---|---|
| Host inventory enabled | false; explicit enrolled user scopes required |
| Full reconciliation interval | 30 seconds, plus immediate reconciliation after a detected gap |
| Maximum retained live process records | 32,768; overflow yields Degraded coverage, never a false complete inventory |
| Maximum observation batch | 256 records and 256 KiB; reject an oversized individual record rather than truncate identity |
| Maximum queued batches per subscriber | 16; visible lag/resync when exceeded |
| Maximum local lifecycle journal | 64 MiB by default; checkpoint and coverage-aware rotation |
| Terminal identity tombstone retention | 300 seconds initially; source delay beyond retained history requires reconciliation, not a guessed join |
| Raw argv/environment export | disabled; separate inspection permission and redaction required |
| External mutation | disabled; protected session services remain denied by default |
| Node-wide capacity enforcement | disabled until explicit resource/slot policy and a supported backend are configured |
| Automatic pressure actions | own admitted background jobs only; at most 6 actions/minute and at least 10 seconds apart |
| New remote admissions during expired authority/partition | refused; existing locally admitted work follows its explicit bounded policy |
| Provision new cloud machines | disabled/out of scope |

The test fixtures pin these defaults so copy/paste examples, help, schema and runtime do not drift. Bounds may be tuned later with measured overhead and compatibility review. Reducing observation detail must never hide lifecycle loss or weaken identity checks.

## Merge and ownership rules

Each implementation PR links its LUC rows, exact source snapshots, owning task and executed test IDs. Shared files are edited by one integration owner. Use small source extractions instead of merging duplicate #146/#147 heads. No old branch is deleted until its unique useful content is either incorporated with evidence or deliberately rejected.

The design is ready for owner/reviewer discussion, not automatic privileged implementation. Decisions on user scopes, permitted external actions, concrete cgroup/resource budgets, proxy dependency profile and any future provisioning spend must be explicit before their corresponding activation phase.

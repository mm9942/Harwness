# 04 — Implementation waves, review gates, metrics and migration ledger

> Status: **proposal** (§§1–6), documented on the pinned `dev@197a92e92d438bf6bf93b129bb964f4852686149` baseline. §7 records what is implemented so far (partial W01–W03; the read-only recipe is wired). The W01–W03 waves are merged on `dev` via PR #148 (ledger entry `MIG-PL90-W03w`); the rest of this document remains proposal.

## 1. Implementation ownership principles

Keep reusable mechanisms outside domain-specific policy. Candidate `harw-chain` or similar shared-infrastructure crate owns typed state machine, transition validation, checkpoint serialization and deterministic replay. Existing `harw-job-core`/job worker own lease, durable job state and cancellation, without depending on Harwness-specific recipes. `harw-agent-dsl` owns declarative composition/recipe references and compiler diagnostics. `harw-runtime` owns registry assembly, tool/model preflight and actual child admission. `harw-plan-bridge` may provide planning cells and workload-specific WorkDriver integration but should not permanently own the generic chain runtime. `harw-ops` can expose status/stop/inspect and intent submission only after authorization review. Domain adaptations in `harw-home`, matrix, evidence and LaTeX modules own schemas and verification policies. DoD remains a sibling security domain and can consume read-only composition primitives without embedding privileged mutation in generic chain code.

No new dependency from a lower workspace ring to a higher composition/application ring. Rust first-party code retains `unsafe_code = "forbid"`. A `Cargo.toml` owner, tests and `xtask/arch-policy.toml` dependency check must be part of the first implementation wave.

## 2. Wave sequence

### W00 — Freeze semantics and acceptance

Deliver authoritative versioned schemas for `AgentIntentV1`, `EvidenceStateV1`, `ChainTransitionV1`, `AgentCycleCheckpointV1`, `ComposedAgentReturnV1`, `CapabilityPreflightV1`, plus explicit source ownership. Add a migration ledger entry and crosswalk PL-69 W00/W01/W02 and WorkDriver. Decide whether `Cycle` is a UI nickname while the Rust system calls it `AdaptiveChain`; do not expose two subtly different runtimes.

Proof: tests reject unknown keys, unsupported transition and unauthorized target; round-trip checkpoints; explicit README status. Design review checks old root spawn restrictions and matrix ownership preserved.

### W01 — Pure transition core

Implement deterministic `evaluate_transition(intent, evidence, proposal, policies) -> AdmittedTransition | Refusal`. It must reject invented permissions, unknown recipes, cycles in a single stage DAG, unbounded recursion, excess parallel width, stale intent revisions and no-progress repetitions. The model can recommend `Reorient`, `Fork`, `Nest`, `ProposePatch`; runtime picks a policy-admitted operation, returning a typed refusal with remediation hints. Avoid tool I/O inside this core.

Proof: table/property tests for all transition families; fuzz unexpected JSON; deterministic result for identical trusted snapshots; no permission widening from checkpoint or learned memory.

### W02 — Durable chain job adapter

Connect generic chain to job claim/lease, epoch-fenced atomic checkpoint, job-specific limit fields and structured step observer. Reuse existing pacing and cancellation; never spawn a new job per model turn by default. A crash before commit may repeat only idempotent steps; external effects require a durable planned/executed/verified state. Add status/resume/stop and per-run telemetry with model route, state delta and budget.

Proof: killed process mid-checkpoint, stale epoch rejection, idempotent replay, cancellation cascade, provider-429 wait semantics, no hidden retry on external write.

### W03 — Read-only adaptive explorer (small-model benchmark)

First end-to-end composed-agent recipe: `intent → candidate repo map → evidence frontier → targeted read → hypotheses → transition → cited output`. Reuse `explore.*`, `fs.*`, `deps.*`, context programs and current explorer under inherited read rights; do not create root privileges for the explorer. Run model/tool schema preflight. Benchmark local ~30B against larger baseline on path/source accuracy, evidence coverage, unsupported claims, median tokens, wall time, failure recovery and permission refusal. At least one benchmark requires a factual reorientation not predictable from initial file names.

Proof: fake README versus source truth, surprise dependency boundary, symlink/out-of-scope denial, identical denied Egress not retried, reviewer can reproduce cited lines.

### W03c/W03d — Durable error observations and independent agent runs (pre-W04 safety gates)

Before widening orchestration, implement [05 — Error Learning & Agent Run Isolation](05-error-learning-and-run-isolation.md) as **two small separate waves**:

- **W03c (CURRENT: library and `CycleDriver` integration; the consumer is the W09a drain in the `harw-cli` intent-cycle worker):** classify admission, executor, provider and reconciliation failures into typed non-secret run observations; atomically persist a bounded, epoch-fenced outbox **in the same CycleStore transaction as a committed checkpoint**. Do not write global memory from CycleDriver, do not count unverified fixes, and never retry an uncertain external effect blindly. `CycleDriver` builds the observations (`build_observation`) at refusal, permanent proposer failure, executor failure, uncertain reconciliation and verification, and commits them through `CycleStore::commit_step_observed` into `CycleRecord.outbox` (`harw-plan-bridge/src/cycle_runtime.rs`).
- **W03d (CURRENT for identity, recall and the parent boundary; per-session STM enforcement is a library contract; limitations in 05 §4):** trusted run/agent/session principal through the job/child boundary; enforce per-session STM and independently assembled per-run context. A parent receives only an authorized bounded return, siblings receive no private history. Audit MemoryContextProvider session matching and bulk project/global fact injection; make child retrieval explicit and policy checked, with root compatibility governed separately.

The W03c pieces live in `harw-plan-bridge` (`cycle_runtime.rs`, `cycle_observation.rs`, `cycle_outbox.rs`); the W03d pieces live in `harw-knowledge`, `harw-ops`, `harw-research`, `harw-core-bridge` and `harw-runtime`. Cross-run learning consumption (W09a) and top-k lesson recall (W09b) are CURRENT in the intent-cycle worker (§7.7, [05 §4](05-error-learning-and-run-isolation.md)); reviewed global promotion remains W09c (PLANNED).

Proof: different agents cannot share STM/transcripts via provider reuse or the same trace; stale lease cannot append or ack learning; a crash between commit/delivery is idempotent; forbidden actions never become learned permissions; no context growth with sibling count. Keep current W01–W03 tests and arch gates green.

### W04 — Root and sub-orchestrator intent reconciliation

Add the minimal intent contract and bounded DAG-change tool to Root/Sub. Detect material state delta, recompute frontier, cancel/supersede pending nodes, admit new specialist targets only through exact named grants and runtime capability snapshot. Do not inject every agent definition and every tool into context. Use compact top-k relevant role offers with capability and model readiness. Ensure `agents.delegate`, `delegate_wave`, generated transfer tools and execution/approval paths agree.

Proof: full `advertised → schema → registry → approval → executor → admission → execution → return` matrix for 0, 1, 8 and 9 visible targets, Plan Mode, missing grant, background/synchronous and model 403; end-to-end test with >8 visible targets; unavailable model 403 path; `no delegation capability` refusal; plan mode keeps writes unavailable; large returned evidence referenced instead of copied; child cannot spawn forbidden child; no deadlock on parent pause.

### W05 — Safe corrective writing and Bug Hunt adapter

Build a graph-to-edit bridge: verify the finding → classify one-file vs contract-wave → reserve canonical file set → spawn scoped editor in isolated worktree/branch → inspect exact diff → independent review → ripple verification → centrally run tests/gates once at frozen integration commit → retain manifest/goal ledger and propose completion. Preserve the Gap-Hunt exact gates: per-workflow branch/worktree, immutable SHA-fenced wave manifest, `change=false` declared no-edit contract entries, ripple `clear|skipped|findings|missing`, `contract-mismatch`, `unverified`, human-only achievement, and frozen-SHA central gate record. Add an explicit state-to-transition crosswalk before porting. Use Gap-Hunt existing `gap-hunt-area`, `gap-fix`, `gap-verify`, `contract-wave` and `layered-wave` as behavior references without copying provider-specific JS. Never allow generic chain state to issue unchecked `fs.write`.

Proof: two concurrent agents targeting one file cannot both write; overlapping multi-file contract refused; foreign worktree content never committed; regression test demanded on critical security issue; central verifier uniquely owned; a failed gate creates a *new* bounded corrective frontier; verify “no cargo in leaf wave” policy where selected. Human-only achievement remains.

### W06 — Research/analysis/scenario composed families

Adapt researcher-web/deps, evidence collector, intel-analysis, evidence-review and wargaming to intent/evidence frontiers. Differentiate source absence, model/tool failure and substantive disproof. Scenario agents keep actual vs simulated evidence separate. Add capability-bound nested chain recipes and divergent child-return contract checks.

Proof: contradictory source triggers evidence critic and method audit; blocked primary source retained as unknown; changed key driver invalidates only dependent analysis; no unfair promotion of unsupported scenario confidence.

### W07 — Matrix Game composition

Preserve UIA-owned Matrix Game Master and deterministic engine. First implement a facade + evidence package to Master; only consider dedicated matrix *child* adapter after a separate authority review and tests. Reuse existing approval requirement on start/run/finish, seat privacy, game randomness and result report. A surprise may trigger a read-only inquiry, not engine mutation without rules.

Proof: no Root→Root illegal nesting; simulated result never labeled verified real-world fact; approvals remain mandatory; deterministic replay from game event log; no role-information leak between players.

### W08 — Business writing and LaTeX

Adapt business-author/reviewer pipeline to approved versioned `PaperPackageV1` with claim/evidence graph and independent review. A material new source reorients research and invalidates only affected sections. Emit small sectional writing tasks. Return approved package to UIA; only UIA spawns `uia-latex-writer` with `latex.template/check/build`. Retain `business-paper` Matrix consumer; add any `research-paper` kind as backwards-compatible extension.

Proof: independent reviewer rejects unsupported claim; non-pausing worker cannot receive synchronous handoff; LaTeX truncation resumes from checkpoint, not full regeneration; PDF visual render/QA; no fictitious citation; approved claims unchanged by typesetting.

### W09 — Knowledge feedback and intent lifecycle

**Binding addendum:** [05 — Error Learning & Agent Run Isolation](05-error-learning-and-run-isolation.md) is mandatory. W09a consumes the W03c durable run observation outbox using existing learning/epistemic/outcome gates; W09b provides explicit authorized, versioned, top-k retrieval rather than shared agent contexts; W09c reviews project-to-global promotion and revocation. Global-home knowledge is not a global model prompt. Other agents' private conversations, tool transcripts, STM and diaries are never injected merely because they share project, trace, parent or role.


Integrate context-ledger observation references, Diary/Dream/Palace/Maintenance as optional bounded segments; global promotion and embeddings require separate ownership/privacy policy. Link new sessions to stable intent/project while preventing child-context leaks. Journal corrections, confidence and source scope, not hidden provider reasoning text. Introduce replay and regression snapshots for learned policies.

Proof: scoped memory does not escape to unauthorized global space; deleted/expired facts are not reintroduced from stale checkpoints; contradictory memories are quarantined; intent revisions preserve audit trail.

### W10 — DoD security integration and release gates

Consume only safe observation/analysis primitives in Detect/Orient and keep Defend separately approved in small privileged TCB. Threat model prompt injection through evidence, forgery of checkpoint authority, replay of approval, write scope races and recursive budget escape. Roll out in stages: read-only experiments → opt-in write pilots → runtime hardening → default recipes after acceptance metrics.

Proof: user-level agent cannot become defender through chain nesting; all external state changes auditable; high-risk actions fail closed; runtime/job/authority boundaries maintain small dependency footprint.

## 3. Cross-workstream failure injection matrix

| Failure | Required expected behavior |
|---|---|
| Unavailable model / HTTP 403 | preflight or circuit breaker, no repeated same-route burn |
| External Egress denied | blocked remote evidence; local alternative only with same policy |
| `agents.delegate` offered but executor unavailable | cannot advertise unusable route; trace refusal |
| Nested chain tries to spawn forbidden Worker→Child | fail admission, no side effect |
| Parent lease lost / process killed | stale checkpoint fenced, children canceled or recovered under policy |
| Child output truncated | continuation with artifact reference; no retry of huge output |
| Two writers own same canonical file | one reservation refused before edit |
| Unexpected cross-file ripple | stop current single-file acceptance, open contract wave |
| Critical fix lacks regression evidence | refuse gate, do not publish as verified |
| Build red on frozen SHA | fix/reorient or escalate, no green marker |
| Model says objective achieved | human/criteria verification still controls status |
| New source refutes key claim | targeted invalidation, re-review, no silent rewrite of intent |
| Untrusted repo config asks for full access | ignore widening, apply host ceiling |
| External action outcome uncertain | reconciliation state, not blind replay or false success |
| Infinite low-value search | no-progress bound and escalation |

## 4. Metrics and quality gates

Measure not only token count: **goal coverage and factual precision**, evidence-trace completeness, surprise reorientation success, unauthorized-action attempts rejected, median+tail wall time, model calls per verified claim, network/model error retry waste, write conflicts, regressions per patch, number of re-review rounds, successful recovery after interruption, and user-approval burden. Compare agents on the *same pinned task sets* with identical rights. At least one simple, one complex, one contradicting-evidence and one unavailable-capability case per adapter. Public benchmark results should list model, backend, actual offered context, environment, baseline date and confidence; do not generalize from one 30B result.

## 5. Migration ledger and PR discipline

Each delivered wave records `MIG-PL90-Wxx`: baseline commit, planning compartment, implementation branch/commit/PR, touched crates and ring deltas, before/after behavior, compat retained, tests/gates, accepted evidence and remaining risks. Do not mark CURRENT until merged and tested on dev. Use file-disjoint worktrees, human code review and frozen-SHA central gate pattern from Gap-Hunt; branch/PR should be narrowly scoped (DSL core, runtime adapter, explore recipe, write adapter, etc.), no mega-change pretending all domains are done.

## 6. Open decisions requiring explicit design review

1. Exact crate placement and dependencies for generic chain core versus runtime job adapter.
2. Representation of recipe execution (closed typed enum vs registered trait implementations) and stable recipe versioning.
3. How to expose compact, dynamic agent capability offers without context flood, while preserving admission parity.
4. Who signs intent revisions and how parent/child approval references are persisted.
5. Whether a composed agent can resume in a different process/model with identical facade identity, and how runner leases fence it.
6. Which sources are considered trusted evidence across Workspace/Project/Global memory boundaries.
7. How workflow constraints such as “central build only” become enforceable policies instead of model instructions.
8. Whether the matrix facade should remain UIA-owned forever or gain a separately designed constrained child mode.
9. Objective no-progress thresholds and when a stronger model route is warranted versus stop.
10. Required performance improvement for enabling default local ~30B composed explorer instead of existing explorer.
11. How intent-driven writes request expansion of path scope without silently violating original acceptance contract.
12. Trace/event schema that makes graph evolution understandable in TUI/Web/Telegram while hiding secret material.

**Additional PL-90 status note (2026-10-08, updated 2026-10-27):** W09a (job-local learning drain) and W09b (top-k lesson recall for the intent-cycle proposer) are **CURRENT** in the intent-cycle worker; only W09c (reviewed global promotion, expiry/supersession/revocation) is **PLANNED ONLY**, so there is still no global or cross-agent memory consumer. W03c is **CURRENT**: `CycleDriver` builds typed observations (`cycle_observation.rs`) at refusal, permanent proposer failure (`ProviderUnavailable` / `ProposerFailed`, `classify_proposer_failure`), executor failure, uncertain reconciliation and verification, and persists them in the same `CycleStore` commit as the checkpoint (`commit_step_observed` into `CycleRecord.outbox`, `cycle_outbox.rs`, §7.7); the consumer is the `harw-cli` learning drain, which reads pending observations only lease-fenced (`CycleStore::pending_observations(cycle_id, token)`). W03d is **CURRENT** with documented limitations (identity-bound recall, parent envelope sanitizer, root-only facts pinned by test; see [05 §4](05-error-learning-and-run-isolation.md)). The [05 addendum](05-error-learning-and-run-isolation.md) still introduces evidence-based gates for the unfinished parts, not a tested learning feature.

## 7. Implementation status

Status vocabulary as in the dossier: `CURRENT` = merged and checked, `IN PR` = code in this PR with locally green tests, `PLANNED` = design only.

### 7.1 `harw-plan-bridge/src/intent_cycle.rs` — CURRENT (MIG-PL90-W03w, PR #148)

Verified locally with `cargo test -p harw-plan-bridge` (222 lib + 20 `intent_cycle` tests green) and `cargo clippy -p harw-plan-bridge --all-targets -- -D warnings` (clean). The original seven source-only tests had never been run; running them found one real defect: `{"kind":"complete","permission":"full_access"}` deserialized, because serde does not enforce `deny_unknown_fields` on unit variants of internally tagged enums. All proposal and join variants are now struct variants and the test covers four injection shapes.

| Area | What the module now does | Review thread |
|---|---|---|
| Transition family | `Advance`, `Fork`+`JoinPolicy` (`All`/`Any`/`Quorum`), `Reorient`, `Revisit` (drops invalidated claims and the criteria built on the revisited evidence), `ProposePatch` (multi-file contract set, every target in scope), `Verify`, `RequestApproval` (a request, not an approval), `Complete`, `Wait`, `Escalate`, `Blocked`, `Failed` | — |
| Bounds | separate chain-nesting and agent-spawn depth with distinct refusals (`nested()`/`child()` derive seeds), transition budget, stall budget, parallel width, duplicate segments; honest exits (`Escalate`/`Blocked`/`Failed`) stay admissible when exhausted or stalled; terminal checkpoints admit nothing more | — |
| Trusted construction | `CycleAdmission` is not `Deserialize` and only built through `CycleAdmission::new` (rejects malformed intent, zero limits, blank targets); every serialized type is `deny_unknown_fields` | — |
| Intent identity | `IntentBinding` carries `revision`, canonical `ContentDigest` and `predecessor`; `supersedes()` rejects other-intent, stale, forked, skipped and wrong-predecessor revisions; admission and resume reject any revision/digest mismatch | R02 |
| Evidence & progress | `EvidenceRecord` with source kind, locator, digest and trust (`Reported < Observed < Verified`); `apply_observations` derives progress from new content digests, trust upgrades, newly covered criteria or invalidated claims — a known digest under a new id is false novelty; evidence ids cannot be rebound to other content | — |
| Acceptance | `Complete` needs every acceptance criterion mapped to `Verified` evidence and yields `CycleTerminal::CompletionProposed`; goal acceptance stays with WorkDriver/human | R06 (partial: no WorkDriver wiring) |
| Durable fence | `CycleCheckpoint` (schema v1) with `CheckpointFence { epoch, sequence }`; `check_commit` is the CAS rule: same epoch → sequence + 1, new epoch → never rewind, stale epoch refused, intent/authority/ceiling immutable across commits | — |
| Resume | checkpoint stores the non-authorizing `AuthoritySnapshot` and `AdmissionCeiling`; `resume_admission` takes the context from `PolicyBootstrap::reissue`, refuses another workspace or any permission the snapshot never held, intersects stored ceiling × current admission × reissued permissions (write targets need `WriteWorkspace`, reads need `ReadWorkspace`), narrows limits, re-fences under the new epoch; test covers a policy + role upgrade between crash and resume | R01 |

### 7.2 `harw-plan-bridge/src/cycle_runtime.rs` — CURRENT (MIG-PL90-W03w, PR #148; W02 runtime binding)

Verified locally: `cargo test -p harw-plan-bridge` (235 lib tests incl. 13 `cycle_runtime`), clippy `-D warnings` clean, `xtask gates arch` without violations for `harw-plan-bridge` (new edges A → J `harw-job-core`/`harw-job-store` are permitted by `A.may_depend_on = ["*"]`).

| Concern | Implementation | Reused mechanism |
|---|---|---|
| Persistence | `CycleStore` over `RecordStore<CycleRecord>`; every mutation is a fenced CAS under the record lock (`check_commit` for steps, new `check_rebind` for takeovers) | `harw-job-store::RecordStore` (lock, temp+rename+fsync, quarantine) |
| Lease fencing | checkpoint epoch = `LeaseToken::epoch`; an older epoch is refused on every operation, a newer one must rebind first; `check_rebind` allows only a newer epoch at the same sequence, narrowing ceiling/permissions/network scope, nothing else changed (`StateTampered`) | job lease epochs from `JobStore` |
| Resume | every claim (including the first, records start at epoch 0) reissues the persisted `AuthoritySnapshot` via `AuthorityReissuer` (`PolicyReissuer` → `PolicyBootstrap::reissue`), narrows via `resume_admission` (now also network scope) and rebinds | `harw-authority` |
| Effect journal | `InFlightStep` with stable idempotency key `<cycle>-<sequence>` journaled before execution; on takeover `reconcile` → `NotStarted` (dropped), `Completed` (observations committed, never re-executed), `Uncertain` (cycle escalates) | — |
| Driver | `CycleDriver`: proposer (model) and step executor (trusted) are separate traits; refusals are fed back to the proposer and escalate after `max_refusals_per_step` (default 3); proposer failure (e.g. provider 403) → `Blocked`; executor failure → stall → bounded escalation; cancellation between steps, in-flight step left for reconciliation | `CancelToken` |
| Jobs | `cycle_job_operation` plugs the driver into `DurableJobRunner::run_with_cancel` (claim, heartbeat, lease-loss cancel, job commit); `job_outcome`: `CompletionProposed` → `Succeeded` with `requires_owner_acceptance: true`, `Escalated`/`Blocked` → `Blocked`, `Failed` → `Failed`, cancel → `Cancelled` | `harw-core::DurableJobRunner` |
| Status | `CycleRecord::status()` — bounded projection (counts, covered criteria, in-flight key, terminal), no evidence bodies | — |

Covered failure injections: stale epoch / zombie executor, foreign job, double journal, crash with completed / uncertain / not-started step, tampered or widened rebind, policy downgrade between submit and claim, unavailable proposer, executor capability failure, cancellation, end-to-end through a real `JobStore` + `DurableJobRunner`.

### 7.3 `cycle_explorer.rs` / `cycle_proposer.rs` — CURRENT (MIG-PL90-W03w, PR #148; W03 building blocks)

Intermediate state: the two halves of the W03 explorer exist as independently tested modules and are now merged together with one cross-module test (`cycle_w03_tests.rs`: `CycleDriver` + `ModelCycleProposer` over a scripted fake `ModelProvider` + `ReadOnlyExplorerExecutor` over a temp workspace; Advance(read) -> digest-bound `Observed` evidence -> Reorient on that evidence -> read -> Escalate; it also asserts the turn-2 prompt contains the evidence id and locator but no file content and no digest). Verified locally: `cargo test -p harw-plan-bridge` (271 lib + 20 `intent_cycle` tests green), clippy `-D warnings` clean, `rustfmt --edition 2024 --check` clean, `xtask gates arch` shows only the 6 pre-existing violations (`harw-cloud*`, `harw-tool-obsidian`). New edge: `harw-plan-bridge` -> `harw-tool-fsread`.

| Module | What it does | Key decisions |
|---|---|---|
| `cycle_explorer.rs` (`ReadOnlyExplorerExecutor`) | First trusted `CycleStepExecutor`: turns admitted `Segment::Read` segments into `EvidenceRecord`s (source `Repository`, digest of the bytes, workspace-relative locator). Refuses child/nested segments and every non-read proposal. Reads go through `harw-tool-fsread` `Scope` (no symlink escape, regular files only, secret paths denied) and need `ReadWorkspace` | Target ids map to paths only via a trusted, non-`Deserialize` `ReadTargetMap`. All-or-nothing step: any failing read returns `Err(StepFailure)` (no partial evidence); the driver counts it as a stall. `reconcile` always returns `NotStarted` (reads are idempotent; a recovered digest could be stale). Oversized files are refused, never truncated or digested as a prefix. Optional bounded in-memory `ExplorerReadCache` (oldest-first eviction, filled only when the whole step succeeded, lost on restart). It only ever produces `Observed` evidence |
| `cycle_proposer.rs` (`ModelCycleProposer`) | `CycleProposer` over a `ModelProvider`: pure `render_prompt` -> one JSON object -> `serde` parse (`deny_unknown_fields`) -> `CycleProposal`; admission and executor still decide everything | Route policy: deterministic default/escalation route chosen from refusal streak and `stall_transitions`, never by the model. One repair re-prompt with the bounded parse error, then `Err` -> `Blocked` (no synthesized `Escalate`). Retryable provider errors retried up to `max_retries` times with bounded waits (`retry_wait`; a provider wait above `retry_max_wait` fails at once). Auth circuit breaker: after `auth_trip_after` consecutive `Auth` failures a route is no longer used. Prompt exclusions: no authority snapshot, ceiling, permissions, digests, file contents or claim bodies; ids and locators are sanitized and capped |

What is NOT done:

- ~~No runtime caller builds a `ReadTargetMap`, and there is no tool or registry entry that starts a cycle with these modules.~~ Done in 7.4.
- ~~No real verifier: `Verify` is refused by the explorer, so `Complete` is unreachable.~~ Done in 7.4 (`cycle_verifier`); the in-module `Verified` stand-in in `cycle_explorer` tests is still a test double.
- No `CancelToken` in the proposer (`CycleProposer::propose` does not receive one; requests carry `cancel: None`).
- ~~No retry backoff.~~ The proposer now sleeps bounded waits (see the table above); the provider layer still owns pacing.
- No small-model benchmark (the W03 exit criterion in section 2 is still open).

### 7.4 Production wiring of the read-only recipe — CURRENT (MIG-PL90-W03w, PR #148)

The W03 building blocks are now reachable end to end:

| Layer | What | Where |
|---|---|---|
| Verifier | `ContentCheck` per acceptance criterion; `Verify` re-reads the observed file, requires the same digest, yields `Verified` evidence (source `Verification`) or an `unmet:`/`stale:` claim. `Complete` is now reachable without trusting the model | `harw-plan-bridge/src/cycle_verifier.rs` |
| Executor | `ExplorerCycleExecutor` = read-only explorer (read, wait) + verifier (verify); everything else refused | same |
| Spec | `ReadOnlyCycleSpec` (objective, read targets, checks, limits ≤ `MAX_CYCLE_LIMITS`), one canonical intent digest, `plan()` = the single validation of operation and worker | `harw-plan-bridge/src/cycle_spec.rs` |
| Reissue | `CeilingReissuer`: persisted snapshot ∩ current trusted ceiling of the same workspace (never widens) | `cycle_runtime.rs` |
| Tools | `intent_cycle.start` (approval always), `.status` and `.list` (observer tier, read-only model tools; `GET /api/intent-cycle/status` / `GET /api/intent-cycle/list`), `.stop` (approval always); behind `[tools.plan]`; `ReadWorkspace` right; catalog class `Read`/`Meta` | `harw-ops/src/intent_cycle.rs`, `harw-registry-defaults` |
| Operator surfaces | `/cycle list\|status\|stop` (TUI-only command, operator tier, no model-tool surface) and the `harw cycle list\|status\|stop` CLI, both dispatching to the same operation | `harw-ops/src/intent_cycle.rs`, `harw-cli/src/cli/cycle.rs` |
| Job | kind `intent_cycle`, long-run lane, claim fence, read-only explore sandbox as ceiling, record created on first claim (epoch 0 → first lease rebinds), budgeted proposer, job cancel → driver cancel, wall budget | `harw-cli/src/job_worker_intent_cycle.rs` |

Verified locally: `cargo test -p harw-plan-bridge` (cycle tests incl. `spec_read_verify_complete_reaches_completion_proposed`), `cargo test -p harw-ops`, `cargo test -p harw-registry-defaults`, `cargo test -p harw-cli --lib intent_cycle` (worker e2e: admitted job → `Succeeded { terminal: completion_proposed, requires_owner_acceptance: true }`).

Proposer cancellation: the driver races every `propose` against the job's cancel token (`tokio::select!`), so a stop or lease loss drops an in-flight model request; the trait itself still takes no token.

### 7.5 Not done (PLANNED)

- No concrete executor for tools beyond file reads, waits and the content verifier; no child spawn or nested recipes.
- W05 is PARTIAL — `harw-plan-bridge::cycle_write_admission` (trusted `PatchProposal::from_admitted` + `admit_patch`: WriteWorkspace right, admitted target, scope-safe path, target→path re-check, preimage re-read; `from_admitted` takes a `&JournaledStep`, so a proposal is minted only from a step the store journaled under the current lease via the fenced `CycleStore::journaled_step`, never from caller-built steps or checkpoints) and `harw-plan-bridge::file_lease` (per-job, epoch-fenced, all-or-nothing file reservations) landed; there is still no write executor, `ReadOnlyCycleSpec` still forces empty write targets, and no production caller grants WriteWorkspace to a cycle. W04 is PARTIAL (compact offers), see §7.7.
- No small-model benchmark (W03 exit criterion).
- Network-scope widening on resume is checked; the test is scheduled with the proposer-correctness wave (`resume_refuses_widened_network_scope`).

- W04–W10: no wired orchestrator, write-wave, research, Matrix, paper/LaTeX or DoD adapter is wired; review threads R03–R05 and R07–R12 remain documentation/contract findings for those waves. Exception: W09a (job-local learning consumer) and W09b (`learning_recall` top-k lesson recall) are CURRENT in the intent-cycle worker (§7.7, [05 §4](05-error-learning-and-run-isolation.md)); W09c (reviewed global promotion, expiry/revocation) stays PLANNED.
- Crate ownership: both modules stay in `harw-plan-bridge` (layer A). `intent_cycle` adds no dependency edge; `cycle_runtime` adds A → J edges to `harw-job-core`/`harw-job-store` and a direct `tokio` dependency already in the graph. Moving the pure core into a dedicated chain crate needs the ring analysis from W00.

### 7.6 Base-branch notes found while verifying

- `dev@197a92e` did not compile `harw-core` (missing `WorkId` import, missing `&` in a `ReferencedSnapshotId::confirm` call, both from the merge `9ae603a`/`2fc7234`); this PR carries the three-line fix because nothing downstream could be built otherwise.
- The root `Cargo.toml` lists `harw-cloud/*` workspace members that are not in the repository; local verification used untracked stub manifests (not committed).
- Two `harw-core` lib tests fail on the base once it compiles (`child_controller::tests::teil_o::without_a_relay_the_child_still_fails_closed`, `turn_loop::tests::uia_keeps_transfer_tools_with_one_line_descriptions`); unrelated to this PR and left for a separate fix.

### 7.7 Landed library modules and their production callers (audit)

| Module | Contract | Missing integration step |
|---|---|---|
| `harw-plan-bridge/src/cycle_observation.rs` (`RunObservationV1`, typed classifiers) + `cycle_outbox.rs` (epoch-fenced `Outbox<T>`, 64 pending / 128 acked tail) | W03c (05 §3) — **CURRENT (wired: `CycleDriver` calls `build_observation` at refusal, permanent proposer failure (`classify_proposer_failure`: `ProviderUnavailable` / `ProposerFailed`), executor failure, uncertain reconciliation and verification, and commits through `CycleStore::commit_step_observed` into `CycleRecord.outbox` in the same CAS write as the checkpoint; `harw-plan-bridge/src/cycle_runtime.rs`)** | consumer = the W09a learning drain of the intent-cycle worker in `harw-cli`, which reads pending observations only through the lease-fenced `CycleStore::pending_observations(cycle_id, token)`; fence refusals cannot be recorded in the record they are refused for and are only logged; no other consumer |
| `harw-plan-bridge/src/child_return_envelope.rs` | W03d-3 (05 §2.2) — **CURRENT (wired: the production child→parent path is `harw-research::return_envelope::sanitize_for_parent`, called from the `ReturnEnvelope` contract arm in `harw-core-bridge/src/agent_tool.rs`; limit violations are content-free contract violations, an over-long summary is truncated and flagged)** | this module stays the contract for future `Segment::Child` steps and has no production caller; its caps are pinned equal to the research gate by `caps_match_harw_research_envelope_caps` |
| `harw-extension-api/src/target_offer.rs` | W04 (top-k role offers) — **PARTIAL** | PARTIAL — compact offers rendered for large target lists (`DelegationTargets::offers` feeds the `delegation.targets` block in `harw-core/src/turn_loop.rs`; the tool schema and admission are unchanged); DAG-change tool, reconciliation, capability snapshot still PLANNED |
| `harw-plan-bridge/src/cycle_benchmark.rs` | W03 exit (small-model benchmark scoring) | benchmark harness and fixtures |
| `harw-knowledge/src/caller_identity.rs` | W03d-2 (05 §1) — **CURRENT (wired: `RecallQuery::with_caller_identity` and `artifact_visible` decide recall and index visibility by identity; `KnowledgeCaller::from_context` reads the `Arc<CallerIdentity>` service; `RuntimeChildRegistryFactory::build_registry` derives one per child from the announced child session (`ChildRegistryFactory::announce_child_session`) into the `OpContext` services of its operation tools)** | limitations (05 §4): `ancestors` stay empty because `SpawnInput` has no parent role (needs a trusted spawn record); ids are run-bound (`<role>.<escaped child session id>` via `run_agent_id`, derived once per child in `build_registry`, stable across durable recovery, and shared by diary authorship and `CallerIdentity`); the root `ServiceMap` carries no identity; child knowledge readers (`diary.read`, `palace.*`, …) run outside `OpContext` |
| `harw-knowledge/src/learning_recall.rs` | W09b (05 §2.3) — **CURRENT (wired: `harw-cli/src/job_worker_intent_cycle_learning.rs::lesson_lines` keeps only project-scoped `pl90` lessons before any limit, ranks them by objective keywords and calls `select_lessons` with top-k ≤ 8 / ≤ 1024 tokens; the intent-cycle worker passes the lines to `ModelCycleProposer::with_lessons`, rendered as low-trust data. The lessons come from the W09a drain of the same worker, which reads the outbox through the lease-fenced `CycleStore::pending_observations(cycle_id, token)` and writes only under the trusted `IntentCycleJobInput::learning_root`)** | only the intent-cycle proposer recalls; keyword matching only, `require_verified` off, no expiry/supersession/revocation (W09c PLANNED) |
| `harw-memory/src/context_provider.rs::with_session_gate` | W03d-1 (05 §1) — **CURRENT (library contract; root-only facts policy wired and pinned)**: with the opt-in gate a foreign session's STM contributes nothing | no production caller: `MemoryContextProvider` is not mounted (the root uses the facts-only `MemoryFactsContextProvider`, root-only by explicit policy and pinned by `child_registry_has_no_memory_facts_provider`; children mount none); a future STM mount must enable the gate |
| `harw-model-catalog/src/lifecycle.rs` | PL-86 D1/D5 — **CURRENT (wired: explicit selection via `enforce_explicit` in `harw-runtime/src/live_model.rs`, `harw-ops/src/live_model.rs` and `harw-cli/src/models.rs`; internal model points via `implicitly_selectable_bundled` in `harw-runtime/src/live_model.rs`)** | PL-86 D3 beyond these callers (router) not audited here |
| `harw-tui-layout/src/mini_live.rs`, `harw-tui/src/{live_ranker,recency_view}.rs` | PL-94 W02 U0 / W01 M2b — **CURRENT (wired in `harw-tui`: `panes::portrait_live_strip` calls `harw_tui_layout::chat_first`, the renderer in `app.rs` uses `recency_view::slot_lines`, `app/live_order_glue.rs` ranks child activity with `LiveActivityRanker`)** | none found |
| `harw-job-linux/src/inventory.rs` | linux-user-cloud-control H04/H05 | procfs/DoD source |

Rows marked CURRENT (wired …) have a production caller. Still without one (no use outside their own module and `lib.rs`): `cycle_benchmark.rs`, `child_return_envelope.rs` (contract only; the production child→parent path is `harw-research::return_envelope::sanitize_for_parent`), `harw-memory::context_provider::MemoryContextProvider` with its `with_session_gate` (not mounted) and `harw-job-linux/src/inventory.rs`. `target_offer.rs` has a caller but W04 stays PARTIAL. A wired module is not evidence of the corresponding wave being complete.

### 7.8 W05 crosswalk (draft): Gap-Hunt gates → cycle transitions

This is the state-to-transition crosswalk that §2 W05 requires before porting, and it is a draft.

| Gap-Hunt gate | Cycle transition / check |
|---|---|
| `gap-fix` single file | `ProposePatch` + `admit_patch` |
| `contract-wave` | `MultiFile` refused in slice 1 |
| `change=false` entries | future `WriteTargetMap` entries with no patch |
| ripple `clear\|skipped\|findings\|missing` | future `Verify` / `Reorient` |
| frozen-SHA central gate | future `Verify` with pinned digest |
| human-only achievement | `CompletionProposed` stays owner-accepted |

The comparison to Claude Code remains limited to repository-visible workflows (Gap-Hunt kit), **not a claim about Anthropic's internal implementation**.

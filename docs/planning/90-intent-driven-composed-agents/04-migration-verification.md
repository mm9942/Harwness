# 04 — Implementation waves, review gates, metrics and migration ledger

> Status: **proposal only**, documented on the pinned `dev@197a92e92d438bf6bf93b129bb964f4852686149` baseline. This documentation PR does not implement a runtime, does not widen admission rights, and must not be presented as a passed build.

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

## 7. Implementation status

Status vocabulary as in the dossier: `CURRENT` = merged and checked, `IN PR` = code in this PR with locally green tests, `PLANNED` = design only.

### 7.1 `harw-plan-bridge/src/intent_cycle.rs` — IN PR

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

### 7.2 Not done (PLANNED)

- W02 runtime: job claim/lease integration, actual persistence through `harw-job-store`, cancellation cascade, idempotent effect journal, model/provider route per turn.
- W03–W10: no explorer, orchestrator, write-wave, research, Matrix, paper/LaTeX, memory or DoD adapter is wired; review threads R03–R05 and R07–R12 remain documentation/contract findings for those waves.
- Crate ownership: the module stays in `harw-plan-bridge` (already depends on `harw-authority` and `harw-types`; no new dependency edge). Moving it to a dedicated chain crate needs the ring analysis from W00.

### 7.3 Base-branch notes found while verifying

- `dev@197a92e` did not compile `harw-core` (missing `WorkId` import, missing `&` in a `ReferencedSnapshotId::confirm` call, both from the merge `9ae603a`/`2fc7234`); this PR carries the three-line fix because nothing downstream could be built otherwise.
- The root `Cargo.toml` lists `harw-cloud/*` workspace members that are not in the repository; local verification used untracked stub manifests (not committed).
- Two `harw-core` lib tests fail on the base once it compiles (`child_controller::tests::teil_o::without_a_relay_the_child_still_fails_closed`, `turn_loop::tests::uia_keeps_transfer_tools_with_one_line_descriptions`); unrelated to this PR and left for a separate fix.

The comparison to Claude Code remains limited to repository-visible workflows (Gap-Hunt kit), **not a claim about Anthropic's internal implementation**.

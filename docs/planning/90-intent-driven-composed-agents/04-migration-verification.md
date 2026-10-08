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

Proof: end-to-end test with >8 visible targets; unavailable model 403 path; `no delegation capability` refusal; plan mode keeps writes unavailable; large returned evidence referenced instead of copied; child cannot spawn forbidden child; no deadlock on parent pause.

### W05 — Safe corrective writing and Bug Hunt adapter

Build a graph-to-edit bridge: verify the finding → classify one-file vs contract-wave → reserve canonical file set → spawn scoped editor in isolated worktree/branch → inspect exact diff → independent review → ripple verification → centrally run tests/gates once at frozen integration commit → retain manifest/goal ledger and propose completion. Use Gap-Hunt existing `gap-hunt-area`, `gap-fix`, `gap-verify`, `contract-wave` and `layered-wave` as behavior references without copying provider-specific JS. Never allow generic chain state to issue unchecked `fs.write`.

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

This PR: planning/docs only, no Rust or workflow source change and no local build. Source-grounding used connected GitHub at pinned dev and the Gap-Hunt kit; the comparison to Claude Code is limited to repository-visible workflows, **not a verified claim about Anthropic's internal proprietary implementation**. The first code action after design review should be the read-only transition core + adapter test harness, not enabling unrestricted write-capable agents.

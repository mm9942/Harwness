# 02 — Adaptive intent, evidence transitions, corrective writes and cycle-as-agent contracts

> **PLANNED / normative design proposal**, not an implemented Rust API. Rooted in PL-69 Model-Turn-Chain revision, current Agent DSL/IR, WorkDriver and the Gap-Hunt kit. Requirements below are test targets, not status claims.

## A. Agent as addressable composition

A composed agent is an externally stable interface around mutable *execution*, not mutable *authorization*: it accepts a typed intent, maintains durable observation/evidence state, decides a useful next graph, submits that graph for trusted runtime validation, observes outcome, reconciles the intent and returns one evidence-backed outcome. It can run a 30B open-weights model with no provider-native thinking interface: explicit iterative reasoning states and discriminating questions are the chain's application-level method, not an assertion that the model has acquired hidden reasoning tokens.

```text
Parent / UIA
    │ AgentIntent + bound AuthoritySnapshot + budgets
    ▼
ComposedAgent(identity, role, contracts)
    ├─ Intent Reconciler (purpose and acceptance unchanged without owner decision)
    ├─ Evidence State Projector (immutable observation references + confidence)
    ├─ Adaptive Policy (model proposes next graph / goals)
    ├─ Capability Preflight (actual available model/tool/agent route)
    ├─ Admission Gate (trusted, checks ceilings/approvals/leases)
    ├─ Execution Supervisor (Job, cell/worker/chain graph)
    ├─ Verification/Adversarial Gate
    └─ Durable Return (outcome, evidence, changed artifacts, residual risks)
              │
              └─ cycle: checkpoint → reorient → new graph
```

Authority role, agent identity, chain recipe, job and cell have distinct identifiers and lifetimes. A worker may execute nested *cognitive* chains without acquiring worker-spawn capability. An orchestrator may spawn only specifically admitted children. A nested chain may not become a privilege escalation channel by specifying `agent_role`, `network`, `shell`, `write` or `budget` in checkpoint state.

## B. The Intent contract

Proposed strictly versioned, deny-unknown-fields contract (illustrative Rust, not compile-verified):

```rust
struct AgentIntentV1 {
    schema_version: u32,
    intent_id: IntentId,
    revision: u64,                  // trusted monotonic intent revision
    predecessor_digest: Option<ContentDigest>,
    canonical_digest: ContentDigest,
    issuer: PrincipalRef,
    objective: Objective,
    immutable_constraints: Vec<Constraint>,
    acceptance_criteria: Vec<Criterion>,
    evidence_policy: EvidencePolicy,
    requested_output: OutputContract,
    read_scope: ScopeRef,
    authorized_write_scope: Option<ScopeRef>,
    safety_policy: SafetyPolicyRef,
    budget: BudgetCeiling,
    parent_intent: Option<IntentId>,
}
```

Intent is distinct from an imperative plan. A user asks “identify and fix the S3 retention bug, keep old behavior and prove it.” The corresponding intent has an observable success criterion (test/behavior evidence), explicit non-goals, source/date requirements and a file write scope. The plan may change from one-file patch to multi-file contract *only if* that remains within the grant and receives needed scope approval. A parent can replace/supersede the intent with an auditable version change; a child cannot silently rewrite objective or evidence acceptance. Filesystem path discoveries outside read scope become scope-expansion requests, not permission to read anyway.

Proposed supplemental fields: `priority`, `deadline`, `stakeholders`, `risk_class`, `completion_authority` (worker/parent/human), `allowed_recipe_ids`, `external_effect_policy`. Prefer references to trusted immutable grants instead of copying raw permissions into an LLM-visible JSON.

## C. Evidence state, hypotheses and frontier

```rust
struct EvidenceStateV1 {
    epoch: u64,
    step: u64,
    claims: Vec<ClaimRecord>,       // observed | inferred | proposed
    observations: Vec<EvidenceRef>, // immutable provider/path/revision/digest
    hypotheses: Vec<Hypothesis>,    // status, counterevidence, tests
    contradictions: Vec<Conflict>,
    key_drivers: Vec<Driver>,
    frontier: Vec<OpenQuestion>,    // priority + information gain + cost + permission
    decisions: Vec<DecisionRecord>,
    changed_files: Vec<ArtifactRevision>,
    criteria_coverage: Vec<CriterionCoverage>,
    last_delta: StateDeltaDigest,
}
```

Observations must retain source URI or repo path, precise range/commit, timestamp where applicable, trust class and content digest. Tool failure is an observation about **capability**, not about the researched fact. One unavailable web endpoint does not imply the substantive proposition is false. A no-progress measure should distinguish genuine new evidence, verified contradiction, narrowed uncertainty and false novelty (repeated snippets/renamed searches). Parent receives summary + handles, not the raw entire evidence collection.

Changes in model belief should be represented as explicit state diffs: `hypothesis_confirmed`, `hypothesis_refuted`, `driver_changed`, `scope_dependency_discovered`, `external_blocker`, `test_failed`, `new_ripple`, `criterion_met`. A failed test should lower the confidence of the proposed fix and can create a new targeted question; it does not trigger an unbounded “try harder.”

## D. Adaptive transition proposals

The terminal `SYNTHESIS -> DECISION` structure from PL-69 remains. Synthesis aggregates evidence **before** the next graph is proposed; the trusted runtime validates graph topology and authorizations. Example high-level transition vocabulary:

```rust
enum ChainTransitionV1 {
    Complete { result: ResultRef },
    Advance { segments: Vec<SegmentProposal> },
    Fork { branches: Vec<SegmentProposal>, join: JoinPolicy },
    Nest { recipe: RecipeId, input: InputRef },
    Reorient { cause: EvidenceRef, frontier: Vec<QuestionRef> },
    Revisit { checkpoint: CheckpointId, invalidated: Vec<ClaimId> },
    Inspect { sources: Vec<SourceRef> },
    ProposePatch { change: ChangeIntent },
    Verify { plan: VerificationPlanRef },
    RequestApproval { requested: ScopeDelta },
    Wait { dependency: ExternalConditionRef },
    Escalate { reason: Blocker },
    Blocked { reason: Blocker },
    Failed { reason: Failure },
}
```

**Important:** `ProposePatch` is not `write` authority; `RequestApproval` is not successful approval; `Complete` is not achieved acceptance unless independent criteria and human-approval policy hold. The executor revalidates all proposals using trusted state and treats missing fields and inconsistent joins as failures. Reorient may change plan and chosen child specialization; cannot change immutable intent/rights. Route selection prefers admitted internal typed FS operations to shell and uses tool equivalence only when semantics and permissions match.

## E. How the next graph is derived

1. **Observe:** ingest new `ToolResult`, child ReturnEnvelope, verified artifact diff, remote event or human instruction, each with provenance.
2. **Reconcile:** compare against current hypothesis, acceptance and scope; mark superseded assumptions; identify contradictions and material deltas.
3. **Score frontier:** priority combines intent relevance, expected information gain, impact, cost, risk and availability; policy may allow a cheaper local source before an expensive web round. Do not fabricate numeric precision from model text: optional weights are configuration, not objective truth.
4. **Propose:** model returns a small structured graph delta, *not* a full rewritten plan. In minimal capacity mode, allow only one next action and narrow feedback, reducing small-model output requirements.
5. **Admit:** check graph is acyclic for the current stage; nesting and repetition are bounded by independent controls. Check tool and model readiness, capabilities, sandbox, leases, actor spawn depth, branch write conflicts and human approvals.
6. **Execute:** schedule bounded cells/children/turns, checkpoint at durable transitions; a reused warm session is an optimization, not source of truth.
7. **Verify:** independent evaluator/judge/test as appropriate; derive acceptance evidence and potential new frontier from failures.
8. **Commit:** atomic, epoch-fenced checkpoint then decide complete, wait, blocked, revise or reorient.

Reorientation examples:
- Source contradicts current theory → `Revisit` evidence assumptions, re-rank frontier.
- Two modules share unexpected privileged dependency → `Fork` source and security traces, `Join`, `Nest` evidence review.
- Web is forbidden → `Blocked` for remote-only proof, or `Advance` local sources **without bypassing network policy**.
- Patch passes unit test, fails integration → add ripple dependency, switch one-file fix into contract wave if approved.
- User changes priority → new authoritative intent revision, invalidate only plan nodes that depended on changed criterion.

## F. Write-capable corrective cycles: strict transaction protocol

This is the most critical distinction from read-only research chains. A composed orchestrator with no direct write tools may *intend* modifications, but actual writing is delegated to a worker/executor with narrower authorization. The parent may change the assigned plan within already granted scope, not simply rewrite filesystem paths. A trusted runtime owns write leases and file reservations.

```text
confirmed finding / requested change
       ↓
intent + acceptance + base commit + canonical path set
       ↓
classify impact: read-only? one-file? cross-file? external effect?
       ↓
make patch proposal + affected API/consumer set
       ↓
preflight capabilities/rights/approval/worktree
       ↓
reserve disjoint file set & owner
       ↓
one writer per file (or validated contract cluster)
       ↓
read back diff + targeted review / adversarial test design
       ↓
ripple scan (callers, config, docs, tests, artifacts)
       ├─ unresolved → new bounded fix branch and re-review
       └─ clear
       ↓
central verification at pinned integration SHA
       ├─ red → identify owning scope; reorient, never pretend green
       └─ green → record test evidence and gates
       ↓
verified proposal for commit / PR / human completion
```

**Writer admission is separate from the pure transition core**. Revalidate actual canonical path/symlinks against the trusted workspace root, branch/worktree identity, index state, parent/child rights, lease ownership and preimage hashes at execution and merge time. A successful `ProposePatch` transition has no filesystem effect.

A patch operation must carry: `intent_id`, `base_sha`, `repository_ref`, `allowed_paths`, `expected_preimage_digests`, `change_kind`, `owner_worker`, `risk_class`, `review_plan`, `rollback_strategy`, `approval_ref` if needed, and `idempotency_key`. Recheck base/preimages immediately before edit and again before merge; an unseen file modification is a conflict, not permission for `git add -A`. For multi-file changes, reserve the entire declared contract set and reject undeclared files; untouched declared entries can be `change=false`.

Reversal is not always equivalent to `git reset`: before publication, prefer separate worktrees / clean revert of owned changes only; after publication, create explicit compensating commits; for irreversible external actions use a human-approved mitigation plan. A model can propose rollback, not erase someone else's changes without ownership proof.

### Specialized corrective decisions

- **False-positive finding:** reject with refuting evidence; no fixer created.
- **Single-file verified defect:** spawn one fixer on exclusively leased path, independent review, optional bounded repair.
- **Cross-file defect:** contract designer determines signatures/call sites and partition; one writer per file, ripple review.
- **Security-critical defect:** mandatory regression test and independent adversarial review, fail-closed acceptance.
- **Existing unrelated changes:** fence off foreign paths and report conflict; do not stage all changes.
- **Unverifiable remote claim:** keep as open; do not substitute local docs as official confirmation.
- **No-progress or provider failure:** switch to *admitted* alternate model/method only if feasibility changes; otherwise stop/escalate.

## G. What is dynamic and what is not

Dynamic: investigation frontier, hypotheses, number/order of segments, evidence-driven subrecipes, allowed model routing and effort, choice among already permitted tools/agents, corrective work boundaries *within* the approved write scope, re-test plans and synthesis audience detail. Static/governed: intent owner and hard constraints, authority ceilings, approved path and egress boundaries, model/provider global pacing, maximum nesting/spawn depth, budgets and approval rules. An agent can decide it *needs* a security reviewer; spawning one is conditional on its exact availability/authorization.

Boundedness requires **independent** `max_agent_spawn_depth`, `max_chain_nesting_depth`, `max_transitions`, `max_parallel_segments`, `max_tokens`, `max_wall_secs`, `max_external_effects`, `max_no_progress` and `max_artifact_bytes`. A nested chain can reuse permitted tools without increasing organizational spawn depth. All descendants charge the owner's budget and obey global provider pacing. Child contract `allow_pause=false` disallows synchronous handoff that requires pause; use approved background/return-based composition.

## H. Durable semantics and replay

An `AgentCycleCheckpoint` should pin job work id, epoch/lease, parent intent revision and digest, recipe version, branch frontier, decision proposal+admission verdict, model/provider/effort selection, evidence refs, owned artifact revisions, coverage, budget and next intent. **Crucially**, it stores a non-authorizing `harw_authority::AuthoritySnapshot` reference plus its bound workspace/policy provenance; on resume the trusted runtime must reissue and intersect that snapshot under current trusted policy (`harw-authority/src/lib.rs::PolicyBootstrap::reissue`; `harw-core/src/child_controller.rs::ChildRecoveryView`). Serialized checkpoints cannot grant or widen rights. Stale epoch cannot write. A crashed uncommitted action may be retried: external effects need idempotency identifiers, writes compare preimages and parent sees an explicit uncertain-effect state when success cannot be proven. A checkpoint stores *explicit argument/evidence summaries*, not private model chain-of-thought. Session compaction is not the checkpoint. Streaming partial tokens are not committed evidence.

## I. Root and sub-orchestrator control policy

Root first acquires bounded workspace orientation and plans a small DAG, then delegates research, file and implementation phases. Sub-orchestrator resolves local frontier and may request a capability handoff upward if its allowed child list excludes a specialist. Root is responsible for global intent reconciliation and cross-subtree conflicts, not raw worker transcripts; the worker receives one narrow piece. Root may reorient on new evidence by cancelling or superseding pending DAG nodes and creating replacements within a stable intent revision. Cancellation must propagate to jobs/waves, release file leases and preserve verified evidence/owned diffs. The UIA remains the place for user approval and specialized UiaWorker routes (such as LaTeX), never a justification to widen Root's child roles.

## J. Model selection for small open weights models

Use explicit task difficulty and model capabilities for each segment: a cheap local model for path classification, manifest understanding, search query formulation and extraction; an optional stronger or reasoning-tuned model for complex causal analysis or adjudicating contradictory evidence; deterministic code for path parsing, graph search, checks and tool admission. The `(model, effort)` pair from PL-69 represents a requested policy, not a magical fixed count of thinking tokens. If model/provider does not implement native reasoning-effort, use an *external* chain to structure operations; do not pass an unsupported effort parameter as if available.

For each candidate route measure actual max input/output, tool schema reliability, latency, error/403 status, provider budget, structured output adherence, permission feasibility and benchmark pass rate. A failed route should trip a bounded circuit breaker, preserving evidence state; repeated identical 403/host denial is not a reasoning iteration. Prefer tiny structured responses with stable IDs and local deterministic reducers. Verify that a ~30B composed explorer matches a stronger baseline on source precision, recall, tool errors, authority safety, completion and total normalized cost before claiming improvement.

## K. Domain-level stop conditions

A terminal decision is allowed only if the output contract passes; required criteria have evidence; no hard invariants are violated; independent reviewer gates pass where required; unresolved high-impact contradictions are visible; relevant external approvals are recorded; and no unfinished write lease remains. `Complete` signals chain terminal; `ProposeAchieved` is human-governed goal acceptance when WorkDriver-like contract requires it. `Blocked` and `Unknown` are honest successful safety outcomes and not to be recast as `Complete`.

## L. Minimal deterministic transition test matrix

- Refuting evidence changes graph from local search to verification, not repeated search.
- A fake `permission: full` inside a checkpoint or model tool output never changes actual authority.
- Nested `Nest` and agent-spawn depths are counted separately.
- Unauthorized file path in `ProposePatch` fails admission before write.
- Existing foreign file revision fails preimage match with no silent overwrite.
- A critical fix without regression-test evidence cannot reach accepted gate.
- Egress forbidden + web search recommended => blocked or local alternative, never shell bypass.
- Failed test creates corrective frontier, retains original criterion and records cost.
- Three equivalent no-information returns trip no-progress rather than unlimited retries.
- Stale job epoch cannot overwrite a newer checkpoint.
- Parent cancel releases leases and stops descendants; terminal state is unambiguous.
- Large generated artifacts referenced by digest do not overflow parent context.

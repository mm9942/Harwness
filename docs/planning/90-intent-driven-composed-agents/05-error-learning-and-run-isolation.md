# 05 — Error learning without shared agent context

> **DESIGN ADDENDUM / PARTLY IMPLEMENTED IN PR #151 (NOT MERGED).** Grounded in draft PR #136, branch ccr-d0359234-af3m9p at 8ac2715b3c29174e4a285cc9227dd2def8992eb6 (2026-10-08). This addendum specifies a required implementation and test contract. W03c (durable run observations: `CycleDriver` commits them into `CycleRecord.outbox` with the checkpoint), W03d (run isolation), W09a (job-local learning drain) and W09b (top-k lesson recall for the intent-cycle proposer) are IN PR (PR #151) with the limitations listed in section 4; W09c (audited global promotion) remains PLANNED. It is not evidence of verified lessons: the facts the W09a drain writes are low-confidence hints.

## 0. Non-negotiable principle

**Shared knowledge is not shared context.** Every agent has its own bounded run, session/history, scratch, context budget, rights, execution and job outcome. Several runs may consult one *indexed* knowledge substrate, but must not share their whole prompt, short-term memory, tool transcripts, private diary, internal reasoning or privileged runtime state.

A root/orchestrator receives only an explicitly permitted, bounded child return envelope and resolvable evidence references, not the child's private session. Sibling agents never get one another's contexts by default. A correlation trace is not a shared context or an access grant.

A run may be resumed after a fenced lease reissue; a new delegated agent gets a different run and different context. Intent/project/global identities support addressing and authorized discovery, not prompt concatenation.

## 1. Reviewed current code

| Module | CURRENT evidence | Required delta |
| --- | --- | --- |
| harw-core/src/child_controller.rs | ManagedAgentSpawner creates a governed child session through SessionManager with child role, parent linkage, intersected ContextCeiling, budget and cancellation. | Preserve run-local session and context assembly; define trusted per-run identity at admission. |
| harw-agent-runner/src/job_child_backend.rs and child.rs | The job-backed backend starts an independent process and protocol for each child when selected; the child owns/resumes its SDK session. | No pooling of child prompts; continuation token only reattaches to the authorized original child. |
| harw-job-core/src/stored.rs | JobScope binds tenant, workspace and submitter; StoredJob uses a lease and persisted trace. | JobScope alone does not identify an agent run: bind agent_id/run_id/session_id separately in a trusted adapter. |
| harw-plan-bridge/src/cycle_runtime.rs | CycleRecord is bound to cycle_id and WorkId; CycleDriver uses lease fencing, admission/refusal, execution, reconciliation and fenced commit. | Add typed, durable, run-owned failure observations, not a second job runtime. |
| CycleDriver::run | Executor errors proven to have no effect are warned and converted into empty CycleObservations; refusal feedback is local to the current loop. | Persist a cause classification and correlate later fixes; never promote a log message directly to a lesson. |
| harw-memory/src/short_term.rs | ShortTermMemory is session-labelled and bounded. | Enforce identity when using its content in turn assembly. |
| harw-memory/src/context_provider.rs | **IN PR (PR #151) (library contract).** `MemoryContextProvider::with_session_gate(true)` makes `fragments(ctx)` contribute no STM text, and derive no fact keywords from it, when `ctx.session_id` differs from `stm.session_id()`; HOT memory is kept. The gate is opt-in (default off, previous behavior). `MemoryContextProvider` has **no production caller**: the root registers the facts-only `MemoryFactsContextProvider` (`harw-runtime/src/assembly.rs`) and child registries mount no memory context provider. The root facts provider is **root-only by explicit policy** (W03d-1 audit; facts carry neither author nor visibility scope), pinned by `child_registry_has_no_memory_facts_provider` in `harw-runtime/src/children.rs`. | Any future STM mount must enable the gate. Child retrieval stays explicit: a trusted, policy-checked top-k query (W09b), never bulk project/global injection. This was a potential boundary gap, not proof of production cross-session leakage; with no production caller it stays a contract rather than an exercised path. |
| harw-knowledge/src/visibility.rs, caller_identity.rs, artifact.rs and context_provider.rs | **IN PR (PR #151) (identity-bound recall).** SelfOnly, DescendantTree, ExplicitlyGranted and OperatorOnly visibility; recall is visibility-filtered. `RecallQuery` can carry a trusted `CallerIdentity`; `artifact_visible` then decides by identity (`is_visible` over the indexed author and scope, never over frontmatter `extra`) at the direct-candidate, backlink and steward-leak checks, and without an identity the previous fail-closed scope heuristic (`visible_to_caller`) applies unchanged. `KnowledgeCaller::from_context` (`harw-ops`) reads the trusted `Arc<CallerIdentity>` service. Each child registry derives a run-bound identity per child from the child session the controller announces before the build (`ChildRegistryFactory::announce_child_session`, `run_agent_id` in `harw-runtime/src/children.rs`: `agent_id` = `<role>.<escaped child session id>`, `role` = base role) into the `OpContext` services of its operation tools; the same `agent_id` is the author of the child's `diary.read` and of its diary recorder entries. | Limitations in section 4 (W03d): no ancestors, no identity service at the root, child knowledge readers other than `diary.read` outside `OpContext`, one diary directory per child run. Never bypass filtered recall. |
| harw-memory/src/learning.rs, learning_gate.rs, epistemic.rs, outcome_tracker.rs, feedback.rs | Learning/promotion heuristics, candidate screening and outcome feedback already exist. | Adapt those mechanisms to typed run-local events and verified outcomes rather than inventing a competing memory system. |

At the PR #136 baseline there was no transactional learning outbox, no run-identity proof on a memory recall request and no production W09 consumer; the rows above describe that baseline. Since then the W03c outbox (including the `CycleDriver` integration), the W03d identity-bound recall, the parent-envelope sanitizer and the root-only facts policy, and the job-local W09a drain and W09b lesson recall of the intent-cycle worker are in PR #151 (section 4). Only the intent-cycle worker consumes its outbox, and there is no global promotion (W09c PLANNED).

## 2. Independent run context

### 2.1 Trusted, non-model-controlled identity

Resolve at agent admission, from the runtime, not from a model-generated JSON field:

~~~text
tenant_id, workspace_id, project_id
agent_id, agent_role, run_id, session_id, optional parent_run_id
job_id, attempt_id, lease_epoch, intent_id, intent_revision
effective policy / tool/role grants / visibility principal / context ceiling
~~~

A trace_id is telemetry only. A parent-child edge is not itself a permission. A new job attempt may continue its *own* run after lease and authority reissue, never another agent's run. Persisted recovery data carries references and evidence, not new grants.

### 2.2 Context assembly for one turn

~~~text
Agent A: own history + own STM + own role + admitted tools
       + explicit parent task envelope + authorized relevant retrieval

Agent B: own history + own STM + own role + admitted tools
       + its own parent task envelope + its own authorized retrieval
~~~

Both may refer to the same public or project-approved lesson. Neither receives the other's transcript, full result payload or private artifact. A child return to the parent contains bounded status, summary, typed evidence references and usage/failure metadata; dereferencing an artifact must check visibility independently.

Never mount the same mutable ShortTermMemory instance for two different session IDs. Never infer visibility from agent names in untrusted free-form metadata. The existing ContextCeiling constrains context sections/budget; it does not substitute for identity and content authorization.

### 2.3 Retrieval is explicit and bounded

Define a trusted LearningRecallRequest with caller binding, current intent/task labels, allowed knowledge scopes, evidence requirements, top-k limit, hard token ceiling and optional artifact references.

- Default child context contains only its own run, compiled/role context and approved handoff. No implicit sibling or complete project/global transcript.
- Global home is an **indexable knowledge scope**, not a prompt automatically injected into all agents.
- Project/global facts are loaded only through policy-checked retrieval. Scope filtering runs **before** relevance ranking. Use current context ceiling and role authority; render lessons as lower-trust data, never as instructions or rights.
- Retain current root experience only behind an explicit audited compatibility policy; do not silently change root behavior while migrating child isolation.
- All delivered items have provenance, version, expiry, trust, usage/outcome status and revocation/supersession hooks. References are preferable to large copied bodies.

## 3. Learning from failure: proposed exact wiring

~~~text
one run / one CycleDriver / one CycleRecord / one fenced Job lease
  -> proposal + admission / tool / verifier / recovery outcome
  -> bounded typed RunObservation (no raw private content)
  -> SAME fenced atomic CycleStore record transaction as checkpoint
  -> durable per-run outbox
  -> idempotent scoped learning consumer
  -> existing harw-memory epistemic + learning_gate + outcome checks
  -> project-scoped proposal / eventually independently confirmed lesson
  -> authorized top-k retrieval into a later agent's OWN context
~~~

A debug log or best-effort observer after the checkpoint is not a durable learning mechanism. Crash after checkpoint before delivery must leave a pending outbox event. A stale epoch may neither append nor acknowledge an observation. Event deduplication is based on a stable (cycle_id, committed checkpoint sequence, local event index) key and a trusted source owner, NOT on text similarity alone.

**Typed RunObservationV1 fields as proposed (the record in `harw-plan-bridge/src/cycle_observation.rs` (PR #151) carries `schema_version`, `event_id`, `intent_id`/`intent_revision`/`intent_digest`, `transition`, `category`, `cause`, `evidence_ids`, `outcome` and `epoch`; it has no `owner` or `visibility` field):**

~~~text
event_id: {cycle_id, sequence, index}
owner: trusted {tenant, workspace, project, agent_id, run_id, job_id}
intent: {id, revision, digest}
transition: known CycleProposal kind
category: admission_refused | tool_failed_no_effect | capability_unavailable
        | provider_unavailable | verification_failed | contradicted
        | uncertain_effect | recovered | validated_fix
cause_code: bounded typed enum, never raw error bodies
evidence_refs: immutable locators, content digests and trust levels
outcome: pending | attempted | confirmed | refuted | inconclusive
checkpoint: epoch, sequence, record schema
visibility: private-by-default; explicit promotion eligibility
~~~

Never persist raw prompts, model reasoning, shell commands, file contents, secrets, credentials, unredacted provider error bodies, or authority snapshots in a learning event. Models may propose an explanation; only trusted observations and deterministic gates can confirm a correction.

### 3.1 Failure classification

| Observed case | Existing run policy | Correct learning policy |
| --- | --- | --- |
| Invalid proposal, unauthorized tool, forbidden scope | Refuse, bounded feedback, fail closed | Record typed refusal without learning a privilege bypass. |
| Tool error proven no external effect | Stall or escalate | Candidate operational pitfall, only promoted after verified correction or repeated bounded evidence. |
| Effect uncertain after crash | Reconcile/escalate, never blind replay | Record unresolved uncertainty; cannot count it as a fix. |
| Provider 403 / 429 / unavailable model | Block or provider-owned pacing | Time- and configuration-scoped capability hint; cannot generalize to global advice forever. |
| Contradictory repository evidence | Revisit affected claims | Recheck and supersede only affected hypotheses. |
| Failing test later fixed by confirmed regression | Existing owner acceptance still governs completion | Publish project lesson with references to failing and passing evidence and environment/version. |
| Learned workaround later refuted | Reorient or stop | Quarantine/downgrade/supersede the lesson; do not repeatedly inject stale advice. |

**Reported, Observed and Verified are different.** A model saying “fixed” is not a verified outcome. CompletionProposed is not owner-accepted achievement. Lessons should be scored by later successful outcomes, not by generated lesson count.

## 4. Ownership and migration

**W03c — durable run observations — IN PR (PR #151) (intent-cycle driver; consumer is the W09a drain):** Add typed event classification and a bounded atomic outbox to harw-plan-bridge::cycle_runtime / existing CycleStore. Keep it job-local and avoid a dependency from generic harw-job-core to Harwness memory code. Preserve existing read-only W03 behavior and lease fencing. No global promotion at this stage. *Status: the observation types and the epoch-fenced `Outbox<T>` (`cycle_observation.rs`, `cycle_outbox.rs`, see 04 section 7.7) are wired into `CycleDriver` (`cycle_runtime.rs`): it calls `build_observation` at refusal, permanent proposer failure, executor failure, uncertain reconciliation and verification (unmet or stale claims) and commits the observations through `CycleStore::commit_step_observed` into `CycleRecord.outbox` in the same CAS write as the checkpoint. A permanent proposer failure is observed as `ProviderUnavailable` / `ProposerFailed` (`classify_proposer_failure`), never as a tool failure; fence refusals cannot be recorded in the record they are refused for and are only logged. Pending observations are read only lease-fenced via `CycleStore::pending_observations(cycle_id, token)`; the consumer is the W09a drain below.*

**W03d — run identity and provider isolation — IN PR (PR #151) (identity-bound recall, parent envelope sanitizer, root-only facts pinned by test), with the limitations below.** The wave bound the agent identity in the trusted composition layer, audited `RuntimeAssembly` and the child registries for provider reuse, parent handoff and fact injection, added the session gate to `harw-memory::MemoryContextProvider` and chose explicit root compatibility and child default-deny behavior:

- **Identity-bound recall:** `RecallQuery::with_caller_identity` plus `artifact_visible` (`harw-knowledge`); `KnowledgeCaller::query` (`harw-ops`) builds the query. Tests: `identity_recall_sees_self_only_artifacts_of_the_author`, `sibling_identity_never_sees_sibling_self_only`, `query_without_identity_keeps_scope_heuristic`, `forged_identity_metadata_in_frontmatter_does_not_grant_access`, `caller_with_identity_builds_identity_bound_query`.
- **Child identity:** `build_registry` derives an immutable, run-bound identity once per child from the registered role and the child session id (`run_agent_id`: `agent_id` = `<role>.<escaped child session id>`, e.g. `explorer.3f2b8c1e-…` for a UUID session id), never from `SpawnInput` or model JSON. The controller announces the child session before the build (`ChildRegistryFactory::announce_child_session`; a build without an announcement derives the id from a fresh session id), so a durably recovered child keeps the id, diary and `SelfOnly` identity of its original run. The role has path separators, `:` and control characters replaced by `-` and the session id has `%`, `.`, path separators, `:` and control characters `%XX`-escaped, so the id is a single path-safe component (it also passes `diary::validate_agent_id`) and stays injective. The id stays the same for the child's whole run and is the author everywhere the child registry hands one out: the `CallerIdentity` in the `OpContext` services of its operation tools, the `diary.read` provider and the diary recorder (`child_session_observers`). No sibling shares the instance or the id, **including siblings of the same role**. Tests: `child_registry_carries_caller_identity_of_its_role`, `child_diary_records_compaction_and_one_session_end_under_the_run_bound_id`, `siblings_do_not_share_session_registry_or_identity`, `announced_child_session_makes_the_run_identity_stable_across_a_restart`, `run_agent_id_is_a_safe_and_injective_path_component` and the end-to-end `same_role_siblings_cannot_recall_each_others_self_only_artifacts` (`harw-runtime/tests/sibling_isolation.rs`).
- **Parent boundary:** `harw_research::sanitize_for_parent` bounds identifiers, lists, summary and payload of a child `ReturnEnvelope` before the parent sees it; a violation is a content-free contract violation (`return_envelope_with_oversized_payload_is_a_content_free_violation`, `return_envelope_summary_is_truncated_not_refused`). `harw-plan-bridge/src/child_return_envelope.rs` stays the contract for future `Segment::Child` steps; its caps are pinned equal (`caps_match_harw_research_envelope_caps`).
- **Provider isolation:** the root facts provider is root-only by explicit policy and children mount no memory context provider (`child_registry_has_no_memory_facts_provider`). The session gate is a library contract without a production caller.

Known limitations (not hidden by the IN PR (PR #151) status):

1. **No ancestors.** `SpawnInput` carries only `parent_session_id`, no parent role, so `CallerIdentity::ancestors` is empty for every child. This fails closed: a child does not see its parent's `DescendantTree` artifacts. Sibling isolation does not depend on it, because a sibling's author is neither the caller nor one of its ancestors. Populating ancestors needs a trusted spawn record from the admission layer.
2. **Reach of the child identity.** The `CallerIdentity` is carried into the `OpContext` of a child's operation tools (`delegate_wave`, the Matrix game-master tools). The other child knowledge readers (`palace.*`, `workbench.show`, `kanban.*`) are legacy providers outside `OpContext`; they keep their own bounds (`established` palace nodes, project-capped workbench) and do not consume the identity. `diary.read` is also outside `OpContext` but reads under the same run-bound `agent_id`.
3. **No identity service at the root.** The root `ServiceMap` carries no `CallerIdentity`, so operator and root reads keep the scope heuristic.
4. **Session-derived, not job-bound.** The id embeds the child session id (stable across durable recovery) but is not a job run id, and `CallerIdentity` has no separate session or job field. A build without an announced child session (a direct `build_registry` call) derives it from a fresh session id. The sibling tests cover registry, identity and recall objects, not child sessions, STM, budgets or job records (acceptance tests 1 and 3 below stay open beyond that).
5. **Per-role hand-overs.** The announced child session (`announce_child_session` to `build_registry`) and the run identity (`build_registry` to `child_session_observers`, which feeds the diary recorder) are handed over per role; both rely on the child controller holding its session lock across the calls (it does today). Without a handed-over identity (the agent-steward special registry never passes through `build_registry`) the observer hook derives the id from the child session (`run_agent_id`), never the bare role name.
6. **One diary directory per child run.** `diary/<role>.<child session id>/` replaces the shared `diary/<role>/` (a recovered child keeps its id and so its directory); the operator `/diary` listing and Dream's diary scan (both use `list_agents`) now see one agent per child run and no longer merge a role's history.
7. **Session gate is opt-in** and has no production caller, so no per-session STM mount exists to protect today.

**W09a — scoped learning consumer — IN PR (PR #151) (job-local, project scope only), with the limitations below.** Drain the per-run outbox idempotently via the composition layer. Reuse harw-memory learning_gate/epistemic/outcome_tracker and harw-knowledge visibility; store source ownership, never auto-promote to global. *Status:* `harw-cli/src/job_worker_intent_cycle_learning.rs::drain_cycle_observations`, called once by the intent-cycle worker (`job_worker_intent_cycle.rs`) after every driver run (all result arms, wall-clock timeout included; a drain failure is logged and never changes the job outcome). It runs only when the trusted job input names a project memories root (`IntentCycleJobInput::learning_root`, written solely by `intent_cycle.start` from the operator's resolved home, `<project>/.harw/memories`; never a tool argument). The pending outbox is read through the store's lease fence (`CycleStore::pending_observations(cycle_id, token)`): a stale epoch, another job's lease or a not yet rebound lease gets an error before any payload leaves the store, and the drain then opens no memories, writes and acknowledges nothing. Every learnable category (tool failure without effect, capability or provider unavailable, verification failed, uncertain effect, contradicted) becomes one deterministic, text-free project `Pitfall` fact (enum names and ids only, confidence 0.3, tag `pl90`, name derived from the observation's dedupe key, so redelivery rewrites the same file), screened by `harw-memory::learning_gate` and acknowledged under the claim's lease. `AdmissionRefused` is never learned (acknowledged without a fact), nor are `Recovered` and `ValidatedFix`; an observation naming another cycle is screened out. The global scope is never opened. Every fenced `CycleStore` operation (the driver's rebind, journal and commits as well as the drain's read) and the drain's acknowledgement are bound to the live job-store lease through `CycleStore::with_lease_liveness` and `JobStore::verify_lease` (nonce, epoch, expiry, terminal job state; read-only, and checked inside the record lock of each write), so an expired or revoked claim cannot read, commit or acknowledge even before a reclaim rebinds the cycle. The drain decides in memory, acknowledges once through that fence and only then writes the facts (no check-then-write window; a write that fails after the acknowledgement loses that lesson), and a terminal cycle is rebound to a newer lease before `run` returns `Terminal`. Tests: `drain_writes_project_pitfalls_and_acks_exactly_once`, `stale_token_reads_nothing_and_writes_no_facts`, `admission_refusals_are_acked_but_never_learned`, `drain_never_opens_global_scope`, `observations_of_another_cycle_are_never_learned`, the fence regressions `expired_claim_cannot_learn_even_before_a_rebind`, `cancellation_after_expiry_writes_no_fact` and `lease_lost_between_read_and_ack_writes_no_fact` (all in `job_worker_intent_cycle_learning.rs`), end-to-end `worker_learns_project_pitfalls_from_failed_verifications` (`harw-cli/src/job_worker.rs`). Limitations: epistemic/outcome_tracker scoring and harw-knowledge visibility are not used yet, so a fact is a low-confidence hint, not a confirmed lesson; only the intent-cycle worker consumes its outbox; there is no drain before the run (the fence needs the lease the driver rebinds inside `CycleDriver::run`); entries an earlier claim left between commit and ack are pending under the new fence after the rebind and are learned and acked by the drain after the run; the end-to-end worker path with an expired lease is covered at module level (a driver `run` with a cancelled token followed by the drain), not through the scripted-provider worker harness.

**W09b — indexed retrieval — IN PR (PR #151) (intent-cycle proposer only), with the limitations below.** Build a tiny task-matched, trust- and visibility-filtered retrieval path. Allow future runs to benefit from confirmed lessons but do not share source agent histories, prompts or STM. *Status:* `job_worker_intent_cycle_learning.rs::lesson_lines` lists the project facts of the same root and keeps only project-scoped `pl90` lessons *before* any limit (ordinary notes cannot crowd a lesson out), ranks them by the number of objective keywords they match (ties by name), de-duplicates identical lines and selects through `harw-knowledge::learning_recall::select_lessons` (top-k ≤ 8, ≤ 1024 estimated tokens). The worker hands the lines to `ModelCycleProposer::with_lessons`, which renders them as single-line, low-trust data under `LESSONS`, never as instructions or rights. Tests: `lesson_lines_are_bounded_to_eight_and_sanitized`, `lessons_are_not_crowded_out_by_ordinary_facts`, `identical_lessons_are_recalled_once`, `keywords_are_lowercase_deduped_and_capped`, and the recall leg of `worker_learns_project_pitfalls_from_failed_verifications`. Limitations: keyword matching only; `require_verified` is off because no lesson is verified yet; no expiry, supersession or revocation; no other agent or child prompt recalls lessons.

**W09c — audited global promotion — PLANNED:** Explicit owner review for promotion from project to global home, expiry/supersession/revocation, optional embeddings as retrieval indexes (not new authority or source truth), recall-usefulness and regression metrics.

Do not use Dream/Diary/Palace as ambient extra context. They are independent, scoped knowledge surfaces; a Dream proposal must pass the same promotion gate. Never let learned material change sandbox profile, rights, agent roles, model credentials, child-spawn ceiling or approval requirements.

## 5. Mandatory acceptance tests

1. **Two siblings:** concurrent child A and B have independent sessions, prompts, STM, token budgets and job records. B cannot receive A's private diary, transcript, reasoning or raw tool output even under the same trace or project.
2. **Bounded parent return:** parent sees only an explicitly authorized ChildReturn and evidence references; exact artifact dereference rechecks visibility.
3. **Provider mismatch:** try to assemble B's turn with A's MemoryContextProvider/ShortTermMemory. A-private STM contributes zero bytes; rightful A recall is unchanged.
4. **Scope denial:** cross-tenant/workspace/project/private-agent memory is absent even with exact-match search or forged metadata. Missing identity proof fails closed.
5. **No bulk child injection:** global/project preference, pitfall and index content does not automatically flood a child prompt. A trusted top-k query can return a relevant approved lesson within budget.
6. **Crash matrix:** kill between journal, execution, checkpoint, outbox publish, outbox ack. Accepted observations survive and apply exactly once despite at-least-once delivery.
7. **Lease fence:** stale attempt cannot commit learning, ack another owner's outbox or read newer context. A new epoch reissues authority before resuming.
8. **Failure fidelity:** no-effect tool error remains a failure observation; uncertain external effect never becomes “fixed”; provider 403 cannot trigger unlimited retries.
9. **No policy laundering:** a rejected full_access/tool/egress proposal can never become a learned permission change. Raw adversarial tool content remains untrusted data.
10. **Promotion and refutation:** repeated guess alone remains proposed; independently verified regression promotes a project lesson; contrary evidence quarantines it; deleted facts are not reintroduced.
11. **Efficiency:** context size does not grow with sibling count, earlier agent runs or project job history; retrieval obeys fixed token/top-k limits.
12. **Existing tests:** W01–W03 cycle safety, epoch fencing, WorkDriver owner acceptance, child controller and architecture gates stay green.

Every implementation wave must record its actual tested commit and before/after behavior in the PL-90 migration ledger. Documentation is not a passing runtime test.

## 6. Definition of done

A failing action in agent run A produces a durable, scope- and source-bound, falsifiable candidate. After verification, a **later authorized** run B can retrieve a short relevant lesson and measurably avoid repeating the failure. **B still cannot read A's private context.** A solution that makes error-learning work by pooling all agent transcripts, injecting all global memory into every run, relaxing visibility, or auto-changing permissions fails this requirement.

The new design must preserve the existing JobManager, per-run admission, resume and RecordStore primitives rather than introducing another global chat executor.

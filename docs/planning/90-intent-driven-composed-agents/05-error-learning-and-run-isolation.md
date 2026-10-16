# 05 — Error learning without shared agent context

> **DESIGN ADDENDUM / NOT IMPLEMENTED.** Grounded in draft PR #136, branch ccr-d0359234-af3m9p at 8ac2715b3c29174e4a285cc9227dd2def8992eb6 (2026-10-08). This addendum specifies a required implementation and test contract; it is not evidence of a working learning consumer.

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
| harw-memory/src/context_provider.rs | fragments(ctx) does not directly compare ctx.session_id and stm.session_id(); optional project/global facts include a project index, preferences and pitfalls as unconditional fragments. | Audit real callers and prevent a provider from contributing a different session's private STM; restrict broad child memory injection. This is a potential boundary gap, not proof of production cross-session leakage. |
| harw-knowledge/src/visibility.rs and context_provider.rs | SelfOnly, DescendantTree, ExplicitlyGranted and OperatorOnly visibility; recall is visibility-filtered. | Reuse this policy and carry a trusted caller identity; never bypass filtered recall. |
| harw-memory/src/learning.rs, learning_gate.rs, epistemic.rs, outcome_tracker.rs, feedback.rs | Learning/promotion heuristics, candidate screening and outcome feedback already exist. | Adapt those mechanisms to typed run-local events and verified outcomes rather than inventing a competing memory system. |

There is no transactional learning outbox, no run-identity proof on a memory recall request and no production W09 consumer in PR #136 at this baseline.

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

**Typed proposed RunObservationV1 fields (not landed code):**

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

**W03c — durable run observations:** Add typed event classification and a bounded atomic outbox to harw-plan-bridge::cycle_runtime / existing CycleStore. Keep it job-local and avoid a dependency from generic harw-job-core to Harwness memory code. Preserve existing read-only W03 behavior and lease fencing. No global promotion at this stage.

**W03d — run identity and provider isolation:** Bind agent/run/session in the trusted composition/admission layer. Audit RuntimeAssembly and child registries for provider reuse, parent handoff and fact injection. Add a session-ID regression gate to harw-memory::MemoryContextProvider. Choose explicit root compatibility behavior and child default-deny behavior.

**W09a — scoped learning consumer:** Drain the per-run outbox idempotently via the composition layer. Reuse harw-memory learning_gate/epistemic/outcome_tracker and harw-knowledge visibility; store source ownership, never auto-promote to global.

**W09b — indexed retrieval:** Build a tiny task-matched, trust- and visibility-filtered retrieval path. Allow future runs to benefit from confirmed lessons but do not share source agent histories, prompts or STM.

**W09c — audited global promotion:** Explicit owner review for promotion from project to global home, expiry/supersession/revocation, optional embeddings as retrieval indexes (not new authority or source truth), recall-usefulness and regression metrics.

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

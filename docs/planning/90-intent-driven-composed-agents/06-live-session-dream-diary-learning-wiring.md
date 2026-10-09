# 06 — Live-session cognition: Dream, Diary, Learn and governed memory wiring

> **DESIGN / PLANNING ONLY — NOT IMPLEMENTED.** Addendum to PL-90, draft PR #136. Inspected baseline: mm9942/Harwness, branch ccr-d0359234-af3m9p at 13fa7f1b81b5b25b9c9498fc9d1f926079cd90dc, 2026-10-08. This is a design and verification contract, not proof of active background learning. PR #136 is stacked on #135; other collaborators may concurrently advance its branch. This addition deliberately changes one new documentation file only.

**Companion specification:** [05 — Error learning without shared agent context](05-error-learning-and-run-isolation.md). Its W03c transactional observations, W03d run identity, and W09a–W09c scoped learning, retrieval and global promotion boundaries remain authoritative. Do not bypass these constraints to achieve an early demonstration.

## 0. Problem statement

A continuously active conversation can last hours without a meaningful automatic Diary entry, Learn scan or Dream run. The user might correct the model, make architectural decisions, fix an error, approve a direction and explain a recurring workflow. Nevertheless:

- The existing Diary recorder writes on model-backed compaction or session close, not at an important moment during a running session.
- The automatic Dream scheduler lives in the Gateway, responds to idle time or configured cron and is not a live TUI-session event listener. A busy session never reaches the idle threshold.
- The existing Learn scan is a manual slash command; the foreground turn does not automatically call it.
- The opt-in learning_extract LLM pipeline requires a worker and pending digests; current assembly submits such work on startup rather than at milestones during an open session.
- Existing contextual retrieval, Palace, knowledge indexing, feedback and memory heartbeat do not themselves schedule an in-session reflection.

**Goal:** important *committed and trusted* session moments cause inexpensive, durable, scoped capture while the chat continues. Carefully admitted background work may then produce Diary checkpoints, Learn proposals, Dream reflection reports, memory candidates and eventually narrowly retrievable approved lessons. The operator can tell what actually ran, what was deferred, and what awaits review. The model may propose a lesson, never silently declare it true or broaden its authority.

## 1. CURRENT: confirmed source-level wiring

| Code owner / anchor | CURRENT | DELTA |
| --- | --- | --- |
| harw-runtime/src/diary_wiring.rs — DiaryRecorder | Observer chaining counts turns and records Compaction and EndOfSession entries. | No meaningful in-session milestone trigger, nor periodic factual checkpoint. |
| harw-knowledge/src/diary.rs — DiaryTrigger | EndOfSession, Compaction, DreamReflection and Manual are the persisted labels. | Extend format compatibly for Milestone; verify historical JSONL, Markdown, range readers and rollups. |
| harw-runtime/src/assembly.rs | Creates DiaryRecorder and compaction/tool observers for root session; registers close hooks. The TUI/OneShot receives an explicit manual RuntimeDreamLauncher. | No unified post-persisted-turn milestone dispatcher, no live TUI Dream scheduler, and no auto /learn invocation. |
| harw-cli/src/gateway.rs — dream_scheduler | Gateway Tokio ticker evaluates persistent scheduler state and activity/cooldown, then launches Dream. | Active conversation continually refreshes the idle condition. Cron can still fire but does not respond to important events. Gateway lifecycle does not prove local TUI coverage. |
| harw-config/src/harness_config.rs — DreamToml | Code defaults: enabled=true, idle=15 min, cooldown=60 min, token budget=16,384; optional UTC cron replaces idle triggering. | Do not confuse enabled configuration with a running scheduler. Older docs/design/knowledge-surfaces.md §4.3 says disabled by default; reconcile this documentation/code discrepancy without silently changing the default. |
| harw-ops/src/dream_run.rs | Single shared Dream execution core: lock, state, Dream Job/lease, model reflection without tools, repair, maintenance, bounded report and pending suggestions. | Reuse it; do not create a second ungoverned reflection engine or write directly to permanent memory. |
| harw-runtime/src/dream_run.rs | RuntimeDreamLauncher builds a bounded redacted context from recently modified profile transcript files and other knowledge. | A profile-wide transcript selector is not an authorization proof. Require trusted owner/run/scope filtering before reusing it for child or active-session reflection. |
| harw-ops/src/learn.rs | Explicit /learn scan loads current-session history, examines user sentences deterministically and files review-gated Learn/Skill proposals. MAX_CANDIDATES is 8 across one scan. | No automatic incremental scan. Early qualifying sentences may crowd out consequential later moments; design per-window prioritization. |
| harw-memory/src/llm_extract.rs; harw-ops/src/learning_job.rs | Opt-in redacted digest, a governed learning_extract job, candidate gates and _incoming destination. | LLM extraction is disabled by default; current work is keyed to quiescent session digests. An active writer must not be treated as a completed session or have its future evidence deleted. |
| harw-runtime/src/memory_wiring.rs | MemoryCaptureObserver sees user/assistant/tool events; on_turn_finished is a deliberate no-op for consolidation. MemoryConsolidationHook flushes at session close; startup sweep repairs pending state; enqueue_learning_extract is called during startup. | No reliable live-session checkpoint/extraction scheduling. |
| harw-core/src/capture.rs; harw-core/src/agent_events.rs | Cheap synchronous observer methods exist. AgentEventHub is an explicitly lossy, nonblocking live-event broadcast. | The broadcast is suitable for status/wakeup, not an authoritative durable journal; verify a real post-transcript-commit seam. |
| harw-knowledge/src/context_steward.rs | Proposal curation and StewardDigest logic exist; documentation explicitly says the digest-building functions lack a production caller. | Optional composition-layer caller needed; proposal curation is not automatic approval. |
| harw-memory; harw-knowledge; harw-lens-source | Distinct fact/memory/Diary/Palace stores, visibility scopes, physical visibility-index buckets, digest-based embedding cache and retrieval facilities. | Automatic capture, promotion and targeted next-turn recall are distinct workflows; do not confuse indexing with authorizing memory. |
| PL-90 W01–W03 in PR #136 | Intent-cycle admission, fenced durable execution, read-only explorer and proposer building blocks. No full production caller. | W03c learning outbox, W03d run binding and W09a–c consumers are still PLANNED; coordinate with [05](05-error-learning-and-run-isolation.md). |

**Root-cause hypothesis to falsify:** trigger coverage is lifecycle-based and disconnected: closed session / compaction / Gateway idle / manual scan / startup sweep. A perpetually active open session misses them by design. The absence of automatic activity must be tested per entry mode before assuming identical behavior in TUI, Telegram and other clients.

## 2. TARGET: SessionCognitionCoordinator, not another autonomous model daemon

A single composition-layer coordinator is proposed; expensive work is always a governed Job with budget, lease, scope and cancellation, whereas event recognition and durable bookkeeping are cheap and deterministic.

~~~text
TUI / OneShot / Gateway / Telegram / Work / Chat / child outcome
                  |
     trusted committed turn / tool / session event
     + owner principal + source offset + visibility
                  |
                  v
       SessionCognitionCoordinator (PLANNED)
           |
           +-- incremental classifier (no LLM)
           +-- per-run watermark, durable outbox, dedup
           +-- salience, coalescing, cooldown, admission
           |
           +--> DiaryCheckpointWriter --> existing harw-knowledge Diary
           +--> LearnSignalScanner   --> existing LearnProposalStore / SkillProposalStore
           +--> ExtractionDispatcher --> existing learning_extract job / _incoming
           +--> ReflectionDispatcher --> existing governed Dream run / proposal report
           +--> StewardDigestBuilder --> existing context_steward, proposals only
           +--> IndexRefresh         --> existing KnowledgeIndex / harw-lens
           |
           +--> status, observability, operator review
           |
           +--> authorized, bounded retrieval for a LATER turn
                  (never pooled child prompts or shared STM)

PL-90 W03c CycleDriver checkpoint/outbox
  --> typed verified-or-uncertain RunObservation
  --> W09a scoped learning consumer
  --> same knowledge proposal gates; no second job runtime
~~~

Ownership is narrow. harw-core retains generic cheap hooks, harw-session-store (or verified persistence adapter) owns committed source offsets, harw-runtime composes policies/orchestration, harw-ops adapts commands and proposal stores, harw-memory owns signal/gate/fact machinery, harw-knowledge owns scoped artifacts, existing JobStore/worker owns durable expensive work, and harw-lens-source owns indexing. Generic harw-job-core MUST NOT depend on Harwness memory domain. DoD and Warden remain out of the model-reflection dependency graph.

The coordinator is not allowed to cause one agent to receive another's transcript, scratch, model reasoning, private diary or short-term memory. The root may receive a bounded approved child return and independently visibility-checked evidence references, not child prompt internals.

## 3. The trusted event and persistence contract

Proposed type, **not implemented**:

~~~text
SessionMomentV1 {
  schema_version,
  event_id: (source_kind, source_id, committed_sequence, local_index),
  principal: {tenant_id, workspace_id, project_id?, agent_id, run_id,
              session_id, parent_run_id?, role},
  timestamp, source_ref, source_digest, visibility,
  moment_kind: ExplicitRemember | Correction | DurableDecision |
               RecurringWorkflow | ToolFailure | VerifiedRecovery |
               ApprovalDecision | EvidenceChanged | IntentBoundary |
               ChildOutcome | Compaction | PeriodicCheckpoint |
               Pause | Resume | Close,
  classified_signals: bounded list of typed enums,
  evidence_refs: [{locator, content_digest, verification_level}],
  payload_ref: optional scoped, short-lived source pointer,
  status: Pending | Recorded | Proposed | Deferred | Skipped | Failed,
  job_id?, lease_epoch?, retry_count, reason_code?
}
~~~

All principal, tenant, run/session, lease epoch, sequence and source identity fields derive from trusted admission and persistence, NEVER from model prose or child-supplied JSON. Raw private prompts, unredacted tool outputs, provider errors, secrets, shell commands, source bodies and authority snapshots do not belong in the shared moment/outbox record. Dereferencing an artifact or payload_ref rechecks caller visibility; an id or digest alone is not authorization.

**Commit ordering:** attach dispatch to a verified durable transcript/turn commit. The present ToolOutcomeObserver is synchronous; no universal post-commit callback has been established by this review. First trace source ordering for every ingress. If stores cannot atomically commit source and outbox, retain a persisted monotonic source high-water mark and recover missing events by scanning only committed source records. Do not advertise cross-store atomicity without proof.

**Delivery and fencing:** at-least-once with deterministic event_id, idempotent consumers, ordered session cursors, bounded queue, explicit replay/reconcile after crash. Persist dispatch intent before job admission and ack only after checking target artifact/job state. A stale lease cannot append or acknowledge another run's events. W03c has its own fenced transactional CycleStore outbox; reusing its *semantics* is required, but forcing chat events into CycleStore is not.

**Broadcast:** use AgentEventHub only to update TUI/Workbench status or wake consumers, because its lagging subscribers can lose events. The journal/source cursor determines truth.

## 4. In-session trigger matrix

The cheap stage processes every relevant **committed** event incrementally without LLM, full-session re-read or provider call. An optional bounded second-stage model is admitted only if deterministic evidence is insufficient, and produces a proposal.

| Trigger | Synchronous/nonblocking observation | Deferred governed action | Guardrail |
| --- | --- | --- | --- |
| User explicitly says remember / never / new lasting preference | Signal + cursor + owner | Scoped Learn proposal; optional short Diary entry | Does not directly modify global memory or agent rules |
| User corrects agent | Correction and source evidence | Learn proposal / focused factual checkpoint | The correction is user-authored, not proof a technical fix succeeded |
| Decision or accepted plan change | Mark verified actor/approval vs tentative text | Diary milestone and project topic proposal | Assistant speculation is not a binding decision |
| Tool fails, then an independent verifier later confirms the fix | Link failed+passing evidence and outcome | Error lesson candidate via existing gates | No verified status from model self-report |
| Evidence, goal or plan phase changes | Record trusted typed change / pending question | Short reflection proposal | IntentCycle may only change at the existing trusted transition gate |
| Child produces governed result | Capture bounded ChildReturn references | Root/project learning if policy allows | No sibling context, no raw private child history |
| **12 completed turns or ~20 active minutes** with meaningful new events | Coalesce factual checkpoint | Optional small summarization | Proposed pilot values only; no spam for chatter |
| Each tool call / token burst with no meaningful state change | Update bounded counters | None | No Dream per tool call |
| Compaction, pause/detach, resume, close | Existing hooks + cursor reconciliation | Flush pending slices without blocking foreground | Detach from phone is NOT cancellation or permission promotion |
| Gateway idle / configured cron / manual Dream | Existing scheduler/admission | Existing full Dream | Preserve current behavior; add events alongside, not as an undocumented override |

Prioritize important signals over time-only checkpoints. Maintain per-run last checkpoint, last scanned sequence, last confirmed observation and pending job; after one busy hour the coordinator should not keep rescanning all earlier messages. When more than eight significant candidates exist, retain high-priority **late** candidates rather than allowing a first-page ceiling to hide them.

**Starvation invariant:** a 3-hour continuously busy TUI or channel session with committed significant moments must yield an observable in-session checkpoint or Learn proposal WITHOUT waiting for idle time, compaction, close or a new session. If provider or worker is missing, deterministic capture still occurs and the background stage reports NoWorker / Deferred rather than fictitious completion.

## 5. Diary — timely, factual, append-only

Extend DiaryTrigger with a version-compatible Milestone variant (or an explicit supplementary type); preserve existing EndOfSession / Compaction / DreamReflection / Manual labels and historic readers. Verify read_day_entries, JSONL sidecar parsing, markdown recovery, retention rollups, /diary show/search, index rebuild and exported transcripts.

A bounded milestone contains: precise timestamp, source session/run, typed cause, 1–4 factual sentences, explicit unresolved issue, evidence refs, trust status and visibility. For example, distinguish “user accepted approach” from “assistant suggested approach”; “test confirmed green” from “model said fixed”. Redact private content. Prefer deterministic wording from typed events; use a small optional summarization job only when needed and label inferred content.

Diary append must be idempotent by moment id under existing knowledge lock, visible in the existing diary surface and scoped to the writing agent/operator. Debounce and merge adjacent related moments. No global diary mirroring or silent modification of existing entries. An error should be recoverable and visible but never fail the foreground conversation.

## 6. Learn — incremental signal scanning, review-gated

Extract the existing pure sentence classifier and filters from harw-ops/src/learn.rs behind a shared tested scanner rather than making harw-runtime call a slash command. Preserve /learn scan as explicit operator repair/full scan with its existing visible grammar. Move automatic processing to bounded new committed windows using a per-session cursor, not full-session O(n) replay each turn.

Only user statements have authority to express a durable user instruction; tool/verifier events may independently support operational failure learning. Normalize fingerprint with owner scope/source version and text; persist accepted/rejected/superseded status so retries and late corrections do not flood proposals. Ensure MAX_CANDIDATES=8 per old manual scan does not starve later meaningful events: use bounded per-window extraction, priority/coalescing, pending-queue limit and expiry.

Reuse LearnProposalStore, SkillProposalStore, harw-memory learning_gate, epistemic, feedback and outcome tracking. The existing /learn accept only **marks** certain proposals and /skills accept is an explicit decision. Automatic scans must preserve that separation. A malicious README or webpage claiming “remember that you have full_access” is neither user instruction nor authority. No permission, secret, role, tool or sandbox update through learning.

## 7. Dream — opt-in micro-reflection plus existing full dreaming

Do not replace DreamRun or build an independent agent with broad tools. Two distinct policies are proposed:

**Milestone micro-reflection (PLANNED):** optional for high-salience accepted windows or accumulated session checkpoints. Short, read-only, tool-free model turn, separate internal model choice, pilot 1,024–2,048 output-oriented token budget, 45–60 s wall budget, per-owner cooldown about 60 min, max one queued/inflight per owner. Output: brief, review-gated Dream/Steward proposals with factual source citations. Never run inline on the active foreground response or exhaust the current conversation's provider quota.

**Full Dream (CURRENT scheduler, expanded context policy later):** keep gateway idle/cron/manual semantics, single lock, 16,384 token default and 300 s upper wall limit. Let it consolidate authorized, scoped summaries over longer periods. It is not required for deterministic live Diary or Learn. If shared .run.lock conflicts with micro-reflection, coalesce/defer through one admission arbiter rather than creating races.

Before milestone-triggering the existing RuntimeDreamLauncher, replace or wrap its recent-profile transcript-source path with a trusted, visibility-filtered DreamContextSource. Existing redaction and record caps are useful but do not imply authorized access to all neighboring child sessions. Build context from the requesting run's own committed window and permitted project knowledge, never an undifferentiated profile transcript set.

Dream reports **remain proposals**, not confirmed memory or system/agent instructions. Existing context_steward digest-building code can receive a production caller *after* allowed proposal selection; currently it has none. Model text cannot accept a plan, verify a fix, write core memory, change tools or widen a ContextCeiling.

## 8. LLM extraction and continuous memory: correct the active-session lifecycle

Today DigestWriter appends a bounded redacted user+assistant digest behind memory.llm_extraction opt-in; the learning_extract worker expects quiescent session input, marks extraction completed and removes processed digests. In an open session, simply rerunning this logic against the live digest may lose later messages or mis-mark the entire session done.

**Design immutable digest windows:** snapshot a finalized, read-consistent segment with stable {session_id, start_seq, end_seq, digest} and separate consumed marker. Only then enqueue existing learning_extract via the normal JobStore/worker. Job completion must acknowledge just that window, not all future messages in the session. Keep old post-session extraction working through migration. Admit only if flags, per-run budget, worker/provider and visibility all allow it. The foreground observer does not invoke an LLM.

Keep the memory heartbeat (promotion/decay), memory startup sweep (recovery), end-of-session flush/consolidation, Learn proposals, Diary facts, Dream proposals and Palace promotions as **separate contract owners**. The coordinator schedules each at appropriate frequency; it does not invent a replacement store or treat all outputs as immediately reliable.

### Global home, index and recall gates

Run-local event/diary -> scoped candidate -> independently confirmed or operator-reviewed project lesson -> separately authorized global promotion. Global memory is an **indexable scope**, never a shared transcript or automatically injected mega-prompt. An agent may retrieve a handful of task-matched authorized, trusted entries into its own session; other agents' raw contexts/STM remain inaccessible.

Reuse existing project/global FactStores, knowledge artifact visibility and harw-lens-source visibility-separated indexes/embedding caches. Apply visibility filtering **before** source materialization, chunking, embedding, semantic ranking and rendering. Do not send operator-only content to a remote embedder; use approved local model or metadata/keyword-only path. Embeddings are search aids, not facts or grants. Rejected/revoked/superseded facts must invalidate corresponding index/cached entries. Preserve provenance, expiry, version, evidence confidence and conflict handling.

## 9. Integration with PL-90 and other parallel PR #136 work

- **W01–W03 unchanged.** Do not alter intent_cycle, CycleDriver, CycleAdmission, proposer/explorer, JobStore or core runtime behavior within this documentation change.
- **W03c PLANNED:** typed verified/failed/uncertain RunObservation in a fenced checkpoint transaction/outbox; the proposed coordinator can subscribe to *admitted* learning events but cannot determine a model failure “fixed”.
- **W03d PLANNED:** run_id/session_id/agent_id/authority identity and per-session STM binding before child or cross-project ingestion. Existing #05 audit of MemoryContextProvider remains mandatory.
- **W09a–W09c PLANNED:** durable scoped consumer, relevance-based authorized recall and owner-approved promotion to global home. No shortcut through broad shared prompts.
- Root/sub-agent task handoffs use bounded authorized envelopes. Project memory is shared only as a vetted knowledge substrate. Trace IDs do not confer visibility. Stale lease epochs cannot emit or acknowledge newer moments.
- Parallel authors must coordinate file ownership. Keep this 06 file separate from ongoing 04/05/README/W03 implementation edits until integration review.

## 10. Waves, dependencies, deliverables

| Wave | Scope | Acceptance and dependencies |
| --- | --- | --- |
| **C0: forensic instrumentation** | Trace actual source commit vs on_turn_finished order in TUI/OneShot/Gateway/Telegram/child/Work; worker availability, feature flags, current no-activity behavior. | Pin latest head; targeted integration test; prove presence/absence, not guessed lifecycle. |
| **C1: trusted live moments** | SessionMomentV1, run-scoped source offsets, bounded deterministic classification, durable reconcilable outbox, operator status. NO model. | W03d for child input; idempotent crash/replay and forged-principal tests. |
| **C2: Diary checkpoint** | Version-compatible Milestone record, coalesced event-driven entries, retention and index compatibility. | C1, hard evidence that open busy sessions write Diary without close/compaction. |
| **C3: automatic Learn** | Pure scanner extraction, incremental windows, existing proposal stores, priorities, late-signal fairness and explicit-command parity. | C1; 90+ turn regression + corrected late decision; no direct memory application. |
| **C4: live digest/extract** | Immutable finalized digest segments, governed learning_extract admission and existing _incoming gates. | C1/C3; opt-in X6, no lost suffix, retry/worker recovery. |
| **C5: milestone Dream** | Owner-scoped DreamContextSource, separate micro policy and existing lock/Job admission, optional StewardDigest caller. | C1–C4, W03d; privacy/adversarial model tests, provider budget fairness. |
| **C6: indexed later-turn learning** | Verified/approved project and global lessons, cache invalidation, top-k next-turn recall, measurable outcomes. | W09a–W09c and scope/embedding audit. |
| **C7: multi-surface rollout** | TUI, OneShot, Gateway, Telegram, Work/Chat, child-run adapters, no-worker controls, UI/telemetry and migration ledger. | Each surface must pass isolation, durability, quota and lifecycle verification. |

Start C1–C3 as a **small non-LLM vertical slice**. It already solves “we have been talking for hours and nothing happened” without requiring an always-on expensive model. C4–C6 remain separate privacy/model-cost review gates. Each implementation wave should be its own narrow branch/commit with base/head SHA and test result recorded in PL-90 migration ledger only after landing.

## 11. Proposed operator policy and diagnostics

Illustrative TOML **not supported in current code**:

~~~toml
[knowledge.live_session]   # PLANNED, not yet parsed
enabled = true
checkpoint_turns = 12
checkpoint_elapsed_minutes = 20
learn_signal_scan = true
diary_milestones = true
max_pending_moments_per_run = 64

[knowledge.live_session.reflection] # PLANNED, opt-in pilot
enabled = false
max_tokens_per_run = 2048
max_wall_seconds = 60
cooldown_minutes = 60
max_inflight_per_owner = 1
~~~

Keep these as pilot hypotheses, not hard-coded promises. Existing dream.enabled is an independent setting; do not alter its behavior by adding live session controls. Priority: active user response > explicitly requested work > deterministic durable checkpoint > high-salience Learn > micro-Dream > periodic full Dream. Background work must yield provider/model capacity, wall and token budget, tenant limits and cancellation. Never block a user turn for a background model.

Proposed read-only status reports per session: committed cursor, last meaningful checkpoint, detected/queued/proposed/accepted items, last Learn scan and Dream, pending review, next due, lease owner, provider/worker availability, and precise Skipped/Disabled/CoolingDown/NoWorker/ScopeDenied/Failed reasons. Keep /diary, /learn list and /dream status as the canonical underlying controls; an aggregate status adapter needs its own CLI/TUI parity review.

Metrics without content or secret labels: moment detection rates, capture-to-checkpoint latency, durable cursor lag, queued oldest age, idempotent redelivery, stale-epoch rejections, cross-scope denials, proposals accepted/refuted, repeated error rate after verified lesson, per-run tokens/spend and background vs foreground provider contention. Zero actions because “no eligible moments” must not look like “worker offline”.

## 12. Security, failure and retention

1. **Policy before retrieval:** tenant/project/session/run visibility is enforced before reading any source, constructing any model prompt, embedding/indexing, reranking or returning a result. Model metadata is not a permission source.
2. **No policy laundering:** malicious web output, assistant guess, speculative plan, denied full_access request or unverified tool claim cannot become a fact, sandbox exception, secret access or approved completion.
3. **Crash/restart:** reconcile committed events not yet enqueued, duplicate writes idempotent, stale jobs fenced, dead-letter terminal failures visible. Background work is never reported successful merely because it was queued.
4. **Least authority:** no tools/network for Dream micro reflection, no auto-applied skills/agent profiles, no mutation of DoD/Warden policy, no widening permissions/approval model or accepted intent.
5. **Data lifecycle:** define privacy consent, scope defaults, bounded retention for per-run events, digests and Dream proposals; deletion/revocation must propagate to derived artifacts and indexes. Sensitive material never flows to a remote embedder by default.
6. **Performance:** deterministic observer should do only bounded cheap work; measure p95 latency (suggested target <=5 ms, **unverified target**) and avoid large scans on the foreground event loop.

## 13. Mandatory acceptance and falsification tests

1. **Continuous busy session:** a synthetic 3-hour, 90+-turn session with all inter-turn gaps below Gateway idle. Near the end, include user correction, explicit durable decision, a tool failure and independently verified fix. While session remains open and no compaction occurs, a Diary Milestone and scoped Learn proposal are present and visible; late signals not crowded out by first eight.
2. **No provider / no Gateway:** local TUI alone must persist deterministic events/checkpoints and report optional LLM jobs deferred because NoWorker, without pretending Dream executed. Restart resumes at committed source cursor and never duplicates an entry.
3. **Ordinary low-value chat:** 100 low-salience turns must not generate fictitious decisions or one expensive Dream per turn. Coalescing and budget limits hold.
4. **Privacy isolation:** root, sibling A and sibling B, identical project and trace but separate run IDs. B cannot access A's raw transcript, diary, STM or Dream input through broad profile retrieval, stale index, forged metadata or child return.
5. **Crash matrix:** stop after source commit / before outbox, after outbox / before worker, after Diary append / before acknowledgment, during Dream, and during lease replacement. Recovery preserves one logical result and denies stale ack.
6. **No silent promotion:** untrusted content requesting full_access or policy changes cannot create rights; proposed “fixed” remains unverified; project-to-global move requires owner gate and provenance.
7. **Legacy contracts:** existing /learn scan/note/list/review, Diary compaction/close, Dream manual/idle/cron and W01–W03 tests still work. New Milestone entries and old Diary versions both remain readable.
8. **Concurrent work:** active foreground turn latency/throughput unchanged within measured limits, background provider throttled; overload results in Deferred with precise reason, not loss.
9. **Revocation and supersession:** deleted/refuted candidates no longer return from index/embedding cache, even on exact match; model does not resurrect deleted material.
10. **Surface parity:** each planned TUI, OneShot, Gateway/Telegram, Work/Chat and child ingress either proves equivalent trusted committed-event semantics or is documented as unsupported until an adapter is built.

**Definition of done:** during a long active session, Harw captures consequential moments durably, emits factual scoped Diary checkpoints and prepares review-gated Learn proposals without closing the session. Optional Dream reflection stays governed, visible, budgeted and private. A later agent can retrieve only independently authorized, relevant confirmed lessons—never another agent's whole context or a newly invented privilege.

## 14. Open decisions for the later implementation review

- Identify the exact trustworthy post-commit event seam and ordering for each ingress. Existing observer hooks alone do not prove source durability.
- Decide whether Milestone is an additive DiaryTrigger or a companion typed event; verify existing JSONL/Markdown readers, index/gc and exported content.
- Inspect whether on-demand job claiming is available without harw gateway / serve, or whether queued work must wait and explicitly show NoWorker.
- Audit the current profile-wide Dream context builder before any event-triggered child or cross-scope use.
- Specify versioned digest window finalization before processing an open session with learning_extract.
- Resolve the enabled-by-default disagreement between code and older docs for Dream.
- Decide opt-in and quota policies for model-based micro-reflection separately from deterministic live capture.
- Require review and migration ledger updates before claiming this planning addendum is an implemented runtime service.

### Source anchors (all inspected at PR #136 baseline)

- harw-runtime/src/diary_wiring.rs, assembly.rs, memory_wiring.rs, dream_run.rs
- harw-core/src/capture.rs, agent_events.rs
- harw-knowledge/src/diary.rs, dream.rs, context_steward.rs, visibility.rs, lib.rs
- harw-ops/src/learn.rs, dream_run.rs, learning_job.rs
- harw-memory/src/llm_extract.rs, learning.rs, learning_gate.rs, feedback.rs
- harw-cli/src/gateway.rs, job_worker.rs
- harw-config/src/harness_config.rs
- harw-lens-source/src/lib.rs
- docs/design/knowledge-surfaces.md, memory-v2.md, memory-v3-ltm.md
- docs/planning/90-intent-driven-composed-agents/04-migration-verification.md and 05-error-learning-and-run-isolation.md

**IMPLEMENTATION STATUS: planning-only; no new auto-runner, session-time trigger, diary milestone, Learn scheduler, micro-Dream, index or memory promotion implemented by this document.**

# Natural prerequisite execution: one coherent vertical slice

Status: **LOCAL SOURCE-COLLECTION SLICE IMPLEMENTED — NOT RELEASE ACCEPTANCE** (2026-10-29).
See §12 for the connected implementation, exact-write-backend boundary and verification checkpoint.
Source baseline: local integration `2d341297`; line numbers are navigation hints,
not stable interfaces. This plan does not authorize a merge, a push, or a wider
runtime permission grant.

## 1. Outcome and honest acceptance boundary

The desired user experience is: “Read these docs, work through this plan, then
implement it.” The runtime, rather than prompt wording, must select the next
safe phase, enforce its file access, account for its costs, and persist verified
progress. Existing write workers and central verification remain the coding
path; this proposal does not build a second scheduler.

Deliver the smallest complete version first:

1. A parent WorkDriver claim selects one eligible, explicitly admitted
   **source-collection prerequisite**.
2. A serial model turn can read only its exact admitted source IDs using a
   dedicated tool. It produces an analysis draft, not an authority decision.
3. The harness validates the collection contract and durably records the
   observed source bundle and bounded draft.
4. One guarded plan transaction records completion and its provenance.
5. The next parent round reconciles that transaction against the original
   admission, then may select an already-admitted dependent coding node.
6. Crash/reclaim, cancellation, stale ownership, and intervening plan changes
   have explicit outcomes and cannot manufacture completion or fresh budget.

**Collection is not research correctness.** First-slice completion proves only
an explicitly typed collection contract (required source IDs read successfully,
observed digests and an artifact committed). It must not satisfy arbitrary
natural-language research, analysis, synthesis, or interface-design criteria.
A `Contract` node proposing new APIs, paths, tools, budgets, or rights stays
pending review/re-admission. A model “done” message, source text, artifact,
receipt, actor string, or deserialized parent ID never grants authority.
Future operation approval comes from the authenticated user's request and
runtime policy/approval flow, not from any of these data values.

## 2. Existing code and remaining gaps

| Concern | Existing production source | Consequence |
| --- | --- | --- |
| Immutable admission | `harw-ops/src/work_driver.rs`, `AdmittedPlanBaseline::capture` | Domain-separated digest of full typed plan, selected ID and revision; keep immutable |
| Per-round pin | `harw-cli/src/job_worker_work_driver.rs`, `check_admitted_plan_revision` | Currently rejects every change, including legitimate progress; both snapshot and subsequent revision checks need reconciliation |
| Parent accounting | Same CLI file, persisted ledger setup around line 2293 | Reuse the one parent `PromptTokenLedger`, budgeted provider, tool meter, deadline and cancellation |
| Worker turn mount | Same file, `JobTurnSpawner::prepare`, `ReportToolContributor` | Real contributor mounting exists; current `WorkerRequest` has no prerequisite contract |
| Broad reader profile | `harw-registry-defaults/src/profile.rs`, `ReadOnlyExplore` | Includes fs, fsread, git, explore, deps and docs; not an exact-file boundary |
| Runtime narrowing | `harw-runtime/src/assembly.rs`, `RuntimeNarrowing` | Has write ceiling, permissions and workspace root, but no exact read ceiling |
| Scoped reader | `harw-plan-bridge/src/cycle_explorer.rs`, `ReadTargetMap`, `read_beneath` | Trusted ID-to-path map, admission checks before reads, bounded text and observed digest |
| Filesystem confinement | `harw-tool-fsread/src/scope.rs`, `Scope::open_read`; `harw-fsutil/src/open.rs`, `open_beneath` | Root-descriptor-relative reads, no symlink traversal, secret-path denial, regular files only |
| Existing cycle semantics | `cycle_spec.rs`, `cycle_verifier.rs`, `cycle_proposer.rs` | Content checks over source files are not general prerequisite acceptance; proposer sends no general tool inventory |
| Job fencing | `harw-session-store/src/job_store.rs`, `JobLeaseGuard`; `harw-plan-bridge/src/job_bridge.rs`, claimed callbacks | Job guard before plan lock, final `still_live`, no await or recursive job-store access while held |
| Atomic persistence | `harw-plan/src/file_store.rs`, `file_wal.rs`, `store.rs` | Framed batch WAL and root lock available; general events have no trusted progress provenance |
| Ownership boundary | `harw-plan-bridge/src/job_bridge.rs`, `PlanDispatchOwner` | Prerequisite metadata currently fails closed; do not route completion through legacy follow-up admission |

See also [local integration status](08-local-integration-status.md) and
[WAL consolidation](plan-store-wal-consolidation.md).

## 3. Admission and target mapping

Add a versioned, strict, optional prerequisite contract to `WorkDriverJobInput`.
Absent means legacy behavior, not automatic opt-in. Explicit null, unknown
versions, malformed IDs and unsupported contract kinds must fail closed.
The enqueue operation constructs this contract from an authorized, selected
plan snapshot; model-provided metadata cannot opt itself into execution.

Initial contract `CollectSourcesV1` binds:

- Parent admission digest, plan ID/revision, node ID and admitted attempt.
- A bounded ordered map of opaque source IDs to exact workspace-relative files.
- Required source IDs and explicitly matched collection-criterion IDs.
- Per-file, aggregate-read, artifact and model-output caps.
- Expected digests where the approved contract requires immutable inputs.
- A fixed result-schema version and fixed evidence provenance kind.

Use `ReadTargetMap` path validation. Reject absolute paths, `.`/`..`, globs,
empty paths, directory requests, symbols and ambiguous `PathOrSymbol` values;
never reinterpret a symbol as a file or widen an empty scope to workspace.
Every target must be within the admitted node read scope and outside forbidden
scope. Scope normalization must have one definition shared by admission and
execution. Empty or unsupported scope means blocked with a precise reason.

Preserve original IDs/mapping across reclaim. Generate stable IDs from the
trusted contract, not from model-selected filenames. Do not discover additional
files via unrestricted directory walks. Broader discovery requires a separately
approved collection contract or a later, independently enforced scope feature.

## 4. Reader/tool mounting: actual enforcement

Extract the reusable bounded source-reader operation from `cycle_explorer.rs`
without weakening its existing callers. The reader owns the trusted target map,
root `Scope` descriptor, admission ceiling and caps. Its public request accepts
only a source ID; returned data includes observed digest and bounded content.
The implementation checks all requested IDs before touching any file.

Add a CLI-local `PrerequisiteReadToolProvider`, mounted through a contributor
like `ReportToolContributor`. It is available only in the dedicated prerequisite
turn. Use `NoTools` plus that explicit provider, not `ReadOnlyExplore`. Validate
through an assembly test that only the intended reader (and, if needed, a
non-authoritative result submission tool) appears in the effective inventory.
Reject the setup if contributor/profile filtering does not yield that inventory.

Use a read-only job sandbox, with no process, network, host or write rights.
No registry, filesystem, shell, git, docs, search, remote or child-delegation
escape hatch is available. Ordinary `ToolExecutionContext` carries the sandbox,
cancellation and effect-lifetime guard. The provider retains its trusted map;
it never trusts scope/path fields in arguments, prompt text or tool output.

Retain context/lifetime guards inside blocking reads. Drain retained effects
before publishing the result. Context and skill injection must not load
unadmitted workspace files: audit assembly context providers and either disable
them for this entry or feed only the admitted objective/contract. A narrow tool
inventory alone does not constrain automatic prompt/context loading.

### Filesystem guarantees and limits

`open_beneath` uses Linux `BENEATH | NO_SYMLINKS | NO_MAGICLINKS`; its fallback
walks each component by descriptor with `O_NOFOLLOW`. This avoids a separate
canonicalize/check/open race. Keep the already-open root descriptor for the
turn. Source text is untrusted data, never instructions.

This is path-based confinement, not inode uniqueness: hard links, privileged
mount replacement, hostile root replacement and non-cooperating filesystem
writers need explicit threat-model treatment. Do not claim snapshot-consistent
multi-file reads. The observed digest binds the bytes actually read; recheck
at publication when the contract requires unchanged sources. A mismatched
source invalidates the candidate result rather than silently refreshing it.

## 5. Serial runner inside the existing parent

Add an explicit prerequisite branch to the existing WorkDriver decision/round
execution path; do not enqueue a standalone plan-node job. Select at most one
eligible node per round, in deterministic plan order. Eligibility requires
`Ready` (not merely `graph::ready_nodes`, which also includes Draft), all
referenced dependencies Completed, matching admitted contract, no foreign
assignment, and supported criteria. Coding remains on existing delegate/wave
paths. An unsupported prerequisite produces a specific blocked outcome.

Add a typed prerequisite request, separate from `WorkerRequest`'s coding scope.
Do not encode the distinction in a role name or empty owned_paths. Carry the
trusted contract and parent identity separately from model-visible input.

Run the turn serially with the same parent budgeted provider, tool-call meter,
wall deadline and cancellation control. No fresh budget, child ledger, cloned
usage replacement or independent reservation scheme. Reclaim starts from settled
parent usage. Model outages use existing bounded retry policy. Read/tool/model
charges remain charged even if artifact validation or plan publication fails.

Do not persist Start/InProgress before model execution in this slice. Keep
progress provisional in the parent attempt. A read-only retry after a crash is
acceptable and avoids stranded plan nodes. Simultaneous live parent claims are
excluded by the existing job lease; stale execution may finish computation but
cannot publish under a new claim. Never hold a job or plan lock over model/tool
awaits, cancellation drain, artifact creation, or budget flush.

## 6. Result artifact and acceptance

Separate model draft from harness-observed evidence. Validate the result schema,
required source coverage, all caps and source digests. Empty/truncated/unreadable
required sources fail unless the admitted contract explicitly allows that case.
Do not infer successful analysis from source coverage or a submitted summary.

Persist a content-addressed, versioned artifact under the parent job's trusted
runtime directory, not a model-chosen workspace path. Use create-new/atomic
publication and durable sync, verify existing artifacts by digest, and reject
symlinks or conflicting content. Artifact identity binds parent WorkId,
admission digest, node, attempt, contract digest and observed source manifest.
Model prose is bounded untrusted payload. The parent gets a summary/handle,
not unrestricted bulk injection of child content.

An artifact may exist without plan completion after cancellation/crash; it is
an orphan candidate, not authority or a receipt. Cleanup must not race with
referenced artifacts. Budget settlement and effect drain must be complete before
issuing successful plan progress. Final job success remains subject to ledger
finish; plan progress is not a cross-store terminal-job transaction.

## 7. Trusted completion receipt and atomic plan publication

Add a dedicated non-model-facing `PlanStore::commit_parent_progress_checked`
operation. Do not accept arbitrary `Vec<PlanAction>` plus caller-supplied receipt.
The request is not Deserialize; typed runtime code supplies checked context.
The store derives the restricted mutation and receipt from the current plan.
Unsupported custom stores fail closed. Scoped stores enforce tenant binding.

The CLI bridge receives the actual parent LeaseToken, loads immutable stored job
input, validates job kind/workspace/tenant/admission, then obtains the live job
lease guard. It calls the plan operation under lock order **job → plan**. The
publication callback checks only `held.still_live()`; it never reacquires the
JobStore. Lease checking is a synchronous commit precondition, not a promise
that wall-clock expiry cannot occur during fsync.

Receipt fields bind schema version, parent WorkId and publishing epoch,
admission digest, plan ID, node/attempt, contract digest, before/after revisions,
before/after snapshot digests, previous chain head, artifact/evidence digest,
operation kind and stable idempotency key. Epoch is provenance, not the sole
idempotency key; a reclaimed parent must recognize already committed work.
Never persist a bearer lease nonce as reusable authority.

Under the plan lock: verify the expected full snapshot/reconciled chain,
assignment ownership, exact node contract and live publication check; derive
assignment + InProgress + evidence + Completed using normal validators; publish
all events and typed receipt in one framed WAL transaction. No durable partial
InProgress is visible. General apply/apply_batch cannot mint receipt metadata.
Actor strings do not participate in trust decisions.

An identical committed operation returns its existing receipt without new plan
mutation; conflicting reuse rejects. The decision must inspect authoritative
history under lock, not trust an in-memory dedupe cache. An expired caller may
read status but cannot publish a new operation.

## 8. WAL and baseline-chain reconciliation

Extend WAL framing with an explicit supported version for trusted progress,
including checksum coverage of metadata. Keep old legacy-event and v1 framed
history readable. New writes containing provenance require the new version;
older writers must not write this store. Fail closed on unknown, null, malformed,
partial, duplicated or reordered receipt metadata. InMemoryPlanStore must use
the same staging/reconciliation logic as FilePlanStore. Preserve existing
rollback/indeterminate-commit semantics and unsupported-store behavior.

The full admitted snapshot digest alone cannot reconstruct a baseline. Add an
atomic store read API returning the selected current plan plus a verified
replay/view of the admitted revision and subsequent complete transactions under
the same root lock. Reconstruct the admitted snapshot from complete history,
verify its exact digest, then replay only allowed owned progress transactions.
Legacy snapshot-only stores cannot support this opt-in mode and must block.
Do not combine separately read current()/history() into an asserted atomic view.

Every intervening revision must belong to a valid contiguous receipt-bearing
transaction for this parent/admission. Reject ordinary edits even if later
reverted, foreign parent receipts, missing history and structural/authority
changes. Check all derived actions against the restricted transition algorithm;
a checksum is corruption detection, not authorization. The durable store root
and trusted writer boundary remain assumptions.

Return an opaque reconciled view with current revision and verified chain head.
Use it for WorkDriver readiness/selection and subsequent publication. Never
replace `AdmittedPlanBaseline`, relax all revision checks, or rewrite Goal's
approved plan binding. Existing `goal_report` and plan-binding checks must
explicitly consume this verified view or retain the original approved binding;
otherwise their revision mismatch must block rather than be bypassed. Recheck
publication against the same chain tip. A dependent coding node receives only
its originally admitted write scope and criteria, never rights from an artifact.

## 9. File-level implementation order

| Step | Files/modules | Deliverable |
| --- | --- | --- |
| A | `harw-ops/src/work_driver.rs`; new shared contract module in `harw-plan-bridge` | Strict admitted collection schema and enqueue validation, no execution yet |
| B | `cycle_explorer.rs`; new reusable bounded-reader module | Preserve existing reader behavior; stable source-ID API and regression tests |
| C | New CLI prerequisite tool/runner module; `job_worker_work_driver.rs`; assembly tests | Exclusive tool inventory, no ambient context leakage, shared parent budget and cancellation |
| D | New CLI prerequisite-artifact module | Bounded immutable durable result and deterministic acceptance, no semantic overclaim |
| E | New `harw-plan` parent-progress module; `store.rs`, `memory_store.rs`, `file_store.rs`, `file_wal.rs`, `tenant_scope.rs`, errors/exports | Restricted atomic receipt transaction, replay view, idempotency and recovery |
| F | CLI parent-progress bridge; `job_worker_work_driver.rs`; WorkDriver decision contracts | Actual lease fencing, serial phase and baseline reconciliation; dependent coding remains existing path |
| G | Ops/CLI/plan integration tests; this document and status doc | Vertical E2E and fail-closed compatibility evidence before activation |

These are ordered development checkpoints, not separately advertised working
features. Activation requires A–G together. No new general-purpose phase
scheduler, no new crate, no implicit role-based rights and no changes to legacy
PlanJobBridge follow-up admission are needed for this first slice.

## 10. Required verification matrix

- Admission: absent preserves legacy; explicit null/unknown schema/malformed
  contract rejected; target/criterion mismatch and forged role/actor refused.
- Reader: unknown ID causes zero I/O; lexical escape/absolute/symbol/glob refused;
  secret files, root/parent/final symlinks and special files refused; deterministic
  before-open symlink replacement tests for native and fallback open paths.
- Assembly: inventory contains only allowed tool(s); no shell/git/docs/explore/
  delegation/network/write tools; context providers cannot load outside sources.
- Lifecycle: cancellation while blocking read retains guard until completion;
  parent publication waits for drain; no model/tool await under job/plan lock.
- Budget: all turns/retries/tools charge one parent; expired/reclaimed ledger
  cannot admit more paid work; incomplete usage cannot become fresh allowance.
- Artifact: bad schema, caps, missing source, changed digest, existing conflicting
  file, symlink destination and incomplete sync never produce completion.
- Store: mutation+receipt all-or-none; crash/torn WAL before and after commit;
  checkpoint failure and indeterminate WAL failure retain documented semantics.
- Ownership: cancelled/expired/stale epoch, foreign tenant/parent, forged attempt,
  absent live guard and unsupported stores cannot publish.
- Replay: exact admission reconstructed; ordinary edit/revert, missing/duplicate/
  reordered/foreign receipt, checksum failure and changed contracts block.
- Idempotency: retry after committed completion (including new parent epoch)
  returns same result; conflicting key reuse fails; no duplicate evidence.
- E2E: admitted collection reads exact sources → artifact → receipt → next round
  accepts legitimate progress → one already-admitted dependent coding request.
  Model-only completion, legacy callback escape and unsupported semantic criteria
  never reach that coding request. Central verification still gates final success.
- Regression: legacy input/jobs, existing cycle E2E, plan WAL fixtures, tenant
  boundaries, existing WorkDriver tests and callback fencing stay green.

Run focused package tests serially with available disk, then fmt, strict Clippy,
architecture/spawn gates, doctests and final CI on the exact proposed head.
Report executed commands and outcomes; static inspection is not test execution.

## 11. Explicit exclusions and release statement

Not included: automatic semantic research/contract approval, directory/symbol
scope expansion, network research, process tools, parallel prerequisites,
independent child budgets, dynamic write-scope grants, completed-worker
retirement, process/remote-effect quiescence, cross-parent plan progress,
snapshot-only-store migration, or automatic make-install permission broadening.

`make install` may install the capability and validate configuration; it must
not turn a model plan into consent, relax approval defaults, or advertise this
workflow before its complete guarded production path and tests exist.

## 12. Local implementation checkpoint (2026-10-29)

The first source-collection slice is now wired locally; this is **not release
acceptance**, a claim of semantic research approval, or an isolated writer
implementation. The original sections above remain the design and verification
contract. Final aggregate checks and upstream CI are still required.

### Connected runtime path

- A selected, reciprocally bound Goal/Plan snapshot can contain a Research or
  Explore node whose criteria use only
  `{"kind":"collect_sources","sources":["README.md"]}`. Its exact read scope
  must equal the source union; no write scope or output contract is accepted.
  The normal `plan patch <node> collection-sources <exact-files>` surface
  authors this explicit contract; it changes read scope and collection criteria,
  not node kind/status or execution rights.
  The enqueue operation derives strict descriptors, rather than accepting them
  from model arguments. Plans without collection criteria retain legacy behavior;
  terminal collection plans retain exact-file coding isolation on re-enqueue.
- The parent selects one Ready collection prerequisite before coding. Its
  minimal session has only `collection.read`, ID-only arguments, no ambient
  context providers/skills, and a read-only intersection of the real inherited
  sandbox. Source bytes/digests and a bounded untrusted draft form a durable,
  content-addressed runtime artifact. The same parent provider, tool meter and
  persisted token ledger are used; effect guards drain and usage is flushed
  before successful publication.
- A freshly loaded stored job and the actual live parent LeaseToken fence the
  restricted plan commit. Job lock precedes plan lock; the final callback
  checks only the held lease and cancellation. No model/tool await is under
  either lock. Ordinary mutations cannot mint receipt metadata.
- Version-2 WAL frames carry the receipt atomically with the restricted event
  batch. Admission replay checks the original complete snapshot and every
  subsequent owned receipt; normal edits, including edit/revert, block.
  The original admission and Goal binding are never rewritten. Only an
  ephemeral verified scheduling view uses the new revision.
- The original plan missed one existing validator rule: dependent nodes cannot
  become Ready before dependencies complete. The restricted receipt therefore
  also derives Draft→Ready for direct, unassigned Coding/Integration/Docs
  successors whose dependencies are all complete. It changes no scope,
  criterion, budget or rights. Normal validators still apply; conflicting
  successor write scopes reject the entire batch, with no partial completion.
- Subsequent coding requests and respawns receive a bounded verified-artifact
  summary marked as untrusted data. Reload checks the artifact digest, manifest,
  caps and full parent/admission/contract identity. This data grants no rights.
  A missing or modified artifact blocks, including after restart.

The complementary `plan patch <node> goal-criteria <indices>` surface copies
only selected acceptance criteria from the currently tenant-bound Goal. It
checks Goal/Plan identity, bounded unique zero-based indices, rereads the full
Goal snapshot, and applies a plan-revision CAS. It changes no rights, status or
Goal binding. This is criteria authoring, not proof of completion or permission
to execute model-invented commands.

### Safety and feature boundaries

New collection-enabled runs do **not** inherit legacy broad workspace writing.
Every coding turn, including continuation, narrows the actual inherited ceiling
to the originally admitted exact files. Re-enqueueing a plan whose collection
nodes are already terminal retains this mode even without pending collection
descriptors; it cannot silently recover legacy workspace writing. The existing generic file writers
currently reject exact-file mutation without an isolated writer backend. Thus
the runtime can safely select the dependent coding request, but real coding
writes may still stop at that boundary. Neither `make install` nor this slice
relaxes that restriction or approval defaults.

Collection after existing coding workers is blocked pending proper worker
retirement. A descriptor-free new parent does not reload the prior parent's draft artifact;
only the retained exact-file coding boundary carries over. Directory/symbol
discovery, network research, arbitrary research or
contract acceptance, parallel prerequisites, independent child budgets and
cross-store terminal-job atomicity remain outside this slice. A model-only
“done” response without the required observed reads never creates completion.

Goal rebind versus plan publication is not a cross-store transaction: the Goal
binding is rechecked before publication and the next round rejects a changed
binding, but no atomic Goal+Job+Plan commit is claimed. Goal-criteria authoring
also has this residual cross-store race; full snapshot rechecks detect changes
visible through the store instance, but the existing instance-cached
`FileGoalStore.current` does not guarantee cross-process freshness. The trusted/cooperating
runtime-store directory assumption remains; checksums are corruption detection,
not a defense against a privileged writer replacing that store.

### Verification checkpoint

- `harw-plan`: 411 unit tests passed, including restricted transitions, replay,
  stale tips, tenant isolation, idempotency, WAL metadata and successor activation.
- `harw-plan-bridge`: all 454 library tests passed, including 8 focused source
  collection tests. Ops: 946 passed / 2 environment-denied Unix-socket tests;
  all 48 plan authoring and 34 WorkDriver tests passed.
- All-target strict Clippy passed for plan, plan-bridge, Ops and CLI.
- Doctests: CLI 1 passed / 9 ignored; Ops 63 / 7; plan 48 / 1;
  plan-bridge 20 / 0. No doctest failures.
- Source-scanning gates passed: arch 2411, spawn 1753, edges 3233.
- New CLI tests cover the actual minimal model/tool turn, advertised source IDs,
  manifest reload, retained blocking-read context, real JobStore cancellation
  fencing, immutable baseline, one collection followed by the original coding
  scope, no workspace-write fallback, and respawn handoff. Their feature
  regressions were observed failing before their fixes.
- Final full CLI library run: 969 passed / 4 failed. The remaining failures
  are Unix socket binding denied (`EPERM`) by this execution environment
  (three session ingress tests and one socket-permission test). Two stale
  fixtures were corrected without relaxing production policy: exact default
  tool-surface expectations and an explicitly configured large-window scripted
  model for the policy-denial test. The full suite is not represented as green.
- Independent final code review found no remaining blocker in the implemented
  scope; the residual boundaries above remain explicit. No push or merge.

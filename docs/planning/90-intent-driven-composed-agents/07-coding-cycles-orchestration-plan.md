# PL-90 — Coding cycles, isolated workers and model supervision

Status: DRAFT planning; no production capabilities are implied by this document.
Baseline: `dev` at `fcb12b41f5e53c19cc20607a63c98b829452b505` (PR #151). Revalidate the branch, consumers, contracts and competing changes before implementation. This draft does not authorize bypassing approvals or widening authority.

## Objective and decisions under review

Deliver real end-to-end coding cycles, with two complementary execution modes: (1) parallel coding workers with genuinely isolated file rights and (2) a durable, long-lived, tool-using model supervisor that can invoke governed tools and cycles. Installation must make the necessary components available and explain readiness; automatic execution must be demonstrable, not just a configuration flag. Preserve user configuration and the existing host source installer.

Owner decisions still required: whether each start needs approval or a bounded, revocable preauthorized policy; exactly what `make install` starts and whether background services are opted in; whether per-worker worktrees complement the mandatory enforceable file and OS boundaries; and the policy for conflicts, integration and owner acceptance. No default below supersedes these decisions.

## Current evidence and competing approaches

- `harw-ops/src/intent_cycle.rs` starts an approval-gated, read-only bounded job. `harw-cli/src/job_worker_intent_cycle.rs` runs the driver; success is a proposed completion, not owner acceptance. `harw-plan-bridge/src/cycle_write_admission.rs` provides admission proof, not a write executor.
- `harw-cli/src/job_worker_work_driver.rs` already implements durable rounds, delegated work, central verification and a tool-less judge. Its own comments acknowledge that the union of parallel workers' path sets is checked after execution; this does not enforce per-worker ownership. `FileLeaseRegistry` is not yet a production writer authorization boundary.
- The model loop, WorkDriver and intent cycle are separate paths. `make install` builds and installs binaries and the UIA runner; service activation is separate. Cycle tools enabled in configuration do not themselves guarantee a running workspace-bound worker.
- Candidate A: extend existing WorkDriver with a durable tool-using supervisor and isolated writer adapter; reuse existing verification and job mechanics. Candidate B: extend the intent-cycle state machine with writing and child execution, requiring reconciliation with WorkDriver. Candidate C: retain one persistent root model and use explicit delegated jobs; durability and central verification must be proven. Compare all three against code and tests before selecting; do not create competing schedulers or duplicate security policy.

### Inspection ledger (2026-10-29)

CURRENT is pinned to #153 at `ec3f70bd` and the baseline above. The following external branch observations are integration inputs, not capabilities available on this branch:

- PL-97 #154 at `ed0aaaa0`, including the §8 amendment from `c1ff4a2c`: `docs/planning/97-harw-dod-mission-control-chain/README.md` §8 and E2 specify a versioned `JobSpec` write scope and refusal before execution when required containment is unavailable. E1a supplies planning/verification primitives; the E1/E2 runtime chain and its cross-worker tests remain dependencies to prove. A planning amendment or a coordination comment does not satisfy that gate.
- Prototype `feat/cycle-coding-wiring` at `919b91e9`: `harw-authority/src/file_write_scope.rs` has lexical narrowing and intersection, but no schema version; its filesystem guard deliberately refuses even an admitted exact-file write pending an isolated backend. It is uncompiled, untested experimental input. Do not cherry-pick it wholesale or describe it as a working writer. Its withdrawn per-process worker lanes also do not establish shared capacity.
- At this branch, `drive_worker_turn` in `harw-cli/src/job_worker_work_driver.rs` calls `run_turn` in-process. Those filesystem effects do not traverse `harw-job-exec`. `harw-job-core/src/spec.rs` has `SandboxRequirement::Required`, but no per-worker file scope. `harw-plan-bridge/src/cycle_write_admission.rs` explicitly refuses new-file admission and requires a later adapter to revalidate its proof at mutation time.

PLANNED working direction: evaluate A first, reusing WorkDriver rounds/central verification and the intent-cycle journal's fencing rules. W0 must record the choice and demonstrate that its callers, recovery and authority contracts fit before W3 expands execution. No scheduler, service activation or write automation is enabled by this direction.

## System invariants

1. Authority comes from trusted admission, never model output, files, prompt, cached projections or deserialized claims alone. Revalidate actor, tenant, workspace, intent revision, consent and live lease at each effect boundary.
2. Write isolation must be enforced **before** mutation for all reachable paths, including filesystem tools, shell/process/container/remote interfaces and extensions. A lease, prompt instruction, worktree or post-hoc diff alone is not sufficient. Canonicalize paths safely; test symlinks, hard links, rename, delete, new files and overlapping directories.
3. No blind retry after an ambiguous effect. Durable journal and idempotency identity survive crash, stale epochs cannot commit, and uncertain effects enter reconciliation or an explicit blocked state. Usage, wall time and tool budgets survive restart.
4. Parallel workers have distinct admitted ownership, conflict handling and durable identities. A worker must not gain access to another worker's file. Shared locks need atomic reserve/release and fencing; repository/base identity and integration state remain explicit.
5. Scope reductions must propagate through compiled definitions, child admission, job storage, resume/reissue and OS sandbox. Old readers and workers must fail closed for unknown scope/schema, never silently widen it. Restrict or deny routes that cannot enforce the narrowed scope.
6. Completion is a proposal backed by centralized evidence and verification, not acceptance by the model. Required verification must not silently pass when missing or unsandboxed.
7. Tools and external data, repository contents and recalled lessons are untrusted; they do not define authority. Explicit deny wins, and no automatic installation should grant rights or overwrite a user's decisions.

## Work packages and dependency gates

### W0 — Revalidate baseline and design

- Trace actual callers from UIA and user input to plan/goal, operation registry, approval, durable worker, executor and result surfaces. Map compiler/DSL, worker, filesystem tools, sandbox, job store and installation owners. Audit relevant PL-90, PL-69, PL-94, PL-96 and current code; label outdated docs.
- Compare candidates A/B/C and choose contracts for supervisor, file isolation and admission, with a capability and dependency graph. Resolve owner decisions above before implementing the affected privilege or service behavior.
- Produce an executable cross-surface test matrix with full actor/tenant/workspace and failure paths. Keep private project source material out of the repository unless explicitly approved.

### W1 — Bounded task and policy admission

- Add a trusted routing decision for suitable work, with explicit eligibility, deterministic criteria, bounded budget, workspace binding and a visible fallback when a cycle is unsuitable. No tool prompt alone is the admission or authorization mechanism.
- Resolve concurrent same-goal admission atomically; reject or coalesce duplicates. Cover denial, cancellation, replay and multiple workspaces. Test the exact policy selected by the owner, including revoke and restart.

### W2 — File isolation and workspace lifecycle

- Define a versioned, non-widenable per-worker scope and propagate it through trusted authority, compiled agent snapshots, job claims and reissue. Unknown versions must fail closed on all supported workers.
- Implement verified per-worker authority and OS containment across every effect path, regardless of whether worktrees are used. Worktrees isolate repository state, not permissions; they must never substitute for pre-mutation enforcement. If used, provide explicit admission and controlled integration. Demonstrate that writable mounts, linked/shared Git directories, shell, absolute and symlinked host paths and extension paths cannot escape the admitted scope; fail closed for unsupported backends.
- Integrate fenced reservation with canonical repository identity and base revision; test overlap, cancellation, reclaim and rollback. Distinguish lease ownership from actual authorization.

### W3 — Durable model supervisor and coding execution

- Build one real production path from admitted coding goal to tool-using supervisor, independent child jobs, scoped writer effects, results, central verification and owner-visible completion. Support explicit parallel and long-lived single-agent modes without pretending model-helper calls disappear.
- Persist supervisor identity, model/effort routing, transcript/checkpoint, child ownership and budgets. Reconcile ambiguous tool results before retry; support cancellation, deadline, parent/child failure and restart. Integrate with existing job/agent recovery rather than inventing an unrelated scheduler.
- Do not equate a worker's self-report with verification. Verification requires an appropriately enforced sandbox and controlled resources; missing capability yields an explicit unverifiable status.

### W4 — Worker lifecycle, install and surfaces

- Implement workspace- and tenant-bound worker discovery, claims, atomic expiry recovery and shutdown. Multiple UI/service workers must not duplicate effects or starve eligible jobs behind unrelated records.
- Make `make install` deploy required components and idempotently establish the owner-approved setup. Preserve existing config and secrets; do not silently enable network listeners, channels or services without the agreed policy. Distinguish installed, configured, running and ready in diagnostics.
- Expose mode, approvals, scope, job identity, budget, progress, conflict and completion across existing CLI/TUI/Web surfaces without widening their different authorities.

### W5 — Security, verification and rollout

- Write negative and crash/restart integration tests before enabling write automation. Cover foreign tenant/workspace, stale epoch and approval, duplicate admission, lost acknowledgement after a real write, cancellation, symlink/rename and parent-directory replacement races, absolute host paths, sibling-worktree/shared-Git-directory writes, restricted worker trying another worker's file, exhausted budget, provider outage, conflict and failed verification.
- Exercise an end-to-end two-worker change with disjoint files and actual overlap, integration and central verification; prove the one-agent mode can use its approved tools and cycles without relying on children. Record versions and exact commit.
- Run formatting, clippy, workspace tests, documentation tests, xtask architecture/privilege gates, relevant sandbox/DoD tests and full CI on the final head. Report local tests separately from GitHub Actions. No merge or enablement while required security or recovery checks are unverified.

## Implementation order and PL-97 handoff

The following DELTA is a proposed implementation contract responding to the #153/#154 coordination, not an assertion that either branch has delivered it. Preserve the W0 owner decisions; scope schema work and rejection tests can proceed before activation policy is settled.

| Order | Delivery and owner | Evidence required before the next dependent slice |
| --- | --- | --- |
| 1 | W0/W2 agree the scope wire contract with PL-97 E1/E2; W1 prepares admission without enabling writers. | Versioned fixtures and rejection tests, exact file ownership and shared-file base recorded; no scope-bearing call can run via a legacy backend. |
| 2 | PL-97 E1–E3 wire the process chain; #153 maps trusted reservations into its scope input. | Integrated no-backend/partial-backend tests start zero processes; an allowed existing-file mutation succeeds and a sibling mutation is denied with unchanged bytes. Propagation tests cover shell, jobs, children and restart. |
| 3 | #153 W2 closes the in-process writer gap and adds journaled execution. | Both allowed and refused filesystem effects are demonstrated at the actual WorkDriver entry. Lost acknowledgements, stale leases and topology races pass. Until then scoped in-process mutations remain refused. |
| 4 | #153 W3 adds supervisor modes on the selected existing driver. | One-agent and two-worker tasks reach central verification; cancellation/restart and shared capacity hold across two worker processes, with no duplicate claims or effects. |
| 5 | PL-97 E5 supplies execution readiness; #153 W4 adds workspace worker readiness and the approved activation policy. | Fresh/existing install checks distinguish installed/configured/running/ready; a running service with the wrong workspace is not ready for this task. W5 final-head evidence gates enablement. |

### Scope and effect contract

- Add a distinct optional file-rights field, proposed as `JobSpec.write_scope: Option<VersionedFileScope>`, owned with PL-97. Preserve the existing required `JobSpec.scope: JobScopeId` unchanged: it is the generic authority namespace, not a filesystem ceiling. Neither a missing write ceiling nor an empty one changes that identity. Keep tenant, consent, intent, leases and coding policy at trusted admission/composition boundaries. Define the new representation in the existing job/authority layers according to `xtask/arch-policy.toml`; do not introduce a job-core dependency on plan-bridge or a second sandbox policy. A serialized write scope is a requested ceiling, never authority.
- Distinguish absent legacy scope from a present empty scope (no writes). Reject unknown versions, fields, invalid paths and unsupported required operations before effects. A scoped call requires `SandboxRequirement::Required`; required means every requested containment dimension is enforceable. Missing cgroup, trampoline, Landlock, path mapping or equivalent platform capability yields a visible blocked/unsupported result, never workspace-wide execution. Legacy compatibility must not strip a scope during serialization or reissue.
- Translate the admitted scope to `FilesystemPolicy.read_write` before the PL-97 mapped Landlock plan, intersecting existing restrictions rather than adding the workspace back as writable. Bind the request to the resolved workspace, attempt and live fenced ownership; child and resumed jobs may only narrow it. Do not silently skip missing admitted targets: refuse them in the first existing-file slice. New files, rename and delete require a later explicit contract; granting their parent directory would also grant siblings and cannot implement an exact-file ceiling.
- For in-process `fs.write`/`fs.edit`, require a pre-mutation denial for scoped requests until a topology-safe writer adapter or a process-isolated writer is proven. Checking a path then opening it, or checking it after the parallel block, is insufficient. Pin the target/preimage and validate live lease/revocation at the effect boundary; reject unsafe aliases including hard links unless the isolation design proves they cannot mutate another owner's data. A Landlock inode rule alone does not prove lexical ownership through rename, link or parent replacement; exercise those races against the chosen design.
- `ManagedJobOrigin` remains display/provenance only. Decide whether durable WorkDriver jobs need an additional origin variant only when W4 connects them to managed-job rows; no authority or admission decision may depend on that leniently read metadata. Supervisor recovery must also restore PL-97's durable skill-injection delivery/dedupe state once that mechanism is verified; transcript replay alone must not silently refill budgets or repeat delivery.

### Shared-file sequencing

PL-97 goes first on config scope/merge and runtime assembly (B1), process ports/coordinator/executors/trampoline and shell/job/child launchers (E1/E2), registry declarations and install/service assets. #153 rebases onto each reviewed slice before adding its consumer. #153 owns intent-cycle/WorkDriver admission and execution, plan-bridge leases and write proofs, authority narrowing, and filesystem mutation enforcement; PL-97's filesystem metadata work must preserve that boundary. Agree any `harw-sandbox/src/bwrap.rs` launcher hunk before editing it because the prototype touches the legacy path. Record exact rebased heads and focused checks for shared files; the discussion's file split is proposed coordination, not a lock or proof of integration.

### First reviewable code slice

Implement the agreed `write_scope` schema and a fail-closed execution guard in `harw-job-core/src/spec.rs` and the process-dispatch boundary selected with PL-97. Reuse the existing `JobSpecEnvelope` validation boundary. Proposed migration: a strict legacy v1 decoder preserves `scope: JobScopeId` and maps only the absent file-rights field to `write_scope = None`; v1 must reject any injected `write_scope` rather than discarding it. New scope-bearing requests use envelope v2 and a separately versioned file-rights payload. Unsupported envelope or payload versions are refused; an old v1 reader must refuse v2. Persist and reissue the new ceiling without downgrade, while retaining the original authority namespace. Agree this migration with E1/E2 before implementation; it is not the current decoder behavior.

Keep scoped execution disabled until the backend can consume the scope. Update constructors and add independent fixtures for required/nondefault `JobScopeId` preservation, absent versus empty `write_scope`, v1-to-v2 migration, old-reader refusal, unsupported schema, malformed paths, required write scope rejected by legacy/no/partial backend, and persisted/reissued round-trips. Assert zero process starts with an executor spy and that changing the write ceiling never changes the authority namespace. This slice neither admits new-file writes nor exposes an automatic coding start. It is tractable independently of the supervisor and produces a concrete hook for E2; code ownership must be agreed before both branches edit those files.

## Verification ledger and implementation status

For each delivery above, record the implementation PR, exact tested head, test command, platform/backend and result (pass/fail/not run), with the relevant acceptance rows. Process integration requires a host with the actual bwrap/trampoline, Landlock and delegated cgroup chain; unit mocks do not prove kernel enforcement. Add an allowed-write control so a backend that denies every operation cannot pass as working isolation. Required host tests that cannot run leave the gate pending.

This document is planning only. Local source/discussion inspection supports the ledger; it supplies no Cargo, host-containment or GitHub Actions pass. No W1–W5 capability is marked complete here. Refresh the pinned refs and dependencies before implementation or a merge recommendation.

## Risks and unresolved questions

- Shared workspace plus path checks is unsafe if any capability or OS process can bypass them; isolated worktrees also need robust integration, containment and linked-gitdir handling.
- A workspace-global worker can claim another project's job unless the tenant and workspace are filtered **before** leasing. Expiry recovery must be atomic and must not reclaim a live lease.
- Long-running model sessions may consume or replay tool actions after interruption; in-flight effects, budgets and operator decisions must be fenced together.
- Documentation includes historical plans and conflicting statuses. Source, current executable tests, implemented docs and accepted ADRs take precedence over older planning snapshots.

## Acceptance

On a fresh and an existing installation, demonstrate a configured and observable worker and a real coding task in each mode. In parallel mode, one worker's attempt to modify another's file is refused before mutation. Kill and restart mid-effect without a duplicate write or privilege widening. Preserve explicit user approvals and show evidence-backed completion. Changes are not complete until implementation, independent review and verification on the actual final revision agree.

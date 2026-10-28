# PL-90 — Coding cycles, isolated workers and model supervision

Status: DRAFT planning; no production capabilities are implied by this document.
Baseline: `dev` at `fcb12b41f5e53c19cc20607a63c98b829452b505` (PR #151). Revalidate the branch, consumers, contracts and competing changes before implementation. This draft does not authorize bypassing approvals or widening authority.

## Objective and decisions under review

Deliver real end-to-end coding cycles, with two complementary execution modes: (1) parallel coding workers with genuinely isolated file rights and (2) a durable, long-lived, tool-using model supervisor that can invoke governed tools and cycles. Installation must make the necessary components available and explain readiness; automatic execution must be demonstrable, not just a configuration flag. Preserve user configuration and the existing host source installer.

Owner decisions still required: whether each start needs approval or a bounded, revocable preauthorized policy; exactly what `make install` starts and whether background services are opted in; whether isolation uses per-worker worktrees, enforceable exact-file capabilities, or both; and the policy for conflicts, integration and owner acceptance. No default below supersedes these decisions.

## Current evidence and competing approaches

- `harw-ops/src/intent_cycle.rs` starts an approval-gated, read-only bounded job. `harw-cli/src/job_worker_intent_cycle.rs` runs the driver; success is a proposed completion, not owner acceptance. `harw-plan-bridge/src/cycle_write_admission.rs` provides admission proof, not a write executor.
- `harw-cli/src/job_worker_work_driver.rs` already implements durable rounds, delegated work, central verification and a tool-less judge. Its own comments acknowledge that the union of parallel workers' path sets is checked after execution; this does not enforce per-worker ownership. `FileLeaseRegistry` is not yet a production writer authorization boundary.
- The model loop, WorkDriver and intent cycle are separate paths. `make install` builds and installs binaries and the UIA runner; service activation is separate. Cycle tools enabled in configuration do not themselves guarantee a running workspace-bound worker.
- Candidate A: extend existing WorkDriver with a durable tool-using supervisor and isolated writer adapter; reuse existing verification and job mechanics. Candidate B: extend the intent-cycle state machine with writing and child execution, requiring reconciliation with WorkDriver. Candidate C: retain one persistent root model and use explicit delegated jobs; durability and central verification must be proven. Compare all three against code and tests before selecting; do not create competing schedulers or duplicate security policy.

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
- Implement either verified file-capability enforcement across every effect path, or isolated worktrees with explicit admission and controlled integration; demonstrate how writable mounts, linked Git directories, shell, symlinks and extension paths are constrained. Fail closed for unsupported backends.
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

- Write negative and crash/restart integration tests before enabling write automation. Cover foreign tenant/workspace, stale epoch and approval, duplicate admission, lost acknowledgement after a real write, cancellation, symlink/rename escape, restricted worker trying another worker's file, exhausted budget, provider outage, conflict and failed verification.
- Exercise an end-to-end two-worker change with disjoint files and actual overlap, integration and central verification; prove the one-agent mode can use its approved tools and cycles without relying on children. Record versions and exact commit.
- Run formatting, clippy, workspace tests, documentation tests, xtask architecture/privilege gates, relevant sandbox/DoD tests and full CI on the final head. Report local tests separately from GitHub Actions. No merge or enablement while required security or recovery checks are unverified.

## Risks and unresolved questions

- Shared workspace plus path checks is unsafe if any capability or OS process can bypass them; isolated worktrees also need robust integration, containment and linked-gitdir handling.
- A workspace-global worker can claim another project's job unless the tenant and workspace are filtered **before** leasing. Expiry recovery must be atomic and must not reclaim a live lease.
- Long-running model sessions may consume or replay tool actions after interruption; in-flight effects, budgets and operator decisions must be fenced together.
- Documentation includes historical plans and conflicting statuses. Source, current executable tests, implemented docs and accepted ADRs take precedence over older planning snapshots.

## Acceptance

On a fresh and an existing installation, demonstrate a configured and observable worker and a real coding task in each mode. In parallel mode, one worker's attempt to modify another's file is refused before mutation. Kill and restart mid-effect without a duplicate write or privilege widening. Preserve explicit user approvals and show evidence-backed completion. Changes are not complete until implementation, independent review and verification on the actual final revision agree.

---
id: DEC-004
title: No parallel builds — central verification
status: accepted
date: 2026-09-27
tags: [decision, build, verification, concurrency]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# DEC-004 — No parallel builds — central verification

## Decision
Workers (subagents) never call `cargo`/`rustc` or a `make` target path that
invokes them — they only read and write code within their scope.
Verification (build, test, lint) runs exclusively and centrally, once per
wave, over the full combined state of all workers. At most one verification
runs at a time per workspace, enforced across processes by a file lock.

## Why
- Multiple simultaneous compiler runs fill `target/` with differing build
  states (flags, crate combinations) and have already exhausted disk space
  several times.
- A worker compiles an intermediate state that never exists: worker A builds
  while worker B is in the middle of an edit to a shared crate — this
  produces errors that the finished state does not have. The worker then
  chases phantom errors or "repairs" someone else's code.
- A green run by a single worker verifies a state that is never shipped as
  such. Only a build after all workers have finished is meaningful.
- Competing builds invalidate each other's cache and wait on the Cargo lock
  file; a central build that normally takes minutes thereby took almost an
  hour.
- This mirrors the repo-wide build rule from `CLAUDE.md` and makes it
  technically enforceable instead of merely a prompt convention.

## Consequences
- Workers have no process tools at all (no `cargo`, `rustc`, `make`); at the
  end they only report which tests they added and which commands the central
  build has to run.
- The only verification runs centrally, once per wave, over the full
  combined state — not per worker and not per file.
- A workspace-wide lock file (`<workspace_root>/.harw/verify.lock`) prevents
  two processes from verifying at the same time; a caller that hits a held
  lock gets `VerifyRunOutcome::Busy` — this is neither a test failure nor a
  `VerifyOutcome::Failed`, but a signal meaning "try again later".
- Trade-off: feedback to individual workers is delayed until the next
  central verification; this is accepted deliberately, since parallel
  building produces unreliable results and resource conflicts.
- Fits together with small, isolated scopes (DEC-005): the smaller the
  scopes, the less often workers collide in shared files before the central
  verification runs.

## Where in the code
- [harw-plan-bridge/src/verify_exec.rs](../../../harw-plan-bridge/src/verify_exec.rs) —
  `VerificationExecutor`, `VerifyRunOutcome::Busy`, `DEFAULT_LOCK_TIMEOUT`,
  `DEFAULT_LOCK_POLL_INTERVAL`, lock file `<workspace_root>/.harw/verify.lock`
  (`LOCK_DIR_NAME`, `LOCK_FILE_NAME`).
- [harw-plan-bridge/src/work_driver.rs](../../../harw-plan-bridge/src/work_driver.rs) —
  Work driver that deploys workers without process tools and triggers the
  central verification after each wave.

## Related
- [DEC-003 Provider limits](DEC-003-provider-limits.md)
- [DEC-005 Small scopes, many waves](DEC-005-small-scopes-waves.md)
- [DEC-007 Worker rights](DEC-007-worker-rights.md)

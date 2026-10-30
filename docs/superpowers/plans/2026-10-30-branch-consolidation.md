# Local Branch Consolidation Implementation Plan

> **For agentic workers:** Execute authorized local consolidation; root owns Git mutations and final integration. Independent audits and narrowly scoped fixes may run in parallel.

**Goal:** Produce tested local main/dev from every GitHub branch without losing source history or changing GitHub.

**Architecture:** Content reconciliation onto online dev, followed by explicit history preservation for reviewed branch and PR heads. Keep current runtime interfaces and security boundaries.

**Tech Stack:** Rust workspace, Git, GitHub CLI, Python script tests.

**Spec:** docs/superpowers/specs/2026-10-30-branch-consolidation-design.md

## Global Constraints

- No push, remote deletion, PR closure, service restart, or deployment.
- Preserve original heads in verified bundle and final ancestry.
- Centralize index/commits and expensive workspace verification in root.
- Work in the user-designated local checkout; no additional working branches.

## Review Focus

- Concurrent normal and piped starts respect a dynamically changed max_running.
- Failed or cancelled starts release reserved slots without double counting.
- Obsolete executors and old permission semantics never replace current code.
- Directory rename preserves manifests, packaging, tests, and new manuscript.
- Historical automation remains opt-in and never executes during consolidation.

### Task 1: Inventory and preserve

- [x] Enumerate every GitHub branch and PR head with gh pagination.
- [x] Fetch all online heads and record immutable SHAs.
- [x] Create and verify a complete pre-consolidation bundle.
- [x] Fast-forward local dev to frozen online dev.
- [x] Record content-based decisions for all 79 branches and 158 PR heads.

### Task 2: Recover atomic job admission

**Files:** harw-tool-job/src/manager.rs, harw-tool-job/src/manager/tests.rs.
**Interface:** Existing async start paths and live max_running API stay intact.

- [x] Reproduce oversubscription with a deterministic mixed-start test.
- [x] Reserve a slot across start awaits and release it on insert/failure/cancellation.
- [x] Run focused regressions and the crate suite; retain red/green evidence.

### Task 3: Recover independent additions

**Files:** harw-runtime/src/deployment.rs and lib.rs; historical paper/ and
scripts/ additions from 0a90f12e; docs/planning/87-tool-gaps/README.md;
docs/planning/92-web-research-fabric/README.md.

- [x] Restore the profile contract and its original tests.
- [x] Restore historical paper files with canonical-manuscript notices.
- [x] Recover factual PL92 review corrections and the historical tool-gap report.
- [x] Run profile tests and isolated paper script tests without external actions.

### Task 4: Apply directory rename

**Files:** paths in 397a1232d4117710e8f854e39cf69a5861cf58e3.

- [x] Take externally merged rename by fast-forward after outstanding Rust tests finish.
- [x] New upstream merge already resolves manuscript conflict while preserving new manuscript and renamed paths.
- [x] Check metadata, shell/Python checks, formatting, and gates.

### Task 5: Consolidate and verify

- [x] Commit per-head decisions and a non-executing remote-cleanup manifest.
- [x] Run canonical workspace clippy/tests/gates and resolve actual failures.
- [x] Obtain independent final review of recovered code and integration.
- [x] Preserve all reviewed heads in final ancestry with documented history-only merges.
- [x] Point local main and dev to the verified result; leave only these local branches.
- [x] Verify clean working tree, equal main/dev content, head coverage, and unchanged remote state.

## Execution rulings

- Current source and contracts take precedence over stale incompatible prototypes;
  source histories remain reachable. Translation-only old docs stay historical.
- No extra worktree: user designated this local checkout for consolidation,
  initially clean; pre-work bundle provides recovery.
- Online dev advanced during audit; took PR163 by fast-forward instead of
  independently replaying its rename.
- Canonical all-features verification revealed a pre-existing Cloud Gateway
  trait/feature wiring error. Fixed with explicit upstream refusal, six passing
  package tests and package Clippy; full workspace verification is running.

- Full-workspace Clippy passed. Full-debug test linking exhausted disk; only
  generated target/debug/deps was removed. Tests and gates run with the CI
  line-tables-only debug settings, one Cargo job, no incremental cache.

- Full tests exposed global command-store lifetime coupling in four agent-runner
  fixtures. Inject per-fixture ports; isolated/full red evidence and full-package
  green evidence retained. Independent review approved the test-only fix.
- Role guidance recovery exceeded the pre-existing 1700-byte per-role budget.
  Compact both texts to 1633/1683 bytes, retaining authority and notification
  rules plus recovered evidence/error discipline; targeted budget/job tests pass.

- Final full-workspace Clippy passes all targets/features with warnings denied.
  Full no-fail-fast suite completed18545pass/1failure/202ignored. The sole
  failed GNU target was corrected to honor existing truncation semantics and
  passed all7 tests on rerun; no production code changed for that correction.
  All other targets/doc-tests, gates and final formatting pass.

- History merge02eddce3 preserves all174 audited tips using40 independent
  parents, with identical tree before/after. Complete pre-cleanup bundle
  verified;240 local tracking/audit refs deleted atomically with expected SHAs.
  Only main/dev plus origin/main/origin/dev remain; GitHub79heads unchanged.

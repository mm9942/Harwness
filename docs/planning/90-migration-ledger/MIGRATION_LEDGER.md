# Harw Planning Migration Ledger

## MIG-000 — Planning tree established

**Baseline:** `d3e0b965696e7a631687a73618c934cda7ad3d2c`

**Code changes:** none.

**Planning documents registered:**
- Ecosystem workspace architecture
- Generic/Linux job runtime foundation
- Crypto/infrastructure masterplan
- CryptGuard service/Hyper/Tower plan
- DoD integration compartment
- Platform execution compartment

**Result:** no repository behavior is claimed to have changed.

---

## Entry template

### MIG-XXX — <title>

**Planning compartment:**  
**Baseline commit:**  
**Implementation commit / PR:**  
**Affected crates:**  

**Before:**  
**Target:**  
**Landed delta:**  
**Compatibility retained:**  
**Tests added/changed:**  
**Architecture gates changed:**  
**Documentation updated:**  
**Remaining plan items:**  

---

## MIG-001 — Architecture inventories and the `arch` gate (PL-10 P0–P2, Crypto H0)

**Planning compartment:** PL-10, PL-70 (H0)
**Baseline commit:** `d3e0b96`
**Implementation commit / PR:** branch `claude/r11-harw-ecosystem` (R11)
**Affected crates:** `xtask`, `harw-observe`

**Before:** crate ownership existed only as prose; no machine check of ring direction.
**Target:** every crate classified F/I/C/J/D/A with TCB allowlists, enforced in CI.
**Landed delta:** `docs/architecture/{harw-workspace-inventory, harw-dependency-inversions, job-extraction-map, crypto-drift-report, dod-workspace-merge-plan}.md`; `xtask/arch-policy.toml` plus gate `arch` (layer rules, ratcheting exceptions, TCB allowlists, no `*-sys` in J/TCB); the unused `harw-observe → harw-authority` edge is removed.
**Compatibility retained:** no runtime behaviour change.
**Tests added/changed:** 20 unit tests in `xtask/src/gate_arch.rs`, including a real-repository run.
**Architecture gates changed:** new `arch` gate (1966 checks, green).
**Documentation updated:** `CONTRIBUTING.md`, `docs/setup/install.md`, `docs/architecture/README.md`.
**Remaining plan items:** resolve the exceptions `harw-config → harw-agent-dsl`, `harw-agent-compiler → harw-registry-defaults`, `harw-session-store → harw-job-*`.

## MIG-002 — DoD joins the root workspace (PL-60)

**Planning compartment:** PL-60
**Baseline commit:** `d3e0b96`
**Implementation commit / PR:** branch `claude/r11-harw-ecosystem` (R11)
**Affected crates:** all 32 `dod/crates/*`, `xtask`

**Before:** `exclude = ["dod"]`, separate `dod/Cargo.toml` and `dod/Cargo.lock`.
**Target:** one workspace and one lockfile; isolation through gates.
**Landed delta:** nested workspace removed, DoD crates listed explicitly in root `members`, `semver` joins `[workspace.dependencies]`, xtask gates read one graph, CI/Makefile/Dependabot run DoD package-scoped from the root.
**Compatibility retained:** `dod/Makefile` still builds the privileged binaries package-scoped into `dod/target`.
**Tests added/changed:** `test_dod_crates_are_root_workspace_members`.
**Architecture gates changed:** `warden-deps` stays at 54. `cargo tree -e normal` before/after: harw-warden 88→88, harw-dod-warden 41→41, harw-dod-sentinel 70→70, harw-sentinel 182→182, harw-probe-bpf 159→159, harw-probe-fs 101→100. No `libbpf-sys`, `openssl-sys` or `libseccomp` in the graph.
**Documentation updated:** DoD design docs, setup docs, `CHANGELOG.md`.
**Remaining plan items:** feature-unification guard for `--workspace` builds of privileged binaries (see `dod-workspace-merge-plan.md`).

## MIG-003 — `harw-job-core` and `harw-job-store` (Job P1, P7 part)

**Planning compartment:** PL-20
**Affected crates:** `harw-job-core` (new), `harw-job-store` (new), `harw-job-runtime`, `harw-session-store`

**Landed delta:** governance types moved from `harw-job-runtime` into `harw-job-core` (compatibility re-exports kept); new platform-neutral ids, lifecycle state machine (all 110 state/event pairs tested), `JobSpec`/envelope, `ExitOutcome`, `SandboxReport` with `EnforcementState`. `harw-job-store` owns the generic record mechanics (cap-std root, fs4 locks, atomic persist, fencing); `harw-session-store::JobStore` is a thin adapter with unchanged API and on-disk layout.
**Remaining plan items:** move the known inversions (`TenantId`, `WorkspaceId`, `TraceContext`, `serde_json::Value`) out of the moved governance types.

## MIG-004 — Linux mechanics: `harw-job-linux`, `harw-job-exec`, `harw-job-tokio` (Job P2–P6)

**Planning compartment:** PL-30
**Landed delta:** pidfd process handle, procfs-based identity, identity-verified recovery, in-house cgroup v2 backend (no libcgroups, no C), Landlock / NO_NEW_PRIVS / capability drop with an honest `SandboxReport`; the safe re-exec trampoline `harw-job-exec` replaces `pre_exec` (order cgroup → rlimits → sandbox → report → requirement check → exec); the async supervisor over `AsyncFd` pidfd readiness.
**Deviation:** cgroup v2 is implemented in-house over cap-std instead of `libcgroups` (MSRV and C risk).

## MIG-005 — Callers, Bubblewrap and Darwin backends (Job P7, PL-40, PL-50)

**Planning compartment:** PL-20, PL-40, PL-50
**Landed delta:** `harw-tool-job` uses `harw-job-linux` (meta.json v2 with process identity, v1 still read; a reused PID is never signalled); `harw-agent-runner` routes its group kill through the checked path; `harw-job-executor-bwrap` maps `SandboxPolicy` onto the existing `harw-sandbox` bwrap mechanism (filesystem reported `Partial`, since bwrap cannot separate read from execute); `harw-job-darwin` (waitid watcher instead of the unsafe kqueue API, `sandbox-exec` reported `Partial`, unverifiable recovered processes are never signalled); CI job `macos`.

## MIG-006 — Coordinator and facade (Job P9)

**Planning compartment:** PL-20
**Landed delta:** `harw-job-runtime::coordinator` (claim → lease heartbeat → executor → finalize; restartable recovery; sandbox requirement enforced before a job counts as running) and the facade crate `harw-job`.
**Remaining plan items:** automatic retry/requeue, re-opening a job cgroup after restart, trampoline backend end-to-end test, supply-chain tier document (`docs/architecture/dependency-review.md`), removing the `harw-job-runtime` compatibility re-exports once callers move to `harw-job-core`.

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
**Remaining plan items:** resolve the exceptions `harw-agent-compiler → harw-registry-defaults`, `harw-session-store → harw-job-*` (`harw-config → harw-agent-dsl` is resolved: raw `agent_sources` in `harw-config`, lowering in `harw_registry_defaults::config_agents`).

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

## MIG-007 — One systemd/package source of truth (Crypto H10)

**Planning compartment:** PL-70 (Crypto Masterplan v2 §22, §23, §34 H10, §40)
**Baseline commit:** `140ab6a`
**Implementation commit / PR:** R12 round (number to be fixed at merge)
**Affected crates:** `harw-install`, `harw-cli`; DoD packaging (`dod/scripts`, `dod/Makefile`, `dod/packaging`)

**Before:** two diverging unit trees (`deploy/systemd/*`, embedded only under `cfg(test)`; `dod/packaging/systemd/*`, actually installed) — see `docs/architecture/crypto-drift-report.md` §5.
**Target:** one canonical service/socket definition per daemon, embedded and tested.
**Landed delta:** `deploy/` is the only source: DoD units (`harw-dod.target`, `harw-sentinel`, `harw-probe-bpf`, `harw-probe-fs`, `harw-warden.{service,socket}`), infra units (`harw-infra.target`, `harw-control.service`, `harw-auth-hub.{socket,service}`, `harw-netsec.{socket,service}`, `harw-security-hub.service`), `deploy/sysusers.d/harw.conf`, `deploy/tmpfiles.d/harw.conf`. Stricter variant per unit: warden on its own user with `CAP_DAC_OVERRIDE CAP_NET_ADMIN` (not root, not `CAP_SYS_ADMIN`), BPF probe `CAP_BPF CAP_PERFMON` (loader requires both), `SystemCallFilter=@system-service` everywhere. `harw-install::deployment` embeds every `deploy/` file in production code (`EmbeddedDeploymentAsset { path, contents, kind }`); new CLI `harw install --print-systemd [UNIT]`. `dod/scripts/install.sh` installs from `deploy/`, rejects unresolved `@PLACEHOLDER@`s and removes the pre-H10 `harw-dod-*` unit files. `dod/packaging/{systemd,sysusers.d,tmpfiles.d}` deleted.
**Compatibility retained:** DoD install knobs (`PREFIX`, `LIBEXECDIR`, `BPFDIR`, `SYSCONFDIR`, `STATEDIR`, `LOGDIR`, `DESTDIR`) unchanged; `RUNSTATEDIR`/`RUNTIMEDIR` removed (runtime is fixed `/run/harw`). Unit names change (`harw-dod-sentinel` → `harw-sentinel`, `harw-dod-bpf` → `harw-probe-bpf`, `harw-dod-warden` → `harw-warden`), accounts change (`harw-dod`/`harw-dod-bpf`/`harw-dod-ipc` → per-binary accounts and `harw-ipc`); old accounts are not deleted automatically.
**Tests added/changed:** `harw-install/src/deployment/tests.rs` (parity deploy/ ↔ embedded ↔ manifest ↔ install.sh, INI shape, ExecStart binaries are workspace binaries, socket/service/target references, sysusers coverage, socket directories from tmpfiles, hardening, `print_systemd`); `dod_units` tests now read the embedded assets; `harw-cli` parse and `lifecycle::install` tests.
**Architecture gates changed:** none.
**Documentation updated:** `docs/setup/dod.md`, `docs/cli.md`, `dod/README.md`, `docs/design/dod-system-operations-authority.md`, `docs/architecture/crypto-drift-report.md`.
**Remaining plan items:**
- ~~TODO(H9): `harw web` socket activation~~ done: `harw web --systemd-socket` takes the LISTEN_FDS=1 descriptor; `deploy/systemd/harw-control.socket` (`/run/harw/infra/control.sock`, `0660 harw-control:harw-control-clients`) ships and `harw-control.service` is socket-activated (no `[Install]`, no `ReadWritePaths=/run/harw/infra`). All infra daemons are socket-activated, so `/run/harw/infra` is `0755 root:root` and the `harw-infra` group is gone. Still open, TODO(H9): verify bubblewrap tool sandboxing under the unit's `SystemCallFilter=`.
- `harw-auth-hub` and `harw-netsec` use their real `--systemd-socket` flag. The three infra crates landed in parallel with H10; `PLANNED_BINARIES` in the parity test tolerates them until they are root workspace members, then the list can be emptied.
- Verify the warden capability set (`CAP_DAC_OVERRIDE CAP_NET_ADMIN`) and the per-unit `SystemCallFilter=` additions on a real host (`systemd-analyze verify`, `systemd-analyze security`).
- `dod/crates/harw-probe-bpf` still names `/run/harw-dod/sentinel.sock` in CLI tests and `harw-dod-bpf.service` in `src/sensors.rs` docs (cosmetic; owned by the DoD crates).

## MIG-008 — Compiler ↔ execution contract (PL-90)

**Planning compartment:** PL-90
**Implementation commit / PR:** branch `claude/r12-execution-crypto`
**Affected crates:** `harw-agent-dsl`, `harw-agent-compiler`, `harw-agent-runner`, `harw-job-linux`, `harw-job-runtime`, `harw-job`, `harw-registry-defaults` (golden IRs)

**Landed delta:**
- `ExecutionRequirements` is a hashed IR section. The new pass `DeriveRequirements` sits between `ResolveModels` and `ChildClosure` and unions children into the parent.
- `harw agent inspect` shows the section.
- The side-effect-free host probe is `HostReport::probe`, covering the Landlock ABI (via a throwaway thread), cgroup v2 detection, bwrap and user namespaces.
- Runner admission runs before start:
  - **Required** = the dimension is actually enforced, fully or partially, and is never overridable.
  - **BestEffort** can be overridden with `--allow-degraded`.
  - A refusal exits with 69; `--requirements` shows the requirements and verdict.
- Landlock is no longer derived as a hard kernel requirement, because the backend is the runtime's choice.
- The job layer (`SandboxRequirement::Required`) stays strict: fully enforced only.
- All 62 golden IRs were re-blessed once; only `requirements` and the digest changed.

**Remaining:** a native (`--native`) end-to-end build in CI; DSL syntax for explicit target and kernel demands.

## MIG-009 — CryptGuard 3.1 and security vocabulary (Crypto H0/H1)

**Landed delta:**
- The workspace uses `crypt_guard` 3.1 from git (branch `main`, lock pins `9b2df9a`).
- The `harw-secrets` KATs and six V1/V2 golden records frozen under 3.0.2 still pass unchanged.
- The ignores for RUSTSEC-2026-0207/0208/0212 are removed; `libcrux-ml-kem` is now 0.0.10.
- `harw-types` gains:
  - the IDs `NodeId`, `DeviceId`, `ServiceIdentityId` and `SecurityContextId`;
  - `TrustZone` and `AuthStrength`;
  - `SecurityContext`, which is not Deserialize, not constructible from outside, and only issued via a non-Clone `SecurityContextIssuer`; compile_fail doctests guard this;
  - a `SecurityContextSummary` for display.
- There is no `KeyGeneration` type: the CryptGuard `KeyVersion` is the Harwness `KeyVersion` + 1.

**Remaining:** switch from git to crates.io once 3.1.0 is published.

## MIG-010 — `harw-dod-encrypt` and crypto gates (Crypto H2)

**Landed delta:**
- New DoD crate with the §4 purposes, a key-usage policy (a signing key only signs transcripts of its own purpose, so there is no signing oracle), canonical `SignTranscript`, a HarwSecureFrameV1 header, a replay window, and a `cg` boundary module with `HarwUsageAuthorizer`.
- The facade cannot import it: compile_fail doctests plus a manifest test.
- New gate rule `FORBIDDEN_REACH` checks the dependency hull. It keeps warden, probes and sensors from reaching `crypt_guard*`, `hyper`, `tower` and `harw-dod-encrypt`.
- The warden hull is unchanged at 88.

## MIG-011 — AuthHub, infrastructure client, runtime wiring, operation domains (Crypto H3–H5)

**Landed delta:**
- `harw-auth-hub` (lib + daemon):
  - A CryptGuard KMS on `/run/harw/infra/secure.sock`.
  - Authentication by SO_PEERCRED, with bearer tokens as fallback.
  - Namespace policy, an audit trail, health/version/capabilities routes, and systemd socket activation.
- `harw-infra-client`:
  - `AuthHubClient` (Clone as a network handle only), plus `NetworkControlClient` and `SecurityHubClient` with context verification.
  - `AuthHubDekWrapper` implements the secrets `DekWrapper` trait through a sync/async bridge with no nested runtime.
- Runtime and config:
  - New `[infrastructure]` config section; untrusted repo layers are ignored.
  - `RuntimeServicesParts.infrastructure` is exposed on the Slash and Web surfaces only, not to models.
  - `InfrastructureContributor`.
  - Operations `infra.status`, `infra.health`, `infra.auth.keys.describe` and `infra.auth.keys.rotate` (Owner tier, approval always).
- `OperationDomain` gains Identity, Network, Security and Crypto. Naming rule: `infra.<area>.<noun-plural>.<verb>`.

**Remaining:** persistent, sealed key storage in the AuthHub (keys are in memory today); an AuthHub-backed `NodeSigner`.

## MIG-012 — Secrets V3 (Crypto H6)

**Landed delta:**
- `SecretEnvelopeFormat::KmsWrappedV3` adds `key_id`, `key_generation` and `crypto_profile_id` as optional fields, so V1 and V2 JSON stay byte-identical.
- The `DekWrapper` trait comes with `LocalHpkeDekWrapper`.
- Migration is explicit only: `rewrap_to_v3` (the V2 ciphertext is kept) and `SecretStore::migrate_all_to_v3`. A store with a wrapper writes V3; the default stays V2.

## MIG-013 — NetSec, Security Hub, sockets, remote transport, multi-user (Crypto H7–H9, H11, H12)

**Landed delta:**
- `harw-netsec`: node state machine, atomic store and `network.sock`.
- `harw-security-hub`:
  - The only issuer of `SecurityContext`s, on its own `security.sock` (deviation from §39).
  - Contexts can only be narrowed, never broadened.
  - Read-only DoD correlation from the sentinel's new optional JSONL export (`--findings-export`).
- All infra daemons, including `harw web --systemd-socket` on `control.sock`, are socket-activated. `/run/harw/infra` is now `0755 root:root`, and `harw-control-clients` is the client group.
- `/v1/health|version|capabilities` share one JSON contract across all daemons.
- `harw-node-transport`:
  - TLS 1.3 using X25519MLKEM768 only, over the rustls/aws-lc stack already locked.
  - Mutual ML-DSA-65 node authentication bound to the TLS exporter, a replay cache, and the DoD uplink as NDJSON.
- `harw-web` multi-user:
  - A `LocalPeerIdentityResolver` with the tier map as default, plus a security-hub mode.
  - A client presents a hub context id, which is bound to its uid; the tier can only be narrowed.
  - The tenant is scoped into the `OpContext`.

**Remaining:**
- Validate the warden/probe capabilities and `SystemCallFilter=` on a real host.
- Tenant filtering in the individual operations; only the plumbing and `tenant_admits` exist.
- Harden the key rotation workflow for remote nodes.

## MIG-014 — Work driver, KMS hardening, tenants, dependency inversions (R14)

**Landed delta:**
- **Work driver:** job kind `work_driver` plus the tools `work_driver.enqueue`, `.status` and `.stop` (no TUI).
  - `WorkDriver::decide`; the internal judge worker (`work_driver_judge`, `{"passed": bool}`).
  - AIMD parallelism within provider limits; one verification per workspace at a time (`verify.lock`).
  - Its own job lane (at most 2 concurrent runs); owned-path checks; resume after a restart.
  - Decisions DEC-001…DEC-008 in `docs/planning/70-decisions/`.
- **KMS:**
  - Sealed persistent key store with a stable store epoch, a KEK from a file or a systemd credential.
  - `KmsGuardProvider`: rotation is compare-and-set, wrapping uses the primary only.
  - Purpose-bound signing (`HarwUsageAuthorizer`, no signing oracle), a crypto worker thread, HTTP/1 timeouts and a connection limit.
  - Client `sign`; unwrap of older generations pinned to the recorded version; `AuthHubNodeSigner`.
  - ML-DSA is enabled in `crypt_guard_service`.
- **Tenants** on goals, plans, approvals and work-driver jobs. The tenant filtering left open in MIG-013 is done for these operations.
- **Architecture:**
  - The compiler → registry-defaults and config → agent-dsl inversions are removed (install slot, raw `agent_sources`).
  - harw-core, harw-session-store, harw-mcp-server and harw-knowledge now depend on `harw-job-core`.
  - The arch gate flags C build helpers; `openssl` is banned; the tool-name codec follows the strictest common provider rule.
- **Job coordinator:** retry, and the cgroup is reopened after a restart (MIG-006 rest).
- **Docs:** work-driver and KMS guides; dependency review and inventory; the Remote Sessions plan (`65-cloud-sessions`); Copilot instructions and backlog.

**Verification:**
- `cargo test --workspace`: 14,408 passed, 0 failed, 180 ignored (the last 6 fixes were re-verified per crate).
- DoD crates `--locked`: 1,636 passed.
- clippy `-D warnings`, gates, deny and actionlint are green.

**Remaining:**
- A sandboxed verify runner for the work driver under `harw serve` (C-09).
- A provider pacing hook for TPM (C-06).
- Preventive path-level write rights (DEC-009 proposal, C-08).

## MIG-015 — R15: sandboxed verify, proactive provider pacing, CryptGuard from crates.io

**Status:** round R15 complete on branch `claude/r15-verify-pacing`, still an uncommitted working-tree diff at the time of writing — pending the one central build/test run and the merge to `dev`. `origin/dev` HEAD (`02401aa`) does not contain any of this delta yet.

**Landed delta (on the branch, not yet merged):**
- **C-09, sandboxed verify for the work driver:**
  - `harw-cli/src/verify_sandbox.rs` builds a `CoordinatorVerifyRunner` from `FsJobRecordStore` and `LinuxExecutor`, with the Landlock trampoline `harw-job-exec` and bwrap as the fallback.
  - `harw serve` puts it into `JobWorkerContext.verify_runner`.
  - Without a backend, Command steps still escalate as unverifiable; they never run unsandboxed.
- **C-06, proactive pacing:**
  - `ModelProvider::pacing_wait()` has a default of `None`. The HTTP providers return the larger of the rate limiter wait (headers or 429 cooldown) and the configured budget preview (`preview_wait`).
  - The wrappers forward it; the router asks its default backend.
  - The work driver pauses before each wave chunk (the pause can be cancelled).
- **CryptGuard 3.1.0 now comes from crates.io instead of git.** `deny.toml` no longer allows any git source. The KATs and the frozen V1/V2 fixtures are unchanged.

**Verification:** not yet run. Per the repository's build rule, only the main session builds once, after every agent has finished, over the complete combined state. Once that central build has run green, append the actual results here (matching the MIG-014 pattern) before changing the status to LANDED:
- `cargo fmt --all` clean.
- `cargo test --workspace` (incl. doc tests): pass/fail counts.
- DoD crates, package-scoped `--locked`: pass/fail counts.
- clippy `-D warnings`, `cargo run -q -p xtask -- gates`, `cargo deny check`, `actionlint`: green/red.

**Remaining:**
- Merge to `dev` once the central build above is green.
- Preventive path-level write rights (DEC-009 proposal, C-08).
- A cloud home proposal (DEC-010, C-10).

---

## MIG-PL90-W03w — Intent cycles wired end to end (read-only explorer recipe)

**Planning compartment:** `docs/planning/90-intent-driven-composed-agents` (04 §7)
**Baseline commit:** `dev@6e34f2e3` plus PR branches #141, #143, #144, #145/#146/#147 and the newest docs branches (provider lifecycle 2026-10-14, Linux user cloud control 2026-10-16), merged on the integration branch
**Implementation commit / PR:** integration branch `ccr-c2654e2c-284ibb` → `dev`
**Affected crates:** `harw-plan-bridge`, `harw-ops`, `harw-registry-defaults`, `harw-runtime`, `harw-cli`

**Before:** W01–W03 building blocks existed (`intent_cycle`, `cycle_runtime`, `cycle_explorer`, `cycle_proposer`), but nothing called them: no tool, no job kind, no worker, no verifier. `Complete` was unreachable because nothing produced `Verified` evidence.
**Target:** a durable, human-approved, read-only intent cycle that can be started, observed and stopped as a tool and runs as a fenced job.
**Landed delta:**
- `harw-plan-bridge::cycle_verifier` — deterministic `ContentCheck` per acceptance criterion; `Verify` re-reads the observed file, requires an unchanged digest, emits `Verified` evidence of source `Verification` or an `unmet:`/`stale:` claim. `ExplorerCycleExecutor` = explorer (read/wait) + verifier (verify).
- `harw-plan-bridge::cycle_spec` — `ReadOnlyCycleSpec` (objective, read targets, checks, limits ≤ host ceiling) with one canonical intent digest; `plan()` is the single validation used by the operation and the worker.
- `CeilingReissuer` — narrowing-only reissue of the persisted `AuthoritySnapshot` against the worker's current trusted ceiling; `INTENT_CYCLE_JOB_KIND`, `CycleStore::open_in_job_root`.
- `harw-ops::intent_cycle` — `intent_cycle.start` (operator, `approval = "always"`), `intent_cycle.status` (observer, read-only), `intent_cycle.stop` (operator, `approval = "always"`); registered by `register_intent_cycle_tools` behind the `[tools.plan]` gate and mounted by the runtime assembly.
- `harw-registry-defaults` — catalog rows (`start` = `Read`, `status`/`stop` = `Meta`) and `ReadWorkspace` as the tool right of all three.
- `harw-cli` job worker — `intent_cycle` jobs run in the long-run lane: fence + scope check, spec re-validation, read-only explore sandbox as ceiling, record creation on first claim, `CycleDriver` with `ModelCycleProposer` (budgeted provider, `Explorer` internal model or default model), job cancel → driver cancel, wall budget.
**Compatibility retained:** no existing tool, job kind or permission changed; the cycle is read-only (no write targets, children or recipes).
**Tests added/changed:** verifier (pass, unmet, stale, permission, non-repository evidence, explorer contract), spec validation and digest, W03 cross-module test `spec_read_verify_complete_reaches_completion_proposed`, ops start/refusal/status/stop tests, worker e2e `worker_drives_a_cycle_to_completion_proposed` and malformed-input test; registry completeness lists updated.
**Architecture gates changed:** none (no new crate edges: `harw-ops → harw-plan-bridge` and `harw-cli → harw-plan-bridge` already existed).
**Documentation updated:** PL-90 README and 04 §7.
**Remaining plan items:** W03c learning outbox, W03d run isolation, small-model benchmark, W04+ (orchestrator, write waves, research, Matrix, LaTeX, DoD).

---

## MIG-016 — Harw-DoD suite reachable through harw (PL-97 D1–D5)

**Planning compartment:** `docs/planning/97-harw-dod-mission-control-chain` (§3.1, §4 D1–D5), `docs/planning/50-dod-integration`
**Baseline commit:** `dev@fcb12b41`
**Implementation commit / PR:** branch `ccr-54aa3dfd-o46he2`, draft PR #154 → `dev`: D1 `30b44321`, D2 `7903df02`, D3 `af2b53a0`, D4 `703d9745`, D5 `2ce5b2fd`; security fixes from the wave-1 review `4b080e50`, `3eaccea2`, `64d3831c` (PR #156)
**Affected crates:** new `harw-dod-client` (layer A); `harw-sentinel`, `harw-config`, `harw-install`, `harw-runtime`, `harw-ops`, `harw-cli`, `harw-registry-defaults`, `harw-home`, `harw-tui`

**Before:** the DoD daemons ran as a separate install; harw read at most `<home>/dod/export/findings.jsonl` through the opt-in `security_signals` source; no CLI, operation, model tool or TUI surface; the sentinel unit passed neither `--config` nor exports.
**Target:** one read-only path from harw to the suite's posture, the same on every surface; enforcement stays human-only (invariant S1).
**Landed delta:**
- "Harw-DoD (Detect · Orient · Defend)" is the suite name in human-facing text; crate, binary, unit and path names are unchanged.
- `harw-sentinel.service` runs with `--config`, `--findings-export` and the new atomic `--health-export` (`health.json`); findings line `schema_version: 2` with optional `scope` (additive). New empty group `harw-dod-readers`; export dir `2750 harw-sentinel:harw-dod-readers`.
- `[dod] export_dir` (trusted layers only) and `findings_window_hours` (default 24); an invalid `[dod]` table fails config loading.
- `harw-dod-client`: export resolution, bounded tail reader (256 KiB, 16 KiB lines, no symlinks), `health.json`, config check, `systemctl show` unit state, `DodStatus`. Only the system export (`source: system`) is verified; home fallback, `export_dir` override and an unreadable system export carry an `UNVERIFIED` hint and are never silently replaced.
- `harw dod status|doctor|findings|sensors|config check|config suggest|enable|disable|restart|warden status|enable|disable|install --print-systemd`, `--json`, exit codes 0/1/2/3 (3 = refused: not euid 0, or no valid `active_profile`).
- `/dod` operation and the read-only, auto-approved model tools `dod.status`, `dod.findings`, `dod.sensors` (5 s cache, bounded, sanitized). The four security triage roles admit exactly these tools; their child sessions get them through a role-bound read-only provider (`harw-runtime/src/children.rs`).
- Bundled skill `harw-dod` with the first real `triggers.toml` (tools `dod.*`, keywords such as `egress`, `sentinel`, `warden`, `cgroup`).
- TUI: `/dod view` overlay, status-line mark `⛨ n high`, Finished/Failed title suffix `· ⛨ DoD n`.
**What changed for operators:** `make -C dod install` run unprivileged (it builds, then asks for sudo for the file install and the enable step; or `make -C dod build` followed by `sudo make -C dod install-existing`), then `harw dod config suggest`, edit `/etc/harw-dod/config.toml`, `sudo harw dod enable`, `harw dod status`; add users to `harw-dod-readers` by hand to read the exports; `security_signals` now uses the same resolver.
**Compatibility retained:** v1 export lines parse in every reader; no harw crate links the Warden or the escalation ladder; the Warden stays disabled by default.
**Tests added/changed:** see PL-97 §9.1 rows D1–D5 (harw-sentinel, harw-install, harw-dod-client, harw-cli `dod_cmd`, harw-ops `dod`, harw-tui `dod_view`).
**Architecture gates changed:** `harw-dod-client` in `xtask/arch-policy.toml` (layer A, forbidden from the Warden/escalation/daemons); spawn policy allows the `systemctl` status query.
**Documentation updated:** `README.md`, `dod/README.md`, `docs/setup/dod.md`, `docs/cli.md`, `docs/design/harw-dod-charter.md`, `docs/security/pl97-security-review.md`.
**Remaining plan items:** E4 jobs ↔ findings correlation (client half only); `harw dod enable --grant-read`; the `make -C dod install`/`stage` enable step versus "no silent services" (PL-97 §8.4); Warden v2 proofs (PL-96 W01).

## MIG-017 — Skill triggers and the tool landscape (PL-97 B1–B5, PL-95 S2–S6, PL-93)

**Planning compartment:** `docs/planning/97-harw-dod-mission-control-chain` (§3.2, §4 B1–B5), `docs/planning/95-cli-tools-skills`, `docs/planning/93-common-command-tools`, `docs/planning/87-tool-gaps`
**Baseline commit:** `dev@fcb12b41` (PL-95 S1 `triggers.toml` engine already on `dev`, `3df91189`)
**Implementation commit / PR:** draft PR #154: B4 `6f33d09a`, B1 `765d6e9d`, B2 + B3 `af2b53a0`, B5 + G1 `703d9745`; review fix `8c39f26b` (unknown skills fail closed to Imported), `87d7fa45` (git read cap)
**Affected crates:** `harw-core`, `harw-catalog`, `harw-context`, `harw-config`, `harw-session-store`, `harw-runtime`, `harw-registry-defaults`, `harw-ops`, `harw-instructions`, `harw-cli`, `harw-home`, `harw-agent-runner`, `harw-tool-{sys,gitread,fsread}`

**Before:** `triggers.toml` was parsed but nothing injected it; `fsread.*`, `git.*` and `sys.*` were not registered; no tool index CLI, no tool→skill link, no description lint.
**Target:** the right skill excerpt reaches the model when a trigger fires, within budget, with provenance; the internal tools are registered and discoverable.
**Landed delta:**
- Per-session `SkillInjector` in the turn loop: `skills.triggered` fragments after the cache-stable prefix, dedupe across rounds, per-turn and per-session budgets with typed drop reasons, reset after compaction, `injection` records in the session store; skills whose tools the session lacks are skipped. New `[skills]` (`triggered_injection`, `inject_imported`, `per_turn_bytes`, `per_session_bytes`; profile scope).
- `fsread.*` (16) and `git.*` (6) in `Full` + `ReadOnlyExplore` (auto-approved), `sys.*` (12) in `Full` only (asks like `process.list`); the four tool skills bundled with `triggers.toml`.
- `ToolIndexEntry::skill`, read-only model tool `tool.index`, `harw tool list|describe`, the cache-stable `## Tool families` prompt block, one registry lint (≥ 60 chars, start pattern, every argument described; allowlists with reasons).
- `harw skill list|show|preview|validate` and `/skills preview|validate` (same selection code as the injector, no model call).
- B5: clap-tree walk (help text everywhere; `--json` or an explicit text-only allowlist). G1: pinned Full-root tool-spec budget test.
**What changed for operators:** sessions now receive triggered skill excerpts by default (`[skills] triggered_injection = false` turns it off); `harw skill preview …` shows what would be injected; `harw tool list|describe` lists every tool of the build; `git.*`/`fsread.*` calls are auto-approved, `sys.*` asks.
**Compatibility retained:** the stable instruction prefix is byte-identical across turns; imported skills are `Evidence` and off unless `[skills] inject_imported = true`.
**Tests added/changed:** PL-97 §9.1 rows B1–B5, G1.
**Architecture gates changed:** none beyond the new registrations (catalog, `tool_permission`, approval lists).
**Documentation updated:** `docs/cli.md` (`harw skill`, `harw tool`), `docs/design/config-structure.md` (`[skills]`), PL-93 and PL-95 status.
**Remaining plan items:** G1 small-window refusal of a Full root (**owner decision**); the eight remaining `cargo.*` tools and `cargo-tools` command/error-code triggers; synthetic first-use `Tool` trigger; the root's real tool set in `## Tool families`; `job.*` lint; `SkillInjector` for child sessions; `harw tool call`, `harw skill new|import|stats` (PL-95 S7–S9).

## MIG-018 — TUI Mission Control (PL-97 C1–C5, C4b, R1; PL-94 W00–W02, PL-68 W20)

**Planning compartment:** `docs/planning/97-harw-dod-mission-control-chain` (§3.3, §4 C1–C5, C4b), `docs/planning/94-adaptive-tui-mission-control`, `docs/planning/68-mobile-tui`
**Baseline commit:** `dev@fcb12b41`
**Implementation commit / PR:** draft PR #154: C1 `ca2dfd6c`, C4a `d21fc698`, C2 `6db6745a`, C3 + C4 `3d98064d`, M3 + D5 + C4b + R1 `2ce5b2fd`, C5 `654b2677`, spawn allow-list `86867b59`
**Affected crates:** `harw-tui-layout`, `harw-tui`, `harw-tool-job`, `harw-agent-runner`, `harw-config`, `xtask`

**Before:** one portrait dock and strip; agents and jobs in one panel; no terminal aggregates, no job stop from the panel, no hard stop, reasoning as a dim aside.
**Target:** chat-first Mission Control with separately bordered windows in portrait and landscape (W02), bounded live projections, operator stop through the command path.
**Landed delta:**
- `harw_tui_layout::classify_mission`: PortraitStack / LandscapeThreeColumns / ShallowLandscape / CompactSummary.
- `harw-tui/src/mission/`: Active Agents, Active Jobs, Finished/Failed and LIVE windows, each with its own focus, scroll and selection; landscape anchors (Chat left, Finished top-centre, LIVE below, Agents over Jobs right); click-to-focus; wheel scrolls only the window under the pointer.
- Keys: `Alt+J` (`focus_jobs`), `Alt+F` (`focus_finished`), `Alt+L` (`toggle_live_detail`), `p` pins the selected agent to LIVE, `x` in the Jobs window opens a confirmation and dispatches `/jobs stop <id>` through the regular command path.
- Terminal aggregates by typed provenance (`ManagedJobOrigin`; agent+job of one child counted once; `Unknown job`; `Agent child (pending)`), `✗ n NEW` badge, retention 50 per category.
- 2×Ctrl+C hard stop while work runs: managed jobs via `JobManager::stop`, `harw-job-*` cgroups via `cgroup.kill`, process-tree fallback only without a known cgroup; exit 130.
- `[tui] reasoning = "message" | "collapsed" | "hidden"` (default `message`).
- C5: inline activity-ranked live cells (`app/live_slots.rs`), at most two virtual cells at slot #2/#3, `+N weitere aktiv`, identity anchor while scrolled up.
**What changed for operators:** the TUI layout changes at every size (portrait windows, three landscape columns, compact counters at the smallest sizes, e.g. 39×60 and 48×19); new default chords `Alt+J/F/L` yield to chords a keybindings file already uses; reasoning summaries are expanded by default (`[tui] reasoning = "collapsed"` restores a compact view); a second Ctrl+C during a turn or a running job is now an emergency stop (exit code 130).
**Compatibility retained:** R19 portrait tests unchanged; canonical history cells, export, persistence and model context are untouched by the live projections; `ManagedJobOrigin` is provenance only.
**Tests added/changed:** PL-97 §9.1 rows C1–C5, C4b, R1 (harw-tui 1426 lib tests at `654b2677`).
**Architecture gates changed:** spawn policy allow-lists the read-only `ps` snapshot of the hard stop.
**Documentation updated:** `docs/design/tui-command-contract.md` §8, `docs/design/tui-architecture.md`, PL-94/PL-68 status.
**Remaining plan items:** durable-job stop from the Jobs window (PL-97 §8.3); the `⛨ n` job badge (E4); a TUI mark for the execution chain report (E5); window title language.

## MIG-019 — Execution chain: Bubblewrap ∘ Landlock ∘ cgroup v2 ∘ DoD (PL-97 E1–E5)

**Planning compartment:** `docs/planning/97-harw-dod-mission-control-chain` (§3.4, §4 E1–E5, §8.2), `docs/planning/90-intent-driven-composed-agents/07-coding-cycles-orchestration-plan.md` (PR #153 contract)
**Baseline commit:** `dev@fcb12b41`
**Implementation commit / PR:** draft PR #154: E1a `2cfd4e7a`, E1b `a07f7f6f`, E2a `0922d70e`, E2c `08aaecd8`, E2d `93471a44`, E2b `05d2d5b7`, E3b `1b63871c`, E3c `a214d590`, E2e + E3 + E5 `f31d639f`; E2f (RLIMIT_NPROC through `ResourceRequest` and the trampoline) `7b9e23e3`; E2g (RLIMIT_NPROC refused or reported not enforced for an exempt uid 0 / `CAP_SYS_ADMIN` / `CAP_SYS_RESOURCE` identity unless `pids.max` covers it) `97804cbb`
**Affected crates:** `harw-job-exec`, `harw-job-linux`, `harw-job-core`, `harw-job-runtime`, `harw-job-executor-{bwrap,oci,k8s,darwin}`, `harw-command`, `harw-sandbox`, `harw-tool-shell`, `harw-tool-job`, `harw-agent-runner`, `harw-config`, `harw-install`, `harw-dod-client`, `harw-cli`, `dod/crates/{harw-dod-bpf,harw-probe-bpf}`

**Before:** bwrap, the Landlock trampoline and cgroups were alternative backends; `shell.exec` ran a `prlimit … -- bwrap …` argv as an unsandboxed host job; the probe always attached with host scope.
**Target:** every sandboxed job runs in its cgroup, inside a Bubblewrap namespace, with Landlock, NNP, capability drop and rlimits; DoD observes the job cgroups; one honest readiness report.
**Landed delta:**
- `BwrapTrampoline` backend: `harw-job-exec` joins the job cgroup, execs bwrap; the inner stage verifies `/proc/self/cgroup` (`CgroupJoin::Verify`), applies NNP, capability drop and the Landlock policy mapped through `PathMap`; the job can never rewrite its report. `select_backend` picks BwrapTrampoline, Bwrap, LandlockTrampoline or none and records why.
- Versioned per-job write scope in `JobSpec` (unknown version or out-of-workspace path refused); scoped and `SandboxRequirement::Required` jobs fail closed without an enforcing backend; Landlock hard mode for scoped policies.
- `shell.exec`, `job.start` and child agents submit a `JobSpec` to the chain (child agents receive the parent workspace root but stay unconfined); **`[sandbox] execution` defaults to `chain`, `legacy` is the opt-out** (profile scope; an untrusted project layer cannot set it); the no-config fallback inside the providers stays `Legacy`.
- `resolve_delegated_root`: `[jobs] cgroup_root` → `HARW_CGROUP_ROOT` → own delegated cgroup (never as euid 0) → none with hint; `[jobs] trampoline` override (trusted layers only); `harw-control.service` and the `harw-serve` user unit set `Delegate=yes` and `DelegateSubgroup=harw-main`; `harw-job-exec` ships in every release path and `harw doctor` reports it missing.
- DoD: incremental BPF descendant scope with 2 s rescans; the probe loads its profile before opening a socket and refuses a missing/untrusted/invalid config; `harw dod config suggest [--drop-in]` prints the profile, `[dod] allowed_cgroup_prefixes` and the sentinel/probe drop-ins.
- `harw_dod_client::chain::ExecutionChainReport` (Installed / Configured / Running / Ready) in `harw doctor` (`chain/<stage>` checks, warnings only), `harw dod doctor` (table, `execution_chain` in `--json`) and the verify runner.
- OCI, K8s and Darwin executors never over-report the new resource fields and refuse `require_rlimits`.
**What changed for operators:** jobs and `shell.exec` now run through the chain by default; set `[sandbox] execution = "legacy"` to keep the previous path; start harw under a delegated cgroup (`harw service install`, `harw-control.service`, or `systemd-run --user --scope -p Delegate=yes`) or set `[jobs] cgroup_root`; check with `harw doctor` / `harw dod doctor`.
**Compatibility retained:** jobs without a sandbox requirement and unscoped BestEffort jobs behave as before when no backend is available; old plan JSON and job records parse.
**Tests added/changed:** PL-97 §9.1 rows E1a–E5.
**Architecture gates changed:** `harw-job-exec` stays in `gate_privileges`; no `*-sys` crate in layer J; no new DoD capability; `harw-cli` drops its unused `harw-job-executor-bwrap` dependency.
**Documentation updated:** `docs/design/mediated-process-execution.md`, `docs/design/config-scopes.md`, `docs/setup/install.md`, `docs/setup/dod.md`, `docs/cli.md`.
**Remaining plan items:** a separate rlimit (`process_count`) dimension in `SandboxReport` (E2f/E2g follow-up); child agents stay unconfined on the chain path; `harw serve`/`harw web` do not build their command port from the `[jobs]` options; E4 correlation; kernel-side ancestor matching; TUI chain mark. Note: the `f31d639f` message says `DelegateSubgroup=harw.slice`; the tree uses `harw-main`.

## MIG-020 — Semantic activity and macro metadata (PL-97 M1–M3, PL-71)

**Planning compartment:** `docs/planning/97-harw-dod-mission-control-chain` (§3.5, §4 M1–M3), `docs/planning/71-semantic-activity-patterns`
**Baseline commit:** `dev@fcb12b41`
**Implementation commit / PR:** draft PR #154: M1 `27ea6e10` (fixtures `a7111029`), M2 `47a0720e`, M3 registry side `703d9745`, M3 TUI side `2ce5b2fd`; review fix `ff7ca74a`
**Affected crates:** new `harw-activity` (layer I); `harw-macros`, `harw-tools`, `harw-tool-fs`, `harw-tool-fsread`, `harw-tool-sys`, `harw-registry-defaults`, `harw-tui`

**Before:** PL-71 was planning only; tool cells were grouped by ad-hoc rules in the TUI.
**Target:** a deterministic activity vocabulary and reducer, declared in the tool macros, driving the TUI history without carrying authority.
**Landed delta:**
- `harw-activity`: descriptors, events with a total order, the v1 patterns `file-read-burst@1`, `file-edit-read-loop@1`, `job-lifecycle@1`, `agent-role-failure-burst@1`, `standalone@1`, deterministic reducer with explicit anomalies and bounded retention (`evict_closed`).
- `#[tool(activity(...), skill = "...")]`, `TOOL_ACTIVITIES`/`TOOL_SKILLS`, `permission_request!` (compile-time validated); `ToolSpec` JSON unchanged. First migration wave: `harw-tool-fs` read/edit tools.
- `activity_for(tool)` in the registry; `app/activity_glue.rs` feeds tool, job and agent lifecycles into the reducer (`A · Edit ×1 · Read ×3`, one updating job cell, unknown job ends count as failed); `ToolVerbosity::Activity` salvaged from PR #76 (`Compact` stays the default).
**What changed for operators:** edit/read loops and job lifecycles in the chat history update one cell in place; `/verbose` cycles Compact → Verbose → Activity; failure bursts back the Finished window's `NEW` badge.
**Compatibility retained:** existing `ToolGroupCell` tally lines; activity data feeds no permission, approval or sandbox decision.
**Tests added/changed:** PL-97 §9.1 rows M1–M3 (harw-activity 51 unit + 11 replay tests, harw-macros trybuild 40–47).
**Architecture gates changed:** `harw-activity` in `xtask/arch-policy.toml` (layer I); `harw-macros` stays layer F.
**Documentation updated:** PL-71 status, `docs/design/tui-architecture.md`.
**Remaining plan items:** `permission_request!` call site in the operation framework; further migration waves of tool descriptors (PL-71 §Migration).

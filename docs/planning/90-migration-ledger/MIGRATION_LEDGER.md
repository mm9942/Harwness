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

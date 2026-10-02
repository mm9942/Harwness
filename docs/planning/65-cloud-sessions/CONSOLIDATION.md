# Consolidation branch (`consolidate/main`)

Fixed integration branch for the coding work. Every worker branch is merged here
(no-ff, one merge commit per branch) by the orchestrator, never by workers. `dev` is
not touched without explicit per-PR approval.

## Step contract (runs after every worker wave)

1. **Collect**: fetch worker branches, verify pushed head SHA and clean worktree.
2. **Merge** in dependency order (M1 → M2…M9; skeleton → S01…S10 per DAG).
3. **Synthesis**: resolve overlaps (shared `harw-macros/src/lib.rs`, registration files),
   remove now-redundant duplicates, align naming, update this log.
4. **Gates on a frozen SHA** (any later commit invalidates): `cargo fmt --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
   xtask gates, `cargo deny`, `make -C dod clippy test`, actionlint. Record results below;
   gates that cannot run in the sandbox are listed as NOT RUN, never as passed.
5. **Publish**: push `consolidate/main`; draft PR only on request.

## Log

| Branch | Merged | Head SHA | Notes | Gates |
|--------|--------|----------|-------|-------|
| macro/m6-kebab-snake | yes | c2899e3 | KebabEnum snake_case; WaveJoin::parse now lenient (open question); harw-tui/harw-ops/harw-plan not yet built | NOT RUN (workspace) |
| ws/s01-contract | yes (UNBUILT merged) | c6e5ec5 | W00 §3.5/3.6 contract delta, PlacementGeneration, golden fixtures; fmt+tests passed on branch, clippy unverified; H0–H9 labels do not exist in hub docs (hub uses RS0–RS9) — drop/replace H column in W00-ws-integration-map.md | NOT RUN |
| ws/s07-reconnect-alias | yes (UNBUILT) | see merge commit | backoff, resume cursors, AliasStore; never compiled; expected_node charset unchecked vs NodeId | NOT RUN |
| ws/s02-node-upgrade | yes (UNBUILT merged) | see merge commit | upgrade fns implemented; clippy passed before last small edits, tests never ran; tests use raw I/O not harw-session-ws (needs dev-deps harw-session-ws + futures-util added by orchestrator) | NOT RUN |
| ws/s08-node-listener | yes (UNBUILT) | see merge commit | RegistryIdentityMapper (invented file format node-devices.conf, no workspace device registry exists — needs design decision), UpgradeHandler, HostRevoker; never compiled; serve() depends on S02 stub replaced by S02; extra dev-deps in Cargo.toml/lock; revocation report counts inputs_dropped/contexts_revoked always 0 | NOT RUN |
| ws/s09-attach | yes (UNBUILT) | see merge commit | attach_loop/run_attach, `harw attach`; first compile attempt failed on imports (fixed by hand, unverified); default socket path is a placeholder (daemon not on this base); --host only for unix: aliases; frame read must be cancel-safe in S06 (confirm); no live token deltas | NOT RUN |
| mem/g3-tools | yes | e9dbd10 | memory.recall / memory.record tools (project|global), golden catalog, redaction, never auto-approved; harw-memory dep added; provider NOT registered in any role/registry yet; lib tests 330 pass + the known tunnel.status failure | partial |
| mem/g4-promote-consolidate | yes | 2f9b116 | `/memory promote --to-global`, scope-generic consolidation, Deadline + acquire_until, stale-lock bug fix; 225 harw-memory lib tests pass, clippy clean on harw-memory/harw-ops; 3 harw-ops forget-deadline tests unrun; JOB-SYSTEM REQUIREMENT NOT MET (runs synchronously): needs a `memory_maintenance` job kind handler in harw-cli/src/job_worker.rs + enqueue in harw-runtime (follow-up G5, after G1) | partial |
| mem/g1-wiring | yes | see merge commit | global facts for every entry point via RuntimeAssemblyBuilder, record_usage + confidence sort, fenced untrusted fragments, stronger redact (URL creds/JWT/entropy), decay_with_deadline, startup sweep as ledger job (120 s budget); after merge with G4: clippy -D warnings clean on harw-memory/runtime/ops/cli/registry-defaults, harw-memory 231 lib tests pass; gaps: sweep not cancellable mid-run, on_session_closed still synchronous | partial |
| mem/g5-jobs-config | yes | see merge commit | `memory_maintenance` job kind (harw-ops/src/memory_job.rs, harw-cli/src/job_worker_memory.rs), /memory consolidate|forget|promote enqueue + return job id; startup sweep is a real job; `[memory]` config (memory_toml.rs, ProfileReplaces) + FactLimits caps; gaps: no TimedOut job state (recorded as failed `timed_out:`), runtime claims the sweep job itself in non-serve entries (second claimer), cancel does not interrupt a running job; /memory forget no longer reports missing fact synchronously | workspace clippy -D warnings clean |
| ws/s10-edge-ws | NOT merged | 91c69d7 | pure WS route policy in harw-reverse-proxy/src/ws.rs; never compiled; on #92 base; ceilings in WsLimits::CEILING are the agent's own choice | – |
| ws/s05-daemon | NOT merged | de8e789 | based on #91 (coop/hub-daemon); needs workspace member + arch-policy via stacked skeleton; fmt+clippy clean, tests never ran | – |
| macro/m1-test-support | pending | – | – | – |
| macro/m2-harwerror | yes (merged, not rebuilt together) | see merge commit | `#[source]`/`#[from]` on named fields; 14 enums migrated (net ~160 lines); 3 compile-fail cases parked in tests/compile_fail_pending (need .stderr via TRYBUILD=overwrite); harw-macros ui test did not complete (disk); 12 enums skipped (need format args in #[msg]); ChildRunError reverted (alias clash) | NOT RUN |
| macro/m3-tool-provider | yes (UNBUILT) | see merge commit | conflicts in harw-tool-plan provider.rs and skill_tools.rs resolved by hand (M3 macro form + M4 helpers); needs `cargo check -p harw-tool-plan -p harw-registry-defaults --all-targets` (unused imports likely: JsonSchema, Permission); 11 providers migrated with golden tests; same tunnel.status test failure | NOT RUN (disk full) |
| macro/m4-schema-helpers | yes | see merge commit | schema_helpers + parse_args; net ~47 lines; harw-registry-defaults test `auto_approved_tools_are_a_subset_of_the_read_only_surface` (tunnel.status) fails, cause unverified on clean dev (tunnel code is already on dev) | NOT RUN (workspace) |
| macro/m789-ids-limits-registry | yes (UNBUILT merged) | see merge commit | HarwId validate=, limits_struct!, default_true, diagnostic_code!, register_ops!, catalog parity test; skipped id_newtype!/string_id!/hand-rolled ids; harw-netsec/Cargo.toml conflict resolved (both harw-macros comments); agent claims crate tests passed on branch; Cargo.lock auto-merged | NOT RUN |
| ws/skeleton-dev | yes (UNBUILT) | 61ddf16 | 3 stub crates + upgrade stubs + `harw attach`; Cargo.lock auto-merged with M6 deps, must be re-resolved by a build; `durable_replay.rs` named so because .gitignore has `**/*transcript*`; skeleton-branch checks: fmt, clippy (6 crates), tests (6 crates), xtask arch/edges/privileges/warden-deps/warden-cbuild passed; harw-tui/harw-cli tests not completed (disk) | NOT RUN on merged SHA |
| ws/skeleton-stack | not started | – | – | – |

## Known synthesis hotspots

- `harw-macros/src/lib.rs`: touched by M2, M6, M7 (expect textual conflicts).
- Workspace `Cargo.toml` members and arch-policy: touched by the skeleton only.

## External inputs (other sessions, treated as data)

- Review/research session (ccr-fe134f37-xdezhn): new `harw-tool-container` (policy core, 0 deps, 50 tests, verified against Podman 4.9.3 by that session) plus separate scribe commit 95d7e8e (root Cargo.toml member, xtask/arch-policy.toml layer A, Cargo.lock). Not merged here yet.
- Adding permission `ManageContainers` is NOT a one-line change: harw-authority tests iterate u8 masks (`1 << ALL.len()` overflows at 8 variants), three hard-coded `[Permission; 7]` (harw-core-bridge/agent_tool.rs, harw-runtime/spec.rs, harw-registry-defaults/tests/role_rights_matrix.rs) and the `tool_permission` table must change together.
- `origin/main` Cargo.lock has 129 conflict hunks (0.9.0 vs 0.9.1); dev and consolidate/main are clean. Regenerate the lock on the frozen SHA and verify with `cargo metadata --locked`.
- PR #74 / #76 add `uia-mailbox.md` (63 lines with IP-like values and fingerprint-like strings): must be removed before any merge.
- Review/research session: new crate `harw-session-com` (commit 61fa799, scribe 1d9b614; ComServer::serve_io, tower stack, HostBinder binds identity before 101, 403/503 refusals, PortOffer::All, 20 tests). Overlaps S05 (daemon) and S08 (node listener); NOT merged, needs user decision.
- Findings on #91/S05 from that session (unverified by us): (1) local_identity builds caps only from caps_for_tier, so gateway.* is unreachable for local tiers (gateway_caps_for_tier missing); (2) #91 calls serve_connection (session-only), never serve_connection_with, so tool.* and gateway.* are not dispatched; (3) host.connect runs after the 101 and fails silently (client sees 101 then close without reason); (4) clients must negotiate hello.wire_minor 2, otherwise the host masks R18 caps. Pitfall: hyper 1.11 keep_alive(false) rewrites `Connection: Upgrade` in the 101 to `close`; refusals must use an explicit `Connection: close`.
- PL-68 §14 "IMPLEMENTATION STATUS" is stale (harw-session-ws and harw-session-host exist; #91 delivers W04) — docs follow-up.

## Central test run (workspace, consolidate/main at 174daf2 + fixes)

- `cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_nested_refdefs`: clean (after G5 merge).
- `cargo test --workspace --no-fail-fast`: 14163 passed, 1 failed, plus 1 hang.
  - FAILED `harw-registry-defaults::auto_approved_tools_are_a_subset_of_the_read_only_surface` (`tunnel.status`): cause is a test-surface omission, `AUTO_APPROVED_TOOLS` lists `tunnel.status`/`tunnel.list` but the test's read-only surface did not (commit a953d85 added the entries only). Fixed in the test (read-only tunnel tools added to the surface). It is not caused by any worker branch.
  - HANG `harw-tui attach` tests (S09): test helper `drive` kept the input sender alive, the loop never saw EOF. Fixed (7c633f3); 10 attach tests pass.
- NOT RUN here: `cargo deny` and `actionlint` (not installed), `make -C dod clippy test`, `cargo xtask gates` on the final SHA.

## Decisions applied (user)
- Remote `approve` is a per-device opt-in (PL-68 §13): `DeviceRecord.approve_optin`, registry line gets an optional 7th field `approve` (any other seventh field fails closed); `identity_of` strips `approve` from the tier ceiling unless opted in; session/gateway caps stay by tier. Tests: `remote_approve_needs_the_per_device_opt_in_not_the_tier`, `opt_in_marker_round_trips_and_other_seventh_fields_fail_closed`.
- Merged to consolidate/main (not dev): `harw-session-com` (with `hyper-tungstenite`), `harw-session-daemon` on the com layer, `harw-reverse-proxy` (S10, `WsDecision::Allow` boxed for clippy). Verified: clippy -D warnings clean on the three crates, tests 50 (proxy) + 43 (com) + 5/13 (daemon compose/e2e_uds) pass, `cargo xtask gates` green.

## Composition root (ws/composition-root, merged 12c6948)
`harw gateway --session-socket[=PATH]` (opt-in; path needs `=`): Daemon::start + CoreTurnDriver::with_factory (GatewayCoreFactory, one RuntimeAssembly per hosted session, approval actor `Operator{uid:<euid>}`) + DurableTranscripts/DurableApprovals under `<profile>/session-host/`; default socket `$XDG_RUNTIME_DIR/harw/session.sock` (what `harw attach` expects), 0700 dir / 0600 socket, same-uid only. Verified: clippy -D warnings clean (harw-cli), agent reports tests + xtask gates pass.
Known gaps: no node listener composed (harw-config has no node transport section); needs an active UIA in home config (no startup preflight); `EntryKind::Tui` gives hosted sessions TUI-level shell/net rights (dedicated EntryKind recommended); factory does sync file reads per new session (small jobs-rule break); e2e uses echo model, second-controller approval not tested end to end (driver unit test covers actor binding).

Review follow-up (independent review by the research session, composition root): `with_operator_actor` now fails closed (Err(DriverBridgeError::Runtime)) when the hosted session has no spawn context, with a test. BLOCKER for the node-listener composition step: the same factory must not serve remote ingress (EntryKind::Tui grants TUI shell/net rights) — add a dedicated EntryKind or derive the tier from ClientIdentity first. Async factory trait (spawn_blocking) remains open (low).

## Central gates (code state 2c744f3; later commits are docs-only)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_nested_refdefs` | clean (the allowed lint is the pre-existing `harw-ops` doc lint on dev) |
| `cargo test --workspace --no-fail-fast` | 15,512 passed, 0 failed |
| `cargo xtask gates` | edges 2999, privileges 6, warden-dependency-budget 52, warden-no-c-build 52 (documented blake3/cc exception), arch 2134: all green |
| `make -C dod stage-test` | passed (static packaging checks) |
| NOT RUN here | `cargo deny`, `actionlint`, `make -C dod clippy test` (tools not installed) |

Fixes made during the central run: proc-macro cache (`cargo clean -p harw-macros` once), `tunnel.status` test surface, attach test hang (test helper kept the input sender alive), `WsDecision::Allow` boxed (clippy), `harw-retention` privileges entry, redundant closure, `cargo fmt` on 21 agent-written files, session-host approval actor fail-closed, doctor assembly session-event channel.

## Native build (full personalized harw, role `user-interface`)

- `harw agent check examples/agents/uia-mia`: ok (one note: no `[models]` table).
- `harw agent build examples/agents/uia-mia --native --harw-src <checkout>`: success, first release build 12 min 56 s, binary 105 MB; rebuilt after the doctor fix (cache warm).
- Verified on the binary: `--version`, `--help`, `doctor` (runtime rights printed without warning, retention PASS line), `cleanup` (enqueues a `retention_sweep` job and returns its id).
- Not done: `scripts/native-e2e-uia.sh` (the Harw-flavor binary has the full harw CLI, not the runner's `--verify/--manifest` flags, so the existing script does not apply as is); an interactive chat with a model credential was not tried.

## Retention (R0-R3, wave 3)

`harw-retention` + `retention_classes!` + `[retention]`; telemetry pair pruning, tui.log rotation, bug-report cap, sentinel `--telemetry-max-files`; DoD spool age/bytes limits (opt-in); session/freeze stores (opt-in); `retention_sweep` job kind and `harw cleanup` (dry run by default, async job, deadline, cooperative cancel); `harw doctor` retention checks; `dod/Makefile clean-ephemeral` (dry run by default); `deploy/tmpfiles.d` deliberately sets no age on security directories.
Open: sentinel export rotation still has only the fixed ~32 MiB bound; DoD spool is not yet used by the product (limits apply once wired); `retention_sweep` worker handler is covered by driver unit tests, not by a live worker run.

## Still open (needs the user or a later round)

Context strand and automated learning design (`CONTEXT-AND-LEARNING-DESIGN.md`, decisions in §7); node-listener composition (blocked on a dedicated EntryKind); `harw-tool-container` + `ManageContainers`; any merge to dev/main; `uia-mailbox.md` in PRs #74/#76; `origin/main` Cargo.lock conflict markers.

## Context strand, first slice (e9facc0)

`harw-context-ledger` (layer I; labels, provider, trust, sizes, omission reason; never content; `MemoryLedger`, bounded `FileLedger`), `AgentSession::with_context_ledger`, per-turn `Offered`/`Omitted` entries in `build_request` (`harw-core/src/context_ledger_hook.rs`), config `[memory] context_ledger` (default off, profile-only), runtime opens `<home>/context-ledger`. Verified: clippy -D warnings and xtask gates green; end-to-end turn test (fragment recorded as offered, no content in the ledger); config switch test. Workspace tests re-run after the change: 15,521 passed, 0 failed (a first re-run failed with `no space left on device`, not a test result; space was freed and the run repeated).
Open (design `CONTEXT-AND-LEARNING-DESIGN.md`): X1 `context_sources!`, X3 assembly paths, X4 used signal, X5-X9 learning loop; blocked on the user's decisions in §7.

## harw-tool-container and ManageContainers (user decision: all, including the permission)

- `harw-tool-container` (policy core, 0 dependencies, no I/O; image refs only as name@sha256, hermetic/build profiles, mount validation, hardened rootless-Podman argv builder, readback verification) taken by path from the research session's branch (it verified the argv against Podman 4.9.3); registered as workspace member, arch layer A. 50 tests pass.
- New `Permission::ManageContainers` (harw-authority; never implied by `ExecuteProcess`; no entry profile grants it by default, it needs an explicit policy). The four coordinated changes: `ALL` (8 variants), the exhaustive-algebra test masks widened from `u8` to `u16` (`1 << 8` overflows `u8`), the hard-coded `[Permission; 7]` lists (harw-core-bridge agent_tool tests, harw-runtime spec tests, role_rights_matrix) and the exhaustive match in `harw-core/src/mode.rs`; capability labels `container`/`containers.manage` map to it (`harw-registry-defaults/src/authority.rs`). The container *tool layer* (`container.*` tools, catalog rows) is not built yet.
- Verified: workspace compiles, authority/core-bridge/registry-defaults/runtime/core tests pass, xtask gates green.

## EntryKind::SessionHost (user decision: narrow rights, derived from the identity)

- New `EntryKind::SessionHost` (harw-runtime `spec.rs`): ceiling `{R, W, X}` without network, Full registry, spawner BuiltinRoles, LocalRoot ceiling, project context; approvals are forced to `Delegated` (never automatic, not configurable), tenant `session-host`, budget unbounded-local (same machine, same user).
- `GatewayCoreFactory` (harw-cli `session_serve.rs`) now assembles hosted sessions with `EntryKind::SessionHost` and cuts the sandbox with `RuntimeNarrowing { permissions: permissions_for_tier(tier) }`: observer `{R}`, operator `{R, W}`, maintainer/owner `{R, W, X}`; never network, secrets, plugins or containers. `SessionServeConfig.tier` (default operator, like `local_principal`).
- Behaviour change to be aware of: hosted sessions of an operator no longer get shell (they had `{R, W, X, N}` as `Tui`). A maintainer/owner tier gets shell, still behind delegated approvals.
- Verified: rights matrix over the real assembly (`rights_matrix` 33 tests), `hosted_session_rights_follow_the_connection_tier`, clippy `-D warnings`, xtask gates green.
- Still open for the node listener: it needs a node transport section in `harw-config` (none exists) and a per-connection factory that passes the mapped device's tier; this EntryKind removes the earlier blocker.

## Lernschleife: Feedback-Signal (X2)
`FeedbackTracker` (harw-memory) verbucht delivered/used/corrected je Fakt; Provider meldet Lieferungen, `MemoryCaptureObserver` Nachrichten; Decay demotet nutzlose (>=5 geliefert, 0 genutzt) und schädliche Fakten. clippy, Tests (core/runtime/memory), `xtask gates` grün.

## Lernschleife: X7 Gate, X8 Präzisionsbericht
- X7: `harw-memory::learning_gate` (Injection-Screen verwirft, Größenlimit, Schwärzung, projektlokal = auto, sonst Vorschlag) läuft vor `plan_consolidation` in `run_consolidation`.
- X8: `feedback::report` + `harw doctor` Zeile `check memory.precision` (WARN bei oft gelieferten, nie genutzten Fakten).
- OFFEN (bewusst nicht gebaut): X1 `context_sources!`-Makro, X3 Zusammenlegen der zwei Assembly-Pfade, X5 `OutcomeTracker`/Contradiction-Index (anderes Modell `EpistemicSignal`, kein Mapping auf Fakten), X6 `learning_extract`-Job mit LLM-Extraktion (braucht Provider im Worker), X9 `security_signals`.

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
Sentinel export rotation is now configurable (`--findings-export-keep N`, default 1, max 64; disk bound `(N+1) x 16 MiB`). Open: DoD spool is not yet used by the product (limits apply once wired); `retention_sweep` worker handler is covered by driver unit tests, not by a live worker run.

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
- X1: `harw_context::sources` (`context_sources!`, `macro_rules`): Tabelle der Kontextquellen (Namensraum, Vertrauensobergrenze, Budgetanteil, Standard an/aus); Anbieter beziehen Namensraum/Trust daraus (Test gleicht ab).
- X3: Auslassungsgründe (`over-budget`, `below-ceiling`, `excluded-by-program`, `superseded`, Bestandspfad `over-context-budget`) gehen in `ContextAssembly::omission_reasons` und ins Ledger. Der Bestandspfad bleibt als Fallback ohne Programm/Decke bestehen (er kann Sektion/Vertrauen nicht erfinden, siehe `context_budget.rs`-Moduldoku).
- X5: Werkzeug-Ergebnisse nach Lieferung je Fakt (`outcome_ok/err`, nur Beobachtung, im Doctor-Bericht), Korrektur-Erkennung und Konflikt-Erkennung der Konsolidierung. `OutcomeTracker`/`EpistemicSignal` bleiben ein getrenntes, nicht auf Fakten abgebildetes Modell und sind NICHT verdrahtet.
- X6: `[memory] llm_extraction` (Standard aus): begrenzter, geschwärzter Digest (`_digest/<sitzung>.jsonl`), Job `learning_extract` (async, Frist, Abbruch, atomar je Sitzung), Worker mit Provider-Adapter; Ergebnis nur `_incoming`-Kandidaten durch Gate + Konsolidierung. Die Runtime reiht nur ein; ohne `harw serve`-Worker bleibt der Job `Ready`. Veraltete Digests (>7 d) räumt der Job. Der Dream-Lesepfad ist NICHT zusätzlich geschwärzt.
- X9: `[memory] security_signals` (Standard aus): Anbieter `security.signals` (nur Zähler hoher/kritischer DoD-Befunde der letzten 24 h je Regel, Evidenz).
- Verifikation: Workspace-Clippy, Tests (memory/context/core/runtime/knowledge/config/ops/cli), `xtask gates` grün.

## Lernschleife: X5-Brücke, Dream-Schwärzung
- X5: `harw-memory::fact_signals` bildet Fakten mit entscheidender Rückmeldung (genutzt vs. korrigiert) auf `EpistemicSignal` ab und nutzt nur die Regel `OppositeOutcomes` im selben Themenkorb (Fakt-Typ + erstes Namenswort); Treffer gehen als Konflikt-Hinweise in die Konfliktliste der Konsolidierung (kein Auto-Löschen). `OutcomeTracker` ist über `fact_outcomes::run_outcome_cycle` verdrahtet (Konsolidierungslauf, `outcomes.json`): geliefert → vorgemerkt (14 d), genutzt ohne Korrektur → `Confirmed` (+0.05 Konfidenz, max 0.95), mehr Korrekturen als Nutzungen → `Refuted` (Demotion durch den Verfall), Fristablauf → `Inconclusive`; Doctor zeigt die Zähler.
- Dream-Lesepfad: `build_recent_dream_context` schwärzt Transkripttext (`facts::redact`) vor dem Modell.
- Platzproblem: Scratch-Worktrees `/tmp/claude-0/wt70`, `wtdev` verloren ihr `target/`.

- Doc tests: `cargo test --workspace --doc --no-fail-fast` 1571 passed / 0 failed; four pre-existing broken `harw-cli` doc examples (private module paths in `runtime_web`, `uia_bootstrap`) marked `ignore`.

## Node-listener composition (remote ingress)
`harw-cli/src/session_serve_remote.rs`: `start_remote` composes node transport + `harw-node-listener` on a TCP listener with its **own host and state** (`<profile>/session-host/remote`), `GatewayCoreFactory` cut to `RemoteServeConfig::tier` (default observer) and `RegistryIdentityMapper` (device registry). Approvals of remote sessions bind to one configured device (`approval_device`, also needs the registry `approve` opt-in); otherwise they park (fail closed). Verified over a real loopback socket (enrolled device lists sessions, revocation refuses the next handshake, unenrolled pinned node refused). Now wired into `harw gateway` (see below).

## container.* tool layer
- `harw-tool-container-run` (new): `ContainerEngine`/`PodmanEngine` (cleared engine environment, bounded stdout/stderr 64 KiB each, wall-time limit, cancellation, **runtime inspect read-back**: a `NotEnforced` property kills the container at once, unreported properties are `Unverifiable` and reported; a container that exits before the first inspect is reported as *not verified*), `parse_inspect` (Podman JSON, present `null` = empty set), tools `container.images` (read-only) and `container.run` (alias-only image, fixed profile limits, no mounts/network/caps from the call). Engine tested against a fake `podman` script, tools against a fake engine.
- Config `[tools.container]` (`enabled`=false, `engine`, `connection`, `images=["alias=name@sha256:..."]`): **global-only** (home layer; profile/project layers are rejected and tested), 4 new scope-table rows.
- Registration: `harw-runtime::container_wiring` when enabled and the entry's sandbox carries `ManageContainers`. Decision: **only `EntryKind::Tui` carries `ManageContainers`** (interactive, can ask); all other entries, tiers and `SessionHost` do not. `container.run` is in `ALWAYS_ASK_TOOLS` and never auto-approved; both tools map to `ManageContainers` in `tool_permission`; catalog rows + `tool-container` feature (off by default in the runner, which does not link them).
- Not done: approval dialog text from `ContainerPlan::approval_text` (the generic tool-call approval is used), image pulling/signature policy, Docker, the job-executor container system of `docs/planning/67-containers` (separate plan).

## Remote ingress wired into `harw gateway`
`[session_listener]` (global-only, default off; 6 scope rows): `enabled`, `listen` (default `127.0.0.1:7443`, non-loopback needs `allow_non_loopback`), `node_id`, `tier` (default `observer`), `approval_device`. `harw gateway` starts it via `harw-cli/src/session_listener.rs`: node key = AuthHub key `harw.node-identity/<node_id>` through `HubSign` (`AuthHubClient::sign`) + `AuthHubNodeSigner` with self-check against the hub's public key; peers verified by `TranscriptWrappedVerifier` over pinned keys from `<profile>/session-host/remote/node-peers.conf` (`node_id|public_key_hex`); authorisation from `node-devices.conf` in the same directory (operator-written; marking a record `revoked` is **live revocation**: the gateway scans the file every 5 s, cuts the device's authority in the host and closes its open connections via `HostRevoker`, and the next handshake is refused; no network control endpoint is involved). Start failure ends the gateway. Verified: config validation, peers parsing, loopback with a wrapped fleet (pinned + enrolled but unwrapped key is refused). NOT verified against a real AuthHub (no hub in this sandbox).

## Container job system (docs/planning/67-containers, C1–C6)
Reviewed against the code before building (read-only Plan agent); the review found 8 README/code mismatches, the ones that changed the design are listed here.
- **C1 `harw-container-model` (I):** `ImageDigest` (tag refused; doc names `RepoDigests`, the manifest digest, not the config digest an engine reports as image id), `ContainerId`, `ContainerInstance`, `PodInstance` (**validated on deserialize** after the review found a fail-open path), `OwnerLabels` (round trip, label-safe charset), forward-only `Lifecycle`.
- **C2 `harw-job-executor-oci` (J, `harw-job` feature `oci`):** `Executor` over a Docker-compatible engine socket with a small blocking HTTP/1.1 client (no async runtime needed by the sync `Executor::start`); socket owner checked with `SO_PEERCRED` (`PeerPolicy::SameUser` default, `Uid(0)` = explicit rootful opt-in); image must be present locally at the pinned manifest digest (nothing is pulled); create is locked down (`CapDrop ALL`, `no-new-privileges`, read-only root, tmpfs scratch, no host mounts, no restart, never privileged); **inspect read-back before start** is mapped to `SandboxReport` and an unmet `Required` removes the container without a start call; missing inspect fields are never `Enforced`. Recovery re-verifies id, creation time, image digest, labels, owner and tenant; `reattach` also checks job and lease epoch. `AttemptContext` gained `lease_epoch` (coordinated change in `harw-job-runtime`) because the labels need it. 15+ tests against a fake engine on a Unix socket (happy path, dropped limit, forged labels, missing image, other-user socket, oversized body, probe/reattach, recorded exit, cancel idempotence).
- **C3 (same crate):** `plan_gc` is pure (listed containers + `AttemptView` → removals): labels are unauthenticated, so only fully parsing labels with our owner and tenant are ever touched; younger than the grace is kept; a **higher epoch than the store knows is kept**; a live attempt of the current epoch is kept; stale epochs go even while running. `WarmPool` is deliberately **not** a pool of pre-created containers (engines fix labels, mounts and limits at create time, so a warm container would carry another attempt's identity): it holds verified image slots keyed by tenant, digest and profile, single use, idle TTL, capacity. There is no API that looks across keys.
- **C4 `harw-dod-container` (D):** new `Capability::ReadContainerScopes` (also in the `harw-macros` list and its compile-fail snapshot). Reads scope directories (`libpod-/docker-/crio-<64 hex>.scope`), only for **expected** ids; any other scope is reported as `container_unowned` and nothing more is read. Per expected scope: effective capability count (`container_privileged` from 32 bits), seccomp mode, `NoNewPrivs`; an unreadable pid or a status without the fields is `container_unverifiable`, never clean; at most 64 scopes, cut flagged. Not covered: host network namespace (needs `readlink`, which the read seam does not offer; the executor's `NetworkMode` read-back covers it). Own fixture tests (the shared `sensor_suite!` was not used: it needs fixture trees and an eight-check contract this sensor was not adapted to).
- **C5 `harw-job-executor-k8s` (A):** one restricted pod per attempt, `reqwest` (rustls, same stack as `harw-oauth`) on its own thread per call. Pods are created behind a **scheduling gate**; the admitted pod (webhook changes included) is read back, mapped to `SandboxReport`, and the gate is lifted by a JSON patch only afterwards (needs Kubernetes ≥ 1.30). Refused: removed gate, other image, lost labels, mounted service-account token, more or fewer than one container, hostPath volumes. Identity is the immutable pod UID (pod name = container name, stored in `PodInstance::container`). Honest limits: network is at most `Partial` unless the operator attests the namespace NetworkPolicy (`network_attested`); CPU weight and pids are not expressible per pod (`Partial`); logs do not separate stderr. A bug found by the tests: with an attestation the network was `Enforced` even when `hostNetwork` was not reported.
- **C6 `harw-placement-model` (I, P1 minimum):** `Enforcement`, `DimensionStates`, `RuntimeBackend`, `RuntimeOffer` (observation time + TTL, a clock that went backwards is not fresh), `NodeOffer` (only `Active` and recently seen). Rootful backends (`oci-podman-rootful`, `oci-docker`) are never eligible without explicit opt-in. Producers: `harw_job_runtime::offer::runtime_offer` (report → offer) and `probe_offer` in the OCI and K8s executors, which create a **canary** (never started / gate never lifted), read it back, delete it, and publish what was really applied (a dropped limit shows up; unreachable engine or API → no offer). Requirements, grants, filter and score (P2+) are not built.
- Verification: clippy `-D warnings` on all touched crates, crate tests (oci 27, k8s 18, container-model 11, placement-model 4, dod-container 7, dod-cap 64, job-runtime 70), `xtask gates` (edges, privileges, arch) green.
- NOT done: a run against a real Podman, Docker or Kubernetes (only fakes in this sandbox), wiring the executors into `harw serve` or the coordinator, a `[job.container]` config section, image pulling and signature policy, gVisor/Kata `RuntimeClass` offers, workspace bind mounts (jobs get a tmpfs `/work`).

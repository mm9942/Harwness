# Harwness modular architecture and state-ownership refactoring

**Status:** proposal + wave 1 structural implementation; compiler/test acceptance pending.
**Baseline:** PR #172 `6bd6b984adcc5d941561d1b55d4206321f36f2d3` (PR #173 is a separate Android backup branch).
**Implementation:** `378fd899964f0715878622c6e07093cf98a2c2c0`.
**Inventory:** [all 180 crates, purpose, dependency adjacency, source shape and priority](crate-inventory.md).

## Findings and intent

4,363 Git blobs; 2,480 Rust files / 56,878,878 bytes; 2,179 Rust files in member src, 1,941 flat and 238 nested; 150/180 members have no nested Rust source; 19 member mod.rs files across 15 crates. 72 Rust files >=100 KB including tests. Counts refer to baseline and include tests/comments; they are not production SLOC. The crate-level architecture is mature, but internal module hierarchy and state ownership have not grown equally. Do not create another crate by default. First establish coherent internal subsystems with directories and mod.rs, private state, narrow interfaces, deterministic composition and explicit authority. Preserve existing architecture policy and coding philosophy.

## Rules

- One domain owner per crate, one coherent subsystem per module directory, one owner per state invariant, one purpose per operation. Pure functions and enum matches are valid; no artificial class hierarchy or trait per struct.
- Larger modules use foo/mod.rs as a facade with private siblings. lib.rs and main.rs remain public/composition roots. Prefer structs + impl, typed state machines, RAII, composition and dependency inversion. Traits are actual ports/alternative backends; generics/enums where appropriate. Keep fields private; limit pub(crate), pub(super), pub(in ...).
- Authority, scopes, budgets, cancellation, persistence/fencing and enforcement stay with their authoritative owner. A facade must not hide a forbidden dependency or duplicate decisions.
- Mechanical extraction commits preserve public paths via re-exports, avoid new dependencies and changes to wire/schema, policy or runtime behavior. Move tests without changing assertions; later split by invariants.
- File length is a review signal: proposed warning at 800 non-test lines, deliberate split review at 1500, tracked exceptions for generated/schema/table/algorithm files. Measure test and production code separately. Implement baseline/non-growth gate rather than failing every historical file.

## Wave 1 (initial implementation)

| Old module | Old lines incl. tests | New main mod.rs lines | Extracted groups |
|---|---:|---:|---|
| harw-tui/src/app.rs | 19,734 | 6,830 | input, terminal, turn_events, key_routing, viewport, error, composition, session_overlays, permission_state, tool_history, monitoring, conversation_state, agent_event_state, state_access, host_mode, history_access, composer, tests |
| harw-core/src/turn_loop.rs | 17,161 | 5,471 | control, api, delegation, round_context, persistence, tests |

Preserve public re-exports, keep TurnControl meter and budget fields private with internal operations, retain shared clone semantics. The other extracted groups use transitional parent imports and pub(super) visibility; replace with narrow dependencies and real state owners in later waves. ChatApp is still a broad state owner; RoundBudget fields are visible within turn_loop; large test modules and both main drivers still require decomposition. Moving functions changes implicit tracing module targets; check exact target filters. No Cargo dependency changes. No Rust toolchain/private checkout in this environment: fmt, Clippy, compiler, doctests, feature matrix and tests remain unverified. Draft only.

### TUI ownership target

ChatApp composes ConversationUi (history, streaming, tool cells), InteractionUi (composer/commands/shortcuts), TurnUi (queue/cancellation), ApprovalUi (tool/child/sudo/host permit projection), MonitoringUi (agents/jobs/mission), SessionUi (export/attachments/switching), ViewportRenderer, TerminalGuard. UI is projection, not execution authority. HistoryCell already has a trait; move concrete families to own modules. Keep event order, approval arming, localization, scroll/render parity, background updates, terminal restoration and key mappings.

### Core ownership target

Turn engine coordinates typed input/outcome, control, context/skills/compaction, model invocation, tool admission/approval, serial/parallel execution, delegation/background, persistence and resume. Keep meter admission before side effects, eligibility before parallel approvals, DoD taint sequence, transcript call/result pairing, cancellation, title-seed timing and durable approval/recovery. Do not introduce a second authority path. Extract remaining engine/entry/resume/dispatch and tests by invariant.

## Further P0/P1 hotspots

| Component | Target ownership and invariant |
|---|---|
| core/child_controller.rs | ChildAdmission, Registry, Lifecycle, BudgetLedger, LeaseRecovery, ContinuationLedger, Execution, result archive. One atomic admission/lease owner. |
| runtime/assembly.rs | Staged config/trust, authority/ceiling, providers, stores, registry, children, session lifecycle. One composition root for all entrypoints. |
| provider-http/lib.rs | Credentials/auth, transport, provider codecs, routing, replay per identity, budgets/rate/concurrency, response stream. No cross-session replay or secrets leakage. |
| registry-defaults/profile.rs | Profile policy vs role resolution vs tool catalog vs provider assembly. One visibility/authority decision. |
| cli/job_worker.rs + job_worker_work_driver.rs | Worker supervisor, claim/lease, accounting, job handlers, WorkDriver/Cycle ports; execution logic not CLI presentation. |
| runtime/children.rs | Role, identity, route, narrowing, skills/context and registry assembly; pending identity binding lifecycle. |
| core-bridge/agent_tool.rs | Parse, validate budgets/return contracts, dispatch, fanout, projection; never duplicate ChildController authority. |
| ops/dod.rs + provider.rs | Command parsing, application services, query/filter, rendering, admin plans; privileged enforcement elsewhere. |
| session-host/host.rs | Identity/grants, session slot, turn arbiter, replay/attach, admin and owner placement. Preserve lock/fence boundaries. |
| tool-shell/exec.rs | authorize -> prepare -> execute -> capture; host permit separate from backend; never implicit host fallback. |
| session-store/dod_verdict.rs | Canonical record, chain verification, retention, segments and anchors; no unsigned data promoted to authority. |
| sentinel/main.rs | Startup, profile/scope, probe IPC, polling, export, health. Unprivileged aggregation, bounded input. |
| dod-warden/warden.rs | Verification, policy, action transaction, hold/recovery and backend. Replay/fail-closed and kernel identity invariants. |
| tool-job/manager.rs + job-runtime/coordinator/linux.rs | One job lifecycle/fenced store; process-local handles distinct from durable state. Converge semantics, not blind moves. |
| config/merge.rs + discovery.rs | Trust-aware domain merge, provenance/validation; lower trust cannot loosen policy. |
| authority/lib.rs + protocol/session_wire.rs | Permissions/scopes/workspace/snapshot, wire families/version gates. High fan-in and API compatibility. |
| agent-dsl/executable.rs + plan-bridge/cycle_runtime.rs | Typed IR; fenced cycle journal, proposer/executor/verification/recovery ports. |
| tui/history_cell.rs + agent_monitor.rs | Concrete cell families; monitor reduction separate from panel projection. |
| mobile/rust/src/core.rs + Android SessionConversation.kt / HarwConnection.kt | Stable FFI; connectivity, session projection, credentials, inference, VPN, conversation state and render. Real-device tests. |

## Roadmap and acceptance

W0 baseline and audit; W1 TUI+turn_loop initial extraction; W2 remaining engine, UI state owners, child controller, history cells and runtime assembly; W3 authority/config/protocol/extension contracts, providers/registry; W4 job/CLI workers/plan bridge/DSL; W5 DoD sentinel/Warden/verdict store/dispatch with TCB review; W6 session host/cloud/mobile/attachments/extensions; W7 remaining P1/P2 and non-growth gate. P3 small cohesive members need no forced split. Freeze lower contracts before dependent work, parallelize only disjoint write scopes, integrate at evidence barriers.

Acceptance: `cargo fmt --all -- --check`, `cargo clippy --workspace --tests -- -D warnings`, `cargo check --locked -p harw-core -p harw-tui`, `cargo test --locked -p harw-core -p harw-tui`, `cargo test --workspace --doc`, normal CI target/feature matrix and `cargo run -p xtask -- gates`. Verify public paths/test names/counts, no Cargo.lock changes, path-sensitive tools and exact tracing filters. Regression cases: pause/resume, tool approvals, cancellation, context overflow, budgets/handoff, history rewrite/title seed, DoD taint; TUI snapshots/locale, hotkeys, permits, jobs, export, session switch and terminal recovery. Record actual evidence, no auto-merge.

## Complete >=100 KB Rust file inventory (baseline, including tests)

| Rank | File | KB |
|---:|---|---:|
| 1 | `harw-tui/src/app.rs` | 849.5 |
| 2 | `harw-core/src/turn_loop.rs` | 714.2 |
| 3 | `harw-core/src/child_controller.rs` | 683.3 |
| 4 | `harw-runtime/src/assembly.rs` | 514.6 |
| 5 | `harw-provider-http/src/lib.rs` | 384.8 |
| 6 | `harw-registry-defaults/src/profile.rs` | 355.7 |
| 7 | `harw-cli/src/job_worker.rs` | 340.2 |
| 8 | `harw-runtime/src/children.rs` | 333.2 |
| 9 | `harw-core-bridge/src/agent_tool.rs` | 303.6 |
| 10 | `harw-ops/src/dod.rs` | 285.6 |
| 11 | `harw-cli/src/job_worker_work_driver.rs` | 283.8 |
| 12 | `harw-tui/src/history_cell.rs` | 251.3 |
| 13 | `harw-tool-shell/src/exec.rs` | 250.0 |
| 14 | `harw-session-host/src/host.rs` | 236.4 |
| 15 | `harw-cli/src/lib.rs` | 235.6 |
| 16 | `harw-session-store/src/dod_verdict.rs` | 230.9 |
| 17 | `harw-dod/crates/harw-sentinel/src/main.rs` | 216.1 |
| 18 | `harw-ops/src/provider.rs` | 201.8 |
| 19 | `harw-tool-job/src/manager.rs` | 195.9 |
| 20 | `harw-dod/crates/harw-dod-warden/src/warden.rs` | 191.3 |
| 21 | `harw-registry-defaults/src/embedded_agents.rs` | 190.8 |
| 22 | `harw-job-runtime/src/coordinator/linux.rs` | 176.7 |
| 23 | `harw-core/src/context_budget.rs` | 175.3 |
| 24 | `harw-tui/src/agent_monitor.rs` | 169.7 |
| 25 | `harw-cli/src/gateway.rs` | 161.0 |
| 26 | `harw-plan-bridge/src/cycle_runtime.rs` | 155.6 |
| 27 | `harw-config/src/merge.rs` | 155.3 |
| 28 | `harw-config/src/discovery.rs` | 154.9 |
| 29 | `harw-registry-defaults/src/agent_definition_tools.rs` | 150.1 |
| 30 | `harw-tool-job/src/manager/suspend_tests.rs` | 148.5 |
| 31 | `harw-ops/src/matrix/runner.rs` | 145.8 |
| 32 | `harw-provider-http/src/anthropic.rs` | 143.6 |
| 33 | `harw-cli/src/dod_cmd.rs` | 142.0 |
| 34 | `harw-core/src/session.rs` | 140.3 |
| 35 | `harw-ops/src/plan.rs` | 139.7 |
| 36 | `harw-matrix-game/src/phases.rs` | 134.0 |
| 37 | `harw-agent-dsl/src/executable.rs` | 132.3 |
| 38 | `harw-ops/src/skills.rs` | 130.6 |
| 39 | `harw-plan-bridge/src/work_driver.rs` | 130.3 |
| 40 | `mobile/rust/src/core.rs` | 130.1 |
| 41 | `xtask/src/gate_operator_decision.rs` | 129.4 |
| 42 | `harw-ops/src/analyze.rs` | 129.1 |
| 43 | `xtask/src/gate_warden.rs` | 124.4 |
| 44 | `harw-sandbox/src/host_permit_session.rs` | 123.5 |
| 45 | `harw-runtime/src/auto_classifier.rs` | 123.2 |
| 46 | `harw-ops/src/models.rs` | 122.4 |
| 47 | `harw-cli/src/chat.rs` | 121.6 |
| 48 | `harw-core/src/compaction.rs` | 121.5 |
| 49 | `harw-ops/src/work_driver.rs` | 121.3 |
| 50 | `harw-core/src/skill_injection.rs` | 119.6 |
| 51 | `harw-tui/src/command_exec.rs` | 119.1 |
| 52 | `harw-plan/src/file_store.rs` | 118.1 |
| 53 | `harw-registry-defaults/src/lib.rs` | 115.7 |
| 54 | `harw-protocol/src/session_wire.rs` | 112.6 |
| 55 | `harw-tui/src/approval.rs` | 112.0 |
| 56 | `harw-sandbox/src/bwrap.rs` | 111.2 |
| 57 | `harw-cli/src/session_serve_remote.rs` | 110.7 |
| 58 | `harw-operations/src/operation.rs` | 110.3 |
| 59 | `harw-runtime/src/services.rs` | 109.2 |
| 60 | `harw-plan/src/validate.rs` | 109.0 |
| 61 | `harw-cli/src/cli/tests.rs` | 108.8 |
| 62 | `harw-session-store/src/job_store.rs` | 108.2 |
| 63 | `harw-dod/crates/harw-sentinel/src/findings.rs` | 108.0 |
| 64 | `harw-tui/src/input_editor.rs` | 107.8 |
| 65 | `harw-dod/crates/harw-sentinel/src/export.rs` | 105.6 |
| 66 | `harw-tui/src/knowledge_view.rs` | 105.1 |
| 67 | `harw-plan-bridge/src/job_bridge.rs` | 105.0 |
| 68 | `harw-secrets/src/store.rs` | 104.6 |
| 69 | `harw-tool-shell/src/sudo.rs` | 103.8 |
| 70 | `harw-dod/crates/harw-probe-bpf/src/main.rs` | 103.4 |
| 71 | `harw-registry-defaults/src/skill_proposal_tools.rs` | 103.1 |
| 72 | `harw-core-bridge/src/delegate_wave.rs` | 102.8 |

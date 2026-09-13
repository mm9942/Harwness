# Changelog

All notable changes to this workspace are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/) and this project uses
Semantic Versioning within the 0.x pre-release range.

## [Unreleased]

### Security

- `harw-agent-dsl::roles::can_spawn` (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.
- **G11 — `deny_unknown_fields` audit**: Added `#[serde(deny_unknown_fields)]`
  to 17 container-level structs across `harw-config` and `harw-protocol` that
  deserialize from untrusted TOML configs or wire payloads. Unknown fields are
  now rejected at deserialization, preventing silent authority injection via
  surplus keys. Identified via bottom-up codebase review inspired by
  `hardening-suggestive-inspiration.md` §11.
- **G12 — Remove redundant `unsafe impl Send/Sync`**: `ShortTermMemory` in
  `harw-memory/src/short_term.rs` had manual `unsafe impl Send` and `unsafe
  impl Sync` blocks that were redundant — `RwLock<Inner>` with all-`Send`
  fields is automatically `Send + Sync`. Removed both blocks, eliminating a
  soundness risk surface.
- **Hardening gap analysis**: `docs/design/hardening-gap-analysis.md` —
  comprehensive 14-gap analysis from bottom-up codebase review against
  `hardening-suggestive-inspiration.md` (32 sections). (the closed UIA/Root/Child/Worker spawn
  matrix, §3 of the DSL spec) was documented as "enforced by the runtime" but
  was never actually called anywhere outside `harw-agent-dsl` itself —
  `harw-core` had no dependency on `harw-agent-dsl` at all, so
  `ManagedAgentSpawner::admit` never checked whether a spawning session's
  organizational role was permitted to create the target role (e.g. a
  `Worker`, which must never create durable children, was not prevented from
  doing so by the runtime). `SpawnContext` gained an `organizational_role:
  AgentRoleId` field, `ChildRoleDefinition`/`ManagedAgentSpawner::with_role`
  now carry the target organizational role, and `admit()` rejects the spawn
  fail-closed via `can_spawn` before any sandbox/depth/lease check. Covered
  by a new `harw-core` integration test. `ManagedAgentSpawner` is not yet
  wired into `harw-cli`/`harw-tui` production paths, so this closes a latent
  gap in the library ahead of that wiring rather than a currently reachable
  vulnerability. Identified via source-grounded review inspired by
  `hardening-suggestive-inspiration.md`.

### TUI Hardening

- **Grapheme cluster cursor movement**: InputEditor cursor movement
  (move_left, move_right, backspace, delete, word-jump) now operates on
  Unicode extended grapheme cluster boundaries via unicode_segmentation,
  not char boundaries. This fixes cursor corruption with combining diacritics
  (e.g. German umlauts encoded as base + combining mark, ZWJ emoji sequences).
  Backspace and forward-delete now drain the entire grapheme cluster range,
  not a single byte. Inspired by codex-rs TextArea grapheme-aware movement.

- **Display-width-aware wrapping**: visible_lines and cursor_position
  now use unicode_width::UnicodeWidthStr for column math instead of char
  count. CJK/wide glyphs (display width 2) are correctly accounted for in
  soft-wrap breakpoints and cursor column calculation. Fixes misalignment
  with wide-character text.

- **History recall boundary gate**: Up/Down keys now only trigger history
  recall when the buffer is empty or the cursor is at position 0/len AND the
  current text matches the last-recalled entry. A last_recalled field tracks
  the most recently loaded history entry. This prevents accidental history
  navigation when the cursor is mid-text in a single-line draft.

- **Paste normalization**: TuiEvent::Paste handler now normalizes CRLF
  to LF and CR to LF before inserting. Prevents stray carriage returns
  from corrupting multi-line pasted text.

- **Dynamic composer height**: The input box height now grows with the actual
  wrapped line count (visible_lines(width)) instead of counting only hard
  newlines. Max height raised from 6 to 10 rows.

- **Semantic border color**: The input box border now uses
  style::border_color(theme), applying the semantic palette (RGB values
  for dark/light themes) to the block border. Fixes the dead_code warning
  for the previously unused border_color function.

## [0.2.0] — 2026-07-16

### Added

**Operation Registry & Command Surface**
- `OperationMeta.aliases` flow through `CommandRegistry::from_operation_registry`;
  `/m`, `/p`, `/reasoning` now dispatch to the same handlers as `/model`,
  `/provider`, `/effort`.
- `OperationRegistry::try_register` returns `Err(RegistryError::DuplicateName |
  AliasCollision | SelfCollision)` on name/alias conflicts at registration time.
- Multi-segment `Surface::Command { path }` values are accepted (previously
  silently skipped).
- `FromRawArgs` derive gained strict compile-time checks: field type must be
  `Option<String>` for positional attrs, conflicting `#[raw(...)]` attrs on one
  field are an error, missing `#[raw(...)]` on a named field is an error, tuple
  and unit structs are rejected, multiple `#[raw(required)]` fields are rejected,
  unknown raw keys are rejected, `nth = 0` is rejected with a friendly message.
  Seven trybuild compile-fail cases enforce all diagnostics.

**Long-lived Session Controller**
- `ChatApp` holds `Arc<TuiSessionController>` as a persistent field.
- `command_exec::build_services()` accepts the controller Arc as a parameter.
- `apply_pending_controller_state` hook fires at the safe turn boundary (before
  `run_turn_streaming` starts), flushing queued mutations onto `AgentSession`.
- `TuiSessionController::apply_to_session` propagates `reasoning_effort`,
  `active_model`, and `active_provider` onto the `AgentSession`.

**Real /model, /provider, /effort routing**
- `AgentSession` gained `active_model: Option<ModelId>` and
  `active_provider: Option<ProviderId>` fields with accessors and setters,
  mirroring `reasoning_effort`.
- `ModelRequest` gained `model_id` and `provider_id` fields; `turn_loop.rs`
  plumbs both from the session into the next request.
- `/provider switch <id>` validates provider existence, credential resolvability,
  and active-model compatibility before mutating. Atomic fail-close on any failure.
- `/model switch <id>` validates via `harw_model_catalog::resolve` and
  cross-checks against the active provider. Atomic fail-close on incompatibility.
- `/provider show` reports the actual runtime provider; `/provider list` marks
  active, auth-ok, auth-missing states.
- `/model show` reports the actual runtime model; `/model list` filters by active
  provider and marks the current selection.
- `ReasoningEffort::Minimal` maps to `None` on the Anthropic adapter (omits
  `output_config.effort`; reasoning stays adaptive).

**HARW SDK Facade**
- New crate `harw` at the workspace root re-exports canonical types from nine
  crates under modules: `extension`, `ops`, `agent`, `provider`, `model`, `core`,
  `defaults`, `types`. `harw::prelude` re-exports the load-bearing types.
- `harw/examples/minimal.rs` provides a runnable example.

**Agent DSL / IR**
- `ExecutableAgentIr` gained `snapshot_id: SnapshotId` computed via BLAKE3 over a
  stable byte stream; deterministic across processes, excludes `trace` (which
  carries timestamps).
- `DslError` variants `MissingBase`, `MissingMixin`, `AuthorityElevation`,
  `IllegalRoleForMixin` carry `DiagLocation { layer, field_path }`. Display now
  includes the field path (e.g. `"authority.capabilities"` on `AuthorityElevation`).

**Type Consolidation**
- `harw-types` newtypes (`ProviderId`, `ModelId`, `ProviderName`, `ModelName`,
  `AgentName`, `CustomerId`) gained `Deref<Target = str>`, `AsRef<str>`,
  `Borrow<str>`, bidirectional `PartialEq<str/&str/String>`, `PartialOrd`, `Ord`.

**Testing Infrastructure**
- `harw-core::testing::RecordingModelProvider` records every `ModelRequest` for
  integration tests.
- New integration tests: `harw-agent-dsl/tests/toml_to_ir_e2e.rs` (5 tests),
  `harw/tests/sdk_example.rs` (1 test), plus expanded unit tests in all touched
  modules.
- E2E suite §7 slices 1–4, 12, 13 shipped; slices 5–11 (real-turn recording) are
  in flight and not gated for this pre-release.

### Changed

- `CommandRegistry::from_operation_registry` now returns
  `Result<Self, TuiRegistryError>`; call sites must propagate or handle the error.
- The dispatch-adapter list in `harw-tui/src/command_exec.rs` no longer maintains
  a parallel alias truth source; the executor consults `OperationMeta::aliases`
  directly.
- `register_all_second_pass_rejects_duplicates` replaces the old
  `register_all_is_additive` test; additive registration is no longer permitted.
- `/model` and `/provider` unknown subcommands now return `InvalidArguments`
  listing the supported set instead of silently falling back to `show`.
- `harw-model-catalog` migrated from `pub type ProviderId = String` to
  `pub use harw_types::{ProviderId, ModelId}` (199 mechanical substitutions across
  12 files; no runtime behaviour change).
- `AgentArgs / SkillsArgs / PluginsArgs`: the single `cmd: Option<String>` field
  is replaced by `action / target / value` fields.

### Fixed

- `AgentArgs / SkillsArgs / PluginsArgs` argument-tokenization bug that discarded
  the second token in `/agent stop <id>`.
- `FromRawArgs` permissive derive accepted invalid field configurations silently;
  now a compile-time error.
- Fresh-per-command `TuiSessionController` construction caused state resets on
  every command; the controller is now long-lived on `ChatApp`.
- Silent snapshot writes for `/model` and `/provider` when no actual switch
  occurred.
- `run_loop` in `harw-tui` no longer hard-crashes the process when a chat
  turn fails (e.g. an invalid/expired provider credential returning HTTP
  401): `TuiError::Core` is now caught, shown as a readable system line in
  the chat transcript, and the session stays alive; only `TuiError::Io`
  (fatal terminal failures) still exits and ends the process.
- `harw-ops::provider::auth_status_label` had an unreachable wildcard match
  arm that only surfaced under the `harw-provider` crate's default (non
  `chatgpt-oauth`) feature set, breaking `cargo clippy -- -D warnings` on
  ordinary builds; replaced with an explicit `#[cfg(feature =
  "chatgpt-oauth")]`-gated arm, and `harw-ops` now forwards a matching
  `chatgpt-oauth` feature to `harw-provider`.
- `harw` SDK facade example test extended (`sdk_example_toml_to_executable_runtime_context`)
  to prove the full pipeline — TOML parse → `resolve_definition` → `lower`
  into `ExecutableAgentIr` → `assemble_default_registry` →
  `AgentSession::new` — is expressible using only the public `harw::`
  facade, closing the previously-partial coverage of E2E test item #12
  (SDK example must build a registry, compile an agent definition, AND
  produce an executable runtime context).

### Deprecated

Nothing formally deprecated in this 0.x pre-release cycle.

### Known Unstable

The following areas are intentionally out of scope for this 0.2.0 milestone and carry no
stability guarantee:

- `Harness::builder()` fluent API — planned for a later 0.2.x pre-release.
- Typed Parent-to-Child return pipeline (Agent-as-Tool) — child returns a string
  today; fachliche typed return is deferred.
- Full IR-to-Runtime consumption — `ExecutableAgentIr` exists but the runtime
  still consumes `AgentRole` from `harw-types` directly, not the IR.
- `GoalGraph` / `FinalObjective` / `GoalEvaluator` — architecture only, not
  implemented runtime.
- `DurableJobRunner` has no CLI runtime caller yet.
- Memory system Cognition Loop is not fully wired end-to-end.

### Migration

See [docs/migration/0.1.0-to-0.2.0.md](docs/migration/0.1.0-to-0.2.0.md).

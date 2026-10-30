# TUI Settings and Localization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Execute sequential implementation tasks with independent task reviews.

**Goal:** Default the TUI to English and provide discoverable, persistent multilingual user settings and existing governed model controls through `/settings`.

**Architecture:** Profile-scoped typed language config and curated settings operations feed a native OverlayView. Stable message catalogs localize TUI-owned display text using session-local locale context, with English fallback and no mutation of user/model content.

**Tech Stack:** Rust 1.98.1 workspace (MSRV 1.89), Ratatui/Crossterm, existing harw-config/harw-operations/harw-ops, serde JSON catalogs.

**Spec:** `docs/superpowers/specs/2026-10-29-tui-settings-localization-design.md`

## Global Constraints

- Supported language codes: `en`, `de`, `es`, `fr`, `pt`, `ja`, `zh-CN`, `ko`, `vi`, `hi`; default `en`.
- English activation label is exactly `Activated`; German is `Aktiviert`.
- Preserve `unsafe_code = "forbid"`; no new network/service/privileged side effects.
- Do not translate user/model content, code, paths, command IDs, tool payloads or historical transcript text.
- Settings output is allowlisted, operator-only and contains no secret/environment/auth dump.
- Persist interface preferences in the active profile; do not conflate home-global, profile and project scopes.
- Keep model/effort/mode/approval changes on existing validated command paths.
- Locale is session-specific; any scoped thread-local helper restores state after nested calls/panics and never spans await.
- Subagents do not spawn their own agents. Coordinator owns reviews and final validation. No agent pushes or merges independently.

## Review Focus

- Concurrent sessions with different languages must not leak locale; test nested/scoped and panic restoration.
- Failed/invalid persistence must leave file and live preferences unchanged; test invalid locale/value, unwritable destination and unrelated TOML preservation.
- CJK/Indic labels must remain navigable at narrow widths; test width/grapheme behavior across all locales.
- Fixed-agent or busy-turn restrictions must remain effective through settings model actions; use existing actions and cover refusal/staging.
- A language switch must not rewrite transcript/user/model text or translate machine identifiers; test with mixed-language user content and stable command routes.

---

### Task 1: Typed locale and validated settings operations

**Files:** modify `harw-config/src/harness_config.rs`, `harw-config/src/lib.rs`, `harw-config/src/scope.rs`, `harw-config/src/merge.rs` and `harw-config/src/writer.rs` plus directly affected tests; create `harw-ops/src/settings.rs`; register in `harw-ops/src/lib.rs`; adjust live configuration/persistence helpers only as needed.

**Interfaces:** export `harw_config::UiLanguage` (Copy/Default/Eq/serde, English default) with `as_str()`, `native_name()`, `ALL` and strict parsing. Add `TuiSection.language`. Register operator command `/settings show|set <key> <value>|reset <key>` without model tool exposure. Supported editable keys: `tui.language`, `tui.theme`, `tui.reasoning`, `tui.child_stream`, `tui.status_expiry`. `show` returns allowlisted `OpOutput.data` containing language and rows with `key`, `value`, `choices`, `scope`=`profile`, `apply`=`now`; values are stable config tokens. Include safe model/navigation summaries only if needed by Task 3; no arbitrary config serialization. Reset removes the selected override and correctly recomputes its inherited/default live value.

- [x] Add focused tests for English default, all codes round-trip, unsupported code refusal, profile-only scope/merge and missing-field backwards compatibility.
- [x] Implement enum, field and merge/scope wiring; update schema field counts as actually required.
- [x] Add settings operation tests for typed validation, reset/inheritance, active profile isolation, unrelated TOML/comment preservation, failure atomicity and no secret fields/model exposure.
- [x] Implement fresh-read validated persistence and live mirroring after successful save; add a cooperating ConfigWriter file lock with clear busy/conflict failure (no indefinite blocking), hold new-setting serialization through live publication, and prepare the resolved reset result before committing. Test competing writers; external editors ignoring the lock are outside the guarantee.
- [x] Run focused config/settings tests, diff/format checks; commit and report exact commands and remaining integration needs.

### Task 2: Localization infrastructure and catalog contract

**Files:** create `harw-tui/src/i18n.rs`, `harw-tui/src/locales/*.json`, tests; register module in `harw-tui/src/lib.rs`.

**Interfaces:** `tr(key: &str, english: &'static str) -> &'static str`, `tr_args(key: &str, english: &str, args: &[(&str, &str)]) -> String`, `with_language<R>(UiLanguage, impl FnOnce() -> R) -> R`, and explicit-language equivalents `tr_for`/`tr_args_for`. Immutable catalogs are split by UI zone and loaded once. Each key maps all ten locale codes to complete messages; `en` matches fallback. Unknown/missing entries use supplied English fallback, never internal keys. Parameter substitution replaces catalog placeholders once without reprocessing inserted user text. No dynamic translation of arbitrary rendered strings.

- [x] Test all-locale key/placeholder parity and exact Activated/Aktiviert values.
- [x] Test fallback, interpolation (including user braces), per-thread isolation, nested scopes and unwind restoration; no unsafe and no async-held global locale.
- [x] Implement catalog loader/translation helpers with component namespaces and docs for adding keys/locales.
- [x] Run focused tests, format/diff checks, commit and report.

### Task 3: Native settings panel and live application wiring

**Files:** create `harw-tui/src/settings_view.rs`; modify `local_commands.rs`, `command_catalog.rs`, `app.rs`, `runtime_root.rs`, `overlay_view.rs` as needed, `style.rs`, appropriate app submodules and settings locale catalog.

**Interfaces:** SettingsView implements OverlayView, reads `/settings show`, sends `/settings set|reset` and delegates model/effort/mode/permissions to existing local/action commands. ChatApp holds current UiLanguage and establishes only synchronous locale scopes around rendering/input/event handling; refreshes display preferences from current_config after successful settings operations, rebuilds cached command summaries/popups, and passes captured locale explicitly across async command/fetch helpers. Existing constructors/default fixtures remain English by default. Provide explicit profile/apply timing and native language names.

- [x] Add interaction tests for open/navigate/edit/cancel/reset/refresh and safe command generation; numeric/text editors only for supported settings.
- [x] Implement categories Interface, Models, Behavior/Approvals and safe provider navigation. Use existing pickers rather than duplicating model admission.
- [x] Wire language, configured theme, reasoning, child stream and status expiry to actual live consumers; preserve transcript text during changes.
- [x] Test persistence reopening, two differently localized apps with interleaved async completions, narrow widths, busy/fixed-agent model behavior and no secret output.
- [x] Run focused overlay/config-consumer tests and relevant operation integration checks; commit and report.

### Task 4: Localize dialogs, pickers, help and setup

**Files:** TUI dialog/picker/help/setup files identified in inventory; component JSON catalogs. Avoid app.rs except narrowly necessary caller locale wiring agreed with coordinator.

- [x] Inventory production literals separately from comments/tests; assign stable semantic keys.
- [x] Migrate complete UI-owned messages and format templates to catalog calls for all ten languages. Preserve command strings, model/provider identifiers, payloads and user content.
- [x] Update affected tests to explicit language fixtures; add English-default and non-Latin width/navigation cases.
- [x] Run relevant dialog/picker/help/setup tests; commit and report remaining production German strings with reasons.

### Task 5: Localize main chrome, notices and remaining TUI views

Checkpoint (2026-10-30): Tasks 1–4 and reviewed slices 5A–D are complete.
5A covers main chrome; 5B typed command/control feedback; 5C history/tool/activity
presentation; 5D agent monitoring and Mission Control. The integration checkpoint
also fixes the generic busy-result timing wrapper and one Japanese observed-job
label. Remaining translation groups **5E–I remain open**, including the rest of
async/app notices, background agents and remaining view zones. The unchecked
items below describe that remaining aggregate scope; they do not undo reviewed
slices. CI/default-feature workspace checkpoint verification passed; independent
whole-branch review remains pending under Task 6. This is not a claim of complete
TUI translation.

- [x] 5A: Main chrome and app notices.
- [x] 5B: Typed command/control feedback.
- [x] 5C: History, tool and activity presentation.
- [x] 5D: Agent monitoring and Mission Control.
- [ ] 5E: Remaining async application and agent notices (busy timing wrapper alone is fixed at this checkpoint).
- [ ] 5F: Workspace and board views.
- [ ] 5G: Knowledge and palace views.
- [ ] 5H: Attachments, session startup and export.
- [ ] 5I: DoD chrome and coverage closure.

**Files:** `app.rs`, app submodules, `status_line.rs`, `command_exec.rs`, `command_catalog.rs`, history/mission/side-panel views and remaining production TUI display code; component catalogs.

- [ ] Migrate remaining TUI-owned labels/notices to complete localized messages, with exact English Activated/German Aktiviert where activation is displayed.
- [ ] Use structured operation output for localized settings feedback; do not apply search/replace to arbitrary OpOutput.text or transcript text.
- [ ] Replace relevant character-count truncation with terminal-column-aware helpers; keep grapheme-safe input and layout constraints.
- [ ] Audit German production literals, documenting non-UI/protocol/test exclusions; test common English paths, language switching and render size matrices.
- [ ] Run focused UI/render regressions; commit and report coverage boundaries honestly.

### Task 6: Integration validation and documentation

**Files:** user-facing settings/language documentation, implementation status/plan checkboxes, regressions required by review.

Checkpoint limitation: the canonical `make clippy-tests` all-features command
currently fails in the pre-existing optional cloud proxy placeholder
(`harw-cloud/harw-cloud-gateway/src/proxy.rs`, missing `ProxyHttp` with the current
Pingora feature declaration). That source and manifest are unchanged from the
integration baseline; implementing the future PL-90 proxy is outside this
checkpoint. CI/default-feature workspace validation is recorded separately.
The all-features gate must not be reported as passing. The default-feature
workspace test run passed 18,519 tests/doctests with 202 explicitly ignored;
workspace Clippy across all targets with warnings denied, all six architecture
gates, formatting and 15 installer Python tests also passed. Ignored
live/platform tests and the known pre-existing PL-97 Full-root context-window
regression remain unverified. Independent whole-branch review and remote CI are
coordinator-owned and pending.

- [x] Verify catalog coverage/placeholder parity across all ten locales and settings semantics across profile/restart boundaries.
- [ ] Run affected crate tests, warnings-as-errors Clippy, rustfmt, architecture gates and Python checks where relevant; one final workspace verification after fixes stabilize.
- [ ] Fresh independent whole-branch review against the design, with concrete code/test evidence; fix actionable regressions.
- [ ] Record exact validated commit, tests and explicit limitations. Publish a reviewable feature PR targeting dev; merge only if covered by the user's applicable authorization and all required checks pass.

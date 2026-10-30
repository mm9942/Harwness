# TUI settings and localization

## Intent and baseline

User requests English by default (including **Activated**, not **Aktiviert**), a `/settings` interface for language and user/model configuration, and additional European and Asian languages. Implementation uses subagents as requested. Baseline: `dev@df08fc77baacacd085e6f5c6a7824f2aa391dedf` after PRs #154–#157; validated merge tree `4462e0c7d99b881b8e0421c9c94546694822a974`.

## Original baseline

Before this work, TUI text was largely hard-coded German, with no locale field or `/settings` surface. Existing OverlayView returns actions to the normal command path. RuntimeAssembly.current_config()/SharedLiveConfig expose live persisted preferences. Existing model/effort/mode/permissions operations enforce their own policies and must remain the authority for those changes. Generic ConfigWriter alone does not fully validate arbitrary settings. CLI “global” and permissions “global” target different locations.

## Target

- Default English; profile-level `tui.language` selects `en`, `de`, `es`, `fr`, `pt`, `ja`, `zh-CN`, `ko`, `vi`, or `hi`. Picker uses native language names. Unsupported values are errors, not silently guessed languages.
- Translate TUI-owned chrome, settings, help, pickers, dialogs, setup, and generated notices. Do not translate model/user content, code, paths, identifiers, tool payloads, or historical transcript text. Shared operation text remains a separate presentation boundary; prefer structured output for settings instead of translating arbitrary strings.
- `/settings` opens a keyboard-driven category/row panel. Arrow keys/Tab navigate; Enter selects/edits; Escape cancels; reset restores inherited/default values.
- Interface controls: language, theme (auto/dark/light), reasoning display, child streaming, status-expiry display. Wire each exposed setting to a real live consumer. Do not expose inert configuration.
- Model controls open existing root/UIA/internal-role pickers; reasoning effort, interaction mode and approvals use existing governed command paths. Show current/session versus persisted/profile scope and when effects apply. Provider settings expose safe summaries and existing configuration actions, never secret values.
- Settings mutations validate typed allowlisted values before persistence, target the active profile, preserve unrelated TOML/comments, and leave file/live state unchanged on pre-commit failure. If rename commits but directory synchronization fails, reconcile live state to the committed file and report durability uncertainty explicitly. Never dump ResolvedConfig or environment/auth state. Keep mutation operator-only rather than adding model tools.

## Approaches and implementation choices

1. Native curated settings overlay plus validated operations (selected engineering approach): consistent keyboard UX, safe scopes, reuses model rules.
2. Raw TOML editor: rejected because generic writes do not establish whole-config validity and many fields require reconstruction/restart.
3. Commands alone: retained as accessible explicit `/settings show|set|reset`, but insufficient for discoverable user controls.

Localization uses stable catalog keys, complete parameterized messages, English fallback and catalog/placeholder validation. Keep locale session-specific. Explicit locale parameters are preferred for new components; existing synchronous rendering/event paths may use a tightly scoped thread-local locale helper with restoration on exit/panic, never a process-global mutable locale or a guard spanning await. This allows incremental migration of numerous existing view APIs without mixing locales across concurrently running sessions. Document and test this tradeoff. Locale changes redraw chrome immediately and affect future notices; historical content is preserved.

## Builder-check resolutions

- Async command helpers and background fetches capture/pass an explicit UiLanguage; only individual synchronous formatting/rendering closures use scoped locale. Test interleaved asynchronous sessions.
- Serialize read-modify-write config access using a cooperating ConfigWriter file lock (return a clear busy/conflict error rather than block indefinitely). New settings mutation holds its ordering boundary through live publication. Compute/validate the post-reset resolved value before committing. Atomic rename alone is not concurrency control; external editors that ignore locks remain outside this guarantee.
- Rebuild cached command summaries/popup labels when language changes. Inventory generated activation feedback; add structured presentation where needed instead of translating arbitrary operation text.
- Only the new interface preference allowlist targets the active profile. Delegated model/mode/approval controls preserve their existing scopes and display them explicitly; they are not generic `/settings set` keys.

## Ownership and sequence

1. Config language schema/scope/merge and validated settings backend.
2. Localization catalog infrastructure and primary chrome.
3. Settings overlay and app/runtime wiring.
4. Migrate remaining TUI-owned display text in disjoint view/dialog/app zones, preserving machine identifiers and test semantics.
5. Independent review, targeted tests, clippy, formatting, architecture gates and final integration verification.

No new privileged service or security policy is introduced. Existing model admission, fixed-agent restrictions, consent and secret handling remain in force.

## Acceptance and verification

- Empty config boots English; explicit German boots German; each listed language is selectable and persists in only the active profile.
- “Activated” is the English activation label; German uses “Aktiviert”.
- Settings reopen with live values; language/theme/display changes actually apply; invalid inputs and pre-commit write failures do not partially mutate state; post-commit durability uncertainty is reported truthfully with disk/live state reconciled.
- Model actions honor existing model lifecycle/capability/agent constraints and busy-turn behavior.
- Catalog keys/placeholders agree across supported locales; missing translations fall back to English, never display internal keys.
- Rendering in narrow, portrait and landscape terminals keeps controls reachable and handles Unicode terminal columns/graphemes.
- User/model text, code, paths, provider IDs and secrets are never rewritten by localization.
- Two independently localized apps do not affect one another; scoped locale is restored after nested calls and panics.

## Version and release consistency

The user additionally requested version/release bumps. Target workspace version is `0.10.0`: synchronize internal dependency pins and workspace lockfile records, reconcile changelog entries against published `v0.9.1`, and check tag/Cargo version equality before release builds. The private webui package version remains independent. Creating release tags is separate from updating release metadata.

## Implementation status

Integration checkpoint locally verified with CI/default features (2026-10-30): Tasks 1–4 and 5A–D are
implemented and independently reviewed. These cover the typed settings backend,
catalogs, native settings UI, dialogs/pickers/help/setup, main chrome, typed
control feedback, history/tool/activity presentation, and agent/Mission Control
views. A bounded checkpoint adds neutral busy completion wording, the Japanese
no-longer-listed correction, synchronized 0.10.0 release metadata and user docs.
Remaining translation groups 5E–I stay open; backend/freeform output and remaining
app/view zones can still display German or English. Default-feature workspace
Clippy and 18,519 tests/doctests passed (202 explicitly ignored); a fresh
whole-branch review and remote CI are still required before integration is
claimed verified. Language set is user-directed; UI/backend decomposition and localization
mechanism are engineering choices. Original PRs #154–#157 are merged and their
integration is independently verified. No transcript translation is introduced.

The canonical all-features workspace gate exposes a pre-existing optional cloud
proxy placeholder build failure (`ProxyHttp` is unavailable with the current
Pingora feature declaration). The coordinator leaves that future PL-90 work out
of this checkpoint. Default-feature workspace checks are tracked separately;
all-features workspace validation remains blocked.

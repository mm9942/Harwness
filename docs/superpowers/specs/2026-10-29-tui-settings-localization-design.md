# TUI settings and localization

## Intent and baseline

User requests English by default (including **Activated**, not **Aktiviert**), a `/settings` interface for language and user/model configuration, and additional European and Asian languages. Implementation uses subagents as requested. Baseline: `dev@df08fc77baacacd085e6f5c6a7824f2aa391dedf` after PRs #154–#157; validated merge tree `4462e0c7d99b881b8e0421c9c94546694822a974`.

## Current

TUI text is largely hard-coded German. There is no locale field or `/settings` surface. Existing OverlayView returns actions to the normal command path. RuntimeAssembly.current_config()/SharedLiveConfig expose live persisted preferences. Existing model/effort/mode/permissions operations enforce their own policies and must remain the authority for those changes. Generic ConfigWriter alone does not fully validate arbitrary settings. CLI “global” and permissions “global” target different locations.

## Target

- Default English; profile-level `tui.language` selects `en`, `de`, `es`, `fr`, `pt`, `ja`, `zh-CN`, `ko`, `vi`, or `hi`. Picker uses native language names. Unsupported values are errors, not silently guessed languages.
- Translate TUI-owned chrome, settings, help, pickers, dialogs, setup, and generated notices. Do not translate model/user content, code, paths, identifiers, tool payloads, or historical transcript text. Shared operation text remains a separate presentation boundary; prefer structured output for settings instead of translating arbitrary strings.
- `/settings` opens a keyboard-driven category/row panel. Arrow keys/Tab navigate; Enter selects/edits; Escape cancels; reset restores inherited/default values.
- Interface controls: language, theme (auto/dark/light), reasoning display, child streaming, status-expiry display. Wire each exposed setting to a real live consumer. Do not expose inert configuration.
- Model controls open existing root/UIA/internal-role pickers; reasoning effort, interaction mode and approvals use existing governed command paths. Show current/session versus persisted/profile scope and when effects apply. Provider settings expose safe summaries and existing configuration actions, never secret values.
- Settings mutations validate typed allowlisted values before persistence, target the active profile, preserve unrelated TOML/comments, and leave file/live state unchanged on failure. Never dump ResolvedConfig or environment/auth state. Keep mutation operator-only rather than adding model tools.

## Approaches and implementation choices

1. Native curated settings overlay plus validated operations (selected engineering approach): consistent keyboard UX, safe scopes, reuses model rules.
2. Raw TOML editor: rejected because generic writes do not establish whole-config validity and many fields require reconstruction/restart.
3. Commands alone: retained as accessible explicit `/settings show|set|reset`, but insufficient for discoverable user controls.

Localization uses stable catalog keys, complete parameterized messages, English fallback and catalog/placeholder validation. Keep locale session-specific. Explicit locale parameters are preferred for new components; existing synchronous rendering/event paths may use a tightly scoped thread-local locale helper with restoration on exit/panic, never a process-global mutable locale or a guard spanning await. This allows incremental migration of numerous existing view APIs without mixing locales across concurrently running sessions. Document and test this tradeoff. Locale changes redraw chrome immediately and affect future notices; historical content is preserved.

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
- Settings reopen with live values; language/theme/display changes actually apply; invalid inputs and failed writes do not partially mutate state.
- Model actions honor existing model lifecycle/capability/agent constraints and busy-turn behavior.
- Catalog keys/placeholders agree across supported locales; missing translations fall back to English, never display internal keys.
- Rendering in narrow, portrait and landscape terminals keeps controls reachable and handles Unicode terminal columns/graphemes.
- User/model text, code, paths, provider IDs and secrets are never rewritten by localization.
- Two independently localized apps do not affect one another; scoped locale is restored after nested calls and panics.

## Implementation status

Planned; not implemented or merged. Language set is user-directed; UI/backend decomposition and localization mechanism are engineering choices. Original PR integration is complete and independently verified.

# W2d-1 / F-T — Fix-Agent TUI (Befunde aus Z2d1-tui: T1, T2, T3, T6, T7, T8, T9)

Rolle: focused-bug-fix (Opus). BUILD-POLICY eingehalten: nur lesen und grep, kein cargo build/check/test/clippy, kein
make/rustc/rust-analyzer, keine git-Schreibbefehle. **Nichts kompiliert, keine Tests ausgeführt.** Verifiziert wurde
nur durch Lesen.

## Geänderte Dateien

- `harw-tui/src/tools_command.rs`: bounded-Pfad, Helfer, Doku, Tests.
- `harw-tui/src/runtime_commands.rs`: Parameter `base` heißt jetzt `ceiling`, Variante `BeyondBase` heißt jetzt `BeyondCeiling`. Dazu T1-Doku, Moduldoku und Tests.
- `harw-tui/src/command_exec.rs`: nur die Rustdoc-Zeilen 99–101 (Shell/ShellRepeat) wurden geändert (T6).

Unverändert: `dispatch_tools_command`. Die Signatur und das Verhalten sind gleich, nur der Veraltet-Hinweis nennt jetzt die Decke. `handle_tools_command`, `reset_all`, `reset_one` und `set_profile` sind ebenfalls unverändert.

## Exakte Signaturen (neu/geändert)

```rust
// tools_command.rs:275
#[must_use]
pub fn dispatch_tools_command_bounded(
    args: &str,
    tool_snapshot: &[(String, bool)],
    activation: &mut SessionActivation,
    ceiling: &SessionActivation,          // vorher: base
) -> ToolsCommandOutcome;

// privat
fn bounded_set_tool(activation: &mut SessionActivation, ceiling: &SessionActivation,
                    known_tools: &[(String, bool)], name: &str, enable: bool) -> ToolsCommandOutcome; // :417
fn bounded_reset_all(activation: &mut SessionActivation, ceiling: &SessionActivation) -> ToolsCommandOutcome; // :442 (neu)
fn bounded_reset_one(activation: &mut SessionActivation, ceiling: &SessionActivation, name: &str) -> ToolsCommandOutcome; // :465 (neu)
fn bounded_set_profile(activation: &mut SessionActivation, ceiling: &SessionActivation,
                       known_tools: &[(String, bool)], p: &str) -> ToolsCommandOutcome; // :507 (+known_tools)

// runtime_commands.rs:139
pub(crate) fn validate_tool_toggle(
    ceiling: &SessionActivation,          // vorher: base
    known_tools: &[(String, bool)],
    tool: &str,
    enable: bool,
) -> Result<(), ToolToggleError>;

// runtime_commands.rs:81-92
pub(crate) enum ToolToggleError {
    UnknownTool { name: String },
    BeyondCeiling { name: String },       // vorher: BeyondBase
}
```

Aufrufer-Vertrag für W2d-2: `dispatch_tools_command_bounded(args, &snapshot, activation, &session.mode_ceiling())`.
F-C liefert `AgentSession::mode_ceiling(&self) -> SessionActivation` (Basis ∩ Modus) in `harw-core/src/session.rs`; die Datei habe ich nicht angefasst.

## Befunde → Umsetzung

| ID | Umsetzung |
|---|---|
| T3 | Der bounded-Pfad prüft `on` gegen `ceiling` (`validate_tool_toggle`). `profile` wird mit `ceiling` geschnitten. Die Doku definiert die Decke als Basis ∩ Modus (`AgentSession::mode_ceiling`). Workspace-grep nach `BeyondBase`, `validate_tool_toggle`, `ToolToggleError` und `dispatch_tools_command_bounded` findet in `*.rs` außerhalb der zwei owned files keinen Treffer. |
| T2 | `reset` führt `*activation = ceiling.clone()` aus (`SessionActivation: Clone`, activation.rs:154). `reset <name>` ruft zuerst `reset_tool` auf. Weicht der Zustand danach von `ceiling.is_tool_enabled(name)` ab, wird er per `enable_tool`/`disable_tool` erzwungen. Ein Tool bleibt nie über der Decke. Bestätigungen lauten `reset all tool overrides to the session ceiling` und `reset <name> to session ceiling (on\|off)`. |
| T7 | `profile <p>` wendet das Profil zuerst auf eine Kopie an (`requested`) und berechnet dann `effective = requested.intersect(ceiling)`. Kandidaten sind die Snapshot-Namen und die Allowlist des angefragten Profils. Withheld sind Kandidaten mit requested an und effective aus, sortiert über ein `BTreeSet`. Ist keiner withheld, lautet die Meldung `profile set to <p>`. Sonst lautet sie `profile <p> limited by session ceiling: effective profile {:?}, withheld: a, b`. |
| T8 | Die Intra-Doc-Links `[`crate::runtime_commands::validate_tool_toggle`]` sind jetzt Backticks. Das betrifft das pub-Item `dispatch_tools_command_bounded` und das private `bounded_set_tool`. `runtime_commands` ist `pub(crate) mod` (lib.rs:27), dort bleiben die modulinternen Links. |
| T6 | Die Rustdoc in command_exec.rs:99-101 lautet jetzt: Ablehnung über die Capability `commands.shell` mit dem wörtlichen Text aus :281. Der TUI-Kontext aktiviert diese Capability nicht. |
| T1 | `caller_tier` (:52) und `slash_service_map` (:71) bleiben. Beide haben die Doc-Zeile „Vertrag: Aufrufer folgt in W2d-2/D5 (app.rs).“ |

## API-Nachweise

- `SessionActivation::is_tool_enabled`, `disable_tool`, `enable_tool`, `reset_tool`: harw-core/src/activation.rs:233, :251, :261, :271.
- `SessionActivation::intersect(&self, &Self) -> Self`: activation.rs:395. Das Ergebnisprofil ist `Full` nur, wenn beide Seiten `Full` sind, sonst `Minimal` mit Extras.
- `#[derive(Debug, Clone, Default)] SessionActivation`: activation.rs:154.
- `ToolProfile` ist `Copy` (activation.rs:49). `ToolProfile::allowlist(self) -> Option<HashSet<ToolName>>`: activation.rs:107.
- `ToolName(pub String)`, `new(impl Into<String>)`, `as_str()`: harw-tools/src/spec.rs:10-19. Re-Export über `harw_extension_api::ToolName`: harw-extension-api/src/lib.rs:18-21.
- Muster für die Modus-Decke: `apply_mode` bildet `base_activation.intersect(&mode_activation)` (harw-core/src/session.rs:547 ff.).

## Tests

`runtime_commands.rs` (migriert):
- `test_validate_tool_toggle_enable_forbidden_by_ceiling_is_err` (vorher `…_by_base_…`)
- `test_validate_tool_toggle_enable_allowed_by_ceiling_is_ok` (vorher `…_by_base_…`)
- `test_tool_toggle_error_display_names_tool` nutzt jetzt `BeyondCeiling`
- Die Fixture `base()` heißt jetzt `ceiling()`

`tools_command.rs`:
- Die Fixture `bounded_base()` heißt jetzt `bounded_ceiling()`. Neu sind `full_ceiling_without_beta()` und `both_known()`.
- Umbenannt: `test_dispatch_tools_command_bounded_enable_beyond_ceiling_is_rejected` und `test_dispatch_tools_command_bounded_profile_is_cut_to_ceiling`. Der zweite Test hat einen leeren Snapshot und erwartet weiterhin `profile set to full`.
- Neu zu T9(a):
  - `test_dispatch_tools_command_bounded_reset_restores_ceiling`
  - `test_dispatch_tools_command_bounded_reset_one_keeps_ceiling_disabled_tool_off`
  - `test_dispatch_tools_command_bounded_reset_then_reset_one_stays_within_ceiling`: Ausgangspunkt ist eine Aktivierung über der Decke. Nach `reset` und `reset test.beta` bleibt beta aus, nach `reset test.alpha` ist alpha wieder an.
- Neu zu T9(b):
  - `test_dispatch_tools_command_bounded_on_outside_ceiling_is_beyond_ceiling`: prüft auf exakt `ToolToggleError::BeyondCeiling{..}.to_string()`.
  - `test_dispatch_tools_command_bounded_profile_full_reports_withheld_tools`: prüft ⊆ ceiling für alle registrierten Tools. Die Meldung ist exakt `profile full limited by session ceiling: effective profile Minimal, withheld: test.beta`.

## Hinweise für Parent / Integrations-Audit

- Die Bestätigungstexte des bounded-Pfads für `reset` und für das geschnittene `profile` weichen vom unbeschränkten Pfad ab. Das ist beabsichtigt (T2/T7). Bisher gibt es keinen Prod-Aufrufer, W2d-2 muss das beim Umstieg in app.rs beachten.
- `rustfmt` wurde nicht ausgeführt. Einzelne neue Testzeilen können über 100 Zeichen lang sein, `cargo fmt` gehört zum Orchestrator-Lauf.
- Kein `unwrap`/`expect` in Prod, kein `#[allow]`, kein `unsafe`. Tests stehen am Dateiende.
- dep-requests: keine.

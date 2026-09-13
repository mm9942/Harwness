# SLICE 6 — Contract Master: `OnboardingWizard`-Neustrukturierung

Ziel: `setup.rs` (1264 Zeilen) verhaltenserhaltend in einen `onboarding/`-Modulbaum
zerlegen. **Keine Verhaltensänderung** — die funktionale OAuth/Paste-Korrektur ist
bereits in SLICE 1 gelandet. Dieser Slice ist ein reiner Struktur-Refactor für
Wartbarkeit + die vom Spec (§2.6/2.7, SLICE 6) geforderten Typen.

## Ausgangslage (`setup.rs`, Ist)

| Symbol | Zeile | Zieldatei |
|---|---|---|
| `SetupStage` (Provider/Auth/Model/Done) | 87 | `onboarding/mod.rs` (bleibt als Re-Export via `setup.rs`) |
| `SetupOutcome` | 108 | `onboarding/mod.rs` |
| `AuthOption` (+ `impl`) | 125/146 | `onboarding/auth.rs` |
| `SetupApp` (Zustandsmaschine) | 180 | wird zu `OnboardingWizard` in `onboarding/mod.rs` |
| `on_key_provider` | 306 | `onboarding/provider_picker.rs` |
| `on_key_auth`, `on_paste`, `is_auth_editing`, `cancel_auth_editing`, `selected_is_secret` | 269–329, 307 | `onboarding/auth.rs` |
| `on_key_model` | 308 | `onboarding/model_picker.rs` |
| `build_auth_options` | 577 | `onboarding/auth.rs` |
| `model_options_for` | 634 | `onboarding/model_picker.rs` |
| `api_str` | 645 | `onboarding/provider_picker.rs` (oder shared) |
| `TerminalGuard`, `run_setup`, `setup_loop`, `draw`, `body_lines`, `selectable_line`, `mask` | 666–921 | `onboarding/mod.rs` (Loop + Rendering) |
| Tests | 975+ | jeweils in die Zieldatei mitwandern (delegiert an rust-test-designer) |

## Ziel-Modulbaum

```
onboarding/
  mod.rs            // OnboardingWizard, WizardStep, StepState, StepStateProvider,
                    // SetupStage(re-export), SetupOutcome, run_setup, Loop, Rendering
  provider_picker.rs// ProviderPickerWidget + StepStateProvider
  auth.rs           // AuthWidget, AuthState, AuthOption + StepStateProvider
  model_picker.rs   // ModelPickerWidget + StepStateProvider
setup.rs            // dünner Wrapper: `pub use crate::onboarding::{SetupApp?, SetupStage, SetupOutcome, run_setup};`
```

`lib.rs`: `pub mod setup;` bleibt; zusätzlich `pub(crate) mod onboarding;`. Die
öffentlichen Re-Exporte (`SetupApp`/`SetupStage`/`SetupOutcome`/`run_setup`) MÜSSEN
erhalten bleiben — `harw-cli/src/onboarding.rs:80` ruft `harw_tui::run_setup(...)`.

## Shared-Typen (fixiert — VOR der Parallelisierung)

```rust
// onboarding/mod.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepState { Hidden, InProgress, Complete }

pub(crate) trait StepStateProvider {
    /// Aktueller Sichtbarkeits-/Fortschrittszustand dieses Wizard-Schritts.
    fn step_state(&self) -> StepState;
}

pub(crate) enum WizardStep {
    Provider(ProviderPickerWidget),
    Auth(AuthWidget),
    Model(ModelPickerWidget),
}
```

```rust
// onboarding/auth.rs
pub(crate) enum AuthState {
    PickMode { highlighted: usize },
    ApiKeyEntry { value: String, prepopulated: bool },
    OAuthPending { url: Option<String>, token: String },
    Configured { secret_ref: String },
}
```

## Integrations-Reihenfolge (atomar — dead_code-Zwang)

Wegen `-D warnings` ist KEIN Zwischenzustand grün, solange `onboarding/` deklariert,
aber nicht von `setup.rs`/`run_setup` genutzt wird. Deshalb:

1. Widgets (`provider_picker.rs`, `auth.rs`, `model_picker.rs`) parallel authoren
   (focused-coding-task-agents) — Verifikation vorerst NUR `cargo build` gegen eine
   lokale Stub-`mod.rs`, die alle Typen verdrahtet.
2. `mod.rs` (OnboardingWizard + Loop + Rendering) durch Main integrieren.
3. `setup.rs` auf Re-Export-Wrapper eindampfen.
4. `lib.rs` anpassen.
5. Ein einziger grüner `make clippy-tests`-Lauf zertifiziert.

## Akzeptanzkriterien (unverändert ggü. Spec)

- Provider-Auswahl → Auth-Schritt erscheint automatisch.
- `1` im Auth-Schritt → sofort `ApiKeyEntry`-Feld.
- Paste im `ApiKeyEntry`-State → Key wird direkt eingesetzt.
- Auth-Schritt `Complete` → Model-Schritt erscheint automatisch.
- `harw_tui::run_setup` + `SetupOutcome` bleiben öffentlich & signaturgleich.
- Alle bestehenden `setup.rs`-Tests bleiben grün (nach Umzug in die Zielmodule).

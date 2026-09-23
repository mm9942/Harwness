//! Zweistufiger Provider/Modell-Umschalt-Picker für `/model`- und
//! `/uia model`-artige Befehle.
//!
//! Spec-Quelle: `recursive-cooking-lobster.md`, Abschnitt "Welle 1 — 1d".
//!
//! # Verantwortung
//! Kapselt die Zwei-Stufen-Navigation (Provider auswählen → Modell
//! auswählen) hinter einem einzigen Widget, das intern zwei
//! [`crate::choice_dialog::ChoiceDialog`]-Instanzen verwaltet. Kennt weder
//! die konkrete Overlay-Verdrahtung noch Config-/Registry-Typen des
//! Aufrufers (`harw-tui/src/app.rs`) — dieser liest nur [`PickerAction`] und
//! [`ModelSwitchPicker::target`] und entscheidet, was mit einer
//! `Accept`-Auswahl geschieht.
//!
//! # Schlüsseltypen
//! - [`PickerTarget`] — für welchen Umschalt-Kontext (Orchestrator, UIA,
//!   fester UIA-Worker) der Picker instanziiert wurde.
//! - [`ProviderEntry`] / [`ModelEntry`] — Anzeige-Fixtures, vom Aufrufer
//!   befüllt (keine Config-Abhängigkeit in diesem Modul).
//! - [`ModelSwitchPicker`] — Widget-Zustand mit `on_key`/`render`-Muster,
//!   analog zu [`crate::choice_dialog::ChoiceDialog`] und
//!   [`crate::session_picker::SessionPicker`].
//! - [`PickerAction`] — Ereignis, das [`ModelSwitchPicker::on_key`]
//!   zurückgibt.
//!
//! # Nebenläufigkeit
//! Kein interner Zustand wird geteilt; der Aufrufer hält `ModelSwitchPicker`
//! exklusiv (`&mut`), analog zu `ChoiceDialog`.
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel. Degenerierte Eingaben (leere
//! Providerliste, Provider ohne Modelle) werden über [`PickerAction::Cancel`]
//! bzw. `Option::None` bei der Konstruktion signalisiert, nicht über Panics.
//!
//! # Verdrahtungshinweis
//! Dieses Modul ist in `harw-tui/src/app.rs` eingehängt: `ChatApp::overlay`
//! trägt die (dortige, private) Variante `Overlay::ModelSwitch`, die
//! [`ModelSwitchPicker::on_key`] aufruft und `PickerAction::Accept` je nach
//! [`ModelSwitchPicker::target`] in `/model switch …`- bzw.
//! `/uia model …`-Befehle übersetzt.
#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect};

use crate::choice_dialog::{ChoiceAction, ChoiceDialog};
use crate::style;

/// Titel der Provider-Auswahlstufe.
const PROVIDER_DIALOG_TITLE: &str = "Provider wählen";

/// Fußzeilen-Hinweis für die Modell-Stufe, wenn ein Rücksprung zur
/// Provider-Stufe möglich ist (`Left`).
const MODEL_FOOTER_WITH_BACK: &str = "↑↓ wählen · ← zurück · Enter bestätigen · Esc abbrechen";

/// Für welchen Umschalt-Kontext der Picker instanziiert wurde.
///
/// # Beschreibung
/// Steuert, ob die Provider-Stufe überhaupt durchlaufen wird.
/// [`PickerTarget::UiaWorker`] überspringt sie vollständig, da ein
/// UIA-Worker an genau einen Provider gebunden ist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerTarget {
    /// Umschaltung des Haupt-Orchestrator-Modells.
    Orchestrator,
    /// Umschaltung des UIA-Modells (Provider frei wählbar).
    Uia,
    /// Umschaltung eines an einen festen Provider gebundenen UIA-Workers.
    UiaWorker {
        /// Provider-ID, an die dieser Worker gebunden ist.
        fixed_provider: String,
    },
}

/// Interne Navigationsstufe des Pickers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerStage {
    /// Provider-Auswahlstufe (übersprungen bei [`PickerTarget::UiaWorker`]).
    Provider,
    /// Modell-Auswahlstufe für den zuvor gewählten (oder festen) Provider.
    Model,
}

/// Anzeige-Fixture für einen wählbaren Provider.
///
/// # Beschreibung
/// Vom Aufrufer befüllt (aus Config/Registry abgeleitet); dieses Modul
/// kennt keine Config-Typen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderEntry {
    /// Stabile Provider-ID (z. B. `"anthropic"`).
    pub id: String,
    /// Anzeigebeschriftung im Dialog (z. B. `"Anthropic"`).
    pub label: String,
}

/// Anzeige-Fixture für ein wählbares Modell innerhalb eines Providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelEntry {
    /// Stabile Modell-ID (z. B. `"claude-sonnet-4-6"`).
    pub id: String,
    /// Anzeigebeschriftung im Dialog (z. B. `"Claude Sonnet 4.6"`).
    pub label: String,
}

/// Zweistufiger Provider/Modell-Auswahl-Zustand.
///
/// # Beschreibung
/// Delegiert Navigation und Rendering je Stufe an eine interne
/// [`ChoiceDialog`]-Instanz. Fängt `Left` in der Modell-Stufe selbst ab
/// (bevor an `ChoiceDialog::handle_key` delegiert wird), da `ChoiceDialog`
/// selbst kein `Left` kennt.
///
/// `active_provider`/`active_model` werden als Kontext aus dem Konstruktor
/// gemerkt (nicht Teil der ursprünglichen Feld-Skizze des Auftrags — siehe
/// Antwort am Ende des Berichts), damit beim Wechsel von der Provider- in
/// die Modell-Stufe die ursprünglich aktive Modellauswahl vorausgewählt
/// werden kann, sofern der neu gewählte Provider mit dem ursprünglich
/// aktiven Provider übereinstimmt.
#[derive(Debug)]
pub(crate) struct ModelSwitchPicker {
    /// Umschalt-Kontext (steuert, ob die Provider-Stufe existiert).
    target: PickerTarget,
    /// Aktuelle Navigationsstufe.
    stage: PickerStage,
    /// Alle wählbaren Provider in Anzeigereihenfolge.
    providers: Vec<ProviderEntry>,
    /// Dialogzustand der Provider-Stufe (auch bei [`PickerTarget::UiaWorker`]
    /// aufgebaut, aber dort nie sichtbar/erreichbar).
    provider_dialog: ChoiceDialog,
    /// Modell-Listen je Provider-ID.
    models_by_provider: Vec<(String, Vec<ModelEntry>)>,
    /// Dialogzustand der Modell-Stufe; `None`, solange noch kein Provider
    /// gewählt wurde (nur relevant außerhalb von [`PickerTarget::UiaWorker`]).
    model_dialog: Option<ChoiceDialog>,
    /// Provider-ID, für die `model_dialog` aktuell aufgebaut ist.
    selected_provider: Option<String>,
    /// Ursprünglich aktiver Provider (Kontext für Vorauswahl-Logik).
    active_provider: Option<String>,
    /// Ursprünglich aktives Modell (Kontext für Vorauswahl-Logik).
    active_model: Option<String>,
}

/// Ereignis, das [`ModelSwitchPicker::on_key`] zurückgibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerAction {
    /// Picker bleibt geöffnet, keine weitere Aktion erforderlich.
    Stay,
    /// Nutzer hat vollständig abgebrochen (Esc auf beliebiger Stufe, oder
    /// ein Provider ohne Modelle wurde gewählt); Picker soll geschlossen
    /// werden.
    Cancel,
    /// Nutzer hat Provider und Modell final bestätigt.
    Accept {
        /// Gewählte (oder feste) Provider-ID.
        provider: String,
        /// Gewählte Modell-ID.
        model: String,
    },
}

/// Sucht die Modell-Liste für eine Provider-ID; leerer Slice, falls der
/// Provider in `models_by_provider` nicht vorkommt.
fn models_for_provider<'a>(
    models_by_provider: &'a [(String, Vec<ModelEntry>)],
    provider_id: &str,
) -> &'a [ModelEntry] {
    models_by_provider
        .iter()
        .find(|(id, _)| id == provider_id)
        .map_or(&[], |(_, models)| models.as_slice())
}

/// Berechnet den initial markierten Index für eine Modell-Liste.
///
/// `provider_matches` muss vom Aufrufer vorab bestimmt werden (ob der
/// Provider, für den diese Modell-Liste aufgebaut wird, mit dem
/// ursprünglich aktiven Provider übereinstimmt) — nur dann wird
/// `active_model` überhaupt berücksichtigt.
fn initial_model_index(
    models: &[ModelEntry],
    provider_matches: bool,
    active_model: Option<&str>,
) -> usize {
    if !provider_matches {
        return 0;
    }
    active_model
        .and_then(|model_id| models.iter().position(|m| m.id == model_id))
        .unwrap_or(0)
}

/// Ob von der Modell-Stufe per `Left` zur Provider-Stufe zurückgesprungen
/// werden kann. Nur [`PickerTarget::UiaWorker`] hat keine Provider-Stufe.
fn can_go_back(target: &PickerTarget) -> bool {
    !matches!(target, PickerTarget::UiaWorker { .. })
}

impl ModelSwitchPicker {
    /// Erstellt einen neuen `ModelSwitchPicker`.
    ///
    /// # Beschreibung
    /// Liefert `None`, wenn `providers` leer ist. Für
    /// [`PickerTarget::UiaWorker`] wird die Provider-Stufe übersprungen —
    /// `stage` startet direkt bei [`PickerStage::Model`], aufgebaut aus der
    /// Modell-Liste des festen Providers (leer, falls der Provider in
    /// `models_by_provider` fehlt — dann bleibt `Enter` auf der Modell-Stufe
    /// wirkungslos, analog zu `ChoiceDialog` bei leeren Optionen; das ist ein
    /// bewusster Degenerationsfall, siehe Bericht). Für alle anderen Ziele
    /// startet `stage` bei [`PickerStage::Provider`], vorausgewählt auf die
    /// Position von `active_provider` (falls vorhanden, sonst Index 0).
    ///
    /// # Argumente
    /// - `target` (`PickerTarget`): Umschalt-Kontext.
    /// - `providers` (`Vec<ProviderEntry>`): wählbare Provider.
    /// - `models_by_provider` (`Vec<(String, Vec<ModelEntry>)>`): Modell-Listen
    ///   je Provider-ID.
    /// - `active_provider` (`Option<&str>`): aktuell aktive Provider-ID, falls
    ///   bekannt (für die Vorauswahl).
    /// - `active_model` (`Option<&str>`): aktuell aktive Modell-ID, falls
    ///   bekannt (für die Vorauswahl).
    ///
    /// # Rückgabe
    /// `Some(Self)`, außer `providers` ist leer.
    #[must_use]
    pub(crate) fn new(
        target: PickerTarget,
        providers: Vec<ProviderEntry>,
        models_by_provider: Vec<(String, Vec<ModelEntry>)>,
        active_provider: Option<&str>,
        active_model: Option<&str>,
    ) -> Option<Self> {
        if providers.is_empty() {
            return None;
        }

        let provider_labels: Vec<String> = providers.iter().map(|p| p.label.to_owned()).collect();
        let provider_start_index = active_provider
            .and_then(|ap| providers.iter().position(|p| p.id == ap))
            .unwrap_or(0);
        let provider_dialog = ChoiceDialog::new(PROVIDER_DIALOG_TITLE, None, provider_labels)
            .with_selected(provider_start_index);

        let active_provider_owned = active_provider.map(str::to_owned);
        let active_model_owned = active_model.map(str::to_owned);

        if let PickerTarget::UiaWorker { ref fixed_provider } = target {
            let models = models_for_provider(&models_by_provider, fixed_provider);
            let model_labels: Vec<String> = models.iter().map(|m| m.label.to_owned()).collect();
            let provider_matches = active_provider == Some(fixed_provider.as_str());
            let model_start_index = initial_model_index(models, provider_matches, active_model);
            let model_dialog =
                ChoiceDialog::new(model_dialog_title(fixed_provider), None, model_labels)
                    .with_selected(model_start_index);
            let selected_provider = Some(fixed_provider.to_owned());

            return Some(Self {
                target,
                stage: PickerStage::Model,
                providers,
                provider_dialog,
                models_by_provider,
                model_dialog: Some(model_dialog),
                selected_provider,
                active_provider: active_provider_owned,
                active_model: active_model_owned,
            });
        }

        Some(Self {
            target,
            stage: PickerStage::Provider,
            providers,
            provider_dialog,
            models_by_provider,
            model_dialog: None,
            selected_provider: None,
            active_provider: active_provider_owned,
            active_model: active_model_owned,
        })
    }

    /// Gibt den Umschalt-Kontext zurück, für den dieser Picker aufgebaut
    /// wurde.
    ///
    /// # Rückgabe
    /// Referenz auf [`PickerTarget`], u. a. damit der spätere
    /// `app.rs`-Umsetzer bei `Accept` unterscheiden kann, welches Modell
    /// (Orchestrator/UIA/UIA-Worker) tatsächlich umgeschaltet werden soll.
    #[must_use]
    pub(crate) fn target(&self) -> &PickerTarget {
        &self.target
    }

    /// Verarbeitet einen Tastendruck und gibt eine [`PickerAction`] zurück.
    ///
    /// # Beschreibung
    /// Auf der Provider-Stufe wird direkt an
    /// [`ChoiceDialog::handle_key`] delegiert. `Chosen(idx)` löst die
    /// Provider-ID auf, baut die Modell-Stufe auf (`Cancel`, falls der
    /// Provider keine Modelle hat) und wechselt `stage`.
    ///
    /// Auf der Modell-Stufe fängt diese Methode `Left` selbst ab, bevor an
    /// [`ChoiceDialog::handle_key`] delegiert wird (`ChoiceDialog` kennt
    /// kein `Left`): außerhalb von [`PickerTarget::UiaWorker`] springt
    /// `Left` eine Stufe zurück (Provider-Cursor bleibt erhalten, kein
    /// Reset); bei `PickerTarget::UiaWorker` ist `Left` ein No-op
    /// (`Stay`), da keine Provider-Stufe existiert. Jede andere Taste wird
    /// an die Modell-`ChoiceDialog` delegiert; `Esc` bricht auf beiden
    /// Stufen immer vollständig ab (kein Stufen-Rücksprung über `Esc`,
    /// bewusst konsistent zu allen anderen Overlays dieser Codebasis).
    ///
    /// # Argumente
    /// - `key` (`KeyEvent`): das eingegangene Crossterm-Tastenereignis.
    ///
    /// # Rückgabe
    /// [`PickerAction::Stay`], [`PickerAction::Cancel`] oder
    /// [`PickerAction::Accept`].
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        match self.stage {
            PickerStage::Provider => self.on_key_provider(key),
            PickerStage::Model => self.on_key_model(key),
        }
    }

    /// Verarbeitet einen Tastendruck auf der Provider-Stufe.
    fn on_key_provider(&mut self, key: KeyEvent) -> PickerAction {
        match self.provider_dialog.handle_key(key) {
            ChoiceAction::Stay => PickerAction::Stay,
            ChoiceAction::Cancel => PickerAction::Cancel,
            ChoiceAction::Chosen(idx) => {
                let Some(provider) = self.providers.get(idx) else {
                    return PickerAction::Cancel;
                };
                let provider_id = provider.id.to_owned();
                let models = models_for_provider(&self.models_by_provider, &provider_id);
                if models.is_empty() {
                    return PickerAction::Cancel;
                }

                let provider_matches =
                    self.active_provider.as_deref() == Some(provider_id.as_str());
                let model_start_index =
                    initial_model_index(models, provider_matches, self.active_model.as_deref());
                let model_labels: Vec<String> = models.iter().map(|m| m.label.to_owned()).collect();

                let mut model_dialog =
                    ChoiceDialog::new(model_dialog_title(&provider_id), None, model_labels)
                        .with_selected(model_start_index);
                if can_go_back(&self.target) {
                    model_dialog = model_dialog.with_footer_hint(MODEL_FOOTER_WITH_BACK);
                }

                self.model_dialog = Some(model_dialog);
                self.selected_provider = Some(provider_id);
                self.stage = PickerStage::Model;
                PickerAction::Stay
            }
        }
    }

    /// Verarbeitet einen Tastendruck auf der Modell-Stufe.
    fn on_key_model(&mut self, key: KeyEvent) -> PickerAction {
        if key.code == KeyCode::Left {
            if matches!(self.target, PickerTarget::UiaWorker { .. }) {
                return PickerAction::Stay;
            }
            self.stage = PickerStage::Provider;
            self.model_dialog = None;
            return PickerAction::Stay;
        }

        let Some(model_dialog) = self.model_dialog.as_mut() else {
            return PickerAction::Cancel;
        };

        match model_dialog.handle_key(key) {
            ChoiceAction::Stay => PickerAction::Stay,
            ChoiceAction::Cancel => PickerAction::Cancel,
            ChoiceAction::Chosen(idx) => {
                let provider_id = match &self.target {
                    PickerTarget::UiaWorker { fixed_provider } => fixed_provider.to_owned(),
                    PickerTarget::Orchestrator | PickerTarget::Uia => {
                        match self.selected_provider.take() {
                            Some(id) => id,
                            None => return PickerAction::Cancel,
                        }
                    }
                };

                let models = models_for_provider(&self.models_by_provider, &provider_id);
                let Some(model_entry) = models.get(idx) else {
                    return PickerAction::Cancel;
                };

                PickerAction::Accept {
                    provider: provider_id,
                    model: model_entry.id.to_owned(),
                }
            }
        }
    }

    /// Zeichnet die aktuell aktive Stufe (Provider oder Modell) in den
    /// angegebenen `Buffer`-Bereich.
    ///
    /// # Beschreibung
    /// Delegiert 1:1 an [`ChoiceDialog::render`] der jeweils aktiven Stufe.
    /// Rendert nichts, falls `stage == Model` aber `model_dialog` (noch)
    /// `None` ist — dieser Zustand ist nach [`Self::new`]/[`Self::on_key`]
    /// nicht erreichbar, aber defensiv statt panisch behandelt.
    ///
    /// # Argumente
    /// - `area` (`Rect`): der Zeichenbereich im Terminal-Buffer.
    /// - `buf` (`&mut Buffer`): der ratatui-Buffer, in den geschrieben wird.
    /// - `theme` (`style::Theme`): aktives Farbschema.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; kein Locking erforderlich.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        match self.stage {
            PickerStage::Provider => self.provider_dialog.render(area, buf, theme),
            PickerStage::Model => {
                if let Some(model_dialog) = self.model_dialog.as_ref() {
                    model_dialog.render(area, buf, theme);
                }
            }
        }
    }
}

/// Baut den Titel der Modell-Stufe aus der Provider-ID.
///
/// Nutzt bewusst dieselbe Formatierung wie die bestehenden
/// `app.rs`-Overlays (`"Modell wählen ({provider})"`,
/// `"UIA-Modell wählen ({provider})"`), vereinfacht hier aber auf die
/// Provider-ID (Label-Auflösung ist Sache des app.rs-Umsetzers, der die
/// vollständige Provider-Config kennt).
fn model_dialog_title(provider_id: &str) -> String {
    format!("Modell wählen ({provider_id})")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn providers_fixture() -> Vec<ProviderEntry> {
        vec![
            ProviderEntry {
                id: "anthropic".to_owned(),
                label: "Anthropic".to_owned(),
            },
            ProviderEntry {
                id: "openai".to_owned(),
                label: "OpenAI".to_owned(),
            },
            ProviderEntry {
                id: "empty-provider".to_owned(),
                label: "Ohne Modelle".to_owned(),
            },
        ]
    }

    fn models_fixture() -> Vec<(String, Vec<ModelEntry>)> {
        vec![
            (
                "anthropic".to_owned(),
                vec![
                    ModelEntry {
                        id: "claude-sonnet".to_owned(),
                        label: "Claude Sonnet".to_owned(),
                    },
                    ModelEntry {
                        id: "claude-opus".to_owned(),
                        label: "Claude Opus".to_owned(),
                    },
                ],
            ),
            (
                "openai".to_owned(),
                vec![ModelEntry {
                    id: "gpt-5".to_owned(),
                    label: "GPT-5".to_owned(),
                }],
            ),
            ("empty-provider".to_owned(), vec![]),
        ]
    }

    /// `providers` leer → `new` liefert `None`.
    #[test]
    fn test_new_with_empty_providers_returns_none() {
        let picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            vec![],
            models_fixture(),
            None,
            None,
        );
        assert!(picker.is_none());
    }

    /// Provider-Stufe: `Up`/`Down` bleiben in Grenzen (kein Panic).
    #[test]
    fn test_provider_stage_navigation_stays_in_bounds() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        // Über das Ende hinaus.
        for _ in 0..10 {
            assert_eq!(picker.on_key(make_key(KeyCode::Down)), PickerAction::Stay);
        }
        // Zurück ans Anfang und darüber hinaus.
        for _ in 0..10 {
            assert_eq!(picker.on_key(make_key(KeyCode::Up)), PickerAction::Stay);
        }
        Ok(())
    }

    /// `Enter`/`Chosen` auf der Provider-Stufe wechselt zur Modell-Stufe mit
    /// korrekt vorausgewähltem Modell.
    #[test]
    fn test_enter_on_provider_switches_to_model_stage_with_preselection() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            Some("anthropic"),
            Some("claude-opus"),
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        // Startindex ist bereits auf "anthropic" (aktiver Provider) vorausgewählt;
        // Enter wählt ihn direkt.
        assert_eq!(picker.on_key(make_key(KeyCode::Enter)), PickerAction::Stay);
        assert_eq!(picker.stage, PickerStage::Model);

        // Modell-Vorauswahl war "claude-opus" (Index 1); Enter bestätigt es sofort.
        assert_eq!(
            picker.on_key(make_key(KeyCode::Enter)),
            PickerAction::Accept {
                provider: "anthropic".to_owned(),
                model: "claude-opus".to_owned(),
            }
        );
        Ok(())
    }

    /// `Left` in der Modell-Stufe springt zur Provider-Stufe zurück; der
    /// Provider-Cursor bleibt erhalten (kein Reset).
    #[test]
    fn test_left_in_model_stage_returns_to_provider_without_reset() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        // Provider-Cursor bewegen (Index 1 = "openai"), dann wählen.
        assert_eq!(picker.on_key(make_key(KeyCode::Down)), PickerAction::Stay);
        assert_eq!(picker.on_key(make_key(KeyCode::Enter)), PickerAction::Stay);
        assert_eq!(picker.stage, PickerStage::Model);

        assert_eq!(picker.on_key(make_key(KeyCode::Left)), PickerAction::Stay);
        assert_eq!(picker.stage, PickerStage::Provider);

        // Cursor blieb bei Index 1 ("openai"): Enter wählt erneut "openai" statt
        // auf Index 0 zurückgesetzt zu sein.
        assert_eq!(picker.on_key(make_key(KeyCode::Enter)), PickerAction::Stay);
        assert_eq!(picker.selected_provider.as_deref(), Some("openai"));
        Ok(())
    }

    /// `Left` bei `PickerTarget::UiaWorker` ist ein No-op (`Stay`), bleibt in
    /// der Modell-Stufe.
    #[test]
    fn test_left_on_uia_worker_target_is_noop() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::UiaWorker {
                fixed_provider: "anthropic".to_owned(),
            },
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(picker.stage, PickerStage::Model);
        assert_eq!(picker.on_key(make_key(KeyCode::Left)), PickerAction::Stay);
        assert_eq!(picker.stage, PickerStage::Model);
        Ok(())
    }

    /// `Enter` in der Modell-Stufe liefert `Accept` mit korrekten IDs.
    #[test]
    fn test_enter_in_model_stage_returns_accept_with_correct_ids() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::UiaWorker {
                fixed_provider: "openai".to_owned(),
            },
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(
            picker.on_key(make_key(KeyCode::Enter)),
            PickerAction::Accept {
                provider: "openai".to_owned(),
                model: "gpt-5".to_owned(),
            }
        );
        Ok(())
    }

    /// `Esc` auf der Provider-Stufe bricht vollständig ab.
    #[test]
    fn test_esc_on_provider_stage_returns_cancel() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(picker.on_key(make_key(KeyCode::Esc)), PickerAction::Cancel);
        Ok(())
    }

    /// `Esc` auf der Modell-Stufe bricht vollständig ab (kein Stufen-Rücksprung).
    #[test]
    fn test_esc_on_model_stage_returns_cancel() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(picker.on_key(make_key(KeyCode::Enter)), PickerAction::Stay);
        assert_eq!(picker.stage, PickerStage::Model);
        assert_eq!(picker.on_key(make_key(KeyCode::Esc)), PickerAction::Cancel);
        Ok(())
    }

    /// Provider ohne Modelle gewählt (Enter auf Provider-Stufe) → `Cancel`.
    #[test]
    fn test_choosing_provider_without_models_returns_cancel() -> TestResult {
        let mut picker = ModelSwitchPicker::new(
            PickerTarget::Orchestrator,
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        // Index 2 = "empty-provider" (keine Modelle in der Fixture).
        assert_eq!(
            picker.on_key(make_key(KeyCode::Char('3'))),
            PickerAction::Cancel
        );
        // Stage bleibt unverändert (kein Wechsel bei Abbruch).
        assert_eq!(picker.stage, PickerStage::Provider);
        Ok(())
    }

    /// `PickerTarget::UiaWorker`-Konstruktion startet direkt in
    /// `PickerStage::Model`; die Provider-Stufe ist nie über `render`/`on_key`
    /// erreichbar.
    #[test]
    fn test_uia_worker_starts_in_model_stage_provider_stage_unreachable() -> TestResult {
        let picker = ModelSwitchPicker::new(
            PickerTarget::UiaWorker {
                fixed_provider: "anthropic".to_owned(),
            },
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(picker.stage, PickerStage::Model);
        assert!(picker.model_dialog.is_some());

        // Rendering greift auf den Model-Dialog zu, nicht auf provider_dialog.
        let area = Rect::new(0, 0, 50, 8);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, style::Theme::Dark);
        let rendered: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");
        assert!(rendered.contains("Claude Sonnet"));
        assert!(!rendered.contains(PROVIDER_DIALOG_TITLE));
        Ok(())
    }

    /// `target()` liefert den bei der Konstruktion übergebenen Kontext zurück.
    #[test]
    fn test_target_getter_returns_constructed_target() -> TestResult {
        let picker = ModelSwitchPicker::new(
            PickerTarget::UiaWorker {
                fixed_provider: "openai".to_owned(),
            },
            providers_fixture(),
            models_fixture(),
            None,
            None,
        )
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        assert_eq!(
            picker.target(),
            &PickerTarget::UiaWorker {
                fixed_provider: "openai".to_owned(),
            }
        );
        Ok(())
    }
}

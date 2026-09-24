//! Modellwahl der UIA-Worker-Rollen in der TUI (Runde 5, Teil G).
//!
//! # Beschreibung
//! - `/models pick uia` öffnet die UIA-Modellwahl; nach der Bestätigung
//!   öffnet sich direkt der Bereich „UIA-Worker-Modelle“
//!   ([`crate::model_roles_view::ModelRolesView::uia_workers`]).
//! - Jede Wahl dort (Picker `PickerTarget::UiaWorkerRole`, `r`, `a`) wird
//!   gespeichert (`/models worker …`) **und** sofort an die
//!   Laufzeit-Wahl ([`harw_runtime::uia_worker_routing::UiaWorkerRouting`])
//!   gemeldet: neu gestartete Worker nehmen sie, laufende behalten ihr
//!   Modell.
//! - An jeder Turn-Grenze meldet [`sync_live_uia`] die Live-Auswahl der
//!   UIA-Wurzel, damit „wie UIA“-Worker einem UIA-Wechsel folgen.

use harw_config::{UIA_WORKER_ROLES, UiaWorkerModelChoice};
use harw_core::AgentSession;

use super::{ChatApp, Overlay};
use crate::model_roles_view::ModelRolesView;
use crate::model_switch_picker::PickerTarget;

impl ChatApp {
    /// Öffnet den Bereich „UIA-Worker-Modelle“.
    pub(super) fn open_uia_worker_models_view(&mut self) {
        let config = self.resolved_config();
        let view = ModelRolesView::uia_workers(config.as_deref());
        self.open_overlay_view(Box::new(view));
    }

    /// Öffnet die UIA-Modellwahl; nach ihrer Bestätigung folgt der Bereich
    /// „UIA-Worker-Modelle“ (`/models pick uia`).
    pub(super) fn open_uia_picker_then_workers(&mut self) {
        self.open_model_switch_picker(PickerTarget::Uia);
        if let Some(Overlay::ModelSwitch(picker)) = self.overlay.as_mut() {
            picker.set_uia_workers_follow_up();
        }
    }
}

/// Nach einem bestätigten Modell-Picker: Wahl live übernehmen und ggf. den
/// Worker-Bereich (wieder) öffnen.
///
/// # Argumente
/// - `command`: die abgeschickte Slash-Zeile des Pickers.
/// - `reopen_workers`: `ModelSwitchPicker::opens_uia_workers_after`.
pub(super) fn after_model_switch_accept(app: &mut ChatApp, command: &str, reopen_workers: bool) {
    observe_command(app, command);
    if reopen_workers {
        app.open_uia_worker_models_view();
    }
}

/// Meldet eine `/models worker <rolle|all> <uia|provider/modell>`-Zeile an
/// die Laufzeit-Wahl. Andere Zeilen, eine Wahl ohne Provider (sie löst erst
/// die Operation gegen den Katalog auf) oder eine TUI ohne Laufzeit bleiben
/// ohne Wirkung — gespeichert wird in jedem Fall über die Operation.
pub(super) fn observe_command(app: &ChatApp, command: &str) {
    let Some(runtime) = app.runtime() else {
        return;
    };
    let Some((roles, choice)) = parse_worker_command(command) else {
        return;
    };
    let routing = runtime.uia_worker_routing();
    if roles.len() == UIA_WORKER_ROLES.len() && choice.follows_uia() {
        routing.follow_uia_everywhere();
        return;
    }
    for role in roles {
        routing.set_choice(role, choice.clone());
    }
}

/// Zerlegt `/models worker <rolle|all> <wert>` in Rollen und Wahl.
///
/// # Rückgabe
/// `None` für jede andere Zeile, eine unbekannte Rolle oder einen Wert, der
/// weder `uia` noch `provider/modell` ist.
fn parse_worker_command(command: &str) -> Option<(Vec<&'static str>, UiaWorkerModelChoice)> {
    let mut words = command.split_whitespace();
    if words.next()? != "/models" || !matches!(words.next()?, "worker" | "workers") {
        return None;
    }
    let role = words.next()?.to_ascii_lowercase().replace('_', "-");
    let choice = UiaWorkerModelChoice::parse(words.next()?)?;
    if words.next().is_some() {
        return None;
    }
    let roles = if matches!(role.as_str(), "all" | "alle") {
        UIA_WORKER_ROLES.to_vec()
    } else {
        vec![UIA_WORKER_ROLES.into_iter().find(|known| *known == role)?]
    };
    Some((roles, choice))
}

/// Meldet die Live-Auswahl der UIA-Wurzel an die Laufzeit-Wahl (an jeder
/// Turn-Grenze nach dem Übernehmen der Controller-Auswahl).
pub(super) fn sync_live_uia(app: &ChatApp, session: &AgentSession) {
    let Some(runtime) = app.runtime() else {
        return;
    };
    runtime.uia_worker_routing().set_live_uia(
        session
            .active_provider()
            .map(|provider| provider.as_str().to_owned()),
        session
            .active_model()
            .map(|model| model.as_str().to_owned()),
    );
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_chat_app;
    use super::super::{Overlay, handle_key};
    use super::parse_worker_command;
    use crate::events::HarwEvent;
    use crate::events::harw_event_channel;
    use crate::model_switch_picker::{ModelEntry, ModelSwitchPicker, PickerTarget, ProviderEntry};
    use crate::test_support::{TestError, TestResult};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use harw_config::{UIA_WORKER_ROLES, UiaWorkerModelChoice};

    /// Zwei Provider mit je einem Modell.
    fn picker(target: PickerTarget) -> TestResult<ModelSwitchPicker> {
        ModelSwitchPicker::new(
            target,
            vec![
                ProviderEntry {
                    id: "anthropic".to_owned(),
                    label: "Anthropic".to_owned(),
                },
                ProviderEntry {
                    id: "openai".to_owned(),
                    label: "OpenAI".to_owned(),
                },
            ],
            vec![
                (
                    "anthropic".to_owned(),
                    vec![ModelEntry {
                        id: "claude-opus-5-5".to_owned(),
                        label: "Claude Opus 5.5".to_owned(),
                    }],
                ),
                (
                    "openai".to_owned(),
                    vec![ModelEntry {
                        id: "gpt-5".to_owned(),
                        label: "GPT-5".to_owned(),
                    }],
                ),
            ],
            Some("anthropic"),
            None,
        )
        .ok_or(TestError::Missing("picker"))
    }

    fn press(app: &mut super::ChatApp, code: KeyCode, bus: &crate::events::HarwEventSender) {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), bus);
    }

    fn overlay_is_worker_view(app: &super::ChatApp) -> bool {
        matches!(&app.overlay, Some(Overlay::View(view)) if format!("{view:?}").contains("UiaWorkers"))
    }

    /// Nach der UIA-Wahl aus `/models` (Providerwechsel auf `openai`) öffnet
    /// sich direkt der Bereich „UIA-Worker-Modelle“.
    #[test]
    fn uia_pick_from_models_opens_the_worker_section() -> TestResult {
        let mut app = test_chat_app()?;
        let mut uia = picker(PickerTarget::Uia)?;
        uia.set_uia_workers_follow_up();
        app.overlay = Some(Overlay::ModelSwitch(Box::new(uia)));
        let (bus, mut receiver) = harw_event_channel();

        press(&mut app, KeyCode::Down, &bus);
        press(&mut app, KeyCode::Enter, &bus);
        press(&mut app, KeyCode::Enter, &bus);

        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => assert_eq!(command, "/uia-model switch gpt-5"),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert!(
            overlay_is_worker_view(&app),
            "nach der UIA-Wahl muss der Worker-Bereich offen sein: {:?}",
            app.overlay
        );
        Ok(())
    }

    /// Eine Worker-Wahl wird über `/models worker` gespeichert, darf einen
    /// anderen Provider als die UIA nutzen und führt zurück in den
    /// Worker-Bereich.
    #[test]
    fn worker_role_pick_is_persisted_and_returns_to_the_worker_section() -> TestResult {
        let mut app = test_chat_app()?;
        let worker = picker(PickerTarget::UiaWorkerRole {
            role: "uia-shell-worker".to_owned(),
        })?;
        app.overlay = Some(Overlay::ModelSwitch(Box::new(worker)));
        let (bus, mut receiver) = harw_event_channel();

        press(&mut app, KeyCode::Down, &bus);
        press(&mut app, KeyCode::Enter, &bus);
        press(&mut app, KeyCode::Enter, &bus);

        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => {
                assert_eq!(command, "/models worker uia-shell-worker openai/gpt-5");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert!(overlay_is_worker_view(&app), "{:?}", app.overlay);
        Ok(())
    }

    /// Ein normaler UIA-Picker (`/uia-model`) öffnet danach nichts.
    #[test]
    fn plain_uia_pick_closes_without_worker_section() -> TestResult {
        let mut app = test_chat_app()?;
        app.overlay = Some(Overlay::ModelSwitch(Box::new(picker(PickerTarget::Uia)?)));
        let (bus, _receiver) = harw_event_channel();
        press(&mut app, KeyCode::Enter, &bus);
        press(&mut app, KeyCode::Enter, &bus);
        assert!(app.overlay.is_none());
        Ok(())
    }

    #[test]
    fn worker_command_parses_role_and_choice() {
        assert_eq!(
            parse_worker_command("/models worker uia-writer openai/gpt-5"),
            Some((
                vec!["uia-writer"],
                UiaWorkerModelChoice::Fixed {
                    provider: "openai".to_owned(),
                    model: "gpt-5".to_owned(),
                }
            ))
        );
        assert_eq!(
            parse_worker_command("/models worker all uia"),
            Some((UIA_WORKER_ROLES.to_vec(), UiaWorkerModelChoice::FollowUia))
        );
        assert_eq!(
            parse_worker_command("/models worker uia-writer gpt-5"),
            None
        );
        assert_eq!(
            parse_worker_command("/models worker host-process-worker uia"),
            None
        );
        assert_eq!(
            parse_worker_command("/models set uia-worker openai/gpt-5"),
            None
        );
        assert_eq!(parse_worker_command("/uia-model switch gpt-5"), None);
    }
}

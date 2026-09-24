//! Gemeinsame Tastenbehandlung der Eingabe-Popups (Runde 5, Teil G).
//!
//! # Beschreibung
//! Das `/command`-Popup und das `@`-Erwähnungs-Popup werden im Idle-Pfad
//! (`scroll_and_composer_key`) und im Busy-Pfad (`handle_busy_event`)
//! gleich bedient: beide rufen [`handle_popup_key`] als Erstes nach den
//! exklusiven Fenstern (sudo-Fenster, Overlay) auf. Vorher griff im
//! Busy-Pfad nur die Texteingabe — Hoch/Runter/Tab erreichten das Popup
//! nicht, und Esc schloss es nicht, weil `sync_popup` es sofort wieder
//! öffnete.
//!
//! # Belegung bei offenem `/command`-Popup
//! - **Hoch/Runter** (und Shift+Tab als „Hoch") bewegen die Markierung.
//! - **Tab** vervollständigt shell-artig ([`CommandPopup::tab_outcome`]).
//! - **Ziffern 1–9** wählen direkt, sofern das Popup sie zulässt.
//! - **Enter** übernimmt die markierte Auswahl in den Composer (ohne
//!   abzusenden). Im Unterkommando-Modus ohne Suchtext gibt es keine
//!   Übernahme; Enter geht dann an den Composer und sendet ab — im
//!   Busy-Pfad nach den Busy-Klassen (`route_busy_command`).
//! - **Esc** schließt nur das Popup (der Composer-Text bleibt).
//!
//! Tasten mit Shift/Alt (Transkript-Scroll, Alt+↑-Rückholung,
//! Shift/Alt+Enter-Zeilenumbruch) bleiben unberührt.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{ChatApp, handle_mention_popup_key, subcommand_query_is_empty};
use crate::command_popup::{CommandPopup, PopupAction, PopupMode, TabOutcome};

/// Behandelt eine Taste für ein offenes Eingabe-Popup.
///
/// # Beschreibung
/// Zuerst bekommt das `@`-Erwähnungs-Popup die Taste, danach das
/// `/command`-Popup (beide schließen sich gegenseitig aus). Nicht
/// verarbeitete Tasten (Texteingabe, Backspace, Enter ohne Auswahl …) gehen
/// an den Aufrufer zurück, der sie wie bisher an den Composer gibt.
///
/// # Rückgabe
/// `Some(redraw)`, wenn ein Popup die Taste verarbeitet hat; `None`, wenn
/// sie an den normalen Pfad weitergeht.
pub(super) fn handle_popup_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    if let Some(redraw) = handle_mention_popup_key(app, key) {
        return Some(redraw);
    }
    app.command_popup.as_ref()?;
    let shift_or_alt = key
        .modifiers
        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
    match key.code {
        // Das erste Escape schließt ausschließlich die Autovervollständigung.
        // Ein direkt folgendes Escape erreicht danach den Composer (Idle:
        // leert ihn; Busy: unterbricht den Turn).
        KeyCode::Esc => {
            app.command_popup = None;
            app.escape_armed = true;
            Some(true)
        }
        KeyCode::Tab if !shift_or_alt => {
            accept_tab(app);
            Some(true)
        }
        // Shift+Tab bleibt bei offenem Popup Popup-Navigation (kein
        // Freigabemodus-Zyklus); es wirkt wie „Hoch".
        KeyCode::BackTab => {
            apply_popup_action(app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            Some(true)
        }
        KeyCode::Up | KeyCode::Down if !shift_or_alt => {
            apply_popup_action(app, key);
            Some(true)
        }
        KeyCode::Char('1'..='9')
            if key.modifiers.is_empty()
                && app
                    .command_popup
                    .as_ref()
                    .is_some_and(CommandPopup::digits_select) =>
        {
            apply_popup_action(app, key);
            Some(true)
        }
        KeyCode::Enter if !shift_or_alt => {
            let line = enter_completion(app)?;
            app.input.clear();
            app.input.insert_str(&line);
            app.command_popup = None;
            Some(true)
        }
        _ => None,
    }
}

/// Zeile, die Enter bei offenem Popup übernimmt.
///
/// # Rückgabe
/// `None` im Unterkommando-Modus ohne getippten Suchtext (`/kanban `) oder
/// ohne Auswahl — dann sendet Enter die Zeile ab.
fn enter_completion(app: &ChatApp) -> Option<String> {
    let popup = app.command_popup.as_ref()?;
    if matches!(popup.mode(), PopupMode::Subcommand { .. })
        && subcommand_query_is_empty(app.input.text())
    {
        return None;
    }
    popup
        .selected_name()
        .map(|name| popup.completion_line(name))
}

/// Tab: unveränderte Markierung → Rang-/Präfixlogik
/// ([`CommandPopup::tab_outcome`]); bewusst bewegte Markierung → deren
/// Auswahl gilt.
fn accept_tab(app: &mut ChatApp) {
    let outcome = app
        .command_popup
        .as_ref()
        .map(|popup| match popup.tab_outcome() {
            TabOutcome::None => TabOutcome::None,
            TabOutcome::Accept(name) => TabOutcome::Accept(popup.completion_line(&name)),
            TabOutcome::ExtendQuery(common) => TabOutcome::ExtendQuery(popup.query_line(&common)),
        });
    match outcome.unwrap_or(TabOutcome::None) {
        TabOutcome::None => {}
        TabOutcome::Accept(line) => {
            app.input.clear();
            app.input.insert_str(&line);
            app.command_popup = None;
        }
        TabOutcome::ExtendQuery(line) => {
            app.input.clear();
            app.input.insert_str(&line);
            app.sync_popup();
        }
    }
}

/// Reicht Hoch/Runter/Ziffern an [`CommandPopup::on_key`] weiter und wendet
/// das Ergebnis an.
fn apply_popup_action(app: &mut ChatApp, key: KeyEvent) {
    let Some(popup) = app.command_popup.as_mut() else {
        return;
    };
    match popup.on_key(key) {
        PopupAction::Stay => {}
        PopupAction::Cancel => app.command_popup = None,
        PopupAction::Accept(name) => {
            let line = popup.completion_line(&name);
            app.input.clear();
            app.input.insert_str(&line);
            app.command_popup = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use harw_core::cancel::CancelToken;

    use super::super::tests::test_chat_app;
    use super::super::{BusyKeyOutcome, handle_busy_event};
    use crate::command_popup::CommandPopup;
    use crate::test_support::{TestError, TestResult};
    use crate::tui_event::TuiEvent;

    fn key(code: KeyCode) -> TuiEvent {
        TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn selected(app: &super::ChatApp) -> Option<String> {
        app.command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name)
            .map(str::to_owned)
    }

    /// Busy + `/mo` + Runter (bis `/models` markiert) + Tab → `/models`.
    #[test]
    fn busy_popup_down_and_tab_complete_models() -> TestResult {
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.input.insert_str("/mo");
        app.sync_popup();
        assert!(selected(&app).is_some(), "`/mo` muss das Popup öffnen");

        for _ in 0..30 {
            if selected(&app).as_deref() == Some("models") {
                break;
            }
            assert_eq!(
                handle_busy_event(&mut app, key(KeyCode::Down)),
                BusyKeyOutcome::Redraw
            );
        }
        assert_ne!(
            selected(&app),
            None,
            "Runter darf das Popup im Busy-Pfad nicht schließen"
        );
        if selected(&app).as_deref() != Some("models") {
            return Err(TestError::Unexpected(format!(
                "Runter muss `/models` erreichen, markiert: {:?}",
                selected(&app)
            )));
        }

        assert_eq!(
            handle_busy_event(&mut app, key(KeyCode::Tab)),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.input().trim_end(), "/models");
        assert!(app.command_popup.is_none());
        assert!(
            !cancel.is_cancelled(),
            "Popup-Tasten brechen den Turn nicht ab"
        );
        assert!(app.pending_turns.is_empty());
        Ok(())
    }

    /// Busy + Esc bei offenem Popup schließt nur das Popup: Turn läuft
    /// weiter, der Composer-Text bleibt stehen.
    #[test]
    fn busy_esc_closes_only_the_popup() -> TestResult {
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.input.insert_str("/mo");
        app.sync_popup();
        assert!(app.command_popup.is_some());

        assert_eq!(
            handle_busy_event(&mut app, key(KeyCode::Esc)),
            BusyKeyOutcome::Redraw
        );
        assert!(app.command_popup.is_none(), "Esc muss das Popup schließen");
        assert_eq!(app.input(), "/mo");
        assert!(
            !cancel.is_cancelled(),
            "das erste Esc darf bei offenem Popup den Turn nicht abbrechen"
        );

        // Ein zweites Esc (ohne Popup) unterbricht wie bisher den Turn.
        assert_eq!(
            handle_busy_event(&mut app, key(KeyCode::Esc)),
            BusyKeyOutcome::Redraw
        );
        assert!(cancel.is_cancelled());
        Ok(())
    }

    /// Hoch/Runter bewegen im Busy-Pfad die Markierung (vorher gingen sie an
    /// den Composer und ließen das Popup unverändert).
    #[test]
    fn busy_up_down_move_popup_selection() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        app.input.insert_str("/");
        app.sync_popup();
        let start = selected(&app).ok_or(TestError::Missing("Popup-Auswahl"))?;

        handle_busy_event(&mut app, key(KeyCode::Down));
        let after_down = selected(&app).ok_or(TestError::Missing("Popup-Auswahl"))?;
        assert_ne!(start, after_down, "Runter muss die Markierung bewegen");

        handle_busy_event(&mut app, key(KeyCode::Up));
        assert_eq!(selected(&app), Some(start), "Hoch muss zurückbewegen");
        assert_eq!(app.input(), "/", "Pfeiltasten ändern den Composer nicht");
        Ok(())
    }

    /// Enter übernimmt im Busy-Pfad zuerst die markierte Auswahl (wie im
    /// Idle-Pfad), ohne abzusenden; erst das nächste Enter sendet die Zeile
    /// über die Busy-Klassen ab.
    #[test]
    fn busy_enter_accepts_selection_before_submit() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        app.input.insert_str("/hel");
        app.sync_popup();
        assert_eq!(selected(&app).as_deref(), Some("help"));

        assert_eq!(
            handle_busy_event(&mut app, key(KeyCode::Enter)),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.input().trim_end(), "/help");
        assert!(app.command_popup.is_none());
        assert!(app.pending_turns.is_empty());
        assert!(app.deferred_input.is_empty());

        let _ = handle_busy_event(&mut app, key(KeyCode::Enter));
        assert!(app.input().is_empty(), "das zweite Enter sendet ab");
        assert!(
            app.pending_turns.is_empty(),
            "ein Befehl wird nie als Chat-Nachricht eingereiht"
        );
        Ok(())
    }

    /// Shift+Tab bleibt bei offenem Popup Popup-Navigation und schaltet den
    /// Freigabemodus nicht weiter.
    #[test]
    fn busy_shift_tab_with_popup_does_not_cycle_permission_mode() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        app.input.insert_str("/");
        app.sync_popup();
        let before = format!("{:?}", app.current_permission_stage());

        let outcome = handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        );
        assert_eq!(outcome, BusyKeyOutcome::Redraw);
        assert!(app.command_popup.is_some(), "Popup bleibt offen");
        assert_eq!(format!("{:?}", app.current_permission_stage()), before);
        Ok(())
    }

    /// Legt `/models`, `hallo`, `/new` in die History.
    fn app_with_history() -> TestResult<super::ChatApp> {
        let mut app = test_chat_app()?;
        for line in ["/models", "hallo", "/new"] {
            app.remember_input(line);
        }
        Ok(app)
    }

    /// Hoch/Runter blättern durch die History, auch über `/`-Einträge
    /// hinweg: kein Popup, solange der Inhalt ein unveränderter Eintrag ist;
    /// nach dem Editieren öffnet sich das Popup wieder (Idle-Pfad).
    #[test]
    fn idle_history_browsing_skips_slash_popup() -> TestResult {
        let mut app = app_with_history()?;
        let (bus, _receiver) = crate::events::harw_event_channel();
        let press = |app: &mut super::ChatApp, code| {
            super::super::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), &bus)
        };

        for expected in ["/new", "hallo", "/models"] {
            press(&mut app, KeyCode::Up);
            assert_eq!(app.input(), expected);
            assert!(
                app.command_popup.is_none(),
                "History-Eintrag {expected} darf kein Popup öffnen"
            );
        }
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input(), "hallo");
        press(&mut app, KeyCode::Down);
        assert_eq!(app.input(), "/new");
        assert!(app.command_popup.is_none());

        // Zurück zu `/models`, dann editieren: das Popup gilt wieder.
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.input(), "/models");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input(), "/model");
        assert!(
            app.command_popup.is_some(),
            "nach dem Editieren muss das Popup wieder aufgehen"
        );
        Ok(())
    }

    /// Dasselbe History-Blättern im Busy-Pfad.
    #[test]
    fn busy_history_browsing_skips_slash_popup() -> TestResult {
        let mut app = app_with_history()?;
        app.active_cancel = Some(CancelToken::new());

        for expected in ["/new", "hallo", "/models"] {
            handle_busy_event(&mut app, key(KeyCode::Up));
            assert_eq!(app.input(), expected);
            assert!(app.command_popup.is_none(), "{expected}: kein Popup");
        }
        handle_busy_event(&mut app, key(KeyCode::Down));
        assert_eq!(app.input(), "hallo");

        handle_busy_event(&mut app, key(KeyCode::Up));
        assert_eq!(app.input(), "/models");
        handle_busy_event(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.input(), "/model");
        assert!(
            app.command_popup.is_some(),
            "Tippen öffnet das Popup wieder"
        );
        assert!(app.pending_turns.is_empty());
        Ok(())
    }
}

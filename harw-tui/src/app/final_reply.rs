//! Schlussantwort eines Turns ohne Historie-Index (Runde 6, Teil C).
//!
//! # Verantwortungsbereich
//! Früher zog `drive_turn_animated` die Schlussantwort über
//! `latest_final_reply(history, history_len_before)` aus den Items **ab dem
//! Historie-Stand vor dem Turn**. Verkleinert eine Auto-Verdichtung den
//! Verlauf mitten im Turn (oder direkt nach der Antwort), liegt die Antwort
//! danach auf einem Index **unter** `history_len_before` — `.skip(..)` fand
//! nichts, `reveal_reply` entfiel, und die Antwort fehlte in TUI und Export.
//!
//! Jetzt gilt, in dieser Reihenfolge:
//! 1. **Ereignis:** Die letzte nicht-`Commentary` Assistant-Nachricht, die
//!    der Turn über `TurnEvent::ItemAdded` gemeldet hat
//!    ([`FinalReplyCapture::record_final`], gesetzt in `handle_turn_event`).
//!    Sie ist unabhängig davon, was eine Verdichtung mit dem Verlauf macht.
//! 2. **Index (unverändert):** Ohne Ereignis und ohne Verdichtung bleibt es
//!    beim bisherigen `latest_final_reply` ab `history_len_before`. Damit
//!    gilt weiterhin: ein Turn ohne neue Antwort zeigt nichts (nie die
//!    Antwort des Vor-Turns).
//! 3. **Rückfall nach Verdichtung:** Die letzte nicht-`Commentary`
//!    Assistant-Nachricht nach der letzten Nutzernachricht. Stimmt sie mit
//!    der zuletzt enthüllten Antwort überein, bleibt sie aus — eine alte
//!    Antwort erscheint nie doppelt.
//!
//! # Nebenläufigkeit
//! Reine Funktionen und ein `Default`-Zustand im `TurnEventState`; läuft auf
//! dem Thread der Ereignisschleife.

use harw_core::ConversationHistory;
use harw_protocol::items::TurnItem;

use super::{ChatApp, Role, latest_final_reply, visible_message_text};

/// Was ein Turn über seine Schlussantwort gemeldet hat.
///
/// # Beschreibung
/// Wird zu Turn-Beginn zurückgesetzt (`drive_turn_animated`) und von
/// `handle_turn_event` fortgeschrieben.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FinalReplyCapture {
    /// Text der letzten nicht-`Commentary` Assistant-Nachricht dieses Turns.
    reply: Option<String>,
    /// `true`, sobald dieser Turn eine Verdichtung gemeldet hat.
    compacted: bool,
}

impl FinalReplyCapture {
    /// Merkt sich eine Schlussantwort (die jüngste gewinnt).
    ///
    /// # Argumente
    /// - `text` (`String`): sichtbarer Text der Assistant-Nachricht.
    pub(super) fn record_final(&mut self, text: String) {
        self.reply = Some(text);
    }

    /// Merkt sich, dass der Verlauf in diesem Turn verdichtet wurde.
    pub(super) fn record_compaction(&mut self) {
        self.compacted = true;
    }
}

/// Wählt die zu enthüllende Schlussantwort eines Turns.
///
/// # Argumente
/// - `capture` (`&FinalReplyCapture`): Ereignisstand dieses Turns.
/// - `history` (`&ConversationHistory`): Sitzungsverlauf nach dem Turn.
/// - `history_len_before` (`usize`): Verlaufslänge vor dem Turn.
/// - `previous_reply` (`Option<&str>`): die zuletzt enthüllte Antwort.
///
/// # Rückgabe
/// `Some(text)` zum Enthüllen, `None`, wenn dieser Turn keine neue
/// Schlussantwort hat (siehe Moduldoku).
pub(super) fn select_final_reply(
    capture: &FinalReplyCapture,
    history: &ConversationHistory,
    history_len_before: usize,
    previous_reply: Option<&str>,
) -> Option<String> {
    if let Some(reply) = &capture.reply {
        return Some(reply.clone());
    }
    let shrunk = history.len() < history_len_before;
    if !capture.compacted && !shrunk {
        return latest_final_reply(history, history_len_before);
    }
    reply_after_last_user(history).filter(|reply| Some(reply.as_str()) != previous_reply)
}

/// Letzte nicht-`Commentary` Assistant-Nachricht nach der letzten
/// Nutzernachricht.
///
/// # Rückgabe
/// `None`, wenn nach der letzten Nutzernachricht keine solche Nachricht
/// steht (auch ohne jede Nutzernachricht wird nur bis zum Anfang gesucht).
fn reply_after_last_user(history: &ConversationHistory) -> Option<String> {
    history
        .items()
        .iter()
        .rev()
        .take_while(|item| !matches!(item, TurnItem::UserMessage(_)))
        .find_map(|item| match item {
            TurnItem::AssistantMessage(message)
                if message.phase != Some(harw_types::MessagePhase::Commentary) =>
            {
                Some(visible_message_text(&message.content))
            }
            _ => None,
        })
}

/// Schreibt die Schlussantwort als genau eine Assistant-Zelle (samt
/// `ExportEntry::Assistant`) und verwirft die Live-Vorschau.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt Zelle und Exporteintrag auf.
/// - `reply` (`&str`): die Schlussantwort.
pub(super) fn commit_final_reply(app: &mut ChatApp, reply: &str) {
    app.clear_live_stream();
    app.push_line(Role::Assistant, reply);
}

#[cfg(test)]
mod tests {
    use harw_core::ConversationHistory;
    use harw_protocol::TurnEvent;
    use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem};
    use harw_types::{ItemId, MessagePhase, TurnId};

    use super::super::tests::test_chat_app;
    use super::super::{ExportOutputFormat, TurnEventState, build_export, handle_turn_event};
    use super::*;
    use crate::export::{ExportEntry, ExportOptions};
    use crate::test_support::{TestError, TestResult};

    fn assistant_event(text: &str, phase: MessagePhase) -> TurnEvent {
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                phase: Some(phase),
            }),
        }
    }

    fn compaction_event(items_before: u32, items_after: u32) -> TurnEvent {
        TurnEvent::CompactionApplied {
            turn_id: None,
            reason: "auto".to_owned(),
            items_before,
            items_after,
            tokens_before: None,
            tokens_after: None,
            summarized: true,
            elided_results: 0,
        }
    }

    /// Ein langer Verlauf vor dem Turn (viele Items).
    fn long_history() -> ConversationHistory {
        let mut history = ConversationHistory::new();
        for index in 0..12 {
            history.push_user_text(format!("Frage {index}"));
            history
                .push_assistant_text(format!("Antwort {index}"), Some(MessagePhase::FinalAnswer));
        }
        history
    }

    /// Auto-Verdichtung mitten im Turn: der Verlauf schrumpft unter den
    /// Stand vor dem Turn, die Schlussantwort erscheint trotzdem — in der
    /// Anzeige und im Export.
    #[test]
    fn compaction_mid_turn_still_reveals_and_exports_the_final_reply() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let before = long_history();
        let history_len_before = before.len();

        // Turn: Zwischentext, Verdichtung, Schlussantwort.
        handle_turn_event(
            &mut app,
            &mut state,
            assistant_event("Ich schaue nach …", MessagePhase::Commentary),
        );
        handle_turn_event(&mut app, &mut state, compaction_event(26, 3));
        handle_turn_event(
            &mut app,
            &mut state,
            assistant_event("Die Schlussantwort.", MessagePhase::FinalAnswer),
        );

        // Verdichteter Verlauf: Zusammenfassung, Frage, Antwort.
        let mut after = ConversationHistory::new();
        after.push_user_text("[Zusammenfassung]");
        after.push_user_text("Aktuelle Frage");
        after.push_assistant_text("Die Schlussantwort.", Some(MessagePhase::FinalAnswer));
        assert!(after.len() < history_len_before);
        // Der alte Index-Weg hätte nichts gefunden.
        assert_eq!(latest_final_reply(&after, history_len_before), None);

        let reply = select_final_reply(&state.final_reply, &after, history_len_before, None)
            .ok_or(TestError::Missing("Schlussantwort trotz Verdichtung"))?;
        assert_eq!(reply, "Die Schlussantwort.");

        commit_final_reply(&mut app, &reply);
        assert!(
            app.export_entries
                .contains(&ExportEntry::Assistant("Die Schlussantwort.".to_owned())),
            "Schlussantwort fehlt im Export: {:?}",
            app.export_entries
        );
        let markdown = build_export(
            &app,
            &ExportOptions::default(),
            ExportOutputFormat::Markdown,
        );
        assert!(markdown.contains("Die Schlussantwort."), "{markdown}");
        Ok(())
    }

    /// Ohne `ItemAdded` (z. B. verpasstes Ereignis) greift nach einer
    /// Verdichtung der Rückfall auf das letzte Assistant-Item nach der
    /// letzten Nutzernachricht.
    #[test]
    fn after_compaction_the_fallback_finds_the_reply_after_the_last_user_message() {
        let mut capture = FinalReplyCapture::default();
        capture.record_compaction();
        let mut after = ConversationHistory::new();
        after.push_user_text("[Zusammenfassung]");
        after.push_assistant_text("Alte Antwort", Some(MessagePhase::FinalAnswer));
        after.push_user_text("Neue Frage");
        after.push_assistant_text("Zwischentext", Some(MessagePhase::Commentary));
        after.push_assistant_text("Neue Antwort", Some(MessagePhase::FinalAnswer));

        assert_eq!(
            select_final_reply(&capture, &after, 40, Some("Alte Antwort")),
            Some("Neue Antwort".to_owned())
        );
    }

    /// Der Rückfall zeigt nie eine alte Antwort doppelt: weder die zuletzt
    /// enthüllte noch eine vor der letzten Nutzernachricht.
    #[test]
    fn the_fallback_never_repeats_an_old_reply() {
        let mut capture = FinalReplyCapture::default();
        capture.record_compaction();

        let mut same = ConversationHistory::new();
        same.push_user_text("Frage");
        same.push_assistant_text("Antwort", Some(MessagePhase::FinalAnswer));
        assert_eq!(
            select_final_reply(&capture, &same, 40, Some("Antwort")),
            None
        );

        let mut no_new = ConversationHistory::new();
        no_new.push_assistant_text("Antwort vorher", Some(MessagePhase::FinalAnswer));
        no_new.push_user_text("Frage ohne Antwort");
        assert_eq!(select_final_reply(&capture, &no_new, 40, None), None);
    }

    /// Ohne Verdichtung bleibt der Index-Weg maßgeblich: ein Turn ohne neue
    /// Antwort zeigt nicht die des vorherigen Turns.
    #[test]
    fn without_compaction_a_turn_without_reply_shows_nothing() {
        let capture = FinalReplyCapture::default();
        let mut history = ConversationHistory::new();
        history.push_user_text("Frage");
        history.push_assistant_text("Antwort", Some(MessagePhase::FinalAnswer));
        let len = history.len();
        history.push_user_text("Zusammengefasster Warteschlangen-Turn");
        assert_eq!(select_final_reply(&capture, &history, len, None), None);
    }

    /// Das Ereignis gewinnt auch, wenn der Verlauf nicht geschrumpft ist,
    /// und eine `Commentary` wird nie als Schlussantwort gemerkt.
    #[test]
    fn the_event_wins_and_commentary_is_never_captured() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        handle_turn_event(
            &mut app,
            &mut state,
            assistant_event("Nur Zwischentext", MessagePhase::Commentary),
        );
        assert_eq!(state.final_reply, FinalReplyCapture::default());
        handle_turn_event(
            &mut app,
            &mut state,
            assistant_event("Fertig.", MessagePhase::FinalAnswer),
        );
        let history = ConversationHistory::new();
        assert_eq!(
            select_final_reply(&state.final_reply, &history, 0, None),
            Some("Fertig.".to_owned())
        );
        Ok(())
    }
}

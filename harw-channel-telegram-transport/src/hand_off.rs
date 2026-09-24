//! Transport-to-runtime bridge contracts for admitted Telegram events.
//!
//! These traits deliberately name only transport-agnostic channel payloads
//! plus Telegram's wire identifiers. The crate that composes a runtime owns
//! the consumer implementation; this transport crate owns implementations of
//! the outbound capability. Keeping the seam here avoids a dependency on
//! `harw-core` or `harw-sandbox`.

use crate::{TelegramCallback, TelegramTransportError, TransportResult};
use harw_channel::{ApprovalPrompt, InboundEvent, OutboundContent, PeerId, SessionKey};

/// Sink registered by a runtime bridge to receive events that passed Telegram
/// admission.
///
/// Implementors construct or resume the appropriate runtime session for the
/// supplied structural [`SessionKey`] and process the normalized
/// [`InboundEvent`]. This notification is intentionally synchronous and does
/// not expose runtime internals through the transport boundary.
pub trait AdmittedEventConsumer: Send + Sync {
    /// Handles one admitted event and its already-derived session key.
    fn handle_admitted(&self, key: SessionKey, event: InboundEvent);
}

/// Sink für Inline-Button-Klicks (`callback_query`).
///
/// Die Ingress-Pfade (Long-Poll und Webhook) reichen deduplizierte
/// Callbacks hierher weiter; die Laufzeit prüft das Freigabe-Token in
/// [`TelegramCallback::data`] gegen den Chat-/Nachrichtenkontext und
/// beantwortet die Query per `answerCallbackQuery`.
pub trait CallbackConsumer: Send + Sync {
    /// Verarbeitet einen Button-Klick.
    fn handle_callback(&self, callback: TelegramCallback);

    /// Meldet einen Callback, der sich nicht auf [`TelegramCallback`] abbilden
    /// ließ (z. B. ohne zugängliche Nachricht oder ohne `data`).
    ///
    /// Die Laufzeit sollte die Query dennoch per `answerCallbackQuery`
    /// beantworten, damit der Client nicht hängen bleibt. Standard: ignorieren.
    fn handle_unroutable_callback(&self, callback_id: String) {
        let _ = callback_id;
    }
}

/// Fehlerwert der Standardimplementierungen für nicht unterstützte Operationen.
fn unsupported(method: &'static str) -> TelegramTransportError {
    TelegramTransportError::ApiRejected {
        method,
        code: 0,
        description: "unsupported by this outbound".into(),
    }
}

/// Capability a runtime bridge uses to render output into a Telegram chat.
///
/// Telegram chat, message, and forum-topic identifiers remain primitive wire
/// values at this boundary. Implementations return [`TransportResult`] so the
/// caller can observe Bot API and local transport failures rather than
/// treating delivery as best effort.
pub trait TelegramOutbound: Send + Sync {
    /// Sends `content` to `chat_id`, optionally into forum topic `thread_id`.
    fn send(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        content: &OutboundContent,
    ) -> TransportResult<()>;

    /// Replaces the content of an existing Telegram message.
    fn edit(&self, chat_id: i64, message_id: i64, content: &OutboundContent)
    -> TransportResult<()>;

    /// Sendet eine an `approver` gebundene Freigabeanfrage mit Inline-Buttons
    /// und liefert die Telegram-`message_id` der gesendeten Nachricht.
    ///
    /// Standard: nicht unterstützt (`ApiRejected`, `code: 0`).
    fn send_approval(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        prompt: &ApprovalPrompt,
        approver: &PeerId,
    ) -> TransportResult<i64> {
        let _ = (chat_id, thread_id, prompt, approver);
        Err(unsupported("send_approval"))
    }

    /// Aktualisiert eine laufend gestreamte Antwort unter `stream_key`.
    ///
    /// Standard: keine Wirkung; erst [`Self::stream_finish`] sendet Text.
    fn stream_update(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        let _ = (chat_id, thread_id, stream_key, text);
        Ok(())
    }

    /// Schließt eine gestreamte Antwort mit ihrem endgültigen Text ab.
    ///
    /// Standard: sendet `text` als gewöhnliche Nachricht über [`Self::send`].
    fn stream_finish(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        let _ = stream_key;
        self.send(
            chat_id,
            thread_id,
            &OutboundContent::Message {
                markdown: text.to_owned(),
            },
        )
    }

    /// Markiert eine Freigabeanfrage als erledigt (Text ersetzen, Buttons
    /// entfernen). Standard: keine Wirkung.
    fn close_approval(
        &self,
        chat_id: i64,
        message_id: i64,
        outcome_text: &str,
    ) -> TransportResult<()> {
        let _ = (chat_id, message_id, outcome_text);
        Ok(())
    }

    /// Zeigt kurz „schreibt …“ im Chat an. Standard: keine Wirkung.
    fn typing(&self, chat_id: i64, thread_id: Option<i64>) -> TransportResult<()> {
        let _ = (chat_id, thread_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AdmittedEventConsumer, CallbackConsumer, TelegramOutbound};
    use crate::test_support::{TestError, TestResult};
    use crate::{TelegramCallback, TelegramTransportError, TransportResult};
    use harw_channel::{ApprovalPrompt, OutboundContent, PeerId};
    use std::sync::Mutex;

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[test]
    fn bridge_contracts_require_thread_safe_implementors() {
        assert_send_sync::<dyn AdmittedEventConsumer>();
        assert_send_sync::<dyn TelegramOutbound>();
        assert_send_sync::<dyn CallbackConsumer>();
    }

    #[derive(Default)]
    struct MinimalOutbound {
        sent: Mutex<Vec<(i64, Option<i64>, OutboundContent)>>,
    }

    impl TelegramOutbound for MinimalOutbound {
        fn send(
            &self,
            chat_id: i64,
            thread_id: Option<i64>,
            content: &OutboundContent,
        ) -> TransportResult<()> {
            if let Ok(mut sent) = self.sent.lock() {
                sent.push((chat_id, thread_id, content.clone()));
            }
            Ok(())
        }

        fn edit(&self, _: i64, _: i64, _: &OutboundContent) -> TransportResult<()> {
            Ok(())
        }
    }

    struct MinimalCallbacks;

    impl CallbackConsumer for MinimalCallbacks {
        fn handle_callback(&self, _: TelegramCallback) {}
    }

    #[test]
    fn default_outbound_methods_follow_the_contract() -> TestResult {
        let outbound = MinimalOutbound::default();
        let prompt = ApprovalPrompt {
            request_id: "work:1".to_owned(),
            summary: "Freigabe".to_owned(),
            risk: "elevated".to_owned(),
            actions: Vec::new(),
        };
        match outbound.send_approval(1, None, &prompt, &PeerId::from_str("7")) {
            Err(TelegramTransportError::ApiRejected {
                method,
                code,
                description,
            }) => {
                assert_eq!(method, "send_approval");
                assert_eq!(code, 0);
                assert_eq!(description, "unsupported by this outbound");
            }
            Ok(_) => return Err(TestError::Unexpected("send_approval succeeded".to_owned())),
            Err(_) => return Err(TestError::Unexpected("wrong error variant".to_owned())),
        }
        assert!(outbound.stream_update(1, Some(2), "turn-1", "teil").is_ok());
        assert!(outbound.close_approval(1, 5, "erledigt").is_ok());
        assert!(outbound.typing(1, None).is_ok());
        assert!(
            outbound
                .stream_finish(1, Some(2), "turn-1", "fertig")
                .is_ok()
        );
        let sent = outbound
            .sent
            .lock()
            .map(|sent| sent.clone())
            .map_err(|_| TestError::Unexpected("poisoned".to_owned()))?;
        assert_eq!(
            sent,
            vec![(
                1,
                Some(2),
                OutboundContent::Message {
                    markdown: "fertig".to_owned()
                }
            )]
        );

        MinimalCallbacks.handle_unroutable_callback("cb-1".to_owned());
        Ok(())
    }
}

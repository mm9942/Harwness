//! Transport-to-runtime bridge contracts for admitted Telegram events.
//!
//! These traits deliberately name only transport-agnostic channel payloads
//! plus Telegram's wire identifiers. The crate that composes a runtime owns
//! the consumer implementation; this transport crate owns implementations of
//! the outbound capability. Keeping the seam here avoids a dependency on
//! `harw-core` or `harw-sandbox`.

use crate::{TelegramCallback, TransportResult};
use harw_channel::{InboundEvent, OutboundContent, SessionKey};

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
}

#[cfg(test)]
mod tests {
    use super::{AdmittedEventConsumer, TelegramOutbound};

    fn assert_send_sync<T: Send + Sync + ?Sized>() {}

    #[test]
    fn bridge_contracts_require_thread_safe_implementors() {
        assert_send_sync::<dyn AdmittedEventConsumer>();
        assert_send_sync::<dyn TelegramOutbound>();
    }
}

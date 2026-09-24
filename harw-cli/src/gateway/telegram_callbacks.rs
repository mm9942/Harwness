//! Inline-Button-Klicks (`callback_query`) des Telegram-Gateways.
//!
//! # Ablauf
//! [`spawn_callback_worker`] startet einen eigenen `std`-Thread
//! ([`GatewayCallbackWorker`]) und liefert den nicht blockierenden
//! [`CallbackConsumer`] für Long-Poll- **und** Webhook-Ingress. Der Worker
//! prüft je Klick:
//!
//! 1. **Admission** — der Klickende ist in diesem Chat zugelassen
//!    (`TelegramChannel::is_sender_admitted`), der Chat ist sein DM oder
//!    eine erlaubte Gruppe (inkl. `group_allowed_senders`) und gepairt.
//!    Erst danach wird das Einmal-Token angefasst, damit ein fremder Klick
//!    kein gültiges Token entwerten kann.
//! 2. **Token** — `consume_bound_at` mit `peer` = Telegram-User-ID des
//!    Klickenden; danach `revoke_request`, damit die Geschwister-Schaltfläche
//!    (Freigeben/Ablehnen) derselben Anfrage tot ist.
//! 3. **Routing** — `request_id` `"work:<WorkId>"` an den
//!    [`WorkRequestStore`] (`*_as`, gleiches Binding/Tenant, Anfragender oder
//!    Admin), `"turn:<id>"` an den [`TurnApprovalSink`] der Modell-Sitzungen.
//! 4. **Abschluss** — `close_approval` ersetzt Text und Tastatur der
//!    Freigabe-Nachricht durch das deutsche Ergebnis; jede Query wird mit
//!    `answerCallbackQuery` beantwortet, auch nicht zuordenbare
//!    ([`CallbackItem::Unroutable`]).
//!
//! Die Callback-Nutzlast (`data`) wird nie geloggt.

use std::sync::{Arc, mpsc};

use jiff::Timestamp;

use harw_channel::PeerId;
use harw_channel_telegram::{
    ApprovalCallbackContext, TelegramChannel, TelegramChatId, TelegramMessageId, TelegramThreadId,
    WorkRequestActor, WorkRequestState, WorkRequestStore,
};
use harw_channel_telegram_transport::{
    CallbackConsumer, TelegramCallback, TelegramClient, TelegramOutbound,
};
use harw_types::{TenantId, WorkId};

use super::telegram_commands::work_request_error_text;

/// `request_id`-Präfix für Freigaben von Arbeitsaufträgen.
pub(super) const APPROVAL_KIND_WORK: &str = "work:";
/// `request_id`-Präfix für Werkzeug-Freigaben laufender Modell-Turns.
pub(super) const APPROVAL_KIND_TURN: &str = "turn:";

/// Kurzantwort (`answerCallbackQuery`), wenn eine Entscheidung ausgeführt wurde.
const CALLBACK_DONE_TEXT: &str = "Entscheidung übernommen";
/// Kurzantwort für unbekannte, abgelaufene, bereits verwendete, nicht zum
/// Nachrichtenkontext passende oder nicht zuordenbare Schaltflächen.
/// Unterscheidet die Ursachen bewusst nicht, damit ein Klickender den
/// Token-Raum nicht abtasten kann.
const CALLBACK_EXPIRED_TEXT: &str = "Schaltfläche ungültig oder abgelaufen";
/// Kurzantwort, wenn der Klickende nicht zugelassen oder der Chat nicht
/// gepairt ist; das Token wird dabei nicht verbraucht.
const CALLBACK_UNAUTHORIZED_TEXT: &str = "Keine Berechtigung für diese Entscheidung";
/// Kurzantwort, wenn die Entscheidung erkannt, aber nicht ausführbar war.
const CALLBACK_FAILED_TEXT: &str = "Entscheidung fehlgeschlagen";

/// Ein Eintrag der Worker-Warteschlange.
#[derive(Debug, Clone)]
pub(super) enum CallbackItem {
    /// Vollständig abgebildeter Klick.
    Routed(TelegramCallback),
    /// Nur die `callback_query.id` eines nicht abbildbaren Klicks; wird mit
    /// [`CALLBACK_EXPIRED_TEXT`] beantwortet.
    Unroutable(String),
}

/// Empfänger für Freigabe-Entscheidungen laufender Modell-Turns
/// (`request_id` `"turn:<id>"`), implementiert vom Session-Dispatcher.
pub(super) trait TurnApprovalSink: Send + Sync {
    /// Wendet die Entscheidung an.
    ///
    /// # Arguments
    /// - `request_id`: die `request_id` **ohne** `"turn:"`-Präfix.
    /// - `approve`: `true` für Freigeben, `false` für Ablehnen.
    /// - `actor`: Telegram-User-ID des Entscheidenden.
    ///
    /// # Errors
    /// Ein kurzer, nutzertauglicher deutscher Grund ohne Geheimnisse (z. B.
    /// Anfrage unbekannt oder bereits entschieden).
    fn resolve(&self, request_id: &str, approve: bool, actor: &PeerId) -> Result<String, String>;
}

/// Nicht blockierender Consumer: reicht Klicks nur an den Worker-Thread weiter.
struct GatewayCallbackConsumer {
    callbacks: mpsc::Sender<CallbackItem>,
}

impl CallbackConsumer for GatewayCallbackConsumer {
    fn handle_callback(&self, callback: TelegramCallback) {
        if self.callbacks.send(CallbackItem::Routed(callback)).is_err() {
            tracing::warn!("Telegram callback worker is gone; callback dropped unanswered");
        }
    }

    fn handle_unroutable_callback(&self, callback_id: String) {
        if self
            .callbacks
            .send(CallbackItem::Unroutable(callback_id))
            .is_err()
        {
            tracing::warn!(
                "Telegram callback worker is gone; unroutable callback dropped unanswered"
            );
        }
    }
}

/// Worker-Seite des Callback-Consumers; läuft auf einem eigenen Thread und
/// endet, sobald der Consumer (mit dem Transport) verworfen wird.
pub(super) struct GatewayCallbackWorker {
    /// Klon des Admission-Adapters: teilt dessen Approval-Token-Store und
    /// Pairing-Store, damit Klicks gegen dieselbe Grenze geprüft werden wie
    /// Text-Befehle.
    pub adapter: TelegramChannel,
    /// Derselbe Store wie für `/approve` und `/deny` als Text-Befehl.
    pub work_requests: Arc<WorkRequestStore>,
    pub outbound: Arc<dyn TelegramOutbound>,
    pub bot_client: Arc<TelegramClient>,
    /// Empfänger für `"turn:"`-Freigaben; `None`, solange keine
    /// Modell-Sitzungen Freigaben anfordern.
    pub turn_approvals: Option<Arc<dyn TurnApprovalSink>>,
}

/// Startet den Worker-Thread und liefert den zugehörigen Consumer.
///
/// # Errors
/// Eine inhaltsfreie Meldung, wenn der Thread nicht startet.
pub(super) fn spawn_callback_worker(
    worker: GatewayCallbackWorker,
) -> Result<Arc<dyn CallbackConsumer>, String> {
    let (callback_tx, callback_rx) = mpsc::channel::<CallbackItem>();
    std::thread::Builder::new()
        .name("harw-telegram-callback".to_owned())
        .spawn(move || worker.run(callback_rx))
        .map_err(|_| "Telegram callback thread could not start".to_owned())?;
    Ok(Arc::new(GatewayCallbackConsumer {
        callbacks: callback_tx,
    }))
}

/// Ergebnis der Entscheidung über einen Klick.
struct Decision {
    /// Kurzantwort für `answerCallbackQuery`.
    notice: &'static str,
    /// Neuer Text der Freigabe-Nachricht (Tastatur wird entfernt), falls das
    /// Token eingelöst wurde.
    outcome: Option<String>,
}

impl Decision {
    fn notice(notice: &'static str) -> Self {
        Self {
            notice,
            outcome: None,
        }
    }
}

impl GatewayCallbackWorker {
    fn run(self, callbacks: mpsc::Receiver<CallbackItem>) {
        // Eine kurzlebige Runtime je Worker: dieser Thread gehört keiner
        // Tokio-Runtime; `answer_callback_query` ist asynchron.
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(error = %error, "Telegram callback answer runtime could not start");
                return;
            }
        };
        for item in callbacks {
            match item {
                CallbackItem::Routed(callback) => {
                    let decision = self.decide(&callback);
                    if let Some(outcome) = decision.outcome.as_deref() {
                        self.close(&callback, outcome);
                    }
                    self.answer(&runtime, &callback.callback_id, decision.notice);
                }
                CallbackItem::Unroutable(callback_id) => {
                    tracing::debug!("Telegram unroutable callback answered as expired");
                    self.answer(&runtime, &callback_id, CALLBACK_EXPIRED_TEXT);
                }
            }
        }
    }

    /// Admission-Prüfung des Klickenden im Chat des Buttons. Liefert den
    /// gepairten Tenant oder die Kurzantwort der Ablehnung.
    fn admit(&self, callback: &TelegramCallback) -> Result<TenantId, &'static str> {
        let sender = callback.sender.id.as_str();
        let chat = PeerId::from_str(callback.chat_id.to_string());
        let config = self.adapter.config();
        match self.adapter.is_sender_admitted(sender, &chat) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("Telegram callback from a non-admitted sender rejected");
                return Err(CALLBACK_UNAUTHORIZED_TEXT);
            }
            Err(error) => {
                tracing::error!(error = %error, "Telegram callback admission lookup failed");
                return Err(CALLBACK_FAILED_TEXT);
            }
        }
        if config.is_group(&chat) {
            if !config.group_allowed_senders.is_empty()
                && !config.group_allowed_senders.contains(sender)
            {
                tracing::warn!(
                    "Telegram callback from a sender outside group_allowed_senders rejected"
                );
                return Err(CALLBACK_UNAUTHORIZED_TEXT);
            }
        } else if chat.as_str() != sender {
            // Weder eigener DM noch erlaubte Gruppe.
            tracing::warn!("Telegram callback from a non-allowed chat rejected");
            return Err(CALLBACK_UNAUTHORIZED_TEXT);
        }
        match self.adapter.resolve_tenant(&chat) {
            Ok(Some(tenant)) => Ok(tenant),
            Ok(None) => {
                tracing::warn!("Telegram callback from an unpaired chat rejected");
                Err(CALLBACK_UNAUTHORIZED_TEXT)
            }
            Err(error) => {
                tracing::error!(error = %error, "Telegram callback pairing lookup failed");
                Err(CALLBACK_FAILED_TEXT)
            }
        }
    }

    /// Prüft Admission und Token-Kontext und führt eine erkannte Entscheidung aus.
    fn decide(&self, callback: &TelegramCallback) -> Decision {
        let tenant = match self.admit(callback) {
            Ok(tenant) => tenant,
            Err(notice) => return Decision::notice(notice),
        };
        let actor_peer = PeerId::from_str(callback.sender.id.clone());
        let mut context = ApprovalCallbackContext::new(
            TelegramChatId(callback.chat_id),
            TelegramMessageId(callback.message_id),
            actor_peer.clone(),
        );
        if let Some(thread_id) = callback.thread_id {
            context = context.with_thread(TelegramThreadId(thread_id));
        }
        let tokens = self.adapter.approval_tokens();
        let Some(pending) = tokens.consume_bound_at(&callback.data, &context, Timestamp::now())
        else {
            tracing::warn!("Telegram callback token is unknown, stale, or out of context");
            return Decision::notice(CALLBACK_EXPIRED_TEXT);
        };
        // Die Geschwister-Schaltflächen derselben Anfrage sind ab jetzt tot.
        tokens.revoke_request(&pending.request_id);

        let approve = match pending.decision.as_str() {
            "approve" => true,
            "deny" => false,
            _ => {
                tracing::warn!("Telegram callback carried an unsupported approval decision");
                return Decision {
                    notice: CALLBACK_FAILED_TEXT,
                    outcome: Some("Entscheidung fehlgeschlagen: unbekannte Aktion.".to_owned()),
                };
            }
        };
        let result = if let Some(work_id) = pending.request_id.strip_prefix(APPROVAL_KIND_WORK) {
            let actor = WorkRequestActor {
                channel: self.adapter.config().channel_id.clone(),
                tenant,
                peer: actor_peer,
                is_admin: self.adapter.config().is_admin_sender(&callback.sender.id),
            };
            self.decide_work(&WorkId::from_str(work_id), &actor, approve)
        } else if let Some(turn_id) = pending.request_id.strip_prefix(APPROVAL_KIND_TURN) {
            match self.turn_approvals.as_ref() {
                Some(sink) => sink.resolve(turn_id, approve, &actor_peer),
                None => {
                    tracing::warn!("Telegram turn approval callback without an installed sink");
                    Err("Diese Freigabe wird nicht mehr erwartet.".to_owned())
                }
            }
        } else {
            tracing::warn!("Telegram callback carried an unknown approval kind");
            Err("Unbekannte Freigabe.".to_owned())
        };
        match result {
            Ok(message) => {
                let verdict = if approve { "Freigegeben" } else { "Abgelehnt" };
                Decision {
                    notice: CALLBACK_DONE_TEXT,
                    outcome: Some(format!("{verdict}: {message}")),
                }
            }
            Err(reason) => Decision {
                notice: CALLBACK_FAILED_TEXT,
                outcome: Some(format!("Entscheidung fehlgeschlagen: {reason}")),
            },
        }
    }

    /// Wendet eine Button-Entscheidung auf einen Arbeitsauftrag an. Ein noch
    /// `Requested`-Auftrag wird beim Freigeben zuerst in Prüfung gesetzt: die
    /// Freigabe-Nachricht ist die Prüfansicht.
    fn decide_work(
        &self,
        work_id: &WorkId,
        actor: &WorkRequestActor,
        approve: bool,
    ) -> Result<String, String> {
        let now = Timestamp::now();
        let result = if approve {
            self.work_requests
                .authorize(work_id, actor)
                .and_then(|record| {
                    if record.state == WorkRequestState::Requested {
                        self.work_requests.review_as(work_id, actor, now)?;
                    }
                    self.work_requests.approve_as(work_id, actor, now)
                })
        } else {
            self.work_requests.deny_as(work_id, actor, now)
        };
        result.map_err(|error| {
            tracing::warn!(error = %error, "Telegram callback work-request decision failed");
            work_request_error_text(&error)
        })
    }

    /// Ersetzt Text und Tastatur der Freigabe-Nachricht; scheitert das,
    /// folgt das Ergebnis als eigene Nachricht.
    fn close(&self, callback: &TelegramCallback, outcome: &str) {
        if let Err(error) =
            self.outbound
                .close_approval(callback.chat_id, callback.message_id, outcome)
        {
            tracing::warn!(error = %error, "Telegram approval message could not be closed");
            if let Err(error) = self.outbound.send(
                callback.chat_id,
                callback.thread_id,
                &harw_channel::OutboundContent::Message {
                    markdown: outcome.to_owned(),
                },
            ) {
                tracing::error!(error = %error, "Telegram callback decision reply delivery failed");
            }
        }
    }

    /// Beantwortet die Query, damit der Ladeindikator beim Nutzer endet.
    fn answer(&self, runtime: &tokio::runtime::Runtime, callback_id: &str, text: &str) {
        if let Err(error) = runtime.block_on(self.bot_client.answer_callback_query(
            callback_id,
            Some(text),
            false,
        )) {
            tracing::warn!(error = %error, "Telegram callback query could not be answered");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_kinds_are_disjoint_prefixes() {
        assert!(!APPROVAL_KIND_WORK.starts_with(APPROVAL_KIND_TURN));
        assert!(!APPROVAL_KIND_TURN.starts_with(APPROVAL_KIND_WORK));
        assert!(APPROVAL_KIND_WORK.ends_with(':') && APPROVAL_KIND_TURN.ends_with(':'));
    }

    #[test]
    fn consumer_forwards_routed_and_unroutable_callbacks() {
        let (tx, rx) = mpsc::channel();
        let consumer = GatewayCallbackConsumer { callbacks: tx };
        consumer.handle_unroutable_callback("cb-1".to_owned());
        consumer.handle_callback(TelegramCallback {
            update_id: 1,
            callback_id: "cb-2".to_owned(),
            data: "opaque".to_owned(),
            chat_id: 5,
            message_id: 6,
            thread_id: None,
            sender: harw_channel::SenderRef {
                id: "5".to_owned(),
                display_name: None,
            },
        });
        let items: Vec<CallbackItem> = rx.try_iter().collect();
        assert!(matches!(items.first(), Some(CallbackItem::Unroutable(id)) if id == "cb-1"));
        assert!(
            matches!(items.get(1), Some(CallbackItem::Routed(callback)) if callback.callback_id == "cb-2")
        );
        // Ohne Worker: kein Panik, nur ein Log.
        drop(rx);
        consumer.handle_unroutable_callback("cb-3".to_owned());
    }
}

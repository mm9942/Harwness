//! Async Telegram webhook ingress.
//!
//! Authentication is deliberately performed against the request headers before
//! the body is read. The resolved secret is supplied by the caller; this
//! boundary does not resolve `SecretRef` values or touch gateway state.
//!
//! Ein leer konfiguriertes Secret authentifiziert niemals: jede Anfrage wird
//! dann mit 401 abgewiesen (fail-closed).
//!
//! Nachrichten-Updates laufen wie beim Long-Poll durch das Dedup-Fenster
//! (`update_id`), sodass von Telegram wiederholt zugestellte Updates nur
//! einmal weitergereicht werden. Konnte ein beanspruchtes Update wegen eines
//! vollen Sinks nicht übergeben werden (503), darf genau dessen erneute
//! Zustellung die Deduplizierung einmal passieren.
//!
//! Inline-Button-Klicks (`callback_query`) werden – sofern ein
//! [`CallbackConsumer`] installiert ist – nach erfolgreicher Dedup-Claim ihrer
//! `update_id` an diesen weitergereicht; nicht zuordenbare Klicks gehen mit
//! ihrer Query-ID an [`CallbackConsumer::handle_unroutable_callback`]. Die
//! opake Callback-Nutzlast wird dabei niemals geloggt.

use std::{
    collections::HashSet,
    future::Future,
    net::SocketAddr,
    sync::{Arc, Mutex, mpsc::SyncSender},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header::HeaderValue},
    response::{IntoResponse, Response},
    routing::post,
};
use harw_channel::{ChannelId, InboundEvent};

use crate::dedup::DedupWindow;
use crate::hand_off::CallbackConsumer;
use crate::mapping::{RawUpdate, callback_query_id, map_callback_query, map_update};

const SECRET_HEADER: &str = "X-Telegram-Bot-Api-Secret-Token";
const MAX_UPDATE_BYTES: usize = 1_048_576;
/// Standardgröße des Dedup-Fensters für Callback-Updates (wie Long-Poll).
const DEFAULT_DEDUP_CAPACITY: usize = 1_024;
/// Höchstzahl gemerkter Update-IDs, deren Übergabe an den Sink scheiterte und
/// deren erneute Zustellung die Deduplizierung einmal passieren darf.
const MAX_REDELIVERY_ALLOWANCES: usize = 1_024;

/// Runtime inputs for one Telegram webhook binding.
#[derive(Clone)]
pub struct WebhookConfig {
    /// Local address on which the webhook listener binds.
    pub bind_addr: SocketAddr,
    /// Route at which Telegram delivers updates.
    pub path: String,
    /// Resolved Telegram webhook secret. Secret references must be resolved by
    /// the caller before constructing this value.
    pub webhook_secret: String,
    /// Harness-side channel binding identifier.
    pub channel_id: String,
    /// Telegram bot identity used by mention and reply mapping.
    pub bot_id: i64,
    pub bot_username: Option<String>,
    /// Bounded synchronous sink owned by the runtime bridge.
    pub sender: SyncSender<InboundEvent>,
    /// Optionaler Empfänger für Inline-Button-Klicks. Ohne Consumer werden
    /// `callback_query`-Updates angenommen (204) und verworfen.
    pub callback_consumer: Option<Arc<dyn CallbackConsumer>>,
    /// Prozesslokales Replay-Fenster für Nachrichten- und
    /// Callback-`update_id`s. Wird über Klone der Konfiguration (Axum-State)
    /// hinweg geteilt.
    pub dedup: Arc<DedupWindow>,
    /// Update-IDs, die zwar beansprucht, aber wegen eines vollen oder
    /// getrennten Sinks nicht übergeben wurden (Antwort 503). Deren erneute
    /// Zustellung durch Telegram darf die Deduplizierung einmal passieren.
    redelivery_allowances: Arc<Mutex<HashSet<i64>>>,
}

impl WebhookConfig {
    /// Creates a webhook binding from already-resolved runtime values.
    #[must_use]
    pub fn new(
        bind_addr: SocketAddr,
        path: impl Into<String>,
        webhook_secret: impl Into<String>,
        channel_id: impl Into<String>,
        bot_id: i64,
        bot_username: Option<String>,
        sender: SyncSender<InboundEvent>,
    ) -> Self {
        Self {
            bind_addr,
            path: path.into(),
            webhook_secret: webhook_secret.into(),
            channel_id: channel_id.into(),
            bot_id,
            bot_username,
            sender,
            callback_consumer: None,
            dedup: Arc::new(DedupWindow::new(DEFAULT_DEDUP_CAPACITY)),
            redelivery_allowances: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Installiert den Empfänger für Inline-Button-Klicks.
    ///
    /// Der Aufrufer muss `callback_query` zusätzlich in `allowed_updates` von
    /// `setWebhook` aufnehmen, sonst stellt Telegram keine Klicks zu.
    #[must_use]
    pub fn with_callback_consumer(mut self, consumer: Arc<dyn CallbackConsumer>) -> Self {
        self.callback_consumer = Some(consumer);
        self
    }

    /// Ersetzt das Dedup-Fenster, z. B. um es mit dem Long-Poll-Pfad zu teilen.
    #[must_use]
    pub fn with_dedup_window(mut self, dedup: Arc<DedupWindow>) -> Self {
        self.dedup = dedup;
        self
    }
}

/// Builds the isolated Telegram webhook router without activating any server.
///
/// `#[must_use]` is omitted here: `axum::Router` already carries its own
/// `#[must_use]`, so a second, message-less attribute on this function would
/// only duplicate that warning (clippy::double_must_use).
pub fn webhook_router(config: WebhookConfig) -> Router {
    let path = config.path.clone();
    Router::new()
        .route(&path, post(webhook_handler))
        .with_state(config)
}

/// Binds and serves the Telegram webhook listener until the server exits.
///
/// Entspricht [`run_webhook_server_with_shutdown`] mit einem niemals
/// abschließenden Shutdown-Future.
pub async fn run_webhook_server(config: WebhookConfig) -> std::io::Result<()> {
    run_webhook_server_with_shutdown(config, std::future::pending()).await
}

/// Bindet den Webhook-Listener und bedient ihn, bis `shutdown` abschließt.
///
/// Danach nimmt der Server keine neuen Verbindungen mehr an; laufende
/// Anfragen werden über Axums `with_graceful_shutdown` noch zu Ende bedient.
pub async fn run_webhook_server_with_shutdown(
    config: WebhookConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if config.webhook_secret.is_empty() {
        tracing::warn!(
            channel = %config.channel_id,
            "Telegram-Webhook ohne konfiguriertes Secret: alle Anfragen werden mit 401 abgewiesen"
        );
    }
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    axum::serve(listener, webhook_router(config))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn webhook_handler(State(config): State<WebhookConfig>, request: Request<Body>) -> Response {
    // Ein leeres konfiguriertes Secret authentifiziert niemals (fail-closed),
    // auch nicht gegen einen ebenfalls leeren Header.
    let authenticated = !config.webhook_secret.is_empty()
        && request
            .headers()
            .get(SECRET_HEADER)
            .is_some_and(|provided| secret_matches(provided, config.webhook_secret.as_bytes()));
    if !authenticated {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let body = match to_bytes(request.into_body(), MAX_UPDATE_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let update = match serde_json::from_slice::<RawUpdate>(&body) {
        Ok(update) => update,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Some(mut event) = map_update(&update, config.bot_id, config.bot_username.as_deref()) else {
        dispatch_callback(&config, &update);
        return StatusCode::NO_CONTENT.into_response();
    };
    event.channel = ChannelId::from_str(config.channel_id.clone());

    if !claim_message_update(&config, update.update_id) {
        // Bereits übergeben: Telegram erneut quittieren, nicht doppelt senden.
        return StatusCode::NO_CONTENT.into_response();
    }

    match config.sender.try_send(event) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(
            std::sync::mpsc::TrySendError::Full(_) | std::sync::mpsc::TrySendError::Disconnected(_),
        ) => {
            allow_redelivery(&config, update.update_id);
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// Beansprucht die `update_id` eines Nachrichten-Updates im Dedup-Fenster.
///
/// Scheitert der Claim, darf das Update trotzdem passieren, wenn seine
/// vorige Zustellung am Sink scheiterte (einmalige Erlaubnis).
fn claim_message_update(config: &WebhookConfig, update_id: i64) -> bool {
    if config.dedup.claim_update(update_id) {
        return true;
    }
    let mut allowances = config
        .redelivery_allowances
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    allowances.remove(&update_id)
}

/// Merkt eine beanspruchte, aber nicht übergebene `update_id` für genau eine
/// erneute Zustellung vor. Ist die Merkliste voll, entfällt die Erlaubnis;
/// die dauerhafte Replay-Prüfung stromabwärts bleibt davon unberührt.
fn allow_redelivery(config: &WebhookConfig, update_id: i64) {
    let mut allowances = config
        .redelivery_allowances
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if allowances.len() < MAX_REDELIVERY_ALLOWANCES {
        allowances.insert(update_id);
    } else {
        tracing::warn!(
            update_id,
            "Telegram-Webhook: Merkliste für erneute Zustellungen voll"
        );
    }
}

/// Reicht eine abbildbare `callback_query` an den installierten Consumer
/// weiter, sofern ihre `update_id` noch nicht beansprucht wurde.
///
/// Die Nutzlast (`data`) ist ein Approval-Token und wird bewusst nicht
/// geloggt. Duplikate und Updates ohne Consumer werden still verworfen. Eine
/// nicht zuordenbare Callback-Query geht nach der Deduplizierung mit ihrer
/// Query-ID an [`CallbackConsumer::handle_unroutable_callback`].
fn dispatch_callback(config: &WebhookConfig, update: &RawUpdate) {
    let Some(consumer) = config.callback_consumer.as_ref() else {
        return;
    };
    match map_callback_query(update) {
        Some(callback) => {
            if config.dedup.claim_update(update.update_id) {
                consumer.handle_callback(callback);
            }
        }
        None => {
            if let Some(callback_id) = callback_query_id(update)
                && config.dedup.claim_update(update.update_id)
            {
                consumer.handle_unroutable_callback(callback_id);
            }
        }
    }
}

fn secret_matches(provided: &HeaderValue, expected: &[u8]) -> bool {
    let provided = provided.as_bytes();
    let mut difference: u8 = u8::from(provided.len() != expected.len());
    for index in 0..provided.len().max(expected.len()) {
        difference |= provided.get(index).copied().unwrap_or_default()
            ^ expected.get(index).copied().unwrap_or_default();
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use axum::body::Body;
    use axum::http::Request;
    use harw_channel::InboundEvent;

    fn config(sender: SyncSender<InboundEvent>) -> TestResult<WebhookConfig> {
        Ok(WebhookConfig::new(
            "127.0.0.1:0"
                .parse()
                .map_err(ctx("test address is valid"))?,
            "/telegram",
            "correct-secret",
            "telegram:support",
            700,
            Some("harwbot".to_owned()),
            sender,
        ))
    }

    fn update_body() -> &'static str {
        r#"{"update_id":42,"message":{"message_id":9,"chat":{"id":123,"type":"private"},"from":{"id":8,"is_bot":false,"first_name":"Mia"},"text":"hello"}}"#
    }

    async fn call(config: WebhookConfig, request: Request<Body>) -> StatusCode {
        webhook_handler(State(config), request).await.status()
    }

    fn request(secret: Option<&str>, body: &'static str) -> TestResult<Request<Body>> {
        let mut request = Request::builder()
            .uri("/telegram")
            .body(Body::from(body))
            .map_err(ctx("request"))?;
        if let Some(secret) = secret {
            request.headers_mut().insert(
                SECRET_HEADER,
                HeaderValue::from_str(secret).map_err(ctx("test secret is valid"))?,
            );
        }
        Ok(request)
    }

    #[tokio::test]
    async fn missing_secret_returns_401_before_body_parse() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            call(config(sender)?, request(None, "not json")?).await,
            StatusCode::UNAUTHORIZED
        );
        Ok(())
    }

    #[tokio::test]
    async fn valid_update_maps_and_forwards() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            call(
                config(sender)?,
                request(Some("correct-secret"), update_body())?
            )
            .await,
            StatusCode::NO_CONTENT
        );
        let event = receiver.try_recv().map_err(ctx("mapped event forwarded"))?;
        assert_eq!(event.channel.as_str(), "telegram:support");
        assert_eq!(event.peer.as_str(), "123");
        assert_eq!(event.text.as_deref(), Some("hello"));
        Ok(())
    }

    #[tokio::test]
    async fn malformed_body_is_rejected_without_forwarding() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            call(
                config(sender)?,
                request(Some("correct-secret"), "not json")?
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_update_is_accepted_without_forwarding() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            call(
                config(sender)?,
                request(
                    Some("correct-secret"),
                    r#"{"update_id":43,"callback_query":{"id":"query","from":{"id":8,"is_bot":false,"first_name":"Mia"}}}"#,
                )?,
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn full_sink_returns_service_unavailable_without_blocking() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(InboundEvent {
                channel: ChannelId::from_str("already-full"),
                peer: harw_channel::PeerId::from_str("1"),
                thread: None,
                sender: None,
                text: None,
                mentioned: false,
                attachments: Vec::new(),
                raw_event_id: None,
                received_at: jiff::Timestamp::now(),
            })
            .map_err(ctx("fill bounded test sink"))?;
        assert_eq!(
            call(
                config(sender)?,
                request(Some("correct-secret"), update_body())?
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        Ok(())
    }

    fn config_with_secret(
        sender: SyncSender<InboundEvent>,
        secret: &str,
    ) -> TestResult<WebhookConfig> {
        let mut config = config(sender)?;
        config.webhook_secret = secret.to_owned();
        Ok(config)
    }

    #[tokio::test]
    async fn empty_configured_secret_always_returns_401() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let config = config_with_secret(sender, "")?;
        for secret in [None, Some(""), Some("anything")] {
            assert_eq!(
                call(config.clone(), request(secret, update_body())?).await,
                StatusCode::UNAUTHORIZED,
                "header {secret:?}"
            );
        }
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_message_update_is_forwarded_once() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let config = config(sender)?;
        for _ in 0..2 {
            assert_eq!(
                call(
                    config.clone(),
                    request(Some("correct-secret"), update_body())?
                )
                .await,
                StatusCode::NO_CONTENT
            );
        }
        receiver
            .try_recv()
            .map_err(ctx("first delivery forwarded"))?;
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn redelivery_after_full_sink_is_forwarded_once() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let config = config(sender.clone())?;
        sender
            .try_send(InboundEvent {
                channel: ChannelId::from_str("already-full"),
                peer: harw_channel::PeerId::from_str("1"),
                thread: None,
                sender: None,
                text: None,
                mentioned: false,
                attachments: Vec::new(),
                raw_event_id: None,
                received_at: jiff::Timestamp::now(),
            })
            .map_err(ctx("fill bounded test sink"))?;
        assert_eq!(
            call(
                config.clone(),
                request(Some("correct-secret"), update_body())?
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        receiver.try_recv().map_err(ctx("drain filler event"))?;
        // Telegram stellt nach 503 erneut zu: diesmal muss es durchgehen …
        assert_eq!(
            call(
                config.clone(),
                request(Some("correct-secret"), update_body())?
            )
            .await,
            StatusCode::NO_CONTENT
        );
        let event = receiver.try_recv().map_err(ctx("redelivery forwarded"))?;
        assert_eq!(event.channel.as_str(), "telegram:support");
        // … eine weitere Zustellung ist wieder ein Duplikat.
        assert_eq!(
            call(config, request(Some("correct-secret"), update_body())?).await,
            StatusCode::NO_CONTENT
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn server_stops_when_shutdown_future_completes() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        run_webhook_server_with_shutdown(config(sender)?, std::future::ready(()))
            .await
            .map_err(ctx("server shuts down cleanly"))?;
        Ok(())
    }

    #[derive(Default)]
    struct RecordingConsumer {
        callbacks: std::sync::Mutex<Vec<crate::mapping::TelegramCallback>>,
        unroutable: std::sync::Mutex<Vec<String>>,
    }

    impl CallbackConsumer for RecordingConsumer {
        fn handle_callback(&self, callback: crate::mapping::TelegramCallback) {
            if let Ok(mut callbacks) = self.callbacks.lock() {
                callbacks.push(callback);
            }
        }

        fn handle_unroutable_callback(&self, callback_id: String) {
            if let Ok(mut unroutable) = self.unroutable.lock() {
                unroutable.push(callback_id);
            }
        }
    }

    impl RecordingConsumer {
        fn recorded(&self) -> TestResult<Vec<crate::mapping::TelegramCallback>> {
            self.callbacks
                .lock()
                .map(|callbacks| callbacks.clone())
                .map_err(ctx("recording consumer lock"))
        }
    }

    fn callback_body() -> &'static str {
        r#"{"update_id":77,"callback_query":{"id":"cb-1","from":{"id":8,"is_bot":false,"first_name":"Mia"},"message":{"message_id":9,"chat":{"id":123,"type":"private"},"text":"approve?"},"data":"opaque-token"}}"#
    }

    fn config_with_consumer(
        sender: SyncSender<InboundEvent>,
        consumer: &Arc<RecordingConsumer>,
    ) -> TestResult<WebhookConfig> {
        let consumer: Arc<dyn CallbackConsumer> = consumer.clone();
        Ok(config(sender)?.with_callback_consumer(consumer))
    }

    #[tokio::test]
    async fn callback_query_reaches_consumer_without_forwarding_event() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let consumer = Arc::new(RecordingConsumer::default());
        assert_eq!(
            call(
                config_with_consumer(sender, &consumer)?,
                request(Some("correct-secret"), callback_body())?
            )
            .await,
            StatusCode::NO_CONTENT
        );
        assert!(receiver.try_recv().is_err());
        let recorded = consumer.recorded()?;
        let callback = recorded
            .first()
            .ok_or(crate::test_support::TestError::Missing(
                "callback delivered",
            ))?;
        assert_eq!(recorded.len(), 1);
        assert_eq!(callback.update_id, 77);
        assert_eq!(callback.callback_id, "cb-1");
        assert_eq!(callback.data, "opaque-token");
        assert_eq!(callback.chat_id, 123);
        assert_eq!(callback.message_id, 9);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_callback_update_is_delivered_once() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let consumer = Arc::new(RecordingConsumer::default());
        let config = config_with_consumer(sender, &consumer)?;
        for _ in 0..2 {
            assert_eq!(
                call(
                    config.clone(),
                    request(Some("correct-secret"), callback_body())?
                )
                .await,
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(consumer.recorded()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn callback_with_wrong_secret_never_reaches_consumer() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let consumer = Arc::new(RecordingConsumer::default());
        assert_eq!(
            call(
                config_with_consumer(sender, &consumer)?,
                request(Some("wrong-secret"), callback_body())?
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert!(consumer.recorded()?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn unmappable_callback_is_answered_as_unroutable_once() -> TestResult {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let consumer = Arc::new(RecordingConsumer::default());
        let config = config_with_consumer(sender, &consumer)?;
        // Ohne `message`/`data` lässt sich der Klick nicht sicher zuordnen.
        for _ in 0..2 {
            assert_eq!(
                call(
                    config.clone(),
                    request(
                        Some("correct-secret"),
                        r#"{"update_id":43,"callback_query":{"id":"query","from":{"id":8,"is_bot":false,"first_name":"Mia"}}}"#,
                    )?,
                )
                .await,
                StatusCode::NO_CONTENT
            );
        }
        assert!(consumer.recorded()?.is_empty());
        let unroutable = consumer
            .unroutable
            .lock()
            .map(|ids| ids.clone())
            .map_err(ctx("recording consumer lock"))?;
        assert_eq!(unroutable, ["query".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_secret_never_reaches_mapping() -> TestResult {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            call(
                config(sender)?,
                request(Some("wrong-secret"), update_body())?
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }
}

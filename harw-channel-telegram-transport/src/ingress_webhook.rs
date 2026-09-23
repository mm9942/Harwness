//! Async Telegram webhook ingress.
//!
//! Authentication is deliberately performed against the request headers before
//! the body is read. The resolved secret is supplied by the caller; this
//! boundary does not resolve `SecretRef` values or touch gateway state.

use std::{net::SocketAddr, sync::mpsc::SyncSender};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header::HeaderValue},
    response::{IntoResponse, Response},
    routing::post,
};
use harw_channel::{ChannelId, InboundEvent};

use crate::mapping::{RawUpdate, map_update};

const SECRET_HEADER: &str = "X-Telegram-Bot-Api-Secret-Token";
const MAX_UPDATE_BYTES: usize = 1_048_576;

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
        }
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
pub async fn run_webhook_server(config: WebhookConfig) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    axum::serve(listener, webhook_router(config)).await
}

async fn webhook_handler(State(config): State<WebhookConfig>, request: Request<Body>) -> Response {
    let authenticated = request
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
        return StatusCode::NO_CONTENT.into_response();
    };
    event.channel = ChannelId::from_str(config.channel_id.clone());

    match config.sender.try_send(event) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
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

//! Token-owning client for the Telegram Bot API.
//!
//! The token lives only in [`TelegramClient`] as a `SecretBox`.  Callers use
//! typed request arguments and never need to construct a token-bearing URL.

use std::{fmt, time::Duration};

use secrecy::{ExposeSecret, SecretBox, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::error::{TelegramTransportError, TransportResult};

const API_BASE: &str = "https://api.telegram.org";
const MAX_SERVER_ATTEMPTS: u32 = 5;
const INITIAL_SERVER_BACKOFF: Duration = Duration::from_secs(1);
const MAX_SERVER_BACKOFF: Duration = Duration::from_secs(30);

/// Maximum number of consecutive HTTP 429 ("Too Many Requests") responses
/// this client retries automatically before giving up and returning
/// `ApiRejected` to the caller.
///
/// Combined with `MAX_RETRY_AFTER`, this bounds the worst-case time a
/// single `TelegramClient::call` blocks on Telegram flood control to
/// `MAX_RETRY_AFTER_ATTEMPTS * MAX_RETRY_AFTER` (currently 3 * 60s = 180s),
/// mirroring the 5xx cap above (`MAX_SERVER_ATTEMPTS` / `MAX_SERVER_BACKOFF`).
/// This matters for every caller that awaits `call` directly, including the
/// long-poll loop in `ingress_long_poll.rs`: its own `POLL_BACKOFF_MAX`
/// (30s, capped by `sleep_unless_shutdown`) only covers the delay *between*
/// `get_updates` attempts and does not see this in-call sleep, so shutdown
/// cannot interrupt it either.
const MAX_RETRY_AFTER_ATTEMPTS: u32 = 3;
/// Upper bound on a single Telegram-supplied `retry_after` delay this client
/// honours. A flood-wait longer than this is rejected instead of slept on, so
/// the caller's own backoff or supervision decides how to proceed.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Telegram Bot API client which owns its bot token.
pub struct TelegramClient {
    http: reqwest::Client,
    token: SecretBox<str>,
}

impl TelegramClient {
    /// Build a client using reqwest's default configuration.
    pub fn new(token: impl Into<SecretString>) -> Self {
        Self::with_http_client(reqwest::Client::new(), token)
    }

    /// Build a client with a caller-configured HTTP client.
    ///
    /// This is useful when the application needs explicit proxy, timeout, or
    /// TLS policy while preserving token ownership in this boundary.
    pub fn with_http_client(http: reqwest::Client, token: impl Into<SecretString>) -> Self {
        Self {
            http,
            token: token.into(),
        }
    }

    /// Fetch this bot's Telegram identity and capabilities.
    pub async fn get_me(&self) -> TransportResult<BotInfo> {
        self.call("getMe", EmptyRequest {}).await
    }

    /// Fetch pending updates using Telegram long polling.
    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
        allowed_updates: &[&str],
    ) -> TransportResult<Vec<Value>> {
        self.call(
            "getUpdates",
            GetUpdatesRequest {
                offset,
                timeout: timeout_secs,
                allowed_updates,
            },
        )
        .await
    }

    /// Send a text message, optionally into a forum topic with an inline keyboard.
    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        message_thread_id: Option<i64>,
        inline_keyboard: Option<&[Vec<InlineKeyboardButton>]>,
    ) -> TransportResult<SentMessage> {
        self.call(
            "sendMessage",
            SendMessageRequest {
                chat_id,
                text,
                message_thread_id,
                reply_markup: inline_keyboard.map(InlineKeyboardMarkup::new),
            },
        )
        .await
    }

    /// Replace the text of an existing message.
    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        inline_keyboard: Option<&[Vec<InlineKeyboardButton>]>,
    ) -> TransportResult<SentMessage> {
        self.call(
            "editMessageText",
            EditMessageTextRequest {
                chat_id,
                message_id,
                text,
                reply_markup: inline_keyboard.map(InlineKeyboardMarkup::new),
            },
        )
        .await
    }

    /// Resolve Telegram metadata for a downloadable file.
    pub async fn get_file(&self, file_id: &str) -> TransportResult<TelegramFile> {
        self.call("getFile", GetFileRequest { file_id }).await
    }

    /// Download a Telegram file, rejecting bodies larger than `max_bytes`.
    ///
    /// The declared content length is checked before reading.  Chunked bodies
    /// are checked incrementally, so an untrusted response cannot grow the
    /// in-memory buffer past the configured ceiling.
    pub async fn download_file(
        &self,
        file: &TelegramFile,
        max_bytes: usize,
    ) -> TransportResult<Vec<u8>> {
        let file_path = file.file_path.as_deref().ok_or_else(|| {
            TelegramTransportError::AttachmentRejected {
                reason: "Telegram getFile response did not include file_path".to_owned(),
            }
        })?;
        let mut server_attempt = 0;
        let mut response = loop {
            let response = self
                .http
                .get(self.file_url(file_path))
                .send()
                .await
                .map_err(TelegramTransportError::from)?;
            if should_retry_server_error(response.status().as_u16().into(), server_attempt) {
                tokio::time::sleep(server_backoff(server_attempt)).await;
                server_attempt += 1;
                continue;
            }
            break response;
        };

        if !response.status().is_success() {
            return Err(api_rejected(
                "downloadFile",
                response.status().as_u16().into(),
                None,
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(TelegramTransportError::AttachmentRejected {
                reason: format!("Telegram file exceeds {max_bytes}-byte limit"),
            });
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(TelegramTransportError::from)?
        {
            let remaining = max_bytes.saturating_sub(body.len());
            if chunk.len() > remaining {
                return Err(TelegramTransportError::AttachmentRejected {
                    reason: format!("Telegram file exceeds {max_bytes}-byte limit"),
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Acknowledge a callback query, optionally with an operator-visible notice.
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> TransportResult<()> {
        self.call_ok(
            "answerCallbackQuery",
            AnswerCallbackQueryRequest {
                callback_query_id,
                text,
                show_alert,
            },
        )
        .await
    }

    /// Register the commands Telegram presents to the user.
    pub async fn set_my_commands(&self, commands: &[BotCommand]) -> TransportResult<()> {
        self.call_ok("setMyCommands", SetMyCommandsRequest { commands })
            .await
    }

    /// Registriert die Befehlsliste für einen bestimmten [`BotCommandScope`].
    ///
    /// Telegram wählt pro Chat die spezifischste passende Liste; so können
    /// Privatchats, Gruppen und einzelne Chats unterschiedliche Menüs sehen.
    pub async fn set_my_commands_scoped(
        &self,
        commands: &[BotCommand],
        scope: &BotCommandScope,
    ) -> TransportResult<()> {
        self.call_ok(
            "setMyCommands",
            SetMyCommandsScopedRequest { commands, scope },
        )
        .await
    }

    /// Entfernt die Befehlsliste eines [`BotCommandScope`]; Telegram fällt
    /// danach auf den nächstallgemeineren Scope zurück.
    pub async fn delete_my_commands(&self, scope: &BotCommandScope) -> TransportResult<()> {
        self.call_ok("deleteMyCommands", DeleteMyCommandsRequest { scope })
            .await
    }

    /// Ersetzt (oder mit `None` entfernt) die Inline-Tastatur einer Nachricht.
    ///
    /// Telegram antwortet je nach Nachrichtenart mit der Nachricht oder mit
    /// `true`; das Ergebnis wird deshalb nur als JSON-Wert angenommen und
    /// verworfen.
    pub async fn edit_message_reply_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        inline_keyboard: Option<&[Vec<InlineKeyboardButton>]>,
    ) -> TransportResult<()> {
        self.call::<Value, _>(
            "editMessageReplyMarkup",
            EditMessageReplyMarkupRequest {
                chat_id,
                message_id,
                reply_markup: InlineKeyboardMarkup::new(inline_keyboard.unwrap_or(&[])),
            },
        )
        .await
        .map(|_| ())
    }

    /// Zeigt eine Chat-Aktion (z. B. `typing`) für etwa fünf Sekunden an.
    ///
    /// Nur die von der Bot API definierten Aktionen werden akzeptiert;
    /// andere Werte werden lokal abgewiesen, ohne eine Anfrage zu senden.
    pub async fn send_chat_action(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        action: &str,
    ) -> TransportResult<()> {
        if !is_valid_chat_action(action) {
            return Err(TelegramTransportError::ApiRejected {
                method: "sendChatAction",
                code: 0,
                description: "unbekannte Telegram-Chat-Aktion".to_owned(),
            });
        }
        self.call_ok(
            "sendChatAction",
            SendChatActionRequest {
                chat_id,
                message_thread_id: thread_id,
                action,
            },
        )
        .await
    }

    /// Configure Telegram to deliver updates to an HTTPS webhook.
    pub async fn set_webhook(
        &self,
        url: &str,
        secret_token: Option<&str>,
        allowed_updates: &[&str],
        drop_pending_updates: bool,
    ) -> TransportResult<()> {
        if let Some(secret_token) = secret_token {
            if !is_valid_webhook_secret(secret_token) {
                return Err(TelegramTransportError::ApiRejected {
                    method: "setWebhook",
                    code: 0,
                    description:
                        "webhook secret token must be 1-256 ASCII characters from [A-Za-z0-9_-]"
                            .to_owned(),
                });
            }
        }
        self.call_ok(
            "setWebhook",
            SetWebhookRequest {
                url,
                secret_token,
                allowed_updates,
                drop_pending_updates,
            },
        )
        .await
    }

    /// Disable Telegram webhook delivery.
    pub async fn delete_webhook(&self, drop_pending_updates: bool) -> TransportResult<()> {
        self.call_ok(
            "deleteWebhook",
            DeleteWebhookRequest {
                drop_pending_updates,
            },
        )
        .await
    }

    async fn call<T: DeserializeOwned, B: Serialize>(
        &self,
        method: &'static str,
        body: B,
    ) -> TransportResult<T> {
        let mut server_attempt = 0;
        let mut retry_after_attempt = 0;
        loop {
            let response = self
                .http
                .post(self.api_url(method))
                .json(&body)
                .send()
                .await
                .map_err(TelegramTransportError::from)?;
            let status = response.status();
            let bytes = response
                .bytes()
                .await
                .map_err(TelegramTransportError::from)?;
            let envelope: TelegramResponse<T> = match serde_json::from_slice(&bytes) {
                Ok(envelope) => envelope,
                Err(_) if should_retry_server_error(status.as_u16().into(), server_attempt) => {
                    tokio::time::sleep(server_backoff(server_attempt)).await;
                    server_attempt += 1;
                    continue;
                }
                Err(_) => {
                    return Err(api_rejected(
                        method,
                        status.as_u16().into(),
                        Some("invalid Telegram API JSON response".to_owned()),
                    ));
                }
            };

            if envelope.ok {
                return envelope
                    .result
                    .ok_or_else(|| TelegramTransportError::ApiRejected {
                        method,
                        code: status.as_u16().into(),
                        description: "successful Telegram response did not include result"
                            .to_owned(),
                    });
            }

            let code = envelope
                .error_code
                .unwrap_or_else(|| status.as_u16().into());
            let retry_after = envelope.parameters.and_then(|value| value.retry_after);
            match retry_decision(code, retry_after, server_attempt, retry_after_attempt) {
                RetryDecision::RetryAfter(delay) => {
                    tokio::time::sleep(delay).await;
                    if (500..600).contains(&code) {
                        server_attempt += 1;
                    } else if code == 429 {
                        retry_after_attempt += 1;
                    }
                    continue;
                }
                RetryDecision::Reject(override_description) => {
                    return Err(api_rejected(
                        method,
                        code,
                        override_description.or(envelope.description),
                    ));
                }
            }
        }
    }

    async fn call_ok<B: Serialize>(&self, method: &'static str, body: B) -> TransportResult<()> {
        self.call::<bool, B>(method, body).await.map(|_| ())
    }

    fn api_url(&self, method: &str) -> String {
        format!("{API_BASE}/bot{}/{method}", self.token.expose_secret())
    }

    fn file_url(&self, file_path: &str) -> String {
        format!(
            "{API_BASE}/file/bot{}/{file_path}",
            self.token.expose_secret()
        )
    }
}

#[derive(Serialize)]
struct EmptyRequest {}

#[derive(Serialize)]
struct GetUpdatesRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u64,
    allowed_updates: &'a [&'a str],
}

#[derive(Serialize)]
struct InlineKeyboardMarkup<'a> {
    inline_keyboard: &'a [Vec<InlineKeyboardButton>],
}

impl<'a> InlineKeyboardMarkup<'a> {
    fn new(inline_keyboard: &'a [Vec<InlineKeyboardButton>]) -> Self {
        Self { inline_keyboard }
    }
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup<'a>>,
}

#[derive(Serialize)]
struct EditMessageTextRequest<'a> {
    chat_id: i64,
    message_id: i64,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_markup: Option<InlineKeyboardMarkup<'a>>,
}

#[derive(Serialize)]
struct GetFileRequest<'a> {
    file_id: &'a str,
}

#[derive(Serialize)]
struct AnswerCallbackQueryRequest<'a> {
    callback_query_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    show_alert: bool,
}

#[derive(Serialize)]
struct SetMyCommandsRequest<'a> {
    commands: &'a [BotCommand],
}

#[derive(Serialize)]
struct SetMyCommandsScopedRequest<'a> {
    commands: &'a [BotCommand],
    scope: &'a BotCommandScope,
}

#[derive(Serialize)]
struct DeleteMyCommandsRequest<'a> {
    scope: &'a BotCommandScope,
}

#[derive(Serialize)]
struct EditMessageReplyMarkupRequest<'a> {
    chat_id: i64,
    message_id: i64,
    reply_markup: InlineKeyboardMarkup<'a>,
}

#[derive(Serialize)]
struct SendChatActionRequest<'a> {
    chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
    action: &'a str,
}

/// Von der Bot API definierte Werte für `sendChatAction`.
const CHAT_ACTIONS: &[&str] = &[
    "typing",
    "upload_photo",
    "record_video",
    "upload_video",
    "record_voice",
    "upload_voice",
    "upload_document",
    "choose_sticker",
    "find_location",
    "record_video_note",
    "upload_video_note",
];

fn is_valid_chat_action(action: &str) -> bool {
    CHAT_ACTIONS.contains(&action)
}

#[derive(Serialize)]
struct SetWebhookRequest<'a> {
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_token: Option<&'a str>,
    allowed_updates: &'a [&'a str],
    drop_pending_updates: bool,
}

#[derive(Serialize)]
struct DeleteWebhookRequest {
    drop_pending_updates: bool,
}

impl fmt::Debug for TelegramClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramClient")
            .field("http", &"reqwest::Client")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Bot identity returned by `getMe`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BotInfo {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub username: Option<String>,
    pub can_join_groups: Option<bool>,
    pub can_read_all_group_messages: Option<bool>,
    pub supports_inline_queries: Option<bool>,
}

/// Message metadata returned by Telegram send and edit calls.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SentMessage {
    pub message_id: i64,
    pub date: i64,
    /// Telegram liefert das Chat-Objekt als `chat`; übernommen wird nur `chat.id`.
    #[serde(rename = "chat", deserialize_with = "deserialize_chat_id")]
    pub chat_id: i64,
    pub message_thread_id: Option<i64>,
}

fn deserialize_chat_id<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct ChatId {
        id: i64,
    }

    ChatId::deserialize(deserializer).map(|chat| chat.id)
}

/// File metadata returned by `getFile`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_unique_id: String,
    pub file_size: Option<u64>,
    pub file_path: Option<String>,
}

/// A command exposed in Telegram's command menu.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotCommand {
    pub command: String,
    pub description: String,
}

/// Geltungsbereich einer Befehlsliste (`BotCommandScope` der Bot API).
///
/// Serialisiert im Telegram-Format, z. B. `{"type":"chat","chat_id":42}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BotCommandScope {
    /// `default`: gilt, wenn kein spezifischerer Scope passt.
    Default,
    /// `all_private_chats`: alle Privatchats.
    AllPrivateChats,
    /// `all_group_chats`: alle Gruppen und Supergruppen.
    AllGroupChats,
    /// `all_chat_administrators`: alle Gruppen-Administratoren.
    AllChatAdministrators,
    /// `chat`: genau ein Chat.
    Chat { chat_id: i64 },
    /// `chat_administrators`: Administratoren genau eines Gruppenchats.
    ChatAdministrators { chat_id: i64 },
    /// `chat_member`: ein Mitglied in genau einem Gruppenchat.
    ChatMember { chat_id: i64, user_id: i64 },
}

/// A supported inline keyboard button.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<i64>,
    parameters: Option<TelegramResponseParameters>,
}

#[derive(Deserialize)]
struct TelegramResponseParameters {
    retry_after: Option<u64>,
}

fn api_rejected(
    method: &'static str,
    code: i64,
    description: Option<String>,
) -> TelegramTransportError {
    TelegramTransportError::ApiRejected {
        method,
        code,
        description: description
            .unwrap_or_else(|| "Telegram API did not provide a description".to_owned()),
    }
}

fn should_retry_server_error(code: i64, prior_retries: u32) -> bool {
    (500..600).contains(&code) && prior_retries < MAX_SERVER_ATTEMPTS - 1
}

/// Outcome of `retry_decision`. `Reject` carries an optional override for
/// the rejection's description: `None` keeps Telegram's own `description`
/// field (the ordinary case), `Some(_)` replaces it when the client itself
/// gave up (attempt or delay cap exceeded) rather than Telegram rejecting
/// the request outright.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RetryDecision {
    RetryAfter(Duration),
    Reject(Option<String>),
}

fn retry_decision(
    code: i64,
    retry_after_secs: Option<u64>,
    prior_server_retries: u32,
    prior_retry_after_retries: u32,
) -> RetryDecision {
    if code == 429 {
        let Some(seconds) = retry_after_secs else {
            return RetryDecision::Reject(None);
        };
        if prior_retry_after_retries >= MAX_RETRY_AFTER_ATTEMPTS {
            return RetryDecision::Reject(Some(format!(
                "Telegram flood control requested retry_after={seconds}s after \
                 {MAX_RETRY_AFTER_ATTEMPTS} retries; giving up"
            )));
        }
        let delay = Duration::from_secs(seconds);
        if delay > MAX_RETRY_AFTER {
            return RetryDecision::Reject(Some(format!(
                "Telegram flood control requested retry_after={seconds}s, exceeding the \
                 {}s cap",
                MAX_RETRY_AFTER.as_secs()
            )));
        }
        return RetryDecision::RetryAfter(delay);
    }
    if should_retry_server_error(code, prior_server_retries) {
        return RetryDecision::RetryAfter(server_backoff(prior_server_retries));
    }
    RetryDecision::Reject(None)
}

fn server_backoff(prior_retries: u32) -> Duration {
    INITIAL_SERVER_BACKOFF
        .checked_mul(2_u32.saturating_pow(prior_retries))
        .unwrap_or(MAX_SERVER_BACKOFF)
        .min(MAX_SERVER_BACKOFF)
}

fn is_valid_webhook_secret(secret_token: &str) -> bool {
    (1..=256).contains(&secret_token.len())
        && secret_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn api_urls_follow_telegram_bot_api_shape() {
        let client = TelegramClient::new("123456:secret-token");
        assert_eq!(
            client.api_url("getMe"),
            "https://api.telegram.org/bot123456:secret-token/getMe"
        );
        assert_eq!(
            client.file_url("documents/file_1.pdf"),
            "https://api.telegram.org/file/bot123456:secret-token/documents/file_1.pdf"
        );
    }

    #[test]
    fn retry_decision_uses_telegram_rate_limit_and_bounded_server_backoff() {
        assert_eq!(
            retry_decision(429, Some(17), 0, 0),
            RetryDecision::RetryAfter(Duration::from_secs(17))
        );
        assert_eq!(retry_decision(429, None, 0, 0), RetryDecision::Reject(None));
        assert_eq!(
            retry_decision(500, None, 0, 0),
            RetryDecision::RetryAfter(Duration::from_secs(1))
        );
        assert_eq!(
            retry_decision(599, None, MAX_SERVER_ATTEMPTS - 2, 0),
            RetryDecision::RetryAfter(Duration::from_secs(8))
        );
        assert_eq!(
            retry_decision(500, None, MAX_SERVER_ATTEMPTS - 1, 0),
            RetryDecision::Reject(None)
        );
        assert_eq!(server_backoff(10), Duration::from_secs(30));
    }

    #[test]
    fn retry_decision_caps_telegram_flood_wait_attempts_and_delay() -> TestResult {
        // A flood-wait far beyond the configured ceiling is rejected instead
        // of slept on, no matter how few attempts already happened.
        match retry_decision(429, Some(3600), 0, 0) {
            RetryDecision::Reject(Some(description)) => {
                assert!(description.contains("3600"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "retry_after above the cap must reject with a description, got {other:?}"
                )));
            }
        }

        // A small delay is still retried up to the attempt cap...
        assert_eq!(
            retry_decision(429, Some(1), 0, MAX_RETRY_AFTER_ATTEMPTS - 1),
            RetryDecision::RetryAfter(Duration::from_secs(1))
        );
        // ...but once the cap is reached the client gives up instead of
        // retrying forever against a steady stream of 429s.
        match retry_decision(429, Some(1), 0, MAX_RETRY_AFTER_ATTEMPTS) {
            RetryDecision::Reject(Some(description)) => {
                assert!(description.contains("retries"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "retry_after attempts at the cap must reject with a description, got {other:?}"
                )));
            }
        }

        // A delay exactly at the cap is still honoured.
        assert_eq!(
            retry_decision(429, Some(MAX_RETRY_AFTER.as_secs()), 0, 0),
            RetryDecision::RetryAfter(MAX_RETRY_AFTER)
        );
        Ok(())
    }

    #[test]
    fn response_envelope_parses_telegram_error_parameters_and_acknowledgements() -> TestResult {
        let rate_limited: TelegramResponse<bool> = serde_json::from_str(
            r#"{"ok":false,"description":"Too Many Requests","error_code":429,"parameters":{"retry_after":17}}"#,
        )
        .map_err(ctx("test JSON must deserialize"))?;
        assert!(!rate_limited.ok);
        assert_eq!(rate_limited.error_code, Some(429));
        assert_eq!(
            rate_limited
                .parameters
                .and_then(|parameters| parameters.retry_after),
            Some(17)
        );

        let acknowledgement: TelegramResponse<bool> =
            serde_json::from_str(r#"{"ok":true,"result":true}"#)
                .map_err(ctx("test JSON must deserialize"))?;
        assert_eq!(acknowledgement.result, Some(true));
        Ok(())
    }

    #[test]
    fn debug_never_emits_token() {
        let token = "123456:secret-token";
        let rendered = format!("{:?}", TelegramClient::new(token));
        assert!(!rendered.contains(token));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn sent_message_decodes_real_send_message_result() -> TestResult {
        let envelope: TelegramResponse<SentMessage> = serde_json::from_str(
            r#"{"ok":true,"result":{"message_id":4711,"from":{"id":7000000001,"is_bot":true,"first_name":"Harw","username":"harw_bot"},"chat":{"id":-1001234567890,"title":"Ops","is_forum":true,"type":"supergroup"},"date":1758700000,"message_thread_id":33,"is_topic_message":true,"text":"hallo"}}"#,
        )
        .map_err(ctx("real sendMessage response must deserialize"))?;
        assert!(envelope.ok);
        assert_eq!(
            envelope.result,
            Some(SentMessage {
                message_id: 4711,
                date: 1758700000,
                chat_id: -1001234567890,
                message_thread_id: Some(33),
            })
        );

        let private: SentMessage = serde_json::from_str(
            r#"{"message_id":5,"from":{"id":1,"is_bot":true,"first_name":"Harw"},"chat":{"id":42,"first_name":"Alice","type":"private"},"date":1758700001,"text":"ok"}"#,
        )
        .map_err(ctx("private sendMessage result must deserialize"))?;
        assert_eq!(private.chat_id, 42);
        assert_eq!(private.message_thread_id, None);
        Ok(())
    }

    #[test]
    fn bot_command_scope_uses_telegram_wire_names() -> TestResult {
        let cases = [
            (BotCommandScope::Default, r#"{"type":"default"}"#),
            (
                BotCommandScope::AllPrivateChats,
                r#"{"type":"all_private_chats"}"#,
            ),
            (
                BotCommandScope::AllGroupChats,
                r#"{"type":"all_group_chats"}"#,
            ),
            (
                BotCommandScope::AllChatAdministrators,
                r#"{"type":"all_chat_administrators"}"#,
            ),
            (
                BotCommandScope::Chat { chat_id: -100 },
                r#"{"type":"chat","chat_id":-100}"#,
            ),
            (
                BotCommandScope::ChatAdministrators { chat_id: -100 },
                r#"{"type":"chat_administrators","chat_id":-100}"#,
            ),
            (
                BotCommandScope::ChatMember {
                    chat_id: -100,
                    user_id: 7,
                },
                r#"{"type":"chat_member","chat_id":-100,"user_id":7}"#,
            ),
        ];
        for (scope, expected) in cases {
            let encoded = serde_json::to_string(&scope).map_err(ctx("scope serializes"))?;
            assert_eq!(encoded, expected);
            let decoded: BotCommandScope =
                serde_json::from_str(expected).map_err(ctx("scope deserializes"))?;
            assert_eq!(decoded, scope);
        }
        Ok(())
    }

    #[test]
    fn new_request_bodies_match_bot_api_shape() -> TestResult {
        let commands = [BotCommand {
            command: "help".to_owned(),
            description: "Hilfe".to_owned(),
        }];
        let scope = BotCommandScope::Chat { chat_id: 42 };
        assert_eq!(
            serde_json::to_value(SetMyCommandsScopedRequest {
                commands: &commands,
                scope: &scope,
            })
            .map_err(ctx("setMyCommands body serializes"))?,
            serde_json::json!({
                "commands": [{ "command": "help", "description": "Hilfe" }],
                "scope": { "type": "chat", "chat_id": 42 }
            })
        );
        assert_eq!(
            serde_json::to_value(DeleteMyCommandsRequest {
                scope: &BotCommandScope::Default,
            })
            .map_err(ctx("deleteMyCommands body serializes"))?,
            serde_json::json!({ "scope": { "type": "default" } })
        );
        assert_eq!(
            serde_json::to_value(EditMessageReplyMarkupRequest {
                chat_id: 1,
                message_id: 2,
                reply_markup: InlineKeyboardMarkup::new(&[]),
            })
            .map_err(ctx("editMessageReplyMarkup body serializes"))?,
            serde_json::json!({
                "chat_id": 1,
                "message_id": 2,
                "reply_markup": { "inline_keyboard": [] }
            })
        );
        assert_eq!(
            serde_json::to_value(SendChatActionRequest {
                chat_id: 1,
                message_thread_id: None,
                action: "typing",
            })
            .map_err(ctx("sendChatAction body serializes"))?,
            serde_json::json!({ "chat_id": 1, "action": "typing" })
        );
        assert_eq!(
            serde_json::to_value(SendChatActionRequest {
                chat_id: 1,
                message_thread_id: Some(9),
                action: "typing",
            })
            .map_err(ctx("sendChatAction body serializes"))?,
            serde_json::json!({ "chat_id": 1, "message_thread_id": 9, "action": "typing" })
        );
        Ok(())
    }

    #[test]
    fn chat_action_allowlist_matches_bot_api() {
        assert!(is_valid_chat_action("typing"));
        assert!(is_valid_chat_action("upload_document"));
        assert!(!is_valid_chat_action(""));
        assert!(!is_valid_chat_action("Typing"));
        assert!(!is_valid_chat_action("hacking"));
    }

    #[test]
    fn webhook_secret_uses_telegram_character_and_length_limits() {
        assert!(is_valid_webhook_secret("secret_123-ABC"));
        assert!(is_valid_webhook_secret(&"a".repeat(256)));
        assert!(!is_valid_webhook_secret(""));
        assert!(!is_valid_webhook_secret(&"a".repeat(257)));
        assert!(!is_valid_webhook_secret("space forbidden"));
    }
}

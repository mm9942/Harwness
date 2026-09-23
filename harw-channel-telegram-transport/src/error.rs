//! Hand-written error type for the Telegram transport boundary (client,
//! ingress, rendering, media intake, and offset persistence).
//!
//! Display and Debug intentionally omit all dynamic detail supplied by Telegram,
//! configuration, or secret resolution. Those values can contain bot tokens or
//! other credentials and must not reach ordinary logs through this boundary.

use std::fmt;

pub type TransportResult<T> = Result<T, TelegramTransportError>;

/// The durable-offset operation that encountered a local I/O failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OffsetPersistenceOperation {
    Lock,
    Read,
    Write,
    Sync,
    Replace,
}

impl fmt::Display for OffsetPersistenceOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self {
            Self::Lock => "lock",
            Self::Read => "read",
            Self::Write => "write",
            Self::Sync => "sync",
            Self::Replace => "atomically replace",
        };
        f.write_str(operation)
    }
}

impl fmt::Debug for OffsetPersistenceOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Failures at the Telegram Bot API transport boundary.
pub enum TelegramTransportError {
    /// Network/HTTP transport failure (connection, timeout, TLS).
    Transport(reqwest::Error),
    /// The Bot API returned a structured error payload (`ok: false`) or a
    /// non-2xx status the retry policy gave up on.
    ApiRejected {
        method: &'static str,
        code: i64,
        description: String,
    },
    /// A `SecretRef` (bot token or webhook secret) failed to resolve.
    TokenResolution { reference: String },
    /// An inbound update failed schema validation (unexpected shape).
    MalformedUpdate {
        update_id: Option<i64>,
        reason: String,
    },
    /// An attachment exceeded a configured byte/count ceiling before download.
    AttachmentRejected { reason: String },
    /// A webhook request's secret-token header was missing or mismatched.
    WebhookAuth,
    /// Telegram transport configuration could not be loaded or validated.
    Config(harw_config::ConfigError),
    /// Durable long-poll offset persistence failed during a named I/O operation.
    OffsetPersistence {
        operation: OffsetPersistenceOperation,
        source: std::io::Error,
    },
    /// Durable long-poll offset state could not be serialized or decoded.
    OffsetSerialization(serde_json::Error),
    /// Local I/O failure outside the offset store (for example, attachment cache).
    Io(std::io::Error),
}

impl fmt::Display for TelegramTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(_) => f.write_str("Telegram Bot API transport failure"),
            Self::ApiRejected { method, code, .. } => {
                write!(f, "Telegram Bot API rejected '{method}' ({code})")
            }
            Self::TokenResolution { .. } => {
                f.write_str("failed to resolve Telegram secret reference")
            }
            Self::MalformedUpdate { update_id, .. } => {
                write!(f, "malformed Telegram update (update_id={update_id:?})")
            }
            Self::AttachmentRejected { .. } => f.write_str("Telegram attachment rejected"),
            Self::WebhookAuth => f.write_str("webhook request failed secret-token verification"),
            Self::Config(_) => f.write_str("Telegram transport configuration failure"),
            Self::OffsetPersistence { operation, .. } => {
                write!(f, "failed to {operation} durable Telegram offset")
            }
            Self::OffsetSerialization(_) => {
                f.write_str("failed to serialize or decode durable Telegram offset")
            }
            Self::Io(_) => f.write_str("local I/O failure in Telegram transport"),
        }
    }
}

impl fmt::Debug for TelegramTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TelegramTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::OffsetPersistence { source, .. } => Some(source),
            Self::OffsetSerialization(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::ApiRejected { .. }
            | Self::TokenResolution { .. }
            | Self::MalformedUpdate { .. }
            | Self::AttachmentRejected { .. }
            | Self::WebhookAuth => None,
        }
    }
}

impl From<reqwest::Error> for TelegramTransportError {
    fn from(value: reqwest::Error) -> Self {
        // Die Bot-API-URL enthält den Token im Pfad (`/bot<token>/…`).
        // `reqwest::Error` haengt diese URL sowohl an `Display` als auch an
        // `Debug` an (siehe reqwest 0.12.28 `error.rs`); `Self::Transport`s
        // eigenes `Display`/`Debug` ist zwar bereits inhaltsfrei (siehe oben),
        // aber `source()` gibt genau diesen `reqwest::Error` unverändert
        // zurück (`impl Error for TelegramTransportError` unten) und jeder
        // Fehlerreporter, der die `source()`-Kette ausgibt, würde den Token
        // sonst leaken (F-041/S8). `without_url()` entfernt die URL, bevor
        // der Fehler überhaupt in diese Variante gelangt.
        Self::Transport(value.without_url())
    }
}

impl From<harw_config::ConfigError> for TelegramTransportError {
    fn from(value: harw_config::ConfigError) -> Self {
        Self::Config(value)
    }
}

impl From<std::io::Error> for TelegramTransportError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{OffsetPersistenceOperation, TelegramTransportError};
    use crate::test_support::{TestError, TestResult};

    fn assert_secret_redacted(error: TelegramTransportError, secret: &str) {
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }

    #[test]
    fn dynamic_error_details_are_redacted_from_display_and_debug() {
        let secret = "123456:telegram-bot-token";

        assert_secret_redacted(
            TelegramTransportError::ApiRejected {
                method: "sendMessage",
                code: 400,
                description: secret.to_owned(),
            },
            secret,
        );
        assert_secret_redacted(
            TelegramTransportError::TokenResolution {
                reference: secret.to_owned(),
            },
            secret,
        );
        assert_secret_redacted(
            TelegramTransportError::MalformedUpdate {
                update_id: Some(42),
                reason: secret.to_owned(),
            },
            secret,
        );
        assert_secret_redacted(
            TelegramTransportError::AttachmentRejected {
                reason: secret.to_owned(),
            },
            secret,
        );
        assert_secret_redacted(
            TelegramTransportError::Config(harw_config::ConfigError::Invalid(secret.to_owned())),
            secret,
        );
    }

    #[test]
    fn offset_persistence_error_reports_operation_without_io_detail() {
        let secret_path = "/private/123456:telegram-bot-token/offset.json";
        let error = TelegramTransportError::OffsetPersistence {
            operation: OffsetPersistenceOperation::Replace,
            source: std::io::Error::other(secret_path),
        };

        assert_eq!(
            error.to_string(),
            "failed to atomically replace durable Telegram offset"
        );
        assert_secret_redacted(error, secret_path);
    }

    #[tokio::test]
    async fn reqwest_conversion_strips_the_bot_token_bearing_url_from_source_too() -> TestResult {
        let secret = "123456:telegram-bot-secret-token";
        let client = reqwest::Client::new();
        // Ein geschlossener Loopback-Port scheitert sofort beim Verbindungsaufbau,
        // ganz ohne echten Netzwerkzugriff, und reqwest haengt dabei die
        // angefragte URL (inkl. Bot-Token im Pfad) an den Fehler.
        let request_result = client
            .get(format!("http://127.0.0.1:1/bot{secret}/getMe"))
            .send()
            .await;
        let Err(request_error) = request_result else {
            return Err(TestError::Unexpected(
                "connecting to a closed local port must fail".to_owned(),
            ));
        };
        assert!(
            request_error.url().is_some(),
            "precondition: reqwest attaches the request URL to a transport error"
        );

        let error = TelegramTransportError::from(request_error);

        // `source()` gibt den gewrappten `reqwest::Error` unveraendert zurueck;
        // dessen Display/Debug duerfen nach der Konvertierung kein Token mehr
        // enthalten (F-041/S8), nicht nur `TelegramTransportError`s eigenes
        // redigiertes Display/Debug.
        let source = std::error::Error::source(&error)
            .ok_or(TestError::Missing("transport error keeps its source"))?;
        assert!(!source.to_string().contains(secret));
        assert!(!format!("{source:?}").contains(secret));
        assert_secret_redacted(error, secret);
        Ok(())
    }

    #[test]
    fn wrapped_errors_remain_available_as_sources() {
        let error = TelegramTransportError::from(harw_config::ConfigError::Invalid(
            "invalid telegram transport configuration".to_owned(),
        ));

        assert!(std::error::Error::source(&error).is_some());
    }
}

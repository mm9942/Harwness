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
        Self::Transport(value)
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

    #[test]
    fn wrapped_errors_remain_available_as_sources() {
        let error = TelegramTransportError::from(harw_config::ConfigError::Invalid(
            "invalid telegram transport configuration".to_owned(),
        ));

        assert!(std::error::Error::source(&error).is_some());
    }
}

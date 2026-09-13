use std::fmt;

use harw_channel::ChannelError;
use harw_types::{ChannelId, PeerId};

pub type TelegramChannelResult<T> = Result<T, TelegramChannelError>;

/// Failures local to the Telegram binding's boundary and eventual transport.
pub enum TelegramChannelError {
    ChannelMismatch {
        expected: ChannelId,
        actual: ChannelId,
    },
    IngressUnavailable,
    UnpairedPeer {
        peer: PeerId,
    },
    Pairing(ChannelError),
}

impl fmt::Display for TelegramChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelMismatch { expected, actual } => {
                write!(
                    f,
                    "event for channel '{actual}' cannot enter Telegram binding '{expected}'"
                )
            }
            Self::IngressUnavailable => f.write_str("Telegram HTTP ingress is not configured"),
            Self::UnpairedPeer { peer } => write!(f, "Telegram peer '{peer}' is not paired"),
            Self::Pairing(err) => write!(f, "pairing store error: {err}"),
        }
    }
}

impl fmt::Debug for TelegramChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TelegramChannelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pairing(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ChannelError> for TelegramChannelError {
    fn from(value: ChannelError) -> Self {
        Self::Pairing(value)
    }
}

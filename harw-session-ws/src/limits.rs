//! Per-connection bounds of the session WebSocket (W00 §3.2, §4).
//!
//! Every queue and timer a connection owns is bounded here, so a slow or
//! hostile peer can never grow host memory or hold a connection open without
//! traffic.

use std::time::Duration;

use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

const MIB: usize = 1024 * 1024;

harw_types::limits_struct! {
    /// Size, time and queue bounds of one session WebSocket connection.
    #[derive(Clone, Copy, Debug)]
    pub struct WsLimits {
        /// Largest inbound message (after reassembly) in bytes.
        pub max_message_bytes: usize = DEFAULT_MAX_MESSAGE_BYTES = MIB,
        /// Largest single inbound frame payload in bytes.
        pub max_frame_bytes: usize = DEFAULT_MAX_FRAME_BYTES = MIB,
        /// Time a client has to complete `session.hello` after the upgrade.
        pub hello_timeout: Duration = DEFAULT_HELLO_TIMEOUT = Duration::from_secs(10),
        /// Concurrent requests per connection before calls are refused as busy.
        pub max_in_flight: usize = DEFAULT_MAX_IN_FLIGHT = 32,
        /// Frames buffered per attachment inside the connection multiplexer.
        pub attachment_buffer: usize = DEFAULT_ATTACHMENT_BUFFER = 16,
        /// Responses and control messages buffered ahead of the writer.
        pub response_buffer: usize = DEFAULT_RESPONSE_BUFFER = 64,
        /// Attached sessions per connection.
        pub max_attachments: usize = DEFAULT_MAX_ATTACHMENTS = 32,
        /// Interval between server pings.
        pub ping_interval: Duration = DEFAULT_PING_INTERVAL = Duration::from_secs(15),
        /// Time without any inbound message (pongs included) before the
        /// connection is closed.
        pub idle_timeout: Duration = DEFAULT_IDLE_TIMEOUT = Duration::from_secs(60),
    }
}

impl WsLimits {
    /// Protocol configuration for the tungstenite stream: message and frame
    /// size caps from these limits; unmasked client frames stay refused
    /// (RFC 6455, library default).
    #[must_use]
    pub fn tungstenite_config(&self) -> WebSocketConfig {
        WebSocketConfig::default()
            .max_message_size(Some(self.max_message_bytes))
            .max_frame_size(Some(self.max_frame_bytes))
            .accept_unmasked_frames(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let limits = WsLimits::default();
        assert_eq!(limits.max_message_bytes, MIB);
        assert_eq!(limits.max_frame_bytes, MIB);
        assert_eq!(limits.hello_timeout, Duration::from_secs(10));
        assert_eq!(limits.max_in_flight, 32);
        assert_eq!(limits.attachment_buffer, 16);
        assert_eq!(limits.response_buffer, 64);
        assert_eq!(limits.max_attachments, 32);
        assert_eq!(limits.ping_interval, Duration::from_secs(15));
        assert_eq!(limits.idle_timeout, Duration::from_secs(60));
        assert!(limits.ping_interval < limits.idle_timeout);
    }

    #[test]
    fn tungstenite_config_carries_the_size_caps() {
        let limits = WsLimits {
            max_message_bytes: 4096,
            max_frame_bytes: 1024,
            ..WsLimits::default()
        };
        let config = limits.tungstenite_config();
        assert_eq!(config.max_message_size, Some(4096));
        assert_eq!(config.max_frame_size, Some(1024));
        assert!(!config.accept_unmasked_frames);
    }
}

//! Configuration of the communication layer.
//!
//! Every bound is finite. A zero or too-small value is raised to the lowest
//! safe value by [`ComConfig::normalized`] instead of being trusted, because
//! `hyper` panics on a header buffer below 8 KiB.

use std::time::Duration;

use harw_session_ws::WsLimits;

/// Smallest header buffer `hyper` accepts.
const MIN_HEADER_BYTES: usize = 8 * 1024;
/// Largest header buffer this layer allows.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Limits and timers of one [`crate::ComServer`].
#[derive(Debug, Clone)]
pub struct ComConfig {
    /// Per-connection WebSocket bounds (sizes, hello timeout, queues, ping).
    pub limits: WsLimits,
    /// Simultaneous connections (HTTP phase and upgraded session together).
    /// An excess connection is refused before any byte is read.
    pub max_connections: usize,
    /// Time allowed to deliver the HTTP request headers.
    pub header_timeout: Duration,
    /// How long [`crate::ComServer::drain`] waits for live connections.
    pub shutdown_grace: Duration,
    /// Header buffer in bytes (8 KiB to 64 KiB).
    pub max_header_bytes: usize,
    /// Most request headers parsed.
    pub max_headers: usize,
}

impl Default for ComConfig {
    fn default() -> Self {
        Self {
            limits: WsLimits::default(),
            max_connections: 16,
            header_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(5),
            max_header_bytes: 16 * 1024,
            max_headers: 32,
        }
    }
}

impl ComConfig {
    /// A copy with every value raised into its safe range.
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            limits: self.limits,
            max_connections: self.max_connections.clamp(1, u32::MAX as usize),
            header_timeout: self.header_timeout.max(Duration::from_millis(1)),
            shutdown_grace: self.shutdown_grace,
            max_header_bytes: self
                .max_header_bytes
                .clamp(MIN_HEADER_BYTES, MAX_HEADER_BYTES),
            max_headers: self.max_headers.clamp(1, 128),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_raises_unsafe_values() {
        let raw = ComConfig {
            max_connections: 0,
            header_timeout: Duration::ZERO,
            max_header_bytes: 1,
            max_headers: 0,
            ..ComConfig::default()
        };
        let n = raw.normalized();
        assert_eq!(n.max_connections, 1);
        assert!(n.header_timeout > Duration::ZERO);
        assert_eq!(n.max_header_bytes, MIN_HEADER_BYTES);
        assert_eq!(n.max_headers, 1);
        let big = ComConfig {
            max_header_bytes: usize::MAX,
            max_headers: usize::MAX,
            ..ComConfig::default()
        };
        assert_eq!(big.normalized().max_header_bytes, MAX_HEADER_BYTES);
        assert_eq!(big.normalized().max_headers, 128);
    }
}

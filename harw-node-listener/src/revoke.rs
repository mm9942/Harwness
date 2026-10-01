//! Revocation (W00 §7, REV-01/REV-02, S08).
//!
//! Revocation closes current authority: mark the registry record revoked,
//! refuse new handshakes, close the active WS connection(s), drop queued
//! inputs of that device, revoke SecurityHub contexts, audit. A test that
//! only proves "reconnect is refused" is insufficient.
//!
//! Skeleton: the host revoker answers [`ListenerError::NotImplemented`].

use std::sync::Arc;

use harw_session_host::SessionHost;
use harw_types::DeviceId;

use crate::ListenerError;

/// What one revocation did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RevocationReport {
    /// Live connections closed with `Revoked`.
    pub connections_closed: usize,
    /// Queued inputs of the device dropped.
    pub inputs_dropped: usize,
    /// Security contexts revoked.
    pub contexts_revoked: usize,
}

/// Revokes a device's authority everywhere it is live.
pub trait RevocationSink: Send + Sync {
    /// Revoke `device`.
    ///
    /// # Errors
    /// Registry, host or audit failures.
    fn revoke_device(&self, device: &DeviceId) -> Result<RevocationReport, ListenerError>;
}

/// Revoker that drives a [`SessionHost`] (live streams, queued inputs) and
/// the device registry.
pub struct HostRevoker {
    _host: Arc<SessionHost>,
}

impl HostRevoker {
    /// Revoker for `host`.
    #[must_use]
    pub fn new(host: Arc<SessionHost>) -> Self {
        Self { _host: host }
    }
}

impl RevocationSink for HostRevoker {
    fn revoke_device(&self, device: &DeviceId) -> Result<RevocationReport, ListenerError> {
        let _ = device;
        Err(ListenerError::NotImplemented(
            "HostRevoker::revoke_device (S08)",
        ))
    }
}

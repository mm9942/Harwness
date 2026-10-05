//! Revocation (W00 §7, REV-01/REV-02, S08).
//!
//! Revocation closes current authority: mark the registry record revoked,
//! refuse new handshakes, close the active WS connection(s), drop queued
//! inputs of that device, revoke SecurityHub contexts, audit. A test that
//! only proves "reconnect is refused" is insufficient.
//!
//! [`HostRevoker`] does, in this order: (1) `SessionHost::revoke_device`
//! (refuse new calls, close attached streams with `Revoked`, drop queued
//! inputs, cancel tool calls), (2) mark the registry record revoked when a
//! registry is configured, (3) close the device's live WebSocket
//! connections through the [`LiveConnections`] table the listener shares,
//! including connections that have no session attached (the host alone only
//! closes attached streams).
//!
//! Honest limits: the host does not report how many queued inputs it
//! dropped, and the listener owns no SecurityHub contexts, so
//! [`RevocationReport::inputs_dropped`] and
//! [`RevocationReport::contexts_revoked`] are `0` here. Audit emission is
//! the caller's (the report is what it audits).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use harw_session_host::{ConnectionId, SessionHost};
use harw_types::DeviceId;
use tokio::sync::watch;

use crate::ListenerError;
use crate::identity::DeviceRegistry;

/// How long a still-open connection is given to receive the host's own
/// `Revoked` close (attached streams) before the listener closes it.
pub const REVOKE_GRACE: Duration = Duration::from_millis(250);

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

struct Live {
    device: DeviceId,
    close: Arc<watch::Sender<bool>>,
}

/// Live WebSocket connections of this listener, by device. Cheap to clone;
/// clones share the table.
#[derive(Clone, Default)]
pub struct LiveConnections {
    inner: Arc<Mutex<HashMap<ConnectionId, Live>>>,
}

impl std::fmt::Debug for LiveConnections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveConnections")
            .field("len", &self.len())
            .finish()
    }
}

impl LiveConnections {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn table(&self) -> MutexGuard<'_, HashMap<ConnectionId, Live>> {
        match self.inner.lock() {
            Ok(guard) => guard,
            // A poisoned table is still a correct table of senders.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Track `connection` of `device`; the receiver flips to `true` when it
    /// must close.
    #[must_use]
    pub fn register(&self, connection: ConnectionId, device: DeviceId) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        self.table().insert(
            connection,
            Live {
                device,
                close: Arc::new(tx),
            },
        );
        rx
    }

    /// Forget `connection`.
    pub fn unregister(&self, connection: ConnectionId) {
        self.table().remove(&connection);
    }

    /// Number of tracked connections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table().len()
    }

    /// True when nothing is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table().is_empty()
    }

    /// Tracked connections of `device`.
    #[must_use]
    pub fn count_for(&self, device: &DeviceId) -> usize {
        self.table()
            .values()
            .filter(|live| &live.device == device)
            .count()
    }

    fn senders_for(&self, device: &DeviceId) -> Vec<Arc<watch::Sender<bool>>> {
        self.table()
            .values()
            .filter(|live| &live.device == device)
            .map(|live| Arc::clone(&live.close))
            .collect()
    }

    /// Close every tracked connection (listener shutdown).
    pub fn close_all(&self) {
        for live in self.table().values() {
            live.close.send_replace(true);
        }
    }
}

/// Revoker that drives a [`SessionHost`] (live streams, queued inputs) and
/// the device registry.
pub struct HostRevoker {
    host: Arc<SessionHost>,
    live: LiveConnections,
    registry: Option<DeviceRegistry>,
    grace: Duration,
}

impl HostRevoker {
    /// Revoker for `host`. It tracks no listener connections of its own;
    /// use [`HostRevoker::with_live`] (or `NodeListener::revoker`) to also
    /// close unattached WebSocket connections.
    #[must_use]
    pub fn new(host: Arc<SessionHost>) -> Self {
        Self {
            host,
            live: LiveConnections::new(),
            registry: None,
            grace: REVOKE_GRACE,
        }
    }

    /// Share the listener's live-connection table.
    #[must_use]
    pub fn with_live(mut self, live: LiveConnections) -> Self {
        self.live = live;
        self
    }

    /// Also mark the device revoked in the registry under `state_dir`, so
    /// new handshakes are refused by the mapper.
    #[must_use]
    pub fn with_registry(mut self, state_dir: &Path) -> Self {
        self.registry = Some(DeviceRegistry::new(state_dir));
        self
    }

    /// Override the close grace (tests).
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }
}

impl RevocationSink for HostRevoker {
    fn revoke_device(&self, device: &DeviceId) -> Result<RevocationReport, ListenerError> {
        // Host first: authority is cut before anything fallible runs.
        self.host.revoke_device(device);
        let registry_result = match &self.registry {
            Some(registry) => registry.mark_revoked(device).map(|_| ()),
            None => Ok(()),
        };
        let senders = self.live.senders_for(device);
        let connections_closed = senders.len();
        let close = move || {
            for sender in &senders {
                sender.send_replace(true);
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if !self.grace.is_zero() => {
                let grace = self.grace;
                handle.spawn(async move {
                    tokio::time::sleep(grace).await;
                    close();
                });
            }
            _ => close(),
        }
        registry_result?;
        Ok(RevocationReport {
            connections_closed,
            inputs_dropped: 0,
            contexts_revoked: 0,
        })
    }
}

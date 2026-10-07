//! The live-connection table: who is connected, and a way to close them.
//!
//! The host closes the *attached streams* of a revoked device, but a
//! connection with no session attached has nothing for the host to close.
//! This table closes the WebSocket itself, per connection or per device, and
//! is what `gateway.connections.revoke`-style administration needs. Every
//! connection is tracked from the upgrade until its session ends; the entry is
//! removed by an RAII guard, also when the upgrade never completes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use harw_session_host::ConnectionId;
use harw_types::DeviceId;
use tokio::sync::watch;

struct Live {
    device: Option<DeviceId>,
    label: String,
    close: Arc<watch::Sender<bool>>,
}

/// One tracked connection, for listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveInfo {
    /// The connection id the host also knows.
    pub connection: ConnectionId,
    /// The enrolled device, for remote callers.
    pub device: Option<DeviceId>,
    /// Presence label of the identity.
    pub label: String,
}

/// Live WebSocket connections. Cheap to clone; clones share the table.
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

    /// Tracks a connection. The receiver flips to `true` when it must close;
    /// dropping the guard forgets the connection.
    #[must_use]
    pub(crate) fn register(
        &self,
        connection: ConnectionId,
        device: Option<DeviceId>,
        label: String,
    ) -> (LiveGuard, Arc<watch::Sender<bool>>, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        let close = Arc::new(tx);
        self.table().insert(
            connection,
            Live {
                device,
                label,
                close: Arc::clone(&close),
            },
        );
        (
            LiveGuard {
                table: self.clone(),
                connection,
            },
            close,
            rx,
        )
    }

    /// Number of tracked connections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table().len()
    }

    /// `true` when nothing is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table().is_empty()
    }

    /// Tracked connections of `device`.
    #[must_use]
    pub fn count_for(&self, device: &DeviceId) -> usize {
        self.table()
            .values()
            .filter(|live| live.device.as_ref() == Some(device))
            .count()
    }

    /// A snapshot of the table.
    #[must_use]
    pub fn list(&self) -> Vec<LiveInfo> {
        let mut out: Vec<LiveInfo> = self
            .table()
            .iter()
            .map(|(connection, live)| LiveInfo {
                connection: *connection,
                device: live.device.clone(),
                label: live.label.clone(),
            })
            .collect();
        out.sort_by_key(|info| info.connection);
        out
    }

    /// Closes one connection. `true` when it was tracked.
    pub fn close_connection(&self, connection: ConnectionId) -> bool {
        let close = self
            .table()
            .get(&connection)
            .map(|live| Arc::clone(&live.close));
        match close {
            Some(tx) => {
                let _ = tx.send(true);
                true
            }
            None => false,
        }
    }

    /// Closes every connection of `device`; returns how many were signalled.
    pub fn close_device(&self, device: &DeviceId) -> usize {
        let senders: Vec<_> = self
            .table()
            .values()
            .filter(|live| live.device.as_ref() == Some(device))
            .map(|live| Arc::clone(&live.close))
            .collect();
        for tx in &senders {
            let _ = tx.send(true);
        }
        senders.len()
    }

    /// Closes every tracked connection; returns how many were signalled.
    pub fn close_all(&self) -> usize {
        let senders: Vec<_> = self
            .table()
            .values()
            .map(|live| Arc::clone(&live.close))
            .collect();
        for tx in &senders {
            let _ = tx.send(true);
        }
        senders.len()
    }
}

/// Removes a connection from the table when dropped.
pub(crate) struct LiveGuard {
    table: LiveConnections,
    connection: ConnectionId,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.table.table().remove(&self.connection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str) -> Result<DeviceId, harw_types::InvalidId> {
        DeviceId::try_from_str(name)
    }

    #[test]
    fn a_registered_connection_is_listed_and_forgotten_with_its_guard()
    -> Result<(), Box<dyn std::error::Error>> {
        let table = LiveConnections::new();
        let id = ConnectionId::next();
        let (guard, _tx, _rx) = table.register(id, Some(device("dev-1")?), "phone".into());
        assert_eq!(table.len(), 1);
        assert_eq!(table.count_for(&device("dev-1")?), 1);
        assert_eq!(table.list()[0].label, "phone");
        drop(guard);
        assert!(table.is_empty());
        Ok(())
    }

    #[test]
    fn closing_a_device_signals_only_its_connections() -> Result<(), Box<dyn std::error::Error>> {
        let table = LiveConnections::new();
        let (_g1, _t1, rx1) =
            table.register(ConnectionId::next(), Some(device("dev-1")?), "a".into());
        let (_g2, _t2, rx2) =
            table.register(ConnectionId::next(), Some(device("dev-2")?), "b".into());
        let (_g3, _t3, rx3) = table.register(ConnectionId::next(), None, "local".into());
        assert_eq!(table.close_device(&device("dev-1")?), 1);
        assert!(*rx1.borrow());
        assert!(!*rx2.borrow());
        assert!(!*rx3.borrow());
        assert_eq!(table.close_all(), 3);
        assert!(*rx2.borrow() && *rx3.borrow());
        Ok(())
    }

    #[test]
    fn closing_an_unknown_connection_is_a_quiet_no_op() {
        let table = LiveConnections::new();
        assert!(!table.close_connection(ConnectionId::next()));
        assert_eq!(table.close_all(), 0);
    }
}

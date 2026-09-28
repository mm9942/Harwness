//! Bounded, per-session lifecycle events for the Streamable HTTP adapter.
//!
//! The bus is deliberately transport-independent.  It carries only
//! redacted lifecycle metadata; authentication material, lease tokens,
//! resolved inputs, and worker handles never enter an event.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use harw_job_core::JobState;
use harw_types::WorkId;
use jiff::Timestamp;
use serde::Serialize;
use tokio::sync::broadcast;

const DEFAULT_EVENT_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum McpLifecycleEventKind {
    JobUpdated {
        work_id: WorkId,
        state: JobState,
        revision: u64,
    },
    JobCancellationRequested {
        work_id: WorkId,
        revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpLifecycleEvent {
    /// Monotonically increasing within one session. Gaps mean the receiver
    /// was too slow and the bounded channel discarded older events.
    pub sequence: u64,
    pub occurred_at: Timestamp,
    pub kind: McpLifecycleEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpEventBusError {
    InvalidSessionKey,
    CapacityZero,
    SessionNotSubscribed,
    BusPoisoned,
}

impl fmt::Display for McpEventBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSessionKey => f.write_str("invalid MCP event session key"),
            Self::CapacityZero => f.write_str("MCP event capacity must be greater than zero"),
            Self::SessionNotSubscribed => f.write_str("MCP session has no event subscribers"),
            Self::BusPoisoned => f.write_str("MCP event bus lock is poisoned"),
        }
    }
}

impl std::error::Error for McpEventBusError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpEventReceiveError {
    Lagged { skipped: u64 },
    Empty,
    Closed,
}

impl fmt::Display for McpEventReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lagged { skipped } => {
                write!(f, "MCP event subscriber lagged by {skipped} events")
            }
            Self::Empty => f.write_str("MCP event subscription has no pending event"),
            Self::Closed => f.write_str("MCP event subscription closed"),
        }
    }
}

impl std::error::Error for McpEventReceiveError {}

struct EventChannel {
    sender: broadcast::Sender<McpLifecycleEvent>,
    next_sequence: u64,
}

struct EventBusInner {
    channels: Mutex<HashMap<String, EventChannel>>,
    capacity: usize,
}

/// Bounded lifecycle event fan-out keyed by an opaque MCP session id.
#[derive(Clone)]
pub struct McpEventBus {
    inner: Arc<EventBusInner>,
}

impl fmt::Debug for McpEventBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpEventBus")
            .field("capacity", &self.inner.capacity)
            .finish_non_exhaustive()
    }
}

impl McpEventBus {
    pub fn new(capacity: usize) -> Result<Self, McpEventBusError> {
        if capacity == 0 {
            return Err(McpEventBusError::CapacityZero);
        }
        Ok(Self {
            inner: Arc::new(EventBusInner {
                channels: Mutex::new(HashMap::new()),
                capacity,
            }),
        })
    }

    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self {
            inner: Arc::new(EventBusInner {
                channels: Mutex::new(HashMap::new()),
                capacity: DEFAULT_EVENT_CAPACITY,
            }),
        }
    }

    pub fn subscribe(&self, session_key: &str) -> Result<McpEventSubscription, McpEventBusError> {
        validate_session_key(session_key)?;
        let mut channels = self
            .inner
            .channels
            .lock()
            .map_err(|_| McpEventBusError::BusPoisoned)?;
        let receiver = channels
            .entry(session_key.to_owned())
            .or_insert_with(|| {
                let (sender, _) = broadcast::channel(self.inner.capacity);
                EventChannel {
                    sender,
                    next_sequence: 1,
                }
            })
            .sender
            .subscribe();
        Ok(McpEventSubscription {
            session_key: session_key.to_owned(),
            receiver,
            bus: Arc::downgrade(&self.inner),
        })
    }

    pub fn publish(
        &self,
        session_key: &str,
        occurred_at: Timestamp,
        kind: McpLifecycleEventKind,
    ) -> Result<McpLifecycleEvent, McpEventBusError> {
        validate_session_key(session_key)?;
        let mut channels = self
            .inner
            .channels
            .lock()
            .map_err(|_| McpEventBusError::BusPoisoned)?;
        let channel = channels
            .get_mut(session_key)
            .ok_or(McpEventBusError::SessionNotSubscribed)?;
        if channel.sender.receiver_count() == 0 {
            channels.remove(session_key);
            return Err(McpEventBusError::SessionNotSubscribed);
        }
        let event = McpLifecycleEvent {
            sequence: channel.next_sequence,
            occurred_at,
            kind,
        };
        channel.next_sequence = channel.next_sequence.saturating_add(1);
        // A broadcast send can only fail after the receiver count changed;
        // treat that race as a normal disconnected subscription.
        if channel.sender.send(event.clone()).is_err() {
            channels.remove(session_key);
            return Err(McpEventBusError::SessionNotSubscribed);
        }
        Ok(event)
    }

    #[must_use]
    pub fn subscriber_count(&self, session_key: &str) -> usize {
        self.inner
            .channels
            .lock()
            .ok()
            .and_then(|channels| channels.get(session_key).map(|c| c.sender.receiver_count()))
            .unwrap_or(0)
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}

/// A bounded receiver. Dropping it removes its per-session channel when the
/// final subscriber disconnects, preventing session-key memory retention.
pub struct McpEventSubscription {
    session_key: String,
    receiver: broadcast::Receiver<McpLifecycleEvent>,
    bus: Weak<EventBusInner>,
}

impl fmt::Debug for McpEventSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpEventSubscription")
            .field("session_key", &self.session_key)
            .finish_non_exhaustive()
    }
}

impl McpEventSubscription {
    #[must_use]
    pub fn session_key(&self) -> &str {
        &self.session_key
    }

    pub async fn recv(&mut self) -> Result<McpLifecycleEvent, McpEventReceiveError> {
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(skipped) => {
                McpEventReceiveError::Lagged { skipped }
            }
            broadcast::error::RecvError::Closed => McpEventReceiveError::Closed,
        })
    }

    pub fn try_recv(&mut self) -> Result<McpLifecycleEvent, McpEventReceiveError> {
        self.receiver.try_recv().map_err(|error| match error {
            broadcast::error::TryRecvError::Empty => McpEventReceiveError::Empty,
            broadcast::error::TryRecvError::Lagged(skipped) => {
                McpEventReceiveError::Lagged { skipped }
            }
            broadcast::error::TryRecvError::Closed => McpEventReceiveError::Closed,
        })
    }
}

impl Drop for McpEventSubscription {
    fn drop(&mut self) {
        let Some(inner) = self.bus.upgrade() else {
            return;
        };
        let Ok(mut channels) = inner.channels.lock() else {
            return;
        };
        let remove = channels
            .get(&self.session_key)
            .is_some_and(|channel| channel.sender.receiver_count() <= 1);
        if remove {
            channels.remove(&self.session_key);
        }
    }
}

fn validate_session_key(session_key: &str) -> Result<(), McpEventBusError> {
    if session_key.is_empty()
        || !session_key
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(McpEventBusError::InvalidSessionKey);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn updated_event(revision: u64) -> McpLifecycleEventKind {
        McpLifecycleEventKind::JobUpdated {
            work_id: WorkId::new(),
            state: JobState::Running,
            revision,
        }
    }

    #[tokio::test]
    async fn bounded_subscriber_reports_lag_without_growing_queue() -> TestResult {
        let bus = McpEventBus::new(2).map_err(ctx("bus new"))?;
        let mut subscription = bus.subscribe("session-a").map_err(ctx("subscribe"))?;
        let now = Timestamp::now();
        bus.publish("session-a", now, updated_event(1))
            .map_err(ctx("publish 1"))?;
        bus.publish("session-a", now, updated_event(2))
            .map_err(ctx("publish 2"))?;
        bus.publish("session-a", now, updated_event(3))
            .map_err(ctx("publish 3"))?;
        assert!(matches!(
            subscription.recv().await,
            Err(McpEventReceiveError::Lagged { skipped: 1 })
        ));
        let event = subscription.recv().await.map_err(ctx("recv"))?;
        assert_eq!(event.sequence, 2);
        assert_eq!(bus.subscriber_count("session-a"), 1);
        Ok(())
    }

    #[test]
    fn dropping_final_subscriber_cleans_up_session_channel() -> TestResult {
        let bus = McpEventBus::new(1).map_err(ctx("bus new"))?;
        let subscription = bus.subscribe("session-a").map_err(ctx("subscribe"))?;
        assert_eq!(bus.subscriber_count("session-a"), 1);
        drop(subscription);
        assert_eq!(bus.subscriber_count("session-a"), 0);
        assert!(matches!(
            bus.publish("session-a", Timestamp::now(), updated_event(1)),
            Err(McpEventBusError::SessionNotSubscribed)
        ));
        Ok(())
    }

    #[test]
    fn event_kind_wire_tags_match_contract() -> TestResult {
        // Locks the SSE wire contract: renaming or removing a variant here
        // must be a deliberate, visible change to the serialized `type` tag.
        let updated = serde_json::to_value(&updated_event(1)).map_err(ctx("serialize update"))?;
        assert_eq!(updated["type"], "job_updated");

        let cancellation = McpLifecycleEventKind::JobCancellationRequested {
            work_id: WorkId::new(),
            revision: 1,
        };
        let cancellation =
            serde_json::to_value(&cancellation).map_err(ctx("serialize cancellation"))?;
        assert_eq!(cancellation["type"], "job_cancellation_requested");
        Ok(())
    }

    #[test]
    fn capacity_and_session_keys_are_validated() -> TestResult {
        assert!(matches!(
            McpEventBus::new(0),
            Err(McpEventBusError::CapacityZero)
        ));
        let bus = McpEventBus::new(1).map_err(ctx("bus new"))?;
        assert!(matches!(
            bus.subscribe(""),
            Err(McpEventBusError::InvalidSessionKey)
        ));
        Ok(())
    }
}

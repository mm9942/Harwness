//! Port to the agent runtime (W00 §2.4 `driver.rs`).
//!
//! The host decides *when* a turn runs (arbiter, single writer); the driver
//! decides *how*: model calls, tools, transcript writes. The production
//! driver wraps `harw-core` (`run_turn_durable`, `resume_after_approval`) in
//! the gateway composition; tests use a scripted driver.
//!
//! Driver events are pushed into an [`EventSink`] that never blocks: the
//! host maps them to frames and fans them out, so a slow client can never
//! slow the turn loop (W00 D8).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use harw_protocol::{ApprovalRequest, SessionEvent, TurnEvent};
use harw_types::{ApprovalActor, SessionId};

use crate::error::HostError;
use crate::identity::ConnectionId;

/// Boxed future of a driver call.
pub type DriverFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + 'a>>;

/// Cancellation signal of one turn: flips to `true` on interrupt or drain.
pub type CancelSignal = tokio::sync::watch::Receiver<bool>;

/// What the runtime reports while it works.
#[derive(Clone, Debug)]
pub enum DriverEvent {
    /// A turn event of the root session.
    Turn(TurnEvent),
    /// A session lifecycle event.
    Session(SessionEvent),
    /// A turn event of a child agent.
    Child {
        agent: SessionId,
        parent: Option<SessionId>,
        role: String,
        event: TurnEvent,
    },
    /// The turn parked on an approval.
    ApprovalRequested(ApprovalRequest),
    /// The durable transcript advanced; the host re-reads the head.
    DurableAdvanced,
}

/// Non-blocking sink for driver events of one session.
pub trait EventSink: Send + Sync {
    /// Record one event. Must not block and must not fail.
    fn emit(&self, event: DriverEvent);
}

/// One queued user submission.
#[derive(Clone, Debug)]
pub struct TurnInput {
    pub session_id: SessionId,
    pub text: String,
    pub client_msg_id: String,
    /// Presence label of the submitting client (audit/log only).
    pub submitted_by: String,
    /// Host-derived actor of the submitting client.
    pub actor: ApprovalActor,
    /// Connection that submitted it (revocation drops its queued inputs).
    pub origin: ConnectionId,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The turn finished; the session is idle.
    Completed,
    /// The turn was cancelled through its [`CancelSignal`].
    Interrupted,
    /// The turn parked on an approval; the host resumes it after the
    /// approval is resolved.
    AwaitingApproval,
    /// The turn failed.
    Failed(String),
}

/// A session setting change, applied at the turn boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    Model(String),
    Mode(String),
    Effort(String),
}

/// The runtime behind the host.
pub trait TurnDriver: Send + Sync + 'static {
    /// Prepare durable state for a new session (transcript, meta).
    fn create_session(&self, session_id: &SessionId, title: Option<&str>) -> DriverFuture<'_, ()>;

    /// Run one user turn to completion, to an approval park, or until
    /// `cancel` flips.
    fn run_turn(
        &self,
        input: TurnInput,
        cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome>;

    /// Continue a turn that parked on an approval which is now resolved.
    fn resume_after_approval(
        &self,
        session_id: &SessionId,
        cancel: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome>;

    /// Apply a setting before the next turn starts.
    fn apply_setting(&self, session_id: &SessionId, setting: Setting) -> DriverFuture<'_, ()>;

    /// Model name for summaries, when known.
    fn model_name(&self, session_id: &SessionId) -> Option<String>;
}

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
    /// The turn failed for an untyped reason (shown to clients as
    /// [`FailureCause::Internal`]).
    Failed(String),
    /// The turn failed for a known cause (provider auth, quota, context
    /// length, refusal, ...). Drivers should prefer this over
    /// [`Self::Failed`] so clients can show an actionable category.
    FailedWith {
        cause: FailureCause,
        /// Raw reason; the host bounds and sanitizes it before it reaches a
        /// client.
        reason: String,
    },
}

/// Why a hosted turn failed, as shown to clients.
///
/// # Failure surfacing (liveness contract)
/// A failed turn never leaves a session dead from the client's point of
/// view. The host (1) publishes one visible `SessionError` frame per failed
/// turn (`"turn failed (<label>): <bounded, sanitized message>"`) and (2)
/// returns the hosted state to `Idle` (or `Queued` when more input waits), so
/// the next submit is accepted. `HostedState::Failed` is deliberately not
/// produced: it exists on the wire for compatibility, but a state the client
/// cannot leave would contradict this contract, and the failure is already
/// reported as an event carrying its cause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCause {
    /// Provider rejected the credentials (HTTP 401/403, expired token).
    ProviderAuth,
    /// Provider quota or budget exhausted.
    Quota,
    /// The request exceeded the model's context window.
    ContextLength,
    /// The model refused the request or its output.
    Refusal,
    /// The model request failed (transport, rate limit, timeout, truncated or
    /// empty response, context assembly).
    RequestFailed,
    /// Anything else: store errors, tool or invariant failures.
    Internal,
}

impl FailureCause {
    /// Human-readable category used in the client notice.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProviderAuth => "provider authentication failed",
            Self::Quota => "provider quota or budget exhausted",
            Self::ContextLength => "context length exceeded",
            Self::Refusal => "model refused",
            Self::RequestFailed => "model request failed",
            Self::Internal => "internal error",
        }
    }
}

/// Longest message (in characters) forwarded to clients.
pub const MAX_NOTICE_CHARS: usize = 300;

/// Make an untrusted provider/error message safe to show: control characters
/// (including ANSI escape introducers) count as whitespace, whitespace runs
/// collapse to one space, and the result is cut to `max_chars` characters
/// plus an ellipsis.
#[must_use]
pub fn sanitize_notice(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_control() || ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if count >= max_chars {
            out.push('\u{2026}');
            return out;
        }
        if pending_space {
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        out.push(ch);
        count += 1;
    }
    out
}

impl TurnOutcome {
    /// Cause and raw reason of a failed outcome; `None` for other outcomes.
    #[must_use]
    pub fn failure(&self) -> Option<(FailureCause, &str)> {
        match self {
            Self::Failed(reason) => Some((FailureCause::Internal, reason)),
            Self::FailedWith { cause, reason } => Some((*cause, reason)),
            _ => None,
        }
    }
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

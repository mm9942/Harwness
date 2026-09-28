//! Client-side session port (PL-65 §3.3, W00 contract D1).
//!
//! One semantic contract for every transport: the in-process host port, the
//! WebSocket on a local Unix socket and the WebSocket over the node
//! transport. Only std futures appear here, so this crate stays free of any
//! async runtime.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use harw_types::SessionId;

use crate::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, FrameEnvelope, HelloAck,
    HelloParams, HistoryParams, InterruptParams, RespondResult, SessionSummary, SetEffortParams,
    SetModeParams, SetModelParams, SubmitParams, SubmitResult,
};

/// Boxed future returned by every port method.
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PortError>> + Send + 'a>>;

/// A stream of frames for one attachment. `Ok(None)` ends the stream.
pub trait FrameSource: Send {
    /// Next frame, or `None` when the attachment ended.
    fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>>;
}

/// The session control plane as seen by a client.
pub trait SessionPort: Send + Sync {
    fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck>;
    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>>;
    fn create(&self, params: CreateParams) -> PortFuture<'_, SessionSummary>;
    fn attach(&self, params: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)>;
    fn detach(&self, session: SessionId) -> PortFuture<'_, ()>;
    fn history(&self, params: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>>;
    fn submit(&self, params: SubmitParams) -> PortFuture<'_, SubmitResult>;
    fn interrupt(&self, params: InterruptParams) -> PortFuture<'_, ()>;
    fn resume(&self, session: SessionId) -> PortFuture<'_, ()>;
    fn close(&self, session: SessionId) -> PortFuture<'_, ()>;
    fn respond(&self, params: ApprovalRespondParams) -> PortFuture<'_, RespondResult>;
    fn set_model(&self, params: SetModelParams) -> PortFuture<'_, ()>;
    fn set_mode(&self, params: SetModeParams) -> PortFuture<'_, ()>;
    fn set_effort(&self, params: SetEffortParams) -> PortFuture<'_, ()>;
}

/// Why a port call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortError {
    /// The connection failed or closed.
    Transport(String),
    /// Capability, tenant or hello admission refused the call.
    Denied(String),
    /// Unknown session or approval.
    NotFound,
    /// The peer violated the protocol.
    Protocol(String),
    /// The client's authority was revoked.
    Revoked,
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(detail) => write!(f, "session transport failed: {detail}"),
            Self::Denied(reason) => write!(f, "session call denied: {reason}"),
            Self::NotFound => f.write_str("session or approval not found"),
            Self::Protocol(detail) => write!(f, "session protocol violation: {detail}"),
            Self::Revoked => f.write_str("session access revoked"),
        }
    }
}

impl std::error::Error for PortError {}

impl PortError {
    /// Stable wire code for this error (see
    /// [`crate::session_wire::error_codes`]).
    #[must_use]
    pub const fn code(&self) -> i32 {
        use crate::session_wire::error_codes;
        match self {
            Self::Transport(_) => error_codes::INTERNAL,
            Self::Denied(_) => error_codes::DENIED,
            Self::NotFound => error_codes::NOT_FOUND,
            Self::Protocol(_) => error_codes::INVALID_PARAMS,
            Self::Revoked => error_codes::REVOKED,
        }
    }

    /// Map a wire error code and message back to a port error.
    #[must_use]
    pub fn from_code(code: i32, message: String) -> Self {
        use crate::session_wire::error_codes;
        match code {
            error_codes::DENIED | error_codes::HELLO_REQUIRED => Self::Denied(message),
            error_codes::NOT_FOUND => Self::NotFound,
            error_codes::REVOKED => Self::Revoked,
            error_codes::INVALID_PARAMS
            | error_codes::METHOD_NOT_FOUND
            | error_codes::PARSE_ERROR => Self::Protocol(message),
            _ => Self::Transport(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PortError;

    #[test]
    fn port_error_codes_round_trip() {
        for error in [
            PortError::Denied("caps".into()),
            PortError::NotFound,
            PortError::Revoked,
            PortError::Protocol("bad".into()),
        ] {
            let back = PortError::from_code(
                error.code(),
                match &error {
                    PortError::Denied(m) | PortError::Protocol(m) => m.clone(),
                    _ => String::new(),
                },
            );
            assert_eq!(back, error);
        }
    }

    #[test]
    fn port_error_is_a_std_error() {
        fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_error::<PortError>();
        assert_eq!(PortError::Revoked.to_string(), "session access revoked");
    }
}

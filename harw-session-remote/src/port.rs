//! The [`SessionPort`] implementation over a [`RemoteConnection`] (S06).
//!
//! Skeleton: every method answers a typed not-implemented [`PortError`]
//! (built from [`RemoteError::NotImplemented`]).

use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, FrameEnvelope, HelloAck,
    HelloParams, HistoryParams, InterruptParams, RespondResult, SessionSummary, SetEffortParams,
    SetModeParams, SetModelParams, SubmitParams, SubmitResult,
};
use harw_protocol::{FrameSource, PortError, PortFuture, SessionPort};
use harw_types::SessionId;

use crate::RemoteError;
use crate::conn::RemoteConnection;

/// A [`SessionPort`] that talks to a remote host over one connection.
#[derive(Debug)]
pub struct RemotePort {
    _connection: RemoteConnection,
}

impl RemotePort {
    /// Wrap an established connection.
    #[must_use]
    pub fn new(connection: RemoteConnection) -> Self {
        Self {
            _connection: connection,
        }
    }
}

fn stub<T: Send + 'static>(what: &'static str) -> PortFuture<'static, T> {
    Box::pin(async move { Err(PortError::from(RemoteError::NotImplemented(what))) })
}

impl SessionPort for RemotePort {
    fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck> {
        let _ = params;
        stub("RemotePort::hello (S06)")
    }

    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        stub("RemotePort::list (S06)")
    }

    fn create(&self, params: CreateParams) -> PortFuture<'_, SessionSummary> {
        let _ = params;
        stub("RemotePort::create (S06)")
    }

    fn attach(&self, params: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
        let _ = params;
        stub("RemotePort::attach (S06)")
    }

    fn detach(&self, session: SessionId) -> PortFuture<'_, ()> {
        let _ = session;
        stub("RemotePort::detach (S06)")
    }

    fn history(&self, params: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
        let _ = params;
        stub("RemotePort::history (S06)")
    }

    fn submit(&self, params: SubmitParams) -> PortFuture<'_, SubmitResult> {
        let _ = params;
        stub("RemotePort::submit (S06)")
    }

    fn interrupt(&self, params: InterruptParams) -> PortFuture<'_, ()> {
        let _ = params;
        stub("RemotePort::interrupt (S06)")
    }

    fn resume(&self, session: SessionId) -> PortFuture<'_, ()> {
        let _ = session;
        stub("RemotePort::resume (S06)")
    }

    fn close(&self, session: SessionId) -> PortFuture<'_, ()> {
        let _ = session;
        stub("RemotePort::close (S06)")
    }

    fn respond(&self, params: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
        let _ = params;
        stub("RemotePort::respond (S06)")
    }

    fn set_model(&self, params: SetModelParams) -> PortFuture<'_, ()> {
        let _ = params;
        stub("RemotePort::set_model (S06)")
    }

    fn set_mode(&self, params: SetModeParams) -> PortFuture<'_, ()> {
        let _ = params;
        stub("RemotePort::set_mode (S06)")
    }

    fn set_effort(&self, params: SetEffortParams) -> PortFuture<'_, ()> {
        let _ = params;
        stub("RemotePort::set_effort (S06)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_error_maps_to_transport() {
        let error = PortError::from(RemoteError::NotImplemented("x"));
        assert!(matches!(error, PortError::Transport(_)));
    }
}

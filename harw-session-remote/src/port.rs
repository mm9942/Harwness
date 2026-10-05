//! The [`SessionPort`] implementation over a [`RemoteConnection`] (S06).
//!
//! Every method is one correlated request on the shared connection, so any
//! number of calls (and sessions) can run concurrently. Host errors come back
//! as typed [`PortError`]s through the stable wire codes; a dead connection
//! is [`PortError::Transport`].

use harw_protocol::methods::{
    METHOD_APPROVAL_RESP, METHOD_SESSION_ATTACH, METHOD_SESSION_CLOSE, METHOD_SESSION_CREATE,
    METHOD_SESSION_DETACH, METHOD_SESSION_HISTORY, METHOD_SESSION_LIST, METHOD_SESSION_RESUME,
    METHOD_SESSION_SET_EFFORT, METHOD_SESSION_SET_MODE, METHOD_SESSION_SET_MODEL,
    METHOD_TURN_INTERRUPT, METHOD_TURN_SUBMIT,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, FrameEnvelope, HelloAck,
    HelloParams, HistoryParams, HistoryResult, InterruptParams, ListResult, RespondResult,
    SessionRef, SessionSummary, SetEffortParams, SetModeParams, SetModelParams, SubmitParams,
    SubmitResult,
};
use harw_protocol::{FrameSource, PortFuture, SessionPort};
use harw_types::SessionId;

use crate::conn::{Effect, RemoteConnection, RemoteFrames, Subscription};

/// A [`SessionPort`] that talks to a remote host over one connection.
#[derive(Debug)]
pub struct RemotePort {
    connection: RemoteConnection,
}

impl RemotePort {
    /// Wrap an established connection.
    #[must_use]
    pub fn new(connection: RemoteConnection) -> Self {
        Self { connection }
    }

    /// The underlying connection (hello acknowledgement, etc.).
    #[must_use]
    pub fn connection(&self) -> &RemoteConnection {
        &self.connection
    }
}

fn session_ref(session: SessionId) -> SessionRef {
    SessionRef {
        session_id: session,
    }
}

impl SessionPort for RemotePort {
    /// `session.hello` already ran when the connection was established (the
    /// host refuses a second one); this returns that acknowledgement. The
    /// label and caps of `params` were fixed at connect time through
    /// [`crate::ConnectOptions`].
    fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck> {
        let _ = params;
        Box::pin(async move { Ok(self.connection.hello_ack().clone()) })
    }

    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        Box::pin(async move {
            let result: ListResult = self
                .connection
                .call(METHOD_SESSION_LIST, &serde_json::Value::Null, Effect::None)
                .await?;
            Ok(result.sessions)
        })
    }

    fn create(&self, params: CreateParams) -> PortFuture<'_, SessionSummary> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_CREATE, &params, Effect::None)
                .await
        })
    }

    fn attach(&self, params: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
        Box::pin(async move {
            // The subscription is registered by the reader in the same step
            // that resolves this response, so no frame can be missed.
            let sub = Subscription::new();
            let ack: AttachAck = self
                .connection
                .call(
                    METHOD_SESSION_ATTACH,
                    &params,
                    Effect::Subscribe(params.session_id.clone(), sub.clone()),
                )
                .await?;
            let frames: Box<dyn FrameSource> = Box::new(RemoteFrames::new(sub));
            Ok((ack, frames))
        })
    }

    fn detach(&self, session: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async move {
            let effect = Effect::Unsubscribe(session.clone());
            self.connection
                .call(METHOD_SESSION_DETACH, &session_ref(session), effect)
                .await
        })
    }

    fn history(&self, params: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
        Box::pin(async move {
            let result: HistoryResult = self
                .connection
                .call(METHOD_SESSION_HISTORY, &params, Effect::None)
                .await?;
            Ok(result.frames)
        })
    }

    fn submit(&self, params: SubmitParams) -> PortFuture<'_, SubmitResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_TURN_SUBMIT, &params, Effect::None)
                .await
        })
    }

    fn interrupt(&self, params: InterruptParams) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_TURN_INTERRUPT, &params, Effect::None)
                .await
        })
    }

    fn resume(&self, session: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_RESUME, &session_ref(session), Effect::None)
                .await
        })
    }

    fn close(&self, session: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_CLOSE, &session_ref(session), Effect::None)
                .await
        })
    }

    fn respond(&self, params: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_APPROVAL_RESP, &params, Effect::None)
                .await
        })
    }

    fn set_model(&self, params: SetModelParams) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_SET_MODEL, &params, Effect::None)
                .await
        })
    }

    fn set_mode(&self, params: SetModeParams) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_SET_MODE, &params, Effect::None)
                .await
        })
    }

    fn set_effort(&self, params: SetEffortParams) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_SET_EFFORT, &params, Effect::None)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use harw_protocol::PortError;

    use crate::RemoteError;

    #[test]
    fn remote_errors_map_to_port_errors() {
        let error = PortError::from(RemoteError::NotImplemented("x"));
        assert!(matches!(error, PortError::Transport(_)));
        let error = PortError::from(RemoteError::Protocol("p".into()));
        assert!(matches!(error, PortError::Protocol(_)));
    }
}

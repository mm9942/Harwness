//! The [`SessionPort`] implementation over a [`RemoteConnection`] (S06).
//!
//! Every method is one correlated request on the shared connection, so any
//! number of calls (and sessions) can run concurrently. Host errors come back
//! as typed [`PortError`]s through the stable wire codes; a dead connection
//! is [`PortError::Transport`].

use harw_protocol::methods::{
    METHOD_APPROVAL_RESP, METHOD_CATALOG_GET, METHOD_COMMAND_RUN, METHOD_COMMANDS_LIST,
    METHOD_CONFIG_GET, METHOD_CONFIG_RESET, METHOD_CONFIG_SET, METHOD_MEDIA_GET, METHOD_SECRET_PUT,
    METHOD_SESSION_ADOPT, METHOD_SESSION_ARCHIVE, METHOD_SESSION_ATTACH, METHOD_SESSION_CLOSE,
    METHOD_SESSION_CREATE, METHOD_SESSION_CREATE_IN, METHOD_SESSION_DETACH, METHOD_SESSION_HISTORY,
    METHOD_SESSION_LIST, METHOD_SESSION_PAGE, METHOD_SESSION_PROJECTS, METHOD_SESSION_RENAME,
    METHOD_SESSION_RESUME, METHOD_SESSION_SET_EFFORT, METHOD_SESSION_SET_MODE,
    METHOD_SESSION_SET_MODEL, METHOD_SESSION_SET_PROJECT, METHOD_STEPUP_CANCEL,
    METHOD_STEPUP_CONFIRM, METHOD_TURN_INTERRUPT, METHOD_TURN_SUBMIT,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, ArchiveParams, AttachAck, AttachParams, CatalogGetParams, CatalogResult,
    CommandRunParams, CommandRunResult, CommandsListParams, CommandsListResult, ConfigGetParams,
    ConfigGetResult, ConfigResetParams, ConfigSetParams, CreateInParams, CreateParams,
    FrameEnvelope, HelloAck, HelloParams, HistoryParams, HistoryResult, InterruptParams,
    ListResult, MediaGetParams, MediaGetResult, PageParams, PageResult, ProjectsParams, ProjectsResult, RenameParams,
    RespondResult, SecretPutParams, SecretPutResult, SessionRef, SessionRow, SessionSummary,
    SetEffortParams, SetModeParams, SetModelParams, SetProjectParams, StepUpCancelParams,
    StepUpConfirmParams, SubmitParams, SubmitResult,
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

    /// Explicit resume/reconnect path (h19): fills `AttachParams::from` with
    /// the newest cursor delivered for the session when the caller left it
    /// `None`, and never fabricates one. Plain [`SessionPort::attach`] does
    /// not call this: `from: None` there means a fresh attach from the tail.
    ///
    /// The host enforces generation correctness on its own (a stale or
    /// foreign-generation cursor triggers a resync, see `attach_sync`);
    /// this helper only hands over what was actually observed.
    #[must_use]
    pub fn attach_with_resume(&self, params: AttachParams) -> AttachParams {
        if params.from.is_none()
            && let Some(cursor) = self.connection.resume_cursor(&params.session_id)
        {
            return AttachParams {
                from: Some(cursor),
                ..params
            };
        }
        params
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
    fn projects(&self, params: ProjectsParams) -> PortFuture<'_, ProjectsResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_PROJECTS, &params, Effect::None)
                .await
        })
    }

    fn media_get(&self, params: MediaGetParams) -> PortFuture<'_, MediaGetResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_MEDIA_GET, &params, Effect::None)
                .await
        })
    }

    fn page(&self, params: PageParams) -> PortFuture<'_, PageResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_PAGE, &params, Effect::None)
                .await
        })
    }

    fn adopt(&self, session: SessionId) -> PortFuture<'_, SessionSummary> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_ADOPT, &session_ref(session), Effect::None)
                .await
        })
    }

    fn create_in(&self, params: CreateInParams) -> PortFuture<'_, SessionSummary> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_CREATE_IN, &params, Effect::None)
                .await
        })
    }

    fn set_project(&self, params: SetProjectParams) -> PortFuture<'_, SessionRow> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_SET_PROJECT, &params, Effect::None)
                .await
        })
    }

    fn rename(&self, params: RenameParams) -> PortFuture<'_, SessionRow> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_RENAME, &params, Effect::None)
                .await
        })
    }

    fn archive(&self, params: ArchiveParams) -> PortFuture<'_, SessionRow> {
        Box::pin(async move {
            self.connection
                .call(METHOD_SESSION_ARCHIVE, &params, Effect::None)
                .await
        })
    }

    fn commands_list(&self, params: CommandsListParams) -> PortFuture<'_, CommandsListResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_COMMANDS_LIST, &params, Effect::None)
                .await
        })
    }

    fn command_run(&self, params: CommandRunParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_COMMAND_RUN, &params, Effect::None)
                .await
        })
    }

    fn catalog_get(&self, params: CatalogGetParams) -> PortFuture<'_, CatalogResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_CATALOG_GET, &params, Effect::None)
                .await
        })
    }

    fn config_get(&self, params: ConfigGetParams) -> PortFuture<'_, ConfigGetResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_CONFIG_GET, &params, Effect::None)
                .await
        })
    }

    fn config_set(&self, params: ConfigSetParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_CONFIG_SET, &params, Effect::None)
                .await
        })
    }

    fn config_reset(&self, params: ConfigResetParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_CONFIG_RESET, &params, Effect::None)
                .await
        })
    }

    fn secret_put(&self, params: SecretPutParams) -> PortFuture<'_, SecretPutResult> {
        // `params` is dropped after sending; never logged or cloned.
        Box::pin(async move {
            self.connection
                .call(METHOD_SECRET_PUT, &params, Effect::None)
                .await
        })
    }

    fn stepup_confirm(&self, params: StepUpConfirmParams) -> PortFuture<'_, CommandRunResult> {
        Box::pin(async move {
            self.connection
                .call(METHOD_STEPUP_CONFIRM, &params, Effect::None)
                .await
        })
    }

    fn stepup_cancel(&self, params: StepUpCancelParams) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.connection
                .call(METHOD_STEPUP_CANCEL, &params, Effect::None)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use harw_protocol::PortError;
    use harw_protocol::methods::{
        METHOD_CATALOG_GET, METHOD_COMMAND_RUN, METHOD_COMMANDS_LIST, METHOD_CONFIG_GET,
        METHOD_CONFIG_RESET, METHOD_CONFIG_SET, METHOD_SECRET_PUT, METHOD_STEPUP_CANCEL,
        METHOD_STEPUP_CONFIRM,
    };
    use harw_protocol::session_wire::error_codes;

    use crate::RemoteError;

    #[test]
    fn remote_errors_map_to_port_errors() {
        let error = PortError::from(RemoteError::NotImplemented("x"));
        assert!(matches!(error, PortError::Transport(_)));
        let error = PortError::from(RemoteError::Protocol("p".into()));
        assert!(matches!(error, PortError::Protocol(_)));
    }

    #[test]
    fn busy_code_maps_to_port_busy_not_transport() {
        let error = PortError::from_code(error_codes::BUSY, "synthetic reason".into());
        assert_eq!(error, PortError::Busy("synthetic reason".into()));
        assert!(!matches!(error, PortError::Transport(_)));
    }

    #[test]
    fn manage_method_names_are_wired() {
        let table = [
            (METHOD_COMMANDS_LIST, "commands.list"),
            (METHOD_COMMAND_RUN, "command.run"),
            (METHOD_CATALOG_GET, "catalog.get"),
            (METHOD_CONFIG_GET, "config.get"),
            (METHOD_CONFIG_SET, "config.set"),
            (METHOD_CONFIG_RESET, "config.reset"),
            (METHOD_SECRET_PUT, "secret.put"),
            (METHOD_STEPUP_CONFIRM, "stepup.confirm"),
            (METHOD_STEPUP_CANCEL, "stepup.cancel"),
        ];
        for (constant, name) in table {
            assert_eq!(constant, name);
        }
    }
}

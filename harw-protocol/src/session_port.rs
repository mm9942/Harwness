//! Client-side session port (PL-65 §3.3, W00 contract D1).
//!
//! One semantic contract for every transport: the in-process host port, the
//! WebSocket on a local Unix socket and the WebSocket over the node
//! transport. Only std futures appear here, so this crate stays free of any
//! async runtime.
//!
//! R18 (contract `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`)
//! adds two separate, equally transport-independent ports: [`ToolPort`]
//! (`tool.*`, agent principals only) and [`GatewayPort`] (`gateway.*`). They
//! are separate traits so existing [`SessionPort`] implementations stay
//! unchanged.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use harw_types::SessionId;

use crate::methods::{
    METHOD_AGENTS_LIST, METHOD_AGENTS_STOP, METHOD_JOBS_CANCEL, METHOD_JOBS_GET, METHOD_JOBS_LIST,
    METHOD_JOBS_RETRY,
    METHOD_CATALOG_GET, METHOD_COMMAND_RUN, METHOD_COMMANDS_LIST, METHOD_CONFIG_GET,
    METHOD_CONFIG_RESET, METHOD_CONFIG_SET, METHOD_MEDIA_GET, METHOD_SECRET_PUT, METHOD_SESSION_ADOPT,
    METHOD_SESSION_ARCHIVE, METHOD_SESSION_CREATE_IN, METHOD_SESSION_PAGE, METHOD_SESSION_PROJECTS,
    METHOD_SESSION_RENAME, METHOD_SESSION_SET_PROJECT, METHOD_STEPUP_CANCEL, METHOD_STEPUP_CONFIRM,
};
use crate::session_wire::{
    AgentStopParams, AgentsListParams, AgentsListResult, JobDetail, JobTargetParams, JobsListParams,
    JobsListResult, ApprovalRespondParams, ArchiveParams, AttachAck, AttachParams, CatalogGetParams, CatalogResult,
    CommandRunParams, CommandRunResult, CommandsListParams, CommandsListResult, ConfigGetParams,
    ConfigGetResult, ConfigResetParams, ConfigSetParams, CreateInParams, CreateParams,
    FrameEnvelope, GatewayConnectionsResult, GatewayDrainParams, GatewayListenerInfo,
    GatewayListenerSetParams, GatewayListenersResult, GatewayRevokeParams, GatewayRevokeResult,
    GatewayStatus, GatewayToolRights, GatewayToolRightsParams, GatewayToolsResult, HelloAck,
    HelloParams, HistoryParams, InterruptParams, MediaGetParams, MediaGetResult, PageParams, PageResult, ProjectsParams,
    ProjectsResult, RenameParams, RespondResult, SecretPutParams, SecretPutResult, SessionRow,
    SessionSummary, SetEffortParams, SetModeParams, SetModelParams, SetProjectParams,
    StepUpCancelParams, StepUpConfirmParams, SubmitParams, SubmitResult, ToolCallParams,
    ToolCallResultFrame, ToolCancelParams, ToolListParams, ToolListResult,
};

/// Boxed future returned by every port method.
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, PortError>> + Send + 'a>>;

/// Answer of a port implementation that does not serve a minor-3 or minor-4 method.
fn unsupported<'a, T: Send + 'a>(method: &'static str) -> PortFuture<'a, T> {
    Box::pin(std::future::ready(Err(PortError::Protocol(format!(
        "{method} is not served by this port"
    )))))
}

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

    /// `session.projects` (observe, feature `projects`).
    fn projects(&self, _params: ProjectsParams) -> PortFuture<'_, ProjectsResult> {
        unsupported(METHOD_SESSION_PROJECTS)
    }
    /// `media.get` (observe, feature `media`): the stored bytes of a prompt image of the session.
    fn media_get(&self, _params: MediaGetParams) -> PortFuture<'_, MediaGetResult> {
        unsupported(METHOD_MEDIA_GET)
    }
    /// `session.page` (observe).
    fn page(&self, _params: PageParams) -> PortFuture<'_, PageResult> {
        unsupported(METHOD_SESSION_PAGE)
    }
    /// `session.adopt` (steer + mode continue). Same code path as adoption on `submit`.
    fn adopt(&self, _session: SessionId) -> PortFuture<'_, SessionSummary> {
        unsupported(METHOD_SESSION_ADOPT)
    }
    /// `session.create_in` (steer + continue).
    fn create_in(&self, _params: CreateInParams) -> PortFuture<'_, SessionSummary> {
        unsupported(METHOD_SESSION_CREATE_IN)
    }
    /// `session.set_project` (steer + continue); returns the updated row.
    fn set_project(&self, _params: SetProjectParams) -> PortFuture<'_, SessionRow> {
        unsupported(METHOD_SESSION_SET_PROJECT)
    }
    /// `session.rename` (steer + continue); returns the updated row.
    fn rename(&self, _params: RenameParams) -> PortFuture<'_, SessionRow> {
        unsupported(METHOD_SESSION_RENAME)
    }
    /// `session.archive` (steer + continue); returns the updated row.
    fn archive(&self, _params: ArchiveParams) -> PortFuture<'_, SessionRow> {
        unsupported(METHOD_SESSION_ARCHIVE)
    }
    /// `commands.list` (observe, feature `manage`): every command like the TUI popup, with availability.
    fn commands_list(&self, _params: CommandsListParams) -> PortFuture<'_, CommandsListResult> {
        unsupported(METHOD_COMMANDS_LIST)
    }
    /// `command.run` (observe at the host; the allow-list decides per form, W00 D5 amendment).
    fn command_run(&self, _params: CommandRunParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_COMMAND_RUN)
    }
    /// `catalog.get` (observe): redacted providers, models, roles, live model.
    fn catalog_get(&self, _params: CatalogGetParams) -> PortFuture<'_, CatalogResult> {
        unsupported(METHOD_CATALOG_GET)
    }
    /// `config.get` (observe): allow-listed keys only.
    fn config_get(&self, _params: ConfigGetParams) -> PortFuture<'_, ConfigGetResult> {
        unsupported(METHOD_CONFIG_GET)
    }
    /// `config.set` (control + approve, feature `stepup`): answers a step-up challenge, writes nothing.
    fn config_set(&self, _params: ConfigSetParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_CONFIG_SET)
    }
    /// `config.reset` (control + approve, feature `stepup`).
    fn config_reset(&self, _params: ConfigResetParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_CONFIG_RESET)
    }
    /// `secret.put` (control + approve, feature `stepup`): write-only staging, never returns the value.
    fn secret_put(&self, _params: SecretPutParams) -> PortFuture<'_, SecretPutResult> {
        unsupported(METHOD_SECRET_PUT)
    }
    /// `stepup.confirm` (control + approve): single use, owner bound, 60 s.
    fn stepup_confirm(&self, _params: StepUpConfirmParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_STEPUP_CONFIRM)
    }
    /// `stepup.cancel` (own step-ups only; an unknown id answers `NotFound`, never `Ok(())`).
    fn stepup_cancel(&self, _params: StepUpCancelParams) -> PortFuture<'_, ()> {
        unsupported(METHOD_STEPUP_CANCEL)
    }
    /// `jobs.list` (observe, feature `overview`): the durable jobs of the profile.
    fn jobs_list(&self, _params: JobsListParams) -> PortFuture<'_, JobsListResult> {
        unsupported(METHOD_JOBS_LIST)
    }
    /// `jobs.get` (observe): detail of one job.
    fn jobs_get(&self, _params: JobTargetParams) -> PortFuture<'_, JobDetail> {
        unsupported(METHOD_JOBS_GET)
    }
    /// `jobs.cancel` (steer): cancels a pending, ready or running job.
    fn jobs_cancel(&self, _params: JobTargetParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_JOBS_CANCEL)
    }
    /// `jobs.retry` (steer): requeues a failed or cancelled job.
    fn jobs_retry(&self, _params: JobTargetParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_JOBS_RETRY)
    }
    /// `agents.list` (observe): running child agents across sessions and the agent library.
    fn agents_list(&self, _params: AgentsListParams) -> PortFuture<'_, AgentsListResult> {
        unsupported(METHOD_AGENTS_LIST)
    }
    /// `agents.stop` (steer): stops a running child agent of a session.
    fn agents_stop(&self, _params: AgentStopParams) -> PortFuture<'_, CommandRunResult> {
        unsupported(METHOD_AGENTS_STOP)
    }
}

/// The tool surface of the gateway as seen by an agent principal (R18 D-A).
///
/// Every call is admitted against the connection's identity: agent
/// principal, `tool_call` cap, tenant, session binding and tool grant. A
/// refusal is a typed [`PortError`] ([`PortError::ToolRefused`],
/// [`PortError::Denied`], [`PortError::Revoked`]); an implementation never
/// falls back to running the tool anywhere else.
pub trait ToolPort: Send + Sync {
    /// `tool.list`: exactly the caller's granted tools.
    fn list_tools(&self, params: ToolListParams) -> PortFuture<'_, ToolListResult>;
    /// `tool.call`: run one tool in the gateway tool host.
    fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame>;
    /// `tool.cancel`: cancel one of the caller's in-flight calls. Unknown or
    /// already finished calls answer `Ok(())` (idempotent).
    fn cancel_tool(&self, params: ToolCancelParams) -> PortFuture<'_, ()>;
}

/// Gateway inspection and administration (R18 D-B).
///
/// Reads need `gateway_read`, mutations `gateway_admin`. Results are
/// filtered by the caller's tenant exactly like `session.list`. Key
/// operations (`infra.auth.keys.*`) are deliberately not part of this port.
pub trait GatewayPort: Send + Sync {
    /// `gateway.status`.
    fn status(&self) -> PortFuture<'_, GatewayStatus>;
    /// `gateway.connections.list`.
    fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult>;
    /// `gateway.sessions.list`.
    fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>>;
    /// `gateway.listeners.list`.
    fn listeners(&self) -> PortFuture<'_, GatewayListenersResult>;
    /// `gateway.tools.list`.
    fn tools(&self) -> PortFuture<'_, GatewayToolsResult>;
    /// `gateway.connections.revoke`.
    fn revoke_connection(&self, params: GatewayRevokeParams)
    -> PortFuture<'_, GatewayRevokeResult>;
    /// `gateway.drain`.
    fn drain(&self, params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus>;
    /// `gateway.listeners.set`.
    fn set_listener(&self, params: GatewayListenerSetParams)
    -> PortFuture<'_, GatewayListenerInfo>;
    /// `gateway.tools.grant`.
    fn grant_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights>;
    /// `gateway.tools.narrow`.
    fn narrow_tools(&self, params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights>;
}

/// Why the gateway refused a `tool.call` before running it (R18 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolRefusal {
    /// The gateway tool host does not serve this tool.
    UnknownTool,
    /// The tool is outside the caller's tool grant.
    NotGranted,
    /// No gateway-side sandbox is available.
    SandboxUnavailable,
    /// The host is draining.
    Draining,
    /// The `call_id` is already in flight in this session.
    DuplicateCall,
}

impl ToolRefusal {
    /// Every refusal, for exhaustive tests and mappings.
    pub const ALL: [Self; 5] = [
        Self::UnknownTool,
        Self::NotGranted,
        Self::SandboxUnavailable,
        Self::Draining,
        Self::DuplicateCall,
    ];

    /// Stable wire code (see [`crate::session_wire::error_codes`]).
    #[must_use]
    pub const fn code(self) -> i32 {
        use crate::session_wire::error_codes;
        match self {
            Self::UnknownTool => error_codes::TOOL_UNKNOWN,
            Self::NotGranted => error_codes::TOOL_NOT_GRANTED,
            Self::SandboxUnavailable => error_codes::TOOL_SANDBOX_UNAVAILABLE,
            Self::Draining => error_codes::HOST_DRAINING,
            Self::DuplicateCall => error_codes::TOOL_CALL_DUPLICATE,
        }
    }

    /// The refusal for a wire code, if it is one.
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        use crate::session_wire::error_codes;
        match code {
            error_codes::TOOL_UNKNOWN => Some(Self::UnknownTool),
            error_codes::TOOL_NOT_GRANTED => Some(Self::NotGranted),
            error_codes::TOOL_SANDBOX_UNAVAILABLE => Some(Self::SandboxUnavailable),
            error_codes::HOST_DRAINING => Some(Self::Draining),
            error_codes::TOOL_CALL_DUPLICATE => Some(Self::DuplicateCall),
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown tool",
            Self::NotGranted => "tool not granted",
            Self::SandboxUnavailable => "gateway sandbox unavailable",
            Self::Draining => "gateway draining",
            Self::DuplicateCall => "duplicate tool call id",
        }
    }
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
    /// R18: a `tool.call` was refused before it ran.
    ToolRefused {
        refusal: ToolRefusal,
        detail: String,
    },
    /// The session is busy (lease held by TUI/job/another gateway) or the
    /// connection is overloaded. Not a transport failure. `reason` is a short
    /// display text.
    Busy(String),
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(detail) => write!(f, "session transport failed: {detail}"),
            Self::Denied(reason) => write!(f, "session call denied: {reason}"),
            Self::NotFound => f.write_str("session or approval not found"),
            Self::Protocol(detail) => write!(f, "session protocol violation: {detail}"),
            Self::Revoked => f.write_str("session access revoked"),
            Self::ToolRefused { refusal, detail } => {
                write!(f, "tool call refused ({}): {detail}", refusal.label())
            }
            Self::Busy(reason) => write!(f, "session busy: {reason}"),
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
            Self::ToolRefused { refusal, .. } => refusal.code(),
            Self::Busy(_) => error_codes::BUSY,
        }
    }

    /// Map a wire error code and message back to a port error.
    #[must_use]
    pub fn from_code(code: i32, message: String) -> Self {
        use crate::session_wire::error_codes;
        if let Some(refusal) = ToolRefusal::from_code(code) {
            return Self::ToolRefused {
                refusal,
                detail: message,
            };
        }
        match code {
            error_codes::DENIED | error_codes::HELLO_REQUIRED => Self::Denied(message),
            error_codes::NOT_FOUND => Self::NotFound,
            error_codes::REVOKED => Self::Revoked,
            error_codes::BUSY => Self::Busy(message),
            error_codes::INVALID_PARAMS
            | error_codes::METHOD_NOT_FOUND
            | error_codes::PARSE_ERROR => Self::Protocol(message),
            _ => Self::Transport(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PortError, ToolRefusal};

    #[test]
    fn port_error_codes_round_trip() {
        for error in [
            PortError::Denied("caps".into()),
            PortError::NotFound,
            PortError::Revoked,
            PortError::Protocol("bad".into()),
            PortError::Busy("Läuft gerade in der TUI".into()),
        ] {
            let back = PortError::from_code(
                error.code(),
                match &error {
                    PortError::Denied(m) | PortError::Protocol(m) | PortError::Busy(m) => m.clone(),
                    _ => String::new(),
                },
            );
            assert_eq!(back, error);
        }
    }

    #[test]
    fn tool_refusals_round_trip_through_their_codes() {
        for refusal in ToolRefusal::ALL {
            let error = PortError::ToolRefused {
                refusal,
                detail: "d".into(),
            };
            assert_eq!(PortError::from_code(error.code(), "d".into()), error);
            assert_eq!(ToolRefusal::from_code(refusal.code()), Some(refusal));
        }
        let codes: std::collections::BTreeSet<i32> = ToolRefusal::ALL
            .iter()
            .map(|refusal| refusal.code())
            .collect();
        assert_eq!(codes.len(), ToolRefusal::ALL.len());
        assert_eq!(ToolRefusal::from_code(-32002), None);
    }

    #[test]
    fn port_error_is_a_std_error() {
        fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
        assert_error::<PortError>();
        assert_eq!(PortError::Revoked.to_string(), "session access revoked");
    }

    #[test]
    fn busy_has_its_own_code_and_display() {
        let busy = PortError::Busy("running elsewhere".into());
        assert_eq!(busy.code(), crate::session_wire::error_codes::BUSY);
        assert_eq!(busy.to_string(), "session busy: running elsewhere");
        assert!(!matches!(
            PortError::from_code(busy.code(), "x".into()),
            PortError::Transport(_)
        ));
    }

    struct Bare;
    impl super::SessionPort for Bare {
        fn hello(
            &self,
            _: crate::session_wire::HelloParams,
        ) -> super::PortFuture<'_, crate::session_wire::HelloAck> {
            unreachable!()
        }
        fn list(&self) -> super::PortFuture<'_, Vec<crate::session_wire::SessionSummary>> {
            unreachable!()
        }
        fn create(
            &self,
            _: crate::session_wire::CreateParams,
        ) -> super::PortFuture<'_, crate::session_wire::SessionSummary> {
            unreachable!()
        }
        fn attach(
            &self,
            _: crate::session_wire::AttachParams,
        ) -> super::PortFuture<'_, (crate::session_wire::AttachAck, Box<dyn super::FrameSource>)>
        {
            unreachable!()
        }
        fn detach(&self, _: harw_types::SessionId) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn history(
            &self,
            _: crate::session_wire::HistoryParams,
        ) -> super::PortFuture<'_, Vec<crate::session_wire::FrameEnvelope>> {
            unreachable!()
        }
        fn submit(
            &self,
            _: crate::session_wire::SubmitParams,
        ) -> super::PortFuture<'_, crate::session_wire::SubmitResult> {
            unreachable!()
        }
        fn interrupt(&self, _: crate::session_wire::InterruptParams) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn resume(&self, _: harw_types::SessionId) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn close(&self, _: harw_types::SessionId) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn respond(
            &self,
            _: crate::session_wire::ApprovalRespondParams,
        ) -> super::PortFuture<'_, crate::session_wire::RespondResult> {
            unreachable!()
        }
        fn set_model(&self, _: crate::session_wire::SetModelParams) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn set_mode(&self, _: crate::session_wire::SetModeParams) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
        fn set_effort(&self, _: crate::session_wire::SetEffortParams) -> super::PortFuture<'_, ()> {
            unreachable!()
        }
    }

    fn ready<T>(future: super::PortFuture<'_, T>) -> Result<T, PortError> {
        use std::task::{Context, Poll, Waker};
        let mut future = future;
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("default body must be ready immediately"),
        }
    }

    #[test]
    fn minor3_default_bodies_answer_protocol_error() {
        use super::SessionPort;
        let session = harw_types::SessionId::try_from_str("s-synthetic").ok();
        if let Some(session) = session {
            let err = ready(Bare.adopt(session)).err();
            assert!(matches!(err, Some(PortError::Protocol(m)) if m.contains("session.adopt")));
        }
    }

    #[test]
    fn minor4_default_bodies_answer_protocol_error() {
        use super::SessionPort;
        let run: Result<crate::session_wire::CommandRunParams, _> =
            serde_json::from_value(serde_json::json!({"line": "/status", "client_msg_id": "m-1"}));
        if let Ok(params) = run {
            let err = ready(Bare.command_run(params)).err();
            assert!(matches!(err, Some(PortError::Protocol(m)) if m.contains("command.run")));
        } else {
            panic!("command.run params must decode");
        }
        let secret: Result<crate::session_wire::SecretPutParams, _> =
            serde_json::from_value(serde_json::json!({
                "name": "key-1",
                "for_provider": "provider-test",
                "value": "sk-test-FAKE-0000000000000000"
            }));
        if let Ok(params) = secret {
            let err = ready(Bare.secret_put(params)).err();
            assert!(
                matches!(err, Some(PortError::Protocol(m)) if m.contains("secret.put")
                && !m.contains("FAKE"))
            );
        } else {
            panic!("secret.put params must decode");
        }
    }
}

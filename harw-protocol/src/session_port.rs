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

use crate::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, FrameEnvelope,
    GatewayConnectionsResult, GatewayDrainParams, GatewayListenerInfo, GatewayListenerSetParams,
    GatewayListenersResult, GatewayRevokeParams, GatewayRevokeResult, GatewayStatus,
    GatewayToolRights, GatewayToolRightsParams, GatewayToolsResult, HelloAck, HelloParams,
    HistoryParams, InterruptParams, RespondResult, SessionSummary, SetEffortParams, SetModeParams,
    SetModelParams, SubmitParams, SubmitResult, ToolCallParams, ToolCallResultFrame,
    ToolCancelParams, ToolListParams, ToolListResult,
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
}

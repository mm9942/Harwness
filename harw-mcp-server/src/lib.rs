//! Inbound Streamable HTTP MCP authority for Harwness.
//!
//! Transport adapters are intentionally separate from the session state
//! machine below: a server must validate session/protocol ownership before a
//! job supervisor sees a tool request. Binding a listener alone does not create
//! an execution owner: submission is available only when a durable supervisor
//! is explicitly composed into the listener.

#![forbid(unsafe_code)]

/// The MCP Streamable HTTP protocol version this server implements and
/// advertises during `initialize`. This is the single source of truth for
/// the crate; `transport.rs` imports it instead of redefining the literal
/// (see Z1-R3-04 / Z1-F4).
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub mod auth;
pub mod events;
pub mod session;
pub mod supervisor;
pub mod transport;

pub use auth::{AuthenticatedPrincipal, McpAuthError, McpAuthenticator, StaticBearerAuthenticator};
pub use events::{
    McpEventBus, McpEventBusError, McpEventReceiveError, McpEventSubscription, McpLifecycleEvent,
    McpLifecycleEventKind,
};
pub use session::{McpServerError, McpServerResult, McpSession, McpSessionRegistry};
pub use supervisor::{
    DurableMcpSupervisor, McpCancellationReceipt, McpJobCapability, McpJobStatus, McpPrincipal,
    McpRequestContext, McpSupervisor, McpSupervisorError, UnavailableWorkerCancellationSink,
    WorkerCancellationSink, WorkerCancellationStatus,
};
pub use transport::{
    BoundMcpListener, DuplicatePrincipalId, McpListenerConfig, PrincipalRegistry,
};

/// The MCP operation surface that can honestly be advertised by a listener
/// composition.
///
/// A listener without a durable supervisor must not advertise submission,
/// even when an authenticated principal carries [`McpJobCapability::SubmitOwn`].
/// The capability becomes actionable only after a supervisor is composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpSurface {
    submit: bool,
}

impl McpSurface {
    /// Returns the conservative surface for a listener with no execution
    /// owner. No job execution or durable submission is available here.
    #[must_use]
    pub const fn without_supervisor() -> Self {
        Self { submit: false }
    }

    /// Returns the surface for a listener with an explicitly composed durable
    /// supervisor. The supervisor still enforces the principal capability.
    #[must_use]
    pub const fn with_supervisor() -> Self {
        Self { submit: true }
    }

    #[must_use]
    pub const fn supports_submit(self) -> bool {
        self.submit
    }
}

#[cfg(test)]
mod tests {
    use super::McpSurface;

    #[test]
    fn listener_without_supervisor_has_no_submit_surface() {
        assert!(!McpSurface::without_supervisor().supports_submit());
        assert!(McpSurface::with_supervisor().supports_submit());
    }
}

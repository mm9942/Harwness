//! `harw-session-host`: the persistent session control plane host (W00 §2.4,
//! PL-65 §2).
//!
//! The host is the **single writer** of hosted sessions. Clients (local TUI,
//! phone, laptop) attach through a transport and send intentions; the host
//! admits every call against the caller's tenant and capabilities, runs at
//! most one turn per session, and fans frames out to every attachment
//! without ever blocking the turn loop.
//!
//! Transport independent: [`SessionHost::connect`] returns a
//! [`harw_protocol::SessionPort`] bound to one authenticated
//! [`ClientIdentity`]. The WebSocket layer (`harw-session-ws`) drives that
//! port; tests drive it in process.
//!
//! Module map:
//! - [`identity`]: who is calling and what they may do (never from payloads).
//! - [`record`]: durable `HostedSessionRecord` files.
//! - [`replay`]: transcript records → durable frames.
//! - [`live_ring`]: bounded live buffer of the running turn.
//! - [`fanout`]: per-attachment bounded queues with coalescing and `Lagged`.
//! - [`arbiter`]: per-session FIFO, compare-and-swap on the head, idempotency.
//! - [`approvals`]: first-writer-wins approval resolution.
//! - [`driver`]: the port to the agent runtime that actually runs turns.
//! - [`durable_approvals`], [`durable_replay`]: durable adapters over
//!   `harw-session-store` for the approval and transcript ports (S04).
//! - [`agents`]: R18 agent principal registry (credential → principal,
//!   delegation, grant/narrow cascade, revocation).
//! - [`tool_host`]: R18 gateway tool host (tool registry, gateway sandbox,
//!   admission steps 8-9, in-flight calls, tool approvals).
//! - [`host`]: the composition of all of the above; `HostConnection` also
//!   implements [`harw_protocol::ToolPort`] and [`harw_protocol::GatewayPort`].

#![forbid(unsafe_code)]

pub mod agents;
pub mod approvals;
pub mod arbiter;
pub mod driver;
pub mod durable_approvals;
pub mod durable_replay;
pub mod error;
pub mod fanout;
pub mod host;
pub mod identity;
pub mod live_ring;
pub mod record;
pub mod replay;
pub mod tool_host;

pub use agents::{AgentCredential, AgentRegistry, Delegation, ResolvedAgent};
pub use durable_approvals::DurableApprovals;
pub use durable_replay::DurableTranscripts;
pub use error::HostError;
pub use host::{HostConfig, HostConnection, SessionHost};
pub use identity::{
    AgentPrincipal, ClientIdentity, ConnectionId, ToolGrant, ToolGrantError, caps_for_tier,
    gateway_caps_for_tier,
};
pub use tool_host::{
    GatewaySandbox, NoGatewaySandbox, SessionScope, ToolApprovalIssuer, ToolHost, ToolHostBuilder,
    ToolRegistration, WorkspaceGatewaySandbox, WorkspaceSandboxConfig,
};

//! `harw-session-remote`: the session control plane client (W00 §2.5).
//!
//! Connects to a host over a local Unix socket or an own-cloud
//! `harw-node-transport` channel, upgrades to `harw.session.v1`, performs
//! `session.hello`, multiplexes request correlation and implements
//! [`harw_protocol::SessionPort`] so a TUI or `harw attach` cannot tell the
//! transports apart (W00 D1). No provider credential or host secret lives
//! here.
//!
//! Client core (S06) is implemented: connect, upgrade, hello, request
//! correlation, frame routing and [`RemotePort`]. Reconnect and the alias
//! store (S07) are still skeleton stubs. File ownership (see
//! `W00-ws-integration-map.md`):
//! - [`conn`] (S06): endpoints, connect, hello, correlation.
//! - [`port`] (S06): the `SessionPort` implementation.
//! - [`reconnect`] (S07): backoff with jitter, resume cursors.
//! - [`alias`] (S07): machine-local host alias store.

#![forbid(unsafe_code)]

pub mod alias;
pub mod conn;
pub mod port;
pub mod reconnect;

pub use alias::{AliasStore, HostAlias};
pub use conn::{
    ConnectOptions, Endpoint, NodeEndpoint, RemoteConnection, RemoteFrames, connect_io,
    connect_node, connect_unix,
};
pub use port::RemotePort;
pub use reconnect::{BackoffPolicy, ResumeCursors};

use harw_protocol::PortError;

/// Failures of the session client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteError {
    /// Skeleton stub: the named entry point is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// Connecting or upgrading failed.
    #[error("connect: {0}")]
    Connect(String),
    /// The host violated the protocol.
    #[error("protocol: {0}")]
    Protocol(String),
    /// A local alias/state file could not be read or written.
    #[error("state: {0}")]
    State(String),
}

impl From<RemoteError> for PortError {
    fn from(error: RemoteError) -> Self {
        match error {
            RemoteError::Protocol(detail) => Self::Protocol(detail),
            other => Self::Transport(other.to_string()),
        }
    }
}

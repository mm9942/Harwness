//! `harw-session-driver`: the production [`harw_session_host::driver::TurnDriver`]
//! over `harw-core` (W00 §2.4 `driver.rs`, S03).
//!
//! The host decides when a turn runs; this crate decides how: it maps the
//! core turn loop's events onto [`harw_session_host::driver::DriverEvent`]s
//! pushed into the non-blocking sink, honours the cancel signal, parks on
//! approvals and resumes them. It never blocks the sink (W00 D8).
//!
//! Layout: [`bridge::CoreTurnDriver`] is the host-facing driver. It talks to
//! the runtime through the [`bridge::CoreRuntime`] port;
//! [`bridge::HarwCoreRuntime`] is the production implementation over
//! `harw-core` (`run_turn_durable`, `resume_after_approval`), assembled per
//! session by a [`bridge::CoreSessionFactory`] that the gateway composition
//! supplies. File ownership (see `W00-ws-integration-map.md`): [`bridge`] is
//! S03's.

#![forbid(unsafe_code)]

pub mod bridge;

pub use bridge::{
    CoreDriverConfig, CoreEvent, CoreEvents, CoreFuture, CoreRuntime, CoreSession,
    CoreSessionFactory, CoreTurnDriver, HarwCoreRuntime, ParkedApproval, SessionWiring,
};

/// Failures while building or running the bridge.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriverBridgeError {
    /// Skeleton stub: the named entry point is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// The core runtime could not be assembled (or is not composed yet).
    #[error("runtime: {0}")]
    Runtime(String),
    /// The core turn loop failed (model, tool or state-store error).
    #[error("core: {0}")]
    Core(String),
    /// The durable approval store failed.
    #[error("approval store: {0}")]
    Store(String),
    /// The session is unknown to the runtime.
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// A resume was requested but the turn is not parked on an approval, or
    /// the approval has not been resolved in the durable backend yet.
    #[error("approval: {0}")]
    Approval(String),
    /// A session setting value was refused.
    #[error("invalid setting: {0}")]
    InvalidSetting(String),
}

impl From<DriverBridgeError> for harw_session_host::HostError {
    fn from(error: DriverBridgeError) -> Self {
        match error {
            DriverBridgeError::NotImplemented(what) => Self::NotImplemented(what),
            DriverBridgeError::UnknownSession(_) => Self::NotFound,
            DriverBridgeError::InvalidSetting(detail) => Self::Protocol(detail),
            DriverBridgeError::Store(detail) => Self::Storage(detail),
            DriverBridgeError::Runtime(detail)
            | DriverBridgeError::Core(detail)
            | DriverBridgeError::Approval(detail) => Self::Driver(detail),
        }
    }
}

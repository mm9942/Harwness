//! `harw-session-driver`: the production [`harw_session_host::driver::TurnDriver`]
//! over `harw-core` (W00 §2.4 `driver.rs`, S03).
//!
//! The host decides when a turn runs; this crate decides how: it maps the
//! core turn loop's events onto [`harw_session_host::driver::DriverEvent`]s
//! pushed into the non-blocking sink, honours the cancel signal, parks on
//! approvals and resumes them. It never blocks the sink (W00 D8).
//!
//! Skeleton: the constructor answers [`DriverBridgeError::NotImplemented`].
//! File ownership (see `W00-ws-integration-map.md`): [`bridge`] is S03's.

#![forbid(unsafe_code)]

pub mod bridge;

pub use bridge::{CoreDriverConfig, CoreTurnDriver};

/// Failures while building or running the bridge.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriverBridgeError {
    /// Skeleton stub: the named entry point is not implemented yet.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// The core runtime could not be assembled.
    #[error("runtime: {0}")]
    Runtime(String),
}

impl From<DriverBridgeError> for harw_session_host::HostError {
    fn from(error: DriverBridgeError) -> Self {
        match error {
            DriverBridgeError::NotImplemented(what) => Self::NotImplemented(what),
            DriverBridgeError::Runtime(detail) => Self::Driver(detail),
        }
    }
}

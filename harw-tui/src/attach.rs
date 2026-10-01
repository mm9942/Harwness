//! Thin attach view over a [`harw_protocol::SessionPort`] (W00 W06, S09).
//!
//! The view works on any port (local UDS, node transport, in-process), so
//! it never learns which transport is behind it. Skeleton: the entry point
//! answers [`AttachError::NotImplemented`]; S09 fills it and keeps the
//! signature.

use std::sync::Arc;

use harw_protocol::SessionPort;
use harw_types::SessionId;

/// Failures of the attach view.
#[derive(Debug)]
#[non_exhaustive]
pub enum AttachError {
    /// Skeleton stub: not implemented yet.
    NotImplemented(&'static str),
    /// The session port failed.
    Port(String),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotImplemented(what) => write!(f, "not implemented: {what}"),
            Self::Port(detail) => write!(f, "session port: {detail}"),
        }
    }
}

impl std::error::Error for AttachError {}

/// Attach to `session` (or let the person pick one when `None`) and run the
/// interactive view until detach.
///
/// # Errors
/// [`AttachError::NotImplemented`] in the skeleton.
pub async fn run_attach(
    port: Arc<dyn SessionPort>,
    session: Option<SessionId>,
) -> Result<(), AttachError> {
    let _ = (port, session);
    Err(AttachError::NotImplemented("run_attach (S09)"))
}

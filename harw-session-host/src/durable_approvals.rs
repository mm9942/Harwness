//! Durable [`ApprovalBackend`] over `harw-session-store` (S04 entry point).
//!
//! Skeleton: every call answers [`HostError::NotImplemented`]. S04 replaces
//! the bodies with an adapter over `harw_session_store::ApprovalStore`
//! (first writer wins, one durable transition, host-derived actor) and keeps
//! the public signatures below.

use std::path::{Path, PathBuf};

use harw_protocol::ApprovalRequest;
use harw_types::{ApprovalActor, ApprovalId, ReviewDecision, SessionId};

use crate::approvals::{ApprovalBackend, ResolveOutcome};
use crate::error::HostError;

const WHAT: &str = "DurableApprovals (S04)";

/// [`ApprovalBackend`] persisted through the session store.
#[derive(Debug)]
pub struct DurableApprovals {
    _root: PathBuf,
}

impl DurableApprovals {
    /// Backend rooted at the session store directory `root`.
    ///
    /// # Errors
    /// [`HostError::NotImplemented`] in the skeleton; storage errors later.
    pub fn open(root: &Path) -> Result<Self, HostError> {
        let _ = root;
        Err(HostError::NotImplemented(WHAT))
    }
}

impl ApprovalBackend for DurableApprovals {
    fn pending(&self, session: &SessionId) -> Result<Vec<ApprovalRequest>, HostError> {
        let _ = session;
        Err(HostError::NotImplemented(WHAT))
    }

    fn session_of(&self, request: &ApprovalId) -> Result<Option<SessionId>, HostError> {
        let _ = request;
        Err(HostError::NotImplemented(WHAT))
    }

    fn resolve(
        &self,
        request: &ApprovalId,
        decision: ReviewDecision,
        reason: Option<String>,
        actor: &ApprovalActor,
        now: jiff::Timestamp,
    ) -> Result<ResolveOutcome, HostError> {
        let _ = (request, decision, reason, actor, now);
        Err(HostError::NotImplemented(WHAT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_typed_not_implemented() {
        let result = DurableApprovals::open(Path::new("."));
        assert!(matches!(result, Err(HostError::NotImplemented(_))));
    }
}

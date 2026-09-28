//! Approval resolution for hosted sessions (PL-65 §2.4 "Approvals").
//!
//! First writer wins: the backend performs one durable state transition and
//! every later responder gets [`ResolveOutcome::AlreadyResolved`]. The actor
//! is always the host-derived actor of the responding connection.

use std::collections::HashMap;
use std::sync::Mutex;

use harw_protocol::ApprovalRequest;
use harw_types::{ApprovalActor, ApprovalId, ReviewDecision, SessionId};

use crate::error::HostError;
use crate::identity::actor_label;

/// Result of one resolve attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveOutcome {
    Resolved,
    AlreadyResolved { by: String },
    Expired,
    NotFound,
}

/// Durable approval state as the host needs it.
pub trait ApprovalBackend: Send + Sync {
    /// Pending requests of one session (sent as frames on attach).
    fn pending(&self, session: &SessionId) -> Result<Vec<ApprovalRequest>, HostError>;

    /// Session a request belongs to, if the request exists.
    fn session_of(&self, request: &ApprovalId) -> Result<Option<SessionId>, HostError>;

    /// Resolve a request. Exactly one caller ever gets `Resolved`.
    fn resolve(
        &self,
        request: &ApprovalId,
        decision: ReviewDecision,
        reason: Option<String>,
        actor: &ApprovalActor,
        now: jiff::Timestamp,
    ) -> Result<ResolveOutcome, HostError>;
}

#[derive(Debug)]
enum Entry {
    Pending {
        session: SessionId,
        request: Box<ApprovalRequest>,
    },
    Resolved {
        session: SessionId,
        by: String,
    },
}

/// In-memory backend: used by tests and by drivers that keep approvals in
/// process. The mutex makes resolution a single atomic transition.
#[derive(Debug, Default)]
pub struct MemoryApprovals {
    entries: Mutex<HashMap<ApprovalId, Entry>>,
}

impl MemoryApprovals {
    /// Empty backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new pending request for `session`.
    pub fn issue(&self, session: &SessionId, request: ApprovalRequest) -> Result<(), HostError> {
        let mut entries = self.lock()?;
        entries.insert(
            request.id.clone(),
            Entry::Pending {
                session: session.clone(),
                request: Box::new(request),
            },
        );
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<ApprovalId, Entry>>, HostError> {
        self.entries
            .lock()
            .map_err(|_| HostError::Storage("approval table poisoned".into()))
    }
}

impl ApprovalBackend for MemoryApprovals {
    fn pending(&self, session: &SessionId) -> Result<Vec<ApprovalRequest>, HostError> {
        let entries = self.lock()?;
        let mut pending: Vec<ApprovalRequest> = entries
            .values()
            .filter_map(|entry| match entry {
                Entry::Pending {
                    session: owner,
                    request,
                } if owner == session => Some((**request).clone()),
                _ => None,
            })
            .collect();
        pending.sort_by_key(|request| request.requested_at);
        Ok(pending)
    }

    fn session_of(&self, request: &ApprovalId) -> Result<Option<SessionId>, HostError> {
        let entries = self.lock()?;
        Ok(entries.get(request).map(|entry| match entry {
            Entry::Pending { session, .. } | Entry::Resolved { session, .. } => session.clone(),
        }))
    }

    fn resolve(
        &self,
        request: &ApprovalId,
        _decision: ReviewDecision,
        _reason: Option<String>,
        actor: &ApprovalActor,
        now: jiff::Timestamp,
    ) -> Result<ResolveOutcome, HostError> {
        let mut entries = self.lock()?;
        let Some(entry) = entries.get(request) else {
            return Ok(ResolveOutcome::NotFound);
        };
        match entry {
            Entry::Resolved { by, .. } => Ok(ResolveOutcome::AlreadyResolved { by: by.clone() }),
            Entry::Pending {
                session,
                request: pending,
            } => {
                if pending.is_expired(now) {
                    return Ok(ResolveOutcome::Expired);
                }
                let session = session.clone();
                entries.insert(
                    request.clone(),
                    Entry::Resolved {
                        session,
                        by: actor_label(actor),
                    },
                );
                Ok(ResolveOutcome::Resolved)
            }
        }
    }
}

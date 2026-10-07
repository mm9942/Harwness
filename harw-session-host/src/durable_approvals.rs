//! Durable [`ApprovalBackend`] over `harw-session-store` (S04).
//!
//! One approval is one [`ApprovalStore`] record pair per session: a pending
//! record (written at [`DurableApprovals::issue`]) and, after the first
//! successful [`ApprovalBackend::resolve`], an atomically persisted resolution
//! record. The store serializes resolution with a per-session file lock, so
//! exactly one caller across threads, hosts and restarts gets
//! [`ResolveOutcome::Resolved`]; everyone after sees
//! [`ResolveOutcome::AlreadyResolved`] with the winner's label.
//!
//! Choices worth knowing:
//! - The wire [`ApprovalRequest`] is stored as a payload next to the pending
//!   record (the store record itself only has ids), so `pending` can replay
//!   it on attach after a restart.
//! - The resolver is the *host-derived actor* of the responding connection.
//!   It is recorded in the resolution; it is not required to equal the actor
//!   bound at issuance (the host authorizes responders before calling).
//! - Expiry is decided by the request's own `timeout_at` against the `now`
//!   the host passes. The store TTL is set far out so it never undercuts it.
//! - Fail closed: an undecodable pending, payload or resolution record is a
//!   [`HostError::Storage`], never `Resolved`, `NotFound` or "no approvals".
//! - The store lock is non-blocking. A resolver that loses the lock race
//!   retries briefly and then reports what the winner persisted.

use std::path::Path;
use std::time::Duration;

use harw_protocol::ApprovalRequest;
use harw_session_store::{ApprovalRecord, ApprovalStore, SessionStoreError};
use harw_types::{
    ApprovalActor, ApprovalId, Clock, ItemId, ReviewDecision, SessionId, TenantId, ToolCallId,
};
use jiff::SignedDuration;

use crate::approvals::{ApprovalBackend, ResolveOutcome};
use crate::error::HostError;
use crate::identity::actor_label;

/// Store TTL: effectively unbounded, so the request's `timeout_at` rules.
const STORE_TTL: SignedDuration = SignedDuration::from_hours(24 * 365 * 10);

/// How often a resolver retries a contended session lock (5 ms apart).
const LOCK_RETRIES: u32 = 400;
const LOCK_BACKOFF: Duration = Duration::from_millis(5);

/// [`ApprovalBackend`] persisted through the session store.
#[derive(Debug)]
pub struct DurableApprovals {
    store: ApprovalStore,
}

/// Fixed-time clock: the host passes `now` explicitly.
struct At(jiff::Timestamp);

impl Clock for At {
    fn now(&self) -> jiff::Timestamp {
        self.0
    }
}

fn item_id(request: &ApprovalId) -> Result<ItemId, HostError> {
    ItemId::try_from_str(request.as_str())
        .map_err(|error| HostError::Protocol(format!("approval id: {error}")))
}

fn storage(error: SessionStoreError) -> HostError {
    HostError::Storage(error.to_string())
}

impl DurableApprovals {
    /// Backend rooted at the session store directory `root`.
    ///
    /// No file is touched until the first call; a missing directory is an
    /// empty backend, and state written by an earlier process is visible.
    ///
    /// # Errors
    /// Never fails today; the `Result` keeps room for storage validation.
    pub fn open(root: &Path) -> Result<Self, HostError> {
        Ok(Self {
            store: ApprovalStore::with_ttl(root, STORE_TTL),
        })
    }

    /// Durably record a new pending `request` for `session`.
    ///
    /// `bound` is the actor recorded in the store record (the authority the
    /// request was issued for). Reusing a request id is refused.
    ///
    /// # Errors
    /// [`HostError::Protocol`] for an unusable id or a duplicate,
    /// [`HostError::Storage`] otherwise.
    pub fn issue(
        &self,
        session: &SessionId,
        request: &ApprovalRequest,
        bound: &ApprovalActor,
        tenant: Option<TenantId>,
    ) -> Result<(), HostError> {
        let id = item_id(&request.id)?;
        let call_id = ToolCallId::try_from_str(request.work_id.as_str())
            .map_err(|error| HostError::Protocol(format!("work id: {error}")))?;
        let record = ApprovalRecord {
            request: id,
            session: session.clone(),
            call_id,
            actor: bound.clone(),
            issued_at: request.requested_at,
            tenant,
        };
        let payload = serde_json::to_value(request)
            .map_err(|error| HostError::Storage(format!("approval payload: {error}")))?;
        self.store
            .issue_with_payload(&record, &payload)
            .map_err(|error| match error {
                SessionStoreError::ApprovalAlreadyExists { .. } => {
                    HostError::Protocol("approval request already issued".into())
                }
                other => storage(other),
            })
    }

    /// Wire request of an issued approval; a missing or undecodable payload
    /// is corruption.
    fn load_request(&self, session: &SessionId, id: &ItemId) -> Result<ApprovalRequest, HostError> {
        let payload = self
            .store
            .payload(session, id)
            .map_err(storage)?
            .ok_or_else(|| HostError::Storage(format!("approval {id} has no payload")))?;
        serde_json::from_value(payload)
            .map_err(|error| HostError::Storage(format!("approval {id} payload corrupt: {error}")))
    }

    /// `AlreadyResolved` from the persisted resolution, fail-closed.
    fn already_resolved(
        &self,
        session: &SessionId,
        id: &ItemId,
    ) -> Result<Option<ResolveOutcome>, HostError> {
        Ok(self
            .store
            .resolution(session, id)
            .map_err(storage)?
            .map(|resolution| ResolveOutcome::AlreadyResolved {
                by: actor_label(&resolution.actor),
            }))
    }
}

impl ApprovalBackend for DurableApprovals {
    fn pending(&self, session: &SessionId) -> Result<Vec<ApprovalRequest>, HostError> {
        // Ordering by request time, like `MemoryApprovals`. Expiry is judged
        // by each request's own `timeout_at` against the wall clock.
        let now = jiff::Timestamp::now();
        let records = self
            .store
            .pending_for_session(session, &At(now))
            .map_err(storage)?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let request = self.load_request(session, &record.request)?;
            if request.id.as_str() != record.request.as_str() {
                return Err(HostError::Storage(format!(
                    "approval {} payload is keyed to another id",
                    record.request
                )));
            }
            if !request.is_expired(now) {
                out.push(request);
            }
        }
        out.sort_by_key(|request| request.requested_at);
        Ok(out)
    }

    fn session_of(&self, request: &ApprovalId) -> Result<Option<SessionId>, HostError> {
        let id = item_id(request)?;
        self.store.find_session(&id).map_err(storage)
    }

    fn resolve(
        &self,
        request: &ApprovalId,
        decision: ReviewDecision,
        reason: Option<String>,
        actor: &ApprovalActor,
        now: jiff::Timestamp,
    ) -> Result<ResolveOutcome, HostError> {
        let id = item_id(request)?;
        let Some(session) = self.store.find_session(&id).map_err(storage)? else {
            return Ok(ResolveOutcome::NotFound);
        };
        if let Some(done) = self.already_resolved(&session, &id)? {
            return Ok(done);
        }
        if self.load_request(&session, &id)?.is_expired(now) {
            return Ok(ResolveOutcome::Expired);
        }
        let clock = At(now);
        for attempt in 0..=LOCK_RETRIES {
            match self.store.resolve_any_actor(
                &session,
                &id,
                decision,
                reason.clone(),
                actor,
                &clock,
            ) {
                Ok(_) => return Ok(ResolveOutcome::Resolved),
                Err(SessionStoreError::ApprovalAlreadyResolved { .. }) => {
                    return self.already_resolved(&session, &id)?.ok_or_else(|| {
                        HostError::Storage("resolution vanished after conflict".into())
                    });
                }
                Err(SessionStoreError::ApprovalExpired { .. }) => {
                    return Ok(ResolveOutcome::Expired);
                }
                Err(SessionStoreError::ApprovalNotFound { .. }) => {
                    return Ok(ResolveOutcome::NotFound);
                }
                Err(SessionStoreError::LockContended { .. }) if attempt < LOCK_RETRIES => {
                    // Another resolver holds the session lock. If it already
                    // persisted its resolution we lost the race cleanly.
                    if let Some(done) = self.already_resolved(&session, &id)? {
                        return Ok(done);
                    }
                    std::thread::sleep(LOCK_BACKOFF);
                }
                Err(other) => return Err(storage(other)),
            }
        }
        Err(HostError::Storage("approval lock stayed contended".into()))
    }
}

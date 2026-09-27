//! In-memory table of issued contexts: lookup by id, expiry, revocation.
//!
//! # Why a table and not a token
//! Across process boundaries the masterplan (§25) allows either an
//! authenticated representation or a short-lived *context reference*. This
//! hub hands out the reference ([`SecurityContextId`]) and keeps the trusted
//! [`SecurityContext`] here; another daemon resolves the reference with
//! `GET /v1/contexts/{id}`. The reference alone grants nothing — it is only
//! as good as the hub's answer at the time of the lookup.
//!
//! # Lifetime
//! - Active until `expires_at`, then reported as [`Lookup::Expired`] until the
//!   next purge removes it.
//! - A revoked context stays in the table (as a tombstone) until its original
//!   expiry, so a verifier sees [`Lookup::Revoked`] rather than
//!   [`Lookup::Unknown`]; after expiry it is purged like any other entry.
//! - The table is bounded ([`ContextTable::with_capacity_limit`]); inserting
//!   into a full table first purges expired entries, then refuses.
//!
//! Not persisted: a hub restart invalidates every outstanding reference,
//! which is the safe direction.

use std::collections::HashMap;

use jiff::Timestamp;

use harw_types::{SecurityContext, SecurityContextId};

/// Default upper bound on simultaneously held contexts.
pub const DEFAULT_MAX_CONTEXTS: usize = 10_000;

#[derive(Debug)]
struct Entry {
    context: SecurityContext,
    owner_uid: u32,
    revoked: bool,
}

/// Result of resolving a context reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// Valid at the lookup time.
    Active(&'a SecurityContext),
    /// Known but past its expiry.
    Expired,
    /// Revoked before its expiry.
    Revoked,
    /// Never issued by this process, or already purged.
    Unknown,
}

/// Result of a revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// Was active, is now revoked.
    Revoked,
    /// Was already revoked.
    AlreadyRevoked,
    /// Already expired; nothing to revoke.
    Expired,
    /// Not in the table.
    Unknown,
}

/// The table refused an insert because it is full of unexpired contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableFull;

impl std::fmt::Display for TableFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("context table is full")
    }
}

impl std::error::Error for TableFull {}

/// Bounded map of issued contexts.
#[derive(Debug)]
pub struct ContextTable {
    entries: HashMap<SecurityContextId, Entry>,
    max_entries: usize,
}

impl Default for ContextTable {
    fn default() -> Self {
        Self::with_capacity_limit(DEFAULT_MAX_CONTEXTS)
    }
}

impl ContextTable {
    /// A table holding at most `max_entries` contexts (at least one).
    #[must_use]
    pub fn with_capacity_limit(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries: max_entries.max(1),
        }
    }

    /// Stores `context` on behalf of the peer `owner_uid`.
    ///
    /// # Errors
    /// [`TableFull`] if the table is still full after purging expired
    /// entries at `now`.
    pub fn insert(
        &mut self,
        context: SecurityContext,
        owner_uid: u32,
        now: Timestamp,
    ) -> Result<(), TableFull> {
        if self.entries.len() >= self.max_entries {
            self.purge_expired(now);
            if self.entries.len() >= self.max_entries {
                return Err(TableFull);
            }
        }
        self.entries.insert(
            context.id().clone(),
            Entry {
                context,
                owner_uid,
                revoked: false,
            },
        );
        Ok(())
    }

    /// Resolves `id` at `now`.
    #[must_use]
    pub fn lookup(&self, id: &SecurityContextId, now: Timestamp) -> Lookup<'_> {
        match self.entries.get(id) {
            None => Lookup::Unknown,
            Some(entry) if entry.revoked => Lookup::Revoked,
            Some(entry) if entry.context.is_expired(now) => Lookup::Expired,
            Some(entry) => Lookup::Active(&entry.context),
        }
    }

    /// The uid that requested `id`, if the entry is still held.
    #[must_use]
    pub fn owner_of(&self, id: &SecurityContextId) -> Option<u32> {
        self.entries.get(id).map(|entry| entry.owner_uid)
    }

    /// Revokes `id` at `now`.
    pub fn revoke(&mut self, id: &SecurityContextId, now: Timestamp) -> RevokeOutcome {
        match self.entries.get_mut(id) {
            None => RevokeOutcome::Unknown,
            Some(entry) if entry.revoked => RevokeOutcome::AlreadyRevoked,
            Some(entry) if entry.context.is_expired(now) => RevokeOutcome::Expired,
            Some(entry) => {
                entry.revoked = true;
                RevokeOutcome::Revoked
            }
        }
    }

    /// Removes every entry (active or revoked) whose expiry lies at or
    /// before `now`. Returns how many were removed.
    pub fn purge_expired(&mut self, now: Timestamp) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| !entry.context.is_expired(now));
        before - self.entries.len()
    }

    /// Number of held entries (including expired ones not yet purged and
    /// revoked tombstones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no entries are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextTable, Lookup, RevokeOutcome, TableFull};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{
        AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind, SecurityClaims,
        SecurityContext, SecurityContextId, SecurityContextIssuer, TrustZone,
    };
    use jiff::{SignedDuration, Timestamp};

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    fn issue(now: Timestamp, ttl_secs: i64) -> TestResult<SecurityContext> {
        let issuer = SecurityContextIssuer::new_for_hub("security-hub").map_err(ctx("issuer"))?;
        let principal = Principal::trusted_ingress(
            PrincipalKind::Human,
            "alice",
            IngressSurface::Cli,
            PermissionTier::Owner,
        );
        let claims = SecurityClaims::new(principal, TrustZone::Local, AuthStrength::PeerCredential);
        SecurityContext::issue(&issuer, claims, now, SignedDuration::from_secs(ttl_secs))
            .map_err(ctx("issue"))
    }

    fn assert_lookup(actual: Lookup<'_>, expected: Lookup<'_>) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(TestError::Unexpected(format!(
                "lookup {actual:?}, expected {expected:?}"
            )))
        }
    }

    #[test]
    fn test_active_until_expiry_then_expired_then_purged() -> TestResult {
        let t0 = at(1_700_000_000)?;
        let context = issue(t0, 60)?;
        let id = context.id().clone();
        let mut table = ContextTable::default();
        table.insert(context, 1000, t0).map_err(ctx("insert"))?;

        assert!(matches!(table.lookup(&id, t0), Lookup::Active(_)));
        assert!(matches!(
            table.lookup(&id, t0 + SignedDuration::from_secs(59)),
            Lookup::Active(_)
        ));
        assert_lookup(
            table.lookup(&id, t0 + SignedDuration::from_secs(60)),
            Lookup::Expired,
        )?;
        assert_eq!(table.owner_of(&id), Some(1000));
        assert_eq!(table.purge_expired(t0 + SignedDuration::from_secs(60)), 1);
        assert_lookup(table.lookup(&id, t0), Lookup::Unknown)?;
        assert!(table.is_empty());
        Ok(())
    }

    #[test]
    fn test_revocation_is_sticky_until_purge() -> TestResult {
        let t0 = at(1_700_000_000)?;
        let context = issue(t0, 60)?;
        let id = context.id().clone();
        let mut table = ContextTable::default();
        table.insert(context, 1000, t0).map_err(ctx("insert"))?;

        assert_eq!(table.revoke(&id, t0), RevokeOutcome::Revoked);
        assert_eq!(table.revoke(&id, t0), RevokeOutcome::AlreadyRevoked);
        assert_lookup(table.lookup(&id, t0), Lookup::Revoked)?;
        // Tombstone survives a purge before expiry …
        assert_eq!(table.purge_expired(t0 + SignedDuration::from_secs(30)), 0);
        assert_lookup(table.lookup(&id, t0), Lookup::Revoked)?;
        // … and is removed at expiry.
        assert_eq!(table.purge_expired(t0 + SignedDuration::from_secs(60)), 1);
        assert_lookup(table.lookup(&id, t0), Lookup::Unknown)?;

        let unknown = SecurityContextId::try_from_str("nope").map_err(ctx("id"))?;
        assert_eq!(table.revoke(&unknown, t0), RevokeOutcome::Unknown);
        Ok(())
    }

    #[test]
    fn test_revoking_expired_context_reports_expired() -> TestResult {
        let t0 = at(1_700_000_000)?;
        let context = issue(t0, 1)?;
        let id = context.id().clone();
        let mut table = ContextTable::default();
        table.insert(context, 1000, t0).map_err(ctx("insert"))?;
        assert_eq!(
            table.revoke(&id, t0 + SignedDuration::from_secs(5)),
            RevokeOutcome::Expired
        );
        Ok(())
    }

    #[test]
    fn test_full_table_purges_expired_then_refuses() -> TestResult {
        let t0 = at(1_700_000_000)?;
        let mut table = ContextTable::with_capacity_limit(2);
        table.insert(issue(t0, 10)?, 1, t0).map_err(ctx("first"))?;
        table
            .insert(issue(t0, 100)?, 1, t0)
            .map_err(ctx("second"))?;
        assert_eq!(table.insert(issue(t0, 100)?, 1, t0), Err(TableFull));

        // After the first one expires, the insert purges and succeeds.
        let later = t0 + SignedDuration::from_secs(10);
        table
            .insert(issue(later, 100)?, 1, later)
            .map_err(ctx("insert after purge"))?;
        assert_eq!(table.len(), 2);
        Ok(())
    }
}

//! The authoritative browser tool collection and its session-ownership registry.
//!
//! # Responsibility
//! [`BrowserToolSet`] holds the browser host, the operator-supplied open
//! policy (origins, profile, limits — never model input) and a registry that
//! binds every browser session to exactly one owner (the harness `SessionId`
//! as string) together with its authorized `OpenBrowserRequest` and
//! `ActionBudget`. Dispatch lives in `dispatch.rs`.
//!
//! # Concurrency
//! `Send + Sync`. The registry is a `std::sync::Mutex` whose guard is only
//! held inside synchronous helpers (never across `await`). Each critical
//! section is one map operation, so a poisoned lock cannot leave a half-updated
//! entry and is recovered instead of disabling cleanup.
//!
//! # Errors
//! `Error::SessionNotFound` for unknown **and** foreign sessions (existence of
//! another owner's session is not revealed); `Error::InvalidArgument` for
//! exhausted budgets and limit violations; `Error::OriginNotAllowed` for
//! navigation targets outside the grant.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use harw_browser::action::ActionBudget;
use harw_browser::error::{Error, Result};
use harw_browser::host::BrowserHost;
use harw_browser::ids::BrowserSessionId;
use harw_browser::policy::{BrowserLimits, OpenBrowserRequest};

use crate::prepare::{
    browser_tool_descriptors, prepare_browser_call_with_policy, validate_request,
};
use crate::types::{
    BrowserOpenPolicy, BrowserToolDescriptor, BrowserToolRequest, PreparedBrowserCall,
    PreparedBrowserRequest,
};

// One registered browser session: owner, frozen authority, running budget.
struct OwnedSession {
    owner: String,
    authority: Arc<OpenBrowserRequest>,
    budget: ActionBudget,
}

/// Compact model-facing browser tool collection backed by an authoritative host.
///
/// # Description
/// Preparation remains pure and host-free. The host, the operator's
/// [`BrowserOpenPolicy`] and the ownership registry are only reachable inside
/// this crate, so model arguments can never select origins, profiles, limits
/// or another owner's session.
///
/// # Concurrency
/// `Send + Sync`; share via `Arc<BrowserToolSet>`.
///
/// # Examples
/// ```rust,no_run
/// use std::sync::Arc;
/// use harw_browser::host::BrowserHost;
/// use harw_browser::policy::{BrowserLimits, OriginPolicy};
/// use harw_tool_browser::{BrowserOpenGrant, BrowserOpenPolicy, BrowserToolSet};
///
/// # fn build(host: Arc<dyn BrowserHost>) -> harw_browser::Result<BrowserToolSet> {
/// let grant = BrowserOpenGrant::ephemeral(
///     OriginPolicy::from_origins(["https://erp.example.com"], true)?,
///     OriginPolicy::default(),
/// )
/// .with_limits(BrowserLimits::default().with_max_actions_per_session(50));
/// Ok(BrowserToolSet::with_open_policy(host, BrowserOpenPolicy::grant(grant)))
/// # }
/// ```
pub struct BrowserToolSet {
    host: Arc<dyn BrowserHost>,
    open_policy: BrowserOpenPolicy,
    sessions: Mutex<HashMap<BrowserSessionId, OwnedSession>>,
}

impl BrowserToolSet {
    /// Creates a browser tool set with no `browser.open` authority.
    ///
    /// # Description
    /// Fail-closed: without a grant every open is rejected, so no session can
    /// ever be registered and all session operations fail with
    /// `SessionNotFound`. Use [`Self::with_open_policy`] for an operator grant.
    pub fn new(host: Arc<dyn BrowserHost>) -> Self {
        Self::with_open_policy(host, BrowserOpenPolicy::default())
    }

    /// Creates a browser tool set with an operator-supplied open policy.
    ///
    /// # Arguments
    /// - `host` (`Arc<dyn BrowserHost>`): authoritative browser host.
    /// - `open_policy` (`BrowserOpenPolicy`): origins, profile and limits from
    ///   configuration; the only source of browser authority.
    pub fn with_open_policy(host: Arc<dyn BrowserHost>, open_policy: BrowserOpenPolicy) -> Self {
        Self {
            host,
            open_policy,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the advertised seven-tool surface.
    pub fn descriptors(&self) -> &'static [BrowserToolDescriptor] {
        browser_tool_descriptors()
    }

    /// Returns the operator open policy.
    pub fn open_policy(&self) -> &BrowserOpenPolicy {
        &self.open_policy
    }

    /// Returns the limits applied at preparation: the grant's, or defaults.
    pub fn limits(&self) -> BrowserLimits {
        self.open_policy
            .configured_grant()
            .map_or_else(BrowserLimits::default, |grant| *grant.limits())
    }

    /// Authorizes/validates a model request and freezes it with its scope.
    ///
    /// # Errors
    /// - `Error::OriginNotAllowed`: open without grant or start outside it.
    /// - `Error::InvalidArgument`: any limit or revision rule violated.
    pub fn prepare(&self, request: BrowserToolRequest) -> Result<PreparedBrowserCall> {
        prepare_browser_call_with_policy(request, Some(&self.open_policy))
    }

    /// Lists the browser sessions currently owned by `owner` (unordered).
    ///
    /// # Arguments
    /// - `owner` (`&str`): harness session id (`SessionId::as_str`).
    pub fn owned_sessions(&self, owner: &str) -> Vec<BrowserSessionId> {
        self.registry()
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Synchronously revokes every browser session of `owner` from the registry.
    ///
    /// # Description
    /// After this call no tool operation can reach those sessions any more,
    /// but the browsers are **not** closed; use
    /// [`Self::close_sessions_owned_by`] (or close the returned ids on the
    /// host) to release them. Intended for synchronous lifecycle hooks.
    ///
    /// # Returns
    /// The revoked session ids (unordered).
    pub fn revoke_sessions_owned_by(&self, owner: &str) -> Vec<BrowserSessionId> {
        let mut sessions = self.registry();
        let revoked: Vec<BrowserSessionId> = sessions
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in &revoked {
            sessions.remove(id);
        }
        revoked
    }

    /// Revokes and closes every browser session owned by `owner`.
    ///
    /// # Description
    /// Session-end cleanup (contract of `harw_runtime::SessionLifecycleHook`,
    /// see ledger W5/B-TOOL). All sessions are revoked first, then closed one by
    /// one; a failing close does not stop the remaining closes.
    ///
    /// # Returns
    /// Number of sessions closed successfully.
    ///
    /// # Errors
    /// The first host close error, after all closes were attempted.
    ///
    /// # Concurrency
    /// No lock is held while awaiting the host.
    pub async fn close_sessions_owned_by(&self, owner: &str) -> Result<usize> {
        let revoked = self.revoke_sessions_owned_by(owner);
        let mut closed = 0_usize;
        let mut first_error = None;
        for session_id in revoked {
            match self.host.close(&session_id).await {
                Ok(()) => closed += 1,
                Err(error) => {
                    tracing::error!(%session_id, %error, "closing browser session at session end failed");
                    first_error.get_or_insert(error);
                }
            }
        }
        tracing::info!(closed, "browser sessions closed for ended harness session");
        match first_error {
            Some(error) => Err(error),
            None => Ok(closed),
        }
    }

    pub(crate) fn host(&self) -> &dyn BrowserHost {
        self.host.as_ref()
    }

    // Registers a freshly opened session for `owner`; never overwrites.
    pub(crate) fn register_session(
        &self,
        owner: &str,
        session_id: BrowserSessionId,
        authority: Arc<OpenBrowserRequest>,
    ) -> Result<()> {
        let mut sessions = self.registry();
        if sessions.contains_key(&session_id) {
            return Err(Error::InvalidArgument {
                detail: format!("browser session '{session_id}' is already registered"),
            });
        }
        let budget = ActionBudget::new(&authority.limits);
        sessions.insert(
            session_id,
            OwnedSession {
                owner: owner.to_owned(),
                authority,
                budget,
            },
        );
        Ok(())
    }

    // Admits one session-bound request: ownership, re-validation with the
    // session's limits, navigation target check and budget (act only), all
    // under one lock so concurrent calls cannot overspend the budget.
    pub(crate) fn admit(
        &self,
        owner: &str,
        session_id: BrowserSessionId,
        request: &PreparedBrowserRequest,
    ) -> Result<Arc<OpenBrowserRequest>> {
        let mut sessions = self.registry();
        let Some(entry) = sessions
            .get_mut(&session_id)
            .filter(|entry| entry.owner == owner)
        else {
            return Err(Error::SessionNotFound { session_id });
        };
        validate_request(request, &entry.authority.limits)?;
        if let PreparedBrowserRequest::Act(act) = request {
            if let Some(target) = act.request.action.navigation_target() {
                entry.authority.check_navigation_target(target)?;
            }
            entry.budget.try_consume()?;
        }
        Ok(Arc::clone(&entry.authority))
    }

    // Whether any owner holds `session_id`.
    pub(crate) fn is_registered(&self, session_id: BrowserSessionId) -> bool {
        self.registry().contains_key(&session_id)
    }

    // Removes `session_id` if `owner` owns it.
    pub(crate) fn unregister_owned(&self, owner: &str, session_id: BrowserSessionId) -> Result<()> {
        let mut sessions = self.registry();
        match sessions.get(&session_id) {
            Some(entry) if entry.owner == owner => {
                sessions.remove(&session_id);
                Ok(())
            }
            _ => Err(Error::SessionNotFound { session_id }),
        }
    }

    // Removes `session_id` regardless of owner (policy breach).
    pub(crate) fn unregister(&self, session_id: BrowserSessionId) {
        self.registry().remove(&session_id);
    }

    // Poison recovery is sound: every critical section is a single map
    // operation, so no invariant can be left half-updated.
    fn registry(&self) -> MutexGuard<'_, HashMap<BrowserSessionId, OwnedSession>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

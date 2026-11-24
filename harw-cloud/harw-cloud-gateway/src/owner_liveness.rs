//! In-process owner observation registry for a selected Root-Hub directory.
//!
//! No network handler is mounted here. Only a future transport-owned handler
//! may pass the peer obtained from the node transport's authenticated request
//! extension. A constructed `AuthenticatedPeer`, address, header, or client
//! payload is not authentication. The cache records liveness only; it cannot
//! create an owner or grant permissions.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harw_node_transport::AuthenticatedPeer;

use crate::owner_directory::OwnerDirectory;
use crate::route_admission::{
    AttachKind, CallerGrant, OwnerFence, OwnerObservation, RouteAdmission, RouteRefusal,
    admit_route, authorized_grant,
};

const MAX_OWNER_OBSERVATIONS: usize = 1024;
pub(crate) const FRESH_FOR: Duration = Duration::from_secs(30);

/// Trusted current lifecycle lookup, independent of the node making reports.
/// The composition must return an epoch only when the exact node is enrolled
/// and Active; missing, Pending, Draining, Drained, Revoked, or unavailable
/// state returns `None`. A new activation must issue a new, increasing epoch
/// so an Undrain cannot revive observations from a previous activation.
pub trait ActiveOwnerLifecycle: Send + Sync {
    fn active_epoch(&self, node_id: &str) -> Option<NonZeroU64>;
}

impl<F> ActiveOwnerLifecycle for F
where
    F: Fn(&str) -> Option<NonZeroU64> + Send + Sync,
{
    fn active_epoch(&self, node_id: &str) -> Option<NonZeroU64> {
        self(node_id)
    }
}

/// A refusal from the trusted owner observation interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerReportError {
    WrongPeer,
    Inactive,
    DirectoryUnavailable,
    FenceMismatch,
    Capacity,
    Unavailable,
    Expired,
}

impl fmt::Display for OwnerReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WrongPeer => "report does not belong to the authenticated owner",
            Self::Inactive => "reporting node is not active",
            Self::DirectoryUnavailable => "owner generation authority is unavailable",
            Self::FenceMismatch => "report does not match the current owner fence",
            Self::Capacity => "owner observation registry reached its bound",
            Self::Unavailable => "owner observation registry is unavailable",
            Self::Expired => "owner observation expired before admission",
        })
    }
}

impl std::error::Error for OwnerReportError {}

#[derive(Debug)]
struct Observation {
    fence: OwnerFence,
    active_epoch: NonZeroU64,
    heard_at: Instant,
}

impl Observation {
    fn is_fresh(&self, now: Instant) -> bool {
        now.checked_duration_since(self.heard_at)
            .is_some_and(|age| age < FRESH_FOR)
    }
}

/// One activation read from the independent lifecycle authority before the
/// body is consumed. Not constructible by the request or reusable as a
/// routing/attach grant.
pub(crate) struct OwnerReportActivation {
    node_id: String,
    epoch: NonZeroU64,
    authority: Arc<()>,
    heard_at: Instant,
    deadline: Instant,
}

impl OwnerReportActivation {
    fn check_deadline(&self) -> Result<(), OwnerReportError> {
        if Instant::now() >= self.deadline {
            Err(OwnerReportError::Expired)
        } else {
            Ok(())
        }
    }
}

type Scope = (String, String);

/// Directory and observation cache selected together at service construction.
/// Cache loss requires fresh authenticated reports after restart.
pub struct HubOwnerRoutes<L> {
    directory: OwnerDirectory,
    lifecycle: L,
    live: Mutex<BTreeMap<Scope, Observation>>,
    authority: Arc<()>,
}

/// Short-lived in-process routing decision. It is not serializable and grants
/// no authority to a remote node. A future relay must still authenticate and
/// authorize the originating device independently at the destination.
#[derive(Debug)]
pub struct LiveRouteAdmission {
    route: RouteAdmission,
    authority: Arc<()>,
    active_epoch: NonZeroU64,
}

impl LiveRouteAdmission {
    pub fn destination_node(&self) -> &str {
        self.route.destination_node()
    }

    pub fn session_id(&self) -> &str {
        self.route.session_id()
    }

    pub(crate) fn destination_instance(&self) -> &str {
        self.route.destination_instance()
    }

    #[cfg(any(feature = "hub-owner-local-host", feature = "hub-owner-process-proof"))]
    pub(crate) fn observation_context(&self) -> Option<(&OwnerFence, &CallerGrant)> {
        self.route.observation_context()
    }

    #[cfg(feature = "hub-owner-process-proof")]
    pub(crate) fn active_epoch(&self) -> NonZeroU64 {
        self.active_epoch
    }
}

impl<L: ActiveOwnerLifecycle> HubOwnerRoutes<L> {
    pub fn new(directory: OwnerDirectory, lifecycle: L) -> Self {
        Self {
            directory,
            lifecycle,
            live: Mutex::new(BTreeMap::new()),
            authority: Arc::new(()),
        }
    }

    /// Capture the active epoch using only the peer derived at the trusted
    /// transport boundary. This must run before consuming an owner report's
    /// body, never from a routed attach or user-supplied node ID.
    pub(crate) fn bind_owner_report_activation(
        &self,
        peer: &AuthenticatedPeer,
        heard_at: Instant,
        deadline: Instant,
    ) -> Result<OwnerReportActivation, OwnerReportError> {
        if Instant::now() >= deadline {
            return Err(OwnerReportError::Expired);
        }
        let node_id = peer.node_id.as_str();
        let epoch = self.lifecycle.active_epoch(node_id).ok_or(OwnerReportError::Inactive)?;
        let activation = OwnerReportActivation {
            node_id: node_id.to_owned(),
            epoch,
            authority: Arc::clone(&self.authority),
            heard_at,
            deadline,
        };
        activation.check_deadline()?;
        Ok(activation)
    }

    /// Record the report against the exact pre-body activation. Sampling a
    /// newer active epoch here would let a report that crossed Drain/Undrain
    /// refresh the wrong incarnation. The peer still has to report the
    /// exact durable owner fence.
    pub(crate) fn record_from_owner_transport(
        &self,
        activation: OwnerReportActivation,
        reported: &OwnerFence,
        current_transport: impl FnOnce() -> bool,
    ) -> Result<(), OwnerReportError> {
        activation.check_deadline()?;
        if !Arc::ptr_eq(&activation.authority, &self.authority)
            || activation.node_id != reported.node_id
        {
            return Err(OwnerReportError::WrongPeer);
        }
        let active_epoch = activation.epoch;
        if self.lifecycle.active_epoch(&activation.node_id) != Some(active_epoch) {
            return Err(OwnerReportError::Inactive);
        }
        activation.check_deadline()?;
        let current = self
            .directory
            .current(&reported.tenant, &reported.session_id)
            .map_err(|_| OwnerReportError::DirectoryUnavailable)?
            .ok_or(OwnerReportError::FenceMismatch)?;
        if &current != reported {
            return Err(OwnerReportError::FenceMismatch);
        }
        // Re-read after validating the durable directory. The admission
        // path independently rejects any cached epoch no longer current.
        if self.lifecycle.active_epoch(&activation.node_id) != Some(active_epoch) {
            return Err(OwnerReportError::Inactive);
        }
        let scope = (current.tenant.clone(), current.session_id.clone());
        let mut live = self.live.lock().map_err(|_| OwnerReportError::Unavailable)?;
        activation.check_deadline()?;
        // The fixed network listener's sealed accepted-channel pin and
        // lifetime must still hold at cache admission, not just before body
        // parsing or before the potentially blocking directory reads.
        if !current_transport() {
            return Err(OwnerReportError::WrongPeer);
        }
        let now = Instant::now();
        let heard_at = activation.heard_at;
        let observation = Observation {
            fence: current,
            active_epoch,
            heard_at,
        };
        if !observation.is_fresh(now) {
            return Err(OwnerReportError::Expired);
        }
        live.retain(|_, old| old.is_fresh(now));
        if let Some(old) = live.get(&scope) {
            if old.fence.generation > observation.fence.generation || old.active_epoch > active_epoch {
                // An in-flight report may have read the former fence while
                // a newer owner/activation was already reported.
                return Err(OwnerReportError::FenceMismatch);
            }
            if old.fence.generation == observation.fence.generation
                && old.active_epoch == active_epoch
                && old.heard_at > heard_at
            {
                return Ok(());
            }
        } else if live.len() >= MAX_OWNER_OBSERVATIONS {
            return Err(OwnerReportError::Capacity);
        }
        live.insert(scope, observation);
        Ok(())
    }

    /// Resolve one session for a fresh, already authorized caller grant.
    /// This entry point does not accept an observation or target from the
    /// caller. A cache hit alone never selects the destination.
    pub(crate) fn admit(
        &self,
        session_id: &str,
        kind: AttachKind,
        grant: Option<&CallerGrant>,
    ) -> Result<LiveRouteAdmission, RouteRefusal> {
        let grant = authorized_grant(grant, session_id, kind)?;
        let fence = self.current_fence(&grant.tenant, session_id)?;
        let active_epoch = self.live_epoch(&fence)?;
        let observation = OwnerObservation {
            reported_fence: fence.clone(),
            eligible: true,
        };
        let route = admit_route(session_id, kind, Some(grant), Some(&fence), Some(&observation))?;
        Ok(LiveRouteAdmission {
            route,
            authority: Arc::clone(&self.authority),
            active_epoch,
        })
    }

    /// Recheck a decision before attachment using fresh authenticated caller
    /// state and destination coordinates, and the same selected authority.
    /// This is not a remote attachment protocol.
    pub fn recheck_before_attach(
        &self,
        decision: &LiveRouteAdmission,
        destination_node: &str,
        destination_instance: &str,
        grant: Option<&CallerGrant>,
    ) -> Result<(), RouteRefusal> {
        if !Arc::ptr_eq(&decision.authority, &self.authority) {
            return Err(RouteRefusal::Forbidden);
        }
        let grant = decision.route.validate_caller(grant)?;
        let fence = self.current_fence(&grant.tenant, decision.session_id())?;
        decision.route.revalidate_at_owner(
            destination_node,
            destination_instance,
            Some(grant),
            Some(&fence),
        )?;
        if self.live_epoch(&fence)? != decision.active_epoch {
            return Err(RouteRefusal::OwnerUnavailable);
        }
        let latest = self.current_fence(&grant.tenant, decision.session_id())?;
        if latest != fence {
            return Err(RouteRefusal::FenceMismatch);
        }
        Ok(())
    }

    #[cfg(feature = "hub-owner-placement-listener")]
    pub(crate) fn confirm_presign_fence(
        &self, expected: &OwnerFence, expected_activation: NonZeroU64,
    ) -> Result<(), RouteRefusal> {
        let current = self.current_fence(&expected.tenant, &expected.session_id)?;
        if &current != expected || self.live_epoch(&current)? != expected_activation
            || &self.current_fence(&expected.tenant, &expected.session_id)? != expected
        {
            return Err(RouteRefusal::FenceMismatch);
        }
        Ok(())
    }

    fn current_fence(&self, tenant: &str, session: &str) -> Result<OwnerFence, RouteRefusal> {
        self.directory
            .current(tenant, session)
            .map_err(|_| RouteRefusal::OwnerUnavailable)?
            .ok_or(RouteRefusal::OwnerUnavailable)
    }

    fn live_epoch(&self, fence: &OwnerFence) -> Result<NonZeroU64, RouteRefusal> {
        let active_epoch = self
            .lifecycle
            .active_epoch(&fence.node_id)
            .ok_or(RouteRefusal::OwnerUnavailable)?;
        let scope = (fence.tenant.clone(), fence.session_id.clone());
        let heard_at = {
            let live = self.live.lock().map_err(|_| RouteRefusal::OwnerUnavailable)?;
            let observation = live.get(&scope).ok_or(RouteRefusal::OwnerUnavailable)?;
            if &observation.fence != fence {
                return Err(RouteRefusal::FenceMismatch);
            }
            if observation.active_epoch != active_epoch {
                return Err(RouteRefusal::OwnerUnavailable);
            }
            observation.heard_at
        };
        if self.lifecycle.active_epoch(&fence.node_id) != Some(active_epoch)
            || !Instant::now()
                .checked_duration_since(heard_at)
                .is_some_and(|age| age < FRESH_FOR)
        {
            return Err(RouteRefusal::OwnerUnavailable);
        }
        Ok(active_epoch)
    }
}

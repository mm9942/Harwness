//! Narrow destination-side authorization for a future Root-Hub observation.
//!
//! The caller must already have a negotiated, end-to-end authenticated
//! HostConnection on this owner. A Hub decision, a node report, and fields
//! serialized by the Hub cannot construct or substitute for that connection.
//! This check does not open a session stream or grant a transport capability.

use std::num::NonZeroU64;
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use harw_protocol::{
    HostedState,
    owner_placement::{
        FencedDirectOwnerStatus, OwnerObservationReservationId, OwnerPlacementLookupId,
        OwnerPlacementLookupPair, OwnerPlacementLookupPhase,
    },
    session_port::PortFuture,
};
use harw_types::{AuthStrength, DeviceId, IngressSurface, NodeId, PrincipalKind, SessionId, TenantId, TrustZone};
use tokio::sync::{Semaphore, watch};

use super::{HostConnection, HostError, HostInner, Need, SessionHost, admit};
use crate::identity::ClientIdentity;
use crate::record::HostedSessionRecord;

pub(super) mod reservation;
pub use reservation::ClaimedOwnerObservation;

const MAX_ACTIVE_PREFLIGHTS: usize = 16;
static ADMISSION: Semaphore = Semaphore::const_new(MAX_ACTIVE_PREFLIGHTS);
const MAX_SELECTED_OBSERVATIONS: usize = 8;
const SELECTED_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(3);
static SELECTED_ADMISSION: Semaphore = Semaphore::const_new(MAX_SELECTED_OBSERVATIONS);

// Deliberately identical for foreign, ineligible, missing, and unavailable
// sessions. This seam must not become a remote session enumeration oracle.
fn unavailable() -> HostError {
    HostError::Denied("owner observation unavailable".into())
}

fn record_allows(
    record: &HostedSessionRecord,
    identity: &ClientIdentity,
    session: &SessionId,
    tenant: &TenantId,
    expected_epoch: NonZeroU64,
) -> bool {
    &record.session_id == session
        && record.tenant.as_ref() == Some(tenant)
        && record.host_epoch == expected_epoch.get()
        && !record.state.is_terminal()
        && record.revalidate_creator_execution(identity, identity.principal.tier()).is_ok()
}

pub use harw_protocol::session_wire::OwnerObservationStatus;

fn check_existing_record(
    host: &HostInner,
    identity: &ClientIdentity,
    session: &SessionId,
    tenant: &TenantId,
    expected_epoch: NonZeroU64,
) -> Result<OwnerObservationStatus, HostError> {
    if host.is_draining() || host.is_revoked(identity) || host.epoch != expected_epoch.get() {
        return Err(unavailable());
    }
    // Read the selected host's record directly. Profile-only/adoptable rows,
    // unpublished archive imports, and absent durable records cannot route.
    // Unlike slot()/profile_slot(), this does not hydrate or allocate a slot
    // and does not open the transcript.
    let durable = host.records.load_for_owner_observation(session)
        .map_err(|_| unavailable())?.ok_or_else(unavailable)?;
    if !record_allows(&durable, identity, session, tenant, expected_epoch) {
        return Err(unavailable());
    }

    // When already loaded, also reject divergence affecting authority; a
    // failed durable close or an unpublished generation must not pass based
    // on the on-disk row alone. Release the slot-table lock before locking
    // the slot itself.
    let loaded = { host.slots.lock().map_err(|_| unavailable())?.get(session).cloned() };
    if let Some(slot) = loaded {
        let state = slot.lock().map_err(|_| unavailable())?;
        if state.ephemeral || state.record.generation != durable.generation
            || !record_allows(&state.record, identity, session, tenant, expected_epoch)
        {
            return Err(unavailable());
        }
    }
    Ok(match durable.state {
        HostedState::Idle => OwnerObservationStatus::Idle,
        HostedState::Running | HostedState::WaitingForApproval
        | HostedState::WaitingForChild | HostedState::Queued { .. } => OwnerObservationStatus::Active,
        HostedState::Interrupted => OwnerObservationStatus::Interrupted,
        HostedState::Failed => OwnerObservationStatus::Failed,
        HostedState::Closed => return Err(unavailable()),
    })
}

impl HostConnection {
    /// Check an observation on the destination only, for an existing hosted
    /// session created by this exact device, pinned node and key. The peer
    /// must still have active verified membership in the mounted profile.
    ///
    /// The trusted composition supplies the expected tenant and owner epoch
    /// from the selected ownership fence. Hub directory generation and host
    /// instance/address are separate fences and must be verified outside
    /// this API. No control path, portable grant, or stream is returned. This
    /// result is a point-in-time preflight; production attach needs the
    /// enclosing fence and normal operation/revocation checks at use time.
    ///
    /// # Errors
    /// Returns a uniform refusal for missing, closed, unpublished, foreign,
    /// not creator-bound, stale, unnegotiated or revoked sessions/peers.
    pub async fn authorize_owner_observation(
        &self,
        session: &SessionId,
        expected_tenant: &TenantId,
        expected_owner_epoch: NonZeroU64,
    ) -> Result<(), HostError> {
        self.owner_observation_status(session, expected_tenant, expected_owner_epoch).await.map(|_| ())
    }

    async fn owner_observation_status(
        &self,
        session: &SessionId,
        expected_tenant: &TenantId,
        expected_owner_epoch: NonZeroU64,
    ) -> Result<OwnerObservationStatus, HostError> {
        let check_connection = || {
            let caps = self.granted().map_err(|_| unavailable())?;
            let identity = &self.identity;
            if self.host.is_draining() || self.host.epoch != expected_owner_epoch.get()
                || identity.zone != TrustZone::Remote || identity.strength != AuthStrength::MutualTls
                || identity.principal.kind() != PrincipalKind::Human
                || identity.principal.surface() != IngressSurface::Gateway
                || identity.agent.is_some() || identity.device.is_none()
                || identity.authenticated_node.is_none()
                || identity.authenticated_key_fingerprint.is_none_or(|key| key == [0; 32])
                || identity.tenant.as_ref() != Some(expected_tenant)
            {
                return Err(unavailable());
            }
            admit(identity, caps, Some(expected_tenant), Need::Observe).map_err(|_| unavailable())
        };
        check_connection()?;
        let permit = ADMISSION.try_acquire().map_err(|_| unavailable())?;
        let memberships = self.host.user_memberships.get().cloned().ok_or_else(unavailable)?;
        let host = Arc::clone(&self.host);
        let identity = self.identity.clone();
        let session = session.clone();
        let tenant = expected_tenant.clone();
        let status = tokio::task::spawn_blocking(move || {
            // Do not release admission when the async caller cancels while
            // a bounded registry or local record read is still in progress.
            let _permit = permit;
            // Read the current pin, enrollment and explicit membership
            // independently of hello caps and the revocation watcher's tick.
            // Check before touching a record and again before returning.
            let resolve = || {
                memberships.resolve_current(&identity).map_err(|_| unavailable())?
                    .filter(|row| row.matches(&identity)).ok_or_else(unavailable)
            };
            let admitted = resolve()?;
            let status = check_existing_record(&host, &identity, &session, &tenant, expected_owner_epoch)?;
            let current = resolve()?;
            if current.user_id != admitted.user_id || current.issued_revision != admitted.issued_revision {
                return Err(unavailable());
            }
            Ok(status)
        })
        .await
        .map_err(|_| unavailable())??;
        check_connection()?;
        Ok(status)
    }
}


/// Exact trusted metadata already configured for the local owner process.
/// The host binds it once and cannot be repinned to other coordinates.
pub(super) struct ProcessCoordinates {
    node: String,
    instance: String,
}

/// Process-local identity pin obtained only from the actual selected host.
/// A separate process, a host reopened after restart, or a different host in
/// this process cannot match it even if the stored epoch and names coincide.
/// Not serializable; not a network attestation or bearer capability.
pub struct OwnerProcessPin {
    owner: Weak<HostInner>,
}

/// Exact selection derived by trusted routing composition, never directly
/// from incoming fields. Independently checked against the pinned process
/// and a genuinely negotiated destination connection.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OwnerObservationScope<'a> {
    pub owner_node: &'a str,
    pub owner_instance: &'a str,
    pub owner_epoch: NonZeroU64,
    pub session: &'a str,
    pub tenant: &'a str,
    pub caller_node: &'a str,
    pub caller_device: &'a str,
    pub caller_key_fingerprint: [u8; 32],
}

/// Fresh placement from the independently trusted Root-Hub authority.
/// `placement_generation` is never the transcript cursor generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentOwnerPlacement {
    pub tenant: TenantId,
    pub session: SessionId,
    pub owner_node: NodeId,
    pub owner_instance: String,
    pub owner_epoch: NonZeroU64,
    pub placement_generation: NonZeroU64,
    pub activation_epoch: NonZeroU64,
    /// Bound by the Hub to the original caller it authenticated, never copied
    /// from the redeeming owner or from an unauthenticated request field.
    pub caller_node: NodeId,
    pub caller_device: DeviceId,
    pub caller_key_fingerprint: [u8; 32],
    pub read_correlation: [u8; 32],
    pub reservation_id: [u8; 32],
    pub phase: OwnerPlacementLookupPhase,
}

/// A separately provisioned owner-side channel to the real placement
/// authority. Redeem only the exact one-use selector on an independently
/// pinned owner-to-Hub control channel. The Hub must return its freshly
/// authorized tuple, not echo owner input or answer a cache lookup. No
/// session-based lookup, fallback or default implementation is installed.
/// Implementations must bound their own work and keep that capacity charged
/// if dropping the future cannot cancel an underlying lookup.
pub trait DirectOwnerPlacementAuthority: Send + Sync {
    fn lookup(&self, id: OwnerPlacementLookupId) -> PortFuture<'_, CurrentOwnerPlacement>;
}

/// The separately negotiated WebSocket task owns the only task-liveness sender.
pub(super) struct DirectOwnerStatus {
    pin: Arc<OwnerProcessPin>,
    task_closed: watch::Receiver<bool>,
    listener_closed: watch::Receiver<bool>,
    connection: OnceLock<Weak<HostConnection>>,
}

impl DirectOwnerStatus {
    pub(super) fn is_live(&self) -> bool {
        self.task_closed.has_changed().is_ok() && !*self.task_closed.borrow()
            && self.listener_closed.has_changed().is_ok() && !*self.listener_closed.borrow()
    }

    pub(super) fn reservable(&self) -> bool {
        self.is_live() && self.connection.get().is_some_and(|weak| weak.strong_count() != 0)
    }
}

impl HostConnection {
    /// Bind a pre-verified, exact accepted owner WebSocket. Only the direct
    /// owner listener may call this adapter, after it captured sealed origin
    /// provenance before upgrade. A node id from an extension or wire payload
    /// alone is not enough. Call within the upgraded task, before hello.
    #[doc(hidden)]
    pub fn bind_direct_owner_status(
        &mut self,
        pin: Arc<OwnerProcessPin>,
        task_closed: watch::Receiver<bool>,
        listener_closed: watch::Receiver<bool>,
    ) {
        if self.direct_owner_status.is_some() || self.wire_minor.load(std::sync::atomic::Ordering::Acquire) != 0
            || !pin.owner.upgrade().is_some_and(|owner| Arc::ptr_eq(&owner, &self.host))
            || self.identity.zone != TrustZone::Remote
            || self.identity.strength != AuthStrength::MutualTls
            || self.identity.principal.kind() != PrincipalKind::Human
            || self.identity.principal.surface() != IngressSurface::Gateway
            || self.identity.agent.is_some() || self.identity.device.is_none()
            || self.identity.authenticated_node.is_none()
            || self.identity.authenticated_key_fingerprint.is_none_or(|key| key == [0; 32])
            || self.identity.tenant.is_none()
        {
            return;
        }
        let binding = DirectOwnerStatus {
            pin, task_closed, listener_closed, connection: OnceLock::new(),
        };
        if binding.is_live() {
            self.direct_owner_status = Some(binding);
        }
    }

    pub(super) async fn direct_owner_status_for(
        &self,
        session: &SessionId,
    ) -> Result<OwnerObservationStatus, HostError> {
        let binding = self.direct_owner_status.as_ref()
            .filter(|binding| binding.is_live()).ok_or_else(unavailable)?;
        if self.host.direct_owner_placement.get().is_some()
            || !self.direct_owner_status_negotiated.load(std::sync::atomic::Ordering::Acquire)
        {
            // Once a fenced authority is installed, plain V1 cannot bypass it.
            return Err(unavailable());
        }
        let owner = binding.pin.owner.upgrade().ok_or_else(unavailable)?;
        let coordinates = owner.owner_process.get().ok_or_else(unavailable)?;
        let identity = &self.identity;
        // Only the session id is supplied by the wire. All other selectors
        // come from the owner startup pin or this exact original connection.
        let selection = OwnerObservationScope {
            owner_node: &coordinates.node,
            owner_instance: &coordinates.instance,
            owner_epoch: NonZeroU64::new(owner.epoch).ok_or_else(unavailable)?,
            session: session.as_str(),
            tenant: identity.tenant.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_node: identity.authenticated_node.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_device: identity.device.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_key_fingerprint: identity.authenticated_key_fingerprint.ok_or_else(unavailable)?,
        };
        let result = binding.pin.observe_status(self, selection).await?;
        if !binding.is_live() {
            return Err(unavailable());
        }
        Ok(result)
    }

    pub(super) async fn fenced_direct_owner_status_for(
        &self,
        request: FencedDirectOwnerStatus,
    ) -> Result<OwnerObservationStatus, HostError> {
        let (expected, pair, reservation) = request.into_selection().ok_or_else(unavailable)?;
        let binding = self.direct_owner_status.as_ref()
            .filter(|binding| binding.is_live()).ok_or_else(unavailable)?;
        let owner = binding.pin.owner.upgrade().ok_or_else(unavailable)?;
        let coordinates = owner.owner_process.get().ok_or_else(unavailable)?;
        let identity = &self.identity;
        let selection = OwnerObservationScope {
            owner_node: &coordinates.node,
            owner_instance: &expected.owner_instance,
            owner_epoch: expected.owner_epoch,
            session: expected.session_id.as_str(),
            tenant: identity.tenant.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_node: identity.authenticated_node.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_device: identity.device.as_ref().ok_or_else(unavailable)?.as_str(),
            caller_key_fingerprint: identity.authenticated_key_fingerprint.ok_or_else(unavailable)?,
        };
        self.observe_selected_direct_owner_status(selection, expected.placement_generation, pair, reservation).await
    }

    /// Read one exact status through the original device's negotiated direct
    /// owner connection, fenced by a separately provisioned placement
    /// authority. The supplied selection is a comparison only; it's never
    /// passed to the verifier as its expected result. The dedicated fenced
    /// operation cannot fall back to the unplaced direct status method.
    ///
    /// # Errors
    /// Denies missing authority, wrong peer or process, stale placement,
    /// expired or busy reads, and changed policy or connection liveness.
    pub async fn observe_selected_direct_owner_status(
        &self,
        selection: OwnerObservationScope<'_>,
        placement_generation: NonZeroU64,
        tickets: OwnerPlacementLookupPair,
        reservation: OwnerObservationReservationId,
    ) -> Result<OwnerObservationStatus, HostError> {
        let binding = self.direct_owner_status.as_ref()
            .filter(|binding| binding.is_live()).ok_or_else(unavailable)?;
        let authority = self.host.direct_owner_placement.get().ok_or_else(unavailable)?;
        let connection_is_current = || {
            let caps = self.granted().map_err(|_| unavailable())?;
            if !caps.observe || !binding.is_live() || self.host.is_draining()
                || !self.fenced_direct_owner_status_negotiated.load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(unavailable());
            }
            Ok(())
        };
        connection_is_current()?;
        let (session, tenant) = binding.pin.match_scope(self, &selection)?;
        if self.identity.tenant.as_ref() != Some(&tenant) {
            return Err(unavailable());
        }
        let _permit = SELECTED_ADMISSION.try_acquire().map_err(|_| unavailable())?;
        let (reservation_id, expires_at) = self.consume_signed_owner_reservation(reservation, selection)?;
        let deadline = Instant::now().checked_add(SELECTED_OBSERVATION_TIMEOUT)
            .ok_or_else(unavailable)?.min(expires_at);
        let matches = |row: &CurrentOwnerPlacement, phase| {
            row.tenant == tenant && row.session == session
                && row.owner_node.as_str() == selection.owner_node
                && row.owner_instance == selection.owner_instance
                && row.owner_epoch == selection.owner_epoch
                && row.placement_generation == placement_generation
                && row.caller_node.as_str() == selection.caller_node
                && row.caller_device.as_str() == selection.caller_device
                && row.caller_key_fingerprint == selection.caller_key_fingerprint
                && row.read_correlation != [0; 32]
                && row.reservation_id == reservation_id
                && row.phase == phase
        };
        let (before_id, after_id) = tickets.into_ids();
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            // Reject an ineligible or revoked local device/session before
            // redeeming any remote selector; no local status leaves preflight.
            binding.pin.authorize(self, selection).await?;
            connection_is_current()?;
            let before = authority.lookup(before_id).await.map_err(|_| unavailable())?;
            if !matches(&before, OwnerPlacementLookupPhase::Before) {
                return Err(unavailable());
            }
            connection_is_current()?;
            let status = binding.pin.observe_status(self, selection).await?;
            let after = authority.lookup(after_id).await.map_err(|_| unavailable())?;
            if !matches(&after, OwnerPlacementLookupPhase::After)
                || after.read_correlation != before.read_correlation
                || after.activation_epoch != before.activation_epoch
            {
                return Err(unavailable());
            }
            connection_is_current()?;
            if Instant::now() >= deadline {
                return Err(unavailable());
            }
            Ok(status)
        }).await.map_err(|_| unavailable())?
    }
}

impl SessionHost {
    /// Install the independently authenticated placement verifier after
    /// pinning this actual host. No call can pass the selected-placement
    /// path until trusted bootstrap performs this one-time composition.
    ///
    /// # Errors
    /// A draining/unpinned host or second installation is refused.
    pub fn install_direct_owner_placement_authority(
        &self,
        authority: Arc<dyn DirectOwnerPlacementAuthority>,
    ) -> Result<(), HostError> {
        if self.inner.is_draining() || self.inner.owner_process.get().is_none() {
            return Err(unavailable());
        }
        self.inner.direct_owner_placement.set(authority).map_err(|_| unavailable())
    }

    /// Bind one trusted startup node and instance to this actual host.
    /// A route, client, report, or file inspected after opening is not a
    /// configuration provenance. The bootstrap caller must already own this
    /// exact host handle and obtain coordinates from its selected deployment.
    pub fn pin_owner_process(
        &self,
        trusted_node: &str,
        trusted_instance: &str,
    ) -> Result<OwnerProcessPin, HostError> {
        let valid = |value: &str| {
            !value.is_empty() && value.len() <= 255 && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        };
        if !valid(trusted_node) || !valid(trusted_instance) || self.inner.is_draining() {
            return Err(unavailable());
        }
        self.inner.owner_process.set(ProcessCoordinates {
            node: trusted_node.to_owned(),
            instance: trusted_instance.to_owned(),
        }).map_err(|_| unavailable())?;
        Ok(OwnerProcessPin { owner: Arc::downgrade(&self.inner) })
    }
}

impl OwnerProcessPin {
    /// Revalidate one operator-selected assignment against the exact open
    /// process and its durable hosted record. This returns no session data
    /// or device grant. Hub-assigned generation is deliberately not inferred
    /// from a local transcript generation or from this local check.
    ///
    /// # Errors
    /// Refuses stale, draining, foreign, non-durable or terminal sessions.
    pub fn validate_publication(
        &self,
        node: &str,
        instance: &str,
        epoch: NonZeroU64,
        session: &SessionId,
        tenant: &TenantId,
    ) -> Result<(), HostError> {
        let owner = self.owner.upgrade().ok_or_else(unavailable)?;
        let coordinates = owner.owner_process.get().ok_or_else(unavailable)?;
        let active = || !owner.is_draining()
            && owner.epoch == epoch.get()
            && owner.listeners.lock().is_ok_and(|listeners| listeners.iter().any(|listener| {
                listener.enabled && matches!(listener.kind, harw_protocol::session_wire::ListenerKind::Node)
            }));
        if !active() || coordinates.node != node || coordinates.instance != instance {
            return Err(unavailable());
        }
        let matches = |record: &HostedSessionRecord| {
            &record.session_id == session
                && record.tenant.as_ref() == Some(tenant)
                && record.host_epoch == epoch.get()
                && !record.state.is_terminal()
        };
        let durable = owner.records.load_for_owner_observation(session)
            .map_err(|_| unavailable())?.filter(|record| matches(record)).ok_or_else(unavailable)?;
        let loaded = { owner.slots.lock().map_err(|_| unavailable())?.get(session).cloned() };
        if let Some(slot) = loaded {
            let slot = slot.lock().map_err(|_| unavailable())?;
            if slot.ephemeral || slot.record.generation != durable.generation || !matches(&slot.record) {
                return Err(unavailable());
            }
        }
        if !active() {
            return Err(unavailable());
        }
        Ok(())
    }

    /// Independently authenticate the destination process identity in a
    /// same-process composition, and then authorize this exact originating
    /// device through that owner's ordinary read-only preflight. Merely
    /// knowing the owner's coordinates or presenting a connection negotiated
    /// at a different Host cannot pass. An intermediary must not construct
    /// or impersonate the originating device's connection.
    pub async fn authorize(
        &self,
        destination: &HostConnection,
        scope: OwnerObservationScope<'_>,
    ) -> Result<(), HostError> {
        self.observe_status(destination, scope).await.map(|_| ())
    }

    /// Read a bounded point-in-time status from the pinned process; only the
    /// exact negotiated originating connection can request it. The outer
    /// trusted composition must recheck its routing grant and fence before
    /// emitting the result. This does not attach, hydrate, or read a transcript.
    ///
    /// # Errors
    /// Returns a uniform refusal if any process, scope, authorization, or
    /// record fence is unavailable, stale, or not eligible.
    pub async fn observe_status(
        &self,
        destination: &HostConnection,
        scope: OwnerObservationScope<'_>,
    ) -> Result<OwnerObservationStatus, HostError> {
        let (session, tenant) = self.match_scope(destination, &scope)?;
        destination.owner_observation_status(&session, &tenant, scope.owner_epoch).await
    }

    fn match_scope(
        &self,
        destination: &HostConnection,
        scope: &OwnerObservationScope<'_>,
    ) -> Result<(SessionId, TenantId), HostError> {
        let owner = self.owner.upgrade().ok_or_else(unavailable)?;
        let coordinates = owner.owner_process.get().ok_or_else(unavailable)?;
        if !Arc::ptr_eq(&owner, &destination.host)
            || coordinates.node != scope.owner_node
            || coordinates.instance != scope.owner_instance
            || owner.epoch != scope.owner_epoch.get()
            || destination.identity.authenticated_node.as_ref().map(|id| id.as_str())
                != Some(scope.caller_node)
            || destination.identity.device.as_ref().map(|id| id.as_str())
                != Some(scope.caller_device)
            || scope.caller_key_fingerprint == [0; 32]
            || destination.identity.authenticated_key_fingerprint != Some(scope.caller_key_fingerprint)
        {
            return Err(unavailable());
        }
        let session = SessionId::parse(scope.session).map_err(|_| unavailable())?;
        let tenant = TenantId::parse(scope.tenant).map_err(|_| unavailable())?;
        Ok((session, tenant))
    }
}

//! Bounded one-use reservation of an already authenticated device's exact
//! owner observation. No registry-only or Hub-provided identity can mint it.

use std::{
    collections::BTreeMap,
    num::NonZeroU64,
    sync::{Arc, Mutex, Weak, atomic::Ordering},
    time::{Duration, Instant},
};

use harw_protocol::owner_placement::{
    OwnerObservationReservationId, OwnerObservationReservationReceipt,
};
use harw_types::{DeviceId, NodeId, SessionId, TenantId};

use super::{HostConnection, HostError, OwnerObservationScope, OwnerProcessPin, SessionHost, unavailable};

const MAX_RESERVATIONS: usize = 128;
const MAX_PER_CONNECTION: usize = 4;
const RESERVATION_TTL: Duration = Duration::from_secs(10);
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct ReservedScope {
    owner_node: String,
    owner_instance: String,
    owner_epoch: NonZeroU64,
    session: SessionId,
    tenant: TenantId,
    caller_node: NodeId,
    caller_device: DeviceId,
    caller_key_fingerprint: [u8; 32],
}

impl ReservedScope {
    fn borrowed(&self) -> OwnerObservationScope<'_> {
        OwnerObservationScope {
            owner_node: &self.owner_node,
            owner_instance: &self.owner_instance,
            owner_epoch: self.owner_epoch,
            session: self.session.as_str(),
            tenant: self.tenant.as_str(),
            caller_node: self.caller_node.as_str(),
            caller_device: self.caller_device.as_str(),
            caller_key_fingerprint: self.caller_key_fingerprint,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Reserved,
    Signing,
    Signed,
}

struct Reservation {
    connection: Weak<HostConnection>,
    scope: ReservedScope,
    expires_at: Instant,
    phase: Phase,
}

#[derive(Default)]
pub(in crate::host) struct OwnerObservationReservations(Mutex<BTreeMap<[u8; 32], Reservation>>);

impl HostConnection {
    /// Called only from the accepted WebSocket task after sealed direct-peer
    /// binding and before hello. The registry never retains the connection.
    #[doc(hidden)]
    pub fn bind_direct_owner_connection(connection: &Arc<Self>) {
        if let Some(binding) = &connection.direct_owner_status
            && binding.is_live()
            && connection.wire_minor.load(Ordering::Acquire) == 0
        {
            let _ = binding.connection.set(Arc::downgrade(connection));
        }
    }

    fn require_reservation_connection(&self) -> Result<(), HostError> {
        let binding = self.direct_owner_status.as_ref().ok_or_else(unavailable)?;
        if !self.fenced_direct_owner_status_negotiated.load(Ordering::Acquire)
            || !self.granted().map_err(|_| unavailable())?.observe
            || self.host.is_draining()
            || self.host.direct_owner_placement.get().is_none()
            || !binding.is_live()
            || !binding.connection.get().is_some_and(|weak| std::ptr::eq(weak.as_ptr(), self))
        {
            return Err(unavailable());
        }
        Ok(())
    }

    pub(in crate::host) async fn reserve_direct_owner_observation(
        &self,
        session: SessionId,
    ) -> Result<OwnerObservationReservationReceipt, HostError> {
        self.require_reservation_connection()?;
        let binding = self.direct_owner_status.as_ref().ok_or_else(unavailable)?;
        let original = binding.connection.get().ok_or_else(unavailable)?;
        let owner = binding.pin.owner.upgrade().ok_or_else(unavailable)?;
        let coordinates = owner.owner_process.get().ok_or_else(unavailable)?;
        let scope = ReservedScope {
            owner_node: coordinates.node.clone(),
            owner_instance: coordinates.instance.clone(),
            owner_epoch: NonZeroU64::new(owner.epoch).ok_or_else(unavailable)?,
            session,
            tenant: self.identity.tenant.clone().ok_or_else(unavailable)?,
            caller_node: self.identity.authenticated_node.clone().ok_or_else(unavailable)?,
            caller_device: self.identity.device.clone().ok_or_else(unavailable)?,
            caller_key_fingerprint: self.identity.authenticated_key_fingerprint.ok_or_else(unavailable)?,
        };
        // This is the existing exact durable creator and current enrolled
        // membership check, not membership alone. It receives no claimed IDs.
        tokio::time::timeout(PREFLIGHT_TIMEOUT, binding.pin.authorize(self, scope.borrowed()))
            .await.map_err(|_| unavailable())??;
        self.require_reservation_connection()?;
        let mut entries = self.host.owner_observation_reservations.0.try_lock()
            .map_err(|_| unavailable())?;
        let now = Instant::now();
        entries.retain(|_, row| now < row.expires_at && row.connection.strong_count() != 0);
        if entries.len() >= MAX_RESERVATIONS
            || entries.values().filter(|row| row.connection.ptr_eq(original))
                .take(MAX_PER_CONNECTION).count() >= MAX_PER_CONNECTION
        {
            return Err(unavailable());
        }
        let expires_at = now.checked_add(RESERVATION_TTL).ok_or_else(unavailable)?;
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| unavailable())?;
        let id = OwnerObservationReservationId::from_bytes(bytes).ok_or_else(unavailable)?;
        if entries.contains_key(&bytes) {
            return Err(unavailable());
        }
        self.require_reservation_connection()?;
        entries.insert(bytes, Reservation {
            connection: original.clone(), scope, expires_at, phase: Phase::Reserved,
        });
        Ok(OwnerObservationReservationReceipt::new(id))
    }

    pub(in crate::host) fn consume_signed_owner_reservation(
        &self,
        id: OwnerObservationReservationId,
        expected: OwnerObservationScope<'_>,
    ) -> Result<([u8; 32], Instant), HostError> {
        self.require_reservation_connection()?;
        let bytes = *id.as_bytes();
        let mut entries = self.host.owner_observation_reservations.0.try_lock()
            .map_err(|_| unavailable())?;
        let row = entries.get(&bytes).ok_or_else(unavailable)?;
        if !std::ptr::eq(row.connection.as_ptr(), self) || row.phase != Phase::Signed
            || Instant::now() >= row.expires_at || row.scope.borrowed() != expected
        {
            return Err(unavailable());
        }
        let expiry = row.expires_at;
        entries.remove(&bytes);
        Ok((bytes, expiry))
    }
}

/// A claimed single signing attempt; it cannot create a fresh connection or
/// keep the actual WebSocket task alive. Drop spends an unfinished attempt.
pub struct ClaimedOwnerObservation {
    key: [u8; 32],
    connection: Arc<HostConnection>,
    pin: Arc<OwnerProcessPin>,
    scope: ReservedScope,
    expires_at: Instant,
    signed: bool,
}

impl ClaimedOwnerObservation {
    #[must_use]
    pub fn connection(&self) -> &HostConnection {
        &self.connection
    }

    #[must_use]
    pub fn pin(&self) -> &OwnerProcessPin {
        &self.pin
    }

    #[must_use]
    pub fn scope(&self) -> OwnerObservationScope<'_> {
        self.scope.borrowed()
    }

    #[must_use]
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }

    fn require_current(&self) -> Result<(), HostError> {
        if Instant::now() >= self.expires_at {
            return Err(unavailable());
        }
        self.connection.require_reservation_connection()
    }

    /// Verify actual WebSocket liveness and current local authorization.
    pub async fn revalidate(&self) -> Result<(), HostError> {
        self.require_current()?;
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(self.expires_at),
            self.pin.authorize(&self.connection, self.scope.borrowed()),
        ).await.map_err(|_| unavailable())??;
        self.require_current()
    }

    /// Trusted custody calls this only after successful exact proof signing.
    /// The reservation then becomes usable once, on the same device socket.
    pub fn mark_signed(mut self) -> Result<(), HostError> {
        self.require_current()?;
        {
            let mut entries = self.connection.host.owner_observation_reservations.0.try_lock()
                .map_err(|_| unavailable())?;
            let row = entries.get_mut(&self.key).ok_or_else(unavailable)?;
            if row.phase != Phase::Signing
                || !std::ptr::eq(row.connection.as_ptr(), self.connection.as_ref())
                || row.expires_at != self.expires_at
            {
                return Err(unavailable());
            }
            self.require_current()?;
            row.phase = Phase::Signed;
        }
        self.signed = true;
        Ok(())
    }
}

impl Drop for ClaimedOwnerObservation {
    fn drop(&mut self) {
        if !self.signed
            && let Ok(mut entries) = self.connection.host.owner_observation_reservations.0.lock()
            && entries.get(&self.key).is_some_and(|row| row.phase == Phase::Signing
                && row.expires_at == self.expires_at
                && std::ptr::eq(row.connection.as_ptr(), self.connection.as_ref()))
        {
            entries.remove(&self.key);
        }
    }
}

impl SessionHost {
    /// Claim one reservation using independently authenticated exact Hub
    /// policy. The expected scope is a comparison, never an identity source.
    pub fn claim_owner_observation_reservation(
        &self,
        id: OwnerObservationReservationId,
        expected: OwnerObservationScope<'_>,
    ) -> Result<ClaimedOwnerObservation, HostError> {
        let key = *id.as_bytes();
        let mut entries = self.inner.owner_observation_reservations.0.try_lock()
            .map_err(|_| unavailable())?;
        let row = entries.get_mut(&key).ok_or_else(unavailable)?;
        if row.phase != Phase::Reserved || Instant::now() >= row.expires_at
            || row.scope.borrowed() != expected
        {
            return Err(unavailable());
        }
        let connection = row.connection.upgrade().ok_or_else(unavailable)?;
        connection.require_reservation_connection()?;
        let pin = Arc::clone(&connection.direct_owner_status.as_ref().ok_or_else(unavailable)?.pin);
        if !Arc::ptr_eq(&connection.host, &self.inner) {
            return Err(unavailable());
        }
        row.phase = Phase::Signing;
        Ok(ClaimedOwnerObservation {
            key,
            connection,
            pin,
            scope: row.scope.clone(),
            expires_at: row.expires_at,
            signed: false,
        })
    }
}

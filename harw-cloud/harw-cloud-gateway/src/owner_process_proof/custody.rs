//! Process-owned key adapter; no provisioning, secret persistence or service.
//! Never hand incoming fields their own authorization. This module requires
//! the already independently authenticated original device's HostConnection.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use harw_node_transport::{ML_DSA_65_SIGNATURE_LEN, OwnerProcessChannelBinding};
use harw_session_host::{HostConnection, OwnerObservationScope, OwnerProcessPin};
use tokio::sync::Semaphore;

use super::{DOMAIN, RESERVED_DOMAIN, DestinationProcessKeyPin, NONCE_BYTES, ProcessProofError, observation_transcript};
use crate::owner_directory::valid_id;
use crate::route_admission::{CallerGrant, OwnerFence};

const MAX_PENDING_SIGNATURES: usize = 8;
const MAX_RECENT_CHALLENGES: usize = 64;
const LOCAL_DEADLINE: Duration = Duration::from_secs(3);
const REPLAY_RETENTION: Duration = Duration::from_secs(30);
const MAX_TRANSCRIPT_BYTES: usize = DOMAIN.len() + NONCE_BYTES + 64 + 6 * (2 + 255) + 4 * 8;
static SIGNING_CAPACITY: Semaphore = Semaphore::const_new(MAX_PENDING_SIGNATURES);

/// Current policy derived independently by the owner for this exact request,
/// after resolving enrolled device authorization, durable owner generation,
/// and independently trusted current node activation. Never populate this
/// from the transcript, an unauthenticated Hub assertion, or the source
/// address. A pinned one-use read from the actual Hub placement authority
/// may supply the current generation and activation; the matched owner
/// reservation independently supplies the original local connection.
pub struct LocalOwnerObservePolicy<'a> {
    /// Current local authority for this exact hosted session.
    pub fence: &'a OwnerFence,
    /// Current exact observe grant for the independently authenticated user.
    pub grant: &'a CallerGrant,
    /// Independently resolved nonzero Active NetSec epoch.
    pub active_epoch: NonZeroU64,
    /// Fingerprint proven by the original device's own owner-side handshake.
    pub caller_key_fingerprint: [u8; 32],
    /// Monotonic expiry of the locally obtained exact policy observation.
    pub expires_at: Instant,
}

/// Holds a dedicated, already-provisioned ML-DSA-65 key for one owner process.
/// No arbitrary signing API or key loading/exporting. Trusted bootstrap must
/// move in a key held only by this exact incarnation; copying a general
/// node or AuthHub signing key into this adapter is not provisioning.
pub struct OwnerProcessCustody {
    pin: DestinationProcessKeyPin,
    key: PqdsaKeyPair,
    hub_node: String,
    recent: Mutex<BTreeMap<[u8; NONCE_BYTES], Instant>>,
}

impl OwnerProcessCustody {
    /// Adopt the exclusive process key handle from trusted bootstrap.
    /// `expected_hub` selects the exact Root-Hub NodeId and must be enforced
    /// by the independently configured NodeTransport listener as well.
    pub fn new(
        pin: DestinationProcessKeyPin,
        key: PqdsaKeyPair,
        expected_hub: &str,
    ) -> Result<Self, ProcessProofError> {
        pin.ensure_live()?;
        if !valid_id(expected_hub)
            || expected_hub == pin.node
            || key.public_key().as_ref() != pin.process_key.as_slice()
        {
            return Err(ProcessProofError::InvalidPin);
        }
        Ok(Self {
            pin,
            key,
            hub_node: expected_hub.to_owned(),
            recent: Mutex::new(BTreeMap::new()),
        })
    }

    /// Import an explicitly provisioned seed for this incarnation. The caller
    /// owns credential custody and zeroizes the seed buffer after import.
    /// The derived public key must match the supplied independent process pin.
    pub fn from_process_seed(
        pin: DestinationProcessKeyPin,
        seed: &[u8; 32],
        expected_hub: &str,
    ) -> Result<Self, ProcessProofError> {
        if *seed == [0; 32] {
            return Err(ProcessProofError::InvalidPin);
        }
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, seed)
            .map_err(|_| ProcessProofError::InvalidPin)?;
        Self::new(pin, key, expected_hub)
    }

    /// Irreversibly withdraw the signing role for this object's lifetime.
    /// Dropping this object relinquishes its exclusive key handle.
    pub fn withdraw(&self) {
        self.pin.withdraw();
    }

    /// Sign only the canonical exact observe challenge on the same accepted
    /// authenticated Hub channel, after the current exact owner preflight of
    /// the original caller's separately authenticated end-to-end connection.
    /// Returns a signature only. Never returns data or creates a grant.
    pub async fn sign_exact_observe(
        &self,
        channel: &OwnerProcessChannelBinding,
        host_pin: &OwnerProcessPin,
        caller: &HostConnection,
        policy: LocalOwnerObservePolicy<'_>,
        challenge: &[u8],
    ) -> Result<[u8; ML_DSA_65_SIGNATURE_LEN], ProcessProofError> {
        self.sign_bound_observe(channel, host_pin, caller, policy, challenge, None).await
    }

    /// Sign only after the actual owner-side reservation was claimed.
    pub async fn sign_reserved_observe(
        &self,
        channel: &OwnerProcessChannelBinding,
        host_pin: &OwnerProcessPin,
        caller: &HostConnection,
        policy: LocalOwnerObservePolicy<'_>,
        challenge: &[u8],
        reservation: [u8; 32],
    ) -> Result<[u8; ML_DSA_65_SIGNATURE_LEN], ProcessProofError> {
        self.sign_bound_observe(channel, host_pin, caller, policy, challenge, Some(reservation)).await
    }

    async fn sign_bound_observe(
        &self,
        channel: &OwnerProcessChannelBinding,
        host_pin: &OwnerProcessPin,
        caller: &HostConnection,
        policy: LocalOwnerObservePolicy<'_>,
        challenge: &[u8],
        reservation: Option<[u8; 32]>,
    ) -> Result<[u8; ML_DSA_65_SIGNATURE_LEN], ProcessProofError> {
        let begin = Instant::now();
        let deadline = begin.checked_add(LOCAL_DEADLINE)
            .ok_or(ProcessProofError::Unavailable)?.min(policy.expires_at);
        ensure_before(deadline)?;
        let _capacity = SIGNING_CAPACITY.try_acquire().map_err(|_| ProcessProofError::Unavailable)?;
        self.pin.ensure_live()?;
        let fence = policy.fence;
        let grant = policy.grant;
        if self.pin.node != fence.node_id
            || self.pin.instance != fence.host_instance
            || self.pin.owner_epoch.get() != fence.owner_epoch
            || grant.tenant != fence.tenant
            || grant.session_id != fence.session_id
            || !grant.may_observe
            || policy.caller_key_fingerprint == [0; 32]
        {
            return Err(ProcessProofError::Scope);
        }
        let binding = self.accepted_binding(channel)?;
        if challenge.len() > MAX_TRANSCRIPT_BYTES {
            return Err(ProcessProofError::Scope);
        }
        let nonce: [u8; NONCE_BYTES] = challenge.strip_prefix(if reservation.is_some() { RESERVED_DOMAIN } else { DOMAIN })
            .and_then(|fields| fields.get(..NONCE_BYTES))
            .and_then(|raw| raw.try_into().ok())
            .ok_or(ProcessProofError::Scope)?;
        if nonce == [0; NONCE_BYTES] {
            return Err(ProcessProofError::Scope);
        }
        let expected = observation_transcript(&nonce, binding, fence, grant, policy.active_epoch, reservation)?;
        if challenge != expected.as_slice() {
            return Err(ProcessProofError::Scope);
        }
        let authorized_scope = || OwnerObservationScope {
            owner_node: &fence.node_id,
            owner_instance: &fence.host_instance,
            owner_epoch: self.pin.owner_epoch,
            session: &fence.session_id,
            tenant: &fence.tenant,
            caller_node: &grant.authenticated_node,
            caller_device: &grant.device_id,
            caller_key_fingerprint: policy.caller_key_fingerprint,
        };
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            host_pin.authorize(caller, authorized_scope()),
        ).await
            .map_err(|_| ProcessProofError::Expired)?
            .map_err(|_| ProcessProofError::Scope)?;
        ensure_before(deadline)?;
        self.pin.ensure_live()?;
        if self.accepted_binding(channel)? != binding {
            return Err(ProcessProofError::Scope);
        }
        self.claim_nonce(nonce)?;
        let mut signature = [0; ML_DSA_65_SIGNATURE_LEN];
        // Sign our canonical reconstruction, not received bytes, after all
        // exact membership, incarnation, channel and policy comparisons.
        let written = self.key.sign(&expected, &mut signature)
            .map_err(|_| ProcessProofError::Unavailable)?;
        if written != signature.len() {
            return Err(ProcessProofError::Unavailable);
        }
        // Refuse to release the signature if independent membership or
        // hosted-record authority changed while the key was in use.
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            host_pin.authorize(caller, authorized_scope()),
        ).await
            .map_err(|_| ProcessProofError::Expired)?
            .map_err(|_| ProcessProofError::Scope)?;
        ensure_before(deadline)?;
        self.pin.ensure_live()?;
        if self.accepted_binding(channel)? != binding {
            return Err(ProcessProofError::Scope);
        }
        Ok(signature)
    }

    fn accepted_binding(&self, channel: &OwnerProcessChannelBinding) -> Result<[u8; 32], ProcessProofError> {
        let peer = channel.peer();
        if channel.is_dialer() || peer.node_id.as_str() != self.hub_node {
            return Err(ProcessProofError::Scope);
        }
        channel.export_for_peer(&peer.node_id).filter(|value| *value != [0; 32])
            .ok_or(ProcessProofError::Scope)
    }

    fn claim_nonce(&self, nonce: [u8; NONCE_BYTES]) -> Result<(), ProcessProofError> {
        let now = Instant::now();
        let mut recent = self.recent.try_lock().map_err(|_| ProcessProofError::Unavailable)?;
        recent.retain(|_, expires| now < *expires);
        if recent.contains_key(&nonce) {
            return Err(ProcessProofError::Scope);
        }
        if recent.len() >= MAX_RECENT_CHALLENGES {
            return Err(ProcessProofError::Unavailable);
        }
        recent.insert(nonce, now.checked_add(REPLAY_RETENTION).ok_or(ProcessProofError::Unavailable)?);
        Ok(())
    }
}

fn ensure_before(deadline: Instant) -> Result<(), ProcessProofError> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(ProcessProofError::Expired)
    }
}

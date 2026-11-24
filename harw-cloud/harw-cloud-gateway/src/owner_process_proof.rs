//! Offline v1 verification contract for an independently pinned owner
//! process, not a node-transport authentication mode or wire endpoint.
//!
//! Only trusted composition supplies the process key and channel binding.
//! A report, network payload, request header, node key alone, address or
//! process label is not process-key provenance.

// The algorithm and key/signature encoding are identical to the canonical
// provider used by harw-node-transport, but the key use and domain are not.
use std::{collections::BTreeMap, fmt, num::NonZeroU64, sync::Arc, time::Instant};
use std::sync::atomic::{AtomicBool, Ordering};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{ML_DSA_65, UnparsedPublicKey};
use harw_node_transport::{ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, node_key_fingerprint};

use crate::owner_directory::valid_id;
use crate::route_admission::{CallerGrant, OwnerFence};

const DOMAIN: &[u8] = b"harw-owner-process-observe/v1\0";
const RESERVED_DOMAIN: &[u8] = b"harw-owner-process-observe/v2\0";
const MAX_PROCESS_PINS: usize = 64;
const NONCE_BYTES: usize = 32;

#[cfg(feature = "hub-owner-process-custody")]
mod custody;
#[cfg(feature = "hub-owner-process-custody")]
pub use custody::{LocalOwnerObservePolicy, OwnerProcessCustody};

/// Refusal to bind or verify the exact offline observation transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessProofError {
    InvalidPin,
    Scope,
    Expired,
    Unavailable,
    InvalidSignature,
}

impl fmt::Display for ProcessProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidPin => "invalid independent owner process pin",
            Self::Scope => "owner process proof scope does not match",
            Self::Expired => "owner process proof expired",
            Self::Unavailable => "owner process proof is unavailable",
            Self::InvalidSignature => "owner process proof did not verify",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ProcessProofError {}

/// One independently selected process pin. Not serializable.
/// The operator/control plane must establish unique process-key custody
/// and supply this pin from a source independent of all routed traffic.
pub struct DestinationProcessKeyPin {
    node: String,
    instance: String,
    owner_epoch: NonZeroU64,
    process_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN],
    enrolled_node_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN],
    identity: Arc<()>,
    withdrawn: AtomicBool,
}

impl DestinationProcessKeyPin {
    /// Bind the explicitly provisioned process public key at bootstrap.
    /// The separately trusted, current long-term key of the same node is
    /// required as a misuse check: a node pin must not be repurposed here.
    /// Distinct bytes alone do not establish independent custody; the
    /// trusted caller must verify exclusive process ownership and revocation
    /// before handing the pin to the Hub. There is no key generation here.
    pub fn new(
        node: &str,
        instance: &str,
        owner_epoch: NonZeroU64,
        process_key: &[u8],
        trusted_node_key: &[u8],
    ) -> Result<Self, ProcessProofError> {
        let process_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN] =
            process_key.try_into().map_err(|_| ProcessProofError::InvalidPin)?;
        let enrolled_node_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN] =
            trusted_node_key.try_into().map_err(|_| ProcessProofError::InvalidPin)?;
        if !valid_id(node)
            || !valid_id(instance)
            || process_key == enrolled_node_key
            || process_key.iter().all(|byte| *byte == 0)
            || trusted_node_key.iter().all(|byte| *byte == 0)
        {
            return Err(ProcessProofError::InvalidPin);
        }
        Ok(Self {
            node: node.to_owned(),
            instance: instance.to_owned(),
            owner_epoch,
            process_key,
            enrolled_node_key,
            identity: Arc::new(()),
            withdrawn: AtomicBool::new(false),
        })
    }

    /// Irreversibly withdraw this pin in the trusted composition.
    /// Replacement must use a new instance; outstanding proofs cannot
    /// be retried under either the withdrawn pin or its replacement.
    pub fn withdraw(&self) {
        self.withdrawn.store(true, Ordering::Release);
    }

    pub(crate) fn matches_enrolled_node_key(&self, key: &[u8]) -> bool {
        self.enrolled_node_key.as_slice() == key
    }

    pub(crate) fn enrolled_key_fingerprint(&self) -> Option<[u8; 32]> {
        node_key_fingerprint(&self.enrolled_node_key)
    }

    pub(crate) fn ensure_live(&self) -> Result<(), ProcessProofError> {
        if self.withdrawn.load(Ordering::Acquire) {
            return Err(ProcessProofError::Scope);
        }
        Ok(())
    }
}

/// Operator-selected, bounded process pins for this Hub. Empty by default.
/// The membership is immutable; withdrawal disables one entry without
/// reviving earlier scope. No NodeId/address fallback or auto-discovery.
#[derive(Default)]
pub struct OwnerProcessTrustSet {
    pins: BTreeMap<(String, String, NonZeroU64), DestinationProcessKeyPin>,
}

impl OwnerProcessTrustSet {
    /// Install exact startup pins. Reject repeated coordinates and process
    /// keys reused across any two configured incarnations. Entries may only
    /// be constructed from independent deployment trust, never peer input.
    pub fn new(pins: Vec<DestinationProcessKeyPin>) -> Result<Self, ProcessProofError> {
        if pins.len() > MAX_PROCESS_PINS {
            return Err(ProcessProofError::InvalidPin);
        }
        let mut selected = Self::default();
        for pin in pins {
            pin.ensure_live()?;
            let coordinates = (pin.node.clone(), pin.instance.clone(), pin.owner_epoch);
            if selected.pins.contains_key(&coordinates)
                || selected.pins.values().any(|existing| existing.process_key == pin.process_key)
            {
                return Err(ProcessProofError::InvalidPin);
            }
            selected.pins.insert(coordinates, pin);
        }
        Ok(selected)
    }

    /// Irreversibly withdraw the exact operator-specified incarnation from
    /// this set. A replacement requires separately provisioned trust.
    pub fn withdraw(&self, node: &str, instance: &str, epoch: NonZeroU64) -> bool {
        if !valid_id(node) || !valid_id(instance) {
            return false;
        }
        self.pins.get(&(node.to_owned(), instance.to_owned(), epoch))
            .is_some_and(|pin| {
                pin.withdraw();
                true
            })
    }

    pub(crate) fn select(&self, fence: &OwnerFence) -> Result<&DestinationProcessKeyPin, ProcessProofError> {
        let epoch = NonZeroU64::new(fence.owner_epoch).ok_or(ProcessProofError::Scope)?;
        let pin = self.pins.get(&(fence.node_id.clone(), fence.host_instance.clone(), epoch))
            .ok_or(ProcessProofError::InvalidPin)?;
        pin.ensure_live()?;
        Ok(pin)
    }
}

/// Exact observation transcript and expiry; created only from bound Hub
/// admission. The nonce is public, fresh, and dedicated to this transcript.
pub(crate) struct ProcessChallenge {
    transcript: Vec<u8>,
    pin: Arc<()>,
    channel_binding: [u8; 32],
    expires_at: Instant,
}

impl ProcessChallenge {
    /// Channel binding must come from the candidate destination channel's
    /// trusted transport exporter, not a claimant. The scope itself is
    /// drawn from the already admitted Hub observation.
    pub(crate) fn issue(
        pin: &DestinationProcessKeyPin,
        fence: &OwnerFence,
        grant: &CallerGrant,
        active_epoch: NonZeroU64,
        channel_binding: [u8; 32],
        expires_at: Instant,
    ) -> Result<Self, ProcessProofError> {
        Self::issue_bound(pin, fence, grant, active_epoch, channel_binding, expires_at, None)
    }

    pub(crate) fn issue_bound(
        pin: &DestinationProcessKeyPin,
        fence: &OwnerFence,
        grant: &CallerGrant,
        active_epoch: NonZeroU64,
        channel_binding: [u8; 32],
        expires_at: Instant,
        reservation: Option<[u8; 32]>,
    ) -> Result<Self, ProcessProofError> {
        if Instant::now() >= expires_at {
            return Err(ProcessProofError::Expired);
        }
        pin.ensure_live()?;
        if pin.node != fence.node_id
            || pin.instance != fence.host_instance
            || pin.owner_epoch.get() != fence.owner_epoch
            || grant.tenant != fence.tenant
            || grant.session_id != fence.session_id
            || !grant.may_observe
            || channel_binding == [0; 32]
        {
            return Err(ProcessProofError::Scope);
        }

        let mut nonce = [0; NONCE_BYTES];
        SystemRandom::new().fill(&mut nonce).map_err(|_| ProcessProofError::Unavailable)?;
        if nonce == [0; NONCE_BYTES] {
            return Err(ProcessProofError::Unavailable);
        }
        let transcript = observation_transcript(&nonce, channel_binding, fence, grant, active_epoch, reservation)?;
        Ok(Self { transcript, pin: Arc::clone(&pin.identity), channel_binding, expires_at })
    }

    pub(crate) fn transcript(&self) -> &[u8] {
        self.transcript.as_slice()
    }

    /// Consume the challenge even on rejection. The caller must supply the
    /// still-current separately provisioned pin; replacing a pin prevents
    /// verification of old challenges even if its bytes were copied.
    pub(crate) fn verify(
        self,
        current_pin: &DestinationProcessKeyPin,
        observed_channel_binding: [u8; 32],
        signature: &[u8],
    ) -> Result<(), ProcessProofError> {
        if Instant::now() >= self.expires_at {
            return Err(ProcessProofError::Expired);
        }
        current_pin.ensure_live()?;
        if !Arc::ptr_eq(&self.pin, &current_pin.identity)
            || self.channel_binding != observed_channel_binding
        {
            return Err(ProcessProofError::Scope);
        }
        if signature.len() != ML_DSA_65_SIGNATURE_LEN {
            return Err(ProcessProofError::InvalidSignature);
        }
        UnparsedPublicKey::new(&ML_DSA_65, current_pin.process_key.as_slice())
            .verify(&self.transcript, signature)
            .map_err(|_| ProcessProofError::InvalidSignature)?;
        if Instant::now() >= self.expires_at {
            return Err(ProcessProofError::Expired);
        }
        current_pin.ensure_live()
    }
}


fn observation_transcript(
    nonce: &[u8; NONCE_BYTES],
    channel_binding: [u8; 32],
    fence: &OwnerFence,
    grant: &CallerGrant,
    active_epoch: NonZeroU64,
    reservation: Option<[u8; 32]>,
) -> Result<Vec<u8>, ProcessProofError> {
    let mut transcript = Vec::with_capacity(512);
    transcript.extend_from_slice(if reservation.is_some() { RESERVED_DOMAIN } else { DOMAIN });
    transcript.extend_from_slice(nonce);
    transcript.extend_from_slice(&channel_binding);
    if let Some(reservation) = reservation {
        if reservation == [0; 32] { return Err(ProcessProofError::Scope); }
        transcript.extend_from_slice(&reservation);
    }
    for field in [
        fence.node_id.as_str(),
        fence.host_instance.as_str(),
        fence.tenant.as_str(),
        fence.session_id.as_str(),
        grant.authenticated_node.as_str(),
        grant.device_id.as_str(),
    ] {
        if !valid_id(field) {
            return Err(ProcessProofError::Scope);
        }
        let len = u16::try_from(field.len()).map_err(|_| ProcessProofError::Scope)?;
        transcript.extend_from_slice(&len.to_be_bytes());
        transcript.extend_from_slice(field.as_bytes());
    }
    for value in [fence.generation.get(), fence.owner_epoch, active_epoch.get(), grant.policy_revision] {
        transcript.extend_from_slice(&value.to_be_bytes());
    }
    Ok(transcript)
}

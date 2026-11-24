//! Current-incarnation challenge proof for the isolated producer. The first
//! supported deployment must read the *same* authoritative NetSec socket at
//! producer and Hub, in one local trust domain. Equal activation integers read
//! from independently managed NetSec stores are not evidence of continuity.
//! This is an internal producer primitive for a fixed proof-only ingress,
//! not an installed route, a session grant, or an assignment authority.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use harw_infra_client::{OwnerActivationObservation, OwnerEpochClient};
use harw_node_transport::{LocalNode, OwnerProcessChannelBinding, node_key_fingerprint};
use harw_session_host::{
    HostError, SessionHost,
    owner_creation::{ML_DSA_65_SIGNATURE_LEN, OwnerCreationReceipt, owner_creation_key_fingerprint},
    owner_readiness_contract::{OwnerReadinessChallenge, OwnerReadinessResponse, owner_creation_receipt_digest},
};
use harw_types::NodeId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout_at};

use super::super::writer_namespace;

mod provision;
use provision::{ArmedReceiptScope, ReadinessConfigurationPin};
pub(in crate::session_serve_remote) use provision::open_configured;

const PROOF_BUDGET: Duration = Duration::from_secs(2);

fn unavailable() -> HostError {
    HostError::Denied("isolated writer readiness is unavailable".into())
}

/// Only the future fixed proof-only listener's trusted startup composition
/// may populate this scope. None of it is selected by a wire challenge.
pub(in crate::session_serve_remote) struct WriterReadinessPin {
    pub hub: NodeId,
    pub hub_public_key: Vec<u8>,
    pub instance: String,
    pub namespace_id: [u8; 32],
    pub receipt_public_key: Vec<u8>,
    pub readiness_public_key: Vec<u8>,
}

struct ProducerScope {
    host: SessionHost,
    remote: PathBuf,
    namespace: [u8; 32],
    owner: NodeId,
    owner_epoch: NonZeroU64,
    instance: String,
    receipt_key: [u8; 32],
    policy: ReadinessConfigurationPin,
}

impl ProducerScope {
    fn matches_guard(&self) -> bool {
        !self.host.is_draining()
            && self.host.epoch() == self.owner_epoch.get()
            && self.policy.current()
            && writer_namespace::active_namespace_id(&self.remote).ok() == Some(self.namespace)
    }

    fn load_current(&self, armed: &ArmedReceiptScope) -> Result<OwnerCreationReceipt, HostError> {
        if !self.matches_guard() || !armed.current() {
            return Err(unavailable());
        }
        // SessionHost checks its installed producer authority, exact record,
        // receipt signature, actual host epoch and live namespace guard.
        let receipt = self.host.owner_creation_receipt(&armed.tenant, &armed.session)?;
        let claims = receipt.claims();
        if claims.node != self.owner
            || claims.instance != self.instance
            || claims.tenant != armed.tenant
            || claims.session != armed.session
            || claims.namespace_id != self.namespace
            || claims.epoch != self.owner_epoch
            || claims.key_fingerprint != self.receipt_key
            || !armed.current()
            || !self.matches_guard()
        {
            return Err(unavailable());
        }
        Ok(receipt)
    }
}

pub(in crate::session_serve_remote) struct GuardedWriterReadiness {
    scope: Arc<ProducerScope>,
    hub: NodeId,
    hub_key: [u8; 32],
    readiness_key: [u8; 32],
    key: PqdsaKeyPair,
    netsec: OwnerEpochClient,
    one_in_flight: Arc<Semaphore>,
}

impl GuardedWriterReadiness {
    /// Accepts an already provisioned readiness-only credential; does not
    /// mint keys, select an endpoint or enable the proof route. The NetSec
    /// socket directory must be managed by the trusted operator and not
    /// writable by the Hub, writer, or other untrusted UIDs.
    fn open(
        host: SessionHost,
        remote: &Path,
        local: &LocalNode,
        pin: WriterReadinessPin,
        netsec_socket: &Path,
        netsec_peer_uid: u32,
        seed: &[u8; 32],
        policy: ReadinessConfigurationPin,
    ) -> Result<Self, HostError> {
        let namespace = writer_namespace::active_namespace_id(remote).map_err(|_| unavailable())?;
        let owner_epoch = NonZeroU64::new(host.epoch()).ok_or_else(unavailable)?;
        if host.is_draining()
            || &pin.hub == local.node_id()
            || pin.namespace_id != namespace
            || *seed == [0; 32]
            || !policy.current()
        {
            return Err(unavailable());
        }
        let owner_key = node_key_fingerprint(&local.identity.public_key).ok_or_else(unavailable)?;
        let hub_key = node_key_fingerprint(&pin.hub_public_key).ok_or_else(unavailable)?;
        let receipt_key = owner_creation_key_fingerprint(&pin.receipt_public_key).ok_or_else(unavailable)?;
        let readiness_key = owner_creation_key_fingerprint(&pin.readiness_public_key).ok_or_else(unavailable)?;
        let keys = [owner_key, hub_key, receipt_key, readiness_key];
        if keys.iter().enumerate().any(|(index, key)| keys[..index].contains(key)) {
            return Err(unavailable());
        }
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, seed).map_err(|_| unavailable())?;
        if key.public_key().as_ref() != pin.readiness_public_key.as_slice() {
            return Err(unavailable());
        }
        let netsec = OwnerEpochClient::new(netsec_socket.to_owned(), netsec_peer_uid)
            .map_err(|_| unavailable())?;
        Ok(Self {
            scope: Arc::new(ProducerScope {
                host,
                remote: remote.to_owned(),
                namespace,
                owner: local.node_id().clone(),
                owner_epoch,
                instance: pin.instance,
                receipt_key,
                policy,
            }),
            hub: pin.hub,
            hub_key,
            readiness_key,
            key,
            netsec,
            one_in_flight: Arc::new(Semaphore::new(1)),
        })
    }

    fn exporter(&self, channel: &OwnerProcessChannelBinding) -> Result<[u8; 32], HostError> {
        if channel.is_dialer()
            || channel.verified_peer_key_fingerprint() != Some(self.hub_key)
        {
            return Err(unavailable());
        }
        channel.export_for_peer(&self.hub).ok_or_else(unavailable)
    }

    async fn current_receipt(
        &self,
        deadline: Instant,
        permit: OwnedSemaphorePermit,
        armed: Option<ArmedReceiptScope>,
        expected_digest: [u8; 32],
    ) -> Result<(OwnerCreationReceipt, OwnedSemaphorePermit, ArmedReceiptScope), HostError> {
        let scope = Arc::clone(&self.scope);
        // A timed-out or cancelled filesystem read can finish detached, but
        // owns the only permit and never holds a channel or signs anything.
        timeout_at(deadline, tokio::task::spawn_blocking(move || {
            let armed = match armed {
                Some(armed) => armed,
                None => ArmedReceiptScope::read(&scope.remote).map_err(|_| unavailable())?,
            };
            let receipt = scope.load_current(&armed)?;
            if owner_creation_receipt_digest(&receipt)? != expected_digest {
                return Err(unavailable());
            }
            Ok((receipt, permit, armed))
        }))
        .await.map_err(|_| unavailable())?
        .map_err(|_| unavailable())?
    }

    async fn require_active(
        &self,
        deadline: Instant,
        epoch: NonZeroU64,
    ) -> Result<OwnerActivationObservation, HostError> {
        timeout_at(deadline, self.netsec.active_authority(self.scope.owner.as_str()))
            .await.map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
            .filter(|current| current.epoch() == epoch)
            .ok_or_else(unavailable)
    }

    /// Signs only this exact current receipt on a live accepted, independently
    /// pinned Hub connection. A verifier consumes its own fresh nonce and
    /// separately rechecks its authoritative NetSec view before using proof.
    /// The fixed ingress must recheck its listener lifetime and this exact
    /// channel before writing the bounded signature-only response.
    pub(in crate::session_serve_remote) async fn answer(
        &self,
        channel: &OwnerProcessChannelBinding,
        challenge: &OwnerReadinessChallenge,
    ) -> Result<OwnerReadinessResponse, HostError> {
        let deadline = Instant::now() + PROOF_BUDGET;
        let exporter = self.exporter(channel)?;
        if challenge.nonce == [0; 32] || challenge.receipt_digest == [0; 32] {
            return Err(unavailable());
        }
        let permit = Arc::clone(&self.one_in_flight).try_acquire_owned().map_err(|_| unavailable())?;
        let activation = self.require_active(deadline, challenge.netsec_epoch).await?;
        let (receipt, permit, armed) = self.current_receipt(
            deadline, permit, None, challenge.receipt_digest,
        ).await?;
        let payload = challenge.signing_bytes(
            &self.hub, self.hub_key, exporter, activation.authority_instance(),
            receipt.claims(), self.readiness_key,
        )?;
        if self.exporter(channel)? != exporter || self.scope.host.is_draining()
            || Instant::now() >= deadline
        {
            return Err(unavailable());
        }
        let mut signature = vec![0; ML_DSA_65_SIGNATURE_LEN];
        let written = self.key.sign(&payload, &mut signature).map_err(|_| unavailable())?;
        if written != signature.len() {
            return Err(unavailable());
        }
        // Do not return even a valid signature if the producer changed
        // during signing or the operation exceeded its end-to-end budget.
        if self.require_active(deadline, challenge.netsec_epoch).await? != activation {
            return Err(unavailable());
        }
        let (_current, _permit, _armed) = self.current_receipt(
            deadline, permit, Some(armed), challenge.receipt_digest,
        ).await?;
        if self.scope.host.is_draining()
            || self.exporter(channel)? != exporter || Instant::now() >= deadline
        {
            return Err(unavailable());
        }
        Ok(OwnerReadinessResponse { signature })
    }
}

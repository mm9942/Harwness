//! Optional guarded-writer pre-assignment proof client. This is independent
//! of OwnerDirectory, caller grants, placement, the Observe protocol and
//! writer retirement. Neither a receipt nor a signed readiness result
//! creates authority to route a session.

use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{ML_DSA_65, UnparsedPublicKey};
use harw_infra_client::{OwnerActivationObservation, OwnerEpochClient};
use harw_node_transport::{LocalNode, NodeTransportClient, OwnerProcessChannelBinding, full_body};
use harw_session_host::owner_creation::{
    ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, OwnerCreationClaims, owner_creation_key_fingerprint,
};
use harw_session_host::owner_readiness_contract::{
    MAX_OWNER_READINESS_REQUEST_BYTES, MAX_OWNER_READINESS_RESPONSE_BYTES, OWNER_READINESS_PATH,
    OwnerReadinessChallenge, OwnerReadinessResponse, owner_creation_receipt_digest,
};
use http::{Request, StatusCode, header};
use tokio::time::{Instant, timeout_at};

use crate::owner_create_receipt::{
    OwnerCreationObserveKey, OwnerCreationPinScope, VerifiedOwnerCreation,
};
use crate::owner_directory::valid_id;
use crate::owner_report::{json_len, read_json};

const WINDOW: Duration = Duration::from_secs(5);
static IN_FLIGHT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerReadinessError {
    InvalidPin,
    Scope,
    Unavailable,
    InvalidResponse,
    InvalidSignature,
}

impl fmt::Display for OwnerReadinessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidPin => "invalid independent guarded-writer readiness pin",
            Self::Scope => "guarded-writer readiness does not match the expected live scope",
            Self::Unavailable => "guarded-writer readiness unavailable",
            Self::InvalidResponse => "invalid guarded-writer readiness response",
            Self::InvalidSignature => "guarded-writer readiness signature did not verify",
        })
    }
}

impl std::error::Error for OwnerReadinessError {}

/// Independent trusted composition for exactly one writer incarnation.
pub struct OwnerReadinessPin {
    node: String,
    instance: String,
    epoch: NonZeroU64,
    namespace_id: [u8; 32],
    readiness_key: [u8; ML_DSA_65_PUBLIC_KEY_LEN],
    readiness_key_fingerprint: [u8; 32],
    owner_transport_fingerprint: [u8; 32],
    creation_key_fingerprint: [u8; 32],
}

impl OwnerReadinessPin {
    /// Select the exact independent readiness, enrolled transport and
    /// creation keys. Pass a pinned Observe key if there is one; Disabled
    /// requires an independent operator assertion. No pin comes from wire or
    /// a receipt. Different key bytes alone cannot establish key custody.
    pub fn new(
        scope: OwnerCreationPinScope<'_>,
        readiness_key: &[u8],
        owner_transport_key: &[u8],
        creation_key: &[u8],
        observe: OwnerCreationObserveKey<'_>,
    ) -> Result<Self, OwnerReadinessError> {
        let fingerprint = |bytes: &[u8]| {
            owner_creation_key_fingerprint(bytes).ok_or(OwnerReadinessError::InvalidPin)
        };
        let readiness_key_fingerprint = fingerprint(readiness_key)?;
        let owner_transport_fingerprint = fingerprint(owner_transport_key)?;
        let creation_key_fingerprint = fingerprint(creation_key)?;
        let key_ids = [readiness_key_fingerprint, owner_transport_fingerprint, creation_key_fingerprint];
        if scope.node.is_empty() || scope.node.len() > 128 || scope.node.starts_with('.')
            || !scope.node.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || !valid_id(scope.instance)
            || scope.namespace_id == [0; 32]
            || key_ids[0] == key_ids[1] || key_ids[0] == key_ids[2] || key_ids[1] == key_ids[2]
            || match observe {
                OwnerCreationObserveKey::Disabled => false,
                OwnerCreationObserveKey::Pinned(key) => key_ids.contains(&fingerprint(key)?),
            }
        {
            return Err(OwnerReadinessError::InvalidPin);
        }
        Ok(Self {
            node: scope.node.to_owned(),
            instance: scope.instance.to_owned(),
            epoch: scope.epoch,
            namespace_id: scope.namespace_id,
            readiness_key: readiness_key.try_into().map_err(|_| OwnerReadinessError::InvalidPin)?,
            readiness_key_fingerprint,
            owner_transport_fingerprint,
            creation_key_fingerprint,
        })
    }

    fn applies(&self, claims: &OwnerCreationClaims) -> bool {
        claims.node.as_str() == self.node.as_str()
            && claims.instance.as_str() == self.instance.as_str()
            && claims.epoch == self.epoch
            && claims.namespace_id == self.namespace_id
            && claims.key_fingerprint == self.creation_key_fingerprint
    }

    fn exact_binding(
        &self,
        candidate: &NodeTransportClient,
        claims: &OwnerCreationClaims,
    ) -> Result<OwnerProcessChannelBinding, OwnerReadinessError> {
        candidate.owner_process_channel_binding()
            .filter(|binding| binding.is_dialer()
                && binding.peer().node_id == claims.node
                && binding.verified_peer_key_fingerprint() == Some(self.owner_transport_fingerprint))
            .ok_or(OwnerReadinessError::Scope)
    }

    /// Ask for one proof on the already connected, independently selected
    /// owner lane. This never dials, follows a redirect, retries or switches
    /// transports. The supplied Hub identity must be the exact local endpoint
    /// used to establish this channel. Both verifiers must independently
    /// reach the same live NetSec writer: its instance marker is signed but
    /// never sent as an input field on the readiness lane.
    pub async fn query<'a>(
        &'a self,
        hub: &LocalNode,
        client: &mut NodeTransportClient,
        netsec: &'a OwnerEpochClient,
        receipt: &VerifiedOwnerCreation<'_>,
    ) -> Result<VerifiedOwnerReadiness<'a>, OwnerReadinessError> {
        let _permit = IN_FLIGHT.try_acquire().map_err(|_| OwnerReadinessError::Unavailable)?;
        let until = Instant::now() + WINDOW;
        timeout_at(until, async {
            let claims = receipt.claims();
            let hub_fingerprint = owner_creation_key_fingerprint(&hub.identity.public_key)
                .ok_or(OwnerReadinessError::InvalidPin)?;
            if !self.applies(claims)
                || [
                    self.readiness_key_fingerprint, self.owner_transport_fingerprint,
                    self.creation_key_fingerprint,
                ].contains(&hub_fingerprint)
            {
                return Err(OwnerReadinessError::Scope);
            }
            let binding = self.exact_binding(client, claims)?;
            let exporter = binding.export_for_peer(&claims.node).ok_or(OwnerReadinessError::Scope)?;
            let activation = netsec.active_authority(&self.node).await
                .map_err(|_| OwnerReadinessError::Unavailable)?
                .ok_or(OwnerReadinessError::Scope)?;
            let digest = owner_creation_receipt_digest(receipt.receipt())
                .map_err(|_| OwnerReadinessError::Scope)?;
            let mut nonce = [0; 32];
            SystemRandom::new().fill(&mut nonce).map_err(|_| OwnerReadinessError::Unavailable)?;
            let challenge = OwnerReadinessChallenge {
                nonce,
                receipt_digest: digest,
                netsec_epoch: activation.epoch(),
            };
            let transcript = challenge.signing_bytes(
                hub.node_id(), hub_fingerprint, exporter, activation.authority_instance(),
                claims, self.readiness_key_fingerprint,
            ).map_err(|_| OwnerReadinessError::Scope)?;
            let body = serde_json::to_vec(&challenge).map_err(|_| OwnerReadinessError::Scope)?;
            if body.len() > MAX_OWNER_READINESS_REQUEST_BYTES
                || !binding.same_live_channel(&self.exact_binding(client, claims)?)
            {
                return Err(OwnerReadinessError::Scope);
            }
            let message = Request::post(OWNER_READINESS_PATH)
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_LENGTH, body.len().to_string())
                .body(full_body(body)).map_err(|_| OwnerReadinessError::Unavailable)?;
            // A single freshly generated challenge is consumed here, whether
            // it succeeds or fails. No pending challenge can be reused.
            let mut response = client.send_request(message).await
                .map_err(|_| OwnerReadinessError::Unavailable)?;
            if response.status() != StatusCode::OK {
                return Err(OwnerReadinessError::Unavailable);
            }
            let size = json_len(response.headers(), MAX_OWNER_READINESS_RESPONSE_BYTES)
                .map_err(|_| OwnerReadinessError::InvalidResponse)?;
            let response = read_json(response.body_mut(), size, MAX_OWNER_READINESS_RESPONSE_BYTES)
                .await.map_err(|_| OwnerReadinessError::InvalidResponse)?;
            let response: OwnerReadinessResponse = serde_json::from_slice(&response)
                .map_err(|_| OwnerReadinessError::InvalidResponse)?;
            if response.signature.len() != ML_DSA_65_SIGNATURE_LEN {
                return Err(OwnerReadinessError::InvalidResponse);
            }
            UnparsedPublicKey::new(&ML_DSA_65, self.readiness_key.as_slice())
                .verify(&transcript, &response.signature)
                .map_err(|_| OwnerReadinessError::InvalidSignature)?;
            if netsec.active_authority(&self.node).await.ok().flatten() != Some(activation)
                || !binding.same_live_channel(&self.exact_binding(client, claims)?)
                || Instant::now() >= until
            {
                return Err(OwnerReadinessError::Scope);
            }
            Ok(VerifiedOwnerReadiness { pin: self, netsec, binding, activation, digest, until })
        }).await.map_err(|_| OwnerReadinessError::Unavailable)?
    }
}

/// A bounded historical proof plus the exact still-live channel. It is not
/// serializable or cloneable and does not provide assignment authority.
pub struct VerifiedOwnerReadiness<'a> {
    pin: &'a OwnerReadinessPin,
    netsec: &'a OwnerEpochClient,
    binding: OwnerProcessChannelBinding,
    activation: OwnerActivationObservation,
    digest: [u8; 32],
    until: Instant,
}

impl VerifiedOwnerReadiness<'_> {
    /// Consume the observation for the independently selected receipt, and
    /// sample NetSec again. This is not atomic across the NetSec daemon and
    /// any caller's local state. Never use it to assert writer retirement.
    pub async fn confirm_current(
        self,
        exact_receipt: &VerifiedOwnerCreation<'_>,
    ) -> Result<(), OwnerReadinessError> {
        if !self.pin.applies(exact_receipt.claims())
            || owner_creation_receipt_digest(exact_receipt.receipt()).ok() != Some(self.digest)
            || !self.binding.is_live()
            || Instant::now() >= self.until
        {
            return Err(OwnerReadinessError::Scope);
        }
        let _permit = IN_FLIGHT.try_acquire().map_err(|_| OwnerReadinessError::Unavailable)?;
        let activation = timeout_at(self.until, self.netsec.active_authority(&self.pin.node)).await
            .map_err(|_| OwnerReadinessError::Unavailable)?
            .map_err(|_| OwnerReadinessError::Unavailable)?;
        if activation != Some(self.activation) || !self.binding.is_live() || Instant::now() >= self.until {
            return Err(OwnerReadinessError::Scope);
        }
        Ok(())
    }
}

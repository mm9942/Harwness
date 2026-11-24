//! Explicit fixed process-proof channels. No endpoint or pin is derived from
//! a message, and no process key is loaded or deployed by this module.

use std::{future::Future, pin::Pin};
use harw_node_transport::{
    LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeBody, NodeTransportClient,
    OwnerProcessChannelBinding, full_body, node_key_fingerprint,
};
use http::{Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};

use crate::owner_direct_link::{DirectOwnerStatusPlan, OwnerProofEndpoint};
use crate::owner_ingress::HubIngressError;
use crate::owner_placement_listener::{OwnerPlacementProofExchange, OwnerPlacementProofLink, refusal};
use crate::owner_report::{json_len, read_json, request_len};

pub use harw_protocol::owner_placement::OWNER_PROOF_PATH;
pub const OWNER_PRESIGN_PATH: &str = "/v1/hub/owner-placement-presign";
pub(crate) const PROOF_LIMIT: usize = 8192;
pub(crate) const PRESIGN_LIMIT: usize = 256;
pub(crate) const POLICY_LIMIT: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProofRequest {
    pub version: u8,
    pub reservation: [u8; 32],
    pub presign: [u8; 32],
    pub transcript: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresignRequest {
    pub version: u8,
    pub presign: [u8; 32],
}

/// Pinned policy snapshot from the actual Hub pre-sign endpoint, not the
/// outer proof request. It is not an owner-side authorization.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerPresignPolicy {
    pub version: u8,
    pub node: String,
    pub instance: String,
    pub tenant: String,
    pub session: String,
    pub generation: std::num::NonZeroU64,
    pub epoch: std::num::NonZeroU64,
    pub activation: std::num::NonZeroU64,
    pub caller_node: String,
    pub caller_device: String,
    pub caller_key: [u8; 32],
    pub policy_revision: u64,
    pub reservation: [u8; 32],
    pub transcript_digest: [u8; 32],
}

pub(crate) fn transcript_digest(bytes: &[u8]) -> [u8; 32] {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes);
    let mut fixed = [0; 32];
    fixed.copy_from_slice(digest.as_ref());
    fixed
}

/// Concrete outbound exchange from one provisioned Hub identity, only to
/// the operator-configured node/address and sealed key in each exact plan.
pub struct PinnedOwnerPlacementProofExchange {
    local: LocalNode,
    route: ProofRoute,
}

enum ProofRoute {
    Tcp,
    #[cfg(feature = "hub-owner-onion")]
    Onion(std::sync::Arc<harw_onion::OnionClient>),
}

impl PinnedOwnerPlacementProofExchange {
    pub fn new(local: LocalNode) -> Self {
        Self { local, route: ProofRoute::Tcp }
    }

    /// Own the startup-selected Tor client. This instance cannot use TCP,
    /// even when given a mismatched plan or when Tor is unavailable.
    #[cfg(feature = "hub-owner-onion")]
    pub fn new_onion(local: LocalNode, client: std::sync::Arc<harw_onion::OnionClient>) -> Self {
        Self { local, route: ProofRoute::Onion(client) }
    }
}

struct FixedProofLink {
    client: NodeTransportClient,
    channel: OwnerProcessChannelBinding,
}

impl OwnerPlacementProofExchange for PinnedOwnerPlacementProofExchange {
    fn open<'a>(&'a self, plan: &'a DirectOwnerStatusPlan) -> Pin<Box<
        dyn Future<Output = Result<Box<dyn OwnerPlacementProofLink>, HubIngressError>> + Send + 'a,
    >> {
        Box::pin(async move {
            let expected = plan.node_identity();
            if self.local.node_id() == &expected.node_id {
                return Err(HubIngressError::Forbidden);
            }
            let remaining = plan.expires_at().checked_duration_since(std::time::Instant::now())
                .filter(|duration| !duration.is_zero()).ok_or(HubIngressError::Unavailable)?;
            let client = tokio::time::timeout(remaining, async {
                match (&self.route, plan.endpoint()) {
                    (ProofRoute::Tcp, OwnerProofEndpoint::Tcp(address)) => {
                        NodeTransportClient::connect(
                            *address, &self.local, &expected.node_id, plan.verifier(),
                        ).await.map_err(|_| HubIngressError::Unavailable)
                    }
                    #[cfg(feature = "hub-owner-onion")]
                    (ProofRoute::Onion(tor), OwnerProofEndpoint::Onion(locator)) => {
                        let io = tor.dial(locator, remaining).await
                            .map_err(|_| HubIngressError::Unavailable)?;
                        NodeTransportClient::connect_over(
                            io, &self.local, &expected.node_id, plan.verifier(),
                            &harw_node_transport::ClientOptions::default(),
                        ).await.map_err(|_| HubIngressError::Unavailable)
                    }
                    #[cfg(feature = "hub-owner-onion")]
                    _ => Err(HubIngressError::Forbidden),
                }
            }).await.map_err(|_| HubIngressError::Unavailable)??;
            let key = node_key_fingerprint(&expected.public_key).ok_or(HubIngressError::Forbidden)?;
            let channel = client.owner_process_channel_binding()
                .filter(|binding| binding.is_dialer() && binding.verified_peer_key_fingerprint() == Some(key))
                .ok_or(HubIngressError::Unauthenticated)?;
            Ok(Box::new(FixedProofLink { client, channel }) as Box<dyn OwnerPlacementProofLink>)
        })
    }
}

impl OwnerPlacementProofLink for FixedProofLink {
    fn channel(&self) -> &OwnerProcessChannelBinding {
        &self.channel
    }

    fn sign_observe<'a>(
        &'a mut self,
        transcript: &'a [u8],
        reservation: [u8; 32],
        presign: [u8; 32],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, HubIngressError>> + Send + 'a>> {
        Box::pin(async move {
            let request = json_request(OWNER_PROOF_PATH, &ProofRequest {
                version: 1, reservation, presign, transcript: transcript.to_vec(),
            }, PROOF_LIMIT)?;
            let mut response = self.client.send_request(request).await
                .map_err(|_| HubIngressError::Unavailable)?;
            if response.status() != StatusCode::OK {
                return Err(HubIngressError::Forbidden);
            }
            let len = json_len(response.headers(), ML_DSA_65_SIGNATURE_LEN * 4 + 128)?;
            let body = read_json(response.body_mut(), len, ML_DSA_65_SIGNATURE_LEN * 4 + 128).await?;
            let result: ProofResponse = serde_json::from_slice(&body).map_err(|_| HubIngressError::InvalidReport)?;
            if result.version != 1 || result.signature.len() != ML_DSA_65_SIGNATURE_LEN || !self.channel.is_live() {
                return Err(HubIngressError::Forbidden);
            }
            Ok(result.signature)
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofResponse {
    version: u8,
    signature: Vec<u8>,
}

pub(crate) fn json_request<T: Serialize>(path: &'static str, payload: &T, cap: usize)
    -> Result<Request<NodeBody>, HubIngressError>
{
    let bytes = serde_json::to_vec(payload).map_err(|_| HubIngressError::InvalidReport)?;
    if bytes.len() > cap {
        return Err(HubIngressError::BodyLimit);
    }
    Request::post(path)
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .body(full_body(bytes)).map_err(|_| HubIngressError::WrongProtocol)
}

/// An explicitly deployed owner implementation redeems the exact local
/// reservation and acquires policy independently from the pinned Hub.
/// No identity from a request becomes a HostConnection.
pub trait ReservedObserveSigner: Send + Sync {
    fn sign<'a>(
        &'a self,
        channel: &'a OwnerProcessChannelBinding,
        reservation: [u8; 32],
        presign: [u8; 32],
        transcript: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<[u8; ML_DSA_65_SIGNATURE_LEN], HubIngressError>> + Send + 'a>>;
}

/// Fixed owner-side proof handler. Call only inside the actual pinned Hub
/// NodeTransport service, on its accepted side. It installs no listener.
pub async fn owner_observe_proof_response<B>(
    mut request: Request<B>,
    expected_hub: &str,
    pinned_hub_key: [u8; 32],
    signer: &dyn ReservedObserveSigner,
) -> Response<NodeBody>
where
    B: BodyExt + Unpin + Send,
    B::Data: AsRef<[u8]> + Send,
{
    let work = async {
        let channel = harw_node_transport::owner_process_channel_binding_of(&request)
            .filter(|binding| !binding.is_dialer()
                && binding.peer().node_id.as_str() == expected_hub
                && pinned_hub_key != [0; 32]
                && binding.verified_peer_key_fingerprint() == Some(pinned_hub_key))
            .ok_or(HubIngressError::Unauthenticated)?;
        let len = request_len(&request, OWNER_PROOF_PATH, PROOF_LIMIT)?;
        let _permit = SIGN_HTTP_SLOTS.try_acquire().map_err(|_| HubIngressError::Busy)?;
        let body = read_json(request.body_mut(), len, PROOF_LIMIT).await?;
        let proof: ProofRequest = serde_json::from_slice(&body).map_err(|_| HubIngressError::InvalidReport)?;
        if proof.version != 1 || proof.reservation == [0; 32] || proof.presign == [0; 32] {
            return Err(HubIngressError::Forbidden);
        }
        let signature = signer.sign(&channel, proof.reservation, proof.presign, &proof.transcript).await?;
        if channel.verified_peer_key_fingerprint() != Some(pinned_hub_key) {
            return Err(HubIngressError::Unauthenticated);
        }
        serde_json::to_vec(&ProofResponse { version: 1, signature: signature.to_vec() })
            .map_err(|_| HubIngressError::Unavailable)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(4), work).await {
        Ok(Ok(body)) => crate::owner_ingress::placement_json_response(body),
        _ => refusal(),
    }
}

static SIGN_HTTP_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// Actual owner-side bridge to the independently pinned Hub authority and
/// the exact originating local WebSocket. No key provisioning or mount.
#[cfg(feature = "hub-owner-process-custody")]
pub struct PinnedReservedObserveSigner {
    host: harw_session_host::SessionHost,
    custody: crate::owner_process_proof::OwnerProcessCustody,
    hub: std::sync::Arc<crate::owner_placement::OwnerPlacementLookupClient>,
}

#[cfg(feature = "hub-owner-process-custody")]
impl PinnedReservedObserveSigner {
    /// Trusted startup supplies the actual mounted Host and dedicated key.
    pub fn new(
        host: harw_session_host::SessionHost,
        custody: crate::owner_process_proof::OwnerProcessCustody,
        hub: std::sync::Arc<crate::owner_placement::OwnerPlacementLookupClient>,
    ) -> Self {
        Self { host, custody, hub }
    }
}

#[cfg(feature = "hub-owner-process-custody")]
impl ReservedObserveSigner for PinnedReservedObserveSigner {
    fn sign<'a>(
        &'a self,
        channel: &'a OwnerProcessChannelBinding,
        reservation: [u8; 32],
        presign: [u8; 32],
        transcript: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<[u8; ML_DSA_65_SIGNATURE_LEN], HubIngressError>> + Send + 'a>> {
        Box::pin(async move {
            use crate::owner_process_proof::LocalOwnerObservePolicy;
            use crate::route_admission::{CallerGrant, OwnerFence};
            use harw_protocol::owner_placement::OwnerObservationReservationId;
            use harw_session_host::OwnerObservationScope;

            if !self.hub.accepts_hub_channel(channel) {
                return Err(HubIngressError::Unauthenticated);
            }
            let policy = self.hub.fetch_presign_once(presign).await?;
            if policy.reservation != reservation || policy.transcript_digest != transcript_digest(transcript) {
                return Err(HubIngressError::Forbidden);
            }
            let reservation_id = OwnerObservationReservationId::from_bytes(reservation)
                .ok_or(HubIngressError::Forbidden)?;
            let expected = OwnerObservationScope {
                owner_node: &policy.node,
                owner_instance: &policy.instance,
                owner_epoch: policy.epoch,
                session: &policy.session,
                tenant: &policy.tenant,
                caller_node: &policy.caller_node,
                caller_device: &policy.caller_device,
                caller_key_fingerprint: policy.caller_key,
            };
            // No caller identity is constructed. The Host checks the scope
            // against the connection that was actually negotiated by the
            // original device and issued this one-use reservation.
            let claim = self.host.claim_owner_observation_reservation(reservation_id, expected)
                .map_err(|_| HubIngressError::Forbidden)?;
            claim.revalidate().await.map_err(|_| HubIngressError::Forbidden)?;
            let fence = OwnerFence {
                tenant: policy.tenant.clone(), session_id: policy.session.clone(),
                node_id: policy.node, host_instance: policy.instance,
                generation: policy.generation, owner_epoch: policy.epoch.get(),
            };
            let grant = CallerGrant {
                tenant: policy.tenant, session_id: policy.session,
                authenticated_node: policy.caller_node, device_id: policy.caller_device,
                policy_revision: policy.policy_revision, may_observe: true, may_control: false,
            };
            let signature = self.custody.sign_reserved_observe(
                channel, claim.pin(), claim.connection(),
                LocalOwnerObservePolicy {
                    fence: &fence, grant: &grant, active_epoch: policy.activation,
                    caller_key_fingerprint: policy.caller_key, expires_at: claim.expires_at(),
                },
                transcript, reservation,
            ).await.map_err(|_| HubIngressError::Forbidden)?;
            claim.revalidate().await.map_err(|_| HubIngressError::Forbidden)?;
            if !self.hub.accepts_hub_channel(channel) {
                return Err(HubIngressError::Unauthenticated);
            }
            claim.mark_signed().map_err(|_| HubIngressError::Forbidden)?;
            Ok(signature)
        })
    }
}

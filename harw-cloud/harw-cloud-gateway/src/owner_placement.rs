//! Fixed owner-to-Hub read-only placement lookup. The selector is sensitive
//! correlation data, not bearer authority: the Hub still requires the exact
//! independently enrolled owner and the currently authorized direct plan.
//! No listener, provisioning, automatic dial, redirect, or retry.

use std::net::SocketAddr;
#[cfg(feature = "hub-owner-onion")]
use std::sync::Arc;
use std::num::NonZeroU64;
use std::time::Duration;

use harw_node_transport::{
    LocalNode, NodeIdentity, NodeTransportClient, PinnedPeers, TranscriptWrappedVerifier,
    full_body, node_key_fingerprint,
};
use http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use http::{Method, Request, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

pub use harw_protocol::owner_placement::{OwnerPlacementLookupId, OwnerPlacementLookupPair, OwnerPlacementLookupPhase};

use crate::owner_directory::valid_id;
use crate::owner_ingress::HubIngressError;
use crate::owner_report::{OWNER_REPORT_PATH, encode, json_len, read_json};
use crate::route_admission::OwnerFence;

/// Reserved fixed route on an authenticated node-control listener only.
pub const OWNER_PLACEMENT_PATH: &str = "/v1/hub/owner-placement";
pub(crate) const REQUEST_LIMIT: usize = 256;
pub(crate) const RESPONSE_LIMIT: usize = 2 * 1024;
const REQUEST_SCHEMA: &str = "harw.owner-placement-lookup";
const RESPONSE_SCHEMA: &str = "harw.owner-placement-current";
pub(crate) const BODY_DEADLINE: Duration = Duration::from_secs(2);
const CLIENT_DEADLINE: Duration = Duration::from_secs(3);
static CLIENT_SLOTS: Semaphore = Semaphore::const_new(16);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupRequest {
    schema: String,
    version: u8,
    id: [u8; 32],
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<OwnerPlacementLookupId, HubIngressError> {
    let request: LookupRequest = serde_json::from_slice(bytes).map_err(|_| HubIngressError::InvalidReport)?;
    if request.schema != REQUEST_SCHEMA || request.version != 1 {
        return Err(HubIngressError::WrongProtocol);
    }
    OwnerPlacementLookupId::from_bytes(request.id).ok_or(HubIngressError::Forbidden)
}

/// Exact read-only owner placement. Never conveys authorization or renews
/// the original device's independently authenticated status read.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerPlacementLookup {
    schema: String,
    version: u8,
    pub tenant: String,
    pub session: String,
    pub node: String,
    pub instance: String,
    pub generation: NonZeroU64,
    pub owner_epoch: NonZeroU64,
    pub activation_epoch: NonZeroU64,
    pub phase: OwnerPlacementLookupPhase,
    pub read_correlation: [u8; 32],
    pub reservation_id: [u8; 32],
    pub caller_node: String,
    pub caller_device: String,
    pub caller_key_fingerprint: [u8; 32],
}

impl OwnerPlacementLookup {
    pub(crate) fn from_fence(
        fence: &OwnerFence,
        activation_epoch: NonZeroU64,
        phase: OwnerPlacementLookupPhase,
        read_correlation: [u8; 32],
        reservation_id: Option<[u8; 32]>,
        caller_node: &str,
        caller_device: &str,
        caller_key_fingerprint: [u8; 32],
    ) -> Result<Self, HubIngressError> {
        Ok(Self {
            schema: RESPONSE_SCHEMA.to_owned(),
            version: if reservation_id.is_some() { 2 } else { 1 },
            tenant: fence.tenant.clone(),
            session: fence.session_id.clone(),
            node: fence.node_id.clone(),
            instance: fence.host_instance.clone(),
            generation: fence.generation,
            owner_epoch: NonZeroU64::new(fence.owner_epoch).ok_or(HubIngressError::Forbidden)?,
            activation_epoch,
            phase,
            read_correlation,
            reservation_id: reservation_id.unwrap_or([0; 32]),
            caller_node: caller_node.to_owned(),
            caller_device: caller_device.to_owned(),
            caller_key_fingerprint,
        })
    }

    fn valid_for(&self, local: &LocalNode) -> bool {
        self.schema == RESPONSE_SCHEMA && self.version == 2 && self.reservation_id != [0; 32]
            && self.node == local.node_id().as_str()
            && self.read_correlation != [0; 32] && self.caller_key_fingerprint != [0; 32]
            && [&self.tenant, &self.session, &self.instance, &self.node, &self.caller_node, &self.caller_device]
                .into_iter().all(|field| valid_id(field))
    }
}

/// Handoff errors deliberately omit received response bodies and lookup IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerPlacementLookupError {
    Configuration,
    Unavailable,
    Denied,
    Deadline,
    WrongProtocol,
}

impl std::fmt::Display for OwnerPlacementLookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Configuration => "invalid pinned Hub placement configuration",
            Self::Unavailable => "Hub placement authority is unavailable",
            Self::Denied => "Hub placement lookup was denied",
            Self::Deadline => "Hub placement lookup deadline expired",
            Self::WrongProtocol => "invalid Hub placement response",
        })
    }
}

impl std::error::Error for OwnerPlacementLookupError {}

/// One startup-selected Hub transport. These routes are mutually exclusive:
/// errors never attempt the other route, DNS or a different destination.
enum HubPlacementTransport {
    LoopbackLab(SocketAddr),
    #[cfg(feature = "hub-owner-onion")]
    Onion {
        client: Arc<harw_onion::OnionClient>,
        locator: harw_onion::OnionLocator,
    },
}

/// A one-route client with an independently provisioned exact Hub key.
pub struct OwnerPlacementLookupClient {
    transport: HubPlacementTransport,
    local: LocalNode,
    hub: NodeIdentity,
    hub_key: [u8; 32],
    verifier: TranscriptWrappedVerifier<PinnedPeers>,
}

impl OwnerPlacementLookupClient {
    /// Explicit loopback laboratory placement. Never permits a remote IP.
    pub fn new_loopback(
        address: SocketAddr,
        local: LocalNode,
        hub: NodeIdentity,
    ) -> Result<Self, OwnerPlacementLookupError> {
        if address.port() == 0 || !address.ip().is_loopback() {
            return Err(OwnerPlacementLookupError::Configuration);
        }
        Self::with_transport(HubPlacementTransport::LoopbackLab(address), local, hub)
    }

    /// Pin the exact independently selected Hub key over one Arti onion
    /// service. This constructor neither opens a stream nor uses a ticket.
    #[cfg(feature = "hub-owner-onion")]
    pub fn new_onion(
        client: Arc<harw_onion::OnionClient>,
        locator: harw_onion::OnionLocator,
        local: LocalNode,
        hub: NodeIdentity,
    ) -> Result<Self, OwnerPlacementLookupError> {
        Self::with_transport(HubPlacementTransport::Onion { client, locator }, local, hub)
    }

    fn with_transport(
        transport: HubPlacementTransport,
        local: LocalNode,
        hub: NodeIdentity,
    ) -> Result<Self, OwnerPlacementLookupError> {
        let hub_key = node_key_fingerprint(&hub.public_key).ok_or(OwnerPlacementLookupError::Configuration)?;
        if !valid_id(hub.node_id.as_str())
            || !valid_id(local.node_id().as_str())
            || local.node_id() == &hub.node_id
            || node_key_fingerprint(&local.identity.public_key).is_none_or(|key| key == hub_key)
        {
            return Err(OwnerPlacementLookupError::Configuration);
        }
        let mut selected = PinnedPeers::new();
        selected.pin_identity(&hub).map_err(|_| OwnerPlacementLookupError::Configuration)?;
        Ok(Self { transport, local, hub, hub_key, verifier: TranscriptWrappedVerifier::new(selected) })
    }

    async fn connect_once(&self) -> Result<NodeTransportClient, OwnerPlacementLookupError> {
        match &self.transport {
            HubPlacementTransport::LoopbackLab(address) => NodeTransportClient::connect(
                *address, &self.local, &self.hub.node_id, &self.verifier,
            ).await.map_err(|_| OwnerPlacementLookupError::Unavailable),
            #[cfg(feature = "hub-owner-onion")]
            HubPlacementTransport::Onion { client, locator } => {
                // Never spend an authenticated proof window waiting for a
                // cold bootstrap. Readiness is not an auth or latency proof.
                if !client.progress().ready {
                    return Err(OwnerPlacementLookupError::Unavailable);
                }
                let stream = client.dial(locator, CLIENT_DEADLINE).await
                    .map_err(|_| OwnerPlacementLookupError::Unavailable)?;
                NodeTransportClient::connect_over(
                    stream, &self.local, &self.hub.node_id, &self.verifier,
                    &harw_node_transport::ClientOptions::default(),
                ).await.map_err(|_| OwnerPlacementLookupError::Unavailable)
            }
        }
    }

    #[cfg(all(feature = "hub-owner-placement-listener", feature = "hub-owner-process-custody"))]
    pub(crate) fn accepts_hub_channel(&self, channel: &harw_node_transport::OwnerProcessChannelBinding) -> bool {
        !channel.is_dialer() && channel.peer().node_id == self.hub.node_id
            && channel.verified_peer_key_fingerprint() == Some(self.hub_key)
    }

    #[cfg(all(feature = "hub-owner-placement-listener", feature = "hub-owner-process-custody"))]
    pub(crate) async fn fetch_presign_once(
        &self,
        selector: [u8; 32],
    ) -> Result<crate::owner_proof_exchange::OwnerPresignPolicy, HubIngressError> {
        use crate::owner_proof_exchange::{
            OWNER_PRESIGN_PATH, POLICY_LIMIT, PRESIGN_LIMIT, OwnerPresignPolicy, PresignRequest, json_request,
        };
        let _permit = CLIENT_SLOTS.try_acquire().map_err(|_| HubIngressError::Busy)?;
        let attempt = async {
            if selector == [0; 32] { return Err(HubIngressError::Forbidden); }
            let mut client = self.connect_once().await.map_err(|_| HubIngressError::Unavailable)?;
            let binding = client.owner_process_channel_binding()
                .filter(|binding| binding.is_dialer()
                    && binding.verified_peer_key_fingerprint() == Some(self.hub_key))
                .ok_or(HubIngressError::Unauthenticated)?;
            let request = json_request(OWNER_PRESIGN_PATH, &PresignRequest { version: 1, presign: selector }, PRESIGN_LIMIT)?;
            let mut response = client.send_request(request).await.map_err(|_| HubIngressError::Unavailable)?;
            if response.status() != StatusCode::OK { return Err(HubIngressError::Forbidden); }
            let len = json_len(response.headers(), POLICY_LIMIT)?;
            let bytes = read_json(response.body_mut(), len, POLICY_LIMIT).await?;
            let policy: OwnerPresignPolicy = serde_json::from_slice(&bytes)
                .map_err(|_| HubIngressError::InvalidReport)?;
            if policy.version != 1 || policy.node != self.local.node_id().as_str()
                || policy.caller_key == [0; 32] || policy.reservation == [0; 32]
                || ![&policy.node, &policy.instance, &policy.tenant, &policy.session, &policy.caller_node, &policy.caller_device]
                    .into_iter().all(|value| valid_id(value))
                || binding.verified_peer_key_fingerprint() != Some(self.hub_key)
            {
                return Err(HubIngressError::Forbidden);
            }
            Ok(policy)
        };
        tokio::time::timeout(CLIENT_DEADLINE, attempt).await.map_err(|_| HubIngressError::Expired)?
    }

    /// Offer one exact locally confirmed hosted assignment to the pinned
    /// Hub. The Hub independently compares it with the authoritative
    /// directory, owner transport key and current NetSec activation. This
    /// publishes no ownership, transcript, listener route or device grant.
    /// Revalidate local placement after connection and before sending.
    ///
    /// # Errors
    /// An invalid, withdrawn, busy, unavailable, refused or timed-out
    /// report never retries or selects a second transport.
    pub async fn publish_owner_report_once<V>(
        &self,
        fence: &OwnerFence,
        authorize: impl FnOnce() -> V,
    ) -> Result<(), OwnerPlacementLookupError>
    where
        V: std::future::Future<Output = bool>,
    {
        static REPORT_SLOTS: Semaphore = Semaphore::const_new(4);
        let _permit = REPORT_SLOTS.try_acquire().map_err(|_| OwnerPlacementLookupError::Unavailable)?;
        if fence.owner_epoch == 0 || fence.node_id != self.local.node_id().as_str() {
            return Err(OwnerPlacementLookupError::Denied);
        }
        let payload = encode(fence).map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut client = self.connect_once().await?;
            let binding = client.owner_process_channel_binding()
                .filter(|binding| binding.is_dialer()
                    && binding.verified_peer_key_fingerprint() == Some(self.hub_key))
                .ok_or(OwnerPlacementLookupError::Denied)?;
            if !authorize().await || binding.verified_peer_key_fingerprint() != Some(self.hub_key) {
                return Err(OwnerPlacementLookupError::Denied);
            }
            let request = Request::builder().method(Method::POST).uri(OWNER_REPORT_PATH)
                .header(HOST, "localhost")
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_LENGTH, payload.len().to_string())
                .body(full_body(payload)).map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            let response = client.send_request(request).await
                .map_err(|_| OwnerPlacementLookupError::Unavailable)?;
            if response.status() != StatusCode::NO_CONTENT
                || binding.verified_peer_key_fingerprint() != Some(self.hub_key)
            {
                return Err(OwnerPlacementLookupError::Denied);
            }
            Ok(())
        }).await.map_err(|_| OwnerPlacementLookupError::Deadline)?
    }

    /// One bounded attempt using one fixed path to one authenticated Hub.
    /// A failure or ambiguous result spends this local ticket; no retries,
    /// cached placement, alternate peer, or response-directed actions.
    pub async fn lookup_once(
        &self,
        ticket: OwnerPlacementLookupId,
    ) -> Result<OwnerPlacementLookup, OwnerPlacementLookupError> {
        let _permit = CLIENT_SLOTS.try_acquire().map_err(|_| OwnerPlacementLookupError::Unavailable)?;
        tokio::time::timeout(CLIENT_DEADLINE, async {
            let mut client = self.connect_once().await?;
            let binding = client.owner_process_channel_binding()
                .filter(|binding| binding.is_dialer()
                    && binding.verified_peer_key_fingerprint() == Some(self.hub_key))
                .ok_or(OwnerPlacementLookupError::Denied)?;
            let payload = serde_json::to_vec(&LookupRequest {
                schema: REQUEST_SCHEMA.to_owned(), version: 1, id: *ticket.as_bytes(),
            }).map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            if payload.len() > REQUEST_LIMIT {
                return Err(OwnerPlacementLookupError::WrongProtocol);
            }
            let request = Request::builder().method(Method::POST).uri(OWNER_PLACEMENT_PATH)
                .header(HOST, "localhost")
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_LENGTH, payload.len().to_string())
                .body(full_body(payload)).map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            let mut response = client.send_request(request).await
                .map_err(|_| OwnerPlacementLookupError::Unavailable)?;
            if response.status() != StatusCode::OK {
                return Err(OwnerPlacementLookupError::Denied);
            }
            let length = json_len(response.headers(), RESPONSE_LIMIT).map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            let bytes = read_json(response.body_mut(), length, RESPONSE_LIMIT).await
                .map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            let result: OwnerPlacementLookup = serde_json::from_slice(&bytes)
                .map_err(|_| OwnerPlacementLookupError::WrongProtocol)?;
            if !result.valid_for(&self.local)
                || binding.verified_peer_key_fingerprint() != Some(self.hub_key)
            {
                return Err(OwnerPlacementLookupError::Denied);
            }
            Ok(result)
        }).await.map_err(|_| OwnerPlacementLookupError::Deadline)?
    }
}


#[cfg(feature = "hub-owner-local-host")]
impl harw_session_host::DirectOwnerPlacementAuthority for OwnerPlacementLookupClient {
    fn lookup(
        &self,
        id: OwnerPlacementLookupId,
    ) -> harw_protocol::PortFuture<'_, harw_session_host::CurrentOwnerPlacement> {
        Box::pin(async move {
            let unavailable = || harw_protocol::PortError::Denied("current owner placement unavailable".into());
            let current = self.lookup_once(id).await.map_err(|_| unavailable())?;
            Ok(harw_session_host::CurrentOwnerPlacement {
                tenant: current.tenant.try_into().map_err(|_| unavailable())?,
                session: current.session.try_into().map_err(|_| unavailable())?,
                owner_node: current.node.try_into().map_err(|_| unavailable())?,
                owner_instance: current.instance,
                owner_epoch: current.owner_epoch,
                placement_generation: current.generation,
                activation_epoch: current.activation_epoch,
                caller_node: current.caller_node.try_into().map_err(|_| unavailable())?,
                caller_device: current.caller_device.try_into().map_err(|_| unavailable())?,
                caller_key_fingerprint: current.caller_key_fingerprint,
                read_correlation: current.read_correlation,
                reservation_id: current.reservation_id,
                phase: current.phase,
            })
        })
    }
}

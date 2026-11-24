//! Explicit client composition for one Root-Hub-fenced owner status read.
//!
//! Open the independently pinned owner WebSocket first and reserve one exact
//! session there. Present that reservation to the separately pinned Hub,
//! then finish on the original owner connection. No pair or reservation can
//! choose a locator, install a pin or grant permission.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;

use harw_node_transport::{
    ClientOptions, LocalNode, NodeSigner, NodeTransportClient, NodeVerifier,
    OwnerProcessChannelBinding, PinnedPeers, SignFuture, SignerError, full_body,
    node_key_fingerprint, wrap_transcript,
};
use harw_protocol::methods::{
    METHOD_DIRECT_OWNER_STATUS_FENCED, METHOD_RESERVE_OWNER_OBSERVATION,
};
use harw_protocol::owner_placement::{
    DirectOwnerStatusExpectation, FencedDirectOwnerStatus, OwnerObservationReservationId,
    OwnerObservationReservationReceipt, OwnerPlacementLookupId, OwnerPlacementLookupPair,
};
use harw_protocol::session_wire::{
    DIRECT_OWNER_STATUS_WIRE_MINOR, OwnerObservationStatus, SessionRef, features,
};
use harw_protocol::{ClientCaps, PortError};
use harw_types::{NodeId, SessionId};
use http::{Request, StatusCode, header};
use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Instant;

use crate::conn::{Effect, bounded, upgrade_node};
use crate::{ConnectOptions, NodeEndpoint, RemoteConnection};

const HUB_PAIR_PATH: &str = "/v1/hub/direct-owner-placement-pair";
const MAX_HUB_REQUEST: usize = 512;
const MAX_HUB_RESPONSE: usize = 2 * 1024;
const MAX_HUB_ID: usize = 255;
const HUB_DEADLINE: Duration = Duration::from_secs(6);
const OWNER_DEADLINE: Duration = Duration::from_secs(4);
// Conservative local stop, measured before the owner receives the reserve
// RPC. It is not an authority: the original owner enforces its own deadline.
const RESERVATION_LIFETIME: Duration = Duration::from_secs(10);

fn unavailable() -> PortError {
    PortError::Denied("owner observation unavailable".into())
}

fn transport_error() -> PortError {
    PortError::Transport("fenced owner observation connection failed".into())
}

fn invalid_response() -> PortError {
    PortError::Protocol("invalid fenced owner observation response".into())
}

#[derive(Clone, PartialEq, Eq)]
struct DeviceBinding {
    node: NodeId,
    key_fingerprint: [u8; 32],
}

/// Locally provisioned device credential for these two independent links.
/// Both dials additionally prove possession of the exact configured local
/// key, even if the configured signer is backed by a rotating key service.
pub struct OwnerStatusDevice {
    local: LocalNode,
    binding: DeviceBinding,
}

struct MatchingLocalSigner {
    delegate: Arc<dyn NodeSigner>,
    node: NodeId,
    key: PinnedPeers,
}

impl NodeSigner for MatchingLocalSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        Box::pin(async move {
            let signature = self.delegate.sign(transcript).await?;
            // Support the two existing NodeTransport signing profiles. This
            // verifies actual handshake output, not the caller's key label.
            if self.key.verify(&self.node, transcript, &signature).is_ok()
                || wrap_transcript(transcript).is_ok_and(|wrapped| {
                    self.key.verify(&self.node, &wrapped, &signature).is_ok()
                })
            {
                Ok(signature)
            } else {
                Err(SignerError::new("local node signature did not match its configured key"))
            }
        })
    }
}

impl OwnerStatusDevice {
    /// Bind the supplied local enrollment key, not any selector from a Hub
    /// response. Remote enrollments and the effective device are still
    /// verified by the two peers.
    ///
    /// # Errors
    /// Refuses an invalid local key description.
    pub fn from_enrolled_node(local: &LocalNode) -> Result<Self, PortError> {
        let key_fingerprint = node_key_fingerprint(&local.identity.public_key)
            .ok_or_else(unavailable)?;
        let node = local.node_id().clone();
        let mut key = PinnedPeers::new();
        key.pin_identity(&local.identity).map_err(|_| unavailable())?;
        let signer = Arc::new(MatchingLocalSigner {
            delegate: Arc::clone(&local.signer),
            node: node.clone(),
            key,
        });
        Ok(Self {
            local: LocalNode::new(local.identity.clone(), signer),
            binding: DeviceBinding { node, key_fingerprint },
        })
    }

    /// Connect to the Root Hub at one explicitly configured TCP locator.
    ///
    /// # Errors
    /// Returns an error if the exact configured peer cannot be authenticated.
    pub async fn connect_hub(
        &self,
        endpoint: NodeEndpoint,
        verifier: &dyn NodeVerifier,
        options: &ClientOptions,
    ) -> Result<OwnerStatusHubLink, PortError> {
        let client = NodeTransportClient::connect_with(
            endpoint.addr, &self.local, &endpoint.expected_node, verifier, options,
        ).await.map_err(|_| transport_error())?;
        self.hub_link(client, &endpoint.expected_node)
    }

    /// Connect to the configured Root Hub over a specifically selected
    /// stream, such as an Onion stream. No alternate route is attempted.
    ///
    /// # Errors
    /// Returns an error if the exact configured peer cannot be authenticated.
    pub async fn connect_hub_over<I>(
        &self,
        io: I,
        expected_hub: &NodeId,
        verifier: &dyn NodeVerifier,
        options: &ClientOptions,
    ) -> Result<OwnerStatusHubLink, PortError>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let client = NodeTransportClient::connect_over(
            io, &self.local, expected_hub, verifier, options,
        ).await.map_err(|_| transport_error())?;
        self.hub_link(client, expected_hub)
    }

    /// Connect directly to one independently configured owner at its TCP
    /// locator. Does not use an endpoint or key obtained from the Hub reply.
    ///
    /// # Errors
    /// Returns an error on authentication, negotiation or connection failure.
    pub async fn connect_owner(
        &self,
        endpoint: NodeEndpoint,
        verifier: &dyn NodeVerifier,
        mut options: ConnectOptions,
    ) -> Result<FencedOwnerStatusLink, PortError> {
        configure_owner_status(&mut options);
        let attempt = async {
            let client = NodeTransportClient::connect(
                endpoint.addr, &self.local, &endpoint.expected_node, verifier,
            ).await.map_err(|_| crate::RemoteError::Connect("owner authentication failed".into()))?;
            verified_channel(&client, &endpoint.expected_node)
                .map_err(|_| crate::RemoteError::Connect("owner pin is not verified".into()))?;
            upgrade_node(client, &options).await
        };
        let connection = bounded(options.limits.hello_timeout, attempt)
            .await.map_err(|_| transport_error())?;
        self.owner_link(connection, endpoint.expected_node)
    }

    /// Establish the direct owner link over one selected stream (including
    /// Onion) using one independently configured owner key; never fall back.
    ///
    /// # Errors
    /// Returns an error on authentication, negotiation or connection failure.
    pub async fn connect_owner_over<I>(
        &self,
        io: I,
        expected_owner: &NodeId,
        verifier: &dyn NodeVerifier,
        mut options: ConnectOptions,
        transport: &ClientOptions,
    ) -> Result<FencedOwnerStatusLink, PortError>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        configure_owner_status(&mut options);
        let attempt = async {
            let client = NodeTransportClient::connect_over(
                io, &self.local, expected_owner, verifier, transport,
            ).await.map_err(|_| crate::RemoteError::Connect("owner authentication failed".into()))?;
            verified_channel(&client, expected_owner)
                .map_err(|_| crate::RemoteError::Connect("owner pin is not verified".into()))?;
            upgrade_node(client, &options).await
        };
        let connection = bounded(options.limits.hello_timeout, attempt)
            .await.map_err(|_| transport_error())?;
        self.owner_link(connection, expected_owner.clone())
    }

    fn hub_link(
        &self,
        client: NodeTransportClient,
        expected_hub: &NodeId,
    ) -> Result<OwnerStatusHubLink, PortError> {
        let channel = verified_channel(&client, expected_hub)?;
        Ok(OwnerStatusHubLink {
            client,
            channel,
            device: self.binding.clone(),
            usable: true,
        })
    }

    fn owner_link(
        &self,
        connection: RemoteConnection,
        owner_node: NodeId,
    ) -> Result<FencedOwnerStatusLink, PortError> {
        let ack = connection.hello_ack();
        if ack.wire_minor < DIRECT_OWNER_STATUS_WIRE_MINOR
            || ack.host_epoch == 0
            || ack.granted != ClientCaps::OBSERVE
            || !ack.features.iter().any(|feature| feature == features::DIRECT_OWNER_STATUS_FENCED_V1)
        {
            return Err(unavailable());
        }
        connection.require_live().map_err(|_| transport_error())?;
        Ok(FencedOwnerStatusLink {
            connection,
            owner_node,
            device: self.binding.clone(),
            usable: true,
        })
    }
}

fn configure_owner_status(options: &mut ConnectOptions) {
    // Unlike the generic connector, this connection is scoped to one
    // explicit purpose. It never offers unplaced direct-owner V1 or control.
    options.features = vec![features::DIRECT_OWNER_STATUS_FENCED_V1.into()];
    options.requested_caps = Some(ClientCaps::OBSERVE);
}

fn verified_channel(
    client: &NodeTransportClient,
    expected_peer: &NodeId,
) -> Result<OwnerProcessChannelBinding, PortError> {
    client.owner_process_channel_binding().filter(|channel| {
        channel.is_dialer()
            && &channel.peer().node_id == expected_peer
            && channel.verified_peer_key_fingerprint().is_some()
    }).ok_or_else(unavailable)
}

/// Authenticated, non-upgraded link to one configured Root Hub.
pub struct OwnerStatusHubLink {
    client: NodeTransportClient,
    channel: OwnerProcessChannelBinding,
    device: DeviceBinding,
    usable: bool,
}

#[derive(Serialize)]
struct HubPairRequest<'a> {
    schema: &'static str,
    version: u8,
    session: &'a SessionId,
    reservation: &'a [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HubPairResponse {
    schema: String,
    version: u8,
    reservation: [u8; 32],
    session: SessionId,
    node: NodeId,
    instance: String,
    generation: NonZeroU64,
    owner_epoch: NonZeroU64,
    before: [u8; 32],
    after: [u8; 32],
}

fn valid_hub_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_HUB_ID
        && !value.chars().any(char::is_control)
}

fn response_json_length(headers: &http::HeaderMap) -> Result<usize, PortError> {
    let types = headers.get_all(header::CONTENT_TYPE);
    let mut types = types.iter();
    if !types.next().is_some_and(|value| value.as_bytes() == b"application/json")
        || types.next().is_some()
        || headers.contains_key(header::CONTENT_ENCODING)
        || headers.contains_key(header::TRANSFER_ENCODING)
    {
        return Err(invalid_response());
    }
    let lengths = headers.get_all(header::CONTENT_LENGTH);
    let mut lengths = lengths.iter();
    let raw = lengths.next().and_then(|value| value.to_str().ok())
        .ok_or_else(invalid_response)?;
    if lengths.next().is_some() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_response());
    }
    raw.parse::<usize>().ok()
        .filter(|length| (1..=MAX_HUB_RESPONSE).contains(length))
        .ok_or_else(invalid_response)
}

impl OwnerStatusHubLink {
    // Only the original owner-bound handle can present a reservation here.
    async fn issue_pair(
        &mut self,
        session: &SessionId,
        reservation: &OwnerObservationReservationId,
        deadline: Instant,
    ) -> Result<HubPairResponse, PortError> {
        if !self.usable || Instant::now() >= deadline {
            return Err(unavailable());
        }
        self.require_live()?;
        let body = serde_json::to_vec(&HubPairRequest {
            schema: "harw.owner-placement-pair-request",
            version: 2,
            session,
            reservation: reservation.as_bytes(),
        }).map_err(|_| invalid_response())?;
        if body.len() > MAX_HUB_REQUEST {
            return Err(unavailable());
        }
        let request = Request::post(HUB_PAIR_PATH)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len().to_string())
            .body(full_body(body))
            .map_err(|_| invalid_response())?;

        // Cancellation does not demonstrate that the request did not reach
        // the Hub. Retire this client link rather than replaying the fetch.
        self.usable = false;
        let response = tokio::time::timeout_at(deadline.min(Instant::now() + HUB_DEADLINE), async {
            let response = self.client.send_request(request).await.map_err(|_| transport_error())?;
            if response.status() != StatusCode::OK {
                return Err(unavailable());
            }
            let length = response_json_length(response.headers())?;

            let mut body = response.into_body();
            let mut bytes = Vec::with_capacity(length);
            while let Some(frame) = body.frame().await {
                let data = frame.map_err(|_| transport_error())?
                    .into_data().map_err(|_| invalid_response())?;
                if data.len() > length - bytes.len() {
                    return Err(invalid_response());
                }
                bytes.extend_from_slice(&data);
            }
            if bytes.len() != length {
                return Err(invalid_response());
            }
            serde_json::from_slice::<HubPairResponse>(&bytes).map_err(|_| invalid_response())
        }).await.map_err(|_| transport_error())??;
        self.require_live()?;

        if response.schema != "harw.owner-placement-pair" || response.version != 2
            || &response.reservation != reservation.as_bytes()
            || &response.session != session || !valid_hub_id(response.node.as_str())
        {
            return Err(invalid_response());
        }
        Ok(response)
    }

    fn require_live(&self) -> Result<(), PortError> {
        let current = self.client.owner_process_channel_binding().ok_or_else(transport_error)?;
        if !self.channel.same_live_channel(&current) {
            return Err(transport_error());
        }
        Ok(())
    }
}

/// An independently authenticated, live owner WebSocket negotiated just
/// for fenced status. Not a generic SessionPort or a source of attach rights.
pub struct FencedOwnerStatusLink {
    connection: RemoteConnection,
    owner_node: NodeId,
    device: DeviceBinding,
    usable: bool,
}

impl FencedOwnerStatusLink {
    /// Reserve one exact session on the original, already-authenticated
    /// owner WS. The resulting value cannot be replayed or moved to another
    /// owner connection, including a reconnect with the same profile.
    ///
    /// # Errors
    /// Refuses a bad session selector, dead link, owner refusal, or an
    /// indeterminate operation. Never retries or dials another connection.
    pub async fn reserve(
        &mut self,
        session: SessionId,
    ) -> Result<ReservedOwnerStatus<'_>, PortError> {
        if !self.usable || !valid_hub_id(session.as_str()) {
            return Err(unavailable());
        }
        self.require_live()?;
        let deadline = Instant::now() + RESERVATION_LIFETIME;
        let params = SessionRef { session_id: session };
        // Keep this link retired until a definitive result. In particular,
        // dropping the reservation does not permit replacing it on this link.
        self.usable = false;
        let result = tokio::time::timeout(OWNER_DEADLINE, self.connection.call::<_, OwnerObservationReservationReceipt>(
            METHOD_RESERVE_OWNER_OBSERVATION, &params, Effect::None,
        )).await.map_err(|_| transport_error())?;
        self.require_live()?;
        let receipt = owner_rpc_result(result, &mut self.usable)?;
        let reservation = receipt.into_id().ok_or_else(invalid_response)?;
        Ok(ReservedOwnerStatus {
            owner: self, session: params.session_id, reservation, deadline,
        })
    }

    fn require_live(&self) -> Result<(), PortError> {
        self.connection.require_live().map_err(|_| transport_error())
    }
}

fn owner_rpc_result<T>(
    result: Result<T, PortError>,
    usable: &mut bool,
) -> Result<T, PortError> {
    match result {
        Ok(value) => Ok(value),
        Err(PortError::Denied(_) | PortError::NotFound | PortError::Busy(_)) => {
            *usable = true;
            Err(unavailable())
        }
        Err(PortError::Revoked) => Err(unavailable()),
        Err(PortError::Transport(_)) => Err(transport_error()),
        // Never echo protocol detail, the peer's raw response or IDs.
        Err(_) => Err(invalid_response()),
    }
}

/// One private reservation on the exact live owner WebSocket. There is
/// no raw constructor, cloning, debug formatting, logging or serialization.
pub struct ReservedOwnerStatus<'owner> {
    owner: &'owner mut FencedOwnerStatusLink,
    session: SessionId,
    reservation: OwnerObservationReservationId,
    deadline: Instant,
}

impl ReservedOwnerStatus<'_> {
    /// Present this reservation to the pinned Hub and finish once on the
    /// original owner link. Both links remain borrowed until result or error.
    /// The response is a point-in-time status, not authorization to attach,
    /// steer, or retry a turn.
    ///
    /// # Errors
    /// Refuses different devices, dead or mismatched peers, expiry, and
    /// malformed, denied or indeterminate results without a fallback.
    pub async fn read(self, hub: &mut OwnerStatusHubLink) -> Result<OwnerObservationStatus, PortError> {
        let Self { owner, session, reservation, deadline } = self;
        if owner.device != hub.device || Instant::now() >= deadline {
            return Err(unavailable());
        }
        owner.require_live()?;
        hub.require_live()?;

        let response = hub.issue_pair(&session, &reservation, deadline).await?;
        owner.require_live()?;
        hub.require_live()?;
        if response.node != owner.owner_node
            || response.owner_epoch.get() != owner.connection.hello_ack().host_epoch
            || Instant::now() >= deadline
        {
            return Err(unavailable());
        }
        let before = OwnerPlacementLookupId::from_bytes(response.before)
            .ok_or_else(invalid_response)?;
        let after = OwnerPlacementLookupId::from_bytes(response.after)
            .ok_or_else(invalid_response)?;
        let pair = OwnerPlacementLookupPair::new(before, after).ok_or_else(invalid_response)?;
        let wire = FencedDirectOwnerStatus::new(DirectOwnerStatusExpectation {
            session_id: session,
            owner_instance: response.instance,
            placement_generation: response.generation,
            owner_epoch: response.owner_epoch,
        }, pair, reservation).ok_or_else(invalid_response)?;

        let result = tokio::time::timeout_at(
            deadline.min(Instant::now() + OWNER_DEADLINE),
            owner.connection.call::<_, OwnerObservationStatus>(
                METHOD_DIRECT_OWNER_STATUS_FENCED, &wire, Effect::None,
            ),
        ).await.map_err(|_| transport_error())?;
        owner.require_live()?;
        hub.require_live()?;
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        let status = owner_rpc_result(result, &mut owner.usable);
        if status.is_ok() {
            owner.usable = true;
        }
        // Neither side is reusable after an ambiguous operation. A known
        // negative result also requires a fresh explicit reservation.
        hub.usable = owner.usable;
        status
    }
}

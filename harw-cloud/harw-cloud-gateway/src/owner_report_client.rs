//! One explicit owner observation over a configured, pinned Root-Hub link.
//! No endpoint resolution, redirect, retry, scheduling or service mounting.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use harw_node_transport::{
    LocalNode, ML_DSA_65_PUBLIC_KEY_LEN, NodeIdentity, NodeTransportClient, PinnedPeers,
    TranscriptWrappedVerifier, full_body,
};
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{Method, Request, StatusCode};

use crate::owner_directory::valid_id;
use crate::owner_report::{OWNER_REPORT_PATH, encode};
use crate::route_admission::OwnerFence;

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerReportPublishError {
    InvalidConfiguration,
    InvalidFence,
    TransportUnavailable,
    Deadline,
    NotAcknowledged,
}

impl fmt::Display for OwnerReportPublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "the configured Root-Hub address or pin is invalid",
            Self::InvalidFence => "the local owner observation is invalid",
            Self::TransportUnavailable => "the pinned Root-Hub transport is unavailable",
            Self::Deadline => "the owner observation deadline expired",
            Self::NotAcknowledged => "the Root-Hub did not acknowledge the owner observation",
        })
    }
}

impl std::error::Error for OwnerReportPublishError {}

/// Binds one local hosted owner fence to one explicit Hub endpoint and pin.
/// Construct from trusted local composition, not from an external report,
/// request headers, or peer-supplied location. Uses the project's AuthHub
/// wrapped node-handshake profile; the supplied local signer must implement
/// that same profile.
pub struct OwnerReportClient {
    addr: SocketAddr,
    local: LocalNode,
    hub: NodeIdentity,
    verifier: TranscriptWrappedVerifier<PinnedPeers>,
    body: Vec<u8>,
}

impl OwnerReportClient {
    /// Prepare a client for a locally selected owner record and exactly one
    /// Hub. No socket is opened. To change the selected fence, construct a
    /// new client from fresh local owner state.
    ///
    /// # Errors
    /// Rejects unspecified or invalid endpoint settings, invalid node IDs
    /// or pins, and fences that do not belong to the local node.
    pub fn new(
        addr: SocketAddr,
        local: LocalNode,
        hub: NodeIdentity,
        local_fence: &OwnerFence,
    ) -> Result<Self, OwnerReportPublishError> {
        if addr.port() == 0
            || addr.ip().is_unspecified()
            || addr.ip().is_multicast()
            || matches!(addr.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
            || !valid_id(local.node_id().as_str())
            || !valid_id(hub.node_id.as_str())
            || local.node_id() == &hub.node_id
            || local.identity.public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN
            || local.identity.public_key.iter().all(|byte| *byte == 0)
            || hub.public_key.iter().all(|byte| *byte == 0)
        {
            return Err(OwnerReportPublishError::InvalidConfiguration);
        }
        let mut pins = PinnedPeers::new();
        pins.pin_identity(&hub).map_err(|_| OwnerReportPublishError::InvalidConfiguration)?;
        if local_fence.node_id != local.node_id().as_str() || local_fence.owner_epoch == 0 {
            return Err(OwnerReportPublishError::InvalidFence);
        }
        let body = encode(local_fence).map_err(|_| OwnerReportPublishError::InvalidFence)?;
        Ok(Self {
            addr,
            local,
            hub,
            verifier: TranscriptWrappedVerifier::new(pins),
            body,
        })
    }

    /// Consume this bound client in one attempt against the exact pinned Hub;
    /// never dials another endpoint. The private protocol maps successful dispatch to an empty
    /// HTTP 204. No response content or headers become authority or retry
    /// instructions. This is an acknowledgement, not a lease or a permit.
    ///
    /// # Errors
    /// A deadline or transport failure leaves delivery unconfirmed; other
    /// HTTP statuses are not acknowledgements. No failure is retried here.
    pub async fn publish_once(self) -> Result<(), OwnerReportPublishError> {
        tokio::time::timeout(PUBLISH_TIMEOUT, async {
            let mut client = NodeTransportClient::connect(
                self.addr,
                &self.local,
                &self.hub.node_id,
                &self.verifier,
            )
            .await
            .map_err(|_| OwnerReportPublishError::TransportUnavailable)?;
            let request = Request::builder()
                .method(Method::POST)
                .uri(OWNER_REPORT_PATH)
                .header(CONTENT_TYPE, "application/json")
                .header(CONTENT_LENGTH, self.body.len().to_string())
                .body(full_body(self.body.clone()))
                .map_err(|_| OwnerReportPublishError::InvalidFence)?;
            let response = client
                .send_request(request)
                .await
                .map_err(|_| OwnerReportPublishError::TransportUnavailable)?;
            // Do not read untrusted response text; dropping this connection
            // aborts it instead of following a redirect or reusing the stream.
            if response.status() != StatusCode::NO_CONTENT {
                return Err(OwnerReportPublishError::NotAcknowledged);
            }
            Ok(())
        })
        .await
        .map_err(|_| OwnerReportPublishError::Deadline)?
    }
}

//! Bounded direct-owner status route metadata. No dialing, relaying, peer
//! discovery, server mount, redirect, DNS or fallback is performed here.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;

use harw_node_transport::{
    ML_DSA_65_PUBLIC_KEY_LEN, NodeIdentity, PinnedPeers, TranscriptWrappedVerifier,
};

use crate::owner_directory::valid_id;
use crate::owner_ingress::DirectStatusAuthority;
use crate::owner_process_proof::ProcessProofError;

/// Exact operator-selected byte transport. None of these alternatives can
/// be chosen by a request, redirect or a Tor failure.
pub enum OwnerProofEndpoint {
    Tcp(SocketAddr),
    #[cfg(feature = "hub-owner-onion")]
    Onion(harw_onion::OnionLocator),
}

/// Exactly one operator-provisioned address and enrolled node key.
/// Independent process trust must match this same long-term node pin.
pub struct ConfiguredOwnerDirectLink {
    endpoint: OwnerProofEndpoint,
    identity: NodeIdentity,
    verifier: TranscriptWrappedVerifier<PinnedPeers>,
}

impl ConfiguredOwnerDirectLink {
    /// Trusted bootstrap only: the selected identity and literal address
    /// are not taken from a current request, report or session profile.
    pub fn new(address: SocketAddr, identity: NodeIdentity) -> Result<Self, ProcessProofError> {
        if address.port() == 0
            || address.ip().is_unspecified()
            || address.ip().is_multicast()
            || matches!(address.ip(), IpAddr::V4(ip) if ip.is_broadcast())
        {
            return Err(ProcessProofError::InvalidPin);
        }
        Self::with_endpoint(OwnerProofEndpoint::Tcp(address), identity)
    }

    /// Select a validated and immutable v3 Onion locator at startup.
    #[cfg(feature = "hub-owner-onion")]
    pub fn new_onion(
        locator: harw_onion::OnionLocator,
        identity: NodeIdentity,
    ) -> Result<Self, ProcessProofError> {
        Self::with_endpoint(OwnerProofEndpoint::Onion(locator), identity)
    }

    fn with_endpoint(
        endpoint: OwnerProofEndpoint,
        identity: NodeIdentity,
    ) -> Result<Self, ProcessProofError> {
        if !valid_id(identity.node_id.as_str())
            || identity.public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN
            || identity.public_key.iter().all(|byte| *byte == 0)
        {
            return Err(ProcessProofError::InvalidPin);
        }
        let mut pins = PinnedPeers::new();
        pins.pin_identity(&identity).map_err(|_| ProcessProofError::InvalidPin)?;
        Ok(Self { endpoint, identity, verifier: TranscriptWrappedVerifier::new(pins) })
    }

    pub(crate) fn node_key(&self) -> &[u8] {
        self.identity.public_key.as_slice()
    }
}

/// Immutable address/key selection for one Hub, empty by default.
/// There is at most one link per enrolled node and no automatic fallback.
#[derive(Default)]
pub struct OwnerDirectLinks {
    links: BTreeMap<String, Arc<ConfiguredOwnerDirectLink>>,
}

impl OwnerDirectLinks {
    /// Build from at most 64 independently configured entries. Multiple
    /// destinations for one node and key reuse across nodes are rejected.
    pub fn new(links: Vec<ConfiguredOwnerDirectLink>) -> Result<Self, ProcessProofError> {
        if links.len() > 64 {
            return Err(ProcessProofError::InvalidPin);
        }
        let mut selected = Self::default();
        for link in links {
            let node = link.identity.node_id.as_str().to_owned();
            if selected.links.contains_key(&node)
                || selected.links.values().any(|other| other.node_key() == link.node_key())
            {
                return Err(ProcessProofError::InvalidPin);
            }
            selected.links.insert(node, Arc::new(link));
        }
        Ok(selected)
    }

    pub(crate) fn select(&self, node: &str) -> Option<Arc<ConfiguredOwnerDirectLink>> {
        self.links.get(node).cloned()
    }
}

/// Local, one exact candidate for the negotiated read-only
/// `session.direct_owner_status` operation. Not serializable or clonable.
/// This is not authenticated process proof or a grant to issue that RPC.
pub struct DirectOwnerStatusPlan {
    link: Arc<ConfiguredOwnerDirectLink>,
    pub(crate) authority: DirectStatusAuthority,
    instance: String,
    generation: NonZeroU64,
    owner_epoch: NonZeroU64,
    expires_at: Instant,
}

impl DirectOwnerStatusPlan {
    pub(crate) fn new(
        link: Arc<ConfiguredOwnerDirectLink>,
        instance: String,
        generation: NonZeroU64,
        owner_epoch: NonZeroU64,
        expires_at: Instant,
        authority: DirectStatusAuthority,
    ) -> Self {
        Self {
            link,
            authority,
            instance,
            generation,
            owner_epoch,
            expires_at,
        }
    }

    /// Exact operator-selected byte transport for this observation.
    pub fn endpoint(&self) -> &OwnerProofEndpoint {
        &self.link.endpoint
    }

    /// Selected node and public key. Do not send a different identity or
    /// swap to a URL, redirect, cached route or owner discovery.
    #[cfg(feature = "hub-owner-placement-listener")]
    pub(crate) fn node_identity(&self) -> &NodeIdentity {
        &self.link.identity
    }

    pub fn owner_node(&self) -> &str {
        self.link.identity.node_id.as_str()
    }

    /// Independently provisioned long-term owner node pin (public bytes).
    /// The separate process pin is not substituted for this identity.
    pub fn public_node_key(&self) -> &[u8] {
        self.link.node_key()
    }

    /// Platform verifier built with only the selected exact owner key.
    pub fn verifier(&self) -> &TranscriptWrappedVerifier<PinnedPeers> {
        &self.link.verifier
    }

    /// Exact session accepted by the Hub's fresh read-only policy.
    pub fn session(&self) -> &str {
        self.authority.session()
    }

    /// The direct owner status RPC does not currently attest these
    /// coordinates. Independent process proof is still required when
    /// consuming the plan; do not substitute a NodeTransport node proof.
    pub fn expected_process(&self) -> (&str, NonZeroU64, NonZeroU64) {
        (&self.instance, self.generation, self.owner_epoch)
    }

    /// Original Hub-monotonic preflight expiry, never extended by selection.
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

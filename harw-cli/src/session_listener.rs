//! `harw gateway` wiring of the own-cloud (remote) session ingress.
//!
//! # Description
//! Builds everything [`crate::session_serve_remote::start_remote`] needs from
//! `[session_listener]` (global-only, default off) and `[infrastructure]`:
//!
//! - **Node identity:** the long-term ML-DSA-65 key lives in the AuthHub as
//!   `harw.node-identity/<node_id>`; this process never sees the private half.
//!   [`HubSign`] adapts `AuthHubClient::sign` to the transport's
//!   `AuthHubSign` seam, `AuthHubNodeSigner` wraps the handshake transcript
//!   and verifies every hub signature against the published public key
//!   (self-check) before it is used.
//! - **Trust store:** pinned peer keys from `node-peers.conf`; peers are
//!   verified through `TranscriptWrappedVerifier` (the wrapped form is a
//!   fleet-wide setting, see `harw-node-transport`).
//! - **Authorisation:** enrolled devices come from `node-devices.conf`
//!   (`harw-node-listener`); a valid key alone never admits anyone.
//! - **Bind:** loopback unless `allow_non_loopback = true`.
//!
//! # Files (operator-written, `<profile>/session-host/remote/`)
//! ```text
//! node-peers.conf     node_id|public_key_hex      # pinned node keys (1952-byte ML-DSA-65, hex)
//! node-devices.conf   node_id|device|tenant|tier|status|label[|approve]
//! ```
//! Malformed lines are ignored and logged, never repaired into a grant.
//! Marking a device `revoked` in `node-devices.conf` revokes it live (the
//! gateway scans the file; see `session_serve_remote`).
//!
//! Failure to start is a hard error of `harw gateway` (the option is explicit
//! and global-only), never a silent fallback.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use harw_config::SessionListenerSection;
use harw_core::ModelProvider;
use harw_infra_client::{AuthHubClient, KeyRef};
use harw_node_transport::{
    AuthHubNodeSigner, AuthHubSign, LocalNode, NodeIdentity, PinnedPeers, SignFuture, SignerError,
    TranscriptWrappedVerifier,
};
use harw_types::{DeviceId, NodeId, PermissionTier};
use tokio::net::TcpListener;

use crate::session_serve_remote::{RemoteIngress, RemoteServeConfig, start_remote};

/// File name of the pinned peer keys inside the remote state directory.
pub const PEERS_FILE: &str = "node-peers.conf";
/// AuthHub namespace of node identity keys.
const NODE_IDENTITY_NAMESPACE: &str = "harw.node-identity";

/// [`AuthHubSign`] over the AuthHub client (the orphan rule forbids a direct
/// impl in this crate on the client type).
pub struct HubSign(pub AuthHubClient);

impl AuthHubSign for HubSign {
    fn sign_with_key<'a>(
        &'a self,
        namespace: &'a str,
        key_id: &'a str,
        message: Vec<u8>,
    ) -> SignFuture<'a> {
        Box::pin(async move {
            let key = KeyRef::latest(namespace, key_id)
                .map_err(|_| SignerError::new("invalid AuthHub key reference"))?;
            // No hub detail, no payload: only a fixed message.
            self.0
                .sign(&key, &message)
                .await
                .map_err(|_| SignerError::new("AuthHub sign request failed"))
        })
    }
}

/// Decodes lowercase/uppercase hex; `None` for odd length or a bad digit.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

/// Parses `node-peers.conf` into the trust store. A line is
/// `node_id|public_key_hex`; `#` starts a comment. Lines that do not parse or
/// whose key is not a valid pin are skipped (and counted by the caller via
/// the returned `skipped`), never partially applied.
#[must_use]
pub fn parse_peers(text: &str) -> (PinnedPeers, usize) {
    let mut peers = PinnedPeers::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = line.split_once('|').and_then(|(node, key)| {
            Some((
                NodeId::try_from_str(node.trim()).ok()?,
                decode_hex(key.trim())?,
            ))
        });
        let pinned = parsed.is_some_and(|(node, key)| peers.pin(node, key).is_ok());
        if !pinned {
            skipped += 1;
        }
    }
    (peers, skipped)
}

/// Short fingerprints of the pinned node keys in `node-peers.conf` text, by
/// node id: `blake3:` + the first 8 bytes of the BLAKE3 hash of the public
/// key, hex. Only lines that [`parse_peers`] would pin are included; the
/// key itself is public, nothing secret is read.
#[must_use]
pub(crate) fn peer_fingerprints(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((node, key)) = line.split_once('|') else {
            continue;
        };
        let (Ok(id), Some(bytes)) = (NodeId::try_from_str(node.trim()), decode_hex(key.trim()))
        else {
            continue;
        };
        if PinnedPeers::new().pin(id, bytes.clone()).is_err() {
            continue;
        }
        let hash = blake3::hash(&bytes);
        let short: String = hash.as_bytes()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        out.insert(node.trim().to_owned(), format!("blake3:{short}"));
    }
    out
}

/// Resolves the listen address; a non-loopback bind needs the explicit
/// opt-in.
///
/// # Errors
/// An unparsable address or a non-loopback address without opt-in.
pub fn resolve_listen(section: &SessionListenerSection) -> Result<SocketAddr, String> {
    let addr: SocketAddr = section
        .listen
        .parse()
        .map_err(|error| format!("[session_listener] listen `{}`: {error}", section.listen))?;
    if !addr.ip().is_loopback() && !section.allow_non_loopback {
        return Err(format!(
            "[session_listener] listen `{addr}` is not loopback; set allow_non_loopback = true to allow it"
        ));
    }
    Ok(addr)
}

pub(crate) fn parse_tier(raw: &str) -> Result<PermissionTier, String> {
    match raw {
        "observer" => Ok(PermissionTier::Observer),
        "operator" => Ok(PermissionTier::Operator),
        "maintainer" => Ok(PermissionTier::Maintainer),
        "owner" => Ok(PermissionTier::Owner),
        other => Err(format!(
            "[session_listener] tier `{other}` must be observer|operator|maintainer|owner"
        )),
    }
}

/// Validated settings derived from the section (pure; no I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerPlan {
    /// Address to bind.
    pub addr: SocketAddr,
    /// This gateway's node id.
    pub node: NodeId,
    /// Sandbox tier of remote sessions.
    pub tier: PermissionTier,
    /// The approving device, if any.
    pub approval_device: Option<DeviceId>,
}

/// Validates `[session_listener]`.
///
/// # Errors
/// The first invalid or missing value.
pub fn plan(section: &SessionListenerSection) -> Result<ListenerPlan, String> {
    let addr = resolve_listen(section)?;
    let node = section
        .node_id
        .as_deref()
        .ok_or_else(|| "[session_listener] node_id is required".to_owned())
        .and_then(|raw| {
            NodeId::try_from_str(raw)
                .map_err(|error| format!("[session_listener] node_id: {error}"))
        })?;
    let approval_device = section
        .approval_device
        .as_deref()
        .map(|raw| {
            DeviceId::try_from_str(raw)
                .map_err(|error| format!("[session_listener] approval_device: {error}"))
        })
        .transpose()?;
    Ok(ListenerPlan {
        addr,
        node,
        tier: parse_tier(&section.tier)?,
        approval_device,
    })
}

/// Starts the remote ingress from configuration.
///
/// # Errors
/// An invalid section, a missing AuthHub, a key that does not verify, an
/// unreadable peers file or a bind failure.
pub async fn start_from_config(
    section: &SessionListenerSection,
    infrastructure: Option<&harw_config::infrastructure_toml::InfrastructureSection>,
    state_dir: &Path,
    home: &Path,
    cwd: &Path,
    model: Arc<dyn ModelProvider>,
) -> Result<RemoteIngress, String> {
    let plan = plan(section)?;
    let infrastructure = infrastructure
        .ok_or_else(|| "[session_listener] needs [infrastructure] (AuthHub)".to_owned())?;
    let client = crate::secret_store::authhub_client(infrastructure)?;

    let key = KeyRef::latest(NODE_IDENTITY_NAMESPACE, plan.node.as_str())
        .map_err(|error| format!("node identity key reference: {error}"))?;
    let public_key = client
        .public_key(&key)
        .await
        .map_err(|_| "could not read the node identity public key from the AuthHub".to_owned())?
        .into_vec();
    let signer = AuthHubNodeSigner::new(Arc::new(HubSign(client)), &plan.node)
        .and_then(|signer| signer.with_self_check(public_key.clone()))
        .map_err(|error| format!("node signer: {error}"))?;
    let local = LocalNode::new(
        NodeIdentity {
            node_id: plan.node.clone(),
            public_key,
            key_ref: format!("{NODE_IDENTITY_NAMESPACE}/{}", plan.node.as_str()),
        },
        Arc::new(signer),
    );

    let mut config = RemoteServeConfig::new(state_dir, home, cwd);
    config.tier = plan.tier;
    config.approval_device = plan.approval_device;
    let remote_dir = state_dir.join("remote");
    let peers_text = std::fs::read_to_string(remote_dir.join(PEERS_FILE)).unwrap_or_default();
    let (peers, skipped) = parse_peers(&peers_text);
    if skipped > 0 {
        tracing::warn!(
            skipped,
            "session_listener: ignored malformed lines in {PEERS_FILE}"
        );
    }
    let verifier = Arc::new(TranscriptWrappedVerifier::new(peers));
    let listener = TcpListener::bind(plan.addr)
        .await
        .map_err(|error| format!("session listener bind {}: {error}", plan.addr))?;
    start_remote(&config, model, local, verifier, listener).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section() -> SessionListenerSection {
        SessionListenerSection {
            enabled: true,
            node_id: Some("gateway-a".to_owned()),
            ..SessionListenerSection::default()
        }
    }

    #[test]
    fn defaults_are_loopback_observer_without_an_approver() -> Result<(), String> {
        let plan = plan(&section())?;
        assert!(plan.addr.ip().is_loopback());
        assert_eq!(plan.tier, PermissionTier::Observer);
        assert_eq!(plan.approval_device, None);
        Ok(())
    }

    #[test]
    fn a_non_loopback_bind_needs_the_explicit_opt_in() {
        let mut open = section();
        open.listen = "0.0.0.0:7443".to_owned();
        assert!(plan(&open).is_err());
        open.allow_non_loopback = true;
        assert!(plan(&open).is_ok());
        let mut bad = section();
        bad.listen = "not an address".to_owned();
        assert!(plan(&bad).is_err());
    }

    #[test]
    fn node_id_tier_and_device_are_validated() {
        let mut missing = section();
        missing.node_id = None;
        assert!(plan(&missing).is_err(), "node_id is required");
        let mut tier = section();
        tier.tier = "root".to_owned();
        assert!(plan(&tier).is_err());
        let mut device = section();
        device.approval_device = Some("dev-1".to_owned());
        assert_eq!(
            plan(&device)
                .ok()
                .and_then(|p| p.approval_device)
                .map(|d| d.as_str().to_owned()),
            Some("dev-1".to_owned())
        );
    }

    #[test]
    fn hex_decoding_is_strict() {
        assert_eq!(decode_hex("00ff"), Some(vec![0, 255]));
        assert_eq!(decode_hex("0Ff"), None, "odd length");
        assert_eq!(decode_hex("zz"), None);
    }

    #[test]
    fn peers_file_pins_valid_keys_and_skips_everything_else() {
        let key = "ab".repeat(1952);
        let text = format!(
            "# comment\n\nphone|{key}\nbad line\nshort|abcd\nphone2|{}\n",
            "zz".repeat(1952)
        );
        let (peers, skipped) = parse_peers(&text);
        assert_eq!(peers.len(), 1, "only the well-formed 1952-byte pin");
        assert_eq!(skipped, 3, "malformed, short key, non-hex key");
    }

    #[tokio::test]
    async fn a_missing_authhub_fails_closed() {
        let temp = tempfile::tempdir().ok();
        let Some(temp) = temp else { return };
        let model: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::default());
        let result = start_from_config(
            &section(),
            None,
            temp.path(),
            temp.path(),
            temp.path(),
            model,
        )
        .await;
        assert!(result.is_err());
    }
}

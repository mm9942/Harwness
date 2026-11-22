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
//! # Onion ingress (`onion = true`, needs the `onion` cargo feature)
//! After the TCP ingress is up, `harw-onion` publishes a Tor v3 onion service
//! whose accepted streams feed the SAME node listener (`StreamIntake`): TLS 1.3
//! plus the node handshake and the device registry still authenticate every
//! peer. Tor is a transport only and provides no identity.
//! ```text
//! <onion_dir>/state    # arti state incl. keystore (stable .onion), 0700
//! <onion_dir>/cache    # arti cache, 0700
//! <onion_dir>/locator  # canonical `<host>.onion:<port>` + newline, 0600
//! ```
//! `onion_dir` defaults to `<profile>/session-host/onion`. The full address is
//! only written to the locator file; it is never printed or logged.
//!
//! Failure to start is a hard error of `harw gateway` (the option is explicit
//! and global-only), never a silent fallback.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "onion")]
use std::time::Duration;

use harw_config::SessionListenerSection;
use harw_core::ModelProvider;
use harw_infra_client::{AuthHubClient, KeyRef};
use harw_node_transport::{
    AuthHubNodeSigner, AuthHubSign, LocalNode, NodeIdentity, PinnedPeers, SignFuture, SignerError,
    TranscriptWrappedVerifier,
};
use harw_session_host::index::ShareMode;
use harw_types::{DeviceId, NodeId, PermissionTier};
use tokio::net::TcpListener;

use crate::session_serve_remote::{RemoteIngress, RemoteServeConfig, start_remote};

/// File name of the pinned peer keys inside the remote state directory.
pub const PEERS_FILE: &str = "node-peers.conf";
/// AuthHub namespace of node identity keys.
const NODE_IDENTITY_NAMESPACE: &str = "harw.node-identity";
/// Directory name of the onion state below the session-host state directory.
pub const ONION_DIR: &str = "onion";
/// File name of the canonical locator inside the onion directory.
pub const ONION_LOCATOR_FILE: &str = "locator";
/// How long to wait for the onion service to become reachable.
#[cfg(feature = "onion")]
const ONION_REACHABLE_TIMEOUT: Duration = Duration::from_secs(180);
/// Node-handshake bound for connections arriving over Tor.
#[cfg(feature = "onion")]
const ONION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

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
    if !text.len().is_multiple_of(2) {
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
    peer_key_fingerprints(text)
        .into_iter()
        .map(|(node, hash)| {
            let short: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
            (node, format!("blake3:{short}"))
        })
        .collect()
}

/// Full BLAKE3 fingerprints of owner-pinned public keys, for persisted creator
/// binding. The source must be the exact validated verifier startup snapshot.
pub(crate) fn peer_key_fingerprints(text: &str) -> std::collections::BTreeMap<String, [u8; 32]> {
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
        out.insert(node.trim().to_owned(), *blake3::hash(&bytes).as_bytes());
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

/// Upper bound of `share_legacy_rows`.
const MAX_SHARE_LEGACY_ROWS: u32 = 1000;

/// Validated sharing settings of TUI sessions (pure; no I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePlan {
    /// `off` (default), `read` or `continue`.
    pub mode: ShareMode,
    /// `Some` exactly when `mode != Off`.
    pub tenant: Option<harw_types::TenantId>,
    /// Legacy rows for clients without the `projects` feature.
    pub legacy_rows: usize,
    /// `share_since` at 00:00 UTC.
    pub since: Option<jiff::Timestamp>,
    /// Syntactically checked (absolute, no `.`/`..`); canonicalised later.
    pub roots: Vec<PathBuf>,
}

impl SharePlan {
    /// Sharing disabled.
    #[must_use]
    pub fn off() -> Self {
        Self {
            mode: ShareMode::Off,
            tenant: None,
            legacy_rows: 100,
            since: None,
            roots: Vec::new(),
        }
    }
}

/// Validates the five `share_*` keys.
fn plan_share(section: &SessionListenerSection) -> Result<SharePlan, String> {
    let raw_mode = section.share_profile_sessions.as_str();
    let mode = ShareMode::parse(raw_mode).ok_or_else(|| {
        format!("[session_listener] share_profile_sessions `{raw_mode}` must be off|read|continue")
    })?;
    let tenant = match (mode, section.share_tenant.as_deref()) {
        (ShareMode::Off, _) => None,
        (_, None) => {
            return Err(
                "[session_listener] share_tenant is required when share_profile_sessions is not off"
                    .to_owned(),
            );
        }
        (_, Some(raw)) => Some(
            harw_types::TenantId::try_from_str(raw)
                .map_err(|error| format!("[session_listener] share_tenant: {error}"))?,
        ),
    };
    if section.share_legacy_rows > MAX_SHARE_LEGACY_ROWS {
        return Err("[session_listener] share_legacy_rows must be at most 1000".to_owned());
    }
    let since = section
        .share_since
        .as_deref()
        .map(|raw| {
            let bad = || "[session_listener] share_since must be a date YYYY-MM-DD".to_owned();
            if raw.len() != 10 {
                return Err(bad());
            }
            let date: jiff::civil::Date = raw.parse().map_err(|_| bad())?;
            date.to_zoned(jiff::tz::TimeZone::UTC)
                .map(|zoned| zoned.timestamp())
                .map_err(|_| bad())
        })
        .transpose()?;
    let mut roots = Vec::with_capacity(section.share_roots.len());
    for raw in &section.share_roots {
        let path = PathBuf::from(raw);
        if !path.is_absolute() || raw.split('/').any(|part| part == "." || part == "..") {
            return Err(
                "[session_listener] share_roots entries must be absolute paths without `.` or `..`"
                    .to_owned(),
            );
        }
        roots.push(path);
    }
    Ok(SharePlan {
        mode,
        tenant,
        legacy_rows: section.share_legacy_rows as usize,
        since,
        roots,
    })
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
    /// Device permitted to link the gateway provider account.
    pub provider_link_device: Option<DeviceId>,
    /// Whether to publish an onion service for the same listener.
    pub onion: bool,
    /// Absolute onion directory override (`None`: `<state>/onion`).
    pub onion_dir: Option<PathBuf>,
    /// Sharing of TUI sessions (default off).
    pub share: SharePlan,
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
    let onion_dir = match section.onion_dir.as_deref() {
        None => None,
        Some(_) if !section.onion => {
            return Err("[session_listener] onion_dir requires onion = true".to_owned());
        }
        Some(raw) => {
            let dir = PathBuf::from(raw);
            if !dir.is_absolute() {
                return Err("[session_listener] onion_dir must be an absolute path".to_owned());
            }
            // Checked on the raw text: `Path::components` drops interior `.`
            // segments, so `/var/./onion` would slip through a component check.
            if raw.split('/').any(|part| part == "." || part == "..") {
                return Err(
                    "[session_listener] onion_dir must not contain `.` or `..` components"
                        .to_owned(),
                );
            }
            Some(dir)
        }
    };
    if section.onion && !cfg!(feature = "onion") {
        return Err(
            "[session_listener] onion = true needs a harw build with the `onion` cargo feature"
                .to_owned(),
        );
    }
    Ok(ListenerPlan {
        addr,
        node,
        tier: parse_tier(&section.tier)?,
        approval_device,
        provider_link_device: section
            .provider_link_device
            .as_deref()
            .map(DeviceId::try_from_str)
            .transpose()
            .map_err(|error| format!("[session_listener] provider_link_device: {error}"))?,
        onion: section.onion,
        onion_dir,
        share: plan_share(section)?,
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
    config.provider_link_device = plan.provider_link_device;
    config.share = plan.share.clone();
    let remote_dir = state_dir.join("remote");
    let peers_text = crate::session_serve_remote::read_peer_pins(&remote_dir.join(PEERS_FILE))?;
    config.peer_pins_digest = Some(*blake3::hash(peers_text.as_bytes()).as_bytes());
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
    let mut ingress = start_remote(&config, model, local, verifier, listener).await?;
    if plan.onion {
        let dir = plan
            .onion_dir
            .clone()
            .unwrap_or_else(|| state_dir.join(ONION_DIR));
        match start_onion(&dir, ingress.intake()).await {
            Ok(server) => ingress.attach(Box::new(server)),
            Err(error) => {
                ingress.shutdown().await;
                return Err(error);
            }
        }
    }
    Ok(ingress)
}

/// Unreachable in builds without the `onion` feature: `plan` rejects onion.
#[cfg(not(feature = "onion"))]
async fn start_onion(
    _dir: &Path,
    _intake: harw_node_listener::StreamIntake,
) -> Result<std::convert::Infallible, String> {
    Err(
        "[session_listener] onion = true needs a harw build with the `onion` cargo feature"
            .to_owned(),
    )
}

/// Launches the onion service and feeds its streams into `intake`.
#[cfg(feature = "onion")]
async fn start_onion(
    dir: &Path,
    intake: harw_node_listener::StreamIntake,
) -> Result<harw_onion::OnionServer, String> {
    use harw_onion::{BootstrapProgress, OnionServer, OnionServerConfig};

    crate::session_serve::private_dir(dir)?;
    let state = dir.join("state");
    let cache = dir.join("cache");
    crate::session_serve::private_dir(&state)?;
    crate::session_serve::private_dir(&cache)?;
    eprintln!("  session onion  : starting Tor (the first start can take several minutes)");
    let mut last_bucket: Option<u8> = None;
    let mut last_blocked: Option<String> = None;
    let on_progress = move |progress: &BootstrapProgress| {
        let bucket = progress.percent / 10;
        if last_bucket != Some(bucket) {
            last_bucket = Some(bucket);
            eprintln!("  session onion  : tor bootstrap {}%", progress.percent);
        }
        if progress.blocked != last_blocked {
            last_blocked.clone_from(&progress.blocked);
            if let Some(blocked) = &progress.blocked {
                eprintln!("  session onion  : tor blocked: {blocked}");
            }
        }
    };
    let handler = move |stream: harw_onion::OnionStream| {
        let intake = intake.clone();
        async move {
            if let Err(error) = intake.serve(stream, ONION_HANDSHAKE_TIMEOUT).await {
                tracing::debug!(%error, "onion session connection ended");
            }
        }
    };
    let server = OnionServer::launch(OnionServerConfig::new(state, cache), on_progress, handler)
        .await
        .map_err(|error| format!("[session_listener] onion: {error}"))?;
    write_locator(&dir.join(ONION_LOCATOR_FILE), server.locator())?;
    if server.wait_reachable(ONION_REACHABLE_TIMEOUT).await {
        eprintln!("  session onion  : reachable");
    } else {
        eprintln!("  session onion  : not confirmed reachable after 180 s; continuing");
    }
    tracing::info!(onion = %server.locator().redacted(), "session listener onion service up");
    Ok(server)
}

/// Writes `<locator>\n` atomically (tmp file, mode 0600, rename).
#[cfg(feature = "onion")]
fn write_locator(path: &Path, locator: &harw_onion::OnionLocator) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let tmp = path.with_extension("tmp");
    let fail = |error: std::io::Error| format!("[session_listener] onion locator file: {error}");
    // A stale tmp (maybe with loose permissions) is removed; `create_new`
    // (O_EXCL) never follows a symlink and guarantees the 0600 mode applies.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(fail(error)),
    }
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(fail)?;
    file.write_all(format!("{locator}\n").as_bytes())
        .map_err(fail)?;
    file.sync_all().map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
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
    fn share_defaults_to_off() -> Result<(), String> {
        let plan = plan(&section())?;
        assert_eq!(plan.share, SharePlan::off());
        assert_eq!(plan.share.mode, ShareMode::Off);
        assert_eq!(plan.share.legacy_rows, 100);
        Ok(())
    }

    #[test]
    fn read_without_tenant_is_rejected() {
        let mut shared = section();
        shared.share_profile_sessions = "read".to_owned();
        assert!(plan(&shared).is_err_and(|e| e.contains("share_tenant is required")));
        shared.share_tenant = Some("tenant-a".to_owned());
        assert!(
            plan(&shared)
                .is_ok_and(|p| p.share.mode == ShareMode::Read && p.share.tenant.is_some())
        );
    }

    #[test]
    fn continue_without_tenant_is_rejected() {
        let mut shared = section();
        shared.share_profile_sessions = "continue".to_owned();
        assert!(plan(&shared).is_err_and(|e| e.contains("share_tenant is required")));
    }

    #[test]
    fn unknown_share_mode_is_rejected() {
        let mut shared = section();
        shared.share_profile_sessions = "write".to_owned();
        assert!(plan(&shared).is_err_and(|e| e.contains("must be off|read|continue")));
    }

    #[test]
    fn share_limits_are_validated() {
        let mut many = section();
        many.share_legacy_rows = 1001;
        assert!(plan(&many).is_err_and(|e| e.contains("at most 1000")));
        many.share_legacy_rows = 1000;
        assert!(plan(&many).is_ok());
    }

    #[test]
    fn share_since_parses_date() -> Result<(), String> {
        let mut dated = section();
        dated.share_since = Some("2026-01-02".to_owned());
        let since = plan(&dated)?.share.since;
        assert_eq!(since.map(|t| t.as_second()), Some(1_767_312_000));
        for bad in ["2026-1-2", "yesterday", "2026-13-01", ""] {
            dated.share_since = Some(bad.to_owned());
            assert!(
                plan(&dated).is_err_and(|e| e.contains("YYYY-MM-DD")),
                "{bad}"
            );
        }
        Ok(())
    }

    #[test]
    fn relative_share_root_is_rejected() {
        for raw in ["work/dir", "/a/../b", "/a/./b"] {
            let mut rooted = section();
            rooted.share_roots = vec![raw.to_owned()];
            assert!(
                plan(&rooted).is_err_and(|e| e.contains("share_roots")),
                "{raw}"
            );
        }
        let mut ok = section();
        ok.share_roots = vec!["/srv/work".to_owned()];
        assert_eq!(
            plan(&ok).map(|p| p.share.roots).ok(),
            Some(vec![PathBuf::from("/srv/work")])
        );
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
    fn onion_dir_requires_onion_and_an_absolute_path() {
        let mut dir_only = section();
        dir_only.onion_dir = Some("/var/onion".to_owned());
        assert!(plan(&dir_only).is_err(), "onion_dir without onion");
        let mut relative = section();
        relative.onion = true;
        relative.onion_dir = Some("onion".to_owned());
        assert!(
            plan(&relative).is_err_and(|e| e.contains("absolute")),
            "relative onion_dir"
        );
        for raw in ["/var/onion/../x", "/var/./onion"] {
            let mut dotted = section();
            dotted.onion = true;
            dotted.onion_dir = Some(raw.to_owned());
            assert!(
                plan(&dotted).is_err_and(|e| e.contains("components")),
                "{raw}"
            );
        }
    }

    #[cfg(not(feature = "onion"))]
    #[test]
    fn onion_without_the_feature_is_a_plan_error() {
        let mut onion = section();
        onion.onion = true;
        assert!(plan(&onion).is_err());
    }

    #[cfg(feature = "onion")]
    #[test]
    fn onion_with_the_feature_plans() -> Result<(), String> {
        let mut onion = section();
        onion.onion = true;
        onion.onion_dir = Some("/var/onion".to_owned());
        let plan = plan(&onion)?;
        assert!(plan.onion);
        assert_eq!(plan.onion_dir, Some(PathBuf::from("/var/onion")));
        Ok(())
    }

    #[cfg(feature = "onion")]
    #[test]
    fn write_locator_is_owner_only_and_canonical() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let locator: harw_onion::OnionLocator =
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443"
                .parse()
                .map_err(|e: harw_onion::OnionError| e.to_string())?;
        let path = temp.path().join(ONION_LOCATOR_FILE);
        write_locator(&path, &locator)?;
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        assert_eq!(
            text,
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443\n"
        );
        let mode = std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        Ok(())
    }

    #[cfg(feature = "onion")]
    #[test]
    fn write_locator_replaces_a_loose_stale_tmp() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let locator: harw_onion::OnionLocator =
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443"
                .parse()
                .map_err(|e: harw_onion::OnionError| e.to_string())?;
        let path = temp.path().join(ONION_LOCATOR_FILE);
        let stale = path.with_extension("tmp");
        std::fs::write(&stale, "old").map_err(|e| e.to_string())?;
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| e.to_string())?;
        write_locator(&path, &locator)?;
        let mode = std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        Ok(())
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

//! `harw attach`: an eine Host-Sitzung über `harw-session-remote` anhängen
//! (W00 W04/W06, Scope S09).
//!
//! Endpoint-Auflösung: `--socket PATH`, sonst `--host ALIAS` (lokaler
//! Alias-Speicher, S07), sonst der Standard-Socket
//! `$XDG_RUNTIME_DIR/harw/session.sock`. Unix- und Node-Verbindungen nutzen
//! `RemotePort` und `harw_tui::attach::run_attach`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_infra_client::KeyRef;
use harw_node_transport::{
    AuthHubNodeSigner, LocalNode, NodeIdentity, NodeVerifier, TranscriptWrappedVerifier,
};
use harw_protocol::SessionPort;
use harw_session_remote::alias::{AliasStore, HostAlias, validate_record};
use harw_session_remote::{ConnectOptions, NodeEndpoint, RemotePort, connect_node, connect_unix};
use harw_tui::attach::{resolve_session, run_attach};
use harw_types::{IngressSurface, NodeId};

use crate::cli::AttachArgs;

/// Relativer Pfad des Standard-Sockets unterhalb von `$XDG_RUNTIME_DIR`.
///
/// Platzhalter, bis die Konvention des Session-Daemons (#91) auf dieser Basis
/// liegt; dann dort statt hier ableiten.
const DEFAULT_SOCKET_REL: &str = "harw/session.sock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeRoute {
    OnionOnly,
    DirectOptIn,
}

/// Wohin verbunden wird.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// Lokaler AF_UNIX-Socket.
    Unix(PathBuf),
    /// Own-Cloud-Knoten (Node-Transport) laut Alias.
    Node {
        alias: String,
        endpoint: String,
        expected_node: String,
        route: NodeRoute,
    },
}

/// Standard-Socket aus dem Laufzeitverzeichnis.
///
/// # Errors
/// Ohne `XDG_RUNTIME_DIR` gibt es keinen sicheren Standardort; der Fehler
/// verweist auf `--socket`.
pub(crate) fn default_socket(xdg_runtime_dir: Option<&str>) -> Result<PathBuf, String> {
    match xdg_runtime_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => Ok(Path::new(dir).join(DEFAULT_SOCKET_REL)),
        None => Err(
            "kein Standard-Socket: XDG_RUNTIME_DIR ist nicht gesetzt; `--socket PATH` angeben"
                .to_owned(),
        ),
    }
}

/// Verzeichnis des maschinenlokalen Zustands (`HARW_STATE_DIR`, sonst XDG).
pub(crate) fn state_dir(
    harw_state_dir: Option<&str>,
    xdg_state_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, String> {
    fn set(value: Option<&str>) -> Option<&str> {
        value.filter(|s| !s.is_empty())
    }
    if let Some(dir) = set(harw_state_dir) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = set(xdg_state_home) {
        return Ok(Path::new(dir).join("harw"));
    }
    if let Some(home) = set(home) {
        return Ok(Path::new(home).join(".local/state/harw"));
    }
    Err("kein Zustandsverzeichnis: HARW_STATE_DIR, XDG_STATE_HOME und HOME fehlen".to_owned())
}

/// Resolve an alias without losing the authenticated node identity.
fn target_from_record(record: &HostAlias) -> Result<Target, String> {
    validate_record(record).map_err(|e| format!("Host-Alias `{}`: {e}", record.alias))?;
    if let Some(path) = record.endpoint.strip_prefix("unix:") {
        return Ok(Target::Unix(PathBuf::from(path)));
    }
    let (endpoint, route) = if let Some(endpoint) = record.endpoint.strip_prefix("node-direct:") {
        if is_onion_endpoint(endpoint) {
            return Err("Für Onion-Adressen ist `node:`, nicht `node-direct:` erforderlich.".into());
        }
        (endpoint, NodeRoute::DirectOptIn)
    } else {
        let endpoint = record.endpoint.strip_prefix("node:")
            .ok_or_else(|| "invalid node endpoint".to_owned())?;
        if !is_onion_endpoint(endpoint) {
            return Err("Direkte Verbindungen sind für diesen Alias aus. Verwende einen .onion-Locator oder konfiguriere bewusst `node-direct:`.".into());
        }
        (endpoint, NodeRoute::OnionOnly)
    };
    let expected_node = record
        .expected_node
        .clone()
        .ok_or_else(|| "node identity pin missing".to_owned())?;
    Ok(Target::Node {
        alias: record.alias.clone(),
        endpoint: endpoint.to_owned(),
        expected_node,
        route,
    })
}

fn is_onion_endpoint(endpoint: &str) -> bool {
    endpoint.rsplit_once(':').is_some_and(|(host, _)| {
        host.trim_end_matches('.').to_ascii_lowercase().ends_with(".onion")
    })
}

impl Target {
    /// Report the selected transport without leaking endpoint or key material.
    pub(crate) const fn transport_name(&self) -> &'static str {
        match self {
            Self::Unix(_) => "unix",
            Self::Node { .. } => "authenticated_node",
        }
    }

    pub(crate) async fn connect(
        self,
        home_override: Option<PathBuf>,
        options: ConnectOptions,
    ) -> Result<RemotePort, String> {
        let connection = match self {
            Self::Unix(path) => connect_unix(path.clone(), options)
                .await
                .map_err(|e| format!("Verbindung zu {} fehlgeschlagen: {e}", path.display()))?,
            Self::Node {
                alias,
                endpoint,
                expected_node,
                route,
            } => {
                connect_node_target(&alias, &endpoint, &expected_node, route, home_override, options)
                    .await?
            }
        };
        Ok(RemotePort::new(connection))
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn resolve_target(args: &AttachArgs) -> Result<Target, String> {
    if let Some(socket) = &args.socket {
        return Ok(Target::Unix(socket.clone()));
    }
    if let Some(alias) = &args.host {
        let dir = state_dir(
            env("HARW_STATE_DIR").as_deref(),
            env("XDG_STATE_HOME").as_deref(),
            env("HOME").as_deref(),
        )?;
        let record = AliasStore::new(&dir)
            .get(alias)
            .map_err(|e| format!("Host-Alias `{alias}`: {e}"))?
            .ok_or_else(|| format!("Host-Alias `{alias}` ist unbekannt"))?;
        return target_from_record(&record);
    }
    default_socket(env("XDG_RUNTIME_DIR").as_deref()).map(Target::Unix)
}

/// Führt `harw attach` aus.
///
/// # Errors
/// Deutscher Fehlertext: Auflösung, Verbindung oder Attach-Fehler.
pub fn run(args: &AttachArgs, home_override: Option<PathBuf>) -> Result<(), String> {
    let target = resolve_target(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("Runtime konnte nicht gestartet werden: {e}"))?;
    runtime.block_on(async move {
        let options = ConnectOptions::new("harw attach");
        let port: Arc<dyn SessionPort> = Arc::new(target.connect(home_override, options).await?);
        let session = match &args.session {
            Some(query) => Some(
                resolve_session(port.as_ref(), query)
                    .await
                    .map_err(|e| e.to_string())?,
            ),
            None => None,
        };
        run_attach(port, session).await.map_err(|e| e.to_string())
    })
}

fn load_node_config(home: &Path, cwd: &Path) -> Result<harw_config::ResolvedConfig, String> {
    let spec = crate::runtime_entry::runtime_spec(
        harw_runtime::EntryKind::SessionHost,
        home,
        cwd,
        crate::runtime_entry::local_principal(IngressSurface::Tui),
    );
    harw_runtime::load_config(&spec)
        .map(|(config, _)| config)
        .map_err(|e| format!("Konfiguration: {e}"))
}

/// Build a node client from the selected home and its trusted peer registry.
async fn connect_node_target(
    alias: &str,
    endpoint: &str,
    expected_node: &str,
    route: NodeRoute,
    home_override: Option<PathBuf>,
    options: ConnectOptions,
) -> Result<harw_session_remote::RemoteConnection, String> {
    if route == NodeRoute::OnionOnly && !cfg!(feature = "onion") {
        return Err(format!("Host-Alias `{alias}`: dieses harw enthält keinen Tor-Client; kein Direktrückfall"));
    }
    let home = crate::home::resolve_home(home_override)?;
    let local = load_node_identity(&home).await?;
    let profile = crate::session_serve_remote::remote_state_dir(&home)?;
    let text = crate::session_serve_remote::read_peer_pins(&profile.join(crate::session_listener::PEERS_FILE))?;
    let (peers, _) = crate::session_listener::parse_peers(&text);
    let verifier = TranscriptWrappedVerifier::new(peers);
    match route {
        NodeRoute::DirectOptIn =>
            connect_authenticated_node(alias, endpoint, expected_node, &local, &verifier, options).await,
        NodeRoute::OnionOnly =>
            connect_onion_node(alias, endpoint, expected_node, &local, &verifier, options, &profile).await,
    }
}

/// Use the existing active-profile AuthHub credential, never a new key.
async fn load_node_identity(home: &Path) -> Result<LocalNode, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("Arbeitsverzeichnis: {e}"))?;
    let config = load_node_config(home, &cwd)?;
    let node = config
        .harness
        .session_listener
        .node_id
        .as_deref()
        .ok_or_else(|| "[session_listener] node_id fehlt für den lokalen Client".to_owned())
        .and_then(|id| NodeId::try_from_str(id).map_err(|e| format!("lokale node_id: {e}")))?;
    let infrastructure = config
        .infrastructure
        .as_ref()
        .ok_or_else(|| "[infrastructure] AuthHub fehlt".to_owned())?;
    let client = crate::secret_store::authhub_client(infrastructure)?;
    let key = KeyRef::latest("harw.node-identity", node.as_str())
        .map_err(|e| format!("node identity key: {e}"))?;
    let public_key = client
        .public_key(&key)
        .await
        .map_err(|_| "AuthHub node identity public key nicht lesbar".to_owned())?
        .into_vec();
    let signer = AuthHubNodeSigner::new(Arc::new(crate::session_listener::HubSign(client)), &node)
        .and_then(|signer| signer.with_self_check(public_key.clone()))
        .map_err(|e| format!("node signer: {e}"))?;
    Ok(LocalNode::new(
        NodeIdentity {
            node_id: node.clone(),
            public_key,
            key_ref: format!("harw.node-identity/{}", node.as_str()),
        },
        Arc::new(signer),
    ))
}

#[cfg(feature = "onion")]
pub(crate) async fn fenced_owner_status(
    owner: Target,
    hub: Target,
    home_override: Option<PathBuf>,
    session: harw_types::SessionId,
) -> Result<harw_protocol::session_wire::OwnerObservationStatus, String> {
    use std::time::Duration;
    use harw_node_transport::{ClientOptions, PinnedPeers, node_key_fingerprint};
    use harw_onion::{OnionClient, OnionClientConfig, OnionLocator};
    use harw_session_remote::OwnerStatusDevice;

    fn target(target: Target) -> Result<(OnionLocator, NodeId), String> {
        let Target::Node { endpoint, expected_node, route: NodeRoute::OnionOnly, .. } = target else {
            return Err("Fenced owner status requires two pinned Onion aliases; no direct fallback".into());
        };
        let locator = endpoint.parse::<OnionLocator>().map_err(|_| "Invalid pinned v3 Onion locator")?;
        let node = NodeId::try_from_str(expected_node).map_err(|_| "Invalid pinned node identity")?;
        Ok((locator, node))
    }

    fn exact_pin(text: &str, expected: &NodeId) -> Option<Vec<u8>> {
        let mut matches = text.lines().map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('|'))
            .filter(|(node, _)| node.trim() == expected.as_str());
        let (_, hex) = matches.next()?;
        if matches.next().is_some() { return None; }
        crate::session_listener::decode_hex(hex.trim())
    }

    let (owner_locator, owner_node) = target(owner)?;
    let (hub_locator, hub_node) = target(hub)?;
    if owner_node == hub_node || owner_locator.host() == hub_locator.host() {
        return Err("Fenced owner status requires distinct owner and Hub identities and Onion services".into());
    }

    let home = crate::home::resolve_home(home_override)?;
    let profile = crate::session_serve_remote::remote_state_dir(&home)?;
    let pins = crate::session_serve_remote::read_private_peer_pins(&profile)
        .map_err(|_| "Fenced owner status: profile peer pins unavailable or ambiguous")?;
    let local = load_node_identity(&home).await
        .map_err(|_| "Fenced owner status: active enrolled profile identity unavailable")?;
    let owner_key = exact_pin(&pins, &owner_node).ok_or("Fenced owner status: owner key is not pinned in this profile")?;
    let hub_key = exact_pin(&pins, &hub_node).ok_or("Fenced owner status: Hub key is not pinned in this profile")?;
    if &owner_node == local.node_id() || &hub_node == local.node_id()
        || owner_key == hub_key || owner_key == local.identity.public_key
        || hub_key == local.identity.public_key || node_key_fingerprint(&owner_key).is_none()
        || node_key_fingerprint(&hub_key).is_none()
    {
        return Err("Fenced owner status: device, owner and Hub must have distinct valid node keys".into());
    }
    let mut owner_pins = PinnedPeers::new();
    owner_pins.pin(owner_node.clone(), owner_key).map_err(|_| "Fenced owner status: invalid owner key pin")?;
    let mut hub_pins = PinnedPeers::new();
    hub_pins.pin(hub_node.clone(), hub_key).map_err(|_| "Fenced owner status: invalid Hub key pin")?;
    let owner_verifier = TranscriptWrappedVerifier::new(owner_pins);
    let hub_verifier = TranscriptWrappedVerifier::new(hub_pins);
    let device = OwnerStatusDevice::from_enrolled_node(&local)
        .map_err(|_| "Fenced owner status: invalid active device key")?;

    let ensure_same_profile = || -> Result<(), String> {
        let current = crate::session_serve_remote::remote_state_dir(&home)?;
        if current != profile
            || !crate::session_serve_remote::read_private_peer_pins(&profile).is_ok_and(|current| current == pins)
        {
            return Err("Fenced owner status: profile or peer pins changed during the operation".into());
        }
        Ok(())
    };
    ensure_same_profile()?;

    let tor_root = profile.join("attach-tor-client");
    let state = tor_root.join("state");
    let cache = tor_root.join("cache");
    for path in [&tor_root, &state, &cache] {
        crate::session_serve::private_dir(path)?;
    }
    let client = OnionClient::launch(&OnionClientConfig::new(state, cache))
        .map_err(|_| "Fenced owner status: Onion client unavailable")?;
    let timeout = Duration::from_secs(120);
    let (owner_io, hub_io) = tokio::try_join!(
        client.dial(&owner_locator, timeout),
        client.dial(&hub_locator, timeout),
    ).map_err(|_| "Fenced owner status: a pinned Onion link could not be reached; no fallback")?;
    let transport = ClientOptions {
        handshake_timeout: Duration::from_secs(30),
        ..ClientOptions::default()
    };
    let mut options = ConnectOptions::new("harw gateway control fenced-owner-status");
    options.limits.hello_timeout = options.limits.hello_timeout.max(Duration::from_secs(60));
    let (mut owner_link, mut hub_link) = tokio::try_join!(
        device.connect_owner_over(owner_io, &owner_node, &owner_verifier, options, &transport),
        device.connect_hub_over(hub_io, &hub_node, &hub_verifier, &transport),
    ).map_err(|_| "Fenced owner status: a pinned peer could not be authenticated or refused Observe")?;
    ensure_same_profile()?;

    // Arm both authenticated Onion links before starting the short owner
    // reservation. Never auto-repeat an ambiguous reservation or fall back to V1.
    let status = owner_link.reserve(session).await
        .map_err(|_| "Fenced owner status: owner reservation unavailable")?
        .read(&mut hub_link).await
        .map_err(|_| "Fenced owner status: the owner and Hub did not confirm the status")?;
    ensure_same_profile()?;
    drop((owner_link, hub_link, client));
    Ok(status)
}

#[cfg(feature = "onion")]
async fn connect_onion_node(
    alias: &str, endpoint: &str, expected_node: &str,
    local: &LocalNode, verifier: &dyn NodeVerifier, mut options: ConnectOptions,
    profile: &Path,
) -> Result<harw_session_remote::RemoteConnection, String> {
    use std::time::Duration;
    use harw_onion::{OnionClient, OnionClientConfig, OnionLocator};

    let locator = endpoint.parse::<OnionLocator>()
        .map_err(|_| format!("Host-Alias `{alias}`: ungültiger v3-.onion-Locator; kein Direktrückfall"))?;
    let node = NodeId::try_from_str(expected_node)
        .map_err(|e| format!("Host-Alias `{alias}` node id: {e}"))?;
    let tor_root = profile.join("attach-tor-client");
    let state = tor_root.join("state");
    let cache = tor_root.join("cache");
    for path in [&tor_root, &state, &cache] {
        crate::session_serve::private_dir(path)?;
    }
    let client = OnionClient::launch(&OnionClientConfig::new(state, cache))
        .map_err(|_| format!("Host-Alias `{alias}`: Tor-Client nicht verfügbar; kein Direktrückfall"))?;
    let stream = client.dial(&locator, Duration::from_secs(120)).await
        .map_err(|_| format!("Host-Alias `{alias}`: Onion-Verbindung fehlgeschlagen; kein Direktrückfall"))?;
    options.limits.hello_timeout = options.limits.hello_timeout.max(Duration::from_secs(60));
    let transport = harw_node_transport::ClientOptions {
        handshake_timeout: Duration::from_secs(30),
        ..harw_node_transport::ClientOptions::default()
    };
    harw_session_remote::conn::connect_node_over(
        CliOnionStream { stream, _client: client }, &node, local, verifier, options, &transport,
    ).await.map_err(|error| format!("Host-Alias `{alias}`: authentifizierte Onion-Verbindung fehlgeschlagen: {error}"))
}

#[cfg(not(feature = "onion"))]
async fn connect_onion_node(
    alias: &str, _endpoint: &str, _expected_node: &str,
    _local: &LocalNode, _verifier: &dyn NodeVerifier, _options: ConnectOptions,
    _profile: &Path,
) -> Result<harw_session_remote::RemoteConnection, String> {
    Err(format!("Host-Alias `{alias}`: dieses harw enthält keinen Tor-Client; kein Direktrückfall"))
}

/// Retain the embedded client's storage/runtime for the live HTTP/WS tunnel.
#[cfg(feature = "onion")]
struct CliOnionStream {
    stream: harw_onion::OnionStream,
    _client: harw_onion::OnionClient,
}

#[cfg(feature = "onion")]
impl tokio::io::AsyncRead for CliOnionStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncRead::poll_read(self.get_mut().stream.as_mut(), cx, buf)
    }
}

#[cfg(feature = "onion")]
impl tokio::io::AsyncWrite for CliOnionStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(self.get_mut().stream.as_mut(), cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(self.get_mut().stream.as_mut(), cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(self.get_mut().stream.as_mut(), cx)
    }
}

pub(crate) async fn connect_authenticated_node(
    alias: &str,
    endpoint: &str,
    expected_node: &str,
    local: &LocalNode,
    verifier: &dyn NodeVerifier,
    options: ConnectOptions,
) -> Result<harw_session_remote::RemoteConnection, String> {
    if is_onion_endpoint(endpoint) {
        return Err("Onion-Locators are never forwarded to direct TCP or DNS.".into());
    }
    let expected_node = NodeId::try_from_str(expected_node)
        .map_err(|e| format!("Host-Alias `{alias}` node id: {e}"))?;
    let addr = tokio::net::lookup_host(endpoint)
        .await
        .map_err(|e| format!("Host-Alias `{alias}`: {e}"))?
        .next()
        .ok_or_else(|| format!("Host-Alias `{alias}`: keine Adresse"))?;
    connect_node(
        NodeEndpoint {
            addr,
            expected_node,
        },
        local,
        verifier,
        options,
    )
    .await
    .map_err(|e| format!("Verbindung zu Host-Alias `{alias}` fehlgeschlagen: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(socket: Option<&str>, host: Option<&str>) -> AttachArgs {
        AttachArgs {
            session: None,
            socket: socket.map(PathBuf::from),
            host: host.map(str::to_owned),
        }
    }

    #[test]
    fn explicit_socket_wins() {
        assert_eq!(
            resolve_target(&args(Some("/tmp/x.sock"), None)),
            Ok(Target::Unix(PathBuf::from("/tmp/x.sock")))
        );
    }

    #[test]
    fn default_socket_needs_runtime_dir() {
        assert_eq!(
            default_socket(Some("/run/user/1")),
            Ok(PathBuf::from("/run/user/1/harw/session.sock"))
        );
        assert!(default_socket(None).is_err_and(|m| m.contains("--socket")));
        assert!(default_socket(Some("")).is_err());
    }

    #[test]
    fn state_dir_precedence() {
        assert_eq!(
            state_dir(Some("/s"), Some("/x"), Some("/h")),
            Ok(PathBuf::from("/s"))
        );
        assert_eq!(
            state_dir(None, Some("/x"), Some("/h")),
            Ok(PathBuf::from("/x/harw"))
        );
        assert_eq!(
            state_dir(None, None, Some("/h")),
            Ok(PathBuf::from("/h/.local/state/harw"))
        );
        assert!(state_dir(None, None, None).is_err());
    }

    #[test]
    fn node_identity_and_authhub_come_from_selected_home_not_untrusted_project()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let project = tempfile::tempdir()?;
        std::fs::write(
            home.path().join("config.toml"),
            "[session_listener]\nnode_id = \"trusted-client\"\n[infrastructure]\nauth_socket = \"/trusted/auth.sock\"\n",
        )?;
        let project_config = project.path().join(".harw");
        std::fs::create_dir(&project_config)?;
        std::fs::write(
            project_config.join("config.toml"),
            "[session_listener]\nnode_id = \"attacker\"\n[infrastructure]\nauth_socket = \"/attacker/auth.sock\"\n",
        )?;
        let config = load_node_config(home.path(), project.path())?;
        assert_eq!(
            config.harness.session_listener.node_id.as_deref(),
            Some("trusted-client")
        );
        assert_eq!(
            config.infrastructure.and_then(|i| i.auth_socket),
            Some(PathBuf::from("/trusted/auth.sock"))
        );
        Ok(())
    }

    #[test]
    fn unix_alias_and_invalid_endpoint_are_validated() {
        let record = HostAlias {
            alias: "local".to_owned(),
            endpoint: "unix:/run/harw.sock".to_owned(),
            expected_node: None,
        };
        assert_eq!(
            target_from_record(&record),
            Ok(Target::Unix(PathBuf::from("/run/harw.sock")))
        );
        let mut invalid = record;
        invalid.endpoint = "ftp://x".to_owned();
        assert!(target_from_record(&invalid).is_err());
        invalid.endpoint = "unix:".to_owned();
        assert!(target_from_record(&invalid).is_err());
    }

    #[test]
    fn node_alias_retains_required_identity_pin() {
        let record = harw_session_remote::alias::HostAlias {
            alias: "lab".to_owned(),
            endpoint: "node-direct:127.0.0.1:7443".to_owned(),
            expected_node: Some("gateway-a".to_owned()),
        };
        assert_eq!(
            target_from_record(&record),
            Ok(Target::Node {
                alias: "lab".to_owned(),
                endpoint: "127.0.0.1:7443".to_owned(),
                expected_node: "gateway-a".to_owned(),
                route: NodeRoute::DirectOptIn,
            })
        );
        let mut missing = record;
        missing.expected_node = None;
        assert!(target_from_record(&missing).is_err());
    }

    #[test]
    fn unreachable_socket_is_an_error_not_a_panic() {
        let result = run(
            &args(Some("/nonexistent/harw-attach-test.sock"), None),
            None,
        );
        assert!(result.is_err());
    }
}

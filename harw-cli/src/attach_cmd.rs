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
    let endpoint = record
        .endpoint
        .strip_prefix("node:")
        .ok_or_else(|| "invalid node endpoint".to_owned())?;
    let expected_node = record
        .expected_node
        .clone()
        .ok_or_else(|| "node identity pin missing".to_owned())?;
    Ok(Target::Node {
        alias: record.alias.clone(),
        endpoint: endpoint.to_owned(),
        expected_node,
    })
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn resolve_target(args: &AttachArgs) -> Result<Target, String> {
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Runtime konnte nicht gestartet werden: {e}"))?;
    runtime.block_on(async move {
        let options = ConnectOptions::new("harw attach");
        let connection = match target {
            Target::Unix(path) => connect_unix(path.clone(), options)
                .await
                .map_err(|e| format!("Verbindung zu {} fehlgeschlagen: {e}", path.display()))?,
            Target::Node {
                alias,
                endpoint,
                expected_node,
            } => {
                connect_node_target(&alias, &endpoint, &expected_node, home_override, options)
                    .await?
            }
        };
        let port: Arc<dyn SessionPort> = Arc::new(RemotePort::new(connection));
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
    home_override: Option<PathBuf>,
    options: ConnectOptions,
) -> Result<harw_session_remote::RemoteConnection, String> {
    let home = crate::home::resolve_home(home_override)?;
    let cwd = std::env::current_dir().map_err(|e| format!("Arbeitsverzeichnis: {e}"))?;
    let config = load_node_config(&home, &cwd)?;
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
    let local = LocalNode::new(
        NodeIdentity {
            node_id: node.clone(),
            public_key,
            key_ref: format!("harw.node-identity/{}", node.as_str()),
        },
        Arc::new(signer),
    );
    let peers_path = crate::session_serve_remote::remote_state_dir(&home)?
        .join(crate::session_listener::PEERS_FILE);
    let text = std::fs::read_to_string(&peers_path)
        .map_err(|e| format!("Peer-Pins {}: {e}", peers_path.display()))?;
    let (peers, _) = crate::session_listener::parse_peers(&text);
    let verifier = TranscriptWrappedVerifier::new(peers);
    connect_authenticated_node(alias, endpoint, expected_node, &local, &verifier, options).await
}

pub(crate) async fn connect_authenticated_node(
    alias: &str,
    endpoint: &str,
    expected_node: &str,
    local: &LocalNode,
    verifier: &dyn NodeVerifier,
    options: ConnectOptions,
) -> Result<harw_session_remote::RemoteConnection, String> {
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
            endpoint: "node:127.0.0.1:7443".to_owned(),
            expected_node: Some("gateway-a".to_owned()),
        };
        assert_eq!(
            target_from_record(&record),
            Ok(Target::Node {
                alias: "lab".to_owned(),
                endpoint: "127.0.0.1:7443".to_owned(),
                expected_node: "gateway-a".to_owned(),
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

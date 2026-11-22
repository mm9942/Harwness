//! Peer-Bridge-Verdrahtung für `harw serve-mcp`.
//!
//! # Zweck
//! Baut aus der aufgelösten `[peer_bridge]`-Konfiguration die dateibasierte
//! Peer-Bridge ([`FilePeerBridge`]), die der MCP-Listener für die Tools
//! `harw_peer_sessions`, `harw_peer_send` und `harw_peer_poll` nutzt.
//!
//! # Fehler
//! Ein Peer, dessen Principal kein konfigurierter `[[mcp_listener.principals]]`
//! ist, oder eine ungültige Peer-Id führt zu einem `Err(String)`; der Aufrufer
//! bricht den Start damit ab (fail closed).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harw_config::{PeerConnectorToml, ResolvedConfig};
use harw_mcp_server::{FilePeerBridge, McpPeerBridge, PeerBridgeServerConfig, PeerServerConfig};
use harw_session_store::peer_bridge::PEER_BRIDGE_DIR;

/// Baut die Peer-Bridge, falls `peer_bridge.enabled` gesetzt ist.
///
/// `storage_root` ist das Profilverzeichnis; die Bridge liegt darunter in
/// `peer-bridge/`.
///
/// # Errors
/// Wenn ein Peer-Principal nicht in `mcp_listener.principals` steht, ein
/// Limit nicht in `usize` passt oder [`FilePeerBridge::new`] die Konfiguration
/// ablehnt.
pub(crate) fn build_peer_bridge(
    config: &ResolvedConfig,
    storage_root: &Path,
) -> Result<Option<Arc<dyn McpPeerBridge>>, String> {
    let section = &config.harness.peer_bridge;
    if !section.enabled {
        return Ok(None);
    }
    let mut peers = Vec::with_capacity(section.peers.len());
    for peer in &section.peers {
        let known = config
            .harness
            .mcp_listener
            .principals
            .iter()
            .any(|p| p.id == peer.principal);
        if !known {
            return Err(format!(
                "peer_bridge.peers[].principal '{}' is not a configured mcp_listener principal",
                peer.principal
            ));
        }
        peers.push(PeerServerConfig {
            id: peer.id.clone(),
            principal: peer.principal.clone(),
            wake: peer.wake,
            wake_burst: peer.wake_burst,
            wake_interval: Duration::from_secs(peer.wake_interval_secs),
            max_sends_per_minute: peer.max_sends_per_minute,
            mcp_poll: peer.has_connector(PeerConnectorToml::Mcp),
        });
    }
    let max_bytes = usize::try_from(section.max_bytes)
        .map_err(|_| "peer_bridge.max_bytes does not fit into usize".to_owned())?;
    let queue_cap = usize::try_from(section.queue_cap)
        .map_err(|_| "peer_bridge.queue_cap does not fit into usize".to_owned())?;
    let bridge = FilePeerBridge::new(PeerBridgeServerConfig {
        root: storage_root.join(PEER_BRIDGE_DIR),
        max_bytes,
        queue_cap,
        peers,
    })?;
    Ok(Some(Arc::new(bridge)))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{TestResult, ctx, some_or};
    use harw_config::{
        HarnessConfig, McpListenerSection, McpPrincipalToml, PeerBridgeSection, PeerToml,
        SecretRef,
    };

    fn config(enabled: bool, listener_ids: &[&str], peer_principal: &str) -> ResolvedConfig {
        ResolvedConfig {
            harness: HarnessConfig {
                mcp_listener: McpListenerSection {
                    principals: listener_ids
                        .iter()
                        .copied()
                        .map(|id| McpPrincipalToml {
                            id: id.to_owned(),
                            credential_ref: SecretRef::Env("HARW_TEST_MCP_TOKEN".to_owned()),
                            tenant: "tenant-a".to_owned(),
                            workspace: "workspace-a".to_owned(),
                            job_capabilities: Vec::new(),
                        })
                        .collect(),
                    ..Default::default()
                },
                peer_bridge: PeerBridgeSection {
                    enabled,
                    peers: vec![PeerToml {
                        id: "peer-a".to_owned(),
                        principal: peer_principal.to_owned(),
                        connectors: vec![],
                        webhook_url: None,
                        secret_ref: None,
                        wake: true,
                        wake_burst: 3,
                        wake_interval_secs: 30,
                        max_sends_per_minute: 30,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_build_peer_bridge_disabled_returns_none() -> TestResult {
        let root = std::env::temp_dir().join("harw-peer-bridge-test");
        let bridge = build_peer_bridge(&config(false, &["alice"], "alice"), &root)
            .map_err(ctx("build_peer_bridge"))?;
        assert!(bridge.is_none());
        Ok(())
    }

    #[test]
    fn test_build_peer_bridge_unknown_principal_is_rejected() {
        let root = std::env::temp_dir().join("harw-peer-bridge-test");
        let result = build_peer_bridge(&config(true, &["alice"], "ghost"), &root);
        assert!(matches!(
            result,
            Err(ref e) if e.contains("not a configured mcp_listener principal")
        ));
    }

    #[test]
    fn test_build_peer_bridge_valid_principal_is_peer() -> TestResult {
        let root = std::env::temp_dir().join("harw-peer-bridge-test");
        let bridge = build_peer_bridge(&config(true, &["alice"], "alice"), &root)
            .map_err(ctx("build_peer_bridge"))?;
        let bridge = some_or(bridge, "bridge")?;
        assert!(bridge.is_peer("alice"));
        Ok(())
    }
}

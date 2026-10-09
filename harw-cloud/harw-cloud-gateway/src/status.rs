//! Runtime status probing for the cloud gateway.

use crate::config::GatewayConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Probe timeout for a single TCP connect.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Listening status of a single mapped port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    /// Logical name of the mapped service.
    pub name: String,
    /// Port probed.
    pub port: u16,
    /// Whether a listener accepted the connection.
    pub listening: bool,
}

/// Aggregate status of the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    /// Whether the gateway reports at least one listening port.
    pub running: bool,
    /// Per-port probe results.
    pub ports: Vec<PortStatus>,
}

/// Probe all mapped ports of the given gateway configuration.
///
/// Each port is probed with a short TCP connect against `127.0.0.1`;
/// `running` is true if at least one port is listening.
pub async fn probe(config: &GatewayConfig) -> CloudStatus {
    let mut ports = Vec::with_capacity(config.ports.len());
    for mapping in &config.ports {
        let listening = match timeout(
            PROBE_TIMEOUT,
            TcpStream::connect(("127.0.0.1", mapping.port)),
        )
        .await
        {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        };
        ports.push(PortStatus {
            name: mapping.name.clone(),
            port: mapping.port,
            listening,
        });
    }
    let running = ports.iter().any(|p| p.listening);
    CloudStatus { running, ports }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PortMap;

    #[tokio::test]
    async fn closed_port_is_not_listening() {
        // Bind a listener, take its port, then drop the listener so the
        // port is (very likely) closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let config = GatewayConfig {
            listen_addr: "127.0.0.1:0".into(),
            tls: None,
            ports: vec![PortMap {
                name: "probe".into(),
                port,
            }],
        };
        let status = probe(&config).await;
        assert!(!status.running);
        assert_eq!(status.ports.len(), 1);
        assert_eq!(status.ports[0].name, "probe");
        assert!(!status.ports[0].listening);
    }

    #[tokio::test]
    async fn open_port_is_listening() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let config = GatewayConfig {
            listen_addr: format!("127.0.0.1:{port}"),
            tls: None,
            ports: vec![PortMap {
                name: "api".into(),
                port,
            }],
        };
        let status = probe(&config).await;
        assert!(status.running);
        assert!(status.ports[0].listening);
    }
}

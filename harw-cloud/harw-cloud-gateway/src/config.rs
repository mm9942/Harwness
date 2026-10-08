//! Gateway configuration types and YAML loading.

use serde::{Deserialize, Serialize};

/// Paths to TLS material for the gateway listener.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsPaths {
    /// Path to the PEM certificate chain.
    pub cert_path: String,
    /// Path to the PEM private key.
    pub key_path: String,
}

/// A named upstream port mapped by the gateway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortMap {
    /// Logical name of the mapped service.
    pub name: String,
    /// Port the gateway listens on for this service.
    pub port: u16,
}

/// Top-level gateway configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    /// Address (host:port) the gateway listens on.
    pub listen_addr: String,
    /// Optional TLS material.
    pub tls: Option<TlsPaths>,
    /// Ports mapped by the gateway.
    pub ports: Vec<PortMap>,
}

impl GatewayConfig {
    /// Parse a gateway configuration from a YAML string.
    pub fn load_from_str(s: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_from_str_parses_example() {
        let yaml = r"
listenAddr: 127.0.0.1:8443
tls:
  certPath: /etc/harw/cert.pem
  keyPath: /etc/harw/key.pem
ports:
  - name: api
    port: 8443
  - name: enrollment
    port: 8444
";
        let config = GatewayConfig::load_from_str(yaml).unwrap();
        assert_eq!(config.listen_addr, "127.0.0.1:8443");
        let tls = config.tls.expect("tls");
        assert_eq!(tls.cert_path, "/etc/harw/cert.pem");
        assert_eq!(tls.key_path, "/etc/harw/key.pem");
        assert_eq!(config.ports.len(), 2);
        assert_eq!(config.ports[0].name, "api");
        assert_eq!(config.ports[0].port, 8443);
        assert_eq!(config.ports[1].name, "enrollment");
        assert_eq!(config.ports[1].port, 8444);
    }

    #[test]
    fn load_from_str_without_tls() {
        let yaml = "listenAddr: 0.0.0.0:9000\nports:\n  - name: probe\n    port: 9000\n";
        let config = GatewayConfig::load_from_str(yaml).unwrap();
        assert!(config.tls.is_none());
        assert_eq!(config.ports.len(), 1);
    }
}

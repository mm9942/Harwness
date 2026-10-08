//! Harwness cloud gateway: configuration loading and status probing.
//!
//! The optional `proxy` feature enables the pingora-based reverse proxy.

pub mod config;
pub mod status;

#[cfg(feature = "proxy")]
pub mod proxy;

#[cfg(test)]
mod tests {
    #[test]
    fn modules_are_wired() {
        let yaml = "listenAddr: 127.0.0.1:0\nports: []\n";
        let cfg = crate::config::GatewayConfig::load_from_str(yaml).unwrap();
        assert!(cfg.ports.is_empty());
        assert!(cfg.tls.is_none());
    }
}

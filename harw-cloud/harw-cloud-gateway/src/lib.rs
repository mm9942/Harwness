//! Harwness cloud gateway: configuration, status probing and root-hub routing prerequisites.
//!
//! The optional `proxy` feature exposes a fail-closed Pingora proxy placeholder;
//! forwarding is not implemented.

pub mod config;
#[cfg(all(unix, feature = "hub-owner-liveness"))]
pub mod owner_ingress;
#[cfg(unix)]
pub mod owner_directory;
#[cfg(all(unix, feature = "hub-owner-create-receipt"))]
pub mod owner_create_receipt;
#[cfg(all(target_os = "linux", feature = "hub-owner-readiness"))]
pub mod owner_readiness;
#[cfg(all(target_os = "linux", feature = "hub-owner-readiness"))]
pub mod owner_readiness_listener;
#[cfg(all(unix, feature = "hub-owner-liveness"))]
pub mod owner_liveness;
#[cfg(all(target_os = "linux", feature = "hub-owner-netsec"))]
pub mod owner_netsec;
#[cfg(all(unix, feature = "hub-owner-liveness"))]
mod owner_report;
#[cfg(all(unix, feature = "hub-owner-liveness"))]
pub mod owner_report_client;
#[cfg(all(unix, feature = "hub-owner-process-proof"))]
pub mod owner_process_proof;
#[cfg(all(unix, feature = "hub-owner-process-proof"))]
pub mod owner_direct_link;
#[cfg(all(unix, feature = "hub-owner-process-proof"))]
pub mod owner_placement;
#[cfg(all(unix, feature = "hub-owner-process-proof"))]
pub mod owner_session_grants;
#[cfg(all(unix, feature = "hub-owner-placement-listener"))]
pub mod owner_placement_listener;
pub mod route_admission;
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

#[cfg(all(unix, feature = "hub-owner-placement-listener"))]
pub mod owner_proof_exchange;

//! `cloud.status` — probe the local cloud stack (gateway ports + systemd
//! unit state of `harw.cloud.service`).

use std::process::Command;

use harw_cloud_gateway::config::{GatewayConfig, PortMap};
use harw_cloud_gateway::status::{self as gateway_status, CloudStatus};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::json;

/// Arguments of `cloud.status` (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudStatusArgs {}

/// Build the local gateway configuration for the cloud stack probe.
fn local_gateway_config() -> GatewayConfig {
    GatewayConfig {
        listen_addr: "127.0.0.1:7443".into(),
        tls: None,
        ports: vec![
            PortMap {
                name: "api".into(),
                port: 7443,
            },
            PortMap {
                name: "enrollment".into(),
                port: 8080,
            },
        ],
    }
}

/// Query whether the `harw.cloud.service` systemd unit is active.
fn systemd_active() -> String {
    Command::new("systemctl")
        .args(["is-active", "harw.cloud.service"])
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_ascii_lowercase();
            if text.is_empty() {
                "unknown".into()
            } else {
                text
            }
        })
        .unwrap_or_else(|_| "unknown".into())
}

/// Render a probe result as JSON.
fn status_json(status: &CloudStatus, systemd: &str) -> serde_json::Value {
    let ports: Vec<serde_json::Value> = status
        .ports
        .iter()
        .map(|p| json!({ "name": p.name, "port": p.port, "listening": p.listening }))
        .collect();
    json!({
        "running": status.running,
        "ports": ports,
        "systemd": systemd,
    })
}

/// Probe the local cloud stack: gateway ports on `127.0.0.1` and the
/// `harw.cloud.service` systemd unit state.
///
/// # Errors
/// [`OpError::Execution`] is never returned by the probe itself; port checks
/// are reported as `listening: false`, not as errors.
#[operation(
    name = "cloud.status",
    summary = "Probe the local cloud stack: gateway ports (127.0.0.1:7443 and :8080) and the state of the harw.cloud.service systemd unit.",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none")
)]
async fn cloud_status(_ctx: &OpContext, _args: CloudStatusArgs) -> Result<OpOutput, OpError> {
    let config = local_gateway_config();
    let status = gateway_status::probe(&config).await;
    let systemd = systemd_active();
    let data = status_json(&status, &systemd);
    let running = if status.running { "running" } else { "stopped" };
    Ok(OpOutput {
        text: format!(
            "Cloud stack: {running} (ports: {}, systemd: {systemd})",
            status
                .ports
                .iter()
                .map(|p| format!("{}:{}={}", p.name, p.port, if p.listening { "up" } else { "down" }))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_port_reports_not_running() {
        // Probe an ephemeral local port that is (almost certainly) closed.
        let config = GatewayConfig {
            listen_addr: "127.0.0.1:1".into(),
            tls: None,
            ports: vec![PortMap { name: "api".into(), port: 1 }],
        };
        let status = gateway_status::probe(&config).await;
        assert!(!status.running);
        assert_eq!(status.ports.len(), 1);
        assert!(!status.ports[0].listening);
    }

    #[test]
    fn systemd_active_never_panics() {
        // May be "unknown" in a sandbox without systemctl; must not panic.
        let value = systemd_active();
        assert!(!value.is_empty());
    }
}

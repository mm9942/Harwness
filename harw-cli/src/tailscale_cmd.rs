//! `harw tailscale status`: shows whether `tailscaled` is reachable and
//! connected, this node's MagicDNS name and tailnet addresses, and how to
//! open the control plane to the tailnet (`harw web --tailnet`).

use crate::cli::TailscaleAction;

/// Runs `harw tailscale …`.
///
/// # Errors
/// A message when `tailscaled` is not reachable or the runtime cannot start.
pub(crate) fn run(action: TailscaleAction) -> Result<(), String> {
    match action {
        TailscaleAction::Status => status(),
    }
}

fn status() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start runtime: {error}"))?;
    let api = harw_tailscale::LocalApi::discover().map_err(|error| error.to_string())?;
    let status = runtime
        .block_on(api.status())
        .map_err(|error| error.to_string())?;
    println!("{}", render(api.socket(), &status));
    Ok(())
}

fn render(socket: &std::path::Path, status: &harw_tailscale::Status) -> String {
    let ips = if status.ips.is_empty() {
        "-".to_owned()
    } else {
        status
            .ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = vec![
        format!("tailscaled: {}", socket.display()),
        format!("state:      {}", status.backend_state),
        format!(
            "node:       {}",
            if status.dns_name.is_empty() {
                "-"
            } else {
                &status.dns_name
            }
        ),
        format!("addresses:  {ips}"),
    ];
    if let Some(tailnet) = &status.tailnet {
        lines.push(format!("tailnet:    {tailnet}"));
    }
    if status.is_running() {
        lines.push(format!(
            "Open the control plane to your tailnet: harw web --tailnet (port {})",
            harw_tailscale::DEFAULT_TAILNET_PORT
        ));
    } else {
        lines.push("Not connected: run `tailscale up` first.".to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn render_names_node_addresses_and_next_step() -> Result<(), String> {
        let status = harw_tailscale::localapi::parse_status(
            br#"{"BackendState":"Running","Self":{"DNSName":"vps.tail1234.ts.net.","TailscaleIPs":["100.101.1.2"]}}"#,
        )
        .map_err(|error| error.to_string())?;
        let text = render(
            std::path::Path::new("/run/tailscale/tailscaled.sock"),
            &status,
        );
        assert!(text.contains("node:       vps.tail1234.ts.net"), "{text}");
        assert!(text.contains("addresses:  100.101.1.2"), "{text}");
        assert!(text.contains("harw web --tailnet"), "{text}");

        let stopped = harw_tailscale::localapi::parse_status(br#"{"BackendState":"Stopped"}"#)
            .map_err(|error| error.to_string())?;
        let text = render(std::path::Path::new("/x"), &stopped);
        assert!(text.contains("tailscale up"), "{text}");
        Ok(())
    }
}

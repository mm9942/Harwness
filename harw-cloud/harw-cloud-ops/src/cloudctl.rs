//! `cloud.cloudctl.*` — control plane of the cloud home stack: thin wrappers
//! around the `harw-ctl` binary (`up`, `down`, `restart`, `enroll`, `revoke`).

use std::process::Command;

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::json;

/// Arguments of the `cloud.cloudctl.*` operations (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudctlArgs {}

/// Run `harw-ctl <subcommand>` and render its result.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing;
/// [`OpError::Execution`] when the command fails to run or exits non-zero.
fn run_cloudctl(sub: &str) -> Result<OpOutput, OpError> {
    let output = Command::new("harw-ctl").arg(sub).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            OpError::NotAvailable("harw-ctl binary not found in PATH".into())
        } else {
            OpError::Execution(format!("failed to spawn harw-ctl {sub}: {err}"))
        }
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(OpError::Execution(format!(
            "harw-ctl {sub} failed with status {}: {stderr}",
            output.status.code().unwrap_or(-1)
        )));
    }
    Ok(OpOutput {
        text: format!("cloudctl {sub}: {stdout}"),
        data: Some(json!({ "subcommand": sub, "output": stdout })),
    })
}

/// Start the cloud stack via `harw-ctl up`.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing.
#[operation(
    name = "cloud.cloudctl.up",
    summary = "Start the cloud home stack via the harw-ctl binary (subcommand up).",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_up(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl("up")
}

/// Stop the cloud stack via `harw-ctl down`.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing.
#[operation(
    name = "cloud.cloudctl.down",
    summary = "Stop the cloud home stack via the harw-ctl binary (subcommand down).",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_down(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl("down")
}

/// Restart the cloud stack via `harw-ctl restart`.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing.
#[operation(
    name = "cloud.cloudctl.restart",
    summary = "Restart the cloud home stack via the harw-ctl binary (subcommand restart).",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_restart(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl("restart")
}

/// Enroll a device into the cloud stack via `harw-ctl enroll`.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing.
#[operation(
    name = "cloud.cloudctl.enroll",
    summary = "Enroll a device into the cloud home stack via the harw-ctl binary (subcommand enroll).",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_enroll(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl("enroll")
}

/// Revoke a device enrollment via `harw-ctl revoke`.
///
/// # Errors
/// [`OpError::NotAvailable`] when the `harw-ctl` binary is missing.
#[operation(
    name = "cloud.cloudctl.revoke",
    summary = "Revoke a device enrollment of the cloud home stack via the harw-ctl binary (subcommand revoke).",
    domain = "execution",
    permission = "maintainer",
    model_tool(approval = "always")
)]
async fn cloudctl_revoke(_ctx: &OpContext, _args: CloudctlArgs) -> Result<OpOutput, OpError> {
    run_cloudctl("revoke")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_cloudctl_unknown_subcommand_yields_defined_error() {
        match run_cloudctl("definitely-not-a-subcommand") {
            Ok(_) | Err(OpError::NotAvailable(_)) | Err(OpError::Execution(_)) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }
}

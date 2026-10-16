//! `cloud.enrollments.list` — list device enrollments recorded in the local
//! `node-devices.conf` file.

use std::path::PathBuf;

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use serde::Deserialize;
use serde_json::json;

/// Arguments of `cloud.enrollments.list` (none).
#[derive(Debug, Default, Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct CloudEnrollmentsListArgs {}

/// One enrollment row parsed from `node-devices.conf`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnrollmentRow {
    device_id: String,
    node_id: String,
    tenant: String,
    tier: String,
    status: String,
    label: String,
}

impl EnrollmentRow {
    /// Parse a single `device_id|node_id|tenant|tier|status|label` line.
    fn parse(line: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 6 {
            return None;
        }
        Some(Self {
            device_id: fields[0].trim().to_string(),
            node_id: fields[1].trim().to_string(),
            tenant: fields[2].trim().to_string(),
            tier: fields[3].trim().to_string(),
            status: fields[4].trim().to_string(),
            label: fields[5].trim().to_string(),
        })
    }

    /// Serialize as JSON object.
    fn to_json(&self) -> serde_json::Value {
        json!({
            "device_id": self.device_id,
            "node_id": self.node_id,
            "tenant": self.tenant,
            "tier": self.tier,
            "status": self.status,
            "label": self.label,
        })
    }
}

/// Path of the enrollment file: `$HARW_HOME/profiles/default/session-host/
/// remote/node-devices.conf`, falling back to `~/.harw/…` when `HARW_HOME`
/// is unset.
fn node_devices_path() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("HARW_HOME") {
        return Some(
            PathBuf::from(home).join("profiles/default/session-host/remote/node-devices.conf"),
        );
    }
    std::env::var("HOME").ok().map(|home| {
        PathBuf::from(home).join(".harw/profiles/default/session-host/remote/node-devices.conf")
    })
}

/// Read enrollments from the given path. A missing file yields an empty list;
/// malformed lines are skipped.
fn read_enrollments(path: &std::path::Path) -> Result<Vec<EnrollmentRow>, OpError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(OpError::Execution(format!(
                "failed to read {}: {err}",
                path.display()
            )));
        }
    };
    Ok(content
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(EnrollmentRow::parse)
        .collect())
}

/// List device enrollments recorded in `node-devices.conf`.
///
/// # Errors
/// [`OpError::Execution`] when the file exists but cannot be read; a missing
/// file yields an empty enrollment list.
#[operation(
    name = "cloud.enrollments.list",
    summary = "List the cloud device enrollments recorded in the local node-devices.conf file (device id, node id, tenant, tier, status, label).",
    domain = "execution",
    permission = "maintainer",
    model_tool(readonly, approval = "none")
)]
async fn cloud_enrollments_list(
    _ctx: &OpContext,
    _args: CloudEnrollmentsListArgs,
) -> Result<OpOutput, OpError> {
    let path = node_devices_path()
        .ok_or_else(|| OpError::Execution("cannot resolve home directory".into()))?;
    let rows = read_enrollments(&path)?;
    let entries: Vec<serde_json::Value> = rows.iter().map(EnrollmentRow::to_json).collect();
    Ok(OpOutput {
        text: format!("Cloud enrollments: {}", entries.len()),
        data: Some(json!({ "enrollments": entries })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_row() {
        let row = EnrollmentRow::parse("dev-1|node-42|acme|premium|active|Laptop").unwrap();
        assert_eq!(row.device_id, "dev-1");
        assert_eq!(row.node_id, "node-42");
        assert_eq!(row.tenant, "acme");
        assert_eq!(row.tier, "premium");
        assert_eq!(row.status, "active");
        assert_eq!(row.label, "Laptop");
    }

    #[test]
    fn parse_rejects_wrong_field_count() {
        assert!(EnrollmentRow::parse("dev-1|node-42|acme").is_none());
        assert!(EnrollmentRow::parse("a|b|c|d|e|f|g").is_none());
        assert!(EnrollmentRow::parse("").is_none());
    }

    #[test]
    fn missing_file_yields_empty_list() {
        let rows =
            read_enrollments(std::path::Path::new("/nonexistent/node-devices.conf")).unwrap();
        assert!(rows.is_empty());
    }
}

//! Read-only access to the same workload policy parser as runtime placement.
//! A valid file is a configuration ceiling, not live availability or authority.

use std::io::Read;
use std::path::Path;

use harw_fsutil::{OpenMode, ensure_private_regular, open_nofollow};
use harw_runtime::deployment::cloud_placement::CloudWorkloadPolicy;
use serde_json::json;

use crate::cli::{CloudAction, CloudPolicyAction, GlobalArgs};
use crate::output::{Printer, render_table};

/// Execute an offline cloud operation without opening a control connection.
pub(crate) fn run(global: &GlobalArgs, action: CloudAction) -> Result<(), String> {
    let CloudAction::Policy { action } = action;
    let (file, describe) = match action {
        CloudPolicyAction::Validate { file } => (file, false),
        CloudPolicyAction::Describe { file } => (file, true),
    };
    let policy = read_policy(&file)?;
    let revision = policy.revision().get();
    let count = policy.configured_nodes().count();
    let mut text = format!(
        "Valid cloud workload policy, revision {revision}, {count} configured node(s).\n         Configured ceilings only; no live workers or execution checked."
    );
    let mut output = json!({
        "valid": true,
        "scope": "configured_policy_only",
        "revision": revision,
        "configured_node_count": count,
    });
    if describe {
        let mut rows = Vec::with_capacity(count);
        let mut nodes = Vec::with_capacity(count);
        for entry in policy.configured_nodes() {
            let node = entry.node().as_str();
            let profiles: Vec<_> = entry.profiles().map(|profile| profile.as_str()).collect();
            let workloads: Vec<_> = entry.workloads().map(|workload| workload.as_str()).collect();
            rows.push(vec![
                node.to_owned(),
                profiles.join(", "),
                workloads.join(", "),
            ]);
            nodes.push(json!({
                "node": node,
                "profiles": profiles,
                "workloads": workloads,
            }));
        }
        output["nodes"] = json!(nodes);
        if !rows.is_empty() {
            text.push_str("\n\n");
            text.push_str(&render_table(
                &["Node", "Configured profiles", "Configured workloads"],
                &rows,
            ));
        }
    }
    Printer::new(global.output()).value(&text, output)
}

/// Read a bounded snapshot from an already opened owner-private descriptor.
/// Never echo the input, including on a parser or encoding failure.
fn read_policy(path: &Path) -> Result<CloudWorkloadPolicy, String> {
    let file = open_nofollow(path, OpenMode::read_only())
        .map_err(|error| format!("cannot open cloud policy file: {error}"))?;
    ensure_private_regular(&file)
        .map_err(|error| format!("cloud policy must be an owner-private regular file: {error}"))?;
    let mut bytes = Vec::new();
    const LIMIT: usize = CloudWorkloadPolicy::MAX_BYTES;
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read cloud policy file: {error}"))?;
    if bytes.len() > LIMIT {
        return Err(format!("cloud policy file exceeds {LIMIT} bytes"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "cloud policy file must be valid UTF-8".to_owned())?;
    CloudWorkloadPolicy::load_from_str(text).map_err(|error| error.to_string())
}

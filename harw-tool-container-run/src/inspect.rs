//! `podman inspect` JSON → [`InspectFacts`].
//!
//! Absent keys stay `None` (unverifiable). Podman renders an empty capability
//! set as JSON `null`; a *present* `null` for `EffectiveCaps`/`CapAdd` is an
//! empty set, an *absent* key is not reported.

use harw_tool_container::InspectFacts;
use serde_json::Value;

/// Parses the output of `podman inspect <container>` (an array with one
/// object).
///
/// # Errors
/// The text is not JSON or not an array with an object.
pub fn parse_inspect(text: &str) -> Result<InspectFacts, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let object = value
        .as_array()
        .and_then(|items| items.first())
        .or(Some(&value))
        .filter(|item| item.is_object())
        .ok_or_else(|| "inspect output is not an object".to_owned())?;
    let host = object.get("HostConfig");
    let host_field = |key: &str| host.and_then(|h| h.get(key));

    Ok(InspectFacts {
        privileged: host_field("Privileged").and_then(Value::as_bool),
        cap_add: string_list(host_field("CapAdd"), true),
        cap_drop: string_list(host_field("CapDrop"), true),
        effective_caps: string_list(object.get("EffectiveCaps"), true),
        network_mode: host_field("NetworkMode")
            .and_then(Value::as_str)
            .map(str::to_owned),
        read_only_rootfs: host_field("ReadonlyRootfs").and_then(Value::as_bool),
        security_opt: string_list(host_field("SecurityOpt"), true),
        memory_bytes: host_field("Memory").and_then(Value::as_i64),
        memory_swap_bytes: host_field("MemorySwap").and_then(Value::as_i64),
        pids_limit: host_field("PidsLimit").and_then(Value::as_i64),
        workspace_read_only: workspace_read_only(object),
    })
}

/// `None` key → `None`; JSON `null` → empty set (when `null_is_empty`).
fn string_list(value: Option<&Value>, null_is_empty: bool) -> Option<Vec<String>> {
    match value? {
        Value::Null if null_is_empty => Some(Vec::new()),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect(),
        ),
        _ => None,
    }
}

/// `Some(true)` when the `/workspace` mount exists and is not writable.
fn workspace_read_only(object: &Value) -> Option<bool> {
    let mounts = object.get("Mounts")?.as_array()?;
    let mount = mounts
        .iter()
        .find(|m| m.get("Destination").and_then(Value::as_str) == Some("/workspace"))?;
    mount.get("RW").and_then(Value::as_bool).map(|rw| !rw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"[{
        "EffectiveCaps": null,
        "HostConfig": {
            "Privileged": false, "CapAdd": null, "CapDrop": ["CAP_CHOWN"],
            "NetworkMode": "none", "ReadonlyRootfs": true,
            "SecurityOpt": ["no-new-privileges"],
            "Memory": 1073741824, "MemorySwap": 1073741824, "PidsLimit": 256
        },
        "Mounts": [{"Destination": "/workspace", "RW": false}]
    }]"#;

    #[test]
    fn full_podman_output_maps_every_fact() {
        let facts = parse_inspect(FULL).unwrap_or_default();
        assert_eq!(facts.privileged, Some(false));
        assert_eq!(facts.cap_add, Some(Vec::new()));
        assert_eq!(
            facts.effective_caps,
            Some(Vec::new()),
            "present null = none"
        );
        assert_eq!(facts.network_mode.as_deref(), Some("none"));
        assert_eq!(facts.read_only_rootfs, Some(true));
        assert_eq!(facts.memory_bytes, Some(1_073_741_824));
        assert_eq!(facts.pids_limit, Some(256));
        assert_eq!(facts.workspace_read_only, Some(true));
    }

    #[test]
    fn absent_keys_stay_unreported() {
        let facts = parse_inspect(r#"[{"HostConfig": {}}]"#).unwrap_or_default();
        assert_eq!(facts, InspectFacts::default(), "nothing invented");
        let writable =
            parse_inspect(r#"[{"Mounts": [{"Destination": "/workspace", "RW": true}]}]"#)
                .unwrap_or_default();
        assert_eq!(writable.workspace_read_only, Some(false));
        let other = parse_inspect(r#"[{"Mounts": [{"Destination": "/x", "RW": false}]}]"#)
            .unwrap_or_default();
        assert_eq!(
            other.workspace_read_only, None,
            "no /workspace mount reported"
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_default() {
        assert!(parse_inspect("not json").is_err());
        assert!(parse_inspect("[]").is_err());
        assert!(parse_inspect("42").is_err());
    }
}

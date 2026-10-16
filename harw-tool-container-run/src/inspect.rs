//! `podman inspect` JSON → [`InspectFacts`].
//!
//! Absent keys stay `None` (unverifiable). Podman renders an empty capability
//! set as JSON `null`; a *present* `null` for `EffectiveCaps`/`CapAdd` is an
//! empty set, an *absent* key is not reported.

use harw_tool_container::{InspectFacts, ObservedMount};
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
        id: object.get("Id").and_then(Value::as_str).map(str::to_owned),
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
        mounts: observed_mounts(object),
        timeout_s: object
            .get("Config")
            .and_then(|config| config.get("Timeout"))
            .and_then(Value::as_u64)
            .and_then(|seconds| u32::try_from(seconds).ok()),
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

/// Every mount the engine reports; `None` when the key is absent, an empty
/// list when it is `null` or `[]` (the engine says there are none).
fn observed_mounts(object: &Value) -> Option<Vec<ObservedMount>> {
    let mounts = match object.get("Mounts")? {
        Value::Null => return Some(Vec::new()),
        Value::Array(items) => items,
        _ => return None,
    };
    let text = |m: &Value, key: &str| m.get(key).and_then(Value::as_str).map(str::to_owned);
    mounts
        .iter()
        .map(|m| {
            Some(ObservedMount {
                kind: text(m, "Type")?,
                name: text(m, "Name"),
                source: text(m, "Source"),
                destination: text(m, "Destination")?,
                rw: m.get("RW").and_then(Value::as_bool)?,
            })
        })
        .collect()
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
        "Id": "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12",
        "Config": {"Timeout": 30},
        "Mounts": [{"Type": "bind", "Source": "/w", "Destination": "/workspace", "RW": false}]
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
        assert_eq!(facts.id.as_deref().map(str::len), Some(64));
        assert_eq!(facts.timeout_s, Some(30));
        let mounts = facts.mounts.unwrap_or_default();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].destination, "/workspace");
        assert!(!mounts[0].rw);
    }

    #[test]
    fn absent_keys_stay_unreported() {
        let facts = parse_inspect(r#"[{"HostConfig": {}}]"#).unwrap_or_default();
        assert_eq!(facts, InspectFacts::default(), "nothing invented");
        // `null` is "none", an incomplete mount makes the whole list unreported.
        let none = parse_inspect(r#"[{"Mounts": null}]"#).unwrap_or_default();
        assert_eq!(none.mounts, Some(Vec::new()));
        let partial = parse_inspect(r#"[{"Mounts": [{"Destination": "/x"}]}]"#).unwrap_or_default();
        assert_eq!(
            partial.mounts, None,
            "a mount without type or RW is not evidence"
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_default() {
        assert!(parse_inspect("not json").is_err());
        assert!(parse_inspect("[]").is_err());
        assert!(parse_inspect("42").is_err());
    }
}

//! `harw agent diff`: the IR difference between two definitions, versions
//! or artifacts, with the rights delta first.

use std::collections::BTreeMap;

use harw_agent_dsl::ir_v2::AgentIr;
use serde::Serialize;
use serde_json::Value;

/// One changed field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldChange {
    /// Flattened path (`permissions.tools[3]`, `spawn.max_depth`).
    pub path: String,
    /// Value on the left, `None` if absent.
    pub left: Option<Value>,
    /// Value on the right, `None` if absent.
    pub right: Option<Value>,
}

/// The rights part of a diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RightsChange {
    /// Tools only on the right (widening).
    pub added_tools: Vec<String>,
    /// Tools only on the left (narrowing).
    pub removed_tools: Vec<String>,
    /// Flags that turn on (`shell`, `host`, `filesystem.write`, network).
    pub widened: Vec<String>,
    /// Flags that turn off.
    pub narrowed: Vec<String>,
    /// Hosts added / removed.
    pub added_hosts: Vec<String>,
    /// Hosts removed.
    pub removed_hosts: Vec<String>,
    /// Environment variables added.
    pub added_env: Vec<String>,
}

impl RightsChange {
    /// `true` if the right side has rights the left does not.
    #[must_use]
    pub fn widens(&self) -> bool {
        !self.added_tools.is_empty()
            || !self.widened.is_empty()
            || !self.added_hosts.is_empty()
    }
}

/// A complete diff.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IrDiff {
    /// Left label.
    pub left: String,
    /// Right label.
    pub right: String,
    /// Snapshots (left, right).
    pub snapshots: (Option<String>, Option<String>),
    /// Rights delta.
    pub rights: RightsChange,
    /// Every changed field (trace and snapshot excluded).
    pub changes: Vec<FieldChange>,
}

fn flatten(value: &Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(inner, &path, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (index, inner) in items.iter().enumerate() {
                flatten(inner, &format!("{prefix}[{index}]"), out);
            }
        }
        other => {
            out.insert(prefix.to_owned(), other.clone());
        }
    }
}

fn comparable(ir: &AgentIr) -> Value {
    let mut clean = ir.clone();
    clean.trace = harw_agent_dsl::ir_v2::Trace::default();
    clean.snapshot = None;
    serde_json::to_value(&clean).unwrap_or(Value::Null)
}

fn only_in(left: &[String], right: &[String]) -> Vec<String> {
    left.iter().filter(|item| !right.contains(item)).cloned().collect()
}

/// Compares two IRs.
#[must_use]
pub fn diff_irs(left_label: &str, left: &AgentIr, right_label: &str, right: &AgentIr) -> IrDiff {
    let mut left_flat = BTreeMap::new();
    let mut right_flat = BTreeMap::new();
    flatten(&comparable(left), "", &mut left_flat);
    flatten(&comparable(right), "", &mut right_flat);
    let mut changes = Vec::new();
    let paths: std::collections::BTreeSet<&String> = left_flat.keys().chain(right_flat.keys()).collect();
    for path in paths {
        let a = left_flat.get(path);
        let b = right_flat.get(path);
        if a != b {
            changes.push(FieldChange {
                path: path.clone(),
                left: a.cloned(),
                right: b.cloned(),
            });
        }
    }
    let (lp, rp) = (&left.permissions, &right.permissions);
    let mut rights = RightsChange {
        added_tools: only_in(&rp.tools, &lp.tools),
        removed_tools: only_in(&lp.tools, &rp.tools),
        added_hosts: only_in(&rp.network.hosts, &lp.network.hosts),
        removed_hosts: only_in(&lp.network.hosts, &rp.network.hosts),
        added_env: only_in(&rp.required_env, &lp.required_env),
        ..RightsChange::default()
    };
    let flags = [
        ("filesystem.read", lp.filesystem.read, rp.filesystem.read),
        ("filesystem.write", lp.filesystem.write, rp.filesystem.write),
        ("shell", lp.shell, rp.shell),
        ("host", lp.host, rp.host),
        (
            "network",
            !lp.network.tools.is_empty(),
            !rp.network.tools.is_empty(),
        ),
    ];
    for (name, before, after) in flags {
        match (before, after) {
            (false, true) => rights.widened.push(name.to_owned()),
            (true, false) => rights.narrowed.push(name.to_owned()),
            _ => {}
        }
    }
    if rp.spawn.max_depth > lp.spawn.max_depth {
        rights.widened.push(format!(
            "spawn.max_depth {} → {}",
            lp.spawn.max_depth, rp.spawn.max_depth
        ));
    } else if rp.spawn.max_depth < lp.spawn.max_depth {
        rights.narrowed.push(format!(
            "spawn.max_depth {} → {}",
            lp.spawn.max_depth, rp.spawn.max_depth
        ));
    }
    IrDiff {
        left: left_label.to_owned(),
        right: right_label.to_owned(),
        snapshots: (
            left.snapshot.as_ref().map(|s| s.digest.clone()),
            right.snapshot.as_ref().map(|s| s.digest.clone()),
        ),
        rights,
        changes,
    }
}

impl IrDiff {
    /// Text form: rights delta first (widenings marked `!!`), then fields.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!("--- {}\n+++ {}\n", self.left, self.right);
        if self.changes.is_empty() {
            out.push_str("no differences\n");
            return out;
        }
        let r = &self.rights;
        out.push_str(if r.widens() {
            "rights: !! the right side WIDENS the rights\n"
        } else {
            "rights: no widening\n"
        });
        for tool in &r.added_tools {
            out.push_str(&format!("  !! + tool {tool}\n"));
        }
        for tool in &r.removed_tools {
            out.push_str(&format!("     - tool {tool}\n"));
        }
        for flag in &r.widened {
            out.push_str(&format!("  !! + {flag}\n"));
        }
        for flag in &r.narrowed {
            out.push_str(&format!("     - {flag}\n"));
        }
        for host in &r.added_hosts {
            out.push_str(&format!("  !! + host {host}\n"));
        }
        for host in &r.removed_hosts {
            out.push_str(&format!("     - host {host}\n"));
        }
        for env in &r.added_env {
            out.push_str(&format!("     + env {env}\n"));
        }
        out.push_str("fields:\n");
        let show = |value: &Option<Value>| value.as_ref().map_or_else(|| "∅".to_owned(), Value::to_string);
        for change in &self.changes {
            out.push_str(&format!(
                "  {}: {} → {}\n",
                change.path,
                show(&change.left),
                show(&change.right)
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flatten_paths() {
        let mut out = BTreeMap::new();
        flatten(&serde_json::json!({"a": {"b": [1, {"c": true}]}, "e": []}), "", &mut out);
        assert_eq!(out.get("a.b[0]"), Some(&serde_json::json!(1)));
        assert_eq!(out.get("a.b[1].c"), Some(&serde_json::json!(true)));
        assert_eq!(out.get("e"), Some(&serde_json::json!([])));
    }

    #[test]
    fn test_only_in() {
        let a = vec!["x".to_owned(), "y".to_owned()];
        let b = vec!["y".to_owned()];
        assert_eq!(only_in(&a, &b), ["x"]);
        assert!(only_in(&b, &a).is_empty());
    }
}

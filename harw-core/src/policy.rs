//! Runtime consumption of the declarative `[policy]` configuration.
//!
//! This is deliberately narrow but real: a configured tool name produces an
//! approval pause before dispatch. The turn loop separately enforces that the
//! pause is bound to a trusted [`harw_types::ApprovalActor`]. More elaborate
//! policy inputs (tool provenance, tenant, channel, risk) can compose as
//! additional `ApprovalHandler`s without bypassing this baseline.

use harw_config::PolicySection;
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture};
use harw_tools::ToolCall;
use harw_types::ItemId;
use std::collections::BTreeSet;

/// Exact-name approval policy compiled from `[policy].require_approval_for`.
#[derive(Debug, Clone, Default)]
pub struct ConfigApprovalPolicy {
    required_tools: BTreeSet<String>,
}

impl ConfigApprovalPolicy {
    #[must_use]
    pub fn from_policy_section(section: &PolicySection) -> Self {
        Self::new(section.require_approval_for.iter().cloned())
    }

    #[must_use]
    pub fn new(names: impl IntoIterator<Item = String>) -> Self {
        Self {
            required_tools: names.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn requires_approval(&self, call: &ToolCall) -> bool {
        self.required_tools.contains(call.name.as_str())
    }
}

impl ApprovalHandler for ConfigApprovalPolicy {
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let requires_approval = self.requires_approval(call);
        Box::pin(async move {
            if requires_approval {
                ApprovalDecision::AskUser(ItemId::new())
            } else {
                ApprovalDecision::Allow
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_tools::ToolName;
    use harw_types::ToolCallId;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn config_policy_requires_only_named_tools() {
        let section = PolicySection {
            default_visibility_scope: "self".to_owned(),
            require_approval_for: vec!["shell".to_owned()],
        };
        let policy = ConfigApprovalPolicy::from_policy_section(&section);

        assert!(matches!(
            policy.review(&call("shell")).await,
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            policy.review(&call("search")).await,
            ApprovalDecision::Allow
        ));
    }
}

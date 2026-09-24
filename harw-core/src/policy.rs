//! Runtime consumption of the declarative `[policy]` configuration.
//!
//! This is deliberately narrow but real: a configured tool name produces an
//! approval pause before dispatch. The turn loop separately enforces that the
//! pause is bound to a trusted [`harw_types::ApprovalActor`]. More elaborate
//! policy inputs (tool provenance, tenant, channel, risk) can compose as
//! additional `ApprovalHandler`s without bypassing this baseline.

use harw_config::PolicySection;
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture};
use harw_tools::ToolCall;
use harw_types::ItemId;
use std::collections::BTreeSet;

/// Exact-name approval policy compiled from `[policy].require_approval_for`.
///
/// # Full Access
/// With a mode cell attached ([`Self::with_mode`]) the policy never asks
/// while that cell reads [`ApprovalMode::FullAccess`] (user decision
/// 2026-09-24: Full Access means no confirmations at all). Without a cell it
/// asks for the listed tools in every mode, as before.
#[derive(Debug, Clone, Default)]
pub struct ConfigApprovalPolicy {
    required_tools: BTreeSet<String>,
    /// Approval mode of the run this policy belongs to; `None` = mode-blind.
    mode: Option<ApprovalModeCell>,
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
            mode: None,
        }
    }

    /// Binds the policy to the approval-mode cell of its run.
    ///
    /// # Arguments
    /// - `mode` ([`ApprovalModeCell`]): read on every review; while it reads
    ///   [`ApprovalMode::FullAccess`] the policy allows every call.
    ///
    /// # Returns
    /// The policy with the cell attached (replacing any earlier one).
    #[must_use]
    pub fn with_mode(mut self, mode: ApprovalModeCell) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Whether `call` needs an approval under the current mode.
    ///
    /// # Returns
    /// `false` while the attached mode cell reads
    /// [`ApprovalMode::FullAccess`]; otherwise `true` exactly for the listed
    /// tool names.
    #[must_use]
    pub fn requires_approval(&self, call: &ToolCall) -> bool {
        let full_access = self
            .mode
            .as_ref()
            .is_some_and(|mode| mode.get() == ApprovalMode::FullAccess);
        !full_access && self.required_tools.contains(call.name.as_str())
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

    /// Full Access never asks, not even for a `[policy]`-listed tool; the
    /// other modes keep asking for it.
    #[tokio::test]
    async fn config_policy_never_asks_under_full_access() {
        let cell = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let policy = ConfigApprovalPolicy::new(["shell".to_owned()]).with_mode(cell.clone());

        assert!(matches!(
            policy.review(&call("shell")).await,
            ApprovalDecision::Allow
        ));
        for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::Delegated] {
            cell.set(mode);
            assert!(matches!(
                policy.review(&call("shell")).await,
                ApprovalDecision::AskUser(_)
            ));
        }
    }
}

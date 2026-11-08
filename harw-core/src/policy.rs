//! Runtime consumption of the declarative `[policy]` configuration.
//!
//! This is deliberately narrow but real: a configured tool name produces an
//! approval pause before dispatch. The turn loop separately enforces that the
//! pause is bound to a trusted [`harw_types::ApprovalActor`]. More elaborate
//! policy inputs (tool provenance, tenant, channel, risk) can compose as
//! additional `ApprovalHandler`s without bypassing this baseline.
//!
//! # DoD taint (PL-100 phase 4, threat B22, decision O1)
//! [`DodTaintPolicy`] is the second handler in this module: after a `dod.*`
//! tool ran in the current turn, it asks for every taint-gated tool
//! ([`crate::dod_taint::is_taint_gated`]) — except under Full Access. The
//! taint state lives in the turn's control block
//! ([`crate::dod_taint::DodTaint`]); `check_approval` hands the per-call
//! taint flag to every handler through
//! [`crate::dod_taint::with_review_taint`]. Without an installed
//! [`DodTaintPolicy`], the turn loop asks mode-blind wherever
//! [`DodTaintPolicy::fallback_applies`] holds (fail-closed).
//!
//! Decision O1 ("only outside Full Access") therefore holds only where the
//! run's approval chain installs [`DodTaintPolicy::with_mode`] over the run's
//! [`ApprovalModeCell`] (`harw-runtime/src/approval.rs`, `ApprovalChain`).
//! The mode-blind fallback also asks under Full Access — and denies where
//! nobody can answer — so it must never be the production path: every
//! production chain has to install the policy (the runtime tests in
//! `harw-runtime/src/permission_rules.rs` check that).
//!
//! Where nobody can answer a taint question
//! ([`DodTaintPolicy::ask_answerable`]: no approval actor, or a
//! non-interactive entry whose ask-resolution handler carries one of
//! [`NON_INTERACTIVE_ASK_LABELS`]), the turn loop denies the call with
//! [`crate::dod_taint::unanswerable_reason`] instead of pausing.
//!
//! # Concurrency
//! Both policies are `Clone` and `Send + Sync`; the only shared state is the
//! attached [`ApprovalModeCell`], read on every review.
//!
//! # Errors
//! None: both policies are total.
//!
//! # Examples
//! ```rust
//! use harw_core::policy::DodTaintPolicy;
//! use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
//! use harw_tools::{ToolCall, ToolName};
//! use harw_types::ToolCallId;
//!
//! let call = ToolCall {
//!     id: ToolCallId::new(),
//!     name: ToolName::new("job.stop"),
//!     arguments: serde_json::Value::Null,
//! };
//! let auto = DodTaintPolicy::new().with_mode(ApprovalModeCell::new(ApprovalMode::Delegated));
//! assert!(auto.requires_approval(&call, true));
//! assert!(!auto.requires_approval(&call, false));
//! let full = DodTaintPolicy::new().with_mode(ApprovalModeCell::new(ApprovalMode::FullAccess));
//! assert!(!full.requires_approval(&call, true));
//! ```

use crate::dod_taint;
use harw_config::PolicySection;
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture};
use harw_tools::ToolCall;
use harw_types::ItemId;
use std::collections::BTreeSet;
use std::sync::Arc;

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

/// Stable short name of [`DodTaintPolicy`] (`ApprovalHandler::label`).
///
/// The turn loop looks for it to decide whether the mode-blind fallback
/// ([`DodTaintPolicy::fallback_applies`]) has to step in. No other handler
/// may use this label.
pub const DOD_TAINT_POLICY_LABEL: &str = "dod-taint";

/// Labels of the runtime's ask-resolution handlers of non-interactive
/// entries.
///
/// # Description
/// `harw_runtime::approval::AskResolutionPolicy::label_for` names the handler
/// of every entry whose `AskResolution` is not `Interactive` (one-shot,
/// jobs, analyze, doctor, …). Its presence in a registry means nobody can
/// answer an `AskUser` in that run. `harw-core` cannot depend on
/// `harw-runtime`; `harw-runtime/src/permission_rules.rs` holds the test that
/// keeps this list equal to `label_for` (the labels are part of the stable
/// diagnostic surface `RightsSnapshot::approval_chain`).
pub const NON_INTERACTIVE_ASK_LABELS: &[&str] = &["ask:reject-turn", "ask:block-job", "ask:fail"];

/// Approval policy of the per-turn DoD taint (PL-100 phase 4, threat B22).
///
/// # Description
/// After a `dod.*` tool ran in the current turn, sensor text that an attacker
/// on the host can shape is in the model context. For the rest of that turn
/// every taint-gated tool ([`dod_taint::is_taint_gated`]: `process.kill`,
/// `host.sudo_exec`, every mutating `job.*`) needs an explicit user answer —
/// even where auto mode, a session lease or a remembered allow rule would let
/// it through. `check_approval` aggregates `Deny` > `AskUser` > `Allow`, so
/// this handler's `AskUser` beats every other handler's `Allow`; a `Deny` from
/// another handler still wins.
///
/// # Full Access (decision O1)
/// With a mode cell attached ([`Self::with_mode`]) the policy never asks while
/// that cell reads [`ApprovalMode::FullAccess`] — Full Access means no
/// confirmations at all. Without a cell it asks in every mode (fail-closed).
///
/// # Where the taint flag comes from
/// The policy holds no taint state. [`ApprovalHandler::review`] reads
/// [`dod_taint::review_turn_tainted`], which `check_approval` sets for the
/// reviewed call's turn. Outside such a review context the call counts as
/// untainted. A child chain may therefore share or copy this policy without
/// sharing a taint: every session runs its own turns.
///
/// # Concurrency
/// `Clone`, `Send + Sync`; reads the mode cell on every review.
#[derive(Debug, Clone, Default)]
pub struct DodTaintPolicy {
    /// Approval mode of the run this policy belongs to; `None` = mode-blind.
    mode: Option<ApprovalModeCell>,
}

impl DodTaintPolicy {
    /// Creates a mode-blind policy (asks after a DoD query in every mode).
    ///
    /// # Returns
    /// A [`DodTaintPolicy`] without a mode cell.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Whether the attached mode cell currently reads Full Access.
    ///
    /// # Returns
    /// `false` without a cell.
    #[must_use]
    pub fn full_access(&self) -> bool {
        self.mode
            .as_ref()
            .is_some_and(|mode| mode.get() == ApprovalMode::FullAccess)
    }

    /// Whether `call` needs an explicit approval because of the DoD taint.
    ///
    /// # Arguments
    /// - `call` (`&ToolCall`): the reviewed tool call.
    /// - `tainted` (`bool`): whether the call's turn already dispatched a
    ///   `dod.*` tool.
    ///
    /// # Returns
    /// `true` exactly when `tainted`, the tool is taint-gated and the mode is
    /// not Full Access.
    #[must_use]
    pub fn requires_approval(&self, call: &ToolCall, tainted: bool) -> bool {
        tainted && dod_taint::is_taint_gated(call.name.as_str()) && !self.full_access()
    }

    /// Whether the fail-closed fallback has to turn `decision` into `AskUser`.
    ///
    /// # Description
    /// The fallback applies exactly when no handler labelled
    /// [`DOD_TAINT_POLICY_LABEL`] is registered, every handler allowed
    /// (`decision` is `Allow`) and a mode-blind [`DodTaintPolicy`] would ask
    /// (tainted turn, taint-gated tool). `check_approval` evaluates it exactly
    /// once: the same result turns the decision into `AskUser` and attributes
    /// that ask to the taint, so the two can never disagree.
    ///
    /// # Arguments
    /// - `handlers` (`&[Arc<dyn ApprovalHandler>]`): the session's registered
    ///   approval handlers.
    /// - `call` (`&ToolCall`): the reviewed tool call.
    /// - `tainted` (`bool`): whether the call's turn is tainted.
    /// - `decision` (`&ApprovalDecision`): the aggregated decision.
    ///
    /// # Returns
    /// `true` when [`Self::fallback_decision`] would change `decision`.
    #[must_use]
    pub fn fallback_applies(
        handlers: &[Arc<dyn ApprovalHandler>],
        call: &ToolCall,
        tainted: bool,
        decision: &ApprovalDecision,
    ) -> bool {
        matches!(decision, ApprovalDecision::Allow)
            && !Self::installed(handlers)
            && Self::new().requires_approval(call, tainted)
    }

    /// The fail-closed fallback for a registry without this policy.
    ///
    /// # Description
    /// Thin wrapper over [`Self::fallback_applies`] for callers outside the
    /// turn loop (and the tests): if no handler labelled
    /// [`DOD_TAINT_POLICY_LABEL`] is registered, a mode-blind
    /// [`DodTaintPolicy`] decides in its place, so an aggregated `Allow` for a
    /// taint-gated call in a tainted turn becomes `AskUser`. Without a mode
    /// cell this also asks under Full Access; installing the policy with the
    /// run's mode cell restores decision O1. `check_approval` does not call
    /// this wrapper: it evaluates [`Self::fallback_applies`] once and reuses
    /// the result. Whether anyone can answer is decided afterwards by
    /// `check_approval` ([`Self::ask_answerable`]).
    ///
    /// # Arguments
    /// - `handlers` (`&[Arc<dyn ApprovalHandler>]`): the session's registered
    ///   approval handlers.
    /// - `call` (`&ToolCall`): the reviewed tool call.
    /// - `tainted` (`bool`): whether the call's turn is tainted.
    /// - `decision` ([`ApprovalDecision`]): the aggregated decision.
    ///
    /// # Returns
    /// `decision` unchanged, unless the fallback applies; then
    /// `AskUser` with a fresh request id. `Deny` and `AskUser` stay as they
    /// are.
    #[must_use]
    pub fn fallback_decision(
        handlers: &[Arc<dyn ApprovalHandler>],
        call: &ToolCall,
        tainted: bool,
        decision: ApprovalDecision,
    ) -> ApprovalDecision {
        if Self::fallback_applies(handlers, call, tainted, &decision) {
            ApprovalDecision::AskUser(ItemId::new())
        } else {
            decision
        }
    }

    /// Whether anyone can answer a question the taint raises in this run.
    ///
    /// # Description
    /// A taint question needs a person who may answer it. Nobody can when the
    /// session has no approval actor (job workers, children without a
    /// relay) or when the registry carries the ask-resolution handler of a
    /// non-interactive entry ([`NON_INTERACTIVE_ASK_LABELS`]). That handler
    /// predicts the asks of the *other* handlers and denies them; it does not
    /// know the taint, so `check_approval` denies the taint question itself
    /// instead of pausing a run that no one watches.
    ///
    /// # Arguments
    /// - `handlers` (`&[Arc<dyn ApprovalHandler>]`): the session's registered
    ///   approval handlers.
    /// - `actor_present` (`bool`): whether the session's spawn context names
    ///   an approval actor.
    ///
    /// # Returns
    /// `true` when an `AskUser` can reach a person; `false` means the call
    /// must be denied.
    #[must_use]
    pub fn ask_answerable(handlers: &[Arc<dyn ApprovalHandler>], actor_present: bool) -> bool {
        actor_present
            && !handlers
                .iter()
                .any(|handler| NON_INTERACTIVE_ASK_LABELS.contains(&handler.label()))
    }

    // Whether a handler labelled `DOD_TAINT_POLICY_LABEL` is registered.
    fn installed(handlers: &[Arc<dyn ApprovalHandler>]) -> bool {
        handlers
            .iter()
            .any(|handler| handler.label() == DOD_TAINT_POLICY_LABEL)
    }
}

impl ApprovalHandler for DodTaintPolicy {
    /// Asks for a taint-gated call in a tainted turn, outside Full Access.
    ///
    /// # Description
    /// Reads the taint flag of the review context
    /// ([`dod_taint::review_turn_tainted`]) and the mode cell when the
    /// returned future is polled; side-effect free.
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            if self.requires_approval(call, dod_taint::review_turn_tainted()) {
                ApprovalDecision::AskUser(ItemId::new())
            } else {
                ApprovalDecision::Allow
            }
        })
    }

    /// [`ApprovalHandlerKind::Other`]: neither config nor default policy, and
    /// it never asks a person itself.
    fn kind(&self) -> ApprovalHandlerKind {
        ApprovalHandlerKind::Other
    }

    /// [`DOD_TAINT_POLICY_LABEL`]; the turn loop's fallback looks for it.
    fn label(&self) -> &'static str {
        DOD_TAINT_POLICY_LABEL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
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

    /// Reviews `name` with `policy` inside a review context with `tainted`.
    async fn review_in(policy: &DodTaintPolicy, name: &str, tainted: bool) -> ApprovalDecision {
        let call = call(name);
        dod_taint::with_review_taint(tainted, policy.review(&call)).await
    }

    /// The taint-gated tools of the spec, including the ALWAYS_ASK host tools.
    const GATED: &[&str] = &["job.stop", "job.start", "process.kill", "host.sudo_exec"];

    /// Auto mode (`Delegated`) and `ask`: after a DoD query every gated tool
    /// asks; without the taint nothing changes.
    #[tokio::test]
    async fn dod_taint_policy_asks_for_gated_tools_in_a_tainted_turn() -> TestResult {
        for mode in [ApprovalMode::Delegated, ApprovalMode::AlwaysAsk] {
            let policy = DodTaintPolicy::new().with_mode(ApprovalModeCell::new(mode));
            for tool in GATED {
                assert!(
                    matches!(
                        review_in(&policy, tool, true).await,
                        ApprovalDecision::AskUser(_)
                    ),
                    "{tool} under {mode:?}"
                );
                assert!(
                    matches!(
                        review_in(&policy, tool, false).await,
                        ApprovalDecision::Allow
                    ),
                    "{tool} without taint under {mode:?}"
                );
            }
        }
        Ok(())
    }

    /// Read-only job tools and the DoD tools themselves are never affected.
    #[tokio::test]
    async fn dod_taint_policy_leaves_read_only_job_tools_alone() -> TestResult {
        let policy = DodTaintPolicy::new();
        for tool in [
            "job.status",
            "job.logs",
            "job.list",
            "dod.findings",
            "fs.read",
        ] {
            assert!(
                matches!(
                    review_in(&policy, tool, true).await,
                    ApprovalDecision::Allow
                ),
                "{tool}"
            );
        }
        Ok(())
    }

    /// Decision O1: Full Access never asks, even after a DoD query; the mode
    /// is read live, so switching back to `ask`/`auto` asks again.
    #[tokio::test]
    async fn dod_taint_policy_never_asks_under_full_access() -> TestResult {
        let cell = ApprovalModeCell::new(ApprovalMode::FullAccess);
        let policy = DodTaintPolicy::new().with_mode(cell.clone());
        for tool in GATED {
            assert!(
                matches!(
                    review_in(&policy, tool, true).await,
                    ApprovalDecision::Allow
                ),
                "{tool}"
            );
        }
        cell.set(ApprovalMode::Delegated);
        assert!(matches!(
            review_in(&policy, "job.stop", true).await,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Outside a review context (a handler reviewed outside
    /// `check_approval`) the call counts as untainted.
    #[tokio::test]
    async fn dod_taint_policy_allows_outside_a_review_context() -> TestResult {
        let policy = DodTaintPolicy::new();
        assert!(matches!(
            policy.review(&call("job.stop")).await,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// The label is the stable marker the turn loop looks for.
    #[test]
    fn dod_taint_policy_reports_its_label_and_kind() -> TestResult {
        let policy = DodTaintPolicy::new();
        assert_eq!(policy.label(), DOD_TAINT_POLICY_LABEL);
        assert_eq!(policy.kind(), ApprovalHandlerKind::Other);
        Ok(())
    }

    /// Without an installed policy the fallback asks mode-blind for a
    /// tainted, gated call that every handler allowed.
    #[test]
    fn fallback_asks_when_no_policy_is_installed() -> TestResult {
        let none: Vec<Arc<dyn ApprovalHandler>> = Vec::new();
        let decision = DodTaintPolicy::fallback_decision(
            &none,
            &call("job.stop"),
            true,
            ApprovalDecision::Allow,
        );
        assert!(matches!(decision, ApprovalDecision::AskUser(_)));
        for (tool, tainted) in [("job.stop", false), ("job.logs", true)] {
            assert!(
                matches!(
                    DodTaintPolicy::fallback_decision(
                        &none,
                        &call(tool),
                        tainted,
                        ApprovalDecision::Allow
                    ),
                    ApprovalDecision::Allow
                ),
                "{tool} tainted={tainted}"
            );
        }
        Ok(())
    }

    /// The fallback never weakens: `Deny` and an existing `AskUser` (with its
    /// request id) pass through unchanged.
    #[test]
    fn fallback_keeps_deny_and_existing_ask() -> TestResult {
        let none: Vec<Arc<dyn ApprovalHandler>> = Vec::new();
        let denied = DodTaintPolicy::fallback_decision(
            &none,
            &call("job.stop"),
            true,
            ApprovalDecision::Deny("rule".to_owned()),
        );
        assert!(matches!(denied, ApprovalDecision::Deny(reason) if reason == "rule"));
        let request = ItemId::new();
        let asked = DodTaintPolicy::fallback_decision(
            &none,
            &call("job.stop"),
            true,
            ApprovalDecision::AskUser(request.clone()),
        );
        match asked {
            ApprovalDecision::AskUser(kept) => assert_eq!(kept, request),
            other => {
                return Err(TestError::Unexpected(format!(
                    "AskUser must pass through, was {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Allows everything and reports `label`; stands in for a runtime handler
    /// such as the ask resolution of a non-interactive entry.
    struct Labelled(&'static str);

    impl ApprovalHandler for Labelled {
        fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Allow })
        }

        fn label(&self) -> &'static str {
            self.0
        }
    }

    /// `fallback_applies` is exactly the condition under which
    /// `fallback_decision` changes the decision.
    #[test]
    fn fallback_applies_only_to_an_allowed_tainted_gated_call_without_policy() -> TestResult {
        let none: Vec<Arc<dyn ApprovalHandler>> = Vec::new();
        let installed: Vec<Arc<dyn ApprovalHandler>> = vec![Arc::new(DodTaintPolicy::new())];
        let allow = ApprovalDecision::Allow;
        assert!(DodTaintPolicy::fallback_applies(
            &none,
            &call("job.stop"),
            true,
            &allow
        ));
        for (handlers, tool, tainted, decision) in [
            (&none, "job.stop", false, ApprovalDecision::Allow),
            (&none, "job.logs", true, ApprovalDecision::Allow),
            (&installed, "job.stop", true, ApprovalDecision::Allow),
            (
                &none,
                "job.stop",
                true,
                ApprovalDecision::AskUser(ItemId::new()),
            ),
            (
                &none,
                "job.stop",
                true,
                ApprovalDecision::Deny("rule".to_owned()),
            ),
        ] {
            assert!(
                !DodTaintPolicy::fallback_applies(handlers, &call(tool), tainted, &decision),
                "{tool} tainted={tainted} {decision:?}"
            );
        }
        Ok(())
    }

    /// A taint question is answerable only with an approval actor and
    /// without the ask resolution of a non-interactive entry.
    #[test]
    fn ask_answerable_needs_an_actor_and_an_interactive_entry() -> TestResult {
        let interactive: Vec<Arc<dyn ApprovalHandler>> = vec![
            Arc::new(Labelled("default-policy")),
            Arc::new(Labelled("tui-responder")),
        ];
        assert!(DodTaintPolicy::ask_answerable(&interactive, true));
        assert!(!DodTaintPolicy::ask_answerable(&interactive, false));
        for label in NON_INTERACTIVE_ASK_LABELS {
            let non_interactive: Vec<Arc<dyn ApprovalHandler>> = vec![
                Arc::new(Labelled("default-policy")),
                Arc::new(Labelled(*label)),
            ];
            assert!(
                !DodTaintPolicy::ask_answerable(&non_interactive, true),
                "{label}"
            );
        }
        Ok(())
    }

    /// With the policy installed the turn loop leaves the decision to it —
    /// under Full Access the aggregated `Allow` stays `Allow` (O1).
    #[test]
    fn fallback_defers_to_an_installed_policy() -> TestResult {
        let installed: Vec<Arc<dyn ApprovalHandler>> = vec![Arc::new(
            DodTaintPolicy::new().with_mode(ApprovalModeCell::new(ApprovalMode::FullAccess)),
        )];
        assert!(matches!(
            DodTaintPolicy::fallback_decision(
                &installed,
                &call("job.stop"),
                true,
                ApprovalDecision::Allow
            ),
            ApprovalDecision::Allow
        ));
        Ok(())
    }
}

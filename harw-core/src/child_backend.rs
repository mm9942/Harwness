//! `ChildBackend`: the seam that lets [`crate::child_controller::ManagedAgentSpawner`]
//! hand "run this child" to something other than the in-process
//! `AgentSession` (`docs/adr/0001-agent-compiler.md`).
//!
//! # Description
//! Nothing in this crate changes behavior on its own: without a backend,
//! `ManagedAgentSpawner` drives every child in-process exactly as before.
//! A compiled parent (built by `harw-agent-compiler`) instead wires an
//! `Arc<dyn ChildBackend>` — in practice `harw-agent-runner`'s
//! `JobChildBackend`, which starts the child as a job via
//! `harw_tool_job::JobManager` and speaks the `harwness.agent-child/v1`
//! protocol over its stdio — through
//! [`crate::child_controller::ManagedAgentSpawner::with_child_backend`].
//!
//! This module intentionally names no job system, no wire protocol and no
//! process: `harw-core` cannot depend on `harw-tool-job` or
//! `harwness-sdk` (both depend on `harw-core`), so every type here is
//! self-contained and only *mappable* to the richer types a caller already
//! has (`crate::child_comms::ChildEndCause`, `crate::child_controller::ChildUsage`).
//!
//! # Concurrency
//! [`ChildBackend`] is `Send + Sync`; [`ChildBackend::run`] returns a boxed
//! future so the trait stays object-safe behind `Arc<dyn ChildBackend>`, and
//! a spawner may run several children through the same backend at once.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use harw_agent_dsl::ir_v2::Budget;
use harw_types::SessionId;
use harw_types::cancel::CancelToken;

use crate::child_comms::ChildEndCause;
use crate::child_controller::ChildUsage;

/// Boxed future returned by [`ChildBackend::run`].
pub type ChildBackendFuture<'a> = Pin<Box<dyn Future<Output = ChildRunOutcome> + Send + 'a>>;

/// Boxed future returned by [`ChildIo::on_question`] and
/// [`ChildIo::on_approval_request`]: the parent's raw reply text, mirroring
/// the child protocol's `Answer.text` (plan §3C).
pub type ChildAnswerFuture<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;

/// Rights a child run may use: the child's current effective rights as the
/// parent's runtime sees them — exactly what the same child would get
/// in-process (its tool surface after the parent-authority cut, its sandbox,
/// the tree's approval mode), filled by
/// `crate::child_controller::ManagedAgentSpawner` and never more than the
/// parent holds. A [`ChildBackend`] passes them on unchanged or narrows
/// further, never widens; the child itself then intersects them with its
/// own manifest.
///
/// `Default` is "no rights at all" (every set empty, every flag `false`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildBackendRights {
    /// Exact tool names the child may call.
    pub tools: BTreeSet<String>,
    /// Hosts the child may reach over the network (only meaningful with
    /// `network_open`).
    pub network_hosts: BTreeSet<String>,
    /// Network access is allowed at all. Despite the name (kept for the
    /// child protocol's `ChildRights::network_open`) this is not
    /// "unrestricted": only `network_hosts` are reachable.
    pub network_open: bool,
    /// Write access to the workspace.
    pub write: bool,
    /// Shell execution.
    pub shell: bool,
    /// Host-level (unsandboxed) execution.
    pub host: bool,
    /// Automatic approval inside the rights above (the parent runs in full
    /// access); never widens any of them.
    pub full_access: bool,
}

/// What a [`ChildBackend`] needs to run one child.
#[derive(Debug, Clone)]
pub struct ChildRunSpec {
    /// The child's session id.
    pub child: SessionId,
    /// The parent's session id.
    pub parent: SessionId,
    /// The child's agent/role id inside the bundle.
    pub agent_id: String,
    /// The child's task text (its first user turn).
    pub task: String,
    /// Structured context handed to the child alongside `task`, if any.
    pub context: Option<serde_json::Value>,
    /// Opaque continuation token from a prior budget-ended run of the same
    /// child, if this run resumes one.
    pub continue_from: Option<String>,
    /// Rights for this run (the child's current effective rights; see
    /// [`ChildBackendRights`]).
    pub rights: ChildBackendRights,
    /// Budget for this run, if any.
    pub budget: Option<Budget>,
    /// Whether the tree runs live (vs. plan mode).
    pub live_mode: bool,
    /// Cancels this run — the same hierarchical primitive
    /// `ManagedAgentSpawner` already derives per child (`CancelToken::child`).
    /// A backend awaits this alongside its own work (`tokio::select!`) and,
    /// once cancelled, ends the run with
    /// [`ChildRunStatus::Cancelled`] carrying [`CancelToken::reason`]'s
    /// short label.
    pub cancel: CancelToken,
}

/// Sink/source a [`ChildBackend`] uses to relay a running child's activity
/// to the parent and take the parent's replies, independent of any wire
/// format (a job-backed backend translates these to/from
/// `harwness.agent-child/v1` frames; an in-process one could call the
/// existing progress/approval channels directly).
///
/// # Concurrency
/// `Send + Sync`; a backend may call `on_event` from any task, and may have
/// at most one outstanding `on_question`/`on_approval_request` future per
/// question/request id.
pub trait ChildIo: Send + Sync {
    /// A translated child event (the backend's own JSON shape; job backends
    /// use the child protocol's `Event.sdk_event`).
    fn on_event(&self, event_json: serde_json::Value);

    /// The child asked a free-text question; resolves to the parent's reply
    /// text once answered.
    fn on_question<'a>(&'a self, id: String, text: String) -> ChildAnswerFuture<'a>;

    /// The child asks approval for a held tool call; resolves to the
    /// parent's reply text (a backend's own encoding of the decision, e.g.
    /// `"approve"` or `"deny: <reason>"`).
    fn on_approval_request<'a>(
        &'a self,
        id: String,
        tool: String,
        args_summary: String,
    ) -> ChildAnswerFuture<'a>;
}

/// How a child's run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildRunStatus {
    /// Regular completion.
    Completed,
    /// The run's token/tool/wall-time budget ended it; `text` on the
    /// outcome is the last (or handed-off) answer, not an error.
    BudgetExhausted,
    /// Cancelled (parent, user, shutdown); `reason` is a short label.
    Cancelled {
        /// Short reason label (`"user"`, `"parent"`, `"shutdown"`, …).
        reason: String,
    },
    /// The child process crashed or exited unexpectedly.
    Crashed {
        /// Process exit code, if the platform reported one.
        exit_code: Option<i32>,
        /// Tail of the child's stderr (logs only, never protocol frames).
        stderr_tail: String,
    },
    /// Any other non-successful, non-crash end (provider/turn error,
    /// protocol violation, ...).
    Failed {
        /// Human-readable reason.
        reason: String,
    },
}

impl ChildRunStatus {
    /// `true` for [`Self::Completed`] and [`Self::BudgetExhausted`] — both
    /// deliver a usable `text`, unlike every other variant.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed | Self::BudgetExhausted)
    }

    /// Maps to the existing [`ChildEndCause`] used by the child journal and
    /// end report (`crate::child_comms`). Returns `None` for
    /// [`Self::Completed`], which has no `ChildEndCause` (only non-regular
    /// ends get an end report today).
    #[must_use]
    pub fn to_child_end_cause(&self) -> Option<ChildEndCause> {
        match self {
            Self::Completed => None,
            Self::BudgetExhausted => Some(ChildEndCause::Outcome(
                "budget exhausted (job-backed child)".to_owned(),
            )),
            Self::Cancelled { reason } => Some(ChildEndCause::Cancelled {
                reason: reason.clone(),
            }),
            Self::Crashed {
                exit_code,
                stderr_tail,
            } => Some(ChildEndCause::TurnError(format!(
                "child process ended unexpectedly (exit_code={}): {stderr_tail}",
                exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned())
            ))),
            Self::Failed { reason } => Some(ChildEndCause::TurnError(reason.clone())),
        }
    }
}

/// Result of one [`ChildBackend::run`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRunOutcome {
    /// How the run ended.
    pub status: ChildRunStatus,
    /// The child's final (or last, for a budget end) assistant text, if any.
    pub text: Option<String>,
    /// Consumption of this run — directly the type
    /// [`crate::child_controller::ChildRunResult::usage`] already uses.
    pub usage: ChildUsage,
    /// Opaque continuation token for a subsequent
    /// [`ChildRunSpec::continue_from`], if the backend supports resuming a
    /// budget-ended child. `None` when the run has nothing to continue from
    /// (completed normally, crashed, or the backend does not support it).
    pub continuation: Option<String>,
}

/// Runs a child "somewhere other than in-process", on behalf of
/// [`crate::child_controller::ManagedAgentSpawner`].
///
/// # Errors
/// A backend never returns a `Result`: every failure mode (crash, protocol
/// violation, provider error, budget end) is a [`ChildRunStatus`] variant of
/// a successfully-returned [`ChildRunOutcome`], so the caller always gets a
/// journal-mappable end cause instead of an opaque error.
pub trait ChildBackend: Send + Sync {
    /// Runs `spec` to completion (or to a non-regular end), relaying
    /// activity through `io` as it happens.
    fn run<'a>(&'a self, spec: ChildRunSpec, io: &'a dyn ChildIo) -> ChildBackendFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn completed_has_no_end_cause() {
        assert_eq!(ChildRunStatus::Completed.to_child_end_cause(), None);
        assert!(ChildRunStatus::Completed.is_success());
    }

    #[test]
    fn budget_exhausted_is_success_with_an_end_cause() {
        assert!(ChildRunStatus::BudgetExhausted.is_success());
        assert!(matches!(
            ChildRunStatus::BudgetExhausted.to_child_end_cause(),
            Some(ChildEndCause::Outcome(_))
        ));
    }

    #[test]
    fn cancelled_maps_reason_through() -> TestResult {
        let status = ChildRunStatus::Cancelled {
            reason: "user".to_owned(),
        };
        assert!(!status.is_success());
        match status.to_child_end_cause() {
            Some(ChildEndCause::Cancelled { reason }) => assert_eq!(reason, "user"),
            other => {
                return Err(TestError::Unexpected(format!("unexpected: {other:?}")));
            }
        }
        Ok(())
    }

    #[test]
    fn crashed_reports_exit_code_and_stderr_tail() -> TestResult {
        let status = ChildRunStatus::Crashed {
            exit_code: Some(137),
            stderr_tail: "panicked at ...".to_owned(),
        };
        assert!(!status.is_success());
        match status.to_child_end_cause() {
            Some(ChildEndCause::TurnError(message)) => {
                assert!(message.contains("137"));
                assert!(message.contains("panicked at"));
            }
            other => {
                return Err(TestError::Unexpected(format!("unexpected: {other:?}")));
            }
        }
        Ok(())
    }

    #[test]
    fn crashed_without_exit_code_says_unknown() -> TestResult {
        let status = ChildRunStatus::Crashed {
            exit_code: None,
            stderr_tail: String::new(),
        };
        match status.to_child_end_cause() {
            Some(ChildEndCause::TurnError(message)) => assert!(message.contains("unknown")),
            other => {
                return Err(TestError::Unexpected(format!("unexpected: {other:?}")));
            }
        }
        Ok(())
    }

    #[test]
    fn failed_maps_reason_through() -> TestResult {
        let status = ChildRunStatus::Failed {
            reason: "provider timeout".to_owned(),
        };
        match status.to_child_end_cause() {
            Some(ChildEndCause::TurnError(reason)) => assert_eq!(reason, "provider timeout"),
            other => {
                return Err(TestError::Unexpected(format!("unexpected: {other:?}")));
            }
        }
        Ok(())
    }
}

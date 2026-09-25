//! A terminal [`ApprovalHandler`]: asks a human at the controlling terminal
//! whether a held-back tool call may run.
//!
//! # Description
//! Shared by `iface::cli` and `iface::repl` — the two interfaces that run
//! at an actual terminal and can plausibly ask someone. `iface::mcp` and
//! `iface::http` do not use this: they have no interactive channel, so they
//! run with [`RunnerContext::harwness`]'s default (`harwness_sdk::AutoDeny`)
//! instead.
//!
//! [`RunnerContext::harwness`] deliberately builds without an approval
//! handler ("the interface that runs the session sets its own approval
//! handler ... on top"); since `Harwness` is already built by the time that
//! method returns, an interface that wants one builds its own `Harwness`
//! the same way `harwness()` does, with `.approval_handler_arc(..)` added.
//! [`build_harwness`] is that one extra step, kept here next to the handler
//! it wires in so `cli.rs` and `repl.rs` do not each repeat it.
//!
//! # Behaviour
//! - `--full-access`: every call is approved without asking — the manifest
//!   (narrowed by [`harw_runtime::embedded::RightsFlags`] before this
//!   handler ever runs) is the only gate left, exactly as the flag's name
//!   promises.
//! - No terminal attached to stdin (a pipe, a redirected file, `/dev/null`):
//!   every call is denied. An unattended run that cannot be asked is not an
//!   approved run — silently letting things through would defeat the point
//!   of asking at all.
//! - Otherwise: the call is printed and stdin is read for one line —
//!   `j`/`ja`/`y`/`yes` approves once, `i`/`immer` approves this call *and*
//!   every later call for the same tool name in this session, anything else
//!   (including a blank line or EOF) denies.
//!
//! Denials are also tracked (`any_denied`) so the interface can distinguish
//! "the turn failed on its own" from "a human said no" for its exit code.

use std::collections::HashSet;
use std::io::{IsTerminal, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use harwness_sdk::{ApprovalHandler, ApprovalRequest, BoxFuture, Decision, Harwness};

use crate::context::RunnerContext;
use crate::error::RunnerError;

/// Reason given to the model when no terminal can answer interactively.
pub const NON_INTERACTIVE_DENY_REASON: &str =
    "approval denied: no terminal is attached to ask interactively (pass --full-access for an \
     unattended run within the manifest)";

/// Reason given to the model when a human typed anything but yes/always.
pub const USER_DENY_REASON: &str = "approval denied: the user declined this tool call";

/// What a human typed at the `[j]a / [n]ein / [i]mmer für diese Sitzung`
/// prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// `j`/`ja`/`y`/`yes`: approve this one call.
    Approve,
    /// `i`/`immer`: approve this call and remember the tool for the rest of
    /// the session.
    AlwaysThisSession,
    /// Anything else, a blank line, or end of input: deny.
    Deny,
}

/// Asks a human at the controlling terminal to approve or deny a held-back
/// tool call.
pub struct TerminalApproval {
    full_access: bool,
    interactive: bool,
    remembered: Mutex<HashSet<String>>,
    denied: AtomicBool,
}

impl TerminalApproval {
    /// Builds the handler. `full_access` mirrors `--full-access`;
    /// interactivity is detected from stdin (a real terminal vs. a pipe or
    /// redirected file).
    #[must_use]
    pub fn new(full_access: bool) -> Self {
        Self::with_interactive(full_access, std::io::stdin().is_terminal())
    }

    /// As [`Self::new`], with interactivity given explicitly (tests: a real
    /// terminal is not available in CI).
    #[must_use]
    fn with_interactive(full_access: bool, interactive: bool) -> Self {
        Self {
            full_access,
            interactive,
            remembered: Mutex::new(HashSet::new()),
            denied: AtomicBool::new(false),
        }
    }

    /// `true` once at least one call has been denied (by the user, or for
    /// lack of a terminal) — never by `--full-access`, which never denies.
    #[must_use]
    pub fn any_denied(&self) -> bool {
        self.denied.load(Ordering::Relaxed)
    }

    fn remembers(&self, tool: &str) -> bool {
        self.remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(tool)
    }

    fn remember(&self, tool: &str) {
        self.remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tool.to_owned());
    }

    /// Prints the request and reads one line of the human's answer. Run on
    /// a blocking thread ([`tokio::task::spawn_blocking`]) so it never stalls
    /// the async runtime the rest of the turn is driven on.
    fn ask(tool: &str, arguments: &serde_json::Value) -> Choice {
        eprintln!("\nWerkzeugaufruf erfordert Freigabe: {tool}");
        eprintln!("  Argumente: {arguments}");
        eprint!("  [j]a / [n]ein / [i]mmer für diese Sitzung > ");
        if std::io::stderr().flush().is_err() {
            return Choice::Deny;
        }
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => Choice::Deny, // EOF: nothing left to ask.
            Ok(_) => match line.trim().to_lowercase().as_str() {
                "j" | "ja" | "y" | "yes" => Choice::Approve,
                "i" | "immer" | "always" => Choice::AlwaysThisSession,
                _ => Choice::Deny,
            },
            Err(_) => Choice::Deny,
        }
    }
}

impl ApprovalHandler for TerminalApproval {
    fn decide<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            if self.full_access {
                return Decision::Approve;
            }
            if self.remembers(&request.tool) {
                return Decision::Approve;
            }
            if !self.interactive {
                self.denied.store(true, Ordering::Relaxed);
                return Decision::deny(NON_INTERACTIVE_DENY_REASON);
            }
            let tool = request.tool.clone();
            let arguments = request.arguments.clone();
            let choice = tokio::task::spawn_blocking(move || Self::ask(&tool, &arguments))
                .await
                .unwrap_or(Choice::Deny);
            match choice {
                Choice::Approve => Decision::Approve,
                Choice::AlwaysThisSession => {
                    self.remember(&request.tool);
                    Decision::Approve
                }
                Choice::Deny => {
                    self.denied.store(true, Ordering::Relaxed);
                    Decision::deny(USER_DENY_REASON)
                }
            }
        })
    }
}

/// Builds this invocation's [`Harwness`] with `approvals` wired in.
///
/// # Description
/// [`RunnerContext::harwness`] builds without an approval handler on
/// purpose; this repeats its two settings (`.embedded(..)`, `.cwd(..)`) and
/// adds `.approval_handler_arc(approvals)`, so `iface::cli` and
/// `iface::repl` embed the agent exactly the way every other interface
/// does, plus the one thing only they need.
///
/// # Errors
/// [`RunnerError::Io`] if the current working directory cannot be read;
/// [`RunnerError::Sdk`] if [`harwness_sdk::HarwnessBuilder::build`] rejects
/// the embedded agent.
pub fn build_harwness(
    ctx: &RunnerContext,
    approvals: Arc<dyn ApprovalHandler>,
) -> Result<Harwness, RunnerError> {
    let cwd = std::env::current_dir().map_err(|source| RunnerError::Io {
        context: "read the current working directory",
        source,
    })?;
    Harwness::builder()
        .embedded(ctx.agent.clone())
        .cwd(cwd)
        .approval_handler_arc(approvals)
        .build()
        .map_err(RunnerError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn request(tool: &str) -> Result<ApprovalRequest, harwness_sdk::SdkError> {
        Ok(ApprovalRequest {
            session_id: harwness_sdk::SessionId::new("s")?,
            request_id: "r".into(),
            call_id: "c".into(),
            tool: tool.into(),
            arguments: json!({"path": "a"}),
        })
    }

    #[tokio::test]
    async fn full_access_approves_without_asking_and_never_marks_denied() -> TestResult {
        let handler = TerminalApproval::with_interactive(true, false);
        let decision = handler.decide(&request("shell.exec")?).await;
        assert_eq!(decision, Decision::Approve);
        assert!(!handler.any_denied());
        Ok(())
    }

    #[tokio::test]
    async fn a_non_interactive_terminal_denies_and_marks_denied() -> TestResult {
        let handler = TerminalApproval::with_interactive(false, false);
        let decision = handler.decide(&request("fs.write")?).await;
        assert_eq!(decision, Decision::deny(NON_INTERACTIVE_DENY_REASON));
        assert!(handler.any_denied());
        Ok(())
    }

    #[tokio::test]
    async fn remembering_a_tool_skips_asking_again() -> TestResult {
        let handler = TerminalApproval::with_interactive(false, false);
        // Not interactive, so this denies rather than asking — but a
        // remembered tool must short-circuit even that: prove the lookup
        // runs before the interactivity check by remembering first.
        handler.remember("fs.write");
        let decision = handler.decide(&request("fs.write")?).await;
        assert_eq!(decision, Decision::Approve);
        assert!(!handler.any_denied());
        Ok(())
    }
}

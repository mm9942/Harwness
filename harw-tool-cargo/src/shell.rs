//! Der einzige Weg, auf dem dieses Crate einen Prozess startet: der injizierte
//! `shell.exec`-Ausführer.
//!
//! [`ShellDelegate`] kapselt einen `Arc<dyn ToolExecutor>`, im Betrieb den
//! `ShellExecutor` aus `harw-tool-shell`. Der Aufruf geht mit dem **echten**
//! [`ToolExecutionContext`] des Agentenaufrufs durch: bwrap-Sandbox, rlimits,
//! Host-Permit-Prüfung, Zeitlimit und Ausgabekappung des Ausführers gelten
//! unverändert. Dieses Crate erzeugt weder `Command` noch `spawn`.
//!
//! # Annahmen über `shell.exec`
//! Eine erfolgreiche Antwort ist JSON mit `exit_code`, `stdout`, `stderr`,
//! `truncated`; ein Zeitablauf oder eine Ablehnung kommt als Fehlerausgabe.

use harw_tools::{ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput};
use harw_types::ToolCallId;
use serde_json::{Value, json};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

/// Name des delegierten Werkzeugs.
pub const SHELL_TOOL: &str = "shell.exec";

/// Der geteilte `shell.exec`-Ausführer.
#[derive(Clone)]
pub struct ShellDelegate(Arc<dyn ToolExecutor>);

impl fmt::Debug for ShellDelegate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ShellDelegate(shell.exec)")
    }
}

/// Ergebnis eines delegierten Aufrufs.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellRun {
    /// Exit-Code (`-1` bei Signal).
    pub exit_code: i64,
    /// Standardausgabe (vom Ausführer gekürzt).
    pub stdout: String,
    /// Fehlerausgabe (vom Ausführer gekürzt).
    pub stderr: String,
    /// Der Ausführer hat die Ausgabe gekürzt.
    pub truncated: bool,
    /// `"host"`, wenn der Aufruf außerhalb der Sandbox lief.
    pub executed_on: Option<String>,
    /// Wanduhrzeit des Aufrufs in Millisekunden.
    pub wall_ms: u128,
}

impl ShellDelegate {
    /// Kapselt den Ausführer.
    #[must_use]
    pub fn new(executor: Arc<dyn ToolExecutor>) -> Self {
        Self(executor)
    }

    /// Führt `command` mit `timeout_secs` über `shell.exec` aus.
    ///
    /// # Errors
    /// Meldung bei Ablehnung, Zeitablauf, Abbruch oder unerwarteter Antwort.
    pub async fn run(
        &self,
        context: &ToolExecutionContext,
        command: &str,
        timeout_secs: u64,
    ) -> Result<ShellRun, String> {
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(SHELL_TOOL),
            arguments: json!({"command": command, "timeout_secs": timeout_secs}),
        };
        let started = Instant::now();
        let output = self
            .0
            .execute(context, &call)
            .await
            .map_err(|error| error.to_string())?;
        let wall_ms = started.elapsed().as_millis();
        match output {
            ToolOutput::Json { content } => parse_run(&content, wall_ms),
            ToolOutput::Error { message } => Err(message),
            ToolOutput::Text { .. } => Err("shell.exec returned text instead of JSON".to_owned()),
        }
    }
}

fn parse_run(content: &Value, wall_ms: u128) -> Result<ShellRun, String> {
    let exit_code = content
        .get("exit_code")
        .and_then(Value::as_i64)
        .ok_or_else(|| "shell.exec result has no exit_code".to_owned())?;
    let text = |key: &str| {
        content
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    Ok(ShellRun {
        exit_code,
        stdout: text("stdout"),
        stderr: text("stderr"),
        truncated: content
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        executed_on: content
            .get("executed_on")
            .and_then(Value::as_str)
            .map(str::to_owned),
        wall_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, ScriptedShell, TestResult};

    #[tokio::test]
    async fn passes_command_and_timeout_and_parses_the_answer() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let shell = ScriptedShell::json(
            json!({"exit_code": 3, "stdout": "out", "stderr": "err", "truncated": true, "executed_on": "host"}),
        );
        let delegate = ShellDelegate::new(shell.clone());
        let run = delegate
            .run(&ctx, "cargo check", 77)
            .await
            .map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!(
            (
                run.exit_code,
                run.stdout.as_str(),
                run.stderr.as_str(),
                run.truncated
            ),
            (3, "out", "err", true)
        );
        assert_eq!(run.executed_on.as_deref(), Some("host"));
        let calls = shell.calls();
        assert_eq!(calls, vec![("cargo check".to_owned(), 77)]);
        Ok(())
    }

    #[tokio::test]
    async fn errors_and_odd_answers_are_reported() -> TestResult {
        let fx = Fixture::new()?;
        let ctx = fx.exec_ctx()?;
        let timeout = ShellDelegate::new(ScriptedShell::error("shell.exec timed out after 5s"));
        assert_eq!(
            timeout.run(&ctx, "x", 5).await,
            Err("shell.exec timed out after 5s".to_owned())
        );
        let text = ShellDelegate::new(ScriptedShell::text("hello"));
        assert!(text.run(&ctx, "x", 5).await.is_err());
        let no_code = ShellDelegate::new(ScriptedShell::json(json!({"stdout": "x"})));
        assert!(no_code.run(&ctx, "x", 5).await.is_err());
        let minimal = ShellDelegate::new(ScriptedShell::json(json!({"exit_code": 0})));
        let run = minimal
            .run(&ctx, "x", 5)
            .await
            .map_err(crate::test_support::TestError::Unexpected)?;
        assert_eq!((run.stdout.as_str(), run.truncated), ("", false));
        Ok(())
    }
}

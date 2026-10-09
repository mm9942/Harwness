//! Ausführung blockierender Tool-Arbeit außerhalb des async-Executors.
//!
//! Walks und Datei-IO laufen über [`run_blocking`] in
//! `tokio::task::spawn_blocking`, damit ein langer Walk den (häufig
//! `current_thread`-)Runtime des Harness nicht einfriert. Ohne Runtime läuft
//! die Arbeit direkt. Ein Panic im Blocking-Task wird als Tool-Fehler gemeldet.

use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use std::path::PathBuf;

/// Kanonische Workspace-Wurzel aus dem Ausführungskontext.
#[must_use]
pub fn workspace_root(context: &ToolExecutionContext) -> PathBuf {
    context.sandbox().workspace().canonical_root().to_path_buf()
}

/// Öffnet den [`crate::scope::Scope`] und führt `body` aus; jede `Err`-Meldung
/// wird zu einer Fehlerausgabe `"<tool>: <meldung>"`.
#[must_use]
pub fn scoped<F>(tool: &str, root: &std::path::Path, body: F) -> ToolOutput
where
    F: FnOnce(&crate::scope::Scope) -> Result<ToolOutput, String>,
{
    let scope = match crate::scope::Scope::new(root) {
        Ok(scope) => scope,
        Err(error) => {
            return crate::budget::fail(
                tool,
                format!(
                    "workspace root is not accessible: {}",
                    crate::scope::io_message(&error)
                ),
            );
        }
    };
    match body(&scope) {
        Ok(output) => output,
        Err(message) => crate::budget::fail(tool, message),
    }
}

/// Führt `job` im Blocking-Pool des aktuellen Tokio-Runtimes aus.
///
/// # Errors
/// Das Ergebnis von `job`; ein Panic im Task wird zu `Ok(ToolOutput::error(..))`.
pub async fn run_blocking<F>(tool: &'static str, job: F) -> Result<ToolOutput, ToolsError>
where
    F: FnOnce() -> ToolOutput + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => match handle.spawn_blocking(job).await {
            Ok(output) => Ok(output),
            Err(error) => Ok(ToolOutput::error(format!(
                "{tool}: internal error in blocking task: {error}"
            ))),
        },
        Err(_) => Ok(job()),
    }
}

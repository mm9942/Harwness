//! Fehlertypen für `harw-tools`.

use harw_macros::HarwError;

/// Fehler aus dem Tool-Subsystem.
///
/// `ToolsResult<T>` wird vom `HarwError`-Derive miterzeugt.
#[derive(Debug, HarwError)]
pub enum ToolsError {
    #[msg("tool '{name}' not found")]
    NotFound { name: String },

    #[msg("invalid arguments for tool '{name}': {reason}")]
    InvalidArguments { name: String, reason: String },

    #[msg("tool execution failed: {0}")]
    ExecutionFailed(String),

    #[from]
    SerdeJson(serde_json::Error),
}

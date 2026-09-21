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

    /// Die Tool-Ausführung wurde abgebrochen (`harw_core::cancel::CancelToken`,
    /// W3/C-CANCEL), bevor ein Ergebnis vorlag.
    ///
    /// # Hinweis zur Herkunft
    /// `harw-tools` selbst kennt `CancelToken` (noch) nicht — `harw-core`
    /// hängt bereits von `harw-tools` ab, sodass eine Abhängigkeit in die
    /// Gegenrichtung einen Zirkel erzeugen würde. Diese Variante ist deshalb
    /// bewusst payload-los; ein Aufrufer, der einen Abbruch gegen einen
    /// `CancelToken` erkennt (z. B. `harw-core`, sobald `ToolExecutionContext`
    /// dort einen Abbruchpfad verdrahtet bekommt), erzeugt sie ohne
    /// Cancel-spezifische Nutzdaten mitzuführen.
    #[msg("tool execution was cancelled")]
    Cancelled,

    #[from]
    SerdeJson(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::ToolsError;

    #[test]
    fn test_tools_error_cancelled_display_text() {
        let err = ToolsError::Cancelled;

        assert_eq!(err.to_string(), "tool execution was cancelled");
    }
}

//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

#![allow(dead_code)] // Gemeinsames Test-Gerüst: nicht jede Crate nutzt alle Varianten/Helfer.

use crate::McpError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Fehler aus diesem Crate ([`McpError`]).
    Mcp(McpError),
}

/// Ergebnistyp für Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "test: erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(detail) => write!(f, "test: unerwartetes Ergebnis: {detail}"),
            TestError::Context { context, source } => write!(f, "test: {context}: {source}"),
            TestError::Mcp(err) => write!(f, "test: MCP-Fehler: {err}"),
        }
    }
}

impl std::fmt::Debug for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestError::Mcp(err) => Some(err),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<McpError> for TestError {
    fn from(err: McpError) -> Self {
        TestError::Mcp(err)
    }
}

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// [`TestError::Context`] übersetzt (ersetzt `.expect("…")` auf `Result`).
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

//! Test-Fehlertyp dieses Crates: ersetzt `panic!`/`unwrap`/`expect` in Tests
//! (Rust Coding Bible R087/R165/R182). Tests geben [`TestResult`] zurück und
//! melden Fehlschläge als `Err` statt zu paniken.

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben.
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
pub enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fremdfehler mit Kontext (ersetzt `expect("…")`).
    Context {
        /// Was gerade versucht wurde.
        context: &'static str,
        /// Gerenderter Quellfehler.
        source: String,
    },
}

/// Ergebnis einer Testfunktion bzw. eines Test-Helfers.
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
pub type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

// Debug delegiert an Display (Bible R081), damit fehlgeschlagene Tests lesbar bleiben.
impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

/// Liefert einen `map_err`-Adapter, der einen Fremdfehler mit Kontext versieht.
///
/// # Examples
/// ```rust,ignore
/// let text = std::fs::read_to_string(path).map_err(ctx("Datei lesen"))?;
/// ```
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
pub fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

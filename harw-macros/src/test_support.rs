//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

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
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

/// Hilfsfunktion: übersetzt `.expect("…")` in `.map_err(ctx("…"))?` und behält die
/// ursprüngliche Kontextmeldung. Deckt u. a. `syn::Error` ab (Parse-/Expansionsfehler
/// der geprüften Makros), da jeder Fremdfehler nur `Display` erfüllen muss.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

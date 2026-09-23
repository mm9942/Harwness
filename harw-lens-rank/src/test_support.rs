//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
// Nur `Missing` wird in diesem Crate aktuell verwendet (siehe fuse.rs); die
// übrigen Varianten bleiben als vollständige, crate-weit einheitliche
// Test-Fehlerform erhalten.
#[allow(dead_code)]
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

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Ergebnistyp für Tests dieses Crates; ersetzt Panics durch `Err`-Rückgaben.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// `TestError::Context` überführt (ersetzt `.expect("…")` auf `Result`).
// In diesem Crate haben bisher nur Option-Expects existiert (siehe fuse.rs);
// `ctx` bleibt als Vorlage für künftige Result-Expects erhalten.
#[allow(dead_code)]
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

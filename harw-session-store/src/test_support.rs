//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! Nur unter `#[cfg(test)]` eingebunden (siehe `lib.rs`). Jeder bisher
//! paniknde Testpfad (`.unwrap()`, `.expect(...)`, `panic!(...)`) gibt
//! stattdessen ein `TestResult` zurück; ein fehlgeschlagener Test bricht
//! über `?` mit einem lesbaren `TestError` ab statt den Prozess zu killen.

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
    /// I/O-Fehler aus Testdateioperationen (`tempfile`, `std::fs`, …).
    Io(std::io::Error),
    /// JSON-(De-)Serialisierungsfehler aus direkten `serde_json`-Aufrufen im Test.
    Json(serde_json::Error),
    /// Fehler aus dem Crate selbst (`SessionStoreError`), z. B. von Store-Methoden.
    Store(crate::error::SessionStoreError),
}

/// Ergebnistyp aller Tests dieses Crates; Default `T = ()` für `#[test] fn … -> TestResult`.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            TestError::Unexpected(detail) => write!(f, "unerwartetes Ergebnis: {detail}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Io(error) => write!(f, "I/O-Fehler im Test: {error}"),
            TestError::Json(error) => write!(f, "JSON-Fehler im Test: {error}"),
            TestError::Store(error) => write!(f, "Store-Fehler im Test: {error}"),
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
        match self {
            TestError::Io(error) => Some(error),
            TestError::Json(error) => Some(error),
            TestError::Store(error) => Some(error),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(error: std::io::Error) -> Self {
        TestError::Io(error)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(error: serde_json::Error) -> Self {
        TestError::Json(error)
    }
}

impl From<crate::error::SessionStoreError> for TestError {
    fn from(error: crate::error::SessionStoreError) -> Self {
        TestError::Store(error)
    }
}

/// Baut aus einem Kontextlabel eine Closure, die einen Fremdfehler in
/// `TestError::Context` überführt — ersetzt `.expect("…")` auf `Result`
/// (Aufruf: `.map_err(ctx("…"))?`).
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

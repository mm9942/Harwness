//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// E/A-Fehler (`std::io::Error`), z. B. aus `std::fs::*` oder `write_atomic`.
    Io(std::io::Error),
}

/// Ergebnistyp für Integrationstests dieses Crates.
pub type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "test: erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(detail) => write!(f, "test: unerwartetes Ergebnis: {detail}"),
            TestError::Context { context, source } => write!(f, "test: {context}: {source}"),
            TestError::Io(err) => write!(f, "test: E/A-Fehler: {err}"),
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
            TestError::Io(err) => Some(err),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// [`TestError::Context`] übersetzt (ersetzt `.expect("…")` auf `Result`).
pub fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

//! Test-Fehlertyp der Integrationstests: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

use harw_agent_dsl::error::DslError;

/// Fehler eines Integrationstests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[allow(dead_code)]
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
    /// Ein Fehler aus dem geprüften Crate selbst (`DslError`).
    Dsl(DslError),
    /// Ein I/O-Fehler aus einer Test-Fixture.
    Io(std::io::Error),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Dsl(err) => write!(f, "DSL-Fehler: {err}"),
            TestError::Io(err) => write!(f, "I/O-Fehler: {err}"),
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
            TestError::Dsl(err) => Some(err),
            TestError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DslError> for TestError {
    fn from(err: DslError) -> Self {
        TestError::Dsl(err)
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

/// Ergebnistyp für Integrationstests dieses Crates; ersetzt Panics durch `Err`-Rückgaben.
pub type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// `TestError::Context` überführt (ersetzt `.expect("…")` auf `Result`).
#[allow(dead_code)]
pub fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

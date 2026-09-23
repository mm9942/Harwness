//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

use crate::context_program::ContextProgramError;
use crate::error::DslError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
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
    /// Ein Fehler aus diesem Crate selbst (`DslError`).
    Dsl(DslError),
    /// Ein Fehler bei der Auflösung eines Kontextprogramms (`ContextProgramError`).
    ContextProgram(ContextProgramError),
    /// Ein I/O-Fehler aus einer Test-Fixture.
    Io(std::io::Error),
    /// Ein TOML-Deserialisierungsfehler aus einer Test-Fixture.
    Toml(toml::de::Error),
    /// Ein JSON-(De-)Serialisierungsfehler aus einer Test-Fixture.
    Json(serde_json::Error),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Dsl(err) => write!(f, "DSL-Fehler: {err}"),
            TestError::ContextProgram(err) => write!(f, "Kontextprogramm-Fehler: {err}"),
            TestError::Io(err) => write!(f, "I/O-Fehler: {err}"),
            TestError::Toml(err) => write!(f, "TOML-Fehler: {err}"),
            TestError::Json(err) => write!(f, "JSON-Fehler: {err}"),
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
            TestError::ContextProgram(err) => Some(err),
            TestError::Io(err) => Some(err),
            TestError::Toml(err) => Some(err),
            TestError::Json(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DslError> for TestError {
    fn from(err: DslError) -> Self {
        TestError::Dsl(err)
    }
}

impl From<ContextProgramError> for TestError {
    fn from(err: ContextProgramError) -> Self {
        TestError::ContextProgram(err)
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

impl From<toml::de::Error> for TestError {
    fn from(err: toml::de::Error) -> Self {
        TestError::Toml(err)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(err: serde_json::Error) -> Self {
        TestError::Json(err)
    }
}

/// Ergebnistyp für Tests dieses Crates; ersetzt Panics durch `Err`-Rückgaben.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// `TestError::Context` überführt (ersetzt `.expect("…")` auf `Result`).
#[allow(dead_code)]
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

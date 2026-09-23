//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

use crate::error::HomeError;

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
    /// Ein Dateisystem-Zugriff im Test ist fehlgeschlagen.
    Io(std::io::Error),
    /// Eine Crate-Operation ist mit [`HomeError`] fehlgeschlagen.
    Home(HomeError),
}

/// Kurzform für `Result<T, TestError>` in Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Io(source) => write!(f, "I/O-Fehler: {source}"),
            Self::Home(source) => write!(f, "harw-home-Fehler: {source}"),
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
            Self::Io(source) => Some(source),
            Self::Home(source) => Some(source),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

impl From<HomeError> for TestError {
    fn from(source: HomeError) -> Self {
        Self::Home(source)
    }
}

/// Wandelt einen Fremdfehler mit Kontext in einen [`TestError::Context`] um —
/// Ersatz für `.expect("…")` in Tests.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

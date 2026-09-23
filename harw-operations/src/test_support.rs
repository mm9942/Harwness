//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

use crate::error::OpError;
use crate::registry::RegistryError;

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
    /// Eine Operation ist mit [`OpError`] fehlgeschlagen.
    Op(OpError),
    /// Eine Registry-Operation ist mit [`RegistryError`] fehlgeschlagen.
    Registry(RegistryError),
}

/// Kurzform für `Result<T, TestError>` in Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Op(source) => write!(f, "Operationsfehler: {source}"),
            Self::Registry(source) => write!(f, "Registry-Fehler: {source}"),
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
            Self::Op(source) => Some(source),
            Self::Registry(source) => Some(source),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<OpError> for TestError {
    fn from(source: OpError) -> Self {
        Self::Op(source)
    }
}

impl From<RegistryError> for TestError {
    fn from(source: RegistryError) -> Self {
        Self::Registry(source)
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

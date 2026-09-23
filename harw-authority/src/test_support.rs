//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    #[allow(dead_code)] // von anderen Tests in diesem Crate ggf. ungenutzt
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    #[allow(dead_code)]
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Ein Fehler aus `harw_authority::AuthorityError`.
    Crate(crate::AuthorityError),
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            Self::Unexpected(message) => write!(f, "unerwartetes Ergebnis: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Crate(error) => write!(f, "{error}"),
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
            Self::Crate(error) => Some(error),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<crate::AuthorityError> for TestError {
    fn from(error: crate::AuthorityError) -> Self {
        Self::Crate(error)
    }
}

/// Hilfsfunktion: übersetzt `.expect("…")` in `.map_err(ctx("…"))?` und behält die
/// ursprüngliche Kontextmeldung.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

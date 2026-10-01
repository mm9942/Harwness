//! Test-Fehlertyp dieses Crates: Tests geben [`TestResult`] zurück und melden
//! Fehlschläge als `Err` statt zu paniken (kein `unwrap`/`expect`).

use std::fmt;

/// Fehler eines Tests.
pub(crate) enum TestError {
    /// Ein Ergebnis hatte nicht die erwartete Form.
    Unexpected(String),
    /// Ein Fremdfehler (z. B. ein Policy-Fehler aus einem Konstruktor).
    Source(String),
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unexpected(m) => write!(f, "unexpected: {m}"),
            Self::Source(m) => write!(f, "source error: {m}"),
        }
    }
}

impl<E: std::error::Error> From<E> for TestError {
    fn from(e: E) -> Self {
        Self::Source(e.to_string())
    }
}

/// Ergebnis einer Testfunktion.
pub(crate) type TestResult = Result<(), TestError>;

/// Fehlschlag, wenn `cond` nicht gilt.
pub(crate) fn ensure(cond: bool, msg: &str) -> TestResult {
    if cond {
        Ok(())
    } else {
        Err(TestError::Unexpected(msg.to_owned()))
    }
}

//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! # Verantwortungsbereich
//! [`TestError`] ist der einzige Fehlertyp, den `#[cfg(test)]`-Module dieser
//! Crate zurückgeben. Kein Testfehlschlag paniert mehr — jeder Fehlschlag
//! propagiert als `Err(TestError)` aus der jeweiligen `#[test] fn … ->
//! TestResult`-Funktion.
//!
//! # Exportierte Typen
//! [`TestError`], [`TestResult`], die Hilfsfunktion [`ctx`].
//!
//! # Nebenläufigkeit
//! Reiner Werttyp ohne innere Veränderlichkeit; `Send + Sync`.

use std::fmt;

use crate::error::NetlinkError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
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
    /// Ein Fehler dieser Crate (aus [`crate::error::NetlinkError`]).
    Netlink(NetlinkError),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Netlink(source) => write!(f, "netlink-Fehler: {source}"),
        }
    }
}

impl fmt::Debug for TestError {
    /// Delegiert an [`Display`](fmt::Display) (eine Implementierung, keine Duplikation).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestError::Netlink(source) => Some(source),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<NetlinkError> for TestError {
    fn from(source: NetlinkError) -> Self {
        TestError::Netlink(source)
    }
}

/// Kurzform für `Result<T, TestError>`.
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Hilfsfunktion, die einen Fremdfehler mit Kontext in [`TestError::Context`] überführt.
///
/// # Description
/// Ersetzt das Muster `.expect("kontext")` für `Result`-Werte: `.map_err(ctx("kontext"))?`.
///
/// # Arguments
/// - `context` (`&'static str`): menschenlesbarer Kontext des Fehlschlags.
///
/// # Returns
/// Eine Closure, die einen `Display`-fähigen Fremdfehler in [`TestError::Context`] überführt.
#[allow(dead_code)] // nicht jede Testdatei nutzt jedes Element
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

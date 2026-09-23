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

use crate::error::CatalogError;

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
    /// Ein I/O-Fehler in einem Test (z. B. Lesen einer Fixture-Datei).
    Io(std::io::Error),
    /// Ein JSON-(De)Serialisierungsfehler in einem Test.
    Json(serde_json::Error),
    /// Ein TOML-Deserialisierungsfehler in einem Test.
    TomlDe(toml::de::Error),
    /// Ein TOML-Serialisierungsfehler in einem Test.
    TomlSer(toml::ser::Error),
    /// Ein Fehler dieser Crate (aus [`crate::error::CatalogError`]).
    Catalog(CatalogError),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Io(source) => write!(f, "i/o-Fehler: {source}"),
            TestError::Json(source) => write!(f, "JSON-Fehler: {source}"),
            TestError::TomlDe(source) => write!(f, "TOML-Deserialisierungsfehler: {source}"),
            TestError::TomlSer(source) => write!(f, "TOML-Serialisierungsfehler: {source}"),
            TestError::Catalog(source) => write!(f, "Katalog-Fehler: {source}"),
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
            TestError::Io(source) => Some(source),
            TestError::Json(source) => Some(source),
            TestError::TomlDe(source) => Some(source),
            TestError::TomlSer(source) => Some(source),
            TestError::Catalog(source) => Some(source),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(source: std::io::Error) -> Self {
        TestError::Io(source)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(source: serde_json::Error) -> Self {
        TestError::Json(source)
    }
}

impl From<toml::de::Error> for TestError {
    fn from(source: toml::de::Error) -> Self {
        TestError::TomlDe(source)
    }
}

impl From<toml::ser::Error> for TestError {
    fn from(source: toml::ser::Error) -> Self {
        TestError::TomlSer(source)
    }
}

impl From<CatalogError> for TestError {
    fn from(source: CatalogError) -> Self {
        TestError::Catalog(source)
    }
}

/// Kurzform für `Result<T, TestError>`.
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
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

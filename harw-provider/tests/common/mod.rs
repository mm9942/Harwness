//! Test-Fehlertyp der Integrationstests: ersetzt panic!/unwrap/expect (Bible R087/R165/R182).

#![allow(dead_code)] // Gemeinsames Test-Gerüst: nicht jede Crate nutzt alle Varianten/Helfer.

/// Fehler eines Integrationstests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
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
    /// Ein Fehler aus der Provider-Schicht selbst (ersetzt `.unwrap()` auf
    /// `ProviderResult`).
    Provider(harw_provider::ProviderError),
    /// Ein Fehler beim Parsen einer URL (ersetzt `.unwrap()` auf `Url::parse`).
    UrlParse(url::ParseError),
}

pub type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "fehlender Wert: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Provider(source) => write!(f, "Provider-Fehler: {source}"),
            TestError::UrlParse(source) => write!(f, "URL-Parse-Fehler: {source}"),
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
            TestError::Provider(source) => Some(source),
            TestError::UrlParse(source) => Some(source),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<harw_provider::ProviderError> for TestError {
    fn from(source: harw_provider::ProviderError) -> Self {
        TestError::Provider(source)
    }
}

impl From<url::ParseError> for TestError {
    fn from(source: url::ParseError) -> Self {
        TestError::UrlParse(source)
    }
}

/// Baut aus einem statischen Kontext eine `map_err`-Closure (ersetzt `.expect("…")`).
///
/// # Arguments
/// - `context` (`&'static str`): was versucht wurde.
///
/// # Returns
/// Eine Closure, die einen fremden Fehler in [`TestError::Context`] übersetzt.
#[allow(dead_code)] // nicht jede Testdatei, die `mod common;` einbindet, nutzt `ctx`.
pub fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

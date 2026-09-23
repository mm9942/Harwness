//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! Ergänzt (statt ersetzt) [`crate::testing`]: `testing` ist eine öffentliche,
//! über `harw-plan-bridge`/`harw-ops`/`harw-tui` hinweg genutzte Fixture-API
//! und bleibt unverändert (siehe Bericht des Umbau-Auftrags). Dieses Modul
//! ist rein crate-intern (`pub(crate)`) und trägt die eigentliche
//! `Result`-Rückgabe für Testfunktionen, die vorher `.unwrap()`/`.expect(`
//! oder `panic!` nutzten.

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
    /// Ein Fehler aus `harw-plan` selbst (ersetzt `.unwrap()`/`.unwrap_err()`
    /// auf [`crate::error::PlanResult`]).
    Plan(crate::error::PlanError),
    /// Ein Fehler aus der Plan-Tool-Konfiguration.
    Config(crate::error::PlanToolConfigError),
    /// Ein I/O-Fehler (Temp-Verzeichnisse, Dateizugriffe in Store-Tests).
    Io(std::io::Error),
    /// Ein JSON-Fehler (Serialisierung/Deserialisierung in Tests).
    Json(serde_json::Error),
    /// Ein Fehler beim Parsen einer Ganzzahl (z. B. `RevisionId::from_str`).
    ParseInt(std::num::ParseIntError),
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "fehlender Wert: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Plan(source) => write!(f, "Plan-Fehler: {source}"),
            TestError::Config(source) => write!(f, "Konfigurationsfehler: {source}"),
            TestError::Io(source) => write!(f, "I/O-Fehler: {source}"),
            TestError::Json(source) => write!(f, "JSON-Fehler: {source}"),
            TestError::ParseInt(source) => write!(f, "Parse-Fehler: {source}"),
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
            TestError::Plan(source) => Some(source),
            TestError::Config(source) => Some(source),
            TestError::Io(source) => Some(source),
            TestError::Json(source) => Some(source),
            TestError::ParseInt(source) => Some(source),
            TestError::Missing(_) | TestError::Unexpected(_) | TestError::Context { .. } => None,
        }
    }
}

impl From<crate::error::PlanError> for TestError {
    fn from(source: crate::error::PlanError) -> Self {
        TestError::Plan(source)
    }
}

impl From<crate::error::PlanToolConfigError> for TestError {
    fn from(source: crate::error::PlanToolConfigError) -> Self {
        TestError::Config(source)
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

impl From<std::num::ParseIntError> for TestError {
    fn from(source: std::num::ParseIntError) -> Self {
        TestError::ParseInt(source)
    }
}

impl From<std::convert::Infallible> for TestError {
    fn from(source: std::convert::Infallible) -> Self {
        match source {}
    }
}

/// Baut aus einem statischen Kontext eine `map_err`-Closure (ersetzt `.expect("…")`).
///
/// # Arguments
/// - `context` (`&'static str`): was versucht wurde.
///
/// # Returns
/// Eine Closure, die einen fremden Fehler in [`TestError::Context`] übersetzt.
#[allow(dead_code)] // nicht jede Testdatei, die dieses Modul einbindet, nutzt `ctx`.
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

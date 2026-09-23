//! Test-Fehlertyp der Integrationstests: ersetzt panic!/unwrap/expect in
//! Tests (Bible R087/R165/R182).
//!
//! `TestError` macht jeden Testfehlschlag zu einem `Err`-Rückgabewert statt
//! einer Panik. Tests, die heute `#[test] fn x() { ... }` sind, werden zu
//! `fn x() -> TestResult { ...; Ok(()) }` umgebaut; `?` propagiert Fehler statt
//! `.unwrap()`/`.expect(...)` zu paniken.

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[derive(Debug)]
#[allow(dead_code)] // nicht jede Integrationstest-Datei nutzt jede Variante
pub enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form (ersetzt `panic!`/`unwrap_err`-Fehlschläge).
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Ein Fehler aus `harw_core::CoreError`.
    Core(harw_core::CoreError),
    /// Ein Fehler aus `harw_core::ModelError` (Model-Provider-Aufrufe in Tests).
    Model(harw_core::ModelError),
    /// Ein `serde_json`-Fehler (Serialisierung/Deserialisierung in Tests).
    Json(serde_json::Error),
    /// Ein I/O-Fehler (z. B. `tempfile`-Nutzung in Tests).
    Io(std::io::Error),
    /// Ein Fehler aus `harw_extension_api::AgentSpawnError` (Kind-Admission,
    /// -Ausführung und Lease-Reconciliation in Tests).
    Spawn(harw_extension_api::AgentSpawnError),
    /// Ein Fehler aus `harw_session_store::SessionStoreError` (z. B.
    /// `ChildLeaseStore`-Zugriffe in Tests).
    SessionStore(harw_session_store::SessionStoreError),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Core(e) => write!(f, "CoreError im Test: {e}"),
            Self::Model(e) => write!(f, "ModelError im Test: {e}"),
            Self::Json(e) => write!(f, "JSON-Fehler im Test: {e}"),
            Self::Io(e) => write!(f, "I/O-Fehler im Test: {e}"),
            Self::Spawn(e) => write!(f, "AgentSpawnError im Test: {e}"),
            Self::SessionStore(e) => write!(f, "SessionStoreError im Test: {e}"),
        }
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(e) => Some(e),
            Self::Model(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Spawn(e) => Some(e),
            Self::SessionStore(e) => Some(e),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<harw_core::CoreError> for TestError {
    fn from(value: harw_core::CoreError) -> Self {
        Self::Core(value)
    }
}

impl From<harw_core::ModelError> for TestError {
    fn from(value: harw_core::ModelError) -> Self {
        Self::Model(value)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<std::io::Error> for TestError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<harw_extension_api::AgentSpawnError> for TestError {
    fn from(value: harw_extension_api::AgentSpawnError) -> Self {
        Self::Spawn(value)
    }
}

impl From<harw_session_store::SessionStoreError> for TestError {
    fn from(value: harw_session_store::SessionStoreError) -> Self {
        Self::SessionStore(value)
    }
}

pub type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem statischen Kontext-Text eine `map_err`-Closure, die einen
/// beliebigen (`Display`-fähigen) Fremdfehler in `TestError::Context`
/// übersetzt — Ersatz für `.expect("…")` auf Fehlertypen ohne eigene
/// `TestError`-Variante.
#[allow(dead_code)] // nicht jede Integrationstest-Datei nutzt den Helfer
pub fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

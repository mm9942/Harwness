//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! `TestError` macht jeden Testfehlschlag zu einem `Err`-Rückgabewert statt
//! einer Panik. Tests, die heute `#[test] fn x() { ... }` sind, werden zu
//! `fn x() -> TestResult { ...; Ok(()) }` umgebaut; `?` propagiert Fehler statt
//! `.unwrap()`/`.expect(...)` zu paniken.

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[derive(Debug)]
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form (ersetzt `panic!`/`unwrap_err`-Fehlschläge).
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Ein Fehler aus dem Crate-eigenen `CoreError`.
    Core(crate::error::CoreError),
    /// Ein Fehler aus `ModelError` (Model-Provider-Aufrufe in Tests).
    Model(crate::model::ModelError),
    /// Ein `serde_json`-Fehler (Serialisierung/Deserialisierung in Tests).
    Json(serde_json::Error),
    /// Ein I/O-Fehler (z. B. `tempfile`-Nutzung in Tests).
    Io(std::io::Error),
    /// Ein Fehler aus `harw_context` (z. B. `SectionName`/Fragment-Validierung in Tests).
    HarwContext(harw_context::ContextError),
    /// Ein Fehler aus [`crate::execution_registry::ExecutionRegistryError`].
    ExecutionRegistry(crate::execution_registry::ExecutionRegistryError),
    /// Ein Fehler aus [`crate::state_store::StateStoreError`].
    StateStore(crate::state_store::StateStoreError),
    /// Ein Fehler aus `harw_session_store::SessionStoreError`.
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
            Self::HarwContext(e) => write!(f, "harw_context-Fehler im Test: {e}"),
            Self::ExecutionRegistry(e) => write!(f, "ExecutionRegistryError im Test: {e}"),
            Self::StateStore(e) => write!(f, "StateStoreError im Test: {e}"),
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
            Self::HarwContext(e) => Some(e),
            Self::ExecutionRegistry(e) => Some(e),
            Self::StateStore(e) => Some(e),
            Self::SessionStore(e) => Some(e),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<crate::error::CoreError> for TestError {
    fn from(value: crate::error::CoreError) -> Self {
        Self::Core(value)
    }
}

impl From<crate::model::ModelError> for TestError {
    fn from(value: crate::model::ModelError) -> Self {
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

impl From<harw_context::ContextError> for TestError {
    fn from(value: harw_context::ContextError) -> Self {
        Self::HarwContext(value)
    }
}

impl From<crate::execution_registry::ExecutionRegistryError> for TestError {
    fn from(value: crate::execution_registry::ExecutionRegistryError) -> Self {
        Self::ExecutionRegistry(value)
    }
}

impl From<crate::state_store::StateStoreError> for TestError {
    fn from(value: crate::state_store::StateStoreError) -> Self {
        Self::StateStore(value)
    }
}

impl From<harw_session_store::SessionStoreError> for TestError {
    fn from(value: harw_session_store::SessionStoreError) -> Self {
        Self::SessionStore(value)
    }
}

pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem statischen Kontext-Text eine `map_err`-Closure, die einen
/// beliebigen (`Display`-fähigen) Fremdfehler in `TestError::Context`
/// übersetzt — Ersatz für `.expect("…")` auf Fehlertypen ohne eigene
/// `TestError`-Variante.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

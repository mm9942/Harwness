//! Test-Fehlertyp dieses Crates: ersetzt `panic!`/`.unwrap()`/`.expect(…)` in
//! Tests (Bible R087/R165/R182). Jeder Fehlschlag wird als `Err`
//! zurückgegeben statt zu paniken.

#![allow(dead_code)] // Gemeinsames Test-Gerüst: nicht jede Crate nutzt alle Varianten/Helfer.

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu
/// paniken (Bible R087/R165/R182).
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `.expect("…")` auf `Result`-Werten).
    Context {
        /// Kurzbeschreibung der fehlgeschlagenen Operation.
        context: &'static str,
        /// Textform des ursprünglichen Fehlers.
        source: String,
    },
    /// I/O-Fehler beim Aufbau der Testumgebung (z. B. `tempfile::tempdir`).
    Io(std::io::Error),
    /// Fehler dieser Crate selbst (`collect_design_docs`, `collect_rust_sources`,
    /// `build_index`, `index_status`).
    Source(crate::SourceError),
    /// Fehler aus `harw-home` bei der Sichtbarkeitspfad-Auflösung
    /// (`harw_home::paths::visibility_index_dir`, direkt aufgerufen statt
    /// über eine Crate-Funktion dieses Crates).
    Home(harw_home::HomeError),
    /// Fehler aus `harw-lens-store` (`LensStore::open`, `LensStore::has_chunk`,
    /// direkt aufgerufen).
    Store(harw_lens_store::LensStoreError),
    /// Fehler aus `harw-lens-index` (`FlatIndex::load`, `FlatIndex::search`,
    /// direkt aufgerufen).
    Index(harw_lens_index::IndexError),
    /// Fehler aus `harw-lens-embed` (`Embedder::embed`, direkt aufgerufen).
    Embed(harw_lens_embed::EmbedError),
}

/// Kurzform für `Result<T, TestError>` in Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Io(err) => write!(f, "I/O-Fehler: {err}"),
            Self::Source(err) => write!(f, "Crate-Fehler: {err}"),
            Self::Home(err) => write!(f, "harw-home-Fehler: {err}"),
            Self::Store(err) => write!(f, "harw-lens-store-Fehler: {err}"),
            Self::Index(err) => write!(f, "harw-lens-index-Fehler: {err}"),
            Self::Embed(err) => write!(f, "harw-lens-embed-Fehler: {err}"),
        }
    }
}

impl fmt::Debug for TestError {
    /// Delegiert an [`fmt::Display`] (Bible: keine doppelte Fehlerdarstellung).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Source(err) => Some(err),
            Self::Home(err) => Some(err),
            Self::Store(err) => Some(err),
            Self::Index(err) => Some(err),
            Self::Embed(err) => Some(err),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<crate::SourceError> for TestError {
    fn from(err: crate::SourceError) -> Self {
        Self::Source(err)
    }
}

impl From<harw_home::HomeError> for TestError {
    fn from(err: harw_home::HomeError) -> Self {
        Self::Home(err)
    }
}

impl From<harw_lens_store::LensStoreError> for TestError {
    fn from(err: harw_lens_store::LensStoreError) -> Self {
        Self::Store(err)
    }
}

impl From<harw_lens_index::IndexError> for TestError {
    fn from(err: harw_lens_index::IndexError) -> Self {
        Self::Index(err)
    }
}

impl From<harw_lens_embed::EmbedError> for TestError {
    fn from(err: harw_lens_embed::EmbedError) -> Self {
        Self::Embed(err)
    }
}

/// Übersetzt einen Fremdfehler mit Kontext in einen [`TestError::Context`]
/// (ersetzt `.expect("…")` auf `Result`-Werten).
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

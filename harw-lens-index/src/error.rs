//! Fehlertyp für `harw-lens-index`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`IndexError`] für alle Fehler dieses Crates: die
//! zurückgewiesene Abfrage gegen ein abweichendes Manifest (die wichtigste
//! Zusage des Knotens AW4-06), eine Abfrage ohne das für den jeweiligen
//! Indextyp nötige Feld (`Query::embedding` bzw. `Query::text`), eine
//! abweichende Embedding-Dimension bei Einfügen oder Abfrage gegen
//! [`crate::FlatIndex`] (Knoten W10-L1), Fehler aus `harw-lens-store` beim
//! Laden/Speichern eines Index sowie JSON-(De-)Serialisierungsfehler des
//! Persistenzformats. `Display`, die
//! `Debug`-Delegation und
//! `std::error::Error` werden nicht von Hand geschrieben, sondern von
//! `#[derive(harw_macros::HarwError)]` erzeugt (Muster:
//! `harw-lens-types/src/error.rs`, `harw-lens-store/src/error.rs`). Kein
//! `anyhow`, kein `thiserror`. Weil der Enum-Name auf `Error` endet, erzeugt
//! das Makro außerdem den Typalias `IndexResult<T>`.
//!
//! Exportierte Typen: [`IndexError`], [`IndexResult`].
//!
//! # Nebenläufigkeit
//! `IndexError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit und
//! ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone` noch
//! `PartialEq` ab, weil die `Store`- und `Serde`-Varianten fremde Fehlertypen
//! wickeln, die selbst keines von beidem anbieten.
//!
//! # Examples
//! ```rust
//! use harw_lens_index::IndexError;
//!
//! let err = IndexError::ManifestMismatch { field: "model" };
//! assert_eq!(err.to_string(), "index manifests differ in field 'model'");
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (erzeugt den Typalias `IndexResult<T>`).
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zum Verstehen des
/// Fehlers ohne Quellcode-Lektüre nötig ist.
#[derive(Debug, HarwError)]
pub enum IndexError {
    /// Die wichtigste Zusage dieses Crates: eine Abfrage gegen ein Manifest,
    /// das in `model` (nur [`crate::FlatIndex`]) oder `chunker_version`
    /// (beide Indextypen) vom Manifest des Index abweicht. Entsteht
    /// ausschließlich **vor** jeder Berechnung, in
    /// [`crate::VectorIndex::search`].
    ///
    /// # Arguments
    /// - `field` (`&'static str`): der Name des abweichenden Feldes
    ///   (`"model"` oder `"chunker_version"`).
    #[msg("index manifests differ in field '{field}'")]
    ManifestMismatch {
        /// Name des abweichenden Feldes.
        field: &'static str,
    },

    /// [`crate::FlatIndex::search`] wurde mit einer [`crate::Query`] ohne
    /// `embedding` aufgerufen. Eine Vektorsuche ohne Vektor ist nicht
    /// beantwortbar.
    #[msg("query has no embedding, but FlatIndex::search requires one")]
    MissingEmbedding,

    /// [`crate::Bm25Index::search`] wurde mit einer [`crate::Query`] ohne
    /// `text` aufgerufen. Eine lexikalische Suche ohne Text ist nicht
    /// beantwortbar.
    #[msg("query has no text, but Bm25Index::search requires one")]
    MissingText,

    /// [`crate::FlatIndex`] hat ein Embedding gesehen, dessen Dimension von
    /// der Dimension aller bisherigen Embeddings dieses Index abweicht.
    /// Entsteht entweder beim Einfügen — [`crate::FlatIndex::build`] oder
    /// [`crate::FlatIndex::load`] prüfen jedes Embedding gegen die Dimension
    /// des ersten —, oder bei der Abfrage — [`crate::FlatIndex::search`]
    /// prüft `query.embedding` gegen die im Index festgestellte Dimension,
    /// **bevor** irgendeine Ähnlichkeit berechnet wird. Ein leerer Index hat
    /// keine festgestellte Dimension und kann diesen Fehler nicht auslösen.
    ///
    /// # Arguments
    /// - `expected` (`usize`): die Dimension, die der Index bereits
    ///   festgestellt hat (aus dem ersten Embedding bzw. den vorhandenen
    ///   Einträgen).
    /// - `actual` (`usize`): die abweichende Dimension des neu eingefügten
    ///   oder abgefragten Embeddings.
    #[msg("embedding dimension mismatch: expected {expected}, got {actual}")]
    EmbeddingDimensionMismatch {
        /// Die bereits festgestellte, erwartete Dimension.
        expected: usize,
        /// Die abweichende, tatsächlich erhaltene Dimension.
        actual: usize,
    },

    /// Kein Index namens `name` im übergebenen `LensStore` gefunden (weder
    /// Manifest noch Datenteil vorhanden). Entsteht ausschließlich in
    /// `load`.
    ///
    /// # Arguments
    /// - `name` (`String`): der angefragte, nicht gefundene Indexname.
    #[msg("no index named '{name}' exists in the store")]
    IndexNotFound {
        /// Der angefragte, nicht gefundene Indexname.
        name: String,
    },

    /// Das über [`harw_lens_store::LensStore::get_index_manifest`] separat
    /// geladene Manifest stimmt nicht mit dem Manifest überein, das im
    /// Datenteil desselben Index eingebettet ist. Entsteht ausschließlich in
    /// `load`, als zusätzliche Prüfung über die Digest-Prüfung des Stores
    /// hinaus (siehe Modul-Dokumentation von `flat` und `bm25` zum
    /// Persistenzformat).
    ///
    /// # Arguments
    /// - `name` (`String`): der Indexname, unter dem der Widerspruch
    ///   entdeckt wurde.
    #[msg(
        "index '{name}': manifest stored alongside the data does not match the manifest embedded in the data itself"
    )]
    PersistedManifestMismatch {
        /// Der Indexname, unter dem der Widerspruch entdeckt wurde.
        name: String,
    },

    /// Fehler aus `harw-lens-store` beim Lesen oder Schreiben eines Index;
    /// deferiert `Display`/`source()` an den inneren Fehler.
    #[from]
    Store(harw_lens_store::LensStoreError),

    /// JSON-(De-)Serialisierungsfehler des Persistenzformats; deferiert
    /// `Display`/`source()` an den inneren Fehler.
    #[from]
    Serde(serde_json::Error),
}

impl IndexError {
    /// Übersetzt das Ergebnis von
    /// [`harw_lens_types::IndexManifest::compatible_with`] in einen
    /// [`IndexError`].
    ///
    /// # Description
    /// `compatible_with` liefert nach seiner eigenen Implementierung
    /// ausschließlich [`harw_lens_types::LensTypesError::ManifestMismatch`]
    /// als Fehlerfall; die beiden übrigen Varianten von
    /// [`harw_lens_types::LensTypesError`] (`InvalidSpan`,
    /// `SpanOutOfBounds`) sind bei diesem Aufruf strukturell unerreichbar.
    /// Dieser Helfer übersetzt statt zweitzuschreiben, was `compatible_with`
    /// bereits geprüft hat — er vergleicht selbst keine Felder.
    ///
    /// # Arguments
    /// - `err` (`harw_lens_types::LensTypesError`): der von `compatible_with`
    ///   zurückgegebene Fehler.
    ///
    /// # Returns
    /// [`IndexError::ManifestMismatch`] mit dem betroffenen Feldnamen. Für
    /// den strukturell unerreichbaren Fall (eine andere Variante) liefert
    /// dieser Helfer denselben Fehler mit `field = "unknown"`, statt zu
    /// paniken.
    #[must_use]
    pub(crate) fn from_manifest_check(err: harw_lens_types::LensTypesError) -> Self {
        match err {
            harw_lens_types::LensTypesError::ManifestMismatch { field } => {
                Self::ManifestMismatch { field }
            }
            // Strukturell unerreichbar: `compatible_with` gibt nur
            // `ManifestMismatch` zurueck. Kein Panic, sondern ein
            // konservativer Fallback.
            harw_lens_types::LensTypesError::InvalidSpan { .. }
            | harw_lens_types::LensTypesError::SpanOutOfBounds { .. } => {
                Self::ManifestMismatch { field: "unknown" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_index_error_display_manifest_mismatch() {
        let err = IndexError::ManifestMismatch { field: "model" };
        assert_eq!(err.to_string(), "index manifests differ in field 'model'");
    }

    #[test]
    fn test_index_error_display_missing_embedding() {
        let err = IndexError::MissingEmbedding;
        assert_eq!(
            err.to_string(),
            "query has no embedding, but FlatIndex::search requires one"
        );
    }

    #[test]
    fn test_index_error_display_missing_text() {
        let err = IndexError::MissingText;
        assert_eq!(
            err.to_string(),
            "query has no text, but Bm25Index::search requires one"
        );
    }

    #[test]
    fn test_index_error_display_index_not_found() {
        let err = IndexError::IndexNotFound {
            name: "my-index".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "no index named 'my-index' exists in the store"
        );
    }

    #[test]
    fn test_index_error_display_embedding_dimension_mismatch() {
        let err = IndexError::EmbeddingDimensionMismatch {
            expected: 8,
            actual: 4,
        };
        assert_eq!(
            err.to_string(),
            "embedding dimension mismatch: expected 8, got 4"
        );
    }

    #[test]
    fn test_index_error_display_persisted_manifest_mismatch() {
        let err = IndexError::PersistedManifestMismatch {
            name: "my-index".to_owned(),
        };
        assert!(err.to_string().contains("my-index"));
    }

    #[test]
    fn test_index_error_from_manifest_check_translates_manifest_mismatch() {
        let inner = harw_lens_types::LensTypesError::ManifestMismatch {
            field: "chunker_version",
        };
        let err = IndexError::from_manifest_check(inner);
        assert!(matches!(
            err,
            IndexError::ManifestMismatch {
                field: "chunker_version"
            }
        ));
    }

    #[test]
    fn test_index_error_from_serde() -> TestResult {
        let Err(serde_err) = serde_json::from_str::<u32>("not json") else {
            return Err(TestError::Unexpected(
                "serde_json::from_str sollte an ungültigem JSON scheitern".into(),
            ));
        };
        let err: IndexError = serde_err.into();
        assert!(matches!(err, IndexError::Serde(_)));
        Ok(())
    }
}

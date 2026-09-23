//! Fehlertyp für `harw-lens-source`.
//!
//! # Verantwortungsbereich
//! Ein einziges Enum [`SourceError`] für alle Fehler dieser Crate: fehlerhafte
//! Sichtbarkeitsnamen bei der Pfadauflösung, Speicherfehler aus
//! `harw-lens-store`, Indexfehler aus `harw-lens-index`, Einbettungsfehler aus
//! `harw-lens-embed`, Dateisystem- und JSON-Fehler beim Lesen von
//! Design-Dokumenten bzw. beim Persistieren des internen
//! Embedding-Caches (siehe `crate::build`), zwei interne
//! Konsistenzfehler, die nur bei einem Programmierfehler in dieser Crate
//! selbst auftreten können, sowie [`SourceError::OperatorOnlyRemoteEmbed`]
//! (Knoten AW7-05): der Fail-Closed-Fehler, wenn `build_index` einen
//! `operator-only`-Bucket einem Embedder mit
//! [`harw_lens_types::Locality::Remote`] zuweisen würde. `Display`, die
//! `Debug`-Delegation und
//! `std::error::Error` werden nicht von Hand geschrieben, sondern von
//! `#[derive(harw_macros::HarwError)]` erzeugt (Muster:
//! `harw-lens-types/src/error.rs`, `harw-lens-store/src/error.rs`). Kein
//! `anyhow`, kein `thiserror`. Weil der Enum-Name auf `Error` endet, erzeugt
//! das Makro außerdem den Typalias `SourceResult<T>`.
//!
//! Exportierte Typen: [`SourceError`], [`SourceResult`].
//!
//! # Nebenläufigkeit
//! `SourceError` ist reine Daten (`Debug`) ohne interne Veränderlichkeit und
//! ohne Einschränkung zwischen Threads teilbar. Es leitet weder `Clone` noch
//! `PartialEq` ab, weil mehrere `#[from]`-Varianten fremde Fehlertypen
//! wickeln, die selbst keines von beidem anbieten (`std::io::Error`,
//! `serde_json::Error`, `harw_lens_store::LensStoreError`,
//! `harw_lens_index::IndexError`).
//!
//! # Examples
//! ```rust
//! use harw_lens_source::SourceError;
//!
//! let err = SourceError::MissingEmbedding {
//!     digest: "deadbeef".to_owned(),
//! };
//! assert!(err.to_string().contains("deadbeef"));
//! ```

use harw_macros::HarwError;

/// Fehler dieser Crate (erzeugt den Typalias `SourceResult<T>`).
///
/// # Description
/// Jede Variante trägt den vollständigen Kontext, der zum Verstehen des
/// Fehlers ohne Quellcode-Lektüre nötig ist.
#[derive(Debug, HarwError)]
pub enum SourceError {
    /// Ein `Embedder` hat für einen Batch eine andere Anzahl Vektoren
    /// geliefert, als Texte angefragt wurden. Entsteht ausschließlich in
    /// [`crate::build_index`], als Konsistenzprüfung direkt nach dem Aufruf
    /// von `Embedder::embed`.
    ///
    /// # Arguments
    /// - `expected` (`usize`): Anzahl angefragter Texte.
    /// - `actual` (`usize`): Anzahl tatsächlich gelieferter Vektoren.
    #[msg("embedder returned {actual} vector(s) for {expected} requested text(s)")]
    EmbedderCountMismatch {
        /// Anzahl angefragter Texte.
        expected: usize,
        /// Anzahl tatsächlich gelieferter Vektoren.
        actual: usize,
    },

    /// Ein Chunk hat beim Zusammenbau des Index weder eine zwischengespeicherte
    /// noch eine frisch berechnete Einbettung — ein interner
    /// Konsistenzfehler in [`crate::build_index`], kein aus Nutzereingaben
    /// erreichbarer Zustand.
    ///
    /// # Arguments
    /// - `digest` (`String`): der betroffene Chunk-Digest, bereits als
    ///   Hex-Zeichenkette formatiert.
    #[msg("chunk {digest} has neither a cached nor a freshly computed embedding")]
    MissingEmbedding {
        /// Der betroffene Chunk-Digest, als Hex-Zeichenkette.
        digest: String,
    },

    /// Der `operator-only`-Sichtbarkeits-Bucket wurde mit einem Embedder
    /// aufgerufen, dessen [`harw_lens_embed::Embedder::locality`]
    /// [`harw_lens_types::Locality::Remote`] meldet. Fail-Closed, wie
    /// [`harw_lens_embed::catalog::route`]s Behandlung von
    /// `EmbeddingRole::Confidential`: es wird nicht auf einen Teil-Erfolg
    /// ausgewichen, der `operator-only`-Chunks unembedded lässt -- der ganze
    /// Bucket-Build schlägt fehl, **bevor** `Embedder::embed` aufgerufen
    /// wird. Jeder Auftritt dieser Variante erhöht zugleich
    /// `crate::build::LENS_REMOTE_EMBED_ON_OPERATOR_ONLY`, den Nullzähler
    /// dieser Invariante (Knoten AW7-05).
    ///
    /// # Arguments
    /// - `index_name` (`String`): der Index, dessen `operator-only`-Bucket
    ///   betroffen ist.
    #[msg(
        "index '{index_name}': the operator-only visibility bucket was given a remote-locality \
         embedder; operator-only material must only ever be embedded locally"
    )]
    OperatorOnlyRemoteEmbed {
        /// Der Index, dessen `operator-only`-Bucket betroffen ist.
        index_name: String,
    },

    /// Fehler bei der Auflösung eines Sichtbarkeits-Indexpfads (ungültiger
    /// Sichtbarkeitsname); deferiert `Display`/`source()` an den inneren
    /// Fehler.
    #[from]
    Home(harw_home::HomeError),

    /// Dateisystem-I/O-Fehler beim Einlesen von Design-Dokumenten; deferiert
    /// `Display`/`source()` an den inneren Fehler.
    #[from]
    Io(std::io::Error),

    /// Fehler aus `harw-lens-store` beim Ablegen von Chunks, Indizes oder dem
    /// internen Embedding-Cache; deferiert `Display`/`source()` an den
    /// inneren Fehler.
    #[from]
    Store(harw_lens_store::LensStoreError),

    /// Fehler aus `harw-lens-index` beim Laden/Speichern eines
    /// [`harw_lens_index::FlatIndex`]; deferiert `Display`/`source()` an den
    /// inneren Fehler.
    #[from]
    Index(harw_lens_index::IndexError),

    /// Fehler aus `harw-lens-embed` beim Einbetten neuer Chunks; deferiert
    /// `Display`/`source()` an den inneren Fehler.
    #[from]
    Embed(harw_lens_embed::EmbedError),

    /// JSON-(De-)Serialisierungsfehler des internen Embedding-Caches;
    /// deferiert `Display`/`source()` an den inneren Fehler.
    #[from]
    Serde(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_source_error_display_embedder_count_mismatch() -> TestResult {
        let err = SourceError::EmbedderCountMismatch {
            expected: 3,
            actual: 1,
        };
        assert_eq!(
            err.to_string(),
            "embedder returned 1 vector(s) for 3 requested text(s)"
        );
        Ok(())
    }

    #[test]
    fn test_source_error_display_missing_embedding() -> TestResult {
        let err = SourceError::MissingEmbedding {
            digest: "deadbeef".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "chunk deadbeef has neither a cached nor a freshly computed embedding"
        );
        Ok(())
    }

    #[test]
    fn test_source_error_from_io() -> TestResult {
        let io_err = std::io::Error::other("boom");
        let err: SourceError = io_err.into();
        assert!(matches!(err, SourceError::Io(_)));
        Ok(())
    }

    #[test]
    fn test_source_error_from_serde() -> TestResult {
        let Err(serde_err) = serde_json::from_str::<u32>("not json") else {
            return Err(TestError::Unexpected(
                "expected serde_json::from_str to fail on invalid JSON".to_owned(),
            ));
        };
        let err: SourceError = serde_err.into();
        assert!(matches!(err, SourceError::Serde(_)));
        Ok(())
    }

    #[test]
    fn test_source_error_from_home() -> TestResult {
        let home_err = harw_home::HomeError::NoHomeDirectory;
        let err: SourceError = home_err.into();
        assert!(matches!(err, SourceError::Home(_)));
        Ok(())
    }

    #[test]
    fn test_source_error_display_operator_only_remote_embed() -> TestResult {
        let err = SourceError::OperatorOnlyRemoteEmbed {
            index_name: "knowledge.palace".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "index 'knowledge.palace': the operator-only visibility bucket was given a \
             remote-locality embedder; operator-only material must only ever be embedded locally"
        );
        Ok(())
    }
}

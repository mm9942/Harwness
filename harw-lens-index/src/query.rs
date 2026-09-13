//! Abfragevokabular für `harw-lens-index`.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`Query`]: eine Abfrage trägt neben ihrem Inhalt
//! (Embedding und/oder Text) das Manifest ihrer eigenen Herkunft mit. Das ist
//! die Grundlage der wichtigsten Zusage des Knotens AW4-06: [`VectorIndex::search`](crate::VectorIndex::search)
//! vergleicht dieses Manifest gegen das des Index, **bevor** irgendeine
//! Ähnlichkeit berechnet wird, damit eine Abfrage mit abweichendem Modell
//! oder abweichender Chunker-Version nie stillschweigend beantwortet wird.
//!
//! # Nebenläufigkeit
//! [`Query`] ist reine Daten (`Clone`, `PartialEq`) ohne interne
//! Veränderlichkeit und ohne Einschränkung zwischen Threads teilbar.
//!
//! # Fehler
//! Dieses Modul selbst erzeugt keine Fehler. Fehlt ein für den jeweiligen
//! Indextyp nötiges Feld (`embedding` für [`crate::FlatIndex`], `text` für
//! [`crate::Bm25Index`]), entsteht [`crate::IndexError::MissingEmbedding`]
//! bzw. [`crate::IndexError::MissingText`] erst in `search`, nicht hier.
//!
//! # Examples
//! ```rust
//! use harw_lens_index::Query;
//! use harw_lens_types::{IndexManifest, Locality, Metric};
//! use harw_types::ContentDigest;
//!
//! let manifest = IndexManifest {
//!     model: "text-embed-3".to_owned(),
//!     locality: Locality::Local,
//!     chunker_version: 1,
//!     visibility: "workspace".to_owned(),
//!     metric: Metric::Cosine,
//!     source_set_digest: ContentDigest::of(b"sources"),
//! };
//! let query = Query {
//!     embedding: Some(vec![1.0, 0.0]),
//!     text: Some("hallo welt".to_owned()),
//!     manifest,
//! };
//! assert!(query.embedding.is_some());
//! ```

use harw_lens_types::IndexManifest;

/// Eine Abfrage gegen einen [`crate::VectorIndex`], samt dem Manifest ihrer
/// eigenen Herkunft.
///
/// # Description
/// Trägt beide möglichen Inhaltsformen (`embedding` für Vektorsuche, `text`
/// für lexikalische Suche) gleichzeitig, weil ein Aufrufer nicht wissen muss,
/// gegen welchen konkreten Indextyp die Abfrage am Ende läuft — [`crate::FlatIndex`]
/// nutzt nur `embedding`, [`crate::Bm25Index`] nur `text`. `manifest` ist in
/// jedem Fall Pflicht: ohne die Herkunft der Abfrage lässt sich die
/// wichtigste Zusage dieses Crates nicht einlösen.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// Der Abfragevektor für eine Vektorsuche, falls vorhanden. `None`, wenn
    /// die Abfrage nicht auf ein Embedding zielt (z. B. eine rein
    /// lexikalische Anfrage).
    pub embedding: Option<Vec<f32>>,
    /// Der Abfragetext für eine lexikalische Suche, falls vorhanden. `None`,
    /// wenn die Abfrage nicht auf Text zielt (z. B. eine reine
    /// Vektorabfrage).
    pub text: Option<String>,
    /// Das Manifest, unter dem `embedding`/`text` erzeugt wurden. Wird von
    /// [`crate::VectorIndex::search`] gegen das Manifest des durchsuchten
    /// Index geprüft, bevor irgendetwas berechnet wird.
    pub manifest: IndexManifest,
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{Locality, Metric};
    use harw_types::ContentDigest;

    fn sample_manifest() -> IndexManifest {
        IndexManifest {
            model: "text-embed-3".to_owned(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    #[test]
    fn test_query_clone_and_equality() {
        let query = Query {
            embedding: Some(vec![1.0, 0.0]),
            text: Some("hallo".to_owned()),
            manifest: sample_manifest(),
        };
        assert_eq!(query.clone(), query);
    }

    #[test]
    fn test_query_allows_both_fields_absent() {
        let query = Query {
            embedding: None,
            text: None,
            manifest: sample_manifest(),
        };
        assert!(query.embedding.is_none());
        assert!(query.text.is_none());
    }
}

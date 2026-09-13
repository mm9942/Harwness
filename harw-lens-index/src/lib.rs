//! Vektor- und BM25-Index.
//!
//! # Verantwortungsbereich
//! Besitzt den Vertrag [`VectorIndex`] und zwei Implementierungen:
//! [`FlatIndex`] (exakte Vektorsuche ohne Näherung, die Referenz — siehe
//! `flat.rs`) und [`Bm25Index`] (lexikalische Suche — siehe `bm25.rs`, samt
//! der dort dokumentierten bewussten Doppelung mit `harw-knowledge`). Baut
//! oder liest keinen Chunk-Text selbst (das liefert `harw-lens-chunk`
//! bereits fertig zerlegt) und tut keine eigene Persistenzmechanik (das
//! liefert `harw-lens-store`) — dieses Crate interpretiert Chunks und
//! Embeddings zu einer durchsuchbaren Struktur, mehr nicht.
//!
//! # Die wichtigste Zusage
//! **Eine Abfrage gegen ein abweichendes Modell oder eine abweichende
//! `chunker_version` wird abgelehnt — niemals stillschweigend beantwortet.**
//! Ein Index, der mit Modell A gebaut und mit Vektoren aus Modell B
//! abgefragt wird, liefert Treffer. Sie sind Unsinn, aber sie sehen aus wie
//! Treffer, und niemand merkt es. Deshalb trägt jede [`Query`] das Manifest
//! ihrer eigenen Herkunft mit, und
//! [`VectorIndex::search`] vergleicht es gegen das Manifest des Index,
//! **bevor** irgendeine Ähnlichkeit oder Relevanz berechnet wird:
//! [`FlatIndex`] über [`harw_lens_types::IndexManifest::compatible_with`]
//! (`model` und `chunker_version`), [`Bm25Index`] nur über
//! `chunker_version` (er hat kein Modell, siehe `bm25.rs`).
//!
//! Kein ANN: `FlatIndex` ist eine Schleife über alle Vektoren, bewusst ohne
//! beschleunigende Datenstruktur — siehe `flat.rs` dazu, warum das die
//! Referenz ist, gegen die jede spätere Näherung gemessen wird.
//!
//! # Nebenläufigkeit
//! [`VectorIndex`] ist `Send + Sync`; [`FlatIndex`] und [`Bm25Index`] sind
//! reine Daten ohne interne Veränderlichkeit, `search` nimmt `&self` und ist
//! sicher aus mehreren Threads gleichzeitig aufrufbar.
//!
//! # Fehler
//! [`IndexError`] (Typalias [`IndexResult`]) ist der einzige Fehlertyp
//! dieser Crate. Siehe `error.rs` für die vollständige Variantenliste.
//!
//! # Examples
//! ```rust
//! use harw_lens_index::{FlatIndex, Query, VectorIndex};
//! use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, IndexManifest, Locality, Metric, SourceRef};
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
//! let chunk = Chunk {
//!     digest: ChunkDigest(ContentDigest::of(b"hello")),
//!     source: SourceRef::File { path: "a.txt".to_owned() },
//!     span: ByteSpan::new(0, 5).expect("valid span"),
//!     text: "hello".to_owned(),
//! };
//! let index = FlatIndex::build(manifest.clone(), vec![(chunk, vec![1.0, 0.0])]);
//!
//! // Eine Abfrage mit abweichendem Modell wird abgelehnt, nie beantwortet.
//! let mut wrong_model = manifest.clone();
//! wrong_model.model = "other-model".to_owned();
//! let bad_query = Query { embedding: Some(vec![1.0, 0.0]), text: None, manifest: wrong_model };
//! assert!(index.search(&bad_query, 10).is_err());
//!
//! let good_query = Query { embedding: Some(vec![1.0, 0.0]), text: None, manifest };
//! assert_eq!(index.search(&good_query, 10).expect("compatible manifest").len(), 1);
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entstand in
//! Knoten **AW4-06**; Ebene **L3** im Zielgraphen.

mod bm25;
mod error;
mod flat;
mod query;
mod sort;
mod vector_index;

pub use bm25::Bm25Index;
pub use error::{IndexError, IndexResult};
pub use flat::FlatIndex;
pub use query::Query;
pub use vector_index::VectorIndex;

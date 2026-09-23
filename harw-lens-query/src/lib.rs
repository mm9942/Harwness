//! Abfragepfad: eine Frage rein, `Ranked`-Treffer raus.
//!
//! # Verantwortungsbereich
//! Diese Crate ist der Lesepfad des Lens-Retrieval-Layers, das Gegenstück zu
//! `harw-lens-source` (dem Schreibpfad). Sie besitzt:
//!
//! - [`IndexSelector`]/[`ReadScope`] — welcher Index/welche Sichtbarkeit, und
//!   wer welche Sichtbarkeiten befragen darf.
//! - [`resolve_index`] — die eine Stelle, an der ein Selektor gegen den
//!   Lesebereich geprüft wird, **bevor** ein Dateisystemzugriff stattfindet.
//! - [`query`]/[`query_scoped`] — betten die Frage ein, fragen den Index ab,
//!   lassen Duplikate über `harw_lens_rank::collapse` zusammenfallen.
//! - [`QueryError`] — der eine Fehlertyp dieser Crate.
//!
//! Diese Crate baut und schreibt **keinen** Index — das ist
//! `harw-lens-source`s Aufgabe. Sie implementiert auch **kein** Kollabieren
//! neu — sie ruft `harw_lens_rank::collapse` auf, das bereits existiert.
//!
//! # Warum ein Selektor außerhalb des Lesebereichs einen Fehler liefert, nie eine leere Trefferliste
//! Eine leere Trefferliste ist für einen Aufrufer nicht von „nichts
//! gefunden" zu unterscheiden — beide sehen identisch aus. Würde
//! [`resolve_index`] einen zu weit gefassten Selektor stillschweigend als
//! leeres Ergebnis beantworten, hielte der Aufrufer eine **unvollständige**
//! Antwort für eine **vollständige**, und das ist schlimmer als ein Fehler:
//! ein Fehler wird bemerkt und behandelt, ein falsch-negatives „nichts
//! gefunden" wird es nicht. [`resolve_index`] prüft den [`ReadScope`] deshalb
//! als **erste** Handlung, bevor überhaupt ein Pfad aufgelöst oder eine Datei
//! angefasst wird, und liefert bei Verstoß
//! [`QueryError::IndexNotVisible`] — nie `Ok(vec![])`.
//!
//! Das ist dieselbe Struktur wie `harw-lens-source`s physische
//! Sichtbarkeitstrennung, nur auf der Leseseite gespiegelt: dort verhindert
//! ein getrennter physischer Index, dass geschütztes Material überhaupt
//! *gefunden werden kann*; hier verhindert eine Prüfung vor jedem
//! Dateisystemzugriff, dass eine unzulässige Anfrage überhaupt *gestellt
//! werden kann*. Beide Hälften zusammen — nicht nur eine — sind der Grund,
//! warum ein `OperatorOnly`-Artefakt weder über einen falschen Index noch
//! über eine falsch gescopte Anfrage erreichbar ist.
//!
//! # Der Abfragepfad im Detail
//! `prepare_query` (aus `harw-lens-embed`) setzt das Abfragepräfix, **nie**
//! am Aufrufort geschrieben — siehe [`query`]s Dokumentation für die exakte
//! Stelle und für die Begründung, warum `Query::text` (für lexikalische
//! Suche) bewusst *unpräfixiert* bleibt.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Crate sind reine Daten oder zustandslose Funktionen.
//! [`query`] und [`query_scoped`] halten keinen Zustand zwischen Aufrufen;
//! sicher aus mehreren Threads parallel aufrufbar.
//!
//! # Fehler
//! [`QueryError`] (Typalias [`QueryResult`]) ist der einzige Fehlertyp
//! dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{DeterministicEmbedder, Embedder, EmbeddingDescriptor};
//! use harw_lens_index::FlatIndex;
//! use harw_lens_query::{query, QueryProvenance};
//! use harw_lens_types::{
//!     ByteSpan, Chunk, ChunkDigest, CollapsePolicy, EdgeIndex, IndexManifest, Locality, Metric,
//!     SourceRef,
//! };
//! use harw_types::ContentDigest;
//!
//! let descriptor = EmbeddingDescriptor {
//!     document_prefix: "passage: ".to_owned(),
//!     query_prefix: "query: ".to_owned(),
//!     normalize: false,
//! };
//! let embedder = DeterministicEmbedder::new(8);
//! let text = "hallo welt";
//! let embedding = embedder
//!     .embed(&[harw_lens_embed::prepare_document(&descriptor, text)])
//!     .expect("embeds")
//!     .pop()
//!     .expect("one vector");
//! let chunk = Chunk {
//!     digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
//!     source: SourceRef::File { path: "a.txt".to_owned() },
//!     span: ByteSpan::new(0, text.len()).expect("valid span"),
//!     text: text.to_owned(),
//! };
//! let manifest = IndexManifest {
//!     model: "test-model".to_owned(),
//!     locality: Locality::Local,
//!     chunker_version: 1,
//!     visibility: "workspace".to_owned(),
//!     metric: Metric::Cosine,
//!     source_set_digest: ContentDigest::of(b"sources"),
//! };
//! let index = FlatIndex::build(manifest, vec![(chunk, embedding)])
//!     .expect("consistent embedding dimension");
//!
//! // Die Provenienz sagt, womit *diese Abfrage* eingebettet wurde. Sie wird
//! // bewusst nicht aus `index.manifest()` abgeleitet — sonst vergliche die
//! // Kompatibilitätsprüfung den Index mit sich selbst und könnte nie
//! // fehlschlagen (siehe `QueryProvenance`).
//! let provenance = QueryProvenance {
//!     model: "test-model".to_owned(),
//!     chunker_version: 1,
//! };
//!
//! let hits = query(
//!     &index,
//!     "hallo welt",
//!     &embedder,
//!     &descriptor,
//!     &provenance,
//!     &EdgeIndex::default(),
//!     CollapsePolicy::ByDigest,
//!     10,
//! )
//! .expect("query succeeds");
//! assert_eq!(hits.len(), 1);
//! ```
//!
//! # Stand
//! Knoten **AW5-08**; Ebene **L4** im Zielgraphen. Abhängigkeiten
//! `harw-lens-types`, `harw-lens-index`, `harw-lens-store`, `harw-lens-embed`,
//! `harw-lens-rank` (alle vorgelagert gelandet) und `harw-home` (AW0-02).

mod error;
mod query;
mod resolve;
mod selector;

pub use error::{QueryError, QueryResult};
pub use query::{query, query_scoped, QueryProvenance};
pub use resolve::resolve_index;
pub use selector::{IndexSelector, ReadScope};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

//! Föderation über mehrere Indizes: eine Frage rein, eine Antwort raus.
//!
//! # Verantwortungsbereich
//! Diese Crate baut **keinen** neuen Selektor- oder Sichtbarkeitstyp: sie
//! erweitert `harw-lens-query`s bereits bestehende Einzelindex-Bausteine
//! ([`harw_lens_query::IndexSelector`], [`harw_lens_query::ReadScope`],
//! [`harw_lens_query::resolve_index`], [`harw_lens_query::query`]) auf
//! mehrere Indizes gleichzeitig, statt eine zweite, gleichbedeutende Fassung
//! davon zu bauen. Sie besitzt:
//!
//! - [`federated_query`]: löst mehrere Selektoren auf, fragt jeden ab und
//!   verschmilzt die Ergebnisse über [`harw_lens_rank::rrf_fuse`].
//! - [`federated_pack`]: füllt aus dem verschmolzenen Ergebnis ein einziges
//!   Budget über [`harw_lens_rank::pack`] -- derselbe Vertrag, den
//!   `harw-core::Assembly::budget` (Montage) laut `docs/aw-contract-master.md`
//!   ebenfalls einlösen soll (siehe [`federated_pack`]s
//!   Moduldokumentation für den dokumentierten Befund, dass das heute noch
//!   nicht der Fall ist).
//! - [`FederationError`]: der eine Fehlertyp dieser Crate, ein dünner Wickel
//!   um [`harw_lens_query::QueryError`].
//!
//! Diese Crate implementiert weder RRF noch die Budgetfüllung noch die
//! Duplikaterkennung neu -- sie ruft ausschließlich vorhandene Funktionen aus
//! `harw-lens-rank` und `harw-lens-query` auf. Siehe die Moduldokumentation
//! von [`federated_query`] für die ausführliche Begründung der Sichtbarkeits-,
//! Provenienz- und Determinismus-Entscheidungen dieses Knotens.
//!
//! # Nebenläufigkeit
//! Alle exportierten Funktionen sind zustandslos zwischen Aufrufen; sicher
//! aus mehreren Threads parallel aufrufbar, solange der übergebene `Embedder`
//! es selbst ist.
//!
//! # Fehler
//! [`FederationError`] (Typalias `FederationResult<T>`) ist der einzige
//! Fehlertyp dieser Crate.
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
//! use harw_lens_federation::federated_query;
//! use harw_lens_query::{IndexSelector, QueryProvenance, ReadScope};
//! use harw_lens_types::{CollapsePolicy, EdgeIndex};
//!
//! let home = std::path::Path::new("/does/not/matter/for/this/check");
//! let selectors = [IndexSelector::new("knowledge.palace", "operator-only")];
//! let scope = ReadScope::single("workspace");
//! let embedder = DeterministicEmbedder::new(8);
//! let descriptor = EmbeddingDescriptor {
//!     document_prefix: "passage: ".to_owned(),
//!     query_prefix: "query: ".to_owned(),
//!     normalize: false,
//! };
//! let provenance = QueryProvenance { model: "m".to_owned(), chunker_version: 1 };
//!
//! // Ein unsichtbarer Selektor bricht die gesamte Foederation ab -- nie ein
//! // stillschweigend gekuerztes Ergebnis.
//! let err = federated_query(
//!     home,
//!     &selectors,
//!     &scope,
//!     "does it matter",
//!     &embedder,
//!     &descriptor,
//!     &provenance,
//!     &EdgeIndex::default(),
//!     CollapsePolicy::ByDigest,
//!     10,
//!     60.0,
//! )
//! .unwrap_err();
//! assert!(err.to_string().contains("outside the caller's read scope"));
//! ```
//!
//! # Stand
//! Inhalt aus Knoten **AW6-07**; Ebene **L4** im Zielgraphen. Abhängigkeiten
//! `harw-lens-types`, `harw-lens-index`, `harw-lens-embed`, `harw-lens-rank`,
//! `harw-lens-query` (alle vorgelagert gelandet).

mod error;
mod federated;

pub use error::{FederationError, FederationResult};
pub use federated::{federated_pack, federated_query, FederatedOutcome, SkipReason, SkippedIndex};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

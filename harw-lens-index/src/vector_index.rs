//! Der `VectorIndex`-Vertrag für `harw-lens-index`.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich den Trait [`VectorIndex`]: das gemeinsame
//! Verhalten, gegen das [`crate::FlatIndex`] und [`crate::Bm25Index`]
//! implementieren. Der Trait selbst prüft nichts — jede Implementierung
//! muss die Manifest-Prüfung aus `# Fehler` unten selbst durchführen, bevor
//! sie irgendetwas berechnet.
//!
//! # Nebenläufigkeit
//! `VectorIndex: Send + Sync`, weil Indizes hinter `&dyn VectorIndex` oder
//! `Arc<dyn VectorIndex>` geteilt und aus mehreren Threads gleichzeitig
//! abgefragt werden. `search` nimmt `&self`: keine Implementierung darf
//! veränderlichen Zustand über einen Aufruf hinaus benötigen.
//!
//! # Fehler
//! Jede Implementierung von `search` muss
//! [`crate::IndexError::ManifestMismatch`] zurückgeben, **bevor** sie
//! irgendeine Ähnlichkeit oder Relevanz berechnet, wenn das Manifest der
//! Abfrage vom Manifest des Index in einem relevanten Feld abweicht — das
//! ist die wichtigste Zusage des Knotens AW4-06.
//!
//! # Examples
//! ```rust,no_run
//! use harw_lens_index::{FlatIndex, Query, VectorIndex};
//!
//! fn print_top_hit(index: &dyn VectorIndex, query: &Query) {
//!     match index.search(query, 1) {
//!         Ok(hits) => println!("{} Treffer", hits.len()),
//!         Err(err) => eprintln!("Abfrage abgelehnt: {err}"),
//!     }
//! }
//! # let _ = print_top_hit as fn(&dyn VectorIndex, &Query);
//! ```

use harw_lens_types::{IndexManifest, Ranked};

use crate::error::IndexError;
use crate::query::Query;

/// Ein durchsuchbarer Index über Chunks.
///
/// # Description
/// Verbindliche Signatur zwischen `harw-lens-index` und seinen Konsumenten
/// (`harw-lens-query`, `harw-lens-federation`, nachgelagert). Zwei
/// Implementierungen liegen in diesem Crate: [`crate::FlatIndex`] (exakte
/// Vektorsuche, die Referenz) und [`crate::Bm25Index`] (lexikalische Suche).
pub trait VectorIndex: Send + Sync {
    /// Das Manifest, das dieser Index über sich selbst behauptet.
    ///
    /// # Description
    /// Wird von Konsumenten genutzt, die vor dem eigentlichen `search`-Aufruf
    /// bereits prüfen wollen, gegen welchen Index sie sprechen (z. B. um eine
    /// passende Abfrage zu bauen), sowie von `save`, das dieses Manifest
    /// unverändert an [`harw_lens_store::LensStore::put_index`] weiterreicht.
    ///
    /// # Returns
    /// Eine geliehene Referenz auf das [`IndexManifest`] dieses Index.
    fn manifest(&self) -> &IndexManifest;

    /// Sucht die `limit` besten Treffer zu `query`.
    ///
    /// # Description
    /// Vergleicht zuerst `query.manifest` gegen [`Self::manifest`] — die
    /// wichtigste Zusage dieses Crates: eine Abfrage gegen ein abweichendes
    /// Modell oder eine abweichende `chunker_version` wird abgelehnt, bevor
    /// irgendeine Ähnlichkeit berechnet wird. Erst danach bewertet die
    /// Implementierung ihre Kandidaten, sortiert absteigend nach
    /// [`Ranked::score`] und schneidet auf `limit` zu. Ein leerer Index ist
    /// kein Fehler: er liefert eine leere Liste.
    ///
    /// # Arguments
    /// - `query` (`&Query`): die Abfrage, samt dem Manifest ihrer Herkunft.
    /// - `limit` (`usize`): die maximale Anzahl zurückgegebener Treffer.
    ///
    /// # Returns
    /// Bis zu `limit` [`Ranked`]-Treffer, absteigend nach `score` sortiert.
    /// Bei Punktgleichheit entscheidet ein stabiler Tiebreak über
    /// `ChunkDigest`, damit dieselbe Abfrage immer dieselbe Reihenfolge
    /// liefert.
    ///
    /// # Errors
    /// - [`IndexError::ManifestMismatch`]: `query.manifest` weicht in
    ///   `model` (nur [`crate::FlatIndex`]) oder `chunker_version` (beide
    ///   Indextypen) vom Manifest dieses Index ab.
    /// - [`IndexError::MissingEmbedding`]: [`crate::FlatIndex::search`] ohne
    ///   `query.embedding`.
    /// - [`IndexError::MissingText`][]: [`crate::Bm25Index::search`] ohne
    ///   `query.text`.
    /// - [`IndexError::EmbeddingDimensionMismatch`][]: nur
    ///   [`crate::FlatIndex::search`] — `query.embedding` hat eine andere
    ///   Länge als die im Index gespeicherten Embeddings (Knoten W10-L1).
    ///
    /// # Concurrency
    /// Nimmt `&self`; sicher aus mehreren Threads gleichzeitig aufrufbar.
    fn search(&self, query: &Query, limit: usize) -> Result<Vec<Ranked>, IndexError>;
}

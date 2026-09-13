//! Der Abfragepfad: eine Frage rein, `Ranked`-Treffer raus.
//!
//! # Verantwortungsbereich
//! Besitzt [`query`] (Abfrage gegen einen bereits vorliegenden
//! [`VectorIndex`]) und [`query_scoped`] (löst zuerst über
//! [`crate::resolve_index`] auf, dann ruft [`query`]). Beide Funktionen
//! wenden [`prepare_query`] auf den Abfragetext an, **bevor** er an den
//! Embedder geht — das Abfragepräfix wird an keiner anderen Stelle von Hand
//! geschrieben. Das Ranking selbst (Ähnlichkeit, Sortierung) liegt bei
//! [`VectorIndex::search`]; das Entdoppeln liegt bei
//! [`harw_lens_rank::collapse`] — diese Funktion ruft ihn nur auf, sie
//! implementiert ihn nicht erneut.
//!
//! # Warum `Query::text` den unpräfixierten Text trägt
//! [`EmbeddingDescriptor::query_prefix`] ist eine Konvention für
//! *Embedding*-Modelle (z. B. `"query: "` vs. `"passage: "`); eine
//! lexikalische BM25-Suche (`harw_lens_index::Bm25Index`) tokenisiert
//! `Query::text` dagegen wörtlich und hätte durch ein angehängtes Präfix
//! einen zusätzlichen, bedeutungslosen Term in jeder Abfrage. Diese Funktion
//! wendet [`prepare_query`] deshalb ausschließlich auf den Text an, der an
//! den `Embedder` geht (`Query::embedding`), nicht auf `Query::text`.
//!
//! # Nebenläufigkeit
//! [`query`] hält keinen Zustand zwischen Aufrufen; sicher aus mehreren
//! Threads parallel aufrufbar, solange `index`/`embedder` es selbst sind
//! (beide sind `Send + Sync`, siehe ihre jeweilige Crate).
//!
//! # Fehler
//! Siehe [`crate::QueryError`] für die vollständige Variantenliste.
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
//! let index = FlatIndex::build(manifest, vec![(chunk, embedding)]);
//!
//! let provenance = QueryProvenance {
//!     model: "test-model".to_owned(),
//!     chunker_version: 1,
//! };
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

use harw_lens_embed::{prepare_query, Embedder, EmbeddingDescriptor};
use harw_lens_index::{Query, VectorIndex};
use harw_lens_rank::collapse;
use harw_lens_types::{CollapsePolicy, EdgeIndex, IndexManifest, Ranked};
use std::path::Path;

use crate::error::QueryError;
use crate::resolve::resolve_index;
use crate::selector::{IndexSelector, ReadScope};

/// Womit der Aufrufer seine Abfrage eingebettet hat.
///
/// # Description
/// Ein Index ist an das Modell gebunden, mit dem er gebaut wurde, und an die
/// Zerlegungsfassung, aus der seine Chunks stammen. Eine Abfrage, die mit
/// einem anderen Modell eingebettet wurde, liefert **Treffer** — sie sind
/// Unsinn, aber sie sehen aus wie Treffer, und niemand merkt es.
///
/// Dieser Typ existiert, damit der Aufrufer sagen **muss**, womit er
/// gearbeitet hat. [`query`] vergleicht das gegen das Manifest des Index und
/// lehnt eine Abweichung ab, **bevor** gerechnet wird.
///
/// # Warum nicht aus dem Index ableiten
/// Genau das war der Fehler der ersten Fassung: sie nahm das Manifest des
/// Index, den sie gleich durchsuchte, als Manifest der Abfrage. Der Vergleich
/// war damit tautologisch und konnte nie fehlschlagen — eine dokumentierte
/// Zusage ohne jede Wirkung.
///
/// # Examples
/// ```rust
/// use harw_lens_query::QueryProvenance;
///
/// let provenance = QueryProvenance {
///     model: "local-minilm-l6-v2".to_owned(),
///     chunker_version: 1,
/// };
/// assert_eq!(provenance.chunker_version, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProvenance {
    /// Der Name des Modells, mit dem die Abfrage eingebettet wurde.
    pub model: String,
    /// Die Zerlegungsfassung, gegen die der Aufrufer arbeitet.
    pub chunker_version: u32,
}

/// Fragt einen bereits vorliegenden [`VectorIndex`] ab und kollabiert das
/// Ergebnis.
///
/// # Description
/// Baut den Abfragevektor über [`prepare_query`] (Präfix) und
/// [`Embedder::embed`], reicht ihn zusammen mit dem unpräfixierten
/// `question`-Text (siehe `# Warum Query::text den unpräfixierten Text
/// trägt` in der Moduldokumentation) und `index.manifest()` als
/// [`Query`] an [`VectorIndex::search`] weiter, und lässt Duplikate im
/// Ergebnis über [`harw_lens_rank::collapse`] zusammenfallen.
///
/// # Arguments
/// - `index` (`&dyn VectorIndex`): der abzufragende Index.
/// - `question` (`&str`): der rohe, unpräfixierte Abfragetext.
/// - `embedder` (`&dyn Embedder`): berechnet den Abfragevektor.
/// - `descriptor` (`&EmbeddingDescriptor`): liefert das Abfrage-Präfix für
///   [`prepare_query`].
/// - `provenance` (`&QueryProvenance`): **womit der Aufrufer eingebettet
///   hat.** Wird gegen das Manifest des Index geprüft, bevor gerechnet wird.
///   Siehe [`QueryProvenance`] für den Grund, warum dieser Wert nicht aus dem
///   Index abgeleitet werden darf.
/// - `edges` (`&EdgeIndex`): bekannte `SupersededBy`-/`Contradicts`-Kanten
///   zwischen Chunks, an [`harw_lens_rank::collapse`] weitergereicht.
/// - `collapse_policy` (`CollapsePolicy`): wonach Duplikate erkannt werden.
/// - `limit` (`usize`): die maximale Anzahl an [`VectorIndex::search`]
///   weitergereichter Treffer, **vor** dem Kollabieren.
///
/// # Returns
/// Die von `index.search` gelieferten Treffer, nach `collapse_policy`
/// entdoppelt.
///
/// # Errors
/// - [`QueryError::Embed`]: der Embedder schlägt fehl.
/// - [`QueryError::EmptyEmbedding`]: der Embedder liefert keinen Vektor für
///   den (einzelnen) Abfragetext.
/// - [`QueryError::Index`]: `index.search` lehnt die Abfrage ab — insbesondere
///   wenn `provenance` nicht zum Manifest des Index passt (abweichendes
///   Modell oder abweichende Zerlegungsfassung).
///
/// # Examples
/// Siehe die Moduldokumentation.
// Acht Argumente statt sieben: die Provenienz (K43) kam hinzu, und sie durch
// ein Sammelstruct zu ersetzen verschöbe die Frage nur -- der Aufrufer müsste
// es dann füllen.
#[allow(clippy::too_many_arguments)]
pub fn query(
    index: &dyn VectorIndex,
    question: &str,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
    provenance: &QueryProvenance,
    edges: &EdgeIndex,
    collapse_policy: CollapsePolicy,
    limit: usize,
) -> Result<Vec<Ranked>, QueryError> {
    let prepared = prepare_query(descriptor, question);
    let mut vectors = embedder.embed(std::slice::from_ref(&prepared))?;
    let embedding = vectors.pop().ok_or(QueryError::EmptyEmbedding)?;

    // Das Manifest der Abfrage beschreibt, **womit der Aufrufer eingebettet
    // hat** — nicht, womit der Index gebaut wurde.
    //
    // Die erste Fassung nahm hier `index.manifest().clone()`, also das
    // Manifest genau des Index, den sie gleich durchsucht. Damit konnte
    // `IndexManifest::compatible_with` **nie** fehlschlagen: die
    // dokumentierte Zusage „eine Abfrage gegen ein abweichendes Modell wird
    // abgelehnt" war über den vorgesehenen Einstiegspunkt nicht auslösbar.
    // Der Unit-Test in `harw-lens-index` bestand, weil er sich sein
    // abweichendes `Query` von Hand baute — kein echter Aufrufer konnte
    // diesen Zustand erzeugen.
    //
    // `locality`, `visibility` und `source_set_digest` beschreiben den Index,
    // nicht die Abfrage; sie werden übernommen, damit `compatible_with`
    // genau die zwei Felder vergleicht, über die eine Abfrage etwas aussagen
    // kann.
    let query_manifest = IndexManifest {
        model: provenance.model.clone(),
        chunker_version: provenance.chunker_version,
        ..index.manifest().clone()
    };

    let search_query = Query {
        embedding: Some(embedding),
        text: Some(question.to_owned()),
        manifest: query_manifest,
    };
    let hits = index.search(&search_query, limit)?;
    Ok(collapse(&hits, collapse_policy, edges))
}

/// Löst `selector` gegen `scope` auf und fragt den resultierenden Index ab.
///
/// # Description
/// Kombiniert [`resolve_index`] und [`query`]. Ein `selector` außerhalb von
/// `scope` scheitert bereits in [`resolve_index`] mit
/// [`QueryError::IndexNotVisible`], bevor `question` überhaupt eingebettet
/// wird.
///
/// # Arguments
/// - `home` (`&Path`): der Root-Space, unter dem
///   [`harw_home::paths::visibility_index_dir`] aufgelöst wird.
/// - `selector` (`&IndexSelector`): welcher Index, welche Sichtbarkeit.
/// - `scope` (`&ReadScope`): welche Sichtbarkeiten der Aufrufer befragen
///   darf.
/// - `question`, `embedder`, `descriptor`, `edges`, `collapse_policy`,
///   `limit`: siehe [`query`].
///
/// # Returns
/// Siehe [`query`].
///
/// # Errors
/// Siehe [`resolve_index`] und [`query`] für die vollständige
/// Variantenliste.
///
/// # Examples
/// ```rust
/// use harw_lens_query::{IndexSelector, QueryError, QueryProvenance, ReadScope, query_scoped};
/// use harw_lens_embed::{DeterministicEmbedder, EmbeddingDescriptor};
/// use harw_lens_types::{CollapsePolicy, EdgeIndex};
///
/// let home = std::path::Path::new("/does/not/matter/for/this/check");
/// let selector = IndexSelector::new("knowledge.palace", "operator-only");
/// let scope = ReadScope::single("workspace");
/// let embedder = DeterministicEmbedder::new(8);
/// let descriptor = EmbeddingDescriptor {
///     document_prefix: "passage: ".to_owned(),
///     query_prefix: "query: ".to_owned(),
///     normalize: false,
/// };
///
/// let err = query_scoped(
///     home,
///     &selector,
///     &scope,
///     "does it matter",
///     &embedder,
///     &descriptor,
///     // Die Provenienz bringt der Aufrufer mit -- sie aus dem Index
///     // abzuleiten hieße, ihn mit sich selbst zu vergleichen (K43).
///     &QueryProvenance { model: "test-model".to_owned(), chunker_version: 1 },
///     &EdgeIndex::default(),
///     CollapsePolicy::ByDigest,
///     10,
/// )
/// .unwrap_err();
/// assert!(matches!(err, QueryError::IndexNotVisible { .. }));
/// ```
#[allow(clippy::too_many_arguments)]
pub fn query_scoped(
    home: &Path,
    selector: &IndexSelector,
    scope: &ReadScope,
    question: &str,
    embedder: &dyn Embedder,
    descriptor: &EmbeddingDescriptor,
    provenance: &QueryProvenance,
    edges: &EdgeIndex,
    collapse_policy: CollapsePolicy,
    limit: usize,
) -> Result<Vec<Ranked>, QueryError> {
    let index = resolve_index(home, selector, scope)?;
    query(
        &index,
        question,
        embedder,
        descriptor,
        provenance,
        edges,
        collapse_policy,
        limit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_embed::DeterministicEmbedder;
    use harw_lens_index::{Bm25Index, FlatIndex};
    use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, EdgeKind, IndexManifest, Locality, Metric, SourceRef};
    use harw_types::ContentDigest;

    fn descriptor() -> EmbeddingDescriptor {
        EmbeddingDescriptor {
            document_prefix: "passage: ".to_owned(),
            query_prefix: "query: ".to_owned(),
            normalize: false,
        }
    }

    fn manifest() -> IndexManifest {
        IndexManifest {
            model: "test-model".to_owned(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    /// Die zu [`manifest`] passende Provenienz -- das, womit ein echter
    /// Aufrufer eingebettet hätte, wenn er mit demselben Modell und derselben
    /// Zerlegungsfassung wie der Index gearbeitet hat.
    fn provenance() -> QueryProvenance {
        QueryProvenance {
            model: manifest().model,
            chunker_version: manifest().chunker_version,
        }
    }

    fn chunk_at(text: &str, path: &str, start: usize, end: usize) -> Chunk {
        Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: path.to_owned(),
            },
            span: ByteSpan { start, end },
            text: text.to_owned(),
        }
    }

    /// Baut einen `FlatIndex` aus roh vorgegebenen Chunks, mit Embeddings, die
    /// über `prepare_document` konsistent zum Query-Pfad entstehen.
    fn build_flat_index(embedder: &DeterministicEmbedder, chunks: &[Chunk]) -> FlatIndex {
        let entries: Vec<(Chunk, Vec<f32>)> = chunks
            .iter()
            .map(|chunk| {
                let prepared = harw_lens_embed::prepare_document(&descriptor(), &chunk.text);
                let embedding = embedder
                    .embed(&[prepared])
                    .expect("deterministic embedder never fails")
                    .pop()
                    .expect("one vector");
                (chunk.clone(), embedding)
            })
            .collect();
        FlatIndex::build(manifest(), entries)
    }

    #[test]
    fn test_query_finds_matching_chunk_and_carries_correct_source_ref() {
        let embedder = DeterministicEmbedder::new(8);
        let chunk = chunk_at("hallo welt", "a.txt", 0, 10);
        let index = build_flat_index(&embedder, std::slice::from_ref(&chunk));

        let hits = query(
            &index,
            "hallo welt",
            &embedder,
            &descriptor(),
            &provenance(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("query succeeds");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.source, chunk.source);
    }

    #[test]
    fn test_query_applies_query_prefix_never_document_prefix() {
        // A recording embedder proves *which* text reached the embedder,
        // rather than only whether retrieval happened to work.
        struct RecordingEmbedder {
            inner: DeterministicEmbedder,
            seen: std::sync::Mutex<Vec<String>>,
        }
        impl Embedder for RecordingEmbedder {
            fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, harw_lens_embed::EmbedError> {
                self.seen.lock().expect("lock poisoned").extend(texts.iter().cloned());
                self.inner.embed(texts)
            }
            fn dimensions(&self) -> usize {
                self.inner.dimensions()
            }
        }

        let recorder = RecordingEmbedder {
            inner: DeterministicEmbedder::new(8),
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let chunk = chunk_at("hallo welt", "a.txt", 0, 10);
        let index = build_flat_index(&recorder.inner, std::slice::from_ref(&chunk));

        query(
            &index,
            "hallo welt",
            &recorder,
            &descriptor(),
            &provenance(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("query succeeds");

        let seen = recorder.seen.lock().expect("lock poisoned").clone();
        assert_eq!(seen, vec!["query: hallo welt".to_owned()]);
    }

    #[test]
    fn test_query_leaves_query_text_field_unprefixed_for_lexical_search() {
        // `Bm25Index` tokenizes `Query::text` literally. If the embedding
        // query-prefix ("query: ") ever leaked into that field, the decoy
        // chunk below (which literally contains the word "query") would
        // wrongly gain a nonzero BM25 score and appear in the results too --
        // this test would then fail with `hits.len() == 2`.
        let embedder = DeterministicEmbedder::new(4);
        let manifest = IndexManifest {
            model: String::new(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        };
        let matching = chunk_at("welt example", "a.txt", 0, 12);
        let decoy = chunk_at("query unrelated text", "b.txt", 0, 20);
        let index = Bm25Index::build(manifest, vec![matching.clone(), decoy.clone()]);
        let matching_provenance = QueryProvenance {
            model: String::new(),
            chunker_version: 1,
        };

        let hits = query(
            &index,
            "welt",
            &embedder,
            &descriptor(),
            &matching_provenance,
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("query succeeds");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.source, matching.source);
    }

    #[test]
    fn test_query_is_deterministic_across_repeated_calls() {
        let embedder = DeterministicEmbedder::new(8);
        let chunks = vec![
            chunk_at("first entry", "a.txt", 0, 11),
            chunk_at("second entry", "b.txt", 0, 12),
            chunk_at("third entry", "c.txt", 0, 11),
        ];
        let index = build_flat_index(&embedder, &chunks);

        let first = query(
            &index,
            "entry",
            &embedder,
            &descriptor(),
            &provenance(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("first query");
        let second = query(
            &index,
            "entry",
            &embedder,
            &descriptor(),
            &provenance(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("second query");

        let first_order: Vec<_> = first.iter().map(|r| r.chunk.digest).collect();
        let second_order: Vec<_> = second.iter().map(|r| r.chunk.digest).collect();
        assert_eq!(first_order, second_order);
    }

    #[test]
    fn test_query_collapse_fixture_superseded_by_keeps_only_the_newer_chunk() {
        let embedder = DeterministicEmbedder::new(8);
        let older = chunk_at("draft v1", "notes.md", 0, 8);
        let newer = chunk_at("draft v2", "notes.md", 0, 8);
        let index = build_flat_index(&embedder, &[older.clone(), newer.clone()]);

        let edges = EdgeIndex::from_edges([(older.digest, newer.digest)], std::iter::empty());

        let hits = query(
            &index,
            "draft",
            &embedder,
            &descriptor(),
            &provenance(),
            &edges,
            CollapsePolicy::BySourceAndSpan,
            10,
        )
        .expect("query succeeds");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.digest, newer.digest);
    }

    #[test]
    fn test_query_collapse_fixture_contradicts_keeps_both_chunks() {
        let embedder = DeterministicEmbedder::new(8);
        let claim_a = chunk_at("the build passes", "status.md", 0, 17);
        let claim_b = chunk_at("the build fails", "status.md", 0, 17);
        let index = build_flat_index(&embedder, &[claim_a.clone(), claim_b.clone()]);

        let edges =
            EdgeIndex::from_edges(std::iter::empty(), [(claim_a.digest, claim_b.digest)]);
        assert!(edges.has_edge(claim_a.digest, claim_b.digest, EdgeKind::Contradicts));

        let hits = query(
            &index,
            "the build",
            &embedder,
            &descriptor(),
            &provenance(),
            &edges,
            CollapsePolicy::BySourceAndSpan,
            10,
        )
        .expect("query succeeds");

        // The most important fixture: two contradicting claims never collapse
        // into one -- doing so would silently discard whichever one loses the
        // tiebreak, deleting exactly the information the edge exists to keep.
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn test_query_rejects_query_manifest_with_mismatched_model() {
        // This is the test that the whole node exists to make possible: the
        // first version of `query` built its query manifest from
        // `index.manifest().clone()`, so `compatible_with` could never see a
        // mismatch through this entry point. `provenance` is now an
        // independent argument, so a caller who embedded with a different
        // model than the index was built with is rejected here -- not
        // answered with lookalike hits from the wrong vector space.
        let embedder = DeterministicEmbedder::new(8);
        let chunk = chunk_at("hallo welt", "a.txt", 0, 10);
        let index = build_flat_index(&embedder, std::slice::from_ref(&chunk));

        let mismatched_provenance = QueryProvenance {
            model: "model-b".to_owned(),
            chunker_version: manifest().chunker_version,
        };

        let err = query(
            &index,
            "hallo welt",
            &embedder,
            &descriptor(),
            &mismatched_provenance,
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect_err("a query embedded with a different model than the index must be rejected");

        assert!(matches!(err, QueryError::Index(_)));
    }

    #[test]
    fn test_query_accepts_query_manifest_with_matching_provenance() {
        // The counter test to the one above: without it, the mismatch test
        // would only show that *something* about the call fails, not that
        // the check is specific to a mismatched model.
        let embedder = DeterministicEmbedder::new(8);
        let chunk = chunk_at("hallo welt", "a.txt", 0, 10);
        let index = build_flat_index(&embedder, std::slice::from_ref(&chunk));

        let hits = query(
            &index,
            "hallo welt",
            &embedder,
            &descriptor(),
            &provenance(),
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect("provenance matching the index manifest must be accepted");

        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn test_query_rejects_query_manifest_with_mismatched_chunker_version() {
        // Same rejection, different cause: the chunk boundaries shifted
        // between chunker versions, not the vector space -- but a caller
        // still embedded against an assumption the index no longer holds,
        // and the check must fire just the same.
        let embedder = DeterministicEmbedder::new(8);
        let chunk = chunk_at("hallo welt", "a.txt", 0, 10);
        let index = build_flat_index(&embedder, std::slice::from_ref(&chunk));

        let mismatched_provenance = QueryProvenance {
            model: manifest().model,
            chunker_version: manifest().chunker_version + 1,
        };

        let err = query(
            &index,
            "hallo welt",
            &embedder,
            &descriptor(),
            &mismatched_provenance,
            &EdgeIndex::default(),
            CollapsePolicy::ByDigest,
            10,
        )
        .expect_err("a query embedded against a different chunker version must be rejected");

        assert!(matches!(err, QueryError::Index(_)));
    }
}

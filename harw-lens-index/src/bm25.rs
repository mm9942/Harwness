//! Lexikalische BM25-Suche.
//!
//! # Verantwortungsbereich
//! Besitzt [`Bm25Index`]: eine reine Textsuche über die `text`-Felder aller
//! gespeicherten [`Chunk`]s, ohne Embedding.
//!
//! # BM25-Formel jetzt in `harw-lens-rank` (Knoten W10-L1, F-206)
//! Vor diesem Knoten implementierte dieses Modul die BM25-Formel
//! (Termfrequenz-Sättigung, IDF mit `+1`-Glättung, `avg_dl`-Fallback) selbst
//! — wörtlich identisch zu `harw-knowledge`s `KeywordRanker`
//! (`harw-knowledge/src/memory/recall.rs`), dokumentiert als bewusst
//! geduldete Doppelung (Befund F-206 im Register). Der Knoten **W9-C4** hat
//! diese Formel als reine Funktion [`harw_lens_rank::bm25_scores`] extrahiert;
//! dieser Knoten (**W10-L1**) stellt [`Bm25Index::search`] darauf um und
//! löscht die eigene Kopie der Formel. Was hierbleibt, ist ausschließlich
//! Lens-eigen: die [`Chunk`]/[`IndexManifest`]-Anbindung, das
//! Persistenzformat ([`Bm25IndexFile`]) und die Tokenisierung
//! ([`tokenize`], siehe unten) — `harw-lens-rank` kennt weder `Chunk` noch
//! `IndexManifest` und bleibt entsprechend schlank. Die Tokenisierung bleibt
//! bewusst hier (nicht auch nach `harw-lens-rank` gezogen): sie ist
//! Lens-spezifisch (`chunk.text`), während `harw-knowledge`s Tokenisierung
//! auf eigenen Texttypen arbeitet — nur die BM25-**Formel**, nicht die
//! Tokenisierung, war die 1:1-Kopie, die F-206 meinte. Die Übernahme durch
//! `harw-knowledge` selbst ist ausdrücklich **nicht** Teil dieses Knotens
//! (das Crate ist für W9-K2 gesperrt) und bleibt Folgearbeit.
//!
//! # Modell vs. Chunker-Version
//! Ein `Bm25Index` hat kein Modell — Text braucht kein Embedding-Modell, um
//! durchsucht zu werden. [`Bm25Index::search`] prüft deshalb **nur**
//! `chunker_version` gegen die Abfrage, nicht `model`: ein `model`-Feld
//! existiert im Manifest zwar (jedes [`harw_lens_types::IndexManifest`] trägt
//! eines), ist für diesen Indextyp aber bedeutungslos und wird ignoriert.
//! Das ist der Grund, warum dieses Modul nicht
//! [`harw_lens_types::IndexManifest::compatible_with`] verwendet wie
//! [`crate::FlatIndex`] es tut — `compatible_with` prüft immer beide Felder
//! und würde eine für BM25 irrelevante `model`-Abweichung fälschlich
//! ablehnen.
//!
//! # Nebenläufigkeit
//! [`Bm25Index`] ist reine Daten (keine interne Veränderlichkeit) und daher
//! `Send + Sync`; [`Bm25Index::search`] nimmt `&self`.
//!
//! # Fehler
//! [`crate::IndexError::ManifestMismatch`] (Feld `"chunker_version"`) vor
//! jeder Berechnung bei abweichender Chunker-Version.
//! [`crate::IndexError::MissingText`], wenn die Abfrage keinen Text trägt.
//! [`crate::IndexError::Store`] und [`crate::IndexError::Serde`] aus
//! [`Bm25Index::save`]/[`Bm25Index::load`].
//!
//! # Examples
//! ```rust
//! use harw_lens_index::{Bm25Index, Query, VectorIndex};
//! use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, IndexManifest, Locality, Metric, SourceRef};
//! use harw_types::ContentDigest;
//!
//! let manifest = IndexManifest {
//!     model: String::new(),
//!     locality: Locality::Local,
//!     chunker_version: 1,
//!     visibility: "workspace".to_owned(),
//!     metric: Metric::Cosine,
//!     source_set_digest: ContentDigest::of(b"sources"),
//! };
//! let chunk = Chunk {
//!     digest: ChunkDigest(ContentDigest::of(b"hallo welt")),
//!     source: SourceRef::File { path: "a.txt".to_owned() },
//!     span: ByteSpan::new(0, 10).expect("valid span"),
//!     text: "hallo welt".to_owned(),
//! };
//! let index = Bm25Index::build(manifest.clone(), vec![chunk]);
//! let query = Query { embedding: None, text: Some("welt".to_owned()), manifest };
//! let hits = index.search(&query, 10).expect("compatible chunker_version");
//! assert_eq!(hits.len(), 1);
//! ```

use serde::{Deserialize, Serialize};

use harw_lens_rank::{Bm25Params, bm25_scores};
use harw_lens_store::LensStore;
use harw_lens_types::{Chunk, IndexManifest, Ranked};

use crate::error::IndexError;
use crate::query::Query;
use crate::sort::sort_ranked_desc;
use crate::vector_index::VectorIndex;

/// Ein Dokument im BM25-Korpus: der Chunk plus seine vorab tokenisierte
/// Fassung.
///
/// # Description
/// Die Tokenisierung wird einmalig in [`Bm25Index::build`] durchgeführt,
/// nicht bei jeder Suche neu — der Korpus ändert sich nach dem Bauen nicht.
/// Die eigentliche BM25-Bewertung (Termfrequenz, Dokumentfrequenz,
/// Längennormalisierung) übernimmt [`harw_lens_rank::bm25_scores`] bei jeder
/// [`Bm25Index::search`] aus diesen Tokens neu (siehe Modul-Dokumentation).
#[derive(Debug, Clone, PartialEq)]
struct Bm25Document {
    /// Der zugrundeliegende Chunk.
    chunk: Chunk,
    /// Die tokenisierte Fassung von `chunk.text` (siehe [`tokenize`]).
    tokens: Vec<String>,
}

/// Das Persistenzformat eines [`Bm25Index`]: eine Hülle aus dem Manifest und
/// den rohen Chunks (nicht der abgeleiteten Tokenisierung), gemeinsam als
/// ein JSON-Dokument serialisiert.
///
/// # Description
/// Es werden bewusst nur die [`Chunk`]s gespeichert, nicht
/// `Bm25Document::tokens`: diese sind aus `chunk.text` deterministisch
/// ableitbar, und [`Bm25Index::load`] baut sie über [`Bm25Index::build`] neu
/// auf, statt eine zweite, potenziell inkonsistente Quelle der Wahrheit auf
/// der Platte zu halten. Das Manifest liegt hier zusätzlich zu dem
/// Manifest, das [`harw_lens_store::LensStore::put_index`] separat unter
/// `index/<name>/manifest.json` ablegt — Begründung siehe `FlatIndexFile`
/// in `flat.rs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Bm25IndexFile {
    /// Das Manifest, wie es zum Zeitpunkt von `save` galt.
    manifest: IndexManifest,
    /// Alle gespeicherten Chunks.
    chunks: Vec<Chunk>,
}

/// Lexikalischer BM25-Index über die `text`-Felder gespeicherter Chunks.
///
/// # Description
/// Kein Embedding, kein Modell — siehe Modul-Dokumentation zur
/// Modell/Chunker-Version-Unterscheidung. Die BM25-Formel selbst liegt in
/// [`harw_lens_rank::bm25_scores`] (Knoten W10-L1, F-206); dieser Typ hält
/// nur die tokenisierten Chunks und ruft sie bei jeder Suche auf.
#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Index {
    manifest: IndexManifest,
    documents: Vec<Bm25Document>,
}

impl Bm25Index {
    /// Baut einen `Bm25Index` aus einem Manifest und den zu indizierenden
    /// Chunks.
    ///
    /// # Description
    /// Tokenisiert jeden `chunk.text` einmalig; die eigentliche
    /// BM25-Statistik (Termfrequenzen, Dokumentfrequenz, Korpus-
    /// Durchschnittslänge) berechnet [`harw_lens_rank::bm25_scores`] erst
    /// bei jeder [`Bm25Index::search`] aus diesen Tokens — sie ist aus dem
    /// Ergebnis deterministisch ableitbar, wird deshalb hier nicht
    /// vorgehalten.
    ///
    /// # Arguments
    /// - `manifest` (`IndexManifest`): das Manifest dieses Index. `model`
    ///   wird von diesem Indextyp nie geprüft (siehe Modul-Dokumentation).
    /// - `chunks` (`Vec<Chunk>`): die zu indizierenden Chunks.
    ///
    /// # Returns
    /// Ein neuer `Bm25Index`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_index::Bm25Index;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    ///
    /// let manifest = IndexManifest {
    ///     model: String::new(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"s"),
    /// };
    /// let index = Bm25Index::build(manifest, Vec::new());
    /// assert_eq!(index.len(), 0);
    /// ```
    #[must_use]
    pub fn build(manifest: IndexManifest, chunks: Vec<Chunk>) -> Self {
        let documents: Vec<Bm25Document> = chunks
            .into_iter()
            .map(|chunk| {
                let tokens = tokenize(&chunk.text);
                Bm25Document { chunk, tokens }
            })
            .collect();

        Self {
            manifest,
            documents,
        }
    }

    /// Die Anzahl indizierter Dokumente.
    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Ob dieser Index keine Dokumente hält.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    /// Speichert diesen Index unter `name` in `store`.
    ///
    /// # Description
    /// Serialisiert Manifest und die rohen Chunks gemeinsam als ein
    /// [`Bm25IndexFile`]-JSON-Dokument. Siehe `flat.rs` für die Begründung
    /// der Manifest-Doppelung im Datenteil.
    ///
    /// # Arguments
    /// - `store` (`&LensStore`): der Zielspeicher.
    /// - `name` (`&str`): der Indexname, unter dem gespeichert wird.
    ///
    /// # Errors
    /// - [`IndexError::Serde`]: die Serialisierung des Datenteils schlägt
    ///   fehl.
    /// - [`IndexError::Store`]: `store.put_index` schlägt fehl.
    pub fn save(&self, store: &LensStore, name: &str) -> Result<(), IndexError> {
        let file = Bm25IndexFile {
            manifest: self.manifest.clone(),
            chunks: self.documents.iter().map(|doc| doc.chunk.clone()).collect(),
        };
        let data = serde_json::to_vec(&file)?;
        store.put_index(name, &self.manifest, &data)?;
        Ok(())
    }

    /// Lädt einen zuvor mit [`Bm25Index::save`] gespeicherten Index.
    ///
    /// # Description
    /// Liest Manifest und Datenteil getrennt aus `store`, vergleicht das im
    /// Datenteil eingebettete Manifest gegen das separat geladene (siehe
    /// `flat.rs`) und baut die Tokenisierung über [`Bm25Index::build`] aus
    /// den geladenen Chunks neu auf.
    ///
    /// # Arguments
    /// - `store` (`&LensStore`): der Quellspeicher.
    /// - `name` (`&str`): der Indexname, unter dem geladen wird.
    ///
    /// # Errors
    /// - [`IndexError::IndexNotFound`]: kein Manifest oder kein Datenteil
    ///   unter `name` vorhanden.
    /// - [`IndexError::PersistedManifestMismatch`]: das separat geladene
    ///   Manifest stimmt nicht mit dem im Datenteil eingebetteten überein.
    /// - [`IndexError::Serde`]: der Datenteil ist kein gültiges
    ///   [`Bm25IndexFile`]-JSON.
    /// - [`IndexError::Store`]: ein Lesefehler in `store`.
    pub fn load(store: &LensStore, name: &str) -> Result<Self, IndexError> {
        let stored_manifest =
            store
                .get_index_manifest(name)?
                .ok_or_else(|| IndexError::IndexNotFound {
                    name: name.to_owned(),
                })?;
        let data = store
            .read_index_data(name)?
            .ok_or_else(|| IndexError::IndexNotFound {
                name: name.to_owned(),
            })?;
        let file: Bm25IndexFile = serde_json::from_slice(&data)?;
        if file.manifest != stored_manifest {
            return Err(IndexError::PersistedManifestMismatch {
                name: name.to_owned(),
            });
        }
        Ok(Self::build(stored_manifest, file.chunks))
    }
}

impl VectorIndex for Bm25Index {
    fn manifest(&self) -> &IndexManifest {
        &self.manifest
    }

    fn search(&self, query: &Query, limit: usize) -> Result<Vec<Ranked>, IndexError> {
        // Nur `chunker_version` zaehlt fuer BM25 - kein
        // `compatible_with`-Aufruf, siehe Modul-Dokumentation.
        if self.manifest.chunker_version != query.manifest.chunker_version {
            return Err(IndexError::ManifestMismatch {
                field: "chunker_version",
            });
        }

        let text = query.text.as_ref().ok_or(IndexError::MissingText)?;
        let query_terms = tokenize(text);

        let doc_tokens: Vec<&[String]> = self
            .documents
            .iter()
            .map(|doc| doc.tokens.as_slice())
            .collect();
        let scores = bm25_scores(&doc_tokens, &query_terms, Bm25Params::default());

        let mut scored: Vec<Ranked> = self
            .documents
            .iter()
            .zip(scores)
            .filter(|(_, score)| *score > 0.0)
            .map(|(doc, score)| Ranked {
                chunk: doc.chunk.clone(),
                score,
            })
            .collect();

        sort_ranked_desc(&mut scored);
        scored.truncate(limit);
        Ok(scored)
    }
}

/// Tokenisiert Text in kleingeschriebene alphanumerische Terme, getrennt an
/// jeder Nicht-Alphanumerik-Stelle. `is_alphanumeric` arbeitet mit Unicode,
/// nicht nur mit ASCII — ein Wort wie "café" bleibt also ein einziges
/// Token, nicht zwei.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_lens_types::{ByteSpan, ChunkDigest, Locality, Metric, SourceRef};
    use harw_types::ContentDigest;

    fn manifest_with(chunker_version: u32) -> IndexManifest {
        IndexManifest {
            model: String::new(),
            locality: Locality::Local,
            chunker_version,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    fn chunk(text: &str) -> TestResult<Chunk> {
        Ok(Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: "a.txt".to_owned(),
            },
            span: ByteSpan::new(0, text.len()).map_err(ctx("valid span"))?,
            text: text.to_owned(),
        })
    }

    #[test]
    fn test_search_doc_with_term_outranks_doc_without() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(
            manifest.clone(),
            vec![chunk("relevant term here")?, chunk("nothing matches")?],
        );
        let query = Query {
            embedding: None,
            text: Some("relevant".to_owned()),
            manifest,
        };
        let hits = index
            .search(&query, 10)
            .map_err(ctx("compatible manifest"))?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "relevant term here");
        Ok(())
    }

    #[test]
    fn test_search_rare_term_scores_higher_than_frequent_term() -> TestResult {
        let manifest = manifest_with(1);
        // "beta" appears in every document (df = 5); "alpha" appears only in
        // the first (df = 1). Every document has the same token length, so
        // the length-normalization term is identical for both queries.
        let chunks = vec![
            chunk("alpha beta")?,
            chunk("beta gamma")?,
            chunk("beta delta")?,
            chunk("beta epsilon")?,
            chunk("beta zeta")?,
        ];
        let index = Bm25Index::build(manifest.clone(), chunks);

        let rare_query = Query {
            embedding: None,
            text: Some("alpha".to_owned()),
            manifest: manifest.clone(),
        };
        let frequent_query = Query {
            embedding: None,
            text: Some("beta".to_owned()),
            manifest,
        };

        let rare_score = index
            .search(&rare_query, 10)
            .map_err(ctx("compatible manifest"))?
            .into_iter()
            .find(|hit| hit.chunk.text == "alpha beta")
            .ok_or(TestError::Missing("alpha matches the first document"))?
            .score;
        let frequent_score = index
            .search(&frequent_query, 10)
            .map_err(ctx("compatible manifest"))?
            .into_iter()
            .find(|hit| hit.chunk.text == "alpha beta")
            .ok_or(TestError::Missing("beta matches every document"))?
            .score;

        assert!(
            rare_score > frequent_score,
            "rare term score {rare_score} should exceed frequent term score {frequent_score}"
        );
        Ok(())
    }

    #[test]
    fn test_search_on_empty_index_returns_empty_list_not_error() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), Vec::new());
        let query = Query {
            embedding: None,
            text: Some("anything".to_owned()),
            manifest,
        };
        let hits = index
            .search(&query, 10)
            .map_err(ctx("empty index is not an error"))?;
        assert!(hits.is_empty());
        Ok(())
    }

    #[test]
    fn test_search_with_empty_query_text_returns_empty_list_not_error() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")?]);
        let query = Query {
            embedding: None,
            text: Some("   ".to_owned()),
            manifest,
        };
        let hits = index
            .search(&query, 10)
            .map_err(ctx("whitespace-only text tokenizes to an empty query"))?;
        assert!(hits.is_empty());
        Ok(())
    }

    #[test]
    fn test_search_rejects_chunker_version_mismatch_before_computing() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")?]);
        let mut query_manifest = manifest;
        query_manifest.chunker_version = 2;
        let query = Query {
            embedding: None,
            text: Some("match".to_owned()),
            manifest: query_manifest,
        };
        let Err(err) = index.search(&query, 10) else {
            return Err(TestError::Unexpected(
                "chunker_version mismatch is rejected".to_owned(),
            ));
        };
        assert!(matches!(
            err,
            IndexError::ManifestMismatch {
                field: "chunker_version"
            }
        ));
        Ok(())
    }

    #[test]
    fn test_search_ignores_model_mismatch_because_bm25_has_no_model() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")?]);
        let mut query_manifest = manifest;
        query_manifest.model = "some-other-model".to_owned();
        let query = Query {
            embedding: None,
            text: Some("match".to_owned()),
            manifest: query_manifest,
        };
        let hits = index
            .search(&query, 10)
            .map_err(ctx("model is irrelevant for Bm25Index"))?;
        assert_eq!(hits.len(), 1);
        Ok(())
    }

    #[test]
    fn test_search_without_text_returns_missing_text() -> TestResult {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")?]);
        let query = Query {
            embedding: None,
            text: None,
            manifest,
        };
        let Err(err) = index.search(&query, 10) else {
            return Err(TestError::Unexpected("missing text".to_owned()));
        };
        assert!(matches!(err, IndexError::MissingText));
        Ok(())
    }

    #[test]
    fn test_search_is_deterministic_on_repeated_calls_with_tied_scores() -> TestResult {
        let manifest = manifest_with(1);
        // Same multiset of terms in both documents => identical BM25 score
        // for the query term, but different digests (different text).
        let index = Bm25Index::build(manifest.clone(), vec![chunk("cat dog")?, chunk("dog cat")?]);
        let query = Query {
            embedding: None,
            text: Some("cat".to_owned()),
            manifest,
        };
        let first = index
            .search(&query, 10)
            .map_err(ctx("compatible manifest"))?;
        let second = index
            .search(&query, 10)
            .map_err(ctx("compatible manifest"))?;
        let first_order: Vec<_> = first.iter().map(|r| r.chunk.digest).collect();
        let second_order: Vec<_> = second.iter().map(|r| r.chunk.digest).collect();
        assert_eq!(first_order, second_order);
        assert_eq!(first.len(), 2);
        assert!((first[0].score - first[1].score).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn test_len_and_is_empty() -> TestResult {
        let manifest = manifest_with(1);
        let empty = Bm25Index::build(manifest.clone(), Vec::new());
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let one = Bm25Index::build(manifest, vec![chunk("x")?]);
        assert!(!one.is_empty());
        assert_eq!(one.len(), 1);
        Ok(())
    }

    #[test]
    fn test_manifest_returns_stored_manifest() {
        let manifest = manifest_with(7);
        let index = Bm25Index::build(manifest.clone(), Vec::new());
        assert_eq!(index.manifest(), &manifest);
    }

    #[test]
    fn test_tokenize_lowercases_and_splits_on_non_alphanumeric() {
        assert_eq!(
            tokenize("Hello, World!"),
            vec!["hello".to_owned(), "world".to_owned()]
        );
    }

    #[test]
    fn test_tokenize_keeps_unicode_words_intact() {
        // Regressionstest fuer F-206: der vorherige Modulkommentar behauptete
        // "ASCII-Wortgrenzen", obwohl `is_alphanumeric` Unicode-bewusst ist.
        assert_eq!(tokenize("café"), vec!["café".to_owned()]);
    }

    #[test]
    fn test_save_and_load_roundtrip_rebuilds_equivalent_index() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = LensStore::open(dir.path()).map_err(ctx("open store"))?;
        let manifest = manifest_with(1);
        let index = Bm25Index::build(
            manifest,
            vec![chunk("hallo welt")?, chunk("noch ein chunk")?],
        );
        index.save(&store, "roundtrip-bm25").map_err(ctx("save"))?;
        let loaded = Bm25Index::load(&store, "roundtrip-bm25").map_err(ctx("load"))?;
        assert_eq!(loaded, index);
        Ok(())
    }

    #[test]
    fn test_load_missing_index_returns_index_not_found() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("temp dir"))?;
        let store = LensStore::open(dir.path()).map_err(ctx("open store"))?;
        let Err(err) = Bm25Index::load(&store, "does-not-exist") else {
            return Err(TestError::Unexpected("missing index".to_owned()));
        };
        assert!(matches!(err, IndexError::IndexNotFound { .. }));
        Ok(())
    }
}

//! Lexikalische BM25-Suche.
//!
//! # Verantwortungsbereich
//! Besitzt [`Bm25Index`]: eine reine Textsuche über die `text`-Felder aller
//! gespeicherten [`Chunk`]s, ohne Embedding.
//!
//! # Bewusste Doppelung mit `harw-knowledge`
//! `harw-knowledge` (`harw-knowledge/src/memory/recall.rs`, `KeywordRanker`)
//! implementiert bereits ein echtes BM25 mit denselben Parametern
//! (`k1 = 1.2`, `b = 0.75`) für seinen eigenen Recall-Pfad. Dieses Modul
//! implementiert BM25 hier **zusätzlich**, für `harw-lens-index` — das ist
//! eine bewusste, **geduldete** Doppelung, kein Versehen. Geduldet, weil
//! `harw-knowledge` zum Zeitpunkt dieses Knotens (AW4-06) parallel von vier
//! anderen Knoten des Ausbauprogramms beschrieben wird; eine Zusammenlegung
//! der beiden Implementierungen mitten in diesem Programm wäre die
//! riskantere Wahl, nicht die sauberere. Die Zusammenlegung (etwa: BM25 in
//! eine gemeinsame Crate ausziehen, beide Konsumenten darauf umstellen) wird
//! bewusst zurückgestellt und nach Abschluss des Ausbauprogramms bewertet.
//! Diese Crate hängt **nicht** von `harw-knowledge` ab (siehe `Cargo.toml`);
//! die Parameter `k1`, `b`, die IDF-Formel mit `+1`-Glättung, der
//! `avg_dl`-Fallback auf `1.0` bei einem Korpusmittel von `0` und das
//! Verwerfen von Kandidaten mit Score `0` sind wörtlich aus
//! `harw-knowledge`s `KeywordRanker` übernommen, nicht neu erfunden.
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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use harw_lens_store::LensStore;
use harw_lens_types::{Chunk, IndexManifest, Ranked};

use crate::error::IndexError;
use crate::query::Query;
use crate::sort::sort_ranked_desc;
use crate::vector_index::VectorIndex;

/// BM25-Sättigungsparameter für die Termfrequenz. Wörtlich aus
/// `harw-knowledge`s `KeywordRanker` übernommen (siehe Modul-Dokumentation).
const BM25_K1: f64 = 1.2;
/// BM25-Längennormalisierungsparameter. Wörtlich aus `harw-knowledge`s
/// `KeywordRanker` übernommen (siehe Modul-Dokumentation).
const BM25_B: f64 = 0.75;

/// Ein Dokument im BM25-Korpus: der Chunk plus seine vorab berechnete
/// Termstatistik.
///
/// # Description
/// Termfrequenzen und Länge werden einmalig in [`Bm25Index::build`]
/// berechnet, nicht bei jeder Suche neu — der Korpus ändert sich nach dem
/// Bauen nicht.
#[derive(Debug, Clone, PartialEq)]
struct Bm25Document {
    /// Der zugrundeliegende Chunk.
    chunk: Chunk,
    /// Termfrequenzen über den tokenisierten `chunk.text`.
    term_frequencies: HashMap<String, u32>,
    /// Die Länge des Dokuments in Tokens (Summe aller Termfrequenzen).
    length: f64,
}

/// Das Persistenzformat eines [`Bm25Index`]: eine Hülle aus dem Manifest und
/// den rohen Chunks (nicht der abgeleiteten Termstatistik), gemeinsam als
/// ein JSON-Dokument serialisiert.
///
/// # Description
/// Es werden bewusst nur die [`Chunk`]s gespeichert, nicht
/// `Bm25Document::term_frequencies`/`length`: diese sind aus `chunk.text`
/// deterministisch ableitbar, und [`Bm25Index::load`] baut sie über
/// [`Bm25Index::build`] neu auf, statt eine zweite, potenziell
/// inkonsistente Quelle der Wahrheit auf der Platte zu halten. Das Manifest
/// liegt hier zusätzlich zu dem Manifest, das
/// [`harw_lens_store::LensStore::put_index`] separat unter
/// `index/<name>/manifest.json` ablegt — Begründung siehe
/// `FlatIndexFile` in `flat.rs`.
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
/// Kein Embedding, kein Modell — siehe Modul-Dokumentation zur bewussten
/// Doppelung mit `harw-knowledge` und zur Modell/Chunker-Version-Unterscheidung.
#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Index {
    manifest: IndexManifest,
    documents: Vec<Bm25Document>,
    /// Durchschnittliche Dokumentlänge in Tokens über den ganzen Korpus.
    /// `1.0`, wenn der Korpus leer ist oder das Mittel `0` ergibt (siehe
    /// `harw-knowledge`s `KeywordRanker`).
    avg_doc_length: f64,
    /// Anzahl Dokumente, die jeden Term mindestens einmal enthalten.
    document_frequency: HashMap<String, usize>,
}

impl Bm25Index {
    /// Baut einen `Bm25Index` aus einem Manifest und den zu indizierenden
    /// Chunks.
    ///
    /// # Description
    /// Tokenisiert jeden `chunk.text` einmalig und berechnet Termfrequenzen,
    /// Dokumentlänge, die Korpus-Durchschnittslänge und die
    /// Dokumentfrequenz jedes Terms. Diese Werte sind aus dem Ergebnis
    /// deterministisch ableitbar; [`Bm25Index::load`] ruft diese Methode
    /// erneut auf, statt sie zu persistieren.
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
                let terms = tokenize(&chunk.text);
                let term_frequencies = term_frequencies(&terms);
                let length = terms.len() as f64;
                Bm25Document {
                    chunk,
                    term_frequencies,
                    length,
                }
            })
            .collect();

        let avg_doc_length = if documents.is_empty() {
            1.0
        } else {
            let total: f64 = documents.iter().map(|doc| doc.length).sum();
            let mean = total / documents.len() as f64;
            if mean > 0.0 { mean } else { 1.0 }
        };

        let mut document_frequency: HashMap<String, usize> = HashMap::new();
        for doc in &documents {
            for term in doc.term_frequencies.keys() {
                *document_frequency.entry(term.clone()).or_insert(0) += 1;
            }
        }

        Self {
            manifest,
            documents,
            avg_doc_length,
            document_frequency,
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
    /// `flat.rs`) und baut die Termstatistik über [`Bm25Index::build`] aus
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

        let mut scored: Vec<Ranked> = Vec::new();
        if !query_terms.is_empty() {
            let doc_count = self.documents.len() as f64;
            for doc in &self.documents {
                let score: f64 = query_terms
                    .iter()
                    .map(|term| self.term_score(term, doc, doc_count))
                    .sum();
                if score > 0.0 {
                    scored.push(Ranked {
                        chunk: doc.chunk.clone(),
                        score: score as f32,
                    });
                }
            }
        }

        sort_ranked_desc(&mut scored);
        scored.truncate(limit);
        Ok(scored)
    }
}

impl Bm25Index {
    /// Der BM25-Beitrag eines einzelnen Query-Terms zum Score von `doc`.
    ///
    /// # Description
    /// Identische Formel wie `harw-knowledge`s `KeywordRanker` (siehe
    /// Modul-Dokumentation): IDF mit `+1`-Glättung, multipliziert mit der
    /// termfrequenz-gesättigten Gewichtung. `0.0`, wenn `term` in `doc` nicht
    /// vorkommt.
    fn term_score(&self, term: &str, doc: &Bm25Document, doc_count: f64) -> f64 {
        let f = f64::from(doc.term_frequencies.get(term).copied().unwrap_or(0));
        if f == 0.0 {
            return 0.0;
        }
        let df = self.document_frequency.get(term).copied().unwrap_or(0) as f64;
        let idf = (((doc_count - df + 0.5) / (df + 0.5)) + 1.0).ln();
        let denom = f + BM25_K1 * (1.0 - BM25_B + BM25_B * doc.length / self.avg_doc_length);
        idf * (f * (BM25_K1 + 1.0)) / denom
    }
}

/// Tokenisiert Text in kleingeschriebene alphanumerische Terme
/// (ASCII-Wortgrenzen). Identisch zu `harw-knowledge`s `tokenize` (siehe
/// Modul-Dokumentation) — absichtlich hier erneut geschrieben, nicht
/// importiert, weil diese Crate nicht von `harw-knowledge` abhängt.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Zählt Vorkommen jedes Terms.
fn term_frequencies(terms: &[String]) -> HashMap<String, u32> {
    let mut map: HashMap<String, u32> = HashMap::new();
    for term in terms {
        *map.entry(term.clone()).or_insert(0) += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn chunk(text: &str) -> Chunk {
        Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: "a.txt".to_owned(),
            },
            span: ByteSpan::new(0, text.len()).expect("valid span"),
            text: text.to_owned(),
        }
    }

    #[test]
    fn test_search_doc_with_term_outranks_doc_without() {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(
            manifest.clone(),
            vec![chunk("relevant term here"), chunk("nothing matches")],
        );
        let query = Query {
            embedding: None,
            text: Some("relevant".to_owned()),
            manifest,
        };
        let hits = index.search(&query, 10).expect("compatible manifest");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.text, "relevant term here");
    }

    #[test]
    fn test_search_rare_term_scores_higher_than_frequent_term() {
        let manifest = manifest_with(1);
        // "beta" appears in every document (df = 5); "alpha" appears only in
        // the first (df = 1). Every document has the same token length, so
        // the length-normalization term is identical for both queries.
        let chunks = vec![
            chunk("alpha beta"),
            chunk("beta gamma"),
            chunk("beta delta"),
            chunk("beta epsilon"),
            chunk("beta zeta"),
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
            .expect("compatible manifest")
            .into_iter()
            .find(|hit| hit.chunk.text == "alpha beta")
            .expect("alpha matches the first document")
            .score;
        let frequent_score = index
            .search(&frequent_query, 10)
            .expect("compatible manifest")
            .into_iter()
            .find(|hit| hit.chunk.text == "alpha beta")
            .expect("beta matches every document")
            .score;

        assert!(
            rare_score > frequent_score,
            "rare term score {rare_score} should exceed frequent term score {frequent_score}"
        );
    }

    #[test]
    fn test_search_on_empty_index_returns_empty_list_not_error() {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), Vec::new());
        let query = Query {
            embedding: None,
            text: Some("anything".to_owned()),
            manifest,
        };
        let hits = index.search(&query, 10).expect("empty index is not an error");
        assert!(hits.is_empty());
    }

    #[test]
    fn test_search_rejects_chunker_version_mismatch_before_computing() {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")]);
        let mut query_manifest = manifest;
        query_manifest.chunker_version = 2;
        let query = Query {
            embedding: None,
            text: Some("match".to_owned()),
            manifest: query_manifest,
        };
        let err = index
            .search(&query, 10)
            .expect_err("chunker_version mismatch is rejected");
        assert!(matches!(
            err,
            IndexError::ManifestMismatch {
                field: "chunker_version"
            }
        ));
    }

    #[test]
    fn test_search_ignores_model_mismatch_because_bm25_has_no_model() {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")]);
        let mut query_manifest = manifest;
        query_manifest.model = "some-other-model".to_owned();
        let query = Query {
            embedding: None,
            text: Some("match".to_owned()),
            manifest: query_manifest,
        };
        let hits = index
            .search(&query, 10)
            .expect("model is irrelevant for Bm25Index");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn test_search_without_text_returns_missing_text() {
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest.clone(), vec![chunk("match term")]);
        let query = Query {
            embedding: None,
            text: None,
            manifest,
        };
        let err = index.search(&query, 10).expect_err("missing text");
        assert!(matches!(err, IndexError::MissingText));
    }

    #[test]
    fn test_search_is_deterministic_on_repeated_calls_with_tied_scores() {
        let manifest = manifest_with(1);
        // Same multiset of terms in both documents => identical BM25 score
        // for the query term, but different digests (different text).
        let index = Bm25Index::build(manifest.clone(), vec![chunk("cat dog"), chunk("dog cat")]);
        let query = Query {
            embedding: None,
            text: Some("cat".to_owned()),
            manifest,
        };
        let first = index.search(&query, 10).expect("compatible manifest");
        let second = index.search(&query, 10).expect("compatible manifest");
        let first_order: Vec<_> = first.iter().map(|r| r.chunk.digest).collect();
        let second_order: Vec<_> = second.iter().map(|r| r.chunk.digest).collect();
        assert_eq!(first_order, second_order);
        assert_eq!(first.len(), 2);
        assert!((first[0].score - first[1].score).abs() < 1e-9);
    }

    #[test]
    fn test_len_and_is_empty() {
        let manifest = manifest_with(1);
        let empty = Bm25Index::build(manifest.clone(), Vec::new());
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let one = Bm25Index::build(manifest, vec![chunk("x")]);
        assert!(!one.is_empty());
        assert_eq!(one.len(), 1);
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
    fn test_save_and_load_roundtrip_rebuilds_equivalent_index() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LensStore::open(dir.path()).expect("open store");
        let manifest = manifest_with(1);
        let index = Bm25Index::build(manifest, vec![chunk("hallo welt"), chunk("noch ein chunk")]);
        index.save(&store, "roundtrip-bm25").expect("save");
        let loaded = Bm25Index::load(&store, "roundtrip-bm25").expect("load");
        assert_eq!(loaded, index);
    }

    #[test]
    fn test_load_missing_index_returns_index_not_found() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LensStore::open(dir.path()).expect("open store");
        let err = Bm25Index::load(&store, "does-not-exist").expect_err("missing index");
        assert!(matches!(err, IndexError::IndexNotFound { .. }));
    }
}

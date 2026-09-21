//! Exakter Referenzindex ohne Näherung.
//!
//! # Verantwortungsbereich
//! Besitzt [`FlatIndex`]: hält jeden ([`Chunk`], Embedding)-Eintrag im
//! Speicher und durchsucht ihn bei jeder Anfrage vollständig — keine
//! beschleunigende Datenstruktur, kein Abbruch vor dem letzten Kandidaten.
//! Das ist Absicht, nicht ein fehlendes Feature: `FlatIndex` ist die
//! **Referenz**, gegen die jede spätere Näherung (HNSW, IVF, ...) gemessen
//! wird. Ohne einen exakten Index lässt sich von einem ungefähren nicht
//! sagen, wie ungefähr er tatsächlich ist — recall@k eines ANN-Index ist nur
//! relativ zur exakten Trefferliste definierbar, die `FlatIndex` liefert.
//! Diese Stufe des Ausbauprogramms baut deshalb bewusst keine
//! ANN-Bibliothek ein (siehe `Cargo.toml` dieser Crate): eine Schleife über
//! alle Vektoren genügt für den jetzigen Umfang.
//!
//! # Nebenläufigkeit
//! [`FlatIndex`] ist reine Daten (`Vec`, keine interne Veränderlichkeit) und
//! daher `Send + Sync`; [`FlatIndex::search`] nimmt `&self` und ist sicher
//! aus mehreren Threads gleichzeitig aufrufbar.
//!
//! # Fehler
//! [`crate::IndexError::ManifestMismatch`] vor jeder Berechnung, wenn
//! `query.manifest` in `model` oder `chunker_version` vom Manifest dieses
//! Index abweicht (geprüft über
//! [`harw_lens_types::IndexManifest::compatible_with`], nicht
//! zweitgeschrieben). [`crate::IndexError::MissingEmbedding`], wenn die
//! Abfrage kein Embedding trägt. [`crate::IndexError::EmbeddingDimensionMismatch`]
//! (Knoten W10-L1), wenn ein eingefügtes oder abgefragtes Embedding von der
//! Dimension der übrigen Einträge dieses Index abweicht — geprüft in
//! [`FlatIndex::build`]/[`FlatIndex::load`] (Einfügen) und
//! [`FlatIndex::search`] (Abfrage), in beiden Fällen vor jeder
//! Ähnlichkeitsberechnung. [`crate::IndexError::Store`] und
//! [`crate::IndexError::Serde`] aus [`FlatIndex::save`]/[`FlatIndex::load`].
//!
//! # Top-k-Auswahl (W10-L1)
//! [`FlatIndex::search`] sammelt nicht mehr alle Kandidaten, sortiert sie
//! vollständig und schneidet dann auf `limit` zu (`O(n log n)`). Stattdessen
//! hält die interne `top_k_ranked`-Hilfe (`sort.rs`, `pub(crate)`) einen
//! Min-Heap fester Kapazität `limit` (`O(n log limit)`) — siehe dort für die
//! Begründung und die `NaN`-Sicherheit über `f32::total_cmp`.
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
//! let index = FlatIndex::build(manifest.clone(), vec![(chunk, vec![1.0, 0.0])])
//!     .expect("consistent embedding dimension");
//! let query = Query { embedding: Some(vec![1.0, 0.0]), text: None, manifest };
//! let hits = index.search(&query, 10).expect("compatible manifest");
//! assert_eq!(hits.len(), 1);
//! ```

use serde::{Deserialize, Serialize};

use harw_lens_store::LensStore;
use harw_lens_types::{Chunk, IndexManifest, Metric, Ranked};

use crate::error::IndexError;
use crate::query::Query;
use crate::sort::top_k_ranked;
use crate::vector_index::VectorIndex;

/// Ein gespeicherter Chunk samt seinem Embedding.
///
/// # Description
/// Das interne Speicherformat von [`FlatIndex`] und zugleich sein
/// Persistenzformat: dieselbe Struktur wird über [`FlatIndex::save`] als
/// JSON abgelegt und über [`FlatIndex::load`] wieder eingelesen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FlatEntry {
    /// Der gespeicherte Chunk.
    chunk: Chunk,
    /// Das zugehörige Embedding, in der Dimensionalität des Modells aus
    /// `manifest.model`.
    embedding: Vec<f32>,
}

/// Das Persistenzformat eines [`FlatIndex`]: eine Hülle aus dem Manifest und
/// den Einträgen, gemeinsam als ein JSON-Dokument serialisiert.
///
/// # Description
/// Das Manifest liegt hier **zusätzlich** zu dem Manifest, das
/// [`harw_lens_store::LensStore::put_index`] separat unter
/// `index/<name>/manifest.json` ablegt. Grund: der Store adressiert einen
/// Index über seinen Namen, nicht über einen Inhalts-Digest — anders als bei
/// Chunks prüft der Store selbst nicht, ob Manifest und Datenteil
/// zueinander passen. [`FlatIndex::load`] vergleicht deshalb das separat
/// geladene Manifest gegen dieses eingebettete und lehnt mit
/// [`crate::IndexError::PersistedManifestMismatch`] ab, wenn sie
/// auseinanderlaufen (etwa weil ein Schreibvorgang zwischen den beiden
/// Dateien unterbrochen wurde).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FlatIndexFile {
    /// Das Manifest, wie es zum Zeitpunkt von `save` galt.
    manifest: IndexManifest,
    /// Alle gespeicherten Einträge.
    entries: Vec<FlatEntry>,
}

/// Exakte Vektorsuche ohne Näherung; die Referenzimplementierung dieses
/// Crates.
///
/// # Description
/// Hält alle Einträge in einem `Vec` und bewertet bei jeder Suche jeden
/// Eintrag einzeln — bewusst ohne Indexstruktur, siehe Modul-Dokumentation.
/// `dimension` ist die Embedding-Länge, die [`FlatIndex::build`]/
/// [`FlatIndex::load`] beim Einfügen aus dem ersten Eintrag festgestellt und
/// gegen jeden weiteren geprüft haben; `None` heißt: der Index ist leer und
/// hat (noch) keine Dimension festgestellt (Knoten W10-L1).
#[derive(Debug, Clone, PartialEq)]
pub struct FlatIndex {
    manifest: IndexManifest,
    entries: Vec<FlatEntry>,
    dimension: Option<usize>,
}

/// Prüft, dass jedes Embedding in `entries` dieselbe Dimension hat.
///
/// # Description
/// Die Dimension des ersten Eintrags gilt als erwartete Dimension für alle
/// folgenden; ein leerer Korpus hat keine Dimension. Wird sowohl von
/// [`FlatIndex::build`] (neu eingefügte Rohdaten) als auch
/// [`FlatIndex::load`] (aus dem Store gelesene Daten) verwendet, damit beide
/// Einfügepfade dieselbe Prüfung durchlaufen.
///
/// # Errors
/// [`IndexError::EmbeddingDimensionMismatch`]: ein Eintrag nach dem ersten
/// hat eine abweichende Embedding-Länge.
fn dimension_of(entries: &[FlatEntry]) -> Result<Option<usize>, IndexError> {
    let mut dimension: Option<usize> = None;
    for entry in entries {
        match dimension {
            None => dimension = Some(entry.embedding.len()),
            Some(expected) if expected != entry.embedding.len() => {
                return Err(IndexError::EmbeddingDimensionMismatch {
                    expected,
                    actual: entry.embedding.len(),
                });
            }
            Some(_) => {}
        }
    }
    Ok(dimension)
}

impl FlatIndex {
    /// Baut einen `FlatIndex` aus einem Manifest und seinen Einträgen.
    ///
    /// # Description
    /// Übernimmt `entries`, nachdem [`dimension_of`] geprüft hat, dass alle
    /// Embeddings dieselbe Länge haben (Knoten W10-L1; vorher unbeprüft).
    /// Ob `manifest.metric` zu den Embeddings passt, bleibt weiterhin
    /// Verantwortung des Aufrufers (typischerweise `harw-lens-embed`,
    /// nachgelagert) — das ist keine Dimensionsfrage.
    ///
    /// # Arguments
    /// - `manifest` (`IndexManifest`): das Manifest dieses Index.
    /// - `entries` (`Vec<(Chunk, Vec<f32>)>`): die gespeicherten Chunks samt
    ///   ihrer Embeddings.
    ///
    /// # Returns
    /// Ein neuer `FlatIndex`.
    ///
    /// # Errors
    /// - [`IndexError::EmbeddingDimensionMismatch`]: nicht alle `entries`
    ///   haben dieselbe Embedding-Länge.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_index::FlatIndex;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    ///
    /// let manifest = IndexManifest {
    ///     model: "m".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"s"),
    /// };
    /// let index = FlatIndex::build(manifest, Vec::new()).expect("empty corpus has no dimension");
    /// assert_eq!(index.len(), 0);
    /// ```
    pub fn build(manifest: IndexManifest, entries: Vec<(Chunk, Vec<f32>)>) -> Result<Self, IndexError> {
        let entries: Vec<FlatEntry> = entries
            .into_iter()
            .map(|(chunk, embedding)| FlatEntry { chunk, embedding })
            .collect();
        let dimension = dimension_of(&entries)?;
        Ok(Self {
            manifest,
            entries,
            dimension,
        })
    }

    /// Die Anzahl gespeicherter Einträge.
    ///
    /// # Returns
    /// Die Anzahl der (Chunk, Embedding)-Paare in diesem Index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ob dieser Index keine Einträge hält.
    ///
    /// # Returns
    /// `true`, wenn [`FlatIndex::len`] `0` ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Die Embedding-Länge dieses Index, falls bekannt.
    ///
    /// # Description
    /// Von [`FlatIndex::build`]/[`FlatIndex::load`] aus dem ersten Eintrag
    /// festgestellt und gegen jeden weiteren geprüft (siehe
    /// [`dimension_of`]). Ein Konsument, der nur wissen will, wie viele
    /// Dimensionen dieser Index tatsächlich trägt (z. B. eine
    /// Status-/Introspektionsausgabe wie `harw lens status`), muss dafür
    /// nicht `manifest()` heranziehen — die Dimension ist kein
    /// [`harw_lens_types::IndexManifest`]-Feld, sondern eine Eigenschaft der
    /// tatsächlich gespeicherten Einträge.
    ///
    /// # Returns
    /// `Some(dimension)`, wenn dieser Index mindestens einen Eintrag hält;
    /// `None` bei einem leeren Index.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_index::FlatIndex;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    ///
    /// let manifest = IndexManifest {
    ///     model: "m".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"s"),
    /// };
    /// let index = FlatIndex::build(manifest, Vec::new()).expect("empty corpus has no dimension");
    /// assert_eq!(index.dimension(), None);
    /// ```
    #[must_use]
    pub fn dimension(&self) -> Option<usize> {
        self.dimension
    }

    /// Speichert diesen Index unter `name` in `store`.
    ///
    /// # Description
    /// Serialisiert Manifest und Einträge gemeinsam als ein
    /// [`FlatIndexFile`]-JSON-Dokument und übergibt es zusammen mit dem
    /// Manifest an [`harw_lens_store::LensStore::put_index`]. Siehe die
    /// Dokumentation von [`FlatIndexFile`] für den Grund der Doppelung des
    /// Manifests.
    ///
    /// # Arguments
    /// - `store` (`&LensStore`): der Zielspeicher.
    /// - `name` (`&str`): der Indexname, unter dem gespeichert wird.
    ///
    /// # Errors
    /// - [`IndexError::Serde`]: die Serialisierung des Datenteils schlägt
    ///   fehl.
    /// - [`IndexError::Store`]: `store.put_index` schlägt fehl (z. B.
    ///   [`harw_lens_store::LensStoreError::InvalidIndexName`] oder
    ///   [`harw_lens_store::LensStoreError::LockContended`]).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_lens_index::FlatIndex;
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    /// use std::path::Path;
    ///
    /// let manifest = IndexManifest {
    ///     model: "m".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"s"),
    /// };
    /// let index = FlatIndex::build(manifest, Vec::new())?;
    /// let store = LensStore::open(Path::new("/tmp/example-harw-home"))?;
    /// index.save(&store, "my-index")?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn save(&self, store: &LensStore, name: &str) -> Result<(), IndexError> {
        let file = FlatIndexFile {
            manifest: self.manifest.clone(),
            entries: self.entries.clone(),
        };
        let data = serde_json::to_vec(&file)?;
        store.put_index(name, &self.manifest, &data)?;
        Ok(())
    }

    /// Lädt einen zuvor mit [`FlatIndex::save`] gespeicherten Index.
    ///
    /// # Description
    /// Liest Manifest und Datenteil getrennt aus `store`, deserialisiert den
    /// Datenteil als [`FlatIndexFile`] und vergleicht das darin eingebettete
    /// Manifest gegen das separat geladene. Diese zusätzliche Prüfung kommt
    /// zu der Digest-Prüfung hinzu, die der Store bereits für Chunks
    /// durchführt — für benannte Indizes (adressiert über `name`, nicht über
    /// einen Inhalts-Digest) gibt es sie sonst nicht.
    ///
    /// # Arguments
    /// - `store` (`&LensStore`): der Quellspeicher.
    /// - `name` (`&str`): der Indexname, unter dem geladen wird.
    ///
    /// # Returns
    /// Der geladene `FlatIndex`.
    ///
    /// # Errors
    /// - [`IndexError::IndexNotFound`]: kein Manifest oder kein Datenteil
    ///   unter `name` vorhanden.
    /// - [`IndexError::PersistedManifestMismatch`]: das separat geladene
    ///   Manifest stimmt nicht mit dem im Datenteil eingebetteten überein.
    /// - [`IndexError::Serde`]: der Datenteil ist kein gültiges
    ///   [`FlatIndexFile`]-JSON.
    /// - [`IndexError::Store`]: ein Lesefehler in `store`.
    /// - [`IndexError::EmbeddingDimensionMismatch`]: die geladenen Einträge
    ///   haben unterschiedliche Embedding-Längen (Knoten W10-L1) — kann nur
    ///   auftreten, wenn die gespeicherten Daten außerhalb von
    ///   [`FlatIndex::build`]s Prüfung entstanden sind.
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
        let file: FlatIndexFile = serde_json::from_slice(&data)?;
        if file.manifest != stored_manifest {
            return Err(IndexError::PersistedManifestMismatch {
                name: name.to_owned(),
            });
        }
        let dimension = dimension_of(&file.entries)?;
        Ok(Self {
            manifest: stored_manifest,
            entries: file.entries,
            dimension,
        })
    }
}

impl VectorIndex for FlatIndex {
    fn manifest(&self) -> &IndexManifest {
        &self.manifest
    }

    fn search(&self, query: &Query, limit: usize) -> Result<Vec<Ranked>, IndexError> {
        self.manifest
            .compatible_with(&query.manifest)
            .map_err(IndexError::from_manifest_check)?;

        let query_embedding = query.embedding.as_ref().ok_or(IndexError::MissingEmbedding)?;

        if let Some(expected) = self.dimension {
            if query_embedding.len() != expected {
                return Err(IndexError::EmbeddingDimensionMismatch {
                    expected,
                    actual: query_embedding.len(),
                });
            }
        }

        let scored = self.entries.iter().map(|entry| Ranked {
            chunk: entry.chunk.clone(),
            score: similarity(self.manifest.metric, query_embedding, &entry.embedding),
        });

        Ok(top_k_ranked(scored, limit))
    }
}

/// Berechnet den Rangwert zwischen `query` und `candidate` gemäß `metric`.
///
/// # Description
/// Höher heißt relevanter, für alle drei Metriken einheitlich: für
/// [`Metric::Euclidean`] ist der Rangwert die **negierte** Distanz, damit
/// der nächstgelegene Kandidat weiterhin den höchsten Wert erhält. Vektoren
/// unterschiedlicher Länge werden über ihr gemeinsames Präfix verglichen
/// (`Iterator::zip`); Normen für [`Metric::Cosine`] werden dennoch über die
/// jeweils volle eigene Länge jedes Vektors gebildet, was für zwei getrennt
/// normierte Vektoren die korrekte Definition bleibt.
fn similarity(metric: Metric, query: &[f32], candidate: &[f32]) -> f32 {
    match metric {
        Metric::Cosine => cosine_similarity(query, candidate),
        Metric::DotProduct => dot_product(query, candidate),
        Metric::Euclidean => -euclidean_distance(query, candidate),
    }
}

/// Das Skalarprodukt zweier Vektoren, über ihr gemeinsames Präfix.
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Die euklidische Norm eines Vektors.
fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Kosinus-Ähnlichkeit zweier Vektoren. `0.0`, wenn einer der beiden die
/// Norm `0` hat (undefiniert, statt einer Division durch `0` und `NaN`).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let denom = norm(a) * norm(b);
    if denom == 0.0 {
        0.0
    } else {
        dot_product(a, b) / denom
    }
}

/// Die euklidische Distanz zweier Vektoren, über ihr gemeinsames Präfix.
fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, ChunkDigest, LensTypesError, Locality, SourceRef};
    use harw_types::ContentDigest;

    fn manifest_with(model: &str, chunker_version: u32, metric: Metric) -> IndexManifest {
        IndexManifest {
            model: model.to_owned(),
            locality: Locality::Local,
            chunker_version,
            visibility: "workspace".to_owned(),
            metric,
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
    fn test_search_orders_hits_by_similarity_descending() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(
            manifest.clone(),
            vec![
                (chunk("far"), vec![0.0, 1.0]),
                (chunk("near"), vec![1.0, 0.0]),
                (chunk("mid"), vec![0.7, 0.7]),
            ],
        )
        .expect("consistent embedding dimension");
        let query = Query {
            embedding: Some(vec![1.0, 0.0]),
            text: None,
            manifest,
        };
        let hits = index.search(&query, 10).expect("compatible manifest");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].chunk.text, "near");
        assert_eq!(hits[2].chunk.text, "far");
        assert!(hits[0].score >= hits[1].score);
        assert!(hits[1].score >= hits[2].score);
    }

    #[test]
    fn test_search_respects_limit() {
        let manifest = manifest_with("m", 1, Metric::DotProduct);
        let index = FlatIndex::build(
            manifest.clone(),
            vec![
                (chunk("a"), vec![1.0]),
                (chunk("b"), vec![2.0]),
                (chunk("c"), vec![3.0]),
            ],
        )
        .expect("consistent embedding dimension");
        let query = Query {
            embedding: Some(vec![1.0]),
            text: None,
            manifest,
        };
        let hits = index.search(&query, 2).expect("compatible manifest");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn test_search_on_empty_index_returns_empty_list_not_error() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest.clone(), Vec::new()).expect("empty corpus has no dimension");
        let query = Query {
            embedding: Some(vec![1.0, 0.0]),
            text: None,
            manifest,
        };
        let hits = index.search(&query, 10).expect("empty index is not an error");
        assert!(hits.is_empty());
    }

    #[test]
    fn test_search_rejects_model_mismatch_before_computing() {
        let manifest = manifest_with("model-a", 1, Metric::Cosine);
        // A candidate whose similarity would be perfect if it were ever scored.
        let index = FlatIndex::build(manifest.clone(), vec![(chunk("x"), vec![1.0, 0.0])])
            .expect("consistent embedding dimension");
        let mut query_manifest = manifest;
        query_manifest.model = "model-b".to_owned();
        let query = Query {
            embedding: Some(vec![1.0, 0.0]),
            text: None,
            manifest: query_manifest,
        };
        let err = index.search(&query, 10).expect_err("model mismatch is rejected");
        assert!(matches!(
            err,
            IndexError::ManifestMismatch { field: "model" }
        ));
    }

    #[test]
    fn test_search_rejects_chunker_version_mismatch_before_computing() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest.clone(), vec![(chunk("x"), vec![1.0, 0.0])])
            .expect("consistent embedding dimension");
        let mut query_manifest = manifest;
        query_manifest.chunker_version = 2;
        let query = Query {
            embedding: Some(vec![1.0, 0.0]),
            text: None,
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
    fn test_search_without_embedding_returns_missing_embedding() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest.clone(), vec![(chunk("x"), vec![1.0, 0.0])])
            .expect("consistent embedding dimension");
        let query = Query {
            embedding: None,
            text: None,
            manifest,
        };
        let err = index.search(&query, 10).expect_err("missing embedding");
        assert!(matches!(err, IndexError::MissingEmbedding));
    }

    #[test]
    fn test_search_is_deterministic_on_repeated_calls_with_tied_scores() {
        let manifest = manifest_with("m", 1, Metric::DotProduct);
        let index = FlatIndex::build(
            manifest.clone(),
            vec![(chunk("a"), vec![0.0]), (chunk("b"), vec![0.0])],
        )
        .expect("consistent embedding dimension");
        let query = Query {
            embedding: Some(vec![1.0]),
            text: None,
            manifest,
        };
        let first = index.search(&query, 10).expect("compatible manifest");
        let second = index.search(&query, 10).expect("compatible manifest");
        let first_order: Vec<_> = first.iter().map(|r| r.chunk.digest).collect();
        let second_order: Vec<_> = second.iter().map(|r| r.chunk.digest).collect();
        assert_eq!(first_order, second_order);
    }

    #[test]
    fn test_cosine_similarity_of_identical_vectors_is_one() {
        assert!((cosine_similarity(&[1.0, 2.0], &[1.0, 2.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_with_zero_vector_is_zero() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn test_euclidean_distance_of_identical_vectors_is_zero() {
        assert_eq!(euclidean_distance(&[1.0, 1.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn test_len_and_is_empty() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let empty = FlatIndex::build(manifest.clone(), Vec::new()).expect("empty corpus has no dimension");
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let one = FlatIndex::build(manifest, vec![(chunk("x"), vec![1.0])])
            .expect("consistent embedding dimension");
        assert!(!one.is_empty());
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn test_manifest_returns_stored_manifest() {
        let manifest = manifest_with("m", 3, Metric::Euclidean);
        let index = FlatIndex::build(manifest.clone(), Vec::new()).expect("empty corpus has no dimension");
        assert_eq!(index.manifest(), &manifest);
    }

    #[test]
    fn test_from_manifest_check_matches_compatible_with_field_order() {
        // `compatible_with` checks `model` before `chunker_version`; verify
        // the translation preserves that ordering rather than re-deriving it.
        let a = manifest_with("model-a", 1, Metric::Cosine);
        let mut b = a.clone();
        b.model = "model-b".to_owned();
        b.chunker_version = 2;
        let err = a.compatible_with(&b).expect_err("both fields differ");
        assert_eq!(err, LensTypesError::ManifestMismatch { field: "model" });
    }

    #[test]
    fn test_build_rejects_mismatched_embedding_dimensions() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let err = FlatIndex::build(
            manifest,
            vec![(chunk("a"), vec![1.0, 0.0]), (chunk("b"), vec![1.0])],
        )
        .expect_err("second entry has a different dimension than the first");
        assert!(matches!(
            err,
            IndexError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 1
            }
        ));
    }

    #[test]
    fn test_build_accepts_uniform_embedding_dimensions() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(
            manifest,
            vec![(chunk("a"), vec![1.0, 0.0]), (chunk("b"), vec![0.0, 1.0])],
        )
        .expect("uniform dimension is accepted");
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn test_build_on_empty_corpus_has_no_dimension_and_never_errors() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest, Vec::new()).expect("empty corpus is always valid");
        assert!(index.is_empty());
        assert_eq!(index.dimension(), None);
    }

    #[test]
    fn test_dimension_reports_embedding_length_of_non_empty_index() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(
            manifest,
            vec![(chunk("a"), vec![1.0, 0.0, 0.0]), (chunk("b"), vec![0.0, 1.0, 0.0])],
        )
        .expect("uniform dimension is accepted");
        assert_eq!(index.dimension(), Some(3));
    }

    #[test]
    fn test_search_rejects_query_embedding_with_wrong_dimension() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest.clone(), vec![(chunk("x"), vec![1.0, 0.0])])
            .expect("consistent embedding dimension");
        let query = Query {
            embedding: Some(vec![1.0, 0.0, 0.0]),
            text: None,
            manifest,
        };
        let err = index
            .search(&query, 10)
            .expect_err("query embedding dimension does not match the index");
        assert!(matches!(
            err,
            IndexError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 3
            }
        ));
    }

    #[test]
    fn test_search_on_empty_index_accepts_any_query_dimension() {
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(manifest.clone(), Vec::new()).expect("empty corpus has no dimension");
        let query = Query {
            embedding: Some(vec![1.0, 0.0, 0.0, 0.0]),
            text: None,
            manifest,
        };
        let hits = index
            .search(&query, 10)
            .expect("an empty index has no dimension to violate");
        assert!(hits.is_empty());
    }

    #[test]
    fn test_save_and_load_roundtrip_preserves_dimension_check() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = LensStore::open(dir.path()).expect("open store");
        let manifest = manifest_with("m", 1, Metric::Cosine);
        let index = FlatIndex::build(
            manifest.clone(),
            vec![(chunk("a"), vec![1.0, 0.0]), (chunk("b"), vec![0.0, 1.0])],
        )
        .expect("consistent embedding dimension");
        index.save(&store, "roundtrip-flat").expect("save");
        let loaded = FlatIndex::load(&store, "roundtrip-flat").expect("load");
        assert_eq!(loaded, index);

        let query = Query {
            embedding: Some(vec![1.0]),
            text: None,
            manifest,
        };
        let err = loaded
            .search(&query, 10)
            .expect_err("loaded index still enforces its dimension");
        assert!(matches!(
            err,
            IndexError::EmbeddingDimensionMismatch {
                expected: 2,
                actual: 1
            }
        ));
    }
}

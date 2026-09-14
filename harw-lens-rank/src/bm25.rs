//! Reine BM25-Bewertung über bereits tokenisierten Dokumenten.
//!
//! # Verantwortungsbereich
//! Besitzt [`bm25_scores`] und [`Bm25Params`]: die BM25-Formel (Okapi BM25,
//! IDF mit `+1`-Glättung) als reine Funktion über bereits tokenisierten Text
//! — kein Tokenizer, kein Chunk-Begriff, kein I/O. Wer tokenisiert
//! (Kleinschreibung, Wortgrenzen, Unicode-Behandlung) bleibt Sache des
//! Aufrufers; diese Funktion sieht nur `&[String]`-Listen.
//!
//! # Herkunft (W9-C4, F-206)
//! Vor diesem Knoten existierte dieselbe BM25-Formel wörtlich zweimal:
//! `harw-lens-index/src/bm25.rs` (`Bm25Index::term_score`, `BM25_K1`/`BM25_B`
//! als Konstanten `1.2`/`0.75`) und `harw-knowledge/src/memory/recall.rs`
//! (`KeywordRanker`), dokumentiert als bewusst geduldete Doppelung (F-206 im
//! Befundregister). Dieser Knoten zieht die Formel hierher, in die einzige
//! Crate des Workspace, die für reine Rangfunktionen ohne I/O/Systemzeit/
//! Zufall vorgesehen ist (siehe `lib.rs`). `harw-lens-index` ruft ab sofort
//! [`bm25_scores`] auf (`bm25.rs` dort); die Übernahme durch
//! `harw-knowledge` ist ausdrücklich **nicht** Teil dieses Knotens (das Crate
//! ist für W9-K2 gesperrt) und bleibt Folgearbeit.
//!
//! # Ausrichtung des Ergebnisses
//! [`bm25_scores`] gibt **genau einen Score pro Eingabedokument** zurück,
//! positionsgleich zu `docs` — auch `0.0` für Dokumente ohne
//! Query-Term-Treffer. Diese Funktion filtert nichts heraus: ob ein
//! Nullscore einen Kandidaten aus einer Trefferliste ausschließt, ist eine
//! Entscheidung des Aufrufers (siehe `harw-lens-index/src/bm25.rs`, das
//! Kandidaten mit Score `0.0` verwirft).
//!
//! # Nebenläufigkeit
//! Zustandslos und ohne innere Veränderlichkeit; beliebig aus mehreren
//! Threads parallel aufrufbar, solange die Eingaben nicht gleichzeitig
//! verändert werden.
//!
//! # Fehler
//! Keine — [`bm25_scores`] ist total und gibt nie `Result` zurück. Ein leerer
//! Korpus liefert eine leere Liste, eine leere Query liefert `0.0` für jedes
//! Dokument.
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::{bm25_scores, Bm25Params};
//!
//! let docs: Vec<Vec<String>> = vec![
//!     vec!["relevant".to_owned(), "term".to_owned(), "here".to_owned()],
//!     vec!["nothing".to_owned(), "matches".to_owned()],
//! ];
//! let doc_slices: Vec<&[String]> = docs.iter().map(Vec::as_slice).collect();
//! let query = vec!["relevant".to_owned()];
//!
//! let scores = bm25_scores(&doc_slices, &query, Bm25Params::default());
//! assert_eq!(scores.len(), 2);
//! assert!(scores[0] > 0.0);
//! assert_eq!(scores[1], 0.0);
//! ```

use std::collections::HashMap;

/// BM25-Sättigungs- und Längennormalisierungsparameter.
///
/// # Description
/// `k1` steuert, wie stark eine steigende Termfrequenz innerhalb eines
/// Dokuments den Beitrag noch erhöht (Sättigung); `b` steuert, wie stark die
/// Dokumentlänge relativ zur Korpus-Durchschnittslänge den Beitrag dämpft.
/// [`Default`] liefert die Werte, die vor diesem Knoten in
/// `harw-lens-index::Bm25Index` und `harw-knowledge::KeywordRanker` jeweils
/// als Konstanten `BM25_K1 = 1.2` und `BM25_B = 0.75` hartkodiert waren.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bm25Params {
    /// BM25-Sättigungsparameter für die Termfrequenz. Üblich: `1.2`.
    pub k1: f32,
    /// BM25-Längennormalisierungsparameter, `0.0`..=`1.0`. Üblich: `0.75`.
    pub b: f32,
}

impl Default for Bm25Params {
    /// Die Standardparameter `k1 = 1.2`, `b = 0.75` — identisch zu den vor
    /// diesem Knoten hartkodierten Konstanten in `harw-lens-index` und
    /// `harw-knowledge` (siehe Modul-Dokumentation).
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// Bewertet jedes Dokument in `docs` gegen `query` nach Okapi BM25.
///
/// # Description
/// Berechnet für jedes Dokument die Summe der BM25-Beiträge aller
/// Query-Terme: IDF mit `+1`-Glättung (`ln(((N - df + 0.5) / (df + 0.5)) +
/// 1)`), multipliziert mit der termfrequenz-gesättigten Gewichtung
/// (`f * (k1 + 1) / (f + k1 * (1 - b + b * |D| / avgdl))`). `avgdl`, die
/// durchschnittliche Dokumentlänge in Tokens über den ganzen Korpus, fällt
/// auf `1.0` zurück, wenn der Korpus leer ist oder das Mittel `0` ergibt —
/// eine Division durch `0` entstünde sonst bei ausschließlich leeren
/// Dokumenten. Terme, die in einem Dokument nicht vorkommen, tragen `0.0`
/// bei, ohne die IDF-Formel für sie auszuwerten.
///
/// # Arguments
/// - `docs` (`&[&[String]]`): die tokenisierten Dokumente des Korpus, je
///   Dokument eine Liste seiner Terme in Vorkommensreihenfolge (Duplikate
///   erlaubt und erwartet — die Termfrequenz zählt sie).
/// - `query` (`&[String]`): die tokenisierten Query-Terme.
/// - `params` (`Bm25Params`): `k1`/`b`, siehe [`Bm25Params`].
///
/// # Returns
/// Ein `Vec<f32>` derselben Länge wie `docs`, positionsgleich: Index `i` ist
/// der BM25-Score von `docs[i]`. Leer, wenn `docs` leer ist. `0.0` für jedes
/// Dokument, wenn `query` leer ist (kein Term, kein Beitrag).
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_lens_rank::{bm25_scores, Bm25Params};
///
/// // "beta" kommt in jedem Dokument vor (df = 5), "alpha" nur im ersten
/// // (df = 1) - der seltenere Term muss höher bewerten.
/// let docs: Vec<Vec<String>> = vec![
///     vec!["alpha".to_owned(), "beta".to_owned()],
///     vec!["beta".to_owned(), "gamma".to_owned()],
///     vec!["beta".to_owned(), "delta".to_owned()],
///     vec!["beta".to_owned(), "epsilon".to_owned()],
///     vec!["beta".to_owned(), "zeta".to_owned()],
/// ];
/// let doc_slices: Vec<&[String]> = docs.iter().map(Vec::as_slice).collect();
///
/// let rare = bm25_scores(&doc_slices, &["alpha".to_owned()], Bm25Params::default());
/// let frequent = bm25_scores(&doc_slices, &["beta".to_owned()], Bm25Params::default());
/// assert!(rare[0] > frequent[0]);
/// ```
#[must_use]
pub fn bm25_scores(docs: &[&[String]], query: &[String], params: Bm25Params) -> Vec<f32> {
    if docs.is_empty() {
        return Vec::new();
    }
    if query.is_empty() {
        return vec![0.0; docs.len()];
    }

    let doc_count = docs.len() as f64;
    let lengths: Vec<f64> = docs.iter().map(|doc| doc.len() as f64).collect();
    let avg_doc_length = {
        let total: f64 = lengths.iter().sum();
        let mean = total / doc_count;
        if mean > 0.0 { mean } else { 1.0 }
    };

    let term_frequencies: Vec<HashMap<&str, u32>> = docs
        .iter()
        .map(|doc| {
            let mut counts: HashMap<&str, u32> = HashMap::new();
            for term in doc.iter() {
                *counts.entry(term.as_str()).or_insert(0) += 1;
            }
            counts
        })
        .collect();

    let mut document_frequency: HashMap<&str, usize> = HashMap::new();
    for counts in &term_frequencies {
        for term in counts.keys() {
            *document_frequency.entry(term).or_insert(0) += 1;
        }
    }

    let k1 = f64::from(params.k1);
    let b = f64::from(params.b);

    (0..docs.len())
        .map(|i| {
            let counts = &term_frequencies[i];
            let length = lengths[i];
            let score: f64 = query
                .iter()
                .map(|term| {
                    let f = f64::from(counts.get(term.as_str()).copied().unwrap_or(0));
                    if f == 0.0 {
                        return 0.0;
                    }
                    let df = document_frequency.get(term.as_str()).copied().unwrap_or(0) as f64;
                    let idf = (((doc_count - df + 0.5) / (df + 0.5)) + 1.0).ln();
                    let denom = f + k1 * (1.0 - b + b * length / avg_doc_length);
                    idf * (f * (k1 + 1.0)) / denom
                })
                .sum();
            score as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| (*t).to_owned()).collect()
    }

    fn slices(docs: &[Vec<String>]) -> Vec<&[String]> {
        docs.iter().map(Vec::as_slice).collect()
    }

    #[test]
    fn test_bm25_params_default_matches_historical_constants() {
        let params = Bm25Params::default();
        assert_eq!(params.k1, 1.2);
        assert_eq!(params.b, 0.75);
    }

    #[test]
    fn test_bm25_scores_empty_docs_returns_empty_vec() {
        let scores = bm25_scores(&[], &[doc(&["query"])[0].clone()], Bm25Params::default());
        assert!(scores.is_empty());
    }

    #[test]
    fn test_bm25_scores_empty_query_returns_zero_for_every_doc() {
        let docs = vec![doc(&["alpha", "beta"]), doc(&["gamma"])];
        let scores = bm25_scores(&slices(&docs), &[], Bm25Params::default());
        assert_eq!(scores, vec![0.0, 0.0]);
    }

    #[test]
    fn test_bm25_scores_doc_with_term_outranks_doc_without() {
        let docs = vec![doc(&["relevant", "term", "here"]), doc(&["nothing", "matches"])];
        let scores = bm25_scores(&slices(&docs), &[doc(&["relevant"])[0].clone()], Bm25Params::default());
        assert!(scores[0] > 0.0);
        assert_eq!(scores[1], 0.0);
    }

    #[test]
    fn test_bm25_scores_rare_term_scores_higher_than_frequent_term() {
        // "beta" erscheint in jedem Dokument (df = 5); "alpha" nur im ersten
        // (df = 1). Alle Dokumente haben dieselbe Tokenlänge, also ist die
        // Längennormalisierung für beide Abfragen identisch.
        let docs = vec![
            doc(&["alpha", "beta"]),
            doc(&["beta", "gamma"]),
            doc(&["beta", "delta"]),
            doc(&["beta", "epsilon"]),
            doc(&["beta", "zeta"]),
        ];
        let doc_slices = slices(&docs);
        let rare = bm25_scores(&doc_slices, &[doc(&["alpha"])[0].clone()], Bm25Params::default());
        let frequent = bm25_scores(&doc_slices, &[doc(&["beta"])[0].clone()], Bm25Params::default());
        assert!(
            rare[0] > frequent[0],
            "rare term score {} should exceed frequent term score {}",
            rare[0],
            frequent[0]
        );
    }

    #[test]
    fn test_bm25_scores_result_length_matches_doc_count() {
        let docs = vec![doc(&["a"]), doc(&["b"]), doc(&["c"])];
        let scores = bm25_scores(&slices(&docs), &[doc(&["a"])[0].clone()], Bm25Params::default());
        assert_eq!(scores.len(), 3);
    }

    #[test]
    fn test_bm25_scores_is_order_aligned_with_input_docs() {
        let docs = vec![doc(&["nothing"]), doc(&["needle"]), doc(&["nothing"])];
        let scores = bm25_scores(&slices(&docs), &[doc(&["needle"])[0].clone()], Bm25Params::default());
        assert_eq!(scores[0], 0.0);
        assert!(scores[1] > 0.0);
        assert_eq!(scores[2], 0.0);
    }

    #[test]
    fn test_bm25_scores_custom_params_change_result() {
        let docs = vec![doc(&["term", "term", "term"]), doc(&["term"])];
        let default_scores = bm25_scores(&slices(&docs), &[doc(&["term"])[0].clone()], Bm25Params::default());
        let flattened_scores = bm25_scores(
            &slices(&docs),
            &[doc(&["term"])[0].clone()],
            Bm25Params { k1: 0.0, b: 0.75 },
        );
        // Bei k1 = 0 sättigt die Termfrequenz sofort: jeder nichttriviale Treffer
        // liefert denselben Beitrag pro Term, unabhängig von f.
        assert!((flattened_scores[0] - flattened_scores[1]).abs() < 1e-6);
        assert_ne!(default_scores[0], default_scores[1]);
    }
}

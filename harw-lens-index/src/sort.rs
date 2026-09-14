//! Interne, geteilte Sortier- und Top-k-Hilfen für [`crate::FlatIndex`] und
//! [`crate::Bm25Index`].
//!
//! # Verantwortungsbereich
//! Besitzt [`sort_ranked_desc`] ("nach Rangwert absteigend, mit stabilem
//! Tiebreak über `ChunkDigest`") und [`top_k_ranked`] (dieselbe Ordnung,
//! aber ohne die vollständige Liste je zu sortieren — siehe dort, Knoten
//! W10-L1). Beide Indextypen dieses Crates brauchen exakt diese Sortierung;
//! sie liegt hier einmal, statt in jeder `search`-Implementierung erneut
//! geschrieben zu werden. Dieses Modul ist `pub(crate)` — kein Konsument
//! außerhalb dieser Crate sieht es.
//!
//! # Nebenläufigkeit
//! Reine Funktionen ohne geteilten Zustand; sicher aus mehreren Threads
//! gleichzeitig aufrufbar.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use harw_lens_types::Ranked;

/// Sortiert `ranked` absteigend nach `score`, mit stabilem Tiebreak über
/// `chunk.digest` bei Punktgleichheit.
///
/// # Description
/// Nutzt `f32::total_cmp` statt `partial_cmp`: `score` entsteht aus einer
/// Kosinus-Ähnlichkeit, einem Skalarprodukt, einer negierten euklidischen
/// Distanz oder einem BM25-Wert — keiner dieser Werte soll `NaN` sein, aber
/// `partial_cmp` liefert bei einem unerwarteten `NaN` `None`, und `sort_by`
/// paniert dann auf einem inkonsistenten Vergleichsergebnis. `total_cmp`
/// definiert stattdessen eine totale Ordnung über alle `f32`-Bitmuster,
/// auch `NaN`, und kann nie paniken. Der Tiebreak über `chunk.digest`
/// (aufsteigend) macht die Reihenfolge bei Punktgleichheit deterministisch:
/// dieselbe Abfrage gegen denselben Index liefert immer dieselbe Reihenfolge,
/// unabhängig von der internen Speicherreihenfolge der Kandidaten.
///
/// # Arguments
/// - `ranked` (`&mut [Ranked]`): die zu sortierende Trefferliste, in place
///   sortiert.
pub(crate) fn sort_ranked_desc(ranked: &mut [Ranked]) {
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk.digest.cmp(&b.chunk.digest))
    });
}

/// Ordnungs-Wrapper um [`Ranked`] für [`top_k_ranked`]'s [`BinaryHeap`].
///
/// # Description
/// [`BinaryHeap`] ist ein Max-Heap: `pop`/`peek` liefern immer das nach `Ord`
/// größte Element. `top_k_ranked` will aber jederzeit den **schlechtesten**
/// der bisher gehaltenen `k` Kandidaten oben liegen haben, um ihn gegen einen
/// besseren neuen Kandidaten auszutauschen. Deshalb spiegelt `Ord::cmp` genau
/// den Vergleich, den [`sort_ranked_desc`] für `sort_by(a, b)` verwendet
/// (`self` an der Stelle von `a`, `other` an der Stelle von `b`): ein
/// besserer Kandidat (höherer Score, bei Punktgleichheit kleinerer
/// `ChunkDigest`) ist unter dieser `Ord`-Implementierung "kleiner", ein
/// schlechterer "größer" — und liegt damit im Max-Heap oben. `total_cmp`
/// macht diesen Vergleich für jedes `f32`-Bitmuster total, auch `NaN`: kein
/// Vergleich kann hier je `None` liefern oder paniken.
#[derive(Debug, Clone, PartialEq)]
struct HeapRanked(Ranked);

impl Eq for HeapRanked {}

impl PartialOrd for HeapRanked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapRanked {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .score
            .total_cmp(&self.0.score)
            .then_with(|| self.0.chunk.digest.cmp(&other.0.chunk.digest))
    }
}

/// Liefert die `k` besten Elemente aus `items`, absteigend nach `score`
/// sortiert, mit demselben Tiebreak wie [`sort_ranked_desc`].
///
/// # Description
/// Ersetzt "alles sammeln, vollständig sortieren (`O(n log n)`), auf `k`
/// abschneiden" durch einen Min-Heap fester Kapazität `k`: jedes Element wird
/// einzeln betrachtet und nur dann eingefügt, wenn der Heap noch nicht voll
/// ist oder das Element besser ist als der aktuell schlechteste gehaltene
/// Kandidat (siehe [`HeapRanked`]) — macht das Verfahren `O(n log k)` statt
/// `O(n log n)`, ein Gewinn immer dann, wenn `k` deutlich kleiner als die
/// Gesamtzahl der Kandidaten ist (der übliche Fall: `limit` ist klein, der
/// Index groß). Am Ende wird nur der Heap selbst (höchstens `k` Elemente)
/// mit [`sort_ranked_desc`] in die endgültige Reihenfolge gebracht —
/// deshalb liefert diese Funktion exakt dieselbe Reihenfolge wie
/// "vollständig sortieren, dann auf `k` abschneiden", nie nur eine
/// Teilmenge in anderer Reihenfolge.
///
/// # Arguments
/// - `items` (`impl Iterator<Item = Ranked>`): die zu bewertenden Kandidaten,
///   in beliebiger Reihenfolge.
/// - `k` (`usize`): die maximale Anzahl zurückgegebener Treffer.
///
/// # Returns
/// Bis zu `k` [`Ranked`]-Treffer, absteigend nach `score` sortiert, mit
/// stabilem Tiebreak über `ChunkDigest`. Leer, wenn `k == 0` oder `items`
/// keine Elemente liefert.
///
/// # Panics
/// Keine — `total_cmp` über [`HeapRanked`] ist für jedes `f32`-Bitmuster
/// definiert, auch `NaN` und `Inf`.
pub(crate) fn top_k_ranked<I: Iterator<Item = Ranked>>(items: I, k: usize) -> Vec<Ranked> {
    if k == 0 {
        return Vec::new();
    }

    let mut heap: BinaryHeap<HeapRanked> = BinaryHeap::with_capacity(k);
    for item in items {
        let candidate = HeapRanked(item);
        if heap.len() < k {
            heap.push(candidate);
        } else if let Some(worst) = heap.peek() {
            if candidate < *worst {
                heap.pop();
                heap.push(candidate);
            }
        }
    }

    let mut result: Vec<Ranked> = heap.into_iter().map(|entry| entry.0).collect();
    sort_ranked_desc(&mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
    use harw_types::ContentDigest;

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
    fn test_sort_ranked_desc_orders_by_score_descending() {
        let mut ranked = vec![
            Ranked {
                chunk: chunk("low"),
                score: 0.1,
            },
            Ranked {
                chunk: chunk("high"),
                score: 0.9,
            },
        ];
        sort_ranked_desc(&mut ranked);
        assert_eq!(ranked[0].chunk.text, "high");
        assert_eq!(ranked[1].chunk.text, "low");
    }

    #[test]
    fn test_sort_ranked_desc_breaks_ties_by_chunk_digest_ascending() {
        let a = chunk("a-content");
        let b = chunk("b-content");
        let (first, second) = if a.digest <= b.digest { (a, b) } else { (b, a) };
        let mut ranked = vec![
            Ranked {
                chunk: second.clone(),
                score: 0.5,
            },
            Ranked {
                chunk: first.clone(),
                score: 0.5,
            },
        ];
        sort_ranked_desc(&mut ranked);
        assert_eq!(ranked[0].chunk.digest, first.digest);
        assert_eq!(ranked[1].chunk.digest, second.digest);
    }

    #[test]
    fn test_sort_ranked_desc_is_deterministic_across_repeated_calls() {
        let a = chunk("a-content");
        let b = chunk("b-content");
        let mut first_run = vec![
            Ranked {
                chunk: a.clone(),
                score: 0.5,
            },
            Ranked {
                chunk: b.clone(),
                score: 0.5,
            },
        ];
        let mut second_run = first_run.clone();
        sort_ranked_desc(&mut first_run);
        sort_ranked_desc(&mut second_run);
        let first_order: Vec<_> = first_run.iter().map(|r| r.chunk.digest).collect();
        let second_order: Vec<_> = second_run.iter().map(|r| r.chunk.digest).collect();
        assert_eq!(first_order, second_order);
    }

    fn ranked(text: &str, score: f32) -> Ranked {
        Ranked {
            chunk: chunk(text),
            score,
        }
    }

    #[test]
    fn test_top_k_ranked_matches_full_sort_then_truncate_reference() {
        let candidates = vec![
            ranked("a", 0.3),
            ranked("b", 0.9),
            ranked("c", 0.1),
            ranked("d", 0.9),
            ranked("e", 0.5),
            ranked("f", 0.7),
            ranked("g", 0.9),
        ];

        for k in 0..=candidates.len() + 2 {
            let mut reference = candidates.clone();
            sort_ranked_desc(&mut reference);
            reference.truncate(k);

            let via_heap = top_k_ranked(candidates.clone().into_iter(), k);
            assert_eq!(
                via_heap.iter().map(|r| r.chunk.digest).collect::<Vec<_>>(),
                reference.iter().map(|r| r.chunk.digest).collect::<Vec<_>>(),
                "mismatch for k = {k}"
            );
            assert_eq!(
                via_heap.iter().map(|r| r.score).collect::<Vec<_>>(),
                reference.iter().map(|r| r.score).collect::<Vec<_>>(),
                "score mismatch for k = {k}"
            );
        }
    }

    #[test]
    fn test_top_k_ranked_zero_limit_returns_empty() {
        let candidates = vec![ranked("a", 1.0), ranked("b", 2.0)];
        assert!(top_k_ranked(candidates.into_iter(), 0).is_empty());
    }

    #[test]
    fn test_top_k_ranked_limit_larger_than_input_returns_all_sorted() {
        let candidates = vec![ranked("low", 0.1), ranked("high", 0.9)];
        let result = top_k_ranked(candidates.into_iter(), 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].chunk.text, "high");
        assert_eq!(result[1].chunk.text, "low");
    }

    #[test]
    fn test_top_k_ranked_on_empty_input_returns_empty() {
        assert!(top_k_ranked(std::iter::empty::<Ranked>(), 5).is_empty());
    }

    #[test]
    fn test_top_k_ranked_is_nan_safe_and_does_not_panic() {
        let candidates = vec![
            ranked("nan", f32::NAN),
            ranked("normal", 0.5),
            ranked("negative-nan", -f32::NAN),
        ];
        // Muss ohne Panic terminieren und darf keine Elemente verlieren.
        let result = top_k_ranked(candidates.into_iter(), 2);
        assert_eq!(result.len(), 2);
    }
}

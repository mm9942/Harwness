//! Interne, geteilte Sortierhilfe für [`crate::FlatIndex`] und
//! [`crate::Bm25Index`].
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`sort_ranked_desc`]: „nach Rangwert absteigend,
//! mit stabilem Tiebreak über `ChunkDigest`". Beide Indextypen dieses Crates
//! brauchen exakt diese Sortierung; sie liegt hier einmal, statt in jeder
//! `search`-Implementierung erneut geschrieben zu werden. Dieses Modul ist
//! `pub(crate)` — kein Konsument außerhalb dieser Crate sieht es.
//!
//! # Nebenläufigkeit
//! Eine reine Funktion ohne geteilten Zustand; sicher aus mehreren Threads
//! gleichzeitig aufrufbar.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler.

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
}

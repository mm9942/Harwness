//! Reciprocal Rank Fusion.
//!
//! # Verantwortungsbereich
//! Verschmilzt mehrere bereits gerankte Kandidatenlisten zu einer einzigen
//! Liste über den RRF-Score. Kennt keine Quelle und kein Retrieval-Backend —
//! nur `Vec<Ranked>` hinein, `Vec<Ranked>` hinaus.
//!
//! # Nebenläufigkeit
//! Zustandslos und ohne innere Veränderlichkeit; beliebig aus mehreren
//! Threads parallel aufrufbar, solange die Eingaben nicht gleichzeitig
//! verändert werden.
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::rrf_fuse;
//!
//! let fused = rrf_fuse(&[], 60.0);
//! assert!(fused.is_empty());
//! ```

use std::collections::HashMap;

use harw_lens_types::{ChunkDigest, Ranked};

/// Verschmilzt mehrere Ranglisten über Reciprocal Rank Fusion.
///
/// # Description
/// Für jedes Vorkommen eines Dokuments in einer Liste trägt sein Rang
/// (1-basiert, Index 0 = Rang 1) den Beitrag `1 / (k + rang)` bei. Die
/// Beiträge über alle Listen werden je Dokument — identifiziert über
/// [`ChunkDigest`] — summiert; das Ergebnis ist absteigend nach dieser Summe
/// sortiert.
///
/// **Reihenfolgeunabhängig:** die Permutation der Eingabelisten ändert das
/// Ergebnis nicht, und zwar exakt, nicht nur ungefähr. Erstens hängt der
/// Rang eines Dokuments allein von seiner Position *innerhalb* seiner
/// eigenen Liste ab, nie von der Position dieser Liste im äußeren Feld.
/// Zweitens werden die Beiträge je Dokument gesammelt und **vor** der
/// Summierung aufsteigend sortiert, statt sie in Ankunftsreihenfolge zu
/// addieren — Gleitkomma-Addition ist nicht assoziativ, ohne diesen Schritt
/// könnte allein die Reihenfolge, in der die Listen hereinkamen, die Summe
/// um ein Bit verschieben. Bei Punktgleichheit entscheidet aufsteigend der
/// [`ChunkDigest`] — ein Merkmal des Dokuments, nie die Fundreihenfolge.
///
/// # Arguments
/// - `lists` (`&[Vec<Ranked>]`): die zu verschmelzenden Ranglisten. Der Rang
///   eines Eintrags ergibt sich aus seiner Position innerhalb seiner Liste.
/// - `k` (`f32`): die RRF-Konstante. Üblich ist `60.0`; die Funktion erzwingt
///   keinen bestimmten Wert.
///
/// # Returns
/// Alle Dokumente, die in mindestens einer Liste vorkamen, mit `score` gleich
/// der summierten RRF-Punktzahl, absteigend sortiert.
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_lens_rank::rrf_fuse;
/// use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, Ranked, SourceRef};
/// use harw_types::ContentDigest;
///
/// fn chunk(text: &str) -> Chunk {
///     Chunk {
///         digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
///         source: SourceRef::Artifact { id: "a".to_string() },
///         span: ByteSpan { start: 0, end: text.len() },
///         text: text.to_string(),
///     }
/// }
///
/// let a = chunk("alpha");
/// let b = chunk("beta");
/// let list_one = vec![
///     Ranked { chunk: a.clone(), score: 1.0 },
///     Ranked { chunk: b.clone(), score: 0.5 },
/// ];
/// let list_two = vec![Ranked { chunk: a, score: 1.0 }];
///
/// let fused = rrf_fuse(&[list_one, list_two], 60.0);
/// assert_eq!(fused.len(), 2);
/// assert!(fused[0].score > fused[1].score);
/// ```
#[must_use]
pub fn rrf_fuse(lists: &[Vec<Ranked>], k: f32) -> Vec<Ranked> {
    let mut contributions: HashMap<ChunkDigest, (Ranked, Vec<f32>)> = HashMap::new();

    for list in lists {
        for (position, ranked) in list.iter().enumerate() {
            let rank = (position + 1) as f32;
            let contribution = 1.0 / (k + rank);

            contributions
                .entry(ranked.chunk.digest)
                .and_modify(|(_, scores)| scores.push(contribution))
                .or_insert_with(|| (ranked.clone(), vec![contribution]));
        }
    }

    let mut fused: Vec<Ranked> = contributions
        .into_values()
        .map(|(mut ranked, mut scores)| {
            // Aufsteigend sortieren, bevor summiert wird: macht die Summe
            // unabhängig davon, in welcher Reihenfolge die Listen hereinkamen
            // (siehe Begründung in der Funktionsdokumentation).
            scores.sort_by(f32::total_cmp);
            ranked.score = scores.into_iter().sum();
            ranked
        })
        .collect();

    fused.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk.digest.cmp(&b.chunk.digest))
    });

    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Chunk, SourceRef};
    use harw_types::ContentDigest;

    fn chunk(text: &str) -> Chunk {
        Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::Artifact {
                id: "test".to_string(),
            },
            span: ByteSpan {
                start: 0,
                end: text.len(),
            },
            text: text.to_string(),
        }
    }

    fn ranked(text: &str, score: f32) -> Ranked {
        Ranked {
            chunk: chunk(text),
            score,
        }
    }

    #[test]
    fn test_rrf_fuse_empty_input_returns_empty() {
        let fused = rrf_fuse(&[], 60.0);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_rrf_fuse_permutation_of_three_lists_is_order_independent() {
        let a = ranked("alpha", 0.9);
        let b = ranked("beta", 0.8);
        let c = ranked("gamma", 0.7);

        let list_a = vec![a.clone(), b.clone(), c.clone()];
        let list_b = vec![b.clone(), c.clone(), a.clone()];
        let list_c = vec![c.clone(), a.clone(), b.clone()];

        let lists = [list_a, list_b, list_c];
        let baseline = rrf_fuse(
            &[lists[0].clone(), lists[1].clone(), lists[2].clone()],
            60.0,
        );

        let permutations: [[usize; 3]; 3] = [[0, 1, 2], [1, 2, 0], [2, 0, 1]];
        for perm in permutations {
            let permuted: Vec<Vec<Ranked>> = perm.iter().map(|&i| lists[i].clone()).collect();
            let fused = rrf_fuse(&permuted, 60.0);
            assert_eq!(fused, baseline);
        }
    }

    #[test]
    fn test_rrf_fuse_consensus_document_beats_single_top_rank() {
        let consensus = ranked("consensus", 0.1);
        let single = ranked("single", 0.1);
        let filler_one = ranked("filler-one", 0.1);
        let filler_two = ranked("filler-two", 0.1);

        // `single` erreicht in seiner einzigen Liste den bestmöglichen Rang 1.
        // `consensus` erscheint in allen drei Listen, aber nie an Rang 1.
        let list_one = vec![filler_one, consensus.clone()];
        let list_two = vec![single.clone(), consensus.clone()];
        let list_three = vec![filler_two, consensus.clone()];

        let fused = rrf_fuse(&[list_one, list_two, list_three], 60.0);

        let consensus_score = fused
            .iter()
            .find(|r| r.chunk.digest == consensus.chunk.digest)
            .expect("consensus present")
            .score;
        let single_score = fused
            .iter()
            .find(|r| r.chunk.digest == single.chunk.digest)
            .expect("single present")
            .score;

        assert!(consensus_score > single_score);
    }
}

//! Maximal Marginal Relevance.
//!
//! # Verantwortungsbereich
//! Wählt aus einer bereits abgerufenen Kandidatenmenge iterativ die Mischung
//! aus Relevanz und Vielfalt. Arbeitet ausschließlich auf dem übergebenen
//! Ausschnitt — kein Zugriff auf den Index, keine Nachbearbeitung.
//!
//! # Nebenläufigkeit
//! Zustandslos und ohne innere Veränderlichkeit; sicher aus mehreren Threads
//! parallel aufrufbar.
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::mmr;
//!
//! let selected = mmr(&[], 0.5, 3);
//! assert!(selected.is_empty());
//! ```

use std::collections::HashSet;

use harw_lens_types::Ranked;

/// Baut die Menge der kleingeschriebenen Wort-Tokens eines Textes.
///
/// # Warum Wort-Jaccard statt Trigramme oder Embeddings
/// `mmr` arbeitet auf einer bereits abgerufenen, kleinen Kandidatenmenge
/// (typischerweise Dutzende, nicht Tausende) — nicht auf dem Index. In dieser
/// Größenordnung genügt ein grobes, embeddingfreies Textmaß: Wort-Jaccard ist
/// billig, ohne externes Modell berechenbar und deterministisch. Trigramme
/// wären robuster gegenüber kleinen Schreibvarianten, aber teurer zu bilden;
/// für den Zweck hier — grobe thematische Überlappung zwischen bereits
/// gewählten Chunks erkennen, um Redundanz zu vermeiden — reicht Wortebene.
fn tokenize(text: &str) -> HashSet<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// Jaccard-Ähnlichkeit zweier Tokenmengen: Schnittmenge über Vereinigung.
///
/// Zwei leere Mengen gelten als unähnlich (`0.0`), nicht als undefiniert —
/// ein leerer Text trägt keine Information, aus der sich Ähnlichkeit
/// begründen ließe.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    intersection as f32 / union as f32
}

/// Maximal Marginal Relevance: Relevanz gegen Vielfalt.
///
/// # Description
/// Wählt iterativ aus `candidates` das Dokument mit dem höchsten Wert
/// `lambda * relevanz - (1 - lambda) * max_ähnlichkeit_zu_bereits_gewählten`,
/// bis `limit` Dokumente gewählt sind oder keine Kandidaten mehr übrig sind.
/// `relevanz` ist [`Ranked::score`]; die Ähnlichkeit ist die Wort-Jaccard-
/// Ähnlichkeit der Chunk-Texte (Begründung in der Moduldokumentation).
///
/// **Deterministisch:** bei gleicher Eingabe stets dasselbe Ergebnis. Bei
/// Punktgleichheit gewinnt der zuerst in `candidates` auftretende Kandidat —
/// eine feste Regel, unabhängig von Nebenläufigkeit oder Hash-Iteration.
///
/// `lambda` wird auf `[0.0, 1.0]` geklemmt statt geprüft: `lambda = 1.0`
/// liefert reine Relevanzreihenfolge (der Vielfaltsterm trägt mit Gewicht
/// `0`); `lambda = 0.0` bevorzugt Vielfalt (der Relevanzterm trägt mit
/// Gewicht `0`). Die erste Wahl hat dann noch keinen Vielfaltsbezugspunkt —
/// die maximale Ähnlichkeit zu einer leeren Auswahl ist `0` für jeden
/// Kandidaten — und fällt deterministisch auf den ersten Kandidaten der
/// Eingabe.
///
/// # Arguments
/// - `candidates` (`&[Ranked]`): die bereits abgerufene Kandidatenmenge.
/// - `lambda` (`f32`): Gewicht zwischen Relevanz (`1.0`) und Vielfalt
///   (`0.0`); wird auf `[0.0, 1.0]` geklemmt.
/// - `limit` (`usize`): maximale Anzahl gewählter Dokumente.
///
/// # Returns
/// Bis zu `limit` Kandidaten in Auswahlreihenfolge (nicht notwendigerweise
/// die Reihenfolge von `candidates`).
///
/// # Panics
/// Keine. Ein außerhalb `[0, 1]` liegendes `lambda` wird geklemmt statt
/// geprüft.
///
/// # Examples
/// ```rust
/// use harw_lens_rank::mmr;
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
/// let candidates = vec![
///     Ranked { chunk: chunk("rust ownership"), score: 0.9 },
///     Ranked { chunk: chunk("rust borrowing"), score: 0.8 },
/// ];
///
/// let selected = mmr(&candidates, 1.0, 1);
/// assert_eq!(selected[0].chunk.text, "rust ownership");
/// ```
#[must_use]
pub fn mmr(candidates: &[Ranked], lambda: f32, limit: usize) -> Vec<Ranked> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }

    let lambda = lambda.clamp(0.0, 1.0);
    let tokens: Vec<HashSet<String>> = candidates
        .iter()
        .map(|c| tokenize(&c.chunk.text))
        .collect();

    let mut remaining: Vec<usize> = (0..candidates.len()).collect();
    let mut selected: Vec<usize> = Vec::with_capacity(limit.min(candidates.len()));

    while !remaining.is_empty() && selected.len() < limit {
        let mut best_pos = 0usize;
        let mut best_value = f32::NEG_INFINITY;

        for (pos, &idx) in remaining.iter().enumerate() {
            let relevance = candidates[idx].score;
            let max_similarity = selected
                .iter()
                .map(|&sel| jaccard(&tokens[idx], &tokens[sel]))
                .fold(0.0f32, f32::max);
            let value = lambda * relevance - (1.0 - lambda) * max_similarity;

            if value > best_value {
                best_value = value;
                best_pos = pos;
            }
        }

        selected.push(remaining.remove(best_pos));
    }

    selected
        .into_iter()
        .map(|idx| candidates[idx].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
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
    fn test_mmr_empty_candidates_returns_empty() {
        assert!(mmr(&[], 0.5, 5).is_empty());
    }

    #[test]
    fn test_mmr_zero_limit_returns_empty() {
        let candidates = vec![ranked("one", 0.5)];
        assert!(mmr(&candidates, 0.5, 0).is_empty());
    }

    #[test]
    fn test_mmr_called_twice_is_deterministic() {
        let candidates = vec![
            ranked("rust ownership model", 0.9),
            ranked("rust borrowing rules", 0.8),
            ranked("python generators", 0.3),
        ];

        let first = mmr(&candidates, 0.5, 3);
        let second = mmr(&candidates, 0.5, 3);

        assert_eq!(first, second);
    }

    #[test]
    fn test_mmr_lambda_one_returns_pure_relevance_order() {
        let candidates = vec![
            ranked("alpha alpha alpha", 1.0),
            ranked("alpha alpha beta", 0.9),
            ranked("gamma delta epsilon", 0.1),
        ];

        let selected = mmr(&candidates, 1.0, 3);

        assert_eq!(selected[0].chunk.text, "alpha alpha alpha");
        assert_eq!(selected[1].chunk.text, "alpha alpha beta");
        assert_eq!(selected[2].chunk.text, "gamma delta epsilon");
    }

    #[test]
    fn test_mmr_lambda_zero_prefers_diversity_over_relevance() {
        let a = ranked("alpha alpha alpha", 1.0);
        let near_duplicate = ranked("alpha alpha beta", 0.9);
        let diverse = ranked("gamma delta epsilon", 0.1);
        let candidates = vec![a.clone(), near_duplicate.clone(), diverse.clone()];

        // Mit lambda = 0 trägt Relevanz nirgends bei; nach der ersten Wahl
        // (deterministisch der erste Kandidat) muss der thematisch entfernte,
        // niedriger bewertete Kandidat vor dem hochbewerteten Beinah-Duplikat
        // gewählt werden.
        let selected = mmr(&candidates, 0.0, 2);

        assert_eq!(selected[0].chunk.text, a.chunk.text);
        assert_eq!(selected[1].chunk.text, diverse.chunk.text);
    }

    #[test]
    fn test_mmr_limit_caps_selection_size() {
        let candidates = vec![
            ranked("one", 0.9),
            ranked("two", 0.8),
            ranked("three", 0.7),
        ];

        let selected = mmr(&candidates, 0.5, 2);
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn test_mmr_lambda_out_of_range_is_clamped_not_panicking() {
        let candidates = vec![ranked("only one", 0.5), ranked("another", 0.4)];

        let above_range = mmr(&candidates, 5.0, 2);
        let below_range = mmr(&candidates, -3.0, 2);

        assert_eq!(above_range.len(), 2);
        assert_eq!(below_range.len(), 2);
    }
}

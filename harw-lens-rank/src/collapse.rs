//! Deduplikation mit Kantenschutz.
//!
//! # Verantwortungsbereich
//! Lässt Duplikate nach [`CollapsePolicy`] zusammenfallen — außer wenn eine
//! `Contradicts`-Kante zwischen ihnen besteht. Dann bleiben beide Fassungen
//! erhalten: zwei einander widersprechende Aussagen sind keine Dublette, und
//! sie zu entdoppeln löscht genau die Information, wegen der man sie
//! behalten will. Eine `SupersededBy`-Kante darf dagegen kollabieren — die
//! neuere Fassung überlebt. Diese Unterscheidung stammt aus dem Vorprojekt
//! Vectory und wird hier bewusst wieder aufgenommen.
//!
//! # Nebenläufigkeit
//! Zustandslos und ohne innere Veränderlichkeit; sicher aus mehreren Threads
//! parallel aufrufbar.
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::collapse;
//! use harw_lens_types::{CollapsePolicy, EdgeIndex};
//!
//! let result = collapse(&[], CollapsePolicy::ByDigest, &EdgeIndex::default());
//! assert!(result.is_empty());
//! ```
//!
//! # Vertragslücke gegenüber Contract-Master Abschnitt C
//! Diese Datei ruft drei Methoden auf [`harw_lens_types::EdgeIndex`] auf, die
//! in `docs/design/build-history.md` Abschnitt C (Stand dieses Knotens) noch
//! nicht aufgeführt sind — dort ist `EdgeIndex` nur als `Default`-fähiger
//! Typ mit privaten Feldern gezeigt, ohne Konstruktor mit konkreten Kanten
//! und ohne Abfragemethoden:
//!
//! - `EdgeIndex::from_edges(superseded_by, contradicts)` — Konstruktor aus
//!   zwei Iteratoren von `(ChunkDigest, ChunkDigest)`-Paaren. Bei
//!   `superseded_by` ist das erste Element die ältere, das zweite die neuere
//!   Fassung. `contradicts`-Paare sind symmetrisch (Reihenfolge egal).
//! - `EdgeIndex::superseded_by(&self, chunk: ChunkDigest) -> Option<ChunkDigest>`
//!   — liefert die neuere Fassung, falls `chunk` bekanntermaßen ersetzt
//!   wurde.
//! - `EdgeIndex::contradicts(&self, a: ChunkDigest, b: ChunkDigest) -> bool`
//!   — symmetrische Abfrage, ob zwei Chunks einander bekanntermaßen
//!   widersprechen.
//!
//! Ohne diese drei Methoden ist `EdgeIndex` von außen weder befüllbar noch
//! abfragbar, und `collapse` könnte seine namensgebende Aufgabe nicht
//! erfüllen. Die genauen Signaturen und die Begründung stehen im
//! Abschlussbericht des Knotens AW0-09.

use std::collections::{HashMap, HashSet};

use harw_lens_types::{ChunkDigest, CollapsePolicy, EdgeIndex, Ranked, SourceRef};

/// Gruppierungsschlüssel für Duplikaterkennung nach [`CollapsePolicy`].
///
/// `ByteSpan` selbst leitet kein `Hash` ab; `start`/`end` werden deshalb als
/// eigene `usize`-Felder geführt statt den Typ als Ganzes zu verwenden.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GroupKey {
    Digest(ChunkDigest),
    SourceAndSpan(SourceRef, usize, usize),
}

/// Berechnet den Gruppierungsschlüssel eines Kandidaten für `policy`.
fn group_key(policy: CollapsePolicy, ranked: &Ranked) -> GroupKey {
    match policy {
        CollapsePolicy::ByDigest => GroupKey::Digest(ranked.chunk.digest),
        CollapsePolicy::BySourceAndSpan => GroupKey::SourceAndSpan(
            ranked.chunk.source.clone(),
            ranked.chunk.span.start,
            ranked.chunk.span.end,
        ),
    }
}

/// Wählt aus einer nichtleeren Indexmenge den Repräsentanten: höchster
/// `score`, bei Gleichstand der kleinste [`ChunkDigest`] — ein stabiler
/// Tiebreak über ein Merkmal des Dokuments, nie über die Fundreihenfolge.
fn pick_representative(indices: &[usize], candidates: &[Ranked]) -> usize {
    debug_assert!(
        !indices.is_empty(),
        "pick_representative erwartet >= 1 Index"
    );
    let mut sorted = indices.to_vec();
    sorted.sort_by(|&a, &b| {
        candidates[b]
            .score
            .total_cmp(&candidates[a].score)
            .then_with(|| candidates[a].chunk.digest.cmp(&candidates[b].chunk.digest))
    });
    sorted[0]
}

/// Lässt Duplikate zusammenfallen. Idempotent.
///
/// # Description
/// Gruppiert `candidates` nach `policy`: identischer [`ChunkDigest`] bei
/// [`CollapsePolicy::ByDigest`], identische Quelle plus Bytebereich bei
/// [`CollapsePolicy::BySourceAndSpan`]. Innerhalb einer Gruppe:
///
/// 1. Ein Kandidat, der mit **irgendeinem** anderen Gruppenmitglied über eine
///    `Contradicts`-Kante verbunden ist, wird nie kollabiert — er erscheint
///    unverändert im Ergebnis.
/// 2. Unter den verbleibenden, nicht widersprechenden Mitgliedern wird
///    entlang der `SupersededBy`-Kanten die jeweils neuere Fassung ermittelt;
///    ältere Fassungen entfallen.
/// 3. Bleiben nach Schritt 2 mehrere Mitglieder ohne Kantenbezug zueinander
///    übrig (reine Dubletten ohne Versionsinformation), überlebt ein
///    deterministisch gewählter Repräsentant (höchster `score`, bei
///    Gleichstand kleinster [`ChunkDigest`]).
///
/// **Idempotent:** ein zweiter Aufruf auf dem Ergebnis des ersten ändert
/// nichts mehr — durch `Contradicts` geschützte Kandidaten bleiben
/// geschützt, bereits kollabierte Gruppen bestehen nur noch aus ihrem
/// Überlebenden und sind dadurch zu klein, um erneut zu kollabieren.
///
/// # Arguments
/// - `candidates` (`&[Ranked]`): die zu entdoppelnde Kandidatenmenge.
/// - `policy` (`CollapsePolicy`): wonach Duplikate erkannt werden.
/// - `edges` (`&EdgeIndex`): bekannte `SupersededBy`- und `Contradicts`-
///   Beziehungen zwischen Chunks.
///
/// # Returns
/// Die Eingabe abzüglich der kollabierten Dubletten, in der Reihenfolge von
/// `candidates`.
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_lens_rank::collapse;
/// use harw_lens_types::{CollapsePolicy, EdgeIndex};
///
/// let result = collapse(&[], CollapsePolicy::ByDigest, &EdgeIndex::default());
/// assert!(result.is_empty());
/// ```
#[must_use]
pub fn collapse(candidates: &[Ranked], policy: CollapsePolicy, edges: &EdgeIndex) -> Vec<Ranked> {
    let mut groups: HashMap<GroupKey, Vec<usize>> = HashMap::new();
    for (idx, ranked) in candidates.iter().enumerate() {
        groups
            .entry(group_key(policy, ranked))
            .or_default()
            .push(idx);
    }

    let mut keep = vec![true; candidates.len()];

    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }

        // Schritt 1: Kandidaten, die mit einem Gruppenmitglied widersprechen,
        // sind für den Rest dieser Gruppe unantastbar.
        let protected: HashSet<usize> = indices
            .iter()
            .copied()
            .filter(|&i| {
                indices.iter().copied().any(|j| {
                    j != i
                        && edges.contradicts(candidates[i].chunk.digest, candidates[j].chunk.digest)
                })
            })
            .collect();

        let collapsible: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|i| !protected.contains(i))
            .collect();

        if collapsible.len() < 2 {
            continue;
        }

        let collapsible_digests: HashSet<ChunkDigest> = collapsible
            .iter()
            .map(|&idx| candidates[idx].chunk.digest)
            .collect();

        // Schritt 2: wer innerhalb der kollabierbaren Teilmenge von einem
        // anderen Mitglied ersetzt wurde, entfällt.
        let survivors: Vec<usize> = collapsible
            .iter()
            .copied()
            .filter(
                |&idx| match edges.superseded_by(candidates[idx].chunk.digest) {
                    Some(newer) => !collapsible_digests.contains(&newer),
                    None => true,
                },
            )
            .collect();

        // Leer nur bei einem Zyklus in den Kantendaten (sollte nicht
        // vorkommen) — Sicherheitsnetz gegen vollständigen Informations-
        // verlust, statt die ganze Gruppe zu verwerfen.
        let winner = if survivors.is_empty() {
            pick_representative(&collapsible, candidates)
        } else {
            pick_representative(&survivors, candidates)
        };

        for &idx in &collapsible {
            if idx != winner {
                keep[idx] = false;
            }
        }
    }

    candidates
        .iter()
        .enumerate()
        .filter(|(idx, _)| keep[*idx])
        .map(|(_, ranked)| ranked.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Chunk, SourceRef};
    use harw_types::ContentDigest;

    fn chunk_at(text: &str, path: &str, start: usize, end: usize) -> Chunk {
        Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: path.to_string(),
            },
            span: ByteSpan { start, end },
            text: text.to_string(),
        }
    }

    fn ranked_at(text: &str, path: &str, start: usize, end: usize, score: f32) -> Ranked {
        Ranked {
            chunk: chunk_at(text, path, start, end),
            score,
        }
    }

    #[test]
    fn test_collapse_empty_input_returns_empty() {
        let result = collapse(&[], CollapsePolicy::ByDigest, &EdgeIndex::default());
        assert!(result.is_empty());
    }

    #[test]
    fn test_collapse_exact_duplicates_by_digest_keep_one() {
        let a = ranked_at("same text", "f.md", 0, 9, 0.5);
        let b = ranked_at("same text", "f.md", 100, 109, 0.9);
        let candidates = vec![a, b];

        let result = collapse(&candidates, CollapsePolicy::ByDigest, &EdgeIndex::default());
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_collapse_superseded_by_edge_keeps_newer_only() {
        let older = ranked_at("draft v1", "notes.md", 0, 8, 0.9);
        let newer = ranked_at("draft v2", "notes.md", 0, 8, 0.1);
        let candidates = vec![older.clone(), newer.clone()];

        let edges = EdgeIndex::from_edges(
            [(older.chunk.digest, newer.chunk.digest)],
            std::iter::empty(),
        );

        let result = collapse(&candidates, CollapsePolicy::BySourceAndSpan, &edges);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chunk.digest, newer.chunk.digest);
    }

    #[test]
    fn test_collapse_contradicts_edge_keeps_both() {
        // Der wichtigste Test der Crate: ein Widerspruch darf nie entdoppelt
        // werden, selbst wenn beide Aussagen an derselben Stelle stehen.
        let claim_a = ranked_at("the build passes", "status.md", 0, 17, 0.9);
        let claim_b = ranked_at("the build fails", "status.md", 0, 17, 0.5);
        let candidates = vec![claim_a.clone(), claim_b.clone()];

        let edges = EdgeIndex::from_edges(
            std::iter::empty(),
            [(claim_a.chunk.digest, claim_b.chunk.digest)],
        );

        let result = collapse(&candidates, CollapsePolicy::BySourceAndSpan, &edges);

        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_collapse_is_idempotent() {
        let older = ranked_at("draft v1", "notes.md", 0, 8, 0.9);
        let newer = ranked_at("draft v2", "notes.md", 0, 8, 0.1);
        let unrelated = ranked_at("something else", "other.md", 0, 15, 0.4);
        let candidates = vec![older.clone(), newer.clone(), unrelated];

        let edges = EdgeIndex::from_edges(
            [(older.chunk.digest, newer.chunk.digest)],
            std::iter::empty(),
        );

        let once = collapse(&candidates, CollapsePolicy::BySourceAndSpan, &edges);
        let twice = collapse(&once, CollapsePolicy::BySourceAndSpan, &edges);

        assert_eq!(once, twice);
    }

    #[test]
    fn test_collapse_contradicts_is_idempotent_too() {
        let claim_a = ranked_at("the build passes", "status.md", 0, 17, 0.9);
        let claim_b = ranked_at("the build fails", "status.md", 0, 17, 0.5);
        let candidates = vec![claim_a.clone(), claim_b.clone()];

        let edges = EdgeIndex::from_edges(
            std::iter::empty(),
            [(claim_a.chunk.digest, claim_b.chunk.digest)],
        );

        let once = collapse(&candidates, CollapsePolicy::BySourceAndSpan, &edges);
        let twice = collapse(&once, CollapsePolicy::BySourceAndSpan, &edges);

        assert_eq!(once, twice);
        assert_eq!(once.len(), 2);
    }
}

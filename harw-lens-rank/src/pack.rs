//! Budgetfüllung.
//!
//! # Verantwortungsbereich
//! Füllt ein Kostenbudget mit den besten Kandidaten in Rangreihenfolge.
//!
//! # Verhältnis zur Kontextmontage (Korrektur K55)
//! `harw-core::context_budget` löst dasselbe Grundproblem für die
//! Kontextmontage, ruft dabei aber ausdrücklich **nicht** diese Funktion auf
//! — siehe dort, Abschnitt „Warum `harw_lens_rank::pack` nicht wiederverwendet
//! wird", für die vollständige Begründung. Die beiden Packprobleme haben
//! verschiedene Eingabeformen und verschiedene Fehlermodelle: `pack` schätzt
//! Kosten aus dem Text neu (ein `Chunk` trägt keinen vorberechneten Wert),
//! die Kontextmontage benutzt das bereits berechnete `Fragment::cost`; `pack`
//! kennt ein **flaches** Budget, während `ContextBudgetSpec` Budgets **je
//! Sektion** trägt; und `pack` kennt keinen `must_include`-Pfad, der hart
//! scheitert. Eine frühere Fassung dieses Knotens verlangte, dass beide
//! Seiten dieselbe Funktion aufrufen — das hätte die reichere Form der
//! Kontextmontage verarmt und wurde zurückgenommen. Das heute geltende
//! Kriterium: **beide arbeiten auf denselben Typen** (die Typverlagerung nach
//! `harw-lens-types` bleibt in Kraft), **und keine der beiden schätzt eine
//! Größe neu, die die andere schon kennt.**
//!
//! # Nebenläufigkeit
//! Die Funktion selbst ist zustandslos. Sie nimmt einen `&dyn
//! CostEstimator` entgegen, dessen Implementierungen laut
//! [`harw_lens_types::CostEstimator`] `Send + Sync` sein müssen — `pack` ist
//! damit sicher aus mehreren Threads parallel aufrufbar.
//!
//! # Examples
//! ```rust
//! use harw_lens_rank::pack;
//! use harw_lens_types::{BudgetSpec, BytesOverFour};
//!
//! let packed = pack(&[], &BytesOverFour, &BudgetSpec { total: 100 });
//! assert_eq!(packed.spent.0, 0);
//! assert_eq!(packed.dropped, 0);
//! ```

use harw_lens_types::{BudgetSpec, CostEstimate, CostEstimator, Packed, Ranked};

/// Füllt ein Budget mit den besten Kandidaten.
///
/// # Description
/// Geht `candidates` in der übergebenen Rangreihenfolge durch. Ein Kandidat
/// wird aufgenommen, wenn seine über `cost.estimate` geschätzten Kosten in
/// das jeweils noch freie Restbudget passen; andernfalls wird er
/// übersprungen und `dropped` erhöht. **Die Funktion bricht dabei nicht ab:**
/// ein einzelner Kandidat, der allein schon über dem Gesamtbudget liegt,
/// würde sonst — bei einem Abbruch beim ersten Fehlschlag — jeden
/// nachfolgenden, unter Umständen viel kleineren Kandidaten mitverhindern.
/// Stattdessen prüft `pack` jeden verbleibenden Kandidaten unabhängig gegen
/// das aktuelle Restbudget und macht mit der Liste weiter.
///
/// # Arguments
/// - `candidates` (`&[Ranked]`): Kandidaten in Rangreihenfolge; frühere
///   Positionen haben Vorrang bei der Aufnahme.
/// - `cost` (`&dyn CostEstimator`): schätzt die Kosten eines Kandidatentexts.
/// - `budget` (`&BudgetSpec`): das verfügbare Gesamtbudget.
///
/// # Returns
/// [`Packed`] mit den aufgenommenen Kandidaten (`selected`), der Summe ihrer
/// Kosten (`spent`, überschreitet `budget.total` nie) und der Anzahl der
/// nicht aufgenommenen Kandidaten (`dropped`).
///
/// # Panics
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_lens_rank::pack;
/// use harw_lens_types::{
///     BudgetSpec, BytesOverFour, ByteSpan, Chunk, ChunkDigest, Ranked, SourceRef,
/// };
/// use harw_types::ContentDigest;
///
/// let text = "ein kurzer chunk";
/// let chunk = Chunk {
///     digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
///     source: SourceRef::Artifact { id: "a".to_string() },
///     span: ByteSpan { start: 0, end: text.len() },
///     text: text.to_string(),
/// };
/// let candidates = vec![Ranked { chunk, score: 1.0 }];
///
/// let packed = pack(&candidates, &BytesOverFour, &BudgetSpec { total: 100 });
/// assert_eq!(packed.selected.len(), 1);
/// assert_eq!(packed.dropped, 0);
/// ```
#[must_use]
pub fn pack(candidates: &[Ranked], cost: &dyn CostEstimator, budget: &BudgetSpec) -> Packed {
    let mut selected = Vec::new();
    let mut spent: u32 = 0;
    let mut dropped: usize = 0;

    for ranked in candidates {
        let estimate = cost.estimate(&ranked.chunk.text);
        let remaining = budget.total.saturating_sub(spent);

        if estimate.0 <= remaining {
            spent = spent.saturating_add(estimate.0);
            selected.push(ranked.clone());
        } else {
            dropped += 1;
        }
    }

    Packed {
        selected,
        spent: CostEstimate(spent),
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
    use harw_types::ContentDigest;

    struct FixedCost(u32);

    impl CostEstimator for FixedCost {
        fn estimate(&self, _text: &str) -> CostEstimate {
            CostEstimate(self.0)
        }
    }

    fn ranked_with(text: &str, score: f32) -> Ranked {
        Ranked {
            chunk: Chunk {
                digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
                source: SourceRef::Artifact {
                    id: "a".to_string(),
                },
                span: ByteSpan {
                    start: 0,
                    end: text.len(),
                },
                text: text.to_string(),
            },
            score,
        }
    }

    #[test]
    fn test_pack_empty_candidates_returns_empty_packed() {
        let packed = pack(&[], &FixedCost(10), &BudgetSpec { total: 100 });
        assert!(packed.selected.is_empty());
        assert_eq!(packed.spent.0, 0);
        assert_eq!(packed.dropped, 0);
    }

    #[test]
    fn test_pack_never_exceeds_budget() {
        let candidates = vec![
            ranked_with("a", 1.0),
            ranked_with("b", 0.9),
            ranked_with("c", 0.8),
        ];
        let packed = pack(&candidates, &FixedCost(40), &BudgetSpec { total: 100 });

        assert!(packed.spent.0 <= 100);
        assert_eq!(packed.selected.len(), 2);
        assert_eq!(packed.spent.0, 80);
        assert_eq!(packed.dropped, 1);
    }

    #[test]
    fn test_pack_oversized_candidate_is_skipped_not_aborted() {
        struct VariableCost;
        impl CostEstimator for VariableCost {
            fn estimate(&self, text: &str) -> CostEstimate {
                if text == "huge" {
                    CostEstimate(1000)
                } else {
                    CostEstimate(10)
                }
            }
        }

        let candidates = vec![ranked_with("huge", 1.0), ranked_with("small", 0.5)];
        let packed = pack(&candidates, &VariableCost, &BudgetSpec { total: 50 });

        assert_eq!(packed.selected.len(), 1);
        assert_eq!(packed.selected[0].chunk.text, "small");
        assert_eq!(packed.dropped, 1);
        assert_eq!(packed.spent.0, 10);
    }

    #[test]
    fn test_pack_dropped_counts_every_rejected_candidate() {
        let candidates = vec![
            ranked_with("a", 1.0),
            ranked_with("b", 0.9),
            ranked_with("c", 0.8),
            ranked_with("d", 0.7),
        ];
        let packed = pack(&candidates, &FixedCost(30), &BudgetSpec { total: 65 });

        // 30 + 30 = 60 passt, das dritte (30) nicht mehr (60+30=90 > 65),
        // das vierte auch nicht.
        assert_eq!(packed.selected.len(), 2);
        assert_eq!(packed.dropped, 2);
        assert_eq!(packed.spent.0, 60);
    }
}

//! Deterministisches Kürzen beim Laden: [`retain_greatest`].
//!
//! # Zweck
//! [`crate::Dispatcher::restore`] übernimmt die Grenzen der **aktiven**
//! Konfiguration, nicht die des Schnappschusses. Trägt eine Tabelle mehr
//! Einträge, als die Grenze erlaubt (kleinere Grenze nach einem
//! Konfigurationswechsel oder ein manipulierter Zustand), wird sie vor dem
//! Bau des Dispatchers auf **genau** die Grenze gekürzt. Es bleiben die
//! Einträge mit dem größten Paar `(Rang, Schlüssel)`: den Rang legt die
//! aufrufende Tabelle fest (meist die Ordnung ihrer Verdrängung zur
//! Laufzeit, etwa „zuletzt benutzt"; wo die Laufzeit lebende Einträge nie
//! verdrängt, eine eigene fail-closed-Regel), der Schlüssel entscheidet
//! Gleichstände. Weil Schlüssel eindeutig sind, ist das Ergebnis unabhängig
//! von der Eingabereihenfolge.
//!
//! # Aufwand
//! Ein Durchlauf über die Tabelle mit einem Heap aus höchstens
//! `min(Grenze, Überschuss)` Paaren aus Rang und **Verweis** auf den
//! Schlüssel, danach ein `retain`. Zeit `O(n log k)`; der zusätzliche
//! Speicher ist höchstens so groß wie die Grenze der aktiven Konfiguration,
//! nie eine Kopie der Tabelle. Geklont wird nur der eine Grenzschlüssel.
//! Liegt die Tabelle innerhalb der Grenze, geschieht nichts. `n` selbst
//! begrenzt nicht diese Funktion, sondern die Vorabprüfung von
//! [`crate::Dispatcher::restore`]: eine Tabelle über ihrem absoluten
//! Höchstwert (`crate::bounds`) erreicht sie nie.
//!
//! # Nebenläufigkeit und Fehler
//! Zustandslos; keine Fehler, keine Panik.
//!
//! # Examples
//! ```ignore
//! let mut by_age = std::collections::BTreeMap::from([(1_u64, 30_i64), (2, 10), (3, 20)]);
//! // Die zwei zuletzt benutzten bleiben.
//! assert_eq!(retain_greatest(&mut by_age, 2, |last| *last), 1);
//! assert_eq!(by_age.keys().copied().collect::<Vec<_>>(), [1, 3]);
//! ```

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};

// Kürzt `map` auf höchstens `limit` Einträge; es bleiben die mit dem größten
// `(rank(Wert), Schlüssel)` (siehe Moduldoku). Liefert die Zahl der
// entfernten Einträge (`0`, wenn die Tabelle schon passt).
pub(crate) fn retain_greatest<K, V, R, F>(map: &mut BTreeMap<K, V>, limit: usize, rank: F) -> usize
where
    K: Ord + Clone,
    R: Ord,
    F: Fn(&V) -> R,
{
    let surplus = map.len().saturating_sub(limit);
    if surplus == 0 {
        return 0;
    }
    // Die Grenze als `(Rang, Schlüssel, bleibt sie selbst)`. Gesucht wird auf
    // der kleineren Seite, damit der Heap nie größer als die Grenze wird.
    let cutoff = if limit <= surplus {
        // Das kleinste behaltene Paar: das `limit`-t größte.
        kth_smallest(
            map.iter().map(|(key, value)| Reverse((rank(value), key))),
            limit,
        )
        .map(|Reverse((cut_rank, cut_key))| (cut_rank, cut_key.clone(), true))
    } else {
        // Das größte verworfene Paar: das `surplus`-t kleinste.
        kth_smallest(map.iter().map(|(key, value)| (rank(value), key)), surplus)
            .map(|(cut_rank, cut_key)| (cut_rank, cut_key.clone(), false))
    };
    match cutoff {
        Some((cut_rank, cut_key, keep_cutoff)) => map.retain(|key, value| {
            match rank(&*value).cmp(&cut_rank).then_with(|| key.cmp(&cut_key)) {
                Ordering::Greater => true,
                Ordering::Equal => keep_cutoff,
                Ordering::Less => false,
            }
        }),
        // Nur bei `limit == 0` (eine Grenze ist nie kleiner als 1): nichts
        // bleibt.
        None => map.clear(),
    }
    surplus
}

// Das `k`-t kleinste Element (`k` ≥ 1) mit einem Max-Heap aus höchstens `k`
// Elementen; `None`, wenn es weniger als `k` Elemente gibt oder `k == 0`.
fn kth_smallest<T: Ord>(items: impl Iterator<Item = T>, k: usize) -> Option<T> {
    if k == 0 {
        return None;
    }
    let mut smallest: BinaryHeap<T> = BinaryHeap::new();
    for item in items {
        if smallest.len() < k {
            smallest.push(item);
        } else if let Some(mut largest) = smallest.peek_mut()
            && item < *largest
        {
            *largest = item;
        }
    }
    if smallest.len() < k {
        return None;
    }
    smallest.pop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    // Kennungen 1..=n mit Rang `rank_of(kennung)`.
    fn table(n: u64, rank_of: impl Fn(u64) -> i64) -> BTreeMap<u64, i64> {
        (1..=n).map(|key| (key, rank_of(key))).collect()
    }

    // Die Schlüssel in aufsteigender Reihenfolge.
    fn keys(map: &BTreeMap<u64, i64>) -> Vec<u64> {
        map.keys().copied().collect()
    }

    #[test]
    fn test_within_the_limit_nothing_changes() -> TestResult {
        let mut map = table(3, |key| {
            10_i64.saturating_sub(i64::try_from(key).unwrap_or(0))
        });
        let before = map.clone();
        assert_eq!(retain_greatest(&mut map, 3, |rank| *rank), 0);
        assert_eq!(retain_greatest(&mut map, 99, |rank| *rank), 0);
        assert_eq!(map, before);
        Ok(())
    }

    #[test]
    fn test_keeps_exactly_the_greatest_ranks_on_both_search_sides() -> TestResult {
        // Rang absteigend zur Kennung: die kleinsten Kennungen bleiben.
        let rank_of = |key: u64| 1_000_i64.saturating_sub(i64::try_from(key).unwrap_or(0));
        // Grenze kleiner als der Überschuss (Heap der Behaltenen) …
        let mut few = table(10, rank_of);
        assert_eq!(retain_greatest(&mut few, 3, |rank| *rank), 7);
        assert_eq!(keys(&few), [1, 2, 3]);
        // … und größer (Heap der Verworfenen).
        let mut many = table(10, rank_of);
        assert_eq!(retain_greatest(&mut many, 8, |rank| *rank), 2);
        assert_eq!(keys(&many), [1, 2, 3, 4, 5, 6, 7, 8]);
        // Genau die Hälfte.
        let mut half = table(10, rank_of);
        assert_eq!(retain_greatest(&mut half, 5, |rank| *rank), 5);
        assert_eq!(keys(&half), [1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn test_ties_are_broken_by_the_greater_key() -> TestResult {
        // Alle gleich alt: die größten Schlüssel bleiben, auf beiden Seiten.
        let mut few = table(6, |_| 7);
        assert_eq!(retain_greatest(&mut few, 2, |rank| *rank), 4);
        assert_eq!(keys(&few), [5, 6]);
        let mut many = table(6, |_| 7);
        assert_eq!(retain_greatest(&mut many, 5, |rank| *rank), 1);
        assert_eq!(keys(&many), [2, 3, 4, 5, 6]);
        // Gemischt: Rang vor Schlüssel.
        let mut mixed = BTreeMap::from([(1_u64, 5_i64), (2, 9), (3, 5), (4, 1), (5, 9)]);
        assert_eq!(retain_greatest(&mut mixed, 3, |rank| *rank), 2);
        assert_eq!(keys(&mixed), [2, 3, 5]);
        Ok(())
    }

    #[test]
    fn test_result_does_not_depend_on_the_rank_layout() -> TestResult {
        // Deterministische Pseudo-Zufallsränge (kein proptest im Workspace):
        // das Ergebnis gleicht dem einer vollständigen Sortierung.
        let mut state = 0x2545_f491_u32;
        let mut map = BTreeMap::new();
        for key in 0..500_u64 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let rank = i64::from(state.checked_shr(24).unwrap_or(0));
            map.insert(key, rank);
        }
        for limit in [1_usize, 7, 249, 250, 251, 499, 500] {
            let mut expected: Vec<(i64, u64)> =
                map.iter().map(|(key, rank)| (*rank, *key)).collect();
            expected.sort_unstable();
            let skip = expected.len().saturating_sub(limit);
            let mut expected: Vec<u64> = expected
                .into_iter()
                .skip(skip)
                .map(|(_, key)| key)
                .collect();
            expected.sort_unstable();
            let mut pruned = map.clone();
            assert_eq!(retain_greatest(&mut pruned, limit, |rank| *rank), skip);
            assert_eq!(pruned.len(), limit.min(map.len()));
            assert_eq!(keys(&pruned), expected, "limit {limit}");
        }
        Ok(())
    }

    #[test]
    fn test_limit_zero_clears_the_table() -> TestResult {
        let mut map = table(4, |_| 1);
        assert_eq!(retain_greatest(&mut map, 0, |rank| *rank), 4);
        assert!(map.is_empty());
        Ok(())
    }
}

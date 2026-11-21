//! Beschränkte Prioritäts-Warteschlange für Triage-Aufträge.
//!
//! # Zweck
//! [`PriorityQueue`] hält höchstens `capacity` Einträge und gibt immer den
//! dringendsten zuerst heraus. Ist sie voll, verdrängt ein neuer Eintrag
//! den am wenigsten dringenden; ist der neue selbst der am wenigsten
//! dringende, wird er abgewiesen. Beides zählt als Verdrängung
//! ([`PriorityQueue::evictions`]).
//!
//! # Ordnung ([`TriagePriority::cmp_urgency`])
//! 1. Neuheit (erstes Auftreten eines `dedup_key`) vor bekannt;
//! 2. Härte: `Observed` vor `Correlated` vor `Inferred`
//!    ([`crate::Hardness::strength`], nicht die Ordnung aus
//!    `harw-dod-signals`);
//! 3. Schwere: höher vor niedriger;
//! 4. Ereigniszeit: älter vor jünger;
//! 5. bei Gleichstand: früher eingereiht zuerst (FIFO).
//!
//! # Aufwand
//! Linearer Durchlauf je Entnahme und Verdrängung. Die Kapazität ist durch
//! [`crate::DispatchConfig::validate`] begrenzt (Vorgabe 1024), deshalb
//! bleibt das billig und der Zustand ist ein schlichter, serialisierbarer
//! Vektor. Ein geladener Zustand kann mehr Einträge tragen als die
//! Kapazität (etwa nach einer Verkleinerung; [`crate::Dispatcher::restore`]
//! lässt höchstens [`crate::QUEUE_CAPACITY_LIMIT`] wartende plus
//! [`crate::MAX_PARALLEL_LIMIT`] zurückgestellte zu); er wird beim
//! Laden mit einer Sortierung auf die Kapazität gekürzt, nicht Eintrag für
//! Eintrag.
//!
//! # Nebenläufigkeit und Fehler
//! `&mut self`, keine inneren Sperren; keine Fehlerfälle.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{Hardness, PriorityQueue, PushOutcome, Severity, TriagePriority};
//!
//! let mut queue = PriorityQueue::new(2);
//! let known = TriagePriority { novel: false, hardness: Hardness::Observed, severity: Severity::High, observed_at_unix: 10 };
//! let novel = TriagePriority { novel: true, hardness: Hardness::Inferred, severity: Severity::Low, observed_at_unix: 20 };
//! assert!(matches!(queue.push(known, "known"), PushOutcome::Queued));
//! assert!(matches!(queue.push(novel, "novel"), PushOutcome::Queued));
//! assert_eq!(queue.pop(), Some("novel"));
//! assert_eq!(queue.pop(), Some("known"));
//! assert_eq!(queue.pop(), None);
//! ```

use std::cmp::Ordering;

use serde::{Deserialize, Deserializer, Serialize};

use crate::bounds::QUEUE_CAPACITY_LIMIT;
use crate::finding::{Hardness, Severity};

/// Dringlichkeit eines Eintrags.
///
/// # Examples
/// ```
/// use std::cmp::Ordering;
/// use harw_dod_dispatch::{Hardness, Severity, TriagePriority};
///
/// let older = TriagePriority { novel: false, hardness: Hardness::Observed, severity: Severity::High, observed_at_unix: 1 };
/// let newer = TriagePriority { observed_at_unix: 2, ..older };
/// assert_eq!(older.cmp_urgency(&newer), Ordering::Greater);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriagePriority {
    /// Erstes Auftreten des `dedup_key`.
    pub novel: bool,
    /// Härte des Belegs.
    pub hardness: Hardness,
    /// Schwere.
    pub severity: Severity,
    /// Ereigniszeit in Unix-Sekunden (älter ist dringender).
    pub observed_at_unix: i64,
}

impl TriagePriority {
    /// Vergleicht die Dringlichkeit (siehe Moduldoku).
    ///
    /// # Arguments
    /// - `other` (`&TriagePriority`): Vergleichswert.
    ///
    /// # Returns
    /// `Ordering::Greater`, wenn `self` dringender ist.
    #[must_use]
    pub fn cmp_urgency(&self, other: &Self) -> Ordering {
        self.novel
            .cmp(&other.novel)
            .then(self.hardness.strength().cmp(&other.hardness.strength()))
            .then(self.severity.cmp(&other.severity))
            .then(other.observed_at_unix.cmp(&self.observed_at_unix))
    }
}

/// Ergebnis von [`PriorityQueue::push`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome<T> {
    /// Eingereiht, nichts verdrängt.
    Queued,
    /// Eingereiht; dieser ältere, weniger dringende Eintrag wurde verdrängt.
    Evicted(T),
    /// Die Warteschlange ist voll und der neue Eintrag ist der am wenigsten
    /// dringende; er kommt unverändert zurück.
    Rejected(T),
}

// Ein Eintrag mit Einreihungsnummer für die FIFO-Regel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QueueEntry<T> {
    // Dringlichkeit beim Einreihen.
    pub(crate) priority: TriagePriority,
    // Laufende Nummer; kleiner ist früher.
    pub(crate) seq: u64,
    // Nutzlast.
    pub(crate) item: T,
}

// Dringlichkeit inklusive FIFO-Gleichstandsregel.
fn entry_cmp<T>(a: &QueueEntry<T>, b: &QueueEntry<T>) -> Ordering {
    a.priority.cmp_urgency(&b.priority).then(b.seq.cmp(&a.seq))
}

// Entfernt das Element an `position`, falls es existiert.
fn take_at<T>(entries: &mut Vec<T>, position: usize) -> Option<T> {
    if position < entries.len() {
        Some(entries.swap_remove(position))
    } else {
        None
    }
}

/// Persistierbarer Inhalt einer [`PriorityQueue`] (ohne Kapazität).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QueueState<T> {
    // Nächste Einreihungsnummer.
    pub(crate) next_seq: u64,
    // Bisherige Verdrängungen.
    pub(crate) evictions: u64,
    // Einträge in beliebiger Reihenfolge; gelesen mit höchstens
    // `QUEUE_CAPACITY_LIMIT` (`read_queue_entries`). Mit `deserialize_with`
    // leitet serde keine Schranke für `T` ab, daher die explizite.
    #[serde(
        deserialize_with = "read_queue_entries",
        bound(deserialize = "T: Deserialize<'de>")
    )]
    pub(crate) entries: Vec<QueueEntry<T>>,
}

// Liest die wartenden Einträge mit höchstens `QUEUE_CAPACITY_LIMIT` (mehr
// kann keine gültige Konfiguration schreiben); der Eintrag darüber bricht
// das Lesen ab, bevor er gelesen wird.
fn read_queue_entries<'de, T, D>(deserializer: D) -> Result<Vec<QueueEntry<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    crate::serde_map::deserialize_vec_at_most(deserializer, QUEUE_CAPACITY_LIMIT)
}

impl<T> QueueState<T> {
    // Nächste freie Einreihungsnummer: hinter `next_seq` und hinter jeder
    // vorhandenen (ein Zustand kann eine zu kleine `next_seq` tragen).
    fn resume_seq(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.seq)
            .max()
            .map_or(self.next_seq, |seq| {
                self.next_seq.max(seq.saturating_add(1))
            })
    }

    // Hängt `items` mit neuen Einreihungsnummern hinten an, als würden sie
    // in dieser Reihenfolge nach allen vorhandenen Einträgen eingereiht
    // (bei gleicher Dringlichkeit kommen sie also danach). `priority`
    // liefert die Dringlichkeit; der erste Fehler bricht ab. Gekürzt wird
    // erst in `PriorityQueue::import`, in einem Schritt für alle Einträge.
    pub(crate) fn append<I, F, E>(&mut self, items: I, mut priority: F) -> Result<(), E>
    where
        I: IntoIterator<Item = T>,
        F: FnMut(&T) -> Result<TriagePriority, E>,
    {
        let mut seq = self.resume_seq();
        for item in items {
            let entry = QueueEntry {
                priority: priority(&item)?,
                seq,
                item,
            };
            self.entries.push(entry);
            seq = seq.saturating_add(1);
        }
        self.next_seq = seq;
        Ok(())
    }
}

/// Beschränkte Prioritäts-Warteschlange (siehe Moduldoku).
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{Hardness, PriorityQueue, PushOutcome, Severity, TriagePriority};
///
/// let low = TriagePriority { novel: false, hardness: Hardness::Inferred, severity: Severity::Low, observed_at_unix: 0 };
/// let high = TriagePriority { severity: Severity::High, ..low };
/// let mut queue = PriorityQueue::new(1);
/// assert!(matches!(queue.push(low, 1), PushOutcome::Queued));
/// assert!(matches!(queue.push(high, 2), PushOutcome::Evicted(1)));
/// assert!(matches!(queue.push(low, 3), PushOutcome::Rejected(3)));
/// assert_eq!(queue.evictions(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityQueue<T> {
    // Höchstzahl der Einträge.
    capacity: usize,
    // Nächste Einreihungsnummer.
    next_seq: u64,
    // Verdrängte oder abgewiesene Einträge.
    evictions: u64,
    // Einträge in beliebiger Reihenfolge.
    entries: Vec<QueueEntry<T>>,
}

impl<T> PriorityQueue<T> {
    /// Leere Warteschlange.
    ///
    /// # Arguments
    /// - `capacity` (`usize`): Höchstzahl der Einträge; `0` weist alles ab.
    ///
    /// # Returns
    /// Die leere Warteschlange.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_seq: 0,
            evictions: 0,
            entries: Vec::new(),
        }
    }

    /// Höchstzahl der Einträge.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Anzahl der Einträge.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Ob die Warteschlange leer ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bisher verdrängte oder abgewiesene Einträge.
    #[must_use]
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Reiht einen Eintrag ein.
    ///
    /// # Arguments
    /// - `priority` (`TriagePriority`): Dringlichkeit des Eintrags.
    /// - `item` (`T`): Nutzlast.
    ///
    /// # Returns
    /// [`PushOutcome::Queued`], [`PushOutcome::Evicted`] mit dem verdrängten
    /// oder [`PushOutcome::Rejected`] mit dem eigenen Eintrag.
    pub fn push(&mut self, priority: TriagePriority, item: T) -> PushOutcome<T> {
        let entry = QueueEntry {
            priority,
            seq: self.next_seq,
            item,
        };
        self.next_seq = self.next_seq.saturating_add(1);
        if self.entries.len() < self.capacity {
            self.entries.push(entry);
            return PushOutcome::Queued;
        }
        self.evictions = self.evictions.saturating_add(1);
        let weakest = self.position_of(Ordering::Less);
        let replace = weakest.filter(|position| {
            self.entries
                .get(*position)
                .is_some_and(|current| entry_cmp(&entry, current) == Ordering::Greater)
        });
        match replace.and_then(|position| take_at(&mut self.entries, position)) {
            Some(evicted) => {
                self.entries.push(entry);
                PushOutcome::Evicted(evicted.item)
            }
            None => PushOutcome::Rejected(entry.item),
        }
    }

    /// Entnimmt den dringendsten Eintrag.
    ///
    /// # Returns
    /// `None`, wenn die Warteschlange leer ist.
    pub fn pop(&mut self) -> Option<T> {
        let position = self.position_of(Ordering::Greater)?;
        take_at(&mut self.entries, position).map(|entry| entry.item)
    }

    /// Entnimmt alle Einträge, auf die `predicate` zutrifft.
    ///
    /// # Arguments
    /// - `predicate` (`FnMut(&T) -> bool`): Auswahl.
    ///
    /// # Returns
    /// Die entnommenen Nutzlasten in Einreihungsreihenfolge.
    pub fn remove_where<F>(&mut self, mut predicate: F) -> Vec<T>
    where
        F: FnMut(&T) -> bool,
    {
        let (mut taken, kept): (Vec<QueueEntry<T>>, Vec<QueueEntry<T>>) =
            std::mem::take(&mut self.entries)
                .into_iter()
                .partition(|entry| predicate(&entry.item));
        self.entries = kept;
        taken.sort_by_key(|entry| entry.seq);
        taken.into_iter().map(|entry| entry.item).collect()
    }

    /// Alle Nutzlasten in beliebiger Reihenfolge.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|entry| &entry.item)
    }

    // Position des Extrems: `Greater` = dringendster, `Less` = schwächster.
    fn position_of(&self, extreme: Ordering) -> Option<usize> {
        let mut best: Option<(usize, &QueueEntry<T>)> = None;
        for (position, entry) in self.entries.iter().enumerate() {
            best = match best {
                Some((_, current)) if entry_cmp(entry, current) != extreme => best,
                _ => Some((position, entry)),
            };
        }
        best.map(|(position, _)| position)
    }

    // Persistierbarer Inhalt.
    pub(crate) fn export(&self) -> QueueState<T>
    where
        T: Clone,
    {
        QueueState {
            next_seq: self.next_seq,
            evictions: self.evictions,
            entries: self.entries.clone(),
        }
    }

    // Baut die Warteschlange aus einem Zustand; was über `capacity` liegt
    // (Grenze der aktiven Konfiguration), wird nach Dringlichkeit verdrängt,
    // gezählt und zurückgegeben, der schwächste zuerst. Das ergibt dieselbe
    // Auswahl wie wiederholtes Verdrängen des schwächsten, aber mit einer
    // Sortierung an Ort und Stelle (`O(n log n)`) statt eines Durchlaufs je
    // verdrängtem Eintrag: ein riesiger Zustand kostet nicht quadratisch
    // Zeit. Neuen Speicher bekommen nur die behaltenen Einträge (höchstens
    // `capacity`); die verdrängten bleiben im geladenen Vektor, über den der
    // zurückgegebene Iterator läuft. Passt alles, bleibt die Reihenfolge
    // unverändert.
    pub(crate) fn import(capacity: usize, state: QueueState<T>) -> (Self, impl Iterator<Item = T>) {
        let next_seq = state.resume_seq();
        let QueueState {
            mut evictions,
            mut entries,
            ..
        } = state;
        let surplus = entries.len().saturating_sub(capacity);
        let overflow = if surplus > 0 {
            // Aufsteigend nach Dringlichkeit: vorn die schwächsten.
            entries.sort_unstable_by(entry_cmp);
            evictions = evictions.saturating_add(u64::try_from(surplus).unwrap_or(u64::MAX));
            let kept = entries.split_off(surplus);
            std::mem::replace(&mut entries, kept)
        } else {
            Vec::new()
        };
        let queue = Self {
            capacity,
            next_seq,
            evictions,
            entries,
        };
        (queue, overflow.into_iter().map(|entry| entry.item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn priority(novel: bool, hardness: Hardness, severity: Severity, at: i64) -> TriagePriority {
        TriagePriority {
            novel,
            hardness,
            severity,
            observed_at_unix: at,
        }
    }

    #[test]
    fn test_order_is_novelty_then_hardness_then_severity_then_age() -> TestResult {
        let mut queue = PriorityQueue::new(16);
        let entries = [
            (
                "old-known-inferred-low",
                priority(false, Hardness::Inferred, Severity::Low, 1),
            ),
            (
                "known-observed-low",
                priority(false, Hardness::Observed, Severity::Low, 5),
            ),
            (
                "known-observed-critical",
                priority(false, Hardness::Observed, Severity::Critical, 9),
            ),
            (
                "novel-inferred-info",
                priority(true, Hardness::Inferred, Severity::Info, 50),
            ),
            (
                "novel-correlated-info",
                priority(true, Hardness::Correlated, Severity::Info, 60),
            ),
            (
                "novel-correlated-high-new",
                priority(true, Hardness::Correlated, Severity::High, 70),
            ),
            (
                "novel-correlated-high-old",
                priority(true, Hardness::Correlated, Severity::High, 30),
            ),
        ];
        for (name, prio) in entries {
            assert!(matches!(queue.push(prio, name), PushOutcome::Queued));
        }
        let mut order = Vec::new();
        while let Some(name) = queue.pop() {
            order.push(name);
        }
        assert_eq!(
            order,
            [
                "novel-correlated-high-old",
                "novel-correlated-high-new",
                "novel-correlated-info",
                "novel-inferred-info",
                "known-observed-critical",
                "known-observed-low",
                "old-known-inferred-low",
            ]
        );
        Ok(())
    }

    #[test]
    fn test_equal_priority_is_fifo() -> TestResult {
        let mut queue = PriorityQueue::new(4);
        let same = priority(true, Hardness::Observed, Severity::High, 7);
        for item in 0..4_u8 {
            assert!(matches!(queue.push(same, item), PushOutcome::Queued));
        }
        let order: Vec<u8> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(order, [0, 1, 2, 3]);
        Ok(())
    }

    #[test]
    fn test_overflow_evicts_the_weakest_and_counts() -> TestResult {
        let mut queue = PriorityQueue::new(2);
        let weak = priority(false, Hardness::Inferred, Severity::Low, 1);
        let middle = priority(false, Hardness::Observed, Severity::Medium, 1);
        let strong = priority(true, Hardness::Observed, Severity::High, 1);
        assert!(matches!(queue.push(middle, "middle"), PushOutcome::Queued));
        assert!(matches!(queue.push(weak, "weak"), PushOutcome::Queued));
        assert_eq!(queue.push(strong, "strong"), PushOutcome::Evicted("weak"));
        assert_eq!(
            queue.push(weak, "weak-again"),
            PushOutcome::Rejected("weak-again")
        );
        // Gleiche Dringlichkeit wie der schwächste: der ältere bleibt.
        assert_eq!(
            queue.push(middle, "middle-2"),
            PushOutcome::Rejected("middle-2")
        );
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.evictions(), 3);
        assert_eq!(queue.pop(), Some("strong"));
        assert_eq!(queue.pop(), Some("middle"));
        Ok(())
    }

    #[test]
    fn test_zero_capacity_rejects_everything() -> TestResult {
        let mut queue = PriorityQueue::new(0);
        let prio = priority(true, Hardness::Observed, Severity::Critical, 0);
        assert_eq!(queue.push(prio, 1), PushOutcome::Rejected(1));
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn test_remove_where_returns_matches_in_fifo_order() -> TestResult {
        let mut queue = PriorityQueue::new(8);
        let prio = priority(false, Hardness::Observed, Severity::High, 0);
        for item in 0..6_u8 {
            assert!(matches!(queue.push(prio, item), PushOutcome::Queued));
        }
        let late = queue.remove_where(|item| *item >= 3);
        assert_eq!(late, [3, 4, 5]);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.iter().count(), 3);
        Ok(())
    }

    #[test]
    fn test_random_pushes_pop_in_non_increasing_urgency() -> TestResult {
        // Deterministische Pseudo-Zufallsfolge (kein proptest im Workspace).
        let hardnesses = [Hardness::Observed, Hardness::Correlated, Hardness::Inferred];
        let severities = [
            Severity::Info,
            Severity::Low,
            Severity::Medium,
            Severity::High,
            Severity::Critical,
        ];
        let mut queue = PriorityQueue::new(64);
        let mut state = 0x9e37_79b9_u32;
        let mut pushed = 0_u64;
        for item in 0..500_u32 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let pick = |shift: u32, modulus: u32| {
                let bits = state.checked_shr(shift).unwrap_or(0);
                usize::try_from(bits.checked_rem(modulus).unwrap_or(0)).unwrap_or(0)
            };
            let age = i64::try_from(pick(20, 100)).unwrap_or(0);
            let prio = priority(
                pick(4, 2) == 0,
                hardnesses
                    .get(pick(8, 3))
                    .copied()
                    .unwrap_or(Hardness::Inferred),
                severities
                    .get(pick(12, 5))
                    .copied()
                    .unwrap_or(Severity::Info),
                age,
            );
            let _outcome = queue.push(prio, (prio, item));
            pushed = pushed.saturating_add(1);
        }
        assert_eq!(queue.len(), 64);
        assert_eq!(queue.evictions(), pushed.saturating_sub(64));
        let order: Vec<(TriagePriority, u32)> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(order.len(), 64);
        for pair in order.windows(2) {
            if let [(earlier, _), (later, _)] = pair {
                assert_ne!(
                    earlier.cmp_urgency(later),
                    std::cmp::Ordering::Less,
                    "{earlier:?} before {later:?}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn test_import_trims_to_capacity_by_urgency() -> TestResult {
        let mut queue = PriorityQueue::new(4);
        for (item, severity) in [
            (1_u8, Severity::Low),
            (2, Severity::High),
            (3, Severity::Medium),
        ] {
            let prio = priority(false, Hardness::Observed, severity, 0);
            assert!(matches!(queue.push(prio, item), PushOutcome::Queued));
        }
        let (mut restored, overflow) = PriorityQueue::import(2, queue.export());
        assert_eq!(overflow.collect::<Vec<_>>(), [1]);
        assert_eq!(restored.evictions(), 1);
        assert_eq!(restored.pop(), Some(2));
        assert_eq!(restored.pop(), Some(3));
        Ok(())
    }

    #[test]
    fn test_import_keeps_what_repeated_eviction_would_keep() -> TestResult {
        // Deterministische Pseudo-Zufallsfolge (kein proptest im Workspace)
        // mit vielen Gleichständen in der Dringlichkeit.
        let hardnesses = [Hardness::Observed, Hardness::Correlated, Hardness::Inferred];
        let mut full = PriorityQueue::new(1_000);
        let mut state = 0x7f4a_7c15_u32;
        for item in 0..300_u32 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let pick = |shift: u32, modulus: u32| {
                state
                    .checked_shr(shift)
                    .unwrap_or(0)
                    .checked_rem(modulus)
                    .unwrap_or(0)
            };
            let hardness = usize::try_from(pick(8, 3))
                .ok()
                .and_then(|index| hardnesses.get(index).copied())
                .unwrap_or(Hardness::Inferred);
            let prio = priority(
                pick(4, 2) == 0,
                hardness,
                Severity::High,
                i64::from(pick(20, 4)),
            );
            assert!(matches!(full.push(prio, item), PushOutcome::Queued));
        }
        // Die Referenz: alle Einträge in Entnahme-Reihenfolge.
        let mut reference = full.clone();
        let order: Vec<u32> = std::iter::from_fn(|| reference.pop()).collect();
        for capacity in [1_usize, 50, 299, 300, 400] {
            let (mut restored, overflow) = PriorityQueue::import(capacity, full.export());
            let overflow: Vec<u32> = overflow.collect();
            let kept = capacity.min(order.len());
            let popped: Vec<u32> = std::iter::from_fn(|| restored.pop()).collect();
            let expected: Vec<u32> = order.iter().take(kept).copied().collect();
            assert_eq!(popped, expected, "capacity {capacity}");
            // Verdrängt: der schwächste zuerst.
            let mut weakest_first: Vec<u32> = order.iter().skip(kept).copied().collect();
            weakest_first.reverse();
            assert_eq!(overflow, weakest_first, "capacity {capacity}");
            let surplus = u64::try_from(order.len().saturating_sub(kept)).unwrap_or(u64::MAX);
            assert_eq!(restored.evictions(), surplus);
            assert_eq!(restored.next_seq, 300);
        }
        Ok(())
    }

    #[test]
    fn test_appended_entries_queue_after_the_existing_ones() -> TestResult {
        let same = priority(true, Hardness::Observed, Severity::High, 7);
        let mut queue = PriorityQueue::new(8);
        for item in [1_u8, 2] {
            assert!(matches!(queue.push(same, item), PushOutcome::Queued));
        }
        let mut state = queue.export();
        // Eine zu kleine `next_seq` (manipuliert) vergibt keine Nummer doppelt.
        state.next_seq = 0;
        let appended: Result<(), &str> = state.append([3_u8, 4], |_| Ok(same));
        assert!(appended.is_ok());
        assert_eq!(state.next_seq, 4);
        let (mut roomy, overflow) = PriorityQueue::import(8, state.clone());
        assert_eq!(overflow.count(), 0);
        let order: Vec<u8> = std::iter::from_fn(|| roomy.pop()).collect();
        assert_eq!(order, [1, 2, 3, 4]);
        // Bei gleicher Dringlichkeit verliert der zuletzt angehängte.
        let (mut tight, overflow) = PriorityQueue::import(3, state);
        assert_eq!(overflow.collect::<Vec<_>>(), [4]);
        let order: Vec<u8> = std::iter::from_fn(|| tight.pop()).collect();
        assert_eq!(order, [1, 2, 3]);
        // Der erste Fehler bricht ab.
        let mut failing = queue.export();
        let refused: Result<(), &str> = failing.append([5_u8, 6], |item| {
            if *item == 6 {
                Err("no priority")
            } else {
                Ok(same)
            }
        });
        assert_eq!(refused, Err("no priority"));
        Ok(())
    }
}

//! Bounded, process-local replay filtering for Telegram update identifiers.
//!
//! This is only a performance optimisation.  The durable `PairingStore` claim
//! remains the security and correctness gate for admitted updates.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

/// A bounded sliding window of recently claimed Telegram `update_id` values.
///
/// Values are evicted in insertion order once `capacity` is reached.  A zero
/// capacity disables the optimisation: every claim succeeds without retaining
/// any identifiers.  The window is safe to share between long-poll and webhook
/// ingress threads.
pub struct DedupWindow {
    capacity: usize,
    state: Mutex<WindowState>,
}

struct WindowState {
    ids: HashSet<i64>,
    order: VecDeque<i64>,
}

impl DedupWindow {
    /// Creates an empty window retaining at most `capacity` identifiers.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(WindowState {
                ids: HashSet::with_capacity(capacity),
                order: VecDeque::with_capacity(capacity),
            }),
        }
    }

    /// Claims `update_id`, returning `true` only if it is not in the window.
    ///
    /// A successful claim inserts the identifier and may evict the oldest
    /// retained identifier.  Duplicate claims do not refresh an identifier's
    /// position in the window.
    pub fn claim_update(&self, update_id: i64) -> bool {
        if self.capacity == 0 {
            return true;
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.ids.contains(&update_id) {
            return false;
        }

        if state.order.len() == self.capacity {
            // `order.len() == self.capacity` und `self.capacity > 0` (siehe
            // frühe Rückkehr oben) garantieren gemeinsam ein nicht-leeres
            // `order`; `pop_front()` liefert trotzdem `Option`, damit hier
            // ohne `.expect()` (Bible R087) ausgekommen wird — ein `None`
            // ließe die Eviction schlicht entfallen, statt zu paniken.
            if let Some(evicted) = state.order.pop_front() {
                state.ids.remove(&evicted);
            }
        }

        state.order.push_back(update_id);
        state.ids.insert(update_id);
        true
    }

    /// Alias for callers that use the shorter claim terminology.
    pub fn claim(&self, update_id: i64) -> bool {
        self.claim_update(update_id)
    }
}

#[cfg(test)]
mod tests {
    use super::DedupWindow;

    #[test]
    fn first_claim_succeeds_and_duplicate_is_rejected() {
        let window = DedupWindow::new(4);

        assert!(window.claim_update(42));
        assert!(!window.claim_update(42));
    }

    #[test]
    fn oldest_identifier_is_evicted_and_can_be_claimed_again() {
        let window = DedupWindow::new(2);

        assert!(window.claim(1));
        assert!(window.claim(2));
        assert!(!window.claim(1));
        assert!(window.claim(3));
        assert!(!window.claim(3));
        assert!(window.claim(1));
        assert!(window.claim(2));
    }

    #[test]
    fn zero_capacity_accepts_every_claim_without_retaining_identifiers() {
        let window = DedupWindow::new(0);

        assert!(window.claim(1));
        assert!(window.claim(1));
        assert!(window.claim(2));
    }

    #[test]
    fn independent_identifiers_are_tracked_separately() {
        let window = DedupWindow::new(2);

        assert!(window.claim_update(10));
        assert!(window.claim_update(11));
        assert!(!window.claim_update(10));
        assert!(!window.claim_update(11));
    }
}

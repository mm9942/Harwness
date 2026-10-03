//! Which events of an attachment are new, and which are replay.
//!
//! # The rule
//! A [`Cursor`] is the position of the **next entry the client has not seen**
//! (`durable`: the transcript sequence, `live`: the index in the live ring of
//! the running turn). Two consequences, both found by failures:
//!
//! - An event **on** the head the client attached at is new, not replay. On a
//!   fresh session the very first live frame carries exactly the attach head
//!   (`0,0,0`); a strict `>` would drop it.
//! - Replay lies **before** the head. After attaching, the host replays the
//!   recent transcript, and a late replay frame must not be taken for an event
//!   of the turn the client is waiting for (it would end that turn at the end
//!   of an old one).
//!
//! On top of that, an event is only processed once and in order: it must be
//! strictly behind the last one that was processed.
//!
//! [`StreamTracker`] holds exactly this state. Frames that are not events
//! (approval requests, presence, snapshots) carry the head position of their
//! moment and are not judged here.

use harw_protocol::session_wire::Cursor;

/// Tracks the head and the last processed event of one attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTracker {
    head: Cursor,
    last_seen: Option<Cursor>,
}

impl StreamTracker {
    /// A tracker for an attachment whose host reported `head` when attaching.
    #[must_use]
    pub fn new(head: Cursor) -> Self {
        Self {
            head,
            last_seen: None,
        }
    }

    /// The head to send as `expect_head` with the next `turn.submit`: the
    /// position of the next entry this client has not seen.
    #[must_use]
    pub fn expect_head(&self) -> Cursor {
        self.head
    }

    /// `true` when an event at `cursor` is new: not before the head, and
    /// strictly behind the last processed event.
    #[must_use]
    pub fn is_new(&self, cursor: &Cursor) -> bool {
        *cursor >= self.head && self.last_seen.as_ref().is_none_or(|seen| cursor > seen)
    }

    /// The opposite of [`Self::is_new`]: a replayed or already processed
    /// event, to be ignored.
    #[must_use]
    pub fn is_replay(&self, cursor: &Cursor) -> bool {
        !self.is_new(cursor)
    }

    /// Judges an event and, if it is new, remembers it. Returns whether to
    /// process it.
    pub fn accept(&mut self, cursor: &Cursor) -> bool {
        let new = self.is_new(cursor);
        if new {
            self.last_seen = Some(*cursor);
        }
        new
    }

    /// The host answered `Stale { head }`: the transcript moved on. Entries
    /// from `head` on are new; what was processed so far stays processed.
    pub fn moved_to(&mut self, head: Cursor) {
        self.head = head;
    }

    /// The client attached again. After a loss (`from` is the position to
    /// resume at) everything from there on counts as new, because it was not
    /// seen; otherwise the host's `head` is the new starting point. What was
    /// processed before is forgotten: it belonged to the old attachment.
    pub fn reattached(&mut self, from: Option<Cursor>, host_head: Cursor) {
        self.head = from.unwrap_or(host_head);
        self.last_seen = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    fn at(generation: u32, durable: u64, live: u32) -> Cursor {
        Cursor {
            generation,
            durable,
            live,
        }
    }

    #[test]
    fn an_event_on_the_attach_head_is_new() -> TestResult {
        // A fresh session: the first live frame sits exactly on the head.
        let mut tracker = StreamTracker::new(at(0, 0, 0));
        ensure(tracker.accept(&at(0, 0, 0)), "on the head")?;
        ensure(tracker.accept(&at(0, 0, 1)), "behind it")
    }

    #[test]
    fn replay_before_the_head_is_ignored() -> TestResult {
        let tracker = StreamTracker::new(at(0, 5, 0));
        ensure(tracker.is_replay(&at(0, 3, 0)), "old transcript item")?;
        ensure(
            tracker.is_replay(&at(0, 4, 9)),
            "late replay of the last turn",
        )?;
        ensure(tracker.is_new(&at(0, 5, 0)), "the head itself")
    }

    #[test]
    fn an_event_is_processed_once_and_in_order() -> TestResult {
        let mut tracker = StreamTracker::new(at(0, 5, 0));
        ensure(tracker.accept(&at(0, 5, 2)), "first")?;
        ensure(!tracker.accept(&at(0, 5, 2)), "the same cursor again")?;
        ensure(!tracker.accept(&at(0, 5, 1)), "an earlier one")?;
        ensure(tracker.accept(&at(0, 6, 0)), "a later durable item")
    }

    #[test]
    fn positions_order_by_generation_then_durable_then_live() -> TestResult {
        let tracker = StreamTracker::new(at(1, 0, 0));
        ensure(tracker.is_replay(&at(0, 99, 99)), "an older generation")?;
        ensure(tracker.is_new(&at(2, 0, 0)), "a newer generation")?;
        let tracker = StreamTracker::new(at(1, 5, 0));
        ensure(tracker.is_replay(&at(1, 4, 9)), "durable before live")?;
        ensure(tracker.is_new(&at(1, 5, 1)), "live after the head")
    }

    #[test]
    fn a_stale_answer_moves_the_head_but_not_what_was_processed() -> TestResult {
        let mut tracker = StreamTracker::new(at(0, 5, 0));
        ensure(tracker.accept(&at(0, 5, 3)), "processed")?;
        tracker.moved_to(at(0, 9, 0));
        ensure(
            tracker.expect_head() == at(0, 9, 0),
            "the new head is expected",
        )?;
        ensure(tracker.is_replay(&at(0, 8, 0)), "before the new head")?;
        ensure(tracker.accept(&at(0, 9, 0)), "on the new head")
    }

    #[test]
    fn reattaching_after_a_loss_counts_everything_from_the_target_as_new() -> TestResult {
        let mut tracker = StreamTracker::new(at(0, 5, 0));
        ensure(tracker.accept(&at(0, 5, 4)), "before the loss")?;
        // The host asked to resume at (0,5,2): those events were missed.
        tracker.reattached(Some(at(0, 5, 2)), at(0, 7, 0));
        ensure(tracker.accept(&at(0, 5, 2)), "a missed event is new again")?;
        // Without a target the host's head starts it.
        tracker.reattached(None, at(0, 7, 0));
        ensure(tracker.is_replay(&at(0, 6, 0)), "before the host head")?;
        ensure(tracker.is_new(&at(0, 7, 0)), "on it")
    }
}

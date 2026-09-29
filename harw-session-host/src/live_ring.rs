//! Bounded live buffer of the running turn (PL-65 §3.4 step 2).
//!
//! Every live frame of a session gets a monotonic live index. On re-attach
//! the host asks the ring for everything from the client's `cursor.live`:
//! if the ring still holds that position, the deltas are replayed from
//! there ([`LiveRing::since`]); otherwise the host sends one
//! [`SessionFrame::Snapshot`] with the text accumulated so far
//! ([`LiveRing::snapshot`]) and continues live.
//!
//! Bounds: the ring keeps at most `max_frames` frames and `max_bytes` of
//! serialized frame JSON; the oldest frames are evicted first. The
//! accumulated snapshot text is capped at [`MAX_SNAPSHOT_TEXT_BYTES`] and
//! keeps the tail.
//!
//! Index overflow: the live index is a `u32` that never wraps. Once it
//! reaches `u32::MAX` the ring can no longer hand out unique indices; it then
//! stops buffering (fail closed): every `since` below `u32::MAX` answers
//! `None`, so the host falls back to a snapshot. Four billion frames in one
//! process are not reachable in practice, but the path must not panic.

use std::collections::VecDeque;

use harw_protocol::{SessionFrame, TurnEvent};
use harw_types::TurnId;

/// Default frame cap of a ring.
pub const DEFAULT_MAX_FRAMES: usize = 4096;

/// Default byte cap (serialized frame JSON) of a ring.
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Cap of the accumulated assistant text a snapshot carries (tail kept).
pub const MAX_SNAPSHOT_TEXT_BYTES: usize = 1024 * 1024;

/// One buffered frame with its live index and serialized size.
#[derive(Debug)]
struct Slot {
    index: u32,
    bytes: usize,
    frame: SessionFrame,
}

/// Bounded live buffer of one session's running turn.
#[derive(Debug)]
pub struct LiveRing {
    max_frames: usize,
    max_bytes: usize,
    slots: VecDeque<Slot>,
    /// Sum of `Slot::bytes` over `slots`.
    bytes: usize,
    /// Next live index to hand out (saturates at `u32::MAX`).
    next: u32,
    /// Oldest live index `since` can still answer. Everything below was
    /// evicted or belongs to an earlier turn.
    floor: u32,
    turn: Option<TurnId>,
    assistant_text: String,
    reasoning_seen: bool,
}

impl Default for LiveRing {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAMES, DEFAULT_MAX_BYTES)
    }
}

impl LiveRing {
    /// Empty ring with the given limits.
    pub fn new(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            max_frames,
            max_bytes,
            slots: VecDeque::new(),
            bytes: 0,
            next: 0,
            floor: 0,
            turn: None,
            assistant_text: String::new(),
            reasoning_seen: false,
        }
    }

    /// Start a new turn: drop buffered frames and accumulated text, remember
    /// the turn id. The live index counter is not reset (monotonic per ring).
    pub fn begin_turn(&mut self, turn_id: TurnId) {
        self.clear_frames();
        self.assistant_text.clear();
        self.reasoning_seen = false;
        self.turn = Some(turn_id);
    }

    /// Append a frame and return its live index.
    ///
    /// Accumulates `AssistantDelta` text of the current turn and notes that
    /// reasoning was seen. Evicts the oldest frames while over `max_frames`
    /// or `max_bytes` (the new frame itself too, if it alone is too large).
    pub fn push(&mut self, frame: SessionFrame) -> u32 {
        self.accumulate(&frame);
        if self.next == u32::MAX {
            // Saturated: no unique index left. Stop buffering so every
            // earlier position falls back to a snapshot.
            self.clear_frames();
            return u32::MAX;
        }
        let index = self.next;
        self.next = self.next.saturating_add(1);
        let bytes = serde_json::to_vec(&frame).map_or(0, |json| json.len());
        self.bytes = self.bytes.saturating_add(bytes);
        self.slots.push_back(Slot {
            index,
            bytes,
            frame,
        });
        self.evict();
        index
    }

    /// Frames with index `>= live`, ascending, each with its index.
    ///
    /// `None` when `live` is older than the oldest retained position
    /// (evicted, or from before [`Self::begin_turn`]); the caller then sends
    /// a snapshot. `Some(empty)` when `live >= next_index()`.
    pub fn since(&self, live: u32) -> Option<Vec<(u32, SessionFrame)>> {
        if live < self.floor {
            return None;
        }
        Some(
            self.slots
                .iter()
                .filter(|slot| slot.index >= live)
                .map(|slot| (slot.index, slot.frame.clone()))
                .collect(),
        )
    }

    /// Snapshot of the running turn (`None` when no turn is active).
    pub fn snapshot(&self) -> Option<SessionFrame> {
        self.turn.as_ref().map(|turn_id| SessionFrame::Snapshot {
            turn_id: turn_id.clone(),
            assistant_text: self.assistant_text.clone(),
            reasoning_collapsed: self.reasoning_seen,
        })
    }

    /// Turn ended: clear buffered frames, keep the counter, no active turn.
    pub fn end_turn(&mut self) {
        self.clear_frames();
        self.assistant_text.clear();
        self.reasoning_seen = false;
        self.turn = None;
    }

    /// Live index the next pushed frame gets.
    pub fn next_index(&self) -> u32 {
        self.next
    }

    /// Number of buffered frames.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no frame is buffered.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Turn the ring is buffering, if any.
    pub fn active_turn(&self) -> Option<&TurnId> {
        self.turn.as_ref()
    }

    /// Drop every buffered frame; only positions from `next` on stay valid.
    fn clear_frames(&mut self) {
        self.slots.clear();
        self.bytes = 0;
        self.floor = self.next;
    }

    fn evict(&mut self) {
        while self.slots.len() > self.max_frames || self.bytes > self.max_bytes {
            let Some(slot) = self.slots.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(slot.bytes);
            self.floor = slot.index.saturating_add(1);
        }
    }

    fn accumulate(&mut self, frame: &SessionFrame) {
        let Some(active) = self.turn.as_ref() else {
            return;
        };
        match frame {
            SessionFrame::Turn(TurnEvent::AssistantDelta { turn_id, text })
                if turn_id == active =>
            {
                self.assistant_text.push_str(text);
                trim_to_tail(&mut self.assistant_text, MAX_SNAPSHOT_TEXT_BYTES);
            }
            SessionFrame::Turn(TurnEvent::ReasoningDelta { turn_id, .. }) if turn_id == active => {
                self.reasoning_seen = true;
            }
            _ => {}
        }
    }
}

/// Keep at most `cap` bytes of `text`, dropping the head and cutting on a
/// char boundary (the result may be a few bytes shorter than `cap`).
fn trim_to_tail(text: &mut String, cap: usize) {
    let Some(mut cut) = text.len().checked_sub(cap) else {
        return;
    };
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut = cut.saturating_add(1);
    }
    text.drain(..cut);
}

#[cfg(test)]
mod tests {
    use harw_protocol::SessionEvent;

    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn turn(id: &str) -> TestResult<TurnId> {
        Ok(TurnId::try_from_str(id)?)
    }

    fn delta(turn_id: &TurnId, text: &str) -> SessionFrame {
        SessionFrame::Turn(TurnEvent::AssistantDelta {
            turn_id: turn_id.clone(),
            text: text.into(),
        })
    }

    fn indices(frames: &[(u32, SessionFrame)]) -> Vec<u32> {
        frames.iter().map(|(index, _)| *index).collect()
    }

    fn snapshot_parts(ring: &LiveRing) -> TestResult<(TurnId, String, bool)> {
        match ring.snapshot() {
            Some(SessionFrame::Snapshot {
                turn_id,
                assistant_text,
                reasoning_collapsed,
            }) => Ok((turn_id, assistant_text, reasoning_collapsed)),
            other => Err(format!("expected snapshot, got {other:?}").into()),
        }
    }

    #[test]
    fn indices_are_monotonic_across_turns() -> TestResult {
        let mut ring = LiveRing::default();
        let t1 = turn("t-1")?;
        let t2 = turn("t-2")?;
        ring.begin_turn(t1.clone());
        assert_eq!(ring.push(delta(&t1, "a")), 0);
        assert_eq!(ring.push(delta(&t1, "b")), 1);
        ring.end_turn();
        ring.begin_turn(t2.clone());
        assert_eq!(ring.push(delta(&t2, "c")), 2);
        assert_eq!(ring.next_index(), 3);
        Ok(())
    }

    #[test]
    fn since_within_ring_replays_from_position() -> TestResult {
        let mut ring = LiveRing::default();
        let t = turn("t-1")?;
        ring.begin_turn(t.clone());
        for text in ["a", "b", "c", "d"] {
            ring.push(delta(&t, text));
        }
        let frames = ring.since(2).ok_or("position 2 retained")?;
        assert_eq!(indices(&frames), vec![2, 3]);
        let all = ring.since(0).ok_or("position 0 retained")?;
        assert_eq!(indices(&all), vec![0, 1, 2, 3]);
        Ok(())
    }

    #[test]
    fn since_after_eviction_is_none() -> TestResult {
        let mut ring = LiveRing::new(2, DEFAULT_MAX_BYTES);
        let t = turn("t-1")?;
        ring.begin_turn(t.clone());
        for text in ["a", "b", "c", "d"] {
            ring.push(delta(&t, text));
        }
        assert_eq!(ring.len(), 2);
        assert!(ring.since(0).is_none());
        assert!(ring.since(1).is_none());
        let frames = ring.since(2).ok_or("position 2 retained")?;
        assert_eq!(indices(&frames), vec![2, 3]);
        Ok(())
    }

    #[test]
    fn since_before_begin_turn_is_none() -> TestResult {
        let mut ring = LiveRing::default();
        let t1 = turn("t-1")?;
        let t2 = turn("t-2")?;
        ring.begin_turn(t1.clone());
        ring.push(delta(&t1, "a"));
        ring.begin_turn(t2.clone());
        assert!(ring.since(0).is_none());
        let frames = ring.since(1).ok_or("fresh turn position")?;
        assert!(frames.is_empty());
        Ok(())
    }

    #[test]
    fn since_future_position_is_empty() -> TestResult {
        let mut ring = LiveRing::default();
        let t = turn("t-1")?;
        ring.begin_turn(t.clone());
        ring.push(delta(&t, "a"));
        let at_head = ring.since(ring.next_index()).ok_or("head position")?;
        assert!(at_head.is_empty());
        let future = ring.since(1000).ok_or("future position")?;
        assert!(future.is_empty());
        Ok(())
    }

    #[test]
    fn snapshot_accumulates_deltas_of_current_turn_only() -> TestResult {
        let mut ring = LiveRing::default();
        assert!(ring.snapshot().is_none());
        let t1 = turn("t-1")?;
        let t2 = turn("t-2")?;
        ring.begin_turn(t1.clone());
        ring.push(delta(&t1, "old "));
        ring.push(delta(&t1, "text"));
        let (id, text, reasoning) = snapshot_parts(&ring)?;
        assert_eq!(
            (id, text.as_str(), reasoning),
            (t1.clone(), "old text", false)
        );

        ring.begin_turn(t2.clone());
        ring.push(delta(&t2, "Hello, "));
        ring.push(delta(&t1, "stale"));
        ring.push(SessionFrame::Turn(TurnEvent::ReasoningDelta {
            turn_id: t2.clone(),
            text: "thinking".into(),
        }));
        ring.push(delta(&t2, "world"));
        let (id, text, reasoning) = snapshot_parts(&ring)?;
        assert_eq!(id, t2);
        assert_eq!(text, "Hello, world");
        assert!(reasoning);
        Ok(())
    }

    #[test]
    fn snapshot_text_is_capped_on_char_boundary() {
        let mut text = "aé".repeat(4);
        trim_to_tail(&mut text, 4);
        assert!(text.len() <= 4);
        assert!(text.ends_with('é'));
        let mut short = String::from("abc");
        trim_to_tail(&mut short, 10);
        assert_eq!(short, "abc");
    }

    #[test]
    fn byte_cap_evicts_oldest_frames() -> TestResult {
        let t = turn("t-1")?;
        let one = serde_json::to_vec(&delta(&t, "0123456789"))?.len();
        let mut ring = LiveRing::new(DEFAULT_MAX_FRAMES, one * 2);
        ring.begin_turn(t.clone());
        for text in ["0123456789", "abcdefghij", "ABCDEFGHIJ"] {
            ring.push(delta(&t, text));
        }
        assert_eq!(ring.len(), 2);
        assert!(ring.since(0).is_none());
        let frames = ring.since(1).ok_or("position 1 retained")?;
        assert_eq!(indices(&frames), vec![1, 2]);
        Ok(())
    }

    #[test]
    fn end_turn_clears_frames_and_turn() -> TestResult {
        let mut ring = LiveRing::default();
        let t = turn("t-1")?;
        ring.begin_turn(t.clone());
        ring.push(delta(&t, "a"));
        ring.push(SessionFrame::Session(SessionEvent::TurnStarted {
            session_id: harw_types::SessionId::try_from_str("s-1")?,
            turn_id: t.clone(),
        }));
        assert_eq!(ring.active_turn(), Some(&t));
        ring.end_turn();
        assert!(ring.is_empty());
        assert!(ring.active_turn().is_none());
        assert!(ring.snapshot().is_none());
        assert_eq!(ring.next_index(), 2);
        assert!(ring.since(0).is_none());
        let frames = ring.since(2).ok_or("head position")?;
        assert!(frames.is_empty());
        Ok(())
    }
}

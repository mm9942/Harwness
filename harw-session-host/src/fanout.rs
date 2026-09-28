//! Per-attachment bounded outbound queues (W00 §4, PL-65 §3.5).
//!
//! Every attachment owns one queue. The host (producer) only ever calls
//! [`AttachmentQueue::push`], which is synchronous: it never blocks and
//! never awaits, so a slow client can never stall the turn loop.
//!
//! Backpressure, in order:
//! - The [`StreamProfile::Compact`] profile drops reasoning deltas, child
//!   progress and every child delta, always keeps only the latest
//!   `UsageUpdated` of a turn and always merges consecutive assistant deltas.
//! - Above half fill (frames or bytes) the full profile merges consecutive
//!   `AssistantDelta`/`ReasoningDelta` of the same turn and keeps only the
//!   latest `UsageUpdated` of a turn.
//! - On overflow the queue is cleared and replaced by a single
//!   [`SessionFrame::Lagged`] whose `resume_from` is the cursor of the oldest
//!   undelivered frame; the attachment then ends and the client re-attaches
//!   from that cursor.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use harw_protocol::session_wire::{Cursor, FrameEnvelope, SessionFrame, StreamProfile};
use harw_protocol::{FrameSource, PortFuture, TurnEvent};
use harw_types::SessionId;
use tokio::sync::Notify;

/// Default frame limit of one attachment queue.
pub const DEFAULT_MAX_FRAMES: usize = 1024;

/// Default byte limit (serialized JSON) of one attachment queue.
pub const DEFAULT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Bounds of one attachment queue. Whichever limit is hit first overflows.
#[derive(Clone, Copy, Debug)]
pub struct QueueLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
}

impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_frames: DEFAULT_MAX_FRAMES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// What [`AttachmentQueue::push`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Appended to the queue.
    Queued,
    /// Merged into (or superseded) an undelivered frame.
    Coalesced,
    /// Dropped by the attachment's stream profile.
    Filtered,
    /// The queue overflowed; the attachment now ends with `Lagged`.
    Lagged,
    /// The attachment is closed (closed, lagged or consumer gone).
    Closed,
}

/// One queued frame.
struct Entry {
    envelope: FrameEnvelope,
    bytes: usize,
    /// Cursor of the oldest content merged into this entry. Resume points
    /// use it so coalescing never skips undelivered content.
    first: Cursor,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Entry>,
    bytes: usize,
    closed: bool,
}

struct Inner {
    session_id: SessionId,
    profile: StreamProfile,
    limits: QueueLimits,
    state: Mutex<State>,
    notify: Notify,
}

impl Inner {
    /// The state, or `None` when the mutex is poisoned (treated as closed).
    fn lock(&self) -> Option<MutexGuard<'_, State>> {
        self.state.lock().ok()
    }
}

/// Producer handle of one attachment. Cheap to clone.
#[derive(Clone)]
pub struct AttachmentQueue {
    inner: Arc<Inner>,
}

/// Consumer side of one attachment. Dropping it closes the attachment.
pub struct AttachmentStream {
    inner: Arc<Inner>,
}

/// Create the queue pair of one attachment.
#[must_use]
pub fn attachment(
    session_id: SessionId,
    profile: StreamProfile,
    limits: QueueLimits,
) -> (AttachmentQueue, AttachmentStream) {
    let inner = Arc::new(Inner {
        session_id,
        profile,
        limits,
        state: Mutex::new(State::default()),
        notify: Notify::new(),
    });
    (
        AttachmentQueue {
            inner: Arc::clone(&inner),
        },
        AttachmentStream { inner },
    )
}

/// Serialized size of an envelope; a frame that cannot be serialized counts
/// as zero (the transport rejects it later anyway).
fn frame_bytes(envelope: &FrameEnvelope) -> usize {
    serde_json::to_vec(envelope).map_or(0, |bytes| bytes.len())
}

/// True when the compact profile drops this frame outright.
fn compact_drops(frame: &SessionFrame) -> bool {
    match frame {
        SessionFrame::Turn(event) => matches!(
            event,
            TurnEvent::ReasoningDelta { .. } | TurnEvent::ChildProgress { .. }
        ),
        SessionFrame::Child { event, .. } => matches!(
            event,
            TurnEvent::AssistantDelta { .. }
                | TurnEvent::ReasoningDelta { .. }
                | TurnEvent::UsageUpdated { .. }
                | TurnEvent::ContextUpdated { .. }
                | TurnEvent::ChildProgress { .. }
        ),
        _ => false,
    }
}

/// Append `incoming`'s delta text to `queued` when both are the same kind of
/// root-turn delta of the same turn. Returns true when merged.
fn merge_delta(queued: &mut SessionFrame, incoming: &SessionFrame, allow_reasoning: bool) -> bool {
    match (queued, incoming) {
        (
            SessionFrame::Turn(TurnEvent::AssistantDelta { turn_id, text }),
            SessionFrame::Turn(TurnEvent::AssistantDelta {
                turn_id: next_turn,
                text: next_text,
            }),
        ) if *turn_id == *next_turn => {
            text.push_str(next_text);
            true
        }
        (
            SessionFrame::Turn(TurnEvent::ReasoningDelta { turn_id, text }),
            SessionFrame::Turn(TurnEvent::ReasoningDelta {
                turn_id: next_turn,
                text: next_text,
            }),
        ) if allow_reasoning && *turn_id == *next_turn => {
            text.push_str(next_text);
            true
        }
        _ => false,
    }
}

/// True when `frame` is a root-turn `UsageUpdated` of `turn`.
fn is_usage_of(frame: &SessionFrame, turn: &harw_types::TurnId) -> bool {
    matches!(
        frame,
        SessionFrame::Turn(TurnEvent::UsageUpdated { turn_id, .. }) if turn_id == turn
    )
}

impl AttachmentQueue {
    /// Queue one frame. Never blocks and never awaits.
    ///
    /// Applies the profile filter and coalescing. On overflow the queue is
    /// cleared, one `Lagged { resume_from }` envelope is queued (cursor of
    /// the oldest undelivered frame, or of `envelope` when nothing was
    /// queued), the attachment is closed and [`PushOutcome::Lagged`] is
    /// returned. The `Lagged` frame itself may exceed the limits.
    pub fn push(&self, envelope: FrameEnvelope) -> PushOutcome {
        let compact = self.inner.profile == StreamProfile::Compact;
        if compact && compact_drops(&envelope.frame) {
            return PushOutcome::Filtered;
        }
        let size = frame_bytes(&envelope);
        let limits = self.inner.limits;
        let Some(mut state) = self.inner.lock() else {
            return PushOutcome::Closed;
        };
        if state.closed {
            return PushOutcome::Closed;
        }
        let pressure = compact
            || state.queue.len().saturating_mul(2) > limits.max_frames
            || state.bytes.saturating_mul(2) > limits.max_bytes;

        let outcome = if pressure {
            Self::coalesce(&mut state, envelope, size, compact, limits)
        } else {
            Self::append(&mut state, envelope, size, None, limits)
        };
        drop(state);
        if outcome != PushOutcome::Filtered {
            self.inner.notify.notify_one();
        }
        outcome
    }

    /// Coalescing path (compact profile or above half fill).
    fn coalesce(
        state: &mut State,
        envelope: FrameEnvelope,
        size: usize,
        compact: bool,
        limits: QueueLimits,
    ) -> PushOutcome {
        // Merge into the previous undelivered delta of the same turn.
        let mut merged: Option<(usize, usize)> = None;
        if let Some(last) = state.queue.back_mut() {
            if merge_delta(&mut last.envelope.frame, &envelope.frame, !compact) {
                last.envelope.cursor = envelope.cursor;
                let old = last.bytes;
                last.bytes = frame_bytes(&last.envelope);
                merged = Some((old, last.bytes));
            }
        }
        if let Some((old, new)) = merged {
            state.bytes = state.bytes.saturating_sub(old).saturating_add(new);
            if state.bytes > limits.max_bytes {
                let resume_from = Self::resume_point(state, envelope.cursor);
                return Self::lag(state, &envelope.session_id, resume_from);
            }
            return PushOutcome::Coalesced;
        }

        // Keep only the latest usage snapshot of a turn.
        if let SessionFrame::Turn(TurnEvent::UsageUpdated { turn_id, .. }) = &envelope.frame {
            let mut first: Option<Cursor> = None;
            let mut removed_bytes = 0usize;
            state.queue.retain(|entry| {
                if is_usage_of(&entry.envelope.frame, turn_id) {
                    first = Some(first.map_or(entry.first, |seen| seen.min(entry.first)));
                    removed_bytes = removed_bytes.saturating_add(entry.bytes);
                    false
                } else {
                    true
                }
            });
            if first.is_some() {
                state.bytes = state.bytes.saturating_sub(removed_bytes);
                return match Self::append(state, envelope, size, first, limits) {
                    PushOutcome::Queued => PushOutcome::Coalesced,
                    other => other,
                };
            }
        }
        Self::append(state, envelope, size, None, limits)
    }

    /// Append one entry, or lag when it does not fit.
    fn append(
        state: &mut State,
        envelope: FrameEnvelope,
        size: usize,
        first: Option<Cursor>,
        limits: QueueLimits,
    ) -> PushOutcome {
        let fits = state.queue.len() < limits.max_frames
            && state.bytes.saturating_add(size) <= limits.max_bytes;
        if !fits {
            let own = first.map_or(envelope.cursor, |cursor| cursor.min(envelope.cursor));
            let resume_from = Self::resume_point(state, own);
            return Self::lag(state, &envelope.session_id, resume_from);
        }
        let first = first.map_or(envelope.cursor, |cursor| cursor.min(envelope.cursor));
        state.bytes = state.bytes.saturating_add(size);
        state.queue.push_back(Entry {
            envelope,
            bytes: size,
            first,
        });
        PushOutcome::Queued
    }

    /// Oldest undelivered cursor, or `fallback` when nothing is queued.
    fn resume_point(state: &State, fallback: Cursor) -> Cursor {
        state
            .queue
            .iter()
            .map(|entry| entry.first)
            .min()
            .map_or(fallback, |oldest| oldest.min(fallback))
    }

    /// Replace the queue by one `Lagged` frame and close the attachment.
    fn lag(state: &mut State, session_id: &SessionId, resume_from: Cursor) -> PushOutcome {
        let lagged = FrameEnvelope {
            session_id: session_id.clone(),
            cursor: resume_from,
            frame: SessionFrame::Lagged { resume_from },
        };
        let bytes = frame_bytes(&lagged);
        state.queue.clear();
        state.queue.push_back(Entry {
            envelope: lagged,
            bytes,
            first: resume_from,
        });
        state.bytes = bytes;
        state.closed = true;
        PushOutcome::Lagged
    }

    /// Close the stream after the currently queued frames plus an optional
    /// final frame (e.g. `Revoked`, `HostDraining`). Further pushes return
    /// [`PushOutcome::Closed`]. No-op when already closed.
    pub fn close(&self, final_frame: Option<FrameEnvelope>) {
        let Some(mut state) = self.inner.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        if let Some(envelope) = final_frame {
            let bytes = frame_bytes(&envelope);
            let first = envelope.cursor;
            state.bytes = state.bytes.saturating_add(bytes);
            state.queue.push_back(Entry {
                envelope,
                bytes,
                first,
            });
        }
        state.closed = true;
        drop(state);
        self.inner.notify.notify_one();
    }

    /// True once the attachment accepts no more frames.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.lock().is_none_or(|state| state.closed)
    }

    /// Frames queued and not yet delivered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |state| state.queue.len())
    }

    /// True when no frame is waiting for delivery.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Session this attachment belongs to.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.inner.session_id
    }
}

/// Result of one non-blocking look at the queue.
enum Next {
    Frame(Box<FrameEnvelope>),
    Ended,
    Empty,
}

impl AttachmentStream {
    fn try_next(&self) -> Next {
        let Some(mut state) = self.inner.lock() else {
            return Next::Ended;
        };
        if let Some(entry) = state.queue.pop_front() {
            state.bytes = state.bytes.saturating_sub(entry.bytes);
            return Next::Frame(Box::new(entry.envelope));
        }
        if state.closed {
            Next::Ended
        } else {
            Next::Empty
        }
    }

    /// Next frame; `None` once the attachment is closed and drained.
    pub async fn recv(&mut self) -> Option<FrameEnvelope> {
        loop {
            // Register interest before checking the state so a push between
            // the check and the await cannot be lost.
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_next() {
                Next::Frame(envelope) => return Some(*envelope),
                Next::Ended => return None,
                Next::Empty => notified.await,
            }
        }
    }
}

impl FrameSource for AttachmentStream {
    fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
        Box::pin(async move { Ok(self.recv().await) })
    }
}

impl Drop for AttachmentStream {
    fn drop(&mut self) {
        // Consumer gone: stop queueing and release the buffered frames.
        if let Some(mut state) = self.inner.lock() {
            state.closed = true;
            state.queue.clear();
            state.bytes = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Display;

    use harw_types::{TokenUsage, TurnId};

    use super::*;

    type TestResult<T = ()> = Result<T, String>;

    fn ctx<T, E: Display>(result: Result<T, E>, context: &str) -> TestResult<T> {
        result.map_err(|error| format!("{context}: {error}"))
    }

    fn check(condition: bool, message: &str) -> TestResult {
        if condition {
            Ok(())
        } else {
            Err(message.to_owned())
        }
    }

    fn sid() -> TestResult<SessionId> {
        ctx(SessionId::try_from_str("s-fanout"), "session id")
    }

    fn tid(value: &str) -> TestResult<TurnId> {
        ctx(TurnId::try_from_str(value), "turn id")
    }

    fn at(live: u32) -> Cursor {
        Cursor {
            generation: 0,
            durable: 0,
            live,
        }
    }

    fn env(live: u32, frame: SessionFrame) -> TestResult<FrameEnvelope> {
        Ok(FrameEnvelope {
            session_id: sid()?,
            cursor: at(live),
            frame,
        })
    }

    fn assistant(turn: &str, text: &str) -> TestResult<SessionFrame> {
        Ok(SessionFrame::Turn(TurnEvent::AssistantDelta {
            turn_id: tid(turn)?,
            text: text.to_owned(),
        }))
    }

    fn reasoning(turn: &str, text: &str) -> TestResult<SessionFrame> {
        Ok(SessionFrame::Turn(TurnEvent::ReasoningDelta {
            turn_id: tid(turn)?,
            text: text.to_owned(),
        }))
    }

    fn usage(turn: &str, output_tokens: u64) -> TestResult<SessionFrame> {
        let round = TokenUsage {
            output_tokens,
            ..TokenUsage::default()
        };
        Ok(SessionFrame::Turn(TurnEvent::UsageUpdated {
            turn_id: tid(turn)?,
            round: round.clone(),
            turn_total: round,
            final_round: false,
        }))
    }

    fn big_limits(max_frames: usize) -> QueueLimits {
        QueueLimits {
            max_frames,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    async fn next_live(stream: &mut AttachmentStream) -> TestResult<(u32, SessionFrame)> {
        let envelope = stream.recv().await.ok_or("stream ended early")?;
        Ok((envelope.cursor.live, envelope.frame))
    }

    #[test]
    fn default_limits_match_the_contract() {
        let limits = QueueLimits::default();
        assert_eq!(limits.max_frames, 1024);
        assert_eq!(limits.max_bytes, 4 * 1024 * 1024);
    }

    #[tokio::test]
    async fn frames_are_delivered_in_fifo_order() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, QueueLimits::default());
        for live in 1..=5 {
            assert_eq!(
                queue.push(env(live, SessionFrame::Heartbeat)?),
                PushOutcome::Queued
            );
        }
        assert_eq!(queue.len(), 5);
        for live in 1..=5 {
            assert_eq!(next_live(&mut stream).await?.0, live);
        }
        assert!(queue.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn waiting_consumer_is_woken_by_push() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, QueueLimits::default());
        let consumer = tokio::spawn(async move { stream.recv().await.map(|e| e.cursor.live) });
        tokio::task::yield_now().await;
        assert_eq!(
            queue.push(env(7, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        assert_eq!(ctx(consumer.await, "join consumer")?, Some(7));
        Ok(())
    }

    /// BP-01: overflow by frame count ends the attachment with `Lagged`
    /// pointing at the oldest undelivered frame.
    #[tokio::test]
    async fn frame_overflow_lags_from_oldest_undelivered() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, big_limits(3));
        for live in 1..=3 {
            assert_eq!(
                queue.push(env(live, SessionFrame::Heartbeat)?),
                PushOutcome::Queued
            );
        }
        assert_eq!(next_live(&mut stream).await?.0, 1);
        assert_eq!(
            queue.push(env(4, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(5, SessionFrame::Heartbeat)?),
            PushOutcome::Lagged
        );
        assert!(queue.is_closed());
        assert_eq!(
            queue.push(env(6, SessionFrame::Heartbeat)?),
            PushOutcome::Closed
        );

        let (_, frame) = next_live(&mut stream).await?;
        check(
            matches!(frame, SessionFrame::Lagged { resume_from } if resume_from == at(2)),
            "expected Lagged resuming at live 2",
        )?;
        check(
            stream.recv().await.is_none(),
            "stream must end after Lagged",
        )
    }

    #[tokio::test]
    async fn byte_overflow_lags() -> TestResult {
        let size = frame_bytes(&env(1, SessionFrame::Heartbeat)?);
        let limits = QueueLimits {
            max_frames: 100,
            max_bytes: size.saturating_mul(3),
        };
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, limits);
        for live in 1..=3 {
            assert_eq!(
                queue.push(env(live, SessionFrame::Heartbeat)?),
                PushOutcome::Queued
            );
        }
        assert_eq!(
            queue.push(env(4, SessionFrame::Heartbeat)?),
            PushOutcome::Lagged
        );
        let (_, frame) = next_live(&mut stream).await?;
        check(
            matches!(frame, SessionFrame::Lagged { resume_from } if resume_from == at(1)),
            "expected Lagged resuming at live 1",
        )?;
        check(
            stream.recv().await.is_none(),
            "stream must end after Lagged",
        )
    }

    #[tokio::test]
    async fn empty_queue_overflow_resumes_from_incoming_frame() -> TestResult {
        let limits = QueueLimits {
            max_frames: 10,
            max_bytes: 1,
        };
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, limits);
        assert_eq!(
            queue.push(env(9, SessionFrame::Heartbeat)?),
            PushOutcome::Lagged
        );
        let (_, frame) = next_live(&mut stream).await?;
        check(
            matches!(frame, SessionFrame::Lagged { resume_from } if resume_from == at(9)),
            "expected Lagged resuming at live 9",
        )
    }

    #[tokio::test]
    async fn deltas_merge_only_above_half_fill() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, big_limits(4));
        assert_eq!(
            queue.push(env(1, assistant("t", "a")?)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(2, assistant("t", "b")?)?),
            PushOutcome::Queued
        );
        // Two of four: not above half yet.
        assert_eq!(
            queue.push(env(3, assistant("t", "c")?)?),
            PushOutcome::Queued
        );
        // Three of four: merge into the previous delta of the same turn.
        assert_eq!(
            queue.push(env(4, assistant("t", "d")?)?),
            PushOutcome::Coalesced
        );
        assert_eq!(
            queue.push(env(5, assistant("t", "e")?)?),
            PushOutcome::Coalesced
        );
        // Another turn never merges.
        assert_eq!(
            queue.push(env(6, assistant("u", "x")?)?),
            PushOutcome::Queued
        );
        assert_eq!(queue.len(), 4);

        let mut texts = Vec::new();
        for _ in 0..4 {
            let (live, frame) = next_live(&mut stream).await?;
            match frame {
                SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) => {
                    texts.push((live, text));
                }
                other => return Err(format!("unexpected frame {}", other.kind())),
            }
        }
        let expected = [(1, "a"), (2, "b"), (5, "cde"), (6, "x")];
        for ((live, text), (want_live, want_text)) in texts.iter().zip(expected) {
            assert_eq!(*live, want_live);
            assert_eq!(text, want_text);
        }
        Ok(())
    }

    #[tokio::test]
    async fn reasoning_deltas_merge_under_pressure_in_full_profile() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, big_limits(2));
        assert_eq!(
            queue.push(env(1, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(2, reasoning("t", "p")?)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(3, reasoning("t", "q")?)?),
            PushOutcome::Coalesced
        );
        next_live(&mut stream).await?;
        let (_, frame) = next_live(&mut stream).await?;
        check(
            matches!(frame, SessionFrame::Turn(TurnEvent::ReasoningDelta { ref text, .. }) if text == "pq"),
            "expected merged reasoning text",
        )
    }

    #[tokio::test]
    async fn usage_updates_keep_only_the_latest_under_pressure() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, big_limits(4));
        for live in 1..=3 {
            assert_eq!(
                queue.push(env(live, SessionFrame::Heartbeat)?),
                PushOutcome::Queued
            );
        }
        assert_eq!(queue.push(env(4, usage("t", 10)?)?), PushOutcome::Queued);
        assert_eq!(queue.push(env(5, usage("t", 20)?)?), PushOutcome::Coalesced);
        assert_eq!(queue.push(env(6, usage("t", 30)?)?), PushOutcome::Coalesced);
        assert_eq!(queue.len(), 4);
        for _ in 0..3 {
            next_live(&mut stream).await?;
        }
        let (live, frame) = next_live(&mut stream).await?;
        assert_eq!(live, 6);
        check(
            matches!(
                frame,
                SessionFrame::Turn(TurnEvent::UsageUpdated { ref round, .. })
                    if round.output_tokens == 30
            ),
            "expected the latest usage",
        )
    }

    #[tokio::test]
    async fn compact_profile_filters_and_coalesces() -> TestResult {
        let (queue, mut stream) =
            attachment(sid()?, StreamProfile::Compact, QueueLimits::default());
        assert_eq!(
            queue.push(env(1, reasoning("t", "r")?)?),
            PushOutcome::Filtered
        );
        let child_delta = SessionFrame::Child {
            agent: ctx(SessionId::try_from_str("child"), "child id")?,
            parent: Some(sid()?),
            role: "worker".into(),
            event: TurnEvent::AssistantDelta {
                turn_id: tid("c")?,
                text: "c".into(),
            },
        };
        assert_eq!(queue.push(env(2, child_delta)?), PushOutcome::Filtered);
        let progress = SessionFrame::Turn(TurnEvent::ChildProgress {
            turn_id: tid("t")?,
            child: ctx(SessionId::try_from_str("child"), "child id")?,
            tool_calls: 1,
            tokens: 2,
        });
        assert_eq!(queue.push(env(3, progress)?), PushOutcome::Filtered);
        assert!(queue.is_empty());

        // Assistant deltas always merge, even with an empty-ish queue.
        assert_eq!(
            queue.push(env(4, assistant("t", "x")?)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(5, assistant("t", "y")?)?),
            PushOutcome::Coalesced
        );
        // Usage always keeps only the latest.
        assert_eq!(queue.push(env(6, usage("t", 1)?)?), PushOutcome::Queued);
        assert_eq!(queue.push(env(7, usage("t", 2)?)?), PushOutcome::Coalesced);
        assert_eq!(queue.len(), 2);

        let (live, frame) = next_live(&mut stream).await?;
        assert_eq!(live, 5);
        check(
            matches!(frame, SessionFrame::Turn(TurnEvent::AssistantDelta { ref text, .. }) if text == "xy"),
            "expected merged assistant text",
        )?;
        let (live, frame) = next_live(&mut stream).await?;
        assert_eq!(live, 7);
        check(
            matches!(
                frame,
                SessionFrame::Turn(TurnEvent::UsageUpdated { ref round, .. })
                    if round.output_tokens == 2
            ),
            "expected the latest usage",
        )
    }

    #[tokio::test]
    async fn close_delivers_queued_then_final_then_ends() -> TestResult {
        let (queue, mut stream) = attachment(sid()?, StreamProfile::Full, QueueLimits::default());
        assert_eq!(
            queue.push(env(1, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        assert_eq!(
            queue.push(env(2, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        queue.close(Some(env(3, SessionFrame::Revoked)?));
        assert!(queue.is_closed());
        assert_eq!(
            queue.push(env(4, SessionFrame::Heartbeat)?),
            PushOutcome::Closed
        );

        assert_eq!(next_live(&mut stream).await?.0, 1);
        assert_eq!(next_live(&mut stream).await?.0, 2);
        let (live, frame) = next_live(&mut stream).await?;
        assert_eq!(live, 3);
        check(matches!(frame, SessionFrame::Revoked), "expected Revoked")?;
        check(stream.recv().await.is_none(), "stream must end after close")?;
        // Through the port trait as well.
        check(
            matches!(FrameSource::next(&mut stream).await, Ok(None)),
            "port source must end too",
        )
    }

    #[tokio::test]
    async fn dropping_the_stream_closes_the_queue() -> TestResult {
        let (queue, stream) = attachment(sid()?, StreamProfile::Full, QueueLimits::default());
        assert_eq!(
            queue.push(env(1, SessionFrame::Heartbeat)?),
            PushOutcome::Queued
        );
        drop(stream);
        assert!(queue.is_closed());
        assert!(queue.is_empty());
        assert_eq!(
            queue.push(env(2, SessionFrame::Heartbeat)?),
            PushOutcome::Closed
        );
        Ok(())
    }

    #[tokio::test]
    async fn producer_never_waits_on_a_full_queue() -> TestResult {
        // No consumer ever reads: every push must still return at once.
        let (queue, _stream) = attachment(sid()?, StreamProfile::Full, big_limits(4));
        let mut outcomes = Vec::new();
        for live in 0..2000 {
            outcomes.push(queue.push(env(live, SessionFrame::Heartbeat)?));
        }
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| **o == PushOutcome::Queued)
                .count(),
            4
        );
        assert_eq!(outcomes.get(4), Some(&PushOutcome::Lagged));
        assert!(outcomes.iter().skip(5).all(|o| *o == PushOutcome::Closed));
        assert_eq!(queue.len(), 1);
        Ok(())
    }
}

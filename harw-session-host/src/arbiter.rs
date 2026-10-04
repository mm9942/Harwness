//! Input arbitration (W00 §2.4 `arbiter.rs`, PL-65 §2.4, DEC-013).
//!
//! One arbiter per session. It is the only place that decides whether a
//! submission runs, waits or is refused:
//!
//! - **Single runner** (SES-03): at most one turn runs; [`Arbiter::next`]
//!   hands out nothing while a turn is running.
//! - **FIFO queue** with a cap ([`ArbiterLimits::queue_cap`]); a full queue
//!   answers `QueueFull` instead of growing.
//! - **Compare-and-swap on the head** (SES-02): every submit carries the
//!   head the client saw. If the durable head moved past it, the submit is
//!   `Stale` unless the client forces it. No lock lease.
//! - **Idempotency** (SES-01): a `client_msg_id` window per session makes a
//!   retried submit (for example after a reconnect) answer `Accepted` again
//!   without ever enqueueing the same input twice. The check runs before the
//!   CAS so a retry after the turn started is not reported `Stale`.

use std::collections::VecDeque;

use harw_protocol::Cursor;
use harw_protocol::session_wire::SubmitResult;

use crate::driver::TurnInput;
use crate::identity::ConnectionId;

/// Longest accepted submission text in bytes (256 KiB).
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

/// Longest accepted `client_msg_id` in bytes.
pub const MAX_CLIENT_MSG_ID_BYTES: usize = 128;

harw_types::limits_struct! {
    /// Bounds of one arbiter.
    #[derive(Clone, Copy, Debug)]
    pub struct ArbiterLimits {
        /// Most queued (not running) inputs.
        pub queue_cap: usize = DEFAULT_QUEUE_CAP = 8,
        /// How many recent `client_msg_id`s are remembered for idempotency.
        pub idempotency_window: usize = DEFAULT_IDEMPOTENCY_WINDOW = 256,
    }
}

/// Per-session FIFO with compare-and-swap and idempotency.
#[derive(Debug)]
pub struct Arbiter {
    limits: ArbiterLimits,
    queue: VecDeque<TurnInput>,
    /// `client_msg_id` of the running turn, if any.
    running: Option<String>,
    /// Recently accepted ids, oldest first (LRU order).
    seen: VecDeque<String>,
}

impl Arbiter {
    #[must_use]
    pub fn new(limits: ArbiterLimits) -> Self {
        Self {
            limits,
            queue: VecDeque::new(),
            running: None,
            seen: VecDeque::new(),
        }
    }

    /// Decide on one submission. Order of checks: input validity, duplicate
    /// `client_msg_id` (idempotent, before the CAS so a retried submit after
    /// the turn started is not reported `Stale`), then stale (unless
    /// `force`), then queue cap, then enqueue.
    ///
    /// Position: 0-based place counting a running turn (running + index in
    /// queue), i.e. first queued while idle = 0, while running = 1.
    pub fn submit(
        &mut self,
        input: TurnInput,
        expect_head: Cursor,
        force: bool,
        head: Cursor,
    ) -> SubmitResult {
        if let Err(reason) = validate(&input) {
            return SubmitResult::Denied { reason };
        }
        if let Some(position) = self.duplicate_position(&input.client_msg_id) {
            self.touch(&input.client_msg_id);
            return SubmitResult::Accepted { position };
        }
        if !force && head.is_durably_ahead_of(&expect_head) {
            return SubmitResult::Stale { head };
        }
        if self.queue.len() >= self.limits.queue_cap {
            return SubmitResult::QueueFull;
        }
        self.touch(&input.client_msg_id);
        self.queue.push_back(input);
        let index = self.queue.len().saturating_sub(1);
        SubmitResult::Accepted {
            position: self.position_of(index),
        }
    }

    /// Pop the next input if no turn is running; marks running.
    pub fn next_input(&mut self) -> Option<TurnInput> {
        if self.running.is_some() {
            return None;
        }
        let input = self.queue.pop_front()?;
        self.running = Some(input.client_msg_id.clone());
        Some(input)
    }

    /// The running turn ended.
    pub fn finish(&mut self) {
        self.running = None;
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Queued (not running) inputs.
    #[must_use]
    pub fn depth(&self) -> u32 {
        u32::try_from(self.queue.len()).unwrap_or(u32::MAX)
    }

    /// Drop queued inputs submitted by `origin` (revocation); returns how
    /// many. The running turn is not touched. Dropped ids stay in the
    /// idempotency window, so a late retry is not enqueued again.
    pub fn drop_origin(&mut self, origin: ConnectionId) -> usize {
        let before = self.queue.len();
        self.queue.retain(|input| input.origin != origin);
        before.saturating_sub(self.queue.len())
    }

    /// Drop everything queued (close); returns how many.
    pub fn clear(&mut self) -> usize {
        let dropped = self.queue.len();
        self.queue.clear();
        dropped
    }

    /// Position of a known `client_msg_id`: its queue position while still
    /// queued, 0 when it runs or already ran. `None` for an unknown id.
    fn duplicate_position(&self, id: &str) -> Option<u32> {
        if let Some(index) = self.queue.iter().position(|q| q.client_msg_id == id) {
            return Some(self.position_of(index));
        }
        if self.running.as_deref() == Some(id) || self.seen.iter().any(|s| s == id) {
            return Some(0);
        }
        None
    }

    fn position_of(&self, index: usize) -> u32 {
        let offset = usize::from(self.running.is_some());
        u32::try_from(index.saturating_add(offset)).unwrap_or(u32::MAX)
    }

    /// Mark `id` as most recently used; evict the oldest beyond the window.
    fn touch(&mut self, id: &str) {
        if let Some(index) = self.seen.iter().position(|s| s == id) {
            self.seen.remove(index);
        }
        self.seen.push_back(id.to_owned());
        while self.seen.len() > self.limits.idempotency_window {
            self.seen.pop_front();
        }
    }
}

impl Default for Arbiter {
    fn default() -> Self {
        Self::new(ArbiterLimits::default())
    }
}

fn validate(input: &TurnInput) -> Result<(), String> {
    if input.text.trim().is_empty() {
        return Err("empty submission".into());
    }
    if input.text.len() > MAX_TEXT_BYTES {
        return Err(format!("submission exceeds {MAX_TEXT_BYTES} bytes"));
    }
    if input.client_msg_id.is_empty() {
        return Err("missing client_msg_id".into());
    }
    if input.client_msg_id.len() > MAX_CLIENT_MSG_ID_BYTES {
        return Err(format!(
            "client_msg_id exceeds {MAX_CLIENT_MSG_ID_BYTES} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use harw_types::{ApprovalActor, InvalidId, SessionId};

    use super::*;

    type TestResult = Result<(), InvalidId>;

    fn input(id: &str, origin: ConnectionId) -> Result<TurnInput, InvalidId> {
        Ok(TurnInput {
            session_id: SessionId::try_from_str("s1")?,
            text: "hello".into(),
            client_msg_id: id.into(),
            submitted_by: "test".into(),
            actor: ApprovalActor::Operator { id: "uid:1".into() },
            origin,
        })
    }

    fn at(durable: u64) -> Cursor {
        Cursor {
            generation: 1,
            durable,
            live: 0,
        }
    }

    fn submit(arbiter: &mut Arbiter, id: &str) -> Result<SubmitResult, InvalidId> {
        Ok(arbiter.submit(input(id, ConnectionId(1))?, at(0), false, at(0)))
    }

    #[test]
    fn positions_count_the_running_turn() -> TestResult {
        let mut arbiter = Arbiter::default();
        assert_eq!(
            submit(&mut arbiter, "a")?,
            SubmitResult::Accepted { position: 0 }
        );
        assert_eq!(
            submit(&mut arbiter, "b")?,
            SubmitResult::Accepted { position: 1 }
        );
        assert!(arbiter.next_input().is_some());
        assert_eq!(arbiter.depth(), 1);
        // Running "a", queued "b" at 1, new "c" at 2.
        assert_eq!(
            submit(&mut arbiter, "c")?,
            SubmitResult::Accepted { position: 2 }
        );
        arbiter.finish();
        assert!(arbiter.next_input().is_some());
        arbiter.finish();
        assert!(arbiter.next_input().is_some());
        assert_eq!(
            submit(&mut arbiter, "d")?,
            SubmitResult::Accepted { position: 1 }
        );
        Ok(())
    }

    #[test]
    fn duplicate_id_returns_same_position_and_does_not_enqueue() -> TestResult {
        // SES-01
        let mut arbiter = Arbiter::default();
        assert_eq!(
            submit(&mut arbiter, "a")?,
            SubmitResult::Accepted { position: 0 }
        );
        assert_eq!(
            submit(&mut arbiter, "b")?,
            SubmitResult::Accepted { position: 1 }
        );
        assert_eq!(
            submit(&mut arbiter, "b")?,
            SubmitResult::Accepted { position: 1 }
        );
        assert_eq!(arbiter.depth(), 2);
        Ok(())
    }

    #[test]
    fn duplicate_after_it_ran_is_accepted_not_stale() -> TestResult {
        let mut arbiter = Arbiter::default();
        assert_eq!(
            submit(&mut arbiter, "a")?,
            SubmitResult::Accepted { position: 0 }
        );
        assert!(arbiter.next_input().is_some());
        // Head moved while running; a retry with the old expect_head.
        let retry = arbiter.submit(input("a", ConnectionId(1))?, at(0), false, at(5));
        assert_eq!(retry, SubmitResult::Accepted { position: 0 });
        arbiter.finish();
        let retry = arbiter.submit(input("a", ConnectionId(1))?, at(0), false, at(9));
        assert_eq!(retry, SubmitResult::Accepted { position: 0 });
        assert_eq!(arbiter.depth(), 0);
        assert!(arbiter.next_input().is_none());
        Ok(())
    }

    #[test]
    fn moved_head_is_stale() -> TestResult {
        // SES-02
        let mut arbiter = Arbiter::default();
        let result = arbiter.submit(input("a", ConnectionId(1))?, at(3), false, at(4));
        assert_eq!(result, SubmitResult::Stale { head: at(4) });
        assert_eq!(arbiter.depth(), 0);
        // Same head is fine.
        let result = arbiter.submit(input("a", ConnectionId(1))?, at(4), false, at(4));
        assert_eq!(result, SubmitResult::Accepted { position: 0 });
        Ok(())
    }

    #[test]
    fn force_bypasses_stale() -> TestResult {
        let mut arbiter = Arbiter::default();
        let result = arbiter.submit(input("a", ConnectionId(1))?, at(3), true, at(4));
        assert_eq!(result, SubmitResult::Accepted { position: 0 });
        Ok(())
    }

    #[test]
    fn full_queue_is_refused() -> TestResult {
        let mut arbiter = Arbiter::new(ArbiterLimits {
            queue_cap: 2,
            idempotency_window: 256,
        });
        submit(&mut arbiter, "a")?;
        submit(&mut arbiter, "b")?;
        assert_eq!(submit(&mut arbiter, "c")?, SubmitResult::QueueFull);
        assert_eq!(arbiter.depth(), 2);
        // A refused id was not recorded: it is accepted once there is room.
        assert!(arbiter.next_input().is_some());
        assert_eq!(
            submit(&mut arbiter, "c")?,
            SubmitResult::Accepted { position: 2 }
        );
        Ok(())
    }

    #[test]
    fn invalid_input_is_denied() -> TestResult {
        let mut arbiter = Arbiter::default();
        let mut blank = input("a", ConnectionId(1))?;
        blank.text = "  \n\t".into();
        let mut huge = input("b", ConnectionId(1))?;
        huge.text = "x".repeat(MAX_TEXT_BYTES + 1);
        let no_id = input("", ConnectionId(1))?;
        let long_id = input(&"i".repeat(MAX_CLIENT_MSG_ID_BYTES + 1), ConnectionId(1))?;
        for bad in [blank, huge, no_id, long_id] {
            assert!(matches!(
                arbiter.submit(bad, at(0), false, at(0)),
                SubmitResult::Denied { .. }
            ));
        }
        assert_eq!(arbiter.depth(), 0);
        Ok(())
    }

    #[test]
    fn drop_origin_removes_only_that_connections_inputs() -> TestResult {
        let mut arbiter = Arbiter::default();
        let (mine, other) = (ConnectionId(1), ConnectionId(2));
        arbiter.submit(input("a", mine)?, at(0), false, at(0));
        arbiter.submit(input("b", other)?, at(0), false, at(0));
        arbiter.submit(input("c", mine)?, at(0), false, at(0));
        assert_eq!(arbiter.drop_origin(mine), 2);
        assert_eq!(arbiter.depth(), 1);
        assert_eq!(
            arbiter.next_input().map(|i| i.client_msg_id),
            Some("b".to_owned())
        );
        assert_eq!(arbiter.clear(), 0);
        Ok(())
    }

    #[test]
    fn clear_drops_everything_queued() -> TestResult {
        let mut arbiter = Arbiter::default();
        submit(&mut arbiter, "a")?;
        submit(&mut arbiter, "b")?;
        assert!(arbiter.next_input().is_some());
        assert_eq!(arbiter.clear(), 1);
        assert!(arbiter.is_running());
        assert_eq!(arbiter.depth(), 0);
        Ok(())
    }

    #[test]
    fn idempotency_window_evicts_oldest_ids() -> TestResult {
        let mut arbiter = Arbiter::default();
        for n in 0..=256 {
            submit(&mut arbiter, &format!("m{n}"))?;
            assert!(arbiter.next_input().is_some());
            arbiter.finish();
        }
        // "m0" fell out of the 256-id window: submitting it again enqueues.
        assert_eq!(
            submit(&mut arbiter, "m0")?,
            SubmitResult::Accepted { position: 0 }
        );
        assert_eq!(arbiter.depth(), 1);
        // "m256" is still remembered.
        assert_eq!(
            submit(&mut arbiter, "m256")?,
            SubmitResult::Accepted { position: 0 }
        );
        assert_eq!(arbiter.depth(), 1);
        Ok(())
    }

    #[test]
    fn next_keeps_a_single_runner() -> TestResult {
        // SES-03
        let mut arbiter = Arbiter::default();
        submit(&mut arbiter, "a")?;
        submit(&mut arbiter, "b")?;
        assert!(!arbiter.is_running());
        assert_eq!(
            arbiter.next_input().map(|i| i.client_msg_id),
            Some("a".to_owned())
        );
        assert!(arbiter.is_running());
        assert!(arbiter.next_input().is_none());
        arbiter.finish();
        assert!(!arbiter.is_running());
        assert_eq!(
            arbiter.next_input().map(|i| i.client_msg_id),
            Some("b".to_owned())
        );
        assert!(arbiter.next_input().is_none());
        arbiter.finish();
        assert!(arbiter.next_input().is_none());
        Ok(())
    }
}

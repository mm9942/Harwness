//! Lease-local attempt ownership. The revocation fence precedes any export of a
//! completed model result; even a retained native reference cannot re-admit.
use harw_types::cancel::{CancelReason, CancelToken};
use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use crate::uia_room_store::{RoomActorKind, RoomPublish, RoomResponseStamp};

const MAX_PUBLICATION_WINDOW: Duration = Duration::from_secs(30);
const MAX_HIDDEN_RESPONSE: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Admission {
    Locked,
    Busy,
    Invalid,
}
struct Attempt {
    id: i64,
    publication: Arc<AttemptPublication>,
    running: bool,
}
#[derive(Default)]
struct State {
    revoked: bool,
    parked: bool,
    last: i64,
    attempt: Option<Attempt>,
}
#[derive(Default)]
pub(crate) struct RoomOperations(Mutex<State>);

impl RoomOperations {
    pub(crate) fn active(&self) -> bool {
        self.0.lock().ok().is_some_and(|s| !s.revoked && !s.parked)
    }
    pub(crate) fn reserve(&self, id: i64) -> Result<(), Admission> {
        let mut s = self.0.lock().map_err(|_| Admission::Locked)?;
        if s.revoked || s.parked {
            return Err(Admission::Locked);
        }
        if id <= s.last {
            return Err(Admission::Invalid);
        }
        if s.attempt.is_some() {
            return Err(Admission::Busy);
        }
        s.last = id;
        s.attempt = Some(Attempt {
            id,
            publication: Arc::new(AttemptPublication::new()),
            running: false,
        });
        Ok(())
    }
    pub(crate) fn take(&self, id: i64) -> Result<CancelToken, Admission> {
        let mut s = self.0.lock().map_err(|_| Admission::Locked)?;
        if s.revoked || s.parked {
            return Err(Admission::Locked);
        }
        let job = s
            .attempt
            .as_mut()
            .filter(|x| x.id == id)
            .ok_or(Admission::Invalid)?;
        if job.running {
            return Err(Admission::Busy);
        }
        job.running = true;
        Ok(job.publication.cancel.clone())
    }

    /// Only the trusted native worker for this already-taken attempt can
    /// obtain its write gate. This object does not reference the vault,
    /// room, profile, registry, or any decryption key.
    pub(crate) fn publication(&self, id: i64) -> Option<Arc<dyn RoomPublish>> {
        let s = self.0.lock().ok()?;
        if s.revoked || s.parked { return None; }
        let job = s.attempt.as_ref().filter(|x| x.id == id && x.running)?;
        Some(job.publication.clone())
    }

    pub(crate) fn finish(&self, id: i64) -> bool {
        let Ok(mut s) = self.0.lock() else {
            return false;
        };
        if s.revoked || !s.attempt.as_ref().is_some_and(|x| x.id == id) {
            return false;
        }
        if let Some(job) = s.attempt.take() {
            job.publication.close();
        }
        true
    }

    pub(crate) fn cancel(&self, id: i64) -> bool {
        let Ok(s) = self.0.lock() else { return false; };
        if s.revoked || s.parked { return false; }
        s.attempt.as_ref().filter(|job| job.id == id)
            .is_some_and(|job| job.publication.cancel(CancelReason::User))
    }

    /// Does not permit a second admission for this lease. The returned
    /// ticket is only a timed cancellation route, not a sealer or reader.
    pub(crate) fn park(&self, id: i64) -> Option<ParkedExpiry> {
        let mut s = self.0.lock().ok()?;
        if s.revoked || s.parked { return None; }
        let job = s.attempt.as_ref().filter(|job| job.id == id && job.running)?;
        let deadline = job.publication.park()?;
        let gate = job.publication.clone();
        s.parked = true;
        Some(ParkedExpiry { gate, deadline })
    }

    pub(crate) fn cancel_parked(&self, id: i64) -> bool {
        let Ok(s) = self.0.lock() else { return false; };
        if !s.parked { return false; }
        s.attempt.as_ref().filter(|job| job.id == id)
            .is_some_and(|job| job.publication.cancel(CancelReason::User))
    }

    pub(crate) fn revoke(&self) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.revoked = true;
        if let Some(job) = s.attempt.take() {
            job.publication.revoke();
        }
    }
}

/// Held only by the native deadline task. No profile, vault, room reader,
/// sealer, model input, or reusable operation.
pub(crate) struct ParkedExpiry {
    gate: Arc<AttemptPublication>,
    deadline: Instant,
}

impl ParkedExpiry {
    pub(crate) async fn run(self) {
        tokio::time::sleep_until(self.deadline.into()).await;
        self.gate.cancel(CancelReason::Budget);
    }
}

/// Exact-attempt cancellation/publication fence; separating it from
/// `RoomOperations` prevents a publisher from owning new-turn admission.
struct AttemptPublication {
    state: Mutex<PublicationState>,
    cancel: CancelToken,
}

#[derive(Default)]
struct PublicationState {
    phase: Phase,
    generation_observed: bool,
    parked_deadline: Option<Instant>,
}

#[derive(Clone, Copy, Default)]
enum Phase {
    #[default]
    WaitingForEngine,
    Generating { attempt: blake3::Hash, kind: RoomActorKind },
    ResponseReady(RoomResponseStamp),
    Armed(RoomResponseStamp, Instant),
    Published,
    Closed,
}

impl AttemptPublication {
    fn new() -> Self {
        Self { state: Mutex::new(PublicationState::default()), cancel: CancelToken::new() }
    }

    fn park(&self) -> Option<Instant> {
        let mut state = self.state.lock().ok()?;
        if !state.generation_observed || state.parked_deadline.is_some() ||
            !matches!(state.phase, Phase::Generating { .. } | Phase::ResponseReady(_) | Phase::Armed(..)) ||
            !self.within_budget(&mut state)
        {
            return None;
        }
        let deadline = Instant::now().checked_add(MAX_HIDDEN_RESPONSE)?;
        state.parked_deadline = Some(deadline);
        Some(deadline)
    }

    fn within_budget(&self, state: &mut PublicationState) -> bool {
        if state.parked_deadline.is_some_and(|until| Instant::now() >= until) {
            state.phase = Phase::Closed;
            self.cancel.cancel(CancelReason::Budget);
            return false;
        }
        !self.cancel.is_cancelled()
    }

    fn is_live_locked(&self, state: &mut PublicationState, stamp: RoomResponseStamp) -> bool {
        let Phase::Armed(expected, deadline) = state.phase else { return false; };
        if expected != stamp { return false; }
        if !self.within_budget(state) || Instant::now() >= deadline {
            state.phase = Phase::Closed;
            self.cancel.cancel(CancelReason::Budget);
            return false;
        }
        true
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(state.phase, Phase::Published) {
            state.phase = Phase::Closed;
        }
    }

    fn cancel(&self, reason: CancelReason) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        if matches!(state.phase, Phase::Closed | Phase::Published) {
            return false;
        }
        state.phase = Phase::Closed;
        self.cancel.cancel(reason);
        true
    }

    fn revoke(&self) {
        self.close();
        // Any actual late rename is independently reconciled on fresh unlock.
        self.cancel.cancel(CancelReason::LeaseLost);
    }
}

impl RoomPublish for AttemptPublication {
    fn begin_response(&self, attempt: blake3::Hash, kind: RoomActorKind) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        if !matches!(state.phase, Phase::WaitingForEngine) || self.cancel.is_cancelled() {
            return false;
        }
        state.phase = Phase::Generating { attempt, kind };
        true
    }

    fn generation_observed(&self, attempt: blake3::Hash, kind: RoomActorKind) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        if !matches!(state.phase, Phase::Generating { attempt: expected, kind: exact }
                    if attempt == expected && kind == exact) || !self.within_budget(&mut state)
        {
            return false;
        }
        state.generation_observed = true;
        true
    }

    fn accept_response(&self, stamp: RoomResponseStamp) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        // A legitimate complete foreground response can have no emitted
        // delta. Never turn that outcome into proof of park eligibility.
        if !matches!(state.phase, Phase::Generating { attempt, .. } if attempt == stamp.attempt) ||
            !self.within_budget(&mut state)
        {
            return false;
        }
        state.phase = Phase::ResponseReady(stamp);
        true
    }

    fn abandon_response(&self, attempt: blake3::Hash) {
        let Ok(mut state) = self.state.lock() else { return; };
        let cancel_inference = match state.phase {
            Phase::Generating { attempt: expected, .. } if expected == attempt => true,
            Phase::ResponseReady(stamp) | Phase::Armed(stamp, _)
                if stamp.attempt == attempt => false,
            _ => return,
        };
        state.phase = Phase::Closed;
        if cancel_inference {
            self.cancel.cancel(CancelReason::Shutdown);
        }
    }

    fn arm(&self, stamp: RoomResponseStamp) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        if !matches!(state.phase, Phase::ResponseReady(expected) if expected == stamp) ||
            !self.within_budget(&mut state)
        {
            return false;
        }
        let Some(deadline) = Instant::now().checked_add(MAX_PUBLICATION_WINDOW) else {
            state.phase = Phase::Closed;
            return false;
        };
        state.phase = Phase::Armed(stamp, deadline);
        true
    }

    fn is_live(&self, stamp: RoomResponseStamp) -> bool {
        let Ok(mut state) = self.state.lock() else { return false; };
        self.is_live_locked(&mut state, stamp)
    }

    fn publish(&self, stamp: RoomResponseStamp, rename: &mut dyn FnMut() -> io::Result<()>) -> io::Result<bool> {
        let Ok(mut state) = self.state.lock() else { return Ok(false); };
        if !self.is_live_locked(&mut state, stamp) { return Ok(false); }
        // Consume before the single syscall, even if rename fails. Neither
        // temp writes nor directory fsync run under the cancellation lock.
        state.phase = Phase::Closed;
        rename()?;
        state.phase = Phase::Published;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_model_return_is_fenced_after_relock() {
        let ops = RoomOperations::default();
        assert!(ops.reserve(1).is_ok());
        let active = ops.take(1).expect("admitted");
        ops.revoke();
        assert!(active.is_cancelled());
        assert!(!ops.finish(1));
        assert!(ops.reserve(2).is_err());
    }

    #[test]
    fn cancelling_one_attempt_cannot_touch_other_turns() {
        let ops = RoomOperations::default();
        ops.reserve(1).expect("first");
        assert!(ops.cancel(1));
        assert!(ops.take(1).expect("first was captured").is_cancelled());
        assert!(ops.finish(1));
        ops.reserve(2).expect("next");
        assert!(!ops.cancel(1));
        let token = ops.take(2).expect("next token");
        assert!(!token.is_cancelled());
        assert!(ops.take(2).is_err());
    }
}

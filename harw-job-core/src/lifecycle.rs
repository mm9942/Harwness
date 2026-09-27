//! Validated lifecycle state machine of one job attempt (Job-Runtime-Doc §6).
//!
//! ```text
//! Created ─Enqueue→ Queued ─Claim→ Claimed ─Start→ Starting ─Spawned→ Running
//!                     ↑              │                                  ├─Succeed→ Succeeded
//!                     └──Release─────┘                                  ├─Fail───→ Failed
//!                                                                       ├─Cancel─→ Cancelled
//!                                                                       ├─TimeOut→ TimedOut
//!                                                                       └─LoseLease→ Lost
//! Failed | TimedOut | Lost ─Retry→ Queued
//! ```
//!
//! The complete table is [`LifecycleState::transition`]; every pair not
//! listed there is rejected with a [`TransitionError`]. Persistence code must
//! not write arbitrary states: it records a [`LifecycleTransition`], which can
//! only be obtained (and deserialized) through that table.
//!
//! Terminal states absorb nothing. The only way out of a terminal state is the
//! explicit [`LifecycleEvent::Retry`] from a retryable outcome
//! ([`LifecycleState::is_retryable`]); incrementing the attempt counter and
//! consulting the retry policy is the caller's job.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Lifecycle state of one job attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Recorded, not yet eligible to run.
    Created,
    /// Eligible to be claimed by a runner.
    Queued,
    /// A runner holds a lease; nothing has been started yet.
    Claimed,
    /// The runner is preparing and spawning the process tree.
    Starting,
    /// The primary process is live.
    Running,
    /// Finished successfully. Final.
    Succeeded,
    /// Finished unsuccessfully. Retryable.
    Failed,
    /// Cancelled on purpose. Final.
    Cancelled,
    /// A deadline elapsed. Retryable.
    TimedOut,
    /// The lease holder vanished; the outcome is unknown. Retryable.
    Lost,
}

impl LifecycleState {
    /// Every state, in declaration order.
    pub const ALL: [Self; 10] = [
        Self::Created,
        Self::Queued,
        Self::Claimed,
        Self::Starting,
        Self::Running,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::TimedOut,
        Self::Lost,
    ];

    /// Whether the attempt has ended. A terminal state accepts no event
    /// except [`LifecycleEvent::Retry`] from a retryable state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut | Self::Lost => true,
            Self::Created | Self::Queued | Self::Claimed | Self::Starting | Self::Running => false,
        }
    }

    /// Whether the terminal state admits an explicit retry.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed | Self::TimedOut | Self::Lost)
    }

    /// Whether a runner holds (or held) the attempt's lease in this state.
    #[must_use]
    pub const fn holds_lease(self) -> bool {
        matches!(self, Self::Claimed | Self::Starting | Self::Running)
    }

    /// The single transition table of the job model.
    ///
    /// # Errors
    /// [`TransitionError`] if `event` is not permitted in `self`.
    pub const fn transition(self, event: LifecycleEvent) -> Result<Self, TransitionError> {
        use LifecycleEvent as E;
        use LifecycleState as S;

        let next = match (self, event) {
            (S::Created, E::Enqueue) => S::Queued,
            (S::Queued, E::Claim) => S::Claimed,
            (S::Claimed, E::Release) => S::Queued,
            (S::Claimed, E::Start) => S::Starting,
            (S::Starting, E::Spawned) => S::Running,
            (S::Running, E::Succeed) => S::Succeeded,
            (S::Starting | S::Running, E::Fail) => S::Failed,
            (S::Created | S::Queued | S::Claimed | S::Starting | S::Running, E::Cancel) => {
                S::Cancelled
            }
            (S::Queued | S::Claimed | S::Starting | S::Running, E::TimeOut) => S::TimedOut,
            (S::Claimed | S::Starting | S::Running, E::LoseLease) => S::Lost,
            (S::Failed | S::TimedOut | S::Lost, E::Retry) => S::Queued,
            (from, event) => return Err(TransitionError { from, event }),
        };
        Ok(next)
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Created => "created",
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Lost => "lost",
        };
        f.write_str(name)
    }
}

/// An event that drives the lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// Make a created job eligible to run.
    Enqueue,
    /// A runner acquired the lease.
    Claim,
    /// The runner returned the lease before starting anything.
    Release,
    /// The runner began preparing the execution.
    Start,
    /// The primary process was spawned.
    Spawned,
    /// The attempt finished successfully.
    Succeed,
    /// The attempt failed (including spawn failure).
    Fail,
    /// The attempt was cancelled (see `CancellationCause`).
    Cancel,
    /// A deadline elapsed.
    TimeOut,
    /// The lease expired or was fenced off; the holder is gone.
    LoseLease,
    /// Explicit retry of a retryable outcome; the caller increments the
    /// attempt counter.
    Retry,
}

impl LifecycleEvent {
    /// Every event, in declaration order.
    pub const ALL: [Self; 11] = [
        Self::Enqueue,
        Self::Claim,
        Self::Release,
        Self::Start,
        Self::Spawned,
        Self::Succeed,
        Self::Fail,
        Self::Cancel,
        Self::TimeOut,
        Self::LoseLease,
        Self::Retry,
    ];
}

impl fmt::Display for LifecycleEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Enqueue => "enqueue",
            Self::Claim => "claim",
            Self::Release => "release",
            Self::Start => "start",
            Self::Spawned => "spawned",
            Self::Succeed => "succeed",
            Self::Fail => "fail",
            Self::Cancel => "cancel",
            Self::TimeOut => "time_out",
            Self::LoseLease => "lose_lease",
            Self::Retry => "retry",
        };
        f.write_str(name)
    }
}

/// A rejected lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    /// State the event was applied to.
    pub from: LifecycleState,
    /// The rejected event.
    pub event: LifecycleEvent,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lifecycle event '{}' is not permitted in state '{}'",
            self.event, self.from
        )
    }
}

impl std::error::Error for TransitionError {}

/// A validated transition record. It can only be built through
/// [`LifecycleState::transition`], and deserialization re-validates it, so a
/// persisted record can never carry an arbitrary state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawTransition")]
pub struct LifecycleTransition {
    from: LifecycleState,
    event: LifecycleEvent,
    to: LifecycleState,
}

impl LifecycleTransition {
    /// Applies `event` to `from`.
    ///
    /// # Errors
    /// [`TransitionError`] if the table does not permit the pair.
    pub const fn apply(
        from: LifecycleState,
        event: LifecycleEvent,
    ) -> Result<Self, TransitionError> {
        match from.transition(event) {
            Ok(to) => Ok(Self { from, event, to }),
            Err(error) => Err(error),
        }
    }

    /// State before the event.
    #[must_use]
    pub const fn source_state(self) -> LifecycleState {
        self.from
    }

    /// The applied event.
    #[must_use]
    pub const fn event(self) -> LifecycleEvent {
        self.event
    }

    /// State after the event.
    #[must_use]
    pub const fn target_state(self) -> LifecycleState {
        self.to
    }
}

/// Wire form of [`LifecycleTransition`] before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransition {
    from: LifecycleState,
    event: LifecycleEvent,
    to: LifecycleState,
}

/// Error for a persisted transition that contradicts the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransitionRecord {
    /// Recorded source state.
    pub from: LifecycleState,
    /// Recorded event.
    pub event: LifecycleEvent,
    /// Recorded target state.
    pub to: LifecycleState,
}

impl fmt::Display for InvalidTransitionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "recorded transition '{}' --{}--> '{}' contradicts the lifecycle table",
            self.from, self.event, self.to
        )
    }
}

impl std::error::Error for InvalidTransitionRecord {}

impl TryFrom<RawTransition> for LifecycleTransition {
    type Error = InvalidTransitionRecord;

    fn try_from(raw: RawTransition) -> Result<Self, Self::Error> {
        let error = InvalidTransitionRecord {
            from: raw.from,
            event: raw.event,
            to: raw.to,
        };
        match Self::apply(raw.from, raw.event) {
            Ok(transition) if transition.to == raw.to => Ok(transition),
            Ok(_) | Err(_) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LifecycleEvent, LifecycleState, LifecycleTransition, TransitionError};
    use crate::test_support::{TestError, TestResult, ctx};

    use LifecycleEvent as E;
    use LifecycleState as S;

    /// Independent restatement of the transition table. Every pair not
    /// listed here must be rejected.
    const ALLOWED: &[(LifecycleState, LifecycleEvent, LifecycleState)] = &[
        (S::Created, E::Enqueue, S::Queued),
        (S::Created, E::Cancel, S::Cancelled),
        (S::Queued, E::Claim, S::Claimed),
        (S::Queued, E::Cancel, S::Cancelled),
        (S::Queued, E::TimeOut, S::TimedOut),
        (S::Claimed, E::Release, S::Queued),
        (S::Claimed, E::Start, S::Starting),
        (S::Claimed, E::Cancel, S::Cancelled),
        (S::Claimed, E::TimeOut, S::TimedOut),
        (S::Claimed, E::LoseLease, S::Lost),
        (S::Starting, E::Spawned, S::Running),
        (S::Starting, E::Fail, S::Failed),
        (S::Starting, E::Cancel, S::Cancelled),
        (S::Starting, E::TimeOut, S::TimedOut),
        (S::Starting, E::LoseLease, S::Lost),
        (S::Running, E::Succeed, S::Succeeded),
        (S::Running, E::Fail, S::Failed),
        (S::Running, E::Cancel, S::Cancelled),
        (S::Running, E::TimeOut, S::TimedOut),
        (S::Running, E::LoseLease, S::Lost),
        (S::Failed, E::Retry, S::Queued),
        (S::TimedOut, E::Retry, S::Queued),
        (S::Lost, E::Retry, S::Queued),
    ];

    fn expected(from: LifecycleState, event: LifecycleEvent) -> Option<LifecycleState> {
        ALLOWED
            .iter()
            .find(|(f, e, _)| *f == from && *e == event)
            .map(|(_, _, to)| *to)
    }

    /// Compile-time exhaustiveness: adding a variant breaks these matches,
    /// forcing `ALL` (and this test) to be updated.
    fn state_index(state: LifecycleState) -> usize {
        match state {
            S::Created => 0,
            S::Queued => 1,
            S::Claimed => 2,
            S::Starting => 3,
            S::Running => 4,
            S::Succeeded => 5,
            S::Failed => 6,
            S::Cancelled => 7,
            S::TimedOut => 8,
            S::Lost => 9,
        }
    }

    fn event_index(event: LifecycleEvent) -> usize {
        match event {
            E::Enqueue => 0,
            E::Claim => 1,
            E::Release => 2,
            E::Start => 3,
            E::Spawned => 4,
            E::Succeed => 5,
            E::Fail => 6,
            E::Cancel => 7,
            E::TimeOut => 8,
            E::LoseLease => 9,
            E::Retry => 10,
        }
    }

    #[test]
    fn all_constants_list_every_variant_exactly_once() {
        for (index, state) in S::ALL.iter().enumerate() {
            assert_eq!(state_index(*state), index, "{state}");
        }
        for (index, event) in E::ALL.iter().enumerate() {
            assert_eq!(event_index(*event), index, "{event}");
        }
    }

    #[test]
    fn every_state_event_pair_matches_the_table() {
        let mut accepted = 0usize;
        for from in S::ALL {
            for event in E::ALL {
                let actual = from.transition(event);
                match expected(from, event) {
                    Some(to) => {
                        assert_eq!(actual, Ok(to), "{from} --{event}-->");
                        accepted += 1;
                    }
                    None => assert_eq!(
                        actual,
                        Err(TransitionError { from, event }),
                        "{from} --{event}--> must be rejected"
                    ),
                }
            }
        }
        assert_eq!(accepted, ALLOWED.len(), "ALLOWED has duplicate rows");
    }

    #[test]
    fn terminal_states_absorb_nothing_but_explicit_retry() {
        for from in S::ALL.into_iter().filter(|state| state.is_terminal()) {
            for event in E::ALL {
                let result = from.transition(event);
                if event == E::Retry && from.is_retryable() {
                    assert_eq!(result, Ok(S::Queued), "{from} retry");
                } else {
                    assert!(result.is_err(), "{from} --{event}--> must be rejected");
                }
            }
        }
    }

    #[test]
    fn terminal_and_retryable_classification() {
        let terminal = [S::Succeeded, S::Failed, S::Cancelled, S::TimedOut, S::Lost];
        let retryable = [S::Failed, S::TimedOut, S::Lost];
        let leased = [S::Claimed, S::Starting, S::Running];
        for state in S::ALL {
            assert_eq!(state.is_terminal(), terminal.contains(&state), "{state}");
            assert_eq!(state.is_retryable(), retryable.contains(&state), "{state}");
            assert_eq!(state.holds_lease(), leased.contains(&state), "{state}");
            assert!(!state.is_retryable() || state.is_terminal());
        }
    }

    #[test]
    fn lost_is_only_reachable_while_the_lease_is_held() {
        for from in S::ALL {
            let reaches_lost = E::ALL
                .into_iter()
                .any(|event| from.transition(event) == Ok(S::Lost));
            assert_eq!(reaches_lost, from.holds_lease(), "{from}");
        }
    }

    #[test]
    fn every_state_is_reachable_from_created() {
        let mut seen = vec![S::Created];
        let mut frontier = vec![S::Created];
        while let Some(state) = frontier.pop() {
            for event in E::ALL {
                let Ok(next) = state.transition(event) else {
                    continue;
                };
                if !seen.contains(&next) {
                    seen.push(next);
                    frontier.push(next);
                }
            }
        }
        for state in S::ALL {
            assert!(seen.contains(&state), "{state} unreachable");
        }
    }

    #[test]
    fn happy_path_and_retry_path() -> TestResult {
        let mut state = S::Created;
        for event in [E::Enqueue, E::Claim, E::Start, E::Spawned, E::Fail] {
            state = state.transition(event).map_err(ctx("happy path"))?;
        }
        assert_eq!(state, S::Failed);
        state = state.transition(E::Retry).map_err(ctx("retry"))?;
        assert_eq!(state, S::Queued);
        for event in [E::Claim, E::Start, E::Spawned, E::Succeed] {
            state = state.transition(event).map_err(ctx("second attempt"))?;
        }
        assert_eq!(state, S::Succeeded);
        Ok(())
    }

    #[test]
    fn transition_record_is_validated_on_construction_and_deserialization() -> TestResult {
        let transition =
            LifecycleTransition::apply(S::Running, E::Succeed).map_err(ctx("apply"))?;
        assert_eq!(transition.source_state(), S::Running);
        assert_eq!(transition.event(), E::Succeed);
        assert_eq!(transition.target_state(), S::Succeeded);

        let json = serde_json::to_string(&transition).map_err(ctx("serialize"))?;
        assert_eq!(
            json,
            r#"{"from":"running","event":"succeed","to":"succeeded"}"#
        );
        let back: LifecycleTransition = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, transition);

        // A record whose target contradicts the table is refused.
        let forged = r#"{"from":"running","event":"succeed","to":"failed"}"#;
        let Err(error) = serde_json::from_str::<LifecycleTransition>(forged) else {
            return Err(TestError::Unexpected("forged target accepted".into()));
        };
        assert!(error.to_string().contains("contradicts"), "{error}");

        // A record whose pair is not in the table is refused.
        let illegal = r#"{"from":"succeeded","event":"retry","to":"queued"}"#;
        assert!(serde_json::from_str::<LifecycleTransition>(illegal).is_err());

        assert_eq!(
            LifecycleTransition::apply(S::Cancelled, E::Retry),
            Err(TransitionError {
                from: S::Cancelled,
                event: E::Retry
            })
        );
        Ok(())
    }

    #[test]
    fn transition_error_display_names_state_and_event() {
        let error = TransitionError {
            from: S::Succeeded,
            event: E::Cancel,
        };
        assert_eq!(
            error.to_string(),
            "lifecycle event 'cancel' is not permitted in state 'succeeded'"
        );
    }

    #[test]
    fn states_and_events_serialize_snake_case() -> TestResult {
        assert_eq!(
            serde_json::to_string(&S::TimedOut).map_err(ctx("state"))?,
            "\"timed_out\""
        );
        assert_eq!(
            serde_json::to_string(&E::LoseLease).map_err(ctx("event"))?,
            "\"lose_lease\""
        );
        for state in S::ALL {
            let json = serde_json::to_string(&state).map_err(ctx("state json"))?;
            assert_eq!(json, format!("\"{state}\""));
        }
        Ok(())
    }
}

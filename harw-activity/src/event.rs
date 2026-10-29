//! Normalized activity observations: [`ActivityEvent`] and its parts.
//!
//! Adapters translate raw subsystem events (tool call lifecycle, job state
//! changes, agent runs) into [`ActivityEvent`]s. The canonical sequencer stamps
//! each one with a unique, monotonic [`ActivityEvent::ingest_seq`]; that number
//! is the only order the reducer knows (PL-71/03 "Determinism rules").

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::descriptor::ActivityDescriptor;

/// Normalized identity used for grouping observations.
///
/// The key is produced by the subsystem that owns the resource semantics
/// (workspace path resolver, job runtime, agent runtime); the pattern layer
/// never canonicalizes paths itself. A key is not a concrete event identity:
/// many calls may share one key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKey {
    /// Canonical, workspace-relative file or directory path.
    Path(String),
    /// Job runtime work id (`WorkId`).
    Job(String),
    /// Provisional identity of one tool call whose real resource is learned
    /// only from its result (`job.start` before the `WorkId` is known).
    Call(String),
    /// Agent session id.
    Agent(String),
    /// Any other normalized key (hosts, chains, ...). Never grouped in v1.
    Other(String),
}

impl ActivityKey {
    /// Stable lower-case kind name (`"path"`, `"job"`, `"call"`, `"agent"`,
    /// `"other"`).
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Path(_) => "path",
            Self::Job(_) => "job",
            Self::Call(_) => "call",
            Self::Agent(_) => "agent",
            Self::Other(_) => "other",
        }
    }

    /// The key value without its kind.
    pub fn value(&self) -> &str {
        match self {
            Self::Path(value)
            | Self::Job(value)
            | Self::Call(value)
            | Self::Agent(value)
            | Self::Other(value) => value,
        }
    }
}

impl fmt::Display for ActivityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind(), self.value())
    }
}

/// Runtime identities that own an observation: session, then turn, then call.
///
/// For tool calls the owner is the calling session, its turn and the concrete
/// tool call id. For a run reported by a runtime (child agent run, job run)
/// there is no call id; for an agent run the owner is the agent itself
/// (`session` equals the agent id, `role` its role, `parent` the spawning
/// session), which is what `agent-role-failure-burst@1` groups by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityOwner {
    /// Session (agent) id that performed the activity.
    pub session: String,
    /// Role of that session (`"reviewer"`, `"explorer"`, ...), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Session that spawned `session`, if any (parent scope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Turn index inside `session` for turn-scoped producers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// Concrete tool call id; pairs `Started`/`Progress`/`Finished` of one call.
    ///
    /// Pairing happens through this id only (runtime runs without one pair
    /// through their `Job(W)` / `Agent(id)` resource). A `Started` or
    /// `Progress` with neither can never finish and is kept apart as an
    /// [`crate::Anomaly::Unpaired`] standalone instance; see the
    /// [adapter contract](crate#adapter-contract), rule 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
}

impl ActivityOwner {
    /// Owner with only a session id.
    pub fn session(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            role: None,
            parent: None,
            turn: None,
            call: None,
        }
    }

    /// Sets the session role.
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// Sets the parent session.
    #[must_use]
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Sets the turn index.
    #[must_use]
    pub fn with_turn(mut self, turn: u64) -> Self {
        self.turn = Some(turn);
        self
    }

    /// Sets the concrete tool call id.
    #[must_use]
    pub fn with_call(mut self, call: impl Into<String>) -> Self {
        self.call = Some(call.into());
        self
    }
}

/// Scope boundary kinds carried by [`Lifecycle::Boundary`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    /// The owner's turn ended: closes turn-scoped file instances of that
    /// session/turn and the failure bursts whose parent is that session.
    TurnEnd,
    /// The owner's session ended: closes every scoped instance of it.
    SessionEnd,
    /// Synthetic idle tick written into the stream by the sequencer (time
    /// break). Replayable because it is an event, never a wall-clock read.
    Timeout,
}

/// Lifecycle step of one observation.
///
/// Recorded traces fail closed: unknown fields inside a variant are rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Lifecycle {
    /// The activity was requested/started and is running.
    Started,
    /// Intermediate observation of a running activity (log chunk, progress).
    Progress,
    /// The activity ended; [`ActivityEvent::outcome`] says how.
    Finished,
    /// Rebase: the provisional key in [`ActivityEvent::resource`] is bound to
    /// `to` (`Call(C)` -> `Job(W)`, or an observed rename `Path(a)` ->
    /// `Path(b)`). The instance keeps its [`crate::ProjectionId`].
    ///
    /// A rebase is a pure binding, never a constituent: the activity that
    /// caused it (`job.start`, the move call) is reported as its own events.
    /// Ordering matters: `Call(C)` -> `Job(W)` must precede every event keyed
    /// `Job(W)`, and a rename follows the move call keyed `Path(a)` in the
    /// same session turn. See the [adapter contract](crate#adapter-contract),
    /// rules 2 and 4.
    Rebase {
        /// The identity learned after the fact.
        to: ActivityKey,
    },
    /// Scope boundary for the owner's session (and turn).
    Boundary(Boundary),
}

/// Terminal outcome of an activity.
///
/// Recorded traces fail closed: unknown fields inside a variant are rejected
/// (a misspelled `class` must not silently drop the failure class).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    /// Finished without a reported failure.
    Succeeded,
    /// Finished with a failure.
    Failed {
        /// Normalized failure class (`"timeout"`, `"exit-1"`, ...). Failures
        /// are aggregated only when their classes are known and equal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<String>,
    },
    /// Stopped before completion by a control action.
    Cancelled,
}

impl Outcome {
    /// Failure with a normalized class.
    pub fn failed(class: impl Into<String>) -> Self {
        Self::Failed {
            class: Some(class.into()),
        }
    }
}

/// One normalized observation in the totally ordered activity stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEvent {
    /// Position in the canonical stream: unique and strictly increasing,
    /// assigned by the single sequencer that appends the event log. Live
    /// reduction and replay both apply events in this order.
    pub ingest_seq: u64,
    /// Who performed the activity.
    pub owner: ActivityOwner,
    /// Static semantics of the producer.
    pub descriptor: ActivityDescriptor,
    /// Canonical resource key, if the producer has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActivityKey>,
    /// Lifecycle step.
    pub lifecycle: Lifecycle,
    /// Outcome of the step. Producers report it with [`Lifecycle::Finished`]
    /// (`None` there means "finished without a reported failure").
    ///
    /// A failure is honoured on every step (fail closed): [`Outcome::Failed`]
    /// on `Started` or `Progress` marks the constituent failed, and a rebase
    /// carrying one is not applied but kept visible as a failed
    /// [`crate::Anomaly::RebaseFailed`] standalone instance. `Succeeded` and
    /// `Cancelled` count only on `Finished`; boundaries ignore the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

impl ActivityEvent {
    /// Event without resource and outcome.
    pub fn new(
        ingest_seq: u64,
        owner: ActivityOwner,
        descriptor: ActivityDescriptor,
        lifecycle: Lifecycle,
    ) -> Self {
        Self {
            ingest_seq,
            owner,
            descriptor,
            resource: None,
            lifecycle,
            outcome: None,
        }
    }

    /// Sets the resource key.
    #[must_use]
    pub fn with_resource(mut self, resource: ActivityKey) -> Self {
        self.resource = Some(resource);
        self
    }

    /// Sets the outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = Some(outcome);
        self
    }
}

/// Error from [`parse_jsonl`]: the 1-based line that failed and why.
#[derive(Debug)]
pub struct JsonlError {
    /// 1-based line number in the input.
    pub line: usize,
    /// Underlying JSON error.
    pub source: serde_json::Error,
}

impl fmt::Display for JsonlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "activity trace line {}: {}", self.line, self.source)
    }
}

impl std::error::Error for JsonlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Parses a recorded activity trace: one JSON [`ActivityEvent`] per line.
///
/// Blank lines and lines whose first non-blank character is `#` (fixture
/// comments) are skipped. Every other line must be one complete, valid event
/// (unknown fields are rejected); the first invalid line fails the whole
/// parse (fail closed).
///
/// # Errors
/// [`JsonlError`] naming the first line that is not a valid event.
pub fn parse_jsonl(input: &str) -> Result<Vec<ActivityEvent>, JsonlError> {
    let mut events = Vec::new();
    for (index, raw) in input.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let event = serde_json::from_str(line).map_err(|source| JsonlError {
            line: index + 1,
            source,
        })?;
        events.push(event);
    }
    Ok(events)
}

/// Serializes events as a JSONL trace readable by [`parse_jsonl`].
///
/// # Errors
/// The [`serde_json::Error`] of the first event that cannot be serialized.
pub fn to_jsonl(events: &[ActivityEvent]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for event in events {
        out.push_str(&serde_json::to_string(event)?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Effect;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn read_event(seq: u64) -> ActivityEvent {
        ActivityEvent::new(
            seq,
            ActivityOwner::session("root").with_turn(1).with_call("c1"),
            ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe),
            Lifecycle::Finished,
        )
        .with_resource(ActivityKey::Path("src/lib.rs".into()))
        .with_outcome(Outcome::Succeeded)
    }

    #[test]
    fn key_kind_value_and_display() {
        let key = ActivityKey::Job("W7".into());
        assert_eq!(key.kind(), "job");
        assert_eq!(key.value(), "W7");
        assert_eq!(key.to_string(), "job:W7");
        assert_eq!(ActivityKey::Path("a/b".into()).to_string(), "path:a/b");
    }

    #[test]
    fn event_json_shape_is_compact_and_round_trips() -> TestResult {
        let event = read_event(3);
        let json = serde_json::to_string(&event)?;
        assert_eq!(
            json,
            concat!(
                r#"{"ingest_seq":3,"owner":{"session":"root","turn":1,"call":"c1"},"#,
                r#""descriptor":{"domain":"filesystem","verb":"read","resource_field":"path","effect":"observe"},"#,
                r#""resource":{"path":"src/lib.rs"},"lifecycle":"finished","outcome":"succeeded"}"#
            )
        );
        let back: ActivityEvent = serde_json::from_str(&json)?;
        assert_eq!(back, event);
        Ok(())
    }

    #[test]
    fn lifecycle_and_outcome_serde_shapes() -> TestResult {
        let rebase = Lifecycle::Rebase {
            to: ActivityKey::Job("W1".into()),
        };
        assert_eq!(
            serde_json::to_string(&rebase)?,
            r#"{"rebase":{"to":{"job":"W1"}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Lifecycle::Boundary(Boundary::TurnEnd))?,
            r#"{"boundary":"turn_end"}"#
        );
        assert_eq!(
            serde_json::to_string(&Outcome::failed("timeout"))?,
            r#"{"failed":{"class":"timeout"}}"#
        );
        let classless: Outcome = serde_json::from_str(r#"{"failed":{}}"#)?;
        assert_eq!(classless, Outcome::Failed { class: None });
        Ok(())
    }

    #[test]
    fn parse_jsonl_skips_blank_and_comment_lines() -> TestResult {
        let trace = to_jsonl(&[read_event(1), read_event(2)])?;
        let annotated = format!("# header comment\n\n{trace}   \n  # trailing\n");
        let events = parse_jsonl(&annotated)?;
        assert_eq!(events, vec![read_event(1), read_event(2)]);
        Ok(())
    }

    #[test]
    fn parse_jsonl_reports_the_failing_line() {
        let input = "# c\n{\"ingest_seq\":1}\n";
        match parse_jsonl(input) {
            Ok(events) => assert!(events.is_empty(), "must fail, got {events:?}"),
            Err(error) => {
                assert_eq!(error.line, 2);
                assert!(error.to_string().starts_with("activity trace line 2:"));
            }
        }
    }

    #[test]
    fn misspelled_field_in_trace_fails_closed() {
        let line = r#"{"ingest_seq":1,"owner":{"session":"s"},"descriptor":{"domain":"fs","verb":"read","effect":"observe"},"lifecycle":"finished","outcom":"succeeded"}"#;
        assert!(parse_jsonl(line).is_err());
    }

    #[test]
    fn misspelled_failure_class_in_trace_fails_closed() -> TestResult {
        let line = r#"{"ingest_seq":1,"owner":{"session":"s"},"descriptor":{"domain":"fs","verb":"read","effect":"observe"},"lifecycle":"finished","outcome":{"failed":{"clas":"timeout"}}}"#;
        let Err(error) = parse_jsonl(line) else {
            return Err("a misspelled failure class must not parse".into());
        };
        assert!(
            error.to_string().contains("unknown field `clas`"),
            "{error}"
        );
        assert!(serde_json::from_str::<Outcome>(r#"{"failed":{"clas":"timeout"}}"#).is_err());
        Ok(())
    }

    #[test]
    fn extra_field_in_rebase_fails_closed() {
        let line = r#"{"ingest_seq":1,"owner":{"session":"s"},"descriptor":{"domain":"job","verb":"start","effect":"execute"},"resource":{"call":"c1"},"lifecycle":{"rebase":{"to":{"job":"W1"},"from":{"call":"c1"}}}}"#;
        assert!(parse_jsonl(line).is_err());
        assert!(
            serde_json::from_str::<Lifecycle>(
                r#"{"rebase":{"to":{"job":"W1"},"from":{"call":"c1"}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_effect_in_trace_fails_closed() {
        let line = r#"{"ingest_seq":1,"owner":{"session":"s"},"descriptor":{"domain":"fs","verb":"read","effect":"telepathy"},"lifecycle":"started"}"#;
        assert!(parse_jsonl(line).is_err());
    }
}

//! Pattern vocabulary: [`PatternId`], [`PatternInstance`], [`PatternState`],
//! [`ProjectionId`] and [`ProjectionUpdate`].
//!
//! A [`PatternInstance`] is derived, mutable state over immutable events. Every
//! concrete activity it covers stays individually addressable as a
//! [`Constituent`] (PL-71 I2/I6), and its compact counters are recomputed from
//! those constituents on every change, so `counts.failed` always equals the
//! number of failed constituents (PL-71/07 "Compact failure invariant").

use std::borrow::Cow;
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::descriptor::{Effect, known_semantics};
use crate::event::{ActivityEvent, ActivityKey, Lifecycle, Outcome};

/// Stable, versioned id of a built-in pattern definition.
///
/// The set is closed: values exist only as the associated constants, and
/// [`PatternId::parse`] rejects anything else (malformed definitions fail
/// closed). Semantic changes get a new `@version`, never a silent edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PatternId(&'static str);

impl PatternId {
    /// Concurrent or repeated observing calls on one path in one turn.
    pub const FILE_READ_BURST: Self = Self("file-read-burst@1");
    /// Edits/creations of one path plus the observing calls that follow, in
    /// one turn.
    pub const FILE_EDIT_READ_LOOP: Self = Self("file-edit-read-loop@1");
    /// Start, status, logs, wait and the run itself of one job (`WorkId`),
    /// including the provisional `Call(C)` phase before the rebase.
    pub const JOB_LIFECYCLE: Self = Self("job-lifecycle@1");
    /// Failed child agent runs with the same parent, role and failure class
    /// inside one bounded window of the parent session.
    pub const AGENT_ROLE_FAILURE_BURST: Self = Self("agent-role-failure-burst@1");
    /// Fallback for one concrete activity that no pattern groups (unknown
    /// descriptor, unkeyed calls, refused rebases; PL-71 I5).
    pub const STANDALONE: Self = Self("standalone@1");

    /// The four grouping patterns of v1.
    pub const V1: [Self; 4] = [
        Self::FILE_READ_BURST,
        Self::FILE_EDIT_READ_LOOP,
        Self::JOB_LIFECYCLE,
        Self::AGENT_ROLE_FAILURE_BURST,
    ];

    /// Every id the reducer can emit (the four v1 patterns plus standalone).
    pub const ALL: [Self; 5] = [
        Self::FILE_READ_BURST,
        Self::FILE_EDIT_READ_LOOP,
        Self::JOB_LIFECYCLE,
        Self::AGENT_ROLE_FAILURE_BURST,
        Self::STANDALONE,
    ];

    /// The full id, e.g. `"file-read-burst@1"`.
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Parses a full id; `None` for anything outside [`PatternId::ALL`].
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.0 == id)
    }
}

impl fmt::Display for PatternId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Serialize for PatternId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> Deserialize<'de> for PatternId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| D::Error::custom(format!("unknown pattern id `{raw}`")))
    }
}

/// Stable identity of one projection (one consumer-facing row/card/message).
///
/// Derived from the replayable stream, never from a process-local counter:
/// `opened_at` is the `ingest_seq` of the event that opened the instance and
/// `pattern` disambiguates the rare event that opens two instances (an agent
/// run's standalone instance and its failure burst). The id never changes for
/// the lifetime of the instance; a rebase only adds an alias key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectionId {
    /// `ingest_seq` of the event that opened the instance.
    pub opened_at: u64,
    /// Pattern of the instance.
    pub pattern: PatternId,
}

impl fmt::Display for ProjectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.pattern, self.opened_at)
    }
}

/// Whether an instance is still active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// At least one constituent is running.
    Open,
    /// Nothing is running, but the instance may still take new constituents
    /// (its scope has not ended).
    Quiescent,
    /// Nothing is running and the instance is sealed; it never reopens.
    Terminal,
}

/// Grouping scope of an instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// File patterns: one session turn.
    Turn {
        /// Owning session.
        session: String,
        /// Turn index, if the producer is turn-scoped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
    },
    /// Standalone instances: owned by one session.
    Session {
        /// Owning session.
        session: String,
    },
    /// Job instances: keyed by a runtime id, independent of the observer.
    Global,
    /// Agent failure bursts: child runs of one parent with one role and one
    /// failure class.
    Parent {
        /// Spawning session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Role of the failed runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// Normalized failure class shared by all members.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_class: Option<String>,
    },
}

/// Concrete identity of one constituent activity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstituentId {
    /// One tool call of one session.
    Call {
        /// Calling session.
        session: String,
        /// Tool call id.
        call: String,
    },
    /// A runtime-reported run identified by its key (`Job(W)`, `Agent(id)`).
    Key(ActivityKey),
    /// A point observation without a pairable identity (its `ingest_seq`).
    Event(u64),
}

impl ConstituentId {
    /// Identity of the activity an event belongs to.
    ///
    /// A call id wins; without one, job and agent keys identify the run the
    /// runtime reports; anything else is a point observation.
    pub fn of(event: &ActivityEvent) -> Self {
        if let Some(call) = &event.owner.call {
            return Self::Call {
                session: event.owner.session.clone(),
                call: call.clone(),
            };
        }
        match &event.resource {
            Some(key @ (ActivityKey::Job(_) | ActivityKey::Agent(_))) => Self::Key(key.clone()),
            _ => Self::Event(event.ingest_seq),
        }
    }
}

/// Status of one constituent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstituentStatus {
    /// Started and not yet finished.
    Running,
    /// Finished without a reported failure.
    Succeeded,
    /// Finished with a failure. Sticky: a later success report never hides it.
    Failed {
        /// Normalized failure class.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<String>,
    },
    /// Cancelled by a control action.
    Cancelled,
}

impl ConstituentStatus {
    /// Status after `event` when the constituent is first seen.
    ///
    /// A reported failure wins on every lifecycle step (fail closed), so a
    /// `Progress` or `Started` event carrying [`Outcome::Failed`] is never
    /// shown as running or succeeded.
    pub(crate) fn initial(event: &ActivityEvent) -> Self {
        if let Some(failed) = Self::failure(event) {
            return failed;
        }
        match event.lifecycle {
            Lifecycle::Started | Lifecycle::Progress => Self::Running,
            Lifecycle::Finished => Self::from_outcome(event.outcome.as_ref()),
            Lifecycle::Rebase { .. } => Self::from_outcome(event.outcome.as_ref()),
            Lifecycle::Boundary(_) => Self::Succeeded,
        }
    }

    /// The failure `event` reports, whatever its lifecycle step.
    fn failure(event: &ActivityEvent) -> Option<Self> {
        match &event.outcome {
            Some(Outcome::Failed { class }) => Some(Self::Failed {
                class: class.clone(),
            }),
            _ => None,
        }
    }

    fn from_outcome(outcome: Option<&Outcome>) -> Self {
        match outcome {
            None | Some(Outcome::Succeeded) => Self::Succeeded,
            Some(Outcome::Failed { class }) => Self::Failed {
                class: class.clone(),
            },
            Some(Outcome::Cancelled) => Self::Cancelled,
        }
    }

    /// Next status after `event`; a failure is never overwritten, and a
    /// failure reported on any step is taken over.
    fn after(&self, event: &ActivityEvent) -> Self {
        if self.is_failed() {
            return self.clone();
        }
        if let Some(failed) = Self::failure(event) {
            return failed;
        }
        match event.lifecycle {
            Lifecycle::Finished => Self::from_outcome(event.outcome.as_ref()),
            _ => self.clone(),
        }
    }

    /// `true` while running.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    /// `true` for a failure.
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// One concrete activity covered by an instance (expansion target, PL-71 I6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constituent {
    /// Concrete identity.
    pub id: ConstituentId,
    /// Domain of the descriptor that introduced it.
    pub domain: Cow<'static, str>,
    /// Verb of the descriptor that introduced it.
    pub verb: Cow<'static, str>,
    /// Effect of the descriptor that introduced it.
    pub effect: Effect,
    /// Resource key it was observed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActivityKey>,
    /// Current status.
    pub status: ConstituentStatus,
    /// `ingest_seq` of its first event.
    pub first_seq: u64,
    /// `ingest_seq` of its latest event.
    pub last_seq: u64,
}

impl Constituent {
    pub(crate) fn from_event(id: ConstituentId, event: &ActivityEvent) -> Self {
        Self {
            id,
            domain: event.descriptor.domain.clone(),
            verb: event.descriptor.verb.clone(),
            effect: event.descriptor.effect,
            resource: event.resource.clone(),
            status: ConstituentStatus::initial(event),
            first_seq: event.ingest_seq,
            last_seq: event.ingest_seq,
        }
    }

    /// `1` when failed, else `0` (the per-constituent failure count).
    pub fn failed(&self) -> u64 {
        u64::from(self.status.is_failed())
    }

    /// `true` when the descriptor that introduced the constituent carries
    /// usable semantics (same rule as
    /// [`crate::ActivityDescriptor::is_known`]). Constituents for which this
    /// is `false` only ever live in standalone instances (PL-71 I5).
    pub fn is_known(&self) -> bool {
        known_semantics(&self.domain, &self.verb, self.effect)
    }
}

/// Compact counters over the constituents of an instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Counts {
    /// Number of constituents.
    pub total: u64,
    /// Constituents still running.
    pub running: u64,
    /// Constituents finished without failure.
    pub succeeded: u64,
    /// Failed constituents (always equals the sum of [`Constituent::failed`]).
    pub failed: u64,
    /// Cancelled constituents.
    pub cancelled: u64,
}

impl Counts {
    fn add(&mut self, status: &ConstituentStatus) {
        self.total += 1;
        match status {
            ConstituentStatus::Running => self.running += 1,
            ConstituentStatus::Succeeded => self.succeeded += 1,
            ConstituentStatus::Failed { .. } => self.failed += 1,
            ConstituentStatus::Cancelled => self.cancelled += 1,
        }
    }
}

/// Per-verb tally (`Edit ×1 · Read ×3`), in order of first appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerbCount {
    /// Descriptor verb.
    pub verb: Cow<'static, str>,
    /// Counters of the constituents with this verb.
    pub counts: Counts,
}

/// Why an instance exists outside the normal matching path.
///
/// Every anomaly marks an event that broke the
/// [adapter contract](crate#adapter-contract); the reducer keeps it visible
/// as a standalone instance of its own and never merges it into a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anomaly {
    /// A job rebase named a `Call(C)` no instance is bound to, or the key
    /// kinds do not fit (only `Call(_)` -> `Job(_)` and `Path(_)` ->
    /// `Path(_)` are rebases).
    RebaseUnmatched,
    /// The rebase would bind one key to two instances, or one job instance
    /// to two work ids; nothing is merged.
    RebaseConflict,
    /// The rebase reported a failure. The binding is not applied; the
    /// failure stays visible in this instance.
    RebaseFailed,
    /// A `Started` or `Progress` without a call id or run key: nothing can
    /// ever finish it, so it joins no pattern.
    Unpaired,
}

/// Compact, neutral state of an instance (no styles, no markup).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternState {
    /// Derived from `counts.running` and `sealed`.
    pub phase: Phase,
    /// `true` once the instance takes no new constituents. Constituents it
    /// already covers keep updating it.
    pub sealed: bool,
    /// Grouping scope.
    pub scope: Scope,
    /// Counters over all constituents.
    pub counts: Counts,
    /// Counters per verb, in order of first appearance.
    pub verbs: Vec<VerbCount>,
    /// Every covered activity, in order of first appearance.
    pub members: Vec<Constituent>,
    /// Keys bound to this instance before a rebase (`Call(C)` after
    /// `Call(C)` -> `Job(W)`), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ActivityKey>,
    /// Set when the instance records a matcher anomaly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anomaly: Option<Anomaly>,
}

/// One concrete match of a pattern over a bounded set of observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternInstance {
    /// Stable projection identity.
    pub id: ProjectionId,
    /// Pattern definition.
    pub pattern: PatternId,
    /// Current canonical key (after any rebase).
    pub key: Option<ActivityKey>,
    /// Compact state.
    pub state: PatternState,
    /// `ingest_seq` of every event applied to this instance, ascending
    /// (provenance, PL-71 I7). Events that only seal it (a boundary, a later
    /// edit or delete of the path, an unknown or unkeyed mutating call) are
    /// not listed; they show up as the [`PatternInstance::revision`].
    pub constituents: Vec<u64>,
    /// `ingest_seq` of the last event that changed this instance. Consumers
    /// drop updates whose revision is not newer than the one they show.
    pub revision: u64,
}

impl PatternInstance {
    /// New, empty instance opened by `opened_at`.
    pub(crate) fn open(
        opened_at: u64,
        pattern: PatternId,
        key: Option<ActivityKey>,
        scope: Scope,
        sealed: bool,
    ) -> Self {
        Self {
            id: ProjectionId { opened_at, pattern },
            pattern,
            key,
            state: PatternState {
                phase: Phase::Quiescent,
                sealed,
                scope,
                counts: Counts::default(),
                verbs: Vec::new(),
                members: Vec::new(),
                aliases: Vec::new(),
                anomaly: None,
            },
            constituents: Vec::new(),
            revision: opened_at,
        }
    }

    /// Number of failed constituents.
    pub fn failed(&self) -> u64 {
        self.state.counts.failed
    }

    /// `true` when the phase is [`Phase::Terminal`].
    pub fn is_terminal(&self) -> bool {
        self.state.phase == Phase::Terminal
    }

    /// The constituent with `id`, if covered.
    pub fn member(&self, id: &ConstituentId) -> Option<&Constituent> {
        self.state.members.iter().find(|member| &member.id == id)
    }

    /// Records `seq` as provenance and as the new revision.
    pub(crate) fn record(&mut self, seq: u64) {
        if self.constituents.last() != Some(&seq) {
            self.constituents.push(seq);
        }
        self.revision = seq;
    }

    /// Adds a new constituent first seen in `event`.
    pub(crate) fn add_member(&mut self, id: ConstituentId, event: &ActivityEvent) {
        self.state.members.push(Constituent::from_event(id, event));
        self.record(event.ingest_seq);
        self.recompute();
    }

    /// Adds a copy of an existing constituent (failure bursts mirror the
    /// failed run they cover).
    pub(crate) fn add_member_copy(&mut self, member: Constituent, seq: u64) {
        self.state.members.push(member);
        self.record(seq);
        self.recompute();
    }

    /// Applies a later event of an already covered constituent. Returns the
    /// constituent's status afterwards, or `None` if it is not covered.
    pub(crate) fn update_member(
        &mut self,
        id: &ConstituentId,
        event: &ActivityEvent,
    ) -> Option<ConstituentStatus> {
        let member = self
            .state
            .members
            .iter_mut()
            .find(|member| &member.id == id)?;
        member.status = member.status.after(event);
        member.last_seq = event.ingest_seq;
        let status = member.status.clone();
        self.record(event.ingest_seq);
        self.recompute();
        Some(status)
    }

    /// Seals the instance at `seq`; `false` if it already was sealed.
    pub(crate) fn seal(&mut self, seq: u64) -> bool {
        if self.state.sealed {
            return false;
        }
        self.state.sealed = true;
        self.revision = seq;
        self.recompute();
        true
    }

    /// Rebuilds counters, verb tallies and phase from the constituents.
    pub(crate) fn recompute(&mut self) {
        let mut counts = Counts::default();
        let mut verbs: Vec<VerbCount> = Vec::new();
        for member in &self.state.members {
            counts.add(&member.status);
            match verbs.iter_mut().find(|tally| tally.verb == member.verb) {
                Some(tally) => tally.counts.add(&member.status),
                None => {
                    let mut tally = VerbCount {
                        verb: member.verb.clone(),
                        counts: Counts::default(),
                    };
                    tally.counts.add(&member.status);
                    verbs.push(tally);
                }
            }
        }
        self.state.phase = if counts.running > 0 {
            Phase::Open
        } else if self.state.sealed {
            Phase::Terminal
        } else {
            Phase::Quiescent
        };
        self.state.counts = counts;
        self.state.verbs = verbs;
    }
}

/// A new revision of one projection, emitted by [`crate::Reducer::apply`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionUpdate {
    /// Projection to replace.
    pub id: ProjectionId,
    /// Revision of `instance` (`ingest_seq` of the last applied event).
    pub revision: u64,
    /// Full snapshot of the instance at that revision.
    pub instance: PatternInstance,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ActivityDescriptor;
    use crate::event::ActivityOwner;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn event(seq: u64, verb: &'static str, lifecycle: Lifecycle) -> ActivityEvent {
        ActivityEvent::new(
            seq,
            ActivityOwner::session("s").with_call(format!("c{seq}")),
            ActivityDescriptor::new("filesystem", verb, Some("path"), Effect::Observe),
            lifecycle,
        )
        .with_resource(ActivityKey::Path("a".into()))
    }

    #[test]
    fn pattern_ids_are_closed_and_round_trip() -> TestResult {
        for id in PatternId::ALL {
            assert_eq!(PatternId::parse(id.as_str()), Some(id));
            let json = serde_json::to_string(&id)?;
            assert_eq!(json, format!("\"{id}\""));
            let back: PatternId = serde_json::from_str(&json)?;
            assert_eq!(back, id);
        }
        assert_eq!(
            PatternId::V1.map(PatternId::as_str),
            [
                "file-read-burst@1",
                "file-edit-read-loop@1",
                "job-lifecycle@1",
                "agent-role-failure-burst@1"
            ]
        );
        assert_eq!(PatternId::parse("file-read-burst@2"), None);
        assert_eq!(PatternId::parse("file-read-burst"), None);
        assert!(serde_json::from_str::<PatternId>("\"compile-fix-cycle@1\"").is_err());
        Ok(())
    }

    #[test]
    fn projection_id_display_and_order() {
        let early = ProjectionId {
            opened_at: 2,
            pattern: PatternId::STANDALONE,
        };
        let late = ProjectionId {
            opened_at: 10,
            pattern: PatternId::AGENT_ROLE_FAILURE_BURST,
        };
        assert_eq!(late.to_string(), "agent-role-failure-burst@1#10");
        assert!(early < late);
    }

    #[test]
    fn constituent_id_prefers_call_then_runtime_key() {
        let call = event(1, "read", Lifecycle::Started);
        assert_eq!(
            ConstituentId::of(&call),
            ConstituentId::Call {
                session: "s".into(),
                call: "c1".into()
            }
        );

        let mut run = ActivityEvent::new(
            2,
            ActivityOwner::session("s"),
            ActivityDescriptor::JOB_RUN,
            Lifecycle::Progress,
        )
        .with_resource(ActivityKey::Job("W".into()));
        assert_eq!(
            ConstituentId::of(&run),
            ConstituentId::Key(ActivityKey::Job("W".into()))
        );

        run.resource = Some(ActivityKey::Path("a".into()));
        assert_eq!(ConstituentId::of(&run), ConstituentId::Event(2));
    }

    #[test]
    fn failure_is_sticky_and_counts_follow_members() {
        let mut instance = PatternInstance::open(
            1,
            PatternId::FILE_READ_BURST,
            Some(ActivityKey::Path("a".into())),
            Scope::Turn {
                session: "s".into(),
                turn: None,
            },
            false,
        );
        let started = event(1, "read", Lifecycle::Started);
        let id = ConstituentId::of(&started);
        instance.add_member(id.clone(), &started);
        assert_eq!(instance.state.phase, Phase::Open);
        assert_eq!(instance.state.counts.running, 1);

        let failed = event(2, "read", Lifecycle::Finished).with_outcome(Outcome::failed("io"));
        assert!(instance.update_member(&id, &failed).is_some());
        let late_ok = event(3, "read", Lifecycle::Finished).with_outcome(Outcome::Succeeded);
        assert!(instance.update_member(&id, &late_ok).is_some());

        assert_eq!(instance.failed(), 1);
        assert_eq!(instance.state.phase, Phase::Quiescent);
        assert_eq!(instance.constituents, vec![1, 2, 3]);
        assert_eq!(instance.revision, 3);
        assert_eq!(instance.state.verbs.len(), 1);
        assert_eq!(instance.state.verbs[0].counts.failed, 1);

        assert!(instance.seal(4));
        assert!(!instance.seal(5));
        assert!(instance.is_terminal());
        assert_eq!(instance.revision, 4);
    }

    #[test]
    fn a_failure_on_any_step_wins_over_running_and_success() {
        let progress = event(1, "read", Lifecycle::Progress).with_outcome(Outcome::failed("io"));
        assert_eq!(
            ConstituentStatus::initial(&progress),
            ConstituentStatus::Failed {
                class: Some("io".into())
            }
        );
        let rebase = event(
            2,
            "read",
            Lifecycle::Rebase {
                to: ActivityKey::Path("b".into()),
            },
        )
        .with_outcome(Outcome::failed("io"));
        assert!(ConstituentStatus::initial(&rebase).is_failed());

        let running = ConstituentStatus::initial(&event(3, "read", Lifecycle::Started));
        assert!(running.after(&progress).is_failed());
        // Non-failure outcomes still only count on Finished.
        let cancelled_progress =
            event(4, "read", Lifecycle::Progress).with_outcome(Outcome::Cancelled);
        assert!(running.after(&cancelled_progress).is_running());
    }

    #[test]
    fn constituent_knows_its_descriptor() {
        let known = Constituent::from_event(
            ConstituentId::Event(1),
            &event(1, "read", Lifecycle::Finished),
        );
        assert_eq!(known.domain, "filesystem");
        assert!(known.is_known());
        let unknown = Constituent::from_event(
            ConstituentId::Event(2),
            &ActivityEvent::new(
                2,
                ActivityOwner::session("s"),
                ActivityDescriptor::UNKNOWN,
                Lifecycle::Finished,
            ),
        );
        assert!(!unknown.is_known());
    }

    #[test]
    fn update_of_unknown_member_is_none() {
        let mut instance =
            PatternInstance::open(1, PatternId::STANDALONE, None, Scope::Global, true);
        let ev = event(1, "read", Lifecycle::Finished);
        assert_eq!(instance.update_member(&ConstituentId::Event(99), &ev), None);
        assert!(instance.constituents.is_empty());
    }
}

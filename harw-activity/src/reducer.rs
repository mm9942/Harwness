//! Deterministic reducer: [`ActivityEvent`] stream -> [`PatternInstance`]s.
//!
//! What producers must guarantee is the crate-level
//! [adapter contract](crate#adapter-contract); this module documents how the
//! reducer reacts.
//!
//! # Matching rules (v1)
//! Every non-boundary event belongs to exactly one *constituent*
//! ([`ConstituentId::of`]). The first event of a constituent decides its home
//! instance; later events of the same constituent update that home, even
//! after the home was sealed.
//!
//! - `Started`/`Progress` without call id and run key (nothing can ever
//!   finish it): standalone, flagged [`Anomaly::Unpaired`].
//! - Unknown descriptor ([`crate::ActivityDescriptor::is_known`] is false):
//!   standalone (PL-71 I5).
//! - `Path(p)` + [`Effect::Observe`]: joins the open `file-edit-read-loop@1`
//!   of `p` in the same session turn, else the open `file-read-burst@1` of
//!   `p`, else opens a new burst.
//! - `Path(p)` + [`Effect::Mutate`]/[`Effect::Create`]: seals the open burst
//!   of `p`, then joins or opens the `file-edit-read-loop@1` of `p`.
//! - `Path(p)` + any other effect: seals the burst and loop of `p`;
//!   standalone.
//! - `Job(W)`, or `Call(C)` with domain [`JOB_DOMAIN`]: joins the
//!   `job-lifecycle@1` instance bound to that key (standalone once it is
//!   terminal), else opens one.
//! - Child agent run (`Agent(id)` without call id): standalone per agent; a
//!   failed run is additionally covered by one `agent-role-failure-burst@1`
//!   when both the descriptor that introduced the run and the one reporting
//!   the failure are known agent-runtime semantics ([`AGENT_DOMAIN`]); it
//!   joins an open burst only when parent, role and failure class are all
//!   known and equal.
//! - Anything else: standalone.
//!
//! Break conditions: a new constituent with unknown semantics, any
//! [`Effect::Execute`] (whatever its key: its side effects are not described
//! by it), or a [`Effect::may_mutate`] effect without a `Path` key seals the
//! open file instances of its session turn. A [`Boundary::TurnEnd`] seals the
//! file instances of that turn, [`Boundary::SessionEnd`] and
//! [`Boundary::Timeout`] those of the whole session; all three seal the
//! failure bursts whose parent is that session.
//!
//! # Rebase rules
//! A rebase is a pure binding: it never becomes a constituent of the
//! instances it rebinds (it is listed in their provenance only).
//! - A rebase carrying [`crate::Outcome::Failed`] is not applied
//!   ([`Anomaly::RebaseFailed`]); a cancelled rebase is also not applied.
//! - `Call(C)` -> `Job(W)`: binds the job instance of `C` (in the rebase's
//!   session) to `W`, keeps its [`ProjectionId`], records `Call(C)` as alias
//!   and adds a running `run` constituent. Refused as
//!   [`Anomaly::RebaseConflict`] when `W` is bound to another instance or the
//!   instance is already bound to another work id; [`Anomaly::RebaseUnmatched`]
//!   when `C` is unbound or the key kinds do not fit (`Job` -> `Job`
//!   included). Repeating an applied binding is provenance only.
//! - `Path(a)` -> `Path(b)`: rebinds the open burst and loop of `a` in the
//!   rebase's session turn to `b` (alias `a`). Open instances of `b` are
//!   sealed, because the rename replaced `b`; if `a` has open instances as
//!   well, nothing is rebound ([`Anomaly::RebaseConflict`]). A rename with
//!   nothing to rebind is a plain standalone instance.
//!
//! Refused rebases and renames without anything to rebind stay visible as
//! standalone instances of their own; two instances are never merged.
//!
//! # Determinism
//! Input order is `ingest_seq` only; events that do not advance it are
//! refused ([`ReduceError::OutOfOrder`]) and counted, never reordered. All
//! state lives in ordered maps, nothing reads a clock, and ids and revisions
//! are `ingest_seq` values, so `replay(log) == live(log)` holds by
//! construction.
//!
//! # Bounded retention
//! On its own the reducer keeps every instance, so its memory grows with the
//! stream. A long-lived consumer (the TUI adapter of PL-97 M3) bounds it with
//! [`Reducer::evict_closed`]: closed ([`crate::Phase::Terminal`]) instances
//! beyond a cap are dropped, oldest [`ProjectionId`] first, together with
//! every index entry that points at them. Open and quiescent instances are
//! never evicted. The counters of evicted instances are preserved
//! ([`Reducer::evicted_instances`], [`Reducer::evicted_counts`]), so a
//! failure never disappears from the totals. Eviction is deterministic: the
//! same events with the same eviction calls yield the same state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::descriptor::{AGENT_DOMAIN, ActivityDescriptor, Effect, JOB_DOMAIN};
use crate::event::{ActivityEvent, ActivityKey, Boundary, Lifecycle, Outcome};
use crate::pattern::{
    Anomaly, Constituent, ConstituentId, ConstituentStatus, Counts, PatternId, PatternInstance,
    ProjectionId, ProjectionUpdate, Scope,
};

/// Error from [`Reducer::try_apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReduceError {
    /// The event does not advance the stream: `got <= last`. Re-delivered
    /// events after a reconnect land here and are ignored, which keeps
    /// reduction idempotent; a sequencer bug lands here too.
    OutOfOrder {
        /// Last applied `ingest_seq`.
        last: u64,
        /// Refused `ingest_seq`.
        got: u64,
    },
}

impl fmt::Display for ReduceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfOrder { last, got } => write!(
                f,
                "activity event ingest_seq {got} does not advance the stream (last applied {last})"
            ),
        }
    }
}

impl std::error::Error for ReduceError {}

/// Index key of an open (joinable) file instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FileSlot {
    session: String,
    turn: Option<u64>,
    pattern: PatternId,
    path: String,
}

/// Index key of a job instance. Call ids are only unique per session, so a
/// provisional `Call(C)` key is qualified by the calling session; work ids
/// are global.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct KeySlot {
    session: Option<String>,
    key: ActivityKey,
}

impl KeySlot {
    fn new(key: &ActivityKey, session: &str) -> Self {
        let session = match key {
            ActivityKey::Call(_) => Some(session.to_owned()),
            _ => None,
        };
        Self {
            session,
            key: key.clone(),
        }
    }
}

/// Index key of a joinable failure burst.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BurstSlot {
    parent: String,
    role: String,
    class: String,
}

/// Deterministic pattern reducer over one totally ordered activity stream.
///
/// The reducer never moves a constituent between instances and removes an
/// instance only when asked to ([`Reducer::evict_closed`], closed instances
/// only), so every applied activity stays visible in exactly one home
/// instance (plus, for failed agent runs, one failure burst) until its
/// instance is closed and evicted; its counters survive the eviction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reducer {
    last_seq: Option<u64>,
    stale: u64,
    instances: BTreeMap<ProjectionId, PatternInstance>,
    homes: BTreeMap<ConstituentId, ProjectionId>,
    files: BTreeMap<FileSlot, ProjectionId>,
    keys: BTreeMap<KeySlot, ProjectionId>,
    bursts: BTreeMap<BurstSlot, ProjectionId>,
    burst_of: BTreeMap<ConstituentId, ProjectionId>,
    /// Number of instances removed by [`Reducer::evict_closed`].
    evicted: u64,
    /// Summed constituent counters of the evicted instances, per pattern.
    evicted_counts: BTreeMap<PatternId, Counts>,
}

impl Reducer {
    /// Empty reducer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reduces a whole recorded stream from zero.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a ActivityEvent>) -> Self {
        let mut reducer = Self::new();
        for event in events {
            reducer.apply(event);
        }
        reducer
    }

    /// Applies one event and returns the new revision of every projection it
    /// changed, ordered by [`ProjectionId`].
    ///
    /// An event that does not advance `ingest_seq` changes nothing and
    /// returns no updates (see [`Reducer::try_apply`] and
    /// [`Reducer::stale_events`]).
    pub fn apply(&mut self, event: &ActivityEvent) -> Vec<ProjectionUpdate> {
        self.try_apply(event).unwrap_or_default()
    }

    /// Like [`Reducer::apply`], but reports a refused event.
    ///
    /// # Errors
    /// [`ReduceError::OutOfOrder`] when `event.ingest_seq` is not greater than
    /// the last applied one; the reducer state is unchanged apart from the
    /// stale counter.
    pub fn try_apply(
        &mut self,
        event: &ActivityEvent,
    ) -> Result<Vec<ProjectionUpdate>, ReduceError> {
        if let Some(last) = self.last_seq.filter(|last| event.ingest_seq <= *last) {
            self.stale += 1;
            return Err(ReduceError::OutOfOrder {
                last,
                got: event.ingest_seq,
            });
        }
        self.last_seq = Some(event.ingest_seq);

        let mut touched = BTreeSet::new();
        match &event.lifecycle {
            Lifecycle::Boundary(boundary) => self.apply_boundary(event, *boundary, &mut touched),
            Lifecycle::Rebase { to } => self.apply_rebase(event, to, &mut touched),
            Lifecycle::Started | Lifecycle::Progress | Lifecycle::Finished => {
                self.apply_activity(event, &mut touched);
            }
        }
        Ok(self.updates(touched))
    }

    /// `ingest_seq` of the last applied event.
    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    /// Number of refused (non-advancing) events.
    pub fn stale_events(&self) -> u64 {
        self.stale
    }

    /// The instance behind `id`.
    pub fn instance(&self, id: &ProjectionId) -> Option<&PatternInstance> {
        self.instances.get(id)
    }

    /// All instances, ordered by [`ProjectionId`].
    pub fn instances(&self) -> impl Iterator<Item = &PatternInstance> {
        self.instances.values()
    }

    /// Owned copy of all instances, ordered by [`ProjectionId`].
    pub fn snapshot(&self) -> Vec<PatternInstance> {
        self.instances.values().cloned().collect()
    }

    /// Home projection of a concrete activity (tool call, job run, agent run).
    pub fn projection_of(&self, constituent: &ConstituentId) -> Option<ProjectionId> {
        self.homes.get(constituent).copied()
    }

    /// Failure burst covering a failed agent run, if any.
    pub fn failure_burst_of(&self, constituent: &ConstituentId) -> Option<ProjectionId> {
        self.burst_of.get(constituent).copied()
    }

    /// `job-lifecycle@1` projection bound to work id `work_id`.
    pub fn job_projection(&self, work_id: &str) -> Option<ProjectionId> {
        let slot = KeySlot {
            session: None,
            key: ActivityKey::Job(work_id.to_owned()),
        };
        self.keys.get(&slot).copied()
    }

    // ── bounded retention ───────────────────────────────────────────────────

    /// Number of closed ([`crate::Phase::Terminal`]) instances currently held.
    pub fn closed_instances(&self) -> usize {
        self.instances
            .values()
            .filter(|instance| instance.is_terminal())
            .count()
    }

    /// Bounded retention: evicts the oldest closed instances so that at most
    /// `keep` closed ones remain, and returns the evicted ids in eviction
    /// order (ascending [`ProjectionId`]).
    ///
    /// Only [`crate::Phase::Terminal`] instances are evicted: nothing runs in
    /// them and they take no new constituents. Open and quiescent instances,
    /// the stream position ([`Reducer::last_seq`]) and the stale counter are
    /// never touched. Every index entry pointing at an evicted instance goes
    /// with it, so memory is bounded by the open instances plus `keep`. The
    /// counters of the evicted instances are added to
    /// [`Reducer::evicted_counts`] (failures included), so totals over live
    /// and evicted instances never shrink.
    ///
    /// A later event of a constituent whose instance was evicted is reduced
    /// as a new constituent (its history is gone); a job whose terminal
    /// instance was evicted opens a new `job-lifecycle@1` instance on its next
    /// observation instead of a standalone one.
    pub fn evict_closed(&mut self, keep: usize) -> Vec<ProjectionId> {
        let closed: Vec<ProjectionId> = self
            .instances
            .iter()
            .filter(|(_, instance)| instance.is_terminal())
            .map(|(id, _)| *id)
            .collect();
        let excess = closed.len().saturating_sub(keep);
        if excess == 0 {
            return Vec::new();
        }
        let evicted: Vec<ProjectionId> = closed.into_iter().take(excess).collect();
        for id in &evicted {
            if let Some(instance) = self.instances.remove(id) {
                let tally = self.evicted_counts.entry(instance.pattern).or_default();
                add_counts(tally, &instance.state.counts);
                self.evicted += 1;
            }
        }
        let gone: BTreeSet<ProjectionId> = evicted.iter().copied().collect();
        self.homes.retain(|_, id| !gone.contains(id));
        self.burst_of.retain(|_, id| !gone.contains(id));
        self.files.retain(|_, id| !gone.contains(id));
        self.keys.retain(|_, id| !gone.contains(id));
        self.bursts.retain(|_, id| !gone.contains(id));
        evicted
    }

    /// Number of instances evicted so far by [`Reducer::evict_closed`].
    pub fn evicted_instances(&self) -> u64 {
        self.evicted
    }

    /// Summed constituent counters of every evicted instance (all patterns).
    pub fn evicted_counts(&self) -> Counts {
        let mut total = Counts::default();
        for counts in self.evicted_counts.values() {
            add_counts(&mut total, counts);
        }
        total
    }

    /// Summed constituent counters of the evicted instances of `pattern`.
    pub fn evicted_counts_of(&self, pattern: PatternId) -> Counts {
        self.evicted_counts
            .get(&pattern)
            .copied()
            .unwrap_or_default()
    }

    fn updates(&self, touched: BTreeSet<ProjectionId>) -> Vec<ProjectionUpdate> {
        touched
            .into_iter()
            .filter_map(|id| {
                self.instances.get(&id).map(|instance| ProjectionUpdate {
                    id,
                    revision: instance.revision,
                    instance: instance.clone(),
                })
            })
            .collect()
    }

    // ── activities ──────────────────────────────────────────────────────────

    fn apply_activity(&mut self, event: &ActivityEvent, touched: &mut BTreeSet<ProjectionId>) {
        let member = ConstituentId::of(event);
        let seq = event.ingest_seq;

        if let Some(home) = self.homes.get(&member).copied() {
            let status = self
                .instances
                .get_mut(&home)
                .and_then(|instance| instance.update_member(&member, event));
            touched.insert(home);
            self.seal_finished_job(home, &member, seq, touched);
            if let Some(status) = status {
                self.cover_agent_failure(event, &member, &status, touched);
            }
            return;
        }

        if breaks_file_instances(event) {
            self.seal_files(&event.owner.session, event.owner.turn, seq, touched);
        }
        let home = self.route_new(event, &member, touched);
        if let Some(instance) = self.instances.get_mut(&home) {
            instance.add_member(member.clone(), event);
        }
        self.homes.insert(member.clone(), home);
        touched.insert(home);
        self.seal_finished_job(home, &member, seq, touched);
        let status = ConstituentStatus::initial(event);
        self.cover_agent_failure(event, &member, &status, touched);
    }

    /// Chooses (or opens) the home instance of a constituent's first event.
    fn route_new(
        &mut self,
        event: &ActivityEvent,
        member: &ConstituentId,
        touched: &mut BTreeSet<ProjectionId>,
    ) -> ProjectionId {
        if matches!(member, ConstituentId::Event(_))
            && ConstituentStatus::initial(event).is_running()
        {
            // Neither a call id nor a run key: nothing can ever finish this
            // observation, so it must not hold a pattern open forever.
            return self.open_standalone(event, Some(Anomaly::Unpaired));
        }
        if !event.descriptor.is_known() {
            return self.open_standalone(event, None);
        }
        match &event.resource {
            Some(ActivityKey::Path(path)) => self.route_path(event, path, touched),
            Some(key @ ActivityKey::Job(_)) => self.route_job(event, key),
            Some(key @ ActivityKey::Call(_)) if event.descriptor.domain == JOB_DOMAIN => {
                self.route_job(event, key)
            }
            _ => self.open_standalone(event, None),
        }
    }

    fn route_path(
        &mut self,
        event: &ActivityEvent,
        path: &str,
        touched: &mut BTreeSet<ProjectionId>,
    ) -> ProjectionId {
        let seq = event.ingest_seq;
        let burst = file_slot(event, PatternId::FILE_READ_BURST, path);
        let loop_slot = file_slot(event, PatternId::FILE_EDIT_READ_LOOP, path);
        match event.descriptor.effect {
            Effect::Observe => {
                if let Some(id) = self.files.get(&loop_slot).copied() {
                    return id;
                }
                if let Some(id) = self.files.get(&burst).copied() {
                    return id;
                }
                self.open_file(event, burst)
            }
            Effect::Mutate | Effect::Create => {
                self.seal_slot(&burst, seq, touched);
                if let Some(id) = self.files.get(&loop_slot).copied() {
                    return id;
                }
                self.open_file(event, loop_slot)
            }
            Effect::Execute
            | Effect::Communicate
            | Effect::Delete
            | Effect::Control
            | Effect::Other => {
                self.seal_slot(&burst, seq, touched);
                self.seal_slot(&loop_slot, seq, touched);
                self.open_standalone(event, None)
            }
        }
    }

    fn route_job(&mut self, event: &ActivityEvent, key: &ActivityKey) -> ProjectionId {
        let slot = KeySlot::new(key, &event.owner.session);
        if let Some(id) = self.keys.get(&slot).copied() {
            let sealed = self
                .instances
                .get(&id)
                .is_none_or(|instance| instance.state.sealed);
            return if sealed {
                // The job is terminal: new calls on it stay individually visible.
                self.open_standalone(event, None)
            } else {
                id
            };
        }
        let seq = event.ingest_seq;
        let instance = PatternInstance::open(
            seq,
            PatternId::JOB_LIFECYCLE,
            Some(key.clone()),
            Scope::Global,
            false,
        );
        let id = instance.id;
        self.instances.insert(id, instance);
        self.keys.insert(slot, id);
        id
    }

    fn open_file(&mut self, event: &ActivityEvent, slot: FileSlot) -> ProjectionId {
        let instance = PatternInstance::open(
            event.ingest_seq,
            slot.pattern,
            Some(ActivityKey::Path(slot.path.clone())),
            Scope::Turn {
                session: slot.session.clone(),
                turn: slot.turn,
            },
            false,
        );
        let id = instance.id;
        self.instances.insert(id, instance);
        self.files.insert(slot, id);
        id
    }

    fn open_standalone(&mut self, event: &ActivityEvent, anomaly: Option<Anomaly>) -> ProjectionId {
        let mut instance = PatternInstance::open(
            event.ingest_seq,
            PatternId::STANDALONE,
            event.resource.clone(),
            Scope::Session {
                session: event.owner.session.clone(),
            },
            true,
        );
        instance.state.anomaly = anomaly;
        let id = instance.id;
        self.instances.insert(id, instance);
        id
    }

    /// Seals a job instance once its run ended, or once its provisional start
    /// call failed before any rebase.
    fn seal_finished_job(
        &mut self,
        home: ProjectionId,
        member: &ConstituentId,
        seq: u64,
        touched: &mut BTreeSet<ProjectionId>,
    ) {
        let Some(instance) = self.instances.get_mut(&home) else {
            return;
        };
        if instance.pattern != PatternId::JOB_LIFECYCLE || instance.state.sealed {
            return;
        }
        let Some(status) = instance.member(member).map(|m| m.status.clone()) else {
            return;
        };
        if status.is_running() {
            return;
        }
        let seal = match (member, &instance.key) {
            (ConstituentId::Key(ActivityKey::Job(_)), _) => true,
            (ConstituentId::Call { call, .. }, Some(ActivityKey::Call(provisional))) => {
                call == provisional && status != ConstituentStatus::Succeeded
            }
            _ => false,
        };
        if seal && instance.seal(seq) {
            touched.insert(home);
        }
    }

    /// Covers a failed child agent run with an `agent-role-failure-burst@1`.
    fn cover_agent_failure(
        &mut self,
        event: &ActivityEvent,
        member: &ConstituentId,
        status: &ConstituentStatus,
        touched: &mut BTreeSet<ProjectionId>,
    ) {
        let ConstituentId::Key(ActivityKey::Agent(_)) = member else {
            return;
        };
        let ConstituentStatus::Failed { class } = status else {
            return;
        };
        // Only agent-runtime semantics aggregate (PL-71 I5): the event that
        // reports the failure must be one, and so must the descriptor that
        // introduced the run.
        if !is_agent_run(&event.descriptor) || self.burst_of.contains_key(member) {
            return;
        }
        let Some(copy) = self
            .homes
            .get(member)
            .and_then(|home| self.instances.get(home))
            .and_then(|home| home.member(member))
            .filter(|run| run.is_known() && run.domain == AGENT_DOMAIN)
            .cloned()
        else {
            return;
        };

        let seq = event.ingest_seq;
        let owner = &event.owner;
        let slot = match (&owner.parent, &owner.role, class) {
            (Some(parent), Some(role), Some(class)) => Some(BurstSlot {
                parent: parent.clone(),
                role: role.clone(),
                class: class.clone(),
            }),
            _ => None,
        };
        let joinable = slot
            .as_ref()
            .and_then(|slot| self.bursts.get(slot).copied());
        let id = match joinable {
            Some(id) => id,
            None => {
                // Incomplete grouping attributes: a burst of its own, sealed
                // at once, that never merges with other failures.
                let instance = PatternInstance::open(
                    seq,
                    PatternId::AGENT_ROLE_FAILURE_BURST,
                    None,
                    Scope::Parent {
                        parent: owner.parent.clone(),
                        role: owner.role.clone(),
                        failure_class: class.clone(),
                    },
                    slot.is_none(),
                );
                let id = instance.id;
                self.instances.insert(id, instance);
                if let Some(slot) = slot {
                    self.bursts.insert(slot, id);
                }
                id
            }
        };
        if let Some(instance) = self.instances.get_mut(&id) {
            instance.add_member_copy(copy, seq);
        }
        self.burst_of.insert(member.clone(), id);
        touched.insert(id);
    }

    // ── boundaries ──────────────────────────────────────────────────────────

    fn apply_boundary(
        &mut self,
        event: &ActivityEvent,
        boundary: Boundary,
        touched: &mut BTreeSet<ProjectionId>,
    ) {
        let session = event.owner.session.as_str();
        let seq = event.ingest_seq;
        let turn = match boundary {
            Boundary::TurnEnd => event.owner.turn,
            Boundary::SessionEnd | Boundary::Timeout => None,
        };
        self.seal_files(session, turn, seq, touched);

        let bursts: Vec<BurstSlot> = self
            .bursts
            .keys()
            .filter(|slot| slot.parent == session)
            .cloned()
            .collect();
        for slot in bursts {
            if let Some(id) = self.bursts.remove(&slot) {
                self.seal_instance(id, seq, touched);
            }
        }
    }

    /// Seals the open file instances of `session`; with `Some(turn)` only
    /// those of that turn (and those recorded without a turn).
    fn seal_files(
        &mut self,
        session: &str,
        turn: Option<u64>,
        seq: u64,
        touched: &mut BTreeSet<ProjectionId>,
    ) {
        let slots: Vec<FileSlot> = self
            .files
            .keys()
            .filter(|slot| {
                slot.session == session
                    && (turn.is_none() || slot.turn.is_none() || slot.turn == turn)
            })
            .cloned()
            .collect();
        for slot in slots {
            self.seal_slot(&slot, seq, touched);
        }
    }

    fn seal_slot(&mut self, slot: &FileSlot, seq: u64, touched: &mut BTreeSet<ProjectionId>) {
        if let Some(id) = self.files.remove(slot) {
            self.seal_instance(id, seq, touched);
        }
    }

    fn seal_instance(&mut self, id: ProjectionId, seq: u64, touched: &mut BTreeSet<ProjectionId>) {
        if self
            .instances
            .get_mut(&id)
            .is_some_and(|instance| instance.seal(seq))
        {
            touched.insert(id);
        }
    }

    // ── rebase ──────────────────────────────────────────────────────────────

    fn apply_rebase(
        &mut self,
        event: &ActivityEvent,
        to: &ActivityKey,
        touched: &mut BTreeSet<ProjectionId>,
    ) {
        let rebound = if matches!(event.outcome, Some(Outcome::Failed { .. })) {
            // Fail closed: a binding that reports a failure is not trusted,
            // and the failure must stay visible.
            Rebound::Standalone(Some(Anomaly::RebaseFailed))
        } else if matches!(event.outcome, Some(Outcome::Cancelled)) {
            // Cancellation does not establish that the binding occurred.
            Rebound::Standalone(None)
        } else {
            match (&event.resource, to) {
                (Some(from @ ActivityKey::Call(_)), ActivityKey::Job(_)) => {
                    self.rebase_job(event, from, to, touched)
                }
                (Some(ActivityKey::Path(old)), ActivityKey::Path(new)) => {
                    self.rebase_path(event, old, new, touched)
                }
                _ => Rebound::Standalone(Some(Anomaly::RebaseUnmatched)),
            }
        };
        if let Rebound::Standalone(anomaly) = rebound {
            // The event stays visible as an instance of its own.
            let id = self.open_standalone(event, anomaly);
            if let Some(instance) = self.instances.get_mut(&id) {
                instance.add_member(ConstituentId::Event(event.ingest_seq), event);
            }
            touched.insert(id);
        }
    }

    fn rebase_job(
        &mut self,
        event: &ActivityEvent,
        from: &ActivityKey,
        to: &ActivityKey,
        touched: &mut BTreeSet<ProjectionId>,
    ) -> Rebound {
        let session = event.owner.session.as_str();
        let seq = event.ingest_seq;
        let Some(id) = self.keys.get(&KeySlot::new(from, session)).copied() else {
            return Rebound::Standalone(Some(Anomaly::RebaseUnmatched));
        };
        let to_slot = KeySlot::new(to, session);
        match self.keys.get(&to_slot).copied() {
            Some(bound) if bound != id => {
                return Rebound::Standalone(Some(Anomaly::RebaseConflict));
            }
            Some(_) => {
                // Already bound (repeated binding): record it as provenance only.
                if let Some(instance) = self.instances.get_mut(&id) {
                    instance.record(seq);
                }
                touched.insert(id);
                return Rebound::Applied;
            }
            None => {}
        }
        let run = ConstituentId::Key(to.clone());
        if self.homes.get(&run).is_some_and(|home| *home != id) {
            return Rebound::Standalone(Some(Anomaly::RebaseConflict));
        }
        let Some(instance) = self.instances.get_mut(&id) else {
            return Rebound::Standalone(Some(Anomaly::RebaseUnmatched));
        };
        if matches!(instance.key, Some(ActivityKey::Job(_))) {
            // One instance, one work id: binding `to` was handled above, so
            // this would bind a second, different work id and merge two jobs.
            return Rebound::Standalone(Some(Anomaly::RebaseConflict));
        }
        self.keys.insert(to_slot, id);
        instance.key = Some(to.clone());
        if !instance.state.aliases.contains(from) {
            instance.state.aliases.push(from.clone());
        }
        if !instance.state.sealed && instance.member(&run).is_none() {
            // The binding proves the job exists: from here on its run is a
            // running constituent until the runtime reports its end.
            let descriptor = ActivityDescriptor::JOB_RUN;
            instance.add_member_copy(
                Constituent {
                    id: run.clone(),
                    domain: descriptor.domain,
                    verb: descriptor.verb,
                    effect: descriptor.effect,
                    resource: Some(to.clone()),
                    status: ConstituentStatus::Running,
                    first_seq: seq,
                    last_seq: seq,
                },
                seq,
            );
            self.homes.insert(run, id);
        } else {
            instance.record(seq);
        }
        touched.insert(id);
        Rebound::Applied
    }

    fn rebase_path(
        &mut self,
        event: &ActivityEvent,
        old: &str,
        new: &str,
        touched: &mut BTreeSet<ProjectionId>,
    ) -> Rebound {
        let seq = event.ingest_seq;
        let open_slots = |reducer: &Self, path: &str| -> Vec<(FileSlot, ProjectionId)> {
            [PatternId::FILE_READ_BURST, PatternId::FILE_EDIT_READ_LOOP]
                .into_iter()
                .filter_map(|pattern| {
                    let slot = file_slot(event, pattern, path);
                    reducer.files.get(&slot).copied().map(|id| (slot, id))
                })
                .collect()
        };
        let found = open_slots(self, old);
        if old != new {
            let replaced = open_slots(self, new);
            if !replaced.is_empty() {
                // The rename replaced `new`: what was observed of it is over.
                for (slot, _) in &replaced {
                    self.seal_slot(slot, seq, touched);
                }
                if !found.is_empty() {
                    // Rebinding `old` as well would put two histories under
                    // one key in one turn: refuse visibly, never merge.
                    return Rebound::Standalone(Some(Anomaly::RebaseConflict));
                }
            }
        }
        if found.is_empty() {
            // Nothing of `old` was observed in this turn: a normal event.
            return Rebound::Standalone(None);
        }
        for (slot, id) in found {
            if old != new {
                self.files.remove(&slot);
                self.files.insert(file_slot(event, slot.pattern, new), id);
            }
            if let Some(instance) = self.instances.get_mut(&id) {
                let old_key = ActivityKey::Path(old.to_owned());
                if old != new && !instance.state.aliases.contains(&old_key) {
                    instance.state.aliases.push(old_key);
                }
                instance.key = Some(ActivityKey::Path(new.to_owned()));
                instance.record(seq);
            }
            touched.insert(id);
        }
        Rebound::Applied
    }
}

/// Result of a rebase attempt.
enum Rebound {
    /// The binding was applied to existing instances.
    Applied,
    /// The rebase event stays visible as a standalone instance of its own,
    /// flagged with the anomaly, if any.
    Standalone(Option<Anomaly>),
}

fn file_slot(event: &ActivityEvent, pattern: PatternId, path: &str) -> FileSlot {
    FileSlot {
        session: event.owner.session.clone(),
        turn: event.owner.turn,
        pattern,
        path: path.to_owned(),
    }
}

/// Adds `more` to `total`, field by field (saturating).
fn add_counts(total: &mut Counts, more: &Counts) {
    total.total = total.total.saturating_add(more.total);
    total.running = total.running.saturating_add(more.running);
    total.succeeded = total.succeeded.saturating_add(more.succeeded);
    total.failed = total.failed.saturating_add(more.failed);
    total.cancelled = total.cancelled.saturating_add(more.cancelled);
}

/// `true` for known agent-runtime semantics (child agent run events).
fn is_agent_run(descriptor: &ActivityDescriptor) -> bool {
    descriptor.is_known() && descriptor.domain == AGENT_DOMAIN
}

/// `true` when a new constituent must close the open file instances of its
/// session turn: unknown semantics, an [`Effect::Execute`] (its side effects
/// are not described by any key, a `Path` key included), or another possible
/// mutation without a `Path` key that would describe it.
fn breaks_file_instances(event: &ActivityEvent) -> bool {
    let effect = event.descriptor.effect;
    !event.descriptor.is_known()
        || effect == Effect::Execute
        || (effect.may_mutate() && !matches!(event.resource, Some(ActivityKey::Path(_))))
}

#[cfg(test)]
mod tests;

/// Tests of the bounded retention API ([`Reducer::evict_closed`]).
#[cfg(test)]
mod retention_tests {
    use super::*;
    use crate::event::ActivityOwner;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const FS_READ: ActivityDescriptor =
        ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe);
    const JOB_START: ActivityDescriptor =
        ActivityDescriptor::new("job", "start", None, Effect::Execute);

    fn read(seq: u64, turn: u64, call: &str, path: &str, lifecycle: Lifecycle) -> ActivityEvent {
        ActivityEvent::new(
            seq,
            ActivityOwner::session("root").with_turn(turn).with_call(call),
            FS_READ,
            lifecycle,
        )
        .with_resource(ActivityKey::Path(path.to_owned()))
    }

    fn turn_end(seq: u64, turn: u64) -> ActivityEvent {
        ActivityEvent::new(
            seq,
            ActivityOwner::session("root").with_turn(turn),
            ActivityDescriptor::SCOPE,
            Lifecycle::Boundary(Boundary::TurnEnd),
        )
    }

    fn call_id(call: &str) -> ConstituentId {
        ConstituentId::Call {
            session: "root".to_owned(),
            call: call.to_owned(),
        }
    }

    fn burst(opened_at: u64) -> ProjectionId {
        ProjectionId {
            opened_at,
            pattern: PatternId::FILE_READ_BURST,
        }
    }

    /// Two closed bursts (one with a failure) and one open burst.
    fn stream() -> Vec<ActivityEvent> {
        vec![
            read(1, 1, "c1", "a", Lifecycle::Started),
            read(2, 1, "c1", "a", Lifecycle::Finished).with_outcome(Outcome::failed("io")),
            read(3, 1, "c2", "b", Lifecycle::Started),
            read(4, 1, "c2", "b", Lifecycle::Finished),
            turn_end(5, 1),
            read(6, 2, "c3", "c", Lifecycle::Started),
        ]
    }

    #[test]
    fn eviction_drops_only_closed_instances_and_keeps_the_counters() -> TestResult {
        let mut reducer = Reducer::replay(&stream());
        assert_eq!(reducer.closed_instances(), 2);
        let failed_before: u64 = reducer.instances().map(PatternInstance::failed).sum();
        assert_eq!(failed_before, 1);

        // Oldest closed instance first; the open burst `c` is never a candidate.
        assert_eq!(reducer.evict_closed(1), vec![burst(1)]);
        assert!(reducer.instance(&burst(1)).is_none());
        assert!(reducer.instance(&burst(3)).is_some());
        assert!(reducer.instance(&burst(6)).is_some());
        assert_eq!(reducer.projection_of(&call_id("c1")), None);
        assert_eq!(reducer.projection_of(&call_id("c2")), Some(burst(3)));

        // Counters survive the eviction: no failure leaves the totals.
        assert_eq!(reducer.evicted_instances(), 1);
        let evicted = reducer.evicted_counts();
        assert_eq!((evicted.total, evicted.failed), (1, 1));
        assert_eq!(
            reducer.evicted_counts_of(PatternId::FILE_READ_BURST),
            evicted
        );
        assert_eq!(
            reducer.evicted_counts_of(PatternId::JOB_LIFECYCLE),
            Counts::default()
        );
        let live_failed: u64 = reducer.instances().map(PatternInstance::failed).sum();
        assert_eq!(live_failed + evicted.failed, failed_before);
        assert_eq!(reducer.last_seq(), Some(6));
        assert_eq!(reducer.stale_events(), 0);

        // keep = 0 evicts the remaining closed burst, never the open one.
        assert_eq!(reducer.evict_closed(0), vec![burst(3)]);
        assert!(reducer.evict_closed(0).is_empty());
        assert_eq!(reducer.closed_instances(), 0);
        assert_eq!(reducer.evicted_instances(), 2);
        assert_eq!(reducer.evicted_counts().total, 2);
        let open = reducer
            .instance(&burst(6))
            .ok_or("the open burst must stay")?;
        assert_eq!(open.state.counts.running, 1);

        // The open burst keeps reducing normally after evictions.
        let updates = reducer.apply(&read(7, 2, "c3", "c", Lifecycle::Finished));
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, burst(6));
        Ok(())
    }

    #[test]
    fn eviction_is_deterministic_and_replayable() {
        let events = stream();
        let run = || {
            let mut reducer = Reducer::new();
            let mut evicted = Vec::new();
            for event in &events {
                reducer.apply(event);
                evicted.extend(reducer.evict_closed(1));
            }
            (reducer, evicted)
        };
        let (first, first_evicted) = run();
        let (second, second_evicted) = run();
        assert_eq!(first, second);
        assert_eq!(first_evicted, second_evicted);
        assert_eq!(first_evicted, vec![burst(1)]);
    }

    #[test]
    fn a_running_job_is_kept_and_its_index_goes_with_the_eviction() -> TestResult {
        let owner = || ActivityOwner::session("root").with_turn(1).with_call("s");
        let mut reducer = Reducer::new();
        reducer.apply(
            &ActivityEvent::new(1, owner(), JOB_START, Lifecycle::Started)
                .with_resource(ActivityKey::Call("s".into())),
        );
        reducer.apply(
            &ActivityEvent::new(2, owner(), JOB_START, Lifecycle::Finished)
                .with_resource(ActivityKey::Call("s".into())),
        );
        reducer.apply(
            &ActivityEvent::new(
                3,
                ActivityOwner::session("root").with_turn(1),
                JOB_START,
                Lifecycle::Rebase {
                    to: ActivityKey::Job("W".into()),
                },
            )
            .with_resource(ActivityKey::Call("s".into())),
        );
        // The run is still going: nothing to evict.
        assert!(reducer.evict_closed(0).is_empty());
        let job = reducer
            .job_projection("W")
            .ok_or("the job is bound to W")?;

        reducer.apply(
            &ActivityEvent::new(
                4,
                ActivityOwner::session("root"),
                ActivityDescriptor::JOB_RUN,
                Lifecycle::Finished,
            )
            .with_resource(ActivityKey::Job("W".into()))
            .with_outcome(Outcome::Succeeded),
        );
        assert_eq!(reducer.evict_closed(0), vec![job]);
        assert_eq!(reducer.job_projection("W"), None);
        let counts = reducer.evicted_counts_of(PatternId::JOB_LIFECYCLE);
        assert_eq!((counts.total, counts.succeeded, counts.failed), (2, 2, 0));
        Ok(())
    }
}

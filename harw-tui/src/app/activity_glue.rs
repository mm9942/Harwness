//! PL-97 M3: activity adapter of the TUI history (PL-71, plan §3.5 "TUI
//! adapter").
//!
//! # Responsibility
//! Feeds the lifecycle of the root session's tool cells into one
//! [`harw_activity::Reducer`] and lets its projections decide where a new
//! cell goes ([`append_tool_cell`]):
//! - `file-read-burst@1`: an observing call on a path (descriptor effect
//!   `observe` and a `Path` key, see [`harw_registry_defaults::activity_for`])
//!   joins the group of its burst. Bursts that open one after another share
//!   one [`ToolGroupKind::Reads`] group, so the tally line looks exactly as
//!   before (`"2 Dateien gelesen, 1 Muster gesucht"`).
//! - `file-edit-read-loop@1`: an edit or write of a path opens a group of its
//!   own; the observing calls on the same path that follow in the turn join
//!   it in place (`A · Edit ×1 · Read ×3`).
//! - `job-lifecycle@1`: `job.start` opens a group keyed by its call; its
//!   result binds the job id (rebase `Call(C) → Job(W)`), and
//!   `job.status`/`job.logs`/`job.stop` on that job update the same cell. The
//!   job ends when a tool result or the jobs panel reports a terminal state
//!   ([`ActivityGlue::sync_jobs`]).
//! - `agent-role-failure-burst@1`: child agent runs from the agent monitor
//!   ([`ActivityGlue::sync_agents`]); the bursts back the unseen-failure badge
//!   of the Finished / Failed window ([`ActivityGlue::failure_badges`]).
//! - everything else stays a single cell (standalone, PL-71 I5).
//!
//! # Adapter contract (`harw-activity`)
//! One sequencer (a local counter), pairing through the tool call id, the
//! rebase of a job before any event keyed by its job id, the run ended only
//! by a runtime-side `Finished` without call id, and turn/session ends as
//! boundary events: a new turn id or a user message ends the turn, a new
//! session id ends the session.
//!
//! # Bounds (PL-97 security review, finding 7)
//! The reducer keeps at most [`RETAINED_CLOSED_PROJECTIONS`] closed
//! projections ([`harw_activity::Reducer::evict_closed`]; their counters are
//! preserved); bindings of evicted projections go with them. Remembered
//! calls and agents are capped ([`MAX_TRACKED_CALLS`],
//! [`MAX_TRACKED_AGENTS`]), finished ones go first. The cells themselves stay
//! in the history; only the adapter forgets them.
//!
//! # Not an authority
//! Grouping changes the layout of the history and nothing else: no
//! permission, approval or sandbox decision reads it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use harw_activity::{
    ActivityDescriptor, ActivityEvent, ActivityKey, ActivityOwner, Boundary, ConstituentId,
    JOB_DOMAIN, Lifecycle, Outcome, PatternId, PatternInstance, ProjectionId, ProjectionUpdate,
    Reducer, Scope,
};
use harw_extension_api::ToolCall;
use harw_protocol::items::ToolCallResult;
use harw_tool_job::JobState;

use super::{ChatApp, ToolCellHandle, ToolHistoryCell};
use crate::agent_monitor::{AgentMonitor, AgentPhase};
use crate::history_cell::{SharedToolCell, ToolGroupCell, ToolGroupKind, ToolState, tool_label};
use crate::mission::finished_window::FailureBurstBadge;
use crate::mission::projection::UNNAMED_AGENT_CATEGORY;
use crate::sanitize::sanitize_inline;

/// Closed projections the reducer keeps; older ones are evicted (their
/// counters stay in [`harw_activity::Reducer::evicted_counts`]).
pub(crate) const RETAINED_CLOSED_PROJECTIONS: usize = 256;

/// Tool calls whose identity the adapter remembers; beyond, the oldest
/// finished ones are forgotten first.
pub(crate) const MAX_TRACKED_CALLS: usize = 1024;

/// Agents whose runs the adapter remembers; beyond, the oldest finished ones
/// are forgotten first.
pub(crate) const MAX_TRACKED_AGENTS: usize = 512;

/// Longest failure class of an agent run (characters).
const FAILURE_CLASS_MAX_CHARS: usize = 80;

/// Separator of the run counter in the key of a repeated agent run.
const RUN_SEPARATOR: &str = "#run";

/// One tool call the adapter has fed into the reducer.
#[derive(Debug, Clone)]
struct CallRecord {
    /// Calling session.
    session: String,
    /// Turn index of the call.
    turn: u64,
    /// Descriptor the call was reduced with.
    descriptor: ActivityDescriptor,
    /// Canonical resource key, if any.
    resource: Option<ActivityKey>,
    /// `ingest_seq` of its start (age for the cap).
    seq: u64,
    /// Its `Finished` was fed.
    finished: bool,
}

/// What the adapter fed for one agent.
#[derive(Debug, Clone, Copy, Default)]
struct AgentFeed {
    /// Run counter (0 = first run); a new run after a finished one counts up.
    run: u32,
    /// The current run's start was fed.
    started: bool,
    /// The current run's end was fed.
    finished: bool,
    /// `ingest_seq` of the last change (age for the cap).
    seq: u64,
}

impl AgentFeed {
    /// Resource key of the current run (`Agent(id)`, then `Agent(id#runN)`).
    fn key(&self, agent: &str) -> String {
        if self.run == 0 {
            agent.to_owned()
        } else {
            format!("{agent}{RUN_SEPARATOR}{}", self.run)
        }
    }
}

/// Where a new tool cell goes, as the reducer decided it.
#[derive(Debug)]
pub(crate) enum Placement {
    /// Into the existing group of its projection. An activity group (edit/read
    /// loop, job lifecycle) takes it in place; a read group only while it is
    /// still the open history tail, otherwise the call starts a new read
    /// burst there ([`append_tool_cell`]).
    Join {
        /// The bound group.
        group: Arc<Mutex<ToolGroupCell>>,
        /// Its projection (rebound when the call moves on to a new group).
        id: ProjectionId,
    },
    /// A new read burst: joins the open read group or opens one.
    ReadBurst(ProjectionId),
    /// A new edit/read loop or job lifecycle: a group of its own.
    Projection {
        /// Projection to bind the group to.
        id: ProjectionId,
        /// Title of its compact line.
        title: String,
        /// First snapshot.
        instance: Box<PatternInstance>,
    },
    /// No pattern: a single cell.
    Single,
}

/// Activity state of one [`ChatApp`] (PL-97 M3).
#[derive(Debug, Default)]
pub(crate) struct ActivityGlue {
    /// The one reducer over the root session's activity stream.
    reducer: Reducer,
    /// Last assigned `ingest_seq`.
    last_seq: u64,
    /// Session the tool calls belong to.
    session: Option<String>,
    /// Current turn index of that session.
    turn: u64,
    /// Turn id the current index belongs to (live turns only).
    turn_key: Option<String>,
    /// Fed tool calls by call id.
    calls: BTreeMap<String, CallRecord>,
    /// Groups bound to their projection.
    groups: BTreeMap<ProjectionId, Arc<Mutex<ToolGroupCell>>>,
    /// Fed agents by agent id.
    agents: BTreeMap<String, AgentFeed>,
}

impl ActivityGlue {
    /// The reducer (read-only).
    #[cfg(test)]
    pub(crate) fn reducer(&self) -> &Reducer {
        &self.reducer
    }

    /// Number of projections with a bound history group.
    #[cfg(test)]
    pub(crate) fn bound_groups(&self) -> usize {
        self.groups.len()
    }

    fn next_seq(&mut self) -> u64 {
        self.last_seq = self.last_seq.saturating_add(1);
        self.last_seq
    }

    /// Applies `event` and refreshes every bound group it touched.
    fn apply(&mut self, event: &ActivityEvent) -> Vec<ProjectionUpdate> {
        let updates = self.reducer.apply(event);
        for update in &updates {
            if let Some(group) = self.groups.get(&update.id)
                && let Ok(mut guard) = group.lock()
            {
                guard.update_projection(&update.instance);
            }
        }
        updates
    }

    /// Boundary event of `session` (turn end with `turn`, else session end).
    fn boundary(&mut self, session: &str, turn: Option<u64>, kind: Boundary) {
        let seq = self.next_seq();
        let mut owner = ActivityOwner::session(session);
        owner.turn = turn;
        let event = ActivityEvent::new(
            seq,
            owner,
            ActivityDescriptor::SCOPE,
            Lifecycle::Boundary(kind),
        );
        self.apply(&event);
        self.retain();
    }

    /// Switches to `session`; the previous session ends.
    fn ensure_session(&mut self, session: &str) {
        if self.session.as_deref() == Some(session) {
            return;
        }
        if let Some(previous) = self.session.take() {
            self.boundary(&previous, None, Boundary::SessionEnd);
        }
        self.session = Some(session.to_owned());
        self.turn_key = None;
    }

    /// Ends the current turn and starts the next one (a user message, or a
    /// new turn id).
    pub(crate) fn next_turn(&mut self) {
        if let Some(session) = self.session.clone() {
            self.boundary(&session, Some(self.turn), Boundary::TurnEnd);
        }
        self.turn = self.turn.saturating_add(1);
        self.turn_key = None;
    }

    /// A live call of turn `key`: a turn id not seen last starts a new turn.
    fn observe_turn(&mut self, key: &str) {
        if self.turn_key.as_deref() == Some(key) {
            return;
        }
        self.next_turn();
        self.turn_key = Some(key.to_owned());
    }

    /// Feeds the start of `call` and returns where its cell goes.
    pub(crate) fn tool_started(
        &mut self,
        session: &str,
        turn: Option<&str>,
        call: &ToolCall,
    ) -> Placement {
        self.ensure_session(session);
        if let Some(turn) = turn {
            self.observe_turn(turn);
        }
        let call_id = call.id.as_str().to_owned();
        if self.calls.contains_key(&call_id) {
            // Derselbe Aufruf zweimal (sollte `ensure_tool_cell` verhindern):
            // nie zweimal reduzieren.
            return Placement::Single;
        }
        let descriptor = harw_registry_defaults::activity_for(call.name.as_str())
            .unwrap_or(ActivityDescriptor::UNKNOWN);
        let resource = resource_key(&descriptor, call);
        let seq = self.next_seq();
        let owner = ActivityOwner::session(session)
            .with_turn(self.turn)
            .with_call(call_id.clone());
        let mut event = ActivityEvent::new(seq, owner, descriptor.clone(), Lifecycle::Started);
        event.resource = resource.clone();
        let updates = self.apply(&event);
        self.calls.insert(
            call_id.clone(),
            CallRecord {
                session: session.to_owned(),
                turn: self.turn,
                descriptor,
                resource,
                seq,
                finished: false,
            },
        );
        self.retain();

        let member = ConstituentId::Call {
            session: session.to_owned(),
            call: call_id,
        };
        let Some(home) = self.reducer.projection_of(&member) else {
            return Placement::Single;
        };
        if let Some(group) = self.groups.get(&home) {
            return Placement::Join {
                group: Arc::clone(group),
                id: home,
            };
        }
        match home.pattern {
            PatternId::FILE_READ_BURST if ToolGroupCell::accepts(call.name.as_str()) => {
                Placement::ReadBurst(home)
            }
            PatternId::FILE_EDIT_READ_LOOP | PatternId::JOB_LIFECYCLE => {
                let instance = updates
                    .into_iter()
                    .find(|update| update.id == home)
                    .map(|update| update.instance)
                    .or_else(|| self.reducer.instance(&home).cloned());
                match instance {
                    Some(instance) => Placement::Projection {
                        id: home,
                        title: projection_title(&instance, call),
                        instance: Box::new(instance),
                    },
                    None => Placement::Single,
                }
            }
            _ => Placement::Single,
        }
    }

    /// Binds `group` to projection `id` (its later updates refresh it).
    pub(crate) fn bind(&mut self, id: ProjectionId, group: &Arc<Mutex<ToolGroupCell>>) {
        if self.reducer.instance(&id).is_some() {
            self.groups.insert(id, Arc::clone(group));
        }
    }

    /// Feeds the end of call `call_id`: its cell reached `state`, with
    /// `result` when the runtime reported one (job ids and job states are
    /// read from it).
    pub(crate) fn tool_finished(
        &mut self,
        call_id: &str,
        state: ToolState,
        result: Option<&ToolCallResult>,
    ) {
        let Some(record) = self.calls.get_mut(call_id) else {
            return;
        };
        if record.finished {
            return;
        }
        record.finished = true;
        let record = record.clone();
        let outcome = match (state, result) {
            (ToolState::Succeeded, _) => Outcome::Succeeded,
            (ToolState::Failed, Some(ToolCallResult::Error { .. })) => Outcome::failed("error"),
            (ToolState::Failed, _) => Outcome::failed("failed"),
            // Ohne Ergebnis beendet: nie als Erfolg zählen (fail closed).
            (ToolState::Running, _) => Outcome::failed("incomplete"),
        };
        let seq = self.next_seq();
        let owner = ActivityOwner::session(record.session.clone())
            .with_turn(record.turn)
            .with_call(call_id);
        let mut event =
            ActivityEvent::new(seq, owner, record.descriptor.clone(), Lifecycle::Finished)
                .with_outcome(outcome);
        event.resource = record.resource.clone();
        self.apply(&event);

        if record.descriptor.domain == JOB_DOMAIN
            && let Some(ToolCallResult::Success { value }) = result
        {
            let job_id = string_field(value, "job_id");
            if let Some(job_id) = job_id {
                if record.descriptor.verb == "start" && state == ToolState::Succeeded {
                    // Regel 2: die Bindung kommt vor jedem Ereignis auf `Job(W)`.
                    self.rebase_job(&record, call_id, &job_id);
                }
                if let Some(outcome) = string_field(value, "state").and_then(|state| {
                    job_outcome(
                        job_state_of(&state)?,
                        string_field(value, "end_reason").map(|reason| failure_class(&reason)),
                    )
                }) {
                    self.job_ended(&job_id, outcome);
                }
            }
        }
        self.retain();
    }

    /// Binds the provisional `Call(call_id)` of a started job to `Job(job_id)`.
    fn rebase_job(&mut self, record: &CallRecord, call_id: &str, job_id: &str) {
        let seq = self.next_seq();
        let owner = ActivityOwner::session(record.session.clone()).with_turn(record.turn);
        let event = ActivityEvent::new(
            seq,
            owner,
            record.descriptor.clone(),
            Lifecycle::Rebase {
                to: ActivityKey::Job(job_id.to_owned()),
            },
        )
        .with_resource(ActivityKey::Call(call_id.to_owned()));
        self.apply(&event);
    }

    /// The job runtime reports the end of `job_id` (from a tool result or
    /// the jobs panel). Only for a job the reducer follows and only once.
    fn job_ended(&mut self, job_id: &str, outcome: Outcome) {
        let open = self
            .reducer
            .job_projection(job_id)
            .and_then(|id| self.reducer.instance(&id))
            .is_some_and(|instance| !instance.state.sealed);
        if !open {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let seq = self.next_seq();
        let event = ActivityEvent::new(
            seq,
            ActivityOwner::session(session),
            ActivityDescriptor::JOB_RUN,
            Lifecycle::Finished,
        )
        .with_resource(ActivityKey::Job(job_id.to_owned()))
        .with_outcome(outcome);
        self.apply(&event);
    }

    /// Ends the jobs the jobs panel shows as finished (the job runtime's own
    /// terminal state, PL-71 adapter rule 3).
    pub(crate) fn sync_jobs(&mut self, jobs: &[crate::jobs_panel::JobRow]) {
        for job in jobs.iter().filter(|job| !job.active) {
            let Some(outcome) = job_outcome(job.state, job.reason.as_deref().map(failure_class))
            else {
                continue;
            };
            self.job_ended(&job.id, outcome);
        }
        self.retain();
    }

    /// Feeds the runs of the agents in `touched` as the monitor shows them:
    /// a start when an agent is first seen active, an end (with its failure
    /// class) when it is finished. A run that starts again after its end is
    /// a new run.
    ///
    /// A finished run also ends its agent's session for the reducer
    /// (`Boundary::SessionEnd`), so the failure bursts of the agents it
    /// started seal and become evictable; a child that ends after its parent
    /// ended seals its parent's bursts again. Only the root session gets
    /// boundaries otherwise, and bursts of grandchildren would stay open.
    pub(crate) fn sync_agents(&mut self, monitor: &AgentMonitor, touched: &[String]) {
        for id in touched {
            let Some(live) = monitor.agent(id) else {
                continue;
            };
            let mut feed = self.agents.get(id).copied().unwrap_or_default();
            let active = live.phase.is_active();
            if active && feed.finished {
                feed = AgentFeed {
                    run: feed.run.saturating_add(1),
                    ..AgentFeed::default()
                };
            }
            let key = ActivityKey::Agent(feed.key(id));
            let owner = agent_owner(id, &live.role, live.parent.as_deref());
            if !feed.started {
                let seq = self.next_seq();
                let event = ActivityEvent::new(
                    seq,
                    owner.clone(),
                    ActivityDescriptor::AGENT_RUN,
                    Lifecycle::Started,
                )
                .with_resource(key.clone());
                self.apply(&event);
                feed.started = true;
                feed.seq = seq;
            }
            if !active && !feed.finished {
                let outcome = match live.phase {
                    AgentPhase::Failed => Outcome::Failed {
                        class: live.failure_reason.as_deref().map(agent_failure_class),
                    },
                    AgentPhase::Cancelled => Outcome::Cancelled,
                    _ => Outcome::Succeeded,
                };
                let seq = self.next_seq();
                let event = ActivityEvent::new(
                    seq,
                    owner,
                    ActivityDescriptor::AGENT_RUN,
                    Lifecycle::Finished,
                )
                .with_resource(key)
                .with_outcome(outcome);
                self.apply(&event);
                feed.finished = true;
                feed.seq = seq;
                self.agents.insert(id.clone(), feed);
                self.boundary(id, None, Boundary::SessionEnd);
                let parent_ended = live
                    .parent
                    .as_deref()
                    .and_then(|parent| self.agents.get(parent))
                    .is_some_and(|parent| parent.finished);
                if let Some(parent) = live.parent.as_deref().filter(|_| parent_ended) {
                    self.boundary(parent, None, Boundary::SessionEnd);
                }
                continue;
            }
            self.agents.insert(id.clone(), feed);
        }
        self.retain();
    }

    /// The `agent-role-failure-burst@1` projections per role, for the
    /// unseen-failure badge of the Finished / Failed window. A covered run
    /// counts as unseen while it is its agent's current run and the monitor
    /// shows it as an unacknowledged failure.
    pub(crate) fn failure_badges(&self, monitor: &AgentMonitor) -> Vec<FailureBurstBadge> {
        let mut by_role: BTreeMap<String, FailureBurstBadge> = BTreeMap::new();
        for instance in self
            .reducer
            .instances()
            .filter(|instance| instance.pattern == PatternId::AGENT_ROLE_FAILURE_BURST)
        {
            let Scope::Parent { role, .. } = &instance.state.scope else {
                continue;
            };
            let role = role
                .clone()
                .unwrap_or_else(|| UNNAMED_AGENT_CATEGORY.to_owned());
            let badge = by_role
                .entry(role.clone())
                .or_insert_with(|| FailureBurstBadge {
                    role,
                    ..FailureBurstBadge::default()
                });
            badge.bursts += 1;
            for member in &instance.state.members {
                if !member.status.is_failed() {
                    continue;
                }
                badge.failed += 1;
                let ConstituentId::Key(ActivityKey::Agent(key)) = &member.id else {
                    continue;
                };
                if !self.is_current_run(key) {
                    continue;
                }
                badge.current_failed += 1;
                if monitor
                    .agent(agent_of_run(key))
                    .is_some_and(crate::agent_monitor::AgentLive::is_unseen_failure)
                {
                    badge.unseen += 1;
                }
            }
        }
        by_role.into_values().collect()
    }

    /// `true` when run `key` is its agent's current run.
    fn is_current_run(&self, key: &str) -> bool {
        let agent = agent_of_run(key);
        self.agents
            .get(agent)
            .is_some_and(|feed| feed.key(agent) == key)
    }

    /// Bounded retention: evicts closed projections beyond
    /// [`RETAINED_CLOSED_PROJECTIONS`] with their bindings and caps the
    /// remembered calls and agents.
    fn retain(&mut self) {
        let evicted = self.reducer.evict_closed(RETAINED_CLOSED_PROJECTIONS);
        for id in &evicted {
            self.groups.remove(id);
        }
        if !evicted.is_empty() {
            let reducer = &self.reducer;
            self.calls.retain(|call, record| {
                !record.finished
                    || reducer
                        .projection_of(&ConstituentId::Call {
                            session: record.session.clone(),
                            call: call.clone(),
                        })
                        .is_some()
            });
        }
        while self.calls.len() > MAX_TRACKED_CALLS {
            // Beendete zuerst, dann die ältesten.
            let oldest = self
                .calls
                .iter()
                .min_by_key(|(_, record)| (!record.finished, record.seq))
                .map(|(call, _)| call.clone());
            match oldest {
                Some(call) => {
                    self.calls.remove(&call);
                }
                None => break,
            }
        }
        while self.agents.len() > MAX_TRACKED_AGENTS {
            let oldest = self
                .agents
                .iter()
                .min_by_key(|(_, feed)| (!feed.finished, feed.seq))
                .map(|(agent, _)| agent.clone());
            match oldest {
                Some(agent) => {
                    self.agents.remove(&agent);
                }
                None => break,
            }
        }
    }
}

/// Places a new tool cell of the root session where the reducer puts it
/// (PL-97 M3; replaces the name-based read grouping).
///
/// # Arguments
/// - `app`: the history that takes the cell.
/// - `call`: the call (id, name, arguments).
/// - `turn`: turn id of a live call, `None` on the approval and resume paths.
/// - `cell`: the new shared cell.
pub(super) fn append_tool_cell(
    app: &mut ChatApp,
    call: &ToolCall,
    turn: Option<&str>,
    cell: SharedToolCell,
) {
    let session = app.session_id().as_str().to_owned();
    match app.activity.tool_started(&session, turn, call) {
        Placement::Join { group, id } => {
            // Ein vergifteter Lock zählt als Lese-Gruppe (nie an alter Stelle).
            let reads = group
                .lock()
                .map(|guard| guard.kind == ToolGroupKind::Reads)
                .unwrap_or(true);
            let open = app
                .open_tool_group
                .as_ref()
                .is_some_and(|open| Arc::ptr_eq(open, &group));
            if reads && !open {
                // Die Lese-Gruppe liegt nicht mehr am Ende der Historie: der
                // Aufruf beginnt dort eine neue (wie vor PL-97 M3).
                append_read_burst(app, id, cell);
            } else if let Ok(mut guard) = group.lock() {
                guard.push(cell);
            }
        }
        Placement::ReadBurst(projection) => append_read_burst(app, projection, cell),
        Placement::Projection {
            id,
            title,
            instance,
        } => {
            app.close_tool_group();
            let group = Arc::new(Mutex::new(ToolGroupCell::for_projection(title, *instance)));
            if let Ok(mut guard) = group.lock() {
                guard.push(cell);
            }
            push_group(app, &group);
            app.activity.bind(id, &group);
        }
        Placement::Single => {
            app.close_tool_group();
            app.push_cell(Box::new(ToolHistoryCell {
                handle: ToolCellHandle::Single(Arc::clone(&cell)),
                verbosity: app.tool_verbosity,
            }));
            app.tool_cells.push(ToolCellHandle::Single(cell));
        }
    }
}

/// Puts `cell` into the open read group (or opens one at the history tail)
/// and binds `projection` to that group.
fn append_read_burst(app: &mut ChatApp, projection: ProjectionId, cell: SharedToolCell) {
    let group = match app.open_tool_group.clone() {
        Some(group) => group,
        None => {
            let group = Arc::new(Mutex::new(ToolGroupCell::new()));
            push_group(app, &group);
            app.open_tool_group = Some(Arc::clone(&group));
            group
        }
    };
    if let Ok(mut guard) = group.lock() {
        guard.push(cell);
    }
    app.activity.bind(projection, &group);
}

/// Feeds the end of a root tool call whose cell was just completed (or
/// marked incomplete).
pub(super) fn tool_completed(
    app: &mut ChatApp,
    call_id: &str,
    cell: &SharedToolCell,
    result: Option<&ToolCallResult>,
) {
    // Ein vergifteter Lock zählt als nicht erfolgreich (fail closed).
    let state = cell
        .lock()
        .map(|guard| guard.state)
        .unwrap_or(ToolState::Failed);
    app.activity.tool_finished(call_id, state, result);
}

/// Appends a group as one history cell with a Ctrl+O handle.
fn push_group(app: &mut ChatApp, group: &Arc<Mutex<ToolGroupCell>>) {
    app.push_cell(Box::new(ToolHistoryCell {
        handle: ToolCellHandle::Group(Arc::clone(group)),
        verbosity: app.tool_verbosity,
    }));
    app.tool_cells
        .push(ToolCellHandle::Group(Arc::clone(group)));
}

/// Canonical resource key of a call from its descriptor: the `path` argument
/// as `Path` (lexically normalized; a missing path is the workspace root
/// `.`), the `job_id` of a job tool as `Job`, the call itself (`Call`) for
/// `job.start`, whose job id only its result names; any other declared field
/// as `Other`.
fn resource_key(descriptor: &ActivityDescriptor, call: &ToolCall) -> Option<ActivityKey> {
    if !descriptor.is_known() {
        return None;
    }
    if descriptor.domain == JOB_DOMAIN && descriptor.verb == "start" {
        return Some(ActivityKey::Call(call.id.as_str().to_owned()));
    }
    let field = descriptor.resource_field.as_deref()?;
    let value = call
        .arguments
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match field {
        "path" => Some(ActivityKey::Path(normalize_path(value.unwrap_or(".")))),
        _ if descriptor.domain == JOB_DOMAIN => value.map(|id| ActivityKey::Job(id.to_owned())),
        _ => value.map(|value| ActivityKey::Other(format!("{field}={value}"))),
    }
}

/// Lexical path key: `./a//b/` → `a/b`, empty → `.`. Display grouping only;
/// the tool resolves the real path inside its sandbox.
fn normalize_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let parts: Vec<&str> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// Title of a projection group: the path of an edit/read loop, the label of
/// `job.start` (`Job(build)`) for a job it started, else `Job(<id>)`.
fn projection_title(instance: &PatternInstance, call: &ToolCall) -> String {
    match (instance.pattern, &instance.key) {
        (PatternId::FILE_EDIT_READ_LOOP, Some(ActivityKey::Path(path))) => path.clone(),
        (PatternId::JOB_LIFECYCLE, Some(key @ ActivityKey::Job(_))) => {
            format!("Job({})", key.value())
        }
        _ => tool_label(call),
    }
}

/// Owner of an agent run: the agent itself with its terminal-safe role
/// (spelled like its Finished / Failed row) and its parent.
fn agent_owner(agent: &str, role: &str, parent: Option<&str>) -> ActivityOwner {
    let role = sanitize_inline(role);
    let role = role.trim();
    let role = if role.is_empty() {
        UNNAMED_AGENT_CATEGORY
    } else {
        role
    };
    let owner = ActivityOwner::session(agent).with_role(role);
    match parent {
        Some(parent) => owner.with_parent(parent),
        None => owner,
    }
}

/// Normalized failure class from a free-text reason: terminal-safe, trimmed,
/// at most [`FAILURE_CLASS_MAX_CHARS`] characters.
fn failure_class(reason: &str) -> String {
    let class = sanitize_inline(reason);
    class.trim().chars().take(FAILURE_CLASS_MAX_CHARS).collect()
}

/// Failure class of an agent run: the part of the reason before the first
/// `:` (`model request failed: req-42` → `model request failed`), so ids and
/// paths after it never open a burst of their own.
fn agent_failure_class(reason: &str) -> String {
    failure_class(reason.split(':').next().unwrap_or(reason))
}

/// Agent id of run key `key` (`id` or `id#runN`).
fn agent_of_run(key: &str) -> &str {
    key.rsplit_once(RUN_SEPARATOR)
        .filter(|(_, run)| run.parse::<u32>().is_ok())
        .map_or(key, |(agent, _)| agent)
}

/// A non-empty string field of a result, also under `status`.
fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .or_else(|| value.get("status").and_then(|status| status.get(key)))
        .and_then(|field| field.as_str())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_owned)
}

/// Typed job state of a `state` field in a `job.*` result.
fn job_state_of(state: &str) -> Option<JobState> {
    serde_json::from_value(serde_json::Value::String(state.to_owned())).ok()
}

/// Outcome of a job state; `None` while the job is not terminal
/// ([`JobState::is_terminal`]). A terminal state without a known result
/// (`unknown`, or one added later) counts as failed with its state name
/// (fail closed, never as success).
fn job_outcome(state: JobState, reason: Option<String>) -> Option<Outcome> {
    if !state.is_terminal() {
        return None;
    }
    Some(match state {
        JobState::Succeeded => Outcome::Succeeded,
        JobState::Stopped => Outcome::Cancelled,
        JobState::Failed => Outcome::failed(reason.unwrap_or_else(|| "failed".to_owned())),
        other => Outcome::failed(other.as_str()),
    })
}

#[cfg(test)]
mod tests {
    use harw_extension_api::ToolName;
    use harw_protocol::TurnEvent;
    use harw_types::{ToolCallId, TurnId};
    use serde_json::json;

    use super::super::{TurnEventState, handle_turn_event, tests::test_chat_app};
    use super::*;
    use crate::style::Theme;
    use crate::test_support::{TestError, TestResult};

    /// Rendered text of every history cell (one string per cell).
    fn rendered(app: &ChatApp) -> Vec<String> {
        app.cells
            .iter()
            .map(|cell| {
                cell.display_lines(100, Theme::Dark)
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    }

    /// One tool call through `handle_turn_event`: requested, then completed
    /// with `result`.
    fn run_tool(
        app: &mut ChatApp,
        state: &mut TurnEventState,
        turn: &TurnId,
        tool: &str,
        arguments: serde_json::Value,
        result: ToolCallResult,
    ) -> ToolCallId {
        let call_id = ToolCallId::new();
        handle_turn_event(
            app,
            state,
            TurnEvent::ToolCallRequested {
                turn_id: turn.clone(),
                call_id: call_id.clone(),
                tool_name: tool.to_owned(),
                arguments,
            },
        );
        handle_turn_event(
            app,
            state,
            TurnEvent::ToolCallCompleted {
                turn_id: turn.clone(),
                call_id: call_id.clone(),
                result,
                duration_ms: 3,
                placement: None,
            },
        );
        call_id
    }

    fn read_ok() -> ToolCallResult {
        ToolCallResult::success(json!({ "content": "x\ny" }))
    }

    fn edit_ok(path: &str) -> ToolCallResult {
        ToolCallResult::success(
            json!({ "replacements": 1, "path": path, "diff_excerpt": "-a\n+b" }),
        )
    }

    /// PL-97 M3: `fs.edit(A)` und drei `fs.read(A)` werden eine Zelle
    /// `A · Edit ×1 · Read ×3`, an Ort und Stelle fortgeschrieben.
    #[test]
    fn edit_then_three_reads_of_one_path_render_one_loop_cell() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        let before = app.cells.len();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.edit",
            json!({ "path": "src/a.rs", "old": "a", "new": "b" }),
            edit_ok("src/a.rs"),
        );
        // Allein sieht die Bearbeitung aus wie bisher.
        let alone = rendered(&app).last().cloned().unwrap_or_default();
        assert!(alone.starts_with("● Edit(src/a.rs)"), "war: {alone:?}");
        for _ in 0..3 {
            run_tool(
                &mut app,
                &mut state,
                &turn,
                "fs.read",
                json!({ "path": "./src/a.rs" }),
                read_ok(),
            );
        }
        assert_eq!(app.cells.len(), before + 1, "eine Zelle für Edit + 3 Reads");
        let cell = rendered(&app).last().cloned().unwrap_or_default();
        let header = cell.lines().next().unwrap_or_default();
        assert_eq!(header, "● src/a.rs · Edit ×1 · Read ×3");
        assert!(!cell.contains("fehlgeschlagen"), "war: {cell:?}");

        // Ctrl+O zeigt die einzelnen Aufrufe.
        assert!(app.toggle_tool_cells());
        let expanded = rendered(&app).last().cloned().unwrap_or_default();
        assert!(expanded.starts_with("● src/a.rs · Edit ×1 · Read ×3"));
        assert!(expanded.contains("Edit(src/a.rs)"), "war: {expanded:?}");
        assert_eq!(expanded.matches("Read(./src/a.rs)").count(), 3);
        Ok(())
    }

    /// PL-97 M3: ein fehlgeschlagener verändernder Aufruf bleibt in der
    /// kompakten Zeile als ✗ sichtbar.
    #[test]
    fn a_failed_edit_keeps_the_cross_visible_in_the_loop() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.edit",
            json!({ "path": "A", "old": "a", "new": "b" }),
            ToolCallResult::error("old string not found"),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.read",
            json!({ "path": "A" }),
            read_ok(),
        );
        let cell = rendered(&app).last().cloned().unwrap_or_default();
        let mut lines = cell.lines();
        assert_eq!(lines.next(), Some("✗ A · Edit ×1 ✗1 · Read ×1"));
        assert_eq!(lines.next(), Some("  ⎿  1 fehlgeschlagen"));
        Ok(())
    }

    /// PL-97 M3: aufeinanderfolgende Lesezugriffe auf verschiedene Pfade
    /// bleiben eine Sammelzeile wie bisher (byte-gleich).
    #[test]
    fn consecutive_reads_of_different_paths_keep_one_tally_line() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        let before = app.cells.len();
        for path in ["a", "b"] {
            run_tool(
                &mut app,
                &mut state,
                &turn,
                "fs.read",
                json!({ "path": path }),
                read_ok(),
            );
        }
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.search",
            json!({ "pattern": "TODO" }),
            ToolCallResult::success(json!({ "matches": [] })),
        );
        assert_eq!(app.cells.len(), before + 1);
        assert_eq!(
            rendered(&app).last().map(String::as_str),
            Some("● 2 Dateien gelesen, 1 Muster gesucht")
        );
        // `shell.exec` schließt die Gruppe; der nächste Lesezugriff beginnt
        // eine neue.
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "shell.exec",
            json!({ "command": "true" }),
            ToolCallResult::success(json!({ "exit_code": 0, "stdout": "", "stderr": "" })),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.read",
            json!({ "path": "a" }),
            read_ok(),
        );
        assert_eq!(app.cells.len(), before + 3);
        assert_eq!(
            rendered(&app).last().map(String::as_str),
            Some("● 1 Datei gelesen")
        );
        Ok(())
    }

    /// PL-97 M3: `job.start`, `job.status`, `job.logs` und das Ende eines
    /// Jobs bleiben eine fortgeschriebene Zelle.
    #[test]
    fn job_lifecycle_collapses_into_one_updating_cell() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        let before = app.cells.len();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.start",
            json!({ "name": "build", "command": "make" }),
            ToolCallResult::success(json!({ "job_id": "job-7", "state": "running" })),
        );
        let alone = rendered(&app).last().cloned().unwrap_or_default();
        assert!(alone.starts_with("● Job(build)"), "war: {alone:?}");
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.status",
            json!({ "job_id": "job-7" }),
            ToolCallResult::success(json!({ "job_id": "job-7", "state": "running" })),
        );
        let running = rendered(&app).last().cloned().unwrap_or_default();
        assert_eq!(
            running.lines().next(),
            Some("● Job(build) · läuft · status ×1")
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.logs",
            json!({ "job_id": "job-7" }),
            ToolCallResult::success(json!({ "job_id": "job-7" })),
        );
        // Ein späterer Turn trifft dieselbe Zelle.
        let later = TurnId::new();
        run_tool(
            &mut app,
            &mut state,
            &later,
            "job.status",
            json!({ "job_id": "job-7" }),
            ToolCallResult::success(json!({ "job_id": "job-7", "state": "succeeded" })),
        );
        assert_eq!(app.cells.len(), before + 1, "eine Zelle je Job");
        let done = rendered(&app).last().cloned().unwrap_or_default();
        assert_eq!(
            done.lines().next(),
            Some("● Job(build) · beendet · status ×2 · logs ×1")
        );
        let job = app
            .activity
            .reducer()
            .job_projection("job-7")
            .and_then(|id| app.activity.reducer().instance(&id))
            .ok_or(TestError::Missing("job projection"))?;
        assert!(job.is_terminal());

        // Nach dem Ende bleibt ein weiterer Aufruf einzeln sichtbar.
        run_tool(
            &mut app,
            &mut state,
            &later,
            "job.status",
            json!({ "job_id": "job-7" }),
            ToolCallResult::success(json!({ "job_id": "job-7", "state": "succeeded" })),
        );
        assert_eq!(app.cells.len(), before + 2);
        Ok(())
    }

    /// PL-97 M3: das Jobs-Panel beendet einen verfolgten Job (Regel 3).
    #[test]
    fn the_jobs_panel_ends_a_followed_job() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.start",
            json!({ "name": "test" }),
            ToolCallResult::success(json!({ "job_id": "job-9", "state": "running" })),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.status",
            json!({ "job_id": "job-9" }),
            ToolCallResult::success(json!({ "job_id": "job-9", "state": "running" })),
        );
        let failed = crate::jobs_panel::JobRow {
            reason: Some("exit 2".to_owned()),
            ..crate::mission::projection::tests::job_in("job-9", JobState::Failed)
        };
        app.activity.sync_jobs(&[failed]);
        let cell = rendered(&app).last().cloned().unwrap_or_default();
        assert_eq!(
            cell.lines().next(),
            Some("✗ Job(test) · fehlgeschlagen · status ×1")
        );
        Ok(())
    }

    /// PL-97 M3 (Review): eine Lese-Gruppe nimmt nur am Ende der Historie
    /// auf; `read(A)`, `edit(B)`, `read(A)` bleiben drei Zellen in dieser
    /// Reihenfolge (byte-gleich zu vorher).
    #[test]
    fn a_read_after_another_cell_starts_a_new_read_group() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        let before = app.cells.len();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.read",
            json!({ "path": "A" }),
            read_ok(),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.edit",
            json!({ "path": "B", "old": "a", "new": "b" }),
            edit_ok("B"),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.read",
            json!({ "path": "A" }),
            read_ok(),
        );
        assert_eq!(app.cells.len(), before + 3);
        let cells = rendered(&app);
        let tail: Vec<&str> = cells
            .iter()
            .skip(before)
            .map(|cell| cell.lines().next().unwrap_or_default())
            .collect();
        assert_eq!(tail.first().copied(), Some("● 1 Datei gelesen"));
        assert!(
            tail.get(1)
                .is_some_and(|line| line.starts_with("● Edit(B)")),
            "war: {tail:?}"
        );
        assert_eq!(tail.get(2).copied(), Some("● 1 Datei gelesen"));
        // Ein weiterer Lesezugriff auf A trifft jetzt die offene, neue Gruppe.
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "fs.read",
            json!({ "path": "A" }),
            read_ok(),
        );
        assert_eq!(app.cells.len(), before + 3);
        assert_eq!(
            rendered(&app).last().map(String::as_str),
            Some("● 2 Dateien gelesen")
        );
        Ok(())
    }

    /// PL-97 M3 (Review): ein Job mit unbekanntem Ende (`JobState::Unknown`)
    /// endet als fehlgeschlagen und wird entfernbar, statt ewig zu laufen.
    #[test]
    fn a_job_with_an_unknown_end_counts_as_failed() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.start",
            json!({ "name": "test" }),
            ToolCallResult::success(json!({ "job_id": "job-u", "state": "running" })),
        );
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.status",
            json!({ "job_id": "job-u" }),
            ToolCallResult::success(json!({ "job_id": "job-u", "state": "running" })),
        );
        app.activity
            .sync_jobs(&[crate::mission::projection::tests::job_in(
                "job-u",
                JobState::Unknown,
            )]);
        let cell = rendered(&app).last().cloned().unwrap_or_default();
        assert_eq!(
            cell.lines().next(),
            Some("✗ Job(test) · fehlgeschlagen · status ×1")
        );
        let job = app
            .activity
            .reducer()
            .job_projection("job-u")
            .and_then(|id| app.activity.reducer().instance(&id))
            .ok_or(TestError::Missing("job projection"))?;
        assert!(job.is_terminal());

        // Auch ein `state: "unknown"` im Ergebnis beendet den Job.
        run_tool(
            &mut app,
            &mut state,
            &turn,
            "job.start",
            json!({ "name": "other" }),
            ToolCallResult::success(json!({ "job_id": "job-v", "state": "unknown" })),
        );
        let job = app
            .activity
            .reducer()
            .job_projection("job-v")
            .and_then(|id| app.activity.reducer().instance(&id))
            .ok_or(TestError::Missing("job projection"))?;
        assert!(job.is_terminal());
        assert_eq!(
            job_outcome(JobState::Running, None),
            None,
            "nur Endzustände beenden"
        );
        Ok(())
    }

    /// PL-97 M3 (Review, Befund 7): Fehlschlag-Bursts von Enkel-Agenten
    /// schließen mit dem Ende ihres Eltern-Agenten; freie Fehlertexte öffnen
    /// keine eigenen Bursts.
    #[test]
    fn failure_bursts_of_nested_agents_seal_with_their_parent() -> TestResult {
        use harw_core::{AgentEvent, AgentEventKind};
        use harw_types::SessionId;

        let nested = |id: &str, turn: TurnEvent| AgentEvent {
            agent: SessionId::from_str(id),
            parent: Some(SessionId::from_str("p")),
            role: "worker".to_owned(),
            kind: AgentEventKind::Turn(turn),
        };
        let failed = |n: usize| TurnEvent::TurnFailed {
            turn_id: TurnId::new(),
            reason: format!("boom: req-{n}"),
            retryable: false,
        };
        let open_bursts = |glue: &ActivityGlue| {
            glue.reducer()
                .instances()
                .filter(|instance| {
                    instance.pattern == PatternId::AGENT_ROLE_FAILURE_BURST
                        && !instance.state.sealed
                })
                .count()
        };
        let mut app = test_chat_app()?;
        crate::mission::tests::thinking(&mut app.agent_monitor, "p", "lead", "plan");
        app.activity
            .sync_agents(&app.agent_monitor, &["p".to_owned()]);
        for n in 0..10 {
            let id = format!("g{n}");
            app.agent_monitor.apply(&nested(&id, failed(n)));
            app.activity.sync_agents(&app.agent_monitor, &[id]);
        }
        assert_eq!(open_bursts(&app.activity), 1, "eine Fehlerklasse");

        app.agent_monitor.apply(&crate::mission::tests::turn_event(
            "p",
            "lead",
            TurnEvent::TurnCompleted {
                turn_id: TurnId::new(),
                usage: None,
            },
        ));
        app.activity
            .sync_agents(&app.agent_monitor, &["p".to_owned()]);
        assert_eq!(open_bursts(&app.activity), 0);

        // Ein Kind, das nach seinem Eltern-Agenten endet, bleibt nicht offen.
        app.agent_monitor.apply(&nested("late", failed(99)));
        app.activity
            .sync_agents(&app.agent_monitor, &["late".to_owned()]);
        assert_eq!(open_bursts(&app.activity), 0);
        assert_eq!(agent_failure_class("boom: req-1"), "boom");
        Ok(())
    }

    /// PL-97 M3: Zellen ohne Muster (unbekannte Werkzeuge) bleiben einzeln.
    #[test]
    fn unknown_tools_stay_single_cells() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn = TurnId::new();
        let before = app.cells.len();
        for _ in 0..2 {
            run_tool(
                &mut app,
                &mut state,
                &turn,
                "custom.thing",
                json!({ "x": 1 }),
                ToolCallResult::success(json!("ok")),
            );
        }
        assert_eq!(app.cells.len(), before + 2);
        assert_eq!(app.activity.bound_groups(), 0);
        Ok(())
    }

    /// PL-97 M3 (Sicherheitsreview, Befund 7): der Reducer der TUI bleibt
    /// begrenzt; die Zähler entfernter Projektionen bleiben erhalten.
    #[test]
    fn the_adapter_bounds_the_reducer_and_keeps_the_counters() {
        let mut glue = ActivityGlue::default();
        let turns = RETAINED_CLOSED_PROJECTIONS + 44;
        for index in 0..turns {
            let call = ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new("fs.read"),
                arguments: json!({ "path": format!("f{index}") }),
            };
            let placement = glue.tool_started("root", None, &call);
            if let Placement::ReadBurst(id) = placement {
                let group = Arc::new(Mutex::new(ToolGroupCell::new()));
                glue.bind(id, &group);
            }
            let state = if index % 10 == 0 {
                ToolState::Failed
            } else {
                ToolState::Succeeded
            };
            glue.tool_finished(call.id.as_str(), state, None);
            glue.next_turn();
        }
        let reducer = glue.reducer();
        assert!(reducer.closed_instances() <= RETAINED_CLOSED_PROJECTIONS);
        assert!(reducer.evicted_instances() > 0);
        assert!(glue.bound_groups() <= RETAINED_CLOSED_PROJECTIONS + 1);
        assert!(glue.calls.len() <= RETAINED_CLOSED_PROJECTIONS + 1);
        let live: u64 = reducer.instances().map(|i| i.state.counts.total).sum();
        let live_failed: u64 = reducer.instances().map(PatternInstance::failed).sum();
        let evicted = reducer.evicted_counts();
        assert_eq!(live + evicted.total, turns as u64);
        assert_eq!(live_failed + evicted.failed, turns.div_ceil(10) as u64);
    }

    /// PL-97 M3: `agent-role-failure-burst@1` speist das NEW-Abzeichen des
    /// Fensters Finished / Failed; quittierte Fehlschläge zählen nicht.
    #[test]
    fn failure_bursts_back_the_unseen_failure_badge() -> TestResult {
        use crate::mission::finished_window::apply_failure_bursts;
        use crate::mission::projection::{TerminalCategory, terminal_aggregate};

        let mut app = test_chat_app()?;
        for id in ["w1", "w2", "w3"] {
            app.agent_monitor.apply(&crate::mission::tests::turn_event(
                id,
                "uia-writer",
                TurnEvent::TurnFailed {
                    turn_id: TurnId::new(),
                    reason: "model request failed".to_owned(),
                    retryable: false,
                },
            ));
        }
        let touched: Vec<String> = ["w1", "w2", "w3"].map(str::to_owned).to_vec();
        app.activity.sync_agents(&app.agent_monitor, &touched);
        let badges = app.activity.failure_badges(&app.agent_monitor);
        assert_eq!(
            badges,
            vec![FailureBurstBadge {
                role: "uia-writer".to_owned(),
                bursts: 1,
                failed: 3,
                current_failed: 3,
                unseen: 3,
            }]
        );

        app.agent_monitor.mark_seen("w2");
        let badges = app.activity.failure_badges(&app.agent_monitor);
        assert_eq!(badges.first().map(|badge| badge.unseen), Some(2));
        let mut rows = terminal_aggregate(&app.agent_monitor, app.agent_monitor.jobs());
        apply_failure_bursts(&mut rows, &badges);
        let row = rows
            .iter()
            .find(|row| row.category == TerminalCategory::Role("uia-writer".to_owned()))
            .ok_or(TestError::Missing("uia-writer row"))?;
        assert_eq!((row.failed, row.unseen_failures), (3, 2));

        // Ein Badge ohne Zeile ändert nichts, und weniger Bursts verstecken
        // nie, was die Zeile selbst zählt (fail visible).
        apply_failure_bursts(&mut rows, &[]);
        let row = rows
            .iter()
            .find(|row| row.category == TerminalCategory::Role("uia-writer".to_owned()))
            .ok_or(TestError::Missing("uia-writer row"))?;
        assert_eq!(row.unseen_failures, 2);
        Ok(())
    }

    /// PL-97 M3: die Deskriptoren der Jobs tragen die Domäne, über die der
    /// Reducer `job.start` in `job-lifecycle@1` legt.
    #[test]
    fn job_descriptors_use_the_reducer_job_domain() -> TestResult {
        let start = harw_registry_defaults::activity_for("job.start")
            .ok_or(TestError::Missing("job.start"))?;
        assert_eq!(start.domain, JOB_DOMAIN);
        let call = ToolCall {
            id: ToolCallId::from_str("c1"),
            name: ToolName::new("job.start"),
            arguments: json!({}),
        };
        assert_eq!(
            resource_key(&start, &call),
            Some(ActivityKey::Call("c1".into()))
        );
        let read =
            harw_registry_defaults::activity_for("fs.read").ok_or(TestError::Missing("fs.read"))?;
        let call = ToolCall {
            id: ToolCallId::from_str("c2"),
            name: ToolName::new("fs.read"),
            arguments: json!({ "path": "./src//a.rs/" }),
        };
        assert_eq!(
            resource_key(&read, &call),
            Some(ActivityKey::Path("src/a.rs".into()))
        );
        assert_eq!(normalize_path(""), ".");
        assert_eq!(normalize_path("/etc/./x"), "/etc/x");
        Ok(())
    }

    /// Kind-Gruppen ohne Reducer bleiben Lese-Gruppen.
    #[test]
    fn a_new_group_is_a_read_group() {
        assert_eq!(ToolGroupCell::new().kind, ToolGroupKind::Reads);
    }

    /// PL-97 M3 (aus PR #76 B11c): `Compact` bleibt der Default; `/verbose`
    /// schaltet `Compact → Verbose → Activity → Compact`, und neue Zellen
    /// rendern im Aktivitätsmodus einzeilig.
    #[test]
    fn verbose_cycles_compact_verbose_activity() -> TestResult {
        use crate::history_cell::ToolVerbosity;
        let mut app = test_chat_app()?;
        let (bus, _receiver) = crate::events::harw_event_channel();
        assert_eq!(app.tool_verbosity, ToolVerbosity::Compact);
        let mut seen = Vec::new();
        for _ in 0..3 {
            let intercept = super::super::local_intercept_for(&app, "/verbose")
                .ok_or(TestError::Missing("verbose"))?;
            super::super::apply_local_intercept(&mut app, intercept, &bus);
            seen.push(app.tool_verbosity);
        }
        assert_eq!(
            seen,
            vec![
                ToolVerbosity::Verbose,
                ToolVerbosity::Activity,
                ToolVerbosity::Compact
            ]
        );
        assert!(
            rendered(&app)
                .iter()
                .any(|cell| cell.contains("Werkzeuganzeige: einzeilig"))
        );

        // Im Aktivitätsmodus ist eine abgeschlossene Zelle genau eine Zeile.
        let intercept = super::super::local_intercept_for(&app, "/verbose")
            .ok_or(TestError::Missing("verbose"))?;
        super::super::apply_local_intercept(&mut app, intercept, &bus);
        let intercept = super::super::local_intercept_for(&app, "/verbose")
            .ok_or(TestError::Missing("verbose"))?;
        super::super::apply_local_intercept(&mut app, intercept, &bus);
        assert_eq!(app.tool_verbosity, ToolVerbosity::Activity);
        let mut state = TurnEventState::default();
        run_tool(
            &mut app,
            &mut state,
            &TurnId::new(),
            "shell.exec",
            json!({ "command": "ls" }),
            ToolCallResult::success(json!({ "exit_code": 0, "stdout": "a\nb\n", "stderr": "" })),
        );
        assert_eq!(
            rendered(&app).last().map(String::as_str),
            Some("● Shell(ls) · 3ms")
        );
        Ok(())
    }
}

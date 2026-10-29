//! Pure, read-only projections for the Mission Control windows (PL-97 C2,
//! C4 / W20-A).
//!
//! Everything is derived from [`AgentMonitor`] and the job snapshot
//! ([`JobRow`]) on every frame; there is no second store. The only state the
//! projections read besides runs and jobs is the acknowledgement of
//! failures, which lives in the monitor as well (agents: `failure_seen`,
//! jobs: [`AgentMonitor::job_failure_seen`]).
//!
//! # Terminal aggregate (W20-A)
//! [`terminal_aggregate`] groups every finished agent run and job by a typed
//! [`TerminalCategory`]:
//! - agent runs by role;
//! - jobs started by a model tool as `tool:<origin_tool>`, jobs the operator
//!   started as `Operator job`;
//! - jobs without provenance as `Unknown job` -- never guessed from the job
//!   name;
//! - a job-backed child agent (`ManagedJobOrigin::AgentChild`) is counted
//!   once, by its agent run: the finished run owns the terminal count. While
//!   the run is unknown to the monitor, or still shows as active although its
//!   job has ended (its terminal event is late or was lost), the job shows as
//!   `Agent child (pending)`; that row is never counted as a category of its
//!   own (no totals, no `NEW`) and is never promoted to another category.
//!
//! A run whose job ended with a different outcome carries a
//! [`TerminalBadge`]. Each row lists at most
//! [`TERMINAL_RETENTION_PER_CATEGORY`] constituents, newest first; its
//! counters cover all of them, so they survive the eviction of a listed
//! constituent ("60 total, 50 listed").
//!
//! The provenance is display only: nothing here feeds an authority decision.

use std::collections::BTreeMap;
use std::time::Instant;

use harw_tool_job::{JobState, ManagedJobOrigin};
use jiff::Timestamp;

use crate::agent_monitor::{AgentLive, AgentMonitor, AgentPhase};
use crate::jobs_panel::JobRow;
use crate::sanitize::sanitize_inline;

/// Constituents one Finished / Failed row lists at most (newest first); the
/// row's counters keep counting every constituent beyond it.
pub(crate) const TERMINAL_RETENTION_PER_CATEGORY: usize = 50;

/// Category label of agent runs whose role is empty after sanitizing (never
/// an empty label in the strip).
pub(crate) const UNNAMED_AGENT_CATEGORY: &str = "agent";

/// Category label of jobs without provenance.
pub(crate) const UNKNOWN_JOB_LABEL: &str = "Unknown job";

/// Category label of jobs the operator started.
pub(crate) const OPERATOR_JOB_LABEL: &str = "Operator job";

/// Label of job-backed child agents whose run the monitor does not know.
pub(crate) const PENDING_CHILD_LABEL: &str = "Agent child (pending)";

/// Prefix of tool-origin job categories (`tool:job.start`).
const TOOL_PREFIX: &str = "tool:";

/// Tool name of a legacy record that kept only the `call_id`.
const UNNAMED_TOOL: &str = "?";

/// Key prefix of agent-role categories ([`TerminalCategory::key`]); tool
/// categories use [`TOOL_PREFIX`], the other variants a fixed key without
/// either prefix, so the keys never collide.
const ROLE_KEY_PREFIX: &str = "role:";

/// Key of [`TerminalCategory::Operator`].
const OPERATOR_KEY: &str = "operator";

/// Key of [`TerminalCategory::UnknownJob`].
const UNKNOWN_JOB_KEY: &str = "unknown-job";

/// Key of [`TerminalCategory::AgentChildPending`].
const PENDING_CHILD_KEY: &str = "agent-child-pending";

/// Terminal-safe role of an agent run ([`UNNAMED_AGENT_CATEGORY`] when
/// nothing printable is left).
fn agent_category(role: &str) -> String {
    let category = sanitize_inline(role);
    let category = category.trim();
    if category.is_empty() {
        UNNAMED_AGENT_CATEGORY.to_owned()
    } else {
        category.to_owned()
    }
}

/// Rows of the Active Jobs window: running jobs first (oldest first, as in
/// the snapshot), then finished ones (newest first) so their detail view stays
/// reachable. A job-backed child agent that its agent run represents has no
/// row of its own ([`JobRow::is_listed`]).
#[must_use]
pub(crate) fn job_window_rows(jobs: &[JobRow]) -> Vec<&JobRow> {
    let mut rows: Vec<&JobRow> = jobs
        .iter()
        .filter(|row| row.active && row.is_listed())
        .collect();
    rows.extend(
        jobs.iter()
            .rev()
            .filter(|row| !row.active && row.is_listed()),
    );
    rows
}

/// Running jobs with a row of their own -- the count every jobs counter shows
/// (Jobs window title, compact summary). A job-backed child agent its active
/// run represents is counted as that agent, never a second time as a job.
#[must_use]
pub(crate) fn running_job_count(jobs: &[JobRow]) -> usize {
    jobs.iter()
        .filter(|row| row.active && row.is_listed())
        .count()
}

/// Header totals `(ok, failed)` of the Finished / Failed rows: the sum over
/// the counted categories only -- the pending-link row (`Agent child
/// (pending)`) never adds to them.
#[must_use]
pub(crate) fn counted_totals(rows: &[TerminalRow]) -> (usize, usize) {
    rows.iter()
        .filter(|row| row.category.is_counted())
        .fold((0, 0), |(ok, failed), row| {
            (ok + row.ok, failed + row.failed)
        })
}

/// Typed category of a Finished / Failed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalCategory {
    /// Agent runs of one role (terminal-safe).
    Role(String),
    /// Jobs started by a model tool call (terminal-safe tool name; empty for
    /// a legacy record that kept only the `call_id`).
    Tool(String),
    /// Jobs started by the operator.
    Operator,
    /// Jobs without provenance.
    UnknownJob,
    /// Finished job-backed child agents whose run the monitor does not know
    /// (yet) or still shows as active.
    AgentChildPending,
}

impl TerminalCategory {
    /// Display label (display only: labels can coincide, see [`Self::key`]).
    #[must_use]
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Role(role) => role.clone(),
            Self::Tool(tool) if tool.trim().is_empty() => format!("{TOOL_PREFIX}{UNNAMED_TOOL}"),
            Self::Tool(tool) => format!("{TOOL_PREFIX}{tool}"),
            Self::Operator => OPERATOR_JOB_LABEL.to_owned(),
            Self::UnknownJob => UNKNOWN_JOB_LABEL.to_owned(),
            Self::AgentChildPending => PENDING_CHILD_LABEL.to_owned(),
        }
    }

    /// Selection key of the row: the variant plus its raw value, so two
    /// categories never share a key even when their labels coincide (an
    /// agent role named `Unknown job` or `tool:job.start` next to that job
    /// category). The window anchors its selection on this key; the label is
    /// never used to find a row.
    #[must_use]
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Role(role) => format!("{ROLE_KEY_PREFIX}{role}"),
            Self::Tool(tool) => format!("{TOOL_PREFIX}{tool}"),
            Self::Operator => OPERATOR_KEY.to_owned(),
            Self::UnknownJob => UNKNOWN_JOB_KEY.to_owned(),
            Self::AgentChildPending => PENDING_CHILD_KEY.to_owned(),
        }
    }

    /// `false` for the pending-link row: it never counts as a category of
    /// its own (no header totals, no `NEW`).
    #[must_use]
    pub(crate) fn is_counted(&self) -> bool {
        !matches!(self, Self::AgentChildPending)
    }
}

/// Outcome of one finished run or job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalOutcome {
    /// Completed successfully.
    Ok,
    /// Failed, or ended with an unknown outcome.
    Failed,
    /// Cancelled or stopped.
    Cancelled,
}

impl TerminalOutcome {
    /// Outcome of a finished agent run (`None` while it is active).
    fn of_agent(phase: AgentPhase) -> Option<Self> {
        match phase {
            AgentPhase::Done => Some(Self::Ok),
            AgentPhase::Failed => Some(Self::Failed),
            AgentPhase::Cancelled => Some(Self::Cancelled),
            AgentPhase::Admitted
            | AgentPhase::Thinking
            | AgentPhase::Tool
            | AgentPhase::Waiting => None,
        }
    }

    /// Outcome of a finished job (`None` while it is not terminal).
    fn of_job(state: JobState) -> Option<Self> {
        match state {
            JobState::Succeeded => Some(Self::Ok),
            JobState::Failed | JobState::Unknown => Some(Self::Failed),
            JobState::Stopped => Some(Self::Cancelled),
            JobState::Queued | JobState::Running | JobState::Detached => None,
        }
    }

    /// Glyph of the outcome (`✓`, `✗`, `⊘`).
    #[must_use]
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Failed => "✗",
            Self::Cancelled => "⊘",
        }
    }
}

/// What a constituent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstituentKind {
    /// A finished agent run.
    Agent,
    /// A finished job.
    Job,
}

/// One finished run or job of a Finished / Failed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalConstituent {
    /// Agent run or job.
    pub(crate) kind: ConstituentKind,
    /// Run or job id (terminal-safe).
    pub(crate) id: String,
    /// Display label: `role#abcd` for runs, the job name for jobs
    /// (terminal-safe).
    pub(crate) label: String,
    /// How it ended.
    pub(crate) outcome: TerminalOutcome,
    /// When it ended, if known.
    pub(crate) at: Option<Timestamp>,
    /// A failure the operator has not acknowledged yet.
    pub(crate) unseen: bool,
    /// Failure reason, if any (terminal-safe).
    pub(crate) reason: Option<String>,
    /// Outcome of the run's job when it differs from the run's own.
    pub(crate) diverged: Option<TerminalOutcome>,
}

/// Flag of a Finished / Failed row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalBadge {
    /// The job of `runs` job-backed child runs ended with a different
    /// outcome than the run itself.
    OutcomeDiverges {
        /// Number of runs with a diverging job outcome.
        runs: usize,
    },
}

/// One aggregate row of the Finished / Failed window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalRow {
    /// Typed category.
    pub(crate) category: TerminalCategory,
    /// Selection key of the row ([`TerminalCategory::key`]; unique per
    /// category, never displayed).
    pub(crate) key: String,
    /// Display label of the category (terminal-safe; display only).
    pub(crate) label: String,
    /// Finished runs or jobs of this category (all of them, listed or not).
    pub(crate) total: usize,
    /// Of these: completed successfully.
    pub(crate) ok: usize,
    /// Of these: failed (or ended with an unknown outcome).
    pub(crate) failed: usize,
    /// Of these: cancelled or stopped.
    pub(crate) cancelled: usize,
    /// Failures the operator has not acknowledged yet (`NEW`).
    pub(crate) unseen_failures: usize,
    /// End of the newest constituent, if known.
    pub(crate) last_at: Option<Timestamp>,
    /// Flag of the row (diverging agent/job outcome).
    pub(crate) badge: Option<TerminalBadge>,
    /// The newest [`TERMINAL_RETENTION_PER_CATEGORY`] constituents, newest
    /// first.
    pub(crate) constituents: Vec<TerminalConstituent>,
}

impl TerminalRow {
    fn new(category: TerminalCategory) -> Self {
        Self {
            key: category.key(),
            label: category.label(),
            category,
            total: 0,
            ok: 0,
            failed: 0,
            cancelled: 0,
            unseen_failures: 0,
            last_at: None,
            badge: None,
            constituents: Vec::new(),
        }
    }

    /// Constituents still listed (at most
    /// [`TERMINAL_RETENTION_PER_CATEGORY`]).
    #[must_use]
    pub(crate) fn listed(&self) -> usize {
        self.constituents.len()
    }

    fn add(&mut self, constituent: TerminalConstituent) {
        self.total += 1;
        match constituent.outcome {
            TerminalOutcome::Ok => self.ok += 1,
            TerminalOutcome::Failed => self.failed += 1,
            TerminalOutcome::Cancelled => self.cancelled += 1,
        }
        if constituent.unseen {
            self.unseen_failures += 1;
        }
        if constituent.diverged.is_some() {
            let runs = match self.badge {
                Some(TerminalBadge::OutcomeDiverges { runs }) => runs + 1,
                None => 1,
            };
            self.badge = Some(TerminalBadge::OutcomeDiverges { runs });
        }
        if constituent.at > self.last_at {
            self.last_at = constituent.at;
        }
        self.constituents.push(constituent);
    }

    /// Newest first, then cut to the retention (counters stay).
    fn retain_newest(&mut self) {
        self.constituents
            .sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
        self.constituents.truncate(TERMINAL_RETENTION_PER_CATEGORY);
    }
}

/// The clocks a projection is computed at: agent runs end on the monotonic
/// clock ([`Instant`]), jobs on the wall clock (`meta.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AsOf {
    instant: Instant,
    wall: Timestamp,
}

impl AsOf {
    /// Both clocks now.
    #[must_use]
    pub(crate) fn now() -> Self {
        Self {
            instant: Instant::now(),
            wall: Timestamp::now(),
        }
    }

    /// Wall-clock time of the monotonic instant `at`.
    fn wall_of(&self, at: Instant) -> Option<Timestamp> {
        self.wall
            .checked_sub(self.instant.saturating_duration_since(at))
            .ok()
    }
}

/// One finished run or job with its category and its untouched key (the
/// monitor's run id or the job id, for acknowledging it).
struct Member {
    category: TerminalCategory,
    key: String,
    constituent: TerminalConstituent,
}

/// Short run id for constituent labels (last four characters).
fn short_id(id: &str) -> String {
    let tail: Vec<char> = id.chars().rev().take(4).collect();
    tail.into_iter().rev().collect()
}

fn agent_member(
    live: &AgentLive,
    outcome: TerminalOutcome,
    job_outcome: Option<TerminalOutcome>,
    as_of: &AsOf,
) -> Member {
    let role = agent_category(&live.role);
    Member {
        key: live.id.clone(),
        constituent: TerminalConstituent {
            kind: ConstituentKind::Agent,
            id: sanitize_inline(&live.id),
            label: format!("{role}#{}", sanitize_inline(&short_id(&live.id))),
            outcome,
            at: live.finished.and_then(|at| as_of.wall_of(at)),
            unseen: live.is_unseen_failure(),
            reason: live.failure_reason.as_deref().map(sanitize_inline),
            diverged: job_outcome.filter(|job| *job != outcome),
        },
        category: TerminalCategory::Role(role),
    }
}

/// Category of a finished job that is not represented by a finished agent
/// run (`None` for a job its finished run owns).
///
/// A job-backed child whose run is unknown, or still shows as active (its
/// terminal event is late or was lost), stays visible as the uncounted
/// pending-link row; only the finished run takes over its count.
fn job_category(monitor: &AgentMonitor, job: &JobRow) -> Option<TerminalCategory> {
    if let Some(child) = job.child_session_id() {
        return match monitor.agent(child) {
            Some(live) if !live.phase.is_active() => None,
            Some(_) | None => Some(TerminalCategory::AgentChildPending),
        };
    }
    Some(match &job.origin {
        // Alle Altdatensätze ohne Werkzeugnamen bilden eine Kategorie.
        Some(ManagedJobOrigin::Tool { tool, .. }) if tool.trim().is_empty() => {
            TerminalCategory::Tool(String::new())
        }
        Some(ManagedJobOrigin::Tool { tool, .. }) => TerminalCategory::Tool(tool.clone()),
        Some(ManagedJobOrigin::Operator) => TerminalCategory::Operator,
        Some(ManagedJobOrigin::AgentChild { .. }) => TerminalCategory::AgentChildPending,
        None => TerminalCategory::UnknownJob,
    })
}

/// Every finished agent run and every finished job not owned by a known run.
fn members(monitor: &AgentMonitor, jobs: &[JobRow], as_of: &AsOf) -> Vec<Member> {
    // Ausgang des (zuletzt gelisteten) beendeten Jobs je Kind-Lauf.
    let mut child_outcomes: BTreeMap<&str, TerminalOutcome> = BTreeMap::new();
    for job in jobs.iter().filter(|job| !job.active) {
        if let (Some(child), Some(outcome)) =
            (job.child_session_id(), TerminalOutcome::of_job(job.state))
        {
            child_outcomes.insert(child, outcome);
        }
    }
    let mut out = Vec::new();
    for (_, live) in monitor.rows() {
        let Some(outcome) = TerminalOutcome::of_agent(live.phase) else {
            continue;
        };
        let job_outcome = child_outcomes.get(live.id.as_str()).copied();
        out.push(agent_member(live, outcome, job_outcome, as_of));
    }
    for job in jobs.iter().filter(|job| !job.active) {
        let Some(outcome) = TerminalOutcome::of_job(job.state) else {
            continue;
        };
        let Some(category) = job_category(monitor, job) else {
            continue;
        };
        let unseen = category.is_counted()
            && outcome == TerminalOutcome::Failed
            && !monitor.job_failure_seen(&job.id);
        out.push(Member {
            category,
            key: job.id.clone(),
            constituent: TerminalConstituent {
                kind: ConstituentKind::Job,
                id: sanitize_inline(&job.id),
                label: job.name.clone(),
                outcome,
                at: job.ended_at,
                unseen,
                reason: job.reason.clone(),
                diverged: None,
            },
        });
    }
    out
}

/// Per-category aggregate of all finished agent runs and jobs as of now (see
/// the module docs).
#[must_use]
pub(crate) fn terminal_aggregate(monitor: &AgentMonitor, jobs: &[JobRow]) -> Vec<TerminalRow> {
    terminal_aggregate_at(monitor, jobs, AsOf::now())
}

/// [`terminal_aggregate`] at the clocks `as_of`.
///
/// Order: counted categories before the pending-link row; categories with
/// unacknowledged failures first, then by total (descending), then by the
/// newest end, then by label, then by key -- deterministic for equal input,
/// also when two labels coincide.
#[must_use]
pub(crate) fn terminal_aggregate_at(
    monitor: &AgentMonitor,
    jobs: &[JobRow],
    as_of: AsOf,
) -> Vec<TerminalRow> {
    let mut rows: Vec<TerminalRow> = Vec::new();
    for member in members(monitor, jobs, &as_of) {
        let index = match rows.iter().position(|row| row.category == member.category) {
            Some(index) => index,
            None => {
                rows.push(TerminalRow::new(member.category));
                rows.len() - 1
            }
        };
        if let Some(row) = rows.get_mut(index) {
            row.add(member.constituent);
        }
    }
    for row in &mut rows {
        row.retain_newest();
    }
    rows.sort_by(|a, b| {
        b.category
            .is_counted()
            .cmp(&a.category.is_counted())
            .then((b.unseen_failures > 0).cmp(&(a.unseen_failures > 0)))
            .then(b.total.cmp(&a.total))
            .then(b.last_at.cmp(&a.last_at))
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.key.cmp(&b.key))
    });
    rows
}

/// Acknowledges every unseen failure of `category` (`Enter`/`a` on its row
/// in Finished / Failed): agent runs through their `failure_seen`, jobs
/// through the monitor's acknowledged set. Nothing is deleted; the counters
/// stay.
///
/// # Returns
/// `true` when a failure was newly acknowledged.
pub(crate) fn acknowledge(monitor: &mut AgentMonitor, category: &TerminalCategory) -> bool {
    let jobs = monitor.jobs().to_vec();
    let targets: Vec<(ConstituentKind, String)> = members(monitor, &jobs, &AsOf::now())
        .into_iter()
        .filter(|member| member.category == *category && member.constituent.unseen)
        .map(|member| (member.constituent.kind, member.key))
        .collect();
    let mut changed = false;
    for (kind, key) in targets {
        changed |= match kind {
            ConstituentKind::Agent => monitor.mark_seen(&key),
            ConstituentKind::Job => monitor.mark_job_failure_seen(&key),
        };
    }
    changed
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::jobs_panel::ChildRunLink;
    use crate::mission::tests::{thinking, turn_event};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_protocol::TurnEvent;
    use harw_types::TurnId;

    /// A job row in `state` (no provenance, not linked).
    pub(crate) fn job_in(id: &str, state: JobState) -> JobRow {
        JobRow {
            id: id.to_owned(),
            name: format!("name-{id}"),
            state,
            progress: "—".to_owned(),
            runtime: "1s".to_owned(),
            active: !state.is_terminal(),
            failed: matches!(state, JobState::Failed | JobState::Unknown),
            reason: None,
            origin: None,
            cgroup: None,
            ended_at: None,
            child_link: ChildRunLink::NotAChild,
        }
    }

    /// A job row from flags; the label only picks the typed state among the
    /// localized labels (falling back to the flags), for older callers.
    pub(crate) fn job(id: &str, active: bool, failed: bool, label: &'static str) -> JobRow {
        let fallback = if active {
            JobState::Running
        } else if failed {
            JobState::Failed
        } else {
            JobState::Succeeded
        };
        let state = [
            JobState::Queued,
            JobState::Running,
            JobState::Succeeded,
            JobState::Failed,
            JobState::Stopped,
            JobState::Detached,
            JobState::Unknown,
        ]
        .into_iter()
        .find(|state| harw_ops::jobs::state_label(*state) == label)
        .unwrap_or(fallback);
        JobRow {
            active,
            failed,
            ..job_in(id, state)
        }
    }

    fn with_origin(row: JobRow, origin: ManagedJobOrigin) -> JobRow {
        JobRow {
            origin: Some(origin),
            ..row
        }
    }

    fn tool(name: &str) -> ManagedJobOrigin {
        ManagedJobOrigin::Tool {
            tool: name.to_owned(),
            call_id: None,
        }
    }

    fn child(session: &str) -> ManagedJobOrigin {
        ManagedJobOrigin::AgentChild {
            child_session_id: session.to_owned(),
        }
    }

    fn done(monitor: &mut AgentMonitor, id: &str, role: &str) {
        monitor.apply(&turn_event(
            id,
            role,
            TurnEvent::TurnCompleted {
                turn_id: TurnId::new(),
                usage: None,
            },
        ));
    }

    fn failed(monitor: &mut AgentMonitor, id: &str, role: &str) {
        monitor.apply(&turn_event(
            id,
            role,
            TurnEvent::TurnFailed {
                turn_id: TurnId::new(),
                reason: "kaputt".to_owned(),
                retryable: false,
            },
        ));
    }

    fn row_of<'a>(rows: &'a [TerminalRow], label: &str) -> TestResult<&'a TerminalRow> {
        rows.iter()
            .find(|row| row.label == label)
            .ok_or(TestError::Missing("terminal row"))
    }

    #[test]
    fn job_window_lists_running_jobs_first_then_finished_newest_first() {
        let jobs = vec![
            job_in("a", JobState::Succeeded),
            job_in("b", JobState::Running),
            job_in("c", JobState::Failed),
            job_in("d", JobState::Running),
        ];
        let ids: Vec<&str> = job_window_rows(&jobs)
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "d", "c", "a"]);
    }

    /// A job-backed child agent its run represents has no row of its own in
    /// the Jobs window (C2 handoff 2); unknown runs keep the job visible.
    #[test]
    fn job_window_leaves_out_children_their_run_represents() {
        let linked = JobRow {
            child_link: ChildRunLink::RunActive,
            ..with_origin(job_in("linked", JobState::Running), child("c1"))
        };
        let pending = JobRow {
            child_link: ChildRunLink::Pending,
            ..with_origin(job_in("pending", JobState::Running), child("c2"))
        };
        let ended = JobRow {
            child_link: ChildRunLink::RunEnded,
            ..with_origin(job_in("ended", JobState::Succeeded), child("c3"))
        };
        let jobs = vec![linked, pending, ended, job_in("plain", JobState::Running)];
        let ids: Vec<&str> = job_window_rows(&jobs)
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, vec!["pending", "plain"]);
    }

    #[test]
    fn terminal_aggregate_counts_roles_and_jobs_by_state() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "run", "uia-worker", "");
        done(&mut monitor, "w1", "uia-worker");
        done(&mut monitor, "w2", "uia-worker");
        failed(&mut monitor, "w3", "uia-worker");
        done(&mut monitor, "e1", "uia-explorer");
        monitor.set_jobs(vec![
            job_in("j1", JobState::Succeeded),
            job_in("j2", JobState::Failed),
            job_in("j3", JobState::Stopped),
            job_in("j4", JobState::Running),
        ]);
        let jobs = monitor.jobs().to_vec();
        let rows = terminal_aggregate(&monitor, &jobs);
        let counts: Vec<(&str, usize, usize, usize, usize, usize)> = rows
            .iter()
            .map(|row| {
                (
                    row.label.as_str(),
                    row.total,
                    row.ok,
                    row.failed,
                    row.cancelled,
                    row.unseen_failures,
                )
            })
            .collect();
        assert_eq!(
            counts,
            vec![
                ("uia-worker", 3, 2, 1, 0, 1),
                ("Unknown job", 3, 1, 1, 1, 1),
                ("uia-explorer", 1, 1, 0, 0, 0),
            ],
            "the running worker and the running job are not counted; unseen \
             failures first, then by total, then the newest end (the jobs \
             have none)"
        );
        assert_eq!(
            row_of(&rows, "uia-worker")?.category,
            TerminalCategory::Role("uia-worker".to_owned())
        );

        monitor.acknowledge_failures();
        let rows = terminal_aggregate(&monitor, &jobs);
        assert!(rows.iter().all(|row| row.unseen_failures == 0));
        assert_eq!(rows.iter().map(|row| row.total).sum::<usize>(), 7);
        Ok(())
    }

    #[test]
    fn terminal_aggregate_sanitizes_the_role() {
        let mut monitor = AgentMonitor::default();
        done(&mut monitor, "x", "evil\u{1b}]0;title\u{7}role");
        done(&mut monitor, "y", "  ");
        done(&mut monitor, "z", "");
        done(&mut monitor, "c", "\u{1b}[31mfarbig");
        let rows = terminal_aggregate(&monitor, &[]);
        assert!(
            rows.iter()
                .all(|row| !row.label.chars().any(char::is_control)
                    && !row.label.contains("[31m")
                    && !row.label.contains("]0;")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.label == UNNAMED_AGENT_CATEGORY && row.total == 2),
            "empty roles share a fixed label, never an empty one: {rows:?}"
        );
        assert!(rows.iter().all(|row| !row.label.is_empty()), "{rows:?}");
    }

    /// W20-A: a legacy `meta.json` without any origin field is an `Unknown
    /// job` (never guessed from its name), one with the R18 `origin_tool` a
    /// `tool:<name>` job, an operator start an `Operator job`.
    #[test]
    fn job_categories_come_from_provenance_only() -> TestResult {
        let meta = |extra: &str, name: &str| {
            format!(
                r#"{{"version":2,"job_id":"job-{name}","name":"{name}","command":"true",
                   "state":"succeeded",{extra}"harw_instance":"harw-t",
                   "owner":{{"session":"uia"}},"created_at":"2026-01-01T00:00:00Z",
                   "notify_every_secs":60}}"#
            )
        };
        let status = |json: String| -> TestResult<harw_tool_job::JobStatus> {
            Ok(harw_tool_job::JobStatus {
                meta: serde_json::from_str(&json).map_err(ctx("meta.json"))?,
                runtime_secs: Some(1),
                stdout_lines: 0,
                stderr_lines: 0,
                last_lines: Vec::new(),
                log_dir: std::path::PathBuf::from("/nonexistent/harw-job-test"),
            })
        };
        let jobs = vec![
            JobRow::from_status(&status(meta("", "agent-child-explorer"))?),
            JobRow::from_status(&status(meta(
                r#""origin_tool":"job.start","origin_call_id":"call-1","#,
                "tests",
            ))?),
            JobRow::from_status(&status(meta(r#""origin_call_id":"call-2","#, "lint"))?),
            JobRow::from_status(&status(meta(r#""origin":{"kind":"operator"},"#, "op"))?),
        ];
        let rows = terminal_aggregate(&AgentMonitor::default(), &jobs);
        let mut labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            vec!["Operator job", "Unknown job", "tool:?", "tool:job.start"]
        );
        let unknown = row_of(&rows, UNKNOWN_JOB_LABEL)?;
        assert_eq!(unknown.category, TerminalCategory::UnknownJob);
        assert_eq!(
            unknown.constituents.first().map(|c| c.label.as_str()),
            Some("agent-child-explorer"),
            "the name stays a label, never a category"
        );
        assert_eq!(
            row_of(&rows, "tool:job.start")?.category,
            TerminalCategory::Tool("job.start".to_owned())
        );
        Ok(())
    }

    /// W20-A: an agent run and the job that ran it are counted once, by the
    /// run; a job-backed child without a known run is the pending-link row,
    /// never counted as a category of its own.
    #[test]
    fn agent_and_job_of_the_same_child_count_once() -> TestResult {
        let mut monitor = AgentMonitor::default();
        done(&mut monitor, "child-7", "uia-explorer");
        let jobs = vec![
            with_origin(job_in("job-7", JobState::Succeeded), child("child-7")),
            with_origin(job_in("job-9", JobState::Failed), child("child-9")),
        ];
        let rows = terminal_aggregate(&monitor, &jobs);
        let explorer = row_of(&rows, "uia-explorer")?;
        assert_eq!((explorer.total, explorer.ok), (1, 1));
        assert_eq!(explorer.badge, None, "same outcome, no badge");
        assert!(
            !rows
                .iter()
                .any(|row| row.category == TerminalCategory::UnknownJob),
            "{rows:?}"
        );
        let pending = row_of(&rows, PENDING_CHILD_LABEL)?;
        assert_eq!(pending.category, TerminalCategory::AgentChildPending);
        assert_eq!((pending.total, pending.failed), (1, 1));
        assert_eq!(pending.unseen_failures, 0, "no NEW for an unlinked job");
        assert!(!pending.category.is_counted());
        assert_eq!(
            rows.last().map(|row| &row.category),
            Some(&TerminalCategory::AgentChildPending),
            "the pending-link row sorts last"
        );
        assert_eq!(rows.iter().map(|row| row.total).sum::<usize>(), 2);

        // Der Lauf taucht auf: der Job zählt ab jetzt beim Lauf.
        failed(&mut monitor, "child-9", "uia-worker");
        let rows = terminal_aggregate(&monitor, &jobs);
        assert!(
            !rows
                .iter()
                .any(|row| row.category == TerminalCategory::AgentChildPending),
            "{rows:?}"
        );
        assert_eq!(row_of(&rows, "uia-worker")?.total, 1);
        Ok(())
    }

    /// A child's job that ended while its run still shows as active (the
    /// run's terminal event is late or was lost) never disappears: it stays
    /// in the Jobs window and in the uncounted pending-link row (no `NEW`).
    /// Once the run ends, the pair is counted once, by the run, with the
    /// divergence badge.
    #[test]
    fn ended_job_of_a_still_active_run_stays_visible_until_the_run_ends() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "child-1", "uia-explorer", "");
        monitor.set_jobs(vec![with_origin(
            job_in("job-1", JobState::Failed),
            child("child-1"),
        )]);
        assert_eq!(
            monitor.jobs().first().map(|row| row.child_link),
            Some(ChildRunLink::RunActive)
        );
        let ids: Vec<&str> = job_window_rows(monitor.jobs())
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, vec!["job-1"], "the failed job stays visible");
        assert_eq!(running_job_count(monitor.jobs()), 0);
        let rows = terminal_aggregate(&monitor, monitor.jobs());
        let pending = row_of(&rows, PENDING_CHILD_LABEL)?;
        assert_eq!(
            (pending.total, pending.failed, pending.unseen_failures),
            (1, 1, 0),
            "visible, not counted, no NEW"
        );
        assert!(
            !rows.iter().any(|row| row.category.is_counted()),
            "{rows:?}"
        );
        assert_eq!(counted_totals(&rows), (0, 0));

        // Der Lauf endet: ab jetzt zählt er, einmal, mit Abweichungs-Marke.
        done(&mut monitor, "child-1", "uia-explorer");
        assert_eq!(
            monitor.jobs().first().map(|row| row.child_link),
            Some(ChildRunLink::RunEnded)
        );
        assert!(job_window_rows(monitor.jobs()).is_empty());
        let rows = terminal_aggregate(&monitor, monitor.jobs());
        assert_eq!(rows.len(), 1, "no pending row any more: {rows:?}");
        let explorer = row_of(&rows, "uia-explorer")?;
        assert_eq!((explorer.total, explorer.ok, explorer.failed), (1, 1, 0));
        assert_eq!(
            explorer.badge,
            Some(TerminalBadge::OutcomeDiverges { runs: 1 })
        );
        assert_eq!(counted_totals(&rows), (1, 0));
        Ok(())
    }

    /// The counters of the window titles and the compact summary count a
    /// job-backed child once: its running job is the active agent, not a
    /// running job of its own, and the pending-link failure adds nothing to
    /// the totals.
    #[test]
    fn counters_count_a_job_backed_child_once_and_skip_the_pending_row() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "child-1", "uia-explorer", "");
        failed(&mut monitor, "w1", "uia-worker");
        monitor.set_jobs(vec![
            with_origin(job_in("job-1", JobState::Running), child("child-1")),
            with_origin(job_in("job-2", JobState::Failed), child("child-unknown")),
            job_in("job-3", JobState::Running),
        ]);
        assert_eq!(running_job_count(monitor.jobs()), 1, "only job-3");
        let rows = terminal_aggregate(&monitor, monitor.jobs());
        assert_eq!(
            counted_totals(&rows),
            (0, 1),
            "the worker's failure, not the pending job: {rows:?}"
        );
        assert_eq!(row_of(&rows, PENDING_CHILD_LABEL)?.failed, 1);
        Ok(())
    }

    /// A run whose job ended differently is flagged with a badge on its row
    /// and on the constituent.
    #[test]
    fn diverging_run_and_job_outcome_shows_a_badge() -> TestResult {
        let mut monitor = AgentMonitor::default();
        done(&mut monitor, "child-1", "uia-explorer");
        done(&mut monitor, "child-2", "uia-explorer");
        let jobs = vec![
            with_origin(job_in("job-1", JobState::Failed), child("child-1")),
            with_origin(job_in("job-2", JobState::Succeeded), child("child-2")),
        ];
        let rows = terminal_aggregate(&monitor, &jobs);
        let explorer = row_of(&rows, "uia-explorer")?;
        assert_eq!(explorer.total, 2);
        assert_eq!(explorer.ok, 2, "the run owns the outcome");
        assert_eq!(
            explorer.badge,
            Some(TerminalBadge::OutcomeDiverges { runs: 1 })
        );
        let flagged: Vec<&str> = explorer
            .constituents
            .iter()
            .filter(|c| c.diverged == Some(TerminalOutcome::Failed))
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(flagged, vec!["child-1"]);
        Ok(())
    }

    /// Unseen failures stay `NEW` until their row is acknowledged; the
    /// acknowledgement covers runs and jobs of that category only and
    /// deletes nothing.
    #[test]
    fn acknowledging_a_row_clears_new_and_keeps_the_counts() -> TestResult {
        let mut monitor = AgentMonitor::default();
        failed(&mut monitor, "w1", "uia-worker");
        failed(&mut monitor, "e1", "uia-explorer");
        monitor.set_jobs(vec![with_origin(
            job_in("j1", JobState::Failed),
            tool("job.start"),
        )]);
        let before = terminal_aggregate(&monitor, monitor.jobs());
        assert_eq!(
            before.iter().filter(|row| row.unseen_failures > 0).count(),
            3
        );

        let worker = TerminalCategory::Role("uia-worker".to_owned());
        assert!(acknowledge(&mut monitor, &worker));
        assert!(!acknowledge(&mut monitor, &worker), "already acknowledged");
        let tool_category = TerminalCategory::Tool("job.start".to_owned());
        assert!(acknowledge(&mut monitor, &tool_category));
        let after = terminal_aggregate(&monitor, monitor.jobs());
        assert_eq!(row_of(&after, "uia-worker")?.unseen_failures, 0);
        assert_eq!(row_of(&after, "tool:job.start")?.unseen_failures, 0);
        assert_eq!(
            row_of(&after, "uia-explorer")?.unseen_failures,
            1,
            "another category keeps its NEW"
        );
        for label in ["uia-worker", "tool:job.start", "uia-explorer"] {
            let (was, is) = (row_of(&before, label)?, row_of(&after, label)?);
            assert_eq!((was.total, was.failed), (is.total, is.failed), "{label}");
            assert_eq!(was.listed(), is.listed(), "nothing deleted: {label}");
        }
        assert!(
            monitor
                .agent("w1")
                .is_some_and(|live| !live.is_unseen_failure())
        );
        assert!(monitor.job_failure_seen("j1"));
        Ok(())
    }

    /// Retention: a row lists the newest `TERMINAL_RETENTION_PER_CATEGORY`
    /// constituents, its counters keep counting the evicted ones.
    #[test]
    fn retention_keeps_counters_beyond_the_listed_constituents() -> TestResult {
        assert_eq!(TERMINAL_RETENTION_PER_CATEGORY, 50);
        let base = Timestamp::from_second(1_767_225_600).map_err(ctx("timestamp"))?;
        let jobs: Vec<JobRow> = (0..60)
            .map(|index: i64| -> TestResult<JobRow> {
                let state = if index % 6 == 0 {
                    JobState::Failed
                } else {
                    JobState::Succeeded
                };
                Ok(JobRow {
                    ended_at: Some(
                        base.checked_add(jiff::SignedDuration::from_secs(index))
                            .map_err(ctx("ended_at"))?,
                    ),
                    ..with_origin(job_in(&format!("job-{index:02}"), state), tool("job.start"))
                })
            })
            .collect::<TestResult<_>>()?;
        let rows = terminal_aggregate(&AgentMonitor::default(), &jobs);
        let row = row_of(&rows, "tool:job.start")?;
        assert_eq!((row.total, row.listed()), (60, 50));
        assert_eq!((row.ok, row.failed), (50, 10));
        assert_eq!(row.unseen_failures, 10, "evicted failures stay NEW");
        assert_eq!(
            row.constituents.first().map(|c| c.id.as_str()),
            Some("job-59"),
            "newest first"
        );
        assert_eq!(
            row.constituents.last().map(|c| c.id.as_str()),
            Some("job-10")
        );
        assert_eq!(row.last_at, jobs.last().and_then(|job| job.ended_at));

        let mut monitor = AgentMonitor::default();
        for index in 0..60 {
            done(&mut monitor, &format!("w{index:02}"), "uia-worker");
        }
        let rows = terminal_aggregate(&monitor, &[]);
        let worker = row_of(&rows, "uia-worker")?;
        assert_eq!((worker.total, worker.listed()), (60, 50));
        assert!(worker.last_at.is_some());
        Ok(())
    }
}

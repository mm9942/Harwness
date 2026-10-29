//! Pure, read-only projections for the Mission Control windows (PL-97 C2,
//! W20-A groundwork).
//!
//! Everything is derived from [`AgentMonitor`] and the job snapshot
//! ([`JobRow`]) on every frame; there is no second store. The terminal
//! aggregate here is the simple per-role / per-state count of C2. C4 replaces
//! [`terminal_aggregate`] with the typed aggregation (provenance categories,
//! retention, acknowledged failures) behind the same [`TerminalRow`] shape.

use harw_tool_job::JobState;

use crate::agent_monitor::{AgentMonitor, AgentPhase};
use crate::jobs_panel::JobRow;
use crate::sanitize::sanitize_inline;

/// Category of all finished jobs until C4 splits them by provenance.
pub(crate) const JOB_CATEGORY: &str = "job";

/// `true` while an agent run is not finished (admitted, thinking, using a
/// tool or waiting).
#[must_use]
pub(crate) fn is_active(phase: AgentPhase) -> bool {
    matches!(
        phase,
        AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool | AgentPhase::Waiting
    )
}

/// Rows of the Active Jobs window: running jobs first (oldest first, as in
/// the snapshot), then finished ones (newest first) so their detail view stays
/// reachable.
#[must_use]
pub(crate) fn job_window_rows(jobs: &[JobRow]) -> Vec<&JobRow> {
    let mut rows: Vec<&JobRow> = jobs.iter().filter(|row| row.active).collect();
    rows.extend(jobs.iter().rev().filter(|row| !row.active));
    rows
}

/// One aggregate row of the Finished / Failed window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalRow {
    /// Category: the agent role for agent runs, [`JOB_CATEGORY`] for jobs
    /// (terminal-safe).
    pub(crate) category: String,
    /// Finished runs of this category.
    pub(crate) total: usize,
    /// Of these: completed successfully.
    pub(crate) ok: usize,
    /// Of these: failed (or ended with an unknown outcome).
    pub(crate) failed: usize,
    /// Of these: cancelled or stopped.
    pub(crate) cancelled: usize,
    /// Failures the operator has not acknowledged yet (`NEW`).
    pub(crate) unseen_failures: usize,
}

impl TerminalRow {
    fn new(category: String) -> Self {
        Self {
            category,
            total: 0,
            ok: 0,
            failed: 0,
            cancelled: 0,
            unseen_failures: 0,
        }
    }
}

/// Per-category counts of all finished agent runs and jobs.
///
/// Agents count by role (each run key once), jobs under [`JOB_CATEGORY`].
/// Order: categories with unacknowledged failures first, then by total
/// (descending), then by name -- deterministic for equal input.
#[must_use]
pub(crate) fn terminal_aggregate(monitor: &AgentMonitor, jobs: &[JobRow]) -> Vec<TerminalRow> {
    let mut rows: Vec<TerminalRow> = Vec::new();
    let mut row_for = |category: String| -> usize {
        match rows.iter().position(|row| row.category == category) {
            Some(index) => index,
            None => {
                rows.push(TerminalRow::new(category));
                rows.len() - 1
            }
        }
    };
    let mut counted: Vec<(usize, AgentPhase, bool)> = Vec::new();
    for (_, live) in monitor.rows() {
        if is_active(live.phase) {
            continue;
        }
        let index = row_for(sanitize_inline(live.role.trim()));
        counted.push((index, live.phase, live.is_unseen_failure()));
    }
    let stopped = harw_ops::jobs::state_label(JobState::Stopped);
    let mut counted_jobs: Vec<(usize, &JobRow)> = Vec::new();
    for job in jobs.iter().filter(|row| !row.active) {
        counted_jobs.push((row_for(JOB_CATEGORY.to_owned()), job));
    }
    for (index, phase, unseen) in counted {
        if let Some(row) = rows.get_mut(index) {
            row.total += 1;
            match phase {
                AgentPhase::Failed => row.failed += 1,
                AgentPhase::Cancelled => row.cancelled += 1,
                _ => row.ok += 1,
            }
            if unseen {
                row.unseen_failures += 1;
            }
        }
    }
    for (index, job) in counted_jobs {
        if let Some(row) = rows.get_mut(index) {
            row.total += 1;
            if job.failed {
                row.failed += 1;
            } else if job.state == stopped {
                row.cancelled += 1;
            } else {
                row.ok += 1;
            }
        }
    }
    rows.sort_by(|a, b| {
        (b.unseen_failures > 0)
            .cmp(&(a.unseen_failures > 0))
            .then(b.total.cmp(&a.total))
            .then_with(|| a.category.cmp(&b.category))
    });
    rows
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mission::tests::{thinking, turn_event};
    use harw_protocol::TurnEvent;
    use harw_types::TurnId;

    pub(crate) fn job(id: &str, active: bool, failed: bool, state: &'static str) -> JobRow {
        JobRow {
            id: id.to_owned(),
            name: format!("name-{id}"),
            state,
            progress: "—".to_owned(),
            runtime: "1s".to_owned(),
            active,
            failed,
            reason: None,
            origin: None,
            cgroup: None,
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

    #[test]
    fn job_window_lists_running_jobs_first_then_finished_newest_first() {
        let jobs = vec![
            job("a", false, false, "fertig"),
            job("b", true, false, "läuft"),
            job("c", false, true, "fehlgeschlagen"),
            job("d", true, false, "läuft"),
        ];
        let ids: Vec<&str> = job_window_rows(&jobs)
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "d", "c", "a"]);
    }

    #[test]
    fn terminal_aggregate_counts_roles_and_jobs_by_state() {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "run", "uia-worker", "");
        done(&mut monitor, "w1", "uia-worker");
        done(&mut monitor, "w2", "uia-worker");
        failed(&mut monitor, "w3", "uia-worker");
        done(&mut monitor, "e1", "uia-explorer");
        let jobs = vec![
            job("j1", false, false, "fertig"),
            job("j2", false, true, "fehlgeschlagen"),
            job("j3", false, false, "gestoppt"),
            job("j4", true, false, "läuft"),
        ];
        let rows = terminal_aggregate(&monitor, &jobs);
        assert_eq!(
            rows,
            vec![
                TerminalRow {
                    category: "uia-worker".to_owned(),
                    total: 3,
                    ok: 2,
                    failed: 1,
                    cancelled: 0,
                    unseen_failures: 1,
                },
                TerminalRow {
                    category: JOB_CATEGORY.to_owned(),
                    total: 3,
                    ok: 1,
                    failed: 1,
                    cancelled: 1,
                    unseen_failures: 0,
                },
                TerminalRow {
                    category: "uia-explorer".to_owned(),
                    total: 1,
                    ok: 1,
                    failed: 0,
                    cancelled: 0,
                    unseen_failures: 0,
                },
            ],
            "the running worker is not counted; unseen failures come first"
        );

        monitor.acknowledge_failures();
        let rows = terminal_aggregate(&monitor, &jobs);
        assert!(rows.iter().all(|row| row.unseen_failures == 0));
        assert_eq!(
            rows.first().map(|row| row.category.as_str()),
            Some("job"),
            "equal totals sort by name once acknowledged"
        );
    }

    #[test]
    fn terminal_aggregate_sanitizes_the_role() {
        let mut monitor = AgentMonitor::default();
        done(&mut monitor, "x", "evil\u{1b}]0;title\u{7}role");
        let rows = terminal_aggregate(&monitor, &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            rows.iter()
                .all(|row| !row.category.chars().any(char::is_control)),
            "{rows:?}"
        );
    }
}

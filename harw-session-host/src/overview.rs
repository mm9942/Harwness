//! Backend of the global `overview` methods (`jobs.*`, `agents.*`): the
//! durable job records of the profile and the agent library.
//!
//! One implementation serves the gateway and the on-device host. It reads and
//! mutates the job store only through the store's own transitions
//! (`JobStore::list|get|cancel|retry`, the same ones `/ps`, `/cancel` and
//! `/retry` use) and applies the tenant rule of `job_tenant`: a caller with a
//! tenant sees only its own jobs, a foreign job answers like an unknown one.
//! The host admits every call first (hello, feature, caps, tenant); nothing
//! here reads rights or tenants from request params.

use std::sync::Arc;

use harw_job_core::{JobDisposition, JobKind, JobOutcome, JobState, StoredJob};
use harw_protocol::session_wire::{
    AgentDefinition, CommandEffect, CommandRunResult, JobDetail, JobHistoryEntry, JobRow,
    JobsListParams, JobsListResult, MAX_OVERVIEW_ROWS,
};
use harw_session_store::{
    CancelRequest, JobListQuery, JobStore, RetryRequest, SessionStoreError,
};
use harw_types::{ApprovalActor, TenantId, WorkId};
use jiff::Timestamp;

use crate::error::HostError;

/// Source of the agent library (built-in roster and custom definitions).
pub type AgentLibrary = Arc<dyn Fn() -> Vec<AgentDefinition> + Send + Sync>;

/// Records scanned per call at most (bounds the cost of a listing).
const MAX_SCAN: usize = 2_000;
/// Page size of the store listing.
const PAGE: usize = 200;
/// Bound of one `key: value` line of the spec summary (chars).
const SPEC_VALUE_CHARS: usize = 240;
/// Bound of the spec summary lines.
const SPEC_MAX_LINES: usize = 24;
/// Bound of the output tail (chars).
const OUTPUT_TAIL_CHARS: usize = 2_000;
/// Keys of the job input that never appear in the summary.
const SECRET_HINTS: &[&str] = &[
    "secret", "token", "password", "passwd", "auth", "credential", "env", "key",
];
/// Input keys tried (in order) for the row title.
const TITLE_KEYS: &[&str] = &["title", "name", "task", "goal", "summary", "prompt", "command"];
/// Input keys tried (in order) for the session link.
const SESSION_KEYS: &[&str] = &[
    "session_id",
    "parent_session",
    "parent_session_id",
    "owner_session",
];

/// The global jobs and agents backend.
pub struct Overview {
    jobs: Option<Arc<JobStore>>,
    library: Option<AgentLibrary>,
}

impl std::fmt::Debug for Overview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overview")
            .field("jobs", &self.jobs.is_some())
            .field("library", &self.library.is_some())
            .finish()
    }
}

impl Overview {
    /// Backend over `jobs` (`None`: no job store, listings are empty) and an
    /// optional agent library.
    #[must_use]
    pub fn new(jobs: Option<Arc<JobStore>>, library: Option<AgentLibrary>) -> Self {
        Self { jobs, library }
    }

    /// The agent library rows; empty without a source.
    #[must_use]
    pub fn library(&self) -> Vec<AgentDefinition> {
        self.library.as_ref().map(|library| library()).unwrap_or_default()
    }

    /// `jobs.list`: the visible jobs, newest first, filtered by state.
    ///
    /// # Errors
    /// [`HostError::Storage`] when the store cannot be read.
    pub fn jobs_list(
        &self,
        tenant: Option<&TenantId>,
        params: &JobsListParams,
    ) -> Result<JobsListResult, HostError> {
        let Some(store) = &self.jobs else {
            return Ok(JobsListResult {
                jobs: Vec::new(),
                truncated: false,
            });
        };
        let limit = params
            .limit
            .map_or(MAX_OVERVIEW_ROWS, |limit| limit.clamp(1, MAX_OVERVIEW_ROWS))
            as usize;
        let mut rows: Vec<(Timestamp, JobRow)> = Vec::new();
        let mut cursor = None;
        let mut scanned = 0usize;
        loop {
            let page = store.list(&JobListQuery {
                states: None,
                holder: None,
                limit: PAGE,
                cursor: cursor.clone(),
            })?;
            scanned += page.jobs.len();
            for record in &page.jobs {
                if !visible(tenant, record) {
                    continue;
                }
                let row = row_of(record);
                if params.states.is_empty() || params.states.iter().any(|s| *s == row.state) {
                    rows.push((record.submitted_at, row));
                }
            }
            match page.next_cursor {
                Some(next) if scanned < MAX_SCAN && !page.jobs.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.work_id.cmp(&b.1.work_id)));
        let truncated = rows.len() > limit;
        rows.truncate(limit);
        Ok(JobsListResult {
            jobs: rows.into_iter().map(|(_, row)| row).collect(),
            truncated,
        })
    }

    /// `jobs.get`: spec summary, status history and output tail of one job.
    ///
    /// # Errors
    /// [`HostError::NotFound`] for an unknown or foreign job.
    pub fn jobs_get(
        &self,
        tenant: Option<&TenantId>,
        work_id: &str,
    ) -> Result<JobDetail, HostError> {
        let record = self.visible_job(tenant, work_id)?;
        Ok(JobDetail {
            job: row_of(&record),
            spec_summary: spec_summary(&record.input),
            history: history_of(&record),
            holder: record.lease.as_ref().map(|lease| lease.holder.clone()),
            output_tail: output_tail(&record),
        })
    }

    /// `jobs.cancel`: the store's cancel transition (the same one `/cancel`
    /// and `/stop` use). A foreign job answers like an unknown one.
    ///
    /// # Errors
    /// [`HostError::NotFound`] for an unknown or foreign job.
    pub fn jobs_cancel(
        &self,
        tenant: Option<&TenantId>,
        actor: &ApprovalActor,
        work_id: &str,
    ) -> Result<CommandRunResult, HostError> {
        let store = self.store()?;
        let id = self.visible_job(tenant, work_id)?.job.id;
        let outcome = store.cancel(
            &id,
            &CancelRequest {
                cancelled_at: Timestamp::now(),
                cancelled_by: actor.clone(),
                reason: "abgebrochen über die Übersicht".to_owned(),
            },
        );
        Ok(match outcome {
            Ok(transition) => applied(format!(
                "Job {} abgebrochen (war {:?}, Revision {}).",
                transition.work_id, transition.previous_state, transition.revision
            )),
            Err(error) => refused(&error),
        })
    }

    /// `jobs.retry`: the store's retry transition (the same one `/retry`
    /// uses); respects the retry policy.
    ///
    /// # Errors
    /// [`HostError::NotFound`] for an unknown or foreign job.
    pub fn jobs_retry(
        &self,
        tenant: Option<&TenantId>,
        actor: &ApprovalActor,
        work_id: &str,
    ) -> Result<CommandRunResult, HostError> {
        let store = self.store()?;
        let id = self.visible_job(tenant, work_id)?.job.id;
        let outcome = store.retry(
            &id,
            &RetryRequest {
                retried_at: Timestamp::now(),
                retried_by: actor.clone(),
            },
        );
        Ok(match outcome {
            Ok(event) => applied(format!(
                "Job {} erneut angefordert (jetzt {:?}, Revision {}).",
                event.work_id, event.state, event.revision
            )),
            Err(error) => refused(&error),
        })
    }

    fn store(&self) -> Result<&Arc<JobStore>, HostError> {
        self.jobs
            .as_ref()
            .ok_or_else(|| HostError::Denied("no job store on this host".into()))
    }

    fn visible_job(
        &self,
        tenant: Option<&TenantId>,
        work_id: &str,
    ) -> Result<StoredJob, HostError> {
        if work_id.is_empty() || work_id.len() > 200 {
            return Err(HostError::Protocol("invalid work_id".into()));
        }
        let store = self.store()?;
        match store.get(&WorkId::from_str(work_id)) {
            Ok(record) if visible(tenant, &record) => Ok(record),
            Ok(_) | Err(SessionStoreError::JobNotFound { .. }) => Err(HostError::NotFound),
            Err(error) => Err(error.into()),
        }
    }
}

fn visible(caller: Option<&TenantId>, record: &StoredJob) -> bool {
    crate::identity::tenant_admits(caller, Some(record.scope.tenant()))
}

fn applied(text: String) -> CommandRunResult {
    CommandRunResult {
        ok: true,
        text,
        data: None,
        effect: CommandEffect::Applied,
        reason_code: None,
        stepup: None,
    }
}

fn refused(error: &SessionStoreError) -> CommandRunResult {
    CommandRunResult {
        ok: false,
        text: error.to_string().chars().take(300).collect(),
        data: None,
        effect: CommandEffect::Refused,
        reason_code: Some(harw_protocol::session_wire::refusal_codes::OP_FAILED.to_owned()),
        stepup: None,
    }
}

fn millis(at: Timestamp) -> u64 {
    u64::try_from(at.as_millisecond()).unwrap_or(0)
}

/// Stable state code of a record.
fn state_code(record: &StoredJob) -> &'static str {
    match record.disposition() {
        JobDisposition::Succeeded => "succeeded",
        JobDisposition::Failed => "failed",
        JobDisposition::TimedOut => "timed_out",
        JobDisposition::Cancelled => "cancelled",
        JobDisposition::Blocked => "blocked",
        JobDisposition::Active => match record.job.state {
            JobState::Running => "running",
            _ => "queued",
        },
    }
}

fn kind_label(kind: &JobKind) -> String {
    match kind {
        JobKind::Dream => "dream".to_owned(),
        JobKind::Worker => "worker".to_owned(),
        JobKind::Custom(name) => name.clone(),
    }
}

fn first_line(text: &str, max: usize) -> String {
    text.lines().next().unwrap_or("").chars().take(max).collect()
}

fn text_of(value: &serde_json::Value, max: usize) -> Option<String> {
    let text = match value {
        serde_json::Value::String(text) => first_line(text, max),
        serde_json::Value::Null => return None,
        other => first_line(&other.to_string(), max),
    };
    (!text.trim().is_empty()).then_some(text)
}

fn reason_of(record: &StoredJob) -> Option<String> {
    record
        .completion
        .as_ref()
        .and_then(|completion| match &completion.outcome {
            JobOutcome::Failed { reason }
            | JobOutcome::Cancelled { reason }
            | JobOutcome::Blocked { reason } => Some(first_line(reason, 160)),
            JobOutcome::Succeeded { result } => text_of(result, 160),
        })
        .or_else(|| {
            record
                .cancellation
                .as_ref()
                .map(|cancellation| first_line(&cancellation.reason, 160))
        })
        .filter(|text| !text.is_empty())
}

/// One list row of a record.
fn row_of(record: &StoredJob) -> JobRow {
    let kind = kind_label(&record.job.kind);
    let title = TITLE_KEYS
        .iter()
        .find_map(|key| record.input.get(*key).and_then(|value| text_of(value, 100)))
        .unwrap_or_else(|| kind.clone());
    let session_id = SESSION_KEYS.iter().find_map(|key| {
        record
            .input
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
    let started = record.lease.as_ref().map(|lease| lease.acquired_at);
    let finished = record
        .completion
        .as_ref()
        .map(|completion| completion.completed_at)
        .or_else(|| {
            record
                .cancellation
                .as_ref()
                .map(|cancellation| cancellation.cancelled_at)
        });
    let started_ms = started.map(millis);
    let finished_ms = finished.map(millis);
    let duration_ms = started_ms.map(|start| {
        let end = finished_ms.unwrap_or_else(|| millis(Timestamp::now()));
        end.saturating_sub(start)
    });
    let state = record.job.state;
    JobRow {
        work_id: record.job.id.to_string(),
        kind,
        state: state_code(record).to_owned(),
        title,
        summary: reason_of(record),
        session_id,
        submitted_ms: millis(record.submitted_at),
        started_ms,
        finished_ms,
        duration_ms,
        attempts: record.job.attempts,
        can_cancel: matches!(
            state,
            JobState::Pending | JobState::Ready | JobState::Running
        ),
        can_retry: matches!(state, JobState::Failed | JobState::Cancelled)
            && record.job.attempts < record.job.retry.max_attempts,
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|hint| lower.contains(hint))
}

/// Bounded `key: value` lines of the job input; secret-looking keys left out.
fn spec_summary(input: &serde_json::Value) -> String {
    let Some(map) = input.as_object() else {
        return text_of(input, SPEC_VALUE_CHARS).unwrap_or_default();
    };
    let mut lines = Vec::new();
    for (key, value) in map {
        if is_secret_key(key) {
            continue;
        }
        let Some(text) = text_of(value, SPEC_VALUE_CHARS) else {
            continue;
        };
        lines.push(format!("{key}: {text}"));
        if lines.len() >= SPEC_MAX_LINES {
            break;
        }
    }
    lines.join("\n")
}

fn history_of(record: &StoredJob) -> Vec<JobHistoryEntry> {
    let mut history = vec![JobHistoryEntry {
        at_ms: millis(record.submitted_at),
        event: "submitted".to_owned(),
        detail: Some(format!("{} · Versuche {}", kind_label(&record.job.kind), record.job.attempts)),
    }];
    if let Some(lease) = &record.lease {
        history.push(JobHistoryEntry {
            at_ms: millis(lease.acquired_at),
            event: "claimed".to_owned(),
            detail: Some(lease.holder.clone()),
        });
        if lease.heartbeat_at != lease.acquired_at {
            history.push(JobHistoryEntry {
                at_ms: millis(lease.heartbeat_at),
                event: "heartbeat".to_owned(),
                detail: None,
            });
        }
    }
    if let Some(cancellation) = &record.cancellation {
        history.push(JobHistoryEntry {
            at_ms: millis(cancellation.cancelled_at),
            event: "cancelled".to_owned(),
            detail: Some(first_line(&cancellation.reason, 160)),
        });
    }
    if let Some(completion) = &record.completion {
        let (event, detail) = match &completion.outcome {
            JobOutcome::Succeeded { .. } => ("succeeded", None),
            JobOutcome::Failed { reason } => ("failed", Some(first_line(reason, 160))),
            JobOutcome::Cancelled { reason } => ("cancelled", Some(first_line(reason, 160))),
            JobOutcome::Blocked { reason } => ("blocked", Some(first_line(reason, 160))),
        };
        if !(event == "cancelled" && record.cancellation.is_some()) {
            history.push(JobHistoryEntry {
                at_ms: millis(completion.completed_at),
                event: event.to_owned(),
                detail,
            });
        }
    }
    history.sort_by_key(|entry| entry.at_ms);
    history
}

/// Tail of the stored result or reason.
fn output_tail(record: &StoredJob) -> Option<String> {
    let text = match &record.completion.as_ref()?.outcome {
        JobOutcome::Succeeded { result } => match result {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Null => return None,
            other => serde_json::to_string_pretty(other).ok()?,
        },
        JobOutcome::Failed { reason }
        | JobOutcome::Cancelled { reason }
        | JobOutcome::Blocked { reason } => reason.clone(),
    };
    let count = text.chars().count();
    if count <= OUTPUT_TAIL_CHARS {
        return Some(text);
    }
    let tail: String = text.chars().skip(count - OUTPUT_TAIL_CHARS).collect();
    Some(format!("…{tail}"))
}

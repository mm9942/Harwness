//! Durable status/result lookup by WorkId for model-facing async work.
//!
//! This surface deliberately exposes only lifecycle metadata and the bounded
//! terminal result. StoredJob::input, recovery envelopes, leases and worker
//! identities never cross this operation boundary.

use harw_job_runtime::{JobOutcome, JobState, StoredJob};
use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_session_store::JobStore;
use harw_types::WorkId;
use std::sync::Arc;

const DEFAULT_RESULT_CHARS: usize = 32 * 1024;
const MAX_RESULT_CHARS: usize = 128 * 1024;

#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
#[serde(deny_unknown_fields)]
pub struct WorkResultArgs {
    /// Durable WorkId returned by async delegation or another job-backed action.
    #[serde(default)]
    pub work_id: Option<String>,
    /// Maximum number of result characters returned to the model.
    #[serde(default)]
    pub max_chars: Option<usize>,
}

impl FromRawArgs for WorkResultArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Err(OpError::NotAvailable(
            "work.result has no command surface; use the model tool or /review as an operator"
                .to_owned(),
        ))
    }
}

#[operation(
    name = "work.result",
    summary = "Reads durable status and bounded terminal output by WorkId in the current workspace.",
    domain = "execution",
    permission = "observer",
    model_tool(readonly, approval = "none")
)]
async fn work_result(ctx: &OpContext, args: WorkResultArgs) -> Result<OpOutput, OpError> {
    let raw_id = args
        .work_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::InvalidArguments("work_id is required".to_owned()))?;
    let max_chars = args.max_chars.unwrap_or(DEFAULT_RESULT_CHARS);
    if !(1..=MAX_RESULT_CHARS).contains(&max_chars) {
        return Err(OpError::InvalidArguments(format!(
            "max_chars must be between 1 and {MAX_RESULT_CHARS}"
        )));
    }

    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let work_id = WorkId::from_str(raw_id);
    let record =
        crate::job_tenant::get_bound_workspace_job(ctx, store, &work_id).map_err(|error| {
            OpError::Execution(format!("could not read durable job '{raw_id}': {error}"))
        })?;

    render_result(&record, max_chars)
}

fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Pending => "pending",
        JobState::Ready => "ready",
        JobState::Running => "running",
        JobState::Completed => "completed",
        JobState::Blocked => "blocked",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn clip_chars(value: &str, limit: usize) -> (String, bool) {
    let mut chars = value.chars();
    let clipped: String = chars.by_ref().take(limit).collect();
    let truncated = chars.next().is_some();
    (clipped, truncated)
}

fn render_result(record: &StoredJob, max_chars: usize) -> Result<OpOutput, OpError> {
    let state = state_name(record.job.state);
    let mut lines = vec![
        format!("Work {}", record.job.id),
        format!("State: {state}"),
        format!("Revision: {}", record.revision),
    ];
    let mut data = serde_json::json!({
        "work_id": record.job.id.as_str(),
        "state": state,
        "revision": record.revision,
    });

    let Some(completion) = record.completion.as_ref() else {
        lines.push("Result: not terminal yet".to_owned());
        data["outcome"] = serde_json::json!({"kind": "active"});
        return Ok(OpOutput {
            text: lines.join("\n"),
            data: Some(data),
        });
    };

    data["completed_at"] = serde_json::Value::String(completion.completed_at.to_string());

    match &completion.outcome {
        JobOutcome::Succeeded { result } => {
            lines.push("Outcome: succeeded".to_owned());
            if let Some(text) = result.get("text").and_then(serde_json::Value::as_str) {
                let (text, truncated) = clip_chars(text, max_chars);
                lines.push(format!(
                    "Result{}:\n{}",
                    if truncated { " (truncated)" } else { "" },
                    text
                ));
                let child_id = result
                    .get("child_id")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
                let budget_exhausted = result
                    .get("budget_exhausted")
                    .and_then(serde_json::Value::as_bool);
                data["outcome"] = serde_json::json!({
                    "kind": "succeeded",
                    "text": text,
                    "truncated": truncated,
                    "child_id": child_id,
                    "budget_exhausted": budget_exhausted,
                });
            } else {
                let serialized = serde_json::to_string(result).map_err(|error| {
                    OpError::Execution(format!("could not serialize durable result: {error}"))
                })?;
                let (serialized, truncated) = clip_chars(&serialized, max_chars);
                lines.push(format!(
                    "Result{}:\n{}",
                    if truncated { " (truncated)" } else { "" },
                    serialized
                ));
                data["outcome"] = serde_json::json!({
                    "kind": "succeeded",
                    "result_json": serialized,
                    "truncated": truncated,
                });
            }
        }
        JobOutcome::Failed { reason } => {
            let (reason, truncated) = clip_chars(reason, max_chars);
            lines.push("Outcome: failed".to_owned());
            lines.push(format!(
                "Reason{}: {}",
                if truncated { " (truncated)" } else { "" },
                reason
            ));
            data["outcome"] = serde_json::json!({
                "kind": "failed",
                "reason": reason,
                "truncated": truncated,
            });
        }
        JobOutcome::Cancelled { reason } => {
            let (reason, truncated) = clip_chars(reason, max_chars);
            lines.push("Outcome: cancelled".to_owned());
            lines.push(format!(
                "Reason{}: {}",
                if truncated { " (truncated)" } else { "" },
                reason
            ));
            data["outcome"] = serde_json::json!({
                "kind": "cancelled",
                "reason": reason,
                "truncated": truncated,
            });
        }
        JobOutcome::Blocked { reason } => {
            let (reason, truncated) = clip_chars(reason, max_chars);
            lines.push("Outcome: blocked".to_owned());
            lines.push(format!(
                "Reason{}: {}",
                if truncated { " (truncated)" } else { "" },
                reason
            ));
            data["outcome"] = serde_json::json!({
                "kind": "blocked",
                "reason": reason,
                "truncated": truncated,
            });
        }
    }

    Ok(OpOutput {
        text: lines.join("\n"),
        data: Some(data),
    })
}

#[cfg(test)]
mod tests {
    use super::{WorkResultArgs, clip_chars, work_result};
    use crate::job_tenant::fixtures::{TENANT_A, TenantJobs, context, job};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_runtime::{JobCompletion, JobOutcome, JobScope, JobState};
    use harw_operations::OpError;
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, TenantId, WorkId, WorkspaceId};
    use jiff::Timestamp;
    use std::sync::Arc;

    fn result_jobs(id: &str, text: &str, workspace: &str) -> TestResult<TenantJobs> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("job-store")));
        let mut record = job(id, TENANT_A, JobState::Completed);
        record.scope = JobScope::new(
            TenantId::from_str(TENANT_A),
            WorkspaceId::from_str(workspace),
            ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            },
        );
        record.completion = Some(JobCompletion {
            completed_at: Timestamp::now(),
            outcome: JobOutcome::Succeeded {
                result: serde_json::json!({
                    "child_id": "child-42",
                    "text": text,
                    "budget_exhausted": false,
                }),
            },
        });
        store.admit(&record).map_err(ctx("admit result job"))?;
        Ok(TenantJobs { dir, store })
    }

    #[tokio::test]
    async fn successful_agent_result_is_read_by_work_id() -> TestResult {
        let jobs = result_jobs("agent-result", "finished safely", "ws")?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = work_result(
            &op_ctx,
            WorkResultArgs {
                work_id: Some("agent-result".to_owned()),
                max_chars: None,
            },
        )
        .await
        .map_err(ctx("work.result"))?;

        assert!(output.text.contains("State: completed"), "{}", output.text);
        assert!(output.text.contains("finished safely"), "{}", output.text);
        let data = output.data.ok_or(TestError::Missing("structured result"))?;
        assert_eq!(data["outcome"]["child_id"], "child-42");
        assert_eq!(data["outcome"]["text"], "finished safely");
        Ok(())
    }

    #[tokio::test]
    async fn active_job_has_no_synthetic_result() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("job-store")));
        store
            .admit(&job("agent-active", TENANT_A, JobState::Ready))
            .map_err(ctx("admit active job"))?;
        let jobs = TenantJobs { dir, store };
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = work_result(
            &op_ctx,
            WorkResultArgs {
                work_id: Some("agent-active".to_owned()),
                max_chars: None,
            },
        )
        .await
        .map_err(ctx("work.result active"))?;

        assert!(output.text.contains("Result: not terminal yet"));
        assert_eq!(
            output
                .data
                .as_ref()
                .and_then(|data| data["outcome"]["kind"].as_str()),
            Some("active")
        );
        Ok(())
    }

    #[tokio::test]
    async fn result_text_is_character_bounded() -> TestResult {
        let jobs = result_jobs("agent-long", "äöüabcdef", "ws")?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = work_result(
            &op_ctx,
            WorkResultArgs {
                work_id: Some("agent-long".to_owned()),
                max_chars: Some(4),
            },
        )
        .await
        .map_err(ctx("work.result bounded"))?;
        let data = output.data.ok_or(TestError::Missing("structured result"))?;
        assert_eq!(data["outcome"]["text"], "äöüa");
        assert_eq!(data["outcome"]["truncated"], true);
        Ok(())
    }

    #[tokio::test]
    async fn foreign_workspace_is_hidden_like_missing() -> TestResult {
        let jobs = result_jobs("other-workspace", "secret", "other-ws")?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let foreign = work_result(
            &op_ctx,
            WorkResultArgs {
                work_id: Some("other-workspace".to_owned()),
                max_chars: None,
            },
        )
        .await;
        let missing = work_result(
            &op_ctx,
            WorkResultArgs {
                work_id: Some("missing-work".to_owned()),
                max_chars: None,
            },
        )
        .await;
        match (foreign, missing) {
            (Err(OpError::Execution(foreign)), Err(OpError::Execution(missing))) => {
                let foreign = foreign.replace("other-workspace", "<id>");
                let missing = missing.replace("missing-work", "<id>");
                assert_eq!(foreign, missing);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected two hidden execution errors, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn clip_chars_counts_unicode_characters_not_bytes() {
        assert_eq!(clip_chars("äöüabc", 4), ("äöüa".to_owned(), true));
        assert_eq!(clip_chars("äöü", 3), ("äöü".to_owned(), false));
    }

    #[test]
    fn work_id_parser_accepts_durable_ids_used_by_the_store() {
        assert_eq!(
            WorkId::from_str("work-result-test").as_str(),
            "work-result-test"
        );
    }
}

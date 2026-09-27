//! `/retry` — fordert einen terminal `Failed`- oder `Cancelled`-Job erneut an.
//!
//! # Verantwortungsbereich
//! Implementiert die `retry`-Operation als Teil der Genehmigungs-Befehlsgruppe
//! (Interaktionsvertrag §2.3/§4), im selben Stil wie `crate::approve` und
//! `crate::deny`. Exponiert **nur** einen Command `/retry <WorkId>` mit
//! `channel_parity`-Sichtbarkeit — **kein** `ModelTool`, aus demselben Grund
//! wie bei `crate::approve`/`crate::deny` (siehe dessen Moduldoku): ein
//! laufendes Modell darf eine gescheiterte oder abgelehnte eigene Aktion nicht
//! selbst erneut in die Warteschlange stellen.
//!
//! # Zugrunde liegende Transition
//! Nutzt [`harw_session_store::JobStore::retry`] (`Failed`/`Cancelled` →
//! `Ready`, Versuchszähler erhöht, respektiert `RetryPolicy::max_attempts`).
//! Ist die Retry-Policy erschöpft, liefert der Store einen typisierten Fehler
//! ([`harw_session_store::SessionStoreError::JobRetryLimitExhausted`]) statt
//! den Job stillschweigend erneut einzureihen; diese Operation übersetzt ihn
//! unverändert in [`OpError::Execution`].

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{JobStore, RetryRequest};
use harw_types::{ApprovalActor, WorkId};
use std::sync::Arc;

/// Eingabe-Argumente für die `retry`-Operation.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct RetryArgs {
    /// Die erneut anzufordernde `WorkId`.
    #[serde(default)]
    #[raw(first)]
    pub work_id: Option<String>,
}

#[operation(
    name = "retry",
    summary = "Fordert einen Failed- oder Cancelled-Job (WorkId) erneut an — respektiert die Retry-Policy.",
    domain = "execution",
    permission = "operator",
    command(path = "/retry", visibility = "channel_parity")
)]
async fn retry(ctx: &OpContext, args: RetryArgs) -> Result<OpOutput, OpError> {
    let Some(work_id) = args.work_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "work_id ist erforderlich".to_owned(),
        ));
    };
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;

    let work_id_typed = WorkId::from_str(work_id);
    // H12: ein fremder Job wird wie ein unbekannter behandelt.
    let event = crate::job_tenant::ensure_job_visible(ctx, store, &work_id_typed)
        .and_then(|()| {
            store.retry(
                &work_id_typed,
                &RetryRequest {
                    retried_at: jiff::Timestamp::now(),
                    retried_by: ApprovalActor::Operator {
                        id: "local-command".to_owned(),
                    },
                },
            )
        })
        .map_err(|error| {
            OpError::Execution(format!("could not retry durable job `{work_id}`: {error}"))
        })?;

    Ok(OpOutput::from(format!(
        "Retried {} (now {:?}, revision {}).",
        event.work_id, event.state, event.revision
    )))
}

#[cfg(test)]
mod tests {
    use super::{RetryArgs, retry};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, SessionId, TenantId, TurnId, WorkId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    fn make_test_ctx(
        with_store: bool,
    ) -> TestResult<(OpContext, std::path::PathBuf, Option<Arc<JobStore>>)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-retry-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let store = with_store.then(|| Arc::new(JobStore::new(&root.join("jobs"))));
        let mut services = ServiceMap::new();
        if let Some(store) = &store {
            services.insert(Arc::clone(store));
        }
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
            store,
        ))
    }

    fn job_in_state(id: &str, state: JobState, attempts: u32) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 2,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            now,
        );
        job.state = state;
        job.attempts = attempts;
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "retry me"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        }
    }

    #[test]
    fn test_retry_args_from_raw_args_sets_work_id() -> TestResult {
        let args = RetryArgs::from_raw_args(&toks(&["work-42"]))
            .map_err(ctx("RetryArgs::from_raw_args"))?;
        assert_eq!(args.work_id.as_deref(), Some("work-42"));
        Ok(())
    }

    #[tokio::test]
    async fn retry_without_work_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = retry(&ctx, RetryArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("work_id")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn retry_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = retry(
            &ctx,
            RetryArgs {
                work_id: Some("work-missing-store".to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("job store")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn retry_requeues_a_failed_job_and_reports_ready_state() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-retry");
        store
            .admit(&job_in_state(work_id.as_str(), JobState::Failed, 1))
            .map_err(crate::test_support::ctx("admit failed job"))?;

        let output = retry(
            &ctx,
            RetryArgs {
                work_id: Some(work_id.as_str().to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("retry requeues the failed job"))?;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read requeued job"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert_eq!(persisted.job.state, JobState::Ready);
        assert_eq!(persisted.job.attempts, 2);
        assert!(output.text.contains(work_id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn retry_fails_for_a_job_that_is_not_failed_or_cancelled() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-not-retryable");
        store
            .admit(&job_in_state(work_id.as_str(), JobState::Ready, 0))
            .map_err(crate::test_support::ctx("admit ready job"))?;

        let result = retry(
            &ctx,
            RetryArgs {
                work_id: Some(work_id.as_str().to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert!(matches!(result, Err(OpError::Execution(_))));
        Ok(())
    }

    #[tokio::test]
    async fn retry_fails_once_the_retry_limit_is_exhausted_and_does_not_requeue() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-retry-exhausted");
        store
            .admit(&job_in_state(work_id.as_str(), JobState::Failed, 2))
            .map_err(crate::test_support::ctx("admit exhausted failed job"))?;

        let result = retry(
            &ctx,
            RetryArgs {
                work_id: Some(work_id.as_str().to_owned()),
            },
        )
        .await;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read job after failed retry"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert!(matches!(result, Err(OpError::Execution(_))));
        assert_eq!(
            persisted.job.state,
            JobState::Failed,
            "must not requeue silently"
        );
        assert_eq!(persisted.job.attempts, 2);
        Ok(())
    }

    // H12: Mandanten-Filter (unscoped sieht alles, scoped nur den eigenen
    // Mandanten, fremde Jobs verhalten sich wie unbekannte).
    #[tokio::test]
    async fn retry_unscoped_caller_reaches_every_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_B, context, state_of, two_tenants};
        let jobs = two_tenants(JobState::Failed)?;
        let op_ctx = context(&jobs, None)?;
        let output = retry(
            &op_ctx,
            RetryArgs {
                work_id: Some(JOB_B.to_owned()),
            },
        )
        .await
        .map_err(ctx("retry foreign job without scope"))?;
        assert!(output.text.contains(JOB_B), "{}", output.text);
        assert_eq!(state_of(&jobs, JOB_B)?, JobState::Ready);
        Ok(())
    }

    #[tokio::test]
    async fn retry_scoped_caller_reaches_own_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, TENANT_A, context, state_of, two_tenants};
        let jobs = two_tenants(JobState::Failed)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = retry(
            &op_ctx,
            RetryArgs {
                work_id: Some(JOB_A.to_owned()),
            },
        )
        .await
        .map_err(ctx("retry own job"))?;
        assert!(output.text.contains(JOB_A), "{}", output.text);
        assert_eq!(state_of(&jobs, JOB_A)?, JobState::Ready);
        Ok(())
    }

    #[tokio::test]
    async fn retry_scoped_caller_foreign_job_is_hidden_like_missing() -> TestResult {
        use crate::job_tenant::fixtures::{
            JOB_B, MISSING, TENANT_A, assert_hidden_like_missing, context, state_of, two_tenants,
        };
        let jobs = two_tenants(JobState::Failed)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let foreign = retry(
            &op_ctx,
            RetryArgs {
                work_id: Some(JOB_B.to_owned()),
            },
        )
        .await;
        let missing = retry(
            &op_ctx,
            RetryArgs {
                work_id: Some(MISSING.to_owned()),
            },
        )
        .await;
        assert_hidden_like_missing(foreign, missing)?;
        assert_eq!(
            state_of(&jobs, JOB_B)?,
            JobState::Failed,
            "foreign job untouched"
        );
        Ok(())
    }
}

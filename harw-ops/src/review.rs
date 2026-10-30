//! `/review` — vollständige Ansicht eines einzelnen Governance-Jobs.
//!
//! # Verantwortungsbereich
//! Implementiert die `review`-Operation aus dem Interaktionsvertrag §2.3/§4.2
//! (`/review <WorkId>` "opens the diff/plan"). Exponiert **nur** einen
//! Command `/review` mit `channel_parity`-Sichtbarkeit, schreibgeschützt wie
//! `crate::work`. **Kein** `ModelTool` — dieselbe bewusste Beschränkung wie
//! bei `crate::approve`/`crate::deny`: `/review` gehört zur selben
//! Genehmigungs-Befehlsgruppe (§4.2), und diese Gruppe bleibt geschlossen
//! command-only, statt Teilflächen unterschiedlich zu behandeln.
//!
//! # Umfang gegenüber dem Vertrag
//! Der Vertrag beschreibt `/review` als "opens the diff/plan" in einem
//! TUI-Arbeitsgraph-Panel. Ein solches Panel existiert in `harw-tui` (Stand
//! dieser Änderung) nicht — siehe `harw-ops`-Abschlussbericht dieses Knotens.
//! Diese Operation liefert stattdessen den vollständigen
//! [`harw_job_runtime::stored::StoredJob`]-Datensatz als Text: Zustand,
//! Budget/Verbrauch, aktive Lease, Abschluss- und Abbruchdaten. Das ist die
//! Grundlage, aus der ein künftiges Panel einen Diff/Plan rendern würde, aber
//! selbst noch keine Diff-/Plan-Darstellung.

use harw_job_runtime::StoredJob;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::JobStore;
use harw_session_store::error::SessionStoreError;
use harw_types::WorkId;
use std::sync::Arc;

/// Eingabe-Argumente für die `review`-Operation.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ReviewArgs {
    /// Die anzuzeigende `WorkId`.
    #[serde(default)]
    #[raw(first)]
    pub work_id: Option<String>,
}

#[operation(
    name = "review",
    summary = "Zeigt den vollständigen Zustand eines Governance-Jobs (WorkId) — Freigabekontext, Budget, Lease, Abschluss.",
    domain = "execution",
    permission = "observer",
    command(path = "/review", visibility = "channel_parity", busy = "immediate")
)]
async fn review(ctx: &OpContext, args: ReviewArgs) -> Result<OpOutput, OpError> {
    let Some(work_id) = args.work_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "work_id ist erforderlich".to_owned(),
        ));
    };
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;

    // H12: fremde Jobs melden sich wie unbekannte (keine Existenz-Preisgabe).
    let record = crate::job_tenant::get_visible_job(ctx, store, &WorkId::from_str(work_id))
        .map_err(|error| map_get_error(work_id, error))?;

    Ok(OpOutput::from(render_review(&record)))
}

/// Übersetzt einen `JobStore::get`-Fehler in einen `OpError`.
///
/// # Description
/// Ein unbekannter Job ist eine Eingabeaussage ([`OpError::InvalidArguments`]);
/// jeder andere Speicherfehler ist ein Laufzeitfehler
/// ([`OpError::Execution`]).
fn map_get_error(work_id: &str, error: SessionStoreError) -> OpError {
    match error {
        SessionStoreError::JobNotFound { .. } => {
            OpError::InvalidArguments(format!("kein Job mit WorkId '{work_id}' gefunden"))
        }
        other => OpError::Execution(format!("Job-Speicher-Fehler: {other}")),
    }
}

/// Rendert einen [`StoredJob`] als mehrzeiligen Textbericht.
fn render_review(record: &StoredJob) -> String {
    let job = &record.job;
    let mut lines = vec![
        format!("WorkId: {}", job.id),
        format!("Kind: {:?}", job.kind),
        format!("State: {:?}", job.state),
        format!("Attempts: {}", job.attempts),
        format!("Created: {}", job.created_at),
        format!("Updated: {}", job.updated_at),
        format!("Revision: {}", record.revision),
        format!("Submitted: {}", record.submitted_at),
        format!("NotBefore: {}", record.not_before),
        format!("LeaseEpoch: {}", record.lease_epoch),
    ];
    match &record.lease {
        Some(lease) => lines.push(format!("Lease: {lease:?}")),
        None => lines.push("Lease: none".to_owned()),
    }
    match &record.completion {
        Some(completion) => lines.push(format!(
            "Completion: {:?} at {}",
            completion.outcome, completion.completed_at
        )),
        None => lines.push("Completion: none".to_owned()),
    }
    match &record.cancellation {
        Some(cancellation) => lines.push(format!(
            "Cancellation: by {:?} at {} — {}",
            cancellation.cancelled_by, cancellation.cancelled_at, cancellation.reason
        )),
        None => lines.push("Cancellation: none".to_owned()),
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{ReviewArgs, review};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
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
            std::env::temp_dir().join(format!("harw-review-test-{}-{id}", std::process::id()));
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

    fn admitted_job(id: &str) -> TestResult<StoredJob> {
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
                jitter: 0.0,
            },
            now,
        );
        job.mark_ready(now)
            .map_err(ctx("new job can become ready"))?;
        Ok(StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "review me"}),
            submitted_at: now,
            not_before: now,
            lease: None,
            lease_epoch: 0,
            completion: None,
            cancellation: None,
            revision: 0,
            trace: None,
        })
    }

    #[test]
    fn test_review_args_from_raw_args_sets_work_id() -> TestResult {
        let args = ReviewArgs::from_raw_args(&toks(&["work-42"]))
            .map_err(ctx("ReviewArgs::from_raw_args"))?;
        assert_eq!(args.work_id.as_deref(), Some("work-42"));
        Ok(())
    }

    #[tokio::test]
    async fn review_without_work_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = review(&ctx, ReviewArgs::default()).await;
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
    async fn review_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = review(
            &ctx,
            ReviewArgs {
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
    async fn review_of_unknown_work_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let _store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let result = review(
            &ctx,
            ReviewArgs {
                work_id: Some("work-ghost".to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("work-ghost")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn review_renders_state_and_revision_for_an_admitted_job() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-review");
        store
            .admit(&admitted_job(work_id.as_str())?)
            .map_err(crate::test_support::ctx("admit job"))?;

        let output = review(
            &ctx,
            ReviewArgs {
                work_id: Some(work_id.as_str().to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("review reads the admitted job"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert!(output.text.contains(work_id.as_str()));
        assert!(output.text.contains("Ready"));
        assert!(output.text.contains("Revision: 0"));
        assert!(output.text.contains("Lease: none"));
        assert!(output.text.contains("Completion: none"));
        assert!(output.text.contains("Cancellation: none"));
        Ok(())
    }

    // H12: Mandanten-Filter (unscoped sieht alles, scoped nur den eigenen
    // Mandanten, fremde Jobs verhalten sich wie unbekannte).
    #[tokio::test]
    async fn review_unscoped_caller_reaches_every_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_B, context, two_tenants};
        use harw_job_runtime::JobState;
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, None)?;
        let output = review(
            &op_ctx,
            ReviewArgs {
                work_id: Some(JOB_B.to_owned()),
            },
        )
        .await
        .map_err(ctx("review foreign job without scope"))?;
        assert!(output.text.contains(JOB_B), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn review_scoped_caller_reaches_own_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, TENANT_A, context, two_tenants};
        use harw_job_runtime::JobState;
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = review(
            &op_ctx,
            ReviewArgs {
                work_id: Some(JOB_A.to_owned()),
            },
        )
        .await
        .map_err(ctx("review own job"))?;
        assert!(output.text.contains(JOB_A), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn review_scoped_caller_foreign_job_is_hidden_like_missing() -> TestResult {
        use crate::job_tenant::fixtures::{
            JOB_B, MISSING, TENANT_A, assert_hidden_like_missing, context, two_tenants,
        };
        use harw_job_runtime::JobState;
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let foreign = review(
            &op_ctx,
            ReviewArgs {
                work_id: Some(JOB_B.to_owned()),
            },
        )
        .await;
        let missing = review(
            &op_ctx,
            ReviewArgs {
                work_id: Some(MISSING.to_owned()),
            },
        )
        .await;
        assert_hidden_like_missing(foreign, missing)?;
        Ok(())
    }
}

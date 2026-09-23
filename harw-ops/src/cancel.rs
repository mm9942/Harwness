//! `/cancel` — bricht einen Job kontrolliert ab (Genehmigungs-Befehlsgruppe).
//!
//! # Verantwortungsbereich
//! Implementiert die `cancel`-Operation aus dem Interaktionsvertrag §2.3/§4.
//! Exponiert einen Command `/cancel <WorkId> [reason]` mit
//! `channel_parity`-Sichtbarkeit **und** ein `ModelTool` mit
//! `approval = "always"` — genau wie `crate::stop`, dessen
//! [`harw_session_store::JobStore::cancel`]-Transition diese Operation
//! wiederverwendet.
//!
//! # Verhältnis zu `/stop`
//! `/stop` (Ist-Stand, siehe `docs/design/interaction-contract.md` §2.6) und
//! dieses `/cancel` (Vertrags-Sollstand §2.3, Genehmigungs-Befehlsgruppe) sind
//! heute **dieselbe darunterliegende Transition** —
//! [`harw_session_store::JobStore`] kennt genau eine Abbruch-Fläche
//! (`cancel`, Pending/Ready/Running → Cancelled), keine zweite,
//! genehmigungs-spezifische. Diese Operation existiert trotzdem als
//! eigenständiger Befehl, weil der Vertrag `/cancel` als Teil der
//! Genehmigungs-Befehlsgruppe (zusammen mit `/approve`/`/deny`/`/review`)
//! auflistet und ein Bediener sie dort erwartet, unabhängig davon, dass
//! `/stop` bereits denselben Effekt erreicht. Ein künftiger Knoten kann
//! `/stop` zu einem Alias dieser Operation machen (oder umgekehrt), sobald
//! eine der beiden Befehlsgruppen aufgelöst wird — außerhalb des
//! Bearbeitungsumfangs dieser Änderung.
//!
//! # Freitext statt `--reason`-Flag
//! Siehe `crate::approve`-Moduldoku: `#[derive(FromRawArgs)]` kennt keine
//! `--flag=value`-Syntax. `reason` ist der Rest der Zeile nach der `WorkId`
//! (`#[raw(join_from = 1)]`); fehlt er, greift derselbe feste Text wie bei
//! `/stop` ("cancelled through …"), angepasst auf diesen Befehlsnamen.

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{CancelRequest, JobStore};
use harw_types::{ApprovalActor, WorkId};
use std::sync::Arc;

/// Begründung, wenn `/cancel` ohne expliziten Freitext aufgerufen wird.
const DEFAULT_REASON: &str = "cancelled through /cancel";

/// Eingabe-Argumente für die `cancel`-Operation.
#[derive(Debug, Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct CancelArgs {
    /// Die abzubrechende `WorkId`.
    #[serde(default)]
    #[raw(first)]
    pub work_id: Option<String>,
    /// Optionale Begründung (siehe Moduldoku, „Freitext statt
    /// `--reason`-Flag"); Vorgabe [`DEFAULT_REASON`], wenn nicht angegeben.
    #[serde(default)]
    #[raw(join_from = 1)]
    pub reason: Option<String>,
}

#[operation(
    name = "cancel",
    summary = "Bricht einen laufenden Job (WorkId) kontrolliert ab — Genehmigungs-Befehlsgruppe (siehe Moduldoku, Verhältnis zu /stop).",
    domain = "execution",
    permission = "operator",
    command(path = "/cancel", visibility = "channel_parity", busy = "immediate"),
    model_tool(approval = "always")
)]
async fn cancel(ctx: &OpContext, args: CancelArgs) -> Result<OpOutput, OpError> {
    let Some(work_id) = args.work_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "work_id ist erforderlich".to_owned(),
        ));
    };
    let reason = args
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(DEFAULT_REASON);
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;

    let transition = store
        .cancel(
            &WorkId::from_str(work_id),
            &CancelRequest {
                cancelled_at: jiff::Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "local-command".to_owned(),
                },
                reason: reason.to_owned(),
            },
        )
        .map_err(|error| {
            OpError::Execution(format!("could not cancel durable job `{work_id}`: {error}"))
        })?;

    Ok(OpOutput::from(format!(
        "Cancelled {} (was {:?}, revision {}).",
        transition.work_id, transition.previous_state, transition.revision
    )))
}

#[cfg(test)]
mod tests {
    use super::{CancelArgs, DEFAULT_REASON, cancel};
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
            std::env::temp_dir().join(format!("harw-cancel-test-{}-{id}", std::process::id()));
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
            input: serde_json::json!({"task": "cancel me"}),
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
    fn test_cancel_args_from_raw_args_splits_work_id_and_reason() -> TestResult {
        let args = CancelArgs::from_raw_args(&toks(&["work-1", "no", "longer", "needed"]))
            .map_err(ctx("CancelArgs::from_raw_args"))?;
        assert_eq!(args.work_id.as_deref(), Some("work-1"));
        assert_eq!(args.reason.as_deref(), Some("no longer needed"));
        Ok(())
    }

    #[tokio::test]
    async fn cancel_without_work_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = cancel(&ctx, CancelArgs::default()).await;
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
    async fn cancel_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = cancel(
            &ctx,
            CancelArgs {
                work_id: Some("work-missing-store".to_owned()),
                reason: None,
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
    async fn cancel_cancels_admitted_job_with_a_default_reason_when_none_given() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-cancel");
        store
            .admit(&admitted_job(work_id.as_str())?)
            .map_err(crate::test_support::ctx("admit job"))?;

        let output = cancel(
            &ctx,
            CancelArgs {
                work_id: Some(work_id.as_str().to_owned()),
                reason: None,
            },
        )
        .await
        .map_err(crate::test_support::ctx("cancel admitted job"))?;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read cancelled job"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert_eq!(
            persisted
                .cancellation
                .as_ref()
                .ok_or(TestError::Missing("cancellation recorded"))?
                .reason,
            DEFAULT_REASON
        );
        assert!(output.text.contains(work_id.as_str()));
        assert!(output.text.contains(&persisted.revision.to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn cancel_uses_the_given_reason_when_provided() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-cancel-reason");
        store
            .admit(&admitted_job(work_id.as_str())?)
            .map_err(crate::test_support::ctx("admit job"))?;

        cancel(
            &ctx,
            CancelArgs {
                work_id: Some(work_id.as_str().to_owned()),
                reason: Some("superseded by newer job".to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("cancel admitted job"))?;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read cancelled job"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert_eq!(
            persisted
                .cancellation
                .as_ref()
                .ok_or(TestError::Missing("cancellation recorded"))?
                .reason,
            "superseded by newer job"
        );
        Ok(())
    }
}

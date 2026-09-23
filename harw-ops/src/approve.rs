//! `/approve` — genehmigt einen wegen einer Freigabe blockierten Job.
//!
//! # Verantwortungsbereich
//! Implementiert die `approve`-Operation aus dem Interaktionsvertrag §2.3/§4.
//! Exponiert **nur** einen Command `/approve <WorkId> [note]` mit
//! `channel_parity`-Sichtbarkeit — **kein** `ModelTool`. Das laufende Modell
//! darf eine Genehmigungsentscheidung nicht selbst treffen, aus demselben
//! Grund, aus dem `/mode` kein `ModelTool` bekommt (siehe dessen Moduldoku):
//! ein Modell, das seine eigene gesperrte Aktion freigeben könnte, hätte eine
//! Bitte statt einer Grenze vor sich.
//!
//! # Welcher Zustand hier tatsächlich erreichbar ist
//! Der Interaktionsvertrag (§4.1) beschreibt eine `ApprovalRequest`, die einer
//! `WorkId` zugeordnet ist. In dieser Laufzeit gibt es dafür genau eine
//! erreichbare, WorkId-adressierte Transition:
//! [`harw_session_store::JobStore::unblock`] — sie führt einen
//! [`harw_job_runtime::JobState::Blocked`]-Job (laut dessen Moduldoku: "z. B.
//! pausiert für eine Freigabe") zurück nach `Ready`. Diese Operation ist
//! deshalb ein dünner Wrapper über genau diese Transition, im selben Stil wie
//! `crate::stop` über `JobStore::cancel`.
//!
//! # `note` wird dauerhaft gespeichert
//! `JobStore::unblock` nimmt seit dieser Änderung Akteur und optionalen
//! Freitext entgegen und persistiert beide als
//! [`harw_session_store::JobApproval`]-Sidecar unter derselben `WorkId` (das
//! `StoredJob`-Schema selbst liegt in `harw-job-runtime`, außerhalb dieser
//! Crate, und wird deshalb nicht erweitert — siehe `JobStore::unblock`s
//! Moduldoku für die Begründung). Der Akteur ist aktuell fest
//! `ApprovalActor::Operator { id: "local-command" }`, wie bei
//! `crate::deny`/`crate::cancel`, weil `OpContext` keine authentifizierte
//! Bediener-Identität transportiert.
//!
//! # Freitext statt `--note`-Flag
//! Das `#[derive(FromRawArgs)]`-Makro (`harw-macros`) kennt keine
//! `--flag=value`-Syntax — nur `first`/`nth`/`join`/`join_from`/`required`
//! auf dem Token-Slice. `note` ist deshalb der gesamte Rest der Zeile nach der
//! `WorkId` (`#[raw(join_from = 1)]`), keine literale `--note`-Flagge.

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::JobStore;
use harw_types::{ApprovalActor, WorkId};
use std::sync::Arc;

/// Eingabe-Argumente für die `approve`-Operation.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct ApproveArgs {
    /// Die zu genehmigende `WorkId`.
    #[serde(default)]
    #[raw(first)]
    pub work_id: Option<String>,
    /// Optionaler Freitext (siehe Moduldoku, „Freitext statt `--note`-Flag").
    #[serde(default)]
    #[raw(join_from = 1)]
    pub note: Option<String>,
}

#[operation(
    name = "approve",
    summary = "Genehmigt einen wegen einer Freigabe blockierten Job (WorkId) — führt ihn zurück nach Ready.",
    domain = "execution",
    permission = "operator",
    command(path = "/approve", visibility = "channel_parity", busy = "immediate")
)]
async fn approve(ctx: &OpContext, args: ApproveArgs) -> Result<OpOutput, OpError> {
    let Some(work_id) = args.work_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "work_id ist erforderlich".to_owned(),
        ));
    };
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;

    let note = args
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    let event = store
        .unblock(
            &WorkId::from_str(work_id),
            jiff::Timestamp::now(),
            ApprovalActor::Operator {
                id: "local-command".to_owned(),
            },
            note.clone(),
        )
        .map_err(|error| {
            OpError::Execution(format!(
                "could not approve (unblock) job `{work_id}`: {error}"
            ))
        })?;

    let mut text = format!(
        "Approved {} (now {:?}, revision {}).",
        event.work_id, event.state, event.revision
    );
    if let Some(note) = note {
        text.push_str(&format!("\nNote: {note}"));
    }
    Ok(OpOutput::from(text))
}

#[cfg(test)]
mod tests {
    use super::{ApproveArgs, approve};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::JobScope;
    use harw_job_runtime::{Budget, Job, JobKind, JobState, RetryPolicy, StoredJob};
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
            std::env::temp_dir().join(format!("harw-approve-test-{}-{id}", std::process::id()));
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

    fn blocked_job(id: &str) -> StoredJob {
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
        job.state = JobState::Blocked;
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "approve me"}),
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
    fn test_approve_args_from_raw_args_splits_work_id_and_note() -> TestResult {
        let args = ApproveArgs::from_raw_args(&toks(&["work-1", "looks", "fine"]))
            .map_err(ctx("ApproveArgs::from_raw_args"))?;
        assert_eq!(args.work_id.as_deref(), Some("work-1"));
        assert_eq!(args.note.as_deref(), Some("looks fine"));
        Ok(())
    }

    #[tokio::test]
    async fn approve_without_work_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = approve(&ctx, ApproveArgs::default()).await;
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
    async fn approve_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = approve(
            &ctx,
            ApproveArgs {
                work_id: Some("work-missing-store".to_owned()),
                note: None,
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
    async fn approve_unblocks_a_blocked_job_and_reports_ready_state() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-approve");
        store
            .admit(&blocked_job(work_id.as_str()))
            .map_err(crate::test_support::ctx("admit blocked job"))?;

        let output = approve(
            &ctx,
            ApproveArgs {
                work_id: Some(work_id.as_str().to_owned()),
                note: Some("reviewed, fine".to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("approve unblocks the job"))?;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read unblocked job"))?;
        let approval = store
            .get_approval(&work_id)
            .map_err(crate::test_support::ctx("read approval sidecar"))?
            .ok_or(TestError::Missing("approval was recorded"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert_eq!(persisted.job.state, JobState::Ready);
        assert!(output.text.contains(work_id.as_str()));
        assert!(output.text.contains("reviewed, fine"));
        assert_eq!(approval.note.as_deref(), Some("reviewed, fine"));
        assert_eq!(approval.revision, persisted.revision);
        assert!(matches!(
            approval.approved_by,
            ApprovalActor::Operator { ref id } if id == "local-command"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn approve_fails_for_a_job_that_is_not_blocked() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-not-blocked");
        let mut job = blocked_job(work_id.as_str());
        job.job.state = JobState::Ready;
        store
            .admit(&job)
            .map_err(crate::test_support::ctx("admit ready job"))?;

        let result = approve(
            &ctx,
            ApproveArgs {
                work_id: Some(work_id.as_str().to_owned()),
                note: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert!(matches!(result, Err(OpError::Execution(_))));
        Ok(())
    }
}

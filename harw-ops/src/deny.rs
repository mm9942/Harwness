//! `/deny` — lehnt einen Job endgültig ab (Ablehnung einer Freigabeanfrage).
//!
//! # Verantwortungsbereich
//! Implementiert die `deny`-Operation aus dem Interaktionsvertrag §2.3/§4.
//! Exponiert **nur** einen Command `/deny <WorkId> reason` mit
//! `channel_parity`-Sichtbarkeit — **kein** `ModelTool`, aus demselben Grund
//! wie bei `crate::approve` (siehe dessen Moduldoku): das laufende Modell
//! darf eine gegen es selbst gerichtete Ablehnung nicht selbst treffen.
//!
//! # Welcher Zustand hier tatsächlich erreichbar ist
//! [`harw_session_store::JobStore`] kennt zwei Abbruch-Flächen:
//! [`harw_session_store::JobStore::cancel`] (Pending/Ready/Running →
//! Cancelled) und, seit dieser Änderung,
//! [`harw_session_store::JobStore::deny_blocked`] (Blocked → Cancelled — die
//! Transition, die `JobStore::cancel` bewusst ausschließt, siehe dessen
//! Moduldoku). Diese Operation liest den aktuellen Zustand über
//! [`harw_session_store::JobStore::get`] und wählt danach die passende
//! Transition: **`Blocked` → `deny_blocked`**, jeder andere abbrechbare
//! Zustand → `cancel`. Beide Pfade liefern dieselbe
//! [`harw_session_store::CancellationTransition`]-Form (Akteur + Begründung
//! landen in `StoredJob::cancellation`), sodass der abschließende Report
//! identisch bleibt, unabhängig vom Ausgangszustand.
//!
//! Ein Job, der weder `Blocked` noch über `cancel` abbrechbar ist (z. B.
//! bereits terminal), liefert weiterhin ehrlich einen Fehler
//! ([`OpError::Execution`]) statt eine Ablehnung vorzutäuschen, die die
//! Laufzeit nicht durchführen kann.
//!
//! # Freitext statt `--reason`-Flag
//! Siehe `crate::approve`-Moduldoku: `#[derive(FromRawArgs)]` kennt keine
//! `--flag=value`-Syntax. `reason` ist deshalb der gesamte Rest der Zeile
//! nach der `WorkId` (`#[raw(join_from = 1)]`) und — anders als `note` bei
//! `/approve` — **verpflichtend**: eine leere Begründung wird abgelehnt,
//! bevor der Store überhaupt aufgerufen wird (derselbe Fehlertext wie bei
//! `JobStore::cancel` selbst, siehe `SessionStoreError::InvalidJobCancellationReason`).

use harw_job_runtime::JobState;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{CancelRequest, JobStore};
use harw_types::{ApprovalActor, WorkId};
use std::sync::Arc;

/// Eingabe-Argumente für die `deny`-Operation.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct DenyArgs {
    /// Die abzulehnende `WorkId`.
    #[serde(default)]
    #[raw(first)]
    pub work_id: Option<String>,
    /// Verpflichtende Begründung (siehe Moduldoku, „Freitext statt
    /// `--reason`-Flag").
    #[serde(default)]
    #[raw(join_from = 1)]
    pub reason: Option<String>,
}

#[operation(
    name = "deny",
    summary = "Lehnt einen Job (WorkId) endgültig ab — bricht ihn ab, sofern er nicht bereits Blocked ist (siehe Moduldoku).",
    domain = "execution",
    permission = "operator",
    command(path = "/deny", visibility = "channel_parity", busy = "immediate")
)]
async fn deny(ctx: &OpContext, args: DenyArgs) -> Result<OpOutput, OpError> {
    let Some(work_id) = args.work_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "work_id ist erforderlich".to_owned(),
        ));
    };
    let Some(reason) = args
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    else {
        return Err(OpError::InvalidArguments(
            "reason ist erforderlich — /deny <WorkId> <Begründung>".to_owned(),
        ));
    };
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let work_id_typed = WorkId::from_str(work_id);

    // A Blocked job needs the dedicated transition (`JobStore::cancel`
    // explicitly excludes it, see its moduldoc); every other state keeps
    // using `cancel` as before. `get` failing (e.g. unknown WorkId) falls
    // through to `cancel`, which reports the same underlying error.
    let is_blocked = matches!(
        store.get(&work_id_typed).map(|record| record.job.state),
        Ok(JobState::Blocked)
    );

    let request = CancelRequest {
        cancelled_at: jiff::Timestamp::now(),
        cancelled_by: ApprovalActor::Operator {
            id: "local-command".to_owned(),
        },
        reason: reason.to_owned(),
    };
    let transition = if is_blocked {
        store.deny_blocked(&work_id_typed, &request)
    } else {
        store.cancel(&work_id_typed, &request)
    }
    .map_err(|error| {
        OpError::Execution(format!("could not deny durable job `{work_id}`: {error}"))
    })?;

    Ok(OpOutput::from(format!(
        "Denied {} (was {:?}, revision {}).",
        transition.work_id, transition.previous_state, transition.revision
    )))
}

#[cfg(test)]
mod tests {
    use super::{DenyArgs, deny};
    use crate::testutil::toks;
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, SessionId, TenantId, TurnId, WorkId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    fn make_test_ctx(with_store: bool) -> (OpContext, std::path::PathBuf, Option<Arc<JobStore>>) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-deny-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let store = with_store.then(|| Arc::new(JobStore::new(&root.join("jobs"))));
        let mut services = ServiceMap::new();
        if let Some(store) = &store {
            services.insert(Arc::clone(store));
        }
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
            store,
        )
    }

    fn job_in_state(id: &str, state: JobState) -> StoredJob {
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
        if state == JobState::Ready {
            job.mark_ready(now).expect("pending job can become ready");
        } else {
            job.state = state;
        }
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "deny me"}),
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
    fn test_deny_args_from_raw_args_splits_work_id_and_reason() {
        let args = DenyArgs::from_raw_args(&toks(&["work-1", "too", "risky"])).unwrap();
        assert_eq!(args.work_id.as_deref(), Some("work-1"));
        assert_eq!(args.reason.as_deref(), Some("too risky"));
    }

    #[tokio::test]
    async fn deny_without_work_id_returns_invalid_arguments() {
        let (ctx, root, _) = make_test_ctx(false);
        let result = deny(&ctx, DenyArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("work_id")),
            other => panic!("expected InvalidArguments, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deny_without_reason_returns_invalid_arguments() {
        let (ctx, root, _) = make_test_ctx(false);
        let result = deny(
            &ctx,
            DenyArgs {
                work_id: Some("work-1".to_owned()),
                reason: None,
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("reason")),
            other => panic!("expected InvalidArguments, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deny_without_job_store_returns_not_available() {
        let (ctx, root, _) = make_test_ctx(false);
        let result = deny(
            &ctx,
            DenyArgs {
                work_id: Some("work-missing-store".to_owned()),
                reason: Some("no store".to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        match result {
            Err(OpError::NotAvailable(message)) => assert!(message.contains("job store")),
            other => panic!("expected NotAvailable, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn deny_cancels_a_ready_job_and_reports_previous_state() {
        let (ctx, root, store) = make_test_ctx(true);
        let store = store.expect("test context includes job store");
        let work_id = WorkId::from_str("work-deny");
        store
            .admit(&job_in_state(work_id.as_str(), JobState::Ready))
            .expect("admit ready job");

        let output = deny(
            &ctx,
            DenyArgs {
                work_id: Some(work_id.as_str().to_owned()),
                reason: Some("too risky".to_owned()),
            },
        )
        .await
        .expect("deny cancels the ready job");
        let persisted = store.get(&work_id).expect("read denied job");
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(output.text.contains(work_id.as_str()));
    }

    /// `/deny` gegen den zentralen §4-Fall: ein bereits `Blocked`er Job (die
    /// eigentliche "wartet auf Freigabe"-Situation) muss über die dedizierte
    /// `JobStore::deny_blocked`-Transition ablehnbar sein.
    #[tokio::test]
    async fn deny_cancels_a_blocked_job_via_the_dedicated_transition() {
        let (ctx, root, store) = make_test_ctx(true);
        let store = store.expect("test context includes job store");
        let work_id = WorkId::from_str("work-blocked");
        store
            .admit(&job_in_state(work_id.as_str(), JobState::Blocked))
            .expect("admit blocked job");

        let output = deny(
            &ctx,
            DenyArgs {
                work_id: Some(work_id.as_str().to_owned()),
                reason: Some("cannot proceed".to_owned()),
            },
        )
        .await
        .expect("deny cancels the blocked job via deny_blocked");
        let persisted = store.get(&work_id).expect("read denied job");
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(matches!(
            persisted.cancellation,
            Some(harw_job_runtime::JobCancellation { ref reason, .. })
                if reason == "cannot proceed"
        ));
        assert!(output.text.contains(work_id.as_str()));
        assert!(output.text.contains("Blocked"));
    }
}

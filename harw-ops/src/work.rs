//! `/work` — durable Job-Übersicht für das `harw-ops`-Crate.
//!
//! Dieses Modul implementiert den `/work`-Befehl als lokale, schreibgeschützte
//! Übersicht der im [`harw_session_store::JobStore`] persistierten Jobs. Es
//! fasst deren Gesamtzahl und Zustände zusammen. Approval-Queues und
//! Diff-Vorschauen gehören nicht zu dieser lokalen Store-Abfrage und werden
//! daher ausdrücklich nicht als verfügbar dargestellt.
//!
//! # Verantwortlichkeit
//! - Registriert `/work` als Channel-Parity-Command.
//! - Domain: `execution`, Permission: `observer`.
//! - Liest den kompositionsseitig registrierten `Arc<JobStore>` aus dem
//!   `ServiceMap`-Boundary.
//!
//! # Concurrency
//! Die Operation ist schreibgeschützt. Snapshot- und Sperrsemantik liegen beim
//! `JobStore`.
//!
//! # Fehlertypen
//! - [`OpError::NotAvailable`], wenn kein dauerhafter Job-Store registriert ist.
//! - [`OpError::Execution`], wenn die Store-Abfrage fehlschlägt.

use harw_job_runtime::JobState;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{JobListQuery, JobStore};
use std::sync::Arc;

/// Argumentstruktur für den `/work`-Befehl.
///
/// Die lokale Übersicht hat derzeit keine Filter- oder Paginierungsparameter.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct WorkArgs {}

/// Zeigt eine dauerhafte Job-Übersicht für den lokalen Ausführungskontext.
///
/// Alle Seiten des [`JobStore`] werden gelesen, damit Gesamt- und
/// Zustandszählungen nicht durch die Seitengröße begrenzt sind. Die Ausgabe
/// führt Zustände in fester Lifecycle-Reihenfolge auf, auch wenn ihr Zähler
/// null ist.
///
/// Approval- und Diff-Vorschau-Daten werden nicht aus Job-Datensätzen
/// abgeleitet: Sie sind in diesem lokalen Command-Kontext nicht verfügbar.
#[operation(
    name = "work",
    summary = "Zeigt dauerhafte Job-Zusammenfassung; Approval- und Diff-Daten lokal nicht verfügbar.",
    domain = "execution",
    permission = "observer",
    command(path = "/work", visibility = "channel_parity"),
    // Web-Fläche: laut Moduldoku "schreibgeschützte Übersicht" — liest nur
    // den `JobStore`, keine Mutation. `readonly`, `approval = "none"`.
    web(path = "/api/work", readonly, approval = "none")
)]
async fn work(ctx: &OpContext, _args: WorkArgs) -> Result<OpOutput, OpError> {
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let counts = list_state_counts(store.as_ref())?;

    Ok(OpOutput {
        text: render_work_panel(&counts),
    })
}

fn list_state_counts(store: &JobStore) -> Result<[usize; 7], OpError> {
    let mut counts = [0; 7];
    let mut cursor = None;

    loop {
        let page = store
            .list(&JobListQuery {
                limit: 100,
                cursor,
                ..JobListQuery::default()
            })
            .map_err(|error| OpError::Execution(format!("could not list durable jobs: {error}")))?;

        for record in page.jobs {
            counts[state_index(record.job.state)] += 1;
        }

        let Some(next_cursor) = page.next_cursor else {
            return Ok(counts);
        };
        cursor = Some(next_cursor);
    }
}

fn state_index(state: JobState) -> usize {
    match state {
        JobState::Pending => 0,
        JobState::Ready => 1,
        JobState::Running => 2,
        JobState::Completed => 3,
        JobState::Blocked => 4,
        JobState::Failed => 5,
        JobState::Cancelled => 6,
    }
}

fn render_work_panel(counts: &[usize; 7]) -> String {
    let total: usize = counts.iter().sum();
    format!(
        "Durable jobs: {total}\n\
         Pending: {}\n\
         Ready: {}\n\
         Running: {}\n\
         Completed: {}\n\
         Blocked: {}\n\
         Failed: {}\n\
         Cancelled: {}\n\
         Approval and diff-preview data are unavailable in this local command context.",
        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5], counts[6],
    )
}

#[cfg(test)]
mod tests {
    use super::{WorkArgs, work};
    use crate::testutil::toks;
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, SessionId, TenantId, TurnId, WorkId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_root(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
    }

    fn test_context(store: Option<Arc<JobStore>>) -> (OpContext, PathBuf) {
        let root = unique_root("harw-work-test");
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(store) = store {
            services.insert(store);
        }
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        )
    }

    fn admitted_job(id: &str, state: JobState) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 3,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(30),
            },
            now,
        );
        if state == JobState::Ready {
            job.mark_ready(now).expect("pending job can become ready");
        }
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "test"}),
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
    fn test_work_args_from_raw_args_empty_tokens_returns_ok() {
        let result = WorkArgs::from_raw_args(&toks(&[]));
        assert!(result.is_ok());
    }

    #[test]
    fn test_work_args_from_raw_args_ignores_extra_tokens() {
        let result = WorkArgs::from_raw_args(&toks(&["ignored"]));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn work_without_job_store_returns_not_available() {
        let (ctx, root) = test_context(None);
        let result = work(&ctx, WorkArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(matches!(result, Err(OpError::NotAvailable(_))));
    }

    #[tokio::test]
    async fn work_with_empty_store_reports_zero_counts() {
        let store_root = unique_root("harw-work-empty-store");
        let store = Arc::new(JobStore::new(&store_root));
        let (ctx, workspace_root) = test_context(Some(store));
        let output = work(&ctx, WorkArgs::default())
            .await
            .expect("render empty work panel");
        let _ = std::fs::remove_dir_all(store_root);
        std::fs::remove_dir_all(workspace_root).expect("remove test workspace");

        assert_eq!(
            output.text,
            "Durable jobs: 0\nPending: 0\nReady: 0\nRunning: 0\nCompleted: 0\nBlocked: 0\nFailed: 0\nCancelled: 0\nApproval and diff-preview data are unavailable in this local command context."
        );
    }

    #[tokio::test]
    async fn work_with_admitted_jobs_reports_total_and_state_counts() {
        let store_root = unique_root("harw-work-admitted-store");
        let store = Arc::new(JobStore::new(&store_root));
        store
            .admit(&admitted_job("job-pending", JobState::Pending))
            .expect("admit pending job");
        store
            .admit(&admitted_job("job-ready", JobState::Ready))
            .expect("admit ready job");
        store
            .admit(&admitted_job("job-ready-second", JobState::Ready))
            .expect("admit second ready job");
        let (ctx, workspace_root) = test_context(Some(store));
        let output = work(&ctx, WorkArgs::default())
            .await
            .expect("render admitted job panel");
        std::fs::remove_dir_all(store_root).expect("remove job store");
        std::fs::remove_dir_all(workspace_root).expect("remove test workspace");

        assert!(output.text.contains("Durable jobs: 3"));
        assert!(output.text.contains("Pending: 1"));
        assert!(output.text.contains("Ready: 2"));
        assert!(output.text.contains("Running: 0"));
        assert!(output.text.contains(
            "Approval and diff-preview data are unavailable in this local command context."
        ));
    }
}

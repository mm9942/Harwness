//! `/ps` — listet admitted durable Jobs und ihre Zustände.
//!
//! # Verantwortungsbereich
//! Diese Operation exponiert den aktuellen Job-Status als Slash-Command
//! (`/ps`) und als schreibgeschütztes Modell-Tool ohne Approval-Anforderung.
//!
//! # Schlüsseltypen
//! - [`PsArgs`] — optionaler Statusfilter für die Abfrage.
//! - [`PsOperation`] — generierter Unit-Struct, impl [`harw_operations::Operation`].
//!
//! # Nebenläufigkeit
//! `PsOperation` ist `Send + Sync` (Unit-Struct, kein innerer Zustand).
//!
//! # Fehlerfälle
//! Gibt [`harw_operations::OpError::InvalidArguments`] zurück, wenn
//! `json_args` nicht in [`PsArgs`] deserialisiert werden kann.
//!
//! # Durable Jobs
//! Die Operation liest eine Snapshot-Liste aus dem in [`OpContext`] registrierten
//! [`Arc<JobStore>`]. Angezeigt werden die durable Job-ID, der aktuelle Zustand
//! und die monotone Store-Revision; ohne Store ist die Operation nicht verfügbar.
//!
//! # Beispiel
//! ```no_run
//! // Wird über die Op-Registry aufgerufen — kein direkter Konstruktoraufruf nötig.
//! let _op = harw_ops::ps::PsOperation;
//! ```

use harw_job_runtime::JobState;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{JobListQuery, JobStore};
use std::sync::Arc;

/// Argumente für die `/ps`-Operation.
///
/// # Beschreibung
/// Steuert optionale Filterung der Job-Liste nach Status. Wird via
/// `serde_json::from_value` aus `OpInput::json_args` deserialisiert; bei
/// fehlendem JSON-Body wird [`Default::default`] verwendet (kein Filter).
///
/// # Felder
/// - `status` (`Option<String>`): Filterwert; gültige Werte sind die Zustände
///   `pending`, `ready`/`waiting`, `running`, `completed`/`finished`, `blocked`,
///   `failed` und `cancelled`/`canceled`. `None` bedeutet: alle Jobs.
///
/// Der Filter wird in [`JobListQuery`] als serverseitige Zustandsauswahl an den
/// durable Store weitergereicht.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct PsArgs {
    /// Filtert nach Job-Status, etwa `"running"`, `"waiting"` oder
    /// `"finished"`. `None` (Standard) zeigt alle Jobs an.
    #[serde(default)]
    #[raw(first)]
    pub status: Option<String>,
}

/// Listet alle laufenden Jobs und Kindprozesse.
///
/// # Beschreibung
/// Liest admitted durable Jobs aus dem [`JobStore`] des Kontextes. Die Ausgabe
/// enthält pro Datensatz die Job-ID, den Zustand und die Persistenz-Revision;
/// eine leere Snapshot-Liste ergibt `No jobs.`.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Liefert den erforderlichen `Arc<JobStore>`.
/// - `args` (`PsArgs`): Optionaler Statusfilter.
///
/// # Rückgabe
/// [`OpOutput`] mit einer Zeile pro durable Job im Snapshot.
///
/// # Fehler
/// Gibt [`OpError::InvalidArguments`] zurück, wenn `json_args` nicht in
/// [`PsArgs`] deserialisiert werden kann (wird vom Makro gehandhabt).
///
/// # Nebenläufigkeit
/// Rein lesend; die Dateisperren und Snapshot-Semantik gehören dem [`JobStore`].
///
/// # Beispiel
/// ```no_run
/// // Wird indirekt über Operation::run aufgerufen.
/// ```
#[operation(
    name = "ps",
    summary = "Listet alle laufenden Jobs und Kindprozesse.",
    domain = "execution",
    permission = "observer",
    command(path = "/ps", visibility = "channel_reduced", busy = "immediate"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: reines
    // Auflisten laufender Jobs, keine Mutation, keine Bestätigung nötig.
    web(path = "/api/ps", method = "get", approval = "none")
)]
async fn ps(ctx: &OpContext, args: PsArgs) -> Result<OpOutput, OpError> {
    let states = args.status.as_deref().map(parse_state).transpose()?;
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    // H12: mandantengebundene Aufrufer sehen nur Jobs des eigenen Mandanten.
    let jobs = crate::job_tenant::list_visible_jobs(
        ctx,
        store,
        JobListQuery {
            states: states.map(|state| vec![state]),
            ..JobListQuery::default()
        },
    )
    .map_err(|error| OpError::Execution(format!("could not list durable jobs: {error}")))?;
    if jobs.is_empty() {
        return Ok(OpOutput::from("No jobs.".to_owned()));
    }
    let text = jobs
        .iter()
        .map(|record| {
            format!(
                "{}\t{:?}\trev {}",
                record.job.id, record.job.state, record.revision
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(OpOutput::from(text))
}

fn parse_state(value: &str) -> Result<JobState, OpError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "pending" => Ok(JobState::Pending),
        "ready" | "waiting" => Ok(JobState::Ready),
        "running" => Ok(JobState::Running),
        "completed" | "finished" => Ok(JobState::Completed),
        "blocked" => Ok(JobState::Blocked),
        "failed" => Ok(JobState::Failed),
        "cancelled" | "canceled" => Ok(JobState::Cancelled),
        _ => Err(OpError::InvalidArguments(format!(
            "unknown job status `{value}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{PsArgs, ps};
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_root(prefix: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{}-{id}", std::process::id()))
    }

    fn test_context(store: Option<Arc<JobStore>>) -> TestResult<(OpContext, PathBuf)> {
        let root = unique_root("harw-ps-test");
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry::build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        if let Some(store) = store {
            services.insert(store);
        }
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, services),
            root,
        ))
    }

    fn stored_job(id: &str, state: harw_job_runtime::JobState) -> TestResult<StoredJob> {
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
        if state == harw_job_runtime::JobState::Ready {
            job.mark_ready(now).map_err(ctx("mark_ready"))?;
        }
        Ok(StoredJob {
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
        })
    }

    #[test]
    fn test_ps_args_from_raw_args_sets_status() -> TestResult {
        let args = PsArgs::from_raw_args(&toks(&["running"]));
        match args {
            Ok(a) => assert_eq!(a.status.as_deref(), Some("running")),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_ps_args_from_raw_args_empty_tokens_sets_status_none() -> TestResult {
        let args = PsArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.status.is_none()),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn ps_without_job_store_is_not_available() -> TestResult {
        let (ctx, _root) = test_context(None)?;
        assert!(matches!(
            ps(&ctx, PsArgs::default()).await,
            Err(OpError::NotAvailable(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn ps_rejects_invalid_status() -> TestResult {
        let (ctx, _root) = test_context(None)?;
        let result = ps(
            &ctx,
            PsArgs {
                status: Some("unknown".to_owned()),
            },
        )
        .await;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn ps_lists_admitted_jobs_with_state_and_revision() -> TestResult {
        let root = unique_root("harw-ps-store");
        let store = Arc::new(JobStore::new(&root));
        store
            .admit(&stored_job(
                "job-pending",
                harw_job_runtime::JobState::Pending,
            )?)
            .map_err(ctx("admit job-pending"))?;
        let (ctx, _root) = test_context(Some(store))?;
        let output = ps(&ctx, PsArgs::default())
            .await
            .map_err(crate::test_support::ctx("ps"))?;
        assert!(output.text.contains("job-pending\tPending\trev 0"));
        Ok(())
    }

    #[tokio::test]
    async fn ps_honors_state_filter() -> TestResult {
        let root = unique_root("harw-ps-filter");
        let store = Arc::new(JobStore::new(&root));
        store
            .admit(&stored_job(
                "job-pending",
                harw_job_runtime::JobState::Pending,
            )?)
            .map_err(ctx("admit job-pending"))?;
        store
            .admit(&stored_job("job-ready", harw_job_runtime::JobState::Ready)?)
            .map_err(ctx("admit job-ready"))?;
        let (ctx, _root) = test_context(Some(store))?;
        let output = ps(
            &ctx,
            PsArgs {
                status: Some("waiting".to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("ps"))?;
        assert_eq!(output.text, "job-ready\tReady\trev 0");
        Ok(())
    }

    // H12: Mandanten-Filter.
    #[tokio::test]
    async fn ps_unscoped_caller_sees_all_tenants() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, JOB_B, context, two_tenants};
        let jobs = two_tenants(harw_job_runtime::JobState::Ready)?;
        let op_ctx = context(&jobs, None)?;
        let output = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert!(output.text.contains(JOB_A), "{}", output.text);
        assert!(output.text.contains(JOB_B), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_scoped_caller_sees_only_own_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, JOB_B, TENANT_A, context, two_tenants};
        let jobs = two_tenants(harw_job_runtime::JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert!(output.text.contains(JOB_A), "{}", output.text);
        assert!(!output.text.contains(JOB_B), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_scoped_caller_without_own_jobs_sees_none() -> TestResult {
        use crate::job_tenant::fixtures::{context, two_tenants};
        let jobs = two_tenants(harw_job_runtime::JobState::Ready)?;
        let op_ctx = context(&jobs, Some("tenant-other"))?;
        let output = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert_eq!(output.text, "No jobs.");
        Ok(())
    }
}

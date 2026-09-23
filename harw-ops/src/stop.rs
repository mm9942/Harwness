//! `/stop` — bricht einen laufenden Job kontrolliert ab.
//!
//! # Verantwortungsbereich
//! Implementiert die `stop`-Operation gemäß Harwness Plan v2.
//! Exponiert zwei Flächen:
//! - **Command** `/stop` mit `channel_parity`-Sichtbarkeit.
//! - **ModelTool** mit `approval = "always"` (keine `readonly`-Markierung,
//!   da die Operation einen laufenden Job abbricht und damit Wirkung hat).
//!
//! # Schlüsseltypen
//! - [`StopArgs`] — deserialisierbare Eingabe-Argumente.
//! - `StopOperation` — vom `#[operation]`-Makro erzeugter Implementations-Struct.
//! - [`JobStore`] — persistiert die Abbruch-Transition dauerhaft, bevor die
//!   erfolgreiche Antwort ausgegeben wird.
//!
//! # Dauerhafte Abbruchgrenze
//! Die Operation löst den konfigurierten [`JobStore`] als `Arc<JobStore>` aus
//! dem [`OpContext`] auf und ruft dessen `cancel`-Transition auf. Damit ist
//! `/stop` kein flüchtiges Signal: Ein zugelassener abbrechbarer Job wird
//! dauerhaft in den Zustand `Cancelled` überführt und die Antwort enthält die
//! vom Store vergebene Revision.
//!
//! Der Actor an dieser lokalen Command-Grenze ist bewusst
//! [`ApprovalActor::Operator`] mit der ID `"local-command"`. Adapter, die einen
//! extern authentifizierten Actor kennen, müssen diese Vertrauensgrenze vor dem
//! Aufruf der Operation entsprechend modellieren.
//!
//! # Nebenläufigkeit
//! `StopOperation` ist ein Unit-Struct ohne inneren Zustand → `Send + Sync`.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: wenn `job_id` nicht angegeben wurde.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::stop::StopArgs;
//!
//! let args = StopArgs { job_id: Some("job-42".to_owned()) };
//! assert_eq!(args.job_id.as_deref(), Some("job-42"));
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::{CancelRequest, JobStore};
use harw_types::{ApprovalActor, WorkId};
use std::sync::Arc;

/// Eingabe-Argumente für die `stop`-Operation.
///
/// # Beschreibung
/// Kapselt die optionale Job-ID des abzubrechenden Jobs. Das Feld ist optional
/// auf Deserialisierungsebene, wird aber in der Ausführung erzwungen —
/// fehlt `job_id`, gibt die Operation [`OpError::InvalidArguments`] zurück.
///
/// # Felder
/// - `job_id` (`Option<String>`): ID des Jobs, der abgebrochen werden soll.
///
/// # Beispiel
/// ```rust
/// use harw_ops::stop::StopArgs;
///
/// let args: StopArgs = serde_json::from_str(r#"{"job_id":"abc-123"}"#).unwrap();
/// assert_eq!(args.job_id.as_deref(), Some("abc-123"));
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct StopArgs {
    /// ID des Jobs, der abgebrochen werden soll.
    #[serde(default)]
    #[raw(first)]
    pub job_id: Option<String>,
}

#[operation(
    name = "stop",
    summary = "Bricht einen laufenden Job kontrolliert ab.",
    domain = "execution",
    permission = "operator",
    command(path = "/stop", visibility = "channel_parity", busy = "immediate"),
    model_tool(approval = "always"),
    // Web-Fläche übernimmt exakt dieselbe Achse wie das ModelTool:
    // `method = "post"` (Abbruch ist ein Seiteneffekt), `approval = "always"` —
    // ein Job-Abbruch ist irreversibel und wird dadurch für `harw-web` zu
    // einem ApprovalRequest, nie zu einem direkt ausführbaren Knopf.
    web(path = "/api/stop", method = "post", approval = "always")
)]
async fn stop(ctx: &OpContext, args: StopArgs) -> Result<OpOutput, OpError> {
    let Some(job) = args.job_id.as_deref() else {
        return Err(OpError::InvalidArguments(
            "job_id ist erforderlich".to_owned(),
        ));
    };
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let transition = store
        .cancel(
            &WorkId::from_str(job),
            &CancelRequest {
                cancelled_at: jiff::Timestamp::now(),
                cancelled_by: ApprovalActor::Operator {
                    id: "local-command".to_owned(),
                },
                reason: "cancelled through /stop".to_owned(),
            },
        )
        .map_err(|error| {
            OpError::Execution(format!("could not cancel durable job `{job}`: {error}"))
        })?;
    Ok(OpOutput::from(format!(
        "Cancelled job {} (revision {}).",
        transition.work_id, transition.revision
    )))
}

#[cfg(test)]
mod tests {
    use super::{StopArgs, stop};
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
        let root = std::env::temp_dir().join(format!("harw-stop-test-{}-{id}", std::process::id()));
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
    fn test_stop_args_from_raw_args_sets_job_id() -> TestResult {
        let args = StopArgs::from_raw_args(&toks(&["job-42"]));
        match args {
            Ok(a) => assert_eq!(a.job_id.as_deref(), Some("job-42")),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_stop_args_from_raw_args_empty_tokens_sets_job_id_none() -> TestResult {
        let args = StopArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.job_id.is_none()),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn stop_without_job_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = stop(&ctx, StopArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::InvalidArguments(message)) => assert!(message.contains("job_id")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected InvalidArguments, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn stop_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root, _) = make_test_ctx(false)?;
        let result = stop(
            &ctx,
            StopArgs {
                job_id: Some("work-missing-store".to_owned()),
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
    async fn stop_cancels_admitted_job_and_reports_id_and_revision() -> TestResult {
        let (ctx, root, store) = make_test_ctx(true)?;
        let store = store.ok_or(TestError::Missing("test context includes job store"))?;
        let work_id = WorkId::from_str("work-cancel");
        store
            .admit(&admitted_job(work_id.as_str())?)
            .map_err(crate::test_support::ctx("admit job"))?;

        let output = stop(
            &ctx,
            StopArgs {
                job_id: Some(work_id.as_str().to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("cancel admitted job"))?;
        let persisted = store
            .get(&work_id)
            .map_err(crate::test_support::ctx("read cancelled job"))?;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        assert_eq!(persisted.job.state, JobState::Cancelled);
        assert!(output.text.contains(work_id.as_str()));
        assert!(output.text.contains(&persisted.revision.to_string()));
        Ok(())
    }
}

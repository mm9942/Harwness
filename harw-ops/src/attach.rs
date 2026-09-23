//! Implementierung der `/attach`-Operation — Inspektion eines dauerhaft gespeicherten Jobs.
//!
//! # Verantwortungsbereich
//! Stellt den `/attach <job-id>`-Befehl bereit, mit dem der Nutzer den aktuellen
//! Zustand eines bereits zugelassenen Jobs aus dem [`JobStore`] abfragt. Die
//! Operation öffnet keine interaktive Session und verändert den Job nicht.
//!
//! # Schlüsseltypen
//! - [`AttachArgs`] — Argumentstruktur; enthält die optionale Job-ID.
//!
//! # Surface-Matrix
//! - **Command**: `/attach` (visibility = `tui_only`)
//! - **Model-Tool**: nein (für das Modell sinnlos)
//!
//! # Nebenläufigkeit
//! Die erzeugte Op-Struct ist `Send + Sync` (zustandslos, kein innerer Mutationspfad).
//!
//! # Fehler
//! - [`harw_operations::OpError::InvalidArguments`]: wenn `job_id` nicht übergeben wurde.
//!
//! # Beispiele
//! ```rust,no_run
//! use harw_ops::attach::AttachArgs;
//! // AttachArgs::default() liefert job_id = None (führt zu InvalidArguments-Fehler).
//! let args = AttachArgs { job_id: Some("work-attach-42".to_owned()) };
//! assert_eq!(args.job_id.as_deref(), Some("work-attach-42"));
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_session_store::JobStore;
use harw_types::WorkId;
use std::sync::Arc;

/// Argumente für den `/attach`-Befehl.
///
/// # Felder
/// - `job_id` (`Option<String>`): Die ID des Jobs, der inspiziert werden soll
///   (z. B. `"work-attach-42"`). Wird `None` übergeben, gibt die Operation einen
///   [`OpError::InvalidArguments`]-Fehler zurück.
///
/// # Deserialisierung
/// Implementiert [`serde::Deserialize`] und [`Default`], damit das Framework
/// leere Argument-Payloads korrekt handhaben kann.
///
/// # Beispiele
/// ```rust
/// use harw_ops::attach::AttachArgs;
/// let args: AttachArgs = serde_json::from_str(r#"{"job_id":"work-attach-1"}"#).unwrap();
/// assert_eq!(args.job_id.as_deref(), Some("work-attach-1"));
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct AttachArgs {
    /// ID des dauerhaft gespeicherten Jobs, der inspiziert werden soll (z. B. `"work-attach-42"`).
    #[serde(default)]
    #[raw(first)]
    pub job_id: Option<String>,
}

/// Inspiziert einen dauerhaft gespeicherten Job.
///
/// # Beschreibung
/// Liest den [`JobStore`] für die angegebene ID und gibt ID, Zustand und
/// Persistenzrevision des gefundenen Jobs zurück. Es findet keine interaktive
/// TUI-Anbindung und keine Mutation statt.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit dem registrierten [`Arc<JobStore>`].
/// - `args` (`AttachArgs`): Enthält die optionale Job-ID.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit ID, Zustand und Revision des gefundenen Jobs.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: wenn `args.job_id` `None` ist.
/// - [`OpError::NotAvailable`]: wenn kein [`Arc<JobStore>`] im Kontext registriert ist.
///
/// # Nebenläufigkeit
/// Die Funktion ist `async` und führt ausschließlich eine lesende Abfrage aus —
/// sicher aus mehreren Tasks heraus aufrufbar.
///
/// # Beispiele
/// ```rust,no_run
/// // Aufgerufen über das Operation-Trait; hier nur zur Illustration.
/// use harw_ops::attach::AttachArgs;
/// let args = AttachArgs { job_id: Some("work-attach-42".to_owned()) };
/// // Erwartetes Ergebnis: Ok(OpOutput mit ID, Zustand und Revision von `work-attach-42`)
/// ```
#[operation(
    name = "attach",
    summary = "Inspiziert einen dauerhaft gespeicherten Job.",
    domain = "execution",
    permission = "operator",
    command(path = "/attach", visibility = "tui_only"),
    // Web-Fläche: laut Moduldoku "öffnet keine interaktive Session und
    // verändert den Job nicht" — reine Inspektion über den `JobStore`, daher
    // `method = "get"`. `approval = "none"`, weil ein Lesevorgang keine
    // Bestätigung braucht.
    web(path = "/api/attach", method = "get", approval = "none")
)]
async fn attach(ctx: &OpContext, args: AttachArgs) -> Result<OpOutput, OpError> {
    let job = args
        .job_id
        .as_deref()
        .ok_or_else(|| OpError::InvalidArguments("job_id fehlt".to_owned()))?;
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    let work_id = WorkId::from_str(job);
    let record = store.get(&work_id).map_err(|error| {
        OpError::Execution(format!("could not attach to durable job `{job}`: {error}"))
    })?;
    Ok(OpOutput::from(format!(
        "Job {}\nState: {:?}\nRevision: {}",
        record.job.id, record.job.state, record.revision
    )))
}

#[cfg(test)]
mod tests {
    use super::{AttachArgs, attach};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::{Budget, Job, JobKind, RetryPolicy};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, SessionId, TenantId, TurnId, WorkId, WorkspaceId};
    use jiff::Timestamp;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_context(store: Option<Arc<JobStore>>) -> TestResult<(OpContext, PathBuf)> {
        let root = std::env::temp_dir().join(format!(
            "harw-attach-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
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

    fn admitted_record(id: &str) -> TestResult<harw_job_runtime::StoredJob> {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 1,
                base_delay: jiff::SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: jiff::SignedDuration::from_secs(10),
            },
            now,
        );
        job.mark_ready(now).map_err(ctx("mark_ready"))?;
        Ok(harw_job_runtime::StoredJob {
            job,
            scope: harw_job_runtime::JobScope::new(
                TenantId::from_str("test-tenant"),
                WorkspaceId::from_str("workspace"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "inspect"}),
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
    fn test_attach_args_from_raw_args_sets_job_id() -> TestResult {
        let args = AttachArgs::from_raw_args(&toks(&["work-attach-42"]));
        match args {
            Ok(a) => assert_eq!(a.job_id.as_deref(), Some("work-attach-42")),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[test]
    fn test_attach_args_from_raw_args_empty_tokens_sets_job_id_none() -> TestResult {
        let args = AttachArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.job_id.is_none()),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn attach_without_job_id_returns_invalid_arguments() -> TestResult {
        let (ctx, root) = test_context(None)?;
        let result = attach(&ctx, AttachArgs::default()).await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(message)) if message == "job_id fehlt")
        );
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        Ok(())
    }

    #[tokio::test]
    async fn attach_without_job_store_returns_not_available() -> TestResult {
        let (ctx, root) = test_context(None)?;
        let result = attach(
            &ctx,
            AttachArgs {
                job_id: Some("work-attach-42".to_owned()),
            },
        )
        .await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(message)) if message == "durable job store is not configured")
        );
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        Ok(())
    }

    #[tokio::test]
    async fn attach_admitted_job_renders_id_state_and_revision() -> TestResult {
        let (ctx, root) = {
            let store_root = std::env::temp_dir().join(format!(
                "harw-attach-store-{}-{}",
                std::process::id(),
                SessionId::new()
            ));
            std::fs::create_dir_all(&store_root).map_err(ctx("create store root"))?;
            let store = Arc::new(JobStore::new(&store_root));
            store
                .admit(&admitted_record("work-attach-42")?)
                .map_err(ctx("admit record"))?;
            let (ctx, context_root) = test_context(Some(store))?;
            (ctx, (context_root, store_root))
        };
        let result = attach(
            &ctx,
            AttachArgs {
                job_id: Some("work-attach-42".to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("attach"))?;
        assert_eq!(result.text, "Job work-attach-42\nState: Ready\nRevision: 0");
        std::fs::remove_dir_all(ctx.sandbox().workspace().canonical_root())
            .map_err(crate::test_support::ctx("remove workspace root"))?;
        std::fs::remove_dir_all(root.1).map_err(crate::test_support::ctx("remove store root"))?;
        Ok(())
    }
}

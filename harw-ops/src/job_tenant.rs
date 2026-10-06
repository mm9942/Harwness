//! Mandanten-Sichtbarkeit durabler Jobs für die Job-Operationen (H12).
//!
//! # Verantwortungsbereich
//! Durable Jobs ([`StoredJob`]) tragen über [`StoredJob::scope`] einen
//! Mandanten. Die Operationen, die solche Jobs auflisten, lesen oder
//! verändern (`/ps`, `/work`, `/attach`, `/review`, `/approve`, `/deny`,
//! `/cancel`, `/stop`, `/retry`), wenden hier gemeinsam die Regel aus
//! [`OpContext::tenant_admits`] an:
//! - Aufrufer ohne Mandanten-Scope sehen alles (bisheriges Verhalten,
//!   unverändert — für sie liest dieses Modul nichts zusätzlich).
//! - Aufrufer mit Scope sehen nur Jobs des eigenen Mandanten.
//!
//! # Fremde Jobs verraten ihre Existenz nicht
//! Ein Einzelzugriff (Lesen oder Mutation) auf einen Job eines fremden
//! Mandanten liefert **denselben** Fehler wie ein unbekannter Job:
//! [`SessionStoreError::JobNotFound`]. Die aufrufende Operation übersetzt
//! ihn mit ihrer gewohnten Fehlerabbildung, sodass die Antwort für „fremd"
//! und „gibt es nicht" byte-gleich ist. (`OpError` kennt keine eigene
//! `NotFound`-Variante; eine neue Variante wäre eine Schnittstellenänderung
//! über `harw-operations` hinaus.)
//!
//! # Nebenläufigkeit
//! Die Prüfung vor einer Mutation ist ein separater `get`: der Mandant eines
//! Jobs ist nach der Zulassung unveränderlich, das Zeitfenster zwischen
//! Prüfung und Mutation kann ihn also nicht ändern.

use harw_job_runtime::StoredJob;
use harw_operations::OpContext;
use harw_session_store::{JobListQuery, JobStore, SessionStoreError, SessionStoreResult};
use harw_types::WorkId;

/// Darf der Aufrufer von `ctx` diesen Job sehen?
#[must_use]
pub(crate) fn admits_job(ctx: &OpContext, record: &StoredJob) -> bool {
    ctx.tenant_admits(Some(record.scope.tenant()))
}

/// Lädt einen Job, sofern der Aufrufer ihn sehen darf.
///
/// # Fehler
/// - [`SessionStoreError::JobNotFound`] für unbekannte **und** für fremde
///   Jobs (keine Existenz-Preisgabe).
/// - Jeder andere Speicherfehler von [`JobStore::get`] unverändert.
pub(crate) fn get_visible_job(
    ctx: &OpContext,
    store: &JobStore,
    work_id: &WorkId,
) -> SessionStoreResult<StoredJob> {
    let record = store.get(work_id)?;
    if admits_job(ctx, &record) {
        Ok(record)
    } else {
        Err(SessionStoreError::JobNotFound {
            work_id: work_id.clone(),
        })
    }
}

/// Loads a job only when it belongs to the exact trusted workspace binding of
/// this operation context.
///
/// This is intentionally stricter than get_visible_job: model-readable result
/// access is confined to both the bound tenant and workspace even when the
/// caller has no separate tenant scope. A mismatch is indistinguishable from
/// a missing WorkId.
pub(crate) fn get_bound_workspace_job(
    ctx: &OpContext,
    store: &JobStore,
    work_id: &WorkId,
) -> SessionStoreResult<StoredJob> {
    let record = get_visible_job(ctx, store, work_id)?;
    let binding = ctx.sandbox().workspace();
    if record.scope.tenant() == binding.tenant()
        && record.scope.workspace() == binding.workspace()
    {
        Ok(record)
    } else {
        Err(SessionStoreError::JobNotFound {
            work_id: work_id.clone(),
        })
    }
}

/// Wache vor einer Mutation: prüft für mandantengebundene Aufrufer, dass der
/// Job existiert und zum eigenen Mandanten gehört.
///
/// Ohne Mandanten-Scope ist die Wache ein No-op (kein zusätzlicher Lesezugriff;
/// die Mutation meldet unbekannte Jobs wie bisher selbst).
///
/// # Fehler
/// Wie [`get_visible_job`].
pub(crate) fn ensure_job_visible(
    ctx: &OpContext,
    store: &JobStore,
    work_id: &WorkId,
) -> SessionStoreResult<()> {
    if ctx.tenant().is_none() {
        return Ok(());
    }
    get_visible_job(ctx, store, work_id).map(|_| ())
}

/// Listet die für den Aufrufer sichtbaren Jobs einer Seite.
///
/// # Beschreibung
/// Ohne Mandanten-Scope genau eine Store-Seite wie bisher. Mit Scope wird
/// seitenweise weitergelesen, bis `query.limit` sichtbare Jobs gesammelt
/// sind oder der Store erschöpft ist — fremde Jobs dürfen die Seite eines
/// Mandanten nicht „auffressen".
///
/// # Fehler
/// Speicherfehler von [`JobStore::list`].
pub(crate) fn list_visible_jobs(
    ctx: &OpContext,
    store: &JobStore,
    mut query: JobListQuery,
) -> SessionStoreResult<Vec<StoredJob>> {
    if ctx.tenant().is_none() {
        return Ok(store.list(&query)?.jobs);
    }
    let limit = query.limit;
    let mut visible = Vec::new();
    if limit == 0 {
        return Ok(visible);
    }
    loop {
        let page = store.list(&query)?;
        for record in page.jobs {
            if admits_job(ctx, &record) {
                visible.push(record);
                if visible.len() >= limit {
                    return Ok(visible);
                }
            }
        }
        match page.next_cursor {
            Some(cursor) => query.cursor = Some(cursor),
            None => return Ok(visible),
        }
    }
}

/// Test-Bausteine für die Mandanten-Tests der Job-Operationen.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::job_authority::JobMutationScope;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, JobState, RetryPolicy, StoredJob};
    use harw_operations::{OpContext, OpError, OpOutput, context::ServiceMap};
    use harw_session_store::JobStore;
    use harw_types::{ApprovalActor, SessionId, TenantId, TurnId, WorkId, WorkspaceId};
    use jiff::{SignedDuration, Timestamp};
    use std::sync::Arc;

    /// Mandant des Jobs `JOB_A`.
    pub(crate) const TENANT_A: &str = "tenant-a";
    /// Mandant des Jobs `JOB_B`.
    pub(crate) const TENANT_B: &str = "tenant-b";
    /// Job-Kennung im Mandanten `TENANT_A`.
    pub(crate) const JOB_A: &str = "job-tenant-a";
    /// Job-Kennung im Mandanten `TENANT_B`.
    pub(crate) const JOB_B: &str = "job-tenant-b";

    /// Temporäres Verzeichnis plus Job-Store mit je einem Job pro Mandant.
    pub(crate) struct TenantJobs {
        /// Hält das Verzeichnis bis zum Testende am Leben.
        pub(crate) dir: tempfile::TempDir,
        /// Der gefüllte Store.
        pub(crate) store: Arc<JobStore>,
    }

    /// Ein Job in `state` für `tenant` (Retry-Budget: `max_attempts` 3,
    /// `attempts` 1 — damit bleibt ein Failed-Job wiederholbar).
    pub(crate) fn job(id: &str, tenant: &str, state: JobState) -> StoredJob {
        let now = Timestamp::now();
        let mut job = Job::new(
            WorkId::from_str(id),
            JobKind::Worker,
            Budget::unbounded(),
            RetryPolicy {
                max_attempts: 3,
                base_delay: SignedDuration::from_secs(1),
                factor: 2.0,
                max_delay: SignedDuration::from_secs(10),
            },
            now,
        );
        job.state = state;
        job.attempts = 1;
        StoredJob {
            job,
            scope: JobScope::new(
                TenantId::from_str(tenant),
                WorkspaceId::from_str("ws"),
                ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                },
            ),
            input: serde_json::json!({"task": "tenant test"}),
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

    /// Store mit `JOB_A` (Mandant A) und `JOB_B` (Mandant B), beide in `state`.
    pub(crate) fn two_tenants(state: JobState) -> TestResult<TenantJobs> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("jobs")));
        store
            .admit(&job(JOB_A, TENANT_A, state))
            .map_err(ctx("admit tenant-a job"))?;
        store
            .admit(&job(JOB_B, TENANT_B, state))
            .map_err(ctx("admit tenant-b job"))?;
        Ok(TenantJobs { dir, store })
    }

    /// `OpContext` mit dem Store aus `jobs`; `tenant` `None` = ohne Scope.
    pub(crate) fn context(jobs: &TenantJobs, tenant: Option<&str>) -> TestResult<OpContext> {
        let root = jobs.dir.path();
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create workspace"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str(TENANT_A),
                workspace: WorkspaceId::from_str("ws"),
                root: "ws".into(),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str(TENANT_A), &WorkspaceId::from_str("ws"))
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::clone(&jobs.store));
        services.insert(JobMutationScope::TenantVisible);
        let op_ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok(match tenant {
            Some(tenant) => op_ctx.with_tenant(TenantId::from_str(tenant)),
            None => op_ctx,
        })
    }

    /// Eine Job-Kennung, die es in keinem Store gibt.
    pub(crate) const MISSING: &str = "job-does-not-exist";

    /// Prüft, dass der Zugriff auf den fremden `JOB_B` exakt so scheitert wie
    /// der auf den nicht existierenden `MISSING` (gleiche Variante, gleicher
    /// Text bis auf die Kennung) — keine Existenz-Preisgabe.
    pub(crate) fn assert_hidden_like_missing(
        foreign: Result<OpOutput, OpError>,
        missing: Result<OpOutput, OpError>,
    ) -> TestResult {
        match (foreign, missing) {
            (Err(foreign), Err(missing)) => {
                let foreign = format!("{foreign:?}").replace(JOB_B, "<id>");
                let missing = format!("{missing:?}").replace(MISSING, "<id>");
                if foreign == missing {
                    Ok(())
                } else {
                    Err(TestError::Unexpected(format!(
                        "foreign `{foreign}` differs from missing `{missing}`"
                    )))
                }
            }
            (foreign, missing) => Err(TestError::Unexpected(format!(
                "expected two errors, got foreign {foreign:?} / missing {missing:?}"
            ))),
        }
    }

    /// Der persistierte Zustand eines Jobs.
    pub(crate) fn state_of(jobs: &TenantJobs, id: &str) -> TestResult<JobState> {
        jobs.store
            .get(&WorkId::from_str(id))
            .map(|record| record.job.state)
            .map_err(ctx("read job state"))
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{JOB_A, JOB_B, TENANT_A, context, job, two_tenants};
    use super::{ensure_job_visible, get_visible_job, list_visible_jobs};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_runtime::JobState;
    use harw_session_store::{JobListQuery, SessionStoreError};
    use harw_types::WorkId;

    fn ids(records: &[harw_job_runtime::StoredJob]) -> Vec<String> {
        let mut ids: Vec<String> = records.iter().map(|r| r.job.id.to_string()).collect();
        ids.sort();
        ids
    }

    #[test]
    fn unscoped_caller_lists_every_tenant() -> TestResult {
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, None)?;
        let listed = list_visible_jobs(&op_ctx, &jobs.store, JobListQuery::default())
            .map_err(ctx("list"))?;
        assert_eq!(ids(&listed), vec![JOB_A.to_owned(), JOB_B.to_owned()]);
        Ok(())
    }

    #[test]
    fn scoped_caller_lists_only_own_tenant() -> TestResult {
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let listed = list_visible_jobs(&op_ctx, &jobs.store, JobListQuery::default())
            .map_err(ctx("list"))?;
        assert_eq!(ids(&listed), vec![JOB_A.to_owned()]);
        Ok(())
    }

    /// Fremde Jobs verdrängen keine eigenen aus einer Seite: der Scope liest
    /// über Seitengrenzen hinweg weiter, bis `limit` eigene Jobs vorliegen.
    #[test]
    fn scoped_listing_pages_past_foreign_jobs() -> TestResult {
        let jobs = two_tenants(JobState::Ready)?;
        for index in 0..5 {
            jobs.store
                .admit(&job(
                    &format!("job-foreign-{index}"),
                    "tenant-c",
                    JobState::Ready,
                ))
                .map_err(ctx("admit foreign job"))?;
        }
        jobs.store
            .admit(&job("job-tenant-a-2", TENANT_A, JobState::Ready))
            .map_err(ctx("admit second own job"))?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let listed = list_visible_jobs(
            &op_ctx,
            &jobs.store,
            JobListQuery {
                limit: 2,
                ..JobListQuery::default()
            },
        )
        .map_err(ctx("list"))?;
        assert_eq!(
            ids(&listed),
            vec![JOB_A.to_owned(), "job-tenant-a-2".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn foreign_single_read_is_indistinguishable_from_missing() -> TestResult {
        let jobs = two_tenants(JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let own = get_visible_job(&op_ctx, &jobs.store, &WorkId::from_str(JOB_A))
            .map_err(ctx("own job"))?;
        assert_eq!(own.job.id.to_string(), JOB_A);
        match get_visible_job(&op_ctx, &jobs.store, &WorkId::from_str(JOB_B)) {
            Err(SessionStoreError::JobNotFound { work_id }) => {
                assert_eq!(work_id.as_str(), JOB_B);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected JobNotFound, got {other:?}"
                )));
            }
        }
        assert!(matches!(
            ensure_job_visible(&op_ctx, &jobs.store, &WorkId::from_str(JOB_B)),
            Err(SessionStoreError::JobNotFound { .. })
        ));
        let unscoped = context(&jobs, None)?;
        ensure_job_visible(&unscoped, &jobs.store, &WorkId::from_str(JOB_B))
            .map_err(ctx("unscoped guard admits foreign job"))?;
        Ok(())
    }
}

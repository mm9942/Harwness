//! `/ps` — listet admitted durable Jobs und `job.start`-Hintergrundprozesse
//! mit ihren Zuständen.
//!
//! # Verantwortungsbereich
//! Diese Operation exponiert den aktuellen Job-Status als Slash-Command
//! (`/ps`) und als schreibgeschütztes Modell-Tool ohne Approval-Anforderung.
//!
//! # Schlüsseltypen
//! - [`PsArgs`] — optionaler Statusfilter und optionale Art (`work`/`process`).
//! - [`PsOperation`] — generierter Unit-Struct, impl [`harw_operations::Operation`].
//!
//! # Nebenläufigkeit
//! `PsOperation` ist `Send + Sync` (Unit-Struct, kein innerer Zustand).
//!
//! # Fehlerfälle
//! Gibt [`harw_operations::OpError::InvalidArguments`] zurück, wenn
//! `json_args` nicht in [`PsArgs`] deserialisiert werden kann.
//!
//! # Jobs
//! Die Operation zeigt zwei Quellen in einer Liste, unterschieden durch die
//! Art-Spalte:
//! - `work`: durable Arbeitsaufträge aus dem in [`OpContext`] registrierten
//!   [`Arc<JobStore>`] (Snapshot-Liste); ohne Store ist die Operation nicht
//!   verfügbar.
//! - `process`: `job.start`-Hintergrundprozesse. Liegt ein
//!   [`Arc<JobManager>`] im Kontext (TUI), kommt der Live-Zustand aus
//!   [`JobManager::list`]; sonst liest ein rein lesender Leser die
//!   `meta.json`-Dateien unter `<projekt>/.harw/state/jobs` (Pfad aus dem
//!   [`harw_home::ResolvedHomeContext`]) und legt dabei nichts an.
//!
//! Ohne Art (`/ps`, `/ps running`) zeigt die Operation nur die Übersicht:
//! je Art eine Zeile mit Zählern pro Zustand (`work: 1 pending · 2 ready`,
//! `process: none`) und den Hinweis auf `/ps work|process [status]`. Erst mit
//! Art erscheinen die einzelnen Zeilen genau dieser Art.
//!
//! Jede Zeile ist tabulatorgetrennt: ID, Art, Zustand, `owner` (besitzende
//! Agenten-Sitzung), `profile` (`host`/`bwrap`), `end` (Endgrund) und `rev`
//! (Store-Revision); nicht zutreffende Felder stehen als `-`. Befehl, Name,
//! Arbeitsverzeichnis und Umgebungsvariablen werden nie ausgegeben.
//! Mandantengebundene Aufrufer sehen keine `process`-Zeilen, weil
//! Hintergrundprozesse keinen Mandanten tragen (H12, fail closed).
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
use harw_tool_job::model::{JobEndReason, META_FILE};
use harw_tool_job::{Caller, JobManager, JobManagerConfig, JobMeta, JobState as ProcessState};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// Obergrenze für eine gelesene `meta.json` (1 MiB); größere Dateien werden
/// abgeschnitten gelesen und scheitern damit am Parsen.
const MAX_META_BYTES: u64 = 1 << 20;

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
/// durable Store weitergereicht. Für `process`-Zeilen gilt die Zuordnung
/// `pending`/`ready` → `queued`, `running` → `running`/`detached`,
/// `completed` → `succeeded`, `failed` → `failed`/`unknown`,
/// `cancelled` → `stopped`; `blocked` trifft keinen Hintergrundprozess.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs, harw_macros::OpArgs)]
pub struct PsArgs {
    /// Filtert nach Job-Status, etwa `"running"`, `"waiting"` oder
    /// `"finished"`. `None` (Standard) zeigt alle Jobs an.
    #[serde(default)]
    #[raw(first)]
    pub status: Option<String>,
    /// Art der Zeilen: `work` oder `process`. `None` (Standard) zeigt nur die
    /// Übersicht je Art mit Zählern pro Zustand, keine einzelnen Zeilen.
    /// Roh-Aufrufe dürfen die Art zuerst nennen (`/ps process running`).
    #[serde(default)]
    #[raw(nth = 1)]
    pub kind: Option<String>,
}

/// Art einer `/ps`-Zeile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Durable Arbeitsaufträge aus dem [`JobStore`].
    Work,
    /// `job.start`-Hintergrundprozesse.
    Process,
}

fn parse_kind(value: &str) -> Option<Kind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "work" => Some(Kind::Work),
        "process" => Some(Kind::Process),
        _ => None,
    }
}

/// Ordnet Art und Status aus [`PsArgs`] zu.
///
/// Roh-Tokens kommen positionsweise an (`status` = erstes, `kind` = zweites
/// Token). Ist das erste Token eine Art, ist das zweite der Status; so
/// funktionieren `/ps work`, `/ps process running` und `/ps running work`.
///
/// # Fehler
/// [`OpError::InvalidArguments`] für eine unbekannte Art.
fn split_kind(args: PsArgs) -> Result<(Option<Kind>, Option<String>), OpError> {
    let PsArgs { status, kind } = args;
    if let Some(kind_from_status) = status.as_deref().and_then(parse_kind) {
        if let Some(second) = kind.as_deref() {
            if parse_kind(second).is_some() {
                return Err(OpError::InvalidArguments(format!(
                    "two job kinds given: `{}` and `{second}`",
                    status.as_deref().unwrap_or_default()
                )));
            }
        }
        return Ok((Some(kind_from_status), kind));
    }
    match kind {
        None => Ok((None, status)),
        Some(value) => match parse_kind(&value) {
            Some(parsed) => Ok((Some(parsed), status)),
            None => Err(OpError::InvalidArguments(format!(
                "unknown job kind `{value}` (expected `work` or `process`)"
            ))),
        },
    }
}

/// Listet alle laufenden Jobs und Kindprozesse.
///
/// # Beschreibung
/// Liest admitted durable Jobs aus dem [`JobStore`] des Kontextes (Art `work`)
/// und die `job.start`-Hintergrundprozesse (Art `process`) in eine Liste. Die
/// Prozesse kommen live aus einem [`JobManager`] im Kontext, sonst rein lesend
/// aus `<projekt>/.harw/state/jobs/<id>/meta.json`; Zustände einer abgestürzten
/// harw-Instanz erscheinen dabei so, wie sie gespeichert sind.
///
/// Ohne Art liefert sie nur die Übersicht je Art mit Zählern pro Zustand
/// (siehe Moduldoku); mit Art (`kind`) die Zeilen genau dieser Art.
///
/// Jede Zeile ist tabulatorgetrennt: ID, Art (`work`/`process`), Zustand,
/// `owner <sitzung>`, `profile <host|bwrap>`, `end <endgrund>` und
/// `rev <revision>`; nicht zutreffende Felder stehen als `-`. `work`-Zeilen
/// stehen in Store-Reihenfolge, `process`-Zeilen sortiert nach ID. Befehl, Name, Arbeitsverzeichnis und Umgebungsvariablen werden nie
/// ausgegeben. Mandantengebundene Aufrufer sehen keine `process`-Zeilen (H12).
/// Sind beide Listen leer, lautet die Ausgabe `No jobs.`.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Liefert den erforderlichen `Arc<JobStore>` und
///   optional `Arc<JobManager>` bzw. `Arc<harw_home::ResolvedHomeContext>`.
/// - `args` (`PsArgs`): Optionaler Statusfilter und optionale Art.
///
/// # Rückgabe
/// [`OpOutput`] mit einer Zeile pro durable Job und Hintergrundprozess.
///
/// # Fehler
/// Gibt [`OpError::InvalidArguments`] zurück, wenn `json_args` nicht in
/// [`PsArgs`] deserialisiert werden kann (wird vom Makro gehandhabt), und
/// [`OpError::Execution`], wenn das Job-Verzeichnis nicht lesbar ist.
///
/// # Nebenläufigkeit
/// Rein lesend; die Dateisperren und Snapshot-Semantik gehören dem [`JobStore`]
/// bzw. dem [`JobManager`].
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
    let (kind, status) = split_kind(args)?;
    let states = status.as_deref().map(parse_state).transpose()?;
    let store = ctx
        .service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))?;
    // H12: mandantengebundene Aufrufer sehen nur Jobs des eigenen Mandanten.
    let jobs = if kind == Some(Kind::Process) {
        Vec::new()
    } else {
        crate::job_tenant::list_visible_jobs(
            ctx,
            store,
            JobListQuery {
                states: states.map(|state| vec![state]),
                ..JobListQuery::default()
            },
        )
        .map_err(|error| OpError::Execution(format!("could not list durable jobs: {error}")))?
    };
    let mut processes = if kind == Some(Kind::Work) {
        Vec::new()
    } else {
        process_jobs(ctx)?
    };
    if let Some(filter) = states {
        processes.retain(|meta| process_matches(filter, meta.state));
    }
    processes.sort_by(|left, right| left.job_id.cmp(&right.job_id));
    if jobs.is_empty() && processes.is_empty() {
        return Ok(OpOutput::from("No jobs.".to_owned()));
    }
    let Some(kind) = kind else {
        return Ok(OpOutput::from(summary(&jobs, &processes)));
    };
    let text = match kind {
        Kind::Work => jobs
            .iter()
            .map(|record| {
                format!(
                    "{}\twork\t{:?}\towner -\tprofile -\tend -\trev {}",
                    record.job.id, record.job.state, record.revision
                )
            })
            .collect::<Vec<_>>(),
        // Nur ID, Zustand, Besitzer, Profil und Endgrund: Befehl, Name, cwd und
        // Umgebungsnamen bleiben draußen, Umgebungswerte kennt `meta.json` nicht.
        Kind::Process => processes
            .iter()
            .map(|meta| {
                format!(
                    "{}\tprocess\t{}\towner {}\tprofile {}\tend {}\trev -",
                    meta.job_id,
                    meta.state.as_str(),
                    meta.owner.session,
                    process_profile(meta),
                    process_end_reason(meta)
                )
            })
            .collect(),
    }
    .join("\n");
    Ok(OpOutput::from(text))
}

/// Übersicht ohne Art: je Art eine Zeile mit Zählern pro Zustand, danach der
/// Hinweis auf die Detailansicht.
///
/// Zustände erscheinen in der Reihenfolge ihres ersten Auftretens (Store-
/// Reihenfolge bzw. sortiert nach ID), damit die Ausgabe stabil bleibt.
fn summary(jobs: &[harw_job_runtime::StoredJob], processes: &[JobMeta]) -> String {
    let work = count_states(
        jobs.iter()
            .map(|record| format!("{:?}", record.job.state).to_ascii_lowercase()),
    );
    let process = count_states(processes.iter().map(|meta| meta.state.as_str().to_owned()));
    [
        format!("work: {work}"),
        format!("process: {process}"),
        "Details: /ps work|process [status]".to_owned(),
    ]
    .join("\n")
}

/// `2 running · 1 failed` oder `none` für eine leere Art.
fn count_states(states: impl Iterator<Item = String>) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for state in states {
        match counts.iter_mut().find(|(known, _)| *known == state) {
            Some((_, count)) => *count += 1,
            None => counts.push((state, 1)),
        }
    }
    if counts.is_empty() {
        return "none".to_owned();
    }
    counts
        .iter()
        .map(|(state, count)| format!("{count} {state}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Sammelt die `job.start`-Hintergrundprozesse für `/ps`.
///
/// # Beschreibung
/// Reihenfolge der Quellen: mandantengebundene Aufrufer bekommen keine
/// Einträge (Hintergrundprozesse tragen keinen Mandanten, H12, fail closed);
/// ein [`JobManager`] im Kontext liefert den Live-Zustand; sonst liest
/// [`read_process_jobs`] das Job-Verzeichnis des gebundenen Projekts. Ohne
/// beides bleibt die Liste leer.
fn process_jobs(ctx: &OpContext) -> Result<Vec<JobMeta>, OpError> {
    if ctx.tenant().is_some() {
        return Ok(Vec::new());
    }
    if let Some(manager) = ctx.service::<Arc<JobManager>>() {
        return Ok(manager
            .list(Caller::Operator)
            .into_iter()
            .map(|status| status.meta)
            .collect());
    }
    if let Some(home) = ctx.service::<Arc<harw_home::ResolvedHomeContext>>() {
        return read_process_jobs(&JobManagerConfig::new(home.project_home.state_dir()).jobs_dir());
    }
    Ok(Vec::new())
}

/// Liest `meta.json` aller Job-Verzeichnisse unter `jobs_dir`, rein lesend.
///
/// # Beschreibung
/// Legt nichts an und schreibt nichts (anders als `JobManager::new`). Ein
/// fehlendes Verzeichnis ergibt eine leere Liste. Übersprungen werden
/// Einträge, die kein Verzeichnis sind (Symlinks werden nicht verfolgt),
/// deren `meta.json` fehlt, größer als 1 MiB oder unlesbar ist, und solche,
/// deren Verzeichnisname nicht zur gespeicherten `job_id` passt (wie beim
/// Neuladen im Job-Manager).
///
/// # Fehler
/// [`OpError::Execution`], wenn `jobs_dir` existiert, aber nicht lesbar ist.
fn read_process_jobs(jobs_dir: &Path) -> Result<Vec<JobMeta>, OpError> {
    let entries = match std::fs::read_dir(jobs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(OpError::Execution(format!(
                "could not read background jobs: {error}"
            )));
        }
    };
    let mut metas = Vec::new();
    for entry in entries.flatten() {
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if !is_dir {
            continue;
        }
        let Some(meta) = read_meta_capped(&entry.path().join(META_FILE)) else {
            continue;
        };
        if entry.file_name().to_str() != Some(meta.job_id.as_str()) {
            continue;
        }
        metas.push(meta);
    }
    metas.sort_by(|left, right| left.job_id.cmp(&right.job_id));
    Ok(metas)
}

/// Liest und parst eine `meta.json` mit höchstens [`MAX_META_BYTES`] Bytes;
/// `None` bei jedem Lese- oder Parse-Fehler und für Nicht-Dateien (etwa eine
/// FIFO, deren Öffnen blockieren würde).
fn read_meta_capped(path: &Path) -> Option<JobMeta> {
    let is_file = std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file());
    if !is_file {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_META_BYTES).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice::<JobMeta>(&bytes).ok()
}

/// Sandbox-Profil eines Hintergrundprozesses (`host`/`bwrap`), aus
/// [`JobMeta::sandbox_profile`].
fn process_profile(meta: &JobMeta) -> &'static str {
    meta.sandbox_profile()
}

/// Endgrund eines Hintergrundprozesses aus [`JobMeta::end_reason`]; `-` für
/// nicht beendete Jobs und beendete ohne erkennbares Ergebnis.
fn process_end_reason(meta: &JobMeta) -> &'static str {
    meta.end_reason().map_or("-", JobEndReason::as_str)
}

/// Ob ein Hintergrundprozess im Zustand `state` zum Filter `filter` passt
/// (Zuordnung siehe [`PsArgs`]).
fn process_matches(filter: JobState, state: ProcessState) -> bool {
    match filter {
        JobState::Pending | JobState::Ready => state == ProcessState::Queued,
        JobState::Running => matches!(state, ProcessState::Running | ProcessState::Detached),
        JobState::Completed => state == ProcessState::Succeeded,
        JobState::Failed => matches!(state, ProcessState::Failed | ProcessState::Unknown),
        JobState::Cancelled => state == ProcessState::Stopped,
        JobState::Blocked => false,
    }
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
    use super::{PsArgs, process_end_reason, process_profile, ps, read_process_jobs};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_job_runtime::{Budget, Job, JobKind, JobScope, RetryPolicy, StoredJob};
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_session_store::JobStore;
    use harw_tool_job::JobMeta;
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
        let mut services = ServiceMap::new();
        if let Some(store) = store {
            services.insert(store);
        }
        test_context_with(services)
    }

    fn test_context_with(services: ServiceMap) -> TestResult<(OpContext, PathBuf)> {
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
                kind: Some("work".to_owned()),
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
        let output = ps(&ctx, PsArgs { kind: Some("work".to_owned()), ..PsArgs::default() })
            .await
            .map_err(crate::test_support::ctx("ps"))?;
        assert!(
            output
                .text
                .contains("job-pending\twork\tPending\towner -\tprofile -\tend -\trev 0"),
            "{}",
            output.text
        );
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
                kind: Some("work".to_owned()),
            },
        )
        .await
        .map_err(crate::test_support::ctx("ps"))?;
        assert_eq!(
            output.text,
            "job-ready\twork\tReady\towner -\tprofile -\tend -\trev 0"
        );
        Ok(())
    }

    // H12: Mandanten-Filter.
    #[tokio::test]
    async fn ps_unscoped_caller_sees_all_tenants() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, JOB_B, context, two_tenants};
        let jobs = two_tenants(harw_job_runtime::JobState::Ready)?;
        let op_ctx = context(&jobs, None)?;
        let output = ps(&op_ctx, PsArgs { kind: Some("work".to_owned()), ..PsArgs::default() }).await.map_err(ctx("ps"))?;
        assert!(output.text.contains(JOB_A), "{}", output.text);
        assert!(output.text.contains(JOB_B), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_scoped_caller_sees_only_own_tenant() -> TestResult {
        use crate::job_tenant::fixtures::{JOB_A, JOB_B, TENANT_A, context, two_tenants};
        let jobs = two_tenants(harw_job_runtime::JobState::Ready)?;
        let op_ctx = context(&jobs, Some(TENANT_A))?;
        let output = ps(&op_ctx, PsArgs { kind: Some("work".to_owned()), ..PsArgs::default() }).await.map_err(ctx("ps"))?;
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

    // Hintergrundprozesse (`job.start`) in derselben Liste.

    /// Gebundener Home-Kontext in einem Temp-Verzeichnis; das `TempDir` muss
    /// für die Testdauer leben.
    fn home_fixture() -> TestResult<(tempfile::TempDir, Arc<harw_home::ResolvedHomeContext>)> {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let project_dir = home.path().join("project");
        let project = harw_home::ProjectRoot {
            root: project_dir.clone(),
            trust_key: project_dir,
            kind: harw_home::ProjectKind::Directory,
        };
        let context = Arc::new(
            harw_home::ResolvedHomeContext::new(home.path(), "default".to_owned(), project)
                .map_err(ctx("ResolvedHomeContext::new"))?,
        );
        Ok((home, context))
    }

    /// `meta.json` als JSON (nicht als Struct-Literal), damit neue
    /// `serde(default)`-Felder von `JobMeta` die Fixtures nicht brechen.
    /// Befehl, Name, cwd und Umgebungsnamen tragen Sentinels, die nie in der
    /// Ausgabe erscheinen dürfen.
    fn meta_json(id: &str, state: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut meta = serde_json::json!({
            "version": 2,
            "job_id": id,
            "name": "SENTINEL_NAME",
            "command": "echo SENTINEL_CMD",
            "cwd": "/SENTINEL_CWD",
            "env_keys": ["SENTINEL_ENV"],
            "state": state,
            "harw_instance": "harw-test",
            "owner": {"session": "agent-x"},
            "created_at": "2026-01-01T00:00:00Z",
            "notify_every_secs": 60,
        });
        if let (Some(target), serde_json::Value::Object(fields)) = (meta.as_object_mut(), extra) {
            for (key, value) in fields {
                target.insert(key, value);
            }
        }
        meta
    }

    fn write_job_file(
        home: &harw_home::ResolvedHomeContext,
        dir_name: &str,
        bytes: &[u8],
    ) -> TestResult {
        let dir = home.project_home.state_dir().join("jobs").join(dir_name);
        std::fs::create_dir_all(&dir).map_err(ctx("create job dir"))?;
        std::fs::write(dir.join("meta.json"), bytes).map_err(ctx("write meta.json"))?;
        Ok(())
    }

    fn write_meta(
        home: &harw_home::ResolvedHomeContext,
        dir_name: &str,
        meta: &serde_json::Value,
    ) -> TestResult {
        let bytes = serde_json::to_vec(meta).map_err(ctx("serialize meta.json"))?;
        write_job_file(home, dir_name, &bytes)
    }

    /// Ein `succeeded`-Hostprozess `job-proc-1` des Agenten `agent-x`.
    fn write_succeeded_process(home: &harw_home::ResolvedHomeContext) -> TestResult {
        write_meta(
            home,
            "job-proc-1",
            &meta_json(
                "job-proc-1",
                "succeeded",
                serde_json::json!({"exit_code": 0, "executed_on_host": true}),
            ),
        )
    }

    fn work_store() -> TestResult<Arc<JobStore>> {
        let store = Arc::new(JobStore::new(&unique_root("harw-ps-mixed")));
        store
            .admit(&stored_job(
                "job-pending",
                harw_job_runtime::JobState::Pending,
            )?)
            .map_err(ctx("admit job-pending"))?;
        Ok(store)
    }

    fn mixed_context(
        home: &Arc<harw_home::ResolvedHomeContext>,
        tenant: Option<&str>,
    ) -> TestResult<OpContext> {
        let mut services = ServiceMap::new();
        services.insert(work_store()?);
        services.insert(Arc::clone(home));
        let (op_ctx, _root) = test_context_with(services)?;
        Ok(match tenant {
            Some(tenant) => op_ctx.with_tenant(TenantId::from_str(tenant)),
            None => op_ctx,
        })
    }

    const PROCESS_ROW: &str =
        "job-proc-1\tprocess\tsucceeded\towner agent-x\tprofile host\tend exited\trev -";

    fn with_kind(kind: &str) -> PsArgs {
        PsArgs {
            kind: Some(kind.to_owned()),
            ..PsArgs::default()
        }
    }

    #[tokio::test]
    async fn ps_without_kind_shows_only_category_summary() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        let output = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert_eq!(
            output.text,
            "work: 1 pending\nprocess: 1 succeeded\nDetails: /ps work|process [status]"
        );
        assert!(!output.text.contains("job-pending"), "{}", output.text);
        assert!(!output.text.contains("job-proc-1"), "{}", output.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_summary_marks_empty_kind_as_none() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        let op_ctx = mixed_context(&home, None)?;
        let output = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert_eq!(
            output.text,
            "work: 1 pending\nprocess: none\nDetails: /ps work|process [status]"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ps_lists_work_and_process_jobs_with_kind() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        let work = ps(&op_ctx, with_kind("work")).await.map_err(ctx("ps work"))?;
        assert_eq!(
            work.text,
            "job-pending\twork\tPending\towner -\tprofile -\tend -\trev 0"
        );
        let process = ps(&op_ctx, with_kind("process"))
            .await
            .map_err(ctx("ps process"))?;
        assert_eq!(process.text, PROCESS_ROW);
        Ok(())
    }

    #[tokio::test]
    async fn ps_raw_args_accept_kind_first_or_second() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        for tokens in [["process", "finished"], ["finished", "process"]] {
            let args = PsArgs::from_raw_args(&toks(&tokens))
                .map_err(|error| TestError::Unexpected(format!("{error}")))?;
            let output = ps(&op_ctx, args).await.map_err(ctx("ps raw"))?;
            assert_eq!(output.text, PROCESS_ROW, "{tokens:?}");
        }
        let args = PsArgs::from_raw_args(&toks(&["process", "running"]))
            .map_err(|error| TestError::Unexpected(format!("{error}")))?;
        let output = ps(&op_ctx, args).await.map_err(ctx("ps running"))?;
        assert_eq!(output.text, "No jobs.");
        Ok(())
    }

    #[tokio::test]
    async fn ps_json_args_carry_kind_and_status() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        let args: PsArgs =
            serde_json::from_value(serde_json::json!({"kind": "work", "status": "pending"}))
                .map_err(ctx("parse json args"))?;
        let output = ps(&op_ctx, args).await.map_err(ctx("ps json"))?;
        assert_eq!(
            output.text,
            "job-pending\twork\tPending\towner -\tprofile -\tend -\trev 0"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ps_rejects_unknown_or_double_kind() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        let op_ctx = mixed_context(&home, None)?;
        let unknown = ps(&op_ctx, with_kind("threads")).await;
        assert!(matches!(unknown, Err(OpError::InvalidArguments(_))));
        let double = ps(
            &op_ctx,
            PsArgs {
                status: Some("work".to_owned()),
                kind: Some("process".to_owned()),
            },
        )
        .await;
        assert!(matches!(double, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn ps_process_rows_hidden_for_tenant_scoped_caller() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, Some("test-tenant"))?;
        let process = ps(&op_ctx, with_kind("process"))
            .await
            .map_err(ctx("ps process"))?;
        assert_eq!(process.text, "No jobs.");
        let work = ps(&op_ctx, with_kind("work")).await.map_err(ctx("ps work"))?;
        assert!(work.text.contains("job-pending\twork\t"), "{}", work.text);
        let summary = ps(&op_ctx, PsArgs::default()).await.map_err(ctx("ps"))?;
        assert!(summary.text.contains("process: none"), "{}", summary.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_process_filter_maps_states() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        let running = ps(
            &op_ctx,
            PsArgs {
                status: Some("running".to_owned()),
                kind: Some("process".to_owned()),
            },
        )
        .await
        .map_err(ctx("ps running"))?;
        assert!(!running.text.contains("job-proc-1"), "{}", running.text);
        let finished = ps(
            &op_ctx,
            PsArgs {
                status: Some("finished".to_owned()),
                kind: Some("process".to_owned()),
            },
        )
        .await
        .map_err(ctx("ps finished"))?;
        assert!(finished.text.contains(PROCESS_ROW), "{}", finished.text);
        Ok(())
    }

    #[tokio::test]
    async fn ps_skips_mismatched_or_unreadable_meta() -> TestResult {
        // Fehlendes Job-Verzeichnis: leer, kein Fehler, nichts angelegt.
        let (_home_dir, home) = home_fixture()?;
        let jobs_dir = home.project_home.state_dir().join("jobs");
        let missing = read_process_jobs(&jobs_dir).map_err(ctx("read missing dir"))?;
        assert!(missing.is_empty());
        assert!(!jobs_dir.exists());
        let op_ctx = mixed_context(&home, None)?;
        let output = ps(&op_ctx, PsArgs { kind: Some("process".to_owned()), ..PsArgs::default() }).await.map_err(ctx("ps"))?;
        assert!(!output.text.contains("\tprocess\t"), "{}", output.text);

        // Verzeichnisname passt nicht zur job_id bzw. ungültiges JSON.
        write_meta(&home, "job-x", &meta_json("job-y", "running", serde_json::json!({})))?;
        write_job_file(&home, "job-bad", b"{ not json")?;
        let listed = read_process_jobs(&jobs_dir).map_err(ctx("read jobs dir"))?;
        assert!(listed.is_empty(), "{listed:?}");
        let output = ps(&op_ctx, PsArgs { kind: Some("process".to_owned()), ..PsArgs::default() }).await.map_err(ctx("ps"))?;
        for absent in ["job-x", "job-y", "job-bad", "\tprocess\t"] {
            assert!(!output.text.contains(absent), "{absent}: {}", output.text);
        }
        Ok(())
    }

    #[test]
    fn process_end_reason_and_profile_cover_all_cases() -> TestResult {
        let cases = [
            (
                "failed",
                serde_json::json!({"launch_error": "spawn failed", "exit_code": 1}),
                "launch-error",
            ),
            ("stopped", serde_json::json!({"signal": 15}), "stopped"),
            ("failed", serde_json::json!({"stop_requested": true}), "stopped"),
            ("failed", serde_json::json!({"signal": 9}), "signal"),
            ("failed", serde_json::json!({"signal": 24}), "timeout"),
            ("failed", serde_json::json!({"exit_code": 2}), "exited"),
            ("succeeded", serde_json::json!({"exit_code": 0}), "exited"),
            ("unknown", serde_json::json!({}), "unknown"),
            ("running", serde_json::json!({"exit_code": 0}), "-"),
            ("detached", serde_json::json!({}), "-"),
        ];
        for (state, extra, expected) in cases {
            let meta: JobMeta = serde_json::from_value(meta_json("job-1", state, extra))
                .map_err(ctx("parse meta"))?;
            assert_eq!(process_end_reason(&meta), expected, "state {state}");
        }
        let bwrap: JobMeta =
            serde_json::from_value(meta_json("job-1", "running", serde_json::json!({})))
                .map_err(ctx("parse bwrap meta"))?;
        assert_eq!(process_profile(&bwrap), "bwrap");
        let host: JobMeta = serde_json::from_value(meta_json(
            "job-1",
            "running",
            serde_json::json!({"executed_on_host": true}),
        ))
        .map_err(ctx("parse host meta"))?;
        assert_eq!(process_profile(&host), "host");
        Ok(())
    }

    #[tokio::test]
    async fn ps_process_rows_never_contain_command() -> TestResult {
        let (_home_dir, home) = home_fixture()?;
        write_succeeded_process(&home)?;
        let op_ctx = mixed_context(&home, None)?;
        let output = ps(&op_ctx, PsArgs { kind: Some("process".to_owned()), ..PsArgs::default() }).await.map_err(ctx("ps"))?;
        assert!(output.text.contains("job-proc-1"), "{}", output.text);
        for sentinel in [
            "SENTINEL_CMD",
            "SENTINEL_NAME",
            "SENTINEL_CWD",
            "SENTINEL_ENV",
        ] {
            assert!(!output.text.contains(sentinel), "{sentinel}: {}", output.text);
        }
        Ok(())
    }
}

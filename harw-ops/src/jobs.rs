//! `/jobs` — Hintergrund-Jobs der Sitzung ansehen, stoppen und ihre Logs
//! lesen (Plan R9, Teil F; `harw-tool-job`).
//!
//! # Unterbefehle
//! - `/jobs` bzw. `/jobs list` — alle Jobs der Sitzung (auch frühere,
//!   abgelöste), eine Zeile je Job: Kennung, Zustand, Laufzeit, Fortschritt,
//!   Name.
//! - `/jobs show <id>` — Zustand, Befehl, Besitzer, Fortschritt,
//!   Fehler-/Warnzahlen und die letzten Ausgabezeilen.
//! - `/jobs stop <id> [TERM|INT|HUP|KILL]` — Signal an die ganze
//!   Prozessgruppe, nach der Gnadenfrist SIGKILL.
//! - `/jobs logs <id> [n]` — die letzten `n` Zeilen (Vorgabe 40, höchstens
//!   400) von stdout und stderr sowie der Pfad der Logdateien.
//! - `/jobs work` — die durablen Arbeitsaufträge des Job-Stores (auch
//!   `memory_maintenance`) mit Art, Zustand, `end` (Disposition:
//!   `succeeded`, `failed`, `timed_out`, `cancelled`), Frist und Grund;
//!   Abbruch über `/stop <id>`.
//! - `/jobs wait <id> [sekunden]` — wartet auf das Ende eines durablen Jobs
//!   (Vorgabe 30 s, höchstens 300 s) und zeigt dann dieselbe Zeile; kehrt
//!   nach Ablauf der Wartezeit mit dem aktuellen Stand zurück.
//! - `/jobs permits [lane] [n]` — variable Parallelität: ohne Argumente die
//!   Lanes (`work_driver`, `memory` des Job-Workers; `process` der
//!   Prozess-Jobs) mit Limit und belegten Permits, mit `lane n` setzt es das
//!   Limit zur Laufzeit (Erhöhen startet wartende Jobs sofort, Senken stoppt
//!   keinen laufenden Job). Der Worker übernimmt Änderungen beim nächsten
//!   Poll (`lane-limits.json` im Job-Store). Vorgabe aus `[jobs] max_running`.
//!
//! # Rechte
//! Nur Kommando-Fläche (`tui_only`, kein Modell-Werkzeug): die getippte
//! Eingabe handelt als Bedienerin ([`Caller::Operator`]) über alle Jobs der
//! Sitzung. Modelle erreichen Jobs ausschließlich über `job.*` mit
//! Besitzprüfung. Die Verwaltung liegt als `Arc<JobManager>` nur in der
//! Slash-`ServiceMap` der TUI-Montage; sonst meldet die Operation
//! [`OpError::NotAvailable`].
//!
//! # Nebenläufigkeit
//! `stop` wartet höchstens die Gnadenfrist der Verwaltung plus die
//! SIGKILL-Wartezeit; alle anderen Unterbefehle lesen nur.

use std::sync::Arc;

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_job_runtime::WorkId;
use harw_session_store::{JobListQuery, JobStore};
use harw_tool_job::logs::tail_of_file;
use harw_tool_job::model::{STDERR_LOG, STDOUT_LOG};
use harw_tool_job::{Caller, JobId, JobManager, JobSignal, JobState, JobStatus, format_duration};

/// Vorgabe für `/jobs logs <id>` ohne Zeilenzahl.
const DEFAULT_LOG_LINES: usize = 40;
/// Höchstzahl Zeilen je Stream für `/jobs logs`.
const MAX_LOG_LINES: usize = 400;
/// Meldung ohne Job-Verwaltung.
const NOT_AVAILABLE: &str = "Hintergrund-Jobs gibt es nur in der interaktiven TUI-Sitzung";

/// Argument-Container für `/jobs`: rohe Tokens.
#[derive(Debug, Default, serde::Deserialize)]
pub struct JobsArgs {
    /// Alle Tokens nach `/jobs`.
    #[serde(default)]
    pub tokens: Vec<String>,
}

impl FromRawArgs for JobsArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            tokens: tokens.to_vec(),
        })
    }
}

/// Kurzform des Zustands für Listen und das Agenten-Panel.
#[must_use]
pub fn state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "wartet",
        JobState::Running => "läuft",
        JobState::Succeeded => "fertig",
        JobState::Failed => "fehlgeschlagen",
        JobState::Stopped => "gestoppt",
        JobState::Detached => "abgelöst",
        JobState::Unknown => "unbekannt",
    }
}

/// Erkannter Fortschritt als kurzer Text (`ninja 120/900 13%`), sonst `—`.
#[must_use]
pub fn progress_label(status: &JobStatus) -> String {
    status
        .meta
        .progress
        .as_ref()
        .map_or_else(|| "—".to_owned(), harw_tool_job::ProgressSnapshot::render)
}

/// Laufzeit als kurzer Text (`1m15s`), `—` ohne Start.
#[must_use]
pub fn runtime_label(status: &JobStatus) -> String {
    status
        .runtime_secs
        .map_or_else(|| "—".to_owned(), format_duration)
}

/// Eine Listenzeile: `job-… · läuft · 1m15s · ninja 120/900 · Name`.
#[must_use]
pub fn job_line(status: &JobStatus) -> String {
    format!(
        "{} · {} · {} · {} · {}",
        status.meta.job_id,
        state_label(status.meta.state),
        runtime_label(status),
        progress_label(status),
        status.meta.name
    )
}

/// Führt `/jobs` aus (siehe Moduldoku).
///
/// # Fehler
/// - [`OpError::NotAvailable`] ohne Job-Verwaltung.
/// - [`OpError::InvalidArguments`] bei unbekanntem Unterbefehl, fehlender
///   bzw. ungültiger Kennung oder ungültigem Signal.
/// - [`OpError::Execution`] für einen unbekannten Job.
#[operation(
    name = "jobs",
    summary = "Hintergrund-Jobs der Sitzung: list, show <id>, stop <id> [signal], logs <id> [n].",
    domain = "execution",
    permission = "operator",
    command(path = "/jobs", visibility = "tui_only", busy = "immediate")
)]
async fn jobs(ctx: &OpContext, args: JobsArgs) -> Result<OpOutput, OpError> {
    let tokens = args.tokens;
    // Durable Arbeitsaufträge und Permits brauchen keine Prozess-Verwaltung.
    match tokens.first().map(String::as_str) {
        Some("work") => return work_text(ctx),
        Some("wait") => return wait_text(ctx, &tokens).await,
        Some("permits" | "limits") => return permits_text(ctx, &tokens[1..]),
        _ => {}
    }
    let manager = ctx
        .service::<Arc<JobManager>>()
        .ok_or_else(|| OpError::NotAvailable(NOT_AVAILABLE.to_owned()))?;
    match tokens.first().map(String::as_str) {
        None | Some("list" | "ls") => Ok(OpOutput::from(list_text(manager))),
        Some("show") => {
            let id = job_id_arg(&tokens, "show")?;
            let status = status_of(manager, &id)?;
            Ok(OpOutput::from(show_text(&status)))
        }
        Some("stop") => {
            let id = job_id_arg(&tokens, "stop")?;
            let signal = match tokens.get(2) {
                None => JobSignal::Term,
                Some(raw) => JobSignal::parse(raw).ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "unbekanntes Signal `{raw}` (TERM, INT, HUP oder KILL)"
                    ))
                })?,
            };
            let status = manager
                .stop(&id, Caller::Operator, signal)
                .await
                .map_err(|error| OpError::Execution(error.to_string()))?;
            tracing::info!(job_id = %id, signal = signal.name(), "ops.jobs.stop");
            Ok(OpOutput::from(format!(
                "{} an Job {id} gesendet: {}",
                signal.name(),
                job_line(&status)
            )))
        }
        Some("logs" | "log") => {
            let id = job_id_arg(&tokens, "logs")?;
            let lines = match tokens.get(2) {
                None => DEFAULT_LOG_LINES,
                Some(raw) => raw
                    .parse::<usize>()
                    .ok()
                    .filter(|lines| *lines > 0)
                    .ok_or_else(|| {
                        OpError::InvalidArguments(format!("ungültige Zeilenzahl `{raw}`"))
                    })?
                    .min(MAX_LOG_LINES),
            };
            let status = status_of(manager, &id)?;
            Ok(OpOutput::from(logs_text(&status, lines)))
        }
        Some(other) => Err(OpError::InvalidArguments(format!(
            "unbekannter Unterbefehl `{other}` — /jobs [list|show <id>|stop <id> [signal]|logs <id> [n]|work|wait <id> [s]|permits [lane] [n]]"
        ))),
    }
}

/// Obergrenze der Wartezeit von `/jobs wait` (Sekunden).
const MAX_WAIT_SECS: u64 = 300;
/// Vorgabe der Wartezeit von `/jobs wait` (Sekunden).
const DEFAULT_WAIT_SECS: u64 = 30;

fn job_store(ctx: &OpContext) -> Result<&Arc<JobStore>, OpError> {
    ctx.service::<Arc<JobStore>>()
        .ok_or_else(|| OpError::NotAvailable("durable job store is not configured".to_owned()))
}

/// `/jobs work`: die sichtbaren durablen Jobs, eine Zeile je Job
/// (siehe [`crate::ps::work_row`]).
fn work_text(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let store = job_store(ctx)?;
    let records = crate::job_tenant::list_visible_jobs(ctx, store, JobListQuery::default())
        .map_err(|error| OpError::Execution(format!("could not list durable jobs: {error}")))?;
    if records.is_empty() {
        return Ok(OpOutput::from("No durable jobs.".to_owned()));
    }
    Ok(OpOutput::from(
        records
            .iter()
            .map(crate::ps::work_row)
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

/// `/jobs wait <id> [s]`: pollt den Store, bis die Disposition endgültig ist
/// oder die Wartezeit abläuft.
async fn wait_text(ctx: &OpContext, tokens: &[String]) -> Result<OpOutput, OpError> {
    let raw_id = tokens
        .get(1)
        .ok_or_else(|| OpError::InvalidArguments("/jobs wait <job-id> [sekunden]".to_owned()))?;
    let secs = match tokens.get(2) {
        None => DEFAULT_WAIT_SECS,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|secs| *secs > 0)
            .ok_or_else(|| OpError::InvalidArguments(format!("ungültige Wartezeit `{raw}`")))?
            .min(MAX_WAIT_SECS),
    };
    let store = job_store(ctx)?;
    let work_id = WorkId::from_str(raw_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let record = crate::job_tenant::get_visible_job(ctx, store, &work_id)
            .map_err(|error| OpError::Execution(format!("job `{raw_id}`: {error}")))?;
        if record.disposition().is_final() || std::time::Instant::now() >= deadline {
            return Ok(OpOutput::from(crate::ps::work_row(&record)));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// `/jobs permits [lane] [n]`.
fn permits_text(ctx: &OpContext, args: &[String]) -> Result<OpOutput, OpError> {
    let manager = ctx.service::<Arc<JobManager>>();
    let store = ctx.service::<Arc<JobStore>>();
    if manager.is_none() && store.is_none() {
        return Err(OpError::NotAvailable(NOT_AVAILABLE.to_owned()));
    }
    if let Some(lane) = args.first() {
        let raw = args.get(1).ok_or_else(|| {
            OpError::InvalidArguments("/jobs permits <lane> <n> (n >= 1)".to_owned())
        })?;
        let wanted = raw
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| OpError::InvalidArguments(format!("ungültiges Limit `{raw}` (>= 1)")))?;
        if lane == "process" {
            let manager = manager.ok_or_else(|| OpError::NotAvailable(NOT_AVAILABLE.to_owned()))?;
            let applied = manager.set_max_running(wanted);
            return Ok(OpOutput::from(format!(
                "process: Limit {applied} (sofort wirksam; laufende Jobs bleiben)"
            )));
        }
        let store = store.ok_or_else(|| {
            OpError::NotAvailable("durable job store is not configured".to_owned())
        })?;
        let applied = harw_job_runtime::lanes::write_limit(store.root(), lane, wanted)
            .map_err(|error| OpError::InvalidArguments(error.to_string()))?;
        return Ok(OpOutput::from(format!(
            "{lane}: Limit {applied} vermerkt; der Job-Worker übernimmt es beim nächsten Poll \
             (laufende Jobs bleiben, Erhöhen startet wartende sofort)"
        )));
    }
    let mut lines = Vec::new();
    if let Some(store) = store {
        let published = harw_job_runtime::lanes::read_status(store.root());
        let overrides = harw_job_runtime::lanes::read_limits(store.root());
        if published.is_empty() {
            lines.push("work_driver, memory: kein laufender Job-Worker gemeldet".to_owned());
        }
        for status in &published {
            let pending = overrides
                .get(&status.lane)
                .filter(|wanted| **wanted != status.limit)
                .map_or(String::new(), |wanted| format!(" (angefordert {wanted})"));
            lines.push(format!(
                "{}: Limit {}{pending}, belegt {}, frei {}",
                status.lane, status.limit, status.in_use, status.available
            ));
        }
    }
    if let Some(manager) = manager {
        lines.push(format!(
            "process: Limit {}, belegt {}",
            manager.max_running(),
            manager.running_count()
        ));
    }
    Ok(OpOutput::from(lines.join("\n")))
}

fn job_id_arg(tokens: &[String], sub: &str) -> Result<JobId, OpError> {
    let raw = tokens
        .get(1)
        .ok_or_else(|| OpError::InvalidArguments(format!("/jobs {sub} <job-id>")))?;
    JobId::parse(raw)
        .ok_or_else(|| OpError::InvalidArguments(format!("ungültige Job-Kennung `{raw}`")))
}

fn status_of(manager: &JobManager, id: &JobId) -> Result<JobStatus, OpError> {
    manager
        .status(id, Caller::Operator)
        .map_err(|error| OpError::Execution(error.to_string()))
}

fn list_text(manager: &JobManager) -> String {
    let jobs = manager.list(Caller::Operator);
    if jobs.is_empty() {
        return "Keine Jobs. Agenten starten lang laufende Prozesse mit job.start.".to_owned();
    }
    let running = jobs
        .iter()
        .filter(|status| !status.meta.state.is_terminal())
        .count();
    let mut text = format!("Jobs: {} ({running} aktiv)", jobs.len());
    for status in &jobs {
        text.push_str("\n  ");
        text.push_str(&job_line(status));
    }
    text.push_str("\n/jobs show <id> · /jobs logs <id> · /jobs stop <id>");
    text
}

fn show_text(status: &JobStatus) -> String {
    let meta = &status.meta;
    let mut lines = vec![
        format!("Job {} „{}“", meta.job_id, meta.name),
        format!(
            "Zustand: {} · Laufzeit: {}{}",
            state_label(meta.state),
            runtime_label(status),
            meta.exit_code
                .map(|code| format!(" · Exit-Code {code}"))
                .unwrap_or_default()
        ),
        format!("Befehl: {}", meta.command),
        format!(
            "Ort: {} · PID: {}",
            if meta.executed_on_host {
                "Host"
            } else {
                "Sandbox"
            },
            meta.pid
                .map_or_else(|| "—".to_owned(), |pid| pid.to_string())
        ),
        format!("Besitzer: {}", meta.owner.session),
        format!(
            "Fortschritt: {} · {} Warnungen · {} Fehler · {} Zeilen",
            progress_label(status),
            meta.warnings,
            meta.errors,
            status.stdout_lines + status.stderr_lines
        ),
        format!("Logs: {}", status.log_dir.display()),
    ];
    if let Some(error) = &meta.launch_error {
        lines.push(format!("Startfehler: {error}"));
    }
    if !status.last_lines.is_empty() {
        lines.push("Letzte Zeilen:".to_owned());
        lines.extend(status.last_lines.iter().map(|line| format!("  {line}")));
    }
    lines.join("\n")
}

fn logs_text(status: &JobStatus, lines: usize) -> String {
    let mut text = format!(
        "Logs von {} „{}“ ({})",
        status.meta.job_id,
        status.meta.name,
        status.log_dir.display()
    );
    for (label, file) in [("stdout", STDOUT_LOG), ("stderr", STDERR_LOG)] {
        let tail = tail_of_file(&status.log_dir.join(file), lines);
        text.push_str(&format!("\n── {label} (letzte {} Zeilen) ──", tail.len()));
        for line in tail {
            text.push('\n');
            text.push_str(&line);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::ServiceMap;
    use harw_tool_job::{JobOwner, NoopNotifier, PreparedJob, StartRequest};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::time::Duration;

    fn op_context(
        dir: &std::path::Path,
        manager: Option<Arc<JobManager>>,
    ) -> TestResult<OpContext> {
        op_context_with_store(dir, manager, None)
    }

    fn op_context_with_store(
        dir: &std::path::Path,
        manager: Option<Arc<JobManager>>,
        store: Option<Arc<JobStore>>,
    ) -> TestResult<OpContext> {
        std::fs::create_dir_all(dir.join("ws")).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            dir,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("ws"),
                root: "ws".into(),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("ws"))
            .map_err(ctx("resolve"))?;
        let mut services = ServiceMap::new();
        if let Some(manager) = manager {
            services.insert(manager);
        }
        if let Some(store) = store {
            services.insert(store);
        }
        Ok(OpContext::new(
            SessionId::new(),
            TurnId::new(),
            SandboxSpec::from_resolved(binding, PermissionSet::empty()),
            services,
        ))
    }

    fn sleeper() -> PreparedJob {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("echo hallo; sleep 30")
            .stdin(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(false);
        PreparedJob {
            command,
            executed_on_host: true,
        }
    }

    async fn run(op_ctx: &OpContext, tokens: &[&str]) -> Result<OpOutput, OpError> {
        jobs(
            op_ctx,
            JobsArgs {
                tokens: toks(tokens),
            },
        )
        .await
    }

    #[tokio::test]
    async fn jobs_without_manager_is_not_available() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let op_ctx = op_context(dir.path(), None)?;
        assert!(matches!(
            run(&op_ctx, &[]).await,
            Err(OpError::NotAvailable(_))
        ));
        Ok(())
    }

    /// Plan R9, Teil F: `/jobs` listet, zeigt, liest Logs und stoppt einen
    /// echten Job als Bedienerin (fremder Besitzer).
    #[tokio::test]
    async fn jobs_list_show_logs_and_stop() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let manager = JobManager::new(
            harw_tool_job::JobManagerConfig::new(dir.path().join("state")),
            Arc::new(NoopNotifier),
        )
        .map_err(ctx("manager"))?;
        let op_ctx = op_context(dir.path(), Some(Arc::clone(&manager)))?;
        assert!(
            run(&op_ctx, &[])
                .await
                .map_err(ctx("leer"))?
                .text
                .contains("Keine Jobs")
        );

        let status = manager
            .start(
                StartRequest {
                    name: "schlaf".to_owned(),
                    command: "echo hallo; sleep 30".to_owned(),
                    cwd: None,
                    env_keys: Vec::new(),
                    notify_every: Duration::ZERO,
                    owner: JobOwner::new("some-agent", Vec::new()),
                },
                sleeper(),
            )
            .map_err(ctx("start"))?;
        let id = status.meta.job_id.to_string();

        let list = run(&op_ctx, &["list"]).await.map_err(ctx("list"))?;
        assert!(list.text.contains(&id), "{}", list.text);
        assert!(list.text.contains("läuft"), "{}", list.text);
        let show = run(&op_ctx, &["show", &id]).await.map_err(ctx("show"))?;
        assert!(show.text.contains("Besitzer: some-agent"), "{}", show.text);

        // Die Ausgabe erscheint im Log.
        let mut seen = false;
        for _ in 0..50 {
            let logs = run(&op_ctx, &["logs", &id, "5"])
                .await
                .map_err(ctx("logs"))?;
            if logs.text.contains("hallo") {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(seen, "stdout erscheint in /jobs logs");

        let stopped = run(&op_ctx, &["stop", &id, "KILL"])
            .await
            .map_err(ctx("stop"))?;
        assert!(stopped.text.contains("SIGKILL"), "{}", stopped.text);
        let after = manager
            .status(&status.meta.job_id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert!(after.meta.state.is_terminal(), "{:?}", after.meta.state);
        assert_eq!(manager.running_count(), 0);

        assert!(matches!(
            run(&op_ctx, &["stop", &id, "USR1"]).await,
            Err(OpError::InvalidArguments(_))
        ));
        assert!(matches!(
            run(&op_ctx, &["show", "job-unbekannt"]).await,
            Err(OpError::Execution(_))
        ));
        assert!(matches!(
            run(&op_ctx, &["frobnicate"]).await,
            Err(OpError::InvalidArguments(_))
        ));
        Ok(())
    }

    // -- durable Jobs: Art/Frist/Zustand/Grund, wait, permits ----------------

    fn memory_job(store: &JobStore, deadline: u64) -> TestResult<harw_job_runtime::WorkId> {
        let scope = harw_job_runtime::JobScope::new(
            TenantId::from_str("t"),
            WorkspaceId::from_str("ws"),
            harw_types::ApprovalActor::Operator {
                id: "tester".to_owned(),
            },
        );
        let spec = crate::memory_job::MemoryMaintenanceSpec::new(
            crate::memory_job::MemoryMaintenanceOp::Sweep,
            deadline,
        );
        crate::memory_job::admit_memory_maintenance(store, scope, &spec).map_err(ctx("admit"))
    }

    #[tokio::test]
    async fn work_rows_show_kind_deadline_state_and_a_timed_out_end() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("jobs")));
        let id = memory_job(&store, 45)?;
        let op_ctx = op_context_with_store(dir.path(), None, Some(Arc::clone(&store)))?;
        let ready = run(&op_ctx, &["work"]).await.map_err(ctx("work"))?;
        assert!(ready.text.contains("\tkind memory_maintenance"), "{}", ready.text);
        assert!(ready.text.contains("\tdeadline 45s"), "{}", ready.text);
        assert!(ready.text.contains("\tReady\t"), "{}", ready.text);
        assert!(ready.text.contains("end -"), "{}", ready.text);

        let claim = store
            .claim(
                &id,
                &harw_session_store::ClaimRequest {
                    worker_id: "w".to_owned(),
                    lease_ttl: jiff::SignedDuration::from_secs(60),
                    now: jiff::Timestamp::now(),
                },
            )
            .map_err(ctx("claim"))?;
        store
            .complete(
                &id,
                &harw_session_store::CompleteRequest {
                    token: claim.token,
                    completed_at: jiff::Timestamp::now(),
                    outcome: harw_job_runtime::JobOutcome::timed_out("phase 'x'"),
                },
            )
            .map_err(ctx("complete"))?;
        let done = run(&op_ctx, &["wait", id.as_str(), "5"])
            .await
            .map_err(ctx("wait"))?;
        assert!(done.text.contains("\tFailed\t"), "{}", done.text);
        assert!(done.text.contains("end timed_out"), "{}", done.text);
        assert!(done.text.contains("reason timed_out: phase 'x'"), "{}", done.text);
        Ok(())
    }

    #[tokio::test]
    async fn wait_returns_the_current_row_when_the_time_is_up() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("jobs")));
        let id = memory_job(&store, 30)?;
        let op_ctx = op_context_with_store(dir.path(), None, Some(store))?;
        let out = run(&op_ctx, &["wait", id.as_str(), "1"]).await.map_err(ctx("wait"))?;
        assert!(out.text.contains("\tReady\t"), "{}", out.text);
        assert!(matches!(
            run(&op_ctx, &["wait", id.as_str(), "0"]).await,
            Err(OpError::InvalidArguments(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn permits_show_set_and_validate_lanes_and_the_process_cap() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(JobStore::new(&dir.path().join("jobs")));
        let manager = JobManager::new(
            harw_tool_job::JobManagerConfig {
                max_running_jobs: 4,
                ..harw_tool_job::JobManagerConfig::new(dir.path().join("state"))
            },
            Arc::new(NoopNotifier),
        )
        .map_err(ctx("manager"))?;
        let op_ctx =
            op_context_with_store(dir.path(), Some(Arc::clone(&manager)), Some(Arc::clone(&store)))?;

        let shown = run(&op_ctx, &["permits"]).await.map_err(ctx("permits"))?;
        assert!(shown.text.contains("kein laufender Job-Worker"), "{}", shown.text);
        assert!(shown.text.contains("process: Limit 4, belegt 0"), "{}", shown.text);

        // A worker publishes its status; an override is shown as requested.
        let lanes = harw_job_runtime::JobLanes::new(2, 1);
        lanes.publish_status(store.root()).map_err(ctx("publish"))?;
        run(&op_ctx, &["permits", "memory", "3"]).await.map_err(ctx("set"))?;
        let shown = run(&op_ctx, &["permits"]).await.map_err(ctx("permits"))?;
        assert!(shown.text.contains("work_driver: Limit 2, belegt 0, frei 2"), "{}", shown.text);
        assert!(shown.text.contains("memory: Limit 1 (angefordert 3)"), "{}", shown.text);
        assert_eq!(lanes.apply_overrides(store.root()), vec!["memory"]);

        run(&op_ctx, &["permits", "process", "7"]).await.map_err(ctx("process"))?;
        assert_eq!(manager.max_running(), 7);
        for bad in [
            &["permits", "nope", "2"][..],
            &["permits", "memory", "0"][..],
            &["permits", "memory"][..],
            &["permits", "memory", "x"][..],
        ] {
            assert!(
                matches!(run(&op_ctx, bad).await, Err(OpError::InvalidArguments(_))),
                "{bad:?}"
            );
        }
        Ok(())
    }
}

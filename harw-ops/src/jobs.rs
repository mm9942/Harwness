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
    let manager = ctx
        .service::<Arc<JobManager>>()
        .ok_or_else(|| OpError::NotAvailable(NOT_AVAILABLE.to_owned()))?;
    let tokens = args.tokens;
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
            "unbekannter Unterbefehl `{other}` — /jobs [list|show <id>|stop <id> [signal]|logs <id> [n]]"
        ))),
    }
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
}

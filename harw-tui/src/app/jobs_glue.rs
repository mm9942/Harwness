//! Plan R9, Teil F: Hintergrund-Jobs in der TUI.
//!
//! # Verantwortungsbereich
//! - Jobs-Gruppe des Agenten-Panels: [`refresh`] liest
//!   `JobManager::list(Caller::Operator)` im Takt ([`JOBS_REFRESH_INTERVAL`])
//!   bzw. sofort nach einem Job-Ereignis und hält eine offene
//!   Job-Detailansicht (Log-Ende) aktuell — Ansichten bauen beim Öffnen neu.
//! - Zustellung an die Wurzel: [`poll`] holt die Notizen, die kein lebendes
//!   Kind mehr annehmen konnte (`JobEventRouter::take_root_notes`), und reiht
//!   sie wie Hintergrund-Meldungen für den nächsten UIA-Turn ein (das Ende
//!   eines Jobs startet im Leerlauf einen Auto-Turn); Start, Ende und
//!   Hinweise erscheinen zusätzlich als Systemzeile.
//! - Beenden: [`before_quit`] fragt bei laufenden Jobs einmal nach („Jobs
//!   stoppen“ / „Weiterlaufen lassen“ / „Abbrechen“) und führt die Wahl aus
//!   (`stop_all` bzw. `detach_all`).
//!
//! # Nebenläufigkeit
//! Läuft auf dem Thread der Ereignisschleife; die Verwaltung ist
//! `Send + Sync` und wird nie über ein `await` gesperrt.

use std::sync::Arc;
use std::time::{Duration, Instant};

use harw_tool_job::{Caller, JobEvent, JobId, JobManager, JobState, JobStatus};
use harw_types::SessionId;

use super::background_agents::enqueue_background_notice;
use super::{AgentDetailState, ChatApp, Overlay, Role};
use crate::chat_scroll::ChatScroll;
use crate::choice_dialog::{ChoiceAction, ChoiceDialog};
use crate::jobs_panel::{JobDetail, JobRow};

/// Takt, in dem die Jobs-Gruppe (Laufzeit, Fortschritt) nachgeführt wird.
pub(crate) const JOBS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Wahl im Beenden-Dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobsQuitChoice {
    /// Alle laufenden Jobs stoppen (TERM, dann KILL).
    Stop,
    /// Laufende Jobs ablösen: sie laufen weiter und sind nach dem nächsten
    /// Start über ihre `meta.json` wieder sichtbar.
    Detach,
}

/// TUI-Zustand der Jobs (Feld `ChatApp::jobs_ui`).
#[derive(Debug, Default)]
pub(crate) struct JobsUi {
    /// Zeitpunkt des letzten Nachführens der Jobs-Gruppe.
    last_refresh: Option<Instant>,
    /// Im Beenden-Dialog getroffene Wahl (wird beim nächsten Beenden
    /// eingelöst).
    pub(crate) quit_choice: Option<JobsQuitChoice>,
    /// Nur Tests: Verwaltung ohne Runtime-Montage.
    #[cfg(test)]
    pub(crate) test_manager: Option<Arc<JobManager>>,
}

/// Die Job-Verwaltung der Sitzung (nur TUI-Montage).
pub(crate) fn manager(app: &ChatApp) -> Option<Arc<JobManager>> {
    #[cfg(test)]
    if let Some(manager) = &app.jobs_ui.test_manager {
        return Some(Arc::clone(manager));
    }
    app.runtime
        .as_ref()
        .and_then(|runtime| runtime.session_jobs())
        .map(|jobs| Arc::clone(&jobs.manager))
}

/// Wecker für die Leerlauf-Schleife: fertig, sobald ein Job-Ereignis
/// zugestellt wurde; ohne Jobs nie.
pub(crate) fn waker(app: &ChatApp) -> Option<Arc<harw_runtime::job_wiring::JobEventRouter>> {
    app.runtime
        .as_ref()
        .and_then(|runtime| runtime.session_jobs())
        .map(|jobs| Arc::clone(&jobs.router))
}

/// Wartet auf das nächste Job-Ereignis (ohne Router nie).
pub(crate) async fn wait_for_event(router: Option<Arc<harw_runtime::job_wiring::JobEventRouter>>) {
    match router {
        Some(router) => router.notified().await,
        None => std::future::pending().await,
    }
}

/// `true`, solange ein Job läuft (Leerlauf-Takt für Laufzeit/Fortschritt).
pub(crate) fn has_running(app: &ChatApp) -> bool {
    app.agent_monitor.jobs().iter().any(|row| row.active)
}

/// Führt die Jobs-Gruppe (und eine offene Job-Detailansicht) nach.
///
/// # Arguments
/// - `force`: sofort, sonst höchstens alle [`JOBS_REFRESH_INTERVAL`].
///
/// # Returns
/// `true`, wenn sich Sichtbares änderte.
pub(crate) fn refresh(app: &mut ChatApp, force: bool) -> bool {
    let Some(manager) = manager(app) else {
        return false;
    };
    if !force
        && app
            .jobs_ui
            .last_refresh
            .is_some_and(|at| at.elapsed() < JOBS_REFRESH_INTERVAL)
    {
        return false;
    }
    app.jobs_ui.last_refresh = Some(Instant::now());
    let rows = rows_of(&manager.list(Caller::Operator));
    let mut changed = app.agent_monitor.set_jobs(rows);
    if let Some(detail) = app.agent_detail.as_mut()
        && let Some(job) = detail.job.clone()
    {
        let view = detail_of(&manager, &job);
        if detail.job_view != view {
            detail.job_view = view;
            changed = true;
        }
    }
    changed
}

/// Panelzeilen aus der Liste der Verwaltung (älteste zuerst).
pub(crate) fn rows_of(jobs: &[JobStatus]) -> Vec<JobRow> {
    jobs.iter().map(JobRow::from_status).collect()
}

/// Detailansicht eines Jobs (Kopf und Log-Ende), `None` für unbekannte Jobs.
fn detail_of(manager: &JobManager, job: &str) -> Option<JobDetail> {
    let id = JobId::parse(job)?;
    manager
        .status(&id, Caller::Operator)
        .ok()
        .map(|status| JobDetail::from_status(&status))
}

/// Öffnet die Detailansicht des im Panel gewählten Jobs.
///
/// # Returns
/// `true`, wenn ein Job gewählt war (Redraw nötig).
pub(crate) fn open_detail(app: &mut ChatApp) -> bool {
    let Some(job) = app.agent_monitor.selected_job() else {
        return false;
    };
    let job_view = manager(app).and_then(|manager| detail_of(&manager, &job));
    let prev_maximized = app
        .agent_detail
        .as_ref()
        .map_or(app.panels.maximized, |detail| detail.prev_maximized);
    app.agent_detail = Some(AgentDetailState {
        agent: SessionId(job.clone()),
        scroll: ChatScroll::new(),
        show_reasoning: true,
        prev_maximized,
        job: Some(job),
        job_view,
    });
    app.panels.maximized = true;
    true
}

/// Holt zugestellte Job-Ereignisse ab (siehe Moduldoku).
///
/// # Returns
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn poll(app: &mut ChatApp) -> bool {
    let Some(router) = waker(app) else {
        return false;
    };
    let ui_events = router.take_ui_events();
    let root_notes = router.take_root_notes();
    let any = !ui_events.is_empty() || !root_notes.is_empty();
    for notification in &ui_events {
        if let Some(line) = system_line(&notification.event) {
            app.push_line(Role::System, line);
        }
    }
    for notification in root_notes {
        let auto_turn = notification.event.is_finished();
        tracing::info!(
            job_id = %notification.event.job_id(),
            auto_turn,
            "tui.jobs.root_note"
        );
        enqueue_background_notice(app, notification.event.render_note(), auto_turn);
    }
    let refreshed = refresh(app, any);
    any || refreshed
}

/// Höchstlänge (Zeichen) der zitierten letzten Ausgabezeile eines
/// fehlgeschlagenen Jobs in der Systemzeile.
const FAILED_TAIL_CHARS: usize = 120;

/// Einzeilige Systemzeile für Start, Ende und Hinweise eines Jobs;
/// Fortschritt und Fehlerzeilen zeigt nur das Panel.
///
/// # Beschreibung
/// Der Start nennt den startenden Werkzeugaufruf, sofern das Ereignis ihn
/// trägt (`origin_tool`/`origin_call_id`, R18 TUI-07); das Ende eines
/// fehlgeschlagenen Jobs nennt Signal und letzte Ausgabezeile als Grund
/// (R18 F9), nie den Befehl.
fn system_line(event: &JobEvent) -> Option<String> {
    match event {
        JobEvent::Started {
            job_id,
            name,
            executed_on_host,
            origin_call_id,
            origin_tool,
            ..
        } => {
            let origin = match (origin_tool, origin_call_id) {
                (Some(tool), Some(call_id)) => format!(" · Aufruf {tool} {call_id}"),
                (Some(tool), None) => format!(" · Aufruf {tool}"),
                (None, Some(call_id)) => format!(" · Aufruf {call_id}"),
                (None, None) => String::new(),
            };
            Some(format!(
                "⚙ Job {job_id} „{name}“ gestartet ({}){origin}",
                if *executed_on_host { "Host" } else { "Sandbox" }
            ))
        }
        JobEvent::Finished {
            job_id,
            name,
            state,
            exit_code,
            signal,
            duration_secs,
            tail,
        } => {
            let mut line = format!(
                "⚙ Job {job_id} „{name}“ beendet: {} nach {}{}",
                harw_ops::jobs::state_label(*state),
                harw_tool_job::format_duration(*duration_secs),
                exit_code
                    .map(|code| format!(", Exit-Code {code}"))
                    .unwrap_or_default()
            );
            if matches!(state, JobState::Failed | JobState::Unknown) {
                if let Some(signal) = signal {
                    line.push_str(&format!(", Signal {signal}"));
                }
                let last = tail
                    .iter()
                    .rev()
                    .map(|entry| entry.trim())
                    .find(|entry| !entry.is_empty());
                if let Some(last) = last {
                    line.push_str(&format!(
                        " – {}",
                        crate::history_cell::truncate_chars(last, FAILED_TAIL_CHARS)
                    ));
                }
            }
            Some(line)
        }
        JobEvent::Warning {
            job_id,
            name,
            message,
        } => Some(format!("⚙ Job {job_id} „{name}“: {message}")),
        JobEvent::Progress { .. } | JobEvent::ErrorLines { .. } => None,
    }
}

// ── Beenden ──────────────────────────────────────────────────────────────────

/// Optionen des Beenden-Dialogs (Index = Wahl).
const QUIT_OPTIONS: [&str; 3] = ["Jobs stoppen", "Weiterlaufen lassen", "Abbrechen"];

/// Baut den Beenden-Dialog für die laufenden Jobs.
fn quit_dialog(running: &[JobStatus]) -> ChoiceDialog {
    let names: Vec<String> = running
        .iter()
        .take(5)
        .map(|status| format!("„{}“ ({})", status.meta.name, status.meta.job_id))
        .collect();
    let mut prompt = format!(
        "{} Job(s) laufen noch: {}{}.",
        running.len(),
        names.join(", "),
        if running.len() > names.len() {
            ", …"
        } else {
            ""
        }
    );
    let sandboxed = running
        .iter()
        .filter(|status| !status.meta.executed_on_host)
        .count();
    if sandboxed > 0 {
        prompt.push_str(&format!(
            "\nAchtung: {sandboxed} davon laufen in der Sandbox und enden trotzdem mit harw, \
             auch bei „Weiterlaufen lassen“."
        ));
    }
    prompt.push_str(
        "\nAbgelöste Jobs laufen weiter; ihre Logs und ihr Zustand sind nach dem nächsten \
         Start über /jobs sichtbar.",
    );
    ChoiceDialog::new(
        "Laufende Jobs",
        Some(prompt),
        QUIT_OPTIONS
            .iter()
            .map(|option| (*option).to_owned())
            .collect(),
    )
}

/// Entscheidet vor dem Beenden über laufende Jobs.
///
/// # Beschreibung
/// Ohne laufende Jobs: beenden. Ein doppeltes Ctrl+C/Ctrl+D (scharfgestellte
/// `pending_quit`) beendet sofort und löst die Jobs ab. Sonst öffnet der erste
/// Aufruf den Dialog und liefert `false`; die Wahl dort löst ein neues
/// Beenden aus, das hier eingelöst wird (`stop_all` bzw. `detach_all`).
///
/// # Returns
/// `true`, wenn beendet werden soll.
pub(crate) async fn before_quit(app: &mut ChatApp) -> bool {
    let Some(manager) = manager(app) else {
        return true;
    };
    if manager.running_count() == 0 {
        return true;
    }
    if app.pending_quit.is_some() {
        detach_for_exit(app);
        return true;
    }
    match app.jobs_ui.quit_choice.take() {
        Some(JobsQuitChoice::Stop) => {
            let stopped = manager.stop_all().await;
            tracing::info!(stopped, "tui.jobs.quit_stop_all");
            true
        }
        Some(JobsQuitChoice::Detach) => {
            detach_for_exit(app);
            true
        }
        None => {
            let running: Vec<JobStatus> = manager
                .list(Caller::Operator)
                .into_iter()
                .filter(|status| !status.meta.state.is_terminal())
                .collect();
            app.overlay = Some(Overlay::JobsQuitChoice(quit_dialog(&running)));
            false
        }
    }
}

/// Löst alle laufenden Jobs ab (harw endet sofort, z. B. doppeltes Ctrl+C
/// oder „Weiterlaufen lassen“) und warnt, wenn Sandbox-Jobs trotzdem enden.
pub(crate) fn detach_for_exit(app: &mut ChatApp) {
    let Some(manager) = manager(app) else {
        return;
    };
    let summary = manager.detach_all();
    if summary.ends_with_harw > 0 {
        tracing::warn!(
            detached = summary.detached,
            ends_with_harw = summary.ends_with_harw,
            "tui.jobs.detached_sandbox_jobs_end_with_harw"
        );
        app.push_line(
            Role::System,
            format!(
                "{} von {} abgelösten Job(s) laufen in der Sandbox und enden mit harw.",
                summary.ends_with_harw, summary.detached
            ),
        );
    } else if summary.detached > 0 {
        tracing::info!(detached = summary.detached, "tui.jobs.detached_on_quit");
    }
}

/// Tasten des Beenden-Dialogs.
///
/// # Returns
/// `None`: Dialog bleibt offen; `Some(None)`: abgebrochen (nicht beenden);
/// `Some(Some(wahl))`: mit dieser Wahl erneut beenden.
pub(crate) fn handle_quit_choice(
    dialog: &mut ChoiceDialog,
    key: crossterm::event::KeyEvent,
) -> Option<Option<JobsQuitChoice>> {
    match dialog.handle_key(key) {
        ChoiceAction::Stay => None,
        ChoiceAction::Cancel | ChoiceAction::Chosen(2..) => Some(None),
        ChoiceAction::Chosen(0) => Some(Some(JobsQuitChoice::Stop)),
        ChoiceAction::Chosen(_) => Some(Some(JobsQuitChoice::Detach)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_tool_job::{JobManagerConfig, JobOwner, NoopNotifier, PreparedJob, StartRequest};
    use harw_types::{TenantId, WorkspaceId};

    fn sandbox(root: &std::path::Path) -> TestResult<SandboxSpec> {
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-jobs-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: std::path::PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-jobs-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        ))
    }

    /// Eine TUI mit echter Job-Verwaltung und einem laufenden Job.
    fn app_with_running_job(
        dir: &std::path::Path,
        host: bool,
    ) -> TestResult<(ChatApp, Arc<JobManager>, JobId)> {
        let manager = JobManager::new(
            JobManagerConfig::new(dir.join("state")),
            Arc::new(NoopNotifier),
        )
        .map_err(ctx("manager"))?;
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(false);
        let status = manager
            .start(
                StartRequest {
                    name: "ladybird build".to_owned(),
                    command: "sleep 30".to_owned(),
                    cwd: None,
                    env_keys: Vec::new(),
                    notify_every: Duration::ZERO,
                    owner: JobOwner::new("worker", Vec::new()),
                },
                PreparedJob {
                    command,
                    executed_on_host: host,
                },
            )
            .map_err(ctx("start"))?;
        let mut app = ChatApp::new(Vec::new(), sandbox(dir)?, SessionId::new());
        app.jobs_ui.test_manager = Some(Arc::clone(&manager));
        Ok((app, manager, status.meta.job_id))
    }

    /// Plan R9, Teil F: mit laufenden Jobs öffnet das Beenden den Dialog;
    /// „Weiterlaufen lassen“ löst die Jobs ab und warnt für Sandbox-Jobs.
    #[tokio::test]
    async fn quit_with_running_jobs_asks_and_detach_keeps_them() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), false)?;
        assert!(refresh(&mut app, true), "Jobs-Gruppe übernimmt den Job");
        assert_eq!(app.agent_monitor.jobs().len(), 1);

        assert!(!before_quit(&mut app).await, "erst fragen");
        assert!(matches!(app.overlay, Some(Overlay::JobsQuitChoice(_))));

        app.overlay = None;
        app.jobs_ui.quit_choice = Some(JobsQuitChoice::Detach);
        assert!(before_quit(&mut app).await);
        assert_eq!(
            manager.running_count(),
            0,
            "abgelöst, nicht mehr beaufsichtigt"
        );
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, harw_tool_job::JobState::Detached);
        assert!(status.meta.detached);
        // Warnung: der Sandbox-Job endet trotzdem mit harw.
        let warned = app.export_entries.iter().any(|entry| {
            matches!(
                entry,
                crate::export::ExportEntry::System(text)
                    if text.contains("in der Sandbox und enden mit harw")
            )
        });
        assert!(warned, "Warnung für Sandbox-Jobs");
        // Aufräumen: den abgelösten Prozess beenden.
        let _ = manager
            .stop(&id, Caller::Operator, harw_tool_job::JobSignal::Kill)
            .await;
        Ok(())
    }

    /// „Jobs stoppen“ beendet alle laufenden Jobs; ohne laufende Jobs gibt
    /// es keinen Dialog.
    #[tokio::test]
    async fn quit_choice_stop_stops_all_running_jobs() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true)?;
        app.jobs_ui.quit_choice = Some(JobsQuitChoice::Stop);
        assert!(before_quit(&mut app).await);
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert!(status.meta.state.is_terminal(), "{:?}", status.meta.state);
        assert!(
            before_quit(&mut app).await,
            "nichts läuft mehr: kein Dialog"
        );
        assert!(app.overlay.is_none());
        Ok(())
    }

    /// Die Job-Detailansicht öffnet für den gewählten Job und zeigt ihn.
    #[tokio::test]
    async fn job_detail_opens_for_the_selected_job() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true)?;
        refresh(&mut app, true);
        let index = app
            .agent_monitor
            .panel_entries()
            .iter()
            .position(|entry| matches!(entry, crate::agent_monitor::PanelEntry::Job { .. }))
            .ok_or(TestError::Missing("Job-Eintrag"))?;
        app.agent_monitor.selected = index;
        assert!(app.open_agent_detail());
        let detail = app
            .agent_detail
            .as_ref()
            .ok_or(TestError::Missing("Detail"))?;
        assert_eq!(detail.job.as_deref(), Some(id.as_str()));
        let view = detail
            .job_view
            .as_ref()
            .ok_or(TestError::Missing("Ansicht"))?;
        assert_eq!(view.row.name, "ladybird build");
        assert!(view.header.iter().any(|line| line.contains("sleep 30")));
        let _ = manager
            .stop(&id, Caller::Operator, harw_tool_job::JobSignal::Kill)
            .await;
        Ok(())
    }

    /// TUI-05 (R18 F9): das Ende eines fehlgeschlagenen Jobs nennt den
    /// Grund (Signal, letzte Ausgabezeile); der Start nennt den Aufruf.
    #[test]
    fn job_system_lines_show_reason_and_origin() -> TestResult {
        let job_id = JobId::parse("job-9").ok_or(TestError::Missing("job id"))?;
        let started = system_line(&JobEvent::Started {
            job_id: job_id.clone(),
            name: "tests".into(),
            command: "cargo test".into(),
            pid: Some(1),
            executed_on_host: false,
            origin_call_id: Some("call-9".into()),
            origin_tool: Some("job.start".into()),
            owner_agent: None,
        })
        .ok_or(TestError::Missing("Startzeile"))?;
        assert_eq!(
            started,
            "⚙ Job job-9 „tests“ gestartet (Sandbox) · Aufruf job.start call-9"
        );
        assert!(!started.contains("cargo test"), "{started}");

        let failed = system_line(&JobEvent::Finished {
            job_id: job_id.clone(),
            name: "tests".into(),
            state: JobState::Failed,
            exit_code: Some(101),
            signal: None,
            duration_secs: 5,
            tail: vec!["error: test failed".into(), "  ".into()],
        })
        .ok_or(TestError::Missing("Endzeile"))?;
        assert!(
            failed.ends_with(", Exit-Code 101 – error: test failed"),
            "{failed}"
        );

        let ok = system_line(&JobEvent::Finished {
            job_id,
            name: "tests".into(),
            state: JobState::Succeeded,
            exit_code: Some(0),
            signal: None,
            duration_secs: 5,
            tail: vec!["ok".into()],
        })
        .ok_or(TestError::Missing("Endzeile"))?;
        assert!(!ok.contains(" – "), "{ok}");
        Ok(())
    }

    #[test]
    fn quit_dialog_offers_stop_detach_and_cancel() {
        let mut dialog = quit_dialog(&[]);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            handle_quit_choice(&mut dialog, enter),
            Some(Some(JobsQuitChoice::Stop))
        );
        let mut dialog = quit_dialog(&[]);
        assert_eq!(handle_quit_choice(&mut dialog, down), None);
        assert_eq!(
            handle_quit_choice(&mut dialog, enter),
            Some(Some(JobsQuitChoice::Detach))
        );
        let mut dialog = quit_dialog(&[]);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(handle_quit_choice(&mut dialog, esc), Some(None));
    }
}

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
//! - PL-97 C4 (W20-E): nach einem bestätigten `x` im Jobs-Fenster
//!   ([`crate::mission::stop_job`]) läuft `/jobs stop <id>` über den
//!   regulären Befehlsweg; [`note_stop_requested`] merkt sich den Job, und
//!   das nächste Nachführen meldet neutral den Zustand, den es dann sieht
//!   („Zustand jetzt: …“) — nie ein angenommenes „gestoppt“. Dieses Modul
//!   ruft dafür keine Job-API auf.
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

/// PL-97 C4: spätestens nach dieser Zeit meldet das Nachführen den Zustand
/// eines zum Stoppen bestätigten Jobs, auch wenn er sich nicht änderte (die
/// Stopp-Politik wartet `stop_grace` = 5 s, dann KILL).
pub(crate) const STOP_STATE_REPORT_AFTER: Duration = Duration::from_secs(10);

/// PL-97 C4: ein zum Stoppen bestätigter Job, dessen Zustand das nächste
/// Nachführen meldet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingStop {
    /// Job-Kennung.
    id: String,
    /// Anzeigename.
    name: String,
    /// Zustand bei der Bestätigung.
    state: JobState,
    /// Zeitpunkt der Bestätigung.
    requested: Instant,
}

/// TUI-Zustand der Jobs (Feld `ChatApp::jobs_ui`).
#[derive(Debug, Default)]
pub(crate) struct JobsUi {
    /// Zeitpunkt des letzten Nachführens der Jobs-Gruppe.
    last_refresh: Option<Instant>,
    /// PL-97 C4: zum Stoppen bestätigter Job (siehe [`note_stop_requested`]).
    pending_stop: Option<PendingStop>,
    /// Im Beenden-Dialog getroffene Wahl (wird beim nächsten Beenden
    /// eingelöst).
    pub(crate) quit_choice: Option<JobsQuitChoice>,
    /// Semantic snapshot behind the open quit dialog.
    quit_prompt: Option<JobsQuitPrompt>,
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
    crate::i18n::with_language(app.language, || refresh_scoped(app, force))
}

fn refresh_scoped(app: &mut ChatApp, force: bool) -> bool {
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
    changed |= report_stop_state(app, Instant::now());
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

/// PL-97 C4: merkt sich einen bestätigten Stopp (`x` im Jobs-Fenster); die
/// Zeile `/jobs stop <id>` selbst schickt der Aufrufer über den regulären
/// Befehlsweg. Ein neuer Stopp ersetzt einen noch nicht gemeldeten.
pub(crate) fn note_stop_requested(
    app: &mut ChatApp,
    target: &crate::mission::stop_job::StopTarget,
) {
    app.jobs_ui.pending_stop = Some(PendingStop {
        id: target.job.as_str().to_owned(),
        name: target.name.clone(),
        state: target.state,
        requested: Instant::now(),
    });
}

/// PL-97 C4: meldet den Zustand eines zum Stoppen bestätigten Jobs, sobald
/// das Nachführen eine Änderung sieht oder [`STOP_STATE_REPORT_AFTER`]
/// verstrichen ist — neutral, als der Zustand, den die Verwaltung dann
/// meldet (auch nach einer abgelehnten Zulassung, dann unverändert).
///
/// # Rückgabe
/// `true`, wenn eine Zeile hinzukam.
fn report_stop_state(app: &mut ChatApp, now: Instant) -> bool {
    let Some(pending) = app.jobs_ui.pending_stop.as_ref() else {
        return false;
    };
    let row = app
        .agent_monitor
        .jobs()
        .iter()
        .find(|row| row.id == pending.id);
    let due = now.saturating_duration_since(pending.requested) >= STOP_STATE_REPORT_AFTER;
    let state = match row {
        Some(row) if row.state != pending.state || due => row.state_label().to_owned(),
        None if due => crate::i18n::tr("mission.not_listed", "no longer listed").to_owned(),
        _ => return false,
    };
    let line = crate::i18n::tr_args(
        "mission.observed_stop",
        "⚙ Job {p0} “{p1}” – observed state now: {state}",
        &[
            ("p0", &(pending.id).to_string()),
            ("p1", &(pending.name).to_string()),
            ("state", &(state).to_string()),
        ],
    );
    app.jobs_ui.pending_stop = None;
    app.push_line(Role::System, line);
    true
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
    crate::i18n::with_language(app.language, || open_detail_scoped(app))
}

fn open_detail_scoped(app: &mut ChatApp) -> bool {
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
    crate::i18n::with_language(app.language, || poll_scoped(app))
}

fn poll_scoped(app: &mut ChatApp) -> bool {
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
                (Some(tool), Some(call_id)) => crate::i18n::tr_args(
                    "mission.origin_suffix",
                    " · Call {tool} {call_id}",
                    &[
                        ("tool", &(tool).to_string()),
                        ("call_id", &(call_id).to_string()),
                    ],
                ),
                (Some(tool), None) => crate::i18n::tr_args(
                    "mission.origin_tool_suffix",
                    " · Call {tool}",
                    &[("tool", &(tool).to_string())],
                ),
                (None, Some(call_id)) => crate::i18n::tr_args(
                    "mission.origin_call_suffix",
                    " · Call {call_id}",
                    &[("call_id", &(call_id).to_string())],
                ),
                (None, None) => String::new(),
            };
            Some(crate::i18n::tr_args(
                "mission.started",
                "⚙ Job {job_id} “{name}” started ({p0}){origin}",
                &[
                    ("job_id", &(job_id).to_string()),
                    ("name", &(name).to_string()),
                    (
                        "p0",
                        &(if *executed_on_host {
                            crate::i18n::tr("mission.host", "Host")
                        } else {
                            crate::i18n::tr("mission.sandbox", "Sandbox")
                        })
                        .to_string(),
                    ),
                    ("origin", &(origin).to_string()),
                ],
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
            let mut line = crate::i18n::tr_args(
                "mission.ended",
                "⚙ Job {job_id} “{name}” ended: {p0} after {p1}{p2}",
                &[
                    ("job_id", &(job_id).to_string()),
                    ("name", &(name).to_string()),
                    (
                        "p0",
                        &(crate::jobs_panel::job_state_label(*state)).to_string(),
                    ),
                    (
                        "p1",
                        &(harw_tool_job::format_duration(*duration_secs)).to_string(),
                    ),
                    (
                        "p2",
                        &(exit_code
                            .map(|code| {
                                crate::i18n::tr_args(
                                    "mission.exit_comma",
                                    ", exit code {code}",
                                    &[("code", &(code).to_string())],
                                )
                            })
                            .unwrap_or_default())
                        .to_string(),
                    ),
                ],
            );
            if matches!(state, JobState::Failed | JobState::Unknown) {
                if let Some(signal) = signal {
                    line.push_str(&crate::i18n::tr_args(
                        "mission.signal_comma",
                        ", signal {signal}",
                        &[("signal", &(signal).to_string())],
                    ));
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
        } => Some(crate::i18n::tr_args(
            "mission.warning",
            "⚙ Job {job_id} “{name}”: {message}",
            &[
                ("job_id", &job_id.to_string()),
                ("name", name),
                ("message", message),
            ],
        )),
        JobEvent::Progress { .. } | JobEvent::ErrorLines { .. } => None,
    }
}

// ── Beenden ──────────────────────────────────────────────────────────────────

/// Optionen des Beenden-Dialogs (Index = Wahl).
#[derive(Debug, Clone)]
struct JobsQuitPrompt {
    names: Vec<String>,
    count: usize,
    sandboxed: usize,
}

impl JobsQuitPrompt {
    fn from_statuses(running: &[JobStatus]) -> Self {
        Self {
            names: running
                .iter()
                .take(5)
                .map(|status| {
                    format!(
                        "“{}” ({})",
                        crate::sanitize::sanitize_inline(&status.meta.name),
                        status.meta.job_id
                    )
                })
                .collect(),
            count: running.len(),
            sandboxed: running
                .iter()
                .filter(|status| !status.meta.executed_on_host)
                .count(),
        }
    }

    fn dialog(&self) -> ChoiceDialog {
        let names = &self.names;
        let sandboxed = self.sandboxed;
        let mut prompt = crate::i18n::tr_args(
            "mission.quit_prompt",
            "{p0} job(s) still running: {p1}{p2}.",
            &[
                ("p0", &self.count.to_string()),
                ("p1", &names.join(", ")),
                (
                    "p2",
                    if self.count > names.len() {
                        ", …"
                    } else {
                        ""
                    },
                ),
            ],
        );
        if sandboxed > 0 {
            prompt.push_str(&crate::i18n::tr_args("mission.quit_sandbox", "\nWarning: {sandboxed} run in the sandbox and end with harw, even with “Keep running”.", &[("sandboxed", &sandboxed.to_string())]));
        }
        prompt.push_str(crate::i18n::tr("mission.quit_explanation", "\nDetached jobs keep running; their logs and state are available through /jobs after the next start."));
        ChoiceDialog::new(
            crate::i18n::tr("mission.quit_title", "Running jobs"),
            Some(prompt),
            vec![
                crate::i18n::tr("mission.quit_stop", "Stop jobs").to_owned(),
                crate::i18n::tr("mission.quit_detach", "Keep running").to_owned(),
                crate::i18n::tr("mission.quit_cancel", "Cancel").to_owned(),
            ],
        )
    }
}

pub(crate) fn refresh_quit_presentation(app: &mut ChatApp) {
    if let (Some(source), Some(Overlay::JobsQuitChoice(dialog))) =
        (&app.jobs_ui.quit_prompt, &mut app.overlay)
    {
        dialog.refresh_owned_presentation(source.dialog());
    }
}

/// Baut den Beenden-Dialog für die laufenden Jobs.
#[cfg(test)]
fn quit_dialog(running: &[JobStatus]) -> ChoiceDialog {
    JobsQuitPrompt::from_statuses(running).dialog()
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
            let source = JobsQuitPrompt::from_statuses(&running);
            app.overlay = Some(Overlay::JobsQuitChoice(crate::i18n::with_language(
                app.language,
                || source.dialog(),
            )));
            app.jobs_ui.quit_prompt = Some(source);
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
            crate::i18n::tr_args_for(
                app.language,
                "mission.detached_sandbox",
                "{p0} of {p1} detached job(s) run in the sandbox and end with harw.",
                &[
                    ("p0", &summary.ends_with_harw.to_string()),
                    ("p1", &summary.detached.to_string()),
                ],
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
    async fn app_with_running_job(
        dir: &std::path::Path,
        host: bool,
    ) -> TestResult<(ChatApp, Arc<JobManager>, JobId)> {
        crate::test_support::install_host_port();
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
                    env_cleared: false,
                },
            )
            .await
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
        let (mut app, manager, id) = app_with_running_job(dir.path(), false).await?;
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
                    if text.contains("run in the sandbox and end with harw")
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
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
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
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
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

    /// W20-A: das Nachführen übernimmt die typisierte Herkunft und die
    /// eigene Job-cgroup der Verwaltung in die Jobs-Gruppe; ein Start ohne
    /// Herkunft bleibt unbekannt.
    #[tokio::test]
    async fn refresh_carries_origin_and_cgroup_into_job_rows() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, plain_id) = app_with_running_job(dir.path(), true).await?;
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 30")
            .stdin(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(false);
        let tool_job = manager
            .start_with_origin(
                StartRequest {
                    name: "tests".to_owned(),
                    command: "sleep 30".to_owned(),
                    cwd: None,
                    env_keys: Vec::new(),
                    notify_every: Duration::ZERO,
                    owner: JobOwner::new("worker", Vec::new()),
                },
                PreparedJob {
                    command,
                    executed_on_host: true,
                    env_cleared: false,
                },
                Vec::new(),
                harw_tool_job::JobOrigin {
                    call_id: Some("call-1".to_owned()),
                    tool: Some("job.start".to_owned()),
                    owner_agent: None,
                    kind: None,
                },
            )
            .await
            .map_err(ctx("start tool job"))?;
        let tool_id = tool_job.meta.job_id.clone();
        assert!(refresh(&mut app, true), "Jobs-Gruppe übernimmt die Jobs");

        let rows = app.agent_monitor.jobs();
        let plain = rows
            .iter()
            .find(|row| row.id == plain_id.as_str())
            .ok_or(TestError::Missing("Zeile ohne Herkunft"))?;
        assert_eq!(plain.origin, None);
        let tool = rows
            .iter()
            .find(|row| row.id == tool_id.as_str())
            .ok_or(TestError::Missing("Zeile mit Herkunft"))?;
        assert_eq!(
            tool.origin,
            Some(harw_tool_job::ManagedJobOrigin::Tool {
                tool: "job.start".to_owned(),
                call_id: Some("call-1".to_owned()),
            })
        );
        let persisted = manager
            .status(&tool_id, Caller::Operator)
            .map_err(ctx("status"))?;
        // Nur eine eigene Job-cgroup erreicht die Zeile; ohne delegierte
        // cgroup-Wurzel (Testumgebung) läuft der Job in einer geteilten.
        assert_eq!(tool.cgroup.as_deref(), persisted.job_cgroup_path());

        for id in [&plain_id, &tool_id] {
            let _ = manager
                .stop(id, Caller::Operator, harw_tool_job::JobSignal::Kill)
                .await;
        }
        Ok(())
    }

    /// Zeichnet ein Bild, damit Fokusziele und Mission-Fenster stehen.
    fn draw(app: &ChatApp, width: u16, height: u16) -> TestResult {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                super::super::render_viewport(frame, app, &crate::spinner::Spinner::new(), None);
            })
            .map_err(ctx("draw"))?;
        Ok(())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Öffnet über `x` im fokussierten Jobs-Fenster den Stopp-Dialog und
    /// bestätigt ihn; liefert die einzige Zeile, die auf dem Bus ankam.
    fn confirm_stop_via_x(app: &mut ChatApp) -> TestResult<String> {
        draw(app, 70, 90)?;
        app.panels.focus = crate::panes::PaneFocus::Jobs;
        let (bus, mut receiver) = crate::events::harw_event_channel();
        assert!(super::super::handle_key(
            app,
            press(KeyCode::Char('x')),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::StopJob(_))));
        assert!(
            receiver.try_recv().is_err(),
            "der Dialog schickt noch nichts"
        );
        assert!(super::super::handle_key(app, press(KeyCode::Enter), &bus));
        assert!(app.overlay.is_none());
        let line = match receiver.try_recv() {
            Ok(crate::events::HarwEvent::Command(line)) => line,
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete HarwEvent::Command, bekam {other:?}"
                )));
            }
        };
        assert!(receiver.try_recv().is_err(), "genau eine Zeile");
        Ok(line)
    }

    fn system_text(app: &ChatApp, needle: &str) -> bool {
        app.export_entries.iter().any(|entry| {
            matches!(entry, crate::export::ExportEntry::System(text) if text.contains(needle))
        })
    }

    /// PL-97 C4 (W20-E): `x` → Dialog → Enter schickt genau eine Zeile
    /// `/jobs stop <id>` über den Befehlsweg; die Taste selbst ruft keine
    /// Job-API auf (der Job läuft weiter, kein Stopp angefordert). Die Zeile
    /// erreicht das `jobs`-Kommando genau einmal; über die echte Operation
    /// gestoppt, meldet das nächste Nachführen neutral den Zustand.
    #[tokio::test]
    async fn stop_job_x_dialog_dispatches_exactly_one_jobs_stop_line() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
        assert!(refresh(&mut app, true));
        let line = confirm_stop_via_x(&mut app)?;
        assert_eq!(line, format!("/jobs stop {id}"));
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, JobState::Running, "keine Job-API");
        assert!(!status.meta.stop_requested);

        let recorder = crate::mission::stop_job::tests::RecordingJobs::new();
        let output = crate::command_exec::execute_command_as(
            &recorder.adapters(),
            app.sandbox(),
            app.session_id(),
            harw_operations::PermissionTier::Operator,
            &line,
            harw_operations::ServiceMap::new,
        )
        .await;
        assert_eq!(output, "recorded");
        assert_eq!(
            recorder.calls(),
            vec![vec!["stop".to_owned(), id.as_str().to_owned()]]
        );

        // Die echte `/jobs`-Operation stoppt ihn (TERM, Stopp-Politik).
        let mut registry = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let adapters: Vec<harw_operations::adapter::CommandAdapter> = registry
            .iter()
            .flat_map(|op| harw_operations::adapter::CommandAdapter::from_operation(Arc::clone(op)))
            .collect();
        let output = crate::command_exec::execute_command_as(
            &adapters,
            app.sandbox(),
            app.session_id(),
            harw_operations::PermissionTier::Owner,
            &line,
            || {
                let mut services = harw_operations::ServiceMap::new();
                services.insert(Arc::clone(&manager));
                services
            },
        )
        .await;
        assert!(output.contains("TERM"), "{output}");
        assert!(refresh(&mut app, true));
        let label = crate::jobs_panel::job_state_label(JobState::Stopped);
        assert!(
            system_text(
                &app,
                &format!("“ladybird build” – observed state now: {label}")
            ),
            "{:?}",
            app.export_entries
        );
        assert!(app.jobs_ui.pending_stop.is_none(), "einmal gemeldet");
        Ok(())
    }

    /// Esc schließt den Dialog und schickt nichts.
    #[tokio::test]
    async fn stop_job_esc_dispatches_nothing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
        refresh(&mut app, true);
        draw(&app, 70, 90)?;
        app.panels.focus = crate::panes::PaneFocus::Jobs;
        let (bus, mut receiver) = crate::events::harw_event_channel();
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Char('x')),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::StopJob(_))));
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Esc),
            &bus
        ));
        assert!(app.overlay.is_none());
        assert!(receiver.try_recv().is_err(), "Esc schickt nichts");
        assert!(app.jobs_ui.pending_stop.is_none());
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, JobState::Running);
        let _ = manager
            .stop(&id, Caller::Operator, harw_tool_job::JobSignal::Kill)
            .await;
        Ok(())
    }

    /// Eine abgelehnte Zulassung (Aufrufer unter der Stufe von `/jobs`) ist
    /// das Ende: kein zweiter Weg, der Job läuft weiter; das Nachführen
    /// meldet nach der Frist neutral den unveränderten Zustand.
    #[tokio::test]
    async fn stop_job_rejected_admission_has_no_second_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
        refresh(&mut app, true);
        let line = confirm_stop_via_x(&mut app)?;
        let mut registry = harw_operations::registry::OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        let adapters: Vec<harw_operations::adapter::CommandAdapter> = registry
            .iter()
            .flat_map(|op| harw_operations::adapter::CommandAdapter::from_operation(Arc::clone(op)))
            .collect();
        let output = crate::command_exec::execute_command_as(
            &adapters,
            app.sandbox(),
            app.session_id(),
            harw_operations::PermissionTier::Observer,
            &line,
            || {
                let mut services = harw_operations::ServiceMap::new();
                services.insert(Arc::clone(&manager));
                services
            },
        )
        .await;
        assert!(!output.contains("TERM"), "{output}");
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, JobState::Running);
        assert!(!status.meta.stop_requested, "kein Rückfall auf die Job-API");

        refresh(&mut app, true);
        assert!(app.jobs_ui.pending_stop.is_some(), "noch nichts zu melden");
        let later = Instant::now() + STOP_STATE_REPORT_AFTER;
        assert!(report_stop_state(&mut app, later));
        let label = crate::jobs_panel::job_state_label(JobState::Running);
        assert!(
            system_text(&app, &format!("observed state now: {label}")),
            "{:?}",
            app.export_entries
        );
        let _ = manager
            .stop(&id, Caller::Operator, harw_tool_job::JobSignal::Kill)
            .await;
        Ok(())
    }

    /// PL-97 C4: im Fenster Finished/Failed bleibt ein fehlgeschlagener Job
    /// `NEW`, bis seine Kategorie quittiert ist — `a` quittiert die gewählte
    /// Kategorie, `Enter` quittiert und öffnet ihre Einzelläufe (Esc
    /// schließt sie); die Zähler bleiben, nichts wird gelöscht.
    #[test]
    fn finished_window_acknowledges_failed_jobs_and_opens_constituents() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut app = ChatApp::new(Vec::new(), sandbox(dir.path())?, SessionId::new());
        let failed = |id: &str| JobRow {
            origin: Some(harw_tool_job::ManagedJobOrigin::Tool {
                tool: "job.start".to_owned(),
                call_id: None,
            }),
            ..crate::mission::projection::tests::job_in(id, JobState::Failed)
        };
        app.agent_monitor
            .set_jobs(vec![failed("job-1"), failed("job-2")]);
        let rows = |app: &ChatApp| {
            crate::mission::projection::terminal_aggregate(
                &app.agent_monitor,
                app.agent_monitor.jobs(),
            )
        };
        let row = rows(&app);
        assert_eq!(
            row.first()
                .map(|row| (row.label.as_str(), row.unseen_failures)),
            Some(("tool:job.start", 2))
        );
        let (bus, _receiver) = crate::events::harw_event_channel();
        draw(&app, 70, 90)?;
        app.panels.focus = crate::panes::PaneFocus::Finished;
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Enter),
            &bus
        ));
        assert!(app.mission.finished_expanded, "Enter klappt zuerst auf");
        assert!(app.overlay.is_none());
        draw(&app, 70, 90)?;
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Char('a')),
            &bus
        ));
        assert!(app.agent_monitor.job_failure_seen("job-1"));
        assert!(app.agent_monitor.job_failure_seen("job-2"));
        let after = rows(&app);
        assert_eq!(
            after
                .first()
                .map(|row| (row.total, row.failed, row.unseen_failures)),
            Some((2, 2, 0)),
            "quittiert, nichts gelöscht"
        );

        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Enter),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::View(_))), "Einzelläufe");
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Esc),
            &bus
        ));
        assert!(app.overlay.is_none());
        assert_eq!(app.panels.focus, crate::panes::PaneFocus::Finished);
        Ok(())
    }

    /// Text eines gezeichneten Bildes (Zeilen mit `\n` verbunden).
    fn screen_text(app: &ChatApp, width: u16, height: u16) -> TestResult<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                super::super::render_viewport(frame, app, &crate::spinner::Spinner::new(), None);
            })
            .map_err(ctx("draw"))?;
        Ok(crate::test_support::display_buffer_text(
            terminal.backend().buffer(),
        ))
    }

    /// PR #154 (P2): eine Agentenrolle „Unknown job“ trägt dieselbe
    /// Bezeichnung wie die Job-Kategorie ohne Herkunft. `a` auf der
    /// Job-Zeile quittiert genau sie (die Rolle bleibt `NEW`); `Enter` auf
    /// der Rollen-Zeile quittiert die Rolle und öffnet ihre Einzelläufe aus
    /// dem quittierten Stand (kein `NEW` mehr in der Liste).
    #[test]
    fn finished_window_keys_act_on_the_selected_category_despite_a_label_clash() -> TestResult {
        use crate::mission::projection::{TerminalCategory, UNKNOWN_JOB_LABEL, terminal_aggregate};
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut app = ChatApp::new(Vec::new(), sandbox(dir.path())?, SessionId::new());
        app.agent_monitor.apply(&crate::mission::tests::turn_event(
            "r1",
            UNKNOWN_JOB_LABEL,
            harw_protocol::TurnEvent::TurnFailed {
                turn_id: harw_types::TurnId::new(),
                reason: "kaputt".to_owned(),
                retryable: false,
            },
        ));
        app.agent_monitor
            .set_jobs(vec![crate::mission::projection::tests::job_in(
                "job-1",
                JobState::Failed,
            )]);
        let role = TerminalCategory::Role(UNKNOWN_JOB_LABEL.to_owned());
        let index_of = |app: &ChatApp, category: &TerminalCategory| {
            terminal_aggregate(&app.agent_monitor, app.agent_monitor.jobs())
                .iter()
                .position(|row| row.category == *category)
                .ok_or(TestError::Missing("Kategorie"))
        };
        let unseen_of = |app: &ChatApp, category: &TerminalCategory| {
            terminal_aggregate(&app.agent_monitor, app.agent_monitor.jobs())
                .iter()
                .find(|row| row.category == *category)
                .map(|row| row.unseen_failures)
        };
        let (bus, _receiver) = crate::events::harw_event_channel();
        draw(&app, 70, 90)?;
        app.panels.focus = crate::panes::PaneFocus::Finished;
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Enter),
            &bus
        ));
        assert!(app.mission.finished_expanded);
        draw(&app, 70, 90)?;
        // Auf die Job-Zeile gleicher Bezeichnung gehen und quittieren.
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Home),
            &bus
        ));
        for _ in 0..index_of(&app, &TerminalCategory::UnknownJob)? {
            assert!(super::super::handle_key(
                &mut app,
                press(KeyCode::Char('j')),
                &bus
            ));
        }
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Char('a')),
            &bus
        ));
        assert!(app.agent_monitor.job_failure_seen("job-1"));
        assert_eq!(unseen_of(&app, &TerminalCategory::UnknownJob), Some(0));
        assert_eq!(unseen_of(&app, &role), Some(1), "die Rolle bleibt NEW");

        // Auf die Rollen-Zeile gehen: Enter quittiert und öffnet sie.
        draw(&app, 70, 90)?;
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Home),
            &bus
        ));
        for _ in 0..index_of(&app, &role)? {
            assert!(super::super::handle_key(
                &mut app,
                press(KeyCode::Char('j')),
                &bus
            ));
        }
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Enter),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::View(_))), "Einzelläufe");
        assert_eq!(unseen_of(&app, &role), Some(0));
        let shown = screen_text(&app, 70, 90)?;
        assert!(shown.contains("kaputt"), "die Liste der Rolle: {shown}");
        assert!(!shown.contains("NEW"), "quittiert: {shown}");
        Ok(())
    }

    /// PL-97 C4 (Review): zeichnet die Lage kein Fenster Finished/Failed
    /// (flaches Querformat 100x12), behalten die Agenten-Karten die volle
    /// Liste — der ungesehene Fehlschlag bleibt auf dem Bildschirm. Mit dem
    /// Streifen (100x13) zeigen die Karten nur die laufenden Agenten.
    #[test]
    fn cards_without_a_finished_window_keep_unseen_failures_on_screen() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut app = ChatApp::new(Vec::new(), sandbox(dir.path())?, SessionId::new());
        crate::mission::tests::thinking(&mut app.agent_monitor, "w7", "uia-worker", "");
        app.agent_monitor.apply(&crate::mission::tests::turn_event(
            "f1",
            "uia-tester",
            harw_protocol::TurnEvent::TurnFailed {
                turn_id: harw_types::TurnId::new(),
                reason: "kaputt".to_owned(),
                retryable: false,
            },
        ));
        let shown = screen_text(&app, 100, 12)?;
        let regions = app.last_regions.get();
        assert!(regions.agents.is_some(), "{regions:?}");
        assert!(regions.finished.is_none(), "{regions:?}");
        assert!(shown.contains("✗ 1"), "{shown}");
        assert!(shown.contains("uia-tester"), "{shown}");
        assert_eq!(app.agent_monitor.panel_entries().len(), 2, "w7 und f1");

        let shown = screen_text(&app, 100, 13)?;
        assert!(app.last_regions.get().finished.is_some(), "{shown}");
        assert_eq!(
            app.agent_monitor.panel_entries().len(),
            1,
            "nur w7 neben Finished/Failed"
        );
        Ok(())
    }

    /// PL-97 C4 (W20-E): während eines Turns nimmt die bestätigte Zeile den
    /// Busy-Weg einer getippten Zeile (`route_busy_overlay_key` →
    /// `route_busy_command`): genau ein eingereihter Auftrag `/jobs stop
    /// <id>` in der Busy-Klasse von `/jobs`, keine Job-API.
    #[tokio::test]
    async fn stop_job_confirmed_during_a_turn_takes_the_typed_busy_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (mut app, manager, id) = app_with_running_job(dir.path(), true).await?;
        app.command_registry = crate::CommandRegistry::built_in().map_err(ctx("built_in"))?;
        assert!(refresh(&mut app, true));
        draw(&app, 70, 90)?;
        app.panels.focus = crate::panes::PaneFocus::Jobs;
        let (bus, mut receiver) = crate::events::harw_event_channel();
        assert!(super::super::handle_key(
            &mut app,
            press(KeyCode::Char('x')),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::StopJob(_))));

        let typed = format!("/jobs stop {id}");
        let class = crate::command_exec::busy_availability_for(&app.command_registry, &typed);
        assert!(class.runs_during_turn(), "{class:?}");
        assert_eq!(
            super::super::route_busy_overlay_key(&mut app, press(KeyCode::Enter)),
            super::super::BusyKeyOutcome::Dispatch(class)
        );
        assert!(app.overlay.is_none());
        let queued: Vec<(String, harw_operations::BusyAvailability)> = app
            .busy_jobs
            .queued()
            .iter()
            .map(|command| (command.raw.clone(), command.class))
            .collect();
        assert_eq!(queued, vec![(typed, class)], "genau ein Auftrag");
        assert!(receiver.try_recv().is_err(), "nichts auf dem Leerlauf-Bus");
        let status = manager
            .status(&id, Caller::Operator)
            .map_err(ctx("status"))?;
        assert_eq!(status.meta.state, JobState::Running, "keine Job-API");
        assert!(!status.meta.stop_requested);
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
            origin: Some(harw_tool_job::ManagedJobOrigin::Tool {
                tool: "job.start".into(),
                call_id: Some("call-9".into()),
            }),
        })
        .ok_or(TestError::Missing("Startzeile"))?;
        assert_eq!(
            started,
            "⚙ Job job-9 “tests” started (Sandbox) · Call job.start call-9"
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
            failed.ends_with(", exit code 101 – error: test failed"),
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

    #[test]
    fn settings_refreshes_open_stop_and_quit_dialogs_with_stable_actions() -> TestResult {
        use crate::i18n::{tr_args_for, tr_for, with_language};
        use harw_config::UiLanguage;
        let mut app = super::super::tests::test_chat_app()?;
        let mut row = crate::mission::projection::tests::job_in("job-raw", JobState::Running);
        row.name = "raw name {name} 👩🏽‍💻".repeat(12);
        app.language = UiLanguage::German;
        app.overlay = Some(Overlay::StopJob(
            with_language(app.language, || {
                crate::mission::stop_job::StopJobDialog::for_row(&row)
            })
            .ok_or(TestError::Missing("stop dialog"))?,
        ));
        for language in UiLanguage::ALL {
            let mut preferences = harw_config::TuiSection::default();
            preferences.language = language;
            app.apply_display_preferences(&preferences);
            let shown = screen_text(&app, 180, 40)?;
            assert!(
                shown.contains(tr_for(language, "mission.stop_title", "Stop job")),
                "{language:?}: {shown}"
            );
            assert!(shown.contains("job-raw"), "{shown}");
        }
        let (bus, mut receiver) = crate::events::harw_event_channel();
        super::super::handle_key(&mut app, press(KeyCode::Enter), &bus);
        assert!(
            matches!(receiver.try_recv(), Ok(crate::events::HarwEvent::Command(line)) if line == "/jobs stop job-raw")
        );
        assert!(receiver.try_recv().is_err());
        assert!(
            app.export_entries.is_empty(),
            "confirmation never reports stop success"
        );
        app.jobs_ui.quit_prompt = Some(JobsQuitPrompt {
            names: vec!["“raw job” (job-raw)".to_owned()],
            count: 1,
            sandboxed: 1,
        });
        app.overlay = Some(Overlay::JobsQuitChoice(with_language(app.language, || {
            app.jobs_ui.quit_prompt.as_ref().expect("source").dialog()
        })));
        if let Some(Overlay::JobsQuitChoice(dialog)) = app.overlay.as_mut() {
            dialog.handle_key(press(KeyCode::Down));
        }
        for language in UiLanguage::ALL {
            let mut preferences = harw_config::TuiSection::default();
            preferences.language = language;
            app.apply_display_preferences(&preferences);
            let shown = screen_text(&app, 180, 40)?;
            assert!(
                shown.contains(tr_for(language, "mission.quit_title", "Running jobs")),
                "{shown}"
            );
            assert!(shown.contains("job-raw"), "{shown}");
        }
        if let Some(Overlay::JobsQuitChoice(dialog)) = app.overlay.as_mut() {
            assert_eq!(
                handle_quit_choice(dialog, press(KeyCode::Enter)),
                Some(Some(JobsQuitChoice::Detach))
            );
        } else {
            return Err(TestError::Missing("quit dialog"));
        }
        app.agent_monitor.set_jobs(vec![row]);
        app.language = UiLanguage::French;
        let pending = app
            .jobs_ui
            .pending_stop
            .as_ref()
            .ok_or(TestError::Missing("pending stop"))?;
        let raw_name = pending.name.clone();
        let later = pending.requested + STOP_STATE_REPORT_AFTER;
        let reported = with_language(app.language, || report_stop_state(&mut app, later));
        assert!(reported);
        let state = tr_for(UiLanguage::French, "mission.running", "running");
        let expected = tr_args_for(
            UiLanguage::French,
            "mission.observed_stop",
            "⚙ Job {p0} “{p1}” – observed state now: {state}",
            &[("p0", "job-raw"), ("p1", &raw_name), ("state", state)],
        );
        assert!(system_text(&app, &expected));
        assert_eq!(app.agent_monitor.jobs()[0].state, JobState::Running);
        Ok(())
    }
}

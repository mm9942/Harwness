//! Einbindung des Plan-Modus in die `ChatApp` (Runde 5, Teil F).
//!
//! # Beschreibung
//! Die reinen Zustände und Widgets liegen in [`crate::plan_dialog`] und
//! [`crate::ask_user_dialog`]; dieses Kindmodul von `app` wendet ihre
//! Entscheidungen auf die `ChatApp` an:
//! - **Sperre:** [`ChatApp::sync_plan_lock`] spiegelt den angezeigten
//!   Interaktionsmodus sofort in die geteilte `PlanModeLock` der Montage —
//!   die `PlanModeGate` sperrt damit auch mitten im Turn.
//! - **Freigabe (`plan.exit`):** nur Option 1/2 wechselt nach `work` mit
//!   Freigabe `auto` (`ApprovalMode::Delegated`) bzw. `ask`
//!   (`ApprovalMode::AlwaysAsk`) und reiht eine Folge-Nachricht ein, damit
//!   die Umsetzung im nächsten Turn (mit der vollen Werkzeugfläche) beginnt.
//!   Option 3 lässt den Plan-Modus stehen; die Rückmeldung ging bereits als
//!   Werkzeugergebnis an den Agenten.
//! - **Vorschlag (`plan.enter`):** nur „Ja“ schaltet in die Stufe `plan`.
//! - **`/plan …`** und der externe Editor für `/plan edit`.
//!
//! # Nebenläufigkeit
//! Renderer-Thread; der externe Editor blockiert ihn bewusst (nur im
//! Leerlauf, `/plan edit` ist nicht busy-sicher).

use std::path::Path;
use std::time::Instant;

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EnableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use harw_core::InteractionMode;
use harw_extension_api::approval_mode::ApprovalMode;
use harw_operations::SessionController;
use harw_tool_plan::{PlanSession, PlanUiReceiver, PlanUiRequest};

use super::{ChatApp, PermissionCycleStage, Role, TerminalGuard};
use crate::plan_dialog::{
    PlanCommand, PlanCommandEffect, PlanDialogAction, PlanUi, run_plan_command,
};
use crate::tui_event::TuiEvent;

impl ChatApp {
    /// Runde 5, Teil F: hängt den Plan-Fragekanal und den geteilten
    /// Plan-Zustand der Montage an.
    ///
    /// # Argumente
    /// - `receiver`: aus `RuntimeAssembly::take_plan_ui_requests` (`None`
    ///   außerhalb einer TUI-Montage — dann gibt es keine Plan-Fenster).
    /// - `session`: `RuntimeAssembly::plan_session`.
    #[must_use]
    pub(crate) fn with_plan_ui(
        mut self,
        receiver: Option<PlanUiReceiver>,
        session: Option<PlanSession>,
    ) -> Self {
        self.plan_ui = PlanUi::new(receiver, session);
        self.sync_plan_lock();
        self
    }

    /// Spiegelt den angezeigten Interaktionsmodus sofort in die Plan-Sperre
    /// (`true` genau in der Stufe `plan`).
    pub(crate) fn sync_plan_lock(&self) {
        if let Some(session) = self.plan_ui.session() {
            session
                .lock()
                .set(self.active_mode == InteractionMode::Plan);
        }
    }

    /// Wechselt in die Stufe `plan` (wie Shift+Tab bis `plan`); bereits im
    /// Plan-Modus ein No-op.
    pub(crate) fn enter_plan_stage(&mut self) {
        if self.active_mode == InteractionMode::Plan {
            self.sync_plan_lock();
            return;
        }
        self.pending_permission_stage = None;
        self.apply_permission_stage(PermissionCycleStage::Plan);
        self.sync_plan_lock();
    }

    /// Verlässt den Plan-Modus nach einer Freigabe: Modus `work` plus den
    /// gewählten Freigabemodus. Der Kern übernimmt `work` an der
    /// Turn-Grenze; bis dahin bleibt die Werkzeugfläche des Plan-Modus.
    fn leave_plan_for_work(&mut self, approval: ApprovalMode) {
        self.pending_permission_stage = None;
        self.set_approval_mode(approval);
        self.mode_before_plan = None;
        self.active_mode = InteractionMode::Work;
        if let Err(error) = self
            .session_controller
            .request_mode(InteractionMode::Work.as_str())
        {
            tracing::warn!(error = %error, "tui.plan_mode.request_work_failed");
        }
        self.sync_plan_lock();
    }
}

/// Nimmt eine neue Frage (oder das Kanalende) aus dem `select!` entgegen.
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn accept_request(app: &mut ChatApp, maybe_request: Option<PlanUiRequest>) -> bool {
    match maybe_request {
        Some(request) => {
            tracing::info!("tui.plan_mode.request_shown");
            app.plan_ui.accept(request, Instant::now());
            true
        }
        None => {
            tracing::warn!("tui.plan_mode.request_channel_ended");
            app.plan_ui.channel_closed();
            false
        }
    }
}

/// Leitet ein Eingabeereignis an das offene Plan-Fenster (die Einbindung
/// ruft das nur bei `PlanUi::is_open` auf) und wendet eine Entscheidung an.
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn route_event(app: &mut ChatApp, event: TuiEvent) -> bool {
    let outcome = app.plan_ui.handle_event(event, Instant::now());
    if let Some(action) = outcome.action {
        apply_action(app, action);
    }
    outcome.redraw
}

/// Wendet eine Entscheidung aus einem Plan-Fenster an.
fn apply_action(app: &mut ChatApp, action: PlanDialogAction) {
    match action {
        PlanDialogAction::ImplementAuto { display_path } => {
            app.leave_plan_for_work(ApprovalMode::Delegated);
            app.push_line(
                Role::System,
                format!(
                    "Plan {display_path} freigegeben — Umsetzung im Auto-Modus (work · Freigabe \
                     auto); der Plan ist angeheftet."
                ),
            );
            app.pending_turns.push_back(implementation_message(&display_path));
        }
        PlanDialogAction::ImplementAsk { display_path } => {
            app.leave_plan_for_work(ApprovalMode::AlwaysAsk);
            app.push_line(
                Role::System,
                format!(
                    "Plan {display_path} freigegeben — Umsetzung mit Einzelfreigabe (work · \
                     Freigabe ask); der Plan ist angeheftet."
                ),
            );
            app.pending_turns.push_back(implementation_message(&display_path));
        }
        PlanDialogAction::KeepPlanning => app.push_line(
            Role::System,
            "Plan nicht freigegeben — deine Rückmeldung ging an den Agenten, der Plan-Modus bleibt.",
        ),
        PlanDialogAction::EnterPlan => {
            app.enter_plan_stage();
            app.push_line(
                Role::System,
                "Plan-Modus an (vom Agenten vorgeschlagen) — es wird nichts verändert.",
            );
        }
        PlanDialogAction::EnterDeclined => {
            app.push_line(Role::System, "Plan-Modus abgelehnt — es geht weiter wie bisher.");
        }
        PlanDialogAction::Answered(summary) => {
            app.push_line(Role::System, format!("Antwort an den Agenten: {summary}"));
        }
        // Runde 5, Teil P: Bestätigung eines Plan-Graphen der `plan`-Operation.
        // Die Wirkung (aktiv schalten, Goal binden) übernimmt die Operation,
        // sobald die Entscheidung sie erreicht; hier nur die Verlaufszeile.
        PlanDialogAction::PlanConfirmed { plan_id, change } => {
            let text = if change {
                format!("◎ Änderung an Plan {plan_id} bestätigt.")
            } else {
                format!("◎ Plan {plan_id} bestätigt — er ist jetzt aktiv und wird als Goal verfolgt.")
            };
            app.push_line(Role::System, text);
        }
        PlanDialogAction::PlanRejected { plan_id } => {
            app.push_line(
                Role::System,
                format!("Plan {plan_id} nicht bestätigt — deine Rückmeldung ging an den Agenten."),
            );
        }
        PlanDialogAction::Closed => {
            app.push_line(Role::System, "Fenster ohne Entscheidung geschlossen.");
        }
    }
}

/// Die Folge-Nachricht nach einer Freigabe: startet die Umsetzung im
/// nächsten Turn (dann mit der Werkzeugfläche von `work`).
pub(crate) fn implementation_message(display_path: &str) -> String {
    format!(
        "Der Plan {display_path} ist freigegeben. Setze ihn jetzt Schritt für Schritt um und \
         halte dich an seine Verifikation."
    )
}

/// Schließt jedes offene Plan-Fenster ohne Entscheidung (Turn-Ende,
/// Leerlauf).
///
/// # Rückgabe
/// `true`, wenn ein Fenster offen war.
pub(crate) fn close_open(app: &mut ChatApp) -> bool {
    if app.plan_ui.close_all() {
        app.push_line(
            Role::System,
            "Plan-Fenster ohne Entscheidung geschlossen (Turn beendet).",
        );
        true
    } else {
        false
    }
}

/// Führt einen lokalen `/plan`-Befehl aus.
pub(crate) fn apply_plan_command(app: &mut ChatApp, command: &PlanCommand) {
    match run_plan_command(command, app.plan_ui.session()) {
        PlanCommandEffect::EnterPlanStage(text) => {
            app.enter_plan_stage();
            push_text(app, &text);
        }
        PlanCommandEffect::Text(text) => push_text(app, &text),
        PlanCommandEffect::Edit { slug, path } => {
            app.push_line(
                Role::System,
                format!("Öffne {} im Editor …", path.display()),
            );
            app.plan_ui.set_pending_editor(slug, path);
        }
    }
}

fn push_text(app: &mut ChatApp, text: &str) {
    app.push_lines(
        text.split('\n')
            .map(|line| ratatui::text::Line::from(crate::sanitize::sanitize_inline(line)))
            .collect(),
    );
}

/// Öffnet eine vorgemerkte Plan-Datei im externen Editor (`/plan edit`).
///
/// # Beschreibung
/// Verlässt Alternate-Screen und Raw-Mode, startet `$VISUAL`/`$EDITOR`
/// (Rückfall `vi`) blockierend, stellt das Terminal wieder her und
/// erzwingt ein vollständiges Neuzeichnen. War der bearbeitete Plan
/// angeheftet, wird der neue Inhalt angeheftet.
///
/// # Rückgabe
/// `true`, wenn ein Editor gestartet wurde (neu zeichnen).
pub(crate) fn run_pending_editor(guard: &mut TerminalGuard, app: &mut ChatApp) -> bool {
    let Some((slug, path)) = app.plan_ui.take_pending_editor() else {
        return false;
    };
    let result = suspend_and_edit(&path);
    guard.reassert_terminal_modes();
    if let Err(error) = guard.terminal().clear() {
        tracing::warn!(%error, "tui.plan_mode.clear_after_editor_failed");
    }
    match result {
        Ok(()) => {
            let repinned = app.plan_ui.session().is_some_and(|session| {
                let pinned = session.pinned().get().is_some_and(|doc| doc.slug == slug);
                if pinned {
                    if let Ok(content) = session.dir().read(&slug) {
                        session.pinned().pin(&slug, &content);
                    }
                }
                pinned
            });
            let suffix = if repinned {
                " — angehefteter Plan aktualisiert"
            } else {
                ""
            };
            app.push_line(
                Role::System,
                format!("Plan {} gespeichert{suffix}.", path.display()),
            );
        }
        Err(error) => app.push_line(Role::System, format!("/plan edit: {error}")),
    }
    true
}

/// Der Editor-Befehl: `$VISUAL`, sonst `$EDITOR`, sonst `vi`; Wörter werden
/// an Leerraum getrennt (z. B. `code -w`).
fn editor_command() -> (String, Vec<String>) {
    let raw = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".to_owned());
    let mut words = raw.split_whitespace().map(str::to_owned);
    let program = words.next().unwrap_or_else(|| "vi".to_owned());
    (program, words.collect())
}

fn suspend_and_edit(path: &Path) -> Result<(), String> {
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::event::DisableMouseCapture,
        DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    );
    let _ = disable_raw_mode();
    let (program, args) = editor_command();
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(path)
        .status();
    let _ = enable_raw_mode();
    let _ = crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
    );
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{program} endete mit {status}")),
        Err(error) => Err(format!("{program} ließ sich nicht starten: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_authority::SandboxSpec;
    use harw_tool_plan::{PlanDir, PlanExitPrompt};
    use harw_types::SessionId;
    use std::path::PathBuf;

    fn sandbox(temp: &tempfile::TempDir) -> TestResult<SandboxSpec> {
        use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
        use harw_types::{TenantId, WorkspaceId};

        std::fs::create_dir_all(temp.path().join("workspace")).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            temp.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-plan-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-plan-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        ))
    }

    fn app_with_plan_session() -> TestResult<(tempfile::TempDir, ChatApp, PlanSession)> {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let session = PlanSession::new(
            PlanDir::new(temp.path().join("workspace/.harw/plans")),
            false,
        );
        let app = ChatApp::new(Vec::new(), sandbox(&temp)?, SessionId::new())
            .with_plan_ui(None, Some(session.clone()));
        Ok((temp, app, session))
    }

    /// Plan Teil F, Test „Der Shift+Tab-Zyklus erreicht `plan`, setzt dabei
    /// `InteractionMode::Plan` und verlässt ihn wieder sauber“ — inklusive
    /// der sofort wirkenden Sperre (auch mitten im Turn).
    #[test]
    fn plan_stage_sets_plan_mode_locks_immediately_and_unlocks_on_leave() -> TestResult {
        let (_temp, mut app, session) = app_with_plan_session()?;
        assert!(!session.lock().is_locked());
        assert_eq!(
            PermissionCycleStage::Full.next(),
            PermissionCycleStage::Plan
        );
        // Die Stufe `plan` aus dem Zyklus (Full → Plan).
        app.apply_permission_stage(PermissionCycleStage::Plan);
        assert_eq!(app.current_permission_stage(), PermissionCycleStage::Plan);
        assert_eq!(app.active_mode(), InteractionMode::Plan);
        assert!(session.lock().is_locked(), "Sperre wirkt sofort");
        // Shift+Tab während eines laufenden Turns: plan → ask.
        app.cycle_permission_stage(true);
        assert_eq!(app.current_permission_stage(), PermissionCycleStage::Ask);
        assert_ne!(app.active_mode(), InteractionMode::Plan);
        assert!(!session.lock().is_locked());
        Ok(())
    }

    #[test]
    fn slash_plan_enters_plan_mode() -> TestResult {
        let (_temp, mut app, session) = app_with_plan_session()?;
        apply_plan_command(&mut app, &PlanCommand::Enter);
        assert_eq!(app.active_mode(), InteractionMode::Plan);
        assert!(session.lock().is_locked());
        Ok(())
    }

    /// Plan Teil F: „`plan.exit` wechselt Modus und Freigabe nur nach
    /// Bestätigung“ und „Die Ablehnung schickt das Feedback weiter“ (die
    /// Rückmeldung selbst prüft `harw-tool-plan`; hier: kein Wechsel).
    #[tokio::test]
    async fn plan_exit_switches_mode_only_after_confirmation() -> TestResult {
        let (_temp, mut app, session) = app_with_plan_session()?;
        app.enter_plan_stage();
        let (prompt, _decision) = PlanExitPrompt::new(
            "auth".to_owned(),
            PathBuf::from("/p/.harw/plans/auth.md"),
            "# Plan".to_owned(),
        );
        app.plan_ui
            .accept(PlanUiRequest::ExitPlan(prompt), Instant::now());
        assert!(app.plan_ui.is_open());
        // Ablehnung: Plan-Modus bleibt.
        apply_action(&mut app, PlanDialogAction::KeepPlanning);
        assert_eq!(app.active_mode(), InteractionMode::Plan);
        assert!(session.lock().is_locked());
        assert!(app.pending_turns.is_empty());
        // Schließen ohne Entscheidung: nichts ändert sich.
        apply_action(&mut app, PlanDialogAction::Closed);
        assert_eq!(app.active_mode(), InteractionMode::Plan);
        // Freigabe (Option 1): work + Folge-Nachricht, Sperre gelöst.
        apply_action(
            &mut app,
            PlanDialogAction::ImplementAuto {
                display_path: ".harw/plans/auth.md".to_owned(),
            },
        );
        assert_eq!(app.active_mode(), InteractionMode::Work);
        assert!(!session.lock().is_locked());
        assert_eq!(
            app.pending_turns.front().map(String::as_str),
            Some(implementation_message(".harw/plans/auth.md").as_str())
        );
        assert!(
            close_open(&mut app),
            "Turn-Ende schließt das offene Fenster"
        );
        assert!(!app.plan_ui.is_open());
        Ok(())
    }

    #[test]
    fn plan_enter_switches_only_on_yes() -> TestResult {
        let (_temp, mut app, session) = app_with_plan_session()?;
        apply_action(&mut app, PlanDialogAction::EnterDeclined);
        assert_ne!(app.active_mode(), InteractionMode::Plan);
        assert!(!session.lock().is_locked());
        apply_action(&mut app, PlanDialogAction::EnterPlan);
        assert_eq!(app.active_mode(), InteractionMode::Plan);
        assert!(session.lock().is_locked());
        Ok(())
    }

    #[test]
    fn editor_command_is_never_empty() {
        let (program, _args) = editor_command();
        assert!(!program.is_empty());
    }
}

//! `!`-Befehle der Nutzerin: sofort, auf dem Host, asynchron (Runde 6,
//! Teil B).
//!
//! # Verantwortungsbereich
//! - [`intercept`] fängt jede abgeschickte `!`/`!!`-Zeile ab — im Leerlauf
//!   (`HarwEvent::Command` in `run_loop`) wie während eines Turns
//!   (`route_busy_command`), also **immediate**, nie zurückgestellt. Die
//!   Zulassung macht [`crate::command_exec::admit_shell_line`]
//!   (`HARW_DISABLE_SHELL`, Operator-Stufe, `!!`-Auflösung).
//! - [`start`] zeigt „! <cmd> läuft …“ und startet den Befehl über
//!   [`harw_tool_shell::OperatorCommand`] als eigenen Tokio-Task auf dem
//!   Host (echtes `HOME`, cwd = Projektwurzel, Zeitlimit aus
//!   `[shell] max_timeout_secs`, sonst 600 s). Die TUI blockiert nie.
//! - Erst **nach dem Ende** kommt das Ergebnis zurück ([`poll_idle`],
//!   [`poll_busy`]): Ausgabe und Abschlusszeile („auf dem Host ausgeführt,
//!   cwd …“) im Verlauf, ein eigener Exporteintrag, und die Nachricht an den
//!   Agenten — im Leerlauf als Folge-Turn (`queue_shell_follow_up_turn`),
//!   während eines Turns als Kontext des nächsten Turns
//!   ([`super::background_agents::enqueue_background_notice`] ohne
//!   Auto-Turn). Ein halbes Ergebnis gibt es nie.
//!
//! # Nebenläufigkeit
//! Die Tasks teilen nichts mit der App; ihr Ergebnis kommt über einen
//! unbeschränkten Kanal zurück, ein [`FrameRequester`] weckt die
//! Ereignisschleife.

use std::time::Duration;

use harw_operations::PermissionTier;
use harw_tool_shell::{OPERATOR_DEFAULT_TIMEOUT_SECS, OperatorOutcome};
use harw_types::cancel::{CancelReason, CancelToken};
use ratatui::text::Line;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{ChatApp, build_shell_turn_message, queue_shell_follow_up_turn};
use crate::command_exec::{self, ShellAdmission};
use crate::frame_requester::FrameRequester;

/// Ein fertiger `!`-Befehl.
struct OperatorDone {
    /// Laufende Nummer (Zuordnung zu [`OperatorShellJobs::running`]).
    id: u64,
    /// Das vollständige Ergebnis.
    outcome: OperatorOutcome,
}

/// Laufende `!`-Befehle und ihr Ergebniskanal.
pub(super) struct OperatorShellJobs {
    /// Sender, den jeder Task klont.
    tx: UnboundedSender<OperatorDone>,
    /// Empfänger; geleert von [`poll_idle`] bzw. [`poll_busy`].
    rx: UnboundedReceiver<OperatorDone>,
    /// Laufende Befehle (Nummer, Befehlstext, Abbruch-Token für Ctrl+C).
    running: Vec<(u64, String, CancelToken)>,
    /// Nächste laufende Nummer.
    next_id: u64,
    /// Weckt die Ereignisschleife nach einem Ergebnis.
    waker: Option<FrameRequester>,
    /// Nur Tests: ohne Runtime-Montage mit Operator-Stufe zulassen.
    #[cfg(test)]
    allow_without_runtime: bool,
}

impl std::fmt::Debug for OperatorShellJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorShellJobs")
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl OperatorShellJobs {
    /// Leerer Zustand mit frischem Kanal.
    pub(super) fn new() -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            tx,
            rx,
            running: Vec::new(),
            next_id: 0,
            waker: None,
            #[cfg(test)]
            allow_without_runtime: false,
        }
    }

    /// Hinterlegt den Frame-Anforderer, über den ein fertiger Befehl die
    /// Ereignisschleife weckt.
    pub(super) fn set_waker(&mut self, waker: FrameRequester) {
        self.waker = Some(waker);
    }

    /// Anzahl laufender `!`-Befehle.
    #[cfg(test)]
    pub(super) fn running(&self) -> usize {
        self.running.len()
    }

    /// Bricht alle laufenden `!`-Befehle ab (Ctrl+C).
    ///
    /// # Beschreibung
    /// Löst das Abbruch-Token jedes laufenden Befehls aus; der Task beendet
    /// den Prozessbaum und liefert sein Ergebnis wie gewohnt über den Kanal
    /// (Ende „abgebrochen“, Teilausgabe erhalten).
    ///
    /// # Rückgabe
    /// Die Befehlstexte der abgebrochenen Befehle; leer, wenn keiner lief.
    pub(super) fn cancel_running(&mut self) -> Vec<String> {
        self.running
            .iter()
            .filter(|(_, _, cancel)| !cancel.is_cancelled())
            .map(|(_, command, cancel)| {
                cancel.cancel(CancelReason::User);
                command.clone()
            })
            .collect()
    }

    /// Nur Tests: Zulassung ohne Runtime-Montage (Operator-Stufe).
    #[cfg(test)]
    pub(super) fn allow_without_runtime(&mut self) {
        self.allow_without_runtime = true;
    }
}

/// Fängt eine abgeschickte `!`/`!!`-Zeile ab und startet sie sofort.
///
/// # Rückgabe
/// `true`, wenn die Zeile behandelt wurde (gestartet oder mit Meldung
/// abgelehnt); `false` für alles andere — dann gilt der normale
/// Befehlspfad. Ohne Runtime-Montage ebenfalls `false` (der normale Pfad
/// meldet dann „keine Runtime-Montage“).
pub(super) fn intercept(app: &mut ChatApp, raw: &str) -> bool {
    if !raw.starts_with('!') {
        return false;
    }
    let Some(tier) = caller_tier(app) else {
        return false;
    };
    let admission = command_exec::admit_shell_line(
        app.adapters(),
        tier,
        raw,
        app.last_shell_command.as_deref(),
    );
    match admission {
        ShellAdmission::NotShell => false,
        ShellAdmission::Rejected(text) => {
            app.push_system_text_exported(&text);
            true
        }
        ShellAdmission::Run(command) => {
            start(app, command);
            true
        }
    }
}

/// Stufe der Aufruferin aus der Runtime-Montage.
fn caller_tier(app: &ChatApp) -> Option<PermissionTier> {
    if let Some(rt) = app.runtime() {
        return Some(crate::runtime_commands::caller_tier(rt.principal()));
    }
    #[cfg(test)]
    {
        if app.operator_shell.allow_without_runtime {
            return Some(PermissionTier::Operator);
        }
    }
    None
}

/// Zeitlimit eines `!`-Befehls: `[shell] max_timeout_secs` der Konfig,
/// ohne Montage [`OPERATOR_DEFAULT_TIMEOUT_SECS`].
fn operator_timeout(app: &ChatApp) -> Duration {
    let secs = app.runtime().map_or(OPERATOR_DEFAULT_TIMEOUT_SECS, |rt| {
        rt.config().harness.shell.effective_max_timeout_secs()
    });
    Duration::from_secs(secs)
}

/// Startet `command` als eigenen Task auf dem Host und zeigt „läuft …“.
///
/// # Beschreibung
/// Merkt den Befehl sofort als `last_shell_command` (für `!!`), auch wenn
/// er noch läuft. Kehrt sofort zurück.
///
/// # Nebenläufigkeit
/// Muss in einer Tokio-Runtime laufen (`tokio::spawn`).
pub(super) fn start(app: &mut ChatApp, command: String) {
    let cancel = CancelToken::new();
    let request = command_exec::operator_command(app.sandbox(), &command, operator_timeout(app))
        .with_cancel(cancel.clone());
    let cwd = request.cwd().display().to_string();
    app.last_shell_command = Some(command.clone());
    app.push_lines(vec![Line::from(format!(
        "! {command} läuft … (auf dem Host, cwd {cwd})"
    ))]);

    let jobs = &mut app.operator_shell;
    let id = jobs.next_id;
    jobs.next_id = jobs.next_id.wrapping_add(1);
    jobs.running.push((id, command, cancel));
    let tx = jobs.tx.clone();
    let waker = jobs.waker.clone();
    tracing::debug!(id, "tui.operator_shell.started");
    tokio::spawn(async move {
        let outcome = request.run().await;
        let _ = tx.send(OperatorDone { id, outcome });
        if let Some(waker) = waker {
            waker.schedule_frame();
        }
    });
}

/// Ctrl+C: bricht laufende `!`-Befehle ab, bevor es den Turn oder die
/// Sitzung trifft.
///
/// # Beschreibung
/// Läuft mindestens ein `!`-Befehl, wird nur er abgebrochen und eine
/// Hinweiszeile gezeigt; ein laufender Agenten-Turn arbeitet weiter und die
/// Beenden-Scharfstellung bleibt aus. Erst ein weiteres Ctrl+C gilt wieder
/// dem Turn bzw. dem Beenden.
///
/// # Rückgabe
/// `true`, wenn ein `!`-Befehl abgebrochen wurde.
pub(super) fn cancel_on_ctrl_c(app: &mut ChatApp) -> bool {
    let cancelled = app.operator_shell.cancel_running();
    if cancelled.is_empty() {
        return false;
    }
    let lines = cancelled
        .into_iter()
        .map(|command| Line::from(format!("! {command} wird abgebrochen (Ctrl+C) …")))
        .collect();
    app.push_lines(lines);
    true
}

/// Holt alle fertigen Befehle ab, zeigt und exportiert sie.
///
/// # Rückgabe
/// Je gestartetem Befehl das strukturierte Ergebnis für die Nachricht an
/// den Agenten (abgelehnte Befehle liefern keins).
fn take_finished(app: &mut ChatApp) -> (bool, Vec<command_exec::ShellRunOutcome>) {
    let mut changed = false;
    let mut results = Vec::new();
    while let Ok(done) = app.operator_shell.rx.try_recv() {
        changed = true;
        app.operator_shell
            .running
            .retain(|(running, _, _)| *running != done.id);
        let outcome = done.outcome;
        let text = command_exec::operator_display_text(&outcome);
        let mut lines = vec![Line::from(format!("! {}", outcome.command))];
        lines.extend(text.split('\n').map(|line| Line::from(line.to_owned())));
        match command_exec::shell_run_outcome(&outcome) {
            Some(shell) => {
                // Anzeige und genau ein eigener Exporteintrag (Runde 6,
                // Teil C) — nicht `push_lines_exported`, sonst doppelt.
                app.push_lines(lines);
                super::export_capture::export_shell_result(
                    app,
                    &shell.command,
                    Some(shell.exit_code),
                    &shell.combined_output,
                );
                results.push(shell);
            }
            None => app.push_lines_exported(lines),
        }
    }
    (changed, results)
}

/// Leerlauf: fertige Befehle anzeigen und je einen Folge-Turn einreihen.
///
/// # Rückgabe
/// `true`, wenn sich Sichtbares geändert hat.
pub(super) fn poll_idle(app: &mut ChatApp, submitted: &mut Option<String>) -> bool {
    let (changed, results) = take_finished(app);
    for shell in results {
        queue_shell_follow_up_turn(app, submitted, shell);
    }
    changed
}

/// Während eines Turns: fertige Befehle anzeigen und als Kontext des
/// nächsten Turns einreihen (kein Auto-Turn, der laufende Turn bleibt
/// unberührt).
///
/// # Rückgabe
/// `true`, wenn sich Sichtbares geändert hat.
pub(super) fn poll_busy(app: &mut ChatApp) -> bool {
    let (changed, results) = take_finished(app);
    for shell in results {
        let message = build_shell_turn_message(&shell);
        super::background_agents::enqueue_background_notice(app, message, false);
    }
    changed
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use harw_types::cancel::CancelToken;

    use super::super::tests::test_chat_app;
    use super::super::{BusyKeyOutcome, route_busy_command};
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Wartet (höchstens 20 s), bis `poll` ein Ergebnis meldet.
    async fn wait_until(
        app: &mut ChatApp,
        mut poll: impl FnMut(&mut ChatApp) -> bool,
    ) -> TestResult {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if poll(app) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(TestError::Missing("Ergebnis des !-Befehls"))
    }

    /// Im Leerlauf läuft `!` asynchron auf dem Host; erst nach dem Ende
    /// entsteht der Folge-Turn, und er nennt „Host“, cwd und Exit-Code.
    #[tokio::test]
    async fn idle_bang_runs_on_host_and_queues_follow_up_after_end() -> TestResult {
        let mut app = test_chat_app()?;
        app.operator_shell.allow_without_runtime();

        assert!(intercept(&mut app, "! echo hallo-host"));
        assert_eq!(app.operator_shell.running(), 1);
        assert_eq!(app.last_shell_command.as_deref(), Some("echo hallo-host"));

        let mut submitted: Option<String> = None;
        wait_until(&mut app, |app| poll_idle(app, &mut submitted)).await?;

        let message = submitted.ok_or(TestError::Missing("Folge-Turn"))?;
        assert!(message.contains("auf dem Host ausgeführt"), "{message}");
        assert!(message.contains("Exit 0"), "{message}");
        assert!(message.contains("hallo-host"), "{message}");
        let cwd = app
            .sandbox()
            .workspace()
            .canonical_root()
            .display()
            .to_string();
        assert!(message.contains(&cwd), "{message}");
        assert_eq!(app.operator_shell.running(), 0);
        Ok(())
    }

    /// Während eines Turns läuft `!` sofort (nicht zurückgestellt); das
    /// Ergebnis wird erst nach dem Ende als Kontext des nächsten Turns
    /// eingereiht, nie vorher.
    #[tokio::test]
    async fn busy_bang_runs_immediately_and_is_queued_only_after_end() -> TestResult {
        let mut app = test_chat_app()?;
        app.operator_shell.allow_without_runtime();
        app.active_cancel = Some(CancelToken::new());

        let outcome = route_busy_command(&mut app, "!sleep 0.3; echo busy-fertig".to_owned());
        assert_eq!(outcome, BusyKeyOutcome::Redraw);
        assert!(
            app.deferred_input.is_empty(),
            "! darf nicht zurückgestellt werden"
        );
        assert!(app.busy_jobs.queued().is_empty());
        assert_eq!(app.operator_shell.running(), 1);

        // Noch läuft der Befehl: nichts eingereiht.
        assert!(!poll_busy(&mut app));
        let unchanged =
            super::super::background_agents::attach_queued_notices(&mut app, "x".to_owned());
        assert_eq!(unchanged, "x", "vor dem Ende darf nichts eingereiht sein");

        wait_until(&mut app, poll_busy).await?;
        assert!(
            app.pending_turns.is_empty(),
            "kein Auto-Turn während des Turns"
        );
        let next =
            super::super::background_agents::attach_queued_notices(&mut app, "weiter".to_owned());
        assert!(next.contains("auf dem Host ausgeführt"), "{next}");
        assert!(next.contains("busy-fertig"), "{next}");
        assert!(next.contains("weiter"), "{next}");
        Ok(())
    }

    /// Ctrl+C während eines Turns bricht zuerst nur den `!`-Befehl ab: Der
    /// Turn läuft weiter, das Ergebnis meldet den Abbruch.
    #[tokio::test]
    async fn ctrl_c_cancels_a_running_bang_before_the_turn() -> TestResult {
        let mut app = test_chat_app()?;
        app.operator_shell.allow_without_runtime();
        let turn = CancelToken::new();
        app.active_cancel = Some(turn.clone());

        let outcome = route_busy_command(&mut app, "! echo vorher; sleep 30".to_owned());
        assert_eq!(outcome, BusyKeyOutcome::Redraw);
        assert_eq!(app.operator_shell.running(), 1);

        let started = Instant::now();
        assert!(cancel_on_ctrl_c(&mut app));
        assert!(!turn.is_cancelled(), "der Turn läuft weiter");
        assert!(
            !cancel_on_ctrl_c(&mut app),
            "ein zweites Ctrl+C gilt wieder dem Turn"
        );

        wait_until(&mut app, poll_busy).await?;
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(app.operator_shell.running(), 0);
        let next =
            super::super::background_agents::attach_queued_notices(&mut app, "weiter".to_owned());
        assert!(next.contains("abgebrochen"), "{next}");
        Ok(())
    }

    /// Ohne laufenden `!`-Befehl lässt Ctrl+C alles beim Alten.
    #[tokio::test]
    async fn ctrl_c_without_running_bang_is_not_claimed() -> TestResult {
        let mut app = test_chat_app()?;
        assert!(!cancel_on_ctrl_c(&mut app));
        Ok(())
    }

    /// `sudo` wird ohne Start abgelehnt; es folgt kein Turn.
    #[tokio::test]
    async fn bang_sudo_is_rejected_without_follow_up() -> TestResult {
        let mut app = test_chat_app()?;
        app.operator_shell.allow_without_runtime();

        assert!(intercept(&mut app, "!sudo ls"));
        let mut submitted: Option<String> = None;
        wait_until(&mut app, |app| poll_idle(app, &mut submitted)).await?;
        assert!(submitted.is_none());
        assert!(app.pending_turns.is_empty());
        Ok(())
    }

    /// `!!` ohne Vorgänger meldet sich sofort, ohne etwas zu starten.
    #[tokio::test]
    async fn bang_repeat_without_previous_command_reports_hint() -> TestResult {
        let mut app = test_chat_app()?;
        app.operator_shell.allow_without_runtime();

        assert!(intercept(&mut app, "! !"));
        assert_eq!(app.operator_shell.running(), 0);
        Ok(())
    }

    /// Keine `!`-Zeile, oder keine Montage: der normale Pfad bleibt zuständig.
    #[tokio::test]
    async fn non_bang_lines_and_missing_runtime_are_not_intercepted() -> TestResult {
        let mut app = test_chat_app()?;
        assert!(!intercept(&mut app, "!echo ohne-montage"));
        app.operator_shell.allow_without_runtime();
        assert!(!intercept(&mut app, "/status"));
        assert!(!intercept(&mut app, "\\!kein befehl"));
        Ok(())
    }
}

//! Freigabe-Fragen von Kind-Agenten im Freigabedialog (Runde 5, Teil O).
//!
//! # Beschreibung
//! Gegenstück zu `harw_core::child_approval`: die TUI bindet beim Bau der
//! Wurzel-Laufzeit einen [`TuiChildApprovalBroker`] an den Spawner
//! ([`attach`]). Braucht ein Kind eine Freigabe, landet die Frage über einen
//! Kanal hier und erscheint im **selben** Freigabedialog wie Fragen der
//! Wurzel ([`crate::approval_dialog::ApprovalDialog`], Slot
//! `ChatApp::pending_approval_dialog`), mit der Zeile
//! „angefragt von: <rolle> (<pfad im baum>)“ und dem Countdown des
//! Zeitlimits (Vorgabe 10 min).
//!
//! - **Während eines Turns** pollen `drive_turn_animated` und
//!   `drive_pauses_to_completion` den Kanal ([`ChildApprovalUi::recv`],
//!   [`accept`]) und leiten Tasten an [`route_event`], solange die Frage offen
//!   ist.
//! - **Im Leerlauf** (Hintergrund-Agenten, Runde 5 Teil K) holt [`poll`] im
//!   selben 500-ms-Takt wie sudo/Host-Permit ab; [`route_event`] verbraucht
//!   die Tasten.
//! - Eine Frage der Wurzel hat Vorrang: [`yield_to_root`] stellt eine offene
//!   Kind-Frage zurück in die Warteschlange, [`show_next`] zeigt sie wieder,
//!   sobald der Dialog frei ist.
//! - Gibt das Kind auf (Zeitablauf im Kern, Abbruch), ist der Antwortkanal
//!   geschlossen; [`poll`] schließt die Frage dann still.
//!
//! Der Dialog bietet dieselben Optionen wie für die Wurzel; jede Zustimmung
//! gilt aber nur für **diesen einen** Aufruf (keine Regel, kein
//! Moduswechsel) — ein Kind soll über eine Freigabe nie mehr Rechte
//! bekommen, als seine Rolle ohnehin hat.
//!
//! # Nebenläufigkeit
//! Der Broker ist `Send + Sync` (nur ein `UnboundedSender`); alles andere
//! läuft im Task der TUI.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use harw_core::ManagedAgentSpawner;
use harw_core::child_approval::{ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest};
use harw_extension_api::approval_mode::ApprovalMode;
use harw_runtime::AutoModeHandle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use super::{ChatApp, Role, approval_dialog_key_is_armed};
use crate::approval_dialog::{ApprovalChoice, ApprovalDialog, ApprovalDialogRequest, DialogAction};
use crate::tui_event::TuiEvent;

/// Eine zugestellte Frage samt Antwortkanal.
#[derive(Debug)]
pub(crate) struct ChildApprovalPrompt {
    /// Die Frage des Kindes.
    pub(crate) request: ChildApprovalRequest,
    /// Antwort an das wartende Kind.
    reply: oneshot::Sender<ChildApprovalAnswer>,
}

impl ChildApprovalPrompt {
    /// Ob das Kind noch auf die Antwort wartet.
    fn is_waiting(&self) -> bool {
        !self.reply.is_closed()
    }

    /// Beantwortet die Frage (ein aufgegebenes Kind ignoriert das).
    fn answer(self, answer: ChildApprovalAnswer) {
        let _ = self.reply.send(answer);
    }
}

/// Der Broker, den die TUI an den Spawner bindet.
#[derive(Debug)]
pub(crate) struct TuiChildApprovalBroker {
    sender: UnboundedSender<ChildApprovalPrompt>,
}

impl ChildApprovalBroker for TuiChildApprovalBroker {
    fn submit(
        &self,
        request: ChildApprovalRequest,
    ) -> Option<oneshot::Receiver<ChildApprovalAnswer>> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(ChildApprovalPrompt { request, reply })
            .ok()?;
        Some(receiver)
    }
}

/// Zustand der Kind-Freigaben in der TUI (Feld `ChatApp::child_approvals`).
#[derive(Debug, Default)]
pub(crate) struct ChildApprovalUi {
    receiver: Option<UnboundedReceiver<ChildApprovalPrompt>>,
    queue: VecDeque<ChildApprovalPrompt>,
    /// Die gerade im Dialog gezeigte Frage und seit wann.
    open: Option<(ChildApprovalPrompt, Instant)>,
}

impl ChildApprovalUi {
    /// Baut den Zustand samt Broker für den Spawner.
    #[must_use]
    pub(crate) fn channel() -> (Self, TuiChildApprovalBroker) {
        let (sender, receiver) = unbounded_channel();
        (
            Self {
                receiver: Some(receiver),
                ..Self::default()
            },
            TuiChildApprovalBroker { sender },
        )
    }

    /// Ob der Kanal gepollt werden soll.
    #[must_use]
    pub(crate) fn is_listening(&self) -> bool {
        self.receiver.is_some()
    }

    /// Wartet auf die nächste Frage (für `tokio::select!`).
    pub(crate) async fn recv(&mut self) -> Option<ChildApprovalPrompt> {
        match self.receiver.as_mut() {
            Some(receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Ob gerade eine Kind-Frage im Dialog steht.
    #[must_use]
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Ob für das Kind `child` eine Frage offen ist oder wartet (PL-97 C5:
    /// eine wartende Freigabe hebt seine Live-Zelle auf `NeedsOperator`).
    #[must_use]
    pub(crate) fn is_pending_for(&self, child: &str) -> bool {
        let asks = |prompt: &ChildApprovalPrompt| {
            prompt.request.child.as_str() == child && prompt.is_waiting()
        };
        self.open.as_ref().is_some_and(|(prompt, _)| asks(prompt)) || self.queue.iter().any(asks)
    }

    /// Zahl der wartenden (noch nicht gezeigten) Fragen.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn queued(&self) -> usize {
        self.queue.len()
    }

    #[cfg(test)]
    pub(crate) fn backdate_open(&mut self, by: Duration) {
        if let Some((_, shown)) = self.open.as_mut()
            && let Some(earlier) = shown.checked_sub(by)
        {
            *shown = earlier;
        }
    }
}

/// Bindet den Freigabe-Kanal an (Montage der Wurzel-Laufzeit).
///
/// # Beschreibung
/// Ohne Spawner (keine Kindrollen) bleibt alles aus. Mit Auto-Modus wird
/// zusätzlich dessen Kind-Gate umgestellt: `ask` → Anfrage an die Nutzerin.
pub(crate) fn attach(
    app: &mut ChatApp,
    spawner: Option<&Arc<ManagedAgentSpawner>>,
    auto: Option<&AutoModeHandle>,
) {
    let Some(spawner) = spawner else {
        return;
    };
    let (ui, broker) = ChildApprovalUi::channel();
    spawner.attach_child_approval_broker(Arc::new(broker));
    if let Some(auto) = auto {
        auto.set_child_relay_available(true);
    }
    app.child_approvals = ui;
    tracing::info!("tui.child_approvals.attached");
}

/// Nimmt eine zugestellte Frage an und zeigt sie, sobald der Dialog frei ist.
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn accept(app: &mut ChatApp, prompt: Option<ChildApprovalPrompt>) -> bool {
    match prompt {
        Some(prompt) => {
            tracing::info!(
                child = %prompt.request.child,
                role = %prompt.request.role,
                tool = %prompt.request.call.name,
                "tui.child_approvals.prompt_received"
            );
            app.child_approvals.queue.push_back(prompt);
            show_next(app)
        }
        None => {
            app.child_approvals.receiver = None;
            false
        }
    }
}

/// Holt wartende Fragen ab (Leerlauf und Takt), schließt aufgegebene und
/// zeigt die nächste.
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(crate) fn poll(app: &mut ChatApp) -> bool {
    let mut changed = false;
    if let Some(receiver) = app.child_approvals.receiver.as_mut() {
        loop {
            match receiver.try_recv() {
                Ok(prompt) => app.child_approvals.queue.push_back(prompt),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    app.child_approvals.receiver = None;
                    break;
                }
            }
        }
    }
    // Aufgegebene Fragen (Zeitablauf im Kern, Abbruch des Kindes) fallen weg.
    app.child_approvals
        .queue
        .retain(ChildApprovalPrompt::is_waiting);
    if app
        .child_approvals
        .open
        .as_ref()
        .is_some_and(|(prompt, _)| !prompt.is_waiting())
    {
        if let Some((prompt, _)) = app.child_approvals.open.take() {
            app.pending_approval_dialog = None;
            app.push_line(
                Role::System,
                format!(
                    "Freigabe-Frage von {} erledigt (Zeitablauf oder Abbruch) – {} wurde nicht ausgeführt.",
                    prompt.request.requester_label(),
                    prompt.request.call.name
                ),
            );
        }
        changed = true;
    }
    show_next(app) || changed
}

/// Beantwortet unter „Full Access" jede wartende (und eine offene)
/// Kind-Frage selbst mit „freigeben" — ohne Dialog.
///
/// # Beschreibung
/// Nutzerentscheidung 2026-09-24: unter Full Access fragt harw nie. Kinder
/// folgen dem Modus der Wurzel und stellen dann gar keine Fragen mehr
/// (`harw_runtime::ApprovalChain::for_child`); das hier fängt nur Fragen ab,
/// die schon unterwegs waren, als die Nutzerin auf Full Access umschaltete.
///
/// # Rückgabe
/// `true`, wenn mindestens eine Frage beantwortet wurde (neu zeichnen).
fn approve_waiting_in_mode(app: &mut ChatApp, mode: Option<ApprovalMode>) -> bool {
    if mode != Some(ApprovalMode::FullAccess) {
        return false;
    }
    let mut prompts: Vec<ChildApprovalPrompt> = Vec::new();
    if let Some((prompt, _)) = app.child_approvals.open.take() {
        app.pending_approval_dialog = None;
        prompts.push(prompt);
    }
    prompts.extend(app.child_approvals.queue.drain(..));
    let mut answered = false;
    for prompt in prompts {
        if !prompt.is_waiting() {
            continue;
        }
        let line = format!(
            "Full Access: {} für {} ohne Rückfrage freigegeben.",
            prompt.request.call.name,
            prompt.request.requester_label()
        );
        tracing::info!(
            child = %prompt.request.child,
            tool = %prompt.request.call.name,
            "tui.child_approvals.full_access_approved"
        );
        prompt.answer(ChildApprovalAnswer::Approve);
        app.push_line(Role::System, line);
        answered = true;
    }
    answered
}

/// Zeigt die nächste wartende Frage, wenn der Dialog frei ist.
///
/// # Rückgabe
/// `true`, wenn eine Frage geöffnet (oder unter „Full Access" ohne Dialog
/// beantwortet) wurde.
pub(crate) fn show_next(app: &mut ChatApp) -> bool {
    let mode = app.current_approval();
    if approve_waiting_in_mode(app, mode) {
        return true;
    }
    if app.child_approvals.open.is_some()
        || app.pending_approval_dialog.is_some()
        || app.pending_host_permit.is_some()
        || app.sudo.is_open()
        || app.plan_ui.is_open()
    {
        return false;
    }
    while let Some(prompt) = app.child_approvals.queue.pop_front() {
        if !prompt.is_waiting() {
            continue;
        }
        app.pending_approval_dialog = Some(build_dialog(app, &prompt.request));
        app.child_approvals.open = Some((prompt, Instant::now()));
        return true;
    }
    false
}

/// Stellt eine offene Kind-Frage zurück, weil die Wurzel den Dialog braucht.
pub(crate) fn yield_to_root(app: &mut ChatApp) {
    if let Some((prompt, _)) = app.child_approvals.open.take() {
        app.pending_approval_dialog = None;
        app.child_approvals.queue.push_front(prompt);
    }
}

/// Ob gerade eine Kind-Frage den Dialog belegt.
#[must_use]
pub(crate) fn is_open(app: &ChatApp) -> bool {
    app.child_approvals.is_open()
}

/// Leitet ein Ereignis an die offene Kind-Frage.
///
/// # Rückgabe
/// - `Ok(redraw)`: verbraucht (der Dialog ist modal: nichts erreicht den
///   Composer, solange er offen ist).
/// - `Err(event)`: keine Kind-Frage offen, oder Zeichnen/Größe/Maus — normal
///   weiter.
pub(crate) fn route_event(app: &mut ChatApp, event: TuiEvent) -> Result<bool, TuiEvent> {
    if !is_open(app) {
        return Err(event);
    }
    let key = match event {
        TuiEvent::Key(key) => key,
        // Pastes beantworten nie eine Freigabe.
        TuiEvent::Paste(_) => return Ok(false),
        other => return Err(other),
    };
    // Ctrl+C lehnt fail-safe sofort ab, unabhängig vom Arming-Delay.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
    {
        decide(app, ApprovalChoice::Reject { reason: None });
        return Ok(true);
    }
    let since_shown = app
        .child_approvals
        .open
        .as_ref()
        .map_or(Duration::ZERO, |(_, shown)| shown.elapsed());
    let armed = approval_dialog_key_is_armed(key, since_shown);
    let Some(dialog) = app.pending_approval_dialog.as_mut() else {
        return Ok(false);
    };
    match dialog.handle_key(key, armed) {
        DialogAction::Stay | DialogAction::ToggleDetails | DialogAction::Scrolled => Ok(true),
        DialogAction::Decided(choice) => {
            decide(app, choice);
            Ok(true)
        }
    }
}

/// Setzt eine Entscheidung um und zeigt die nächste Frage.
fn decide(app: &mut ChatApp, choice: ApprovalChoice) {
    let Some((prompt, _)) = app.child_approvals.open.take() else {
        return;
    };
    app.pending_approval_dialog = None;
    let requester = prompt.request.requester_label();
    let tool = prompt.request.call.name.as_str().to_owned();
    let answer = answer_for(choice);
    let line = match &answer {
        ChildApprovalAnswer::Approve => {
            format!("✓ {tool} für {requester} einmalig freigegeben.")
        }
        ChildApprovalAnswer::Reject { .. } => format!("✗ {tool} für {requester} abgelehnt."),
    };
    tracing::info!(
        child = %prompt.request.child,
        tool = %tool,
        approved = matches!(answer, ChildApprovalAnswer::Approve),
        "tui.child_approvals.decided"
    );
    prompt.answer(answer);
    app.push_line(Role::System, line);
    show_next(app);
}

/// Übersetzt die Dialogwahl: jede Zustimmung gilt nur für diesen Aufruf.
#[must_use]
pub(crate) fn answer_for(choice: ApprovalChoice) -> ChildApprovalAnswer {
    match choice {
        ApprovalChoice::Approve
        | ApprovalChoice::ApproveAndRemember(_)
        | ApprovalChoice::ApproveAndAutoMode
        | ApprovalChoice::ApproveAndLearn { .. } => ChildApprovalAnswer::Approve,
        ApprovalChoice::Reject { reason } => ChildApprovalAnswer::Reject { reason },
    }
}

/// Baut den Freigabedialog mit Absender, Countdown und — falls der Kern einen
/// kennt — dem Grund der Rückfrage (PL-100 O1: DoD-Taint des Kind-Turns,
/// [`ChildApprovalRequest::reason`]).
fn build_dialog(app: &ChatApp, request: &ChildApprovalRequest) -> ApprovalDialog {
    ApprovalDialog::new(ApprovalDialogRequest {
        call: request.call.clone(),
        cwd: if app.project_root().is_empty() {
            None
        } else {
            Some(app.project_root().to_owned())
        },
        // Bleibt der Begründung des Agenten vorbehalten; der vom Kern
        // zugeschriebene Grund hat eine eigene Zeile (`with_core_reason`).
        justification: None,
        risk: crate::app::approval_risk(&request.call),
        origin: Some(request.requester_label()),
        // Keine Regel, kein Lern-Angebot: eine Kind-Freigabe gilt einmalig.
        remember_rule: None,
        deadline: Instant::now() + request.timeout,
        reason_input_enabled: true,
    })
    .once_only()
    // PL-100 O1: der Taint-Grund des Kind-Turns (bereits escapt,
    // `harw_core::dod_taint::ask_reason`) steht unter eigener Beschriftung
    // vor dem Grund des Auto-Modus; fragen beide, zeigt der Dialog beide.
    .with_core_reason(request.reason.clone())
    // Runde 6, Teil A: Grund des Auto-Modus (auch ein umgewandeltes deny).
    .with_auto_reason(
        app.runtime()
            .and_then(|rt| rt.auto_mode())
            .and_then(|auto| auto.log().verdict_for(request.call.id.as_str()))
            .as_ref()
            .and_then(crate::permissions_view::auto_ask_reason_for),
    )
}

#[cfg(test)]
mod tests;

// PL-100 O1: der vom Kern zugeschriebene Grund (DoD-Taint) erscheint im
// Freigabedialog, für Kind- und Wurzel-Fragen. Eigenes Testmodul neben
// `tests`, damit dessen Fixtures unverändert bleiben; `build_approval_dialog`
// ist als privates Element von `app` hier sichtbar.
#[cfg(test)]
mod reason_tests {
    use std::time::Duration;

    use harw_core::child_approval::ChildApprovalRequest;
    use harw_core::dod_taint::{TAINT_REASON, ask_reason};
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{ItemId, SessionId, ToolCallId};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::super::build_approval_dialog;
    use super::super::tests::test_chat_app;
    use super::build_dialog;
    use crate::approval::{ApprovalPrompt, ApprovalScope, TuiApprovalHandler};
    use crate::approval_dialog::ApprovalDialog;
    use crate::test_support::{TestError, TestResult};

    // Beschriftung der Zeile mit dem Kern-Grund (englischer Katalogtext von
    // `dialogs.core_reason`, wie `info_fields` sie mit „: “ verbindet).
    const CORE_REASON_LABEL: &str = "Security rule: ";

    // Beschriftung der vom Agenten gelieferten Begründung.
    const JUSTIFICATION_LABEL: &str = "Justification: ";

    // Ein Grund des Auto-Modus in der Form „<Kategorie> – <Grund>“.
    const AUTO_REASON: &str = "eingriff – stoppt einen laufenden Job";

    // Wartezeit der Wurzel-Dialoge in diesen Tests.
    const ROOT_TIMEOUT: Duration = Duration::from_secs(60);

    // Der Aufruf, um den es in allen Fragen geht.
    fn stop_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("job.stop"),
            arguments: serde_json::json!({ "id": "job-1" }),
        }
    }

    // Eine Kind-Frage für `job.stop` mit dem Grund `reason`.
    fn request(reason: Option<String>) -> ChildApprovalRequest {
        ChildApprovalRequest {
            child: SessionId::new(),
            role: "egress-triage".to_owned(),
            tree_path: vec!["egress-triage".to_owned()],
            call: stop_call(),
            timeout: Duration::from_secs(600),
            reason,
        }
    }

    // Eine Wurzel-Frage für `job.stop` mit dem Grund `reason`, gestellt über
    // denselben Weg wie `ApprovalDriver::drive_to_completion`.
    fn root_prompt(reason: Option<String>) -> TestResult<ApprovalPrompt> {
        let (handler, mut prompts) = TuiApprovalHandler::with_scope(ApprovalScope::AllTools);
        if !handler.open_prompt_with_reason(&ItemId::new(), &stop_call(), reason) {
            return Err(TestError::Unexpected(
                "the prompt must reach the renderer".to_owned(),
            ));
        }
        prompts
            .try_recv()
            .map_err(|_| TestError::Missing("die gestellte Wurzel-Frage"))
    }

    // Zeichnet den Dialog in voller Höhe und liefert den sichtbaren Text.
    fn rendered(dialog: &ApprovalDialog) -> String {
        let area = Rect::new(0, 0, 140, dialog.desired_height(140));
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, &crate::style::Theme::Dark);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf.cell((x, y)).map_or(" ", |cell| cell.symbol()));
            }
            text.push('\n');
        }
        text
    }

    // Byte-Position von `needle` im gezeichneten Text.
    fn position(text: &str, needle: &str) -> TestResult<usize> {
        text.find(needle)
            .ok_or(TestError::Missing("erwartete Zeile im Freigabedialog"))
    }

    /// Kind-Frage: der Taint-Grund steht unter eigener Beschriftung, nicht
    /// als Begründung des Agenten; ohne Grund fehlt die Zeile.
    #[test]
    fn the_taint_reason_of_a_child_request_is_shown_in_the_dialog() -> TestResult {
        let app = test_chat_app()?;
        let reason = ask_reason("job.stop");

        let tainted = rendered(&build_dialog(&app, &request(Some(reason.clone()))));
        assert!(
            tainted.contains(&format!("{CORE_REASON_LABEL}{reason}")),
            "{tainted}"
        );
        assert!(!tainted.contains(JUSTIFICATION_LABEL), "{tainted}");
        assert!(tainted.contains("egress-triage"), "{tainted}");

        let plain = rendered(&build_dialog(&app, &request(None)));
        assert!(!plain.contains(TAINT_REASON), "{plain}");
        assert!(!plain.contains(CORE_REASON_LABEL), "{plain}");
        Ok(())
    }

    /// Wurzel-Frage: `build_approval_dialog` zeigt den Grund des Prompts
    /// unter eigener Beschriftung; ohne Grund fehlt die Zeile.
    #[test]
    fn the_taint_reason_of_a_root_prompt_is_shown_in_the_dialog() -> TestResult {
        let app = test_chat_app()?;
        let reason = ask_reason("job.stop");

        let tainted = rendered(&build_approval_dialog(
            &root_prompt(Some(reason.clone()))?,
            &app,
            ROOT_TIMEOUT,
        ));
        assert!(
            tainted.contains(&format!("{CORE_REASON_LABEL}{reason}")),
            "{tainted}"
        );
        assert!(!tainted.contains(JUSTIFICATION_LABEL), "{tainted}");

        let plain = rendered(&build_approval_dialog(
            &root_prompt(None)?,
            &app,
            ROOT_TIMEOUT,
        ));
        assert!(!plain.contains(TAINT_REASON), "{plain}");
        assert!(!plain.contains(CORE_REASON_LABEL), "{plain}");
        Ok(())
    }

    /// Fragen Taint und Auto-Modus, zeigt der Dialog beide Zeilen — den
    /// Taint-Grund zuerst, im Kind- wie im Wurzel-Dialog.
    #[test]
    fn the_taint_reason_comes_before_the_auto_mode_reason() -> TestResult {
        let app = test_chat_app()?;
        let reason = ask_reason("job.stop");

        let child = rendered(
            &build_dialog(&app, &request(Some(reason.clone())))
                .with_auto_reason(Some(AUTO_REASON.to_owned())),
        );
        let root = rendered(
            &build_approval_dialog(&root_prompt(Some(reason))?, &app, ROOT_TIMEOUT)
                .with_auto_reason(Some(AUTO_REASON.to_owned())),
        );
        for text in [child, root] {
            let core = position(&text, CORE_REASON_LABEL)?;
            let auto = position(&text, AUTO_REASON)?;
            assert!(core < auto, "{text}");
        }
        Ok(())
    }
}

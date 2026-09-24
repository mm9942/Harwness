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
        DialogAction::Stay | DialogAction::ToggleDetails => Ok(true),
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

/// Baut den Freigabedialog mit Absender und Countdown.
fn build_dialog(app: &ChatApp, request: &ChildApprovalRequest) -> ApprovalDialog {
    ApprovalDialog::new(ApprovalDialogRequest {
        call: request.call.clone(),
        cwd: if app.project_root().is_empty() {
            None
        } else {
            Some(app.project_root().to_owned())
        },
        justification: None,
        risk: crate::app::approval_risk(&request.call),
        origin: Some(request.requester_label()),
        // Keine Regel, kein Lern-Angebot: eine Kind-Freigabe gilt einmalig.
        remember_rule: None,
        deadline: Instant::now() + request.timeout,
        reason_input_enabled: true,
    })
    .once_only()
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

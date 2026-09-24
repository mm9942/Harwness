//! `Session`: eine laufende Konversation mit der Wurzelsitzung.
//!
//! # Beschreibung
//! [`Session::send`] fährt einen vollständigen Turn: Modellrunden,
//! Werkzeugaufrufe, Freigaben (über den [`ApprovalHandler`]) und
//! Kind-Agenten, bis der Turn regulär endet. Live-Ereignisse liefert
//! [`Session::events`] parallel dazu; [`CancelHandle`] bricht einen laufenden
//! Turn von außen ab.
//!
//! # Nebenläufigkeit
//! `send` nimmt `&mut self`: eine Sitzung fährt nie zwei Turns zugleich.
//! [`EventStream`] und [`CancelHandle`] sind eigenständig und dürfen in
//! anderen Tasks leben.

use std::sync::{Arc, Mutex, PoisonError};

use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::turn_loop::TurnControl;
use harw_core::{
    AgentSession, ApprovalResolution, ModelMessage, ToolCallResult, TurnInput, TurnOutcome,
    resume_after_approval, resume_after_child, run_turn,
};
use harw_protocol::{SessionEvent, TurnEvent, TurnItem};
use harw_runtime::RuntimeAssembly;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::approval::{ApprovalHandler, ApprovalRequest, Decision};
use crate::error::SdkError;
use crate::event::{EventStream, Usage, assistant_text};
use crate::ids::SessionId;

/// Obergrenze der Wiederaufnahmen (Freigaben + Kind-Agenten) eines Turns.
///
/// Schutz gegen einen Turn, der nie zur Ruhe kommt; jede reguläre Pause
/// verbraucht genau eine Wiederaufnahme.
pub const MAX_RESUMES_PER_TURN: usize = 64;

/// Wie ein Turn endete.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnStatus {
    /// Regulär abgeschlossen.
    Completed,
    /// Abgebrochen (über [`CancelHandle`] oder eine Budgetgrenze).
    Cancelled {
        /// Grund in Kurzform (`user`, `budget`, `shutdown`, …).
        reason: String,
    },
    /// Die Modellausgabe wurde abgeschnitten.
    Truncated,
    /// Das Modell hat abgelehnt.
    Refused {
        /// Angabe des Providers, falls vorhanden.
        detail: Option<String>,
    },
    /// Der Turn scheiterte in der Runtime.
    Failed {
        /// Menschenlesbarer Grund.
        reason: String,
    },
}

impl TurnStatus {
    /// `true` bei [`TurnStatus::Completed`].
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// Übersetzt den internen Ausgang eines zur Ruhe gekommenen Turns.
    pub(crate) fn from_outcome(outcome: TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed => Self::Completed,
            TurnOutcome::Cancelled { reason } => Self::Cancelled {
                reason: cancel_reason(reason),
            },
            TurnOutcome::Truncated => Self::Truncated,
            TurnOutcome::Refused { detail } => Self::Refused { detail },
            TurnOutcome::Failed { reason } => Self::Failed { reason },
            TurnOutcome::AwaitingApproval { .. } | TurnOutcome::AwaitingChild { .. } => {
                Self::Failed {
                    reason: "the turn is still paused".to_owned(),
                }
            }
        }
    }
}

/// Stabile Kurzform eines Abbruchgrunds.
fn cancel_reason(reason: CancelReason) -> String {
    match reason {
        CancelReason::User => "user",
        CancelReason::Parent => "parent",
        CancelReason::Budget => "budget",
        CancelReason::LeaseLost => "lease_lost",
        CancelReason::Shutdown => "shutdown",
    }
    .to_owned()
}

/// Ergebnis eines [`Session::send`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TurnReport {
    /// Die Sitzung.
    pub session_id: SessionId,
    /// Ausgang des Turns.
    pub status: TurnStatus,
    /// Die letzte Assistenten-Nachricht dieses Turns (bevorzugt die
    /// abschließende Antwort), falls es eine gab.
    pub text: Option<String>,
    /// Token-Nutzung dieses Turns (Wurzelsitzung).
    pub usage: Usage,
    /// Anzahl der Werkzeugaufrufe der Wurzelsitzung in diesem Turn.
    pub tool_calls: u32,
    /// Anzahl der dem [`ApprovalHandler`] vorgelegten Freigaben.
    pub approvals: u32,
}

/// Rolle einer gespeicherten Nachricht.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Role {
    /// Eingabe des Nutzers bzw. Einbettenden.
    User,
    /// Antwort des Modells.
    Assistant,
}

/// Eine Nachricht des Verlaufs (Werkzeugaufrufe ausgenommen).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Message {
    /// Rolle.
    pub role: Role,
    /// Text.
    pub text: String,
}

/// Bricht den laufenden Turn einer Sitzung ab; billig klonbar.
#[derive(Debug, Clone, Default)]
pub struct CancelHandle {
    slot: Arc<Mutex<Option<CancelToken>>>,
}

impl CancelHandle {
    /// Bricht den laufenden Turn ab.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Turn lief. Der Turn endet am nächsten Prüfpunkt mit
    /// [`TurnStatus::Cancelled`]; offene Werkzeugaufrufe bekommen ein
    /// synthetisches Fehlerergebnis.
    pub fn cancel(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.as_ref() {
            Some(token) => {
                token.cancel(CancelReason::User);
                true
            }
            None => false,
        }
    }

    fn arm(&self, token: CancelToken) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
    }

    fn disarm(&self) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Was nach einem Turn aus dem Turn-Kanal der Wurzel gelesen wurde.
#[derive(Debug, Default)]
struct Collected {
    final_text: Option<String>,
    last_text: Option<String>,
    tool_calls: u32,
}

/// Eine Konversation mit der Wurzelsitzung.
///
/// Beim Verwerfen meldet die Sitzung ihr Ende an die Runtime (Aufräumhaken,
/// Transkript). Der gespeicherte Verlauf bleibt erhalten und ist über
/// [`crate::Harwness::resume`] fortsetzbar.
pub struct Session {
    id: SessionId,
    root: AgentSession,
    assembly: RuntimeAssembly,
    approvals: Arc<dyn ApprovalHandler>,
    turn_rx: UnboundedReceiver<TurnEvent>,
    session_rx: UnboundedReceiver<SessionEvent>,
    cancel: CancelHandle,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(
        assembly: RuntimeAssembly,
        root: AgentSession,
        approvals: Arc<dyn ApprovalHandler>,
        turn_rx: UnboundedReceiver<TurnEvent>,
        session_rx: UnboundedReceiver<SessionEvent>,
    ) -> Self {
        Self {
            id: SessionId::from_core(root.id()),
            root,
            assembly,
            approvals,
            turn_rx,
            session_rx,
            cancel: CancelHandle::default(),
        }
    }

    /// Die Kennung dieser Sitzung (für [`crate::Harwness::resume`]).
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Ein neuer Strom der Live-Ereignisse (Wurzel und Kind-Agenten), ab
    /// jetzt.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// # async fn demo(mut session: harwness_sdk::Session) -> Result<(), harwness_sdk::SdkError> {
    /// use harwness_sdk::SdkEvent;
    ///
    /// let mut events = session.events();
    /// let printer = tokio::spawn(async move {
    ///     while let Some(event) = events.next().await {
    ///         if let SdkEvent::TextDelta { text, .. } = &event {
    ///             print!("{text}");
    ///         }
    ///         if event.is_root_finish() {
    ///             break;
    ///         }
    ///     }
    /// });
    /// session.send("Erzähl mir etwas.").await?;
    /// let _ = printer.await;
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn events(&self) -> EventStream {
        EventStream::new(self.assembly.agent_events().subscribe())
    }

    /// Ein Griff, der den laufenden Turn von einem anderen Task aus abbricht.
    #[must_use]
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    /// Bricht den laufenden Turn ab; siehe [`CancelHandle::cancel`].
    pub fn cancel(&self) -> bool {
        self.cancel.cancel()
    }

    /// Die bisherigen Nutzer- und Assistenten-Nachrichten.
    #[must_use]
    pub fn history(&self) -> Vec<Message> {
        self.root
            .history()
            .to_model_messages()
            .into_iter()
            .filter_map(|message| match message {
                ModelMessage::User { text } => Some(Message {
                    role: Role::User,
                    text,
                }),
                ModelMessage::Assistant { text } => Some(Message {
                    role: Role::Assistant,
                    text,
                }),
                _ => None,
            })
            .collect()
    }

    /// Kumulierte Token-Nutzung der Wurzelsitzung.
    #[must_use]
    pub fn total_usage(&self) -> Usage {
        Usage::from_core(self.root.total_usage())
    }

    /// Sendet eine Nutzernachricht und fährt den Turn zu Ende.
    ///
    /// # Beschreibung
    /// Pausen des Turns werden hier aufgelöst: Freigaben über den
    /// [`ApprovalHandler`] (mit dem konfigurierten Freigabe-Timeout),
    /// Übergaben an Kind-Agenten durch Ausführen des Kindes. Verschachtelte
    /// Pausen eines Kindes treibt die SDK nicht; das Kind endet dann mit
    /// einem Fehlerergebnis, das der Elternteil sieht.
    ///
    /// # Fehler
    /// - [`SdkError::InvalidInput`] bei leerem Text.
    /// - [`SdkError::Turn`] bei einem Kernfehler (Sitzung nicht bereit,
    ///   Speicherfehler, zu viele Wiederaufnahmen).
    /// - [`SdkError::Approval`], wenn eine Freigabe nicht zuzuordnen ist.
    ///
    /// Reguläre Ausgänge wie Abbruch, Ablehnung oder Abschneiden sind **kein**
    /// Fehler; sie stehen in [`TurnReport::status`].
    pub async fn send(&mut self, text: impl Into<String>) -> Result<TurnReport, SdkError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(SdkError::invalid("text", "must not be empty"));
        }
        // Reste eines früheren (etwa abgebrochenen) Turns gehören nicht in
        // diesen Bericht.
        let _stale = self.drain();

        let usage_before = self.root.total_usage().clone();
        let token = CancelToken::new();
        self.cancel.arm(token.clone());
        let input = TurnInput::user(text).with_control(TurnControl::new().with_cancel(token.clone()));
        let driven = drive(
            &self.assembly,
            &mut self.root,
            self.approvals.as_ref(),
            &token,
            input,
        )
        .await;
        self.cancel.disarm();
        let (outcome, approvals) = driven?;

        let collected = self.drain();
        Ok(TurnReport {
            session_id: self.id.clone(),
            status: TurnStatus::from_outcome(outcome),
            text: collected.final_text.or(collected.last_text),
            usage: Usage::delta(&usage_before, self.root.total_usage()),
            tool_calls: collected.tool_calls,
            approvals,
        })
    }

    /// Lädt den gespeicherten Verlauf dieser Sitzung.
    ///
    /// # Fehler
    /// [`SdkError::Session`] bei Lesefehlern, [`SdkError::SessionNotFound`]
    /// ohne gespeicherten Verlauf.
    pub(crate) async fn hydrate(&mut self) -> Result<(), SdkError> {
        let hydration = self
            .root
            .hydrate_from_store(self.assembly.state_store().as_ref())
            .await
            .map_err(|error| SdkError::Session {
                detail: format!("could not load session '{}': {error}", self.id),
            })?;
        if !hydration.skipped && hydration.history_items == 0 {
            return Err(SdkError::SessionNotFound {
                id: self.id.as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Leert beide Kanäle und sammelt, was der Bericht braucht.
    fn drain(&mut self) -> Collected {
        let mut collected = Collected::default();
        while let Ok(event) = self.turn_rx.try_recv() {
            match event {
                TurnEvent::ToolCallRequested { .. } => {
                    collected.tool_calls = collected.tool_calls.saturating_add(1);
                }
                TurnEvent::ItemAdded {
                    item: TurnItem::AssistantMessage(message),
                    ..
                } => {
                    let text = assistant_text(&message);
                    if matches!(message.phase, Some(harw_types::MessagePhase::FinalAnswer)) {
                        collected.final_text = Some(text.clone());
                    }
                    collected.last_text = Some(text);
                }
                _ => {}
            }
        }
        while let Ok(event) = self.session_rx.try_recv() {
            if let SessionEvent::SessionError { message, .. } = event {
                tracing::debug!(session_id = %self.id, %message, "sdk.session.error_event");
            }
        }
        collected
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.assembly.close_session(self.root.id());
    }
}

/// Fährt einen Turn samt aller Pausen bis zu einem ruhenden Ausgang.
///
/// # Rückgabe
/// Den ruhenden Ausgang und die Zahl vorgelegter Freigaben.
async fn drive(
    assembly: &RuntimeAssembly,
    session: &mut AgentSession,
    approvals: &dyn ApprovalHandler,
    cancel: &CancelToken,
    input: TurnInput,
) -> Result<(TurnOutcome, u32), SdkError> {
    let model = assembly.model().as_ref();
    let store = assembly.state_store().as_ref();
    let mut asked = 0_u32;
    let mut outcome = run_turn(session, model, store, input)
        .await
        .map_err(SdkError::turn)?;
    for _ in 0..MAX_RESUMES_PER_TURN {
        outcome = match outcome {
            TurnOutcome::AwaitingApproval { .. } => {
                let resolution = decide(assembly, session, approvals, cancel).await?;
                asked = asked.saturating_add(1);
                let actor = assembly.principal().approval_actor().ok_or_else(|| {
                    SdkError::Approval {
                        detail: "the runtime principal has no approval actor".to_owned(),
                    }
                })?;
                resume_after_approval(session, model, store, actor, resolution)
                    .await
                    .map_err(SdkError::turn)?
            }
            TurnOutcome::AwaitingChild {
                child,
                call_id,
                role,
            } => {
                let result = run_child(assembly, &child, &role, store).await;
                resume_after_child(session, model, store, child, call_id, result)
                    .await
                    .map_err(SdkError::turn)?
            }
            settled => return Ok((settled, asked)),
        };
    }
    Err(SdkError::Turn {
        detail: format!("the turn did not settle after {MAX_RESUMES_PER_TURN} resumes"),
    })
}

/// Legt die offene Freigabe dem Handler vor.
///
/// # Beschreibung
/// Ablehnung bei Zeitablauf (Freigabe-Timeout der Montage) und bei Abbruch
/// des Turns — eine unbeantwortete Frage ist nie eine Freigabe.
async fn decide(
    assembly: &RuntimeAssembly,
    session: &AgentSession,
    approvals: &dyn ApprovalHandler,
    cancel: &CancelToken,
) -> Result<ApprovalResolution, SdkError> {
    let pending = session.pending_approval().ok_or_else(|| SdkError::Approval {
        detail: "the turn paused for approval without a pending request".to_owned(),
    })?;
    let request = ApprovalRequest {
        session_id: SessionId::from_core(session.id()),
        request_id: pending.request.as_str().to_owned(),
        call_id: pending.call.id.as_str().to_owned(),
        tool: pending.call.name.as_str().to_owned(),
        arguments: pending.call.arguments.clone(),
    };
    let decision = tokio::select! {
        decided = tokio::time::timeout(assembly.approval_timeout(), approvals.decide(&request)) => {
            match decided {
                Ok(decision) => decision,
                Err(_elapsed) => return Ok(ApprovalResolution::timed_out()),
            }
        }
        () = cancel.cancelled() => {
            return Ok(ApprovalResolution::Reject {
                reason: "the turn was cancelled while awaiting approval".to_owned(),
            });
        }
    };
    Ok(match decision {
        Decision::Approve => ApprovalResolution::Approve,
        Decision::Deny { reason } => ApprovalResolution::Reject { reason },
    })
}

/// Führt einen Kind-Agenten aus und verpackt sein Ergebnis für den Elternteil.
async fn run_child(
    assembly: &RuntimeAssembly,
    child: &harw_types::SessionId,
    role: &str,
    store: &dyn harw_core::StateStore,
) -> ToolCallResult {
    let Some(spawner) = assembly.spawner() else {
        return ToolCallResult::error(format!(
            "child agent '{role}' cannot run: this runtime has no spawner"
        ));
    };
    let run = match spawner.run_child(child, store, TurnInput::default()).await {
        Ok(run) => run,
        Err(error) => {
            return ToolCallResult::error(format!("child agent '{role}' could not run: {error}"));
        }
    };
    match run.outcome {
        TurnOutcome::Completed => match spawner.child_final_assistant_text(child) {
            Ok(text) => ToolCallResult::success(serde_json::Value::String(text)),
            Err(error) => ToolCallResult::error(format!(
                "child agent '{role}' left no answer: {error}"
            )),
        },
        TurnOutcome::AwaitingApproval { .. } => ToolCallResult::error(format!(
            "child agent '{role}' paused for its own approval; nested approvals are not driven \
             by the SDK"
        )),
        TurnOutcome::AwaitingChild { role: nested, .. } => ToolCallResult::error(format!(
            "child agent '{role}' paused on its own handoff to '{nested}'; nested handoffs are \
             not driven by the SDK"
        )),
        other => {
            let status = TurnStatus::from_outcome(other);
            ToolCallResult::error(format!("child agent '{role}' ended without an answer: {status:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_map_to_stable_statuses() {
        assert_eq!(
            TurnStatus::from_outcome(TurnOutcome::Completed),
            TurnStatus::Completed
        );
        assert_eq!(
            TurnStatus::from_outcome(TurnOutcome::Cancelled {
                reason: CancelReason::User
            }),
            TurnStatus::Cancelled {
                reason: "user".into()
            }
        );
        assert_eq!(
            TurnStatus::from_outcome(TurnOutcome::Refused { detail: None }),
            TurnStatus::Refused { detail: None }
        );
        assert!(TurnStatus::from_outcome(TurnOutcome::Truncated) == TurnStatus::Truncated);
        assert!(!TurnStatus::Truncated.is_completed());
    }

    #[test]
    fn cancel_handle_reports_whether_a_turn_was_running() {
        let handle = CancelHandle::default();
        assert!(!handle.cancel(), "no turn is armed yet");
        let token = CancelToken::new();
        handle.arm(token.clone());
        assert!(handle.clone().cancel());
        assert_eq!(token.reason(), Some(CancelReason::User));
        handle.disarm();
        assert!(!handle.cancel());
    }
}

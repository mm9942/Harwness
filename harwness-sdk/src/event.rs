//! Stabile Live-Ereignisse einer Sitzung (`SdkEvent`, `EventStream`).
//!
//! # Beschreibung
//! Intern veröffentlicht jede Sitzung (Wurzel und alle Kind-Agenten) ihre
//! Turn-Ereignisse über einen agenten-übergreifenden Bus. Diese Datei
//! übersetzt sie in eine kleine, stabile Ereignisfläche: nur die Fälle, die
//! ein Einbettender braucht, mit SDK-eigenen Typen statt der internen
//! Protokolltypen. Interne Ereignisse ohne Gegenstück (Plan-Updates,
//! Moduswechsel, Verdichtung, Orchestrierungs-Rohdaten) werden verworfen.
//!
//! # Nebenläufigkeit
//! [`EventStream`] ist ein eigener Empfänger: beliebig viele Streams können
//! parallel zu [`crate::Session::send`] gelesen werden. Wer zu langsam liest,
//! bekommt [`SdkEvent::Lagged`] statt zu blockieren.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_core::Stream;
use harw_core::{AgentEvent, AgentEventKind};
use harw_protocol::{ToolCallResult, TurnEvent, TurnItem};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::ids::SessionId;

/// Token-Nutzung eines Modellaufrufs oder Turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Usage {
    /// Eingabe-Tokens (Provider-Semantik).
    pub input_tokens: u64,
    /// Ausgabe-Tokens.
    pub output_tokens: u64,
    /// Reasoning-Tokens, falls der Provider sie meldet.
    pub reasoning_tokens: Option<u64>,
    /// Aus dem Prompt-Cache gelesene Tokens, falls gemeldet.
    pub cached_tokens: Option<u64>,
    /// Beim Schreiben eines Prompt-Cache-Eintrags angefallene Tokens.
    pub cache_write_tokens: Option<u64>,
}

impl Usage {
    /// Summe aus Eingabe- und Ausgabe-Tokens.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Übernimmt die interne Nutzung.
    pub(crate) fn from_core(usage: &harw_types::TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cached_tokens: usage.cached_tokens,
            cache_write_tokens: usage.cache_write_tokens,
        }
    }

    /// Differenz `after - before` zweier kumulierter Stände (gesättigt).
    pub(crate) fn delta(before: &harw_types::TokenUsage, after: &harw_types::TokenUsage) -> Self {
        fn optional(before: Option<u64>, after: Option<u64>) -> Option<u64> {
            after.map(|after| after.saturating_sub(before.unwrap_or(0)))
        }
        Self {
            input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
            output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
            reasoning_tokens: optional(before.reasoning_tokens, after.reasoning_tokens),
            cached_tokens: optional(before.cached_tokens, after.cached_tokens),
            cache_write_tokens: optional(before.cache_write_tokens, after.cache_write_tokens),
        }
    }
}

/// Absender eines Ereignisses: die Wurzelsitzung oder ein Kind-Agent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventSource {
    /// Sitzung, die das Ereignis erzeugt hat.
    pub session_id: SessionId,
    /// Eltern-Sitzung, falls der Absender ein Kind-Agent ist.
    pub parent: Option<SessionId>,
    /// Anzeigename der Rolle (z. B. `assistant`, `explorer`).
    pub role: String,
}

impl EventSource {
    /// `true` für die Wurzelsitzung (kein Elternteil).
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

/// Ergebnis eines Werkzeugaufrufs.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ToolOutput {
    /// Das Werkzeug lieferte strukturierte Daten.
    Success {
        /// Die Nutzdaten.
        value: serde_json::Value,
    },
    /// Das Werkzeug lehnte ab oder scheiterte.
    Error {
        /// Nutzersichere Meldung.
        message: String,
    },
}

impl ToolOutput {
    /// `true` bei [`ToolOutput::Success`].
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

/// Wie ein Turn aus Sicht des Ereignisstroms endete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishStatus {
    /// Regulär abgeschlossen.
    Completed,
    /// Durch Abbruch beendet.
    Aborted,
}

/// Ein stabiles Live-Ereignis.
///
/// # Stabilität
/// `#[non_exhaustive]`: neue Varianten sind kein Major-Sprung.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SdkEvent {
    /// Ein Turn hat begonnen.
    TurnStarted {
        /// Absender.
        source: EventSource,
        /// Kennung des Turns.
        turn_id: String,
    },
    /// Live-Textdelta (nur streamende Provider).
    TextDelta {
        /// Absender.
        source: EventSource,
        /// Neuer Textteil.
        text: String,
    },
    /// Live-Reasoning-Delta (nur streamende Provider).
    ReasoningDelta {
        /// Absender.
        source: EventSource,
        /// Neuer Reasoning-Teil.
        text: String,
    },
    /// Eine vollständige Assistenten-Nachricht (auch ohne Streaming).
    Message {
        /// Absender.
        source: EventSource,
        /// Der Nachrichtentext.
        text: String,
        /// `true`, wenn dies die abschließende Antwort des Turns ist.
        final_answer: bool,
    },
    /// Das Modell fordert einen Werkzeugaufruf an.
    ToolCall {
        /// Absender.
        source: EventSource,
        /// Kennung des Aufrufs.
        call_id: String,
        /// Werkzeugname.
        tool: String,
        /// Argumente als JSON.
        arguments: serde_json::Value,
    },
    /// Ein Werkzeugergebnis ging an das Modell zurück.
    ToolResult {
        /// Absender.
        source: EventSource,
        /// Kennung des Aufrufs.
        call_id: String,
        /// Ergebnis.
        output: ToolOutput,
        /// Laufzeit des Werkzeugs.
        duration: Duration,
    },
    /// Ein Kind-Agent wurde zugelassen.
    ChildSpawned {
        /// Absender (der Elternteil).
        source: EventSource,
        /// Kennung des Kindes.
        child: SessionId,
        /// Rolle des Kindes.
        role: String,
        /// Auftrag des Kindes, falls bekannt.
        task: Option<String>,
    },
    /// Ein Kind-Agent ist terminiert.
    ChildCompleted {
        /// Absender (der Elternteil).
        source: EventSource,
        /// Kennung des Kindes.
        child: SessionId,
        /// Kurzform des Ausgangs (`completed`, `cancelled`, `failed`, …).
        outcome: String,
        /// Laufzeit des Kindes.
        duration: Duration,
    },
    /// Live-Tokenstand.
    Usage {
        /// Absender.
        source: EventSource,
        /// Stand der aktuellen Modell-Runde.
        round: Usage,
        /// Summe des Turns einschließlich `round`.
        turn_total: Usage,
        /// `true`, sobald die Runde abgeschlossen ist.
        final_round: bool,
    },
    /// Kontextfenster-Auslastung nach einer Modell-Runde.
    Context {
        /// Absender.
        source: EventSource,
        /// Belegte Prompt-Tokens.
        used_tokens: u64,
        /// Effektives Kontextfenster des aktiven Modells.
        window_tokens: u64,
    },
    /// Der Turn schlug fehl.
    Error {
        /// Absender.
        source: EventSource,
        /// Nutzersichere Meldung.
        message: String,
        /// Ob ein erneuter Versuch sinnvoll ist.
        retryable: bool,
    },
    /// Der Turn ist beendet.
    Finished {
        /// Absender.
        source: EventSource,
        /// Ausgang.
        status: FinishStatus,
        /// Aggregierte Nutzung des Turns, falls gemeldet.
        usage: Option<Usage>,
    },
    /// Der Leser war zu langsam; `skipped` Ereignisse gingen verloren.
    Lagged {
        /// Anzahl verlorener Ereignisse.
        skipped: u64,
    },
}

impl SdkEvent {
    /// Der Absender, falls das Ereignis einen hat ([`SdkEvent::Lagged`] nicht).
    #[must_use]
    pub fn source(&self) -> Option<&EventSource> {
        match self {
            Self::TurnStarted { source, .. }
            | Self::TextDelta { source, .. }
            | Self::ReasoningDelta { source, .. }
            | Self::Message { source, .. }
            | Self::ToolCall { source, .. }
            | Self::ToolResult { source, .. }
            | Self::ChildSpawned { source, .. }
            | Self::ChildCompleted { source, .. }
            | Self::Usage { source, .. }
            | Self::Context { source, .. }
            | Self::Error { source, .. }
            | Self::Finished { source, .. } => Some(source),
            Self::Lagged { .. } => None,
        }
    }

    /// `true`, wenn dies das Ende eines Turns der **Wurzelsitzung** ist.
    ///
    /// # Beschreibung
    /// Bequemer Abbruchpunkt für eine Leseschleife, die genau einen
    /// [`crate::Session::send`] begleitet.
    #[must_use]
    pub fn is_root_finish(&self) -> bool {
        match self {
            Self::Finished { source, .. } | Self::Error { source, .. } => source.is_root(),
            _ => false,
        }
    }
}

/// Übersetzt ein Bus-Ereignis; `None` für Ereignisse ohne SDK-Gegenstück.
pub(crate) fn map_agent_event(event: AgentEvent) -> Option<SdkEvent> {
    let AgentEvent {
        agent,
        parent,
        role,
        kind,
    } = event;
    let source = EventSource {
        session_id: SessionId::from_core(&agent),
        parent: parent.as_ref().map(SessionId::from_core),
        role,
    };
    match kind {
        AgentEventKind::Turn(turn) => map_turn_event(source, turn),
        _ => None,
    }
}

/// Übersetzt ein Turn-Ereignis eines bekannten Absenders.
pub(crate) fn map_turn_event(source: EventSource, event: TurnEvent) -> Option<SdkEvent> {
    match event {
        TurnEvent::TurnStarted { turn_id, .. } => Some(SdkEvent::TurnStarted {
            source,
            turn_id: turn_id.as_str().to_owned(),
        }),
        TurnEvent::AssistantDelta { text, .. } => Some(SdkEvent::TextDelta { source, text }),
        TurnEvent::ReasoningDelta { text, .. } => Some(SdkEvent::ReasoningDelta { source, text }),
        TurnEvent::ItemAdded {
            item: TurnItem::AssistantMessage(message),
            ..
        } => Some(SdkEvent::Message {
            source,
            text: assistant_text(&message),
            final_answer: matches!(message.phase, Some(harw_types::MessagePhase::FinalAnswer)),
        }),
        TurnEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
            ..
        } => Some(SdkEvent::ToolCall {
            source,
            call_id: call_id.as_str().to_owned(),
            tool: tool_name,
            arguments,
        }),
        TurnEvent::ToolCallCompleted {
            call_id,
            result,
            duration_ms,
            ..
        } => Some(SdkEvent::ToolResult {
            source,
            call_id: call_id.as_str().to_owned(),
            output: tool_output(result),
            duration: Duration::from_millis(duration_ms),
        }),
        TurnEvent::ChildSpawned {
            child,
            role,
            question,
            ..
        } => Some(SdkEvent::ChildSpawned {
            source,
            child: SessionId::from_core(&child),
            role,
            task: question,
        }),
        TurnEvent::ChildCompleted {
            child,
            outcome,
            duration_ms,
            ..
        } => Some(SdkEvent::ChildCompleted {
            source,
            child: SessionId::from_core(&child),
            outcome,
            duration: Duration::from_millis(duration_ms),
        }),
        TurnEvent::UsageUpdated {
            round,
            turn_total,
            final_round,
            ..
        } => Some(SdkEvent::Usage {
            source,
            round: Usage::from_core(&round),
            turn_total: Usage::from_core(&turn_total),
            final_round,
        }),
        TurnEvent::ContextUpdated {
            used_tokens,
            window_tokens,
            ..
        } => Some(SdkEvent::Context {
            source,
            used_tokens,
            window_tokens,
        }),
        TurnEvent::TurnFailed {
            reason, retryable, ..
        } => Some(SdkEvent::Error {
            source,
            message: reason,
            retryable,
        }),
        TurnEvent::TurnCompleted { usage, .. } => Some(SdkEvent::Finished {
            source,
            status: FinishStatus::Completed,
            usage: usage.as_ref().map(Usage::from_core),
        }),
        TurnEvent::TurnAborted { .. } => Some(SdkEvent::Finished {
            source,
            status: FinishStatus::Aborted,
            usage: None,
        }),
        // Ohne SDK-Gegenstück: übrige Items, Kind-Fortschritt, Plan-Updates,
        // Moduswechsel, Verdichtung und künftige interne Varianten.
        _ => None,
    }
}

/// Verbindet die Textteile einer Assistenten-Nachricht (Bilder entfallen);
/// die Regel steht einmal in `harw-session-client`.
pub(crate) fn assistant_text(message: &harw_protocol::AssistantMessageItem) -> String {
    harw_session_client::message_text(message)
}

/// Übersetzt ein internes Werkzeugergebnis.
fn tool_output(result: ToolCallResult) -> ToolOutput {
    match result {
        ToolCallResult::Success { value } => ToolOutput::Success { value },
        ToolCallResult::Error { message } => ToolOutput::Error { message },
    }
}

/// Live-Ereignisse einer Sitzung als [`Stream`].
///
/// # Beschreibung
/// Entsteht über [`crate::Session::events`] und sieht nur Ereignisse ab
/// seiner Erzeugung. Er endet (`None`), wenn die Sitzung und alle ihre
/// Kind-Agenten verworfen sind. Ohne Stream-Kombinatoren genügt
/// [`EventStream::next`].
pub struct EventStream {
    inner: Pin<Box<BroadcastStream<AgentEvent>>>,
}

impl EventStream {
    /// Umhüllt einen Empfänger des internen Busses.
    pub(crate) fn new(receiver: tokio::sync::broadcast::Receiver<AgentEvent>) -> Self {
        Self {
            inner: Box::pin(BroadcastStream::new(receiver)),
        }
    }

    /// Wartet auf das nächste Ereignis; `None`, wenn der Strom endet.
    pub async fn next(&mut self) -> Option<SdkEvent> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

impl Stream for EventStream {
    type Item = SdkEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SdkEvent>> {
        loop {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Ok(event))) => {
                    if let Some(mapped) = map_agent_event(event) {
                        return Poll::Ready(Some(mapped));
                    }
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(skipped)))) => {
                    return Poll::Ready(Some(SdkEvent::Lagged { skipped }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::{AssistantMessageItem, ContentPart};
    use harw_types::{ItemId, ThreadId, TokenUsage, ToolCallId, TurnId};
    use serde_json::json;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn root() -> EventSource {
        EventSource {
            session_id: SessionId::from_core(&harw_types::SessionId::from_str("root")),
            parent: None,
            role: "assistant".into(),
        }
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    #[test]
    fn deltas_map_to_text_and_reasoning() -> TestResult {
        let text = map_turn_event(
            root(),
            TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: "Hal".into(),
            },
        );
        let Some(SdkEvent::TextDelta { text, source }) = text else {
            return Err("AssistantDelta must map to TextDelta".into());
        };
        assert_eq!(text, "Hal");
        assert!(source.is_root());

        let reasoning = map_turn_event(
            root(),
            TurnEvent::ReasoningDelta {
                turn_id: TurnId::new(),
                text: "denke".into(),
            },
        );
        assert!(matches!(
            reasoning,
            Some(SdkEvent::ReasoningDelta { ref text, .. }) if text == "denke"
        ));
        Ok(())
    }

    #[test]
    fn tool_call_and_result_keep_the_call_id() -> TestResult {
        let call_id = ToolCallId::from_str("call-7");
        let requested = map_turn_event(
            root(),
            TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: call_id.clone(),
                tool_name: "fs.read".into(),
                arguments: json!({"path": "a"}),
            },
        );
        let Some(SdkEvent::ToolCall {
            call_id: requested_id,
            tool,
            arguments,
            ..
        }) = requested
        else {
            return Err("ToolCallRequested must map to ToolCall".into());
        };
        assert_eq!(requested_id, "call-7");
        assert_eq!(tool, "fs.read");
        assert_eq!(arguments, json!({"path": "a"}));

        let completed = map_turn_event(
            root(),
            TurnEvent::ToolCallCompleted {
                turn_id: TurnId::new(),
                call_id,
                result: ToolCallResult::error("nope"),
                duration_ms: 12,
                placement: None,
            },
        );
        let Some(SdkEvent::ToolResult {
            call_id,
            output,
            duration,
            ..
        }) = completed
        else {
            return Err("ToolCallCompleted must map to ToolResult".into());
        };
        assert_eq!(call_id, "call-7");
        assert_eq!(
            output,
            ToolOutput::Error {
                message: "nope".into()
            }
        );
        assert!(!output.is_success());
        assert_eq!(duration, Duration::from_millis(12));
        Ok(())
    }

    #[test]
    fn children_are_reported_with_their_ids() -> TestResult {
        let child = harw_types::SessionId::from_str("child-1");
        let spawned = map_turn_event(
            root(),
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: child.clone(),
                role: "explorer".into(),
                question: Some("Wo?".into()),
            },
        );
        let Some(SdkEvent::ChildSpawned {
            child: spawned_child,
            role,
            task,
            ..
        }) = spawned
        else {
            return Err("ChildSpawned must map".into());
        };
        assert_eq!(spawned_child.as_str(), "child-1");
        assert_eq!(role, "explorer");
        assert_eq!(task.as_deref(), Some("Wo?"));

        let completed = map_turn_event(
            root(),
            TurnEvent::ChildCompleted {
                turn_id: TurnId::new(),
                child,
                outcome: "completed".into(),
                duration_ms: 5,
            },
        );
        assert!(matches!(
            completed,
            Some(SdkEvent::ChildCompleted { ref outcome, .. }) if outcome == "completed"
        ));
        Ok(())
    }

    #[test]
    fn usage_context_and_finish_map() -> TestResult {
        let updated = map_turn_event(
            root(),
            TurnEvent::UsageUpdated {
                turn_id: TurnId::new(),
                round: usage(3, 4),
                turn_total: usage(10, 20),
                final_round: true,
            },
        );
        let Some(SdkEvent::Usage {
            round,
            turn_total,
            final_round,
            ..
        }) = updated
        else {
            return Err("UsageUpdated must map to Usage".into());
        };
        assert_eq!(round.total(), 7);
        assert_eq!(turn_total.total(), 30);
        assert!(final_round);

        // Über die Wire-Form gebaut: optionale Felder (`serde(default)`)
        // dürfen hinzukommen, ohne dass dieser Test angepasst werden muss.
        let context_event: TurnEvent = serde_json::from_value(json!({
            "type": "context_updated",
            "turn_id": "turn-1",
            "used_tokens": 100,
            "window_tokens": 1000,
            "history_items_dropped": 0,
        }))?;
        let context = map_turn_event(root(), context_event);
        assert!(matches!(
            context,
            Some(SdkEvent::Context {
                used_tokens: 100,
                window_tokens: 1000,
                ..
            })
        ));

        let finished = map_turn_event(
            root(),
            TurnEvent::TurnCompleted {
                turn_id: TurnId::new(),
                usage: Some(usage(1, 2)),
            },
        );
        let Some(event) = finished else {
            return Err("TurnCompleted must map".into());
        };
        assert!(event.is_root_finish());
        assert!(matches!(
            event,
            SdkEvent::Finished {
                status: FinishStatus::Completed,
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 2,
                    ..
                }),
                ..
            }
        ));

        let aborted = map_turn_event(
            root(),
            TurnEvent::TurnAborted {
                turn_id: TurnId::new(),
            },
        );
        assert!(matches!(
            aborted,
            Some(SdkEvent::Finished {
                status: FinishStatus::Aborted,
                ..
            })
        ));

        let failed = map_turn_event(
            root(),
            TurnEvent::TurnFailed {
                turn_id: TurnId::new(),
                reason: "kaputt".into(),
                retryable: true,
            },
        );
        assert!(matches!(
            failed,
            Some(SdkEvent::Error { retryable: true, ref message, .. }) if message == "kaputt"
        ));
        Ok(())
    }

    #[test]
    fn assistant_items_become_messages_and_other_items_are_dropped() -> TestResult {
        let message = map_turn_event(
            root(),
            TurnEvent::ItemAdded {
                turn_id: TurnId::new(),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::new(),
                    content: vec![
                        ContentPart::Text { text: "a".into() },
                        ContentPart::ImageUrl {
                            url: "data:".into(),
                            detail: None,
                        },
                        ContentPart::Text { text: "b".into() },
                    ],
                    phase: Some(harw_types::MessagePhase::FinalAnswer),
                }),
            },
        );
        let Some(SdkEvent::Message {
            text, final_answer, ..
        }) = message
        else {
            return Err("assistant items must map to Message".into());
        };
        assert_eq!(text, "ab");
        assert!(final_answer);

        let started = map_turn_event(
            root(),
            TurnEvent::TurnStarted {
                turn_id: TurnId::from_str("t-1"),
                thread_id: ThreadId::new(),
            },
        );
        assert!(matches!(
            started,
            Some(SdkEvent::TurnStarted { ref turn_id, .. }) if turn_id == "t-1"
        ));

        let mode = map_turn_event(
            root(),
            TurnEvent::ModeChanged {
                mode: "plan".into(),
            },
        );
        assert!(mode.is_none(), "internal-only events must be dropped");
        Ok(())
    }

    #[test]
    fn bus_events_carry_parent_and_role() -> TestResult {
        let event = AgentEvent {
            agent: harw_types::SessionId::from_str("child-2"),
            parent: Some(harw_types::SessionId::from_str("root")),
            role: "reviewer".into(),
            kind: AgentEventKind::Turn(TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: "x".into(),
            }),
        };
        let Some(mapped) = map_agent_event(event) else {
            return Err("turn events on the bus must map".into());
        };
        let Some(source) = mapped.source() else {
            return Err("mapped turn events carry a source".into());
        };
        assert!(!source.is_root());
        assert_eq!(source.role, "reviewer");
        assert_eq!(source.parent.as_ref().map(SessionId::as_str), Some("root"));
        assert!(!mapped.is_root_finish());

        let internal = AgentEvent {
            agent: harw_types::SessionId::from_str("root"),
            parent: None,
            role: "assistant".into(),
            kind: AgentEventKind::InternalUsage {
                purpose: "title".into(),
                usage: usage(1, 1),
            },
        };
        assert!(map_agent_event(internal).is_none());
        Ok(())
    }

    #[test]
    fn usage_delta_saturates_and_keeps_optional_fields() {
        let before = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: Some(2),
            ..TokenUsage::default()
        };
        let after = TokenUsage {
            input_tokens: 15,
            output_tokens: 4,
            cached_tokens: Some(7),
            reasoning_tokens: Some(3),
            ..TokenUsage::default()
        };
        let delta = Usage::delta(&before, &after);
        assert_eq!(delta.input_tokens, 5);
        assert_eq!(delta.output_tokens, 0);
        assert_eq!(delta.cached_tokens, Some(5));
        assert_eq!(delta.reasoning_tokens, Some(3));
        assert_eq!(delta.cache_write_tokens, None);
    }

    #[tokio::test]
    async fn stream_maps_and_reports_lag() -> TestResult {
        let hub = harw_core::AgentEventHub::new(1);
        let mut stream = EventStream::new(hub.subscribe());
        for text in ["a", "b"] {
            hub.publish(AgentEvent {
                agent: harw_types::SessionId::from_str("root"),
                parent: None,
                role: "assistant".into(),
                kind: AgentEventKind::Turn(TurnEvent::AssistantDelta {
                    turn_id: TurnId::new(),
                    text: text.into(),
                }),
            });
        }
        // Kapazität 1: das erste Ereignis ist verloren.
        assert!(matches!(
            stream.next().await,
            Some(SdkEvent::Lagged { skipped: 1 })
        ));
        assert!(matches!(
            stream.next().await,
            Some(SdkEvent::TextDelta { ref text, .. }) if text == "b"
        ));
        drop(hub);
        assert!(stream.next().await.is_none());
        Ok(())
    }
}

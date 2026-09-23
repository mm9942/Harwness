//! Lifecycle-Events: `SessionEvent` (gesamte Session) und `TurnEvent`
//! (einzelner Turn). Beide intern getaggt für einheitliche Wire-Form.

use harw_types::{SessionId, ThreadId, TokenUsage, ToolCallId, TurnId};
use serde::{Deserialize, Serialize};

use crate::items::{ToolCallResult, TurnItem};

/// Lifecycle eines kompletten Sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Session wurde initialisiert (Konfiguration bestätigt).
    SessionConfigured {
        session_id: SessionId,
        thread_id: ThreadId,
        model: String,
    },
    /// Session wurde sauber beendet.
    SessionClosed {
        session_id: SessionId,
        reason: Option<String>,
    },
    /// Fehler auf Session-Ebene (nicht turn-spezifisch).
    SessionError {
        session_id: SessionId,
        message: String,
        retryable: bool,
    },
    /// Ein Turn wurde innerhalb der Session gestartet.
    TurnStarted {
        session_id: SessionId,
        turn_id: TurnId,
    },
    /// Ein Turn wurde innerhalb der Session erfolgreich abgeschlossen.
    TurnCompleted {
        session_id: SessionId,
        turn_id: TurnId,
        /// Aggregierte Token-Nutzung für diesen Turn.
        usage: TokenUsage,
    },
    /// Die Session ist in einen terminalen Fehlerzustand übergegangen.
    SessionFailed {
        session_id: SessionId,
        reason: String,
    },
}

/// Lifecycle eines einzelnen Turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// Modell hat den Turn gestartet (erste Token kommen).
    TurnStarted {
        turn_id: TurnId,
        thread_id: ThreadId,
    },
    /// Ein neues `TurnItem` ist verfügbar.
    ItemAdded { turn_id: TurnId, item: TurnItem },
    /// Modell fordert Tool-Ausführung an.
    ToolCallRequested {
        turn_id: TurnId,
        call_id: ToolCallId,
        tool_name: String,
        arguments: serde_json::Value,
    },
    /// Tool-Ergebnis wurde an den Modell-Kontext zurückgegeben.
    ToolCallCompleted {
        turn_id: TurnId,
        call_id: ToolCallId,
        /// Explicit success/error wire outcome for the completed call.
        result: ToolCallResult,
        duration_ms: u64,
    },
    /// Alle Aktionen abgeschlossen, Antwort finalisiert.
    TurnCompleted {
        turn_id: TurnId,
        /// Aggregierte Token-Nutzung für diesen Turn.
        usage: Option<TokenUsage>,
    },
    /// Turn wurde durch Interrupt oder fatalen Fehler abgebrochen.
    TurnFailed {
        turn_id: TurnId,
        reason: String,
        retryable: bool,
    },
    /// Turn wurde durch expliziten Interrupt (Nutzer) abgebrochen.
    TurnAborted { turn_id: TurnId },
    /// Ein Kind-Agent wurde für diesen Turn admittiert (Fan-out). Wird vom
    /// Orchestrator gesendet, sobald ein Kind zur parallelen Bearbeitung
    /// zugelassen wurde.
    ChildSpawned {
        turn_id: TurnId,
        child: SessionId,
        role: String,
        question: Option<String>,
    },
    /// Fortschrittsmeldung eines laufenden Kindes (Tool-Aufrufe/Token
    /// bisher). Wird periodisch von der überwachenden Session gesendet,
    /// solange das Kind aktiv ist.
    ChildProgress {
        turn_id: TurnId,
        child: SessionId,
        tool_calls: u32,
        tokens: u64,
    },
    /// Ein Kind ist terminiert; `outcome` ist die Kurzform des TurnOutcome
    /// ("completed", "budget_exceeded", "cancelled", "failed"). Wird vom
    /// Orchestrator gesendet, sobald das Kind final terminiert ist.
    ChildCompleted {
        turn_id: TurnId,
        child: SessionId,
        outcome: String,
        duration_ms: u64,
    },
    /// Der Plan-Graph wurde mutiert (Revision + Kurzbeschreibung der
    /// Aktion). Wird von der Planungskomponente gesendet, sobald ein
    /// Plan-Update übernommen wurde.
    PlanUpdated {
        plan_id: String,
        revision: u64,
        summary: String,
    },
    /// Der Interaktionsmodus der Session wurde gewechselt
    /// (chat|plan|explore|work|shell). Wird von der Session gesendet, sobald
    /// ein Moduswechsel abgeschlossen ist.
    ModeChanged { mode: String },
    /// Live-Textdelta der laufenden Modell-Runde (nur bei streamenden
    /// Providern). Der finale Text kommt weiterhin als `ItemAdded`.
    AssistantDelta { turn_id: TurnId, text: String },
    /// Live-Reasoning-Delta der laufenden Modell-Runde.
    ReasoningDelta { turn_id: TurnId, text: String },
    /// Live-Tokenstand: `round` ist der (ggf. noch wachsende) Stand der
    /// aktuellen Modell-Runde, `turn_total` die Summe aller abgeschlossenen
    /// Runden dieses Turns plus `round`. `final_round` ist `true`, sobald
    /// die Runde abgeschlossen ist (Pro-Runde-Fallback liefert nur diese).
    UsageUpdated {
        turn_id: TurnId,
        round: TokenUsage,
        turn_total: TokenUsage,
        final_round: bool,
    },
    /// Kontextfenster-Auslastung nach einer Modell-Runde. `used_tokens`
    /// zählt alle Prompt-Tokens (inkl. Cache-Read/-Write), `window_tokens`
    /// ist das effektive Kontextfenster des aktiven Modells.
    /// `history_items_dropped` meldet, wie viele Verlaufseinträge das
    /// Byte-Budget für diese Runde weggelassen hat.
    ContextUpdated {
        turn_id: TurnId,
        used_tokens: u64,
        window_tokens: u64,
        history_items_dropped: u32,
    },
    /// Eine (Auto-)Kompaktierung wurde angewendet.
    CompactionApplied {
        turn_id: Option<TurnId>,
        reason: String,
        items_before: u32,
        items_after: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use serde_json::json;

    /// Prüft Tag, vollständige Wire-Form, Roundtrip und Ablehnung eines
    /// unbekannten `type`-Werts für `ChildSpawned`.
    #[test]
    fn child_spawned_roundtrips_with_snake_case_tag_and_rejects_unknown_type() -> TestResult {
        let turn_id = TurnId::try_from_str("turn-1").map_err(ctx("gültige TurnId"))?;
        let child = SessionId::try_from_str("child-1").map_err(ctx("gültige SessionId"))?;
        let event = TurnEvent::ChildSpawned {
            turn_id: turn_id.clone(),
            child: child.clone(),
            role: "explorer".to_string(),
            question: Some("Wie sieht X aus?".to_string()),
        };

        let value = serde_json::to_value(&event).map_err(ctx("Event serialisiert"))?;
        assert_eq!(
            value,
            json!({
                "type": "child_spawned",
                "turn_id": turn_id.as_str(),
                "child": child.as_str(),
                "role": "explorer",
                "question": "Wie sieht X aus?",
            })
        );

        let restored: TurnEvent =
            serde_json::from_value(value).map_err(ctx("Event deserialisiert"))?;
        match restored {
            TurnEvent::ChildSpawned {
                turn_id: got_turn_id,
                child: got_child,
                role,
                question,
            } => {
                assert_eq!(got_turn_id, turn_id);
                assert_eq!(got_child, child);
                assert_eq!(role, "explorer");
                assert_eq!(question, Some("Wie sieht X aus?".to_string()));
            }
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "unerwartete Variante: {other:?}"
                )));
            }
        }

        let malformed = json!({
            "type": "child_teleported",
            "turn_id": "turn-1",
            "child": "child-1",
            "role": "explorer",
            "question": null,
        });
        assert!(serde_json::from_value::<TurnEvent>(malformed).is_err());
        Ok(())
    }

    /// Prüft Tag, vollständige Wire-Form, Roundtrip und Ablehnung eines
    /// unbekannten `type`-Werts für `ChildProgress`.
    #[test]
    fn child_progress_roundtrips_with_snake_case_tag_and_rejects_unknown_type() -> TestResult {
        let turn_id = TurnId::try_from_str("turn-2").map_err(ctx("gültige TurnId"))?;
        let child = SessionId::try_from_str("child-2").map_err(ctx("gültige SessionId"))?;
        let event = TurnEvent::ChildProgress {
            turn_id: turn_id.clone(),
            child: child.clone(),
            tool_calls: 3,
            tokens: 1234,
        };

        let value = serde_json::to_value(&event).map_err(ctx("Event serialisiert"))?;
        assert_eq!(
            value,
            json!({
                "type": "child_progress",
                "turn_id": turn_id.as_str(),
                "child": child.as_str(),
                "tool_calls": 3,
                "tokens": 1234,
            })
        );

        let restored: TurnEvent =
            serde_json::from_value(value).map_err(ctx("Event deserialisiert"))?;
        match restored {
            TurnEvent::ChildProgress {
                turn_id: got_turn_id,
                child: got_child,
                tool_calls,
                tokens,
            } => {
                assert_eq!(got_turn_id, turn_id);
                assert_eq!(got_child, child);
                assert_eq!(tool_calls, 3);
                assert_eq!(tokens, 1234);
            }
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "unerwartete Variante: {other:?}"
                )));
            }
        }

        let malformed = json!({
            "type": "child_teleported",
            "turn_id": "turn-2",
            "child": "child-2",
            "tool_calls": 3,
            "tokens": 1234,
        });
        assert!(serde_json::from_value::<TurnEvent>(malformed).is_err());
        Ok(())
    }

    /// Prüft Tag, vollständige Wire-Form, Roundtrip und Ablehnung eines
    /// unbekannten `type`-Werts für `ChildCompleted`.
    #[test]
    fn child_completed_roundtrips_with_snake_case_tag_and_rejects_unknown_type() -> TestResult {
        let turn_id = TurnId::try_from_str("turn-3").map_err(ctx("gültige TurnId"))?;
        let child = SessionId::try_from_str("child-3").map_err(ctx("gültige SessionId"))?;
        let event = TurnEvent::ChildCompleted {
            turn_id: turn_id.clone(),
            child: child.clone(),
            outcome: "completed".to_string(),
            duration_ms: 42,
        };

        let value = serde_json::to_value(&event).map_err(ctx("Event serialisiert"))?;
        assert_eq!(
            value,
            json!({
                "type": "child_completed",
                "turn_id": turn_id.as_str(),
                "child": child.as_str(),
                "outcome": "completed",
                "duration_ms": 42,
            })
        );

        let restored: TurnEvent =
            serde_json::from_value(value).map_err(ctx("Event deserialisiert"))?;
        match restored {
            TurnEvent::ChildCompleted {
                turn_id: got_turn_id,
                child: got_child,
                outcome,
                duration_ms,
            } => {
                assert_eq!(got_turn_id, turn_id);
                assert_eq!(got_child, child);
                assert_eq!(outcome, "completed");
                assert_eq!(duration_ms, 42);
            }
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "unerwartete Variante: {other:?}"
                )));
            }
        }

        let malformed = json!({
            "type": "child_teleported",
            "turn_id": "turn-3",
            "child": "child-3",
            "outcome": "completed",
            "duration_ms": 42,
        });
        assert!(serde_json::from_value::<TurnEvent>(malformed).is_err());
        Ok(())
    }

    /// Prüft Tag, vollständige Wire-Form, Roundtrip und Ablehnung eines
    /// unbekannten `type`-Werts für `PlanUpdated`.
    #[test]
    fn plan_updated_roundtrips_with_snake_case_tag_and_rejects_unknown_type() -> TestResult {
        let event = TurnEvent::PlanUpdated {
            plan_id: "plan-1".to_string(),
            revision: 7,
            summary: "Schritt 3 hinzugefügt".to_string(),
        };

        let value = serde_json::to_value(&event).map_err(ctx("Event serialisiert"))?;
        assert_eq!(
            value,
            json!({
                "type": "plan_updated",
                "plan_id": "plan-1",
                "revision": 7,
                "summary": "Schritt 3 hinzugefügt",
            })
        );

        let restored: TurnEvent =
            serde_json::from_value(value).map_err(ctx("Event deserialisiert"))?;
        match restored {
            TurnEvent::PlanUpdated {
                plan_id,
                revision,
                summary,
            } => {
                assert_eq!(plan_id, "plan-1");
                assert_eq!(revision, 7);
                assert_eq!(summary, "Schritt 3 hinzugefügt");
            }
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "unerwartete Variante: {other:?}"
                )));
            }
        }

        let malformed = json!({
            "type": "plan_teleported",
            "plan_id": "plan-1",
            "revision": 7,
            "summary": "Schritt 3 hinzugefügt",
        });
        assert!(serde_json::from_value::<TurnEvent>(malformed).is_err());
        Ok(())
    }

    /// Prüft Tag, vollständige Wire-Form, Roundtrip und Ablehnung eines
    /// unbekannten `type`-Werts für `ModeChanged`.
    #[test]
    fn mode_changed_roundtrips_with_snake_case_tag_and_rejects_unknown_type() -> TestResult {
        let event = TurnEvent::ModeChanged {
            mode: "plan".to_string(),
        };

        let value = serde_json::to_value(&event).map_err(ctx("Event serialisiert"))?;
        assert_eq!(
            value,
            json!({
                "type": "mode_changed",
                "mode": "plan",
            })
        );

        let restored: TurnEvent =
            serde_json::from_value(value).map_err(ctx("Event deserialisiert"))?;
        match restored {
            TurnEvent::ModeChanged { mode } => {
                assert_eq!(mode, "plan");
            }
            other => {
                return Err(crate::test_support::TestError::Unexpected(format!(
                    "unerwartete Variante: {other:?}"
                )));
            }
        }

        let malformed = json!({
            "type": "mode_teleported",
            "mode": "plan",
        });
        assert!(serde_json::from_value::<TurnEvent>(malformed).is_err());
        Ok(())
    }
}

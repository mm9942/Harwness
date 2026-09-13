//! `ConversationHistory` — akkumuliert Turn-Items über die Lebenszeit
//! einer Session.
//!
//! Serialisierbar (für `StateStore`-Persistenz) und mit Komfort-Konstruktoren
//! für die drei zentralen Message-Typen: User, Assistant, Tool.

use harw_protocol::items::{
    AssistantMessageItem, ContentPart, ErrorItem, ToolCallItem, ToolCallResult, ToolResultItem,
    TurnItem, UserMessageItem,
};
use harw_types::{ItemId, MessagePhase, ToolCallId};
use serde::{Deserialize, Serialize};

/// Eine reduzierte, provider-neutrale Sicht auf ein History-Item, wie sie ein
/// `ModelProvider` zum Aufbau seines Wire-Formats konsumiert.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelMessage {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    ToolCall {
        call_id: ToolCallId,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: ToolCallId,
        result: ToolCallResult,
    },
}

/// Akkumuliert Turn-Items über die Lebenszeit einer Session.
///
/// `#[serde(transparent)]` ist hier bewusst NICHT gesetzt: die History wird als
/// Objekt mit `items`-Feld serialisiert, damit spätere Felder (z. B. Cursor,
/// Token-Summen) additiv ergänzt werden können, ohne das Wire-Format zu brechen.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConversationHistory {
    items: Vec<TurnItem>,
}

impl ConversationHistory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Konstruiert eine History aus bereits vorhandenen Items (z. B. aus dem
    /// `StateStore` geladen).
    #[must_use]
    pub fn from_items(items: Vec<TurnItem>) -> Self {
        Self { items }
    }

    pub fn push(&mut self, item: TurnItem) {
        self.items.push(item);
    }

    /// Hängt mehrere Items an (z. B. ein vom Child gelieferter Verlauf).
    pub fn extend(&mut self, items: impl IntoIterator<Item = TurnItem>) {
        self.items.extend(items);
    }

    /// Komfort: eine User-Textnachricht anhängen. Gibt die erzeugte `ItemId`
    /// zurück, damit Aufrufer sie referenzieren können.
    pub fn push_user_text(&mut self, text: impl Into<String>) -> ItemId {
        let id = ItemId::new();
        self.items.push(TurnItem::UserMessage(UserMessageItem {
            id: id.clone(),
            content: vec![ContentPart::Text { text: text.into() }],
        }));
        id
    }

    /// Komfort: eine Assistant-Textnachricht anhängen.
    pub fn push_assistant_text(
        &mut self,
        text: impl Into<String>,
        phase: Option<MessagePhase>,
    ) -> ItemId {
        let id = ItemId::new();
        self.items
            .push(TurnItem::AssistantMessage(AssistantMessageItem {
                id: id.clone(),
                content: vec![ContentPart::Text { text: text.into() }],
                phase,
            }));
        id
    }

    /// Komfort: einen vom Modell angeforderten Tool-Call anhängen.
    pub fn push_tool_call(
        &mut self,
        call_id: ToolCallId,
        tool_name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> ItemId {
        let id = ItemId::new();
        self.items.push(TurnItem::ToolCall(ToolCallItem {
            id: id.clone(),
            call_id,
            tool_name: tool_name.into(),
            arguments,
        }));
        id
    }

    /// Komfort: ein Fehler-Item anhängen.
    pub fn push_error(&mut self, message: impl Into<String>, retryable: bool) -> ItemId {
        let id = ItemId::new();
        self.items.push(TurnItem::Error(ErrorItem {
            id: id.clone(),
            message: message.into(),
            retryable,
        }));
        id
    }

    /// Komfort: ein Tool-Ergebnis anhängen.
    pub fn push_tool_result(
        &mut self,
        call_id: ToolCallId,
        result: ToolCallResult,
        duration_ms: u64,
    ) -> ItemId {
        let id = ItemId::new();
        self.items.push(TurnItem::ToolResult(ToolResultItem {
            id: id.clone(),
            call_id,
            result,
            duration_ms,
        }));
        id
    }

    #[must_use]
    pub fn items(&self) -> &[TurnItem] {
        &self.items
    }

    /// Das zuletzt angehängte Item (falls vorhanden).
    #[must_use]
    pub fn last(&self) -> Option<&TurnItem> {
        self.items.last()
    }

    /// Projiziert den Verlauf auf die provider-neutrale [`ModelMessage`]-Sicht.
    ///
    /// Reasoning-Items werden bewusst ausgelassen — sie sind Surface-Metadaten,
    /// kein Modell-Input. Bild-Inhalte werden zu einem Platzhalter reduziert,
    /// bis ein Provider multimodale Eingaben braucht.
    #[must_use]
    pub fn to_model_messages(&self) -> Vec<ModelMessage> {
        let mut out = Vec::with_capacity(self.items.len());
        for item in &self.items {
            match item {
                TurnItem::UserMessage(m) => out.push(ModelMessage::User {
                    text: flatten_content(&m.content),
                }),
                TurnItem::AssistantMessage(m) => out.push(ModelMessage::Assistant {
                    text: flatten_content(&m.content),
                }),
                TurnItem::ToolCall(c) => out.push(ModelMessage::ToolCall {
                    call_id: c.call_id.clone(),
                    name: c.tool_name.clone(),
                    arguments: c.arguments.clone(),
                }),
                TurnItem::ToolResult(r) => out.push(ModelMessage::ToolResult {
                    call_id: r.call_id.clone(),
                    result: r.result.clone(),
                }),
                TurnItem::Reasoning(_) | TurnItem::Error(_) => {}
            }
        }
        out
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Zerlegt die Historie in atomare Gruppen: ein einzelnes Item, außer es
    /// gehört zu einer Tool-Runde — dann bilden alle Calls dieser Runde und
    /// ihre Ergebnisse zusammen **eine** Gruppe.
    ///
    /// Beantwortet das Modell einen Turn mit mehreren Tool-Calls, protokolliert
    /// der Turn-Loop erst alle Calls und danach alle Ergebnisse. Ein Ergebnis
    /// folgt seinem Call dann nicht unmittelbar. Würde die Gruppierung nur
    /// direkt benachbarte Paare zusammenfassen, könnte die Byte-Budget-Kürzung
    /// die Calls abschneiden und die Ergebnisse behalten. Der Provider lehnt
    /// einen solchen Verlauf ab („messages with role 'tool' must be a response
    /// to a preceeding message with 'tool_calls'"). Deshalb bleibt eine Runde
    /// offen, bis jeder ihrer Calls beantwortet ist.
    ///
    /// # Description
    /// Extrahiert aus [`Self::tail_within_estimated_bytes`] (AW5-07), damit
    /// die Atomaritätsregel „ein Tool-Call/Ergebnis-Paar wird nie getrennt"
    /// an genau einer Stelle steht, statt zwischen dem Byte-Budget-Pfad dort
    /// und dem `DetailMode::References`-Pfad in [`crate::history_tail`]
    /// dupliziert zu werden. `pub(crate)`, weil nur Aufrufer innerhalb dieses
    /// Crates die Gruppierung roh brauchen — externe Aufrufer erhalten stets
    /// bereits fertige [`crate::history::ConversationHistory`]- oder
    /// `Fragment`-Werte.
    ///
    /// # Returns
    /// Die Gruppen in Ankunftsreihenfolge; jede Gruppe enthält mindestens ein
    /// Item und referenziert ausschließlich Items aus `self.items`.
    #[must_use]
    pub(crate) fn atomic_groups(&self) -> Vec<Vec<&TurnItem>> {
        let mut groups: Vec<Vec<&TurnItem>> = Vec::new();
        // Die Call-IDs der laufenden Tool-Runde, die noch kein Ergebnis haben.
        // Solange hier etwas offen ist, gehört alles Weitere zur selben Runde.
        let mut unanswered: Vec<&ToolCallId> = Vec::new();
        for item in &self.items {
            match item {
                TurnItem::ToolCall(call) => {
                    if unanswered.is_empty() {
                        groups.push(vec![item]);
                    } else if let Some(group) = groups.last_mut() {
                        group.push(item);
                    } else {
                        groups.push(vec![item]);
                    }
                    unanswered.push(&call.call_id);
                }
                TurnItem::ToolResult(result) => {
                    let open = unanswered
                        .iter()
                        .position(|call_id| **call_id == result.call_id);
                    match (open, groups.last_mut()) {
                        (Some(index), Some(group)) => {
                            unanswered.remove(index);
                            group.push(item);
                        }
                        // Ein Ergebnis ohne offenen Call gehört zu nichts und
                        // bleibt für sich — so bleibt die Funktion auch auf
                        // beschädigten Verläufen total.
                        _ => groups.push(vec![item]),
                    }
                }
                _ => {
                    unanswered.clear();
                    groups.push(vec![item]);
                }
            }
        }
        groups
    }

    /// Returns the newest history projection whose serialized item groups fit
    /// `max_bytes`. Adjacent tool call/result pairs are retained or dropped as
    /// one unit, so a model never sees an orphaned result after compaction.
    #[must_use]
    pub fn tail_within_estimated_bytes(&self, max_bytes: usize) -> (Self, usize, usize) {
        let groups = self.atomic_groups();

        let mut selected_reversed: Vec<Vec<&TurnItem>> = Vec::new();
        let mut used = 0_usize;
        for group in groups.iter().rev() {
            let group_bytes = group
                .iter()
                .map(|item| serde_json::to_vec(*item).map_or(usize::MAX, |bytes| bytes.len()))
                .fold(0_usize, usize::saturating_add);
            if used.saturating_add(group_bytes) <= max_bytes || selected_reversed.is_empty() {
                used = used.saturating_add(group_bytes);
                selected_reversed.push(group.clone());
            } else {
                break;
            }
        }
        selected_reversed.reverse();
        let items = selected_reversed
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let dropped = self.items.len().saturating_sub(items.len());
        (Self::from_items(items), used, dropped)
    }
}

/// Reduziert eine Liste von `ContentPart`s auf reinen Text. Bilder werden zu
/// einem `[image]`-Platzhalter, bis multimodale Eingaben gebraucht werden.
fn flatten_content(parts: &[ContentPart]) -> String {
    let mut buf = String::new();
    for part in parts {
        match part {
            ContentPart::Text { text } => buf.push_str(text),
            ContentPart::ImageUrl { .. } => buf.push_str("[image]"),
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    // Baut einen Call-Eintrag mit gegebener call_id; `id` bleibt zufällig,
    // weil die Gruppierung nur auf `call_id` schaut.
    fn call(id: &str, name: &str) -> TurnItem {
        TurnItem::ToolCall(ToolCallItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            tool_name: name.to_owned(),
            arguments: serde_json::json!({}),
        })
    }

    // Baut ein Ergebnis-Item zu `id` mit einem klein gehaltenen Payload.
    fn result(id: &str) -> TurnItem {
        TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            result: ToolCallResult::success(serde_json::json!({"ok": true})),
            duration_ms: 1,
        })
    }

    // Baut ein Ergebnis-Item mit einem großen String-Payload, um im
    // Byte-Budget-Test gezielt Gruppen zum Kürzen zu zwingen.
    fn big_result(id: &str, payload_len: usize) -> TurnItem {
        TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            result: ToolCallResult::success(serde_json::json!({
                "data": "x".repeat(payload_len)
            })),
            duration_ms: 1,
        })
    }

    fn user(text: &str) -> TurnItem {
        TurnItem::UserMessage(UserMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        })
    }

    fn assistant(text: &str) -> TurnItem {
        TurnItem::AssistantMessage(AssistantMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            phase: None,
        })
    }

    // Vergleicht Gruppen anhand ihrer call_id/Variant-Signatur, weil
    // `TurnItem` kein `PartialEq` ableitet.
    fn signature(item: &TurnItem) -> &'static str {
        match item {
            TurnItem::UserMessage(_) => "user",
            TurnItem::AssistantMessage(_) => "assistant",
            TurnItem::ToolCall(_) => "call",
            TurnItem::ToolResult(_) => "result",
            TurnItem::Reasoning(_) => "reasoning",
            TurnItem::Error(_) => "error",
        }
    }

    fn signatures(groups: &[Vec<&TurnItem>]) -> Vec<Vec<&'static str>> {
        groups
            .iter()
            .map(|g| g.iter().map(|item| signature(item)).collect())
            .collect()
    }

    #[test]
    fn test_atomic_groups_parallel_round_forms_one_group() {
        let mut history = ConversationHistory::new();
        history.push(user("hi"));
        history.push(call("a", "search"));
        history.push(call("b", "search"));
        history.push(result("a"));
        history.push(result("b"));

        let groups = history.atomic_groups();

        assert_eq!(
            signatures(&groups),
            vec![vec!["user"], vec!["call", "call", "result", "result"]]
        );
    }

    #[test]
    fn test_atomic_groups_sequential_rounds_do_not_merge() {
        let mut history = ConversationHistory::new();
        history.push(call("a", "search"));
        history.push(result("a"));
        history.push(call("b", "search"));
        history.push(result("b"));

        let groups = history.atomic_groups();

        assert_eq!(
            signatures(&groups),
            vec![vec!["call", "result"], vec!["call", "result"]]
        );
    }

    #[test]
    fn test_atomic_groups_results_in_reverse_order_stay_one_group() {
        let mut history = ConversationHistory::new();
        history.push(call("a", "search"));
        history.push(call("b", "search"));
        history.push(result("b"));
        history.push(result("a"));

        let groups = history.atomic_groups();

        assert_eq!(
            signatures(&groups),
            vec![vec!["call", "call", "result", "result"]]
        );
    }

    #[test]
    fn test_atomic_groups_orphaned_result_forms_own_group() {
        let mut history = ConversationHistory::new();
        history.push(result("ghost"));

        let groups = history.atomic_groups();

        assert_eq!(signatures(&groups), vec![vec!["result"]]);
    }

    #[test]
    fn test_atomic_groups_assistant_between_call_and_result_ends_round() {
        let mut history = ConversationHistory::new();
        history.push(call("a", "search"));
        history.push(assistant("thinking out loud"));
        history.push(result("a"));

        let groups = history.atomic_groups();

        // Die Assistant-Nachricht leert `unanswered`; das folgende Ergebnis
        // hat daher keinen offenen Call mehr und bildet eine eigene Gruppe.
        assert_eq!(
            signatures(&groups),
            vec![vec!["call"], vec!["assistant"], vec!["result"]]
        );
    }

    #[test]
    fn test_tail_within_estimated_bytes_never_drops_call_but_keeps_result() {
        let mut history = ConversationHistory::new();
        history.push(call("a", "search"));
        history.push(call("b", "search"));
        history.push(big_result("a", 500));
        history.push(big_result("b", 500));

        // Budget fasst nur einen Bruchteil der vollen Runde, aber die
        // Gruppierung ist atomar: entweder die ganze Runde bleibt, oder sie
        // fällt komplett weg. Kleines Budget erzwingt "mindestens eine
        // Gruppe" (siehe `|| selected_reversed.is_empty()`), das muss dann
        // die vollständige Runde sein.
        let (tail, _used, _dropped) = history.tail_within_estimated_bytes(16);

        let call_ids: std::collections::HashSet<&str> = tail
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(c) => Some(c.call_id.as_str()),
                _ => None,
            })
            .collect();
        let result_ids: std::collections::HashSet<&str> = tail
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolResult(r) => Some(r.call_id.as_str()),
                _ => None,
            })
            .collect();

        // Jedes im Tail vorhandene Ergebnis muss einen passenden Call im
        // selben Tail haben — nie ein verwaistes Tool-Result.
        for id in &result_ids {
            assert!(
                call_ids.contains(id),
                "result for call_id {id} ohne zugehörigen Call im Tail"
            );
        }
    }
}

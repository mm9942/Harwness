//! `ConversationHistory` — akkumuliert Turn-Items über die Lebenszeit
//! einer Session.
//!
//! Serialisierbar (für `StateStore`-Persistenz) und mit Komfort-Konstruktoren
//! für die drei zentralen Message-Typen: User, Assistant, Tool.

use harw_protocol::items::{
    AssistantMessageItem, ContentPart, ErrorItem, ResultTrust, ToolCallItem, ToolCallResult,
    ToolResultItem, TurnItem, UserMessageItem,
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

/// Ergebnis von [`ConversationHistory::tail_preserving_current_turn`] (Knoten
/// A4): wie viele Bytes die zurückgegebene Historie belegt, wie viele Items
/// komplett entfernt wurden, und bei wie vielen Tool-Ergebnissen der Inhalt
/// gekürzt wurde (vollständiger Platzhalter oder Kopf/Fuß-Kappung).
///
/// # Description
/// Ersetzt die abgeschnittene `(Self, usize, usize)`-Rückgabe von
/// [`ConversationHistory::tail_within_estimated_bytes`], die keinen Platz für
/// eine Kürzungs-Zählung hatte. Der alte Wrapper bleibt für bestehende
/// Aufrufer außerhalb dieses Moduls erhalten und leitet intern an diese
/// Methode weiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TailOutcome {
    /// Tatsächlich verbrauchte Bytes der zurückgegebenen Historie.
    pub used_bytes: usize,
    /// Anzahl vollständig entfernter Items (nie ein halbes Call/Ergebnis-Paar).
    pub items_dropped: usize,
    /// Anzahl Tool-Ergebnisse, deren Inhalt gekürzt (Platzhalter oder
    /// Kopf/Fuß-Kappung) statt entfernt wurde.
    pub results_truncated: usize,
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
            trust: ResultTrust::Untrusted,
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
        // Reine Referenz-Projektion über `group_indices` (Knoten A4): dieselbe
        // Atomaritätsregel bedient sowohl diesen geliehenen Pfad als auch den
        // im-Wert-Umbau in `tail_preserving_current_turn`, statt sie zweimal
        // zu pflegen.
        group_indices(&self.items)
            .into_iter()
            .map(|indices| indices.into_iter().map(|index| &self.items[index]).collect())
            .collect()
    }

    /// Rückwärtskompatibler Wrapper um [`Self::tail_preserving_current_turn`]
    /// (Knoten A4).
    ///
    /// # Description
    /// Aufrufer außerhalb dieses Moduls (z. B.
    /// `harw_core::model::ModelRequest::with_context_program`) kennen nur
    /// diese `(Self, usize, usize)`-Signatur und liegen außerhalb des
    /// Schreibbereichs dieses Knotens. Diese Methode leitet deshalb an
    /// [`Self::tail_preserving_current_turn`] weiter — mit `max_bytes / 2`
    /// als Ergebnis-Kappungsgrenze, demselben Wert, den
    /// `harw_core::context_budget::ContextBudget::tool_result_cap` für
    /// `max_history_bytes` liefert — statt die alte, fehleranfällige
    /// Nur-nach-Bytegröße-Auswahl zu behalten. Jeder bestehende Aufrufer
    /// profitiert dadurch automatisch vom Bugfix (die zuletzt gesendete
    /// `UserMessage` wird nicht mehr verdrängt), ohne seine Signatur ändern
    /// zu müssen; die Kürzungs-Zählung geht in diesem schmalen Rückgabetyp
    /// verloren — Aufrufer, die sie brauchen, nutzen
    /// [`Self::tail_preserving_current_turn`] direkt.
    ///
    /// # Returns
    /// `(history, used_bytes, items_dropped)` — siehe [`TailOutcome`] für die
    /// vollständige Zählung inklusive gekürzter Ergebnisse.
    #[must_use]
    pub fn tail_within_estimated_bytes(&self, max_bytes: usize) -> (Self, usize, usize) {
        let (history, outcome) = self.tail_preserving_current_turn(max_bytes, max_bytes / 2);
        (history, outcome.used_bytes, outcome.items_dropped)
    }

    /// Wählt die neueste Projektion der Historie, die in `max_bytes` passt —
    /// unter der Zusicherung, dass die zuletzt angehängte `UserMessage` (der
    /// Auslöser des aktuell offenen Turns) **immer** enthalten bleibt
    /// (Knoten A4).
    ///
    /// # Description
    /// Behebt die Regression, bei der eine lange Werkzeug-Runde (viele
    /// `fs.read`/`shell.exec`-Ergebnisse im selben Turn) die auslösende
    /// `UserMessage` aus dem Modellkontext verdrängte: die alte
    /// Nur-nach-Bytegröße-Auswahl behielt die neuesten Gruppen, ohne zu
    /// wissen, dass eine `UserMessage` wichtiger ist als ein später
    /// eingetroffenes Tool-Ergebnis. Ablauf:
    ///
    /// 1. **Globale Kappung** (unabhängig vom Budget): jedes Tool-Ergebnis,
    ///    das allein mehr als `tool_result_cap` Bytes belegt, wird auf
    ///    Kopf- und Schluss-Hälfte mit dem Kürzungs-Platzhalter dazwischen
    ///    reduziert — ein einzelnes übergroßes Ergebnis darf das Budget nie
    ///    allein sprengen.
    /// 2. Die letzte `UserMessage`-Gruppe wird ermittelt und **immer**
    ///    übernommen, unabhängig davon, ob sie allein schon `max_bytes`
    ///    überschreitet.
    /// 3. Die Gruppen **nach** dieser Nachricht (der aktuell offene Turn)
    ///    werden vollständig zu übernehmen versucht. Passen sie nicht, wird
    ///    zuerst der Inhalt der ältesten Tool-Ergebnisse dieses Bereichs
    ///    vollständig durch den Kürzungs-Platzhalter ersetzt (die
    ///    Call/Ergebnis-Paarung bleibt dabei gültig) — erst wenn das nicht
    ///    reicht, fallen die ältesten Paare dieses Bereichs komplett weg.
    /// 4. Mit dem verbleibenden Budget werden Gruppen **vor** der Nachricht
    ///    (abgeschlossene, ältere Turns) neueste zuerst aufgenommen, ganz
    ///    oder gar nicht — hier wird nicht gekürzt, nur ausgelassen.
    ///
    /// Enthält die Historie keine `UserMessage` (z. B. eine
    /// Zwischenprojektion aus reinen Tool-Runden), verhält sich diese Methode
    /// wie vor diesem Knoten: neueste Gruppen zuerst, ganz oder gar nicht,
    /// nur über bereits global gekappte Items.
    ///
    /// In jedem Fall bleibt die chronologische Reihenfolge der Ausgabe
    /// erhalten, und ein Tool-Call/Ergebnis-Paar wird nie getrennt (siehe
    /// [`Self::atomic_groups`]).
    ///
    /// # Arguments
    /// - `max_bytes` (`usize`): das Gesamtbudget für die zurückgegebene
    ///   Historie (`ContextBudget::max_history_bytes`).
    /// - `tool_result_cap` (`usize`): die pro-Ergebnis-Kappungsgrenze
    ///   (`ContextBudget::tool_result_cap`, standardmäßig `max_bytes / 2`).
    ///
    /// # Returns
    /// Die neue, begrenzte Historie sowie ein [`TailOutcome`] mit den
    /// tatsächlich verbrauchten Bytes, der Anzahl komplett entfernter Items
    /// und der Anzahl gekürzter Tool-Ergebnisse.
    #[must_use]
    pub fn tail_preserving_current_turn(
        &self,
        max_bytes: usize,
        tool_result_cap: usize,
    ) -> (Self, TailOutcome) {
        // Schritt 1: globale, budget-unabhängige Kappung übergroßer
        // Einzelergebnisse.
        let mut results_truncated = 0_usize;
        let processed: Vec<TurnItem> = self
            .items
            .iter()
            .map(|item| match item {
                TurnItem::ToolResult(result_item) if item_bytes(item) > tool_result_cap => {
                    results_truncated += 1;
                    TurnItem::ToolResult(head_tail_truncated_result(result_item, tool_result_cap))
                }
                other => other.clone(),
            })
            .collect();

        let index_groups = group_indices(&processed);
        let mut slots: Vec<Option<TurnItem>> = processed.into_iter().map(Some).collect();
        let mut groups: Vec<Vec<TurnItem>> = index_groups
            .into_iter()
            .map(|indices| {
                indices
                    .into_iter()
                    .map(|index| {
                        slots[index]
                            .take()
                            .expect("group_indices liefert jeden Index genau einmal")
                    })
                    .collect()
            })
            .collect();

        let user_idx = groups
            .iter()
            .rposition(|group| matches!(group.as_slice(), [TurnItem::UserMessage(_)]));

        let Some(user_idx) = user_idx else {
            return newest_first_tail(groups, max_bytes, results_truncated);
        };

        let after = groups.split_off(user_idx + 1);
        let user_group = groups.pop().expect("user_idx ist ein gültiger Index");
        let before = groups;

        let mut used = group_bytes(&user_group);
        let mut items_dropped = 0_usize;

        // Schritt 3: der aktuell offene Turn — erst kürzen, dann (falls das
        // nicht reicht) älteste Paare fallenlassen.
        let mut after = after;
        let mut after_bytes: usize = after.iter().map(|group| group_bytes(group)).sum();

        if used.saturating_add(after_bytes) > max_bytes {
            'truncate: for group in &mut after {
                for item in group.iter_mut() {
                    if used.saturating_add(after_bytes) <= max_bytes {
                        break 'truncate;
                    }
                    if let TurnItem::ToolResult(result_item) = &*item {
                        let before_item_bytes = item_bytes(item);
                        let placeholder_item =
                            TurnItem::ToolResult(full_placeholder_result(result_item, before_item_bytes));
                        let placeholder_bytes = item_bytes(&placeholder_item);
                        if placeholder_bytes < before_item_bytes {
                            *item = placeholder_item;
                            after_bytes = after_bytes
                                .saturating_sub(before_item_bytes)
                                .saturating_add(placeholder_bytes);
                            results_truncated += 1;
                        }
                    }
                }
            }
        }

        while used.saturating_add(after_bytes) > max_bytes && !after.is_empty() {
            let removed = after.remove(0);
            items_dropped += removed.len();
            after_bytes = after_bytes.saturating_sub(group_bytes(&removed));
        }
        used = used.saturating_add(after_bytes);

        // Schritt 4: abgeschlossene, ältere Turns — neueste zuerst, ganz
        // oder gar nicht, keine Kürzung.
        let mut selected_before_reversed: Vec<Vec<TurnItem>> = Vec::new();
        let mut before_iter = before.into_iter().rev();
        while let Some(group) = before_iter.next() {
            let group_size = group_bytes(&group);
            if used.saturating_add(group_size) <= max_bytes {
                used = used.saturating_add(group_size);
                selected_before_reversed.push(group);
            } else {
                items_dropped += group.len();
                items_dropped += before_iter.map(|g| g.len()).sum::<usize>();
                break;
            }
        }
        selected_before_reversed.reverse();

        let mut items = Vec::new();
        for group in selected_before_reversed {
            items.extend(group);
        }
        items.extend(user_group);
        for group in after {
            items.extend(group);
        }

        (
            Self::from_items(items),
            TailOutcome {
                used_bytes: used,
                items_dropped,
                results_truncated,
            },
        )
    }
}

/// Rückfallpfad ohne `UserMessage` in der Historie (siehe
/// [`ConversationHistory::tail_preserving_current_turn`]): unverändertes
/// newest-fit-Verhalten von vor Knoten A4, nur über bereits global gekappte
/// Items.
fn newest_first_tail(
    groups: Vec<Vec<TurnItem>>,
    max_bytes: usize,
    results_truncated: usize,
) -> (ConversationHistory, TailOutcome) {
    let mut selected_reversed: Vec<Vec<TurnItem>> = Vec::new();
    let mut used = 0_usize;
    let mut items_dropped = 0_usize;
    let mut iter = groups.into_iter().rev();
    while let Some(group) = iter.next() {
        let group_size = group_bytes(&group);
        if used.saturating_add(group_size) <= max_bytes || selected_reversed.is_empty() {
            used = used.saturating_add(group_size);
            selected_reversed.push(group);
        } else {
            items_dropped += group.len();
            items_dropped += iter.map(|g| g.len()).sum::<usize>();
            break;
        }
    }
    selected_reversed.reverse();
    let items: Vec<TurnItem> = selected_reversed.into_iter().flatten().collect();
    (
        ConversationHistory::from_items(items),
        TailOutcome {
            used_bytes: used,
            items_dropped,
            results_truncated,
        },
    )
}

/// Gruppiert Item-Indizes nach derselben Atomaritätsregel wie
/// [`ConversationHistory::atomic_groups`] — einzige Quelle der Wahrheit für
/// beide Konsumenten (geliehene und im-Wert-umgebaute Gruppen, siehe Knoten
/// A4).
fn group_indices(items: &[TurnItem]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    // Die Call-IDs der laufenden Tool-Runde, die noch kein Ergebnis haben.
    // Solange hier etwas offen ist, gehört alles Weitere zur selben Runde.
    let mut unanswered: Vec<&ToolCallId> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            TurnItem::ToolCall(call) => {
                if unanswered.is_empty() {
                    groups.push(vec![index]);
                } else if let Some(group) = groups.last_mut() {
                    group.push(index);
                } else {
                    groups.push(vec![index]);
                }
                unanswered.push(&call.call_id);
            }
            TurnItem::ToolResult(result) => {
                let open = unanswered
                    .iter()
                    .position(|call_id| **call_id == result.call_id);
                match (open, groups.last_mut()) {
                    (Some(pos), Some(group)) => {
                        unanswered.remove(pos);
                        group.push(index);
                    }
                    // Ein Ergebnis ohne offenen Call gehört zu nichts und
                    // bleibt für sich — so bleibt die Funktion auch auf
                    // beschädigten Verläufen total.
                    _ => groups.push(vec![index]),
                }
            }
            _ => {
                unanswered.clear();
                groups.push(vec![index]);
            }
        }
    }
    groups
}

/// Geschätzte Serialisierungsgröße eines einzelnen Items in Bytes.
/// `usize::MAX`, falls die Serialisierung fehlschlägt.
fn item_bytes(item: &TurnItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

/// Summe der [`item_bytes`] aller Items einer atomaren Gruppe.
fn group_bytes(group: &[TurnItem]) -> usize {
    group
        .iter()
        .map(item_bytes)
        .fold(0_usize, usize::saturating_add)
}

/// Deutscher Kürzungs-Platzhalter mit der Anzahl ausgelassener Bytes
/// (Knoten A4).
fn truncated_placeholder(elided_bytes: usize) -> String {
    format!("[Tool-Ergebnis gekürzt: {elided_bytes} Bytes ausgelassen]")
}

/// Reintext-Sicht auf ein `ToolCallResult`, als Grundlage für die
/// Kürzungsfunktionen unten. Ein reiner `Value::String` wird unverpackt
/// zurückgegeben (kein zusätzliches JSON-Anführungszeichen-Escaping), jeder
/// andere `Value` über die normale JSON-Serialisierung.
fn tool_result_text(result: &ToolCallResult) -> String {
    match result {
        ToolCallResult::Success {
            value: serde_json::Value::String(text),
        } => text.clone(),
        ToolCallResult::Success { value } => serde_json::to_string(value).unwrap_or_default(),
        ToolCallResult::Error { message } => message.clone(),
    }
}

/// Ersetzt den Inhalt eines `ToolCallResult` durch `text`, unter Beibehaltung
/// der `Success`/`Error`-Variante — der Provider-Vertrag verlangt eine
/// gültige Struktur, kein bestimmtes Feld.
fn tool_result_with_text(result: &ToolCallResult, text: String) -> ToolCallResult {
    match result {
        ToolCallResult::Success { .. } => ToolCallResult::Success {
            value: serde_json::Value::String(text),
        },
        ToolCallResult::Error { .. } => ToolCallResult::Error { message: text },
    }
}

/// Ersetzt den Inhalt eines Tool-Ergebnisses vollständig durch den
/// Kürzungs-Platzhalter — `call_id`/`id`/`duration_ms`/`trust` bleiben
/// unverändert, damit die Call/Ergebnis-Paarung für den Provider gültig
/// bleibt (Schritt 3 in [`ConversationHistory::tail_preserving_current_turn`]).
fn full_placeholder_result(item: &ToolResultItem, original_bytes: usize) -> ToolResultItem {
    ToolResultItem {
        id: item.id.clone(),
        call_id: item.call_id.clone(),
        result: tool_result_with_text(&item.result, truncated_placeholder(original_bytes)),
        duration_ms: item.duration_ms,
        trust: item.trust,
    }
}

/// Kappt den Inhalt eines Tool-Ergebnisses auf ungefähr `cap` Bytes: behält
/// je zur Hälfte Anfang und Ende des ursprünglichen Texts, mit dem
/// Kürzungs-Platzhalter dazwischen — die budget-unabhängige "immer"-Regel
/// aus Schritt 1 von [`ConversationHistory::tail_preserving_current_turn`].
/// Ein Ergebnis, dessen Text schon innerhalb von `cap` liegt, bleibt
/// unverändert.
fn head_tail_truncated_result(item: &ToolResultItem, cap: usize) -> ToolResultItem {
    let text = tool_result_text(&item.result);
    if text.len() <= cap {
        return item.clone();
    }
    let approx_elided = text.len().saturating_sub(cap);
    let placeholder_guess = truncated_placeholder(approx_elided);
    let available = cap.saturating_sub(placeholder_guess.len());
    let head_len = crate::context_budget::floor_char_boundary(&text, available / 2);
    let tail_keep = available.saturating_sub(head_len);
    let tail_from = ceil_char_boundary(&text, text.len().saturating_sub(tail_keep));
    let elided = tail_from.saturating_sub(head_len);
    let placeholder = truncated_placeholder(elided);

    let mut truncated =
        String::with_capacity(head_len + placeholder.len() + text.len().saturating_sub(tail_from));
    truncated.push_str(&text[..head_len]);
    truncated.push_str(&placeholder);
    truncated.push_str(&text[tail_from..]);

    ToolResultItem {
        id: item.id.clone(),
        call_id: item.call_id.clone(),
        result: tool_result_with_text(&item.result, truncated),
        duration_ms: item.duration_ms,
        trust: item.trust,
    }
}

/// Kleinste Byteposition `>= index`, die in `value` auf einer Zeichengrenze
/// liegt — das Gegenstück zu `crate::context_budget::floor_char_boundary`
/// für das Ende des behaltenen Textausschnitts.
fn ceil_char_boundary(value: &str, index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    let mut position = index;
    while position < value.len() && !value.is_char_boundary(position) {
        position += 1;
    }
    position
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
    use harw_protocol::items::ReasoningItem;

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
            trust: ResultTrust::Untrusted,
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
            trust: ResultTrust::Untrusted,
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

    /// W3/C-MODEL-Regressionsschutz (G-015): Reasoning-Blöcke im Verlauf
    /// bleiben über einen Serde-Roundtrip hinweg speicher- und
    /// zurückspielbar — **unverändert** durch den neuen Stop-/
    /// Reasoning-/Fehlervertrag in `model.rs`. Die eigentliche Verdrahtung
    /// mit `harw_protocol::OpaqueReasoning` (unverändertes Zurückspielen an
    /// den Provider) ist Folgearbeit von W4a (A-LOOP/A-ANTH); dieser Test
    /// sichert nur zu, dass die bestehende `TurnItem::Reasoning`-Speicherung
    /// durch diesen Knoten nicht beschädigt wurde.
    #[test]
    fn test_conversation_history_roundtrip_preserves_reasoning_item() {
        let mut history = ConversationHistory::new();
        history.push(user("hi"));
        let reasoning_id = ItemId::new();
        history.push(TurnItem::Reasoning(ReasoningItem {
            id: reasoning_id.clone(),
            summary_text: vec!["thinking about the answer".to_owned()],
            raw_content: vec!["raw provider thinking payload".to_owned()],
        }));
        history.push(assistant("here is the answer"));

        let json = serde_json::to_string(&history).expect("ConversationHistory serializes");
        let restored: ConversationHistory =
            serde_json::from_str(&json).expect("ConversationHistory deserializes");

        assert_eq!(restored.len(), 3);
        let restored_signatures: Vec<&'static str> =
            restored.items().iter().map(signature).collect();
        assert_eq!(restored_signatures, vec!["user", "reasoning", "assistant"]);

        match &restored.items()[1] {
            TurnItem::Reasoning(item) => {
                assert_eq!(item.id, reasoning_id);
                assert_eq!(
                    item.summary_text,
                    vec!["thinking about the answer".to_owned()]
                );
                assert_eq!(
                    item.raw_content,
                    vec!["raw provider thinking payload".to_owned()]
                );
            }
            other => panic!("expected a Reasoning item after roundtrip, got {other:?}"),
        }

        // `to_model_messages` lässt Reasoning-Items weiterhin bewusst aus
        // (unverändert, siehe Moduldoku von `to_model_messages`) — auch nach
        // dem Roundtrip.
        let messages = restored.to_model_messages();
        assert_eq!(messages.len(), 2);
    }

    // ------------------------------------------------------------------
    // Knoten A4: `tail_preserving_current_turn` / `TailOutcome`.
    // ------------------------------------------------------------------

    fn tool_result_string_value(item: &TurnItem) -> Option<String> {
        match item {
            TurnItem::ToolResult(ToolResultItem {
                result: ToolCallResult::Success { value },
                ..
            }) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    #[test]
    fn test_tail_preserving_current_turn_always_keeps_last_user_message() {
        let mut history = ConversationHistory::new();
        // Größer als das gesamte Budget unten (256 KiB): egal wie viel
        // Restbudget die Kürzung des aktuellen Turns freigibt, dieser ältere
        // Turn kann darin nie Platz finden — ein deterministischer Test
        // dafür, dass ältere Turns dem aktuellen Turn weichen, ohne auf eine
        // exakt berechnete Restbudget-Größe angewiesen zu sein.
        history.push(user(&"älterer, abgeschlossener Turn ".repeat(20_000)));
        history.push(user("current trigger message"));
        for i in 0..10 {
            history.push(call(&format!("call-{i}"), "tool"));
            history.push(big_result(&format!("call-{i}"), 40_000));
        }

        let (tail, outcome) = history.tail_preserving_current_turn(256 * 1024, 128 * 1024);

        // Die auslösende UserMessage des aktuell offenen Turns bleibt immer
        // erhalten — der eigentliche Bugfix dieses Knotens.
        let has_current_user = tail.items().iter().any(|item| {
            matches!(
                item,
                TurnItem::UserMessage(m)
                    if flatten_content(&m.content) == "current trigger message"
            )
        });
        assert!(has_current_user, "die aktuelle UserMessage fehlt im Tail");

        // Der abgeschlossene ältere Turn wird zuerst geopfert.
        let has_old_turn = tail.items().iter().any(|item| {
            matches!(
                item,
                TurnItem::UserMessage(m) if flatten_content(&m.content).starts_with("älterer")
            )
        });
        assert!(
            !has_old_turn,
            "der ältere, abgeschlossene Turn hätte vor dem aktuellen Turn weichen müssen"
        );

        // Zehn Paare à 40 KiB passen nicht unbeschnitten neben die
        // UserMessage in 256 KiB — irgendetwas musste gekürzt oder
        // fallengelassen werden.
        assert!(
            outcome.results_truncated > 0 || outcome.items_dropped > 0,
            "weder gekürzt noch fallengelassen, obwohl das Budget das erzwingt"
        );

        // Kein Tool-Call/Ergebnis-Paar wurde getrennt.
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
        assert_eq!(call_ids, result_ids, "ein Paar wurde getrennt");

        // Chronologische Reihenfolge: die im Tail vorhandenen Paar-Indizes
        // steigen monoton.
        let mut seen_indices = Vec::new();
        for item in tail.items() {
            if let TurnItem::ToolCall(c) = item {
                let index: usize = c
                    .call_id
                    .as_str()
                    .strip_prefix("call-")
                    .and_then(|s| s.parse().ok())
                    .expect("Test-Call-IDs haben die Form call-<n>");
                seen_indices.push(index);
            }
        }
        let mut sorted = seen_indices.clone();
        sorted.sort_unstable();
        assert_eq!(seen_indices, sorted, "Reihenfolge ist nicht chronologisch");

        // Das neueste Paar (call-9) ist am unwahrscheinlichsten betroffen und
        // bleibt unbeschnitten erhalten.
        if call_ids.contains("call-9") {
            let newest_result_text = tail
                .items()
                .iter()
                .find_map(|item| match item {
                    TurnItem::ToolResult(r) if r.call_id.as_str() == "call-9" => {
                        tool_result_string_value(item)
                    }
                    _ => None,
                });
            assert!(
                newest_result_text.is_none(),
                "das neueste Ergebnis sollte nicht auf einen reinen String-Platzhalter \
                 reduziert worden sein"
            );
        }
    }

    #[test]
    fn test_tail_preserving_current_turn_always_truncates_oversized_single_result_head_tail() {
        let mut history = ConversationHistory::new();
        history.push(user("trigger"));
        history.push(call("only", "fs.read"));
        let huge_text = format!("HEADMARK{}TAILMARK", "y".repeat(300_000));
        history.push(TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str("only"),
            result: ToolCallResult::success(serde_json::json!(huge_text)),
            duration_ms: 1,
            trust: ResultTrust::Untrusted,
        }));

        // Großzügiges Gesamtbudget: nur die "immer"-Regel (Schritt 1) soll
        // hier greifen, nicht die budgetgetriebene Kürzung aus Schritt 3.
        let (tail, outcome) = history.tail_preserving_current_turn(10 * 1024 * 1024, 128 * 1024);

        assert_eq!(outcome.results_truncated, 1);
        assert_eq!(outcome.items_dropped, 0);

        let truncated_value = tail
            .items()
            .iter()
            .find_map(|item| match item {
                TurnItem::ToolResult(r) if r.call_id.as_str() == "only" => {
                    tool_result_string_value(item)
                }
                _ => None,
            })
            .expect("das einzige Tool-Ergebnis bleibt als Erfolg mit Text-Inhalt erhalten");

        assert!(
            truncated_value.starts_with("HEADMARK"),
            "Kopf des Originaltexts fehlt (Länge: {})",
            truncated_value.len()
        );
        assert!(
            truncated_value.ends_with("TAILMARK"),
            "Fuß des Originaltexts fehlt (Länge: {})",
            truncated_value.len()
        );
        assert!(
            truncated_value.contains("Tool-Ergebnis gekürzt"),
            "Platzhalter fehlt (Länge: {})",
            truncated_value.len()
        );
        assert!(
            truncated_value.len() < huge_text.len(),
            "der gekürzte Text ist nicht kleiner als das Original"
        );
    }

    #[test]
    fn test_tail_within_estimated_bytes_wrapper_keeps_current_user_message() {
        // Regressionsschutz für den genauen Fehler aus dem Auftrag dieses
        // Knotens: eine UserMessage, gefolgt von einem Tool-Paar, das allein
        // schon das komplette Budget ausschöpft, durfte die UserMessage
        // vorher verdrängen.
        let call_id = ToolCallId::new();
        let mut pair_only = ConversationHistory::new();
        pair_only.push_tool_call(call_id.clone(), "lookup", serde_json::json!({"q": "x"}));
        pair_only.push_tool_result(
            call_id.clone(),
            ToolCallResult::success(serde_json::json!({"answer": "ok"})),
            1,
        );
        let (_, pair_bytes, _) = pair_only.tail_within_estimated_bytes(usize::MAX);

        let mut history = ConversationHistory::new();
        history.push_user_text("trigger");
        history.push_tool_call(call_id.clone(), "lookup", serde_json::json!({"q": "x"}));
        history.push_tool_result(
            call_id.clone(),
            ToolCallResult::success(serde_json::json!({"answer": "ok"})),
            1,
        );

        let (tail, _used, dropped) = history.tail_within_estimated_bytes(pair_bytes);

        assert!(
            tail.items()
                .iter()
                .any(|item| matches!(item, TurnItem::UserMessage(_))),
            "die UserMessage wurde trotz Bugfix verdrängt"
        );
        assert!(dropped > 0, "das Tool-Paar hätte weichen müssen");
    }
}

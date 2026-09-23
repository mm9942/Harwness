//! Live-Beobachtung aller Agenten eines Laufs (Wurzel, Kinder, UIA-Worker).
//!
//! [`AgentMonitor`] ist reiner Zustand: er konsumiert [`AgentEvent`]s vom
//! [`harw_core::AgentEventHub`] und hält pro Agent Status, aktuelles
//! Werkzeug, Live-Tokens, Kontextbelegung, eine kurze Text-Vorschau und eine
//! begrenzte Spur ([`AgentTrace`]) aus Text, Reasoning, Werkzeugaufrufen,
//! Ergebnissen und Statusmeldungen.
//! [`render_agents_panel`] projiziert diesen Zustand kompakt in das rechte
//! Seitenpanel der TUI, [`AgentMonitor::render_agent_detail`] zeigt die
//! Detailansicht eines einzelnen Agenten; die Statuszeile liest
//! [`AgentMonitor::totals`].

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use harw_core::{AgentEvent, AgentEventKind};
use harw_protocol::{AgentOrchestrationStatus, ContentPart, ToolCallResult, TurnEvent, TurnItem};
use harw_types::{SessionId, TokenUsage};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::sanitize::{sanitize_display, sanitize_inline};
use crate::style::{self, Theme};

/// Maximale Länge der Live-Text-Vorschau je Agent (Zeichen).
const PREVIEW_CHARS: usize = 240;
/// Maximale Anzahl Einträge einer [`AgentTrace`].
pub(crate) const MAX_TRACE_ENTRIES: usize = 400;
/// Maximale Textmenge (Bytes) einer [`AgentTrace`].
pub(crate) const MAX_TRACE_BYTES: usize = 64 * 1024;
/// Maximale Länge der Argument-Vorschau eines Werkzeugaufrufs (Zeichen).
const ARGS_PREVIEW_CHARS: usize = 160;
/// Maximale Länge der Ergebnis-Vorschau eines Werkzeugaufrufs (Zeichen).
const RESULT_PREVIEW_CHARS: usize = 240;
/// Maximale Länge einer Statusmeldung (Zeichen).
const STATUS_CHARS: usize = 400;
/// Anzahl gemerkter Werkzeugaufrufe für Namensauflösung und Deduplizierung.
const MAX_CALL_MARKS: usize = 64;

/// Ein Eintrag der Agenten-Spur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TraceEntry {
    /// Sichtbarer Antworttext (Live-Deltas bzw. finale Nachricht).
    Text(String),
    /// Reasoning-/Denktext, getrennt vom Antworttext.
    Reasoning(String),
    /// Angeforderter Werkzeugaufruf mit kompakter Argument-Vorschau.
    ToolCall { name: String, args_preview: String },
    /// Ergebnis eines Werkzeugaufrufs.
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    /// Status-/Orchestrierungsmeldung (Aufgabe, Kind-Ereignisse, Turn-Ende …).
    Status(String),
}

impl TraceEntry {
    fn byte_len(&self) -> usize {
        match self {
            Self::Text(text) | Self::Reasoning(text) | Self::Status(text) => text.len(),
            Self::ToolCall { name, args_preview } => name.len() + args_preview.len(),
            Self::ToolResult { name, preview, .. } => name.len() + preview.len(),
        }
    }

    /// Der größte frei wachsende Textteil des Eintrags.
    fn body_mut(&mut self) -> &mut String {
        match self {
            Self::Text(text) | Self::Reasoning(text) | Self::Status(text) => text,
            Self::ToolCall { args_preview, .. } => args_preview,
            Self::ToolResult { preview, .. } => preview,
        }
    }
}

/// Streamende Eintragsart (Deltas werden an den letzten Eintrag gehängt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Text,
    Reasoning,
}

/// Gemerkter Werkzeugaufruf: Name für Ergebnisse, Flags gegen Doppelungen
/// (derselbe Aufruf kommt als `ToolCallRequested` und ggf. als `ItemAdded`).
#[derive(Debug, Clone)]
struct CallMark {
    id: String,
    name: String,
    call_logged: bool,
    result_logged: bool,
}

/// Begrenzte Spur eines Agenten (höchstens [`MAX_TRACE_ENTRIES`] Einträge
/// und [`MAX_TRACE_BYTES`] Text; älteste Einträge fallen zuerst heraus).
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentTrace {
    entries: VecDeque<TraceEntry>,
    bytes: usize,
    open_text: bool,
    open_reasoning: bool,
    calls: VecDeque<CallMark>,
}

impl AgentTrace {
    /// Alle Einträge, älteste zuerst.
    pub(crate) fn entries(&self) -> impl Iterator<Item = &TraceEntry> {
        self.entries.iter()
    }

    /// Anzahl der Einträge.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true`, wenn die Spur leer ist.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Gesamte Textmenge aller Einträge in Bytes.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn byte_len(&self) -> usize {
        self.bytes
    }

    fn open_flag(&mut self, kind: StreamKind) -> &mut bool {
        match kind {
            StreamKind::Text => &mut self.open_text,
            StreamKind::Reasoning => &mut self.open_reasoning,
        }
    }

    fn matches_kind(entry: &TraceEntry, kind: StreamKind) -> bool {
        matches!(
            (entry, kind),
            (TraceEntry::Text(_), StreamKind::Text)
                | (TraceEntry::Reasoning(_), StreamKind::Reasoning)
        )
    }

    fn make(kind: StreamKind, text: String) -> TraceEntry {
        match kind {
            StreamKind::Text => TraceEntry::Text(text),
            StreamKind::Reasoning => TraceEntry::Reasoning(text),
        }
    }

    fn push(&mut self, entry: TraceEntry) {
        self.bytes += entry.byte_len();
        self.entries.push_back(entry);
        self.enforce_caps();
    }

    /// Hängt ein Live-Delta an: an den letzten Eintrag, wenn dieser von
    /// derselben Art ist und noch streamt, sonst als neuen Eintrag.
    fn push_delta(&mut self, kind: StreamKind, text: &str) {
        if text.is_empty() {
            return;
        }
        let open = *self.open_flag(kind);
        if open
            && let Some(last) = self.entries.back_mut()
            && Self::matches_kind(last, kind)
        {
            last.body_mut().push_str(text);
            self.bytes += text.len();
            self.enforce_caps();
            return;
        }
        *self.open_flag(kind) = true;
        self.push(Self::make(kind, text.to_owned()));
    }

    /// Übernimmt den finalen Text einer Runde. Streamte dieselbe Art, ersetzt
    /// der finale Text den zuletzt gestreamten Eintrag (keine Doppelung);
    /// sonst wird er angehängt. Liefert `true`, wenn ersetzt wurde.
    fn finalize(&mut self, kind: StreamKind, text: String) -> bool {
        let was_open = std::mem::replace(self.open_flag(kind), false);
        if was_open
            && let Some(index) = self
                .entries
                .iter()
                .rposition(|entry| Self::matches_kind(entry, kind))
            && let Some(entry) = self.entries.get_mut(index)
        {
            let body = entry.body_mut();
            self.bytes = self.bytes.saturating_sub(body.len()) + text.len();
            *body = text;
            self.enforce_caps();
            return true;
        }
        if !text.is_empty() {
            self.push(Self::make(kind, text));
        }
        false
    }

    /// Schließt laufende Streams (z. B. am Turn-Ende).
    fn close_streams(&mut self) {
        self.open_text = false;
        self.open_reasoning = false;
    }

    /// Hängt eine Statusmeldung an; identische direkt aufeinanderfolgende
    /// Meldungen werden zusammengefasst.
    fn push_status(&mut self, text: &str) {
        let text = clip_chars(&sanitize_inline(text), STATUS_CHARS);
        if text.is_empty() {
            return;
        }
        if matches!(self.entries.back(), Some(TraceEntry::Status(last)) if *last == text) {
            return;
        }
        self.push(TraceEntry::Status(text));
    }

    fn mark(&mut self, call_id: &str) -> Option<&mut CallMark> {
        self.calls.iter_mut().rev().find(|mark| mark.id == call_id)
    }

    /// Merkt sich einen Aufruf; ältere Marken fallen ab [`MAX_CALL_MARKS`] heraus.
    fn remember(&mut self, call_id: &str, name: &str, call_logged: bool, result_logged: bool) {
        self.calls.push_back(CallMark {
            id: call_id.to_owned(),
            name: name.to_owned(),
            call_logged,
            result_logged,
        });
        while self.calls.len() > MAX_CALL_MARKS {
            self.calls.pop_front();
        }
    }

    /// Protokolliert einen Werkzeugaufruf (höchstens einmal je `call_id`).
    fn record_call(&mut self, call_id: &str, name: &str, arguments: &serde_json::Value) {
        let already = self
            .mark(call_id)
            .map(|mark| std::mem::replace(&mut mark.call_logged, true));
        match already {
            Some(true) => return,
            Some(false) => {}
            None => self.remember(call_id, name, true, false),
        }
        self.push(TraceEntry::ToolCall {
            name: sanitize_inline(name),
            args_preview: args_preview(arguments),
        });
    }

    /// Protokolliert ein Werkzeugergebnis (höchstens einmal je `call_id`).
    fn record_result(&mut self, call_id: &str, result: &ToolCallResult) {
        let known = self.mark(call_id).map(|mark| {
            (
                std::mem::replace(&mut mark.result_logged, true),
                mark.name.clone(),
            )
        });
        let name = match known {
            Some((true, _)) => return,
            Some((false, name)) => name,
            None => {
                self.remember(call_id, "?", true, true);
                "?".to_owned()
            }
        };
        let (ok, preview) = result_preview(result);
        self.push(TraceEntry::ToolResult {
            name: sanitize_inline(&name),
            ok,
            preview,
        });
    }

    /// Erzwingt Eintrags- und Byte-Grenze (älteste Einträge zuerst; ein
    /// einzelner übergroßer Eintrag wird vorne gekürzt).
    fn enforce_caps(&mut self) {
        while self.entries.len() > MAX_TRACE_ENTRIES
            || (self.bytes > MAX_TRACE_BYTES && self.entries.len() > 1)
        {
            match self.entries.pop_front() {
                Some(entry) => self.bytes = self.bytes.saturating_sub(entry.byte_len()),
                None => break,
            }
        }
        if self.bytes > MAX_TRACE_BYTES
            && let Some(entry) = self.entries.back_mut()
        {
            let before = entry.byte_len();
            let body = entry.body_mut();
            let excess = self.bytes - MAX_TRACE_BYTES;
            let mut cut = excess.min(body.len());
            while cut < body.len() && !body.is_char_boundary(cut) {
                cut += 1;
            }
            body.drain(..cut);
            let after = entry.byte_len();
            self.bytes = self.bytes.saturating_sub(before - after);
        }
    }
}

/// Kürzt auf `max` Zeichen und markiert Kürzungen mit `…`.
fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Kompakte, einzeilige Vorschau von Werkzeugargumenten.
fn args_preview(arguments: &serde_json::Value) -> String {
    let raw = match arguments {
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    clip_chars(&sanitize_inline(&raw), ARGS_PREVIEW_CHARS)
}

/// Erfolg und kompakte, einzeilige Vorschau eines Werkzeugergebnisses.
fn result_preview(result: &ToolCallResult) -> (bool, String) {
    let (ok, raw) = match result {
        ToolCallResult::Success { value } => (
            true,
            match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            },
        ),
        ToolCallResult::Error { message } => (false, message.clone()),
    };
    (ok, clip_chars(&sanitize_inline(&raw), RESULT_PREVIEW_CHARS))
}

/// Verbindet die Textteile eines Nachrichteninhalts.
fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lebenszyklus eines beobachteten Agenten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentPhase {
    Admitted,
    Thinking,
    Tool,
    Waiting,
    Done,
    Failed,
    Cancelled,
}

impl AgentPhase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Admitted => "zugelassen",
            Self::Thinking => "denkt",
            Self::Tool => "Werkzeug",
            Self::Waiting => "wartet",
            Self::Done => "fertig",
            Self::Failed => "fehlgeschlagen",
            Self::Cancelled => "abgebrochen",
        }
    }

    fn is_active(self) -> bool {
        matches!(
            self,
            Self::Admitted | Self::Thinking | Self::Tool | Self::Waiting
        )
    }
}

/// Live-Zustand eines Agenten.
#[derive(Debug, Clone)]
pub(crate) struct AgentLive {
    pub id: String,
    pub parent: Option<String>,
    pub role: String,
    pub phase: AgentPhase,
    pub task: Option<String>,
    pub current_tool: Option<String>,
    pub tool_calls: u32,
    /// Summe aller abgeschlossenen Turns.
    pub usage_done: TokenUsage,
    /// Stand des laufenden Turns (live, inkl. streamender Runde).
    pub usage_turn: TokenUsage,
    pub context_used: u64,
    pub context_window: u64,
    pub history_dropped: u32,
    pub compactions: u32,
    /// Ende des sichtbaren Antworttexts (ohne Reasoning).
    pub preview: String,
    /// Ende des Reasoning-Texts, getrennt von [`Self::preview`].
    pub reasoning_preview: String,
    pub started: Instant,
    pub finished: Option<Instant>,
    /// Ausgabe-Tokens beim letzten Messpunkt, für die tok/s-Rate.
    rate_mark: (Instant, u64),
    pub tokens_per_sec: f64,
    /// Begrenzte Spur für die Detailansicht.
    pub trace: AgentTrace,
}

impl AgentLive {
    fn new(id: String, parent: Option<String>, role: String) -> Self {
        let now = Instant::now();
        Self {
            id,
            parent,
            role,
            phase: AgentPhase::Admitted,
            task: None,
            current_tool: None,
            tool_calls: 0,
            usage_done: TokenUsage::default(),
            usage_turn: TokenUsage::default(),
            context_used: 0,
            context_window: 0,
            history_dropped: 0,
            compactions: 0,
            preview: String::new(),
            reasoning_preview: String::new(),
            started: now,
            finished: None,
            rate_mark: (now, 0),
            tokens_per_sec: 0.0,
            trace: AgentTrace::default(),
        }
    }

    /// Gesamtnutzung (abgeschlossen + laufend).
    pub(crate) fn usage(&self) -> TokenUsage {
        let mut total = self.usage_done.clone();
        total.add(&self.usage_turn);
        total
    }

    /// Kontextbelegung in Prozent (0–100), `None` ohne bekanntes Fenster.
    pub(crate) fn context_percent(&self) -> Option<u8> {
        (self.context_window > 0).then(|| {
            let pct = self.context_used.saturating_mul(100) / self.context_window;
            u8::try_from(pct.min(100)).unwrap_or(100)
        })
    }

    pub(crate) fn elapsed_secs(&self) -> u64 {
        self.finished
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_secs()
    }

    fn push_preview(&mut self, text: &str) {
        push_tail(&mut self.preview, text);
    }

    fn push_reasoning_preview(&mut self, text: &str) {
        push_tail(&mut self.reasoning_preview, text);
    }

    fn update_rate(&mut self) {
        let output = self.usage().output_tokens;
        let (at, mark) = self.rate_mark;
        let secs = at.elapsed().as_secs_f64();
        if secs >= 0.5 {
            #[allow(clippy::cast_precision_loss)]
            let delta = output.saturating_sub(mark) as f64;
            self.tokens_per_sec = delta / secs;
            self.rate_mark = (Instant::now(), output);
        }
    }
}

/// Hängt `text` an und behält nur die letzten [`PREVIEW_CHARS`] Zeichen.
fn push_tail(target: &mut String, text: &str) {
    target.push_str(text);
    let count = target.chars().count();
    if count > PREVIEW_CHARS {
        *target = target.chars().skip(count - PREVIEW_CHARS).collect();
    }
}

/// Zustand aller beobachteten Agenten plus interne Nutzung.
#[derive(Debug, Default)]
pub(crate) struct AgentMonitor {
    agents: BTreeMap<String, AgentLive>,
    /// Einfügereihenfolge für stabile Darstellung.
    order: Vec<String>,
    /// Nutzung interner Aufrufe (Kompaktierung, Titel …) nach Zweck.
    internal: BTreeMap<String, TokenUsage>,
    /// Ausgewählter Agent im Panel (Index in [`Self::rows`]).
    pub selected: usize,
}

impl AgentMonitor {
    fn entry(&mut self, event: &AgentEvent) -> &mut AgentLive {
        let id = event.agent.as_str().to_owned();
        if !self.agents.contains_key(&id) {
            self.order.push(id.clone());
        }
        let live = self.agents.entry(id.clone()).or_insert_with(|| {
            AgentLive::new(
                id,
                event.parent.as_ref().map(|p| p.as_str().to_owned()),
                event.role.clone(),
            )
        });
        if live.parent.is_none() {
            live.parent = event.parent.as_ref().map(|p| p.as_str().to_owned());
        }
        live
    }

    /// Verarbeitet ein Bus-Event. Liefert `true`, wenn sich Sichtbares änderte.
    pub(crate) fn apply(&mut self, event: &AgentEvent) -> bool {
        match &event.kind {
            AgentEventKind::InternalUsage { purpose, usage } => {
                self.internal.entry(purpose.clone()).or_default().add(usage);
                true
            }
            AgentEventKind::Orchestration(orch) => {
                let live = self.entry(event);
                if let Some(task) = &orch.task
                    && live.task.as_deref() != Some(task.as_str())
                {
                    live.task = Some(task.clone());
                    live.trace.push_status(&format!("Aufgabe: {task}"));
                }
                if let Some(calls) = orch.tool_calls {
                    live.tool_calls = live.tool_calls.max(calls);
                }
                let before = live.phase;
                live.phase = match orch.status {
                    AgentOrchestrationStatus::Admitted => AgentPhase::Admitted,
                    AgentOrchestrationStatus::Running | AgentOrchestrationStatus::Progress => {
                        if live.phase == AgentPhase::Admitted {
                            AgentPhase::Thinking
                        } else {
                            live.phase
                        }
                    }
                    AgentOrchestrationStatus::Paused => AgentPhase::Waiting,
                    AgentOrchestrationStatus::Completed => AgentPhase::Done,
                    AgentOrchestrationStatus::Failed => AgentPhase::Failed,
                    AgentOrchestrationStatus::Cancelled => AgentPhase::Cancelled,
                };
                if let Some(detail) = &orch.detail {
                    live.trace.push_status(detail);
                }
                if !live.phase.is_active() {
                    live.finished.get_or_insert_with(Instant::now);
                    live.current_tool = None;
                    live.trace.close_streams();
                    if before != live.phase {
                        live.trace
                            .push_status(&format!("Status: {}", live.phase.label()));
                    }
                }
                true
            }
            AgentEventKind::Turn(turn) => self.apply_turn(event, turn),
        }
    }

    fn apply_turn(&mut self, event: &AgentEvent, turn: &TurnEvent) -> bool {
        // Fortschritt eines Kindes betrifft den Kind-Eintrag, nicht den Absender.
        if let TurnEvent::ChildProgress {
            child, tool_calls, ..
        } = turn
        {
            return match self.agents.get_mut(child.as_str()) {
                Some(live) if live.tool_calls < *tool_calls => {
                    live.tool_calls = *tool_calls;
                    true
                }
                _ => false,
            };
        }
        let child_label = match turn {
            TurnEvent::ChildCompleted { child, .. } => Some(
                self.agents
                    .get(child.as_str())
                    .map_or_else(|| child.as_str().to_owned(), |c| c.role.clone()),
            ),
            _ => None,
        };
        let live = self.entry(event);
        match turn {
            TurnEvent::TurnStarted { .. } => {
                live.phase = AgentPhase::Thinking;
                live.finished = None;
                live.usage_turn = TokenUsage::default();
                live.trace.close_streams();
            }
            TurnEvent::AssistantDelta { text, .. } => {
                live.phase = AgentPhase::Thinking;
                live.push_preview(text);
                live.trace.push_delta(StreamKind::Text, text);
            }
            TurnEvent::ReasoningDelta { text, .. } => {
                live.phase = AgentPhase::Thinking;
                live.push_reasoning_preview(text);
                live.trace.push_delta(StreamKind::Reasoning, text);
            }
            TurnEvent::ItemAdded { item, .. } => match item {
                TurnItem::AssistantMessage(message) => {
                    let text = content_text(&message.content);
                    if !live.trace.finalize(StreamKind::Text, text.clone()) {
                        live.push_preview(&text);
                    }
                }
                TurnItem::Reasoning(reasoning) => {
                    let parts = if reasoning.summary_text.is_empty() {
                        &reasoning.raw_content
                    } else {
                        &reasoning.summary_text
                    };
                    let text = parts.join("\n");
                    if !live.trace.finalize(StreamKind::Reasoning, text.clone()) {
                        live.push_reasoning_preview(&text);
                    }
                }
                TurnItem::ToolCall(call) => {
                    live.trace
                        .record_call(call.call_id.as_str(), &call.tool_name, &call.arguments);
                }
                TurnItem::ToolResult(result) => {
                    live.trace
                        .record_result(result.call_id.as_str(), &result.result);
                }
                TurnItem::Error(error) => {
                    live.trace
                        .push_status(&format!("Fehler: {}", error.message));
                }
                TurnItem::UserMessage(message) => {
                    live.trace
                        .push_status(&format!("Eingabe: {}", content_text(&message.content)));
                }
            },
            TurnEvent::UsageUpdated { turn_total, .. } => {
                live.usage_turn = turn_total.clone();
                live.update_rate();
            }
            TurnEvent::ContextUpdated {
                used_tokens,
                window_tokens,
                history_items_dropped,
                ..
            } => {
                live.context_used = *used_tokens;
                if *window_tokens > 0 {
                    live.context_window = *window_tokens;
                }
                live.history_dropped = *history_items_dropped;
            }
            TurnEvent::CompactionApplied {
                items_before,
                items_after,
                ..
            } => {
                live.compactions = live.compactions.saturating_add(1);
                live.trace.push_status(&format!(
                    "Kontext verdichtet: {items_before} → {items_after} Einträge"
                ));
            }
            TurnEvent::ToolCallRequested {
                call_id,
                tool_name,
                arguments,
                ..
            } => {
                live.phase = AgentPhase::Tool;
                live.current_tool = Some(tool_name.clone());
                live.trace
                    .record_call(call_id.as_str(), tool_name, arguments);
            }
            TurnEvent::ToolCallCompleted {
                call_id, result, ..
            } => {
                live.tool_calls = live.tool_calls.saturating_add(1);
                live.current_tool = None;
                live.phase = AgentPhase::Thinking;
                live.trace.record_result(call_id.as_str(), result);
            }
            TurnEvent::ChildSpawned { role, question, .. } => {
                live.phase = AgentPhase::Waiting;
                let status = match question {
                    Some(question) => format!("⇢ Kind {role} gestartet: {question}"),
                    None => format!("⇢ Kind {role} gestartet"),
                };
                live.trace.push_status(&status);
            }
            TurnEvent::ChildCompleted {
                outcome,
                duration_ms,
                ..
            } => {
                live.phase = AgentPhase::Thinking;
                let label = child_label.unwrap_or_default();
                #[allow(clippy::cast_precision_loss)]
                let secs = *duration_ms as f64 / 1000.0;
                live.trace
                    .push_status(&format!("⇠ Kind {label} beendet: {outcome} ({secs:.1}s)"));
            }
            TurnEvent::TurnCompleted { usage, .. } => {
                let turn_usage = usage.clone().unwrap_or_else(|| live.usage_turn.clone());
                live.usage_done.add(&turn_usage);
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Done;
                live.current_tool = None;
                live.finished = Some(Instant::now());
                live.trace.close_streams();
                live.trace.push_status("Turn abgeschlossen");
            }
            TurnEvent::TurnFailed { reason, .. } => {
                live.usage_done.add(&live.usage_turn.clone());
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Failed;
                live.finished = Some(Instant::now());
                live.trace.close_streams();
                live.trace
                    .push_status(&format!("Turn fehlgeschlagen: {reason}"));
            }
            TurnEvent::TurnAborted { .. } => {
                live.usage_done.add(&live.usage_turn.clone());
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Cancelled;
                live.finished = Some(Instant::now());
                live.trace.close_streams();
                live.trace.push_status("Turn abgebrochen");
            }
            _ => return false,
        }
        true
    }

    /// Setzt die abgeschlossene Nutzung eines Agenten (z. B. nach `/resume`).
    pub(crate) fn seed_usage(&mut self, agent: &str, role: &str, usage: TokenUsage) {
        if !self.agents.contains_key(agent) {
            self.order.push(agent.to_owned());
        }
        let live = self
            .agents
            .entry(agent.to_owned())
            .or_insert_with(|| AgentLive::new(agent.to_owned(), None, role.to_owned()));
        live.usage_done = usage;
        live.phase = AgentPhase::Done;
    }

    /// Summe über alle Agenten plus interne Aufrufe.
    #[must_use]
    pub(crate) fn totals(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for live in self.agents.values() {
            total.add(&live.usage());
        }
        for usage in self.internal.values() {
            total.add(usage);
        }
        total
    }

    /// Anzahl gerade aktiver Agenten.
    #[must_use]
    pub(crate) fn active_count(&self) -> usize {
        self.agents.values().filter(|a| a.phase.is_active()).count()
    }

    /// Kontextbelegung eines bestimmten Agenten.
    #[must_use]
    pub(crate) fn agent(&self, id: &str) -> Option<&AgentLive> {
        self.agents.get(id)
    }

    /// Spur eines Agenten für die Detailansicht.
    #[must_use]
    pub(crate) fn trace(&self, agent: &SessionId) -> Option<&AgentTrace> {
        self.agents.get(agent.as_str()).map(|live| &live.trace)
    }

    /// Agenten in Baumordnung (Eltern vor Kindern), mit Tiefe.
    #[must_use]
    pub(crate) fn rows(&self) -> Vec<(usize, &AgentLive)> {
        let mut out = Vec::with_capacity(self.order.len());
        let roots: Vec<&String> = self
            .order
            .iter()
            .filter(|id| {
                self.agents
                    .get(*id)
                    .and_then(|a| a.parent.as_ref())
                    .is_none_or(|p| !self.agents.contains_key(p))
            })
            .collect();
        for root in roots {
            self.push_subtree(root, 0, &mut out);
        }
        out
    }

    fn push_subtree<'a>(&'a self, id: &str, depth: usize, out: &mut Vec<(usize, &'a AgentLive)>) {
        let Some(live) = self.agents.get(id) else {
            return;
        };
        if out.iter().any(|(_, a)| a.id == live.id) || depth > 16 {
            return;
        }
        out.push((depth, live));
        for child in &self.order {
            if self.agents.get(child).and_then(|a| a.parent.as_deref()) == Some(id) {
                self.push_subtree(child, depth + 1, out);
            }
        }
    }

    /// Nutzung interner Aufrufe nach Zweck.
    pub(crate) fn internal_usage(&self) -> impl Iterator<Item = (&String, &TokenUsage)> {
        self.internal.iter()
    }

    pub(crate) fn select_next(&mut self) {
        let len = self.agents.len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    pub(crate) fn select_prev(&mut self) {
        let len = self.agents.len();
        if len > 0 {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    /// Live-Zustand des ausgewählten Agenten.
    #[must_use]
    pub(crate) fn selected_live(&self) -> Option<&AgentLive> {
        self.rows().get(self.selected).map(|(_, a)| *a)
    }

    /// Kennung des ausgewählten Agenten (für die Detailansicht).
    #[must_use]
    pub(crate) fn selected_agent(&self) -> Option<SessionId> {
        self.selected_live().map(|live| SessionId(live.id.clone()))
    }

    /// Zeichnet die Detailansicht eines Agenten.
    ///
    /// # Beschreibung
    /// Kopf (Rolle, Status, Aufgabe, Tokens, Kontext-Balken, laufendes
    /// Werkzeug), darunter die umbrochene Spur: Reasoning gedimmt/kursiv mit
    /// `∴`, Werkzeugaufrufe cyan, Ergebnisse grün bzw. rot, Text normal.
    ///
    /// # Argumente
    /// - `scroll`: Abstand in (umbrochenen) Zeilen vom Ende der Spur; `0`
    ///   folgt dem neuesten Eintrag. Zu große Werte werden am Anfang gekappt.
    /// - `show_reasoning`: `false` zeigt Reasoning nur als einzeiligen Hinweis.
    pub(crate) fn render_agent_detail(
        &self,
        agent: &SessionId,
        area: Rect,
        buf: &mut Buffer,
        scroll: u16,
        show_reasoning: bool,
    ) {
        let Some(live) = self.agents.get(agent.as_str()) else {
            Paragraph::new(Line::styled(
                "Agent nicht gefunden.",
                Style::default().add_modifier(Modifier::DIM),
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Agent · Esc zurück "),
            )
            .render(area, buf);
            return;
        };
        let reasoning_hint = if show_reasoning { "an" } else { "aus" };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(
                " Agent · {} · Esc zurück · r Reasoning {reasoning_hint} ",
                sanitize_inline(&live.role)
            ));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let header = Paragraph::new(detail_header_lines(live, usize::from(inner.width)))
            .wrap(Wrap { trim: false });
        let header_height = u16::try_from(header.line_count(inner.width))
            .unwrap_or(u16::MAX)
            .min(inner.height / 2)
            .max(1);
        header.render(
            Rect {
                height: header_height,
                ..inner
            },
            buf,
        );

        let rest = inner.height.saturating_sub(header_height);
        if rest == 0 {
            return;
        }
        let separator = format!("── Verlauf ({} Einträge) ", live.trace.len());
        let separator_width = usize::from(inner.width);
        let fill = separator_width.saturating_sub(separator.chars().count());
        Line::styled(
            format!("{separator}{}", "─".repeat(fill)),
            Style::default().add_modifier(Modifier::DIM),
        )
        .render(
            Rect {
                y: inner.y + header_height,
                height: 1,
                ..inner
            },
            buf,
        );
        let trace_area = Rect {
            y: inner.y + header_height + 1,
            height: rest.saturating_sub(1),
            ..inner
        };
        if trace_area.height == 0 {
            return;
        }
        let trace =
            Paragraph::new(trace_lines(&live.trace, show_reasoning)).wrap(Wrap { trim: false });
        let total = trace.line_count(trace_area.width);
        let max_top = total.saturating_sub(usize::from(trace_area.height));
        let top = max_top.saturating_sub(usize::from(scroll));
        trace
            .scroll((u16::try_from(top).unwrap_or(u16::MAX), 0))
            .render(trace_area, buf);
    }
}

/// Kopfzeilen der Detailansicht.
fn detail_header_lines(live: &AgentLive, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let phase_style = match live.phase {
        AgentPhase::Done => Style::default().fg(Color::Green),
        AgentPhase::Failed | AgentPhase::Cancelled => Style::default().fg(Color::Red),
        AgentPhase::Tool => Style::default().fg(Color::Cyan),
        AgentPhase::Waiting => Style::default().fg(Color::Yellow),
        AgentPhase::Admitted | AgentPhase::Thinking => Style::default().fg(Color::Blue),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            sanitize_inline(&live.role),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(live.phase.label(), phase_style),
        Span::styled(
            format!(
                " · {}s · {}",
                live.elapsed_secs(),
                sanitize_inline(&live.id)
            ),
            dim,
        ),
    ])];
    if let Some(task) = &live.task {
        lines.push(Line::from(vec![
            Span::styled("Aufgabe: ", dim),
            Span::raw(sanitize_inline(task)),
        ]));
    }
    let usage = live.usage();
    let mut stats = format!(
        "↑{} ↓{}",
        human_tokens(usage.prompt_tokens()),
        human_tokens(usage.output_tokens)
    );
    if let Some(cached) = usage.cached_tokens.filter(|c| *c > 0) {
        stats.push_str(&format!(" ⟳{}", human_tokens(cached)));
    }
    stats.push_str(&format!(" · {} Tools", live.tool_calls));
    if live.phase.is_active() && live.tokens_per_sec > 0.5 {
        stats.push_str(&format!(" · {:.0} tok/s", live.tokens_per_sec));
    }
    lines.push(Line::styled(stats, dim));
    if let Some(pct) = live.context_percent() {
        let bar_width = width.saturating_sub(24).clamp(4, 30);
        let ctx_style = if pct >= 85 {
            Style::default().fg(Color::Red)
        } else if pct >= 70 {
            Style::default().fg(Color::Yellow)
        } else {
            dim
        };
        let mut ctx = format!(
            "ctx {} {pct}% / {}",
            gauge(pct, bar_width),
            human_tokens(live.context_window)
        );
        if live.compactions > 0 {
            ctx.push_str(&format!(" · {}× verdichtet", live.compactions));
        }
        if live.history_dropped > 0 {
            ctx.push_str(&format!(" · {} ausgelassen", live.history_dropped));
        }
        lines.push(Line::styled(ctx, ctx_style));
    }
    if let Some(tool) = &live.current_tool {
        lines.push(Line::styled(
            format!("⚙ {}", sanitize_inline(tool)),
            Style::default().fg(Color::Cyan),
        ));
    }
    lines
}

/// Zeilen der Spur mit unterscheidbaren Stilen je Eintragsart.
fn trace_lines(trace: &AgentTrace, show_reasoning: bool) -> Vec<Line<'static>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let reasoning_style = dim.add_modifier(Modifier::ITALIC);
    let call_style = Style::default().fg(Color::Cyan);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if trace.is_empty() {
        lines.push(Line::styled("Noch keine Aktivität.", dim));
        return lines;
    }
    for entry in trace.entries() {
        match entry {
            TraceEntry::Text(text) => {
                let clean = sanitize_display(text);
                for line in clean.lines() {
                    lines.push(Line::raw(line.to_owned()));
                }
            }
            TraceEntry::Reasoning(text) => {
                let clean = sanitize_display(text);
                if show_reasoning {
                    for (index, line) in clean.lines().enumerate() {
                        let prefix = if index == 0 { "∴ " } else { "  " };
                        lines.push(Line::styled(format!("{prefix}{line}"), reasoning_style));
                    }
                } else {
                    let count = clean.lines().count();
                    lines.push(Line::styled(
                        format!("∴ Reasoning ({count} Zeilen, r zeigt)"),
                        reasoning_style,
                    ));
                }
            }
            TraceEntry::ToolCall { name, args_preview } => {
                let mut spans = vec![Span::styled(
                    format!("⚙ {name}"),
                    call_style.add_modifier(Modifier::BOLD),
                )];
                if !args_preview.is_empty() {
                    spans.push(Span::styled(format!(" {args_preview}"), call_style));
                }
                lines.push(Line::from(spans));
            }
            TraceEntry::ToolResult { name, ok, preview } => {
                let (mark, color) = if *ok {
                    ("✓", Color::Green)
                } else {
                    ("✗", Color::Red)
                };
                let result_style = Style::default().fg(color);
                let mut spans = vec![Span::styled(
                    format!("  {mark} {name}"),
                    result_style.add_modifier(Modifier::BOLD),
                )];
                if !preview.is_empty() {
                    spans.push(Span::styled(format!(" {preview}"), result_style));
                }
                lines.push(Line::from(spans));
            }
            TraceEntry::Status(text) => {
                lines.push(Line::styled(format!("· {text}"), dim));
            }
        }
    }
    lines
}

/// Kompakte Token-Darstellung: `1234` → `1.2k`.
#[must_use]
pub(crate) fn human_tokens(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.2}M", n as f64 / 1_000_000.0),
    }
}

/// Textuelle Balkenanzeige für eine Prozentzahl.
#[must_use]
pub(crate) fn gauge(pct: u8, width: usize) -> String {
    let filled = (usize::from(pct) * width).div_ceil(100).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Zeichnet das Agenten-Panel.
pub(crate) fn render_agents_panel(
    monitor: &AgentMonitor,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) {
    let active = monitor.active_count();
    let title = format!(" Agenten · {active} aktiv ");
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner_width = usize::from(area.width.saturating_sub(2));
    let mut lines: Vec<Line<'static>> = Vec::new();
    let rows = monitor.rows();
    if rows.is_empty() {
        lines.push(Line::styled(
            "Noch keine Agenten aktiv.",
            style::dim_style(theme),
        ));
    }
    for (index, (depth, live)) in rows.iter().enumerate() {
        let indent = "  ".repeat(*depth);
        let marker = if focused && index == monitor.selected {
            "▸ "
        } else {
            "  "
        };
        let phase_style = match live.phase {
            AgentPhase::Done => style::success_style(theme),
            AgentPhase::Failed | AgentPhase::Cancelled => style::error_style(theme),
            AgentPhase::Tool => style::tool_style(theme),
            AgentPhase::Waiting => style::warning_style(theme),
            AgentPhase::Admitted | AgentPhase::Thinking => {
                Style::default().fg(style::accent_color(theme))
            }
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{marker}{indent}")),
            Span::styled(
                sanitize_inline(&live.role),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(live.phase.label(), phase_style),
            Span::styled(
                format!(" {}s", live.elapsed_secs()),
                style::dim_style(theme),
            ),
        ]));
        let usage = live.usage();
        let mut stats = format!(
            "{indent}    ↑{} ↓{}",
            human_tokens(usage.prompt_tokens()),
            human_tokens(usage.output_tokens)
        );
        if let Some(cached) = usage.cached_tokens.filter(|c| *c > 0) {
            stats.push_str(&format!(" ⟳{}", human_tokens(cached)));
        }
        stats.push_str(&format!(" · {} Tools", live.tool_calls));
        if live.phase.is_active() && live.tokens_per_sec > 0.5 {
            stats.push_str(&format!(" · {:.0} tok/s", live.tokens_per_sec));
        }
        lines.push(Line::styled(stats, style::dim_style(theme)));
        if let Some(pct) = live.context_percent() {
            let bar_width = inner_width.saturating_sub(indent.len() + 18).clamp(4, 20);
            let ctx_style = if pct >= 85 {
                style::error_style(theme)
            } else if pct >= 70 {
                style::warning_style(theme)
            } else {
                style::dim_style(theme)
            };
            let mut ctx = format!(
                "{indent}    ctx {} {pct}% / {}",
                gauge(pct, bar_width),
                human_tokens(live.context_window)
            );
            if live.compactions > 0 {
                ctx.push_str(&format!(" · {}× verdichtet", live.compactions));
            }
            lines.push(Line::styled(ctx, ctx_style));
        }
        if let Some(tool) = &live.current_tool {
            lines.push(Line::styled(
                format!("{indent}    ⚙ {}", sanitize_inline(tool)),
                style::tool_style(theme),
            ));
        } else if let Some(task) = &live.task
            && live.phase.is_active()
        {
            let task: String = sanitize_inline(task)
                .chars()
                .take(inner_width.max(8))
                .collect();
            lines.push(Line::styled(
                format!("{indent}    „{task}“"),
                style::dim_style(theme),
            ));
        }
    }
    let internal: Vec<_> = monitor.internal_usage().collect();
    if !internal.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("intern", style::dim_style(theme)));
        for (purpose, usage) in internal {
            lines.push(Line::styled(
                format!(
                    "  {purpose}: ↑{} ↓{}",
                    human_tokens(usage.prompt_tokens()),
                    human_tokens(usage.output_tokens)
                ),
                style::dim_style(theme),
            ));
        }
    }
    if focused
        && let Some(live) = monitor.selected_live()
        && (!live.preview.is_empty() || !live.reasoning_preview.is_empty())
    {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("── {} live · Enter Details ──", sanitize_inline(&live.role)),
            style::dim_style(theme),
        ));
        if live.preview.is_empty() {
            lines.push(Line::styled(
                format!("∴ {}", sanitize_inline(&live.reasoning_preview)),
                style::dim_style(theme).add_modifier(Modifier::ITALIC),
            ));
        } else {
            lines.push(Line::raw(sanitize_inline(&live.preview)));
        }
    }
    Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_types::{SessionId, TurnId};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn ev(
        agent: &str,
        parent: Option<&str>,
        role: &str,
        turn: TurnEvent,
    ) -> TestResult<AgentEvent> {
        Ok(AgentEvent {
            agent: SessionId::try_from_str(agent)?,
            parent: parent.map(SessionId::try_from_str).transpose()?,
            role: role.into(),
            kind: AgentEventKind::Turn(turn),
        })
    }

    fn usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            ..TokenUsage::default()
        }
    }

    #[test]
    fn tracks_live_usage_per_agent_and_totals() -> TestResult {
        let turn = TurnId::try_from_str("t1")?;
        let mut monitor = AgentMonitor::default();
        monitor.apply(&ev(
            "root",
            None,
            "assistant",
            TurnEvent::UsageUpdated {
                turn_id: turn.clone(),
                round: usage(100, 10),
                turn_total: usage(100, 10),
                final_round: false,
            },
        )?);
        monitor.apply(&ev(
            "child",
            Some("root"),
            "explorer",
            TurnEvent::UsageUpdated {
                turn_id: turn.clone(),
                round: usage(50, 5),
                turn_total: usage(50, 5),
                final_round: true,
            },
        )?);
        assert_eq!(monitor.totals().input_tokens, 150);
        monitor.apply(&ev(
            "child",
            Some("root"),
            "explorer",
            TurnEvent::TurnCompleted {
                turn_id: turn,
                usage: Some(usage(60, 6)),
            },
        )?);
        assert_eq!(monitor.totals().input_tokens, 160);
        let rows = monitor.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1.id, "root");
        assert_eq!(rows[1].0, 1, "child is nested under root");
        assert_eq!(
            monitor.agent("child").map(|a| a.phase),
            Some(AgentPhase::Done)
        );
        Ok(())
    }

    #[test]
    fn context_and_tool_state() -> TestResult {
        let turn = TurnId::try_from_str("t1")?;
        let mut monitor = AgentMonitor::default();
        monitor.apply(&ev(
            "a",
            None,
            "assistant",
            TurnEvent::ContextUpdated {
                turn_id: turn.clone(),
                used_tokens: 150_000,
                window_tokens: 200_000,
                history_items_dropped: 2,
            },
        )?);
        monitor.apply(&ev(
            "a",
            None,
            "assistant",
            TurnEvent::ToolCallRequested {
                turn_id: turn,
                call_id: harw_types::ToolCallId::new(),
                tool_name: "fs.read".into(),
                arguments: serde_json::Value::Null,
            },
        )?);
        let live = monitor.agent("a").ok_or("agent")?;
        assert_eq!(live.context_percent(), Some(75));
        assert_eq!(live.current_tool.as_deref(), Some("fs.read"));
        assert_eq!(live.phase, AgentPhase::Tool);
        Ok(())
    }

    #[test]
    fn internal_usage_counts_toward_totals() -> TestResult {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&AgentEvent {
            agent: SessionId::try_from_str("x")?,
            parent: None,
            role: "internal".into(),
            kind: AgentEventKind::InternalUsage {
                purpose: "title".into(),
                usage: usage(7, 3),
            },
        });
        assert_eq!(monitor.totals().output_tokens, 3);
        assert!(monitor.rows().is_empty());
        Ok(())
    }

    #[test]
    fn helpers_format() {
        assert_eq!(human_tokens(999), "999");
        assert_eq!(human_tokens(1_500), "1.5k");
        assert_eq!(gauge(50, 4), "██░░");
        assert_eq!(gauge(0, 3), "░░░");
    }

    fn t1() -> TestResult<TurnId> {
        Ok(TurnId::try_from_str("t1")?)
    }

    fn apply_all(monitor: &mut AgentMonitor, events: Vec<TurnEvent>) -> TestResult {
        for turn in events {
            monitor.apply(&ev("a", None, "explorer", turn)?);
        }
        Ok(())
    }

    fn trace_of(monitor: &AgentMonitor) -> TestResult<Vec<TraceEntry>> {
        let id = SessionId::try_from_str("a")?;
        Ok(monitor
            .trace(&id)
            .ok_or("trace")?
            .entries()
            .cloned()
            .collect())
    }

    #[test]
    fn deltas_merge_per_kind_and_final_items_replace_streams() -> TestResult {
        let turn = t1()?;
        let mut monitor = AgentMonitor::default();
        apply_all(
            &mut monitor,
            vec![
                TurnEvent::ReasoningDelta {
                    turn_id: turn.clone(),
                    text: "Ich ".into(),
                },
                TurnEvent::ReasoningDelta {
                    turn_id: turn.clone(),
                    text: "überlege".into(),
                },
                TurnEvent::AssistantDelta {
                    turn_id: turn.clone(),
                    text: "Hal".into(),
                },
                TurnEvent::AssistantDelta {
                    turn_id: turn.clone(),
                    text: "lo".into(),
                },
            ],
        )?;
        assert_eq!(
            trace_of(&monitor)?,
            vec![
                TraceEntry::Reasoning("Ich überlege".into()),
                TraceEntry::Text("Hallo".into()),
            ]
        );
        let live = monitor.agent("a").ok_or("agent")?;
        assert_eq!(live.preview, "Hallo", "Reasoning bleibt aus der Vorschau");
        assert_eq!(live.reasoning_preview, "Ich überlege");

        // Finale Items ersetzen die gestreamten Einträge statt sie zu doppeln.
        let changed = monitor.apply(&ev(
            "a",
            None,
            "explorer",
            TurnEvent::ItemAdded {
                turn_id: turn.clone(),
                item: TurnItem::Reasoning(harw_protocol::ReasoningItem {
                    id: harw_types::ItemId::new(),
                    summary_text: vec!["Ich überlege gründlich".into()],
                    raw_content: Vec::new(),
                }),
            },
        )?);
        assert!(changed, "ItemAdded wird nicht mehr verworfen");
        apply_all(
            &mut monitor,
            vec![TurnEvent::ItemAdded {
                turn_id: turn.clone(),
                item: TurnItem::AssistantMessage(harw_protocol::AssistantMessageItem {
                    id: harw_types::ItemId::new(),
                    content: vec![ContentPart::Text {
                        text: "Hallo Welt".into(),
                    }],
                    phase: None,
                }),
            }],
        )?;
        assert_eq!(
            trace_of(&monitor)?,
            vec![
                TraceEntry::Reasoning("Ich überlege gründlich".into()),
                TraceEntry::Text("Hallo Welt".into()),
            ]
        );

        // Nach dem Abschluss beginnt ein neues Delta einen neuen Eintrag.
        apply_all(
            &mut monitor,
            vec![TurnEvent::AssistantDelta {
                turn_id: turn,
                text: "Neu".into(),
            }],
        )?;
        let entries = trace_of(&monitor)?;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2], TraceEntry::Text("Neu".into()));
        Ok(())
    }

    #[test]
    fn tool_calls_and_results_are_deduplicated_by_call_id() -> TestResult {
        let turn = t1()?;
        let call_id = harw_types::ToolCallId::new();
        let mut monitor = AgentMonitor::default();
        apply_all(
            &mut monitor,
            vec![
                TurnEvent::ToolCallRequested {
                    turn_id: turn.clone(),
                    call_id: call_id.clone(),
                    tool_name: "fs.read".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
                TurnEvent::ItemAdded {
                    turn_id: turn.clone(),
                    item: TurnItem::ToolCall(harw_protocol::ToolCallItem {
                        id: harw_types::ItemId::new(),
                        call_id: call_id.clone(),
                        tool_name: "fs.read".into(),
                        arguments: serde_json::json!({"path": "src/lib.rs"}),
                    }),
                },
                TurnEvent::ToolCallCompleted {
                    turn_id: turn.clone(),
                    call_id: call_id.clone(),
                    result: ToolCallResult::error("nicht gefunden"),
                    duration_ms: 3,
                },
                TurnEvent::ItemAdded {
                    turn_id: turn,
                    item: TurnItem::ToolResult(harw_protocol::ToolResultItem {
                        id: harw_types::ItemId::new(),
                        call_id,
                        result: ToolCallResult::error("nicht gefunden"),
                        duration_ms: 3,
                        trust: harw_protocol::ResultTrust::default(),
                    }),
                },
            ],
        )?;
        assert_eq!(
            trace_of(&monitor)?,
            vec![
                TraceEntry::ToolCall {
                    name: "fs.read".into(),
                    args_preview: r#"{"path":"src/lib.rs"}"#.into(),
                },
                TraceEntry::ToolResult {
                    name: "fs.read".into(),
                    ok: false,
                    preview: "nicht gefunden".into(),
                },
            ]
        );
        assert_eq!(monitor.agent("a").ok_or("agent")?.tool_calls, 1);
        Ok(())
    }

    #[test]
    fn trace_respects_entry_and_byte_caps() -> TestResult {
        let mut trace = AgentTrace::default();
        for index in 0..(MAX_TRACE_ENTRIES + 50) {
            trace.push_status(&format!("Meldung {index}"));
        }
        assert_eq!(trace.len(), MAX_TRACE_ENTRIES);
        assert_eq!(
            trace.entries().next(),
            Some(&TraceEntry::Status("Meldung 50".into())),
            "älteste Einträge fallen zuerst heraus"
        );

        // Ein einzelner, riesiger Stream wird vorne gekürzt, bleibt aber erhalten.
        let chunk = "äbcdefghij".repeat(100);
        for _ in 0..100 {
            trace.push_delta(StreamKind::Text, &chunk);
        }
        assert!(trace.byte_len() <= MAX_TRACE_BYTES);
        let actual: usize = trace.entries().map(TraceEntry::byte_len).sum();
        assert_eq!(trace.byte_len(), actual, "Byte-Buchhaltung stimmt");
        let Some(TraceEntry::Text(text)) = trace.entries().last() else {
            return Err("letzter Eintrag ist Text".into());
        };
        assert!(text.ends_with("äbcdefghij"));
        assert_eq!(trace.len(), 1);

        // Wiederholte identische Statusmeldungen werden zusammengefasst.
        // (Der volle Text-Eintrag fällt dabei wegen der Byte-Grenze heraus.)
        trace.push_status("x");
        trace.push_status("x");
        assert_eq!(trace.len(), 1);
        assert_eq!(
            trace.entries().last(),
            Some(&TraceEntry::Status("x".into()))
        );
        Ok(())
    }

    #[test]
    fn child_progress_updates_child_and_orchestration_adds_status() -> TestResult {
        let turn = t1()?;
        let mut monitor = AgentMonitor::default();
        monitor.apply(&ev(
            "child",
            Some("root"),
            "explorer",
            TurnEvent::TurnStarted {
                turn_id: turn.clone(),
                thread_id: harw_types::ThreadId::new(),
            },
        )?);
        let changed = monitor.apply(&ev(
            "root",
            None,
            "assistant",
            TurnEvent::ChildProgress {
                turn_id: turn,
                child: SessionId::try_from_str("child")?,
                tool_calls: 4,
                tokens: 900,
            },
        )?);
        assert!(changed);
        assert_eq!(monitor.agent("child").ok_or("child")?.tool_calls, 4);
        assert!(
            monitor.agent("root").is_none(),
            "ChildProgress legt keinen Absender-Eintrag an"
        );
        let child = SessionId::try_from_str("child")?;
        let trace = monitor.trace(&child).ok_or("trace")?;
        assert!(trace.is_empty());
        Ok(())
    }

    #[test]
    fn selected_agent_returns_session_id() -> TestResult {
        let mut monitor = AgentMonitor::default();
        assert!(monitor.selected_agent().is_none());
        apply_all(
            &mut monitor,
            vec![TurnEvent::TurnAborted { turn_id: t1()? }],
        )?;
        assert_eq!(
            monitor.selected_agent(),
            Some(SessionId::try_from_str("a")?)
        );
        Ok(())
    }

    fn render_to_string(
        monitor: &AgentMonitor,
        width: u16,
        height: u16,
        scroll: u16,
        show_reasoning: bool,
    ) -> TestResult<String> {
        let id = SessionId::try_from_str("a")?;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| {
            let area = frame.area();
            monitor.render_agent_detail(&id, area, frame.buffer_mut(), scroll, show_reasoning);
        })?;
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        Ok(out)
    }

    #[test]
    fn detail_view_renders_header_and_styled_trace() -> TestResult {
        let turn = t1()?;
        let mut monitor = AgentMonitor::default();
        monitor.apply(&AgentEvent {
            agent: SessionId::try_from_str("a")?,
            parent: None,
            role: "explorer".into(),
            kind: AgentEventKind::Orchestration(harw_protocol::AgentOrchestrationEvent {
                schema_version: harw_protocol::AgentOrchestrationEvent::CURRENT_SCHEMA_VERSION,
                event_id: "e1".into(),
                root_session_id: SessionId::try_from_str("root")?,
                parent_session_id: SessionId::try_from_str("root")?,
                child_session_id: SessionId::try_from_str("a")?,
                turn_id: None,
                role: "explorer".into(),
                depth: 1,
                task: Some("Finde die Config".into()),
                status: AgentOrchestrationStatus::Running,
                usage: None,
                duration_ms: None,
                progress: None,
                detail: None,
                tool_calls: None,
            }),
        });
        let call_id = harw_types::ToolCallId::new();
        apply_all(
            &mut monitor,
            vec![
                TurnEvent::ContextUpdated {
                    turn_id: turn.clone(),
                    used_tokens: 50_000,
                    window_tokens: 200_000,
                    history_items_dropped: 0,
                },
                TurnEvent::ReasoningDelta {
                    turn_id: turn.clone(),
                    text: "geheimer Gedanke".into(),
                },
                TurnEvent::ToolCallRequested {
                    turn_id: turn.clone(),
                    call_id: call_id.clone(),
                    tool_name: "fs.grep".into(),
                    arguments: serde_json::json!({"q": "cfg"}),
                },
                TurnEvent::ToolCallCompleted {
                    turn_id: turn.clone(),
                    call_id,
                    result: ToolCallResult::success(serde_json::json!("3 Treffer")),
                    duration_ms: 1,
                },
                TurnEvent::AssistantDelta {
                    turn_id: turn,
                    text: "Gefunden in harw.toml".into(),
                },
            ],
        )?;

        let shown = render_to_string(&monitor, 70, 20, 0, true)?;
        assert!(shown.contains("explorer"), "{shown}");
        assert!(shown.contains("Aufgabe: Finde die Config"), "{shown}");
        assert!(shown.contains("ctx"), "{shown}");
        assert!(shown.contains("25%"), "{shown}");
        assert!(shown.contains("∴ geheimer Gedanke"), "{shown}");
        assert!(shown.contains("⚙ fs.grep"), "{shown}");
        assert!(shown.contains("✓ fs.grep 3 Treffer"), "{shown}");
        assert!(shown.contains("Gefunden in harw.toml"), "{shown}");

        let hidden = render_to_string(&monitor, 70, 20, 0, false)?;
        assert!(!hidden.contains("geheimer Gedanke"), "{hidden}");
        assert!(hidden.contains("∴ Reasoning (1 Zeilen"), "{hidden}");

        // Stile: Werkzeugaufruf cyan, Ergebnis grün.
        let id = SessionId::try_from_str("a")?;
        let area = Rect::new(0, 0, 70, 20);
        let mut buf = Buffer::empty(area);
        monitor.render_agent_detail(&id, area, &mut buf, 0, true);
        let find = |needle: char| -> Option<Style> {
            (0..area.height).find_map(|y| {
                (0..area.width).find_map(|x| {
                    let cell = &buf[(x, y)];
                    (cell.symbol().starts_with(needle)).then(|| cell.style())
                })
            })
        };
        assert_eq!(find('⚙').and_then(|s| s.fg), Some(Color::Cyan));
        assert_eq!(find('✓').and_then(|s| s.fg), Some(Color::Green));
        let reasoning = find('∴').ok_or("reasoning")?;
        assert!(reasoning.add_modifier.contains(Modifier::ITALIC));
        Ok(())
    }

    #[test]
    fn detail_view_scrolls_from_the_bottom() -> TestResult {
        let mut monitor = AgentMonitor::default();
        apply_all(
            &mut monitor,
            vec![TurnEvent::TurnAborted { turn_id: t1()? }],
        )?;
        let live = monitor.agents.get_mut("a").ok_or("agent")?;
        for index in 0..40 {
            live.trace.push_status(&format!("Eintrag-{index:02}"));
        }
        let tail = render_to_string(&monitor, 50, 16, 0, true)?;
        assert!(tail.contains("Eintrag-39"), "{tail}");
        assert!(!tail.contains("Eintrag-00"), "{tail}");
        let top = render_to_string(&monitor, 50, 16, u16::MAX, true)?;
        assert!(top.contains("Eintrag-00"), "{top}");
        assert!(!top.contains("Eintrag-39"), "{top}");
        Ok(())
    }

    #[test]
    fn detail_view_for_unknown_agent_does_not_panic() -> TestResult {
        let monitor = AgentMonitor::default();
        let shown = render_to_string(&monitor, 40, 5, 0, true)?;
        assert!(shown.contains("Agent nicht gefunden"), "{shown}");
        Ok(())
    }
}

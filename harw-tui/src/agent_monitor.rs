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

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use harw_core::{AgentEvent, AgentEventKind};
use harw_extension_api::{ToolCall, ToolName};
use harw_protocol::{
    AgentOrchestrationStatus, ContentPart, ToolCallResult, ToolPlacement, TurnEvent, TurnItem,
};
use harw_types::{SessionId, TokenUsage};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        StatefulWidget, Widget, Wrap,
    },
};

use crate::chat_scroll::ChatScroll;
use crate::child_stream::redact_result;
use crate::export::redact_json_value;
use crate::history_cell::{ToolCell, ToolState, tool_label};
use crate::jobs_panel::JobRow;
use crate::sanitize::{sanitize_display, sanitize_inline};
use crate::status_line::{display_width, fit_width, model_segment};
use crate::style::{self, Theme};

/// Maximale Länge der Live-Text-Vorschau je Agent (Zeichen).
const PREVIEW_CHARS: usize = 240;
/// Maximale Anzahl Einträge einer [`AgentTrace`].
pub(crate) const MAX_TRACE_ENTRIES: usize = 400;
/// Maximale Textmenge (Bytes) einer [`AgentTrace`].
pub(crate) const MAX_TRACE_BYTES: usize = 64 * 1024;
/// Maximale Länge des Werkzeug-Labels in der Spur (Zeichen).
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
    /// Angeforderter Werkzeugaufruf. `name` ist das Klartext-Label aus
    /// [`crate::history_cell::tool_label`] (z. B. `Shell(ls)`), nie rohes
    /// JSON; `args_preview` bleibt leer (R18 D-D: kein roher
    /// Argument-Dump in der Detailansicht).
    ToolCall { name: String, args_preview: String },
    /// Ergebnis eines Werkzeugaufrufs. `name` ist das Label des Aufrufs,
    /// `preview` der Ausgang aus [`ToolCell::compact_outcome`] (Dauer, Ort,
    /// Zusammenfassung).
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
    /// Klartext-Label des Aufrufs ([`crate::history_cell::tool_label`]).
    label: String,
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
    fn remember(
        &mut self,
        call_id: &str,
        name: &str,
        label: String,
        call_logged: bool,
        result_logged: bool,
    ) {
        self.calls.push_back(CallMark {
            id: call_id.to_owned(),
            name: name.to_owned(),
            label,
            call_logged,
            result_logged,
        });
        while self.calls.len() > MAX_CALL_MARKS {
            self.calls.pop_front();
        }
    }

    /// Protokolliert einen Werkzeugaufruf (höchstens einmal je `call_id`).
    ///
    /// Das Label entsteht wie im Hauptverlauf über
    /// [`crate::history_cell::tool_label`] (aus redigierten Argumenten), statt
    /// rohes JSON zu zeigen.
    fn record_call(
        &mut self,
        call_id: &harw_types::ToolCallId,
        name: &str,
        arguments: &serde_json::Value,
    ) {
        let already = self
            .mark(call_id.as_str())
            .map(|mark| std::mem::replace(&mut mark.call_logged, true));
        if already == Some(true) {
            return;
        }
        let call = ToolCall {
            id: call_id.clone(),
            name: ToolName::new(name),
            arguments: redact_json_value(arguments),
        };
        let label = clip_chars(&sanitize_inline(&tool_label(&call)), ARGS_PREVIEW_CHARS);
        match self.mark(call_id.as_str()) {
            Some(mark) => mark.label = label.clone(),
            None => self.remember(call_id.as_str(), name, label.clone(), true, false),
        }
        self.push(TraceEntry::ToolCall {
            name: label,
            args_preview: String::new(),
        });
    }

    /// Protokolliert ein Werkzeugergebnis (höchstens einmal je `call_id`).
    ///
    /// Der Ausgang wird wie im Hauptverlauf über eine [`ToolCell`] bestimmt
    /// (werkzeugspezifische Zusammenfassung, `exit N` als Fehlschlag), mit
    /// Dauer und Ort ([`ToolCell::compact_outcome`]).
    fn record_result(
        &mut self,
        call_id: &str,
        result: &ToolCallResult,
        duration_ms: u64,
        placement: Option<&ToolPlacement>,
    ) {
        let known = self.mark(call_id).map(|mark| {
            (
                std::mem::replace(&mut mark.result_logged, true),
                mark.name.clone(),
                mark.label.clone(),
            )
        });
        let (name, label) = match known {
            Some((true, _, _)) => return,
            Some((false, name, label)) => (name, label),
            None => {
                self.remember(call_id, "?", String::new(), true, true);
                ("?".to_owned(), String::new())
            }
        };
        let label = if label.is_empty() {
            sanitize_inline(&name)
        } else {
            label
        };
        let mut cell = ToolCell::labelled(&name, label.clone());
        cell.complete_at(&redact_result(result), duration_ms, placement);
        let ok = cell.state != ToolState::Failed;
        let preview = clip_chars(
            &sanitize_inline(&cell.compact_outcome()),
            RESULT_PREVIEW_CHARS,
        );
        self.push(TraceEntry::ToolResult {
            name: label,
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

/// Verbindet die Textteile eines Nachrichteninhalts.
fn content_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
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
    /// Vom Kind angesprochenes Modell (aus `AgentOrchestrationEvent::model`).
    pub model: Option<String>,
    /// Vom Kind angesprochener Provider (aus
    /// `AgentOrchestrationEvent::provider`); Anzeige `<provider>/<modell>`.
    pub provider: Option<String>,
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
    /// Kurzer Fehlergrund (aus `TurnFailed` bzw. dem `Failed`-Detail).
    pub failure_reason: Option<String>,
    /// Ein Fehlschlag wurde gesehen (Detailansicht geöffnet oder mit `c`
    /// quittiert); danach fällt er in die Sammelzeile der fertigen Agenten.
    pub failure_seen: bool,
}

impl AgentLive {
    fn new(id: String, parent: Option<String>, role: String) -> Self {
        let now = Instant::now();
        Self {
            id,
            parent,
            role,
            model: None,
            provider: None,
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
            failure_reason: None,
            failure_seen: false,
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

    /// Fehlgeschlagen und noch nicht gesehen (bleibt einzeln im Panel).
    pub(crate) fn is_unseen_failure(&self) -> bool {
        self.phase == AgentPhase::Failed && !self.failure_seen
    }

    /// Beendet (fertig, abgebrochen oder gesehener Fehlschlag) — gehört in
    /// die Sammelzeile.
    pub(crate) fn is_finished_quietly(&self) -> bool {
        !self.phase.is_active() && !self.is_unseen_failure()
    }

    /// Beginnt einen neuen Lauf desselben Agenten (dieselbe Kind-ID läuft
    /// erneut, bzw. die Wurzel startet ihren nächsten Turn): Dauer und
    /// Fehlerzustand gelten ab jetzt, die Token-Summen bleiben.
    fn begin_run(&mut self) {
        let now = Instant::now();
        self.started = now;
        self.finished = None;
        self.failure_reason = None;
        self.failure_seen = false;
        self.rate_mark = (now, self.usage().output_tokens);
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

/// Ab so vielen fertigen Agenten fasst das Panel sie zu einer Sammelzeile
/// zusammen (ein einzelner fertiger Agent braucht ohnehin nur eine Zeile).
pub(crate) const FINISHED_COLLAPSE_MIN: usize = 2;

/// Eine auswählbare Zeile des Agenten-Panels.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PanelEntry<'a> {
    /// Ein einzelner Agent-Lauf (Schlüssel: Kind-ID).
    Agent {
        /// Tiefe im Agentenbaum.
        depth: usize,
        /// Live-Zustand.
        live: &'a AgentLive,
    },
    /// Sammelzeile der fertigen Agenten (Enter/`f` klappt auf/zu).
    Finished,
    /// Plan R9, Teil F: ein Hintergrund-Job (Jobs-Gruppe unter den Agenten).
    Job {
        /// Schnappschuss des Jobs.
        row: &'a JobRow,
    },
    /// Plan R9, Teil F: Sammelzeile beendeter Jobs (Enter/`f` klappt auf/zu).
    JobsFinished,
}

/// Zustand aller beobachteten Agenten plus interne Nutzung.
#[derive(Debug, Default)]
pub(crate) struct AgentMonitor {
    agents: BTreeMap<String, AgentLive>,
    /// Einfügereihenfolge für stabile Darstellung.
    order: Vec<String>,
    /// Nutzung interner Aufrufe (Kompaktierung, Titel …) nach Zweck.
    internal: BTreeMap<String, TokenUsage>,
    /// Ausgewählte Panelzeile (Index in [`Self::panel_entries`]).
    pub selected: usize,
    /// Fertige Agenten einzeln statt als Sammelzeile zeigen (Taste `f`).
    pub finished_expanded: bool,
    /// Erste sichtbare Zeile der Panelliste (Mausrad, Bild↑↓, Pos1/Ende).
    panel_scroll: Cell<usize>,
    /// Beim nächsten Zeichnen die Auswahl in den sichtbaren Bereich holen.
    follow_selection: Cell<bool>,
    /// Innenhöhe des Panels beim letzten Zeichnen (Seitengröße für Bild↑↓).
    panel_height: Cell<usize>,
    /// Plan R9, Teil F: Schnappschuss der Hintergrund-Jobs der Sitzung
    /// (Jobs-Gruppe), älteste zuerst; nachgeführt über [`Self::set_jobs`].
    jobs: Vec<JobRow>,
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
                if let Some(model) = &orch.model
                    && live.model.as_deref() != Some(model.as_str())
                {
                    live.model = Some(model.clone());
                }
                if let Some(provider) = &orch.provider
                    && live.provider.as_deref() != Some(provider.as_str())
                {
                    live.provider = Some(provider.clone());
                }
                let before = live.phase;
                // Ein neuer Lauf derselben Kind-ID (`Running` nach einem
                // Endzustand) zählt als eigener Lauf: Dauer und Fehler gelten
                // neu, statt den alten Endzustand stehen zu lassen. `Progress`
                // reaktiviert bewusst nicht (ein verspätetes Fortschritts-
                // Ereignis darf einen fertigen Agenten nicht „laufen“ lassen).
                if orch.status == AgentOrchestrationStatus::Running && !before.is_active() {
                    live.begin_run();
                    live.trace.push_status("Neuer Lauf");
                }
                live.phase = match orch.status {
                    AgentOrchestrationStatus::Admitted => AgentPhase::Admitted,
                    AgentOrchestrationStatus::Running => match live.phase {
                        AgentPhase::Thinking | AgentPhase::Tool => live.phase,
                        _ => AgentPhase::Thinking,
                    },
                    AgentOrchestrationStatus::Progress => {
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
                if live.phase == AgentPhase::Failed && before != AgentPhase::Failed {
                    live.failure_seen = false;
                    if let Some(detail) = &orch.detail {
                        live.failure_reason = Some(detail.clone());
                    }
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
            // Matrix-Spielereignisse gehören der Matrix-Ansicht (`app.rs`
            // leitet sie dorthin weiter); der Monitor ignoriert sie.
            AgentEventKind::Matrix { .. } => false,
            // Wissensänderungen markieren Panels als veraltet (`app.rs`);
            // der Monitor ignoriert sie.
            AgentEventKind::Knowledge { .. } => false,
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
                // Nach einem Endzustand beginnt ein neuer Lauf (eigene Dauer).
                if live.finished.is_some() || !live.phase.is_active() {
                    live.begin_run();
                }
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
                        .record_call(&call.call_id, &call.tool_name, &call.arguments);
                }
                TurnItem::ToolResult(result) => {
                    live.trace.record_result(
                        result.call_id.as_str(),
                        &result.result,
                        result.duration_ms,
                        None,
                    );
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
                live.trace.record_call(call_id, tool_name, arguments);
            }
            TurnEvent::ToolCallCompleted {
                call_id,
                result,
                duration_ms,
                placement,
                ..
            } => {
                live.tool_calls = live.tool_calls.saturating_add(1);
                live.current_tool = None;
                live.phase = AgentPhase::Thinking;
                // R18: Dauer und Ort bleiben in der Spur erhalten.
                live.trace.record_result(
                    call_id.as_str(),
                    result,
                    *duration_ms,
                    placement.as_ref(),
                );
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
                live.failure_reason = Some(reason.clone());
                live.failure_seen = false;
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

    /// Zeilen des Panels in Anzeigereihenfolge (Auswahl-Einheiten).
    ///
    /// # Beschreibung
    /// 1. laufende und wartende Agenten (Baumordnung);
    /// 2. fehlgeschlagene, noch nicht gesehene Agenten (einzeln, rot);
    /// 3. fertige Agenten (inkl. abgebrochener und gesehener Fehlschläge):
    ///    ab [`FINISHED_COLLAPSE_MIN`] als eine Sammelzeile
    ///    ([`PanelEntry::Finished`]), ausgeklappt (`f`) danach einzeln.
    ///
    /// Jeder Lauf ist eine eigene Zeile, geschlüsselt nach Kind-ID — ein
    /// fertiger früherer Lauf derselben Rolle verdeckt nie einen laufenden.
    #[must_use]
    pub(crate) fn panel_entries(&self) -> Vec<PanelEntry<'_>> {
        let rows = self.rows();
        let mut entries: Vec<PanelEntry<'_>> = rows
            .iter()
            .filter(|(_, live)| live.phase.is_active())
            .map(|&(depth, live)| PanelEntry::Agent { depth, live })
            .collect();
        entries.extend(
            rows.iter()
                .filter(|(_, live)| live.is_unseen_failure())
                .map(|&(depth, live)| PanelEntry::Agent { depth, live }),
        );
        let finished: Vec<PanelEntry<'_>> = rows
            .iter()
            .filter(|(_, live)| live.is_finished_quietly())
            .map(|&(depth, live)| PanelEntry::Agent { depth, live })
            .collect();
        if finished.len() >= FINISHED_COLLAPSE_MIN {
            entries.push(PanelEntry::Finished);
            if self.finished_expanded {
                entries.extend(finished);
            }
        } else {
            entries.extend(finished);
        }
        // Plan R9, Teil F: die Jobs-Gruppe — laufende Jobs einzeln, beendete
        // ab `FINISHED_COLLAPSE_MIN` als Sammelzeile (dieselbe Taste `f`).
        entries.extend(
            self.jobs
                .iter()
                .filter(|row| row.active)
                .map(|row| PanelEntry::Job { row }),
        );
        let finished_jobs: Vec<PanelEntry<'_>> = self
            .jobs
            .iter()
            .filter(|row| !row.active)
            .map(|row| PanelEntry::Job { row })
            .collect();
        if finished_jobs.len() >= FINISHED_COLLAPSE_MIN {
            entries.push(PanelEntry::JobsFinished);
            if self.finished_expanded {
                entries.extend(finished_jobs);
            }
        } else {
            entries.extend(finished_jobs);
        }
        entries
    }

    /// Plan R9, Teil F: übernimmt den aktuellen Job-Schnappschuss.
    ///
    /// # Rückgabe
    /// `true`, wenn sich Sichtbares änderte.
    pub(crate) fn set_jobs(&mut self, jobs: Vec<JobRow>) -> bool {
        if self.jobs == jobs {
            return false;
        }
        self.jobs = jobs;
        let len = self.panel_entries().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        true
    }

    /// Plan R9, Teil F: der aktuelle Job-Schnappschuss.
    #[must_use]
    pub(crate) fn jobs(&self) -> &[JobRow] {
        &self.jobs
    }

    /// Plan R9, Teil F: Kennung des ausgewählten Jobs (Detailansicht).
    #[must_use]
    pub(crate) fn selected_job(&self) -> Option<String> {
        match self.panel_entries().get(self.selected) {
            Some(PanelEntry::Job { row }) => Some(row.id.clone()),
            _ => None,
        }
    }

    /// Bewegt die Auswahl um eine Zeile (umlaufend).
    fn move_selection(&mut self, forward: bool) {
        let len = self.panel_entries().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected.min(len - 1);
        self.selected = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.follow_selection.set(true);
    }

    pub(crate) fn select_next(&mut self) {
        self.move_selection(true);
    }

    pub(crate) fn select_prev(&mut self) {
        self.move_selection(false);
    }

    /// Bild↑/Bild↓: Auswahl um eine Panelseite, an den Rändern begrenzt.
    pub(crate) fn select_page(&mut self, down: bool) {
        let len = self.panel_entries().len();
        if len == 0 {
            return;
        }
        let page = self.panel_height.get().max(2) - 1;
        self.selected = if down {
            (self.selected + page).min(len - 1)
        } else {
            self.selected.min(len - 1).saturating_sub(page)
        };
        self.follow_selection.set(true);
    }

    /// Pos1/Ende: erste bzw. letzte Panelzeile wählen.
    pub(crate) fn select_edge(&mut self, last: bool) {
        let len = self.panel_entries().len();
        self.selected = if last { len.saturating_sub(1) } else { 0 };
        self.follow_selection.set(true);
    }

    /// Scrollt die Panelliste ohne die Auswahl zu ändern (Mausrad,
    /// `scroll_panel_up`/`scroll_panel_down`); negativ = nach oben.
    pub(crate) fn scroll_panel(&self, delta: isize) {
        let current = self.panel_scroll.get();
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.unsigned_abs())
        };
        // Obergrenze kappt das nächste Zeichnen.
        self.panel_scroll.set(next);
        self.follow_selection.set(false);
    }

    /// Erste sichtbare Zeile der Panelliste (für Tests und Diagnose).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn panel_scroll_offset(&self) -> usize {
        self.panel_scroll.get()
    }

    /// `f`: Sammelzeile der fertigen Agenten auf-/zuklappen.
    pub(crate) fn toggle_finished(&mut self) {
        self.finished_expanded = !self.finished_expanded;
        let len = self.panel_entries().len();
        self.selected = self.selected.min(len.saturating_sub(1));
        self.follow_selection.set(true);
    }

    /// Ist die Sammelzeile der fertigen Agenten ausgewählt?
    #[must_use]
    pub(crate) fn selected_is_summary(&self) -> bool {
        matches!(
            self.panel_entries().get(self.selected),
            Some(PanelEntry::Finished | PanelEntry::JobsFinished)
        )
    }

    /// Markiert den Fehlschlag eines Agenten als gesehen (Detailansicht).
    pub(crate) fn mark_seen(&mut self, id: &str) {
        if let Some(live) = self.agents.get_mut(id) {
            live.failure_seen = true;
        }
    }

    /// `c`: alle Fehlschläge quittieren (sie wandern in die Sammelzeile).
    ///
    /// # Rückgabe
    /// `true`, wenn sich etwas geändert hat.
    pub(crate) fn acknowledge_failures(&mut self) -> bool {
        let mut changed = false;
        for live in self.agents.values_mut() {
            if live.is_unseen_failure() {
                live.failure_seen = true;
                changed = true;
            }
        }
        if changed {
            let len = self.panel_entries().len();
            self.selected = self.selected.min(len.saturating_sub(1));
        }
        changed
    }

    /// Kennzahlen des Panels: (aktiv, ungesehene Fehler, fertig).
    #[must_use]
    pub(crate) fn panel_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for live in self.agents.values() {
            if live.phase.is_active() {
                counts.0 += 1;
            } else if live.is_unseen_failure() {
                counts.1 += 1;
            } else {
                counts.2 += 1;
            }
        }
        counts
    }

    /// `true`, sobald mindestens ein Kind-Agent (mit Elternteil) bekannt ist.
    #[must_use]
    pub(crate) fn has_children(&self) -> bool {
        self.agents.values().any(|live| live.parent.is_some())
    }

    /// Live-Zustand des ausgewählten Agenten.
    #[must_use]
    pub(crate) fn selected_live(&self) -> Option<&AgentLive> {
        match self.panel_entries().get(self.selected) {
            Some(PanelEntry::Agent { live, .. }) => Some(*live),
            _ => None,
        }
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
    /// - `scroll`: Scroll-Zustand der Spur (Abstand in umbrochenen Zeilen vom
    ///   Ende; `0` folgt dem neuesten Eintrag). Hochgescrollt bleibt der
    ///   Ausschnitt bei neuen Spur-Einträgen stehen — der Anker wird hier über
    ///   [`ChatScroll::sync_layout`] nachgeführt; zu große Werte werden am
    ///   Anfang gekappt.
    /// - `show_reasoning`: `false` zeigt Reasoning nur als einzeiligen Hinweis.
    pub(crate) fn render_agent_detail(
        &self,
        agent: &SessionId,
        area: Rect,
        buf: &mut Buffer,
        scroll: &ChatScroll,
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
            .clamp(1, (inner.height / 2).max(1));
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
        let viewport = usize::from(trace_area.height);
        let back = scroll.sync_layout(total, viewport, trace_area.width);
        let max_top = total.saturating_sub(viewport);
        let top = max_top.saturating_sub(back);
        trace
            .scroll((u16::try_from(top).unwrap_or(u16::MAX), 0))
            .render(trace_area, buf);
        // Hinweis, solange die Spur nicht folgt (unterste Zeile, rechtsbündig).
        if let Some(text) = scroll.indicator_text("G") {
            let label = format!(" {text} ");
            let label_width = u16::try_from(label.chars().count())
                .unwrap_or(u16::MAX)
                .min(trace_area.width);
            let indicator_area = Rect {
                x: trace_area.x + trace_area.width - label_width,
                y: trace_area.y + trace_area.height - 1,
                width: label_width,
                height: 1,
            };
            Clear.render(indicator_area, buf);
            Paragraph::new(label)
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::REVERSED),
                )
                .render(indicator_area, buf);
        }
    }
}

impl AgentLive {
    /// Provider und Modell, die dieser Agent tatsächlich anspricht, als
    /// `<provider>/<modell>` (aufgelöste Modell-ID, kein Alias, keine
    /// gekürzte Form); ohne Provider nur das Modell.
    #[must_use]
    pub(crate) fn model_route(&self) -> Option<String> {
        crate::app::live_model::route_label(self.provider.as_deref(), self.model.as_deref())
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
    if let Some(route) = live.model_route() {
        lines.push(Line::from(vec![
            Span::styled("Modell: ", dim),
            Span::raw(sanitize_inline(&route)),
        ]));
    }
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

/// Kompakte Dauer: `42s`, `3m05s`, `1h02m`.
#[must_use]
pub(crate) fn compact_elapsed(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Symbol vor einer Agentenzeile.
fn phase_glyph(phase: AgentPhase) -> &'static str {
    match phase {
        AgentPhase::Admitted | AgentPhase::Thinking => "●",
        AgentPhase::Tool => "⚙",
        AgentPhase::Waiting => "○",
        AgentPhase::Done => "✓",
        AgentPhase::Failed => "✗",
        AgentPhase::Cancelled => "⊘",
    }
}

fn phase_style(phase: AgentPhase, theme: Theme) -> Style {
    match phase {
        AgentPhase::Done => style::success_style(theme),
        AgentPhase::Failed | AgentPhase::Cancelled => style::error_style(theme),
        AgentPhase::Tool => style::tool_style(theme),
        AgentPhase::Waiting => style::warning_style(theme),
        AgentPhase::Admitted | AgentPhase::Thinking => {
            Style::default().fg(style::accent_color(theme))
        }
    }
}

/// Kurzer Zustandstext (Werkzeugname statt „Werkzeug“).
fn state_label(live: &AgentLive) -> String {
    match (live.phase, live.current_tool.as_deref()) {
        (AgentPhase::Tool, Some(tool)) => fit_width(&sanitize_inline(tool), 14),
        (AgentPhase::Admitted, _) => "startet".to_owned(),
        (phase, _) => phase.label().to_owned(),
    }
}

/// Kurz-ID eines Laufs (letzte vier Zeichen der Kind-ID) zur
/// Unterscheidung gleicher Rollen.
fn short_id(id: &str) -> String {
    let tail: Vec<char> = id.chars().rev().take(4).collect();
    tail.into_iter().rev().collect()
}

/// Anzeigename einer Zeile: Rolle, bei mehrfach sichtbarer Rolle mit
/// Kurz-ID (`root-orchestrator#3f2a`), damit zwei Läufe derselben Rolle
/// unterscheidbar bleiben.
fn row_label(live: &AgentLive, duplicate: bool) -> String {
    let role = sanitize_inline(&live.role);
    if duplicate {
        format!("{role}#{}", short_id(&live.id))
    } else {
        role
    }
}

/// Eine Agentenzeile, exakt `width` Spalten breit (nie umgebrochen).
///
/// Links Marker, Einrückung, Symbol, Rolle und — soweit Platz ist —
/// `provider/modell` (zuerst ohne Anbieter, dann gekürzt); rechtsbündig
/// Zustand und Dauer. Reicht der Platz nicht, entfallen zuerst Modell, dann
/// Dauer, dann Zustand; die Rolle wird zuletzt mit `…` gekürzt.
fn agent_line(
    live: &AgentLive,
    label: &str,
    depth: usize,
    selected: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let indent = " ".repeat(depth.min(3));
    let glyph = phase_glyph(live.phase);
    let prefix = format!("{marker}{indent}{glyph} ");
    let available = width.saturating_sub(display_width(&prefix));
    let state = state_label(live);
    let elapsed = compact_elapsed(live.elapsed_secs());
    let label_width = display_width(label);
    let full_right = format!(" {state} {elapsed}");
    let short_right = format!(" {state}");
    let right = if label_width + display_width(&full_right) <= available {
        full_right
    } else if label_width.min(8) + display_width(&short_right) <= available {
        short_right
    } else {
        String::new()
    };
    let right_width = display_width(&right);
    let label = fit_width(label, available.saturating_sub(right_width));
    let label_width = display_width(&label);
    let route_room = available
        .saturating_sub(right_width)
        .saturating_sub(label_width)
        .saturating_sub(3);
    let route = match live.model.as_deref() {
        Some(_) if route_room >= 6 => format!(
            " · {}",
            sanitize_inline(&model_segment(
                live.provider.as_deref(),
                live.model.as_deref(),
                route_room
            ))
        ),
        _ => String::new(),
    };
    let route = fit_width(&route, available.saturating_sub(right_width + label_width));
    let used = label_width + display_width(&route) + right_width;
    let pad = " ".repeat(available.saturating_sub(used));
    let phase_style = phase_style(live.phase, theme);
    let line = Line::from(vec![
        Span::raw(format!("{marker}{indent}")),
        Span::styled(format!("{glyph} "), phase_style),
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(route, style::dim_style(theme)),
        Span::raw(pad),
        Span::styled(right, phase_style),
    ]);
    clip_line(line, width)
}

/// Fehlerzeile: `✗ rolle · grund`, rot, auf `width` gekürzt.
fn failure_line(
    live: &AgentLive,
    label: &str,
    depth: usize,
    selected: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let indent = " ".repeat(depth.min(3));
    let reason = live
        .failure_reason
        .as_deref()
        .map(|reason| {
            format!(
                " · {}",
                sanitize_inline(reason.lines().next().unwrap_or(""))
            )
        })
        .unwrap_or_default();
    let text = format!("{marker}{indent}✗ {label}{reason}");
    Line::styled(fit_width(&text, width), style::error_style(theme))
}

/// Sammelzeile der fertigen Agenten:
/// `✓ 6 fertig · uia-worker×3 · uia-explorer×2 · root-orchestrator · Σ 180.0k Tok`.
fn finished_summary_line(
    finished: &[&AgentLive],
    expanded: bool,
    selected: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let arrow = if expanded { "▾" } else { "▸" };
    let head = format!("{marker}✓ {} fertig {arrow}", finished.len());
    let total: u64 = finished.iter().map(|live| live.usage().total()).sum();
    let sigma = format!(" · Σ {} Tok", human_tokens(total));
    let mut roles: Vec<(String, usize)> = Vec::new();
    for live in finished {
        let role = sanitize_inline(&live.role);
        match roles.iter().position(|(name, _)| *name == role) {
            Some(index) => {
                if let Some(entry) = roles.get_mut(index) {
                    entry.1 += 1;
                }
            }
            None => roles.push((role, 1)),
        }
    }
    roles.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let roles_text: String = if expanded {
        String::new()
    } else {
        roles
            .iter()
            .map(|(role, count)| {
                if *count > 1 {
                    format!(" · {role}×{count}")
                } else {
                    format!(" · {role}")
                }
            })
            .collect()
    };
    let head_width = display_width(&head);
    let sigma = if head_width + display_width(&sigma) <= width {
        sigma
    } else {
        String::new()
    };
    let roles_text = fit_width(
        &roles_text,
        width.saturating_sub(head_width + display_width(&sigma)),
    );
    let line = Line::from(vec![
        Span::styled(head, style::success_style(theme)),
        Span::styled(roles_text, style::dim_style(theme)),
        Span::styled(sigma, style::dim_style(theme)),
    ]);
    clip_line(line, width)
}

/// Dünne Kontextanzeige unter einem laufenden Agenten:
/// `   ctx ███░░░ 11% · 22.1k/202.8k`, auf `width` begrenzt.
fn gauge_line(live: &AgentLive, depth: usize, width: usize, theme: Theme) -> Option<Line<'static>> {
    let pct = live.context_percent()?;
    let indent = " ".repeat(depth.min(3) + 3);
    let tail_full = format!(
        " {pct}% · {}/{}",
        human_tokens(live.context_used),
        human_tokens(live.context_window)
    );
    let tail_short = format!(" {pct}%");
    let fixed = display_width(&indent) + display_width("ctx ");
    let tail = if fixed + 4 + display_width(&tail_full) <= width {
        tail_full
    } else {
        tail_short
    };
    let bar_width = width.saturating_sub(fixed + display_width(&tail)).min(12);
    if bar_width < 3 {
        return None;
    }
    let ctx_style = if pct >= 85 {
        style::error_style(theme)
    } else if pct >= 70 {
        style::warning_style(theme)
    } else {
        style::dim_style(theme)
    };
    let mut text = format!("{indent}ctx {}{tail}", gauge(pct, bar_width));
    if live.compactions > 0 {
        text.push_str(&format!(" · {}× verdichtet", live.compactions));
    }
    Some(Line::styled(fit_width(&text, width), ctx_style))
}

/// Kürzt eine gestylte Zeile auf `width` Spalten (Sicherheitsnetz: keine
/// Zeile des Panels darf umbrechen).
fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let span_width = display_width(&span.content);
        if span_width <= remaining {
            remaining -= span_width;
            spans.push(span);
        } else {
            spans.push(Span::styled(
                fit_width(&span.content, remaining),
                span.style,
            ));
            remaining = 0;
        }
    }
    Line::from(spans)
}

/// Einzeilige Zusammenfassung des Agenten-Panels für schmale Terminals
/// (siehe [`crate::panes::AGENTS_PANEL_MIN_TERMINAL_WIDTH`]):
/// `Agenten: ● 1 aktiv (uia-worker) · ✗ 1 fehlgeschlagen · ✓ 6 fertig · …`.
/// `F4` fokussiert das (unsichtbare) Panel, `Enter` öffnet die Details im
/// Vollbild.
///
/// # Rückgabe
/// `None`, solange es keine Kind-Agenten gibt (dann kostet die Zeile nichts).
#[must_use]
pub(crate) fn render_collapsed_summary(
    monitor: &AgentMonitor,
    width: u16,
    theme: Theme,
    focused: bool,
) -> Option<Line<'static>> {
    if !monitor.has_children() {
        return None;
    }
    let width = usize::from(width);
    let (active, failed, finished) = monitor.panel_counts();
    let running: Vec<String> = monitor
        .rows()
        .into_iter()
        .filter(|(_, live)| live.phase.is_active())
        .map(|(_, live)| sanitize_inline(&live.role))
        .collect();
    let marker = if focused { "▸ " } else { "" };
    let mut spans = vec![Span::styled(
        format!("{marker}Agenten: "),
        style::dim_style(theme),
    )];
    let mut text = format!("● {active} aktiv");
    if !running.is_empty() {
        text.push_str(&format!(" ({})", running.join(", ")));
    }
    spans.push(Span::styled(
        text,
        Style::default().fg(style::accent_color(theme)),
    ));
    if failed > 0 {
        spans.push(Span::styled(
            format!(" · ✗ {failed} fehlgeschlagen"),
            style::error_style(theme),
        ));
    }
    if finished > 0 {
        spans.push(Span::styled(
            format!(" · ✓ {finished} fertig"),
            style::success_style(theme),
        ));
    }
    spans.push(Span::styled(
        " · breiteres Fenster für das Panel",
        style::dim_style(theme),
    ));
    Some(clip_line(Line::from(spans), width))
}

/// Eine gerenderte Panelzeile und die Auswahl-Einheit, zu der sie gehört.
struct PanelRow {
    line: Line<'static>,
    entry: Option<usize>,
}

/// Baut alle Zeilen des Panels (ohne Rahmen) für `width` × `height`.
///
/// Pflichtzeilen: eine Zeile je Auswahl-Einheit. Nur wenn danach Platz
/// bleibt, kommen (in dieser Reihenfolge) Kontextbalken laufender Agenten,
/// interne Nutzung und die Live-Vorschau des ausgewählten Agenten dazu.
fn panel_rows(
    monitor: &AgentMonitor,
    width: usize,
    height: usize,
    theme: Theme,
    focused: bool,
) -> Vec<PanelRow> {
    let entries = monitor.panel_entries();
    let finished: Vec<&AgentLive> = monitor
        .rows()
        .into_iter()
        .filter(|(_, live)| live.is_finished_quietly())
        .map(|(_, live)| live)
        .collect();
    // Rollen, die mehrfach als Einzelzeile sichtbar sind, bekommen eine Kurz-ID.
    let mut role_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in &entries {
        if let PanelEntry::Agent { live, .. } = entry {
            *role_counts.entry(live.role.as_str()).or_default() += 1;
        }
    }
    let selected = monitor.selected.min(entries.len().saturating_sub(1));
    let mut rows: Vec<PanelRow> = Vec::new();
    let mut gauges: Vec<Option<Line<'static>>> = Vec::new();
    let finished_jobs: Vec<&JobRow> = monitor.jobs.iter().filter(|row| !row.active).collect();
    let mut jobs_header_done = false;
    for (index, entry) in entries.iter().enumerate() {
        let is_selected = focused && index == selected;
        // Plan R9, Teil F: eine (nicht auswählbare) Kopfzeile vor der
        // Jobs-Gruppe.
        if !jobs_header_done && matches!(entry, PanelEntry::Job { .. } | PanelEntry::JobsFinished) {
            jobs_header_done = true;
            let active = monitor.jobs.iter().filter(|row| row.active).count();
            rows.push(PanelRow {
                line: crate::jobs_panel::group_header_line(
                    active,
                    monitor.jobs.len(),
                    width,
                    theme,
                ),
                entry: None,
            });
            gauges.push(None);
        }
        match entry {
            PanelEntry::Job { row } => {
                rows.push(PanelRow {
                    line: crate::jobs_panel::job_line(row, is_selected, width, theme),
                    entry: Some(index),
                });
                gauges.push(None);
            }
            PanelEntry::JobsFinished => {
                rows.push(PanelRow {
                    line: crate::jobs_panel::finished_jobs_line(
                        &finished_jobs,
                        monitor.finished_expanded,
                        is_selected,
                        width,
                        theme,
                    ),
                    entry: Some(index),
                });
                gauges.push(None);
            }
            PanelEntry::Agent { depth, live } => {
                let duplicate = role_counts.get(live.role.as_str()).copied().unwrap_or(0) > 1;
                let label = row_label(live, duplicate);
                let line = if live.is_unseen_failure() {
                    failure_line(live, &label, *depth, is_selected, width, theme)
                } else {
                    agent_line(live, &label, *depth, is_selected, width, theme)
                };
                rows.push(PanelRow {
                    line,
                    entry: Some(index),
                });
                let running = matches!(
                    live.phase,
                    AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool
                );
                gauges.push(if running {
                    gauge_line(live, *depth, width, theme)
                } else {
                    None
                });
            }
            PanelEntry::Finished => {
                rows.push(PanelRow {
                    line: finished_summary_line(
                        &finished,
                        monitor.finished_expanded,
                        is_selected,
                        width,
                        theme,
                    ),
                    entry: Some(index),
                });
                gauges.push(None);
            }
        }
    }
    if entries.is_empty() {
        rows.push(PanelRow {
            line: Line::styled(
                fit_width("Noch keine Agenten aktiv.", width),
                style::dim_style(theme),
            ),
            entry: None,
        });
        gauges.push(None);
    }

    // Optionale Zeilen nur, solange alles ohne Scrollen passt.
    let mut room = height.saturating_sub(rows.len());
    let mut with_gauges = Vec::with_capacity(rows.len());
    for (row, gauge) in rows.into_iter().zip(gauges) {
        with_gauges.push(row);
        if let Some(gauge) = gauge
            && room > 0
        {
            with_gauges.push(PanelRow {
                line: gauge,
                entry: None,
            });
            room -= 1;
        }
    }
    let mut rows = with_gauges;
    let internal: Vec<_> = monitor.internal_usage().collect();
    if !internal.is_empty() && room >= internal.len() + 2 {
        rows.push(PanelRow {
            line: Line::default(),
            entry: None,
        });
        rows.push(PanelRow {
            line: Line::styled("intern", style::dim_style(theme)),
            entry: None,
        });
        for (purpose, usage) in internal {
            rows.push(PanelRow {
                line: Line::styled(
                    fit_width(
                        &format!(
                            "  {purpose}: ↑{} ↓{}",
                            human_tokens(usage.prompt_tokens()),
                            human_tokens(usage.output_tokens)
                        ),
                        width,
                    ),
                    style::dim_style(theme),
                ),
                entry: None,
            });
        }
        room = height.saturating_sub(rows.len());
    }
    let live_preview = if focused {
        monitor.selected_live().filter(|live| live.phase.is_active())
    } else {
        monitor
            .rows()
            .into_iter()
            .map(|(_, live)| live)
            .find(|live| {
                live.phase.is_active()
                    && (!live.preview.is_empty() || !live.reasoning_preview.is_empty())
            })
    };
    if let Some(live) = live_preview
        && (!live.preview.is_empty() || !live.reasoning_preview.is_empty())
    {
        // Fokus bekommt weiterhin den Trenner/Details-Hinweis. Ohne Fokus
        // bleibt die Live-Projektion kompakt und nutzt nur tatsächlich freien
        // Platz im Panel.
        if focused && room >= 3 {
            rows.push(PanelRow {
                line: Line::default(),
                entry: None,
            });
            rows.push(PanelRow {
                line: Line::styled(
                    fit_width(
                        &format!("── {} live · Enter Details ──", sanitize_inline(&live.role)),
                        width,
                    ),
                    style::dim_style(theme),
                ),
                entry: None,
            });
        }

        let mut preview_room = height.saturating_sub(rows.len());
        if preview_room > 0 && !live.reasoning_preview.is_empty() {
            let text = format!("∴ {}", sanitize_inline(&live.reasoning_preview));
            let count = text.chars().count();
            let tail: String = text.chars().skip(count.saturating_sub(width)).collect();
            rows.push(PanelRow {
                line: Line::styled(
                    fit_width(&tail, width),
                    style::dim_style(theme).add_modifier(Modifier::ITALIC),
                ),
                entry: None,
            });
            preview_room = preview_room.saturating_sub(1);
        }
        if preview_room > 0 && !live.preview.is_empty() {
            let text = format!("» {}", sanitize_inline(&live.preview));
            let count = text.chars().count();
            let tail: String = text.chars().skip(count.saturating_sub(width)).collect();
            rows.push(PanelRow {
                line: Line::styled(fit_width(&tail, width), Style::default()),
                entry: None,
            });
        }
    }
    rows
}

/// Zeichnet das Agenten-Panel.
///
/// # Beschreibung
/// Kompakt, eine Zeile je Lauf (Schlüssel: Kind-ID), nichts bricht um:
/// - laufende/wartende Agenten zuerst: Rolle, `provider/modell` (gekürzt),
///   Zustand, Dauer; ein dünner Kontextbalken nur für laufende Agenten und
///   nur, wenn Platz ist;
/// - fehlgeschlagene Agenten einzeln in Rot mit Kurzgrund, bis sie gesehen
///   (Detailansicht) oder mit `c` quittiert sind;
/// - fertige Agenten als eine Sammelzeile (`f`/Enter klappt auf).
///
/// Der Titel zählt genau, was gezeigt wird („Agenten · 1 aktiv · 6 fertig“).
/// Ist die Liste höher als das Panel, scrollt sie (Mausrad,
/// `scroll_panel_up/down`, im Fokus Bild↑↓/Pos1/Ende); eine
/// Bildlaufleiste auf dem rechten Rahmen zeigt den Ausschnitt.
pub(crate) fn render_agents_panel(
    monitor: &AgentMonitor,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) {
    let area = area.intersection(buf.area);
    if area.width < 3 || area.height < 3 {
        return;
    }
    let (active, failed, finished) = monitor.panel_counts();
    let mut title = format!(" Agenten · {active} aktiv ");
    if failed > 0 {
        title.push_str(&format!("· ✗ {failed} "));
    }
    if finished > 0 {
        title.push_str(&format!("· {finished} fertig "));
    }
    // Plan R9, Teil F: laufende Hintergrund-Jobs im Titel.
    let running_jobs = monitor.jobs.iter().filter(|row| row.active).count();
    if running_jobs > 0 {
        title.push_str(&format!("· ⚙ {running_jobs} Jobs "));
    }
    let inner_width = usize::from(area.width.saturating_sub(2));
    let title = fit_width(&title, inner_width);
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    block.render(area, buf);
    let height = usize::from(inner.height);
    monitor.panel_height.set(height);
    let rows = panel_rows(monitor, inner_width, height, theme, focused);

    // Ausschnitt: Auswahl sichtbar halten (nach Tastennavigation), sonst den
    // gescrollten Abstand respektieren; immer auf den Inhalt begrenzt.
    let max_offset = rows.len().saturating_sub(height);
    let mut offset = monitor.panel_scroll.get().min(max_offset);
    if monitor.follow_selection.get() {
        let selected = monitor.selected;
        if let Some(line_index) = rows.iter().position(|row| row.entry == Some(selected)) {
            if line_index < offset {
                offset = line_index;
            } else if line_index >= offset + height {
                offset = line_index + 1 - height;
            }
        }
        monitor.follow_selection.set(false);
    }
    monitor.panel_scroll.set(offset);
    let visible: Vec<Line<'static>> = rows
        .into_iter()
        .skip(offset)
        .take(height)
        .map(|row| row.line)
        .collect();
    Paragraph::new(visible).render(inner, buf);
    if max_offset > 0 {
        let bar_area = Rect {
            width: inner.width.saturating_add(1),
            ..inner
        }
        .intersection(area);
        let mut state = ScrollbarState::new(max_offset.saturating_add(1))
            .position(offset)
            .viewport_content_length(height);
        StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            bar_area,
            buf,
            &mut state,
        );
    }
}

/// `true`, wenn der Eintrag zur Jobs-Spalte des geteilten Docks gehört
/// (sonst zur Agenten-Spalte); Grundlage für [`dock_follow_column`].
fn is_job_entry(entry: &PanelEntry<'_>) -> bool {
    matches!(entry, PanelEntry::Job { .. } | PanelEntry::JobsFinished)
}

/// Ermittelt für den Split-Modus des Docks, in welcher Spalte die gewählte
/// Zeile liegt und an welcher Position innerhalb der Zeilen dieser Spalte
/// (dieselbe Reihenfolge, in der [`render_agents_only_panel`] bzw.
/// [`render_jobs_only_panel`] ihre Zeilen aus `entries` aufbauen). Liefert
/// `None`, wenn der Auswahlindex außerhalb von `entries` liegt.
fn dock_follow_column(entries: &[PanelEntry<'_>], selected: usize) -> Option<(bool, usize)> {
    let target = entries.get(selected)?;
    let is_job = is_job_entry(target);
    let position = entries[..=selected]
        .iter()
        .filter(|entry| is_job_entry(entry) == is_job)
        .count()
        .checked_sub(1)?;
    Some((is_job, position))
}

/// Zeichnet das oben angepinnte Portrait-Dock (schmale/hohe Terminals).
///
/// Bei [`crate::panes::DockAreas::Combined`] entspricht das Ergebnis
/// [`render_agents_panel`] eins zu eins (eine Liste, ein Rahmen). Bei
/// [`crate::panes::DockAreas::Split`] werden Agenten und Jobs in zwei
/// nebeneinanderliegende Spalten aus demselben Zustand projiziert; beide
/// Spalten teilen sich weiterhin `panel_scroll` als einzigen Scroll-Offset
/// (eine Liste, zwei sichtbare Spalten) und schreiben zusammen genau einmal
/// je Bild in `panel_height` (das Maximum beider Spaltenhöhen). Ein per
/// Tastatur gewählter Eintrag (`follow_selection`) wird dabei stets sofort
/// sichtbar gehalten — unabhängig davon, ob er in der Agenten- oder der
/// Jobs-Spalte liegt —, denn [`render_agents_only_panel`]/
/// [`render_jobs_only_panel`] werten `follow_selection` selbst nicht mehr
/// aus (siehe [`render_dock_rows`]).
pub(crate) fn render_dock(
    monitor: &AgentMonitor,
    areas: crate::panes::DockAreas,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) {
    match areas {
        crate::panes::DockAreas::Combined(area) => {
            render_agents_panel(monitor, area, buf, theme, focused);
        }
        crate::panes::DockAreas::Split { agents, jobs } => {
            let agents = agents.intersection(buf.area);
            let jobs = jobs.intersection(buf.area);
            if monitor.follow_selection.get() {
                let entries = monitor.panel_entries();
                if let Some((is_job, position)) = dock_follow_column(&entries, monitor.selected) {
                    let column = if is_job { jobs } else { agents };
                    let height = usize::from(column.height.saturating_sub(2));
                    if height > 0 {
                        let mut offset = monitor.panel_scroll.get();
                        if position < offset {
                            offset = position;
                        } else if position >= offset + height {
                            offset = position + 1 - height;
                        }
                        monitor.panel_scroll.set(offset);
                    }
                }
                monitor.follow_selection.set(false);
            }
            let agents_height = render_agents_only_panel(monitor, agents, buf, theme, focused);
            let jobs_height = render_jobs_only_panel(monitor, jobs, buf, theme, focused);
            monitor.panel_height.set(agents_height.max(jobs_height));
        }
    }
}

/// Zeichnet nur die Agenten-Spalte des geteilten Docks (ohne Jobs), mit
/// eigenem Rahmen und Titel; nutzt dieselben Einzelzeilen-Renderer wie
/// [`panel_rows`] (`agent_line`/`failure_line`/`finished_summary_line`), also
/// kein Umbruch, sondern Kürzung mit Ellipse.
fn render_agents_only_panel(
    monitor: &AgentMonitor,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) -> usize {
    if area.width < 3 || area.height < 3 {
        return 0;
    }
    let (active, failed, finished) = monitor.panel_counts();
    let mut title = format!(" Agenten · {active} aktiv ");
    if failed > 0 {
        title.push_str(&format!("· ✗ {failed} "));
    }
    if finished > 0 {
        title.push_str(&format!("· {finished} fertig "));
    }
    let inner_width = usize::from(area.width.saturating_sub(2));
    let title = fit_width(&title, inner_width);
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    block.render(area, buf);
    let entries = monitor.panel_entries();
    let selected = monitor.selected.min(entries.len().saturating_sub(1));
    let finished_live: Vec<&AgentLive> = monitor
        .rows()
        .into_iter()
        .filter(|(_, live)| live.is_finished_quietly())
        .map(|(_, live)| live)
        .collect();
    let mut role_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in &entries {
        if let PanelEntry::Agent { live, .. } = entry {
            *role_counts.entry(live.role.as_str()).or_default() += 1;
        }
    }
    let mut rows: Vec<PanelRow> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let is_selected = focused && index == selected;
        match entry {
            PanelEntry::Agent { depth, live } => {
                let duplicate = role_counts.get(live.role.as_str()).copied().unwrap_or(0) > 1;
                let label = row_label(live, duplicate);
                let line = if live.is_unseen_failure() {
                    failure_line(live, &label, *depth, is_selected, inner_width, theme)
                } else {
                    agent_line(live, &label, *depth, is_selected, inner_width, theme)
                };
                rows.push(PanelRow {
                    line,
                    entry: Some(index),
                });
            }
            PanelEntry::Finished => rows.push(PanelRow {
                line: finished_summary_line(
                    &finished_live,
                    monitor.finished_expanded,
                    is_selected,
                    inner_width,
                    theme,
                ),
                entry: Some(index),
            }),
            PanelEntry::Job { .. } | PanelEntry::JobsFinished => {}
        }
    }
    if rows.is_empty() {
        rows.push(PanelRow {
            line: Line::styled(
                fit_width("Noch keine Agenten aktiv.", inner_width),
                style::dim_style(theme),
            ),
            entry: None,
        });
    }

    // Split-Dock: Live-Reasoning gehört in die Agenten-Spalte und darf nicht
    // vom Fokus abhängen. Pflicht-/Auswahlzeilen bleiben unverändert vorne;
    // Preview-Zeilen werden ausschließlich in den noch freien Raum angehängt,
    // sodass Selection- und Scroll-Indizes weiterhin auf den echten Agenten-
    // Einträgen liegen.
    let max_rows = usize::from(inner.height);
    let mut preview_room = max_rows.saturating_sub(rows.len());
    if preview_room > 0 {
        let active: Vec<&AgentLive> = monitor
            .rows()
            .into_iter()
            .map(|(_, live)| live)
            .filter(|live| live.phase.is_active())
            .collect();
        let show_role = active.len() > 1;
        for live in active {
            if preview_room == 0 {
                break;
            }
            if !live.reasoning_preview.is_empty() {
                let body = sanitize_inline(&live.reasoning_preview);
                let text = if show_role {
                    format!("∴ {} · {body}", sanitize_inline(&live.role))
                } else {
                    format!("∴ {body}")
                };
                rows.push(PanelRow {
                    line: Line::styled(
                        fit_width(&text, inner_width),
                        style::dim_style(theme).add_modifier(Modifier::ITALIC),
                    ),
                    entry: None,
                });
                preview_room -= 1;
            }
            if preview_room == 0 {
                break;
            }
            if !live.preview.is_empty() {
                let body = sanitize_inline(&live.preview);
                let text = if show_role {
                    format!("» {} · {body}", sanitize_inline(&live.role))
                } else {
                    format!("» {body}")
                };
                rows.push(PanelRow {
                    line: Line::styled(fit_width(&text, inner_width), Style::default()),
                    entry: None,
                });
                preview_room -= 1;
            }
        }
    }
    render_dock_rows(monitor, rows, inner, buf)
}

/// Zeichnet nur die Jobs-Spalte des geteilten Docks (ohne Agenten), mit
/// eigenem Rahmen und Titel; nutzt dieselben Zeilen-Renderer aus
/// [`crate::jobs_panel`] wie [`panel_rows`].
fn render_jobs_only_panel(
    monitor: &AgentMonitor,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) -> usize {
    if area.width < 3 || area.height < 3 {
        return 0;
    }
    let running = monitor.jobs.iter().filter(|row| row.active).count();
    let title = fit_width(
        &format!(" Jobs · {running} aktiv "),
        usize::from(area.width.saturating_sub(2)),
    );
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    let inner = block.inner(area);
    block.render(area, buf);
    let inner_width = usize::from(inner.width);
    let entries = monitor.panel_entries();
    let selected = monitor.selected.min(entries.len().saturating_sub(1));
    let finished_jobs: Vec<&JobRow> = monitor.jobs.iter().filter(|row| !row.active).collect();
    let mut rows: Vec<PanelRow> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let is_selected = focused && index == selected;
        match entry {
            PanelEntry::Job { row } => rows.push(PanelRow {
                line: crate::jobs_panel::job_line(row, is_selected, inner_width, theme),
                entry: Some(index),
            }),
            PanelEntry::JobsFinished => rows.push(PanelRow {
                line: crate::jobs_panel::finished_jobs_line(
                    &finished_jobs,
                    monitor.finished_expanded,
                    is_selected,
                    inner_width,
                    theme,
                ),
                entry: Some(index),
            }),
            PanelEntry::Agent { .. } | PanelEntry::Finished => {}
        }
    }
    if rows.is_empty() {
        rows.push(PanelRow {
            line: Line::styled(
                fit_width("Keine Jobs.", inner_width),
                style::dim_style(theme),
            ),
            entry: None,
        });
    }
    render_dock_rows(monitor, rows, inner, buf)
}

/// Gemeinsame Ausschnitts- und Zeichenlogik beider Dock-Spalten: schneidet
/// `rows` auf den gemeinsamen `panel_scroll`-Offset zu und rendert sie ohne
/// Umbruch in `inner`. Bewusst ohne den `follow_selection`-Abgleich aus
/// [`render_agents_panel`]: der ist hier nicht nötig, weil [`render_dock`]
/// `follow_selection` bereits vor dem Aufruf beider Spalten auswertet
/// (siehe [`dock_follow_column`]) und `panel_scroll` entsprechend setzt.
fn render_dock_rows(
    monitor: &AgentMonitor,
    rows: Vec<PanelRow>,
    inner: Rect,
    buf: &mut Buffer,
) -> usize {
    let height = usize::from(inner.height);
    let max_offset = rows.len().saturating_sub(height);
    let offset = monitor.panel_scroll.get().min(max_offset);
    let visible: Vec<Line<'static>> = rows
        .into_iter()
        .skip(offset)
        .take(height)
        .map(|row| row.line)
        .collect();
    Paragraph::new(visible).render(inner, buf);
    height
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
                estimated_next_tokens: None,
                threshold_tokens: None,
                reserve_tokens: None,
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
                    placement: None,
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
                    name: "Read(src/lib.rs)".into(),
                    args_preview: String::new(),
                },
                TraceEntry::ToolResult {
                    name: "Read(src/lib.rs)".into(),
                    ok: false,
                    preview: "3ms · nicht gefunden".into(),
                },
            ]
        );
        assert_eq!(monitor.agent("a").ok_or("agent")?.tool_calls, 1);
        Ok(())
    }

    /// R18 D-D: die Spur zeigt Label statt rohem JSON und behält Dauer, Ort
    /// und den werkzeugspezifischen Ausgang (`exit 1` = Fehlschlag).
    #[test]
    fn tool_trace_uses_labels_duration_and_placement() -> TestResult {
        let turn = t1()?;
        let call_id = harw_types::ToolCallId::new();
        let mut monitor = AgentMonitor::default();
        apply_all(
            &mut monitor,
            vec![
                TurnEvent::ToolCallRequested {
                    turn_id: turn.clone(),
                    call_id: call_id.clone(),
                    tool_name: "shell.exec".into(),
                    arguments: serde_json::json!({"command": "false"}),
                },
                TurnEvent::ToolCallCompleted {
                    turn_id: turn,
                    call_id,
                    result: ToolCallResult::success(serde_json::json!({
                        "exit_code": 1, "stdout": "", "stderr": "boom"
                    })),
                    duration_ms: 1500,
                    placement: Some(ToolPlacement::Sandbox),
                },
            ],
        )?;
        assert_eq!(
            trace_of(&monitor)?,
            vec![
                TraceEntry::ToolCall {
                    name: "Shell(false)".into(),
                    args_preview: String::new(),
                },
                TraceEntry::ToolResult {
                    name: "Shell(false)".into(),
                    ok: false,
                    preview: "1.5s · sandbox · exit 1".into(),
                },
            ]
        );
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
        let mut state = ChatScroll::new();
        state.scroll_up_measured(usize::from(scroll));
        render_with_state(monitor, width, height, &state, show_reasoning)
    }

    fn render_with_state(
        monitor: &AgentMonitor,
        width: u16,
        height: u16,
        scroll: &ChatScroll,
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

    /// Agentenzeilen zeigen `<provider>/<modell>` mit der vollen Modell-ID
    /// des Kindes (kein Alias, kein gekürztes Datum).
    #[test]
    fn model_route_shows_provider_and_full_model_id() {
        let mut live = AgentLive::new("a".into(), None, "explorer".into());
        assert_eq!(live.model_route(), None);
        live.model = Some("claude-sonnet-4-5-20250929".into());
        assert_eq!(
            live.model_route().as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );
        live.provider = Some("anthropic".into());
        assert_eq!(
            live.model_route().as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250929")
        );
        let header: String = detail_header_lines(&live, 80)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(
            header.contains("Modell: anthropic/claude-sonnet-4-5-20250929"),
            "{header}"
        );
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
                model: None,
                provider: None,
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
                    estimated_next_tokens: None,
                    threshold_tokens: None,
                    reserve_tokens: None,
                },
                TurnEvent::ReasoningDelta {
                    turn_id: turn.clone(),
                    text: "geheimer Gedanke".into(),
                },
                TurnEvent::ToolCallRequested {
                    turn_id: turn.clone(),
                    call_id: call_id.clone(),
                    tool_name: "fs.grep".into(),
                    arguments: serde_json::json!({"pattern": "cfg"}),
                },
                TurnEvent::ToolCallCompleted {
                    turn_id: turn.clone(),
                    call_id,
                    result: ToolCallResult::success(serde_json::json!({"matches": [1, 2, 3]})),
                    duration_ms: 1,
                    placement: None,
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
        assert!(shown.contains("⚙ Search(\"cfg\" in .)"), "{shown}");
        assert!(
            shown.contains("✓ Search(\"cfg\" in .) 1ms · 3 Treffer"),
            "{shown}"
        );
        assert!(shown.contains("Gefunden in harw.toml"), "{shown}");

        let hidden = render_to_string(&monitor, 70, 20, 0, false)?;
        assert!(!hidden.contains("geheimer Gedanke"), "{hidden}");
        assert!(hidden.contains("∴ Reasoning (1 Zeilen"), "{hidden}");

        // Stile: Werkzeugaufruf cyan, Ergebnis grün.
        let id = SessionId::try_from_str("a")?;
        let area = Rect::new(0, 0, 70, 20);
        let mut buf = Buffer::empty(area);
        monitor.render_agent_detail(&id, area, &mut buf, &ChatScroll::new(), true);
        let find = |needle: char| -> Option<Style> {
            (0..area.height).find_map(|y| {
                (0..area.width).find_map(|x| {
                    let cell = &buf[(x, y)];
                    cell.symbol().starts_with(needle).then(|| cell.style())
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

    /// Erster sichtbarer `Eintrag-NN` in einer gerenderten Ansicht.
    fn first_entry(shown: &str) -> Option<String> {
        shown
            .find("Eintrag-")
            .map(|at| shown[at..at + 10].to_owned())
    }

    /// Hochgescrollt bleibt der Ausschnitt stehen, wenn der Kind-Agent neue
    /// Spur-Einträge liefert; am Ende folgt die Ansicht.
    #[test]
    fn detail_view_keeps_position_when_scrolled_up_and_follows_at_bottom() -> TestResult {
        let mut monitor = AgentMonitor::default();
        apply_all(
            &mut monitor,
            vec![TurnEvent::TurnAborted { turn_id: t1()? }],
        )?;
        let live = monitor.agents.get_mut("a").ok_or("agent")?;
        for index in 0..40 {
            live.trace.push_status(&format!("Eintrag-{index:02}"));
        }
        let mut reading = ChatScroll::new();
        let following = ChatScroll::new();
        // Erstes Zeichnen misst die Spur, danach 5 Zeilen hochscrollen.
        render_with_state(&monitor, 50, 16, &reading, true)?;
        render_with_state(&monitor, 50, 16, &following, true)?;
        reading.scroll_up_measured(5);
        let before = render_with_state(&monitor, 50, 16, &reading, true)?;
        let first_before = first_entry(&before).ok_or("kein Eintrag sichtbar")?;

        let live = monitor.agents.get_mut("a").ok_or("agent")?;
        for index in 40..46 {
            live.trace.push_status(&format!("Eintrag-{index:02}"));
        }
        let after = render_with_state(&monitor, 50, 16, &reading, true)?;
        assert_eq!(first_entry(&after), Some(first_before), "{after}");
        assert!(!after.contains("Eintrag-45"), "{after}");
        assert!(after.contains("neue Zeilen"), "{after}");
        assert!(after.contains("G springt ans Ende"), "{after}");

        let tail = render_with_state(&monitor, 50, 16, &following, true)?;
        assert!(tail.contains("Eintrag-45"), "{tail}");
        assert!(!tail.contains("springt ans Ende"), "{tail}");
        Ok(())
    }

    #[test]
    fn detail_view_for_unknown_agent_does_not_panic() -> TestResult {
        let monitor = AgentMonitor::default();
        let shown = render_to_string(&monitor, 40, 5, 0, true)?;
        assert!(shown.contains("Agent nicht gefunden"), "{shown}");
        Ok(())
    }

    // ── Kompaktes Panel ──────────────────────────────────────────────────

    fn orch(child: &str, role: &str, status: AgentOrchestrationStatus) -> TestResult<AgentEvent> {
        let child_id = SessionId::try_from_str(child)?;
        let parent_id = SessionId::try_from_str("root")?;
        Ok(AgentEvent {
            agent: child_id.clone(),
            parent: Some(parent_id.clone()),
            role: role.into(),
            kind: AgentEventKind::Orchestration(harw_protocol::AgentOrchestrationEvent {
                schema_version: harw_protocol::AgentOrchestrationEvent::CURRENT_SCHEMA_VERSION,
                event_id: format!("{child}-{status:?}"),
                root_session_id: parent_id.clone(),
                parent_session_id: parent_id,
                child_session_id: child_id,
                turn_id: None,
                role: role.into(),
                depth: 1,
                task: None,
                status,
                usage: None,
                duration_ms: None,
                progress: None,
                detail: None,
                tool_calls: None,
                model: Some("glm-5.3-flash-preview-2026".into()),
                provider: Some("zai-coding-plan".into()),
            }),
        })
    }

    fn context(agent: &str, role: &str) -> TestResult<AgentEvent> {
        ev(
            agent,
            Some("root"),
            role,
            TurnEvent::ContextUpdated {
                turn_id: t1()?,
                used_tokens: 22_280,
                window_tokens: 202_800,
                history_items_dropped: 0,
                estimated_next_tokens: None,
                threshold_tokens: None,
                reserve_tokens: None,
            },
        )
    }

    /// Wie im Screenshot: ein laufender Worker, sechs fertige Läufe.
    fn busy_monitor() -> TestResult<AgentMonitor> {
        let mut monitor = AgentMonitor::default();
        for (id, role) in [
            ("w1", "uia-worker"),
            ("w2", "uia-worker"),
            ("w3", "uia-worker"),
            ("e1", "uia-explorer"),
            ("e2", "uia-explorer"),
            ("o1", "root-orchestrator"),
        ] {
            monitor.apply(&orch(id, role, AgentOrchestrationStatus::Running)?);
            monitor.apply(&orch(id, role, AgentOrchestrationStatus::Completed)?);
        }
        monitor.apply(&orch(
            "w7",
            "uia-worker",
            AgentOrchestrationStatus::Running,
        )?);
        monitor.apply(&context("w7", "uia-worker")?);
        Ok(monitor)
    }

    fn panel_screen(
        monitor: &AgentMonitor,
        width: u16,
        height: u16,
        focused: bool,
    ) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| {
            let area = frame.area();
            render_agents_panel(monitor, area, frame.buffer_mut(), Theme::Dark, focused);
        })?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect())
    }

    #[test]
    fn focused_agent_keeps_reasoning_visible_with_answer_text() -> TestResult {
        let mut monitor = AgentMonitor::default();
        let turn_id = TurnId::new();
        monitor.apply(&ev(
            "a",
            None,
            "root-orchestrator",
            TurnEvent::ReasoningDelta {
                turn_id: turn_id.clone(),
                text: "Prüfe zuerst die Runtime".to_owned(),
            },
        )?);
        monitor.apply(&ev(
            "a",
            None,
            "root-orchestrator",
            TurnEvent::AssistantDelta {
                turn_id,
                text: "Runtime sieht gut aus".to_owned(),
            },
        )?);
        let shown = panel_screen(&monitor, 72, 12, true)?.join("\n");
        assert!(shown.contains("∴ Prüfe zuerst die Runtime"), "{shown}");
        assert!(shown.contains("Runtime sieht gut aus"), "{shown}");
        Ok(())
    }
    #[test]
    fn finished_agents_collapse_into_one_summary_line() -> TestResult {
        let mut monitor = busy_monitor()?;
        let rows = panel_screen(&monitor, 44, 20, false)?;
        let shown = rows.join("\n");
        assert!(rows[0].contains("1 aktiv"), "{shown}");
        assert!(rows[0].contains("6 fertig"), "{shown}");
        let summary: Vec<&String> = rows
            .iter()
            .filter(|row| row.contains("✓ 6 fertig"))
            .collect();
        assert_eq!(summary.len(), 1, "{shown}");
        assert!(summary[0].contains("uia-worker×3"), "{shown}");
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("uia-explorer") && !row.contains("fertig"))
                .count(),
            0,
            "fertige Explorer stehen nur in der Sammelzeile: {shown}"
        );
        // Der laufende Worker steht zuerst, mit Modell und Zustand.
        let running = rows
            .iter()
            .position(|row| row.contains("uia-worker") && row.contains("denkt"))
            .ok_or("laufender Worker fehlt")?;
        let summary_row = rows
            .iter()
            .position(|row| row.contains("✓ 6 fertig"))
            .ok_or("Sammelzeile fehlt")?;
        assert!(running < summary_row, "{shown}");
        // Genug Platz: dünner Kontextbalken unter dem laufenden Agenten.
        assert!(rows[running + 1].contains("ctx"), "{shown}");

        monitor.toggle_finished();
        let expanded = panel_screen(&monitor, 44, 20, false)?.join("\n");
        assert_eq!(expanded.matches("uia-explorer").count(), 2, "{expanded}");
        Ok(())
    }

    fn job_row(id: &str, name: &str, active: bool, failed: bool) -> JobRow {
        JobRow {
            id: id.to_owned(),
            name: name.to_owned(),
            state: if active {
                "läuft"
            } else if failed {
                "fehlgeschlagen"
            } else {
                "fertig"
            },
            progress: if active {
                "ninja 120/900 13%".to_owned()
            } else {
                "—".to_owned()
            },
            runtime: "4m10s".to_owned(),
            active,
            failed,
            reason: None,
        }
    }

    /// Plan R9, Teil F: die Jobs-Gruppe steht unter den Agenten (Kopfzeile,
    /// laufende Jobs einzeln, beendete als Sammelzeile), keine Zeile bricht
    /// um, Auswahl und Sammelzeile folgen den Panel-Konventionen.
    #[test]
    fn jobs_group_renders_one_line_per_job_without_wrapping() -> TestResult {
        let mut monitor = busy_monitor()?;
        assert!(monitor.set_jobs(vec![
            job_row(
                "job-a",
                "ladybird build mit einem sehr langen Namen",
                true,
                false
            ),
            job_row("job-b", "vcpkg restore", false, false),
            job_row("job-c", "tests", false, true),
        ]));
        assert!(!monitor.set_jobs(monitor.jobs().to_vec()), "unverändert");
        for width in [16u16, 24, 30, 44, 60, 100] {
            for height in [6u16, 12, 30] {
                let inner = usize::from(width - 2);
                for row in panel_rows(&monitor, inner, usize::from(height - 2), Theme::Dark, true) {
                    assert!(
                        row.line.width() <= inner,
                        "{width}x{height}: {:?}",
                        row.line
                    );
                }
            }
        }
        let rows = panel_screen(&monitor, 60, 30, true)?;
        let shown = rows.join("\n");
        assert!(rows[0].contains("⚙ 1 Jobs"), "{shown}");
        let header = rows
            .iter()
            .position(|row| row.contains("── Jobs · 1 aktiv"))
            .ok_or("Kopfzeile der Jobs-Gruppe fehlt")?;
        let running = rows
            .iter()
            .position(|row| row.contains("ladybird build") && row.contains("läuft"))
            .ok_or("laufender Job fehlt")?;
        let summary = rows
            .iter()
            .position(|row| row.contains("✓ 2 Jobs beendet"))
            .ok_or("Sammelzeile der Jobs fehlt")?;
        assert!(header < running && running < summary, "{shown}");
        assert!(!shown.contains("vcpkg restore"), "eingeklappt: {shown}");

        // Auswahl: der laufende Job ist wählbar, die Sammelzeile klappt auf.
        let job_index = monitor
            .panel_entries()
            .iter()
            .position(|entry| matches!(entry, PanelEntry::Job { .. }))
            .ok_or("Job-Eintrag")?;
        monitor.selected = job_index;
        assert_eq!(monitor.selected_job().as_deref(), Some("job-a"));
        assert!(monitor.selected_agent().is_none());
        monitor.select_next();
        assert!(monitor.selected_is_summary());
        monitor.toggle_finished();
        let expanded = panel_screen(&monitor, 60, 30, true)?.join("\n");
        assert!(expanded.contains("vcpkg restore"), "{expanded}");
        Ok(())
    }

    #[test]
    fn panel_lines_never_wrap_at_any_width() -> TestResult {
        let mut monitor = busy_monitor()?;
        monitor.apply(&ev(
            "f1",
            Some("root"),
            "uia-worker-mit-sehr-langem-rollennamen",
            TurnEvent::TurnFailed {
                turn_id: t1()?,
                reason: "Budget erschöpft nach 40 Werkzeugaufrufen und 3 Wiederholungen".into(),
                retryable: false,
            },
        )?);
        for width in [16u16, 24, 30, 44, 60, 100] {
            for height in [6u16, 12, 30] {
                let inner = usize::from(width - 2);
                for row in panel_rows(&monitor, inner, usize::from(height - 2), Theme::Dark, true) {
                    assert!(
                        row.line.width() <= inner,
                        "{width}x{height}: {:?}",
                        row.line
                    );
                }
                let rows = panel_screen(&monitor, width, height, true)?;
                for row in &rows {
                    let content = row.trim_matches(|c: char| c == '│' || c.is_whitespace());
                    assert_ne!(content, "202.8k", "{width}x{height}: {rows:?}");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn failed_agent_stays_red_with_reason_until_seen() -> TestResult {
        let mut monitor = busy_monitor()?;
        monitor.apply(&ev(
            "f1",
            Some("root"),
            "uia-tester",
            TurnEvent::TurnFailed {
                turn_id: t1()?,
                reason: "Budget erschöpft".into(),
                retryable: false,
            },
        )?);
        let shown = panel_screen(&monitor, 60, 20, false)?.join("\n");
        assert!(shown.contains("✗ uia-tester · Budget erschöpft"), "{shown}");
        assert!(shown.contains("✗ 1"), "Titel zählt den Fehler: {shown}");
        monitor.mark_seen("f1");
        let seen = panel_screen(&monitor, 60, 20, false)?.join("\n");
        assert!(!seen.contains("Budget erschöpft"), "{seen}");
        assert!(seen.contains("✓ 7 fertig"), "{seen}");
        Ok(())
    }

    /// Ein neuer Lauf derselben Rolle ist eine eigene Zeile (Kind-ID) und
    /// wird von einem fertigen früheren Lauf nicht verdeckt — auch nicht in
    /// einem niedrigen Panel.
    #[test]
    fn newer_running_run_of_the_same_role_is_never_masked() -> TestResult {
        let mut monitor = busy_monitor()?;
        monitor.apply(&orch(
            "o2",
            "root-orchestrator",
            AgentOrchestrationStatus::Running,
        )?);
        let rows = panel_screen(&monitor, 44, 6, false)?;
        let shown = rows.join("\n");
        assert!(rows[0].contains("2 aktiv"), "{shown}");
        assert!(
            rows.iter()
                .any(|row| row.contains("root-orchestrator") && row.contains("denkt")),
            "{shown}"
        );
        assert_eq!(
            monitor.agent("o1").map(|live| live.phase),
            Some(AgentPhase::Done)
        );
        Ok(())
    }

    /// Läuft dieselbe Kind-ID nach einem Endzustand erneut, ist sie wieder
    /// aktiv; Dauer und Endzeit gelten ab dem neuen Lauf.
    #[test]
    fn running_after_done_starts_a_fresh_run() -> TestResult {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&orch(
            "o1",
            "root-orchestrator",
            AgentOrchestrationStatus::Running,
        )?);
        monitor.apply(&orch(
            "o1",
            "root-orchestrator",
            AgentOrchestrationStatus::Completed,
        )?);
        assert!(
            monitor
                .agent("o1")
                .is_some_and(|live| live.finished.is_some())
        );
        monitor.apply(&orch(
            "o1",
            "root-orchestrator",
            AgentOrchestrationStatus::Running,
        )?);
        let live = monitor.agent("o1").ok_or("o1")?;
        assert_eq!(live.phase, AgentPhase::Thinking);
        assert!(live.finished.is_none());
        assert_eq!(monitor.panel_counts(), (1, 0, 0));
        // Ein verspätetes `Progress` nach dem Ende reaktiviert dagegen nicht.
        monitor.apply(&orch(
            "o1",
            "root-orchestrator",
            AgentOrchestrationStatus::Completed,
        )?);
        monitor.apply(&orch(
            "o1",
            "root-orchestrator",
            AgentOrchestrationStatus::Progress,
        )?);
        assert_eq!(
            monitor.agent("o1").map(|live| live.phase),
            Some(AgentPhase::Done)
        );
        Ok(())
    }

    #[test]
    fn panel_scrolls_by_wheel_and_keeps_the_selection_visible() -> TestResult {
        let mut monitor = AgentMonitor::default();
        for index in 0..12 {
            monitor.apply(&context(
                &format!("a{index:02}"),
                &format!("rolle-{index:02}"),
            )?);
        }
        let top = panel_screen(&monitor, 44, 8, true)?.join("\n");
        assert!(top.contains("rolle-00"), "{top}");
        monitor.scroll_panel(3);
        let scrolled = panel_screen(&monitor, 44, 8, true)?.join("\n");
        assert_eq!(monitor.panel_scroll_offset(), 3);
        assert!(!scrolled.contains("rolle-00"), "{scrolled}");
        assert!(scrolled.contains("rolle-03"), "{scrolled}");
        assert_eq!(monitor.selected, 0, "Scrollen ändert die Auswahl nicht");
        monitor.select_edge(true);
        let bottom = panel_screen(&monitor, 44, 8, true)?.join("\n");
        assert!(bottom.contains("▸● rolle-11"), "{bottom}");
        monitor.select_page(false);
        assert!(monitor.selected < 11);
        Ok(())
    }

    #[test]
    fn compact_elapsed_formats() {
        assert_eq!(compact_elapsed(42), "42s");
        assert_eq!(compact_elapsed(185), "3m05s");
        assert_eq!(compact_elapsed(1404), "23m24s");
        assert_eq!(compact_elapsed(3720), "1h02m");
    }

    /// Wandelt einen gezeichneten Puffer in Textzeilen um (wie `panel_screen`,
    /// aber für einen beliebigen `Buffer` statt eines Terminals).
    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn render_dock_combined_shows_agents_and_jobs_without_wrapping() -> TestResult {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&context("a1", "uia-worker")?);
        monitor.apply(&context("a2", "uia-explorer")?);
        monitor.set_jobs(vec![
            job_row("job-1", "build-frontend", true, false),
            job_row("job-2", "build-backend", false, false),
        ]);
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Combined(area),
            &mut buf,
            Theme::Dark,
            false,
        );
        let rows = buffer_rows(&buf);
        let shown = rows.join("\n");
        assert!(shown.contains("uia-worker"), "{shown}");
        assert!(shown.contains("build-frontend"), "{shown}");
        for row in &rows {
            assert!(row.chars().count() <= usize::from(area.width), "{shown}");
        }
        Ok(())
    }

    #[test]
    fn render_dock_split_puts_agents_left_and_jobs_right() -> TestResult {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&context("a1", "uia-worker")?);
        monitor.apply(&context("a2", "uia-explorer")?);
        monitor.set_jobs(vec![
            job_row("job-1", "build-frontend", true, false),
            job_row("job-2", "build-backend", false, false),
        ]);
        let agents = Rect::new(0, 0, 34, 10);
        let jobs = Rect::new(34, 0, 34, 10);
        let area = Rect::new(0, 0, 68, 10);
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Split { agents, jobs },
            &mut buf,
            Theme::Dark,
            false,
        );
        let rows = buffer_rows(&buf);
        let left: String = rows
            .iter()
            .map(|row| row.chars().take(34).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let right: String = rows
            .iter()
            .map(|row| row.chars().skip(34).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(left.contains("uia-worker"), "{left}");
        assert!(!right.contains("uia-worker"), "{right}");
        assert!(right.contains("build-frontend"), "{right}");
        assert!(!left.contains("build-frontend"), "{left}");
        Ok(())
    }

    #[test]
    fn render_dock_split_shows_live_reasoning_without_focus() -> TestResult {
        let mut monitor = AgentMonitor::default();
        let turn = TurnId::new();
        monitor.apply(&orch(
            "a1",
            "root-orchestrator",
            AgentOrchestrationStatus::Running,
        )?);
        monitor.apply(&ev(
            "a1",
            None,
            "root-orchestrator",
            TurnEvent::ReasoningDelta {
                turn_id: turn.clone(),
                text: "Prüfe zuerst die DoD-Konfiguration".to_owned(),
            },
        )?);
        monitor.apply(&ev(
            "a1",
            None,
            "root-orchestrator",
            TurnEvent::AssistantDelta {
                turn_id: turn,
                text: "Validator ist geprüft".to_owned(),
            },
        )?);
        monitor.set_jobs(vec![job_row("job-1", "build", false, false)]);

        let agents = Rect::new(0, 0, 40, 12);
        let jobs = Rect::new(40, 0, 40, 12);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Split { agents, jobs },
            &mut buf,
            Theme::Dark,
            false,
        );
        let rows = buffer_rows(&buf);
        let left = rows
            .iter()
            .map(|row| row.chars().take(40).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let right = rows
            .iter()
            .map(|row| row.chars().skip(40).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(left.contains("∴ Prüfe zuerst die DoD"), "{left}");
        assert!(left.contains("» Validator ist geprüft"), "{left}");
        assert!(!right.contains("Prüfe zuerst die DoD"), "{right}");
        Ok(())
    }
    #[test]
    fn render_dock_split_truncates_long_names_with_ellipsis_and_keeps_row_count_fixed() -> TestResult
    {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&context(
            "a1",
            "ein-sehr-sehr-sehr-sehr-langer-rollenname-der-nicht-passt",
        )?);
        monitor.set_jobs(vec![job_row(
            "job-1",
            "ein-sehr-sehr-sehr-sehr-langer-job-name-der-nicht-passt",
            true,
            false,
        )]);
        let agents = Rect::new(0, 0, 20, 10);
        let jobs = Rect::new(20, 0, 20, 10);
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Split { agents, jobs },
            &mut buf,
            Theme::Dark,
            false,
        );
        // Kein Umbruch: die Zeilenkapazität beider Spalten bleibt exakt an
        // die Innenhöhe (Rahmenhöhe - 2) gebunden, egal wie lang der Name ist.
        assert_eq!(monitor.panel_height.get(), usize::from(10_u16 - 2));
        let shown = buffer_rows(&buf).join("\n");
        assert!(shown.contains('…'), "{shown}");
        Ok(())
    }

    /// Regression: `select_next` im geteilten Dock (68–99 Spalten, häufigster
    /// Fall der Portrait-Breite) muss die Auswahl sofort sichtbar halten,
    /// nicht erst ein Bild später — [`render_dock_rows`] wertet
    /// `follow_selection` bewusst nicht mehr selbst aus, das übernimmt
    /// [`render_dock`] vorab für beide Spalten.
    #[test]
    fn render_dock_split_follows_keyboard_selection_in_agents_column() -> TestResult {
        let mut monitor = AgentMonitor::default();
        for i in 0..8 {
            monitor.apply(&context(&format!("a{i}"), "uia-worker")?);
        }
        let agents = Rect::new(0, 0, 24, 5);
        let jobs = Rect::new(24, 0, 24, 5);
        let area = Rect::new(0, 0, 48, 5);
        for _ in 0..7 {
            monitor.select_next();
        }
        assert!(monitor.follow_selection.get());
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Split { agents, jobs },
            &mut buf,
            Theme::Dark,
            true,
        );
        let left: String = buffer_rows(&buf)
            .iter()
            .map(|row| row.chars().take(24).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // Bei 24 Spalten kürzt die Zeile den Namen (`uia-worker…`), daher
        // prüft der Test die Auswahlmarke und den Scroll-Versatz: drei
        // sichtbare Zeilen (5 minus Rahmen), Auswahl 7 → Versatz 5.
        assert!(left.contains('▸'), "{left}");
        assert_eq!(monitor.panel_scroll.get(), 5);
        assert!(!monitor.follow_selection.get());
        Ok(())
    }

    /// Wie oben, aber die Auswahl wandert über die Agenten hinaus in die
    /// Jobs-Spalte: der Ausgleich muss dann die Jobs- statt der
    /// Agenten-Spaltenhöhe verwenden.
    #[test]
    fn render_dock_split_follows_keyboard_selection_in_jobs_column() -> TestResult {
        let mut monitor = AgentMonitor::default();
        monitor.apply(&context("a1", "uia-worker")?);
        monitor.set_jobs(
            (0..8)
                .map(|i| job_row(&format!("job-{i}"), &format!("job{i}"), true, false))
                .collect(),
        );
        let agents = Rect::new(0, 0, 24, 5);
        let jobs = Rect::new(24, 0, 24, 5);
        let area = Rect::new(0, 0, 48, 5);
        // Erster Eintrag ist der eine Agent, danach folgen die acht Jobs.
        for _ in 0..8 {
            monitor.select_next();
        }
        assert!(monitor.follow_selection.get());
        let mut buf = Buffer::empty(area);
        render_dock(
            &monitor,
            crate::panes::DockAreas::Split { agents, jobs },
            &mut buf,
            Theme::Dark,
            true,
        );
        let right: String = buffer_rows(&buf)
            .iter()
            .map(|row| row.chars().skip(24).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(right.contains("job7"), "{right}");
        assert!(!monitor.follow_selection.get());
        Ok(())
    }
}

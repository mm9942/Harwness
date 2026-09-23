//! Live-Beobachtung aller Agenten eines Laufs (Wurzel, Kinder, UIA-Worker).
//!
//! [`AgentMonitor`] ist reiner Zustand: er konsumiert [`AgentEvent`]s vom
//! [`harw_core::AgentEventHub`] und hält pro Agent Status, aktuelles
//! Werkzeug, Live-Tokens, Kontextbelegung und eine kurze Text-Vorschau.
//! [`render_agents_panel`] projiziert diesen Zustand in das rechte
//! Seitenpanel der TUI; die Statuszeile liest [`AgentMonitor::totals`].

use std::collections::BTreeMap;
use std::time::Instant;

use harw_core::{AgentEvent, AgentEventKind};
use harw_protocol::{AgentOrchestrationStatus, TurnEvent};
use harw_types::TokenUsage;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Maximale Länge der Live-Text-Vorschau je Agent (Zeichen).
const PREVIEW_CHARS: usize = 240;

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
        matches!(self, Self::Admitted | Self::Thinking | Self::Tool | Self::Waiting)
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
    pub preview: String,
    pub started: Instant,
    pub finished: Option<Instant>,
    /// Ausgabe-Tokens beim letzten Messpunkt, für die tok/s-Rate.
    rate_mark: (Instant, u64),
    pub tokens_per_sec: f64,
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
            started: now,
            finished: None,
            rate_mark: (now, 0),
            tokens_per_sec: 0.0,
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
        self.preview.push_str(text);
        let count = self.preview.chars().count();
        if count > PREVIEW_CHARS {
            self.preview = self.preview.chars().skip(count - PREVIEW_CHARS).collect();
        }
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
                if orch.task.is_some() {
                    live.task.clone_from(&orch.task);
                }
                if let Some(calls) = orch.tool_calls {
                    live.tool_calls = live.tool_calls.max(calls);
                }
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
                if !live.phase.is_active() {
                    live.finished.get_or_insert_with(Instant::now);
                    live.current_tool = None;
                }
                true
            }
            AgentEventKind::Turn(turn) => self.apply_turn(event, turn),
        }
    }

    fn apply_turn(&mut self, event: &AgentEvent, turn: &TurnEvent) -> bool {
        let live = self.entry(event);
        match turn {
            TurnEvent::TurnStarted { .. } => {
                live.phase = AgentPhase::Thinking;
                live.finished = None;
                live.usage_turn = TokenUsage::default();
            }
            TurnEvent::AssistantDelta { text, .. } | TurnEvent::ReasoningDelta { text, .. } => {
                live.phase = AgentPhase::Thinking;
                live.push_preview(text);
            }
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
            TurnEvent::CompactionApplied { .. } => {
                live.compactions = live.compactions.saturating_add(1);
            }
            TurnEvent::ToolCallRequested { tool_name, .. } => {
                live.phase = AgentPhase::Tool;
                live.current_tool = Some(tool_name.clone());
            }
            TurnEvent::ToolCallCompleted { .. } => {
                live.tool_calls = live.tool_calls.saturating_add(1);
                live.current_tool = None;
                live.phase = AgentPhase::Thinking;
            }
            TurnEvent::ChildSpawned { .. } => live.phase = AgentPhase::Waiting,
            TurnEvent::ChildCompleted { .. } => live.phase = AgentPhase::Thinking,
            TurnEvent::TurnCompleted { usage, .. } => {
                let turn_usage = usage.clone().unwrap_or_else(|| live.usage_turn.clone());
                live.usage_done.add(&turn_usage);
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Done;
                live.current_tool = None;
                live.finished = Some(Instant::now());
            }
            TurnEvent::TurnFailed { .. } => {
                live.usage_done.add(&live.usage_turn.clone());
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Failed;
                live.finished = Some(Instant::now());
            }
            TurnEvent::TurnAborted { .. } => {
                live.usage_done.add(&live.usage_turn.clone());
                live.usage_turn = TokenUsage::default();
                live.phase = AgentPhase::Cancelled;
                live.finished = Some(Instant::now());
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

    /// Der ausgewählte Agent.
    #[must_use]
    pub(crate) fn selected_agent(&self) -> Option<&AgentLive> {
        self.rows().get(self.selected).map(|(_, a)| *a)
    }
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
            let task: String = sanitize_inline(task).chars().take(inner_width.max(8)).collect();
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
    if focused && let Some(live) = monitor.selected_agent()
        && !live.preview.is_empty()
    {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("── {} live ──", sanitize_inline(&live.role)),
            style::dim_style(theme),
        ));
        lines.push(Line::raw(sanitize_inline(&live.preview)));
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

    fn ev(agent: &str, parent: Option<&str>, role: &str, turn: TurnEvent) -> TestResult<AgentEvent> {
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
        assert_eq!(monitor.agent("child").map(|a| a.phase), Some(AgentPhase::Done));
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
}

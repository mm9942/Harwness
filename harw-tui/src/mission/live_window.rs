//! LIVE window: a small read-only projection of one selected agent
//! (PL-94 W02 "Live source and selection", PL-97 §3.3.3).
//!
//! Source: [`super::MissionUi::live_source`] (pinned agent, else the active
//! root orchestrator, else the top ranked candidate). Content comes from the
//! agent's [`AgentLive`] only, in this order, each line sanitized and clipped
//! to one row:
//!
//! 1. reasoning preview tail `∴ …` (only what the provider exposed);
//! 2. current tool `● name · läuft · elapsed` (elapsed since the call first
//!    showed up, see [`super::MissionUi::tool_started`]);
//! 3. last status (newest status or tool result of the trace), or an honest
//!    terminal summary once the run has ended;
//! 4. the recency slot lines (last agent messages, attributed by role).
//!
//! Nothing is synthesized: without data the window says
//! `no visible events yet`. In the portrait stack it is a header-led strip
//! (`─ LIVE · <role> ───` plus at most four body lines), never focused and
//! never scrolled; it takes no keys. A header-only strip carries the most
//! telling line in the header: the tool, else the status, else the
//! reasoning tail, else the newest recency line (W02 `LIVE role · last tool`).
//!
//! In the landscape centre column (PL-97 C3, [`render_window`]) LIVE is a
//! bordered window titled `LIVE · <role>`: the same lines, but the reasoning
//! preview wrapped in full instead of its one-row tail. It scrolls
//! ChatScroll-style ([`super::MissionUi::live_scroll`], newest line at the
//! bottom while following) and takes the keys of [`handle_key`] when
//! focused; `Alt+L`/Enter show the existing agent detail of the LIVE source
//! in the same rect instead.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget,
    Widget, Wrap,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::MissionUi;
use crate::agent_monitor::{AgentLive, AgentPhase, TraceEntry, compact_elapsed};
use crate::chat_scroll::ChatScroll;
use crate::recency::RecencySlots;
use crate::sanitize::sanitize_inline;
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Title of the window.
pub(crate) const LIVE_TITLE: &str = "LIVE";

/// Shown when the selected agent (or the session) has no visible events.
pub(crate) const NO_VISIBLE_EVENTS: &str = "no visible events yet";

/// Indent of the body lines below the header.
pub(crate) const LIVE_INDENT: u16 = 2;

/// Lines one PageUp/PageDown moves the landscape LIVE window (mirrors
/// `AGENT_DETAIL_PAGE` of the agent detail view).
const LIVE_PAGE: usize = 10;

/// What a body line shows (the header-only strip picks by kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// Reasoning preview tail.
    Reasoning,
    /// Current tool call.
    Tool,
    /// Last status, tool result or terminal summary.
    Status,
    /// A recency slot line (attributed to its author).
    Recency,
}

/// One prepared body line (terminal-safe, at most the content width).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveLine {
    kind: LineKind,
    text: String,
    style: Style,
}

/// Content of the LIVE window for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveView {
    /// Header label after `LIVE` (role, pinned marker); `None` without a
    /// source agent.
    label: Option<String>,
    /// Body lines in display order.
    lines: Vec<LiveLine>,
    /// The whole reasoning preview, sanitized with collapsed whitespace
    /// (the landscape window wraps it instead of showing its tail).
    reasoning: String,
}

impl LiveView {
    /// The line a header-only strip carries: tool, else status, else
    /// reasoning, else the first (recency) line.
    fn header_line(&self) -> Option<&LiveLine> {
        [LineKind::Tool, LineKind::Status, LineKind::Reasoning]
            .into_iter()
            .find_map(|kind| self.lines.iter().find(|line| line.kind == kind))
            .or_else(|| self.lines.first())
    }

    /// Body lines this view can fill (geometry input; the geometry clamps it
    /// to [`harw_tui_layout::MINI_LIVE_MAX_BODY_LINES`]).
    #[must_use]
    pub(crate) fn body_lines_wanted(&self) -> u16 {
        u16::try_from(self.lines.len())
            .unwrap_or(u16::MAX)
            .min(harw_tui_layout::MINI_LIVE_MAX_BODY_LINES)
    }
}

/// Collapses every whitespace run (also former line breaks) to one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `prefix` plus the **tail** of `text`, at most `cols` columns: a long
/// reasoning preview keeps its newest words and starts with `…`.
fn tail_fit(prefix: &str, text: &str, cols: usize) -> String {
    let full = format!("{prefix}{text}");
    if display_width(&full) <= cols {
        return full;
    }
    let room = cols.saturating_sub(display_width(prefix)).saturating_sub(1);
    let mut used = 0usize;
    let mut tail: Vec<&str> = Vec::new();
    for ch in text.graphemes(true).rev() {
        let width = ch.width();
        if used + width > room {
            break;
        }
        used += width;
        tail.push(ch);
    }
    tail.reverse();
    let tail = tail.concat();
    fit_width(&format!("{prefix}…{tail}"), cols)
}

/// Newest status or tool result of the agent's trace as a status line.
fn last_status(live: &AgentLive, cols: usize, theme: Theme) -> Option<LiveLine> {
    live.trace
        .entries()
        .filter_map(|entry| match entry {
            TraceEntry::Status(text) => Some(LiveLine {
                kind: LineKind::Status,
                text: fit_width(&format!("· {}", sanitize_inline(text)), cols),
                style: style::dim_style(theme),
            }),
            TraceEntry::ToolResult { name, ok, preview } => {
                let preview = sanitize_inline(preview);
                let detail = if preview.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {preview}")
                };
                let (glyph, line_style) = if *ok {
                    ("✓", style::success_style(theme))
                } else {
                    ("✗", style::error_style(theme))
                };
                Some(LiveLine {
                    kind: LineKind::Status,
                    text: fit_width(&format!("{glyph} {}{detail}", sanitize_inline(name)), cols),
                    style: line_style,
                })
            }
            TraceEntry::Text(_) | TraceEntry::Reasoning(_) | TraceEntry::ToolCall { .. } => None,
        })
        .last()
}

/// Honest summary of an ended run (`✓ fertig · 2m05s`, `✗ fehlgeschlagen · …`).
fn terminal_summary(live: &AgentLive, cols: usize, theme: Theme) -> Option<LiveLine> {
    let elapsed = compact_elapsed(live.elapsed_secs());
    let (text, line_style) = match live.phase {
        AgentPhase::Done => (
            format!("✓ {} · {elapsed}", live.phase.label()),
            style::success_style(theme),
        ),
        AgentPhase::Failed => {
            let reason = live
                .failure_reason
                .as_deref()
                .and_then(|reason| reason.lines().next())
                .map(|reason| format!(" · {}", sanitize_inline(reason)))
                .unwrap_or_default();
            (
                format!("✗ {}{reason}", live.phase.label()),
                style::error_style(theme),
            )
        }
        AgentPhase::Cancelled => (
            format!("⊘ {} · {elapsed}", live.phase.label()),
            style::error_style(theme),
        ),
        AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool | AgentPhase::Waiting => {
            return None;
        }
    };
    Some(LiveLine {
        kind: LineKind::Status,
        text: fit_width(&text, cols),
        style: line_style,
    })
}

/// Builds the LIVE content for a strip `width` columns wide.
///
/// # Arguments
/// - `source`: the selected agent and whether it is pinned.
/// - `tool_started`: when the source's current tool call first showed up
///   ([`super::MissionUi::tool_started`]); `None` outside a tool call.
/// - `recency`: the last agent messages (attributed lines, appended last).
/// - `now`: reference time for the running tool's elapsed time.
#[must_use]
pub(crate) fn view(
    source: Option<(&AgentLive, bool)>,
    tool_started: Option<Instant>,
    recency: &RecencySlots,
    width: u16,
    theme: Theme,
    now: Instant,
) -> LiveView {
    let content_width = width.saturating_sub(LIVE_INDENT);
    let cols = usize::from(content_width);
    let mut lines = Vec::new();
    let mut label = None;
    let mut full_reasoning = String::new();
    if let Some((live, pinned)) = source {
        let role = sanitize_inline(live.role.trim());
        label = Some(if pinned {
            crate::i18n::tr_args(
                "mission.pinned",
                "{role} · pinned",
                &[("role", &(role).to_string())],
            )
        } else {
            role
        });
        let reasoning = collapse_whitespace(&sanitize_inline(&live.reasoning_preview));
        if !reasoning.is_empty() {
            lines.push(LiveLine {
                kind: LineKind::Reasoning,
                text: tail_fit("∴ ", &reasoning, cols),
                style: style::dim_style(theme).add_modifier(Modifier::ITALIC),
            });
        }
        full_reasoning = reasoning;
        if live.phase == AgentPhase::Tool
            && let Some(tool) = live.current_tool.as_deref()
        {
            let elapsed = now
                .saturating_duration_since(tool_started.unwrap_or(live.last_meaningful))
                .as_secs();
            lines.push(LiveLine {
                kind: LineKind::Tool,
                text: fit_width(
                    &crate::i18n::tr_args(
                        "mission.tool_running",
                        "● {p0} · running · {p1}",
                        &[
                            ("p0", &(sanitize_inline(tool)).to_string()),
                            ("p1", &(compact_elapsed(elapsed)).to_string()),
                        ],
                    ),
                    cols,
                ),
                style: style::tool_style(theme),
            });
        }
        let status = terminal_summary(live, cols, theme).or_else(|| last_status(live, cols, theme));
        lines.extend(status);
    }
    for slot in crate::recency_view::slot_lines(recency, content_width) {
        let line_style = if slot.slot == 1 {
            Style::default()
        } else {
            style::dim_style(theme)
        };
        lines.push(LiveLine {
            kind: LineKind::Recency,
            text: slot.text,
            style: line_style,
        });
    }
    LiveView {
        label,
        lines,
        reasoning: full_reasoning,
    }
}

/// Draws `view` into `area`: the header rule in the first row and
/// `area.height - 1` body lines below it. A header-only strip carries the
/// most telling line ([`LiveView::header_line`], or `no visible events yet`)
/// in the header itself.
pub(crate) fn render(view: &LiveView, area: Rect, buf: &mut Buffer, theme: Theme) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let rule_style = Style::default().fg(style::border_color(theme));
    let dim = style::dim_style(theme);
    let mut spans = vec![
        Span::styled("─ ", rule_style),
        Span::styled(
            crate::i18n::tr("mission.live", LIVE_TITLE),
            Style::default()
                .fg(style::accent_color(theme))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(label) = &view.label {
        spans.push(Span::styled(format!(" · {label}"), Style::default()));
    }
    let body_rows = usize::from(area.height - 1);
    if body_rows == 0 {
        match view.header_line() {
            // Die Begründung behält auch in der Kopfzeile ihr neuestes Ende:
            // neu auf den Platz hinter Titel und Rolle eingepasst (PL-97 C3,
            // der schmale Streifen des flachen Querformats).
            Some(line) if line.kind == LineKind::Reasoning => {
                let used: usize = spans.iter().map(|span| display_width(&span.content)).sum();
                let room = width.saturating_sub(used).saturating_sub(3);
                spans.push(Span::styled(
                    format!(" · {}", tail_fit("∴ ", &view.reasoning, room)),
                    line.style,
                ));
            }
            Some(line) => spans.push(Span::styled(format!(" · {}", line.text), line.style)),
            None => spans.push(Span::styled(
                format!(
                    " · {}",
                    crate::i18n::tr("mission.no_events", NO_VISIBLE_EVENTS)
                ),
                dim,
            )),
        }
    }
    let mut header: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let room = width.saturating_sub(used);
        if room == 0 {
            break;
        }
        let text = fit_width(&span.content, room);
        used += display_width(&text);
        header.push(Span::styled(text, span.style));
    }
    if used < width {
        let fill = width - used;
        let rule = if fill > 1 {
            format!(" {}", "─".repeat(fill - 1))
        } else {
            " ".to_owned()
        };
        header.push(Span::styled(rule, rule_style));
    }
    let header_area = Rect { height: 1, ..area };
    Paragraph::new(Line::from(header)).render(header_area, buf);
    if body_rows == 0 {
        return;
    }

    let indent = " ".repeat(usize::from(LIVE_INDENT));
    let mut body: Vec<Line<'static>> = view
        .lines
        .iter()
        .take(body_rows)
        .map(|line| {
            Line::styled(
                fit_width(&format!("{indent}{}", line.text), width),
                line.style,
            )
        })
        .collect();
    if body.is_empty() {
        body.push(Line::styled(
            fit_width(
                &format!(
                    "{indent}{}",
                    crate::i18n::tr("mission.no_events", NO_VISIBLE_EVENTS)
                ),
                width,
            ),
            dim,
        ));
    }
    let body_area = Rect {
        y: area.y.saturating_add(1),
        height: area.height - 1,
        ..area
    };
    Paragraph::new(body).render(body_area, buf);
}

/// Draws `view` as the bordered landscape LIVE window (PL-97 C3) into
/// `area`: title `LIVE · <role>`, the full reasoning preview wrapped, then
/// the tool, status and recency lines, scrolled by `scroll` (ChatScroll:
/// following shows the newest, lowest lines). Build `view` for the inner
/// width plus [`LIVE_INDENT`] so its one-row lines fit the window.
pub(crate) fn render_window(
    view: &LiveView,
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    scroll: &ChatScroll,
    focused: bool,
) {
    let area = area.intersection(buf.area);
    if area.width < 3 || area.height < 3 {
        return;
    }
    let inner_width = usize::from(area.width.saturating_sub(2));
    let title = match &view.label {
        Some(label) => format!(
            " {} · {label} ",
            crate::i18n::tr("mission.live", LIVE_TITLE)
        ),
        None => format!(" {} ", crate::i18n::tr("mission.live", LIVE_TITLE)),
    };
    let border = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(fit_width(&title, inner_width));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    if !view.reasoning.is_empty() {
        lines.push(Line::styled(
            format!("∴ {}", view.reasoning),
            style::dim_style(theme).add_modifier(Modifier::ITALIC),
        ));
    }
    lines.extend(
        view.lines
            .iter()
            .filter(|line| line.kind != LineKind::Reasoning)
            .map(|line| Line::styled(fit_width(&line.text, inner_width), line.style)),
    );
    if lines.is_empty() {
        lines.push(Line::styled(
            fit_width(
                crate::i18n::tr("mission.no_events", NO_VISIBLE_EVENTS),
                inner_width,
            ),
            style::dim_style(theme),
        ));
    }
    // Wie der Chat-Verlauf: umbrochene Zeilen zählen, der Versatz zählt vom
    // Ende (0 folgt der neuesten Zeile).
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total = paragraph.line_count(inner.width);
    let viewport = usize::from(inner.height);
    let back = scroll.sync_layout(total, viewport, inner.width);
    let max_offset = total.saturating_sub(viewport);
    let from_top = max_offset.saturating_sub(back);
    paragraph
        .scroll((u16::try_from(from_top).unwrap_or(u16::MAX), 0))
        .render(inner, buf);
    if max_offset > 0 {
        let bar_area = Rect {
            width: inner.width.saturating_add(1),
            ..inner
        }
        .intersection(area);
        let mut state = ScrollbarState::new(max_offset.saturating_add(1))
            .position(from_top)
            .viewport_content_length(viewport);
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

/// Keys of the focused landscape LIVE window (fixed, not rebindable):
/// `↑`/`k` and `↓`/`j` scroll one line, PageUp/PageDown ten, Home/`g` jump to
/// the oldest line, End/`G` back to the newest; in the in-place detail `r`
/// toggles the reasoning entries. Enter (open/close the detail) belongs to
/// the caller, which knows the LIVE source.
///
/// # Returns
/// `true` when the key was used (redraw).
pub(crate) fn handle_key(ui: &mut MissionUi, key: KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => ui.live_scroll.scroll_up_measured(1),
        KeyCode::Down | KeyCode::Char('j') => ui.live_scroll.scroll_down(1),
        KeyCode::PageUp => ui.live_scroll.scroll_up_measured(LIVE_PAGE),
        KeyCode::PageDown => ui.live_scroll.scroll_down(LIVE_PAGE),
        KeyCode::Home | KeyCode::Char('g') => ui.live_scroll.jump_to_top_measured(),
        KeyCode::End | KeyCode::Char('G') => ui.live_scroll.jump_to_bottom(),
        KeyCode::Char('r') if ui.live_detail_agent().is_some() => {
            ui.live_detail_reasoning = !ui.live_detail_reasoning;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_monitor::AgentMonitor;
    use crate::mission::MissionUi;
    use crate::mission::tests::{screen_row, thinking, turn_event};
    use crate::recency::RecencyEntry;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_protocol::{ToolCallResult, TurnEvent};
    use harw_types::{ToolCallId, TurnId};

    fn screen(view: &LiveView, width: u16, height: u16) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(view, area, frame.buffer_mut(), Theme::Dark);
            })
            .map_err(ctx("draw"))?;
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
    fn empty_live_says_so_honestly() -> TestResult {
        let view = view(
            None,
            None,
            &RecencySlots::new(),
            40,
            Theme::Dark,
            Instant::now(),
        );
        assert_eq!(view.body_lines_wanted(), 0);
        let rows = screen(&view, 40, 1)?;
        assert_eq!(
            rows,
            vec!["─ LIVE · no visible events yet ─────────".to_owned()]
        );
        let rows = screen(&view, 40, 2)?;
        assert!(
            screen_row(&rows, 1)?.contains(NO_VISIBLE_EVENTS),
            "{rows:?}"
        );
        Ok(())
    }

    #[test]
    fn live_shows_reasoning_tool_status_then_recency() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "w1", "uia-worker", "prüfe die Verträge");
        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: ToolCallId::new(),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::json!({ "path": "README.md" }),
            },
        ));
        let live = monitor.agent("w1").ok_or(TestError::Missing("agent"))?;
        let mut recency = RecencySlots::new();
        recency.push(RecencyEntry {
            child: "w2".to_owned(),
            role: "uia-writer".to_owned(),
            text: "Datei geschrieben".to_owned(),
        });
        let view = view(
            Some((live, true)),
            None,
            &recency,
            46,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, 46, 5)?;
        assert!(
            screen_row(&rows, 0)?.starts_with("─ LIVE · uia-worker · pinned ─"),
            "{rows:?}"
        );
        assert_eq!(
            screen_row(&rows, 1)?.trim_end(),
            "  ∴ prüfe die Verträge",
            "{rows:?}"
        );
        assert!(
            screen_row(&rows, 2)?.starts_with("  ● fs.read · running · "),
            "{rows:?}"
        );
        assert!(
            screen_row(&rows, 3)?.contains("1 uia-writer: Datei geschrieben"),
            "no status yet, the recency line follows: {rows:?}"
        );
        assert_eq!(view.body_lines_wanted(), 3);

        // Nur die Kopfzeile: sie trägt das laufende Werkzeug (W02
        // `LIVE role · last tool`), nicht das Reasoning.
        let rows = screen(&view, 46, 1)?;
        assert!(
            screen_row(&rows, 0)?.contains("pinned · ● fs.read"),
            "{rows:?}"
        );
        Ok(())
    }

    #[test]
    fn long_reasoning_keeps_its_tail_and_terminal_runs_are_summarized() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(
            &mut monitor,
            "w1",
            "uia-worker",
            "Anfang des Gedankens, dann sehr viel mehr Text und am Ende das Neueste",
        );
        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::TurnFailed {
                turn_id: TurnId::new(),
                reason: "Zeitlimit\nzweite Zeile".to_owned(),
                retryable: false,
            },
        ));
        let live = monitor.agent("w1").ok_or(TestError::Missing("agent"))?;
        let view = view(
            Some((live, false)),
            None,
            &RecencySlots::new(),
            30,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, 30, 3)?;
        assert!(screen_row(&rows, 1)?.starts_with("  ∴ …"), "{rows:?}");
        assert!(
            screen_row(&rows, 1)?.trim_end().ends_with("das Neueste"),
            "{rows:?}"
        );
        assert_eq!(
            screen_row(&rows, 2)?.trim_end(),
            "  ✗ failed · Zeitlimit",
            "{rows:?}"
        );
        // Ohne Werkzeug trägt die Kopfzeile den Status vor dem Reasoning.
        let rows = screen(&view, 40, 1)?;
        let header = screen_row(&rows, 0)?;
        assert!(
            header.contains("uia-worker · ✗") && !header.contains('∴'),
            "{rows:?}"
        );
        Ok(())
    }

    /// PL-97 C3: a header-only strip that carries the reasoning keeps its
    /// newest words, fitted to the room behind title and role (not the tail
    /// of the body line cut again from the front).
    #[test]
    fn header_only_reasoning_keeps_its_newest_words() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(
            &mut monitor,
            "w1",
            "uia-worker",
            "Anfang des Gedankens, dann sehr viel mehr Text und am Ende das Neueste",
        );
        let live = monitor.agent("w1").ok_or(TestError::Missing("agent"))?;
        let view = view(
            Some((live, false)),
            None,
            &RecencySlots::new(),
            48,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, 48, 1)?;
        let header = screen_row(&rows, 0)?;
        assert!(header.starts_with("─ LIVE · uia-worker · ∴ …"), "{rows:?}");
        assert!(header.ends_with("am Ende das Neueste"), "{rows:?}");
        Ok(())
    }

    /// Draws `view` as the landscape window filling a `width` × `height`
    /// terminal.
    fn window_screen(
        view: &LiveView,
        scroll: &ChatScroll,
        width: u16,
        height: u16,
    ) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_window(view, area, frame.buffer_mut(), Theme::Dark, scroll, true);
            })
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect())
    }

    /// PL-97 C3: the landscape LIVE window is bordered and titled, wraps the
    /// whole reasoning preview instead of its one-row tail, follows the
    /// newest (lowest) line and scrolls back ChatScroll-style with its keys.
    #[test]
    fn landscape_window_wraps_the_reasoning_and_scrolls_like_the_chat() -> TestResult {
        let mut monitor = AgentMonitor::default();
        thinking(
            &mut monitor,
            "w1",
            "uia-worker",
            "eins zwei drei vier fünf sechs sieben acht neun zehn elf zwölf",
        );
        let live = monitor.agent("w1").ok_or(TestError::Missing("agent"))?;
        let view = view(
            Some((live, false)),
            None,
            &RecencySlots::new(),
            14 + LIVE_INDENT,
            Theme::Dark,
            Instant::now(),
        );
        let mut ui = MissionUi::new(Instant::now());
        let rows = window_screen(&view, &ui.live_scroll, 16, 5)?;
        assert!(
            screen_row(&rows, 0)?.starts_with("┌ LIVE · uia-w"),
            "{rows:?}"
        );
        assert!(screen_row(&rows, 4)?.starts_with('└'), "{rows:?}");
        let shown = rows.join("\n");
        assert!(
            shown.contains("zwölf") && !shown.contains("eins"),
            "following shows the newest lines: {rows:?}"
        );

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(handle_key(&mut ui, key(KeyCode::Home)));
        let shown = window_screen(&view, &ui.live_scroll, 16, 5)?.join("\n");
        assert!(
            shown.contains("∴ eins") && !shown.contains("zwölf"),
            "Home shows the oldest line: {shown}"
        );
        assert!(!ui.live_scroll.is_following());
        assert!(handle_key(&mut ui, key(KeyCode::Char('j'))));
        assert!(handle_key(&mut ui, key(KeyCode::End)));
        assert!(ui.live_scroll.is_following(), "End follows again");
        assert!(handle_key(&mut ui, key(KeyCode::Up)));
        assert_eq!(ui.live_scroll.offset(), 1);
        assert!(handle_key(&mut ui, key(KeyCode::PageDown)));
        assert_eq!(ui.live_scroll.offset(), 0);

        assert!(
            !handle_key(&mut ui, key(KeyCode::Char('r'))),
            "r only acts on the in-place detail"
        );
        ui.open_live_detail("w1");
        assert!(handle_key(&mut ui, key(KeyCode::Char('r'))));
        assert!(!ui.live_detail_reasoning);
        assert!(
            !handle_key(&mut ui, KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
            "Ctrl+↑ belongs to scroll_panel_up"
        );
        assert!(!handle_key(&mut ui, key(KeyCode::Char('x'))));
        Ok(())
    }

    /// PL-97 C3: an empty landscape window says so inside its frame.
    #[test]
    fn empty_landscape_window_says_so_honestly() -> TestResult {
        let view = view(
            None,
            None,
            &RecencySlots::new(),
            30,
            Theme::Dark,
            Instant::now(),
        );
        let rows = window_screen(&view, &ChatScroll::new(), 30, 4)?;
        assert!(screen_row(&rows, 0)?.starts_with("┌ LIVE ─"), "{rows:?}");
        assert!(
            screen_row(&rows, 1)?.starts_with(&format!("│{NO_VISIBLE_EVENTS}")),
            "{rows:?}"
        );
        Ok(())
    }

    /// The body lines of one LIVE view (below the header), trimmed.
    fn body(
        monitor: &AgentMonitor,
        ui: &MissionUi,
        id: &str,
        width: u16,
    ) -> TestResult<Vec<String>> {
        let live = monitor.agent(id).ok_or(TestError::Missing("agent"))?;
        let view = view(
            Some((live, false)),
            ui.tool_started(live),
            &RecencySlots::new(),
            width,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, width, 5)?;
        Ok(rows
            .iter()
            .skip(1)
            .map(|row| row.trim_end().to_owned())
            .filter(|row| !row.is_empty())
            .collect())
    }

    /// PL-97 C2 / W02 acceptance 3: one streaming run updates LIVE in place
    /// -- reasoning tail, then the running tool, then its result as the
    /// status line, then the honest end-of-run summary; the assistant prose
    /// itself never becomes a LIVE line.
    #[test]
    fn a_streaming_run_updates_the_live_lines_in_place() -> TestResult {
        let mut monitor = AgentMonitor::default();
        let ui = MissionUi::new(Instant::now());
        let turn = TurnId::new();
        let call = ToolCallId::new();
        thinking(&mut monitor, "w1", "uia-worker", "lese die Konfiguration");
        assert_eq!(
            body(&monitor, &ui, "w1", 46)?,
            vec!["  ∴ lese die Konfiguration".to_owned()]
        );

        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::ToolCallRequested {
                turn_id: turn.clone(),
                call_id: call.clone(),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::json!({ "path": "harw.toml" }),
            },
        ));
        let shown = body(&monitor, &ui, "w1", 46)?;
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert!(screen_row(&shown, 0)?.starts_with("  ∴ lese"), "{shown:?}");
        assert!(
            screen_row(&shown, 1)?.starts_with("  ● fs.read · running · "),
            "{shown:?}"
        );

        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::ToolCallCompleted {
                turn_id: turn.clone(),
                call_id: call,
                result: ToolCallResult::success(serde_json::json!({ "bytes": 12 })),
                duration_ms: 3,
                placement: None,
            },
        ));
        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::AssistantDelta {
                turn_id: turn.clone(),
                text: "Gefunden in harw.toml".to_owned(),
            },
        ));
        let shown = body(&monitor, &ui, "w1", 46)?;
        assert_eq!(shown.len(), 2, "the tool line is replaced: {shown:?}");
        assert!(
            screen_row(&shown, 1)?.starts_with("  ✓ ")
                && !shown.iter().any(|row| row.contains('●')),
            "the tool result is the status line: {shown:?}"
        );
        assert!(
            !shown.iter().any(|row| row.contains("Gefunden")),
            "assistant prose is not a LIVE line: {shown:?}"
        );

        monitor.apply(&turn_event(
            "w1",
            "uia-worker",
            TurnEvent::TurnCompleted {
                turn_id: turn,
                usage: None,
            },
        ));
        let shown = body(&monitor, &ui, "w1", 46)?;
        assert!(
            screen_row(&shown, 1)?.starts_with("  ✓ done · "),
            "the honest end-of-run summary: {shown:?}"
        );
        Ok(())
    }

    /// PL-97 C2 / W02 acceptance 3: a run whose provider exposes no
    /// reasoning only ever shows truthful tool and status lines.
    #[test]
    fn a_run_without_visible_reasoning_shows_only_tool_and_status_lines() -> TestResult {
        let mut monitor = AgentMonitor::default();
        let ui = MissionUi::new(Instant::now());
        let turn = TurnId::new();
        let call = ToolCallId::new();
        monitor.apply(&turn_event(
            "w2",
            "uia-explorer",
            TurnEvent::ToolCallRequested {
                turn_id: turn.clone(),
                call_id: call.clone(),
                tool_name: "fs.grep".to_owned(),
                arguments: serde_json::json!({ "pattern": "cfg" }),
            },
        ));
        let running = body(&monitor, &ui, "w2", 46)?;
        assert_eq!(running.len(), 1, "{running:?}");
        assert!(
            screen_row(&running, 0)?.starts_with("  ● fs.grep · running · "),
            "{running:?}"
        );
        monitor.apply(&turn_event(
            "w2",
            "uia-explorer",
            TurnEvent::ToolCallCompleted {
                turn_id: turn,
                call_id: call,
                result: ToolCallResult::error("nicht gefunden"),
                duration_ms: 1,
                placement: None,
            },
        ));
        let done = body(&monitor, &ui, "w2", 46)?;
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(screen_row(&done, 0)?.starts_with("  ✗ "), "{done:?}");
        for row in running.iter().chain(done.iter()) {
            assert!(!row.contains('∴'), "no invented reasoning: {row:?}");
        }
        Ok(())
    }

    #[test]
    fn live_tail_preserves_unicode_graphemes_in_every_locale_and_width() {
        use harw_config::UiLanguage;
        let payload = "prefix 👩🏽‍💻 é हिंदी 中文 τέλος";
        for language in UiLanguage::ALL {
            crate::i18n::with_language(language, || {
                for width in [0, 1, 2, 8, 20, 40, 160] {
                    let text = tail_fit("∴ ", payload, width);
                    assert!(display_width(&text) <= width, "{width}: {text}");
                    if let Some(tail) = text.split('…').nth(1).filter(|tail| !tail.is_empty()) {
                        assert!(payload.ends_with(tail));
                        let offset = payload.len() - tail.len();
                        assert!(
                            payload
                                .grapheme_indices(true)
                                .any(|(start, _)| start == offset)
                        );
                    }
                }
            });
        }
    }
}

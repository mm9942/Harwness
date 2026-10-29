//! LIVE window: a small read-only projection of one selected agent
//! (PL-94 W02 "Live source and selection", PL-97 §3.3.3).
//!
//! Source: [`super::MissionUi::live_source`] (pinned agent, else the active
//! root orchestrator, else the top ranked candidate). Content comes from the
//! agent's [`AgentLive`] only, in this order, each line sanitized and clipped
//! to one row:
//!
//! 1. reasoning preview tail `∴ …` (only what the provider exposed);
//! 2. current tool `● name · läuft · elapsed`;
//! 3. last status (newest status or tool result of the trace), or an honest
//!    terminal summary once the run has ended;
//! 4. the recency slot lines (last agent messages, attributed by role).
//!
//! Nothing is synthesized: without data the window says
//! `no visible events yet`. In the portrait stack it is a header-led strip
//! (`─ LIVE · <role> ───` plus at most four body lines), never focused and
//! never scrolled; it takes no keys.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthChar;

use crate::agent_monitor::{AgentLive, AgentPhase, TraceEntry, compact_elapsed};
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

/// One prepared body line (terminal-safe, at most the content width).
#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveLine {
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
}

impl LiveView {
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
    let mut tail: Vec<char> = Vec::new();
    for ch in text.chars().rev() {
        let width = ch.width().unwrap_or(0);
        if used + width > room {
            break;
        }
        used += width;
        tail.push(ch);
    }
    tail.reverse();
    let tail: String = tail.into_iter().collect();
    fit_width(&format!("{prefix}…{tail}"), cols)
}

/// Newest status or tool result of the agent's trace as a status line.
fn last_status(live: &AgentLive, cols: usize, theme: Theme) -> Option<LiveLine> {
    live.trace
        .entries()
        .filter_map(|entry| match entry {
            TraceEntry::Status(text) => Some(LiveLine {
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
        text: fit_width(&text, cols),
        style: line_style,
    })
}

/// Builds the LIVE content for a strip `width` columns wide.
///
/// # Arguments
/// - `source`: the selected agent and whether it is pinned.
/// - `recency`: the last agent messages (attributed lines, appended last).
/// - `now`: reference time for the running tool's elapsed time.
#[must_use]
pub(crate) fn view(
    source: Option<(&AgentLive, bool)>,
    recency: &RecencySlots,
    width: u16,
    theme: Theme,
    now: Instant,
) -> LiveView {
    let content_width = width.saturating_sub(LIVE_INDENT);
    let cols = usize::from(content_width);
    let mut lines = Vec::new();
    let mut label = None;
    if let Some((live, pinned)) = source {
        let role = sanitize_inline(live.role.trim());
        label = Some(if pinned {
            format!("{role} · pinned")
        } else {
            role
        });
        let reasoning = collapse_whitespace(&sanitize_inline(&live.reasoning_preview));
        if !reasoning.is_empty() {
            lines.push(LiveLine {
                text: tail_fit("∴ ", &reasoning, cols),
                style: style::dim_style(theme).add_modifier(Modifier::ITALIC),
            });
        }
        if live.phase == AgentPhase::Tool
            && let Some(tool) = live.current_tool.as_deref()
        {
            let elapsed = now
                .saturating_duration_since(live.last_meaningful)
                .as_secs();
            lines.push(LiveLine {
                text: fit_width(
                    &format!(
                        "● {} · läuft · {}",
                        sanitize_inline(tool),
                        compact_elapsed(elapsed)
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
            text: slot.text,
            style: line_style,
        });
    }
    LiveView { label, lines }
}

/// Draws `view` into `area`: the header rule in the first row and
/// `area.height - 1` body lines below it. A header-only strip carries the
/// first body line (or `no visible events yet`) in the header itself.
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
            LIVE_TITLE,
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
        match view.lines.first() {
            Some(first) => spans.push(Span::styled(format!(" · {}", first.text), first.style)),
            None => spans.push(Span::styled(format!(" · {NO_VISIBLE_EVENTS}"), dim)),
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
            fit_width(&format!("{indent}{NO_VISIBLE_EVENTS}"), width),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_monitor::AgentMonitor;
    use crate::mission::tests::{thinking, turn_event};
    use crate::recency::RecencyEntry;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_protocol::TurnEvent;
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
        let view = view(None, &RecencySlots::new(), 40, Theme::Dark, Instant::now());
        assert_eq!(view.body_lines_wanted(), 0);
        let rows = screen(&view, 40, 1)?;
        assert_eq!(
            rows,
            vec!["─ LIVE · no visible events yet ─────────".to_owned()]
        );
        let rows = screen(&view, 40, 2)?;
        assert!(rows[1].contains(NO_VISIBLE_EVENTS), "{rows:?}");
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
            &recency,
            46,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, 46, 5)?;
        assert!(
            rows[0].starts_with("─ LIVE · uia-worker · pinned ─"),
            "{rows:?}"
        );
        assert_eq!(rows[1].trim_end(), "  ∴ prüfe die Verträge", "{rows:?}");
        assert!(rows[2].starts_with("  ● fs.read · läuft · "), "{rows:?}");
        assert!(
            rows[3].contains("1 uia-writer: Datei geschrieben"),
            "no status yet, the recency line follows: {rows:?}"
        );
        assert_eq!(view.body_lines_wanted(), 3);

        // Nur die Kopfzeile: sie trägt die erste Zeile selbst.
        let rows = screen(&view, 46, 1)?;
        assert!(rows[0].contains("pinned · ∴ prüfe"), "{rows:?}");
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
            &RecencySlots::new(),
            30,
            Theme::Dark,
            Instant::now(),
        );
        let rows = screen(&view, 30, 3)?;
        assert!(rows[1].starts_with("  ∴ …"), "{rows:?}");
        assert!(rows[1].trim_end().ends_with("das Neueste"), "{rows:?}");
        assert_eq!(
            rows[2].trim_end(),
            "  ✗ fehlgeschlagen · Zeitlimit",
            "{rows:?}"
        );
        Ok(())
    }
}

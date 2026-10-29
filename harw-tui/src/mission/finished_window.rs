//! Finished / Failed window (PL-97 C2; PL-94 W02 "three status windows").
//!
//! Collapsed (default) it is a one-row, header-led strip: the rule carries
//! the title and the per-category counts inline,
//! `─ FINISHED / FAILED · uia-worker ×3 ✓2 ✗1 NEW · job ×2 ✓2 ───`.
//! Expanded (`Enter`/`f` in the focused window) the header shows the totals
//! and one selectable row per category follows (3..=8 rows, see
//! [`harw_tui_layout::FINISHED_EXPANDED_MAX_ROWS`]).
//!
//! Counts come from [`TerminalRow`] ([`super::projection::terminal_aggregate`]);
//! the constituent overlay of C4 is not part of this window yet.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::WindowUi;
use super::projection::TerminalRow;
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Title of the window.
pub(crate) const FINISHED_TITLE: &str = "FINISHED / FAILED";

/// Text of a strip without any finished run.
const NOTHING_FINISHED: &str = "none yet";

/// Rows the window wants from the geometry: 1 collapsed, otherwise the
/// header plus one row per category (at least one, for "none yet"); the
/// geometry clamps it to its expanded range.
#[must_use]
pub(crate) fn rows_wanted(rows: &[TerminalRow], expanded: bool) -> u16 {
    if expanded {
        u16::try_from(rows.len().max(1).saturating_add(1)).unwrap_or(u16::MAX)
    } else {
        harw_tui_layout::FINISHED_COLLAPSED_ROWS
    }
}

/// Compact text of one category: `uia-worker ×3 ✓2 ✗1 ⊘1 NEW`.
#[must_use]
pub(crate) fn row_text(row: &TerminalRow) -> String {
    let mut text = format!("{} ×{}", row.category, row.total);
    if row.ok > 0 {
        text.push_str(&format!(" ✓{}", row.ok));
    }
    if row.failed > 0 {
        text.push_str(&format!(" ✗{}", row.failed));
    }
    if row.cancelled > 0 {
        text.push_str(&format!(" ⊘{}", row.cancelled));
    }
    if row.unseen_failures > 0 {
        text.push_str(" NEW");
    }
    text
}

/// Style of one category: unseen failures red, other failures warning, the
/// rest dim.
fn row_style(row: &TerminalRow, theme: Theme) -> Style {
    if row.unseen_failures > 0 {
        style::error_style(theme)
    } else if row.failed > 0 {
        style::warning_style(theme)
    } else {
        style::dim_style(theme)
    }
}

/// Header rule `─ TITLE · <detail> ───…` exactly `width` columns wide.
fn header_line(
    detail: Vec<Span<'static>>,
    width: usize,
    theme: Theme,
    focused: bool,
) -> Line<'static> {
    let rule_style = if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    };
    let title_style = Style::default()
        .fg(style::accent_color(theme))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(if focused { "▸ " } else { "─ " }, rule_style),
        Span::styled(FINISHED_TITLE, title_style),
    ];
    let mut used = 2 + display_width(FINISHED_TITLE);
    for span in detail {
        let room = width.saturating_sub(used);
        if room == 0 {
            break;
        }
        let text = fit_width(&span.content, room);
        used += display_width(&text);
        spans.push(Span::styled(text, span.style));
    }
    if used < width {
        let fill = width - used;
        let rule = if fill > 1 {
            format!(" {}", "─".repeat(fill - 1))
        } else {
            " ".to_owned()
        };
        spans.push(Span::styled(rule, rule_style));
    }
    clip(Line::from(spans), width)
}

/// Cuts a styled line to `width` columns (safety net: nothing wraps).
fn clip(line: Line<'static>, width: usize) -> Line<'static> {
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

/// Draws the window: the header rule in the first row, the category rows
/// below it when `area` has more than one row.
pub(crate) fn render(
    rows: &[TerminalRow],
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    ui: &WindowUi,
    focused: bool,
) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = usize::from(area.width);
    let dim = style::dim_style(theme);
    let header_area = Rect { height: 1, ..area };
    if area.height == 1 {
        // Eingeklappt: alle Kategorien in der Kopfzeile.
        let mut detail = Vec::new();
        if rows.is_empty() {
            detail.push(Span::styled(format!(" · {NOTHING_FINISHED}"), dim));
        }
        for row in rows {
            detail.push(Span::styled(" · ", dim));
            detail.push(Span::styled(row_text(row), row_style(row, theme)));
        }
        Paragraph::new(header_line(detail, width, theme, focused)).render(header_area, buf);
        return;
    }

    let (ok, failed): (usize, usize) = rows.iter().fold((0, 0), |(ok, failed), row| {
        (ok + row.ok, failed + row.failed)
    });
    let detail = vec![
        Span::styled(format!(" · ✓ {ok}"), style::success_style(theme)),
        Span::styled(format!(" · ✗ {failed}"), style::error_style(theme)),
    ];
    Paragraph::new(header_line(detail, width, theme, focused)).render(header_area, buf);

    let body = Rect {
        y: area.y.saturating_add(1),
        height: area.height - 1,
        ..area
    };
    let height = usize::from(body.height);
    if rows.is_empty() {
        ui.window_start(0, height);
        Paragraph::new(Line::styled(
            fit_width(&format!("  {NOTHING_FINISHED}"), width),
            dim,
        ))
        .render(body, buf);
        return;
    }
    let selected = ui.selected(rows.len());
    let offset = ui.window_start(rows.len(), height);
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| {
            let marker = if focused && index == selected {
                "▸ "
            } else {
                "  "
            };
            Line::styled(
                fit_width(&format!("{marker}{}", row_text(row)), width),
                row_style(row, theme),
            )
        })
        .collect();
    Paragraph::new(lines).render(body, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn row(category: &str, ok: usize, failed: usize, unseen: usize) -> TerminalRow {
        TerminalRow {
            category: category.to_owned(),
            total: ok + failed,
            ok,
            failed,
            cancelled: 0,
            unseen_failures: unseen,
        }
    }

    fn screen(
        rows: &[TerminalRow],
        width: u16,
        height: u16,
        ui: &WindowUi,
        focused: bool,
    ) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(rows, area, frame.buffer_mut(), Theme::Dark, ui, focused);
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
    fn collapsed_strip_carries_the_counts_in_one_row() -> TestResult {
        let rows = vec![row("uia-worker", 2, 1, 1), row("job", 2, 0, 0)];
        let shown = screen(&rows, 60, 1, &WindowUi::default(), false)?;
        assert_eq!(
            shown,
            vec!["─ FINISHED / FAILED · uia-worker ×3 ✓2 ✗1 NEW · job ×2 ✓2 ──".to_owned()]
        );
        let empty = screen(&[], 40, 1, &WindowUi::default(), true)?;
        assert_eq!(
            empty,
            vec!["▸ FINISHED / FAILED · none yet ─────────".to_owned()]
        );
        let narrow = screen(&rows, 30, 1, &WindowUi::default(), false)?;
        assert!(narrow[0].ends_with('…'), "{narrow:?}");
        Ok(())
    }

    #[test]
    fn expanded_window_lists_one_selectable_row_per_category() -> TestResult {
        let rows = vec![
            row("uia-worker", 2, 1, 1),
            row("job", 2, 0, 0),
            row("uia-explorer", 1, 0, 0),
        ];
        assert_eq!(rows_wanted(&rows, false), 1);
        assert_eq!(rows_wanted(&rows, true), 4);
        assert_eq!(rows_wanted(&[], true), 2);
        let mut ui = WindowUi::default();
        ui.select_step(true, rows.len());
        let shown = screen(&rows, 40, 3, &ui, true)?;
        assert!(
            shown[0].starts_with("▸ FINISHED / FAILED · ✓ 5 · ✗ 1"),
            "{shown:?}"
        );
        assert_eq!(
            shown[1].trim_end(),
            "  uia-worker ×3 ✓2 ✗1 NEW",
            "{shown:?}"
        );
        assert_eq!(shown[2].trim_end(), "▸ job ×2 ✓2", "{shown:?}");
        ui.select_edge(true, rows.len());
        let shown = screen(&rows, 40, 3, &ui, true)?;
        assert_eq!(shown[1].trim_end(), "  job ×2 ✓2", "{shown:?}");
        assert_eq!(shown[2].trim_end(), "▸ uia-explorer ×1 ✓1", "{shown:?}");
        assert_eq!(ui.scroll_offset(), 1, "the selection is kept visible");
        Ok(())
    }
}

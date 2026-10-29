//! Active Jobs window (PL-97 C2).
//!
//! Its own bordered window next to Active Agents (portrait stack from
//! `PHONE_DOCK_SPLIT_MIN_COLS` columns) with its own selection and scroll
//! ([`WindowUi`]). Rows come from [`job_window_rows`] and are drawn with the
//! same single-line renderer as the legacy jobs group
//! ([`crate::jobs_panel::job_line`]): never wrapped, clipped with `…`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget,
    Widget,
};

use super::WindowUi;
use super::projection::job_window_rows;
use crate::jobs_panel::JobRow;
use crate::status_line::fit_width;
use crate::style::{self, Theme};

/// Draws the Active Jobs window into `area` (bordered, title with the number
/// of running jobs).
pub(crate) fn render(
    jobs: &[JobRow],
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    ui: &WindowUi,
    focused: bool,
) {
    let area = area.intersection(buf.area);
    if area.width < 3 || area.height < 3 {
        return;
    }
    let rows = job_window_rows(jobs);
    let running = rows.iter().filter(|row| row.active).count();
    let inner_width = usize::from(area.width.saturating_sub(2));
    let title = fit_width(&format!(" Jobs · {running} aktiv "), inner_width);
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
    if rows.is_empty() {
        ui.window_start(0, height);
        Paragraph::new(Line::styled(
            fit_width("Keine Jobs.", inner_width),
            style::dim_style(theme),
        ))
        .render(inner, buf);
        return;
    }
    let selected = ui.selected(rows.len());
    let offset = ui.window_start(rows.len(), height);
    let visible: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(index, row)| {
            crate::jobs_panel::job_line(row, focused && index == selected, inner_width, theme)
        })
        .collect();
    Paragraph::new(visible).render(inner, buf);

    let max_offset = rows.len().saturating_sub(height);
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

/// Number of rows the window lists (selection bound for its keys).
#[must_use]
pub(crate) fn row_count(jobs: &[JobRow]) -> usize {
    job_window_rows(jobs).len()
}

/// Id of the selected job, if any.
#[must_use]
pub(crate) fn selected_job(jobs: &[JobRow], ui: &WindowUi) -> Option<String> {
    let rows = job_window_rows(jobs);
    rows.get(ui.selected(rows.len())).map(|row| row.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::projection::tests::job;
    use crate::test_support::{TestResult, ctx};

    fn screen(jobs: &[JobRow], ui: &WindowUi, focused: bool) -> TestResult<Vec<String>> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(34, 4))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(jobs, area, frame.buffer_mut(), Theme::Dark, ui, focused);
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
    fn jobs_window_has_its_own_selection_and_scroll() -> TestResult {
        let jobs = vec![
            job("j1", true, false, "läuft"),
            job("j2", true, false, "läuft"),
            job("j3", false, true, "fehlgeschlagen"),
        ];
        let mut ui = WindowUi::default();
        let rows = screen(&jobs, &ui, true)?;
        assert!(rows[0].contains("Jobs · 2 aktiv"), "{rows:?}");
        assert!(
            rows[1].contains("▸") && rows[1].contains("name-j1"),
            "{rows:?}"
        );
        assert!(rows[2].contains("name-j2"), "{rows:?}");

        ui.select_edge(true, row_count(&jobs));
        let rows = screen(&jobs, &ui, true)?;
        assert!(
            rows[2].contains("▸") && rows[2].contains("name-j3"),
            "the selection is followed: {rows:?}"
        );
        assert_eq!(ui.scroll_offset(), 1);
        assert_eq!(selected_job(&jobs, &ui).as_deref(), Some("j3"));

        let rows = screen(&jobs, &ui, false)?;
        assert!(
            !rows.iter().any(|row| row.contains("▸")),
            "no marker without focus: {rows:?}"
        );
        let empty = screen(&[], &WindowUi::default(), false)?;
        assert!(empty[1].contains("Keine Jobs."), "{empty:?}");
        Ok(())
    }
}

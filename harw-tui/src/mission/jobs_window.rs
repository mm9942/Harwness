//! Active Jobs window (PL-97 C2/C4).
//!
//! Its own bordered window next to Active Agents (portrait stack from
//! `PHONE_DOCK_SPLIT_MIN_COLS` columns) with its own selection and scroll
//! ([`WindowUi`]). Rows come from [`job_window_rows`] and are drawn with the
//! same single-line renderer as the legacy jobs group
//! ([`crate::jobs_panel::job_line`]): never wrapped, clipped with `…`. The
//! selection is anchored by job id ([`row_keys`]), so it stays on the same
//! job when a running job finishes and the rows reorder.
//!
//! # Scope: managed jobs only
//! The window lists the session's **managed** jobs (`JobManager`, the
//! `/jobs` operation) and nothing else. Durable `harw-job-store` work items
//! are out of this window (PL-97 §8.3): they have their own lease/claim
//! path (`/jobs work`, `JobMutationScope`), which a key here must never
//! bypass. A job-backed child agent appears as its agent run, not a second
//! time as a job ([`crate::jobs_panel::JobRow::is_listed`]).
//!
//! # Keys (focused)
//! `↑`/`↓`, `k`/`j` select (`k` stays "up", as in every list), PgUp/PgDn,
//! Home/End, `Enter` opens the job detail, `x` asks to stop the selected
//! running job ([`super::stop_job`]: confirmation, then the literal line
//! `/jobs stop <id>` through the regular command path).

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
    let keys: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    if rows.is_empty() {
        ui.window_start(&keys, height);
        Paragraph::new(Line::styled(
            fit_width("Keine Jobs.", inner_width),
            style::dim_style(theme),
        ))
        .render(inner, buf);
        return;
    }
    let selected = ui.selected(&keys);
    let offset = ui.window_start(&keys, height);
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

/// Row keys (job ids) in display order: the selection anchor of the window.
#[must_use]
pub(crate) fn row_keys(jobs: &[JobRow]) -> Vec<&str> {
    job_window_rows(jobs)
        .into_iter()
        .map(|row| row.id.as_str())
        .collect()
}

/// Id of the selected job, if any.
#[must_use]
pub(crate) fn selected_job(jobs: &[JobRow], ui: &WindowUi) -> Option<String> {
    selected_row(jobs, ui).map(|row| row.id.clone())
}

/// The selected job row, if any.
#[must_use]
pub(crate) fn selected_row<'a>(jobs: &'a [JobRow], ui: &WindowUi) -> Option<&'a JobRow> {
    let rows = job_window_rows(jobs);
    let keys: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    rows.get(ui.selected(&keys)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::projection::tests::job;
    use crate::test_support::{TestError, TestResult, ctx};

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
        let row = |rows: &[String], index: usize| -> TestResult<String> {
            rows.get(index).cloned().ok_or(TestError::Missing("row"))
        };
        let mut ui = WindowUi::default();
        let rows = screen(&jobs, &ui, true)?;
        assert!(row(&rows, 0)?.contains("Jobs · 2 aktiv"), "{rows:?}");
        let first = row(&rows, 1)?;
        assert!(first.contains("▸") && first.contains("name-j1"), "{rows:?}");
        assert!(row(&rows, 2)?.contains("name-j2"), "{rows:?}");

        ui.select_edge(true, &row_keys(&jobs));
        let rows = screen(&jobs, &ui, true)?;
        let last = row(&rows, 2)?;
        assert!(
            last.contains("▸") && last.contains("name-j3"),
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
        assert!(row(&empty, 1)?.contains("Keine Jobs."), "{empty:?}");
        Ok(())
    }

    /// The selection is anchored by job id: when the job above it finishes
    /// and the rows reorder (running first, then finished), Enter still
    /// opens the job the operator picked.
    #[test]
    fn selection_stays_on_its_job_when_the_rows_reorder() -> TestResult {
        let mut jobs = vec![
            job("j1", true, false, "läuft"),
            job("j2", true, false, "läuft"),
            job("j3", true, false, "läuft"),
        ];
        let mut ui = WindowUi::default();
        ui.select_step(true, &row_keys(&jobs));
        assert_eq!(selected_job(&jobs, &ui).as_deref(), Some("j2"));
        let first = jobs.first_mut().ok_or(TestError::Missing("j1"))?;
        first.active = false;
        first.state = harw_tool_job::JobState::Succeeded;
        assert_eq!(row_keys(&jobs), vec!["j2", "j3", "j1"]);
        assert_eq!(selected_job(&jobs, &ui).as_deref(), Some("j2"));
        let rows = screen(&jobs, &ui, true)?;
        let marked = rows
            .iter()
            .find(|row| row.contains("▸"))
            .ok_or(TestError::Missing("selection marker"))?;
        assert!(marked.contains("name-j2"), "{rows:?}");
        jobs.retain(|row| row.id != "j2");
        assert_eq!(
            selected_job(&jobs, &ui).as_deref(),
            Some("j3"),
            "gone: the row that moved into its place is selected"
        );
        Ok(())
    }
}

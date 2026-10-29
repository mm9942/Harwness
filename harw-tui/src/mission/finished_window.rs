//! Finished / Failed window (PL-97 C2/C4; PL-94 W02 "three status windows",
//! PL-68 W20-A/C/D).
//!
//! Collapsed (default) it is a one-row, header-led strip: the rule carries
//! the title and the per-category counts inline,
//! `─ FINISHED / FAILED · uia-worker ×3 ✓2 ✗1 NEW · tool:job.start ×2 ✓2 ───`.
//! Expanded (`Enter`/`f` in the focused window) the header shows the totals
//! and one selectable row per category follows (3..=8 rows, see
//! [`harw_tui_layout::FINISHED_EXPANDED_MAX_ROWS`]).
//!
//! Rows are the typed [`TerminalRow`]s of
//! [`super::projection::terminal_aggregate`] (agent roles, `tool:<name>`,
//! `Operator job`, `Unknown job`, the pending-link row `Agent child
//! (pending)`, which the header totals leave out). Unseen failures show
//! `✗n NEW` until acknowledged: `Enter` on an expanded row acknowledges it
//! and opens its constituents ([`ConstituentsView`], newest first, "60 total,
//! 50 listed"), `a` acknowledges the selected row (collapsed: all rows), `c`
//! all of them. Nothing is deleted.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::WindowUi;
use super::projection::{
    ConstituentKind, TerminalBadge, TerminalConstituent, TerminalOutcome, TerminalRow,
    counted_totals,
};
use crate::dod_view::DodMark;
use crate::overlay_view::{OverlayOutcome, OverlayView, is_down, is_up, plain, render_panel};
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Title of the window.
pub(crate) const FINISHED_TITLE: &str = "FINISHED / FAILED";

/// Title suffix of the window while Harw-DoD reports high/critical findings
/// in its summary window (PL-97 D5): ` · ⛨ DoD 2`, ` · ⛨ DoD ≥ 2` for a
/// lower bound, `unverified` appended for an unverified export source.
/// `None` without such findings (or without Harw-DoD).
#[must_use]
pub(crate) fn dod_suffix(mark: Option<DodMark>) -> Option<String> {
    let mark = mark.filter(|mark| mark.is_alert())?;
    let unverified = if mark.verified { "" } else { " unverified" };
    Some(format!(" · ⛨ DoD {}{unverified}", mark.count_text()))
}

/// The [`dod_suffix`] as a styled header span.
fn dod_span(mark: Option<DodMark>, theme: Theme) -> Option<Span<'static>> {
    let text = dod_suffix(mark)?;
    let mark = mark?;
    Some(Span::styled(text, crate::dod_view::mark_style(mark, theme)))
}

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

/// Row keys in display order: the selection anchor of the expanded window,
/// so the selection stays on its category when the order changes (a new
/// failure sorts a category first).
///
/// The key is the typed [`TerminalRow::key`] (category variant plus raw
/// value), never the display label: an agent role can carry the label of a
/// job category (`Unknown job`, `tool:job.start`), and `Enter`/`a` must act
/// on the selected category, not on the first one with the same label.
#[must_use]
pub(crate) fn row_keys(rows: &[TerminalRow]) -> Vec<&str> {
    rows.iter().map(|row| row.key.as_str()).collect()
}

/// The row selected in the expanded window, if any.
#[must_use]
pub(crate) fn selected_row<'a>(rows: &'a [TerminalRow], ui: &WindowUi) -> Option<&'a TerminalRow> {
    let keys = row_keys(rows);
    rows.get(ui.selected(&keys))
}

/// One `agent-role-failure-burst@1` projection of the activity reducer as
/// the unseen-failure badge sees it (PL-97 M3, PL-71): failed child runs of
/// one role, summed over the bursts of that role
/// (`crate::app::activity_glue::ActivityGlue::failure_badges`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FailureBurstBadge {
    /// Terminal-safe role, spelled like the label of its
    /// [`super::projection::TerminalCategory::Role`] row.
    pub(crate) role: String,
    /// Failure bursts of the role (one per parent and failure class).
    pub(crate) bursts: usize,
    /// Failed runs the bursts cover (every run, earlier runs of an agent
    /// included).
    pub(crate) failed: usize,
    /// Of these: the current run of its agent (what the role's row counts
    /// as failed).
    pub(crate) current_failed: usize,
    /// Of these: not acknowledged yet.
    pub(crate) unseen: usize,
}

/// Badge source of the Finished / Failed window (PL-97 M3): the
/// `agent-role-failure-burst@1` projections back the unseen-failure badge
/// (`✗n NEW`) of every agent-role row.
///
/// The bursts are the source: a row's unseen failures are the unseen runs
/// its bursts cover. Only for the failed runs no burst covers (not reduced
/// yet, or evicted with their burst: `row.failed - current_failed`) the row's
/// own count is kept, at most one per uncovered run, so such a run stays NEW
/// (fail visible). Rows without a badge and rows of other categories (jobs)
/// are left alone.
pub(crate) fn apply_failure_bursts(rows: &mut [TerminalRow], badges: &[FailureBurstBadge]) {
    for row in rows.iter_mut() {
        let super::projection::TerminalCategory::Role(role) = &row.category else {
            continue;
        };
        let mut covered = false;
        let (mut unseen, mut current_failed) = (0usize, 0usize);
        for badge in badges.iter().filter(|badge| badge.role == *role) {
            covered = true;
            unseen = unseen.saturating_add(badge.unseen);
            current_failed = current_failed.saturating_add(badge.current_failed);
        }
        if !covered {
            continue;
        }
        let uncovered = row.failed.saturating_sub(current_failed);
        row.unseen_failures = unseen.saturating_add(row.unseen_failures.min(uncovered));
    }
}

/// Compact text of one category: `uia-worker ×3 ✓2 ✗1 ⊘1 NEW`; with only
/// some failures unseen `✗3 (1 NEW)`; a diverging agent/job outcome adds
/// `⚠n`.
#[must_use]
pub(crate) fn row_text(row: &TerminalRow) -> String {
    let mut text = format!("{} ×{}", row.label, row.total);
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
        if row.unseen_failures >= row.failed {
            text.push_str(" NEW");
        } else {
            text.push_str(&format!(" ({} NEW)", row.unseen_failures));
        }
    }
    if let Some(TerminalBadge::OutcomeDiverges { runs }) = row.badge {
        text.push_str(&format!(" ⚠{runs}"));
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
/// below it when `area` has more than one row. `dod` adds the
/// [`dod_suffix`] right after the title (before the counts, so narrow
/// strips cut the counts first).
pub(crate) fn render(
    rows: &[TerminalRow],
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    ui: &WindowUi,
    focused: bool,
    dod: Option<DodMark>,
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
        let mut detail: Vec<Span<'static>> = dod_span(dod, theme).into_iter().collect();
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

    // Die Zeile „Agent child (pending)“ zählt nie mit (sie gehört später
    // zu ihrem Lauf).
    let (ok, failed) = counted_totals(rows);
    let mut detail: Vec<Span<'static>> = dod_span(dod, theme).into_iter().collect();
    detail.push(Span::styled(
        format!(" · ✓ {ok}"),
        style::success_style(theme),
    ));
    detail.push(Span::styled(
        format!(" · ✗ {failed}"),
        style::error_style(theme),
    ));
    Paragraph::new(header_line(detail, width, theme, focused)).render(header_area, buf);

    let body = Rect {
        y: area.y.saturating_add(1),
        height: area.height - 1,
        ..area
    };
    let height = usize::from(body.height);
    let keys = row_keys(rows);
    if rows.is_empty() {
        ui.window_start(&keys, height);
        Paragraph::new(Line::styled(
            fit_width(&format!("  {NOTHING_FINISHED}"), width),
            dim,
        ))
        .render(body, buf);
        return;
    }
    let selected = ui.selected(&keys);
    let offset = ui.window_start(&keys, height);
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

/// Footer of the constituent list.
const CONSTITUENTS_FOOTER: &str = "j/k scrollen · Esc schließen";

/// Page size of the constituent list for `Bild↑`/`Bild↓`.
const CONSTITUENTS_PAGE: usize = 10;

/// Counter text of a row: `60 total, 50 listed` (only `12 total` while
/// every constituent is listed).
#[must_use]
pub(crate) fn listing_text(row: &TerminalRow) -> String {
    if row.listed() < row.total {
        format!("{} total, {} listed", row.total, row.listed())
    } else {
        format!("{} total", row.total)
    }
}

/// One constituent line: `✗ uia-worker#3f2a · Zeitlimit · job ✓ · NEW`.
fn constituent_text(constituent: &TerminalConstituent) -> String {
    let mut text = format!("{} {}", constituent.outcome.glyph(), constituent.label);
    if constituent.kind == ConstituentKind::Job {
        text.push_str(&format!(" ({})", constituent.id));
    }
    if let Some(reason) = &constituent.reason {
        text.push_str(&format!(" · {reason}"));
    }
    if let Some(job) = constituent.diverged {
        text.push_str(&format!(" · ⚠ job {}", job.glyph()));
    }
    if constituent.unseen {
        text.push_str(" · NEW");
    }
    text
}

/// Constituent list of one Finished / Failed row (`Enter` on the expanded
/// row): a read-only snapshot taken when it opens, newest first, with the
/// row's counters ("60 total, 50 listed"). It never runs anything.
#[derive(Debug)]
pub(crate) struct ConstituentsView {
    /// Frame title (`FINISHED / FAILED · <category>`).
    title: String,
    /// Fixed first line: counters.
    header: String,
    /// One line per listed constituent, with its outcome.
    lines: Vec<(String, TerminalOutcome)>,
    /// First visible line.
    scroll: usize,
}

impl ConstituentsView {
    /// Snapshot of `row`.
    #[must_use]
    pub(crate) fn new(row: &TerminalRow) -> Self {
        let mut header = listing_text(row);
        header.push_str(&format!(
            " · ✓{} ✗{} ⊘{}",
            row.ok, row.failed, row.cancelled
        ));
        if !row.category.is_counted() {
            header.push_str(" · not counted until its run has ended");
        }
        Self {
            title: format!(" {FINISHED_TITLE} · {} ", row.label),
            header,
            lines: row
                .constituents
                .iter()
                .map(|constituent| (constituent_text(constituent), constituent.outcome))
                .collect(),
            scroll: 0,
        }
    }

    fn last_start(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }
}

impl OverlayView for ConstituentsView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let lines: Vec<Line<'_>> = self
            .lines
            .iter()
            .map(|(text, outcome)| {
                let style = match outcome {
                    TerminalOutcome::Ok => style::success_style(theme),
                    TerminalOutcome::Failed => style::error_style(theme),
                    TerminalOutcome::Cancelled => style::dim_style(theme),
                };
                Line::styled(text.as_str(), style)
            })
            .collect();
        render_panel(
            area,
            buf,
            theme,
            &self.title,
            Some(Line::styled(self.header.as_str(), style::dim_style(theme))),
            &lines,
            self.scroll,
            CONSTITUENTS_FOOTER,
        );
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if is_down(&key) {
            self.scroll = (self.scroll + 1).min(self.last_start());
            return OverlayOutcome::Stay;
        }
        if is_up(&key) {
            self.scroll = self.scroll.saturating_sub(1);
            return OverlayOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::PageDown => {
                self.scroll = (self.scroll + CONSTITUENTS_PAGE).min(self.last_start());
                OverlayOutcome::Stay
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(CONSTITUENTS_PAGE);
                OverlayOutcome::Stay
            }
            KeyCode::Home => {
                self.scroll = 0;
                OverlayOutcome::Stay
            }
            KeyCode::End => {
                self.scroll = self.last_start();
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::agent_monitor::AgentMonitor;
    use crate::jobs_panel::JobRow;
    use crate::mission::projection::tests::job_in;
    use crate::mission::projection::{
        TerminalCategory, UNKNOWN_JOB_LABEL, acknowledge, terminal_aggregate,
    };
    use crate::mission::tests::{screen_row, turn_event};
    use crate::test_support::{TestError, TestResult, ctx};
    use crossterm::event::KeyModifiers;
    use harw_protocol::TurnEvent;
    use harw_tool_job::{JobState, ManagedJobOrigin};
    use harw_types::TurnId;

    fn row(category: &str, ok: usize, failed: usize, unseen: usize) -> TerminalRow {
        let typed = TerminalCategory::Role(category.to_owned());
        TerminalRow {
            key: typed.key(),
            category: typed,
            label: category.to_owned(),
            total: ok + failed,
            ok,
            failed,
            cancelled: 0,
            unseen_failures: unseen,
            last_at: None,
            badge: None,
            constituents: Vec::new(),
        }
    }

    /// PL-97 M3: die Bursts sind die Quelle des NEW-Abzeichens; nur nicht
    /// abgedeckte Fehlschläge behalten die Zählung der Zeile.
    #[test]
    fn failure_bursts_are_the_source_of_the_unseen_badge() {
        let badge = |unseen, current_failed| FailureBurstBadge {
            role: "uia-writer".to_owned(),
            bursts: 1,
            failed: current_failed,
            current_failed,
            unseen,
        };
        // Alle drei Fehlschläge abgedeckt: die Bursts entscheiden.
        let mut rows = vec![row("uia-writer", 0, 3, 2)];
        apply_failure_bursts(&mut rows, &[badge(1, 3)]);
        assert_eq!(rows[0].unseen_failures, 1);
        // Ein Burst deckt einen Fehlschlag, den die Zeile noch nicht zählt.
        let mut rows = vec![row("uia-writer", 0, 3, 2)];
        apply_failure_bursts(&mut rows, &[badge(3, 3)]);
        assert_eq!(rows[0].unseen_failures, 3);
        // Ein nicht abgedeckter Fehlschlag bleibt NEW (fail visible).
        let mut rows = vec![row("uia-writer", 0, 3, 2)];
        apply_failure_bursts(&mut rows, &[badge(1, 2)]);
        assert_eq!(rows[0].unseen_failures, 2);
        // Ohne Burst der Rolle bleibt die Zeile unverändert.
        let mut rows = vec![row("uia-writer", 0, 3, 2), row("other", 1, 1, 1)];
        apply_failure_bursts(&mut rows, &[]);
        assert_eq!((rows[0].unseen_failures, rows[1].unseen_failures), (2, 1));
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
                render(
                    rows,
                    area,
                    frame.buffer_mut(),
                    Theme::Dark,
                    ui,
                    focused,
                    None,
                );
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

    /// PL-97 D5: mit hohen/kritischen DoD-Befunden trägt der Titel
    /// `· ⛨ DoD n` direkt hinter „FINISHED / FAILED“; ohne Befunde nichts.
    #[test]
    fn dod_suffix_follows_the_title_when_high_findings_exist() -> TestResult {
        let mark = |count, lower_bound, verified| DodMark {
            count,
            lower_bound,
            verified,
        };
        assert_eq!(dod_suffix(None), None);
        assert_eq!(dod_suffix(Some(mark(0, false, true))), None);
        assert_eq!(
            dod_suffix(Some(mark(2, false, true))).as_deref(),
            Some(" · ⛨ DoD 2")
        );
        assert_eq!(
            dod_suffix(Some(mark(2, true, false))).as_deref(),
            Some(" · ⛨ DoD ≥ 2 unverified")
        );
        let rows = vec![row("uia-worker", 2, 1, 1)];
        for height in [1u16, 4] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, height))
                    .map_err(ctx("test terminal"))?;
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    render(
                        &rows,
                        area,
                        frame.buffer_mut(),
                        Theme::Dark,
                        &WindowUi::default(),
                        false,
                        Some(mark(2, false, true)),
                    );
                })
                .map_err(ctx("draw"))?;
            let buffer = terminal.backend().buffer();
            let header: String = (0..60).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(
                header.starts_with("─ FINISHED / FAILED · ⛨ DoD 2 · "),
                "height {height}: {header:?}"
            );
        }
        Ok(())
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
        assert!(screen_row(&narrow, 0)?.ends_with('…'), "{narrow:?}");
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
        ui.select_step(true, &row_keys(&rows));
        let shown = screen(&rows, 40, 3, &ui, true)?;
        assert!(
            screen_row(&shown, 0)?.starts_with("▸ FINISHED / FAILED · ✓ 5 · ✗ 1"),
            "{shown:?}"
        );
        assert_eq!(
            screen_row(&shown, 1)?.trim_end(),
            "  uia-worker ×3 ✓2 ✗1 NEW",
            "{shown:?}"
        );
        assert_eq!(
            screen_row(&shown, 2)?.trim_end(),
            "▸ job ×2 ✓2",
            "{shown:?}"
        );
        ui.select_edge(true, &row_keys(&rows));
        let shown = screen(&rows, 40, 3, &ui, true)?;
        assert_eq!(
            screen_row(&shown, 1)?.trim_end(),
            "  job ×2 ✓2",
            "{shown:?}"
        );
        assert_eq!(
            screen_row(&shown, 2)?.trim_end(),
            "▸ uia-explorer ×1 ✓1",
            "{shown:?}"
        );
        assert_eq!(ui.scroll_offset(), 1, "the selection is kept visible");
        Ok(())
    }

    /// PR #154 (P2): the selection is keyed by the typed category, never by
    /// the display label. Agent roles that carry the label of a job category
    /// (`Unknown job`, `tool:job.start`) give two rows per label; every row
    /// keeps its own key, a selected row resolves to itself (not to the first
    /// row with its label), and acknowledging it touches only its category.
    #[test]
    fn colliding_role_and_job_labels_keep_their_own_selection_key() -> TestResult {
        let mut monitor = AgentMonitor::default();
        for (id, role) in [("r1", UNKNOWN_JOB_LABEL), ("r2", "tool:job.start")] {
            monitor.apply(&turn_event(
                id,
                role,
                TurnEvent::TurnFailed {
                    turn_id: TurnId::new(),
                    reason: "kaputt".to_owned(),
                    retryable: false,
                },
            ));
        }
        monitor.set_jobs(vec![
            job_in("job-1", JobState::Failed),
            JobRow {
                origin: Some(ManagedJobOrigin::Tool {
                    tool: "job.start".to_owned(),
                    call_id: None,
                }),
                ..job_in("job-2", JobState::Failed)
            },
        ]);
        let rows = terminal_aggregate(&monitor, monitor.jobs());
        let labels: BTreeSet<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(labels.len(), 2, "two labels, each twice: {rows:?}");
        let keys = row_keys(&rows);
        let unique: BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), rows.len(), "{keys:?}");

        let mut ui = WindowUi::default();
        for (index, expected) in rows.iter().enumerate() {
            ui.select_edge(false, &keys);
            for _ in 0..index {
                ui.select_step(true, &keys);
            }
            let selected = selected_row(&rows, &ui).ok_or(TestError::Missing("selected row"))?;
            assert_eq!(selected.category, expected.category, "row {index}");
            assert_eq!(ui.selected_key(&keys), Some(expected.key.as_str()));
        }

        // Die Rolle „Unknown job“ quittieren: die gleichnamige
        // Job-Kategorie bleibt NEW.
        let role = TerminalCategory::Role(UNKNOWN_JOB_LABEL.to_owned());
        assert!(acknowledge(&mut monitor, &role));
        let after = terminal_aggregate(&monitor, monitor.jobs());
        let unseen = |category: TerminalCategory| {
            after
                .iter()
                .find(|row| row.category == category)
                .map(|row| row.unseen_failures)
        };
        assert_eq!(unseen(role), Some(0));
        assert_eq!(unseen(TerminalCategory::UnknownJob), Some(1));
        assert_eq!(
            unseen(TerminalCategory::Role("tool:job.start".to_owned())),
            Some(1)
        );
        assert_eq!(
            unseen(TerminalCategory::Tool("job.start".to_owned())),
            Some(1)
        );
        Ok(())
    }

    /// `✗n NEW` for unseen failures (only some unseen: `(n NEW)`), `⚠n` for
    /// a diverging agent/job outcome; the pending-link row stays out of the
    /// header totals.
    #[test]
    fn row_text_marks_new_failures_and_diverging_outcomes() -> TestResult {
        let mut partly = row("uia-worker", 1, 3, 1);
        assert_eq!(row_text(&partly), "uia-worker ×4 ✓1 ✗3 (1 NEW)");
        partly.unseen_failures = 3;
        partly.badge = Some(TerminalBadge::OutcomeDiverges { runs: 2 });
        assert_eq!(row_text(&partly), "uia-worker ×4 ✓1 ✗3 NEW ⚠2");

        let pending = TerminalRow {
            category: TerminalCategory::AgentChildPending,
            key: TerminalCategory::AgentChildPending.key(),
            label: "Agent child (pending)".to_owned(),
            ..row("x", 4, 2, 0)
        };
        let rows = vec![row("uia-worker", 2, 1, 1), pending];
        let shown = screen(&rows, 50, 3, &WindowUi::default(), false)?;
        assert!(
            screen_row(&shown, 0)?.starts_with("─ FINISHED / FAILED · ✓ 2 · ✗ 1 "),
            "the pending row is not counted: {shown:?}"
        );
        assert_eq!(
            screen_row(&shown, 2)?.trim_end(),
            "  Agent child (pending) ×6 ✓4 ✗2"
        );
        Ok(())
    }

    fn constituent(index: usize, outcome: TerminalOutcome) -> TerminalConstituent {
        TerminalConstituent {
            kind: ConstituentKind::Job,
            id: format!("job-{index:02}"),
            label: format!("build-{index:02}"),
            outcome,
            at: None,
            unseen: outcome == TerminalOutcome::Failed,
            reason: (outcome == TerminalOutcome::Failed).then(|| "Exit-Code 1".to_owned()),
            diverged: None,
        }
    }

    /// `Enter` opens the constituents newest first with the counters that
    /// survive eviction ("60 total, 50 listed"); the list scrolls and closes
    /// with Esc, it never runs anything.
    #[test]
    fn constituent_list_shows_counters_and_newest_first() -> TestResult {
        let constituents: Vec<TerminalConstituent> = (0..50)
            .rev()
            .map(|index| {
                let outcome = if index == 49 {
                    TerminalOutcome::Failed
                } else {
                    TerminalOutcome::Ok
                };
                constituent(index, outcome)
            })
            .collect();
        let row = TerminalRow {
            category: TerminalCategory::Tool("job.start".to_owned()),
            key: TerminalCategory::Tool("job.start".to_owned()).key(),
            label: "tool:job.start".to_owned(),
            total: 60,
            ok: 59,
            failed: 1,
            cancelled: 0,
            unseen_failures: 1,
            last_at: None,
            badge: None,
            constituents,
        };
        assert_eq!(listing_text(&row), "60 total, 50 listed");
        let mut view = ConstituentsView::new(&row);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 8))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let area = frame.area();
                view.render(area, frame.buffer_mut(), Theme::Dark);
            })
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        let shown: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(
            screen_row(&shown, 0)?.contains("FINISHED / FAILED · tool:job.start"),
            "{shown:?}"
        );
        assert!(
            screen_row(&shown, 1)?.contains("60 total, 50 listed · ✓59 ✗1 ⊘0"),
            "{shown:?}"
        );
        assert!(
            screen_row(&shown, 2)?.contains("✗ build-49 (job-49) · Exit-Code 1 · NEW"),
            "newest first: {shown:?}"
        );
        assert!(screen_row(&shown, 3)?.contains("✓ build-48"), "{shown:?}");

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(view.on_key(key(KeyCode::Char('j'))), OverlayOutcome::Stay);
        assert_eq!(view.scroll, 1);
        assert_eq!(view.on_key(key(KeyCode::End)), OverlayOutcome::Stay);
        assert_eq!(view.scroll, 49);
        assert_eq!(view.on_key(key(KeyCode::Char('j'))), OverlayOutcome::Stay);
        assert_eq!(view.scroll, 49, "clamped");
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
        Ok(())
    }
}

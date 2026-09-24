//! Plan R9, Teil F: die Jobs-Gruppe des Agenten-Panels und die
//! Job-Detailansicht.
//!
//! # Verantwortungsbereich
//! Reine Darstellung: [`JobRow`] ist der Schnappschuss eines Jobs
//! (aus `JobManager::list(Caller::Operator)`), [`job_line`] und
//! [`finished_jobs_line`] zeichnen genau eine Panelzeile (nie umgebrochen,
//! exakt `width` Spalten, wie die Agentenzeilen in
//! [`crate::agent_monitor`]), [`render_job_detail`] die Detailansicht mit
//! Kopf und Log-Ende. Das Laden übernimmt `app::jobs_glue` (Takt bzw.
//! Ereignis, Ansichten bauen beim Öffnen neu).

use harw_tool_job::JobStatus;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::chat_scroll::ChatScroll;
use crate::sanitize::sanitize_inline;
use crate::status_line::{display_width, fit_width};
use crate::style::{self, Theme};

/// Zeilen je Stream in der Detailansicht.
pub(crate) const DETAIL_LOG_LINES: usize = 60;

/// Schnappschuss eines Jobs für Panel und Detailansicht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobRow {
    /// Kennung (`job-…`).
    pub id: String,
    /// Anzeigename.
    pub name: String,
    /// Kurzer Zustand (`läuft`, `fertig`, …).
    pub state: &'static str,
    /// Erkannter Fortschritt (`ninja 120/900 13%`) oder `—`.
    pub progress: String,
    /// Laufzeit (`1m15s`) oder `—`.
    pub runtime: String,
    /// Noch nicht beendet (wartet, läuft, abgelöst).
    pub active: bool,
    /// Fehlgeschlagen oder mit unbekanntem Ausgang.
    pub failed: bool,
}

impl JobRow {
    /// Baut die Zeile aus dem Zustand der Verwaltung.
    #[must_use]
    pub(crate) fn from_status(status: &JobStatus) -> Self {
        let state = status.meta.state;
        Self {
            id: status.meta.job_id.to_string(),
            name: sanitize_inline(&status.meta.name),
            state: harw_ops::jobs::state_label(state),
            progress: sanitize_inline(&harw_ops::jobs::progress_label(status)),
            runtime: harw_ops::jobs::runtime_label(status),
            active: !state.is_terminal(),
            failed: matches!(
                state,
                harw_tool_job::JobState::Failed | harw_tool_job::JobState::Unknown
            ),
        }
    }
}

/// Symbol und Stil eines Jobs.
fn job_glyph(row: &JobRow, theme: Theme) -> (&'static str, Style) {
    if row.active {
        ("⚙", style::tool_style(theme))
    } else if row.failed {
        ("✗", style::error_style(theme))
    } else {
        ("✓", style::success_style(theme))
    }
}

/// Eine Jobzeile, exakt `width` Spalten breit (nie umgebrochen).
///
/// Links Marker, Symbol, Name und — soweit Platz ist — Fortschritt;
/// rechtsbündig Zustand und Laufzeit. Reicht der Platz nicht, entfallen
/// zuerst Fortschritt, dann Laufzeit, dann Zustand; der Name wird zuletzt
/// mit `…` gekürzt.
#[must_use]
pub(crate) fn job_line(row: &JobRow, selected: bool, width: usize, theme: Theme) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let (glyph, glyph_style) = job_glyph(row, theme);
    let prefix = format!("{marker}{glyph} ");
    let available = width.saturating_sub(display_width(&prefix));
    let name_width = display_width(&row.name);
    let full_right = format!(" {} {}", row.state, row.runtime);
    let short_right = format!(" {}", row.state);
    let right = if name_width + display_width(&full_right) <= available {
        full_right
    } else if name_width.min(8) + display_width(&short_right) <= available {
        short_right
    } else {
        String::new()
    };
    let right_width = display_width(&right);
    let name = fit_width(&row.name, available.saturating_sub(right_width));
    let name_width = display_width(&name);
    let progress_room = available
        .saturating_sub(right_width)
        .saturating_sub(name_width);
    let progress = if row.progress != "—" && progress_room >= 6 {
        fit_width(&format!(" · {}", row.progress), progress_room)
    } else {
        String::new()
    };
    let used = name_width + display_width(&progress) + right_width;
    let pad = " ".repeat(available.saturating_sub(used));
    let line = Line::from(vec![
        Span::raw(marker.to_owned()),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(progress, style::dim_style(theme)),
        Span::raw(pad),
        Span::styled(right, glyph_style),
    ]);
    clip(line, width)
}

/// Sammelzeile beendeter Jobs: `✓ 4 Jobs beendet ▸ · 1 fehlgeschlagen`.
#[must_use]
pub(crate) fn finished_jobs_line(
    finished: &[&JobRow],
    expanded: bool,
    selected: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let arrow = if expanded { "▾" } else { "▸" };
    let failed = finished.iter().filter(|row| row.failed).count();
    let head = format!("{marker}✓ {} Jobs beendet {arrow}", finished.len());
    let tail = if failed > 0 {
        format!(" · {failed} fehlgeschlagen")
    } else {
        String::new()
    };
    let tail = fit_width(&tail, width.saturating_sub(display_width(&head)));
    let line = Line::from(vec![
        Span::styled(head, style::success_style(theme)),
        Span::styled(tail, style::error_style(theme)),
    ]);
    clip(line, width)
}

/// Kopfzeile der Jobs-Gruppe: `── Jobs · 2 aktiv ──…`, exakt `width` breit.
#[must_use]
pub(crate) fn group_header_line(
    active: usize,
    total: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let label = if active > 0 {
        format!("── Jobs · {active} aktiv ")
    } else {
        format!("── Jobs · {total} ")
    };
    let fill = width.saturating_sub(display_width(&label));
    Line::styled(
        fit_width(&format!("{label}{}", "─".repeat(fill)), width),
        style::dim_style(theme),
    )
}

/// Kürzt eine gestylte Zeile auf `width` Spalten (keine Panelzeile bricht um).
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

/// Inhalt der Job-Detailansicht (beim Öffnen und im Takt neu gebaut).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobDetail {
    /// Die Zeile des Jobs.
    pub row: JobRow,
    /// Kopfzeilen (Befehl, Besitzer, Ort, Zähler, Logs).
    pub header: Vec<String>,
    /// Letzte stdout-Zeilen.
    pub stdout: Vec<String>,
    /// Letzte stderr-Zeilen.
    pub stderr: Vec<String>,
}

impl JobDetail {
    /// Baut die Detailansicht aus dem Zustand und den Logdateien.
    #[must_use]
    pub(crate) fn from_status(status: &JobStatus) -> Self {
        let meta = &status.meta;
        let mut header = vec![
            format!("Befehl: {}", sanitize_inline(&meta.command)),
            format!(
                "Ort: {} · PID: {} · Besitzer: {}",
                if meta.executed_on_host {
                    "Host"
                } else {
                    "Sandbox"
                },
                meta.pid
                    .map_or_else(|| "—".to_owned(), |pid| pid.to_string()),
                sanitize_inline(&meta.owner.session)
            ),
            format!(
                "{} Warnungen · {} Fehler · {} Zeilen{}",
                meta.warnings,
                meta.errors,
                status.stdout_lines + status.stderr_lines,
                meta.exit_code
                    .map(|code| format!(" · Exit-Code {code}"))
                    .unwrap_or_default()
            ),
            format!("Logs: {}", status.log_dir.display()),
        ];
        if let Some(error) = &meta.launch_error {
            header.push(format!("Startfehler: {}", sanitize_inline(error)));
        }
        let tail = |file: &str| {
            harw_tool_job::logs::tail_of_file(&status.log_dir.join(file), DETAIL_LOG_LINES)
                .into_iter()
                .map(|line| sanitize_inline(&line))
                .collect::<Vec<_>>()
        };
        Self {
            row: JobRow::from_status(status),
            header,
            stdout: tail(harw_tool_job::model::STDOUT_LOG),
            stderr: tail(harw_tool_job::model::STDERR_LOG),
        }
    }
}

/// Zeichnet die Detailansicht eines Jobs: Kopf, darunter das Log-Ende
/// (stdout, dann stderr), scrollbar wie die Agenten-Detailansicht.
pub(crate) fn render_job_detail(
    detail: Option<&JobDetail>,
    area: Rect,
    buf: &mut Buffer,
    scroll: &ChatScroll,
    theme: Theme,
) {
    let Some(detail) = detail else {
        Paragraph::new(Line::styled(
            "Job nicht gefunden.",
            Style::default().add_modifier(Modifier::DIM),
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Job · Esc zurück "),
        )
        .render(area, buf);
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(format!(
            " Job · {} · {} · {} · Esc zurück ",
            detail.row.name, detail.row.state, detail.row.runtime
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let dim = style::dim_style(theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::styled(
        format!("{} · Fortschritt: {}", detail.row.id, detail.row.progress),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        detail
            .header
            .iter()
            .map(|line| Line::styled(line.clone(), dim)),
    );
    for (label, stream) in [("stdout", &detail.stdout), ("stderr", &detail.stderr)] {
        lines.push(Line::styled(
            format!("── {label} ({} Zeilen) ──", stream.len()),
            dim,
        ));
        let line_style = if label == "stderr" {
            style::warning_style(theme)
        } else {
            Style::default()
        };
        lines.extend(
            stream
                .iter()
                .map(|line| Line::styled(line.clone(), line_style)),
        );
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total = paragraph.line_count(inner.width);
    let viewport = usize::from(inner.height);
    let back = scroll.sync_layout(total, viewport, inner.width);
    let top = total.saturating_sub(viewport).saturating_sub(back);
    paragraph
        .scroll((u16::try_from(top).unwrap_or(u16::MAX), 0))
        .render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: &'static str, active: bool, failed: bool) -> JobRow {
        JobRow {
            id: "job-20260924-101112-001".to_owned(),
            name: name.to_owned(),
            state,
            progress: "ninja 120/900 13%".to_owned(),
            runtime: "12m03s".to_owned(),
            active,
            failed,
        }
    }

    /// Plan R9, Teil F: jede Zeile der Jobs-Gruppe ist genau `width` breit
    /// (oder schmaler) und bricht nie um — auch bei sehr langen Namen und
    /// sehr schmalen Panels.
    #[test]
    fn job_lines_never_exceed_the_panel_width() {
        let long = row(&"ladybird build ".repeat(10), "läuft", true, false);
        let finished = [
            row("restore", "fertig", false, false),
            row("tests", "fehlgeschlagen", false, true),
        ];
        let refs: Vec<&JobRow> = finished.iter().collect();
        for width in [0, 1, 5, 12, 24, 40, 80] {
            for selected in [false, true] {
                for line in [
                    job_line(&long, selected, width, Theme::Dark),
                    job_line(&finished[1], selected, width, Theme::Dark),
                    finished_jobs_line(&refs, false, selected, width, Theme::Dark),
                    group_header_line(1, 3, width, Theme::Dark),
                ] {
                    assert!(line.width() <= width, "{width}: {line:?}");
                }
            }
        }
        let wide = job_line(&long, false, 80, Theme::Dark);
        let text: String = wide
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("läuft 12m03s"), "{text}");
        assert!(text.contains('…'), "langer Name wird gekürzt: {text}");
    }
}

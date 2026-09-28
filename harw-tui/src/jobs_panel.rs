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

use harw_tool_job::model::JobEndReason;
use harw_tool_job::{JobState, JobStatus};
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
    /// Grund des Fehlschlags (R18 F9), nur bei `failed`; siehe
    /// [`failure_reason`].
    pub reason: Option<String>,
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
            failed: matches!(state, JobState::Failed | JobState::Unknown),
            reason: failure_reason(status),
        }
    }
}

/// Höchstlänge (Zeichen) der zitierten letzten Ausgabezeile im Grund.
const REASON_LINE_CHARS: usize = 120;

/// Grund eines fehlgeschlagenen Jobs in Klartext (R18 F9).
///
/// # Description
/// Vorher zeigte die Jobs-Gruppe bei 11 von 28 fehlgeschlagenen Jobs keinen
/// Grund. Reihenfolge wie [`harw_tool_job::JobMeta::end_reason`]:
/// Startfehler (erste Zeile), CPU-Budget, Signal, Exit-Code, sonst
/// „Ausgang unbekannt“. Bei einem regulären Ende (Exit-Code, Signal, Budget)
/// folgt die letzte nicht-leere Ausgabezeile, meist die Fehlermeldung.
///
/// # Returns
/// `None` für Jobs, die nicht fehlgeschlagen sind (auch gestoppte Jobs).
#[must_use]
pub(crate) fn failure_reason(status: &JobStatus) -> Option<String> {
    let meta = &status.meta;
    if !matches!(meta.state, JobState::Failed | JobState::Unknown) {
        return None;
    }
    if let Some(error) = &meta.launch_error {
        let first = error.lines().next().unwrap_or_default().trim();
        return Some(sanitize_inline(&format!("Startfehler: {first}")));
    }
    let base = match meta.end_reason() {
        Some(JobEndReason::Timeout) => "CPU-Budget erschöpft".to_owned(),
        Some(JobEndReason::Signal) => format!(
            "Signal {}",
            meta.signal
                .map_or_else(|| "?".to_owned(), |signal| signal.to_string())
        ),
        Some(JobEndReason::Exited) => format!(
            "Exit-Code {}",
            meta.exit_code
                .map_or_else(|| "?".to_owned(), |code| code.to_string())
        ),
        Some(JobEndReason::Stopped) => "gestoppt".to_owned(),
        Some(JobEndReason::LaunchError) => "Startfehler".to_owned(),
        Some(JobEndReason::Unknown) => {
            return Some("Ausgang unbekannt (Job einer früheren Sitzung)".to_owned());
        }
        None => return Some("Ausgang unbekannt".to_owned()),
    };
    let last_line = status
        .last_lines
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty());
    let text = match last_line {
        Some(line) => {
            let clipped: String = if line.chars().count() > REASON_LINE_CHARS {
                let mut head: String = line.chars().take(REASON_LINE_CHARS - 1).collect();
                head.push('…');
                head
            } else {
                line.to_owned()
            };
            format!("{base}: {clipped}")
        }
        None => base,
    };
    Some(sanitize_inline(&text))
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
/// Links Marker, Symbol, Name und — soweit Platz ist — Fortschritt bzw.
/// bei einem fehlgeschlagenen Job sein Grund ([`failure_reason`]);
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
    let right = if name_width.min(8) + display_width(&full_right) <= available {
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
    // R18 F9: ein fehlgeschlagener Job zeigt seinen Grund statt des
    // (dann bedeutungslosen) Fortschritts.
    let (detail, detail_style) = match (&row.reason, row.failed) {
        (Some(reason), true) => (reason.as_str(), style::error_style(theme)),
        _ => (row.progress.as_str(), style::dim_style(theme)),
    };
    let progress = if detail != "—" && !detail.is_empty() && progress_room >= 6 {
        fit_width(&format!(" · {detail}"), progress_room)
    } else {
        String::new()
    };
    let used = name_width + display_width(&progress) + right_width;
    let pad = " ".repeat(available.saturating_sub(used));
    let line = Line::from(vec![
        Span::raw(marker.to_owned()),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(progress, detail_style),
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
    /// Kopfzeilen: startender Werkzeugaufruf zuerst, dann Grund eines
    /// Fehlschlags, Ort, Zähler, Logs; der Befehl zuletzt (R18 D-D).
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
        // R18: zuerst der Werkzeugaufruf, der den Job startete (Werkzeug,
        // `call_id`, Agent), dann ggf. der Grund des Fehlschlags; der Befehl
        // rückt ans Ende.
        let agent = meta.owner_agent.as_deref().unwrap_or(&meta.owner.session);
        let origin = match (&meta.origin_tool, &meta.origin_call_id) {
            (Some(tool), Some(call_id)) => format!(
                "Aufruf: {} · {} · Agent {}",
                sanitize_inline(tool),
                sanitize_inline(call_id),
                sanitize_inline(agent)
            ),
            (Some(tool), None) => format!(
                "Aufruf: {} · Agent {}",
                sanitize_inline(tool),
                sanitize_inline(agent)
            ),
            (None, Some(call_id)) => format!(
                "Aufruf: {} · Agent {}",
                sanitize_inline(call_id),
                sanitize_inline(agent)
            ),
            (None, None) => format!("Aufruf: unbekannt · Agent {}", sanitize_inline(agent)),
        };
        let mut header = vec![origin];
        if let Some(reason) = failure_reason(status) {
            header.push(format!("Grund: {reason}"));
        }
        header.extend([
            format!(
                "Ort: {} · PID: {}",
                if meta.executed_on_host {
                    "Host"
                } else {
                    "Sandbox"
                },
                meta.pid
                    .map_or_else(|| "—".to_owned(), |pid| pid.to_string()),
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
        ]);
        if let Some(error) = &meta.launch_error {
            header.push(format!("Startfehler: {}", sanitize_inline(error)));
        }
        header.push(format!("Befehl: {}", sanitize_inline(&meta.command)));
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
    use crate::test_support::{TestResult, ctx};

    fn row(name: &str, state: &'static str, active: bool, failed: bool) -> JobRow {
        JobRow {
            id: "job-20260924-101112-001".to_owned(),
            name: name.to_owned(),
            state,
            progress: "ninja 120/900 13%".to_owned(),
            runtime: "12m03s".to_owned(),
            active,
            failed,
            reason: None,
        }
    }

    /// Baut einen Job-Zustand aus einer `meta.json`-Form (ohne Logdateien).
    fn status_of(meta_json: &str, last_lines: &[&str]) -> TestResult<JobStatus> {
        let meta: harw_tool_job::JobMeta =
            serde_json::from_str(meta_json).map_err(ctx("meta.json"))?;
        Ok(JobStatus {
            meta,
            runtime_secs: Some(3),
            stdout_lines: 0,
            stderr_lines: 0,
            last_lines: last_lines.iter().map(|line| (*line).to_owned()).collect(),
            log_dir: std::path::PathBuf::from("/nonexistent/harw-job-test"),
        })
    }

    fn meta_json(state: &str, extra: &str, owner: &str) -> String {
        format!(
            r#"{{"version":2,"job_id":"job-1","name":"tests","command":"cargo test --workspace",
               "state":"{state}",{extra}"harw_instance":"harw-t",
               "owner":{owner},"created_at":"2026-01-01T00:00:00Z",
               "notify_every_secs":60}}"#
        )
    }

    /// TUI-05 (R18 F9): ein fehlgeschlagener Job zeigt seinen Grund in der
    /// Panelzeile; ein erfolgreicher nicht.
    #[test]
    fn failed_job_line_shows_its_reason() -> TestResult {
        let failed = status_of(
            &meta_json("failed", r#""exit_code":101,"#, r#"{"session":"uia"}"#),
            &["   Compiling x", "error: could not compile `x`", "  "],
        )?;
        let row = JobRow::from_status(&failed);
        assert_eq!(
            row.reason.as_deref(),
            Some("Exit-Code 101: error: could not compile `x`")
        );
        let line = job_line(&row, false, 120, Theme::Dark);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            text.contains("Exit-Code 101: error: could not compile"),
            "{text}"
        );
        assert!(line.width() <= 120, "{text}");

        let launch = status_of(
            &meta_json(
                "failed",
                r#""launch_error":"bwrap: No such file\nmehr","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        assert_eq!(
            failure_reason(&launch).as_deref(),
            Some("Startfehler: bwrap: No such file")
        );

        let signal = status_of(
            &meta_json("failed", r#""signal":9,"#, r#"{"session":"uia"}"#),
            &[],
        )?;
        assert_eq!(failure_reason(&signal).as_deref(), Some("Signal 9"));

        let ok = status_of(
            &meta_json("succeeded", r#""exit_code":0,"#, r#"{"session":"uia"}"#),
            &["fertig"],
        )?;
        assert_eq!(failure_reason(&ok), None);
        assert_eq!(JobRow::from_status(&ok).reason, None);
        Ok(())
    }

    /// TUI-07/R18 D-D: die Detailansicht nennt zuerst den startenden
    /// Werkzeugaufruf (`call_id`), dann den Grund; der Befehl steht zuletzt.
    #[test]
    fn job_detail_lists_origin_call_first_and_command_last() -> TestResult {
        let status = status_of(
            &meta_json(
                "failed",
                r#""exit_code":1,"origin_call_id":"call-42","origin_tool":"job.start","#,
                r#"{"session":"uia"}"#,
            ),
            &["boom"],
        )?;
        let detail = JobDetail::from_status(&status);
        assert_eq!(
            detail.header.first().map(String::as_str),
            Some("Aufruf: job.start · call-42 · Agent uia")
        );
        assert_eq!(
            detail.header.get(1).map(String::as_str),
            Some("Grund: Exit-Code 1: boom")
        );
        assert_eq!(
            detail.header.last().map(String::as_str),
            Some("Befehl: cargo test --workspace")
        );

        let legacy = status_of(&meta_json("running", "", r#"{"session":"worker"}"#), &[])?;
        let detail = JobDetail::from_status(&legacy);
        assert_eq!(
            detail.header.first().map(String::as_str),
            Some("Aufruf: unbekannt · Agent worker")
        );
        assert!(
            !detail.header.iter().any(|line| line.starts_with("Grund:")),
            "{:?}",
            detail.header
        );
        Ok(())
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

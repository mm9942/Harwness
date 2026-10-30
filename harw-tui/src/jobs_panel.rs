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
use harw_tool_job::{JobState, JobStatus, ManagedJobOrigin};
use jiff::Timestamp;
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
    /// PL-97 C4: typed state of the job; the display label comes from
    /// [`Self::state_label`] (never compare localized labels).
    pub state: JobState,
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
    /// Semantic owned reason for current-locale presentation.
    pub reason_source: Option<JobFailure>,
    /// W20-A: typed provenance (`AgentChild`, `Tool`, `Operator`) from
    /// [`harw_tool_job::JobMeta::resolved_origin`]; `None` = unknown
    /// provenance (a legacy record without origin fields), never guessed
    /// from the name. Provenance only: it groups the Finished / Failed
    /// window and links job-backed child agents, it never feeds an
    /// authority decision.
    pub origin: Option<ManagedJobOrigin>,
    /// W20-A/E4: the job's **dedicated** cgroup (`…/harw-job-<attempt>`)
    /// from its persisted process identity ([`JobStatus::job_cgroup_path`]).
    /// The path is relative to the cgroup-v2 hierarchy (`0::` form of
    /// `/proc/<pid>/cgroup`), not under `/sys/fs/cgroup`. `None` when unknown
    /// **or** when the job runs in a shared cgroup (no delegated cgroup root:
    /// harw's own or the session's, e.g. `/user.slice`), so attribution by
    /// this path fails closed. Sanitized; not rendered, only correlated with
    /// DoD findings (PL-97 E4, the `⛨ n` badge of the Jobs window).
    pub cgroup: Option<String>,
    /// PL-97 C4: end of the job (`meta.json` `ended_at`), if known; orders
    /// the constituents of the Finished / Failed window (newest first).
    pub ended_at: Option<Timestamp>,
    /// PL-97 C4 (W20-A): how a job-backed child agent's job relates to the
    /// agent run the monitor knows. [`JobRow::from_status`] only knows the
    /// provenance; [`crate::agent_monitor::AgentMonitor`] keeps it current
    /// against its agent runs (jobs snapshot and agent events).
    pub child_link: ChildRunLink,
}

/// PL-97 C4 (W20-A): relation of a job to the child agent run it backs.
///
/// A job tagged [`ManagedJobOrigin::AgentChild`] runs a child agent; the
/// agent run owns its rows and counts, so the job never shows up a second
/// time. Unknown runs stay `Pending`: never promoted to a category of their
/// own, never guessed from the job name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChildRunLink {
    /// Not a job-backed child agent.
    #[default]
    NotAChild,
    /// A job-backed child whose agent run the monitor does not know (yet).
    Pending,
    /// The agent run is known and still active.
    RunActive,
    /// The agent run is known and finished.
    RunEnded,
}

impl JobRow {
    /// Baut die Zeile aus dem Zustand der Verwaltung.
    #[must_use]
    pub(crate) fn from_status(status: &JobStatus) -> Self {
        let state = status.meta.state;
        let origin = status.meta.resolved_origin().map(sanitize_origin);
        // Die Verknüpfung mit dem Agentenlauf setzt der Monitor; bis dahin
        // gilt ein Kind-Job als „noch nicht zugeordnet“.
        let child_link = if matches!(origin, Some(ManagedJobOrigin::AgentChild { .. })) {
            ChildRunLink::Pending
        } else {
            ChildRunLink::NotAChild
        };
        Self {
            id: status.meta.job_id.to_string(),
            name: sanitize_inline(&status.meta.name),
            state,
            progress: sanitize_inline(&harw_ops::jobs::progress_label(status)),
            runtime: harw_ops::jobs::runtime_label(status),
            active: !state.is_terminal(),
            failed: matches!(state, JobState::Failed | JobState::Unknown),
            reason: failure_reason(status),
            reason_source: JobFailure::from_status(status),
            origin,
            cgroup: status.job_cgroup_path().map(sanitize_inline),
            ended_at: status.meta.ended_at,
            child_link,
        }
    }

    /// Kurzer, lokalisierter Zustand (`läuft`, `fertig`, …) zur Anzeige.
    #[must_use]
    pub(crate) fn state_label(&self) -> &'static str {
        job_state_label(self.state)
    }

    /// Session id of the child agent run this job backs, if it is a
    /// job-backed child agent.
    #[must_use]
    pub(crate) fn child_session_id(&self) -> Option<&str> {
        match &self.origin {
            Some(ManagedJobOrigin::AgentChild { child_session_id }) => Some(child_session_id),
            _ => None,
        }
    }

    /// PL-97 C4: `true` when the job has its own row in the job lists (Jobs
    /// window, jobs group of the agent panel).
    ///
    /// A job-backed child agent is represented by its agent run only while
    /// job and run agree: both active (the Active Agents row) or both
    /// finished (the run's Finished / Failed count). A job whose run is
    /// unknown, a job that outlives its finished run (the process is still
    /// active) and a job that ended while its run still shows as active (the
    /// run's terminal event is late or was lost) keep a job row, so they stay
    /// visible -- and the running ones stoppable.
    #[must_use]
    pub(crate) fn is_listed(&self) -> bool {
        match self.child_link {
            ChildRunLink::NotAChild | ChildRunLink::Pending => true,
            ChildRunLink::RunActive => !self.active,
            ChildRunLink::RunEnded => self.active,
        }
    }
}

/// Macht die Texte einer Herkunft terminal-sicher (sie stammen aus
/// `meta.json`, wie Name und Befehl); saubere Kennungen bleiben unverändert.
fn sanitize_origin(origin: ManagedJobOrigin) -> ManagedJobOrigin {
    match origin {
        ManagedJobOrigin::AgentChild { child_session_id } => ManagedJobOrigin::AgentChild {
            child_session_id: sanitize_inline(&child_session_id),
        },
        ManagedJobOrigin::Tool { tool, call_id } => ManagedJobOrigin::Tool {
            tool: sanitize_inline(&tool),
            call_id: call_id.as_deref().map(sanitize_inline),
        },
        ManagedJobOrigin::Operator => ManagedJobOrigin::Operator,
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
    JobFailure::from_status(status).map(|reason| reason.render())
}

/// Display labels only; protocol state tokens stay in harw-ops.
pub(crate) fn job_state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => crate::i18n::tr("mission.queued", "queued"),
        JobState::Running => crate::i18n::tr("mission.running", "running"),
        JobState::Succeeded => crate::i18n::tr("mission.succeeded", "succeeded"),
        JobState::Failed => crate::i18n::tr("agents.failed", "failed"),
        JobState::Stopped => crate::i18n::tr("mission.stopped", "stopped"),
        JobState::Detached => crate::i18n::tr("mission.detached", "detached"),
        JobState::Unknown => crate::i18n::tr("mission.unknown_state", "unknown"),
    }
}

/// Captured semantic failure data. Never infer a key from an arbitrary reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobFailure {
    kind: FailureKind,
    tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FailureKind {
    Launch(String),
    Timeout,
    Signal(String),
    Exit(String),
    Stopped,
    LaunchUnknown,
    UnknownPrevious,
    Unknown,
}

impl JobFailure {
    fn from_status(status: &JobStatus) -> Option<Self> {
        let meta = &status.meta;
        if !matches!(meta.state, JobState::Failed | JobState::Unknown) {
            return None;
        }
        let (kind, with_tail) = if let Some(error) = &meta.launch_error {
            (
                FailureKind::Launch(sanitize_inline(
                    error.lines().next().unwrap_or_default().trim(),
                )),
                false,
            )
        } else {
            match meta.end_reason() {
                Some(JobEndReason::Timeout) => (FailureKind::Timeout, true),
                Some(JobEndReason::Signal) => (
                    FailureKind::Signal(
                        meta.signal
                            .map_or_else(|| "?".to_owned(), |n| n.to_string()),
                    ),
                    true,
                ),
                Some(JobEndReason::Exited) => (
                    FailureKind::Exit(
                        meta.exit_code
                            .map_or_else(|| "?".to_owned(), |n| n.to_string()),
                    ),
                    true,
                ),
                Some(JobEndReason::Stopped) => (FailureKind::Stopped, true),
                Some(JobEndReason::LaunchError) => (FailureKind::LaunchUnknown, true),
                Some(JobEndReason::Unknown) => (FailureKind::UnknownPrevious, false),
                None => (FailureKind::Unknown, false),
            }
        };
        let tail = with_tail
            .then(|| {
                status
                    .last_lines
                    .iter()
                    .rev()
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty())
                    .map(|line| {
                        sanitize_inline(&crate::history_cell::truncate_chars(
                            line,
                            REASON_LINE_CHARS,
                        ))
                    })
            })
            .flatten();
        Some(Self { kind, tail })
    }

    pub(crate) fn render(&self) -> String {
        use crate::i18n::{tr, tr_args};
        let base = match &self.kind {
            FailureKind::Launch(first) => tr_args(
                "mission.launch_error",
                "Launch error: {first}",
                &[("first", first)],
            ),
            FailureKind::Timeout => tr("mission.timeout", "CPU budget exhausted").to_owned(),
            FailureKind::Signal(value) => {
                tr_args("mission.signal", "Signal {p0}", &[("p0", value)])
            }
            FailureKind::Exit(value) => tr_args("mission.exit", "Exit code {p0}", &[("p0", value)]),
            FailureKind::Stopped => tr("mission.stopped", "stopped").to_owned(),
            FailureKind::LaunchUnknown => tr("mission.launch_failed", "Launch error").to_owned(),
            FailureKind::UnknownPrevious => tr(
                "mission.unknown_previous",
                "Unknown outcome (job from an earlier session)",
            )
            .to_owned(),
            FailureKind::Unknown => tr("mission.unknown_outcome", "Unknown outcome").to_owned(),
        };
        match &self.tail {
            Some(tail) => format!("{base}: {tail}"),
            None => base,
        }
    }
}

impl JobRow {
    pub(crate) fn reason_text(&self) -> Option<String> {
        self.reason_source
            .as_ref()
            .map(JobFailure::render)
            .or_else(|| self.reason.clone())
    }
}

/// PL-97 E4: die Zeile korreliert über ihre **eigene** Job-cgroup
/// ([`JobRow::cgroup`]); ohne sie wird ihr nichts zugeordnet.
impl harw_ops::dod::JobCgroup for JobRow {
    fn job_id(&self) -> &str {
        &self.id
    }

    fn cgroup_path(&self) -> Option<&str> {
        self.cgroup.as_deref()
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
/// Links Marker, Symbol, Name und — soweit Platz ist — Fortschritt bzw.
/// bei einem fehlgeschlagenen Job sein Grund ([`failure_reason`]);
/// rechtsbündig Zustand und Laufzeit. Reicht der Platz nicht, entfallen
/// zuerst Fortschritt, dann Laufzeit, dann Zustand; der Name wird zuletzt
/// mit `…` gekürzt.
#[must_use]
pub(crate) fn job_line(row: &JobRow, selected: bool, width: usize, theme: Theme) -> Line<'static> {
    job_line_with_badge(row, selected, width, theme, None)
}

/// Wie [`job_line`], mit der DoD-Marke `⛨ n` vor dem Namen (PL-97 E4):
/// `badge` ist die Zahl der dem Job zugeordneten high/critical-Befunde;
/// `None` oder `0` blendet sie aus.
#[must_use]
pub(crate) fn job_line_with_badge(
    row: &JobRow,
    selected: bool,
    width: usize,
    theme: Theme,
    badge: Option<u32>,
) -> Line<'static> {
    let marker = if selected { "▸" } else { " " };
    let (glyph, glyph_style) = job_glyph(row, theme);
    let badge = badge
        .filter(|count| *count > 0)
        .map(|count| format!("⛨ {count} "))
        .unwrap_or_default();
    let prefix = format!("{marker}{glyph} {badge}");
    let available = width.saturating_sub(display_width(&prefix));
    let name_width = display_width(&row.name);
    let full_right = format!(" {} {}", row.state_label(), row.runtime);
    let short_right = format!(" {}", row.state_label());
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
    let reason = row.reason_text();
    let (detail, detail_style) = match (reason.as_ref(), row.failed) {
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
        Span::styled(
            badge,
            Style::default()
                .fg(style::error_color(theme))
                .add_modifier(Modifier::BOLD),
        ),
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
    let head = crate::i18n::tr_args(
        "mission.finished_jobs",
        "{marker}✓ {p0} jobs finished {arrow}",
        &[
            ("marker", &(marker).to_string()),
            ("p0", &(finished.len()).to_string()),
            ("arrow", &(arrow).to_string()),
        ],
    );
    let tail = if failed > 0 {
        crate::i18n::tr_args(
            "mission.failed_jobs",
            " · {failed} failed",
            &[("failed", &(failed).to_string())],
        )
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
        crate::i18n::tr_args(
            "mission.jobs_group",
            "── Jobs · {active} active ",
            &[("active", &(active).to_string())],
        )
    } else {
        crate::i18n::tr_args(
            "mission.jobs_group_total",
            "── Jobs · {total} ",
            &[("total", &(total).to_string())],
        )
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
    header_source: JobHeaderData,
    /// Letzte stdout-Zeilen.
    pub stdout: Vec<String>,
    /// Letzte stderr-Zeilen.
    pub stderr: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JobHeaderData {
    origin_tool: Option<String>,
    origin_call_id: Option<String>,
    agent: String,
    reason: Option<JobFailure>,
    executed_on_host: bool,
    pid: Option<u32>,
    warnings: u64,
    errors: u64,
    lines: u64,
    exit_code: Option<i32>,
    log_dir: String,
    launch_error: Option<String>,
    command: String,
}

impl JobHeaderData {
    fn from_status(status: &JobStatus) -> Self {
        let meta = &status.meta;
        Self {
            origin_tool: meta.origin_tool.as_deref().map(sanitize_inline),
            origin_call_id: meta.origin_call_id.as_deref().map(sanitize_inline),
            agent: sanitize_inline(meta.owner_agent.as_deref().unwrap_or(&meta.owner.session)),
            reason: JobFailure::from_status(status),
            executed_on_host: meta.executed_on_host,
            pid: meta.pid,
            warnings: meta.warnings,
            errors: meta.errors,
            lines: status.stdout_lines + status.stderr_lines,
            exit_code: meta.exit_code,
            log_dir: status.log_dir.display().to_string(),
            launch_error: meta.launch_error.as_deref().map(sanitize_inline),
            command: sanitize_inline(&meta.command),
        }
    }

    fn render(&self) -> Vec<String> {
        let meta = self;
        let agent = &self.agent;
        let origin = match (&meta.origin_tool, &meta.origin_call_id) {
            (Some(tool), Some(call_id)) => crate::i18n::tr_args(
                "mission.origin_full",
                "Call: {p0} · {p1} · Agent {p2}",
                &[
                    ("p0", &(sanitize_inline(tool)).to_string()),
                    ("p1", &(sanitize_inline(call_id)).to_string()),
                    ("p2", &(sanitize_inline(agent)).to_string()),
                ],
            ),
            (Some(tool), None) => crate::i18n::tr_args(
                "mission.origin",
                "Call: {p0} · Agent {p1}",
                &[
                    ("p0", &(sanitize_inline(tool)).to_string()),
                    ("p1", &(sanitize_inline(agent)).to_string()),
                ],
            ),
            (None, Some(call_id)) => crate::i18n::tr_args(
                "mission.origin",
                "Call: {p0} · Agent {p1}",
                &[
                    ("p0", &(sanitize_inline(call_id)).to_string()),
                    ("p1", &(sanitize_inline(agent)).to_string()),
                ],
            ),
            (None, None) => crate::i18n::tr_args(
                "mission.origin_unknown",
                "Call: unknown · Agent {p0}",
                &[("p0", &(sanitize_inline(agent)).to_string())],
            ),
        };
        let mut header = vec![origin];
        if let Some(reason) = self.reason.as_ref().map(JobFailure::render) {
            header.push(crate::i18n::tr_args(
                "mission.reason",
                "Reason: {reason}",
                &[("reason", &(reason).to_string())],
            ));
        }
        header.extend([
            crate::i18n::tr_args(
                "mission.location",
                "Location: {p0} · PID: {p1}",
                &[
                    (
                        "p0",
                        &(if meta.executed_on_host {
                            crate::i18n::tr("mission.host", "Host")
                        } else {
                            crate::i18n::tr("mission.sandbox", "Sandbox")
                        })
                        .to_string(),
                    ),
                    (
                        "p1",
                        &(meta
                            .pid
                            .map_or_else(|| "—".to_owned(), |pid| pid.to_string()))
                        .to_string(),
                    ),
                ],
            ),
            crate::i18n::tr_args(
                "mission.counts",
                "{p0} warnings · {p1} errors · {p2} lines{p3}",
                &[
                    ("p0", &(meta.warnings).to_string()),
                    ("p1", &(meta.errors).to_string()),
                    ("p2", &(self.lines).to_string()),
                    (
                        "p3",
                        &(meta
                            .exit_code
                            .map(|code| {
                                crate::i18n::tr_args(
                                    "mission.exit_suffix",
                                    " · Exit code {code}",
                                    &[("code", &(code).to_string())],
                                )
                            })
                            .unwrap_or_default())
                        .to_string(),
                    ),
                ],
            ),
            crate::i18n::tr_args(
                "mission.logs",
                "Logs: {p0}",
                &[("p0", &(self.log_dir).to_string())],
            ),
        ]);
        if let Some(error) = &meta.launch_error {
            header.push(crate::i18n::tr_args(
                "mission.launch_error_full",
                "Launch error: {p0}",
                &[("p0", &(sanitize_inline(error)).to_string())],
            ));
        }
        header.push(crate::i18n::tr_args(
            "mission.command",
            "Command: {p0}",
            &[("p0", &(sanitize_inline(&meta.command)).to_string())],
        ));
        header
    }
}

impl JobDetail {
    /// Baut die Detailansicht aus dem Zustand und den Logdateien.
    #[must_use]
    pub(crate) fn from_status(status: &JobStatus) -> Self {
        let header_source = JobHeaderData::from_status(status);
        let header = header_source.render();
        let tail = |file: &str| {
            harw_tool_job::logs::tail_of_file(&status.log_dir.join(file), DETAIL_LOG_LINES)
                .into_iter()
                .map(|line| sanitize_inline(&line))
                .collect::<Vec<_>>()
        };
        Self {
            row: JobRow::from_status(status),
            header,
            header_source,
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
            crate::i18n::tr("mission.not_found", "Job not found."),
            Style::default().add_modifier(Modifier::DIM),
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(crate::i18n::tr("mission.detail_back", " Job · Esc back ")),
        )
        .render(area, buf);
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(crate::i18n::tr_args(
            "mission.detail_title",
            " Job · {p0} · {p1} · {p2} · Esc back ",
            &[
                ("p0", &(detail.row.name).to_string()),
                ("p1", &(detail.row.state_label()).to_string()),
                ("p2", &(detail.row.runtime).to_string()),
            ],
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let dim = style::dim_style(theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::styled(
        crate::i18n::tr_args(
            "mission.progress",
            "{p0} · Progress: {p1}",
            &[
                ("p0", &(detail.row.id).to_string()),
                ("p1", &(detail.row.progress).to_string()),
            ],
        ),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    lines.extend(
        detail
            .header_source
            .render()
            .iter()
            .map(|line| Line::styled(line.clone(), dim)),
    );
    for (label, stream) in [("stdout", &detail.stdout), ("stderr", &detail.stderr)] {
        lines.push(Line::styled(
            crate::i18n::tr_args(
                "mission.stream_lines",
                "── {label} ({p0} lines) ──",
                &[
                    ("label", &(label).to_string()),
                    ("p0", &(stream.len()).to_string()),
                ],
            ),
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

    fn row(name: &str, state: JobState, active: bool, failed: bool) -> JobRow {
        JobRow {
            id: "job-20260924-101112-001".to_owned(),
            name: name.to_owned(),
            state,
            progress: "ninja 120/900 13%".to_owned(),
            runtime: "12m03s".to_owned(),
            active,
            failed,
            reason: None,
            reason_source: None,
            origin: None,
            cgroup: None,
            ended_at: None,
            child_link: ChildRunLink::NotAChild,
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
            Some("Exit code 101: error: could not compile `x`")
        );
        let line = job_line(&row, false, 120, Theme::Dark);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            text.contains("Exit code 101: error: could not compile"),
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
            Some("Launch error: bwrap: No such file")
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
            Some("Call: job.start · call-42 · Agent uia")
        );
        assert_eq!(
            detail.header.get(1).map(String::as_str),
            Some("Reason: Exit code 1: boom")
        );
        assert_eq!(
            detail.header.last().map(String::as_str),
            Some("Command: cargo test --workspace")
        );

        let legacy = status_of(&meta_json("running", "", r#"{"session":"worker"}"#), &[])?;
        let detail = JobDetail::from_status(&legacy);
        assert_eq!(
            detail.header.first().map(String::as_str),
            Some("Call: unknown · Agent worker")
        );
        assert!(
            !detail.header.iter().any(|line| line.starts_with("Reason:")),
            "{:?}",
            detail.header
        );
        Ok(())
    }

    /// W20-A: `JobRow` übernimmt die typisierte Herkunft — alte R18-Felder
    /// werden `Tool`, ohne Herkunftsfeld bleibt sie unbekannt (nie aus dem
    /// Namen geraten), `AgentChild`/`Operator` kommen unverändert an — und
    /// den cgroup-Pfad aus der gespeicherten Prozessidentität, aber nur eine
    /// eigene Job-cgroup (`harw-job-…`), nie eine geteilte; Steuerzeichen
    /// kommen nur markiert an.
    #[test]
    fn job_row_maps_origin_and_cgroup() -> TestResult {
        let legacy_tool = status_of(
            &meta_json(
                "succeeded",
                r#""exit_code":0,"origin_call_id":"call-42","origin_tool":"job.start","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&legacy_tool);
        assert_eq!(
            row.origin,
            Some(ManagedJobOrigin::Tool {
                tool: "job.start".to_owned(),
                call_id: Some("call-42".to_owned()),
            })
        );
        assert_eq!(row.cgroup, None);

        let unknown = status_of(
            &meta_json("running", "", r#"{"session":"uia"}"#)
                .replace(r#""name":"tests""#, r#""name":"agent-child-explorer""#),
            &[],
        )?;
        assert_eq!(JobRow::from_status(&unknown).origin, None);

        let child = status_of(
            &meta_json(
                "running",
                r#""origin":{"kind":"agent_child","child_session_id":"child-7"},
                   "identity":{"start_ticks":5,"cgroup_path":"/harw.slice/harw-job-3",
                     "dedicated_cgroup":{"job_id":"job-1","attempt_id":"3",
                       "delegated_root":"/harw.slice"}},"#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&child);
        assert_eq!(
            row.origin,
            Some(ManagedJobOrigin::AgentChild {
                child_session_id: "child-7".to_owned(),
            })
        );
        assert_eq!(row.cgroup.as_deref(), Some("/harw.slice/harw-job-3"));

        let operator = status_of(
            &meta_json(
                "running",
                r#""origin":{"kind":"operator"},"origin_tool":"job.start","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        assert_eq!(
            JobRow::from_status(&operator).origin,
            Some(ManagedJobOrigin::Operator),
            "die typisierte Herkunft gewinnt über die R18-Felder"
        );

        // Eine geteilte cgroup (ohne delegierte Wurzel) ist nie die des Jobs.
        let shared = status_of(
            &meta_json(
                "running",
                r#""origin":{"kind":"agent_child","child_session_id":"child-8"},
                   "identity":{"start_ticks":5,"cgroup_path":"/user.slice"},"#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        assert_eq!(shared.cgroup_path(), Some("/user.slice"));
        assert_eq!(JobRow::from_status(&shared).cgroup, None);

        // Ein passender `harw-job-*`-Name ohne Laufzeit-Nachweis ist kein Beleg.
        let unproven = status_of(
            &meta_json(
                "running",
                r#""identity":{"start_ticks":5,"cgroup_path":"/harw.slice/harw-job-3"},"#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        assert_eq!(unproven.cgroup_path(), Some("/harw.slice/harw-job-3"));
        assert_eq!(JobRow::from_status(&unproven).cgroup, None);

        // Steuerzeichen aus `meta.json` (dort als JSON-Escapes) erreichen die
        // Zeile nur markiert.
        let hostile = status_of(
            &meta_json(
                "running",
                r#""origin":{"kind":"tool","tool":"job.start\u001b[2J"},
                   "identity":{"start_ticks":5,"cgroup_path":"/harw\u0007.slice/harw-job-3",
                     "dedicated_cgroup":{"job_id":"job-1","attempt_id":"3",
                       "delegated_root":"/harw\u0007.slice"}},"#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&hostile);
        assert_eq!(
            row.origin,
            Some(ManagedJobOrigin::Tool {
                tool: "job.start⟨ESC⟩".to_owned(),
                call_id: None,
            })
        );
        assert_eq!(
            row.cgroup.as_deref(),
            Some("/harw⟨U+0007⟩.slice/harw-job-3")
        );
        Ok(())
    }

    /// PL-97 C4: die Zeile trägt den typisierten Zustand (die Anzeige kommt
    /// aus `state_label`), das Ende aus `meta.json` und — für einen
    /// Kind-Agenten-Job — zunächst eine offene Verknüpfung; verborgen ist ein
    /// Kind-Job nur, solange Job und Lauf übereinstimmen (beide aktiv oder
    /// beide beendet).
    #[test]
    fn job_row_carries_typed_state_end_and_child_link() -> TestResult {
        let stopped = status_of(
            &meta_json(
                "stopped",
                r#""ended_at":"2026-01-01T00:00:05Z","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&stopped);
        assert_eq!(row.state, JobState::Stopped);
        assert_eq!(
            row.state_label(),
            crate::jobs_panel::job_state_label(JobState::Stopped)
        );
        assert!(!row.active && !row.failed);
        assert_eq!(
            row.ended_at.map(|at| at.as_second()),
            Some(1_767_225_605),
            "{:?}",
            row.ended_at
        );
        assert_eq!(row.child_link, ChildRunLink::NotAChild);
        assert!(row.is_listed());
        assert_eq!(row.child_session_id(), None);

        let child = status_of(
            &meta_json(
                "running",
                r#""origin":{"kind":"agent_child","child_session_id":"child-7"},"#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let mut row = JobRow::from_status(&child);
        assert_eq!(row.child_link, ChildRunLink::Pending);
        assert_eq!(row.child_session_id(), Some("child-7"));
        assert!(row.is_listed(), "unbekannter Lauf: der Job bleibt sichtbar");
        row.child_link = ChildRunLink::RunActive;
        assert!(!row.is_listed(), "der laufende Agent vertritt den Job");
        row.child_link = ChildRunLink::RunEnded;
        assert!(row.is_listed(), "der Job überlebt seinen Lauf");
        row.active = false;
        assert!(!row.is_listed(), "beendet: der Lauf zählt, nicht der Job");
        // Der Job endet, sein Lauf gilt noch als aktiv (Endereignis verspätet
        // oder verloren): der Job bleibt sichtbar.
        row.child_link = ChildRunLink::RunActive;
        assert!(row.is_listed(), "beendeter Job eines aktiven Laufs");
        Ok(())
    }

    /// Plan R9, Teil F: jede Zeile der Jobs-Gruppe ist genau `width` breit
    /// (oder schmaler) und bricht nie um — auch bei sehr langen Namen und
    /// sehr schmalen Panels.
    #[test]
    fn job_lines_never_exceed_the_panel_width() {
        let long = row(
            &"ladybird build ".repeat(10),
            JobState::Running,
            true,
            false,
        );
        let finished = [
            row("restore", JobState::Succeeded, false, false),
            row("tests", JobState::Failed, false, true),
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
        assert!(text.contains("running 12m03s"), "{text}");
        assert!(text.contains('…'), "langer Name wird gekürzt: {text}");
    }

    #[test]
    fn cached_job_semantics_follow_locale_without_changing_payloads() -> TestResult {
        use crate::i18n::{tr, tr_args, with_language};
        use harw_config::UiLanguage;
        let status = status_of(
            &meta_json(
                "failed",
                r#""exit_code":101,"origin_call_id":"call-raw","origin_tool":"job.start","#,
                r#"{"session":"agent-raw"}"#,
            ),
            &["raw stderr {p0}: विफल 中文 👩🏽‍💻 é"],
        )?;
        let raw = serde_json::to_value(&status.meta).map_err(ctx("meta"))?;
        let detail = with_language(UiLanguage::German, || JobDetail::from_status(&status));
        let cached = detail.header.clone();
        for language in UiLanguage::ALL {
            with_language(language, || {
                let headers = detail.header_source.render();
                assert_eq!(
                    headers[0],
                    tr_args(
                        "mission.origin_full",
                        "Call: {p0} · {p1} · Agent {p2}",
                        &[("p0", "job.start"), ("p1", "call-raw"), ("p2", "agent-raw")]
                    )
                );
                let reason = detail.row.reason_text().unwrap_or_default();
                assert!(reason.starts_with(&tr_args(
                    "mission.exit",
                    "Exit code {p0}",
                    &[("p0", "101")]
                )));
                assert!(reason.ends_with(&sanitize_inline("raw stderr {p0}: विफल 中文 👩🏽‍💻 é")));
                for width in [0, 1, 8, 20, 40, 160] {
                    assert!(job_line(&detail.row, true, width, Theme::Dark).width() <= width);
                    let area = Rect::new(0, 0, u16::try_from(width).unwrap_or(160), 20);
                    let mut buf = Buffer::empty(area);
                    render_job_detail(
                        Some(&detail),
                        area,
                        &mut buf,
                        &ChatScroll::new(),
                        Theme::Dark,
                    );
                    if width == 160 {
                        let shown = crate::test_support::display_buffer_text(&buf);
                        assert!(
                            shown.contains(
                                tr("mission.logs", "Logs: {p0}")
                                    .split('{')
                                    .next()
                                    .unwrap_or_default()
                            )
                        );
                        assert!(shown.contains("cargo test --workspace"));
                        assert!(shown.contains("call-raw"));
                    }
                }
            });
        }
        assert_eq!(
            detail.header, cached,
            "rendering does not rewrite the snapshot"
        );
        assert_eq!(
            serde_json::to_value(&status.meta).map_err(ctx("meta"))?,
            raw
        );
        Ok(())
    }
}

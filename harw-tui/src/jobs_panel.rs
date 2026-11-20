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
    /// PL-100 WP 3.5: the job's cgroup is frozen
    /// ([`harw_tool_job::JobMeta::is_suspended`]): a `Suspended` job of this
    /// session **or** a `Detached` one from an earlier session that stayed
    /// frozen. Only the display label uses it ([`Self::state_label`] shows
    /// `mission.suspended`); `state` stays the typed manager state, so stop
    /// and selection logic are unchanged (THREAT-MODEL B13: a freeze stays
    /// visible).
    pub suspended: bool,
    /// PL-100 WP 3.5: the freeze is only recorded as intent
    /// ([`harw_tool_job::JobMeta::is_suspension_unresolved`]): harw cannot
    /// prove that the cgroup's freeze is its own, so `job.resume` refuses and
    /// the operator must resolve it by hand. [`Self::state_label`] shows
    /// `mission.suspension_unresolved` instead of `mission.suspended`.
    pub suspension_unresolved: bool,
    /// Erkannter Fortschritt (`ninja 120/900 13%`) oder `—`.
    pub progress: String,
    /// Laufzeit (`1m15s`) oder `—`.
    pub runtime: String,
    /// Noch nicht beendet (wartet, läuft, angehalten, abgelöst).
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
            suspended: status.meta.is_suspended(),
            suspension_unresolved: status.meta.is_suspension_unresolved(),
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

    /// Kurzer, lokalisierter Zustand (`läuft`, `fertig`, …) zur Anzeige; ein
    /// eingefrorener Job heißt immer `angehalten`/`suspended`, auch wenn er
    /// abgelöst ist ([`Self::suspended`]).
    #[must_use]
    pub(crate) fn state_label(&self) -> &'static str {
        if self.suspension_unresolved {
            crate::i18n::tr("mission.suspension_unresolved", "suspended (unresolved)")
        } else if self.suspended {
            job_state_label(JobState::Suspended)
        } else {
            job_state_label(self.state)
        }
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
        JobState::Suspended => crate::i18n::tr("mission.suspended", "suspended"),
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

/// PL-100 WP 3.5: Symbol einer Zeile mit ungeklärtem Anhalten
/// ([`JobRow::suspension_unresolved`]); eine Spalte breit (`unicode-width`,
/// East-Asian-Width `N`), im Warnstil.
const UNRESOLVED_GLYPH: &str = "⚠";

/// Symbol und Stil eines Jobs.
///
/// Ein ungeklärtes Anhalten hat ein eigenes Symbol im Warnstil
/// ([`UNRESOLVED_GLYPH`]): so trägt schon das Präfix der Zeile den Zustand,
/// auch wenn rechts kein Platz für ihn bleibt. Alle anderen Jobs behalten
/// `⚙` (aktiv), `✗` (fehlgeschlagen) bzw. `✓`.
fn job_glyph(row: &JobRow, theme: Theme) -> (&'static str, Style) {
    if row.suspension_unresolved {
        (UNRESOLVED_GLYPH, style::warning_style(theme))
    } else if row.active {
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
///
/// Ausnahme (PL-100 WP 3.5): ein ungeklärtes Anhalten
/// ([`JobRow::suspension_unresolved`]) verschwindet nie. Passt
/// `mission.suspension_unresolved` nicht, steht rechts die kompakte Marke
/// `mission.suspension_unresolved_short` (`⏸?`); der Fortschritt entfällt
/// dann (er weicht vor dem Zustand), und der Name wird für die Marke bis auf
/// `…` oder nichts gekürzt. Zusätzlich trägt das Präfix das Warnsymbol `⚠`.
/// Mindestbreiten: das Symbol ab 3 Spalten (Auswahlmarker, Symbol,
/// Leerzeichen), die kompakte Marke ohne DoD-Marke ab 5 Spalten (Präfix 3 +
/// Marke 2); mit DoD-Marke kommt deren Breite (`⛨ n `) hinzu.
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
    let name_reserve = name_width.min(8);
    // `compact`: statt des Labels steht die kompakte Marke (PL-100 WP 3.5).
    let (right, compact) = if name_reserve.saturating_add(display_width(&full_right)) <= available {
        (full_right, false)
    } else if name_reserve.saturating_add(display_width(&short_right)) <= available {
        (short_right, false)
    } else if row.suspension_unresolved {
        // Die kompakte Marke hat Vorrang vor dem Namen.
        (unresolved_marker(available), true)
    } else {
        (String::new(), false)
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
    // Ist der Zustand schon auf die kompakte Marke gekürzt, entfällt der
    // Fortschritt ganz: er weicht vor dem Zustand, sein Platz gehört dem
    // Namen.
    let progress = if !compact && detail != "—" && !detail.is_empty() && progress_room >= 6 {
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

/// PL-100 WP 3.5: rechte Seite einer Zeile mit ungeklärtem Anhalten, wenn
/// `mission.suspension_unresolved` nicht neben den Namen passt.
///
/// # Arguments
/// - `available`: Spalten nach dem Präfix (Auswahlmarker, Symbol, DoD-Marke).
///
/// # Returns
/// Die lokalisierte kompakte Marke `mission.suspension_unresolved_short`
/// mit führendem Leerzeichen, wenn beides passt; sonst ohne Leerzeichen
/// (der Name entfällt dann ganz); passt auch das nicht, leer: dann trägt nur
/// noch das Symbol des Präfixes ([`UNRESOLVED_GLYPH`]) den Zustand.
fn unresolved_marker(available: usize) -> String {
    let short = crate::i18n::tr("mission.suspension_unresolved_short", "⏸?");
    let spaced = format!(" {short}");
    if display_width(&spaced) <= available {
        spaced
    } else if display_width(short) <= available {
        short.to_owned()
    } else {
        String::new()
    }
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
            ("marker", marker),
            ("p0", &(finished.len()).to_string()),
            ("arrow", arrow),
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
                        if meta.executed_on_host {
                            crate::i18n::tr("mission.host", "Host")
                        } else {
                            crate::i18n::tr("mission.sandbox", "Sandbox")
                        },
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

/// PL-100 WP 3.5: Zustandszeilen der Detailansicht.
///
/// # Description
/// Der Rahmentitel nennt den Zustand erst hinter dem Namen, und schmale
/// Breiten schneiden ihn dort ab. Darum steht er ausdrücklich in der Ansicht
/// (`mission.detail_state`, `State: …`): fest über dem scrollenden Körper,
/// soweit er dort hinpasst ([`pin_detail_state`]), sonst im Körper direkt
/// unter der fetten Kennungszeile; beides bricht um. Bei einem ungeklärten
/// Anhalten ([`JobRow::suspension_unresolved`]) stehen Zustand und Hinweis
/// (`mission.suspension_unresolved_hint`: `job.resume` lehnt ab; von Hand
/// klären mit `job.stop` oder erneutem `job.suspend`, nur solange die cgroup
/// nicht eingefroren ist) im Warnstil.
///
/// # Returns
/// Eine Zeile (`State: …`), bei einem ungeklärten Anhalten zwei.
fn detail_state_lines(row: &JobRow, theme: Theme) -> Vec<Line<'static>> {
    let state = crate::i18n::tr_args(
        "mission.detail_state",
        "State: {p0}",
        &[("p0", row.state_label())],
    );
    if !row.suspension_unresolved {
        return vec![Line::raw(state)];
    }
    let warning = style::warning_style(theme);
    vec![
        Line::styled(state, warning.add_modifier(Modifier::BOLD)),
        Line::styled(
            crate::i18n::tr(
                "mission.suspension_unresolved_hint",
                "job.resume refuses. Resolve by hand: job.stop (kills without thawing), or re-issue job.suspend only while the cgroup is not frozen.",
            ),
            warning,
        ),
    ]
}

/// PL-100 WP 3.5: Aufteilung der Zustandszeilen ([`detail_state_lines`])
/// zwischen festem Kopf und scrollendem Körper der Detailansicht.
#[derive(Debug)]
struct PinnedState {
    /// Zeilen fest über dem Körper; sie scrollen nicht mit.
    pinned: Vec<Line<'static>>,
    /// Umgebrochene Höhe von `pinned` (Zeilen).
    rows: u16,
    /// Zeilen, die nicht mehr in den Kopf passen; sie stehen im Körper
    /// direkt unter der fetten Kennungszeile und sind per Scrollen
    /// erreichbar.
    rest: Vec<Line<'static>>,
}

/// PL-100 WP 3.5 (Review 4505485685): hält den Zustand beim Öffnen sichtbar.
///
/// # Description
/// Die Detailansicht öffnet am Log-Ende (sie folgt neuem Inhalt); eine
/// Zustandszeile im Körper stünde bei schmalen Breiten erst nach `Home`/`g`
/// im Bild. Darum steht der längste Anfang von `lines`, dessen umgebrochene
/// Höhe in die obere Hälfte passt (`height - height / 2` Zeilen, also
/// aufgerundet, höchstens aber `height - 1`), fest über dem Körper. So bleibt
/// dem scrollenden Körper (Kennung, Kopf, Log) immer mindestens eine Zeile,
/// auch in der einzeiligen Innenfläche des Agentenbereichs (`AGENTS_MIN_ROWS`
/// in `panes.rs`). Der Rest folgt im Körper, etwa der lange Hinweis eines
/// ungeklärten Anhaltens im Hochformat. Passt nicht einmal die erste Zeile,
/// bleibt alles im Körper; bei einem ungeklärten Anhalten trägt dann der
/// Rahmentitel vorn das Warnsymbol ([`UNRESOLVED_GLYPH`]), sonst nennt den
/// Zustand nur der (womöglich hinten abgeschnittene) Titel.
///
/// # Arguments
/// - `lines`: die Zustandszeilen, wichtigste zuerst.
/// - `width`, `height`: Innenmaße der Ansicht (ohne Rahmen).
///
/// # Returns
/// Feste Zeilen samt umgebrochener Höhe und die Zeilen für den Körper; bei
/// `height <= 1` steht nichts fest.
fn pin_detail_state(lines: Vec<Line<'static>>, width: u16, height: u16) -> PinnedState {
    // Aufgerundete Hälfte, aber eine Zeile bleibt immer dem Körper.
    let cap = height
        .saturating_sub(height / 2)
        .min(height.saturating_sub(1));
    for count in (1..=lines.len()).rev() {
        let Some(head) = lines.get(..count) else {
            continue;
        };
        let rows = Paragraph::new(head.to_vec())
            .wrap(Wrap { trim: false })
            .line_count(width);
        if let Some(rows) = u16::try_from(rows).ok().filter(|rows| *rows <= cap) {
            let mut lines = lines.into_iter();
            let pinned = lines.by_ref().take(count).collect();
            return PinnedState {
                pinned,
                rows,
                rest: lines.collect(),
            };
        }
    }
    PinnedState {
        pinned: Vec::new(),
        rows: 0,
        rest: lines,
    }
}

/// Zeichnet die Detailansicht eines Jobs: Kopf, darunter das Log-Ende
/// (stdout, dann stderr), scrollbar wie die Agenten-Detailansicht.
///
/// PL-100 WP 3.5 (Review 4505485685): der Zustand ([`detail_state_lines`])
/// steht fest über dem scrollenden Körper, soweit er in dessen obere Hälfte
/// passt und dem Körper eine Zeile lässt ([`pin_detail_state`]); so ist er
/// schon beim Öffnen (am Log-Ende) zu sehen. Was dort nicht passt, steht im
/// Körper direkt unter der fetten Kennungszeile und ist per Scrollen
/// (`Home`/`g`) erreichbar. Bei einem ungeklärten Anhalten beginnt zudem der
/// Rahmentitel mit dem Warnsymbol [`UNRESOLVED_GLYPH`], denn schmale Breiten
/// schneiden den Titel hinten ab.
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
    let title = crate::i18n::tr_args(
        "mission.detail_title",
        " Job · {p0} · {p1} · {p2} · Esc back ",
        &[
            ("p0", &(detail.row.name).to_string()),
            ("p1", detail.row.state_label()),
            ("p2", &(detail.row.runtime).to_string()),
        ],
    );
    // PL-100 WP 3.5: das Warnsymbol steht vor dem Titel, den schmale
    // Breiten hinten abschneiden.
    let title = if detail.row.suspension_unresolved {
        Line::from(vec![
            Span::styled(format!(" {UNRESOLVED_GLYPH}"), style::warning_style(theme)),
            Span::raw(title),
        ])
    } else {
        Line::raw(title)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // PL-100 WP 3.5: der Zustand steht fest über dem Körper, soweit er passt.
    let state = pin_detail_state(
        detail_state_lines(&detail.row, theme),
        inner.width,
        inner.height,
    );
    let pinned_area = Rect {
        height: state.rows,
        ..inner
    };
    let body_area = Rect {
        y: inner.y.saturating_add(state.rows),
        height: inner.height.saturating_sub(state.rows),
        ..inner
    };
    if state.rows > 0 {
        Paragraph::new(state.pinned)
            .wrap(Wrap { trim: false })
            .render(pinned_area, buf);
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
    lines.extend(state.rest);
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
                &[("label", label), ("p0", &(stream.len()).to_string())],
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
    let total = paragraph.line_count(body_area.width);
    let viewport = usize::from(body_area.height);
    let back = scroll.sync_layout(total, viewport, body_area.width);
    let top = total.saturating_sub(viewport).saturating_sub(back);
    paragraph
        .scroll((u16::try_from(top).unwrap_or(u16::MAX), 0))
        .render(body_area, buf);
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
            suspended: state == JobState::Suspended,
            suspension_unresolved: false,
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

    /// PL-100 WP 3.5: ein angehaltener Job (`suspended`) bleibt eine aktive,
    /// nicht fehlgeschlagene Zeile und zeigt seinen eigenen, lokalisierten
    /// Zustand (`angehalten` auf Deutsch, Englisch außerhalb einer Sprache).
    #[test]
    fn suspended_job_row_is_active_with_its_own_label() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        assert_eq!(job_state_label(JobState::Suspended), "suspended");
        assert_eq!(
            with_language(UiLanguage::German, || job_state_label(JobState::Suspended)),
            "angehalten"
        );
        assert_ne!(
            job_state_label(JobState::Suspended),
            job_state_label(JobState::Running)
        );

        let suspended = status_of(
            &meta_json(
                "suspended",
                r#""started_at":"2026-01-01T00:00:01Z","suspended_at":"2026-01-01T00:00:03Z","suspend_phase":"applied","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&suspended);
        assert_eq!(row.state, JobState::Suspended);
        assert!(row.active && !row.failed, "{row:?}");
        assert_eq!(row.reason, None);
        assert!(row.is_listed());
        let line = job_line(&row, false, 120, Theme::Dark);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("suspended"), "{text}");
        assert!(text.contains('⚙'), "aktives Symbol: {text}");
        Ok(())
    }

    /// PL-100 WP 3.5 (THREAT-MODEL B13): ein abgelöster Job, dessen cgroup
    /// noch eingefroren ist (`detached` mit `suspended_at`, nach `detach_all`
    /// bzw. einem Neustart), zeigt `suspended` statt `detached`; sein Zustand
    /// bleibt typisiert `Detached`. Ohne Vermerk bleibt er `detached`.
    #[test]
    fn frozen_detached_job_row_is_labelled_suspended() -> TestResult {
        let frozen = status_of(
            &meta_json(
                "detached",
                r#""started_at":"2026-01-01T00:00:01Z","suspended_at":"2026-01-01T00:00:03Z","suspend_phase":"applied","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&frozen);
        assert_eq!(row.state, JobState::Detached);
        assert!(row.suspended && row.active && !row.failed, "{row:?}");
        assert!(!row.suspension_unresolved, "{row:?}");
        assert_eq!(row.state_label(), job_state_label(JobState::Suspended));
        let line = job_line(&row, false, 120, Theme::Dark);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("suspended"), "{text}");
        assert!(!text.contains("detached"), "{text}");

        let detached = status_of(
            &meta_json(
                "detached",
                r#""started_at":"2026-01-01T00:00:01Z","#,
                r#"{"session":"uia"}"#,
            ),
            &[],
        )?;
        let row = JobRow::from_status(&detached);
        assert!(!row.suspended, "{row:?}");
        assert_eq!(row.state_label(), job_state_label(JobState::Detached));
        Ok(())
    }

    /// PL-100 WP 3.5: ein Anhalten, von dem nur die Absicht vermerkt ist
    /// (oder ein alter Datensatz ohne Phase), heißt `suspended (unresolved)`
    /// und nie schlicht `suspended`: `job.resume` lehnt es ab, bis der
    /// Operator es von Hand klärt.
    #[test]
    fn unresolved_suspension_has_its_own_label() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        for phase in [r#""suspend_phase":"intent","#, ""] {
            let fields = format!(
                r#""started_at":"2026-01-01T00:00:01Z","suspended_at":"2026-01-01T00:00:03Z",{phase}"#
            );
            let unresolved = status_of(
                &meta_json("suspended", &fields, r#"{"session":"uia"}"#),
                &[],
            )?;
            let row = JobRow::from_status(&unresolved);
            assert!(row.suspended && row.suspension_unresolved, "{row:?}");
            assert!(row.active && !row.failed, "{row:?}");
            assert_eq!(row.state_label(), "suspended (unresolved)");
            assert_ne!(row.state_label(), job_state_label(JobState::Suspended));
            assert_eq!(
                with_language(UiLanguage::German, || row.state_label()),
                "angehalten (ungeklärt)"
            );
        }
        Ok(())
    }

    /// Felder eines Anhaltens, von dem nur die Absicht vermerkt ist.
    const INTENT_FIELDS: &str = r#""started_at":"2026-01-01T00:00:01Z","suspended_at":"2026-01-01T00:00:03Z","suspend_phase":"intent","#;

    /// Felder eines Anhaltens, das harw selbst gesetzt und vermerkt hat.
    const APPLIED_FIELDS: &str = r#""started_at":"2026-01-01T00:00:01Z","suspended_at":"2026-01-01T00:00:03Z","suspend_phase":"applied","#;

    /// Sichtbarer Text einer Panelzeile.
    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Wörter von `text`, mit einfachen Leerzeichen verbunden.
    fn words(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Zeilen im Inneren eines gezeichneten Puffers (ohne Rahmen und Titel),
    /// ohne Leerzeichen am Ende.
    fn inner_rows(buf: &Buffer) -> Vec<String> {
        let height = usize::from(buf.area().height);
        crate::test_support::display_buffer_text(buf)
            .lines()
            .skip(1)
            .take(height.saturating_sub(2))
            .map(|row| row.trim_matches('│').trim_end().to_owned())
            .collect()
    }

    /// Wörter im Inneren eines gezeichneten Puffers (ohne Rahmen und
    /// Titel). Der Wortumbruch trennt nur an Leerzeichen, umbrochene Zeilen
    /// fügen sich so wieder zum Ausgangstext zusammen.
    fn inner_words(buf: &Buffer) -> String {
        words(&inner_rows(buf).join(" "))
    }

    /// Fette Kennungszeile der Detailansicht (`mission.progress`), wie
    /// `render_job_detail` sie setzt.
    fn progress_text(row: &JobRow) -> String {
        crate::i18n::tr_args(
            "mission.progress",
            "{p0} · Progress: {p1}",
            &[("p0", row.id.as_str()), ("p1", row.progress.as_str())],
        )
    }

    /// Zeichnet `detail` in `width`×`height` Zellen, springt wie `Home` an den
    /// Anfang und liefert die Zeilen im Inneren (fester Kopf und Körper).
    fn inner_rows_from_top(detail: &JobDetail, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut scroll = ChatScroll::new();
        // Der erste Durchlauf misst die Geometrie, wie beim Zeichnen der App.
        render_job_detail(
            Some(detail),
            area,
            &mut Buffer::empty(area),
            &scroll,
            Theme::Dark,
        );
        scroll.jump_to_top_measured();
        let mut buf = Buffer::empty(area);
        render_job_detail(Some(detail), area, &mut buf, &scroll, Theme::Dark);
        inner_rows(&buf)
    }

    /// Zeichnet `detail` in `width`×`height` Zellen, springt wie `Home` an den
    /// Anfang und liefert die Wörter im Inneren (fester Kopf und Körper).
    fn body_words_from_top(detail: &JobDetail, width: u16, height: u16) -> String {
        words(&inner_rows_from_top(detail, width, height).join(" "))
    }

    /// Zeichnet `detail` so, wie die Ansicht öffnet: frischer `ChatScroll`,
    /// der dem Log-Ende folgt (`jobs_glue::open_detail`,
    /// `open_job_window_detail`), ohne `Home`.
    fn render_as_opened(detail: &JobDetail, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render_job_detail(
            Some(detail),
            area,
            &mut buf,
            &ChatScroll::new(),
            Theme::Dark,
        );
        buf
    }

    /// Erste Zeile (Rahmen mit Titel) eines gezeichneten Puffers und ob dort
    /// das Warnsymbol im Warnstil steht.
    fn title_row(buf: &Buffer) -> (String, bool) {
        let warning = style::warning_style(Theme::Dark);
        let text = crate::test_support::display_buffer_text(buf)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let area = *buf.area();
        let styled_glyph = (area.left()..area.right()).any(|x| {
            buf.cell((x, area.top())).is_some_and(|cell| {
                cell.symbol() == UNRESOLVED_GLYPH && Some(cell.fg) == warning.fg
            })
        });
        (text, styled_glyph)
    }

    /// PL-100 WP 3.5 (Review 4505485685): ein ungeklärtes Anhalten
    /// verschwindet in schmalen Panelzeilen nie. Wo `suspended (unresolved)`
    /// nicht neben den Namen passt (z. B. 24 Spalten im Hochformat), steht
    /// rechts die kompakte Marke `⏸?`, und das Präfix trägt das Warnsymbol
    /// `⚠`; der Name weicht. Ein schlichtes `suspended` bleibt unverändert.
    #[test]
    fn unresolved_suspension_stays_visible_in_narrow_rows() -> TestResult {
        use crate::i18n::{tr, with_language};
        use harw_config::UiLanguage;
        assert_eq!(display_width(UNRESOLVED_GLYPH), 1);
        let owner = r#"{"session":"uia"}"#;
        let unresolved = JobRow::from_status(&status_of(
            &meta_json("suspended", INTENT_FIELDS, owner),
            &[],
        )?);
        assert!(unresolved.suspension_unresolved, "{unresolved:?}");
        let applied = JobRow::from_status(&status_of(
            &meta_json("suspended", APPLIED_FIELDS, owner),
            &[],
        )?);
        assert!(
            applied.suspended && !applied.suspension_unresolved,
            "{applied:?}"
        );
        let warning = style::warning_style(Theme::Dark);
        for language in [UiLanguage::English, UiLanguage::German] {
            with_language(language, || {
                let label = unresolved.state_label();
                let short = tr("mission.suspension_unresolved_short", "⏸?");
                assert!(display_width(short) <= 3, "{language:?}: {short}");
                for badge in [None, Some(2)] {
                    // Präfix: Auswahlmarker, Symbol, Leerzeichen, DoD-Marke;
                    // ab hier passt die kompakte Marke immer.
                    let prefix = badge.map_or(3, |count| {
                        display_width(&format!("⛨ {count} ")).saturating_add(3)
                    });
                    let marker_from = prefix.saturating_add(display_width(short));
                    // 3: ab hier steht das Symbol; 5: ab hier die Marke (ohne
                    // DoD-Marke), siehe `job_line`.
                    for width in [3, 4, 5, 6, 8, 12, 16, 24, 40] {
                        for selected in [false, true] {
                            let line = job_line_with_badge(
                                &unresolved,
                                selected,
                                width,
                                Theme::Dark,
                                badge,
                            );
                            let text = line_text(&line);
                            let context = format!("{language:?} {width} {badge:?}: {text}");
                            assert!(line.width() <= width, "{context}");
                            assert!(
                                line.spans.iter().any(|span| {
                                    span.content.contains(UNRESOLVED_GLYPH) && span.style == warning
                                }),
                                "Warnsymbol im Präfix: {context}"
                            );
                            if width >= marker_from {
                                assert!(
                                    text.contains(label) || text.contains(short),
                                    "Zustand oder Marke: {context}"
                                );
                            }
                        }
                    }
                }
                // Der Fall aus dem Review: 24 Spalten, das Label passt nicht.
                let text = line_text(&job_line(&unresolved, false, 24, Theme::Dark));
                assert!(
                    text.contains(short) && !text.contains(label),
                    "{language:?}: {text}"
                );
                // Wo es passt, steht das volle Label (mit Laufzeit).
                let text = line_text(&job_line(&unresolved, false, 40, Theme::Dark));
                assert!(
                    text.contains(&format!("{label} {}", unresolved.runtime)),
                    "{language:?}: {text}"
                );
                // Ein belegtes Anhalten bleibt bei 24 Spalten `suspended`.
                let line = job_line(&applied, false, 24, Theme::Dark);
                let text = line_text(&line);
                assert!(text.contains(applied.state_label()), "{language:?}: {text}");
                assert!(
                    text.contains('⚙') && !text.contains(UNRESOLVED_GLYPH),
                    "{language:?}: {text}"
                );
                assert!(!text.contains(short), "{language:?}: {text}");
            });
        }
        let german = with_language(UiLanguage::German, || {
            line_text(&job_line(&applied, false, 24, Theme::Dark))
        });
        assert!(german.contains("angehalten"), "{german}");
        // Untergrenzen aus der Doku von `job_line` (ohne DoD-Marke) und der
        // Name, der der Marke weicht: erst ganz, dann bis auf `t…`.
        for (width, expected) in [(3, " ⚠ "), (5, " ⚠ ⏸?"), (6, " ⚠  ⏸?"), (8, " ⚠ t… ⏸?")]
        {
            assert_eq!(
                line_text(&job_line(&unresolved, false, width, Theme::Dark)),
                expected,
                "{width}"
            );
        }
        Ok(())
    }

    /// PL-100 WP 3.5 (Review 4505485685, Nachprüfung): steht statt des
    /// Labels die kompakte Marke, entfällt der Fortschritt (er weicht vor
    /// dem Zustand); sein Platz gehört dem Namen. Wo das Label passt, bleibt
    /// der Fortschritt wie bisher.
    #[test]
    fn compact_unresolved_marker_drops_the_progress_first() -> TestResult {
        let mut unresolved = row("ladybird build", JobState::Suspended, true, false);
        unresolved.suspension_unresolved = true;
        // 30 Spalten: Präfix 3, Name 14; das Label (23) passt nicht neben
        // die reservierten 8 Spalten des Namens, also steht die Marke.
        let text = line_text(&job_line(&unresolved, false, 30, Theme::Dark));
        assert_eq!(text, format!(" ⚠ ladybird build{} ⏸?", " ".repeat(10)));
        assert!(!text.contains("ninja"), "{text}");
        // Breit genug: Label und Laufzeit, der Fortschritt wie bisher.
        let text = line_text(&job_line(&unresolved, false, 80, Theme::Dark));
        assert!(
            text.contains("ninja 120/900 13%") && text.contains("suspended (unresolved) 12m03s"),
            "{text}"
        );
        // Ein belegtes Anhalten bleibt bei 30 Spalten unverändert: Name auf
        // die reservierten Spalten gekürzt, Zustand und Laufzeit, keine Marke.
        let applied = row("ladybird build", JobState::Suspended, true, false);
        let text = line_text(&job_line(&applied, false, 30, Theme::Dark));
        assert_eq!(text, " ⚙ ladybird … suspended 12m03s");
        Ok(())
    }

    /// PL-100 WP 3.5 (Review 4505485685): die Detailansicht nennt den
    /// Zustand ausdrücklich, nicht nur im Rahmentitel; bei einem ungeklärten
    /// Anhalten folgt der Hinweis zur Klärung im Warnstil. Beides bricht um
    /// und ist auch bei 24 Spalten (Hochformat) per `Home` vollständig
    /// erreichbar, auf Englisch und Deutsch.
    #[test]
    fn job_detail_body_states_an_unresolved_suspension_at_narrow_widths() -> TestResult {
        use crate::i18n::{tr_args, with_language};
        use harw_config::UiLanguage;
        let owner = r#"{"session":"uia"}"#;
        let unresolved = JobDetail::from_status(&status_of(
            &meta_json("suspended", INTENT_FIELDS, owner),
            &[],
        )?);
        let applied = JobDetail::from_status(&status_of(
            &meta_json("suspended", APPLIED_FIELDS, owner),
            &[],
        )?);
        assert!(unresolved.row.suspension_unresolved, "{:?}", unresolved.row);
        assert!(!applied.row.suspension_unresolved, "{:?}", applied.row);

        let warning = style::warning_style(Theme::Dark);
        for language in UiLanguage::ALL {
            with_language(language, || {
                let lines = detail_state_lines(&unresolved.row, Theme::Dark);
                assert_eq!(lines.len(), 2, "{language:?}");
                let state = tr_args(
                    "mission.detail_state",
                    "State: {p0}",
                    &[("p0", unresolved.row.state_label())],
                );
                assert_eq!(
                    lines.first().map(line_text).as_deref(),
                    Some(state.as_str()),
                    "{language:?}"
                );
                let hint = lines.get(1).map(line_text).unwrap_or_default();
                for tool in ["job.resume", "job.stop", "job.suspend"] {
                    assert!(hint.contains(tool), "{language:?}: {hint}");
                }
                assert!(
                    lines.iter().all(|line| line.style.fg == warning.fg),
                    "{language:?}: Warnstil"
                );
                let plain = detail_state_lines(&applied.row, Theme::Dark);
                assert_eq!(plain.len(), 1, "{language:?}");
                assert_eq!(plain.first().map(|line| line.style), Some(Style::default()));
            });
        }

        for language in [UiLanguage::English, UiLanguage::German] {
            with_language(language, || {
                let lines = detail_state_lines(&unresolved.row, Theme::Dark);
                let state = lines
                    .first()
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                let hint = lines
                    .get(1)
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                assert!(!state.is_empty() && !hint.is_empty(), "{language:?}");
                let label = words(unresolved.row.state_label());
                for width in [24, 40] {
                    let body = body_words_from_top(&unresolved, width, 14);
                    assert!(body.contains(&label), "{language:?} {width}: {body}");
                    assert!(
                        body.contains(&state) && body.contains(&hint),
                        "{language:?} {width}: Zustand und Hinweis: {body}"
                    );
                    let body = body_words_from_top(&applied, width, 14);
                    let state = words(&tr_args(
                        "mission.detail_state",
                        "State: {p0}",
                        &[("p0", applied.row.state_label())],
                    ));
                    assert!(body.contains(&state), "{language:?} {width}: {body}");
                    assert!(!body.contains(&hint), "{language:?} {width}: {body}");
                }
            });
        }
        Ok(())
    }

    /// PL-100 WP 3.5 (Review 4505485685, Nachprüfung): die Ansicht öffnet am
    /// Log-Ende (frischer `ChatScroll`, ohne `Home`). Der Zustand steht
    /// trotzdem sofort im Bild, fest über dem Körper; der Rahmentitel beginnt
    /// mit dem Warnsymbol im Warnstil, und das Log-Ende bleibt sichtbar. Bei
    /// 40 Spalten steht auch der Hinweis fest; bei 24 Spalten (Hochformat)
    /// folgt er im Körper (per `Home` erreichbar, siehe
    /// `job_detail_body_states_an_unresolved_suspension_at_narrow_widths`).
    /// Auf Englisch und Deutsch; ein belegtes Anhalten zeigt seinen Zustand
    /// ebenso, ohne Warnsymbol.
    #[test]
    fn job_detail_shows_an_unresolved_suspension_when_it_opens() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let owner = r#"{"session":"uia"}"#;
        let log: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
        let mut unresolved = JobDetail::from_status(&status_of(
            &meta_json("suspended", INTENT_FIELDS, owner),
            &[],
        )?);
        unresolved.stdout = log.clone();
        let mut applied = JobDetail::from_status(&status_of(
            &meta_json("suspended", APPLIED_FIELDS, owner),
            &[],
        )?);
        applied.stdout = log;
        assert!(unresolved.row.suspension_unresolved, "{:?}", unresolved.row);
        assert!(!applied.row.suspension_unresolved, "{:?}", applied.row);

        for language in [UiLanguage::English, UiLanguage::German] {
            with_language(language, || {
                let lines = detail_state_lines(&unresolved.row, Theme::Dark);
                let state = lines
                    .first()
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                let hint = lines
                    .get(1)
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                let label = words(unresolved.row.state_label());
                let applied_state = detail_state_lines(&applied.row, Theme::Dark)
                    .first()
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                assert!(
                    !state.is_empty() && !hint.is_empty() && !applied_state.is_empty(),
                    "{language:?}"
                );
                for width in [24, 40] {
                    let context = format!("{language:?} {width}");
                    let buf = render_as_opened(&unresolved, width, 14);
                    let (title, styled_glyph) = title_row(&buf);
                    assert!(
                        title.contains(UNRESOLVED_GLYPH) && styled_glyph,
                        "Warnsymbol im Titel: {context}: {title}"
                    );
                    let shown = inner_words(&buf);
                    assert!(
                        shown.contains(&state) && shown.contains(&label),
                        "Zustand beim Öffnen: {context}: {shown}"
                    );
                    assert!(shown.contains("line 40"), "Log-Ende: {context}: {shown}");
                    if width == 40 {
                        assert!(shown.contains(&hint), "Hinweis: {context}: {shown}");
                    }

                    let buf = render_as_opened(&applied, width, 14);
                    let (title, styled_glyph) = title_row(&buf);
                    assert!(
                        !title.contains(UNRESOLVED_GLYPH) && !styled_glyph,
                        "{context}: {title}"
                    );
                    let shown = inner_words(&buf);
                    assert!(shown.contains(&applied_state), "{context}: {shown}");
                    assert!(!shown.contains(&hint), "{context}: {shown}");
                    assert!(shown.contains("line 40"), "Log-Ende: {context}: {shown}");
                }
            });
        }
        Ok(())
    }

    /// PL-100 WP 3.5: fest steht der längste Anfang der Zustandszeilen, der
    /// umgebrochen in die (aufgerundete) obere Hälfte passt und dem Körper
    /// mindestens eine Zeile lässt; der Rest folgt in seiner Reihenfolge im
    /// Körper, und passt nicht einmal die erste Zeile, steht nichts fest.
    #[test]
    fn detail_state_pins_the_longest_prefix_that_fits() -> TestResult {
        // Bei 4 Spalten zwei bzw. vier umgebrochene Zeilen.
        let lines = vec![Line::raw("aaaa bbbb"), Line::raw("cccc dddd eeee ffff")];
        let split = |height: u16| {
            let state = pin_detail_state(lines.clone(), 4, height);
            (state.pinned.len(), state.rows, state.rest.len())
        };
        assert_eq!(split(12), (2, 6, 0));
        assert_eq!(split(11), (2, 6, 0), "aufgerundet: 6 von 11");
        assert_eq!(split(10), (1, 2, 1));
        assert_eq!(split(3), (1, 2, 1), "aufgerundet: 2 von 3");
        assert_eq!(split(2), (0, 0, 2));
        let state = pin_detail_state(lines.clone(), 4, 8);
        assert_eq!(
            state.rest.first().map(line_text).as_deref(),
            Some("cccc dddd eeee ffff")
        );
        let state = pin_detail_state(lines, 4, 1);
        assert_eq!(
            state.rest.iter().map(line_text).collect::<Vec<_>>(),
            ["aaaa bbbb", "cccc dddd eeee ffff"]
        );
        // Review: eine Zeile bleibt immer dem Körper. Bei einer Innenhöhe von
        // 1 (Agentenbereich mit `AGENTS_MIN_ROWS`) steht auch eine einzeilige
        // Zustandszeile nicht fest; ab 2 Zeilen steht sie fest.
        let one_row = |height: u16| {
            let state = pin_detail_state(vec![Line::raw("ab")], 40, height);
            (state.pinned.len(), state.rows, state.rest.len())
        };
        assert_eq!(one_row(1), (0, 0, 1));
        assert_eq!(one_row(2), (1, 1, 0));
        assert_eq!(one_row(0), (0, 0, 1));
        Ok(())
    }

    /// PL-100 WP 3.5 (Review, Nachprüfung): bei einer Innenhöhe von 1 (der
    /// Agentenbereich mit `AGENTS_MIN_ROWS = 3`) frisst der feste Zustand den
    /// Körper nicht: beim Öffnen steht das Log-Ende im Bild, nach `Home` die
    /// fette Kennungszeile. Ein ungeklärtes Anhalten trägt weiter das
    /// Warnsymbol vorn im Rahmentitel. Für ein belegtes und ein ungeklärtes
    /// Anhalten, auf Englisch und Deutsch.
    #[test]
    fn job_detail_keeps_a_body_row_in_a_one_row_view() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let owner = r#"{"session":"uia"}"#;
        // stderr steht im Körper zuletzt, `line 40` ist so die letzte Zeile.
        let log: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
        let mut unresolved = JobDetail::from_status(&status_of(
            &meta_json("suspended", INTENT_FIELDS, owner),
            &[],
        )?);
        unresolved.stderr = log.clone();
        let mut applied = JobDetail::from_status(&status_of(
            &meta_json("suspended", APPLIED_FIELDS, owner),
            &[],
        )?);
        applied.stderr = log;
        assert!(unresolved.row.suspension_unresolved, "{:?}", unresolved.row);
        assert!(!applied.row.suspension_unresolved, "{:?}", applied.row);

        for language in [UiLanguage::English, UiLanguage::German] {
            with_language(language, || {
                for detail in [&unresolved, &applied] {
                    let context = format!("{language:?} {}", detail.row.state_label());
                    // 40×3: Innenfläche 38×1.
                    let buf = render_as_opened(detail, 40, 3);
                    assert_eq!(inner_rows(&buf), ["line 40"], "Log-Ende: {context}");
                    let (title, styled_glyph) = title_row(&buf);
                    if detail.row.suspension_unresolved {
                        assert!(
                            title.contains(UNRESOLVED_GLYPH) && styled_glyph,
                            "Warnsymbol im Titel: {context}: {title}"
                        );
                    } else {
                        assert!(
                            !title.contains(UNRESOLVED_GLYPH) && !styled_glyph,
                            "{context}: {title}"
                        );
                    }
                    assert_eq!(
                        inner_rows_from_top(detail, 40, 3),
                        [progress_text(&detail.row)],
                        "Kennungszeile nach Home: {context}"
                    );
                }
            });
        }
        Ok(())
    }

    /// PL-100 WP 3.5 (Review, Nachprüfung): passt nicht einmal die
    /// Zustandszeile in den festen Kopf, steht sie im Körper direkt unter der
    /// fetten Kennungszeile. 24×4: Innenfläche 22×2, fest höchstens eine
    /// Zeile; `State: suspended (unresolved)` bzw. `Zustand: angehalten
    /// (ungeklärt)` braucht dort zwei, also steht nichts fest. Nach `Home`
    /// zeigt die erste Körperzeile die Kennung, die zweite den Anfang der
    /// Zustandszeile.
    #[test]
    fn job_detail_puts_an_unpinned_state_right_below_the_id_line() -> TestResult {
        use crate::i18n::with_language;
        use harw_config::UiLanguage;
        let owner = r#"{"session":"uia"}"#;
        let unresolved = JobDetail::from_status(&status_of(
            &meta_json("suspended", INTENT_FIELDS, owner),
            &[],
        )?);
        assert!(unresolved.row.suspension_unresolved, "{:?}", unresolved.row);
        for language in [UiLanguage::English, UiLanguage::German] {
            with_language(language, || {
                let lines = detail_state_lines(&unresolved.row, Theme::Dark);
                let state = lines
                    .first()
                    .map(|line| words(&line_text(line)))
                    .unwrap_or_default();
                let first_word = state.split_whitespace().next().unwrap_or_default();
                assert!(
                    first_word == "State:" || first_word == "Zustand:",
                    "{language:?}: {state}"
                );
                // Die Voraussetzung: die Zustandszeile braucht zwei Zeilen.
                let pinned = pin_detail_state(lines, 22, 2);
                assert_eq!(
                    (pinned.pinned.len(), pinned.rows, pinned.rest.len()),
                    (0, 0, 2),
                    "{language:?}"
                );

                let rows = inner_rows_from_top(&unresolved, 24, 4);
                assert_eq!(rows.len(), 2, "{language:?}: {rows:?}");
                assert_eq!(
                    rows.first().map(String::as_str),
                    Some(progress_text(&unresolved.row).as_str()),
                    "Kennungszeile: {language:?}: {rows:?}"
                );
                let second = rows.get(1).map(|row| words(row)).unwrap_or_default();
                assert!(
                    second.starts_with(first_word) && state.starts_with(&second),
                    "Zustandszeile darunter: {language:?}: {rows:?}"
                );
            });
        }
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
        // PL-100 WP 3.5: ein ungeklärtes Anhalten verdrängt den Namen, nie die
        // Breitengrenze.
        let mut unresolved = row(
            &"ladybird build ".repeat(10),
            JobState::Suspended,
            true,
            false,
        );
        unresolved.suspension_unresolved = true;
        let warning = style::warning_style(Theme::Dark);
        let short = crate::i18n::tr("mission.suspension_unresolved_short", "⏸?");
        let label = unresolved.state_label();
        // Ab hier steht die Marke: Präfix (Auswahlmarker, Symbol, Leerzeichen,
        // DoD-Marke) plus Marke, siehe `job_line`.
        let marker_from = |badge: Option<u32>| {
            badge
                .map_or(0, |count| display_width(&format!("⛨ {count} ")))
                .saturating_add(3)
                .saturating_add(display_width(short))
        };
        for width in [0, 1, 2, 3, 4, 5, 6, 8, 9, 10, 12, 16, 24, 40, 80] {
            for selected in [false, true] {
                for line in [
                    job_line(&long, selected, width, Theme::Dark),
                    job_line(&unresolved, selected, width, Theme::Dark),
                    job_line_with_badge(&unresolved, selected, width, Theme::Dark, Some(12)),
                    job_line(&finished[1], selected, width, Theme::Dark),
                    finished_jobs_line(&refs, false, selected, width, Theme::Dark),
                    group_header_line(1, 3, width, Theme::Dark),
                ] {
                    assert!(line.width() <= width, "{width}: {line:?}");
                }
                // Der lange Name weicht der Marke, nie umgekehrt; das
                // Warnsymbol steht ab 3 Spalten im Präfix.
                for badge in [None, Some(12)] {
                    let line =
                        job_line_with_badge(&unresolved, selected, width, Theme::Dark, badge);
                    let text = line_text(&line);
                    if width >= 3 {
                        assert!(
                            line.spans.iter().any(|span| {
                                span.content.contains(UNRESOLVED_GLYPH) && span.style == warning
                            }),
                            "Warnsymbol: {width} {badge:?}: {text}"
                        );
                    }
                    if width >= marker_from(badge) {
                        assert!(
                            text.contains(short) || text.contains(label),
                            "Marke oder Label: {width} {badge:?}: {text}"
                        );
                    }
                }
            }
        }
        assert_eq!(marker_from(None), 5);
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
            &["raw stderr {p0}: विफल 中文 👩🏽\u{200D}💻 é"],
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
                assert!(reason.ends_with(&sanitize_inline(
                    "raw stderr {p0}: विफल 中文 👩🏽\u{200D}💻 é"
                )));
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

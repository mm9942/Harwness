//! Harw-DoD view and status-line mark of the TUI (PL-97 D5, plan §3.1
//! "TUI surface").
//!
//! # Surfaces
//! - [`DodView`] ([`OverlayView`]), opened by the TUI-local `/dod view`
//!   ([`is_view_command`]): three sections — units, sensor health and the
//!   last [`FINDINGS_LIMIT`] findings with severity colouring. Every other
//!   `/dod` form stays an inline command of the `dod` operation.
//! - [`DodMark`] / [`DodMarkFeed`]: the `⛨ n high` status-line mark and the
//!   `· ⛨ DoD n` suffix of the Finished / Failed window, read from the
//!   `attention` field of `/dod status`.
//!
//! # Data path
//! Nothing here reads a file or asks systemd. The view names its commands
//! ([`REFRESH_COMMAND`], expanded by [`fetch_commands`] into status,
//! sensors and findings); `app.rs` runs them through the regular
//! `fetch_command_data` / `process_pending_fetches` path and hands the
//! `OpOutput.data` of each to [`OverlayView::apply_data`]. The `dod`
//! operation serves them from its cached `DodService`. Rendering only formats
//! the last received JSON.
//!
//! # Trust boundary
//! The export is attacker-influenced evidence. Every text taken from it is
//! sanitized for the terminal ([`sanitize_inline`]) and cut to
//! [`MAX_TEXT_CHARS`]; findings and sensor health from an unverified export
//! source (`verified: false`) are labelled `UNVERIFIED` line by line, with the
//! source and its hint above them. The view never runs anything but the
//! read-only `/dod status|sensors|findings`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use harw_operations::OpOutput;
use harw_ops::dod::ScopeCount;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use serde_json::Value;

use crate::chat_scroll::ChatScroll;
use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, json_str, plain, render_panel,
};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Data command of the view (and of the status-line mark): `/dod status`.
pub(crate) const REFRESH_COMMAND: &str = "/dod status";
/// Sensor-health command of the view.
pub(crate) const SENSORS_COMMAND: &str = "/dod sensors";
/// Number of findings the view lists (newest first).
pub(crate) const FINDINGS_LIMIT: usize = 30;
/// Minimum spacing of two status-line mark reads; equal to the cache
/// lifetime of the `DodService` behind `/dod`, so a faster cadence would only
/// return the same snapshot.
pub(crate) const MARK_REFRESH_INTERVAL: Duration = harw_ops::dod::CACHE_TTL;
/// Longest text taken from the export per field, in characters (the
/// operation already bounds to 256).
const MAX_TEXT_CHARS: usize = 160;
/// Frame title.
const TITLE: &str = " Harw-DoD ";
/// Footer key hint.
const FOOTER: &str = "j/k scrollen · Bild↑/↓ Seite · r neu laden · Esc schließen";

/// The findings command of the view: `/dod findings <FINDINGS_LIMIT>`.
#[must_use]
pub(crate) fn findings_command() -> String {
    format!("/dod findings {FINDINGS_LIMIT}")
}

/// `true` for the TUI-local `/dod view` (case-insensitive, surrounding
/// whitespace ignored); every other `/dod` form belongs to the operation.
#[must_use]
pub(crate) fn is_view_command(raw: &str) -> bool {
    let mut words = raw.split_whitespace();
    matches!(
        (words.next(), words.next(), words.next()),
        (Some(name), Some(sub), None)
            if name.eq_ignore_ascii_case("/dod") && sub.eq_ignore_ascii_case("view")
    )
}

/// The data commands behind one fetch request of an overlay view.
///
/// [`REFRESH_COMMAND`] of the [`DodView`] stands for its three sections
/// (status, sensors, findings), which `app.rs` queues as separate fetches of
/// the same view generation; every other command stays as it is.
#[must_use]
pub(crate) fn fetch_commands(command: String) -> Vec<String> {
    if command == REFRESH_COMMAND {
        // Genau `SECTION_COUNT` Abrufe: `DodView` zählt seine Fehler je Zyklus.
        vec![command, SENSORS_COMMAND.to_owned(), findings_command()]
    } else {
        vec![command]
    }
}

// ── Status-line mark ────────────────────────────────────────────────────────

/// High/critical findings of the summary window, as `/dod status` reports
/// them in its `attention` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DodMark {
    /// `high + critical` findings in the window.
    pub(crate) count: u64,
    /// `true` when older export data was not read: `count` is a lower bound.
    pub(crate) lower_bound: bool,
    /// `false` when the export does not come from the system sentinel export.
    pub(crate) verified: bool,
}

impl DodMark {
    /// The mark of a `/dod status` result; `None` when Harw-DoD is not
    /// installed or its findings export is unreadable (`attention: null`),
    /// or when the data are not a status result.
    #[must_use]
    pub(crate) fn from_status(data: &Value) -> Option<Self> {
        if data.get("installed").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        let attention = data.get("attention")?;
        Some(Self {
            count: attention.get("count")?.as_u64()?,
            lower_bound: attention
                .get("lower_bound")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            verified: attention
                .get("verified")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// `true` when the mark is worth showing (at least one high/critical
    /// finding).
    #[must_use]
    pub(crate) const fn is_alert(self) -> bool {
        self.count > 0
    }

    /// `n`, or `≥ n` when the count is a lower bound.
    #[must_use]
    pub(crate) fn count_text(self) -> String {
        if self.lower_bound {
            format!("≥ {}", self.count)
        } else {
            self.count.to_string()
        }
    }
}

/// Result of one background `/dod status` read of the status-line mark.
pub(crate) type MarkRead = tokio::sync::oneshot::Receiver<Result<OpOutput, String>>;

/// State of the status-line mark (field `ChatApp::dod_mark`): the last read,
/// when it was asked for, and the read still in flight.
///
/// The read itself runs as a task (`app.rs` spawns it and hands the receiver
/// to [`Self::begin`]); the TUI loop only polls [`Self::poll`], so a slow
/// `DodService` snapshot never delays input handling.
#[derive(Debug, Default)]
pub(crate) struct DodMarkFeed {
    /// When the last read was started.
    last_poll: Option<Instant>,
    /// The last mark read; `None` hides the mark.
    mark: Option<DodMark>,
    /// The read started by [`Self::begin`] that has not reported yet.
    in_flight: Option<MarkRead>,
    /// PL-97 E4: high/critical findings per cgroup scope of the last read
    /// (`attention.scopes`); empty whenever the mark is unavailable.
    scopes: Vec<ScopeCount>,
}

/// Parses `attention.scopes` of a `/dod status` result (bounded to
/// [`harw_ops::dod::MAX_ATTENTION_SCOPES`] entries; malformed entries are
/// skipped).
fn attention_scopes(data: &Value) -> Vec<ScopeCount> {
    data.get("attention")
        .and_then(|attention| attention.get("scopes"))
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(|entry| {
                    Some(ScopeCount {
                        scope: entry.get("scope")?.as_str()?.to_owned(),
                        count: u32::try_from(entry.get("count")?.as_u64()?).ok()?,
                    })
                })
                .take(harw_ops::dod::MAX_ATTENTION_SCOPES)
                .collect()
        })
        .unwrap_or_default()
}

impl DodMarkFeed {
    /// `true` when the next read is due: none in flight, and never read or
    /// the last one is at least [`MARK_REFRESH_INTERVAL`] old.
    #[must_use]
    pub(crate) fn due(&self, now: Instant) -> bool {
        self.in_flight.is_none()
            && self
                .last_poll
                .is_none_or(|at| now.saturating_duration_since(at) >= MARK_REFRESH_INTERVAL)
    }

    /// `true` while a started read has not reported yet.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Records that the read `read` started at `now`.
    pub(crate) fn begin(&mut self, now: Instant, read: MarkRead) {
        self.last_poll = Some(now);
        self.in_flight = Some(read);
    }

    /// Takes the result of the read in flight, if it has arrived, without
    /// waiting. A read task that ended without an answer hides the mark.
    ///
    /// # Returns
    /// `None` while no read finished; `Some(changed)` once one did, with
    /// `changed` as in [`Self::apply`].
    pub(crate) fn poll(&mut self) -> Option<bool> {
        let read = self.in_flight.as_mut()?;
        let result = match read.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                Err("DoD-Abruf ohne Antwort beendet".to_owned())
            }
        };
        self.in_flight = None;
        Some(self.apply(&result))
    }

    /// Takes the result of a `/dod status` read. Any failure — the
    /// operation missing or refused, no data — hides the mark.
    ///
    /// # Returns
    /// `true` when the visible mark changed.
    pub(crate) fn apply(&mut self, result: &Result<OpOutput, String>) -> bool {
        let data = result.as_ref().ok().and_then(|output| output.data.as_ref());
        let mark = data.and_then(DodMark::from_status);
        let scopes = match (mark, data) {
            (Some(_), Some(data)) => attention_scopes(data),
            _ => Vec::new(),
        };
        let changed =
            self.visible() != mark.filter(|mark| mark.is_alert()) || self.scopes != scopes;
        self.mark = mark;
        self.scopes = scopes;
        changed
    }

    /// PL-97 E4: `(job id, n)` for every job row with `n > 0` high/critical
    /// findings attributed to its own cgroup (path-component prefix, most
    /// specific job), from the last read only — no file is read here.
    /// Empty when DoD is unavailable, and for an unverified source (a
    /// home/override export a user process can write): a job row never
    /// shows such data as a DoD fact.
    #[must_use]
    pub(crate) fn job_badges(&self, jobs: &[crate::jobs_panel::JobRow]) -> Vec<(String, u32)> {
        if !self.mark.is_some_and(|mark| mark.verified) || self.scopes.is_empty() {
            return Vec::new();
        }
        harw_ops::dod::correlate_counts(&self.scopes, jobs)
    }

    /// The mark to show: present only with at least one high/critical
    /// finding.
    #[must_use]
    pub(crate) fn visible(&self) -> Option<DodMark> {
        self.mark.filter(|mark| mark.is_alert())
    }
}

/// Style of a mark: error colour for verified findings, warning colour for
/// an unverified source.
#[must_use]
pub(crate) fn mark_style(mark: DodMark, theme: Theme) -> Style {
    let colour = if mark.verified {
        style::error_color(theme)
    } else {
        style::warning_color(theme)
    };
    Style::default().fg(colour).add_modifier(Modifier::BOLD)
}

// ── View ────────────────────────────────────────────────────────────────────

/// Which of the three data commands a JSON result answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// `/dod status` (has `units`).
    Status,
    /// `/dod sensors` (health export view).
    Sensors,
    /// `/dod findings n` (has `query`).
    Findings,
}

impl Section {
    /// Classifies a result by its shape.
    fn of(data: &Value) -> Option<Self> {
        if data.get("units").is_some() {
            Some(Self::Status)
        } else if data.get("query").is_some() {
            Some(Self::Findings)
        } else if data.get("source").is_some() && data.get("state").is_some() {
            Some(Self::Sensors)
        } else {
            None
        }
    }
}

/// Number of data commands (and results) of one view refresh.
const SECTION_COUNT: usize = 3;

/// The Harw-DoD view (`/dod view`).
#[derive(Debug, Default)]
pub(crate) struct DodView {
    /// Last `/dod status` result.
    status: Option<Value>,
    /// Last `/dod sensors` result.
    sensors: Option<Value>,
    /// Last `/dod findings` result.
    findings: Option<Value>,
    /// Errors of the current refresh cycle (one per failed data command,
    /// deduplicated). A cycle is the [`SECTION_COUNT`] results of one
    /// [`fetch_commands`] expansion; the errors are cleared only when the
    /// next cycle starts, so a later section that succeeds does not hide the
    /// failure of an earlier one.
    errors: Vec<String>,
    /// Results of the current refresh cycle received so far
    /// (`0..SECTION_COUNT`).
    cycle_results: usize,
    /// Scroll state (offset from the end, see [`ChatScroll`]).
    scroll: ChatScroll,
    /// `true` until the reader scrolls: the view stays at its first line
    /// while the sections fill in.
    pinned_top: bool,
    /// `(total lines, viewport)` of the last frame.
    measure: std::cell::Cell<(usize, usize)>,
}

impl DodView {
    /// An empty view; `app.rs` fetches its data right after opening it.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            pinned_top: true,
            ..Self::default()
        }
    }

    /// Counts one result of a refresh cycle; the first result of a new
    /// cycle drops the errors of the previous one.
    fn count_result(&mut self) {
        if self.cycle_results == 0 {
            self.errors.clear();
        }
        self.cycle_results = (self.cycle_results + 1) % SECTION_COUNT;
    }

    /// All content lines of the three sections.
    fn lines(&self, theme: Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for error in &self.errors {
            lines.push(Line::styled(
                format!("Fehler: {}", clean(error)),
                style::error_style(theme),
            ));
        }
        if !self.errors.is_empty() {
            lines.push(Line::from(""));
        }
        self.units_lines(theme, &mut lines);
        lines.push(Line::from(""));
        self.sensor_lines(theme, &mut lines);
        lines.push(Line::from(""));
        self.finding_lines(theme, &mut lines);
        lines
    }

    fn units_lines(&self, theme: Theme, lines: &mut Vec<Line<'static>>) {
        lines.push(heading("UNITS", theme));
        let Some(status) = &self.status else {
            lines.push(Line::styled("  lade …", style::dim_style(theme)));
            return;
        };
        if status.get("installed").and_then(Value::as_bool) == Some(false) {
            lines.push(Line::styled(
                "  Harw-DoD is not installed on this host.",
                style::warning_style(theme),
            ));
        }
        let install = status.get("install");
        let install_state = install
            .and_then(|install| json_str(install, "state"))
            .map_or_else(|| "?".to_owned(), |state| state.replace('_', " "));
        lines.push(Line::raw(format!("  install   {}", clean(&install_state))));
        if let Some(warden) = json_str(status, "warden") {
            lines.push(Line::raw(format!("  warden    {}", clean(&warden))));
        }
        let units = status
            .get("units")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if units.is_empty() {
            lines.push(Line::styled("  no units reported", style::dim_style(theme)));
        }
        for unit in units {
            lines.push(unit_line(unit, theme));
        }
        if let Some(mark) = DodMark::from_status(status) {
            let text = format!(
                "  attention {}{} high/critical in the window",
                if mark.verified { "" } else { "UNVERIFIED " },
                mark.count_text()
            );
            let style = if mark.is_alert() {
                mark_style(mark, theme)
            } else {
                Style::default()
            };
            lines.push(Line::styled(text, style));
        }
    }

    fn sensor_lines(&self, theme: Theme, lines: &mut Vec<Line<'static>>) {
        lines.push(heading("SENSORS", theme));
        let Some(sensors) = &self.sensors else {
            lines.push(Line::styled("  lade …", style::dim_style(theme)));
            return;
        };
        let verified = source_note(sensors, theme, lines);
        let prefix = unverified_prefix(verified);
        if json_str(sensors, "state").as_deref() != Some("readable") {
            lines.push(Line::styled(
                format!(
                    "  {prefix}health export {}",
                    export_state_text(sensors, "path")
                ),
                style::warning_style(theme),
            ));
            return;
        }
        let stale = if sensors.get("stale").and_then(Value::as_bool) == Some(true) {
            " — stale, the sentinel stopped writing it"
        } else {
            ""
        };
        lines.push(Line::raw(format!(
            "  {prefix}health written {}{stale}",
            field(sensors, "written_at")
        )));
        let list = sensors
            .get("sensors")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if list.is_empty() {
            lines.push(Line::styled(
                "  no sensors reported",
                style::dim_style(theme),
            ));
        }
        for sensor in list {
            let health = json_str(sensor, "health").unwrap_or_else(|| "unknown".to_owned());
            let health_style = match health.as_str() {
                "bound" => style::success_style(theme),
                "retrying" => style::warning_style(theme),
                "degraded" => style::error_style(theme),
                _ => style::dim_style(theme),
            };
            let reason = json_str(sensor, "degrade_reason")
                .map(|reason| format!("  {}", clean(&reason)))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::raw(format!("  {prefix}{:<16} ", field(sensor, "id"))),
                Span::styled(format!("{:<9}", clean(&health)), health_style),
                Span::raw(format!(
                    " readings {} · errors {} · last ok {}{reason}",
                    number(sensor, "readings_total"),
                    number(sensor, "errors_total"),
                    field(sensor, "last_ok_at"),
                )),
            ]));
        }
        if let Some(ipc) = sensors.get("ipc").filter(|ipc| ipc.is_object()) {
            lines.push(Line::styled(
                format!(
                    "  {prefix}ipc: {} probes connected, {} events, {} dropped",
                    number(ipc, "probes_connected"),
                    number(ipc, "events_total"),
                    number(ipc, "dropped_total"),
                ),
                style::dim_style(theme),
            ));
        }
    }

    fn finding_lines(&self, theme: Theme, lines: &mut Vec<Line<'static>>) {
        let Some(findings) = &self.findings else {
            lines.push(heading(&format!("FINDINGS (last {FINDINGS_LIMIT})"), theme));
            lines.push(Line::styled("  lade …", style::dim_style(theme)));
            return;
        };
        let list = findings
            .get("findings")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let complete = findings.get("complete").and_then(Value::as_bool) == Some(true);
        let matched = number(findings, "matched");
        let matched = if complete {
            matched
        } else {
            format!("≥ {matched}")
        };
        lines.push(heading(
            &format!(
                "FINDINGS (newest first, {} shown of {matched} matching{})",
                list.len(),
                if complete { "" } else { ", partial" }
            ),
            theme,
        ));
        let verified = source_note(findings, theme, lines);
        let readable = findings
            .get("export")
            .and_then(|export| json_str(export, "state"))
            .as_deref()
            == Some("readable");
        if !readable {
            lines.push(Line::styled(
                format!(
                    "  {}findings export {}",
                    unverified_prefix(verified),
                    export_state_text(findings.get("export").unwrap_or(&Value::Null), "")
                ),
                style::warning_style(theme),
            ));
            return;
        }
        if list.is_empty() {
            lines.push(Line::styled("  no findings", style::dim_style(theme)));
        }
        for finding in list {
            lines.push(finding_line(finding, verified, theme));
        }
    }
}

impl OverlayView for DodView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        Clear.render(area, buf);
        let lines = self.lines(theme);
        let viewport = content_height(area, false);
        let total = lines.len();
        self.measure.set((total, viewport));
        // ChatScroll zählt vom Ende; solange niemand scrollt, bleibt die
        // Ansicht oben stehen, während die Abschnitte nachkommen.
        let offset = self.scroll.sync_layout(total, viewport, area.width);
        let start = if self.pinned_top {
            0
        } else {
            total.saturating_sub(viewport).saturating_sub(offset)
        };
        render_panel(area, buf, theme, TITLE, None, &lines, start, FOOTER);
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        let (total, viewport) = self.measure.get();
        let scrolls = is_up(&key)
            || is_down(&key)
            || matches!(
                key.code,
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End
            )
            || (plain(&key) && matches!(key.code, KeyCode::Char('g' | 'G')));
        if scrolls {
            if total <= viewport {
                return OverlayOutcome::Stay;
            }
            if self.pinned_top {
                // Erstes Scrollen: ChatScroll übernimmt ab der obersten Zeile.
                self.pinned_top = false;
                self.scroll.jump_to_top_measured();
            }
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Char('q') if plain(&key) => OverlayOutcome::Close,
            KeyCode::Char('r') if plain(&key) => OverlayOutcome::Fetch(REFRESH_COMMAND.to_owned()),
            KeyCode::PageUp => {
                self.scroll.page_up(total, viewport);
                OverlayOutcome::Stay
            }
            KeyCode::PageDown => {
                self.scroll.page_down(viewport);
                OverlayOutcome::Stay
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll.jump_to_top_measured();
                OverlayOutcome::Stay
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.scroll.jump_to_bottom();
                OverlayOutcome::Stay
            }
            _ if is_up(&key) => {
                self.scroll.scroll_up_measured(1);
                OverlayOutcome::Stay
            }
            _ if is_down(&key) => {
                self.scroll.scroll_down(1);
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(REFRESH_COMMAND.to_owned())
    }

    fn apply_data(&mut self, data: &Value) {
        self.count_result();
        match Section::of(data) {
            Some(Section::Status) => self.status = Some(data.clone()),
            Some(Section::Sensors) => self.sensors = Some(data.clone()),
            Some(Section::Findings) => self.findings = Some(data.clone()),
            None => {}
        }
    }

    fn apply_error(&mut self, text: &str) {
        self.count_result();
        if !self.errors.iter().any(|error| error == text) {
            self.errors.push(text.to_owned());
        }
    }
}

// ── Line helpers ────────────────────────────────────────────────────────────

/// Terminal-safe, bounded text from the export.
fn clean(text: &str) -> String {
    let clean = sanitize_inline(text);
    if clean.chars().count() <= MAX_TEXT_CHARS {
        return clean;
    }
    let mut cut: String = clean.chars().take(MAX_TEXT_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// A cleaned string field, `-` when missing.
fn field(value: &Value, key: &str) -> String {
    json_str(value, key).map_or_else(|| "-".to_owned(), |text| clean(&text))
}

/// A number field, `?` when missing.
fn number(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map_or_else(|| "?".to_owned(), |n| n.to_string())
}

fn heading(text: &str, theme: Theme) -> Line<'static> {
    Line::styled(
        text.to_owned(),
        Style::default()
            .fg(style::accent_color(theme))
            .add_modifier(Modifier::BOLD),
    )
}

/// `"UNVERIFIED "` for an unverified source, else empty.
const fn unverified_prefix(verified: bool) -> &'static str {
    if verified { "" } else { "UNVERIFIED " }
}

/// Pushes the source note of an unverified result (`verified: false`): the
/// source and its hint. Missing `verified` counts as unverified.
///
/// # Returns
/// Whether the source is verified.
fn source_note(data: &Value, theme: Theme, lines: &mut Vec<Line<'static>>) -> bool {
    let verified = data.get("verified").and_then(Value::as_bool) == Some(true);
    if !verified {
        let source = json_str(data, "source").unwrap_or_else(|| "unknown".to_owned());
        let hint = json_str(data, "hint")
            .unwrap_or_else(|| "the export source is not verified".to_owned());
        lines.push(Line::styled(
            format!(
                "  UNVERIFIED source ({}): {}; not Harw-DoD facts",
                clean(&source),
                clean(&hint)
            ),
            style::warning_style(theme),
        ));
    }
    verified
}

/// `missing (<path>)` / `unreadable: <reason> (<path>)` of an export state
/// object; `path_key` names the path field ("" for none).
fn export_state_text(state: &Value, path_key: &str) -> String {
    let mut text = field(state, "state");
    if let Some(reason) = json_str(state, "reason") {
        text.push_str(&format!(": {}", clean(&reason)));
    }
    if let Some(path) = json_str(state, path_key) {
        text.push_str(&format!(" ({})", clean(&path)));
    }
    text
}

fn unit_line(unit: &Value, theme: Theme) -> Line<'static> {
    let name = field(unit, "unit");
    match json_str(unit, "active") {
        Some(active) => {
            let active_style = match active.as_str() {
                "active" => style::success_style(theme),
                "failed" => style::error_style(theme),
                _ => style::dim_style(theme),
            };
            Line::from(vec![
                Span::raw(format!("  {name:<26} ")),
                Span::styled(
                    format!("{}/{}", clean(&active), field(unit, "sub")),
                    active_style,
                ),
                Span::styled(
                    format!(
                        "  {}",
                        json_str(unit, "file")
                            .map_or_else(|| "no unit file".to_owned(), |file| clean(&file))
                    ),
                    style::dim_style(theme),
                ),
            ])
        }
        None => {
            let state = match json_str(unit, "status").as_deref() {
                Some("not_available") => "systemd not available".to_owned(),
                _ => format!("unknown ({})", field(unit, "reason")),
            };
            Line::styled(format!("  {name:<26} {state}"), style::dim_style(theme))
        }
    }
}

fn severity_style(severity: &str, theme: Theme) -> Style {
    match severity {
        "critical" => style::error_style(theme).add_modifier(Modifier::BOLD),
        "high" => style::error_style(theme),
        "medium" => style::warning_style(theme),
        "low" => Style::default(),
        _ => style::dim_style(theme),
    }
}

fn finding_line(finding: &Value, verified: bool, theme: Theme) -> Line<'static> {
    let severity = json_str(finding, "severity").unwrap_or_else(|| "info".to_owned());
    let scope = json_str(finding, "scope")
        .map(|scope| format!("  [{}]", clean(&scope)))
        .unwrap_or_default();
    let mut spans = Vec::new();
    if !verified {
        spans.push(Span::styled("  UNVERIFIED ", style::warning_style(theme)));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(
        format!("{}  ", field(finding, "observed_at")),
        style::dim_style(theme),
    ));
    spans.push(Span::styled(
        format!("{:<8}", clean(&severity)),
        severity_style(&severity, theme),
    ));
    spans.push(Span::raw(format!(
        " {}  {}{scope}",
        field(finding, "rule"),
        field(finding, "summary")
    )));
    Line::from(spans)
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::KeyModifiers;
    use serde_json::json;

    use super::*;
    use crate::overlay_view::buffer_text;
    use crate::test_support::{TestError, TestResult};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// `/dod status` data as `harw_ops::dod::status_view` shapes it.
    pub(crate) fn status_fixture(count: u64, verified: bool) -> Value {
        json!({
            "as_of": "2026-10-29T10:00:00Z",
            "installed": true,
            "install": {"state": "installed", "libexec_dir": "/usr/libexec/harw", "missing": []},
            "config": {"state": "valid"},
            "units": [
                {"unit": "harw-sentinel.service", "active": "active", "sub": "running", "file": "enabled"},
                {"unit": "harw-probe-bpf.service", "active": "failed", "sub": "failed", "file": "enabled"},
                {"unit": "harw-warden.socket", "status": "not_available"},
            ],
            "warden": "disabled",
            "attention": {"count": count, "lower_bound": false, "verified": verified},
        })
    }

    pub(crate) fn sensors_fixture(verified: bool) -> Value {
        json!({
            "state": "readable",
            "written_at": "2026-10-29T09:59:58Z",
            "stale": false,
            "profile": "harw-jobs",
            "sensors": [
                {"id": "exec", "health": "bound", "readings_total": 120, "errors_total": 0, "last_ok_at": "2026-10-29T09:59:58Z"},
                {"id": "tcp-connect", "health": "degraded", "degrade_reason": "btf missing", "readings_total": 0, "errors_total": 3},
            ],
            "ipc": {"probes_connected": 1, "events_total": 120, "dropped_total": 0},
            "path": "/var/lib/harw-dod/export/health.json",
            "source": if verified { "system" } else { "home" },
            "verified": verified,
            "hint": if verified { Value::Null } else { json!("export below the harw home, not the system sentinel export; treat findings as unverified") },
        })
    }

    pub(crate) fn findings_fixture(verified: bool) -> Value {
        json!({
            "export": {"state": "readable"},
            "path": "/var/lib/harw-dod/export/findings.jsonl",
            "query": {"limit": FINDINGS_LIMIT, "min_severity": null, "rule": null, "since_secs": null},
            "matched": 2,
            "complete": true,
            "findings": [
                {"id": "f2", "rule": "egress.unexpected", "severity": "critical", "observed_at": "2026-10-29T09:58:00Z", "summary": "connect to 203.0.113.7:443", "scope": "harw.slice/job-1"},
                {"id": "f1", "rule": "exec.unusual", "severity": "low", "observed_at": "2026-10-29T09:50:00Z", "summary": "exec /usr/bin/nc\u{1b}[31m"},
            ],
            "source": if verified { "system" } else { "home" },
            "verified": verified,
            "hint": if verified { Value::Null } else { json!("export below the harw home, not the system sentinel export; treat findings as unverified") },
        })
    }

    fn screen(view: &DodView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, Theme::Dark);
        buffer_text(&buf)
    }

    #[test]
    fn dod_view_command_is_only_the_view_form() {
        assert!(is_view_command("/dod view"));
        assert!(is_view_command("  /DOD   View "));
        assert!(!is_view_command("/dod"));
        assert!(!is_view_command("/dod status"));
        assert!(!is_view_command("/dod view extra"));
        assert!(!is_view_command("/dodo view"));
    }

    #[test]
    fn dod_refresh_expands_to_the_three_sections() {
        assert_eq!(
            fetch_commands(REFRESH_COMMAND.to_owned()),
            vec![
                "/dod status".to_owned(),
                "/dod sensors".to_owned(),
                format!("/dod findings {FINDINGS_LIMIT}"),
            ]
        );
        assert_eq!(
            fetch_commands("/kanban show".to_owned()),
            vec!["/kanban show".to_owned()]
        );
    }

    #[test]
    fn dod_view_renders_units_sensors_and_findings_from_fixtures() {
        let mut view = DodView::new();
        assert_eq!(view.refresh_command().as_deref(), Some(REFRESH_COMMAND));
        view.apply_data(&status_fixture(1, true));
        view.apply_data(&sensors_fixture(true));
        view.apply_data(&findings_fixture(true));
        let text = screen(&view, 110, 40);
        for expected in [
            "UNITS",
            "harw-sentinel.service",
            "active/running",
            "harw-warden.socket",
            "systemd not available",
            "SENSORS",
            "tcp-connect",
            "degraded",
            "btf missing",
            "FINDINGS (newest first, 2 shown of 2 matching)",
            "egress.unexpected",
            "critical",
            "[harw.slice/job-1]",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert!(!text.contains("UNVERIFIED"), "{text}");
        assert!(!text.contains('\u{1b}'), "escape sequences are stripped");
    }

    #[test]
    fn dod_findings_are_coloured_by_severity() -> TestResult {
        let line = finding_line(
            findings_fixture(true)["findings"]
                .get(0)
                .ok_or(TestError::Missing("finding"))?,
            true,
            Theme::Dark,
        );
        let severity = line
            .spans
            .iter()
            .find(|span| span.content.trim() == "critical")
            .ok_or(TestError::Missing("severity span"))?;
        assert_eq!(severity.style.fg, Some(style::error_color(Theme::Dark)));
        assert!(severity.style.add_modifier.contains(Modifier::BOLD));
        Ok(())
    }

    #[test]
    fn dod_unverified_source_labels_every_finding_with_the_hint() {
        let mut view = DodView::new();
        view.apply_data(&status_fixture(1, false));
        view.apply_data(&sensors_fixture(false));
        view.apply_data(&findings_fixture(false));
        let lines: Vec<String> = view
            .lines(Theme::Dark)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let hint_lines = lines
            .iter()
            .filter(|line| line.contains("UNVERIFIED source (home): export below the harw home"))
            .count();
        assert_eq!(
            hint_lines, 2,
            "sensors and findings carry the hint: {lines:?}"
        );
        for rule in ["egress.unexpected", "exec.unusual", "tcp-connect", "exec "] {
            let line = lines
                .iter()
                .find(|line| line.contains(rule))
                .map_or("", String::as_str);
            assert!(line.contains("UNVERIFIED"), "{rule}: {line:?}");
        }
        assert!(
            lines
                .iter()
                .any(|line| line.contains("attention UNVERIFIED 1 high/critical")),
            "{lines:?}"
        );
    }

    #[test]
    fn dod_view_shows_missing_exports_and_errors() {
        let mut view = DodView::new();
        let text = screen(&view, 80, 20);
        assert!(text.contains("lade"), "{text}");
        view.apply_error("Unbekannter Command: /dod");
        view.apply_data(&json!({
            "state": "missing",
            "path": "/var/lib/harw-dod/export/health.json",
            "source": "system", "verified": true, "hint": null,
        }));
        let text = screen(&view, 100, 20);
        assert!(text.contains("health export missing"), "{text}");
        // Ein späterer erfolgreicher Abschnitt desselben Zyklus löscht den
        // Fehler des früheren nicht.
        assert!(text.contains("Unbekannter Command"), "{text}");
        view.apply_data(&findings_fixture(true));
        assert!(screen(&view, 100, 20).contains("Unbekannter Command"));
        // Erst der nächste Zyklus beginnt ohne die alten Fehler.
        view.apply_data(&status_fixture(0, true));
        let text = screen(&view, 100, 20);
        assert!(!text.contains("Unbekannter Command"), "{text}");
        view.apply_error("Fehler: keine Runtime-Montage");
        assert!(screen(&view, 100, 20).contains("keine Runtime-Montage"));
    }

    /// Bei jedem Auto-Reload scheitert der Status-Abruf, Sensoren und
    /// Befunde gelingen: der Fehler bleibt sichtbar, Zyklus für Zyklus.
    #[test]
    fn dod_view_keeps_a_section_error_across_reloads() {
        let mut view = DodView::new();
        for _ in 0..3 {
            view.apply_error("Zeitlimit überschritten");
            view.apply_data(&sensors_fixture(true));
            view.apply_data(&findings_fixture(true));
            let text = screen(&view, 100, 40);
            assert!(text.contains("Zeitlimit überschritten"), "{text}");
            assert_eq!(text.matches("Zeitlimit").count(), 1, "{text}");
        }
    }

    #[test]
    fn dod_view_scrolls_with_chat_scroll_from_the_top() {
        let mut view = DodView::new();
        view.apply_data(&status_fixture(0, true));
        view.apply_data(&sensors_fixture(true));
        view.apply_data(&findings_fixture(true));
        let first = screen(&view, 100, 8);
        assert!(first.contains("UNITS"), "starts at the top: {first}");
        assert_eq!(view.on_key(key(KeyCode::Char('G'))), OverlayOutcome::Stay);
        let bottom = screen(&view, 100, 8);
        assert!(!bottom.contains("UNITS"), "{bottom}");
        assert!(bottom.contains("exec.unusual"), "{bottom}");
        assert_eq!(view.on_key(key(KeyCode::Char('g'))), OverlayOutcome::Stay);
        assert!(screen(&view, 100, 8).contains("UNITS"));
        assert_eq!(
            view.on_key(key(KeyCode::Char('r'))),
            OverlayOutcome::Fetch(REFRESH_COMMAND.to_owned())
        );
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
    }

    #[test]
    fn dod_mark_reads_attention_and_hides_without_install() -> TestResult {
        let mark =
            DodMark::from_status(&status_fixture(2, true)).ok_or(TestError::Missing("mark"))?;
        assert_eq!(
            mark,
            DodMark {
                count: 2,
                lower_bound: false,
                verified: true
            }
        );
        assert_eq!(mark.count_text(), "2");
        let mut partial = status_fixture(3, false);
        partial["attention"]["lower_bound"] = json!(true);
        assert_eq!(
            DodMark::from_status(&partial).map(DodMark::count_text),
            Some("≥ 3".to_owned())
        );
        let mut not_installed = status_fixture(2, true);
        not_installed["installed"] = json!(false);
        not_installed["attention"] = Value::Null;
        assert_eq!(DodMark::from_status(&not_installed), None);
        let mut unreadable = status_fixture(2, true);
        unreadable["attention"] = Value::Null;
        assert_eq!(DodMark::from_status(&unreadable), None);

        let mut feed = DodMarkFeed::default();
        let now = Instant::now();
        assert!(feed.due(now));
        let (_tx, read) = tokio::sync::oneshot::channel();
        feed.begin(now, read);
        assert!(!feed.due(now));
        // `_tx` lebt: der Abruf läuft noch und blockiert keinen Neustart.
        assert!(!feed.due(now + MARK_REFRESH_INTERVAL));
        let ok = |data: Value| -> Result<OpOutput, String> {
            Ok(OpOutput {
                text: String::new(),
                data: Some(data),
            })
        };
        assert!(feed.apply(&ok(status_fixture(2, true))));
        assert_eq!(feed.visible().map(|mark| mark.count), Some(2));
        assert!(!feed.apply(&ok(status_fixture(2, true))));
        assert!(feed.apply(&ok(not_installed)), "not installed hides it");
        assert_eq!(feed.visible(), None);
        assert!(
            !feed.apply(&ok(status_fixture(0, true))),
            "zero stays hidden"
        );
        assert!(feed.apply(&ok(status_fixture(1, true))));
        assert!(feed.apply(&Err("Unbekannter Command".to_owned())));
        assert_eq!(feed.visible(), None);
        Ok(())
    }

    /// PL-97 D5: der Markenabruf läuft nebenläufig — solange er unterwegs
    /// ist, liefert `poll` nichts und `due` startet keinen zweiten; danach
    /// wird das Ergebnis übernommen und der nächste Abruf erst nach dem
    /// Intervall fällig.
    #[test]
    fn dod_mark_read_in_flight_never_blocks_or_restarts() {
        let mut feed = DodMarkFeed::default();
        let now = Instant::now();
        let (tx, read) = tokio::sync::oneshot::channel();
        feed.begin(now, read);
        assert!(feed.in_flight());
        assert_eq!(feed.poll(), None, "nothing arrived: returns at once");
        assert!(!feed.due(now + MARK_REFRESH_INTERVAL * 3));
        let sent = tx.send(Ok(OpOutput {
            text: String::new(),
            data: Some(status_fixture(2, true)),
        }));
        assert!(sent.is_ok());
        assert_eq!(feed.poll(), Some(true));
        assert!(!feed.in_flight());
        assert_eq!(feed.visible().map(|mark| mark.count), Some(2));
        assert!(!feed.due(now));
        assert!(feed.due(now + MARK_REFRESH_INTERVAL));

        // Ein Task, der ohne Antwort endet, blendet die Marke aus.
        let (tx, read) = tokio::sync::oneshot::channel::<Result<OpOutput, String>>();
        feed.begin(now, read);
        drop(tx);
        assert_eq!(feed.poll(), Some(true));
        assert_eq!(feed.visible(), None);
        assert!(!feed.in_flight());
    }
}

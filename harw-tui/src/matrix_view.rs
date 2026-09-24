//! Matrix-Game-Panel: Beobachter- und Sitz-Projektionen eines laufenden
//! Matrix-Spiels mit Facilitator-Tasten (`docs/design/matrix-game.md` §7).
//!
//! [`MatrixView`] implementiert [`OverlayView`]. Die Daten kommen aus
//! `OpOutput.data` von [`REFRESH_COMMAND`] (`/matrix show`) und werden
//! tolerant geparst. Reiter: „Beobachter“ (vollständiges Journal) und je Sitz
//! dessen Projektion („Was weiß dieser Sitz gerade?“) — das wichtigste
//! Werkzeug gegen Leaks. Nicht-öffentliche Einträge tragen ein Schloss und die
//! feste Kennzeichnung „nur für X & Y sichtbar“.
//!
//! Live-Ereignisse (`AgentEventKind::Matrix`) reicht `app.rs` über
//! [`OverlayView::apply_event`] weiter; die Ansicht merkt sich die letzte
//! Ereigniszeile und markiert sich als veraltet, `app.rs` lädt dann
//! [`REFRESH_COMMAND`] nach.
//!
//! Die Ansicht führt nie selbst etwas aus: Facilitator-Aktionen werden als
//! Slash-Zeilen zurückgegeben (`/matrix step`, `/matrix pause`, …).
//!
//! Erwartetes JSON:
//! `{"run_id","scenario","round","phase","status","seats":[{"id","name","role"}],`
//! `"observer":[E],"views":{"<seat_id>":[E]},"channels":[{"id","members":["a","b"]}]}`
//! mit `E = {"round","kind","audience","from","text"}`; `audience` ∈
//! `public|umpire|seat|seat_and_umpire|pair|observer`.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::overlay_view::{
    OverlayOutcome, OverlayView, content_height, is_down, is_up, plain, render_panel,
};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Lädt den Zustand des laufenden Spiels.
pub(crate) const REFRESH_COMMAND: &str = "/matrix show";
/// Eine Phase weiter.
pub(crate) const STEP_COMMAND: &str = "/matrix step";
/// Nach laufenden Aufrufen anhalten.
pub(crate) const PAUSE_COMMAND: &str = "/matrix pause";
/// Direkt zu Schlussargumenten/AAR.
pub(crate) const END_COMMAND: &str = "/matrix end";
/// Vorbelegung: N Runden automatisch.
pub(crate) const AUTO_PREFILL: &str = "/matrix auto ";
/// Vorbelegung: Ereignis einspielen.
pub(crate) const INJECT_PREFILL: &str = "/matrix inject ";
/// Vorbelegung: Adjudikation überschreiben.
pub(crate) const OVERRIDE_PREFILL: &str = "/matrix override ";
/// Vorbelegung: Argument verwerfen.
pub(crate) const VETO_PREFILL: &str = "/matrix veto ";
/// Vorbelegung: Geheimnis offenlegen.
pub(crate) const REVEAL_PREFILL: &str = "/matrix reveal ";
/// Vorbelegung: Spiel abzweigen.
pub(crate) const FORK_PREFILL: &str = "/matrix fork ";
/// Seitensprung für PageUp/PageDown.
const PAGE_STEP: usize = 10;
/// Tastenhinweis in der Fußzeile.
const FOOTER: &str = "Tab/←→ Reiter · j/k scrollen · s Schritt · a Auto N · p Pause · i Inject · o Override · v Veto · r Reveal · f Fork · e Ende · R neu laden · Esc";

/// Ein Sitz des Spiels.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MatrixSeat {
    pub id: String,
    pub name: String,
    pub role: String,
}

/// Ein Journal-Eintrag in einer Projektion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MatrixEntry {
    pub round: String,
    pub kind: String,
    pub audience: String,
    pub from: String,
    pub text: String,
    /// Optional: Kanal-ID eines Paar-Eintrags.
    pub channel: Option<String>,
    /// Optional: explizite Mitglieder (Paar) bzw. Empfänger (Sitz).
    pub members: Vec<String>,
}

/// Ein privater Paar-Kanal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MatrixChannel {
    pub id: String,
    pub members: Vec<String>,
}

/// Matrix-Game-Overlay.
#[derive(Debug, Clone, Default)]
pub(crate) struct MatrixView {
    run_id: String,
    scenario: String,
    round: String,
    phase: String,
    status: String,
    seats: Vec<MatrixSeat>,
    observer: Vec<MatrixEntry>,
    /// Projektionen je Sitz in Reiter-Reihenfolge.
    views: Vec<(String, Vec<MatrixEntry>)>,
    channels: Vec<MatrixChannel>,
    tab: usize,
    scroll: usize,
    /// Folgt dem Ende der Liste (neue Einträge bleiben sichtbar).
    follow: bool,
    loaded: bool,
    needs_refresh: bool,
    error: Option<String>,
    last_event: Option<String>,
    /// Zuletzt gezeichnete Inhaltshöhe/-breite (für das Scroll-Klemmen).
    last_height: Cell<usize>,
    last_width: Cell<usize>,
}

impl MatrixView {
    /// Leeres Panel; Daten folgen über [`OverlayView::apply_data`].
    pub(crate) fn new() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }

    /// `true`, wenn seit dem letzten Laden ein Live-Ereignis eintraf.
    #[cfg(test)]
    pub(crate) fn needs_refresh(&self) -> bool {
        self.needs_refresh
    }

    /// Reiterbeschriftungen: „Beobachter“ plus je Sitz.
    fn tab_labels(&self) -> Vec<String> {
        let mut labels = vec!["Beobachter".to_owned()];
        labels.extend(
            self.views
                .iter()
                .map(|(seat, _)| format!("Sitz: {}", self.display_name(seat))),
        );
        labels
    }

    fn tab_count(&self) -> usize {
        1 + self.views.len()
    }

    /// Sitz-ID des aktiven Reiters (`None` für den Beobachter).
    fn tab_seat(&self) -> Option<&str> {
        self.tab
            .checked_sub(1)
            .and_then(|index| self.views.get(index))
            .map(|(seat, _)| seat.as_str())
    }

    fn tab_entries(&self) -> &[MatrixEntry] {
        match self.tab.checked_sub(1) {
            None => &self.observer,
            Some(index) => self
                .views
                .get(index)
                .map_or(&[][..], |(_, entries)| entries.as_slice()),
        }
    }

    /// Anzeigename eines Sitzes (Name, sonst ID).
    fn display_name(&self, id: &str) -> String {
        let name = self
            .seats
            .iter()
            .find(|seat| seat.id == id)
            .map(|seat| seat.name.as_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(id);
        sanitize_inline(name)
    }

    fn join_names(&self, members: &[String]) -> String {
        members
            .iter()
            .map(|member| self.display_name(member))
            .collect::<Vec<_>>()
            .join(" & ")
    }

    /// Mitglieder eines Paar-Eintrags: explizit, über den Kanal, über den
    /// aktiven Sitz-Reiter oder über den eindeutigen Kanal des Absenders.
    fn pair_members(&self, entry: &MatrixEntry) -> Option<Vec<String>> {
        if entry.members.len() >= 2 {
            return Some(entry.members.clone());
        }
        if let Some(channel) = entry
            .channel
            .as_deref()
            .and_then(|id| self.channels.iter().find(|channel| channel.id == id))
            && !channel.members.is_empty()
        {
            return Some(channel.members.clone());
        }
        if let Some(seat) = self.tab_seat()
            && !entry.from.is_empty()
            && entry.from != seat
        {
            return Some(vec![seat.to_owned(), entry.from.clone()]);
        }
        if !entry.from.is_empty() {
            let mut own = self
                .channels
                .iter()
                .filter(|channel| channel.members.iter().any(|member| *member == entry.from));
            if let (Some(channel), None) = (own.next(), own.next()) {
                return Some(channel.members.clone());
            }
        }
        None
    }

    /// Empfänger eines Sitz-Eintrags.
    fn seat_target(&self, entry: &MatrixEntry) -> Option<String> {
        entry
            .members
            .first()
            .cloned()
            .or_else(|| self.tab_seat().map(str::to_owned))
            .or_else(|| (!entry.from.is_empty()).then(|| entry.from.clone()))
    }

    /// Feste Sichtbarkeitskennzeichnung eines Eintrags (`None` = öffentlich).
    fn visibility_note(&self, entry: &MatrixEntry) -> Option<String> {
        let audience = entry.audience.trim().to_ascii_lowercase();
        match audience.as_str() {
            "" | "public" => None,
            "observer" => Some("🔒 nur für Beobachter sichtbar".to_owned()),
            "umpire" => Some("🔒 nur für Umpire sichtbar".to_owned()),
            "seat" => Some(match self.seat_target(entry) {
                Some(seat) => format!("🔒 nur für {} sichtbar", self.display_name(&seat)),
                None => "🔒 nur für einen Sitz sichtbar".to_owned(),
            }),
            "seat_and_umpire" => Some(match self.seat_target(entry) {
                Some(seat) => format!("🔒 nur für {} & Umpire sichtbar", self.display_name(&seat)),
                None => "🔒 nur für einen Sitz & Umpire sichtbar".to_owned(),
            }),
            "pair" => Some(match self.pair_members(entry) {
                Some(members) => format!("🔒 nur für {} sichtbar", self.join_names(&members)),
                None => "🔒 nur für die Kanalmitglieder sichtbar".to_owned(),
            }),
            other => Some(format!("🔒 Sichtbarkeit: {}", sanitize_inline(other))),
        }
    }

    /// Alle Inhaltszeilen des aktiven Reiters, umbrochen auf `width`.
    fn content_lines(&self, width: usize, theme: Theme) -> Vec<Line<'static>> {
        let dim = style::dim_style(theme);
        let warn = style::warning_style(theme);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines: Vec<Line<'static>> = Vec::new();

        if let Some(error) = &self.error {
            lines.push(Line::styled(
                format!("Fehler: {}", sanitize_inline(error)),
                style::error_style(theme),
            ));
        }
        if !self.loaded {
            if self.error.is_none() {
                lines.push(Line::styled("Lade Matrix-Spiel …", dim));
            }
            return lines;
        }

        match self.tab_seat() {
            None => {
                lines.push(Line::styled(
                    "Beobachter: vollständiges Journal inklusive verdeckter Einträge.",
                    dim,
                ));
                for channel in &self.channels {
                    lines.push(self.channel_line(channel, warn));
                }
            }
            Some(seat) => {
                let role = self
                    .seats
                    .iter()
                    .find(|candidate| candidate.id == seat)
                    .map(|candidate| candidate.role.clone())
                    .unwrap_or_default();
                let mut info = format!(
                    "Projektion von {}: genau das, was dieser Sitz sieht.",
                    self.display_name(seat)
                );
                if !role.trim().is_empty() {
                    info.push_str(&format!(" Rolle: {}", sanitize_inline(&role)));
                }
                lines.push(Line::styled(info, dim));
                for channel in self
                    .channels
                    .iter()
                    .filter(|channel| channel.members.iter().any(|member| member == seat))
                {
                    lines.push(self.channel_line(channel, warn));
                }
            }
        }

        let entries = self.tab_entries();
        if entries.is_empty() {
            lines.push(Line::styled("Noch keine Einträge.", dim));
            return lines;
        }
        let text_width = width.saturating_sub(2).max(1);
        for entry in entries {
            let mut head = Vec::new();
            if !entry.round.is_empty() {
                head.push(Span::styled(
                    format!("r{} ", sanitize_inline(&entry.round)),
                    dim,
                ));
            }
            let from = if entry.from.is_empty() {
                "Ereignis".to_owned()
            } else {
                self.display_name(&entry.from)
            };
            head.push(Span::styled(from, bold));
            if !entry.kind.is_empty() {
                head.push(Span::styled(
                    format!(" ({})", sanitize_inline(&entry.kind)),
                    dim,
                ));
            }
            if let Some(note) = self.visibility_note(entry) {
                head.push(Span::styled(format!("  {note}"), warn));
            }
            lines.push(Line::from(head));
            for part in wrap(&sanitize_inline(&entry.text), text_width) {
                lines.push(Line::from(format!("  {part}")));
            }
        }
        lines
    }

    fn channel_line(&self, channel: &MatrixChannel, warn: Style) -> Line<'static> {
        let names = self.join_names(&channel.members);
        let label = channel
            .members
            .iter()
            .map(|member| self.display_name(member))
            .collect::<Vec<_>>()
            .join("⇄");
        let label = if label.is_empty() {
            sanitize_inline(&channel.id)
        } else {
            label
        };
        if names.is_empty() {
            return Line::styled(format!("── {label} 🔒 · privater Kanal"), warn);
        }
        Line::styled(format!("── {label} 🔒 · nur für {names} sichtbar"), warn)
    }

    fn tab_bar(&self, theme: Theme) -> Line<'static> {
        let mut spans = Vec::new();
        for (index, label) in self.tab_labels().into_iter().enumerate() {
            let style = if index == self.tab {
                style::selected_style(theme)
            } else {
                style::dim_style(theme)
            };
            spans.push(Span::styled(format!("[{label}]"), style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    }

    fn title(&self) -> String {
        let mut title = String::from(" Matrix");
        if !self.scenario.is_empty() {
            title.push_str(": ");
            title.push_str(&self.scenario);
        }
        if !self.round.is_empty() {
            title.push_str(&format!(" ─ Runde {}", self.round));
        }
        if !self.phase.is_empty() {
            title.push_str(&format!(" ─ Phase: {}", self.phase));
        }
        if !self.status.is_empty() {
            title.push_str(&format!(" ─ [{}]", self.status));
        }
        if self.needs_refresh {
            title.push_str(" ─ ⟳");
        }
        title.push(' ');
        title
    }

    /// Größter sinnvoller Scroll-Offset (nach der letzten Zeichnung).
    fn max_scroll(&self) -> usize {
        let width = self.last_width.get().max(1);
        let total = self.content_lines(width, Theme::Dark).len();
        total.saturating_sub(self.last_height.get().max(1))
    }

    fn effective_scroll(&self) -> usize {
        if self.follow { usize::MAX } else { self.scroll }
    }

    fn scroll_up(&mut self, step: usize) {
        let current = if self.follow {
            self.max_scroll()
        } else {
            self.scroll.min(self.max_scroll())
        };
        self.follow = false;
        self.scroll = current.saturating_sub(step);
    }

    fn scroll_down(&mut self, step: usize) {
        if self.follow {
            return;
        }
        let max = self.max_scroll();
        self.scroll = self.scroll.saturating_add(step).min(max);
        if self.scroll >= max {
            self.follow = true;
        }
    }

    fn switch_tab(&mut self, forward: bool) {
        let count = self.tab_count();
        self.tab = if forward {
            (self.tab + 1) % count
        } else {
            (self.tab + count - 1) % count
        };
        self.scroll = 0;
        self.follow = true;
    }
}

impl OverlayView for MatrixView {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        Clear.render(area, buf);
        let width = usize::from(area.width.saturating_sub(2));
        let height = content_height(area, true);
        self.last_width.set(width);
        self.last_height.set(height);
        let lines = self.content_lines(width, theme);
        let footer = match &self.last_event {
            Some(event) => format!("Zuletzt: {event} · {FOOTER}"),
            None => FOOTER.to_owned(),
        };
        render_panel(
            area,
            buf,
            theme,
            &self.title(),
            Some(self.tab_bar(theme)),
            &lines,
            self.effective_scroll(),
            &footer,
        );
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if key.kind == KeyEventKind::Release {
            return OverlayOutcome::Stay;
        }
        if is_down(&key) {
            self.scroll_down(1);
            return OverlayOutcome::Stay;
        }
        if is_up(&key) {
            self.scroll_up(1);
            return OverlayOutcome::Stay;
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Close,
            KeyCode::Tab | KeyCode::Right => {
                self.switch_tab(true);
                OverlayOutcome::Stay
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.switch_tab(false);
                OverlayOutcome::Stay
            }
            KeyCode::PageDown => {
                self.scroll_down(PAGE_STEP);
                OverlayOutcome::Stay
            }
            KeyCode::PageUp => {
                self.scroll_up(PAGE_STEP);
                OverlayOutcome::Stay
            }
            KeyCode::Char(ch) if plain(&key) => match ch {
                'q' => OverlayOutcome::Close,
                'g' => {
                    self.follow = false;
                    self.scroll = 0;
                    OverlayOutcome::Stay
                }
                'G' => {
                    self.follow = true;
                    OverlayOutcome::Stay
                }
                's' => OverlayOutcome::Run(STEP_COMMAND.to_owned()),
                'p' => OverlayOutcome::Run(PAUSE_COMMAND.to_owned()),
                'e' => OverlayOutcome::Run(END_COMMAND.to_owned()),
                'a' => OverlayOutcome::Prefill(AUTO_PREFILL.to_owned()),
                'i' => OverlayOutcome::Prefill(INJECT_PREFILL.to_owned()),
                'o' => OverlayOutcome::Prefill(OVERRIDE_PREFILL.to_owned()),
                'v' => OverlayOutcome::Prefill(VETO_PREFILL.to_owned()),
                'r' => OverlayOutcome::Prefill(REVEAL_PREFILL.to_owned()),
                'f' => OverlayOutcome::Prefill(FORK_PREFILL.to_owned()),
                'R' => OverlayOutcome::Fetch(REFRESH_COMMAND.to_owned()),
                _ => OverlayOutcome::Stay,
            },
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(REFRESH_COMMAND.to_owned())
    }

    fn apply_data(&mut self, data: &Value) {
        let previous_seat = self.tab_seat().map(str::to_owned);
        self.run_id = str_field(data, "run_id");
        self.scenario = str_field(data, "scenario");
        self.round = str_field(data, "round");
        self.phase = str_field(data, "phase");
        self.status = str_field(data, "status");
        self.seats = array_field(data, "seats")
            .iter()
            .filter(|seat| seat.is_object())
            .map(|seat| MatrixSeat {
                id: str_field(seat, "id"),
                name: str_field(seat, "name"),
                role: str_field(seat, "role"),
            })
            .filter(|seat| !seat.id.is_empty())
            .collect();
        self.observer = entries(data.get("observer"));
        self.channels = array_field(data, "channels")
            .iter()
            .filter(|channel| channel.is_object())
            .map(|channel| MatrixChannel {
                id: str_field(channel, "id"),
                members: string_list(channel.get("members")),
            })
            .filter(|channel| !channel.id.is_empty() || !channel.members.is_empty())
            .collect();

        // Reiter-Reihenfolge: Sitze in Datenreihenfolge, danach übrige
        // Projektionen (z. B. ohne Sitz-Eintrag).
        let views = data.get("views").and_then(Value::as_object);
        let mut ordered: Vec<(String, Vec<MatrixEntry>)> = Vec::new();
        for seat in &self.seats {
            let list = views.and_then(|map| map.get(&seat.id));
            ordered.push((seat.id.clone(), entries(list)));
        }
        if let Some(map) = views {
            for (seat, list) in map {
                if !seat.trim().is_empty() && ordered.iter().all(|(known, _)| known != seat) {
                    ordered.push((seat.clone(), entries(Some(list))));
                }
            }
        }
        self.views = ordered;

        self.tab = previous_seat
            .and_then(|seat| self.views.iter().position(|(id, _)| *id == seat))
            .map_or(0, |index| index + 1);
        if self.tab >= self.tab_count() {
            self.tab = 0;
        }
        self.loaded = true;
        self.needs_refresh = false;
        self.error = None;
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
        self.needs_refresh = false;
    }

    fn apply_event(&mut self, event: &Value) {
        let Some(payload) = event.get("event") else {
            return;
        };
        let run_id = str_field(event, "run_id");
        let mut line = String::new();
        if !run_id.is_empty() && run_id != self.run_id {
            line.push_str(&format!("[{}] ", sanitize_inline(&run_id)));
        }
        let round = str_field(payload, "round");
        if !round.is_empty() {
            line.push_str(&format!("r{} ", sanitize_inline(&round)));
        }
        let kind = str_field(payload, "kind");
        if !kind.is_empty() {
            line.push_str(&sanitize_inline(&kind));
            line.push(' ');
        }
        let from = str_field(payload, "from");
        if !from.is_empty() {
            line.push_str(&self.display_name(&from));
            line.push_str(": ");
        }
        let text = str_field(payload, "text");
        line.push_str(&truncate(&sanitize_inline(&text), 60));
        let line = line.trim().to_owned();
        self.last_event = Some(if line.is_empty() {
            "Ereignis".to_owned()
        } else {
            line
        });
        self.needs_refresh = true;
    }
}

/// Bricht `text` auf höchstens `width` Terminalzellen je Zeile um (an
/// Leerzeichen, sonst hart).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split(' ') {
        let word_width: usize = word.chars().map(|c| c.width().unwrap_or(0)).sum();
        let sep = usize::from(!current.is_empty());
        if used + sep + word_width <= width {
            if sep == 1 {
                current.push(' ');
            }
            current.push_str(word);
            used += sep + word_width;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
            used = 0;
        }
        for ch in word.chars() {
            let w = ch.width().unwrap_or(0);
            if used + w > width && !current.is_empty() {
                out.push(std::mem::take(&mut current));
                used = 0;
            }
            current.push(ch);
            used += w;
        }
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

/// Kürzt `text` auf höchstens `max` Zeichen (mit `…`).
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn entries(value: Option<&Value>) -> Vec<MatrixEntry> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.is_object())
                .map(|item| {
                    let mut members = string_list(item.get("members"));
                    if members.is_empty() {
                        members = string_list(item.get("to"));
                    }
                    if members.is_empty() {
                        members = string_list(item.get("seat"));
                    }
                    MatrixEntry {
                        round: str_field(item, "round"),
                        kind: str_field(item, "kind"),
                        audience: str_field(item, "audience"),
                        from: str_field(item, "from"),
                        text: str_field(item, "text"),
                        channel: opt_str_field(item, "channel"),
                        members,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Array aus Strings (andere Elemente übersprungen) oder ein einzelner String.
fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .filter(|text| !text.trim().is_empty())
            .collect(),
        Some(Value::String(text)) if !text.trim().is_empty() => vec![text.clone()],
        _ => Vec::new(),
    }
}

fn str_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    let text = str_field(value, key);
    (!text.trim().is_empty()).then_some(text)
}

fn array_field<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay_view::buffer_text;
    use crossterm::event::KeyModifiers;
    use serde_json::json;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample() -> Value {
        json!({
            "run_id": "run-1",
            "scenario": "Karst-Inseln",
            "round": 3,
            "phase": "adjudikation",
            "status": "läuft",
            "seats": [
                {"id": "rat", "name": "Inselrat", "role": "player"},
                {"id": "nord", "name": "Nordreich", "role": "player"},
                {"id": "gilde", "name": "Gilde", "role": "player"},
                "kaputt"
            ],
            "observer": [
                {"round": 3, "kind": "argument", "audience": "public", "from": "gilde", "text": "Übernimmt die Anlage"},
                {"round": 3, "kind": "message", "audience": "pair", "from": "rat", "text": "Tankschiffe ohne Fahnen", "channel": "rat-nord"},
                {"round": 3, "kind": "note", "audience": "umpire", "from": "umpire", "text": "Sabotagerisiko"},
                7
            ],
            "views": {
                "rat": [
                    {"round": 3, "kind": "message", "audience": "pair", "from": "nord", "text": "Landerecht am Nordkai"},
                    {"round": 3, "kind": "secret", "audience": "seat_and_umpire", "from": "rat", "text": "Geheimplan"}
                ],
                "nord": [],
                "extra": [{"text": "x"}]
            },
            "channels": [{"id": "rat-nord", "members": ["rat", "nord"]}, {"id": "leer"}]
        })
    }

    fn loaded() -> MatrixView {
        let mut view = MatrixView::new();
        view.apply_data(&sample());
        view
    }

    fn render(view: &MatrixView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, Theme::Dark);
        buffer_text(&buf)
    }

    #[test]
    fn parses_tolerantly_and_orders_tabs() {
        let view = loaded();
        assert_eq!(view.seats.len(), 3);
        assert_eq!(view.observer.len(), 3);
        assert_eq!(view.round, "3");
        let tabs = view.tab_labels();
        assert_eq!(
            tabs,
            [
                "Beobachter",
                "Sitz: Inselrat",
                "Sitz: Nordreich",
                "Sitz: Gilde",
                "Sitz: extra"
            ]
        );
        assert_eq!(view.channels.len(), 2);
        assert_eq!(view.refresh_command().as_deref(), Some("/matrix show"));
    }

    #[test]
    fn garbage_data_yields_empty_view() {
        let mut view = MatrixView::new();
        view.apply_data(&json!({"seats": 3, "views": [1], "observer": {"x": 1}}));
        assert!(view.seats.is_empty());
        assert!(view.views.is_empty());
        assert_eq!(view.tab_count(), 1);
        view.on_key(key(KeyCode::Tab));
        assert_eq!(view.tab, 0);
        assert!(render(&view, 80, 12).contains("Noch keine Einträge"));
    }

    #[test]
    fn pair_entries_are_marked_with_lock_and_members() {
        let mut view = loaded();
        let pair = view.observer[1].clone();
        assert_eq!(
            view.visibility_note(&pair).as_deref(),
            Some("🔒 nur für Inselrat & Nordreich sichtbar")
        );
        assert_eq!(view.visibility_note(&view.observer[0]), None);
        // Sitz-Reiter: Mitglieder aus Sitz und Absender.
        view.on_key(key(KeyCode::Tab));
        assert_eq!(view.tab_seat(), Some("rat"));
        let entries = view.tab_entries().to_vec();
        assert_eq!(
            view.visibility_note(&entries[0]).as_deref(),
            Some("🔒 nur für Inselrat & Nordreich sichtbar")
        );
        assert_eq!(
            view.visibility_note(&entries[1]).as_deref(),
            Some("🔒 nur für Inselrat & Umpire sichtbar")
        );
        let text = render(&view, 120, 20);
        assert!(text.contains("nur für Inselrat & Nordreich sichtbar"));
        assert!(text.contains("Landerecht am Nordkai"));
        // Fremde Sitz-Projektion zeigt keine Einträge des Rats.
        view.on_key(key(KeyCode::Tab));
        let text = render(&view, 120, 20);
        assert!(!text.contains("Geheimplan"));
    }

    #[test]
    fn header_shows_scenario_round_phase_and_status() {
        let view = loaded();
        let text = render(&view, 140, 20);
        assert!(text.contains("Matrix: Karst-Inseln"));
        assert!(text.contains("Runde 3"));
        assert!(text.contains("Phase: adjudikation"));
        assert!(text.contains("[läuft]"));
        assert!(text.contains("[Beobachter]"));
        assert!(text.contains("Übernimmt die Anlage"));
        assert!(render(&MatrixView::new(), 80, 10).contains("Lade Matrix-Spiel"));
    }

    #[test]
    fn facilitator_keys_emit_slash_lines() {
        let mut view = loaded();
        let cases = [
            ('s', OverlayOutcome::Run("/matrix step".to_owned())),
            ('p', OverlayOutcome::Run("/matrix pause".to_owned())),
            ('e', OverlayOutcome::Run("/matrix end".to_owned())),
            ('a', OverlayOutcome::Prefill("/matrix auto ".to_owned())),
            ('i', OverlayOutcome::Prefill("/matrix inject ".to_owned())),
            ('R', OverlayOutcome::Fetch("/matrix show".to_owned())),
            ('x', OverlayOutcome::Stay),
        ];
        for (ch, expected) in cases {
            assert_eq!(view.on_key(key(KeyCode::Char(ch))), expected, "{ch}");
        }
        assert_eq!(view.on_key(key(KeyCode::Esc)), OverlayOutcome::Close);
        assert_eq!(
            view.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            OverlayOutcome::Stay
        );
    }

    #[test]
    fn tabs_cycle_and_keep_seat_after_reload() {
        let mut view = loaded();
        view.on_key(key(KeyCode::Left));
        assert_eq!(view.tab, 4);
        view.on_key(key(KeyCode::Right));
        assert_eq!(view.tab, 0);
        view.on_key(key(KeyCode::Right));
        view.on_key(key(KeyCode::Right));
        assert_eq!(view.tab_seat(), Some("nord"));
        view.apply_data(&sample());
        assert_eq!(view.tab_seat(), Some("nord"));
    }

    #[test]
    fn scrolling_clamps_and_follows() {
        let mut data = sample();
        let many: Vec<Value> = (0..40)
            .map(|i| json!({"round": 1, "audience": "public", "from": "rat", "text": format!("Zeile {i}")}))
            .collect();
        data["observer"] = Value::Array(many);
        let mut view = MatrixView::new();
        view.apply_data(&data);
        let text = render(&view, 80, 12);
        assert!(text.contains("Zeile 39"), "folgt dem Ende");
        view.on_key(key(KeyCode::Char('k')));
        assert!(!view.follow);
        let max = view.max_scroll();
        assert_eq!(view.scroll, max - 1);
        view.on_key(key(KeyCode::Char('g')));
        assert!(render(&view, 80, 12).contains("Zeile 0"));
        for _ in 0..200 {
            view.on_key(key(KeyCode::Char('j')));
        }
        assert!(view.follow);
        assert!(view.scroll <= max);
    }

    #[test]
    fn apply_event_marks_stale_and_records_footer() {
        let mut view = loaded();
        view.apply_event(&json!({"no": "event"}));
        assert!(!view.needs_refresh());
        view.apply_event(&json!({
            "run_id": "run-1",
            "event": {"round": 4, "kind": "roll", "from": "gilde", "text": "Würfel 4+5"}
        }));
        assert!(view.needs_refresh());
        assert_eq!(
            view.last_event.as_deref(),
            Some("r4 roll Gilde: Würfel 4+5")
        );
        assert!(render(&view, 160, 12).contains("Zuletzt: r4 roll Gilde"));
        view.apply_data(&sample());
        assert!(!view.needs_refresh());
    }

    #[test]
    fn wrap_breaks_long_text() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("", 3), [""]);
    }
}

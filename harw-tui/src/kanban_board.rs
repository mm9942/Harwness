//! Kanban-Overlay: Spalten (Lanes) mit Karten, Detailansicht und
//! Schreibaktionen über Slash-Zeilen.
//!
//! [`KanbanBoard`] implementiert [`OverlayView`]. Die Daten kommen aus
//! `OpOutput.data` von [`REFRESH_COMMAND`] (`/kanban show`, nach einer
//! Board-Auswahl `/kanban --board=<id> show`) und werden tolerant geparst.
//! Verschieben (`>`/`<`) nutzt die Reihenfolge der Status-Lanes: Ziel ist der
//! `state` der nächsten bzw. vorigen Lane.
//!
//! # Plan D2
//! - **Board-Auswahl** (`B`): lädt `/kanban boards` (`{"boards":[{"id","name"}]}`)
//!   und zeigt eine Liste; `Enter` wechselt das Board. Danach tragen alle
//!   Befehle `--board=<id>`.
//! - **Worker-Lanes nach Rolle gruppiert**: erst die Status-Lanes in
//!   Datenreihenfolge, dann die Worker-Lanes nach Rolle sortiert, mit Risiko.
//! - **Freigabe an der Karte**: wartende Karten (`awaiting_approval`) tragen
//!   `⏳`; `f` löst `/kanban approve <karte>` aus, `x` belegt
//!   `/kanban reject <karte> ` vor.
//! - **Vorbelegungen**: `c` Kommentar, `e` Text (mit dem bisherigen Text,
//!   sofern vollständig geliefert), `v` Belegverweis.
//! - **Live-Update**: `app.rs` lädt eine offene Ansicht bei
//!   `AgentEventKind::Knowledge { area: "kanban" }` über
//!   [`OverlayView::refresh_command`] nach (Präfix `/kanban`).
//!
//! Erwartetes JSON:
//! `{"board":{"id","name"},"lanes":[{"id","title","kind","state","worker_role","risk"}],`
//! `"cards":[{"id","lane_id","title","state","assignee","tags","retry_count","blocked_reason","work_id",`
//! `"body","body_complete","awaiting_approval","comment_count","evidence","result_status"}]}`

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use crate::overlay_view::{OverlayOutcome, OverlayView};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Lädt das Board.
pub(crate) const REFRESH_COMMAND: &str = "/kanban show";
/// Verschiebt eine Karte (`<cmd> <karte> <status>`).
pub(crate) const MOVE_COMMAND: &str = "/kanban move";
/// Markiert eine Karte als erledigt (`<cmd> <karte>`).
pub(crate) const DONE_COMMAND: &str = "/kanban done";
/// Hebt eine Blockade auf (`<cmd> <karte>`).
pub(crate) const UNBLOCK_COMMAND: &str = "/kanban unblock";
/// Archiviert eine Karte (`<cmd> <karte>`).
pub(crate) const ARCHIVE_COMMAND: &str = "/kanban archive";
/// Blockiert eine Karte (`<cmd> <karte> <art>`), wird vorbelegt.
pub(crate) const BLOCK_COMMAND: &str = "/kanban block";
/// Vorbelegung für eine neue Karte.
pub(crate) const ADD_PREFILL: &str = "/kanban add ";
/// Listet die Boards (Board-Auswahl).
pub(crate) const BOARDS_COMMAND: &str = "/kanban boards";
/// Gibt eine wartende Worker-Karte frei (`<cmd> <karte>`).
pub(crate) const APPROVE_COMMAND: &str = "/kanban approve";
/// Lehnt eine wartende Worker-Karte ab (`<cmd> <karte> <grund>`), wird vorbelegt.
pub(crate) const REJECT_COMMAND: &str = "/kanban reject";
/// Kommentar (`<cmd> <karte> <text>`), wird vorbelegt.
pub(crate) const COMMENT_COMMAND: &str = "/kanban comment";
/// Kartentext ersetzen (`<cmd> <karte> <text>`), wird vorbelegt.
pub(crate) const EDIT_COMMAND: &str = "/kanban edit";
/// Belegverweis (`<cmd> <karte> <pfad|url>`), wird vorbelegt.
pub(crate) const EVIDENCE_COMMAND: &str = "/kanban evidence";
/// Höhe der Board-Auswahl (inklusive Rahmen).
const PICKER_MAX_HEIGHT: u16 = 12;
/// Mindestbreite einer Spalte in Zellen.
const MIN_LANE_WIDTH: u16 = 18;
/// Höhe der Detailansicht (inklusive Rahmen).
const DETAIL_HEIGHT: u16 = 12;

/// Eine Spalte des Boards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Lane {
    pub id: String,
    pub title: String,
    /// `status` oder `worker`.
    pub kind: String,
    pub state: String,
    pub worker_role: Option<String>,
    /// Risiko der Worker-Rolle (`low|medium|high`), nur an Worker-Lanes.
    pub risk: Option<String>,
}

impl Lane {
    fn is_status(&self) -> bool {
        !self.kind.eq_ignore_ascii_case("worker")
    }

    /// Zielstatus beim Verschieben in diese Lane.
    fn target_state(&self) -> &str {
        if self.state.is_empty() {
            &self.id
        } else {
            &self.state
        }
    }

    fn display_title(&self) -> String {
        let base = if self.title.is_empty() {
            if self.state.is_empty() {
                &self.id
            } else {
                &self.state
            }
        } else {
            &self.title
        };
        match (&self.worker_role, &self.risk) {
            (Some(role), Some(risk)) if !self.is_status() => format!("{base} @{role} [{risk}]"),
            (Some(role), None) if !self.is_status() => format!("{base} @{role}"),
            _ => base.clone(),
        }
    }
}

/// Eine Karte.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Card {
    pub id: String,
    pub lane_id: String,
    pub title: String,
    pub state: String,
    pub assignee: Option<String>,
    pub tags: Vec<String>,
    pub retry_count: u64,
    pub blocked_reason: Option<String>,
    pub work_id: Option<String>,
    /// Kartentext (ggf. gekürzt, siehe `body_complete`).
    pub body: String,
    /// `true`, wenn `body` vollständig ist (nur dann wird er für `e` vorbelegt).
    pub body_complete: bool,
    /// Die Karte wartet auf die Freigabe des Operators.
    pub awaiting_approval: bool,
    pub comment_count: u64,
    pub evidence: Vec<String>,
    /// Ausgang des letzten Worker-Laufs (`succeeded|partial|failed`).
    pub result_status: Option<String>,
}

/// Ein Eintrag der Board-Auswahl.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BoardEntry {
    pub id: String,
    pub name: String,
}

/// Kanban-Overlay.
#[derive(Debug, Clone, Default)]
pub(crate) struct KanbanBoard {
    board_id: String,
    board_name: String,
    lanes: Vec<Lane>,
    cards: Vec<Card>,
    lane_index: usize,
    card_index: usize,
    detail: bool,
    loaded: bool,
    error: Option<String>,
    hint: Option<String>,
    /// Vom Nutzer gewähltes Board; `None` = Vorgabe der Op (`default`).
    selected_board: Option<String>,
    /// Offene Board-Auswahl (Liste aus `/kanban boards`).
    picker: Option<Vec<BoardEntry>>,
    picker_index: usize,
    /// `true`, solange die Board-Liste geladen wird.
    picker_loading: bool,
}

impl KanbanBoard {
    /// Leeres Board; Daten folgen über [`OverlayView::apply_data`].
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Lanes (für Tests).
    #[cfg(test)]
    pub(crate) fn lanes(&self) -> &[Lane] {
        &self.lanes
    }

    /// Karten (für Tests).
    #[cfg(test)]
    pub(crate) fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// Baut `/kanban [--board=<id>] <rest>` aus einer `/kanban <rest>`-Konstante.
    fn command(&self, base: &str) -> String {
        match (&self.selected_board, base.strip_prefix("/kanban ")) {
            (Some(board), Some(rest)) => format!("/kanban --board={board} {rest}"),
            _ => base.to_owned(),
        }
    }

    /// Gewähltes Board (für Tests).
    #[cfg(test)]
    pub(crate) fn selected_board(&self) -> Option<&str> {
        self.selected_board.as_deref()
    }

    fn with_selected_card(
        &mut self,
        make: impl FnOnce(&Self, &Card) -> OverlayOutcome,
    ) -> OverlayOutcome {
        match self.selected_card().cloned() {
            Some(card) if !card.id.is_empty() => {
                self.hint = None;
                make(self, &card)
            }
            _ => {
                self.hint = Some("Keine Karte ausgewählt.".to_owned());
                OverlayOutcome::Stay
            }
        }
    }

    fn approve_card(&mut self) -> OverlayOutcome {
        match self.selected_card().cloned() {
            Some(card) if card.awaiting_approval && !card.id.is_empty() => {
                self.hint = None;
                OverlayOutcome::Run(format!("{} {}", self.command(APPROVE_COMMAND), card.id))
            }
            Some(_) => {
                self.hint = Some("Diese Karte wartet nicht auf Freigabe.".to_owned());
                OverlayOutcome::Stay
            }
            None => {
                self.hint = Some("Keine Karte ausgewählt.".to_owned());
                OverlayOutcome::Stay
            }
        }
    }

    fn picker_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        let count = self.picker.as_ref().map_or(0, Vec::len);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('B') => {
                self.picker = None;
                self.picker_loading = false;
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.picker_index = self.picker_index.saturating_sub(1);
                OverlayOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.picker_index + 1 < count {
                    self.picker_index += 1;
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                let chosen = self
                    .picker
                    .as_ref()
                    .and_then(|boards| boards.get(self.picker_index))
                    .map(|entry| entry.id.clone());
                self.picker = None;
                match chosen {
                    Some(id) if !id.is_empty() => {
                        self.selected_board = Some(id);
                        self.lane_index = 0;
                        self.card_index = 0;
                        self.detail = false;
                        self.loaded = false;
                        OverlayOutcome::Fetch(self.refresh())
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
            _ => OverlayOutcome::Stay,
        }
    }

    /// Der aktuelle Nachlade-Befehl.
    fn refresh(&self) -> String {
        self.command(REFRESH_COMMAND)
    }

    /// Indizes der Karten einer Lane in Datenreihenfolge.
    fn lane_cards(&self, lane_index: usize) -> Vec<usize> {
        let Some(lane) = self.lanes.get(lane_index) else {
            return Vec::new();
        };
        self.cards
            .iter()
            .enumerate()
            .filter(|(_, card)| {
                if card.lane_id.is_empty() {
                    !card.state.is_empty() && card.state == lane.state && lane.is_status()
                } else {
                    card.lane_id == lane.id
                }
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_card(&self) -> Option<&Card> {
        let indices = self.lane_cards(self.lane_index);
        indices
            .get(self.card_index)
            .and_then(|index| self.cards.get(*index))
    }

    fn clamp(&mut self) {
        if self.lanes.is_empty() {
            self.lane_index = 0;
        } else if self.lane_index >= self.lanes.len() {
            self.lane_index = self.lanes.len() - 1;
        }
        let count = self.lane_cards(self.lane_index).len();
        if count == 0 {
            self.card_index = 0;
            self.detail = false;
        } else if self.card_index >= count {
            self.card_index = count - 1;
        }
    }

    /// Index der Status-Lane, in der die Karte gerade steht.
    fn status_lane_of(&self, card: &Card) -> Option<usize> {
        self.lanes
            .iter()
            .position(|lane| {
                lane.is_status() && !card.lane_id.is_empty() && lane.id == card.lane_id
            })
            .or_else(|| {
                self.lanes.iter().position(|lane| {
                    lane.is_status() && !card.state.is_empty() && lane.target_state() == card.state
                })
            })
            .or_else(|| {
                self.lanes
                    .get(self.lane_index)
                    .filter(|lane| lane.is_status())
                    .map(|_| self.lane_index)
            })
    }

    /// Zielstatus beim Verschieben um `forward` (true = nächste Lane).
    fn move_target(&self, card: &Card, forward: bool) -> Option<String> {
        let current = self.status_lane_of(card)?;
        let found = if forward {
            self.lanes
                .iter()
                .enumerate()
                .skip(current + 1)
                .find(|(_, lane)| lane.is_status())
        } else {
            self.lanes
                .iter()
                .enumerate()
                .take(current)
                .rev()
                .find(|(_, lane)| lane.is_status())
        };
        found.map(|(_, lane)| lane.target_state().to_owned())
    }

    fn card_action(&mut self, command: &str) -> OverlayOutcome {
        let command = self.command(command);
        match self.selected_card().map(|card| card.id.clone()) {
            Some(id) if !id.is_empty() => {
                self.hint = None;
                OverlayOutcome::Run(format!("{command} {id}"))
            }
            _ => {
                self.hint = Some("Keine Karte ausgewählt.".to_owned());
                OverlayOutcome::Stay
            }
        }
    }

    fn move_card(&mut self, forward: bool) -> OverlayOutcome {
        let Some(card) = self.selected_card().cloned() else {
            self.hint = Some("Keine Karte ausgewählt.".to_owned());
            return OverlayOutcome::Stay;
        };
        match self.move_target(&card, forward) {
            Some(state) if !card.id.is_empty() => {
                self.hint = None;
                OverlayOutcome::Run(format!(
                    "{} {} {state}",
                    self.command(MOVE_COMMAND),
                    card.id
                ))
            }
            _ => {
                self.hint = Some(if forward {
                    "Keine nächste Status-Spalte.".to_owned()
                } else {
                    "Keine vorige Status-Spalte.".to_owned()
                });
                OverlayOutcome::Stay
            }
        }
    }

    fn render_lane(&self, lane_index: usize, area: Rect, buf: &mut Buffer, theme: Theme) {
        let Some(lane) = self.lanes.get(lane_index) else {
            return;
        };
        let active = lane_index == self.lane_index;
        let indices = self.lane_cards(lane_index);
        let border = if active {
            Style::default().fg(style::accent_color(theme))
        } else {
            Style::default().fg(style::border_color(theme))
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(format!(
                " {} ({}) ",
                sanitize_inline(&lane.display_title()),
                indices.len()
            ));
        let width = usize::from(area.width.saturating_sub(2));
        let inner_height = usize::from(area.height.saturating_sub(2));
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected_line = 0;
        if indices.is_empty() {
            lines.push(Line::styled("–", dim));
        }
        for (position, card_index) in indices.iter().enumerate() {
            let Some(card) = self.cards.get(*card_index) else {
                continue;
            };
            let selected = active && position == self.card_index;
            if selected {
                selected_line = lines.len();
            }
            let marker = if selected { "▸ " } else { "  " };
            let title_style = if selected {
                style::selected_style(theme)
            } else if card.awaiting_approval {
                style::warning_style(theme)
            } else if card.blocked_reason.is_some() {
                style::error_style(theme)
            } else {
                Style::default()
            };
            let title = sanitize_inline(if card.title.is_empty() {
                &card.id
            } else {
                &card.title
            });
            lines.push(Line::from(vec![
                Span::raw(marker),
                Span::styled(truncate(&title, width.saturating_sub(2)), title_style),
            ]));
            let mut meta = Vec::new();
            if card.awaiting_approval {
                meta.push("⏳ Freigabe (f)".to_owned());
            } else if card.blocked_reason.is_some() {
                meta.push("⛔".to_owned());
            }
            if let Some(status) = &card.result_status {
                meta.push(result_marker(status).to_owned());
            }
            if card.comment_count > 0 {
                meta.push(format!("💬{}", card.comment_count));
            }
            if let Some(assignee) = &card.assignee {
                meta.push(format!("@{}", sanitize_inline(assignee)));
            }
            if card.retry_count > 0 {
                meta.push(format!("↻{}", card.retry_count));
            }
            if !meta.is_empty() {
                lines.push(Line::styled(
                    truncate(&format!("    {}", meta.join(" ")), width),
                    dim,
                ));
            }
        }
        let scroll = (selected_line + 2).saturating_sub(inner_height);
        Paragraph::new(lines)
            .block(block)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn render_detail(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(" Karte ");
        let dim = style::dim_style(theme);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines: Vec<Line<'static>> = Vec::new();
        match self.selected_card() {
            None => lines.push(Line::styled("Keine Karte ausgewählt.", dim)),
            Some(card) => {
                lines.push(Line::from(vec![
                    Span::styled(sanitize_inline(&card.title), bold),
                    Span::styled(format!("  #{}", sanitize_inline(&card.id)), dim),
                ]));
                let field = |label: &str, value: String| {
                    Line::from(vec![
                        Span::styled(format!("{label}: "), dim),
                        Span::raw(value),
                    ])
                };
                if !card.state.is_empty() {
                    lines.push(field("Status", sanitize_inline(&card.state)));
                }
                if let Some(assignee) = &card.assignee {
                    lines.push(field("Zuständig", sanitize_inline(assignee)));
                }
                if !card.tags.is_empty() {
                    lines.push(field("Tags", sanitize_inline(&card.tags.join(", "))));
                }
                if card.retry_count > 0 {
                    lines.push(field("Wiederholungen", card.retry_count.to_string()));
                }
                if card.awaiting_approval {
                    lines.push(Line::styled(
                        "Wartet auf Freigabe: f freigeben · x ablehnen",
                        style::warning_style(theme),
                    ));
                } else if let Some(reason) = &card.blocked_reason {
                    lines.push(Line::from(vec![
                        Span::styled("Blockiert: ", style::error_style(theme)),
                        Span::raw(sanitize_inline(reason)),
                    ]));
                }
                if let Some(status) = &card.result_status {
                    lines.push(field("Ergebnis", result_label(status).to_owned()));
                }
                if card.comment_count > 0 {
                    lines.push(field("Kommentare", card.comment_count.to_string()));
                }
                if !card.evidence.is_empty() {
                    lines.push(field("Belege", sanitize_inline(&card.evidence.join(", "))));
                }
                if let Some(work_id) = &card.work_id {
                    lines.push(field("Arbeit", sanitize_inline(work_id)));
                }
                if !card.body.trim().is_empty() {
                    lines.push(Line::styled(
                        sanitize_inline(card.body.lines().next().unwrap_or_default()),
                        dim,
                    ));
                }
            }
        }
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl KanbanBoard {
    fn render_picker(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let entries = self.picker.as_deref().unwrap_or_default();
        let height = u16::try_from(entries.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .clamp(3, PICKER_MAX_HEIGHT)
            .min(area.height);
        let width = area.width.min(48);
        let popup = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y,
            width,
            height,
        };
        Clear.render(popup, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(" Board wählen (Enter) ");
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.picker_loading && entries.is_empty() {
            lines.push(Line::styled("Lade Boards …", dim));
        } else if entries.is_empty() {
            lines.push(Line::styled("Keine Boards.", dim));
        }
        let current = self
            .selected_board
            .as_deref()
            .unwrap_or(self.board_id.as_str());
        for (index, entry) in entries.iter().enumerate() {
            let selected = index == self.picker_index;
            let marker = if selected { "▸ " } else { "  " };
            let active = if entry.id == current { " •" } else { "" };
            let label = if entry.name.is_empty() || entry.name == entry.id {
                sanitize_inline(&entry.id)
            } else {
                format!(
                    "{} ({})",
                    sanitize_inline(&entry.name),
                    sanitize_inline(&entry.id)
                )
            };
            let line_style = if selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            lines.push(Line::styled(format!("{marker}{label}{active}"), line_style));
        }
        let inner_height = usize::from(height.saturating_sub(2));
        let scroll = (self.picker_index + 1).saturating_sub(inner_height);
        Paragraph::new(lines)
            .block(block)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(popup, buf);
    }
}

impl OverlayView for KanbanBoard {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        Clear.render(area, buf);
        let mut title = String::from(" Kanban");
        let name = if self.board_name.is_empty() {
            &self.board_id
        } else {
            &self.board_name
        };
        if !name.is_empty() {
            title.push_str(" · ");
            title.push_str(&sanitize_inline(name));
        }
        title.push(' ');
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(title);
        let inner = outer.inner(area);
        outer.render(area, buf);

        let [body, status, footer] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        let dim = style::dim_style(theme);
        Line::styled(
            "←/→ ↑/↓ · Enter Details · >/< · d/b/u/a · f freigeben · x ablehnen · c Kommentar · e Text · v Beleg · B Board · n neu · R · Esc",
            dim,
        )
        .render(footer, buf);
        if let Some(error) = &self.error {
            Line::styled(
                format!("Fehler: {}", sanitize_inline(error)),
                style::error_style(theme),
            )
            .render(status, buf);
        } else if let Some(hint) = &self.hint {
            Line::styled(hint.clone(), style::warning_style(theme)).render(status, buf);
        }

        if self.picker.is_some() || self.picker_loading {
            self.render_picker(body, buf, theme);
            return;
        }
        if !self.loaded {
            if self.error.is_none() {
                Line::styled("Lade Board …", dim).render(body, buf);
            }
            return;
        }
        if self.lanes.is_empty() {
            Line::styled("Keine Spalten vorhanden.", dim).render(body, buf);
            return;
        }

        let (board_area, detail_area) = if self.detail && body.height > DETAIL_HEIGHT + 3 {
            let [top, bottom] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(DETAIL_HEIGHT)])
                    .areas(body);
            (top, Some(bottom))
        } else if self.detail {
            (Rect::default(), Some(body))
        } else {
            (body, None)
        };

        if board_area.height > 0 {
            let visible =
                usize::from((board_area.width / MIN_LANE_WIDTH).max(1)).min(self.lanes.len());
            let first = self
                .lane_index
                .saturating_sub(visible.saturating_sub(1))
                .min(self.lanes.len() - visible);
            let constraints = vec![Constraint::Ratio(1, visible as u32); visible];
            let columns = Layout::horizontal(constraints).split(board_area);
            for (offset, column) in columns.iter().enumerate() {
                self.render_lane(first + offset, *column, buf, theme);
            }
        }
        if let Some(detail_area) = detail_area {
            self.render_detail(detail_area, buf, theme);
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if key.kind == KeyEventKind::Release {
            return OverlayOutcome::Stay;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return OverlayOutcome::Stay;
        }
        if self.picker.is_some() || self.picker_loading {
            return self.picker_key(key);
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.detail {
                    self.detail = false;
                    OverlayOutcome::Stay
                } else {
                    OverlayOutcome::Close
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.lane_index > 0 {
                    self.lane_index -= 1;
                    self.card_index = 0;
                    self.clamp();
                }
                OverlayOutcome::Stay
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.lane_index + 1 < self.lanes.len() {
                    self.lane_index += 1;
                    self.card_index = 0;
                    self.clamp();
                }
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.card_index = self.card_index.saturating_sub(1);
                OverlayOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.card_index + 1 < self.lane_cards(self.lane_index).len() {
                    self.card_index += 1;
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                self.detail = !self.detail && self.selected_card().is_some();
                OverlayOutcome::Stay
            }
            KeyCode::Char('>') => self.move_card(true),
            KeyCode::Char('<') => self.move_card(false),
            KeyCode::Char('d') => self.card_action(DONE_COMMAND),
            KeyCode::Char('u') => self.card_action(UNBLOCK_COMMAND),
            KeyCode::Char('a') => self.card_action(ARCHIVE_COMMAND),
            KeyCode::Char('b') => match self.selected_card().map(|card| card.id.clone()) {
                Some(id) if !id.is_empty() => {
                    OverlayOutcome::Prefill(format!("{} {id} ", self.command(BLOCK_COMMAND)))
                }
                _ => {
                    self.hint = Some("Keine Karte ausgewählt.".to_owned());
                    OverlayOutcome::Stay
                }
            },
            KeyCode::Char('f') => self.approve_card(),
            KeyCode::Char('x') => self.with_selected_card(|board, card| {
                OverlayOutcome::Prefill(format!("{} {} ", board.command(REJECT_COMMAND), card.id))
            }),
            KeyCode::Char('c') => self.with_selected_card(|board, card| {
                OverlayOutcome::Prefill(format!("{} {} ", board.command(COMMENT_COMMAND), card.id))
            }),
            KeyCode::Char('e') => self.with_selected_card(|board, card| {
                let body = if card.body_complete {
                    card.body.trim()
                } else {
                    ""
                };
                OverlayOutcome::Prefill(format!(
                    "{} {} {body}",
                    board.command(EDIT_COMMAND),
                    card.id
                ))
            }),
            KeyCode::Char('v') => self.with_selected_card(|board, card| {
                OverlayOutcome::Prefill(format!("{} {} ", board.command(EVIDENCE_COMMAND), card.id))
            }),
            KeyCode::Char('B') => {
                self.picker = None;
                self.picker_loading = true;
                self.picker_index = 0;
                OverlayOutcome::Fetch(BOARDS_COMMAND.to_owned())
            }
            KeyCode::Char('n') => OverlayOutcome::Prefill(self.command(ADD_PREFILL)),
            KeyCode::Char('R') => OverlayOutcome::Fetch(self.refresh()),
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(self.refresh())
    }

    fn apply_data(&mut self, data: &Value) {
        if let Some(boards) = data.get("boards").filter(|_| data.get("lanes").is_none()) {
            let entries: Vec<BoardEntry> = boards
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter(|entry| entry.is_object())
                .map(|entry| BoardEntry {
                    id: str_field(entry, "id"),
                    name: str_field(entry, "name"),
                })
                .filter(|entry| !entry.id.is_empty())
                .collect();
            let current = self
                .selected_board
                .clone()
                .unwrap_or_else(|| self.board_id.clone());
            self.picker_index = entries
                .iter()
                .position(|entry| entry.id == current)
                .unwrap_or(0);
            self.picker = Some(entries);
            self.picker_loading = false;
            self.error = None;
            return;
        }
        let board = data.get("board").unwrap_or(&Value::Null);
        self.board_id = str_field(board, "id");
        self.board_name = str_field(board, "name");
        self.lanes = array_field(data, "lanes")
            .iter()
            .filter(|lane| lane.is_object())
            .map(|lane| Lane {
                id: str_field(lane, "id"),
                title: str_field(lane, "title"),
                kind: str_field(lane, "kind"),
                state: str_field(lane, "state"),
                worker_role: opt_str_field(lane, "worker_role"),
                risk: opt_str_field(lane, "risk"),
            })
            .filter(|lane| !lane.id.is_empty() || !lane.state.is_empty())
            .collect();
        // Worker-Lanes nach Rolle gruppiert hinter die Status-Lanes (stabil).
        let (mut ordered, mut workers): (Vec<Lane>, Vec<Lane>) = std::mem::take(&mut self.lanes)
            .into_iter()
            .partition(Lane::is_status);
        workers.sort_by(|a, b| {
            a.worker_role
                .as_deref()
                .unwrap_or_default()
                .cmp(b.worker_role.as_deref().unwrap_or_default())
        });
        ordered.append(&mut workers);
        self.lanes = ordered;
        self.cards = array_field(data, "cards")
            .iter()
            .filter(|card| card.is_object())
            .map(|card| Card {
                id: str_field(card, "id"),
                lane_id: str_field(card, "lane_id"),
                title: str_field(card, "title"),
                state: str_field(card, "state"),
                assignee: opt_str_field(card, "assignee"),
                tags: string_list(card.get("tags")),
                retry_count: card
                    .get("retry_count")
                    .and_then(|value| {
                        value
                            .as_u64()
                            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
                    })
                    .unwrap_or(0),
                blocked_reason: opt_str_field(card, "blocked_reason"),
                work_id: opt_str_field(card, "work_id"),
                body: str_field(card, "body"),
                body_complete: card
                    .get("body_complete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                awaiting_approval: card
                    .get("awaiting_approval")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                comment_count: card
                    .get("comment_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                evidence: string_list(card.get("evidence")),
                result_status: opt_str_field(card, "result_status"),
            })
            .collect();
        self.loaded = true;
        self.error = None;
        self.hint = None;
        self.clamp();
    }

    fn apply_error(&mut self, text: &str) {
        self.picker_loading = false;
        self.error = Some(text.to_owned());
    }
}

/// Kurzes Zeichen für den Ausgang des letzten Worker-Laufs.
fn result_marker(status: &str) -> &'static str {
    match status {
        "succeeded" => "✔",
        "partial" => "◐",
        "failed" => "✘",
        _ => "?",
    }
}

/// Deutsche Bezeichnung für den Ausgang des letzten Worker-Laufs.
fn result_label(status: &str) -> &'static str {
    match status {
        "succeeded" => "erledigt",
        "partial" => "teilweise erledigt",
        "failed" => "fehlgeschlagen",
        _ => "unbekannt",
    }
}

/// Kürzt `text` auf höchstens `width` Terminalzellen (mit `…`).
fn truncate(text: &str, width: usize) -> String {
    let total: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(ch);
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Tags: Array aus Strings (andere Elemente werden übersprungen) oder ein
/// kommagetrennter String.
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
        Some(Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
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
    use serde_json::json;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample() -> Value {
        json!({
            "board": {"id": "b1", "name": "Sprint"},
            "lanes": [
                {"id": "l-todo", "title": "Offen", "kind": "status", "state": "todo"},
                {"id": "l-w", "title": "Worker", "kind": "worker", "state": "doing", "worker_role": "coder"},
                {"id": "l-doing", "title": "In Arbeit", "kind": "status", "state": "doing"},
                {"id": "l-done", "title": "Fertig", "state": "done"},
                "kaputt"
            ],
            "cards": [
                {"id": "c1", "lane_id": "l-todo", "title": "Parser", "tags": ["a", "b"], "retry_count": 2},
                {"id": "c2", "lane_id": "l-todo", "title": "Lexer", "blocked_reason": "wartet"},
                {"id": "c3", "state": "done", "title": "Alt", "tags": "x, y", "retry_count": "3"},
                {"id": "c4", "lane_id": "l-w", "state": "doing", "title": "Worker-Karte", "assignee": "coder"}
            ]
        })
    }

    fn loaded() -> KanbanBoard {
        let mut board = KanbanBoard::new();
        board.apply_data(&sample());
        board
    }

    fn render_to_string(board: &KanbanBoard, width: u16, height: u16) -> TestResult<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| {
            let area = frame.area();
            board.render(area, frame.buffer_mut(), Theme::Dark);
        })?;
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        Ok(out)
    }

    fn run_text(outcome: OverlayOutcome) -> Option<String> {
        match outcome {
            OverlayOutcome::Run(text) => Some(text),
            _ => None,
        }
    }

    fn prefill_text(outcome: OverlayOutcome) -> Option<String> {
        match outcome {
            OverlayOutcome::Prefill(text) => Some(text),
            _ => None,
        }
    }

    #[test]
    fn parses_tolerantly() {
        let board = loaded();
        assert_eq!(board.board_name, "Sprint");
        assert_eq!(board.lanes().len(), 4);
        assert_eq!(board.cards().len(), 4);
        assert_eq!(board.cards()[0].tags, vec!["a", "b"]);
        assert_eq!(board.cards()[2].tags, vec!["x", "y"]);
        assert_eq!(board.cards()[2].retry_count, 3);
        // Status-Lanes zuerst, die Worker-Lane dahinter (nach Rolle gruppiert).
        assert_eq!(board.lane_cards(0), vec![0, 1]);
        assert!(board.lane_cards(1).is_empty());
        // Karte ohne lane_id landet über ihren Status in der Status-Lane.
        assert_eq!(board.lane_cards(2), vec![2]);
        assert_eq!(board.lane_cards(3), vec![3]);
        assert_eq!(board.lanes()[3].id, "l-w");
        assert_eq!(board.refresh_command().as_deref(), Some("/kanban show"));
    }

    #[test]
    fn garbage_data_yields_empty_board() {
        let mut board = KanbanBoard::new();
        board.apply_data(&json!({"lanes": 3, "cards": {"x": 1}}));
        assert!(board.lanes().is_empty());
        assert!(board.cards().is_empty());
        assert!(matches!(
            board.on_key(key(KeyCode::Char('d'))),
            OverlayOutcome::Stay
        ));
        assert!(matches!(
            board.on_key(key(KeyCode::Char('>'))),
            OverlayOutcome::Stay
        ));
    }

    #[test]
    fn move_uses_status_lane_order() {
        let mut board = loaded();
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('>')))).as_deref(),
            Some("/kanban move c1 doing")
        );
        assert!(matches!(
            board.on_key(key(KeyCode::Char('<'))),
            OverlayOutcome::Stay
        ));
        assert!(board.hint.is_some());
        // Zur Fertig-Lane (leere Lane überspringen).
        for _ in 0..2 {
            board.on_key(key(KeyCode::Right));
        }
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('<')))).as_deref(),
            Some("/kanban move c3 doing")
        );
        assert!(matches!(
            board.on_key(key(KeyCode::Char('>'))),
            OverlayOutcome::Stay
        ));
    }

    #[test]
    fn card_actions_and_prefills() {
        let mut board = loaded();
        board.on_key(key(KeyCode::Down));
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('d')))).as_deref(),
            Some("/kanban done c2")
        );
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('u')))).as_deref(),
            Some("/kanban unblock c2")
        );
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('a')))).as_deref(),
            Some("/kanban archive c2")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('b')))).as_deref(),
            Some("/kanban block c2 ")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('n')))).as_deref(),
            Some("/kanban add ")
        );
        assert!(matches!(
            board.on_key(key(KeyCode::Char('R'))),
            OverlayOutcome::Fetch(ref cmd) if cmd == "/kanban show"
        ));
        // Down am Ende bleibt stehen, Up geht zurück.
        board.on_key(key(KeyCode::Down));
        board.on_key(key(KeyCode::Up));
        assert_eq!(board.selected_card().map(|c| c.id.as_str()), Some("c1"));
    }

    #[test]
    fn enter_toggles_detail_and_esc_closes_it_first() {
        let mut board = loaded();
        board.on_key(key(KeyCode::Enter));
        assert!(board.detail);
        assert!(matches!(
            board.on_key(key(KeyCode::Esc)),
            OverlayOutcome::Stay
        ));
        assert!(!board.detail);
        assert!(matches!(
            board.on_key(key(KeyCode::Esc)),
            OverlayOutcome::Close
        ));
        // Leere Lane: kein Detail.
        board.on_key(key(KeyCode::Right));
        board.on_key(key(KeyCode::Enter));
        assert!(!board.detail);
    }

    #[test]
    fn renders_lanes_and_detail() -> TestResult {
        let mut board = KanbanBoard::new();
        assert!(render_to_string(&board, 100, 20)?.contains("Lade Board"));
        board.apply_data(&sample());
        let out = render_to_string(&board, 100, 20)?;
        assert!(out.contains("Kanban · Sprint"));
        assert!(out.contains("Offen (2)"));
        assert!(out.contains("▸ Parser"));
        assert!(out.contains("Worker @coder"));
        board.on_key(key(KeyCode::Enter));
        let out = render_to_string(&board, 100, 24)?;
        assert!(out.contains("Tags: a, b"));
        assert!(out.contains("Wiederholungen: 2"));
        // Schmales Terminal zeigt nur ein Fenster der Spalten.
        let narrow = render_to_string(&board, 30, 20)?;
        assert!(narrow.contains("Offen"));
        Ok(())
    }

    fn d2_sample() -> Value {
        json!({
            "board": {"id": "default", "name": "default"},
            "lanes": [
                {"id": "worker/writer", "kind": "worker", "worker_role": "writer", "risk": "medium"},
                {"id": "todo", "kind": "status", "state": "todo"},
                {"id": "worker/explorer", "kind": "worker", "worker_role": "explorer", "risk": "low"}
            ],
            "cards": [
                {"id": "card-1", "lane_id": "worker/explorer", "title": "Lesen", "state": "blocked",
                 "blocked_reason": "AwaitingApproval", "awaiting_approval": true,
                 "body": "Alter Text", "body_complete": true, "comment_count": 2,
                 "evidence": ["docs/a.md"], "result_status": "partial"},
                {"id": "card-2", "lane_id": "todo", "title": "Offen", "state": "todo",
                 "body": "lang…", "body_complete": false}
            ]
        })
    }

    #[test]
    fn worker_lanes_are_grouped_by_role_after_status_lanes() {
        let mut board = KanbanBoard::new();
        board.apply_data(&d2_sample());
        let ids: Vec<&str> = board.lanes().iter().map(|lane| lane.id.as_str()).collect();
        assert_eq!(ids, vec!["todo", "worker/explorer", "worker/writer"]);
        assert_eq!(
            board.lanes()[1].display_title(),
            "worker/explorer @explorer [low]"
        );
        let card = &board.cards()[0];
        assert!(card.awaiting_approval);
        assert_eq!(card.comment_count, 2);
        assert_eq!(card.evidence, vec!["docs/a.md"]);
        assert_eq!(card.result_status.as_deref(), Some("partial"));
    }

    #[test]
    fn approve_reject_and_note_prefills() {
        let mut board = KanbanBoard::new();
        board.apply_data(&d2_sample());
        // Todo-Lane: Karte wartet nicht → Hinweis statt Befehl.
        assert!(matches!(
            board.on_key(key(KeyCode::Char('f'))),
            OverlayOutcome::Stay
        ));
        assert!(board.hint.is_some());
        // Unvollständiger Text wird nicht vorbelegt.
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('e')))).as_deref(),
            Some("/kanban edit card-2 ")
        );
        board.on_key(key(KeyCode::Right));
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('f')))).as_deref(),
            Some("/kanban approve card-1")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('x')))).as_deref(),
            Some("/kanban reject card-1 ")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('c')))).as_deref(),
            Some("/kanban comment card-1 ")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('e')))).as_deref(),
            Some("/kanban edit card-1 Alter Text")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('v')))).as_deref(),
            Some("/kanban evidence card-1 ")
        );
    }

    #[test]
    fn board_picker_switches_the_board_and_scopes_commands() -> TestResult {
        let mut board = KanbanBoard::new();
        board.apply_data(&d2_sample());
        assert!(matches!(
            board.on_key(key(KeyCode::Char('B'))),
            OverlayOutcome::Fetch(ref cmd) if cmd == "/kanban boards"
        ));
        assert!(render_to_string(&board, 80, 20)?.contains("Lade Boards"));
        board.apply_data(&json!({"boards": [
            {"id": "default", "name": "default"},
            {"id": "sprint", "name": "Sprint 7"}
        ]}));
        let out = render_to_string(&board, 80, 20)?;
        assert!(out.contains("Board wählen"));
        assert!(out.contains("Sprint 7 (sprint)"));
        // Während der Auswahl lösen Kartentasten nichts aus.
        assert!(matches!(
            board.on_key(key(KeyCode::Char('d'))),
            OverlayOutcome::Stay
        ));
        board.on_key(key(KeyCode::Down));
        assert!(matches!(
            board.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Fetch(ref cmd) if cmd == "/kanban --board=sprint show"
        ));
        assert_eq!(board.selected_board(), Some("sprint"));
        assert_eq!(
            board.refresh_command().as_deref(),
            Some("/kanban --board=sprint show")
        );
        board.apply_data(&d2_sample());
        assert_eq!(
            run_text(board.on_key(key(KeyCode::Char('d')))).as_deref(),
            Some("/kanban --board=sprint done card-2")
        );
        assert_eq!(
            prefill_text(board.on_key(key(KeyCode::Char('n')))).as_deref(),
            Some("/kanban --board=sprint add ")
        );
        // Esc schließt eine offene Auswahl, ohne das Board zu wechseln.
        board.on_key(key(KeyCode::Char('B')));
        board.apply_data(&json!({"boards": []}));
        assert!(matches!(
            board.on_key(key(KeyCode::Esc)),
            OverlayOutcome::Stay
        ));
        assert_eq!(board.selected_board(), Some("sprint"));
        Ok(())
    }

    #[test]
    fn waiting_cards_render_the_approval_marker() -> TestResult {
        let mut board = KanbanBoard::new();
        board.apply_data(&d2_sample());
        board.on_key(key(KeyCode::Right));
        let out = render_to_string(&board, 120, 20)?;
        assert!(out.contains("Freigabe (f)"), "{out}");
        board.on_key(key(KeyCode::Enter));
        let out = render_to_string(&board, 120, 30)?;
        assert!(out.contains("Wartet auf Freigabe"), "{out}");
        assert!(out.contains("teilweise erledigt"), "{out}");
        assert!(out.contains("docs/a.md"), "{out}");
        Ok(())
    }

    #[test]
    fn truncate_respects_width() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}

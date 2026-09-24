//! Kanban-Overlay: Spalten (Lanes) mit Karten, Detailansicht und
//! Schreibaktionen über Slash-Zeilen.
//!
//! [`KanbanBoard`] implementiert [`OverlayView`]. Die Daten kommen aus
//! `OpOutput.data` von [`REFRESH_COMMAND`] (`/kanban show`) und werden
//! tolerant geparst. Verschieben (`>`/`<`) nutzt die Reihenfolge der
//! Status-Lanes: Ziel ist der `state` der nächsten bzw. vorigen Lane.
//!
//! Erwartetes JSON:
//! `{"board":{"id","name"},"lanes":[{"id","title","kind","state","worker_role"}],`
//! `"cards":[{"id","lane_id","title","state","assignee","tags","retry_count","blocked_reason","work_id"}]}`

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
/// Mindestbreite einer Spalte in Zellen.
const MIN_LANE_WIDTH: u16 = 18;
/// Höhe der Detailansicht (inklusive Rahmen).
const DETAIL_HEIGHT: u16 = 10;

/// Eine Spalte des Boards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Lane {
    pub id: String,
    pub title: String,
    /// `status` oder `worker`.
    pub kind: String,
    pub state: String,
    pub worker_role: Option<String>,
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
        match &self.worker_role {
            Some(role) if !self.is_status() => format!("{base} @{role}"),
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
                OverlayOutcome::Run(format!("{MOVE_COMMAND} {} {state}", card.id))
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
            if card.blocked_reason.is_some() {
                meta.push("⛔".to_owned());
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
                if let Some(reason) = &card.blocked_reason {
                    lines.push(Line::from(vec![
                        Span::styled("Blockiert: ", style::error_style(theme)),
                        Span::raw(sanitize_inline(reason)),
                    ]));
                }
                if let Some(work_id) = &card.work_id {
                    lines.push(field("Arbeit", sanitize_inline(work_id)));
                }
            }
        }
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
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
            "←/→ Spalte · ↑/↓ Karte · Enter Details · >/< verschieben · d erledigt · b blockieren · u lösen · a archivieren · n neu · R neu laden · Esc",
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
                    OverlayOutcome::Prefill(format!("{BLOCK_COMMAND} {id} "))
                }
                _ => {
                    self.hint = Some("Keine Karte ausgewählt.".to_owned());
                    OverlayOutcome::Stay
                }
            },
            KeyCode::Char('n') => OverlayOutcome::Prefill(ADD_PREFILL.to_owned()),
            KeyCode::Char('R') => OverlayOutcome::Fetch(REFRESH_COMMAND.to_owned()),
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(REFRESH_COMMAND.to_owned())
    }

    fn apply_data(&mut self, data: &Value) {
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
            })
            .filter(|lane| !lane.id.is_empty() || !lane.state.is_empty())
            .collect();
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
            })
            .collect();
        self.loaded = true;
        self.error = None;
        self.hint = None;
        self.clamp();
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
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
        assert_eq!(board.lane_cards(0), vec![0, 1]);
        assert_eq!(board.lane_cards(1), vec![3]);
        assert!(board.lane_cards(2).is_empty());
        // Karte ohne lane_id landet über ihren Status in der Status-Lane.
        assert_eq!(board.lane_cards(3), vec![2]);
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
        // Zur Fertig-Lane (Worker-Lane und leere Lane überspringen).
        for _ in 0..3 {
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

    #[test]
    fn truncate_respects_width() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}

//! Wissens-Overlay für Gedächtnispalast, Traumberichte und Tagebuch.
//!
//! [`KnowledgeBrowser`] implementiert [`OverlayView`] und zeigt eine Liste
//! plus Detailansicht. Die Liste kommt aus `OpOutput.data` des
//! Listenbefehls der jeweiligen [`KnowledgeKind`], Details (Palast, Träume)
//! werden per [`OverlayOutcome::Fetch`] nachgeladen. [`OverlayView::apply_data`]
//! unterscheidet Listen- und Detail-Nutzlast am Schlüssel (`nodes`/`reports`/
//! `entries` gegenüber `node`/`report`). Die TUI öffnet keinen
//! KnowledgeStore selbst.
//!
//! Erwartetes JSON:
//! - `/palace list` → `{"nodes":[{"id","title","status","links","updated"}]}`,
//!   `/palace show <id>` → `{"node":{…,"body"}}`
//! - `/dream list` → `{"reports":[{"id","date","proposals"}]}`,
//!   `/dream show <id>` → `{"report":{"date","body"}}`
//! - `/diary today` → `{"entries":[{"time","trigger","text"}]}`

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};
use serde_json::Value;

use crate::overlay_view::{OverlayOutcome, OverlayView};
use crate::sanitize::{sanitize_display, sanitize_inline};
use crate::style::{self, Theme};

/// Listet die Knoten des Gedächtnispalasts.
pub(crate) const PALACE_LIST_COMMAND: &str = "/palace list";
/// Lädt einen Palast-Knoten (`<cmd> <id>`).
pub(crate) const PALACE_SHOW_COMMAND: &str = "/palace show";
/// Listet die Traumberichte.
pub(crate) const DREAM_LIST_COMMAND: &str = "/dream list";
/// Lädt einen Traumbericht (`<cmd> <id>`).
pub(crate) const DREAM_SHOW_COMMAND: &str = "/dream show";
/// Lädt die heutigen Tagebucheinträge.
pub(crate) const DIARY_TODAY_COMMAND: &str = "/diary today";
/// Vorbelegung für einen neuen Tagebucheintrag.
pub(crate) const DIARY_NOTE_PREFILL: &str = "/diary note ";
/// Seitensprung für PageUp/PageDown.
const PAGE_STEP: usize = 10;

/// Welche Wissensquelle das Overlay zeigt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KnowledgeKind {
    /// Gedächtnispalast (`/palace`).
    Palace,
    /// Traumberichte (`/dream`).
    Dream,
    /// Tagebuch von heute (`/diary`).
    Diary,
}

impl KnowledgeKind {
    /// Deutscher Titel des Overlays.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Palace => "Gedächtnispalast",
            Self::Dream => "Traumberichte",
            Self::Diary => "Tagebuch · heute",
        }
    }

    /// Befehl, der die Liste lädt.
    pub(crate) fn list_command(self) -> &'static str {
        match self {
            Self::Palace => PALACE_LIST_COMMAND,
            Self::Dream => DREAM_LIST_COMMAND,
            Self::Diary => DIARY_TODAY_COMMAND,
        }
    }

    /// Befehl, der ein Detail lädt (`None`: Detail kommt aus der Liste).
    pub(crate) fn show_command(self) -> Option<&'static str> {
        match self {
            Self::Palace => Some(PALACE_SHOW_COMMAND),
            Self::Dream => Some(DREAM_SHOW_COMMAND),
            Self::Diary => None,
        }
    }

    fn list_key(self) -> &'static str {
        match self {
            Self::Palace => "nodes",
            Self::Dream => "reports",
            Self::Diary => "entries",
        }
    }

    fn detail_key(self) -> Option<&'static str> {
        match self {
            Self::Palace => Some("node"),
            Self::Dream => Some("report"),
            Self::Diary => None,
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Self::Palace => "Keine Knoten vorhanden.",
            Self::Dream => "Keine Traumberichte vorhanden.",
            Self::Diary => "Heute noch keine Einträge.",
        }
    }
}

/// Ein Listeneintrag.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct KnowledgeItem {
    /// Kennung für den Detailbefehl (Tagebuch: leer).
    pub id: String,
    /// Hauptzeile.
    pub title: String,
    /// Nebeninformation (Status, Datum, Anzahl …).
    pub meta: String,
    /// Volltext, falls ohne Nachladen verfügbar (Tagebuch).
    pub body: Option<String>,
}

/// Geladene Detailansicht.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct KnowledgeDetail {
    pub title: String,
    /// Schlüssel/Wert-Zeilen über dem Text.
    pub fields: Vec<(String, String)>,
    pub body: String,
}

/// Wissens-Overlay.
#[derive(Debug, Clone)]
pub(crate) struct KnowledgeBrowser {
    kind: KnowledgeKind,
    items: Vec<KnowledgeItem>,
    selected: usize,
    detail: Option<KnowledgeDetail>,
    detail_pending: bool,
    scroll: usize,
    loaded: bool,
    error: Option<String>,
}

impl KnowledgeBrowser {
    /// Leeres Overlay für `kind`; die Liste folgt über `refresh_command`.
    pub(crate) fn new(kind: KnowledgeKind) -> Self {
        Self {
            kind,
            items: Vec::new(),
            selected: 0,
            detail: None,
            detail_pending: false,
            scroll: 0,
            loaded: false,
            error: None,
        }
    }

    /// Angezeigte Quelle.
    pub(crate) fn kind(&self) -> KnowledgeKind {
        self.kind
    }

    /// Listeneinträge (für Tests).
    #[cfg(test)]
    pub(crate) fn items(&self) -> &[KnowledgeItem] {
        &self.items
    }

    /// Geöffnetes Detail (für Tests).
    #[cfg(test)]
    pub(crate) fn detail(&self) -> Option<&KnowledgeDetail> {
        self.detail.as_ref()
    }

    fn parse_item(&self, value: &Value) -> Option<KnowledgeItem> {
        if !value.is_object() {
            return None;
        }
        let item = match self.kind {
            KnowledgeKind::Palace => {
                let id = str_field(value, "id");
                let mut meta = Vec::new();
                if let Some(status) = opt_str_field(value, "status") {
                    meta.push(status);
                }
                let links = list_len(value.get("links"));
                if links > 0 {
                    meta.push(format!("{links} Links"));
                }
                if let Some(updated) = opt_str_field(value, "updated") {
                    meta.push(updated);
                }
                KnowledgeItem {
                    title: opt_str_field(value, "title").unwrap_or_else(|| id.clone()),
                    id,
                    meta: meta.join(" · "),
                    body: None,
                }
            }
            KnowledgeKind::Dream => {
                let id = str_field(value, "id");
                let count = list_len(value.get("proposals"));
                KnowledgeItem {
                    title: opt_str_field(value, "date").unwrap_or_else(|| id.clone()),
                    id,
                    meta: format!("{count} Vorschläge"),
                    body: None,
                }
            }
            KnowledgeKind::Diary => {
                let text = str_field(value, "text");
                let mut meta = Vec::new();
                if let Some(time) = opt_str_field(value, "time") {
                    meta.push(time);
                }
                if let Some(trigger) = opt_str_field(value, "trigger") {
                    meta.push(trigger);
                }
                KnowledgeItem {
                    id: String::new(),
                    title: text.lines().next().unwrap_or_default().to_owned(),
                    meta: meta.join(" · "),
                    body: Some(text),
                }
            }
        };
        (!item.id.is_empty() || item.body.is_some()).then_some(item)
    }

    fn parse_detail(&self, value: &Value) -> KnowledgeDetail {
        let mut fields = Vec::new();
        let title = match self.kind {
            KnowledgeKind::Palace => {
                for (key, label) in [
                    ("id", "Kennung"),
                    ("status", "Status"),
                    ("updated", "Aktualisiert"),
                ] {
                    if let Some(text) = opt_str_field(value, key) {
                        fields.push((label.to_owned(), text));
                    }
                }
                let links = list_texts(value.get("links"));
                if !links.is_empty() {
                    fields.push(("Links".to_owned(), links.join(", ")));
                }
                opt_str_field(value, "title").unwrap_or_else(|| str_field(value, "id"))
            }
            KnowledgeKind::Dream => {
                let count = list_len(value.get("proposals"));
                if count > 0 {
                    fields.push(("Vorschläge".to_owned(), count.to_string()));
                }
                let date = str_field(value, "date");
                if date.is_empty() {
                    "Traumbericht".to_owned()
                } else {
                    format!("Traumbericht {date}")
                }
            }
            KnowledgeKind::Diary => str_field(value, "time"),
        };
        KnowledgeDetail {
            title,
            fields,
            body: str_field(value, "body"),
        }
    }

    fn open_selected(&mut self) -> OverlayOutcome {
        let Some(item) = self.items.get(self.selected).cloned() else {
            return OverlayOutcome::Stay;
        };
        self.scroll = 0;
        match (self.kind.show_command(), item.body) {
            (Some(command), _) if !item.id.is_empty() => {
                self.detail_pending = true;
                OverlayOutcome::Fetch(format!("{command} {}", item.id))
            }
            (_, Some(body)) => {
                self.detail = Some(KnowledgeDetail {
                    title: item.meta,
                    fields: Vec::new(),
                    body,
                });
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.detail.is_some() {
            self.scroll = self.scroll.saturating_add_signed(delta);
            return;
        }
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.items.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(max);
    }

    fn render_list(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.items.is_empty() {
            lines.push(Line::styled(self.kind.empty_text(), dim));
        }
        for (index, item) in self.items.iter().enumerate() {
            let selected = index == self.selected;
            let marker = if selected { "▸ " } else { "  " };
            let title_style = if selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(marker)];
            if self.kind == KnowledgeKind::Diary && !item.meta.is_empty() {
                spans.push(Span::styled(
                    format!("{} ", sanitize_inline(&item.meta)),
                    dim,
                ));
                spans.push(Span::styled(sanitize_inline(&item.title), title_style));
            } else {
                spans.push(Span::styled(sanitize_inline(&item.title), title_style));
                if !item.meta.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", sanitize_inline(&item.meta)),
                        dim,
                    ));
                }
            }
            lines.push(Line::from(spans));
        }
        let height = usize::from(area.height);
        let scroll = (self.selected + 1).saturating_sub(height);
        Paragraph::new(lines)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn render_detail(&self, detail: &KnowledgeDetail, area: Rect, buf: &mut Buffer, theme: Theme) {
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        if !detail.title.is_empty() {
            lines.push(Line::styled(
                sanitize_inline(&detail.title),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        for (label, value) in &detail.fields {
            lines.push(Line::from(vec![
                Span::styled(format!("{label}: "), dim),
                Span::raw(sanitize_inline(value)),
            ]));
        }
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        if detail.body.trim().is_empty() {
            lines.push(Line::styled("(kein Text)", dim));
        } else {
            for line in sanitize_display(&detail.body).lines() {
                lines.push(Line::raw(line.to_owned()));
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(self.scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }
}

impl OverlayView for KnowledgeBrowser {
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        Clear.render(area, buf);
        let mut title = format!(" {}", self.kind.title());
        if self.loaded && self.detail.is_none() {
            title.push_str(&format!(" ({})", self.items.len()));
        }
        title.push(' ');
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(title);
        let inner = block.inner(area);
        block.render(area, buf);
        let [body, status, footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        let dim = style::dim_style(theme);
        let hint = match (self.detail.is_some(), self.kind) {
            (true, _) => "j/k scrollen · Esc zurück",
            (false, KnowledgeKind::Diary) => {
                "j/k wählen · Enter anzeigen · n Eintrag · R neu laden · Esc schließen"
            }
            (false, _) => "j/k wählen · Enter öffnen · R neu laden · Esc schließen",
        };
        Line::styled(hint, dim).render(footer, buf);
        if let Some(error) = &self.error {
            Line::styled(
                format!("Fehler: {}", sanitize_inline(error)),
                style::error_style(theme),
            )
            .render(status, buf);
        } else if self.detail_pending {
            Line::styled("Lade Details …", dim).render(status, buf);
        }

        if let Some(detail) = &self.detail {
            self.render_detail(detail, body, buf, theme);
        } else if !self.loaded {
            if self.error.is_none() {
                Line::styled("Lade …", dim).render(body, buf);
            }
        } else {
            self.render_list(body, buf, theme);
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
                if self.detail.is_some() || self.detail_pending {
                    self.detail = None;
                    self.detail_pending = false;
                    self.scroll = 0;
                    OverlayOutcome::Stay
                } else {
                    OverlayOutcome::Close
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                OverlayOutcome::Stay
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                OverlayOutcome::Stay
            }
            KeyCode::PageDown => {
                self.move_selection(PAGE_STEP as isize);
                OverlayOutcome::Stay
            }
            KeyCode::PageUp => {
                self.move_selection(-(PAGE_STEP as isize));
                OverlayOutcome::Stay
            }
            KeyCode::Enter if self.detail.is_none() => self.open_selected(),
            KeyCode::Char('n') if self.kind == KnowledgeKind::Diary => {
                OverlayOutcome::Prefill(DIARY_NOTE_PREFILL.to_owned())
            }
            KeyCode::Char('R') => {
                self.detail = None;
                self.detail_pending = false;
                self.scroll = 0;
                OverlayOutcome::Fetch(self.kind.list_command().to_owned())
            }
            _ => OverlayOutcome::Stay,
        }
    }

    fn refresh_command(&self) -> Option<String> {
        Some(self.kind.list_command().to_owned())
    }

    fn apply_data(&mut self, data: &Value) {
        if let Some(key) = self.kind.detail_key()
            && let Some(detail) = data.get(key).filter(|value| value.is_object())
        {
            self.detail = Some(self.parse_detail(detail));
            self.detail_pending = false;
            self.scroll = 0;
            self.error = None;
            return;
        }
        let items: Vec<KnowledgeItem> = array_field(data, self.kind.list_key())
            .iter()
            .filter_map(|value| self.parse_item(value))
            .collect();
        self.items = items;
        self.loaded = true;
        self.error = None;
        if self.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
    }

    fn apply_error(&mut self, text: &str) {
        self.error = Some(text.to_owned());
        self.detail_pending = false;
    }
}

/// Anzahl bei Array-Feldern, Zahl bei numerischen Feldern, sonst 0.
fn list_len(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Texte eines Link-Felds: Strings direkt, Objekte über `title`/`id`/`target`.
fn list_texts(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                Value::Object(_) => ["title", "id", "target"]
                    .iter()
                    .find_map(|key| opt_str_field(item, key)),
                _ => None,
            })
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

    fn fetch_text(outcome: OverlayOutcome) -> Option<String> {
        match outcome {
            OverlayOutcome::Fetch(text) => Some(text),
            _ => None,
        }
    }

    fn render_to_string(view: &KnowledgeBrowser) -> TestResult<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 16))?;
        terminal.draw(|frame| {
            let area = frame.area();
            view.render(area, frame.buffer_mut(), Theme::Dark);
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

    #[test]
    fn refresh_commands_per_kind() {
        assert_eq!(
            KnowledgeBrowser::new(KnowledgeKind::Palace)
                .refresh_command()
                .as_deref(),
            Some("/palace list")
        );
        assert_eq!(
            KnowledgeBrowser::new(KnowledgeKind::Dream)
                .refresh_command()
                .as_deref(),
            Some("/dream list")
        );
        let diary = KnowledgeBrowser::new(KnowledgeKind::Diary);
        assert_eq!(diary.refresh_command().as_deref(), Some("/diary today"));
        assert_eq!(diary.kind(), KnowledgeKind::Diary);
    }

    #[test]
    fn palace_list_then_detail() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        view.apply_data(&json!({"nodes": [
            {"id": "n1", "title": "Architektur", "status": "aktiv", "links": ["n2", "n3"], "updated": "2026-09-01"},
            {"id": "n2"},
            {"title": "ohne id"},
            "kaputt"
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].meta, "aktiv · 2 Links · 2026-09-01");
        assert_eq!(view.items()[1].title, "n2");
        view.on_key(key(KeyCode::Down));
        view.on_key(key(KeyCode::Down));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/palace show n2")
        );
        assert!(render_to_string(&view)?.contains("Lade Details"));
        view.apply_data(&json!({"node": {
            "id": "n2", "title": "Speicher", "links": [{"id": "n1"}], "body": "Zeile 1\nZeile 2"
        }}));
        let detail = view.detail().ok_or("kein Detail")?;
        assert_eq!(detail.title, "Speicher");
        assert!(
            detail
                .fields
                .contains(&("Links".to_owned(), "n1".to_owned()))
        );
        // Die Liste bleibt bei Detail-Nutzlast erhalten.
        assert_eq!(view.items().len(), 2);
        let out = render_to_string(&view)?;
        assert!(out.contains("Zeile 2"));
        assert!(matches!(
            view.on_key(key(KeyCode::Esc)),
            OverlayOutcome::Stay
        ));
        assert!(view.detail().is_none());
        assert!(matches!(
            view.on_key(key(KeyCode::Esc)),
            OverlayOutcome::Close
        ));
        Ok(())
    }

    #[test]
    fn dream_list_and_show() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Dream);
        view.apply_data(&json!({"reports": [
            {"id": "d1", "date": "2026-09-23", "proposals": [1, 2, 3]},
            {"id": "d2", "proposals": 4}
        ]}));
        assert_eq!(view.items()[0].title, "2026-09-23");
        assert_eq!(view.items()[0].meta, "3 Vorschläge");
        assert_eq!(view.items()[1].meta, "4 Vorschläge");
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Enter))).as_deref(),
            Some("/dream show d1")
        );
        view.apply_data(&json!({"report": {"date": "2026-09-23", "body": "Bericht"}}));
        assert_eq!(
            view.detail().map(|d| d.title.as_str()),
            Some("Traumbericht 2026-09-23")
        );
        assert!(render_to_string(&view)?.contains("Bericht"));
        Ok(())
    }

    #[test]
    fn diary_entries_open_locally_and_prefill_note() -> TestResult {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Diary);
        view.apply_data(&json!({"entries": [
            {"time": "09:15", "trigger": "manuell", "text": "Erster Gedanke\nmehr"},
            {"text": ""}
        ]}));
        assert_eq!(view.items().len(), 2);
        assert_eq!(view.items()[0].title, "Erster Gedanke");
        assert!(render_to_string(&view)?.contains("09:15 · manuell Erster Gedanke"));
        assert!(matches!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Stay
        ));
        assert_eq!(
            view.detail().map(|d| d.body.as_str()),
            Some("Erster Gedanke\nmehr")
        );
        assert!(matches!(
            view.on_key(key(KeyCode::Char('n'))),
            OverlayOutcome::Prefill(ref text) if text == "/diary note "
        ));
        assert_eq!(
            fetch_text(view.on_key(key(KeyCode::Char('R')))).as_deref(),
            Some("/diary today")
        );
        assert!(view.detail().is_none());
        Ok(())
    }

    #[test]
    fn n_is_ignored_outside_diary_and_garbage_is_tolerated() {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Palace);
        assert!(matches!(
            view.on_key(key(KeyCode::Char('n'))),
            OverlayOutcome::Stay
        ));
        view.apply_data(&json!({"nodes": "kaputt", "node": 3}));
        assert!(view.items().is_empty());
        assert!(view.detail().is_none());
        assert!(matches!(
            view.on_key(key(KeyCode::Enter)),
            OverlayOutcome::Stay
        ));
        view.apply_error("nicht verfügbar");
        assert_eq!(view.error.as_deref(), Some("nicht verfügbar"));
    }

    #[test]
    fn selection_clamps_after_shorter_list() {
        let mut view = KnowledgeBrowser::new(KnowledgeKind::Dream);
        view.apply_data(&json!({"reports": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}));
        view.on_key(key(KeyCode::PageDown));
        assert_eq!(view.selected, 2);
        view.apply_data(&json!({"reports": [{"id": "a"}]}));
        assert_eq!(view.selected, 0);
    }
}

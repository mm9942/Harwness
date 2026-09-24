//! Werkbank-Seitenpanel: angeheftete Dateien, Hypothesen und Notizen.
//!
//! [`WorkbenchPane`] ist reiner Zustand plus Rendering. Die Daten stammen aus
//! `OpOutput.data` von [`WorkbenchPane::REFRESH_COMMAND`] (`/workbench show`)
//! und werden tolerant geparst: fehlende oder falsch typisierte Schlüssel
//! fallen auf Standardwerte zurück. Schreibaktionen erzeugt das Panel nie
//! selbst, sondern liefert Slash-Zeilen ([`PaneCommand::Run`]) bzw.
//! Eingabe-Vorbelegungen ([`PaneCommand::Prefill`]); die Ops prüfen und
//! speichern.
//!
//! Erwartetes JSON:
//! `{"scope","pinned":[{"path","note","status","preview"}],
//! "hypotheses":[{"id","text","status"}],"notes":[{"id","at","text"}],"notes_tail"}`
//!
//! # Scope
//! `s` schaltet zwischen Sitzungs- und Projekt-Werkbank um
//! ([`WorkbenchPane::refresh_command`]). Zeigt das Panel einen Projekt-Scope,
//! tragen alle erzeugten Befehle `--scope=project:<slug>` (aus `data.scope`),
//! damit Lösen/Notieren/Entscheiden im angezeigten Scope landet.
//!
//! # Notizen
//! Liefert die Op `notes`, sind Notizen auswählbar: `e` belegt die Eingabe mit
//! `/workbench note edit <id> <text>` vor, `x` löscht per stabilem Zeitstempel.
//! Mehrzeilige Notizen werden beim Vorbelegen zu einer Zeile gefaltet.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use serde_json::Value;

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Befehl zum Lösen einer angehefteten Datei (`<cmd> <pfad>`).
pub(crate) const UNPIN_COMMAND: &str = "/workbench unpin";
/// Vorbelegung für eine neue Notiz.
pub(crate) const NOTE_PREFILL: &str = "/workbench note ";
/// Vorbelegung für eine neue Hypothese.
pub(crate) const HYPOTHESIS_PREFILL: &str = "/workbench hypothesis ";
/// Befehl zum Bestätigen einer Hypothese (`<cmd> <id>`).
pub(crate) const HYPOTHESIS_CONFIRM_COMMAND: &str = "/workbench hypothesis confirm";
/// Befehl zum Verwerfen einer Hypothese (`<cmd> <id>`).
pub(crate) const HYPOTHESIS_REJECT_COMMAND: &str = "/workbench hypothesis reject";
/// Befehlskopf für Notiz-Bearbeitung/-Löschung (`note edit|rm <id>`).
pub(crate) const NOTE_COMMAND: &str = "/workbench note";
/// Nachlade-Befehl der Projekt-Werkbank.
pub(crate) const PROJECT_REFRESH_COMMAND: &str = "/workbench show --scope=project";
/// Höchstzahl angezeigter Notizzeilen.
const MAX_NOTE_LINES: usize = 12;
/// Höchstzahl Zeichen der einzeiligen Notizvorschau.
const NOTE_PREVIEW_CHARS: usize = 80;

/// Ergebnis eines Tastendrucks im fokussierten Panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneCommand {
    /// Taste ohne Wirkung.
    None,
    /// Zustand geändert; neu zeichnen.
    Redraw,
    /// Slash-Zeile ausführen.
    Run(String),
    /// Eingabezeile mit diesem Text vorbelegen und fokussieren.
    Prefill(String),
    /// Fokus an den Chat zurückgeben.
    ReleaseFocus,
}

/// Eine angeheftete Datei.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PinnedEntry {
    pub path: String,
    pub note: Option<String>,
    /// `present|changed|missing|not_a_file`; leer, wenn die Op nichts liefert.
    pub status: String,
    /// Erste Zeilen der Datei (nur für die Auswahl angezeigt).
    pub preview: Vec<String>,
}

/// Eine auswählbare Notiz.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NoteEntry {
    /// `#<n>` (verschiebt sich nach Löschungen).
    pub id: String,
    /// Zeitstempel der Notiz — stabile Auswahl für `note rm|edit`.
    pub at: String,
    pub text: String,
}

impl NoteEntry {
    /// Die stabilste verfügbare Auswahl: Zeitstempel, sonst `#<n>`.
    fn selector(&self) -> &str {
        if self.at.trim().is_empty() {
            &self.id
        } else {
            &self.at
        }
    }
}

/// Eine Hypothese der Werkbank.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HypothesisEntry {
    pub id: String,
    pub text: String,
    pub status: String,
}

/// Auswählbarer Eintrag (angeheftete Dateien zuerst, dann Hypothesen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Pinned(usize),
    Hypothesis(usize),
    Note(usize),
}

/// Zustand des Werkbank-Panels.
#[derive(Debug, Clone)]
pub(crate) struct WorkbenchPane {
    scope: String,
    pinned: Vec<PinnedEntry>,
    hypotheses: Vec<HypothesisEntry>,
    notes: Vec<NoteEntry>,
    notes_tail: Vec<String>,
    project_view: bool,
    selected: usize,
    stale: bool,
    loaded: bool,
    error: Option<String>,
    hint: Option<String>,
}

impl Default for WorkbenchPane {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkbenchPane {
    /// Slash-Zeile, deren `data` das Panel im Sitzungs-Scope füllt.
    pub(crate) const REFRESH_COMMAND: &'static str = "/workbench show";

    /// Die Nachlade-Zeile des gewählten Scopes (Sitzung oder Projekt).
    pub(crate) fn refresh_command(&self) -> &'static str {
        if self.project_view {
            PROJECT_REFRESH_COMMAND
        } else {
            Self::REFRESH_COMMAND
        }
    }

    /// `true`, wenn die Projekt-Werkbank gewählt ist.
    #[cfg(test)]
    pub(crate) fn is_project_view(&self) -> bool {
        self.project_view
    }

    /// ` --scope=<scope>` für Befehle, wenn ein Projekt-Scope angezeigt wird.
    fn scope_flag(&self) -> String {
        if self.scope.starts_with("project:") && !self.scope.contains(char::is_whitespace) {
            format!(" --scope={}", self.scope)
        } else {
            String::new()
        }
    }

    /// `/workbench<flag> <rest>` — der Scope steht vor dem Subcommand, damit
    /// Vorbelegungen am Ende weitergeschrieben werden können.
    fn scoped(&self, command: &str) -> String {
        let rest = command.strip_prefix("/workbench").unwrap_or(command);
        format!("/workbench{}{rest}", self.scope_flag())
    }

    /// Leeres Panel; gilt als veraltet, bis erste Daten ankommen.
    pub(crate) fn new() -> Self {
        Self {
            scope: String::new(),
            pinned: Vec::new(),
            hypotheses: Vec::new(),
            notes: Vec::new(),
            notes_tail: Vec::new(),
            project_view: false,
            selected: 0,
            stale: true,
            loaded: false,
            error: None,
            hint: None,
        }
    }

    /// Übernimmt das `data`-JSON von `/workbench show` (tolerant).
    pub(crate) fn apply_data(&mut self, data: &Value) {
        self.scope = str_field(data, "scope");
        self.pinned = array_field(data, "pinned")
            .iter()
            .filter_map(|item| match item {
                Value::String(path) => Some(PinnedEntry {
                    path: path.clone(),
                    ..PinnedEntry::default()
                }),
                Value::Object(_) => {
                    let path = str_field(item, "path");
                    (!path.is_empty()).then(|| PinnedEntry {
                        path,
                        note: opt_str_field(item, "note"),
                        status: str_field(item, "status"),
                        preview: notes_lines(item.get("preview")),
                    })
                }
                _ => None,
            })
            .collect();
        self.hypotheses = array_field(data, "hypotheses")
            .iter()
            .filter(|item| item.is_object())
            .map(|item| HypothesisEntry {
                id: str_field(item, "id"),
                text: str_field(item, "text"),
                status: str_field(item, "status"),
            })
            .collect();
        self.notes = array_field(data, "notes")
            .iter()
            .filter(|item| item.is_object())
            .map(|item| NoteEntry {
                id: str_field(item, "id"),
                at: str_field(item, "at"),
                text: str_field(item, "text"),
            })
            .filter(|note| !note.selector().trim().is_empty())
            .collect();
        self.notes_tail = notes_lines(data.get("notes_tail"));
        self.loaded = true;
        self.stale = false;
        self.error = None;
        self.hint = None;
        self.clamp_selection();
    }

    /// Merkt einen Ladefehler; die zuletzt gültigen Daten bleiben sichtbar.
    pub(crate) fn apply_error(&mut self, text: String) {
        self.error = Some(text);
        self.stale = false;
    }

    /// Markiert die Daten als veraltet (z. B. nach einer Schreibaktion).
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// `true`, wenn das Panel neu geladen werden sollte.
    pub(crate) fn is_stale(&self) -> bool {
        self.stale
    }

    /// Angeheftete Dateien (für Tests und Aufrufer).
    #[cfg(test)]
    pub(crate) fn pinned(&self) -> &[PinnedEntry] {
        &self.pinned
    }

    /// Hypothesen (für Tests und Aufrufer).
    #[cfg(test)]
    pub(crate) fn hypotheses(&self) -> &[HypothesisEntry] {
        &self.hypotheses
    }

    fn item_count(&self) -> usize {
        self.pinned.len() + self.hypotheses.len() + self.notes.len()
    }

    fn clamp_selection(&mut self) {
        let count = self.item_count();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    fn selection(&self) -> Option<Selection> {
        if self.selected < self.pinned.len() {
            return Some(Selection::Pinned(self.selected));
        }
        let index = self.selected - self.pinned.len();
        if index < self.hypotheses.len() {
            return Some(Selection::Hypothesis(index));
        }
        let index = index - self.hypotheses.len();
        (index < self.notes.len()).then_some(Selection::Note(index))
    }

    fn selected_note(&self) -> Option<&NoteEntry> {
        match self.selection() {
            Some(Selection::Note(index)) => self.notes.get(index),
            _ => None,
        }
    }

    /// Wechselt zwischen Sitzungs- und Projekt-Werkbank und lädt neu.
    fn toggle_scope(&mut self) -> PaneCommand {
        self.project_view = !self.project_view;
        self.selected = 0;
        self.stale = true;
        self.hint = None;
        PaneCommand::Run(self.refresh_command().to_owned())
    }

    fn selected_hypothesis(&self) -> Option<&HypothesisEntry> {
        match self.selection() {
            Some(Selection::Hypothesis(index)) => self.hypotheses.get(index),
            _ => None,
        }
    }

    fn hypothesis_command(&mut self, command: &str) -> PaneCommand {
        let Some(id) = self
            .selected_hypothesis()
            .map(|h| h.id.clone())
            .filter(|id| !id.is_empty())
        else {
            self.hint = Some("Keine Hypothese ausgewählt.".to_owned());
            return PaneCommand::Redraw;
        };
        self.hint = None;
        PaneCommand::Run(self.scoped(&format!("{command} {id}")))
    }

    /// Verarbeitet eine Taste des fokussierten Panels.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PaneCommand {
        if key.kind == KeyEventKind::Release {
            return PaneCommand::None;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return PaneCommand::None;
        }
        match key.code {
            KeyCode::Esc => PaneCommand::ReleaseFocus,
            KeyCode::Char('j') | KeyCode::Down => {
                if self.selected + 1 < self.item_count() {
                    self.selected += 1;
                    PaneCommand::Redraw
                } else {
                    PaneCommand::None
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    PaneCommand::Redraw
                } else {
                    PaneCommand::None
                }
            }
            KeyCode::Char('x') => match self.selection() {
                Some(Selection::Pinned(index)) => match self.pinned.get(index) {
                    Some(entry) => {
                        self.hint = None;
                        PaneCommand::Run(self.scoped(&format!("{UNPIN_COMMAND} {}", entry.path)))
                    }
                    None => PaneCommand::None,
                },
                Some(Selection::Note(index)) => match self.notes.get(index) {
                    Some(note) => {
                        self.hint = None;
                        PaneCommand::Run(
                            self.scoped(&format!("{NOTE_COMMAND} rm {}", note.selector())),
                        )
                    }
                    None => PaneCommand::None,
                },
                _ => {
                    self.hint = Some("Keine Datei oder Notiz ausgewählt.".to_owned());
                    PaneCommand::Redraw
                }
            },
            KeyCode::Char('e') => match self.selected_note() {
                Some(note) => {
                    let text = one_line(&note.text);
                    let line = format!("{NOTE_COMMAND} edit {} {text}", note.selector());
                    self.hint = None;
                    PaneCommand::Prefill(self.scoped(&line))
                }
                None => {
                    self.hint = Some("Keine Notiz ausgewählt.".to_owned());
                    PaneCommand::Redraw
                }
            },
            KeyCode::Char('s') => self.toggle_scope(),
            KeyCode::Char('n') => PaneCommand::Prefill(self.scoped(NOTE_PREFILL)),
            KeyCode::Char('h') => PaneCommand::Prefill(self.scoped(HYPOTHESIS_PREFILL)),
            KeyCode::Char('c') => self.hypothesis_command(HYPOTHESIS_CONFIRM_COMMAND),
            KeyCode::Char('r') => self.hypothesis_command(HYPOTHESIS_REJECT_COMMAND),
            KeyCode::Char('R') => {
                self.stale = true;
                PaneCommand::Run(self.refresh_command().to_owned())
            }
            _ => PaneCommand::None,
        }
    }

    /// Zeichnet das Panel.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme, focused: bool) {
        let mut title = String::from(" Werkbank");
        title.push_str(if self.project_view {
            " [Projekt]"
        } else {
            " [Sitzung]"
        });
        if !self.scope.is_empty() {
            title.push_str(" · ");
            title.push_str(&sanitize_inline(&self.scope));
        }
        if self.stale && self.loaded {
            title.push_str(" · veraltet");
        }
        title.push(' ');
        let border = if focused {
            Style::default().fg(style::accent_color(theme))
        } else {
            Style::default().fg(style::border_color(theme))
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title);
        let inner_height = usize::from(area.height.saturating_sub(2));
        let (lines, selected_line) = self.lines(theme, focused);
        let scroll = selected_line
            .map(|line| (line + 2).saturating_sub(inner_height))
            .unwrap_or(0);
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn lines(&self, theme: Theme, focused: bool) -> (Vec<Line<'static>>, Option<usize>) {
        let dim = style::dim_style(theme);
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected_line = None;
        if let Some(error) = &self.error {
            lines.push(Line::styled(
                format!("Fehler: {}", sanitize_inline(error)),
                style::error_style(theme),
            ));
        }
        if !self.loaded {
            if self.error.is_none() {
                lines.push(Line::styled("Lade Werkbank …", dim));
            }
            return (lines, None);
        }
        let current = self.selection();
        let marker = |sel: Selection| {
            if focused && current == Some(sel) {
                "▸ "
            } else {
                "  "
            }
        };

        lines.push(Line::styled(
            format!("Angeheftet ({})", self.pinned.len()),
            bold,
        ));
        if self.pinned.is_empty() {
            lines.push(Line::styled("  – nichts angeheftet –", dim));
        }
        for (index, entry) in self.pinned.iter().enumerate() {
            let sel = Selection::Pinned(index);
            if focused && current == Some(sel) {
                selected_line = Some(lines.len());
            }
            let path_style = if focused && current == Some(sel) {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::raw(marker(sel))];
            if let Some((label, badge_style)) = pin_badge(&entry.status, theme) {
                spans.push(Span::styled(format!("[{label}] "), badge_style));
            }
            spans.push(Span::styled(sanitize_inline(&entry.path), path_style));
            if let Some(note) = &entry.note {
                spans.push(Span::styled(format!(" — {}", sanitize_inline(note)), dim));
            }
            lines.push(Line::from(spans));
            if focused && current == Some(sel) {
                for preview in &entry.preview {
                    lines.push(Line::styled(
                        format!("      │ {}", sanitize_inline(preview)),
                        dim,
                    ));
                }
            }
        }

        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("Hypothesen ({})", self.hypotheses.len()),
            bold,
        ));
        if self.hypotheses.is_empty() {
            lines.push(Line::styled("  – keine Hypothesen –", dim));
        }
        for (index, entry) in self.hypotheses.iter().enumerate() {
            let sel = Selection::Hypothesis(index);
            if focused && current == Some(sel) {
                selected_line = Some(lines.len());
            }
            let text_style = if focused && current == Some(sel) {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let status = status_label(&entry.status);
            lines.push(Line::from(vec![
                Span::raw(marker(sel)),
                Span::styled(format!("[{status}] "), status_style(&entry.status, theme)),
                Span::styled(sanitize_inline(&entry.text), text_style),
                Span::styled(
                    if entry.id.is_empty() {
                        String::new()
                    } else {
                        format!(" #{}", sanitize_inline(&entry.id))
                    },
                    dim,
                ),
            ]));
        }

        if !self.notes.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("Notizen ({})", self.notes.len()),
                bold,
            ));
            for (index, note) in self.notes.iter().enumerate() {
                let sel = Selection::Note(index);
                if focused && current == Some(sel) {
                    selected_line = Some(lines.len());
                }
                let text_style = if focused && current == Some(sel) {
                    style::selected_style(theme)
                } else {
                    dim
                };
                let preview: String = one_line(&note.text)
                    .chars()
                    .take(NOTE_PREVIEW_CHARS)
                    .collect();
                lines.push(Line::from(vec![
                    Span::raw(marker(sel)),
                    Span::styled(sanitize_inline(&preview), text_style),
                    Span::styled(format!(" {}", sanitize_inline(&note.id)), dim),
                ]));
            }
        } else if !self.notes_tail.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("Notizen", bold));
            let skip = self.notes_tail.len().saturating_sub(MAX_NOTE_LINES);
            for note in self.notes_tail.iter().skip(skip) {
                lines.push(Line::styled(format!("  {}", sanitize_inline(note)), dim));
            }
        }

        if let Some(hint) = &self.hint {
            lines.push(Line::raw(""));
            lines.push(Line::styled(hint.clone(), style::warning_style(theme)));
        }
        if focused {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "j/k wählen · x lösen/löschen · e Notiz bearbeiten · n Notiz · h Hypothese · c/r bestätigen/verwerfen · s Sitzung/Projekt · R neu · Esc",
                dim,
            ));
        }
        (lines, selected_line)
    }
}

/// Abzeichen für den Dateizustand eines Pins; `None` bei „vorhanden“/unbekannt.
fn pin_badge(status: &str, theme: Theme) -> Option<(&'static str, Style)> {
    match status {
        "changed" => Some(("geändert", style::warning_style(theme))),
        "missing" => Some(("fehlt", style::error_style(theme))),
        "not_a_file" => Some(("keine Datei", style::error_style(theme))),
        _ => None,
    }
}

/// Faltet Zeilenumbrüche/Steuerzeichen zu Leerzeichen (Composer ist einzeilig).
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deutsche Kurzbezeichnung eines Hypothesen-Status.
fn status_label(status: &str) -> String {
    match status.to_ascii_lowercase().as_str() {
        "" | "open" | "offen" | "pending" | "testing" => "offen".to_owned(),
        "confirmed" | "bestätigt" => "bestätigt".to_owned(),
        "rejected" | "verworfen" => "verworfen".to_owned(),
        other => sanitize_inline(other),
    }
}

fn status_style(status: &str, theme: Theme) -> Style {
    match status.to_ascii_lowercase().as_str() {
        "confirmed" | "bestätigt" => style::success_style(theme),
        "rejected" | "verworfen" => style::error_style(theme),
        _ => style::warning_style(theme),
    }
}

/// `notes_tail` darf ein String (mehrzeilig) oder ein Array aus Strings bzw.
/// Objekten mit `text` sein.
fn notes_lines(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => opt_str_field(item, "text"),
                _ => None,
            })
            .filter(|line| !line.trim().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Liest ein Feld als Text; Zahlen und Booleans werden formatiert, alles
/// andere (fehlend, `null`, Objekt) ergibt einen leeren String.
fn str_field(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// Wie [`str_field`], aber leer/fehlend → `None`.
fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    let text = str_field(value, key);
    (!text.trim().is_empty()).then_some(text)
}

/// Liest ein Array-Feld; alles andere ergibt eine leere Liste.
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
            "scope": "projekt",
            "pinned": [
                {"path": "src/a.rs", "note": "Einstieg"},
                {"path": "src/b.rs"},
                {"note": "ohne Pfad"},
                "src/c.rs"
            ],
            "hypotheses": [
                {"id": "h1", "text": "Race im Cache", "status": "open"},
                {"id": 7, "text": "Timeout", "status": "confirmed"},
                42
            ],
            "notes_tail": "erste Notiz\n\nzweite Notiz"
        })
    }

    fn render_to_string(pane: &WorkbenchPane, focused: bool) -> TestResult<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24))?;
        terminal.draw(|frame| {
            let area = frame.area();
            pane.render(area, frame.buffer_mut(), Theme::Dark, focused);
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
    fn parses_tolerantly() {
        let mut pane = WorkbenchPane::new();
        assert!(pane.is_stale());
        pane.apply_data(&sample());
        assert!(!pane.is_stale());
        assert_eq!(pane.pinned().len(), 3);
        assert_eq!(pane.pinned()[0].note.as_deref(), Some("Einstieg"));
        assert_eq!(pane.pinned()[1].note, None);
        assert_eq!(pane.pinned()[2].path, "src/c.rs");
        assert_eq!(pane.hypotheses().len(), 2);
        assert_eq!(pane.hypotheses()[1].id, "7");
        assert_eq!(pane.notes_tail, vec!["erste Notiz", "zweite Notiz"]);
    }

    #[test]
    fn empty_or_garbage_data_yields_defaults() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&json!({"pinned": "kaputt", "hypotheses": null}));
        assert!(pane.pinned().is_empty());
        assert!(pane.hypotheses().is_empty());
        pane.apply_data(&json!(17));
        assert!(pane.scope.is_empty());
        assert_eq!(pane.handle_key(key(KeyCode::Char('j'))), PaneCommand::None);
    }

    #[test]
    fn notes_tail_accepts_array() {
        assert_eq!(
            notes_lines(Some(&json!(["a", {"text": "b"}, 3, ""]))),
            vec!["a", "b"]
        );
    }

    #[test]
    fn keys_produce_commands() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&sample());
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Run("/workbench unpin src/a.rs".into())
        );
        // Auf einer Datei gibt es keine Hypothese zu bestätigen.
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('c'))),
            PaneCommand::Redraw
        );
        for _ in 0..3 {
            assert_eq!(
                pane.handle_key(key(KeyCode::Char('j'))),
                PaneCommand::Redraw
            );
        }
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('c'))),
            PaneCommand::Run("/workbench hypothesis confirm h1".into())
        );
        assert_eq!(pane.handle_key(key(KeyCode::Down)), PaneCommand::Redraw);
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('r'))),
            PaneCommand::Run("/workbench hypothesis reject 7".into())
        );
        assert_eq!(pane.handle_key(key(KeyCode::Char('j'))), PaneCommand::None);
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Redraw
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('k'))),
            PaneCommand::Redraw
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('n'))),
            PaneCommand::Prefill("/workbench note ".into())
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('h'))),
            PaneCommand::Prefill("/workbench hypothesis ".into())
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('R'))),
            PaneCommand::Run(WorkbenchPane::REFRESH_COMMAND.into())
        );
        assert!(pane.is_stale());
        assert_eq!(
            pane.handle_key(key(KeyCode::Esc)),
            PaneCommand::ReleaseFocus
        );
    }

    #[test]
    fn selection_is_clamped_after_shrinking_data() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&sample());
        for _ in 0..10 {
            pane.handle_key(key(KeyCode::Char('j')));
        }
        pane.apply_data(&json!({"pinned": [{"path": "x"}]}));
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Run("/workbench unpin x".into())
        );
    }

    #[test]
    fn error_and_stale_flags() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&sample());
        pane.mark_stale();
        assert!(pane.is_stale());
        pane.apply_error("kaputt".into());
        assert!(!pane.is_stale());
        assert_eq!(pane.pinned().len(), 3);
        pane.apply_data(&sample());
        assert!(pane.error.is_none());
    }

    fn project_sample() -> Value {
        json!({
            "scope": "project:harw",
            "pinned": [
                {"path": "/p/a.rs", "status": "changed", "preview": ["fn main() {", "}"]},
                {"path": "/p/weg.rs", "status": "missing", "preview": []}
            ],
            "hypotheses": [{"id": "#1", "text": "H", "status": "testing"}],
            "notes": [
                {"id": "#1", "at": "2026-09-24T10:00:00Z", "text": "erste\nzweite Zeile"},
                {"id": "#2", "at": "", "text": "ohne Zeit"}
            ],
            "notes_tail": ["erste", "zweite Zeile", "ohne Zeit"]
        })
    }

    #[test]
    fn scope_toggle_switches_the_refresh_command() {
        let mut pane = WorkbenchPane::new();
        assert_eq!(pane.refresh_command(), WorkbenchPane::REFRESH_COMMAND);
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('s'))),
            PaneCommand::Run(PROJECT_REFRESH_COMMAND.into())
        );
        assert!(pane.is_project_view());
        assert!(pane.is_stale());
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('R'))),
            PaneCommand::Run(PROJECT_REFRESH_COMMAND.into())
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('s'))),
            PaneCommand::Run(WorkbenchPane::REFRESH_COMMAND.into())
        );
        assert!(!pane.is_project_view());
    }

    #[test]
    fn project_scope_is_carried_into_every_command() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&project_sample());
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Run("/workbench --scope=project:harw unpin /p/a.rs".into())
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('n'))),
            PaneCommand::Prefill("/workbench --scope=project:harw note ".into())
        );
        pane.handle_key(key(KeyCode::Char('j')));
        pane.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('c'))),
            PaneCommand::Run("/workbench --scope=project:harw hypothesis confirm #1".into())
        );
    }

    #[test]
    fn notes_are_selectable_for_edit_and_remove() {
        let mut pane = WorkbenchPane::new();
        pane.apply_data(&json!({
            "scope": "session:s-1",
            "notes": [
                {"id": "#1", "at": "2026-09-24T10:00:00Z", "text": "erste\nzweite Zeile"},
                {"id": "#2", "at": "", "text": "ohne Zeit"},
                {"text": "ohne Auswahl"}
            ]
        }));
        assert_eq!(pane.notes.len(), 2);
        // Auf einer Notiz gibt es keine Hypothese, aber `e` und `x` wirken.
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('e'))),
            PaneCommand::Prefill(
                "/workbench note edit 2026-09-24T10:00:00Z erste zweite Zeile".into()
            )
        );
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Run("/workbench note rm 2026-09-24T10:00:00Z".into())
        );
        pane.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('x'))),
            PaneCommand::Run("/workbench note rm #2".into())
        );
        // Ohne Notiz-Auswahl meldet `e` nur einen Hinweis.
        pane.apply_data(&json!({"pinned": [{"path": "/a"}]}));
        assert_eq!(
            pane.handle_key(key(KeyCode::Char('e'))),
            PaneCommand::Redraw
        );
        assert!(pane.hint.is_some());
    }

    #[test]
    fn renders_pin_status_preview_and_notes() -> TestResult {
        let mut pane = WorkbenchPane::new();
        pane.handle_key(key(KeyCode::Char('s')));
        pane.apply_data(&project_sample());
        let out = render_to_string(&pane, true)?;
        assert!(out.contains("Werkbank [Projekt] · project:harw"), "{out}");
        assert!(out.contains("▸ [geändert] /p/a.rs"), "{out}");
        assert!(out.contains("│ fn main() {"), "{out}");
        assert!(out.contains("[fehlt] /p/weg.rs"), "{out}");
        assert!(out.contains("[offen] H"), "{out}");
        assert!(out.contains("Notizen (2)"), "{out}");
        assert!(out.contains("erste zweite Zeile #1"), "{out}");
        // Die Vorschau erscheint nur für die Auswahl.
        pane.handle_key(key(KeyCode::Char('j')));
        let moved = render_to_string(&pane, true)?;
        assert!(!moved.contains("│ fn main() {"), "{moved}");
        Ok(())
    }

    #[test]
    fn renders_sections() -> TestResult {
        let mut pane = WorkbenchPane::new();
        assert!(render_to_string(&pane, false)?.contains("Lade Werkbank"));
        pane.apply_data(&sample());
        let out = render_to_string(&pane, true)?;
        assert!(out.contains("Werkbank [Sitzung] · projekt"));
        assert!(out.contains("▸ src/a.rs — Einstieg"));
        assert!(out.contains("[bestätigt] Timeout"));
        assert!(out.contains("zweite Notiz"));
        Ok(())
    }
}

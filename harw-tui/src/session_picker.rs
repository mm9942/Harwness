//! Session-Picker: filterbare Liste vergangener Sitzungen zum Fortsetzen.
//!
//! Spec-Quelle: Slice A6 des Kontraktdokuments
//! `docs/design/tui-command-contract.md`.
//! Struktur und Rendering-Stil sind bewusst an [`crate::command_popup`]
//! angelehnt (selbstzeichnendes Widget, `on_key`/`handle_key`-Muster,
//! theme-abhängige Stile aus [`crate::style`]).
//!
//! # Verantwortung
//! - Hält eine Liste von [`SessionEntry`]-Einträgen, sortiert nach zuletzt
//!   aktiv (neueste zuerst).
//! - Filtert live nach Tipp-Eingaben (case-insensitive Substring auf Titel
//!   und Projekt-Label, Präfix auf die Sitzungs-ID).
//! - Zeichnet sich selbst in einen ratatui-`Buffer` (kein `StatefulWidget`,
//!   analog zu `CommandPopup`).
//! - Gibt [`PickerAction`]-Ereignisse an den Aufrufer zurück (Bleiben /
//!   Abbrechen / Öffnen einer Sitzungs-ID).
//!
//! # Zeittyp-Entscheidung
//! Wie in `relative_time.rs` begründet: `jiff` ist keine Abhängigkeit von
//! `harw-tui`, und dieser Auftrag darf `Cargo.toml` nicht ändern. Deshalb
//! verwendet [`SessionEntry::last_active`] `std::time::SystemTime` statt
//! `jiff::Timestamp`.
//!
//! # Nebenläufigkeit
//! Kein interner Zustand wird geteilt; der Aufrufer hält `SessionPicker`
//! exklusiv (`&mut`), analog zu `CommandPopup`.
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::time::SystemTime;

use crate::relative_time::relative_time;
use crate::sanitize;
use crate::style::{self, Theme};

/// Anzahl Einträge, um die `PageUp`/`PageDown` die Auswahl verschieben.
const PAGE_STEP: isize = 10;

/// Spaltenbreite (in Zeichen) für die rechts aufgefüllte Relativzeit-Spalte.
const RELTIME_COLUMN_WIDTH: usize = 12;

/// Breite (in Spalten) der Auswahl-Markierung ("❯ " bzw. zwei Leerzeichen).
const MARKER_WIDTH: usize = 2;

/// Titel des umrahmenden Popup-Rahmens.
const POPUP_TITLE: &str = " Session fortsetzen ";

/// Fußzeile mit Tastaturhinweisen.
const FOOTER_HINT: &str = "↑↓ wählen · Enter öffnen · Esc abbrechen · Ctrl+A alle Projekte";

/// Hinweistext in der Filterzeile, solange noch nichts getippt wurde.
const FILTER_PLACEHOLDER: &str = "Tippen zum Filtern";

/// Text für den Leerzustand (kein Eintrag passt zum Filter).
const EMPTY_STATE_TEXT: &str = "Keine Sessions gefunden";

/// Ein einzelner Eintrag der Sitzungsliste im Picker.
///
/// # Beschreibung
/// Trägt alle Anzeigeinformationen für eine fortsetzbare Sitzung. Der
/// Aufrufer (Session-Store-Integration) befüllt diese Liste; der Picker
/// selbst lädt keine Daten nach. (Spec-Abschnitt Session-Picker)
///
/// # Felder
/// - `id` (`String`): eindeutige Sitzungs-ID, wird bei `Enter` zurückgegeben.
/// - `title` (`String`): Anzeigetitel der Sitzung.
/// - `last_active` (`SystemTime`): Zeitpunkt der letzten Aktivität, für
///   Sortierung und die relative Zeitangabe.
/// - `project_label` (`Option<String>`): z. B. `~/projects/harwness`, wird
///   nur angezeigt wenn Platz ist.
/// - `turns` (`Option<u64>`): Anzahl Gesprächsrunden, sofern bekannt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Eindeutige Sitzungs-ID.
    pub id: String,
    /// Anzeigetitel der Sitzung.
    pub title: String,
    /// Zeitpunkt der letzten Aktivität.
    pub last_active: SystemTime,
    /// Optionales Projekt-Label, z. B. `~/projects/harwness`.
    pub project_label: Option<String>,
    /// Optionale Anzahl Gesprächsrunden.
    pub turns: Option<u64>,
}

/// Aktionssignal, das `SessionPicker::handle_key` (crate-intern) zurückgibt.
///
/// # Beschreibung
/// Teilt dem Aufrufer mit, was mit dem Picker geschehen soll: offen lassen,
/// schließen (Abbruch) oder eine Sitzung öffnen. (Spec-Abschnitt Session-Picker)
///
/// # Varianten
/// - `Stay`: Picker bleibt offen; keine weitere Aktion.
/// - `Cancel`: Nutzer hat den Picker abgebrochen (Esc bei leerem Filter).
/// - `Open(id)`: Nutzer hat eine Sitzung ausgewählt; enthält deren ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Picker bleibt geöffnet, keine weitere Aktion erforderlich.
    Stay,
    /// Nutzer hat abgebrochen; Picker soll geschlossen werden.
    Cancel,
    /// Nutzer hat eine Sitzung gewählt; enthält deren `id`.
    Open(String),
}

/// Zustand des Session-Pickers mit Filterlogik, Navigation und Rendering.
///
/// # Beschreibung
/// Verwaltet die vollständige, nach `last_active` absteigend sortierte
/// Liste aller Sitzungen sowie den aktuellen Filter- und Auswahlzustand.
/// Das Widget zeichnet sich selbst direkt in einen ratatui-`Buffer`.
/// (Spec-Abschnitt Session-Picker)
///
/// `filtered` ist kein im Auftrag benanntes Feld, sondern eine interne
/// Zwischenrepräsentation (Indizes in `entries`, die dem aktuellen `filter`
/// entsprechen) — analog zu `CommandPopup::filtered`. `scroll` wird bei
/// jeder Filteränderung auf `0` zurückgesetzt; die tatsächliche
/// Bildlauf-Position, die die Auswahl sichtbar hält, wird zusätzlich rein
/// aus `selected` und der verfügbaren Zeilenzahl in [`SessionPicker::render`]
/// berechnet, da `render` nur `&self` erhält und daher keinen Zustand
/// zurückschreiben kann.
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; exklusiver `&mut`-Zugriff des Aufrufers erwartet.
#[derive(Debug, Clone)]
pub struct SessionPicker {
    /// Vollständige, nach `last_active` absteigend sortierte Liste.
    entries: Vec<SessionEntry>,
    /// Aktueller Filtertext (Rohtext, wie eingegeben).
    filter: String,
    /// Index in `filtered` — welcher gefilterte Eintrag markiert ist.
    selected: usize,
    /// Zuletzt bekannte Bildlauf-Basiszeile (siehe Typ-Dokumentation).
    scroll: usize,
    /// Ob der Aufrufer Sitzungen aller Projekte (statt nur des aktuellen)
    /// laden soll. Wird per `Ctrl+A` umgeschaltet; der Aufrufer beobachtet
    /// [`SessionPicker::show_all`] und ruft danach [`SessionPicker::set_entries`]
    /// mit der neu geladenen Liste auf.
    show_all: bool,
    /// Referenzzeitpunkt ("jetzt") für relative Zeitangaben.
    now: SystemTime,
    /// Indizes in `entries`, die dem aktuellen `filter` entsprechen.
    filtered: Vec<usize>,
}

/// Prüft, ob ein Eintrag zu einem (bereits kleingeschriebenen) Suchtext passt.
///
/// # Beschreibung
/// Case-insensitiver Substring-Treffer auf `title` und `project_label`,
/// Präfix-Treffer auf `id`. Ein leerer Suchtext trifft immer.
/// (Spec-Abschnitt Session-Picker: "case-insensitive substring match on
/// title, project_label and id prefix")
fn entry_matches(entry: &SessionEntry, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if entry.title.to_ascii_lowercase().contains(needle_lower) {
        return true;
    }
    if let Some(label) = &entry.project_label {
        if label.to_ascii_lowercase().contains(needle_lower) {
            return true;
        }
    }
    entry.id.to_ascii_lowercase().starts_with(needle_lower)
}

/// Kürzt `s` char-sicher (keine UTF-8-Byte-Grenzen-Panik) auf höchstens
/// `max_chars` sichtbare Zeichen und hängt bei Kürzung `"…"` an.
///
/// # Argumente
/// - `s` (`&str`): der zu kürzende Text.
/// - `max_chars` (`usize`): maximale Zeichenzahl der Ausgabe (inklusive `…`).
///
/// # Rückgabe
/// `s` unverändert, falls es bereits kurz genug ist; sonst ein gekürztes
/// Präfix mit angehängtem `…`. Bei `max_chars == 0` wird `""` zurückgegeben.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    if max_chars == 1 {
        return "…".to_owned();
    }
    let mut truncated: String = s.chars().take(max_chars - 1).collect();
    truncated.push('…');
    truncated
}

/// Berechnet die Bildlauf-Basiszeile, sodass `selected` innerhalb des
/// sichtbaren Fensters (`visible` Zeilen von `total`) liegt.
///
/// # Beschreibung
/// Klassische "Fenster hinter Cursor herziehen"-Logik: die Basiszeile wird
/// nur soweit erhöht, wie nötig ist, damit `selected` die letzte sichtbare
/// Zeile ist, und nie über `total - visible` hinaus.
fn clamp_scroll(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let max_scroll = total - visible;
    selected.saturating_sub(visible - 1).min(max_scroll)
}

impl SessionPicker {
    /// Erstellt einen neuen `SessionPicker` aus einer Liste von Sitzungen.
    ///
    /// # Beschreibung
    /// Sortiert `entries` absteigend nach `last_active` (neueste zuerst),
    /// setzt Filter/Auswahl zurück und initialisiert die gefilterte Liste
    /// auf alle Einträge. (Spec-Abschnitt Session-Picker)
    ///
    /// # Argumente
    /// - `entries` (`Vec<SessionEntry>`): die anzuzeigenden Sitzungen.
    /// - `now` (`SystemTime`): Referenzzeitpunkt für relative Zeitangaben.
    ///
    /// # Rückgabe
    /// Initialisierter `SessionPicker` mit vollständiger, ungefilterter,
    /// nach Aktualität sortierter Liste.
    pub fn new(mut entries: Vec<SessionEntry>, now: SystemTime) -> Self {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_active));
        let filtered = (0..entries.len()).collect();
        Self {
            entries,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            show_all: false,
            now,
            filtered,
        }
    }

    /// Ersetzt die Sitzungsliste (z. B. nach `Ctrl+A`-Umschaltung von
    /// [`SessionPicker::show_all`]) und wendet den aktuellen Filter erneut an.
    ///
    /// # Beschreibung
    /// Sortiert die neue Liste absteigend nach `last_active`, behält den
    /// bestehenden Filtertext bei und klemmt `selected` auf die neue
    /// gefilterte Länge. (Spec-Abschnitt Session-Picker: "the caller reloads
    /// entries; provide `set_entries`")
    ///
    /// # Argumente
    /// - `entries` (`Vec<SessionEntry>`): die neu geladenen Sitzungen.
    pub fn set_entries(&mut self, mut entries: Vec<SessionEntry>) {
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.last_active));
        self.entries = entries;
        let filter = std::mem::take(&mut self.filter);
        self.set_filter(filter);
    }

    /// Gibt zurück, ob der Aufrufer Sitzungen aller Projekte laden soll.
    ///
    /// # Beschreibung
    /// Wird per `Ctrl+A` in `SessionPicker::handle_key` umgeschaltet.
    /// Der Aufrufer beobachtet diesen Wert und lädt bei Änderung eine neue
    /// Liste, die er über [`SessionPicker::set_entries`] einspielt.
    ///
    /// # Rückgabe
    /// `true` wenn Sitzungen aller Projekte angezeigt werden sollen.
    pub fn show_all(&self) -> bool {
        self.show_all
    }

    /// Gibt den aktuell markierten Eintrag zurück, falls die gefilterte
    /// Liste nicht leer ist.
    ///
    /// # Rückgabe
    /// `Some(&SessionEntry)` des markierten Eintrags oder `None` bei leerer
    /// gefilterter Liste.
    pub fn selected_entry(&self) -> Option<&SessionEntry> {
        let &entry_idx = self.filtered.get(self.selected)?;
        self.entries.get(entry_idx)
    }

    /// Setzt den Filtertext und berechnet die gefilterte Liste neu.
    ///
    /// # Beschreibung
    /// Klemmt `selected` auf die neue gefilterte Länge und setzt `scroll`
    /// zurück, damit ein verkürzter Filter nicht auf eine nun ungültige
    /// Bildlaufposition zeigt.
    fn set_filter(&mut self, filter: String) {
        let needle_lower = filter.to_ascii_lowercase();
        self.filter = filter;
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry_matches(entry, &needle_lower))
            .map(|(idx, _)| idx)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.scroll = 0;
    }

    /// Bewegt die Markierung um `delta` Positionen innerhalb der gefilterten
    /// Liste, geklemmt auf `[0, filtered.len() - 1]`.
    fn move_by(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max_index = self.filtered.len() as isize - 1;
        let new_index = (self.selected as isize + delta).clamp(0, max_index);
        self.selected = new_index as usize;
    }

    /// Verarbeitet einen Tastendruck und gibt eine [`PickerAction`] zurück.
    ///
    /// # Beschreibung
    /// Steuert Navigation (`Up`/`Down`/`PageUp`/`PageDown`/`Home`/`End`),
    /// Filtereingabe (druckbare Zeichen, `Backspace`), Abbruch (`Esc`,
    /// löscht zuerst einen nicht-leeren Filter), Auswahl (`Enter`) und das
    /// Umschalten von `show_all` (`Ctrl+A`). Es gibt absichtlich **keine**
    /// Buchstaben-Navigation — jeder druckbare Buchstabe geht in den Filter.
    /// (Spec-Abschnitt Session-Picker)
    ///
    /// # Argumente
    /// - `key` (`KeyEvent`): das eingegangene Crossterm-Tastenereignis.
    ///
    /// # Rückgabe
    /// - `PickerAction::Cancel` bei `Esc` und leerem Filter.
    /// - `PickerAction::Open(id)` bei `Enter` mit nicht-leerer gefilterter Liste.
    /// - `PickerAction::Stay` in allen anderen Fällen (inklusive `Enter` bei
    ///   leerer Liste und `Esc` bei nicht-leerem Filter, der dabei geleert wird).
    ///
    /// # Sichtbarkeit
    /// `pub(crate)`: der Parameter ist ein Crossterm-Typ (Fremdcrate vor 1.0),
    /// der nicht in der öffentlichen API stehen soll; einziger Aufrufer ist
    /// `handle_overlay_key` in `app.rs`.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Up => {
                self.move_by(-1);
                PickerAction::Stay
            }
            KeyCode::Down => {
                self.move_by(1);
                PickerAction::Stay
            }
            KeyCode::PageUp => {
                self.move_by(-PAGE_STEP);
                PickerAction::Stay
            }
            KeyCode::PageDown => {
                self.move_by(PAGE_STEP);
                PickerAction::Stay
            }
            KeyCode::Home => {
                self.selected = 0;
                PickerAction::Stay
            }
            KeyCode::End => {
                self.selected = self.filtered.len().saturating_sub(1);
                PickerAction::Stay
            }
            KeyCode::Enter => match self.selected_entry() {
                Some(entry) => PickerAction::Open(entry.id.clone()),
                None => PickerAction::Stay,
            },
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    PickerAction::Cancel
                } else {
                    self.set_filter(String::new());
                    PickerAction::Stay
                }
            }
            KeyCode::Backspace => {
                if !self.filter.is_empty() {
                    let mut filter = std::mem::take(&mut self.filter);
                    filter.pop();
                    self.set_filter(filter);
                }
                PickerAction::Stay
            }
            KeyCode::Char(c)
                if (c == 'a' || c == 'A') && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.show_all = !self.show_all;
                PickerAction::Stay
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut filter = std::mem::take(&mut self.filter);
                filter.push(c);
                self.set_filter(filter);
                PickerAction::Stay
            }
            _ => PickerAction::Stay,
        }
    }

    /// Baut die Spans für eine einzelne Listenzeile (Marker, Relativzeit,
    /// Titel, optionales Suffix aus Turns/Projekt-Label).
    fn build_row_line(
        &self,
        entry: &SessionEntry,
        is_selected: bool,
        width: usize,
        theme: Theme,
    ) -> Line<'static> {
        let marker_style = if is_selected {
            style::selected_style(theme)
        } else {
            Style::default()
        };
        let marker_text = if is_selected { "❯ " } else { "  " };

        let reltime = relative_time(entry.last_active, self.now);
        let reltime_padded = format!("{reltime:<RELTIME_COLUMN_WIDTH$}");

        let remaining = width.saturating_sub(MARKER_WIDTH + RELTIME_COLUMN_WIDTH);

        let mut suffix_parts: Vec<String> = Vec::new();
        if let Some(turns) = entry.turns {
            suffix_parts.push(format!("· {turns} Turns"));
        }
        if let Some(label) = &entry.project_label {
            suffix_parts.push(sanitize::sanitize_inline(label));
        }
        let mut suffix = suffix_parts.join("  ");
        let mut suffix_reserved = if suffix.is_empty() {
            0
        } else {
            suffix.chars().count() + 2
        };
        // Nur anzeigen, wenn neben dem Suffix noch sinnvoll Platz für den
        // Titel bleibt; sonst Suffix ganz weglassen statt den Titel auf
        // Unlesbarkeit zu kürzen.
        if suffix_reserved > 0 && suffix_reserved + 3 > remaining {
            suffix.clear();
            suffix_reserved = 0;
        }

        let title_width = remaining.saturating_sub(suffix_reserved);
        let title_sanitized = sanitize::sanitize_inline(&entry.title);
        let title_truncated = truncate_chars(&title_sanitized, title_width);
        let title_display = format!("{title_truncated:<title_width$}");

        let title_style = if is_selected {
            style::selected_style(theme)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::styled(marker_text.to_owned(), marker_style),
            Span::styled(reltime_padded, style::dim_style(theme)),
            Span::styled(title_display, title_style),
        ];
        if !suffix.is_empty() {
            spans.push(Span::styled(format!("  {suffix}"), style::dim_style(theme)));
        }

        Line::from(spans)
    }

    /// Zeichnet den Session-Picker (Rahmen, Filterzeile, Liste, Fußzeile) in
    /// den angegebenen `Buffer`-Bereich.
    ///
    /// # Beschreibung
    /// Layout von oben nach unten: umrahmender `Block` mit Titel
    /// `" Session fortsetzen "`; Filterzeile (`🔍 <filter>` oder gedimmter
    /// Platzhalter); Liste der gefilterten Einträge (Bildlauf hält die
    /// Auswahl sichtbar, siehe [`clamp_scroll`]) oder gedimmter Leerzustand;
    /// gedimmte Fußzeile mit Tastaturhinweisen. Fremdtext (Titel,
    /// Projekt-Label, Filter) wird über [`crate::sanitize::sanitize_inline`]
    /// terminal-sicher aufbereitet. (Spec-Abschnitt Session-Picker)
    ///
    /// # Argumente
    /// - `area` (`Rect`): der Zeichenbereich im Terminal-Buffer.
    /// - `buf` (`&mut Buffer`): der ratatui-Buffer, in den geschrieben wird.
    /// - `theme` (`&Theme`): aktives Farbschema für Auswahl- und Dim-Stile.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; kein Locking erforderlich.
    ///
    /// # Sichtbarkeit
    /// `pub(crate)` statt `pub`: [`Theme`] ist crate-privat (`style`-Modul ist
    /// `pub(crate)`, siehe `lib.rs`) — eine `pub`-Methode mit `&Theme`-Parameter
    /// wäre von außerhalb der Crate ohnehin nicht aufrufbar gewesen, da externer
    /// Code keinen `Theme`-Wert konstruieren kann. Konsistent mit
    /// `history_cell`/`style`/`approval_dialog::ApprovalDialog::render`
    /// (alle `pub(crate)`); nur der Zustand (`SessionPicker` selbst,
    /// `set_entries`, `selected_entry`, `show_all`) bleibt öffentlich;
    /// `handle_key` ist ebenfalls `pub(crate)`, weil es einen Crossterm-Typ
    /// nimmt.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let theme = *theme;
        let block = Block::default().borders(Borders::ALL).title(POPUP_TITLE);
        let inner = block.inner(area);
        Widget::render(block, area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Filterzeile (immer die erste Innenzeile).
        let filter_area = Rect::new(inner.x, inner.y, inner.width, 1);
        if self.filter.is_empty() {
            Widget::render(
                Line::styled(FILTER_PLACEHOLDER, style::dim_style(theme)),
                filter_area,
                buf,
            );
        } else {
            let filter_text = format!("🔍 {}", sanitize::sanitize_inline(&self.filter));
            Widget::render(Line::raw(filter_text), filter_area, buf);
        }

        if inner.height < 2 {
            return;
        }

        // Fußzeile (immer die letzte Innenzeile, sofern Platz vorhanden).
        let footer_area = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        Widget::render(
            Line::styled(FOOTER_HINT, style::dim_style(theme)),
            footer_area,
            buf,
        );

        let list_top = inner.y + 1;
        let list_height = inner.height.saturating_sub(2) as usize;
        if list_height == 0 {
            return;
        }

        if self.filtered.is_empty() {
            let empty_area = Rect::new(inner.x, list_top, inner.width, 1);
            Widget::render(
                Line::styled(EMPTY_STATE_TEXT, style::dim_style(theme)),
                empty_area,
                buf,
            );
            return;
        }

        let scroll = clamp_scroll(self.selected, self.filtered.len(), list_height);

        for (row_idx, &entry_idx) in self
            .filtered
            .iter()
            .enumerate()
            .skip(scroll)
            .take(list_height)
        {
            let display_row = row_idx - scroll;
            let y = list_top + display_row as u16;
            let row_area = Rect::new(inner.x, y, inner.width, 1);
            let is_selected = row_idx == self.selected;
            let entry = &self.entries[entry_idx];
            let line = self.build_row_line(entry, is_selected, inner.width as usize, theme);
            Widget::render(line, row_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_entry(id: &str, title: &str, secs_ago: u64, now: SystemTime) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            title: title.to_owned(),
            last_active: now - Duration::from_secs(secs_ago),
            project_label: None,
            turns: None,
        }
    }

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// `new` sortiert Einträge absteigend nach `last_active` (neueste zuerst).
    #[test]
    fn test_new_sorts_newest_first() {
        let now = SystemTime::now();
        let entries = vec![
            make_entry("old", "Alt", 3600, now),
            make_entry("new", "Neu", 60, now),
            make_entry("mid", "Mitte", 600, now),
        ];
        let picker = SessionPicker::new(entries, now);
        assert_eq!(picker.entries[0].id, "new");
        assert_eq!(picker.entries[1].id, "mid");
        assert_eq!(picker.entries[2].id, "old");
    }

    /// Navigation mit `Down`/`Up` bleibt innerhalb der Grenzen.
    #[test]
    fn test_navigation_stays_in_bounds() {
        let now = SystemTime::now();
        let entries = vec![
            make_entry("a", "A", 10, now),
            make_entry("b", "B", 20, now),
            make_entry("c", "C", 30, now),
        ];
        let mut picker = SessionPicker::new(entries, now);

        picker.handle_key(make_key(KeyCode::Up));
        assert_eq!(picker.selected, 0, "move_up bei 0 bleibt bei 0");

        for _ in 0..10 {
            picker.handle_key(make_key(KeyCode::Down));
        }
        assert_eq!(picker.selected, 2, "move_down klemmt am Ende der Liste");
    }

    /// `PageDown` bewegt um bis zu 10 Positionen, klemmt am Ende.
    #[test]
    fn test_page_down_clamps() {
        let now = SystemTime::now();
        let entries: Vec<SessionEntry> = (0..5)
            .map(|i| make_entry(&format!("id{i}"), &format!("Titel {i}"), i as u64 * 10, now))
            .collect();
        let mut picker = SessionPicker::new(entries, now);

        picker.handle_key(make_key(KeyCode::PageDown));
        assert_eq!(picker.selected, 4, "PageDown klemmt auf den letzten Index");
    }

    /// Filter engt die Liste ein und setzt die Auswahl zurück.
    #[test]
    fn test_filter_narrows_and_resets_selection() {
        let now = SystemTime::now();
        let entries = vec![
            make_entry("a", "Alpha Projekt", 10, now),
            make_entry("b", "Beta Projekt", 20, now),
            make_entry("c", "Gamma Task", 30, now),
        ];
        let mut picker = SessionPicker::new(entries, now);
        picker.handle_key(make_key(KeyCode::Down));
        picker.handle_key(make_key(KeyCode::Down));
        assert_eq!(picker.selected, 2);

        for c in "Projekt".chars() {
            picker.handle_key(make_key(KeyCode::Char(c)));
        }
        assert_eq!(picker.filtered.len(), 2, "nur 'Projekt'-Titel passen");
        assert_eq!(picker.selected, 1, "Auswahl wird auf neue Länge geklemmt");
    }

    /// `Enter` liefert `PickerAction::Open` mit der ID des markierten Eintrags.
    #[test]
    fn test_enter_returns_open_with_id() {
        let now = SystemTime::now();
        let entries = vec![make_entry("session-42", "Titel", 10, now)];
        let mut picker = SessionPicker::new(entries, now);

        let action = picker.handle_key(make_key(KeyCode::Enter));
        assert_eq!(action, PickerAction::Open("session-42".to_owned()));
    }

    /// `Enter` bei leerer gefilterter Liste liefert `Stay`.
    #[test]
    fn test_enter_on_empty_filtered_list_is_stay() {
        let now = SystemTime::now();
        let entries = vec![make_entry("a", "Alpha", 10, now)];
        let mut picker = SessionPicker::new(entries, now);
        for c in "zzz_kein_treffer".chars() {
            picker.handle_key(make_key(KeyCode::Char(c)));
        }
        assert!(picker.filtered.is_empty());
        assert_eq!(
            picker.handle_key(make_key(KeyCode::Enter)),
            PickerAction::Stay
        );
    }

    /// `Esc` löscht zuerst einen nicht-leeren Filter, danach erst `Cancel`.
    #[test]
    fn test_esc_clears_filter_then_cancels() {
        let now = SystemTime::now();
        let entries = vec![
            make_entry("a", "Alpha", 10, now),
            make_entry("b", "Beta", 20, now),
        ];
        let mut picker = SessionPicker::new(entries, now);
        picker.handle_key(make_key(KeyCode::Char('a')));
        assert_eq!(picker.filter, "a");

        let first_esc = picker.handle_key(make_key(KeyCode::Esc));
        assert_eq!(first_esc, PickerAction::Stay);
        assert!(picker.filter.is_empty(), "erstes Esc leert den Filter");

        let second_esc = picker.handle_key(make_key(KeyCode::Esc));
        assert_eq!(second_esc, PickerAction::Cancel);
    }

    /// `Ctrl+A` schaltet `show_all` um.
    #[test]
    fn test_ctrl_a_toggles_show_all() {
        let now = SystemTime::now();
        let mut picker = SessionPicker::new(vec![make_entry("a", "Alpha", 10, now)], now);
        assert!(!picker.show_all());
        picker.handle_key(make_key_ctrl(KeyCode::Char('a')));
        assert!(picker.show_all());
        picker.handle_key(make_key_ctrl(KeyCode::Char('a')));
        assert!(!picker.show_all());
    }

    /// Ein einfaches `'a'` ohne Ctrl geht in den Filter statt `show_all` umzuschalten.
    #[test]
    fn test_plain_a_goes_to_filter_not_show_all() {
        let now = SystemTime::now();
        let mut picker = SessionPicker::new(vec![make_entry("a", "Alpha", 10, now)], now);
        picker.handle_key(make_key(KeyCode::Char('a')));
        assert_eq!(picker.filter, "a");
        assert!(!picker.show_all());
    }

    /// Rendering in einen 80x12-Buffer enthält Titeltext und "vor 5 min".
    #[test]
    fn test_render_contains_title_and_relative_time() {
        let now = SystemTime::now();
        let entries = vec![make_entry("s1", "Mein Testtitel", 5 * 60, now)];
        let picker = SessionPicker::new(entries, now);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, &Theme::Dark);

        let rendered = buffer_to_string(&buf);
        assert!(rendered.contains("Mein Testtitel"), "{rendered}");
        assert!(rendered.contains("vor 5 min"), "{rendered}");
    }

    /// Die markierte Zeile enthält den `❯`-Marker.
    #[test]
    fn test_render_selected_row_has_marker() {
        let now = SystemTime::now();
        let entries = vec![
            make_entry("s1", "Erste", 60, now),
            make_entry("s2", "Zweite", 120, now),
        ];
        let mut picker = SessionPicker::new(entries, now);
        picker.handle_key(make_key(KeyCode::Down));

        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, &Theme::Dark);

        let rendered = buffer_to_string(&buf);
        assert!(rendered.contains('❯'), "{rendered}");
    }

    /// Ein sehr langer, multibyte-lastiger Titel wird ohne Panik gekürzt.
    #[test]
    fn test_render_truncates_long_multibyte_title_without_panic() {
        let now = SystemTime::now();
        let long_title = "日本語のとても長いタイトルです。".repeat(10);
        let entries = vec![make_entry("s1", &long_title, 60, now)];
        let picker = SessionPicker::new(entries, now);
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        // Darf nicht paniken (UTF-8-Byte-Grenzen bei Kürzung).
        picker.render(area, &mut buf, &Theme::Dark);
    }

    /// Leerer Zustand zeigt den Hinweistext, wenn kein Eintrag passt.
    #[test]
    fn test_render_empty_state() {
        let now = SystemTime::now();
        let mut picker = SessionPicker::new(vec![make_entry("a", "Alpha", 10, now)], now);
        for c in "kein_treffer_xyz".chars() {
            picker.handle_key(make_key(KeyCode::Char(c)));
        }
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        picker.render(area, &mut buf, &Theme::Dark);
        let rendered = buffer_to_string(&buf);
        assert!(rendered.contains(EMPTY_STATE_TEXT), "{rendered}");
    }

    /// Hilfsfunktion: liest den sichtbaren Textinhalt eines `Buffer` zeilenweise aus.
    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area();
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

//! Filterbares `@`-Erwähnungspopup (Dateien und Agentenrollen).
//!
//! Aufbau und Bedienung folgen bewusst dem `/`-Befehlspopup
//! (`command_popup.rs`): gerankte, case-insensitive Filterung, Pfeiltasten zur
//! Auswahl, `Enter`/`Tab` übernimmt, `Esc` bricht ab, abgeleiteter
//! Scroll-Ausschnitt beim Rendern.
//!
//! # Verantwortung
//! - [`current_mention_query`] findet das `@token` unter bzw. vor dem Cursor.
//! - [`MentionPopup`] filtert [`MentionCandidate`]s und meldet
//!   [`MentionPopupAction`]s.
//!
//! Ziffern werden — anders als im Befehlspopup — nicht als Schnellauswahl
//! gebunden, weil sie in Dateinamen häufig vorkommen; alle nicht behandelten
//! Tasten liefern [`MentionPopupAction::Stay`] und gehören dem Editor.
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::Widget};

use crate::style;

// ---------------------------------------------------------------------------
// Öffentliche Typen
// ---------------------------------------------------------------------------

/// Art eines Erwähnungskandidaten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MentionKind {
    /// Datei im Projekt (`@pfad` hängt sie an).
    File,
    /// Agentenrolle (`@rolle` bittet die UIA um Delegation).
    Role,
}

impl MentionKind {
    /// Deutsche Kurzbezeichnung für die Beschreibungsspalte.
    fn label_de(self) -> &'static str {
        match self {
            Self::File => "Datei",
            Self::Role => "Rolle · Delegationswunsch an die UIA",
        }
    }
}

/// Ein Eintrag im Erwähnungspopup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MentionCandidate {
    /// Angezeigter Text (z. B. relativer Pfad oder Rollenname).
    pub label: String,
    /// Text, der nach `@` eingefügt wird.
    pub insert: String,
    /// Datei oder Rolle.
    pub kind: MentionKind,
}

impl MentionCandidate {
    /// Kandidat für eine Datei; `label` und `insert` sind der relative Pfad.
    pub(crate) fn file(rel_path: impl Into<String>) -> Self {
        let rel_path = rel_path.into();
        Self {
            label: rel_path.clone(),
            insert: rel_path,
            kind: MentionKind::File,
        }
    }

    /// Kandidat für eine Rolle; `label` und `insert` sind der Rollenname.
    pub(crate) fn role(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            label: name.clone(),
            insert: name,
            kind: MentionKind::Role,
        }
    }
}

/// Aktionssignal, das [`MentionPopup::handle_key`] zurückgibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MentionPopupAction {
    /// Popup bleibt offen; die Taste wurde verarbeitet oder gehört dem Editor.
    Stay,
    /// `Esc`: Popup schließen.
    Cancel,
    /// Kandidat gewählt; enthält [`MentionCandidate::insert`] (ohne `@`).
    Accept(String),
}

/// Zustand des `@`-Popups mit Filterlogik und Tastaturnavigation.
#[derive(Debug, Clone)]
pub(crate) struct MentionPopup {
    /// Vollständige Kandidatenliste.
    items: Vec<MentionCandidate>,
    /// Indizes in `items`, nach Rang sortiert.
    filtered: Vec<usize>,
    /// Index in `filtered`.
    selected: usize,
    /// Aktueller Suchtext (ohne `@`).
    query: String,
}

/// Spaltenbreite für das Label (in Zellen).
const LABEL_COLUMN_WIDTH: u16 = 40;

impl MentionPopup {
    /// Erstellt ein Popup; der Filter startet leer (alle Kandidaten sichtbar).
    pub(crate) fn new(candidates: Vec<MentionCandidate>) -> Self {
        let mut popup = Self {
            items: candidates,
            filtered: Vec::new(),
            selected: 0,
            query: String::new(),
        };
        popup.filter("");
        popup
    }

    /// Filtert die Kandidaten nach `query` (case-insensitive, Unicode).
    ///
    /// # Beschreibung
    /// Rang 0: exakter Treffer auf Label oder letzte Pfadkomponente;
    /// Rang 1: Präfix-Treffer auf Label oder letzte Pfadkomponente;
    /// Rang 2: Teilstring-Treffer im Label. Innerhalb eines Rangs stehen
    /// Rollen vor Dateien, dann kürzere vor längeren Labels, dann
    /// alphabetisch. Setzt die Auswahl auf den ersten Treffer.
    pub(crate) fn filter(&mut self, query: &str) {
        self.query = query.to_owned();
        let q = query.to_lowercase();
        let mut ranked: Vec<(usize, u8)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let label = item.label.to_lowercase();
                let base = label.rsplit('/').next().unwrap_or(&label);
                if label == q || base == q {
                    Some((idx, 0))
                } else if label.starts_with(&q) || base.starts_with(&q) {
                    Some((idx, 1))
                } else if label.contains(&q) {
                    Some((idx, 2))
                } else {
                    None
                }
            })
            .collect();
        let items = &self.items;
        ranked.sort_by(|(a, tier_a), (b, tier_b)| {
            let kind_rank = |kind: MentionKind| u8::from(kind == MentionKind::File);
            tier_a
                .cmp(tier_b)
                .then_with(|| kind_rank(items[*a].kind).cmp(&kind_rank(items[*b].kind)))
                .then_with(|| items[*a].label.len().cmp(&items[*b].label.len()))
                .then_with(|| items[*a].label.cmp(&items[*b].label))
        });
        self.filtered = ranked.into_iter().map(|(idx, _)| idx).collect();
        self.selected = 0;
    }

    /// Bewegt die Markierung nach oben.
    pub(crate) fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Bewegt die Markierung nach unten (bleibt im Bereich).
    pub(crate) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    /// Aktuell markierter Kandidat.
    pub(crate) fn selected(&self) -> Option<&MentionCandidate> {
        self.filtered
            .get(self.selected)
            .and_then(|&idx| self.items.get(idx))
    }

    /// `true`, wenn kein Kandidat zum Filter passt.
    pub(crate) fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// Verarbeitet einen Tastendruck.
    ///
    /// `Up`/`Down` bewegen, `Enter`/`Tab` übernehmen, `Esc` bricht ab; alles
    /// andere liefert [`MentionPopupAction::Stay`] (der Aufrufer reicht die
    /// Taste an den Editor weiter und ruft danach [`Self::filter`]).
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> MentionPopupAction {
        match key.code {
            KeyCode::Up => {
                self.move_up();
                MentionPopupAction::Stay
            }
            KeyCode::Down => {
                self.move_down();
                MentionPopupAction::Stay
            }
            KeyCode::Esc => MentionPopupAction::Cancel,
            KeyCode::Enter | KeyCode::Tab => match self.selected() {
                Some(item) => MentionPopupAction::Accept(item.insert.clone()),
                None => MentionPopupAction::Stay,
            },
            _ => MentionPopupAction::Stay,
        }
    }

    /// Sichtbarer Ausschnitt; die Auswahl bleibt immer sichtbar.
    fn visible_range(&self, max_rows: usize) -> std::ops::Range<usize> {
        if max_rows == 0 || self.filtered.is_empty() {
            return 0..0;
        }
        let end = self.filtered.len().min(self.selected.saturating_add(1));
        let start = end.saturating_sub(max_rows);
        start..(start + max_rows).min(self.filtered.len())
    }

    /// Zeichnet einen scrollbaren Ausschnitt der Trefferliste
    /// (`@label` links, Art rechts), analog zu `CommandPopup::render`.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        if self.filtered.is_empty() {
            if area.height > 0 {
                let line = Line::styled("Keine Treffer", style::dim_style(theme));
                Widget::render(line, Rect::new(area.left(), area.top(), area.width, 1), buf);
            }
            return;
        }
        let visible = self.visible_range(area.height as usize);
        let visible_start = visible.start;
        let label_width = area.width.min(LABEL_COLUMN_WIDTH);
        for filtered_idx in visible {
            let item = &self.items[self.filtered[filtered_idx]];
            let y = area.top() + (filtered_idx - visible_start) as u16;
            let label_style = if filtered_idx == self.selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let label = format!("@{}", item.label);
            Widget::render(
                Line::styled(label, label_style),
                Rect::new(area.left(), y, label_width, 1),
                buf,
            );
            let desc_x = area.left().saturating_add(LABEL_COLUMN_WIDTH);
            if desc_x < area.right() {
                let desc_area = Rect::new(desc_x, y, area.right() - desc_x, 1);
                Widget::render(
                    Line::styled(item.kind.label_de(), style::dim_style(theme)),
                    desc_area,
                    buf,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cursor-Analyse
// ---------------------------------------------------------------------------

/// Findet das `@token` unter bzw. unmittelbar vor dem Cursor.
///
/// # Parameter
/// - `input`: gesamter Eingabepuffer.
/// - `cursor`: Byte-Offset des Cursors (wie `InputEditor::cursor`); ein
///   Offset außerhalb oder mitten in einem Zeichen wird auf die vorherige
///   Zeichengrenze geklemmt.
///
/// # Rückgabe
/// `Some((start, query))`: `start` ist der Byte-Offset des `@`, `query` das
/// *gesamte* Token nach dem `@` bis zum nächsten Leerraum (auch hinter dem
/// Cursor). Der Aufrufer ersetzt also `start..start + 1 + query.len()`.
/// `None`, wenn das Wort am Cursor nicht mit `@` beginnt (z. B. `a@b`).
pub(crate) fn current_mention_query(input: &str, cursor: usize) -> Option<(usize, &str)> {
    let mut cursor = cursor.min(input.len());
    while !input.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = input[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(idx, c)| idx + c.len_utf8());
    let end = input[cursor..]
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map_or(input.len(), |(idx, _)| cursor + idx);
    let token = &input[start..end];
    let query = token.strip_prefix('@')?;
    if cursor == start {
        // Cursor steht *vor* dem `@` — noch nicht im Token.
        return None;
    }
    Some((start, query))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample() -> MentionPopup {
        MentionPopup::new(vec![
            MentionCandidate::file("src/research.rs"),
            MentionCandidate::file("README.md"),
            MentionCandidate::file("docs/über.md"),
            MentionCandidate::role("research"),
            MentionCandidate::role("explorer"),
        ])
    }

    fn labels(popup: &MentionPopup) -> Vec<&str> {
        popup
            .filtered
            .iter()
            .map(|&idx| popup.items[idx].label.as_str())
            .collect()
    }

    #[test]
    fn empty_query_shows_all_roles_first() {
        let popup = sample();
        assert_eq!(popup.filtered.len(), 5);
        assert_eq!(
            labels(&popup),
            vec![
                "explorer",
                "research",
                "README.md",
                "docs/über.md",
                "src/research.rs"
            ]
        );
    }

    #[test]
    fn filter_ranks_exact_then_prefix_then_substring() {
        let mut popup = sample();
        popup.filter("RESEARCH");
        // Exakt: Rolle „research“; Präfix auf Basisname: src/research.rs.
        assert_eq!(labels(&popup), vec!["research", "src/research.rs"]);
        popup.filter("md");
        assert_eq!(labels(&popup), vec!["README.md", "docs/über.md"]);
        popup.filter("ÜBER");
        assert_eq!(labels(&popup), vec!["docs/über.md"]);
        popup.filter("gibtsnicht");
        assert!(popup.is_empty());
        assert_eq!(popup.selected(), None);
    }

    #[test]
    fn keys_navigate_accept_and_cancel() -> TestResult {
        let mut popup = sample();
        assert_eq!(popup.handle_key(key(KeyCode::Up)), MentionPopupAction::Stay);
        for _ in 0..10 {
            popup.handle_key(key(KeyCode::Down));
        }
        let last = popup
            .selected()
            .ok_or(TestError::Missing("selected"))?
            .insert
            .clone();
        assert_eq!(last, "src/research.rs");
        assert_eq!(
            popup.handle_key(key(KeyCode::Tab)),
            MentionPopupAction::Accept(last)
        );
        popup.filter("expl");
        assert_eq!(
            popup.handle_key(key(KeyCode::Enter)),
            MentionPopupAction::Accept("explorer".to_owned())
        );
        assert_eq!(
            popup.handle_key(key(KeyCode::Char('1'))),
            MentionPopupAction::Stay
        );
        assert_eq!(
            popup.handle_key(key(KeyCode::Esc)),
            MentionPopupAction::Cancel
        );
        popup.filter("nix");
        assert_eq!(
            popup.handle_key(key(KeyCode::Enter)),
            MentionPopupAction::Stay
        );
        Ok(())
    }

    #[test]
    fn render_scrolls_with_selection() {
        let mut popup = sample();
        for _ in 0..4 {
            popup.move_down();
        }
        assert_eq!(popup.visible_range(2), 3..5);
        let area = Rect::new(0, 0, 60, 2);
        let mut buf = Buffer::empty(area);
        popup.render(area, &mut buf, style::Theme::Dark);
        let row: String = (0..area.width)
            .map(|x| buf[(x, 1)].symbol().to_owned())
            .collect();
        assert!(row.starts_with("@src/research.rs"), "{row}");
        assert!(row.contains("Datei"), "{row}");
    }

    #[test]
    fn query_at_cursor_basic() {
        assert_eq!(current_mention_query("@", 1), Some((0, "")));
        assert_eq!(
            current_mention_query("hallo @src/li", 13),
            Some((6, "src/li"))
        );
        // Cursor mitten im Token liefert das ganze Token.
        assert_eq!(current_mention_query("x @abc y", 4), Some((2, "abc")));
        assert_eq!(current_mention_query("x @abc y", 8), None);
        assert_eq!(current_mention_query("mail a@b", 8), None);
        assert_eq!(current_mention_query("x @abc", 2), None);
        assert_eq!(current_mention_query("", 0), None);
    }

    #[test]
    fn query_at_cursor_is_utf8_safe() {
        let input = "grüße @dätei.rs";
        let at = input.find('@').unwrap_or(usize::MAX);
        assert_eq!(
            current_mention_query(input, input.len()),
            Some((at, "dätei.rs"))
        );
        // Cursor mitten im „ä“ (2 Byte) wird geklemmt, kein Panic.
        assert_eq!(current_mention_query(input, at + 3), Some((at, "dätei.rs")));
        // Cursor hinter dem Pufferende wird geklemmt.
        assert_eq!(current_mention_query(input, 999), Some((at, "dätei.rs")));
        // Nicht-ASCII-Leerraum trennt ebenfalls.
        assert_eq!(current_mention_query("a\u{3000}@ü", 6), Some((4, "ü")));
    }
}

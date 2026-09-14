//! Filterbares Befehlspopup für die TUI-Kommandoauswahl.
//!
//! Dieses Modul implementiert das `/command`-Popup analog zum `ListSelectionView`
//! aus der Codex-TUI-Studie (Spec-Abschnitt 2.8).
//!
//! # Verantwortung
//! - Befüllt `CommandItem`-Einträge aus einer `CommandRegistry` über `find`.
//! - Filtert die Liste live nach Tipp-Eingaben (case-insensitive Substring-Suche).
//! - Gibt `PopupAction`-Events an den Aufrufer zurück (Stay / Cancel / Accept).
//!
//! # Schlüsseltypen
//! - [`CommandItem`]: Name + Beschreibung eines Befehls.
//! - [`CommandPopup`]: Widget-Zustand (Items, Filterindex, Auswahl).
//! - [`PopupAction`]: Ereignis das `on_key` zurückgibt.
//!
//! # Nebenläufigkeit
//! Kein interner Zustand wird geteilt; der Aufrufer hält `CommandPopup` exklusiv.
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::Widget};

use crate::CommandRegistry;
use crate::style;

// ---------------------------------------------------------------------------
// Interne Hilfsfunktion
// ---------------------------------------------------------------------------

/// Leitet eine kurze menschenlesbare Beschreibung aus den Debug-Repräsentationen
/// der Felder `domain` und `permission` eines `CommandSpec` ab.
fn make_description(domain_str: &str, permission_str: &str) -> String {
    let domain_label = match domain_str {
        "SessionLifecycle" => "Sitzung",
        "AgentTopology" => "Agenten",
        "WorkGovernance" => "Aufgaben",
        "Execution" => "Ausführung",
        "CatalogConfig" => "Katalog",
        "Knowledge" => "Wissen",
        "Channels" => "Kanäle",
        "Misc" => "Allgemein",
        other => other,
    };
    let perm_label = match permission_str {
        "Observer" => "Beobachter",
        "Operator" => "Operator",
        "Maintainer" => "Maintainer",
        "Owner" => "Eigentümer",
        other => other,
    };
    format!("{domain_label} · Berechtigung: {perm_label}")
}

// ---------------------------------------------------------------------------
// Öffentliche Typen
// ---------------------------------------------------------------------------

/// Ein einzelner Befehlseintrag im Popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandItem {
    /// Kanonischer Befehlsname ohne führendes `/`.
    pub name: String,
    /// Menschenlesbare Kurzbeschreibung für die Anzeige im Popup.
    pub description: String,
}

/// Aktionssignal das `CommandPopup::on_key` zurückgibt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PopupAction {
    /// Popup bleibt geöffnet, keine weitere Aktion erforderlich.
    Stay,
    /// Nutzer hat Esc gedrückt; Popup soll geschlossen werden.
    Cancel,
    /// Nutzer hat einen Befehl gewählt; enthält den kanonischen Namen.
    Accept(String),
}

/// Zustand des `/command`-Popups mit Filterlogik und Tastaturnavigation.
#[derive(Debug, Clone)]
pub(crate) struct CommandPopup {
    /// Vollständige, unveränderliche Liste aller Befehle.
    items: Vec<CommandItem>,
    /// Index in `filtered` — welcher gefilterte Eintrag aktuell markiert ist.
    selected: usize,
    /// Aktueller Suchtext (ohne führendes `/`).
    query: String,
    /// Indizes in `items`, die dem aktuellen `query` entsprechen.
    filtered: Vec<usize>,
}

// ---------------------------------------------------------------------------
// Anzeige-Konstanten
// ---------------------------------------------------------------------------

/// Spaltenbreite (in Zeichen) für den Befehlsnamen im Popup.
const NAME_COLUMN_WIDTH: u16 = 22;

impl CommandPopup {
    /// Erstellt ein neues `CommandPopup` aus einer `CommandRegistry`.
    pub(crate) fn new(registry: &CommandRegistry) -> Self {
        // Die Runtime-Registry ist die einzige Command-Quelle. Neue
        // `#[operation(command(...))]`-Definitionen erscheinen dadurch ohne
        // parallele Whitelist automatisch im Popup.
        let items: Vec<CommandItem> = registry
            .specs()
            .iter()
            .map(|spec| {
                let domain_str = format!("{:?}", spec.domain);
                let permission_str = format!("{:?}", spec.permission);
                CommandItem {
                    name: spec.name.as_str().to_owned(),
                    description: make_description(&domain_str, &permission_str),
                }
            })
            .collect();

        let len = items.len();
        Self {
            items,
            selected: 0,
            query: String::new(),
            filtered: (0..len).collect(),
        }
    }

    /// Aktualisiert den Suchtext und berechnet die gefilterte Liste neu.
    pub(crate) fn on_query_change(&mut self, query: &str) {
        self.query = query.to_owned();
        let q_lower = query.to_ascii_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.name.to_ascii_lowercase().contains(&q_lower))
            .map(|(idx, _)| idx)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    /// Bewegt die Markierung um einen Schritt nach oben.
    pub(crate) fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Bewegt die Markierung um einen Schritt nach unten.
    pub(crate) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    /// Gibt den Namen des aktuell markierten gefilterten Items zurück.
    pub(crate) fn selected_name(&self) -> Option<&str> {
        let &item_idx = self.filtered.get(self.selected)?;
        Some(&self.items[item_idx].name)
    }

    /// Gibt `true` zurück wenn keine Items in der gefilterten Liste vorhanden sind.
    pub(crate) fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// Bestimmt den sichtbaren Ausschnitt der Trefferliste.
    ///
    /// Die Auswahl bleibt immer sichtbar. Der Scroll-Offset ist absichtlich
    /// abgeleitet statt als Zustand gespeichert: Die verfügbare Höhe kennt nur
    /// der Renderer und kann sich bei einem Terminal-Resize jederzeit ändern.
    fn visible_range(&self, max_rows: usize) -> std::ops::Range<usize> {
        if max_rows == 0 || self.filtered.is_empty() {
            return 0..0;
        }
        let end = self.filtered.len().min(self.selected.saturating_add(1));
        let start = end.saturating_sub(max_rows);
        start..(start + max_rows).min(self.filtered.len())
    }

    /// Verarbeitet einen Tastendruck und gibt eine `PopupAction` zurück.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> PopupAction {
        match key.code {
            KeyCode::Up => {
                self.move_up();
                PopupAction::Stay
            }
            KeyCode::Down => {
                self.move_down();
                PopupAction::Stay
            }
            KeyCode::Esc => PopupAction::Cancel,
            KeyCode::Enter => match self.selected_name() {
                Some(name) => PopupAction::Accept(name.to_owned()),
                None => PopupAction::Stay,
            },
            KeyCode::Char(c @ '1'..='9') => {
                let zero_based = (c as usize) - ('1' as usize);
                if zero_based < self.filtered.len() {
                    let item_idx = self.filtered[zero_based];
                    PopupAction::Accept(self.items[item_idx].name.clone())
                } else {
                    PopupAction::Stay
                }
            }
            _ => PopupAction::Stay,
        }
    }

    /// Zeichnet einen scrollbaren Ausschnitt der gefilterten Befehlsliste.
    ///
    /// Der übergebene Bereich begrenzt nur die sichtbaren Zeilen; bei `Up` und
    /// `Down` folgt der Ausschnitt der Auswahl. Damit bleiben auch Befehle nach
    /// der achten Zeile in kleinen Terminals erreichbar und sichtbar.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        let visible = self.visible_range(area.height as usize);
        // `Range` wird vom `for`-Loop konsumiert. Die Startposition brauchen wir
        // daneben für die Zeile innerhalb des sichtbaren Fensters.
        let visible_start = visible.start;

        for filtered_idx in visible {
            let row_idx = filtered_idx - visible_start;
            let item_idx = self.filtered[filtered_idx];
            let y = area.top() + row_idx as u16;
            let is_selected = filtered_idx == self.selected;
            let item = &self.items[item_idx];

            let name_style = if is_selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let desc_style = style::dim_style(theme);

            // Die Ordnungszahl bleibt der Index in der vollständigen gefilterten
            // Liste, damit beim Scrollen klar ist, dass weitere Treffer folgen.
            let label = format!("{:>2}. /{}", filtered_idx + 1, item.name);
            let name_width = area.width.min(NAME_COLUMN_WIDTH);
            let name_area = Rect::new(area.left(), y, name_width, 1);
            Widget::render(Line::styled(label, name_style), name_area, buf);

            let desc_x = area.left().saturating_add(NAME_COLUMN_WIDTH);
            if desc_x < area.right() {
                let desc_width = area.right().saturating_sub(desc_x);
                let desc_area = Rect::new(desc_x, y, desc_width, 1);
                Widget::render(Line::styled(item.description.clone(), desc_style), desc_area, buf);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::CommandRegistry;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn built_in_popup() -> CommandPopup {
        CommandPopup::new(&CommandRegistry::built_in())
    }

    #[test]
    fn popup_contains_every_registry_spec() {
        let registry = CommandRegistry::built_in();
        let popup = CommandPopup::new(&registry);
        assert_eq!(popup.items.len(), registry.specs().len());
        for spec in registry.specs() {
            assert!(popup.items.iter().any(|item| item.name == spec.name.as_str()));
        }
    }

    #[test]
    fn test_filter_narrows_list() {
        let mut popup = built_in_popup();
        let initial_count = popup.filtered.len();
        assert!(initial_count > 1);
        popup.on_query_change("hel");
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.selected_name(), Some("help"));
    }

    #[test]
    fn test_enter_returns_accept() {
        let mut popup = built_in_popup();
        let expected = popup.selected_name().unwrap().to_owned();
        assert_eq!(popup.on_key(make_key(KeyCode::Enter)), PopupAction::Accept(expected));
    }

    #[test]
    fn test_esc_returns_cancel() {
        let mut popup = built_in_popup();
        assert_eq!(popup.on_key(make_key(KeyCode::Esc)), PopupAction::Cancel);
    }

    #[test]
    fn test_digit_selects_nth_item() {
        let mut popup = built_in_popup();
        assert!(popup.filtered.len() >= 2);
        let second_name = popup.items[popup.filtered[1]].name.clone();
        assert_eq!(popup.on_key(make_key(KeyCode::Char('2'))), PopupAction::Accept(second_name));
    }

    #[test]
    fn test_navigation_stays_in_bounds() {
        let mut popup = built_in_popup();
        let len = popup.filtered.len();
        popup.move_up();
        assert_eq!(popup.selected, 0);
        for _ in 0..len + 5 {
            popup.move_down();
        }
        assert_eq!(popup.selected, len - 1);
        assert!(popup.selected_name().is_some());
    }

    #[test]
    fn test_is_empty_after_no_match_query() {
        let mut popup = built_in_popup();
        popup.on_query_change("xyzzy_existiert_nicht_12345");
        assert!(popup.is_empty());
        assert_eq!(popup.selected_name(), None);
    }

    #[test]
    fn test_digit_out_of_range_returns_stay() {
        let mut popup = built_in_popup();
        popup.on_query_change("help");
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.on_key(make_key(KeyCode::Char('2'))), PopupAction::Stay);
    }

    #[test]
    fn test_empty_query_restores_all_items() {
        let mut popup = built_in_popup();
        let initial_count = popup.filtered.len();
        popup.on_query_change("help");
        popup.on_query_change("");
        assert_eq!(popup.filtered.len(), initial_count);
    }

    #[test]
    fn test_selected_clamped_after_filter_shrinks() {
        let mut popup = built_in_popup();
        for _ in 0..20 {
            popup.move_down();
        }
        assert!(popup.selected > 0);
        popup.on_query_change("help");
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn visible_range_scrolls_when_selection_passes_available_rows() {
        let mut popup = built_in_popup();
        assert!(popup.filtered.len() > 8, "test requires more than eight commands");
        for _ in 0..8 {
            popup.move_down();
        }

        assert_eq!(popup.visible_range(8), 1..9);
        assert!(popup.visible_range(8).contains(&popup.selected));
    }
}

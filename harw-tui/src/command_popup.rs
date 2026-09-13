//! Filterbares Befehlspopup für die TUI-Kommandoauswahl.
//!
//! Dieses Modul implementiert das `/command`-Popup analog zum `ListSelectionView`
//! aus der Codex-TUI-Studie (Spec-Abschnitt 2.8 / SLICE 5).
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
///
/// # Beschreibung
/// Da `CommandSpec` kein Freitext-Beschreibungsfeld besitzt, wird die Beschreibung
/// aus den strukturierten Metadaten abgeleitet. (Spec-Abschnitt 2.8)
///
/// # Argumente
/// - `domain_str` (`&str`): Debug-Ausgabe des `CommandDomain`-Wertes.
/// - `permission_str` (`&str`): Debug-Ausgabe des `PermissionTier`-Wertes.
///
/// # Rückgabe
/// Kurze, menschenlesbare Beschreibung als `String`.
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
///
/// # Beschreibung
/// Hält den kanonischen Befehlsnamen (ohne führendes `/`) und eine kurze
/// Beschreibung für die Anzeige im gefilterten Popup. (Spec-Abschnitt 2.8)
///
/// # Felder
/// - `name` (`String`): Kanonischer Befehlsname, z. B. `"help"`.
/// - `description` (`String`): Menschenlesbare Kurzbeschreibung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandItem {
    /// Kanonischer Befehlsname ohne führendes `/`.
    pub name: String,
    /// Menschenlesbare Kurzbeschreibung für die Anzeige im Popup.
    pub description: String,
}

/// Aktionssignal das `CommandPopup::on_key` zurückgibt.
///
/// # Beschreibung
/// Teilt dem Aufrufer mit, was mit dem Popup geschehen soll: offen lassen,
/// schließen (Abbruch) oder einen Befehlsnamen akzeptieren. (Spec-Abschnitt 2.8)
///
/// # Varianten
/// - `Stay`: Popup bleibt offen; keine Aktion.
/// - `Cancel`: Nutzer hat Esc gedrückt; Popup schließen.
/// - `Accept(String)`: Nutzer hat einen Befehl ausgewählt; enthält den Namen.
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
///
/// # Beschreibung
/// Verwaltet die vollständige Liste aller Befehle sowie den aktuellen
/// Filterzustand. Das Widget zeichnet sich selbst direkt in einen ratatui-`Buffer`.
/// Analogon zu `ListSelectionView` aus der Codex-TUI (Spec-Abschnitt 2.8).
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; exklusiver `&mut`-Zugriff des Aufrufers erwartet.
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
// Bekannte kanonische Befehlsnamen des built-in Registry
// ---------------------------------------------------------------------------

/// Kanonische Befehlsnamen, die im `built_in`-Registry vorhanden sind.
///
/// Da `CommandRegistry` kein öffentliches Iterationsinterface besitzt,
/// werden Items per `registry.find(name)` abgerufen. Unbekannte Namen werden
/// stillschweigend übersprungen. (Spec-Abschnitt 2.8)
const KNOWN_COMMAND_NAMES: &[&str] = &[
    "help", "status", "new", "work", "ps", "attach", "skills", "plugins",
];

/// Spaltenbreite (in Zeichen) für den Befehlsnamen im Popup.
const NAME_COLUMN_WIDTH: u16 = 22;

impl CommandPopup {
    /// Erstellt ein neues `CommandPopup` aus einer `CommandRegistry`.
    ///
    /// # Beschreibung
    /// Befüllt `items` durch Aufruf von `registry.find(name)` für jeden bekannten
    /// Befehlsnamen. Nicht gefundene Namen werden übersprungen. `filtered` wird
    /// initial auf alle Indizes gesetzt. (Spec-Abschnitt 2.8)
    ///
    /// # Argumente
    /// - `registry` (`&CommandRegistry`): Referenz auf das Command-Verzeichnis.
    ///
    /// # Rückgabe
    /// Initialisiertes `CommandPopup` mit vollständiger, ungefilterter Liste.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let popup = CommandPopup::new(&registry);
    /// assert!(!popup.is_empty());
    /// ```
    pub(crate) fn new(registry: &CommandRegistry) -> Self {
        let items: Vec<CommandItem> = KNOWN_COMMAND_NAMES
            .iter()
            .filter_map(|&name| {
                let spec = registry.find(name)?;
                let domain_str = format!("{:?}", spec.domain);
                let permission_str = format!("{:?}", spec.permission);
                Some(CommandItem {
                    name: spec.name.as_str().to_owned(),
                    description: make_description(&domain_str, &permission_str),
                })
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
    ///
    /// # Beschreibung
    /// Setzt `query` auf den übergebenen Text (ohne führendes `/`) und berechnet
    /// `filtered` als Indizes aller Items, deren `name` den Suchtext als
    /// case-insensitiven Substring enthält. Der `selected`-Index wird auf
    /// `0` geklemmt, wenn er außerhalb der neuen Liste läge. (Spec-Abschnitt 2.8)
    ///
    /// # Argumente
    /// - `query` (`&str`): Suchtext ohne führendes `/`; leer = alle anzeigen.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let mut popup = CommandPopup::new(&registry);
    /// popup.on_query_change("hel");
    /// assert_eq!(popup.selected_name(), Some("help"));
    /// ```
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
    ///
    /// # Beschreibung
    /// Verringert `selected` um 1, mindestens 0. Wenn `filtered` leer ist,
    /// passiert nichts. (Spec-Abschnitt 2.8)
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let mut popup = CommandPopup::new(&registry);
    /// popup.move_down();
    /// popup.move_up(); // zurück zu 0
    /// ```
    pub(crate) fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Bewegt die Markierung um einen Schritt nach unten.
    ///
    /// # Beschreibung
    /// Erhöht `selected` um 1 bis maximal `filtered.len() - 1`.
    /// Wenn `filtered` leer ist, passiert nichts. (Spec-Abschnitt 2.8)
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let mut popup = CommandPopup::new(&registry);
    /// popup.move_down();
    /// ```
    pub(crate) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }

    /// Gibt den Namen des aktuell markierten gefilterten Items zurück.
    ///
    /// # Beschreibung
    /// Gibt `None` zurück wenn `filtered` leer ist, sonst den `name`-Slice
    /// des selektierten Items. (Spec-Abschnitt 2.8)
    ///
    /// # Rückgabe
    /// `Some(&str)` mit dem kanonischen Befehlsnamen oder `None` bei leerer Liste.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let popup = CommandPopup::new(&registry);
    /// assert!(popup.selected_name().is_some());
    /// ```
    pub(crate) fn selected_name(&self) -> Option<&str> {
        let &item_idx = self.filtered.get(self.selected)?;
        Some(&self.items[item_idx].name)
    }

    /// Gibt `true` zurück wenn keine Items in der gefilterten Liste vorhanden sind.
    ///
    /// # Beschreibung
    /// Prüft ob `filtered` leer ist — d. h. kein Befehl dem aktuellen Suchtext
    /// entspricht. (Spec-Abschnitt 2.8)
    ///
    /// # Rückgabe
    /// `true` wenn `filtered.is_empty()`, sonst `false`.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let mut popup = CommandPopup::new(&registry);
    /// popup.on_query_change("xyzzy_nicht_vorhanden");
    /// assert!(popup.is_empty());
    /// ```
    pub(crate) fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// Verarbeitet einen Tastendruck und gibt eine `PopupAction` zurück.
    ///
    /// # Beschreibung
    /// Steuert Navigation, Abbruch und Auswahl im Popup. (Spec-Abschnitt 2.8)
    ///
    /// # Argumente
    /// - `key` (`KeyEvent`): Das eingegangene Crossterm-Tastenereignis.
    ///
    /// # Rückgabe
    /// - `PopupAction::Stay` bei `Up`/`Down` oder unbekannter Taste.
    /// - `PopupAction::Cancel` bei `Esc`.
    /// - `PopupAction::Accept(name)` bei `Enter` (falls Liste nicht leer).
    /// - `PopupAction::Accept(name)` bei `Char('1'..='9')` falls Index vorhanden.
    /// - `PopupAction::Stay` bei `Enter` wenn Liste leer oder Ziffer außer Bereich.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    /// # use harw_tui::{CommandRegistry, command_popup::{CommandPopup, PopupAction}};
    /// let registry = CommandRegistry::built_in();
    /// let mut popup = CommandPopup::new(&registry);
    /// let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    /// assert_eq!(popup.on_key(key), PopupAction::Cancel);
    /// ```
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

    /// Zeichnet die gefilterte Befehlsliste in den angegebenen `Buffer`-Bereich.
    ///
    /// # Beschreibung
    /// Rendert jeden sichtbaren gefilterten Eintrag als Zeile in `area`. Das
    /// markierte Item erscheint via [`style::selected_style`] (theme-abhängige
    /// Akzentfarbe + fett); Beschreibungen werden via [`style::dim_style`]
    /// (theme-abhängig gedimmt) rechts neben dem Namen gezeigt. Über
    /// `area.height` hinausgehende Einträge werden abgeschnitten. (Spec-Abschnitt 2.8)
    ///
    /// # Argumente
    /// - `area` (`Rect`): Der Zeichenbereich im Terminal-Buffer.
    /// - `buf` (`&mut Buffer`): Der ratatui-Buffer in den geschrieben wird.
    /// - `theme` (`style::Theme`): aktives Farbschema für Auswahl- und Dim-Stile.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; kein Locking erforderlich.
    ///
    /// # Beispiele
    /// ```ignore
    /// # use ratatui::{buffer::Buffer, layout::Rect};
    /// # use harw_tui::{CommandRegistry, command_popup::CommandPopup};
    /// let registry = CommandRegistry::built_in();
    /// let popup = CommandPopup::new(&registry);
    /// let area = Rect::new(0, 0, 60, 10);
    /// let mut buf = Buffer::empty(area);
    /// popup.render(area, &mut buf, harw_tui::style::Theme::Dark);
    /// ```
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        let max_rows = area.height as usize;

        for (row_idx, &item_idx) in self.filtered.iter().enumerate().take(max_rows) {
            let y = area.top() + row_idx as u16;
            let is_selected = row_idx == self.selected;
            let item = &self.items[item_idx];

            let name_style = if is_selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let desc_style = style::dim_style(theme);

            // Spalte 1: Zeilennummer + Befehlsname
            let label = format!("{:>2}. /{}", row_idx + 1, item.name);
            let name_width = area.width.min(NAME_COLUMN_WIDTH);
            let name_area = Rect::new(area.left(), y, name_width, 1);
            Widget::render(Line::styled(label, name_style), name_area, buf);

            // Spalte 2: Beschreibung (wenn Platz vorhanden)
            let desc_x = area.left().saturating_add(NAME_COLUMN_WIDTH);
            if desc_x < area.right() {
                let desc_width = area.right().saturating_sub(desc_x);
                let desc_area = Rect::new(desc_x, y, desc_width, 1);
                Widget::render(
                    Line::styled(item.description.clone(), desc_style),
                    desc_area,
                    buf,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    /// Filter engt die Liste auf passende Einträge ein.
    ///
    /// Nach `on_query_change("hel")` darf nur `help` in `filtered` verbleiben.
    #[test]
    fn test_filter_narrows_list() {
        let mut popup = built_in_popup();
        let initial_count = popup.filtered.len();
        assert!(initial_count > 1, "built_in muss mehrere Befehle liefern");

        popup.on_query_change("hel");
        assert_eq!(popup.filtered.len(), 1, "nur 'help' soll passen");
        assert_eq!(popup.selected_name(), Some("help"));
    }

    /// `Enter` liefert `PopupAction::Accept(name)` mit dem markierten Befehlsnamen.
    #[test]
    fn test_enter_returns_accept() {
        let mut popup = built_in_popup();
        popup.selected = 0;
        let expected = popup.selected_name().unwrap().to_owned();

        let action = popup.on_key(make_key(KeyCode::Enter));
        assert_eq!(action, PopupAction::Accept(expected));
    }

    /// `Esc` liefert `PopupAction::Cancel`.
    #[test]
    fn test_esc_returns_cancel() {
        let mut popup = built_in_popup();
        assert_eq!(popup.on_key(make_key(KeyCode::Esc)), PopupAction::Cancel);
    }

    /// Zifferntaste `'2'` wählt direkt das zweite gefilterte Item.
    #[test]
    fn test_digit_selects_nth_item() {
        let mut popup = built_in_popup();
        assert!(
            popup.filtered.len() >= 2,
            "built_in braucht ≥2 Einträge für diesen Test"
        );

        let second_name = {
            let idx = popup.filtered[1];
            popup.items[idx].name.clone()
        };

        assert_eq!(
            popup.on_key(make_key(KeyCode::Char('2'))),
            PopupAction::Accept(second_name)
        );
    }

    /// Navigation mit `Up`/`Down` bleibt innerhalb der Grenzen.
    #[test]
    fn test_navigation_stays_in_bounds() {
        let mut popup = built_in_popup();
        let len = popup.filtered.len();

        // move_up an Position 0 bleibt bei 0
        popup.move_up();
        assert_eq!(popup.selected, 0);

        // move_down weit über das Ende hinaus
        for _ in 0..len + 5 {
            popup.move_down();
        }
        assert_eq!(popup.selected, len - 1);
        assert!(popup.selected_name().is_some());
    }

    /// Nach einem Query ohne Treffer ist `is_empty()` wahr und `selected_name()` `None`.
    #[test]
    fn test_is_empty_after_no_match_query() {
        let mut popup = built_in_popup();
        popup.on_query_change("xyzzy_existiert_nicht_12345");
        assert!(popup.is_empty());
        assert_eq!(popup.selected_name(), None);
    }

    /// Zifferntaste außerhalb der gefilterten Liste liefert `Stay`.
    #[test]
    fn test_digit_out_of_range_returns_stay() {
        let mut popup = built_in_popup();
        popup.on_query_change("help"); // exakt 1 Treffer
        assert_eq!(popup.filtered.len(), 1);

        assert_eq!(
            popup.on_key(make_key(KeyCode::Char('2'))),
            PopupAction::Stay
        );
    }

    /// Leerer Query nach gefiltertem Query stellt alle Items wieder her.
    #[test]
    fn test_empty_query_restores_all_items() {
        let mut popup = built_in_popup();
        let initial_count = popup.filtered.len();

        popup.on_query_change("help");
        assert!(popup.filtered.len() < initial_count);

        popup.on_query_change("");
        assert_eq!(popup.filtered.len(), initial_count);
    }

    /// `selected` wird nach Filteränderung auf 0 geklemmt wenn außer Bereich.
    #[test]
    fn test_selected_clamped_after_filter_shrinks() {
        let mut popup = built_in_popup();
        // Navigiere ans Ende der vollen Liste
        for _ in 0..20 {
            popup.move_down();
        }
        assert!(popup.selected > 0);

        // Filter auf 1 Eintrag → selected muss 0 werden
        popup.on_query_change("help");
        assert_eq!(popup.selected, 0);
    }
}

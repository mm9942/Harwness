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

/// Längstes gemeinsames Präfix aller `strings` (case-sensitiv, auf
/// Zeichenbasis — Befehlsnamen sind ASCII, daher entspricht die
/// Zeichenanzahl der Byte-Länge).
///
/// # Rückgabe
/// `String::new()` für eine leere Eingabe.
fn longest_common_prefix(strings: &[&str]) -> String {
    let Some(first) = strings.first() else {
        return String::new();
    };
    let mut prefix_len = first.chars().count();
    for other in &strings[1..] {
        let shared = first
            .chars()
            .zip(other.chars())
            .take_while(|(a, b)| a == b)
            .count();
        prefix_len = prefix_len.min(shared);
    }
    first.chars().take(prefix_len).collect()
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

/// Ergebnis von [`CommandPopup::tab_outcome`] — shell-artige
/// Tab-Vervollständigung statt bloßer Übernahme der Markierung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TabOutcome {
    /// Keine Treffer; Tab tut nichts (kein Tab-Zeichen einfügen).
    None,
    /// Der getippte Suchtext wird auf das längste gemeinsame Präfix aller
    /// Prefix-Treffer erweitert; das Popup bleibt offen.
    ExtendQuery(String),
    /// Ein einzelner Befehl wird übernommen (Popup schließt).
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
    /// Indizes in `items`, sortiert nach Rang: exakter Treffer > Präfix-Treffer
    /// > Teilstring-Treffer; innerhalb eines Rangs nach Namenslänge, dann
    /// > alphabetisch (stabil, deterministisch — Registrierungsreihenfolge
    /// > entscheidet nie über die Anzeigereihenfolge).
    filtered: Vec<usize>,
    /// Anzahl der führenden Einträge in `filtered`, die exakte oder
    /// Präfix-Treffer sind (Rang 0/1). Da `filtered` nach Rang sortiert ist,
    /// bilden sie stets einen zusammenhängenden Anfangsabschnitt.
    prefix_count: usize,
    /// `true`, seit der letzten `on_query_change`, wenn der Nutzer die
    /// Markierung per Pfeiltaste/Ziffer bewegt hat. Steuert
    /// [`Self::tab_outcome`]: eine bewusst bewegte Markierung hat Vorrang vor
    /// der automatischen Rang-/Präfix-Logik.
    selection_moved: bool,
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

        let mut popup = Self {
            items,
            selected: 0,
            query: String::new(),
            filtered: Vec::new(),
            prefix_count: 0,
            selection_moved: false,
        };
        // Baut `filtered`/`prefix_count` über dieselbe Rang-Logik wie jede
        // spätere Eingabe auf, statt Registrierungsreihenfolge zu
        // übernehmen — ein leerer Suchtext ist für `starts_with`/`contains`
        // ohnehin bei jedem Namen wahr, ergibt also alle Einträge im
        // Rang „Präfix-Treffer".
        popup.on_query_change("");
        popup
    }

    /// Aktualisiert den Suchtext und berechnet die gerankte, gefilterte
    /// Liste neu.
    ///
    /// # Beschreibung
    /// Case-insensitive Drei-Rang-Klassifikation je Eintrag: exakter Treffer
    /// (Rang 0) > Präfix-Treffer (Rang 1) > Teilstring-Treffer (Rang 2, nur
    /// wenn `query` irgendwo im Namen vorkommt). Einträge ohne Treffer
    /// entfallen. Innerhalb eines Rangs sortiert stabil nach Namenslänge,
    /// dann alphabetisch — deterministisch unabhängig von der
    /// Registrierungsreihenfolge. Setzt `selected` auf `0` und
    /// `selection_moved` auf `false`.
    pub(crate) fn on_query_change(&mut self, query: &str) {
        self.query = query.to_owned();
        self.selection_moved = false;
        let q_lower = query.to_ascii_lowercase();

        let mut ranked: Vec<(usize, u8)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let name_lower = item.name.to_ascii_lowercase();
                if name_lower == q_lower {
                    Some((idx, 0))
                } else if name_lower.starts_with(&q_lower) {
                    Some((idx, 1))
                } else if name_lower.contains(&q_lower) {
                    Some((idx, 2))
                } else {
                    None
                }
            })
            .collect();
        let items = &self.items;
        ranked.sort_by(|(idx_a, tier_a), (idx_b, tier_b)| {
            tier_a
                .cmp(tier_b)
                .then_with(|| items[*idx_a].name.len().cmp(&items[*idx_b].name.len()))
                .then_with(|| items[*idx_a].name.cmp(&items[*idx_b].name))
        });

        self.prefix_count = ranked.iter().take_while(|(_, tier)| *tier <= 1).count();
        self.filtered = ranked.into_iter().map(|(idx, _)| idx).collect();
        self.selected = 0;
    }

    /// Bewegt die Markierung um einen Schritt nach oben.
    pub(crate) fn move_up(&mut self) {
        if !self.filtered.is_empty() {
            self.selection_moved = true;
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// Bewegt die Markierung um einen Schritt nach unten.
    pub(crate) fn move_down(&mut self) {
        if !self.filtered.is_empty() {
            self.selection_moved = true;
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

    /// Berechnet das shell-artige Tab-Vervollständigungsergebnis für den
    /// aktuellen Such-/Filterzustand.
    ///
    /// # Beschreibung
    /// - Keine Treffer → [`TabOutcome::None`].
    /// - Die Markierung wurde seit der letzten `on_query_change` per
    ///   Pfeiltaste/Ziffer bewegt (`selection_moved`) → übernimmt die
    ///   Markierung ([`TabOutcome::Accept`]), unabhängig vom Rang.
    /// - Sonst, ohne Präfix-Treffer (nur Teilstring-Treffer) → übernimmt den
    ///   bestplatzierten Teilstring-Treffer.
    /// - Sonst, mit genau einem Präfix-Treffer → übernimmt ihn.
    /// - Sonst berechnet das längste gemeinsame Präfix aller Präfix-Treffer:
    ///   ist es länger als der getippte Suchtext, wird die Eingabe darauf
    ///   erweitert ([`TabOutcome::ExtendQuery`], Popup bleibt offen); ist es
    ///   nicht länger (entspricht bereits dem Suchtext), übernimmt den
    ///   bestplatzierten Präfix-Treffer.
    ///
    /// # Rückgabe
    /// Das anzuwendende [`TabOutcome`].
    pub(crate) fn tab_outcome(&self) -> TabOutcome {
        if self.filtered.is_empty() {
            return TabOutcome::None;
        }
        if self.selection_moved {
            return TabOutcome::Accept(self.items[self.filtered[self.selected]].name.clone());
        }
        if self.prefix_count <= 1 {
            return TabOutcome::Accept(self.items[self.filtered[0]].name.clone());
        }

        let prefix_names: Vec<&str> = self.filtered[..self.prefix_count]
            .iter()
            .map(|&idx| self.items[idx].name.as_str())
            .collect();
        let common = longest_common_prefix(&prefix_names);
        if common.chars().count() > self.query.chars().count() {
            TabOutcome::ExtendQuery(common)
        } else {
            TabOutcome::Accept(self.items[self.filtered[0]].name.clone())
        }
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
                Widget::render(
                    Line::styled(item.description.clone(), desc_style),
                    desc_area,
                    buf,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::CommandRegistry;
    use crate::test_support::{TestError, TestResult, ctx};

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn built_in_popup() -> TestResult<CommandPopup> {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        Ok(CommandPopup::new(&registry))
    }

    #[test]
    fn popup_contains_every_registry_spec() -> TestResult {
        let registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        let popup = CommandPopup::new(&registry);
        assert_eq!(popup.items.len(), registry.specs().len());
        for spec in registry.specs() {
            assert!(
                popup
                    .items
                    .iter()
                    .any(|item| item.name == spec.name.as_str())
            );
        }
        Ok(())
    }

    #[test]
    fn test_filter_narrows_list() -> TestResult {
        let mut popup = built_in_popup()?;
        let initial_count = popup.filtered.len();
        assert!(initial_count > 1);
        popup.on_query_change("hel");
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.selected_name(), Some("help"));
        Ok(())
    }

    #[test]
    fn test_enter_returns_accept() -> TestResult {
        let mut popup = built_in_popup()?;
        let expected = popup
            .selected_name()
            .ok_or(TestError::Missing("selected_name"))?
            .to_owned();
        assert_eq!(
            popup.on_key(make_key(KeyCode::Enter)),
            PopupAction::Accept(expected)
        );
        Ok(())
    }

    #[test]
    fn test_esc_returns_cancel() -> TestResult {
        let mut popup = built_in_popup()?;
        assert_eq!(popup.on_key(make_key(KeyCode::Esc)), PopupAction::Cancel);
        Ok(())
    }

    #[test]
    fn test_digit_selects_nth_item() -> TestResult {
        let mut popup = built_in_popup()?;
        assert!(popup.filtered.len() >= 2);
        let second_name = popup.items[popup.filtered[1]].name.clone();
        assert_eq!(
            popup.on_key(make_key(KeyCode::Char('2'))),
            PopupAction::Accept(second_name)
        );
        Ok(())
    }

    #[test]
    fn test_navigation_stays_in_bounds() -> TestResult {
        let mut popup = built_in_popup()?;
        let len = popup.filtered.len();
        popup.move_up();
        assert_eq!(popup.selected, 0);
        for _ in 0..len + 5 {
            popup.move_down();
        }
        assert_eq!(popup.selected, len - 1);
        assert!(popup.selected_name().is_some());
        Ok(())
    }

    #[test]
    fn test_is_empty_after_no_match_query() -> TestResult {
        let mut popup = built_in_popup()?;
        popup.on_query_change("xyzzy_existiert_nicht_12345");
        assert!(popup.is_empty());
        assert_eq!(popup.selected_name(), None);
        Ok(())
    }

    #[test]
    fn test_digit_out_of_range_returns_stay() -> TestResult {
        let mut popup = built_in_popup()?;
        popup.on_query_change("help");
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(
            popup.on_key(make_key(KeyCode::Char('2'))),
            PopupAction::Stay
        );
        Ok(())
    }

    #[test]
    fn test_empty_query_restores_all_items() -> TestResult {
        let mut popup = built_in_popup()?;
        let initial_count = popup.filtered.len();
        popup.on_query_change("help");
        popup.on_query_change("");
        assert_eq!(popup.filtered.len(), initial_count);
        Ok(())
    }

    #[test]
    fn test_selected_clamped_after_filter_shrinks() -> TestResult {
        let mut popup = built_in_popup()?;
        for _ in 0..20 {
            popup.move_down();
        }
        assert!(popup.selected > 0);
        popup.on_query_change("help");
        assert_eq!(popup.selected, 0);
        Ok(())
    }

    /// Case-insensitive Rang-Klassifikation: Präfix-Treffer stehen vor
    /// reinen Teilstring-Treffern, unabhängig von Registrierungsreihenfolge
    /// oder alphabetischer Substring-Position.
    ///
    /// Regression: `/mo` listete vorher `/model`, `/research-web`, `/mode`
    /// … in Registrierungsreihenfolge mit `contains`-Filter — ein Teilstring-
    /// Treffer wie `/memory` (enthält „mo", beginnt aber nicht damit) konnte
    /// vor einem echten Präfix-Treffer stehen. `/mode` und `/model` sind
    /// beide Präfix-Treffer für `mo`; `/memory` ist nur ein Teilstring-Treffer
    /// und muss dahinter einsortiert werden.
    #[test]
    fn ranking_prefers_prefix_matches_over_substring_matches() -> TestResult {
        let mut popup = built_in_popup()?;
        popup.on_query_change("mo");

        assert_eq!(
            popup.prefix_count, 2,
            "expected exactly two prefix matches ('mode', 'model') for 'mo'"
        );
        let leading: Vec<&str> = popup.filtered[..popup.prefix_count]
            .iter()
            .map(|&idx| popup.items[idx].name.as_str())
            .collect();
        assert_eq!(
            leading,
            vec!["mode", "model"],
            "prefix matches must lead, sorted by length then alphabetically"
        );

        assert!(
            popup.prefix_count < popup.filtered.len(),
            "fixture must also contain a substring-only match for 'mo' (/memory)"
        );
        let trailing: Vec<&str> = popup.filtered[popup.prefix_count..]
            .iter()
            .map(|&idx| popup.items[idx].name.as_str())
            .collect();
        assert!(
            trailing.contains(&"memory"),
            "substring-only match '/memory' must rank below the prefix matches, got {trailing:?}"
        );
        for &idx in &popup.filtered[popup.prefix_count..] {
            assert!(
                !popup.items[idx].name.to_ascii_lowercase().starts_with("mo"),
                "trailing rank must not contain prefix matches, got {:?}",
                popup.items[idx].name
            );
        }
        Ok(())
    }

    #[test]
    fn visible_range_scrolls_when_selection_passes_available_rows() -> TestResult {
        let mut popup = built_in_popup()?;
        assert!(
            popup.filtered.len() > 8,
            "test requires more than eight commands"
        );
        for _ in 0..8 {
            popup.move_down();
        }

        assert_eq!(popup.visible_range(8), 1..9);
        assert!(popup.visible_range(8).contains(&popup.selected));
        Ok(())
    }
}

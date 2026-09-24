//! Wiederverwendbarer nummerierter Auswahldialog im Stil des Freigabe-Panels.
//!
//! Spec-Quelle: Freigabe-Panel mit nummerierten Optionen,
//! `docs/design/tui-command-contract.md`
//! Slice E1 (`/export`-Auswahl: Zwischenablage / Datei / Abbrechen).
//!
//! # Verantwortung
//! Stellt einen generischen, gerahmten Auswahldialog bereit, der von
//! `/export` (und später weiteren Befehlen) für einfache Mehrfachauswahlen
//! wiederverwendet werden kann. Kennt keine `/export`-spezifische Logik.
//!
//! # Schlüsseltypen
//! - [`ChoiceDialog`] — Widget-Zustand (Titel, optionaler Hinweistext, Optionen, Auswahl).
//! - [`ChoiceAction`] — Ereignis, das [`ChoiceDialog::handle_key`] zurückgibt.
//!
//! # Nebenläufigkeit
//! Kein interner Zustand wird geteilt; der Aufrufer hält `ChoiceDialog` exklusiv,
//! analog zu [`crate::command_popup::CommandPopup`].
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.

use crossterm::event::{KeyCode, KeyEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{
        Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget,
        Widget, Wrap,
    },
};
use unicode_width::UnicodeWidthChar;

use crate::dialog_frame::{BodyScroll, wrapped_rows};
use crate::sanitize::sanitize_inline;
use crate::style;

/// Fußzeilen-Hinweis mit der Tastaturbelegung des Dialogs.
const FOOTER_HINT: &str = "↑↓ wählen · Enter bestätigen · Esc abbrechen";

/// Aktionssignal, das [`ChoiceDialog::handle_key`] zurückgibt.
///
/// # Beschreibung
/// Teilt dem Aufrufer mit, was mit dem Dialog geschehen soll: offen lassen,
/// abbrechen oder eine Option annehmen.
///
/// # Varianten
/// - `Stay`: Dialog bleibt offen; keine Aktion.
/// - `Cancel`: Nutzer hat Esc gedrückt; Dialog schließen.
/// - `Chosen(usize)`: Nutzer hat die Option am angegebenen Index gewählt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceAction {
    /// Dialog bleibt geöffnet, keine weitere Aktion erforderlich.
    Stay,
    /// Nutzer hat Esc gedrückt; Dialog soll geschlossen werden.
    Cancel,
    /// Nutzer hat die Option am angegebenen (nullbasierten) Index gewählt.
    Chosen(usize),
}

/// Zustand eines nummerierten Auswahldialogs mit Tastaturnavigation.
///
/// # Beschreibung
/// Verwaltet Titel, optionalen Hinweistext, die Liste der Optionen und den
/// aktuell markierten Index. Das Widget zeichnet sich selbst direkt in einen
/// ratatui-`Buffer`, analog zu [`crate::command_popup::CommandPopup`].
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; exklusiver `&mut`-Zugriff des Aufrufers erwartet.
#[derive(Debug, Clone)]
pub struct ChoiceDialog {
    /// Titel im Rahmen des Dialogs.
    title: String,
    /// Optionaler Hinweistext oberhalb der Optionsliste.
    prompt: Option<String>,
    /// Liste der Optionsbeschriftungen in Anzeigereihenfolge.
    options: Vec<String>,
    /// Index in `options`, der aktuell markiert ist.
    selected: usize,
    /// Optionaler Fußzeilen-Hinweis, der [`FOOTER_HINT`] überschreibt (siehe
    /// [`Self::with_footer_hint`]). `None` (der Standard bei [`Self::new`])
    /// belässt es beim Standardtext.
    footer_hint: Option<String>,
    /// Scroll-Zustand des Hinweistexts (langer Befehl/Grund der
    /// Host-Permit-Frage), `Strg+↑↓` bzw. Mausrad.
    body_scroll: BodyScroll,
}

impl ChoiceDialog {
    /// Erstellt einen neuen `ChoiceDialog` mit der ersten Option markiert.
    ///
    /// # Beschreibung
    /// `options` darf leer sein; in diesem Fall liefert [`Self::handle_key`]
    /// bei `Enter` stets `ChoiceAction::Stay`.
    ///
    /// # Argumente
    /// - `title` (`impl Into<String>`): Titel im Rahmen des Dialogs.
    /// - `prompt` (`Option<String>`): optionaler Hinweistext oberhalb der Optionen.
    /// - `options` (`Vec<String>`): Optionsbeschriftungen in Anzeigereihenfolge.
    ///
    /// # Rückgabe
    /// Initialisierter `ChoiceDialog` mit `selected == 0`.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::choice_dialog::ChoiceDialog;
    /// let dialog = ChoiceDialog::new(
    ///     "Export".to_owned(),
    ///     None,
    ///     vec!["In die Zwischenablage kopieren".to_owned()],
    /// );
    /// ```
    #[must_use]
    pub fn new(title: impl Into<String>, prompt: Option<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            prompt,
            options,
            selected: 0,
            footer_hint: None,
            body_scroll: BodyScroll::default(),
        }
    }

    /// Setzt die anfänglich markierte Option und gibt `self` für
    /// Builder-Verkettung zurück (z. B. um den aktiven Provider/Modell
    /// vorauszuwählen).
    ///
    /// # Beschreibung
    /// `index` wird auf den gültigen Bereich `0..options.len()` begrenzt
    /// (leere Optionsliste → `selected` bleibt `0`, ohne Panik).
    ///
    /// # Argumente
    /// - `index` (`usize`): der gewünschte Anfangsindex.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_selected(mut self, index: usize) -> Self {
        self.selected = if self.options.is_empty() {
            0
        } else {
            index.min(self.options.len() - 1)
        };
        self
    }

    /// Überschreibt den Fußzeilen-Hinweis (Standard: [`FOOTER_HINT`]) und
    /// gibt `self` für Builder-Verkettung zurück.
    ///
    /// # Beschreibung
    /// Für Aufrufer, die dem Nutzer stufen-spezifische Tastaturhinweise
    /// geben müssen (z. B. `crate::model_switch_picker::ModelSwitchPicker`,
    /// das der Modell-Stufe einen zusätzlichen "← zurück"-Hinweis gibt, den
    /// die Provider-Stufe nicht hat).
    ///
    /// # Argumente
    /// - `hint` (`impl Into<String>`): der Ersatztext für die Fußzeile.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_footer_hint(mut self, hint: impl Into<String>) -> Self {
        self.footer_hint = Some(hint.into());
        self
    }

    /// Berechnet die Höhe (in Zeilen, inklusive Rahmen), die [`Self::render`]
    /// benötigt, um jede Option und den Hinweistext ohne Abschneiden
    /// darzustellen.
    ///
    /// # Beschreibung
    /// Anders als [`crate::approval_dialog::ApprovalDialog::desired_height`]
    /// nimmt diese Methode keine Breite entgegen: [`Self::render`] bricht
    /// weder den Hinweistext noch eine Optionsbeschriftung um (beide werden
    /// als genau eine Zeile gerendert, notfalls vom Terminal seitlich
    /// abgeschnitten) — die benötigte Zeilenzahl hängt deshalb nicht von der
    /// Breite ab. Ein Aufrufer, der diesen Dialog wie
    /// [`crate::approval_dialog::ApprovalDialog`] anstelle des Composers
    /// zeichnet (statt als Vollflächen-Overlay), bemisst die Layout-Zeile
    /// damit über diese Methode statt über einen festen Platzhalterwert.
    ///
    /// # Rückgabe
    /// Rahmen (2) plus, falls ein Hinweistext gesetzt ist, dessen eine Zeile
    /// plus eine Leerzeile (siehe [`Self::render`]) plus eine Zeile je Option
    /// plus die Fußzeile (1).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn desired_height(&self) -> u16 {
        let border = 2u16;
        let prompt_rows: u16 = if self.prompt.is_some() { 2 } else { 0 };
        let option_rows = u16::try_from(self.options.len()).unwrap_or(u16::MAX);
        let footer_rows = 1u16;
        border
            .saturating_add(prompt_rows)
            .saturating_add(option_rows)
            .saturating_add(footer_rows)
    }

    /// Wie [`Self::desired_height`], aber mit umgebrochenem Hinweistext und
    /// umgebrochener Fußzeile bei der Gesamtbreite `width` (inkl. Rahmen) —
    /// für Aufrufer, die den Dialog anstelle des Composers zeichnen.
    #[must_use]
    pub fn desired_height_for(&self, width: u16) -> u16 {
        let inner = width.saturating_sub(2).max(1);
        let prompt_rows = self
            .prompt_lines(style::Theme::Dark)
            .map_or(0, |lines| wrapped_rows(&lines, inner).saturating_add(1));
        let footer_rows = wrapped_rows(&[Line::from(self.footer_text().to_owned())], inner);
        let rows = 2 + prompt_rows + self.options.len() + footer_rows;
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    /// Scrollt den Hinweistext per Mausrad.
    ///
    /// # Rückgabe
    /// `true` für Rad-Ereignisse (neu zeichnen).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scroll_wheel(&self, kind: MouseEventKind) -> bool {
        self.body_scroll.handle_wheel(kind)
    }

    /// Scrollt den Hinweistext um `lines` Zeilen; ändert nie die Auswahl.
    ///
    /// # Rückgabe
    /// Immer `true` (neu zeichnen).
    pub fn scroll_body(&self, up: bool, lines: usize) -> bool {
        if up {
            self.body_scroll.scroll_up(lines);
        } else {
            self.body_scroll.scroll_down(lines);
        }
        true
    }

    fn footer_text(&self) -> &str {
        self.footer_hint.as_deref().unwrap_or(FOOTER_HINT)
    }

    fn prompt_lines(&self, theme: style::Theme) -> Option<Vec<Line<'static>>> {
        self.prompt.as_ref().map(|prompt| {
            vec![Line::styled(
                sanitize_inline(prompt),
                style::dim_style(theme),
            )]
        })
    }

    /// Verarbeitet einen Tastendruck und gibt eine [`ChoiceAction`] zurück.
    ///
    /// # Beschreibung
    /// `Up`/`Down` bewegen die Markierung innerhalb der Optionsliste
    /// (mit Begrenzung an den Rändern). `Enter` wählt die markierte Option,
    /// sofern die Liste nicht leer ist. `Esc` bricht ab. Ziffern `1`-`9`
    /// wählen direkt die entsprechende Option (nullbasiert `Ziffer - 1`),
    /// sofern der Index existiert.
    ///
    /// # Argumente
    /// - `key` (`KeyEvent`): das eingegangene Crossterm-Tastenereignis.
    ///
    /// # Rückgabe
    /// - [`ChoiceAction::Stay`] bei `Up`/`Down`, unbekannter Taste, leerer
    ///   Liste bei `Enter`, oder Ziffer außerhalb des gültigen Bereichs.
    /// - [`ChoiceAction::Cancel`] bei `Esc`.
    /// - [`ChoiceAction::Chosen(index)`] bei `Enter` oder passender Ziffer.
    ///
    /// # Beispiele
    /// ```ignore
    /// use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    /// use harw_tui::choice_dialog::{ChoiceAction, ChoiceDialog};
    ///
    /// let mut dialog = ChoiceDialog::new("Export".to_owned(), None, vec!["Ja".to_owned()]);
    /// let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    /// assert_eq!(dialog.handle_key(key), ChoiceAction::Cancel);
    /// ```
    pub fn handle_key(&mut self, key: KeyEvent) -> ChoiceAction {
        // `Strg+↑↓` scrollt den Hinweistext und ändert nie die Auswahl.
        if self.body_scroll.handle_key(&key) {
            return ChoiceAction::Stay;
        }
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ChoiceAction::Stay
            }
            KeyCode::Down => {
                if !self.options.is_empty() {
                    self.selected = (self.selected + 1).min(self.options.len() - 1);
                }
                ChoiceAction::Stay
            }
            KeyCode::Enter => {
                if self.options.is_empty() {
                    ChoiceAction::Stay
                } else {
                    ChoiceAction::Chosen(self.selected)
                }
            }
            KeyCode::Esc => ChoiceAction::Cancel,
            KeyCode::Char(c @ '1'..='9') => {
                let zero_based = (c as usize) - ('1' as usize);
                if zero_based < self.options.len() {
                    ChoiceAction::Chosen(zero_based)
                } else {
                    ChoiceAction::Stay
                }
            }
            _ => ChoiceAction::Stay,
        }
    }

    /// Zeichnet den Dialog (Rahmen, optionaler Hinweis, nummerierte Optionen,
    /// Fußzeile) in den angegebenen `Buffer`-Bereich.
    ///
    /// # Beschreibung
    /// Layout von oben nach unten: umrahmender `Block` mit `self.title` als
    /// Titel; optionaler, gedimmter Hinweistext gefolgt von einer Leerzeile;
    /// nummerierte Optionen (`1. …`, `2. …`, …) mit `❯`-Marker vor der
    /// markierten Zeile in Akzentfarbe; gedimmte Fußzeile mit
    /// [`FOOTER_HINT`]. Fremdtext (Titel, Hinweis, Optionsbeschriftungen)
    /// wird über [`crate::sanitize::sanitize_inline`] terminal-sicher
    /// aufbereitet.
    ///
    /// # Argumente
    /// - `area` (`Rect`): der Zeichenbereich im Terminal-Buffer.
    /// - `buf` (`&mut Buffer`): der ratatui-Buffer, in den geschrieben wird.
    /// - `theme` (`style::Theme`): aktives Farbschema für Auswahl- und Dim-Stile.
    ///
    /// # Nebenläufigkeit
    /// Rein synchron; kein Locking erforderlich.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        let area = area.intersection(buf.area);
        let title = sanitize_inline(&self.title);
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        Widget::render(block, area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let width = inner.width;
        let height = usize::from(inner.height);

        // Fußzeile: angeheftet unten, umgebrochen statt abgeschnitten.
        let footer = vec![Line::styled(
            self.footer_text().to_owned(),
            style::dim_style(theme),
        )];
        let footer_rows = if height >= 2 {
            wrapped_rows(&footer, width).min(height - 1)
        } else {
            0
        };
        let available = height - footer_rows;

        // Hinweistext: bekommt, was die Optionen übrig lassen (mindestens
        // eine Zeile), und scrollt darüber hinaus.
        let prompt = self.prompt_lines(theme);
        let prompt_rows = prompt
            .as_ref()
            .map_or(0, |lines| wrapped_rows(lines, width));
        let rest = available.saturating_sub(self.options.len());
        let (prompt_height, gap) = if prompt_rows == 0 {
            (0, 0)
        } else if rest > prompt_rows {
            (prompt_rows, 1)
        } else if rest >= 1 {
            (rest, 0)
        } else if available > 1 {
            (1, 0)
        } else {
            (0, 0)
        };
        let max_offset = prompt_rows.saturating_sub(prompt_height);
        self.body_scroll.set_limit(max_offset);
        let offset = self.body_scroll.offset();
        if let Some(lines) = prompt
            && prompt_height > 0
        {
            let prompt_area = Rect {
                height: u16::try_from(prompt_height).unwrap_or(inner.height),
                ..inner
            };
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0))
                .render(prompt_area, buf);
            if max_offset > 0 {
                let bar_area = Rect {
                    width: inner.width.saturating_add(1),
                    ..prompt_area
                }
                .intersection(area);
                let mut state = ScrollbarState::new(max_offset.saturating_add(1))
                    .position(offset)
                    .viewport_content_length(prompt_height);
                StatefulWidget::render(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .begin_symbol(None)
                        .end_symbol(None),
                    bar_area,
                    buf,
                    &mut state,
                );
            }
        }

        // Optionen: ein Fenster um die Auswahl, falls nicht alle passen.
        let options_height = available.saturating_sub(prompt_height + gap);
        let start = if options_height > 0 && self.selected >= options_height {
            self.selected + 1 - options_height
        } else {
            0
        };
        let mut y = inner.y + u16::try_from(prompt_height + gap).unwrap_or(inner.height);
        for (idx, option) in self
            .options
            .iter()
            .enumerate()
            .skip(start)
            .take(options_height)
        {
            let is_selected = idx == self.selected;
            let marker = if is_selected { "❯ " } else { "  " };
            let row_style = if is_selected {
                style::selected_style(theme)
            } else {
                Style::default()
            };
            let label = fit_to_width(
                &format!("{marker}{}. {}", idx + 1, sanitize_inline(option)),
                usize::from(width),
            );
            let row_area = Rect::new(inner.x, y, inner.width, 1);
            Widget::render(Line::styled(label, row_style), row_area, buf);
            y = y.saturating_add(1);
        }

        if footer_rows > 0 {
            let footer_height = u16::try_from(footer_rows).unwrap_or(1);
            let footer_area = Rect {
                y: inner.bottom() - footer_height,
                height: footer_height,
                ..inner
            };
            Paragraph::new(footer)
                .wrap(Wrap { trim: false })
                .render(footer_area, buf);
        }
    }
}

/// Kürzt `text` auf `width` Spalten (mit `…`), ohne Zeichen zu zerteilen.
fn fit_to_width(text: &str, width: usize) -> String {
    let total: usize = text.chars().map(|ch| ch.width().unwrap_or(0)).sum();
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::*;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn export_dialog() -> ChoiceDialog {
        ChoiceDialog::new(
            "Export",
            Some("Wie soll die Session exportiert werden?".to_owned()),
            vec![
                "In die Zwischenablage kopieren".to_owned(),
                "Als Datei speichern".to_owned(),
                "Abbrechen".to_owned(),
            ],
        )
    }

    /// `Esc` liefert `ChoiceAction::Cancel`.
    #[test]
    fn test_esc_returns_cancel() {
        let mut dialog = export_dialog();
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Esc)),
            ChoiceAction::Cancel
        );
    }

    /// `Enter` auf der Startauswahl liefert `ChoiceAction::Chosen(0)`.
    #[test]
    fn test_enter_returns_chosen_zero_by_default() {
        let mut dialog = export_dialog();
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Chosen(0)
        );
    }

    /// `Down` gefolgt von `Enter` wählt die zweite Option.
    #[test]
    fn test_down_then_enter_chooses_second_option() {
        let mut dialog = export_dialog();
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Down)),
            ChoiceAction::Stay
        );
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Chosen(1)
        );
    }

    /// `Down` über das Ende hinaus bleibt bei der letzten Option.
    #[test]
    fn test_down_stays_in_bounds() {
        let mut dialog = export_dialog();
        for _ in 0..10 {
            dialog.handle_key(make_key(KeyCode::Down));
        }
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Chosen(2)
        );
    }

    /// `Up` an Position 0 bleibt bei 0.
    #[test]
    fn test_up_stays_in_bounds() {
        let mut dialog = export_dialog();
        dialog.handle_key(make_key(KeyCode::Up));
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Chosen(0)
        );
    }

    /// Zifferntaste `'3'` wählt direkt die dritte Option.
    #[test]
    fn test_digit_selects_nth_option() {
        let mut dialog = export_dialog();
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('3'))),
            ChoiceAction::Chosen(2)
        );
    }

    /// Zifferntaste außerhalb der Optionsliste liefert `Stay`.
    #[test]
    fn test_digit_out_of_range_returns_stay() {
        let mut dialog = export_dialog();
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Char('9'))),
            ChoiceAction::Stay
        );
    }

    /// Bei leerer Optionsliste liefert `Enter` stets `Stay`.
    #[test]
    fn test_enter_on_empty_options_stays() {
        let mut dialog = ChoiceDialog::new("Export", None, vec![]);
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Stay
        );
    }

    /// Das Rendering enthält Titel, alle Optionsbeschriftungen und die Fußzeile.
    #[test]
    fn test_render_contains_option_labels() {
        let dialog = export_dialog();
        let area = Rect::new(0, 0, 50, 8);
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, style::Theme::Dark);

        let rendered: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered.contains("Export"));
        assert!(rendered.contains("In die Zwischenablage kopieren"));
        assert!(rendered.contains("Als Datei speichern"));
        assert!(rendered.contains("Abbrechen"));
        assert!(rendered.contains("wählen"));
    }

    /// `desired_height` zählt Rahmen, Hinweiszeile plus Leerzeile, eine Zeile
    /// je Option und die Fußzeile.
    #[test]
    fn test_desired_height_counts_prompt_options_and_footer() {
        let dialog = export_dialog();
        // 2 (Rahmen) + 2 (Hinweis + Leerzeile) + 3 (Optionen) + 1 (Fußzeile).
        assert_eq!(dialog.desired_height(), 8);
    }

    /// Ohne Hinweistext entfallen die zwei dafür reservierten Zeilen.
    #[test]
    fn test_desired_height_without_prompt_omits_prompt_rows() {
        let dialog = ChoiceDialog::new("Export", None, vec!["Ja".to_owned(), "Nein".to_owned()]);
        // 2 (Rahmen) + 0 (kein Hinweis) + 2 (Optionen) + 1 (Fußzeile).
        assert_eq!(dialog.desired_height(), 5);
    }

    /// `with_footer_hint` überschreibt den gerenderten Fußzeilentext.
    #[test]
    fn test_with_footer_hint_overrides_rendered_footer() {
        let dialog = export_dialog()
            .with_footer_hint("↑↓ wählen · ← zurück · Enter bestätigen · Esc abbrechen");
        let area = Rect::new(0, 0, 60, 8);
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, style::Theme::Dark);

        let rendered: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered.contains("zurück"));
        assert!(!rendered.contains(FOOTER_HINT));
    }

    /// Ohne `with_footer_hint`-Aufruf bleibt der gerenderte Fußzeilentext
    /// beim Standard [`FOOTER_HINT`].
    #[test]
    fn test_without_footer_hint_override_uses_default_footer() {
        let dialog = export_dialog();
        let area = Rect::new(0, 0, 60, 8);
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, style::Theme::Dark);

        let rendered: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered.contains(FOOTER_HINT));
    }

    /// Host-Permit-Muster: langer Hinweistext in einem kleinen Fenster —
    /// Optionen und Fußzeile bleiben sichtbar, der Hinweis scrollt, ohne
    /// die Auswahl zu ändern.
    #[test]
    fn test_long_prompt_scrolls_while_options_stay_visible() {
        let prompt = (0..30)
            .map(|i| format!("wort{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut dialog = ChoiceDialog::new(
            "Host-Ausführung erlauben?",
            Some(prompt),
            vec![
                "Einmalig".to_owned(),
                "Für die Sitzung".to_owned(),
                "Nein, ablehnen".to_owned(),
            ],
        )
        .with_selected(1);
        let render = |dialog: &ChoiceDialog| {
            let area = Rect::new(0, 0, 40, 8);
            let mut buf = Buffer::empty(area);
            dialog.render(area, &mut buf, style::Theme::Dark);
            (0..8u16)
                .map(|y| (0..40u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let before = render(&dialog);
        for needle in [
            "1. Einmalig",
            "2. Für die Sitzung",
            "3. Nein, ablehnen",
            "wort0",
        ] {
            assert!(before.contains(needle), "{needle}: {before}");
        }
        assert!(dialog.desired_height_for(40) > 8);
        let ctrl_down = KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL);
        assert_eq!(dialog.handle_key(ctrl_down), ChoiceAction::Stay);
        assert!(dialog.scroll_wheel(crossterm::event::MouseEventKind::ScrollDown));
        let after = render(&dialog);
        assert!(!after.contains("wort0 "), "{after}");
        for needle in ["1. Einmalig", "❯ 2. Für die Sitzung", "3. Nein, ablehnen"] {
            assert!(after.contains(needle), "{needle}: {after}");
        }
        assert_eq!(
            dialog.handle_key(make_key(KeyCode::Enter)),
            ChoiceAction::Chosen(1),
            "Scrollen hat die Auswahl nicht verändert"
        );
    }
}

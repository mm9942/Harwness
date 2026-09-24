//! Generischer Overlay-Vertrag für neue TUI-Ansichten.
//!
//! Spec-Quelle: `tui_contract.md`, Entscheidung 1 („Neue TUI-Ansichten über
//! einen generischen Overlay-Typ“).
//!
//! # Verantwortung
//! Definiert das Trait [`OverlayView`], das jede neue Vollbild-/Dialog-
//! Ansicht (Hilfe, Modelle je Rolle, Modus-Picker, Kanban, …) implementiert,
//! sowie das Ergebnis eines Tastendrucks [`OverlayOutcome`]. Die Ansichten
//! selbst führen nie etwas aus: Schreibaktionen werden als Slash-Zeilen
//! zurückgegeben, die `app.rs` über den regulären Befehlsweg schickt (die
//! Operationen prüfen und speichern).
//!
//! Zusätzlich stellt das Modul kleine, gemeinsam genutzte Zeichen- und
//! Tastenhelfer bereit ([`render_panel`], [`is_down`], [`is_up`],
//! [`scroll_offset_for`]), damit alle Ansichten gleich aussehen und sich
//! gleich bedienen lassen.
//!
//! # Schlüsseltypen
//! - [`OverlayView`] — Trait der Ansichten (`render`/`on_key`, optional
//!   Daten-Nachladen über `refresh_command`/`apply_data`/`apply_error` und
//!   Live-Ereignisse über `apply_event`).
//! - [`OverlayOutcome`] — was nach einem Tastendruck geschehen soll.
//!
//! # Nebenläufigkeit
//! Keine; der Aufrufer hält die Ansicht exklusiv (`Box<dyn OverlayView>`).
//!
//! # Fehlertypen
//! Keine — alle Operationen sind infallibel.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Widget},
};

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Ergebnis eines Tastendrucks in einer [`OverlayView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayOutcome {
    /// Ansicht bleibt offen; ggf. neu zeichnen.
    Stay,
    /// Ansicht schließen, nichts ausführen.
    Close,
    /// Slash-Zeile ausführen, Ansicht bleibt offen (typisch: danach
    /// [`OverlayView::refresh_command`] erneut abfragen).
    Run(String),
    /// Slash-Zeile ausführen und Ansicht schließen.
    RunAndClose(String),
    /// Daten-Befehl im Hintergrund ausführen; das Ergebnis kommt über
    /// [`OverlayView::apply_data`] bzw. [`OverlayView::apply_error`] zurück.
    Fetch(String),
    /// Ansicht schließen und die Eingabezeile mit diesem Text vorbelegen.
    Prefill(String),
}

/// Vertrag einer generischen TUI-Overlay-Ansicht.
///
/// # Beschreibung
/// `app.rs` hält eine Ansicht als `Overlay::View(Box<dyn OverlayView>)`,
/// leitet Tasten an [`Self::on_key`] weiter und setzt das
/// [`OverlayOutcome`] um. Liefert [`Self::refresh_command`] einen Befehl,
/// führt `app.rs` ihn beim Öffnen (und nach `Run`) mit Datenrückgabe aus und
/// reicht `OpOutput.data` an [`Self::apply_data`] bzw. einen Fehlertext an
/// [`Self::apply_error`] weiter.
pub(crate) trait OverlayView: std::fmt::Debug {
    /// Zeichnet die Ansicht in `area`.
    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme);

    /// Verarbeitet einen Tastendruck.
    fn on_key(&mut self, key: KeyEvent) -> OverlayOutcome;

    /// Befehl, dessen `OpOutput.data` die Ansicht befüllt (`None`: statische
    /// Ansicht ohne Nachladen).
    fn refresh_command(&self) -> Option<String> {
        None
    }

    /// Übernimmt das `data`-JSON des [`Self::refresh_command`]-Befehls.
    fn apply_data(&mut self, _data: &serde_json::Value) {}

    /// Übernimmt eine Fehlermeldung des [`Self::refresh_command`]-Befehls.
    fn apply_error(&mut self, _text: &str) {}

    /// Übernimmt ein Live-Ereignis vom Agenten-Bus (z. B. ein Matrix-Spiel-
    /// Ereignis als `{"run_id", "event"}`). Standard: ignorieren.
    fn apply_event(&mut self, _event: &serde_json::Value) {}
}

/// `true` für „nach unten“: `↓` oder `j` (ohne Strg/Alt).
#[must_use]
pub(crate) fn is_down(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Down => true,
        KeyCode::Char('j') => plain(key),
        _ => false,
    }
}

/// `true` für „nach oben“: `↑` oder `k` (ohne Strg/Alt).
#[must_use]
pub(crate) fn is_up(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Up => true,
        KeyCode::Char('k') => plain(key),
        _ => false,
    }
}

/// `true`, wenn weder Strg noch Alt gedrückt ist (Shift ist erlaubt).
#[must_use]
pub(crate) fn plain(key: &KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// Scroll-Offset, bei dem Zeile `focus` in einem Fenster von `height`
/// Zeilen sichtbar ist (die Fokuszeile wandert höchstens an den unteren Rand).
#[must_use]
pub(crate) fn scroll_offset_for(focus: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    focus.saturating_sub(height.saturating_sub(1))
}

/// Anzahl scrollbarer Inhaltszeilen, die [`render_panel`] in `area`
/// darstellt (Rahmen, Fußzeile und ggf. feste Kopfzeile abgezogen).
#[must_use]
pub(crate) fn content_height(area: Rect, has_header: bool) -> usize {
    let inner = area.height.saturating_sub(2);
    let footer = u16::from(inner >= 2);
    let rest = inner.saturating_sub(footer);
    let header = u16::from(has_header && rest >= 2);
    usize::from(rest.saturating_sub(header))
}

/// Zeichnet einen gerahmten Bereich mit Titel, Inhaltszeilen ab `scroll`
/// und gedimmter Fußzeile — im Stil von [`crate::choice_dialog::ChoiceDialog`].
///
/// # Argumente
/// - `title`: Rahmentitel (wird bereinigt).
/// - `header`: optionale feste (nicht scrollende) erste Zeile, z. B. eine
///   Reiterleiste (nur gezeichnet, wenn mindestens eine Inhaltszeile bleibt).
/// - `lines`: alle Inhaltszeilen; sichtbar ist das Fenster ab `scroll`.
/// - `scroll`: erster sichtbarer Zeilenindex (wird auf das Ende geklemmt).
/// - `footer`: Tastenhinweis in der letzten Zeile.
// Reine Zeichenfunktion: die Parameter sind unabhängige Layout-Eingaben,
// ein Hilfs-Struct brächte hier keine Klarheit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_panel(
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    title: &str,
    header: Option<Line<'_>>,
    lines: &[Line<'_>],
    scroll: usize,
    footer: &str,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(ratatui::style::Style::default().fg(style::border_color(theme)))
        .title(sanitize_inline(title));
    let inner = block.inner(area);
    Widget::render(block, area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let footer_reserved: u16 = u16::from(inner.height >= 2);
    let mut y = inner.y;
    let mut body_rows = inner.height.saturating_sub(footer_reserved);
    if let Some(header) = header
        && body_rows >= 2
    {
        Widget::render(header, Rect::new(inner.x, y, inner.width, 1), buf);
        y = y.saturating_add(1);
        body_rows = body_rows.saturating_sub(1);
    }
    let height = usize::from(body_rows);
    let max_scroll = lines.len().saturating_sub(height);
    let start = scroll.min(max_scroll);

    for line in lines.iter().skip(start).take(height) {
        let row = Rect::new(inner.x, y, inner.width, 1);
        Widget::render(line.clone(), row, buf);
        y = y.saturating_add(1);
    }

    if footer_reserved == 1 {
        let footer_area = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
        Widget::render(
            Line::styled(footer.to_owned(), style::dim_style(theme)),
            footer_area,
            buf,
        );
    }
}

/// Liest ein Stringfeld aus einem JSON-Objekt (tolerant: fehlend/leer/kein
/// String → `None`).
#[must_use]
pub(crate) fn json_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Rendert einen Buffer zu einem String (nur für Tests der Ansichten).
#[cfg(test)]
pub(crate) fn buffer_text(buf: &Buffer) -> String {
    buf.content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn test_up_down_accept_arrows_and_vim_keys() {
        assert!(is_down(&key(KeyCode::Down)));
        assert!(is_down(&key(KeyCode::Char('j'))));
        assert!(is_up(&key(KeyCode::Up)));
        assert!(is_up(&key(KeyCode::Char('k'))));
        assert!(!is_down(&KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_up(&key(KeyCode::Char('x'))));
    }

    #[test]
    fn test_scroll_offset_keeps_focus_visible() {
        assert_eq!(scroll_offset_for(0, 5), 0);
        assert_eq!(scroll_offset_for(4, 5), 0);
        assert_eq!(scroll_offset_for(5, 5), 1);
        assert_eq!(scroll_offset_for(9, 0), 0);
    }

    #[test]
    fn test_render_panel_shows_title_lines_and_footer() {
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        let lines = vec![Line::from("eins"), Line::from("zwei")];
        render_panel(
            area,
            &mut buf,
            Theme::Dark,
            "Titel",
            Some(Line::from("Kopf")),
            &lines,
            0,
            "Esc",
        );
        let text = buffer_text(&buf);
        assert!(text.contains("Titel"));
        assert!(text.contains("Kopf"));
        assert!(text.contains("eins"));
        assert!(text.contains("zwei"));
        assert!(text.contains("Esc"));
    }

    #[test]
    fn test_json_str_is_tolerant() {
        let value = serde_json::json!({"a": "x", "b": "  ", "c": 3});
        assert_eq!(json_str(&value, "a").as_deref(), Some("x"));
        assert_eq!(json_str(&value, "b"), None);
        assert_eq!(json_str(&value, "c"), None);
        assert_eq!(json_str(&value, "d"), None);
    }
}

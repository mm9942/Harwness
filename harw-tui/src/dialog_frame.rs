//! Gemeinsames Layout der Dialoge, die den Composer ersetzen.
//!
//! # Verantwortung
//! Freigabe-Panel ([`crate::approval_dialog`]), sudo-Fenster
//! ([`crate::sudo_dialog`]), Host-Permit ([`crate::choice_dialog`]) und die
//! Plan-/`ask_user`-Fenster ([`crate::plan_dialog`],
//! [`crate::ask_user_dialog`]) teilen dieselbe Aufteilung:
//!
//! ```text
//! ┌ Titel ───────────────────── noch 4:52 ┐
//! │ Körper (Befehl, Info-Zeilen …)       ▲│  ← scrollt (Strg+↑↓, Mausrad)
//! │ …                                    █│
//! │                                       │  ← Polsterung (entfällt zuerst … )
//! │ ❯ 1. Ja                               │  ← angeheftet: immer sichtbar
//! │   2. Nein (Esc)                       │
//! │ ↑↓ wählen · Enter bestätigen · …      │
//! └──────────────── Strg+↑↓/Rad: mehr ────┘
//! ```
//!
//! - **Angeheftet** ([`PinnedRow`]): Optionen, Eingabezeile, Hinweiszeile —
//!   stehen immer vollständig unten im Fenster.
//! - **Körper**: alles andere; passt er nicht, scrollt er innerhalb des
//!   Fensters ([`BodyScroll`]) statt das Fenster zu sprengen.
//!
//! # Abbau bei Platzmangel
//! Reihenfolge, in der Platz freigegeben wird (der Aufrufer verdichtet
//! vorher ggf. das Hauptargument, z. B. „… v Details“):
//! 1. der Körper schrumpft bis auf eine Zeile und wird scrollbar
//!    (Info-Zeilen wandern aus dem sichtbaren Bereich, bleiben aber per
//!    Scrollen erreichbar);
//! 2. danach entfallen die als Polsterung markierten Leerzeilen;
//! 3. erst dann entfällt der Körper ganz.
//!
//! Die angehefteten Zeilen werden nie weggelassen; sie werden (wie der
//! Körper) umgebrochen, niemals seitlich abgeschnitten. Mindest-Layout:
//! [`MIN_LAYOUT_WIDTH`] × [`MIN_LAYOUT_HEIGHT`] Terminalzellen — dort
//! bleiben Optionen und Hinweis aller Dialoge vollständig sichtbar.
//!
//! # Zeichenfläche
//! Es wird nie außerhalb von `area ∩ buf.area` gezeichnet.
//!
//! # Tasten und Maus
//! Scrollen des Körpers: `Strg+↑`/`Strg+↓` (je eine Zeile) und das Mausrad
//! ([`WHEEL_LINES`] Zeilen) über dem Fenster. `↑`/`↓` bleiben der
//! Optionsauswahl vorbehalten; `Bild↑`/`Bild↓` scrollen weiterhin den
//! Verlauf (bzw. beim Plan-Fenster den Plan) — keine Doppelbelegung.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{
        Block, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
        Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

/// Kleinste Terminalbreite, für die das Mindest-Layout garantiert ist.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MIN_LAYOUT_WIDTH: u16 = 40;
/// Kleinste Terminalhöhe, für die das Mindest-Layout garantiert ist.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const MIN_LAYOUT_HEIGHT: u16 = 12;
/// Zeilen je Mausrad-Raste.
pub(crate) const WHEEL_LINES: usize = 3;
/// Hinweis auf dem unteren Rahmen, solange der Körper überläuft.
const SCROLL_HINT: &str = " Strg+↑↓/Rad: mehr ";
/// Kurzform von [`SCROLL_HINT`] für schmale Fenster.
const SCROLL_HINT_SHORT: &str = " ↕ ";

/// Scroll-Zustand des Dialog-Körpers.
///
/// # Beschreibung
/// `offset` zählt gerenderte (umgebrochene) Zeilen vom Anfang des Körpers.
/// Beim Zeichnen wird er auf den tatsächlich möglichen Bereich gekappt und
/// die Grenze in `max` gemerkt, damit Tasten/Mausrad zwischen zwei Frames
/// nicht über das Ende hinauslaufen. `Cell`, weil das Zeichnen nur `&self`
/// hat (dasselbe Muster wie die Verlaufs-Kennzahlen in `ChatApp`).
#[derive(Debug, Default, Clone)]
pub(crate) struct BodyScroll {
    offset: Cell<usize>,
    max: Cell<usize>,
}

impl BodyScroll {
    /// Aktueller Abstand vom Anfang des Körpers.
    #[must_use]
    pub(crate) fn offset(&self) -> usize {
        self.offset.get()
    }

    /// Scrollt `lines` Zeilen nach oben (Richtung Anfang).
    pub(crate) fn scroll_up(&self, lines: usize) {
        self.offset.set(self.offset.get().saturating_sub(lines));
    }

    /// Scrollt `lines` Zeilen nach unten, höchstens bis zur zuletzt
    /// gezeichneten Grenze.
    pub(crate) fn scroll_down(&self, lines: usize) {
        let next = self.offset.get().saturating_add(lines);
        self.offset.set(next.min(self.max.get()));
    }

    /// Setzt die beim Zeichnen ermittelte Grenze und kappt den Abstand.
    pub(crate) fn set_limit(&self, max: usize) {
        self.max.set(max);
        if self.offset.get() > max {
            self.offset.set(max);
        }
    }

    /// Setzt auf den Anfang zurück (z. B. nach „v Details“).
    pub(crate) fn reset(&self) {
        self.offset.set(0);
    }

    /// `true`, wenn der Körper beim letzten Zeichnen überlief.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn can_scroll(&self) -> bool {
        self.max.get() > 0
    }

    /// Ob `key` eine Scroll-Taste des Körpers ist (`Strg+↑`/`Strg+↓`).
    #[must_use]
    pub(crate) fn is_scroll_key(key: &KeyEvent) -> bool {
        key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
    }

    /// Verarbeitet `Strg+↑`/`Strg+↓`.
    ///
    /// # Rückgabe
    /// `true`, wenn die Taste eine Scroll-Taste war (auch am Rand) — sie
    /// darf dann nicht mehr an die Optionsauswahl gehen.
    pub(crate) fn handle_key(&self, key: &KeyEvent) -> bool {
        if !Self::is_scroll_key(key) {
            return false;
        }
        match key.code {
            KeyCode::Up => self.scroll_up(1),
            _ => self.scroll_down(1),
        }
        true
    }

    /// Verarbeitet das Mausrad.
    ///
    /// # Rückgabe
    /// `true` für Rad-Ereignisse (neu zeichnen), sonst `false`.
    pub(crate) fn handle_wheel(&self, kind: MouseEventKind) -> bool {
        match kind {
            MouseEventKind::ScrollUp => {
                self.scroll_up(WHEEL_LINES);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(WHEEL_LINES);
                true
            }
            _ => false,
        }
    }
}

/// Eine angeheftete Zeile (Option, Eingabe, Hinweis).
#[derive(Debug, Clone)]
pub(crate) struct PinnedRow {
    /// Inhalt; wird umgebrochen, nie abgeschnitten.
    pub(crate) line: Line<'static>,
    /// Reine Polsterung (Leerzeile), darf bei Platzmangel entfallen.
    pub(crate) padding: bool,
}

impl PinnedRow {
    /// Inhaltszeile.
    pub(crate) fn content(line: impl Into<Line<'static>>) -> Self {
        Self {
            line: line.into(),
            padding: false,
        }
    }

    /// Leerzeile als Polsterung.
    pub(crate) fn padding() -> Self {
        Self {
            line: Line::default(),
            padding: true,
        }
    }
}

/// Inhalt eines Dialogs: scrollbarer Körper plus angeheftete Zeilen.
#[derive(Debug, Clone, Default)]
pub(crate) struct DialogContent {
    /// Körperzeilen (werden umgebrochen und bei Bedarf gescrollt).
    pub(crate) body: Vec<Line<'static>>,
    /// Angeheftete Zeilen unten (Reihenfolge von oben nach unten).
    pub(crate) pinned: Vec<PinnedRow>,
}

/// Gerenderte Zeilenzahl von `lines` bei `width` (mit Umbruch).
#[must_use]
pub(crate) fn wrapped_rows(lines: &[Line<'static>], width: u16) -> usize {
    if lines.is_empty() || width == 0 {
        return 0;
    }
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

fn pinned_lines(rows: &[PinnedRow], with_padding: bool) -> Vec<Line<'static>> {
    rows.iter()
        .filter(|row| with_padding || !row.padding)
        .map(|row| row.line.clone())
        .collect()
}

impl DialogContent {
    /// Gesamthöhe inklusive Rahmen, bei der alles ohne Scrollen passt.
    ///
    /// # Argumente
    /// - `width`: Gesamtbreite des Fensters inklusive Rahmen.
    #[must_use]
    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        let inner = width.saturating_sub(2).max(1);
        let rows = wrapped_rows(&self.body, inner)
            + wrapped_rows(&pinned_lines(&self.pinned, true), inner);
        u16::try_from(rows.saturating_add(2)).unwrap_or(u16::MAX)
    }

    /// Ob der Körper bei dieser Innenfläche vollständig (ohne Scrollen und
    /// mit Polsterung) passt.
    #[must_use]
    pub(crate) fn fits(&self, inner: Rect) -> bool {
        let pinned = wrapped_rows(&pinned_lines(&self.pinned, true), inner.width);
        wrapped_rows(&self.body, inner.width) + pinned <= usize::from(inner.height)
    }
}

/// Wählt die längste Variante, die in `width` Spalten passt, sonst die
/// kürzeste (letzte).
#[must_use]
pub(crate) fn pick_fitting<'a>(variants: &[&'a str], width: u16) -> &'a str {
    variants
        .iter()
        .copied()
        .find(|text| UnicodeWidthStr::width(*text) <= usize::from(width))
        .or_else(|| variants.last().copied())
        .unwrap_or("")
}

/// Schreibt `text` in eine Zeile, aber nur innerhalb von `limit`.
fn put_str(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, limit: Rect) {
    if y < limit.top() || y >= limit.bottom() || x < limit.left() || x >= limit.right() {
        return;
    }
    let max = usize::from(limit.right() - x);
    let _ = buf.set_stringn(x, y, text, max, style);
}

/// Schreibt `text` rechtsbündig in die obere Rahmenzeile von `area`
/// (z. B. einen Countdown), sofern es neben `title_width` Spalten Titel passt.
pub(crate) fn render_top_right(
    area: Rect,
    buf: &mut Buffer,
    text: &str,
    title_width: usize,
    style: Style,
) {
    let area = area.intersection(buf.area);
    let text_width = UnicodeWidthStr::width(text);
    // Rahmenecken links/rechts plus Titel plus ein Trenner.
    if area.height == 0 || text_width + title_width + 3 > usize::from(area.width) {
        return;
    }
    let Ok(text_width) = u16::try_from(text_width) else {
        return;
    };
    let x = area.right().saturating_sub(1).saturating_sub(text_width);
    put_str(buf, x, area.top(), text, style, area);
}

/// Zeichnet `block` und darin `content` in `area`.
///
/// # Beschreibung
/// Siehe Moduldoku (Aufteilung, Abbau bei Platzmangel). Der Körper wird ab
/// `scroll.offset()` gezeigt; läuft er über, erscheint rechts auf dem
/// Rahmen eine Bildlaufleiste und unten ein Hinweis.
///
/// # Rückgabe
/// Die Innenfläche des Körpers (leer, wenn kein Körper sichtbar ist).
pub(crate) fn render_dialog(
    block: Block<'_>,
    area: Rect,
    buf: &mut Buffer,
    content: &DialogContent,
    scroll: &BodyScroll,
    hint_style: Style,
) -> Rect {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return Rect::default();
    }
    let height = usize::from(inner.height);
    let body_rows = wrapped_rows(&content.body, inner.width);
    let full = pinned_lines(&content.pinned, true);
    let compact = pinned_lines(&content.pinned, false);
    let full_rows = wrapped_rows(&full, inner.width);
    let compact_rows = wrapped_rows(&compact, inner.width);
    let body_min = body_rows.min(1);
    let (pinned, pinned_rows) = if full_rows + body_min <= height {
        (full, full_rows)
    } else {
        (compact, compact_rows)
    };
    let pinned_rows = pinned_rows.min(height);
    let body_height = height - pinned_rows;

    // Körper (scrollbar).
    let max_offset = body_rows.saturating_sub(body_height);
    scroll.set_limit(max_offset);
    let offset = scroll.offset();
    let body_area = Rect {
        height: u16::try_from(body_height).unwrap_or(inner.height),
        ..inner
    };
    if body_height > 0 && body_rows > 0 {
        Paragraph::new(content.body.clone())
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0))
            .render(body_area, buf);
    }

    // Angeheftete Zeilen unten.
    if pinned_rows > 0 {
        let pinned_height = u16::try_from(pinned_rows).unwrap_or(inner.height);
        let pinned_area = Rect {
            y: inner.bottom() - pinned_height,
            height: pinned_height,
            ..inner
        };
        Paragraph::new(pinned)
            .wrap(Wrap { trim: false })
            .render(pinned_area, buf);
    }

    // Überlauf: Bildlaufleiste auf dem rechten Rahmen, Hinweis unten.
    if max_offset > 0 && body_height > 0 {
        let bar_area = Rect {
            width: inner.width.saturating_add(1),
            ..body_area
        }
        .intersection(area);
        let mut state = ScrollbarState::new(max_offset.saturating_add(1))
            .position(offset)
            .viewport_content_length(body_height);
        StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            bar_area,
            buf,
            &mut state,
        );
        let hint = pick_fitting(
            &[SCROLL_HINT, SCROLL_HINT_SHORT],
            area.width.saturating_sub(4),
        );
        let hint_width = u16::try_from(UnicodeWidthStr::width(hint)).unwrap_or(u16::MAX);
        if hint_width + 2 <= area.width {
            let x = area.right().saturating_sub(2).saturating_sub(hint_width);
            put_str(buf, x, area.bottom() - 1, hint, hint_style, area);
        }
    }
    body_area
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::Borders;

    fn screen(buf: &Buffer) -> Vec<String> {
        let area = buf.area;
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn content(body_lines: usize) -> DialogContent {
        DialogContent {
            body: (0..body_lines)
                .map(|i| Line::from(format!("Körper-{i:02}")))
                .collect(),
            pinned: vec![
                PinnedRow::padding(),
                PinnedRow::content("1. Ja"),
                PinnedRow::content("2. Nein"),
                PinnedRow::padding(),
                PinnedRow::content("Hinweis"),
            ],
        }
    }

    #[test]
    fn pinned_rows_stay_visible_and_body_scrolls() {
        let content = content(20);
        let scroll = BodyScroll::default();
        let area = Rect::new(0, 0, 30, 10);
        let mut buf = Buffer::empty(area);
        render_dialog(
            Block::default().borders(Borders::ALL),
            area,
            &mut buf,
            &content,
            &scroll,
            Style::default(),
        );
        let shown = screen(&buf).join("\n");
        for needle in ["1. Ja", "2. Nein", "Hinweis", "Körper-00"] {
            assert!(shown.contains(needle), "{needle}: {shown}");
        }
        assert!(scroll.can_scroll());
        scroll.handle_wheel(MouseEventKind::ScrollDown);
        let mut buf = Buffer::empty(area);
        render_dialog(
            Block::default().borders(Borders::ALL),
            area,
            &mut buf,
            &content,
            &scroll,
            Style::default(),
        );
        let shown = screen(&buf).join("\n");
        assert!(!shown.contains("Körper-00"), "{shown}");
        assert!(shown.contains("Körper-03"), "{shown}");
        for needle in ["1. Ja", "2. Nein", "Hinweis"] {
            assert!(shown.contains(needle), "{needle}: {shown}");
        }
    }

    #[test]
    fn padding_is_dropped_before_the_body_disappears() {
        let content = content(3);
        let scroll = BodyScroll::default();
        // Innenhöhe 4: Optionen + Hinweis (3) + eine Körperzeile.
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        render_dialog(
            Block::default().borders(Borders::ALL),
            area,
            &mut buf,
            &content,
            &scroll,
            Style::default(),
        );
        let rows = screen(&buf);
        assert!(rows[1].contains("Körper-00"), "{rows:?}");
        assert!(rows[2].contains("1. Ja"), "{rows:?}");
        assert!(rows[4].contains("Hinweis"), "{rows:?}");
    }

    #[test]
    fn never_draws_outside_the_buffer() {
        let content = content(5);
        let scroll = BodyScroll::default();
        let buffer_area = Rect::new(0, 0, 20, 5);
        let mut buf = Buffer::empty(buffer_area);
        // Fläche ragt über den Puffer hinaus: wird geschnitten, keine Panik.
        render_dialog(
            Block::default().borders(Borders::ALL),
            Rect::new(5, 2, 40, 10),
            &mut buf,
            &content,
            &scroll,
            Style::default(),
        );
        assert_eq!(buf.area, buffer_area);
    }

    #[test]
    fn scroll_keys_are_ctrl_arrows_only() {
        let scroll = BodyScroll::default();
        let plain = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        let ctrl = KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL);
        assert!(!scroll.handle_key(&plain));
        assert!(scroll.handle_key(&ctrl));
    }

    #[test]
    fn pick_fitting_prefers_the_longest_variant() {
        assert_eq!(pick_fitting(&["lang lang", "kurz"], 20), "lang lang");
        assert_eq!(pick_fitting(&["lang lang", "kurz"], 5), "kurz");
        assert_eq!(pick_fitting(&["lang lang", "kurz"], 2), "kurz");
    }
}

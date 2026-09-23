//! Scroll-State-Verwaltung für die Chat-Historie-Ansicht.
//!
//! # Verantwortlichkeit
//! Dieses Modul kapselt ausschließlich den Scroll-Zustand der Chat-Ansicht.
//! Es enthält kein Rendering, keinen Zugriff auf History-Cells und keine
//! Kenntnis über den Inhalt der Nachrichten.
//!
//! Der Aufrufer besitzt die Cell-Liste und übergibt lediglich `total_lines`
//! (Gesamtzeilenanzahl) und `viewport` (sichtbare Höhe) für Clamping-Berechnungen.
//!
//! # Schlüsseltypen
//! - [`ChatScroll`] — das zentrale State-Struct (Copy)
//! - [`ScrollAction`] — Rückgabewert von Event-Handlern
//!
//! # Nebenläufigkeit
//! `ChatScroll` ist `Copy` und damit trivial zwischen Threads kopierbar.
//! Kein interner Mutex oder `Arc` notwendig.
//!
//! # Fehler
//! Dieses Modul produziert keine Fehler.
//!
//! # Beispiele
//! ```ignore
//! use harw_tui::chat_scroll::{ChatScroll, ScrollAction};
//!
//! let mut scroll = ChatScroll::new();
//! assert_eq!(scroll.offset(), 0);
//!
//! scroll.scroll_up(5, 100, 20);
//! assert_eq!(scroll.offset(), 5);
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

/// State der Chat-Scroll-Position.
///
/// `offset` ist die Anzahl Zeilen VOM ENDE aus gemessen:
/// - `offset = 0` → am neuesten Ende (Standard, Auto-Follow neuer Nachrichten).
/// - `offset = N` → N Zeilen zurückgescrollt.
///
/// Auto-Follow ist implizit: Solange `offset = 0` ist, zeigt die Ansicht stets
/// das Ende der Historie, auch wenn neue Zeilen hinzukommen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatScroll {
    offset: usize,
}

/// Ergebnis einer Event-Bearbeitung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAction {
    /// Der Event wurde konsumiert; UI muss neu gezeichnet werden.
    Redraw,
    /// Der Event gehört nicht zum Scroll (an anderen Handler weiterreichen).
    Passthrough,
}

impl ChatScroll {
    /// Erstellt einen neuen `ChatScroll` im Auto-Follow-Modus am Ende der Historie.
    ///
    /// # Description
    /// Der initiale Zustand ist `offset = 0` (am Ende, Auto-Follow aktiv).
    /// Entspricht dem typischen Verhalten bei einem frisch geöffneten Chat-Fenster.
    ///
    /// # Returns
    /// Eine neue `ChatScroll`-Instanz mit Standardwerten.
    ///
    /// # Examples
    /// ```ignore
    /// use harw_tui::chat_scroll::ChatScroll;
    /// let s = ChatScroll::new();
    /// assert_eq!(s.offset(), 0);
    /// ```
    pub fn new() -> Self {
        Self { offset: 0 }
    }

    /// Gibt den aktuellen Scroll-Offset zurück (Zeilen vom Ende gemessen).
    ///
    /// # Returns
    /// `usize` — Anzahl Zeilen, um die vom Ende zurückgescrollt wurde.
    ///
    /// # Examples
    /// ```ignore
    /// use harw_tui::chat_scroll::ChatScroll;
    /// let s = ChatScroll::new();
    /// assert_eq!(s.offset(), 0);
    /// ```
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Gibt `true` zurück, wenn der Nutzer aktuell am Ende der Historie ist (offset = 0),
    /// d. h. Auto-Follow aktiv ist.
    ///
    /// # Returns
    /// `true` wenn `offset == 0`.
    #[cfg(test)]
    fn is_at_tail(&self) -> bool {
        self.offset == 0
    }

    /// Scrollt um `lines` Zeilen nach oben (in Richtung älterer Nachrichten).
    ///
    /// # Description
    /// Erhöht den Offset um `lines`, begrenzt auf `total_lines.saturating_sub(viewport)`.
    /// Ein Offset > 0 beendet Auto-Follow.
    ///
    /// # Arguments
    /// - `lines` (`usize`): Anzahl Zeilen, um die nach oben gescrollt wird.
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    pub fn scroll_up(&mut self, lines: usize, total_lines: usize, viewport: usize) {
        let max_offset = total_lines.saturating_sub(viewport);
        self.offset = self.offset.saturating_add(lines).min(max_offset);
    }

    /// Scrollt um `lines` Zeilen nach unten (in Richtung neuerer Nachrichten).
    ///
    /// # Description
    /// Reduziert den Offset um `lines`. Erreicht der Offset 0, ist Auto-Follow wieder aktiv.
    ///
    /// # Arguments
    /// - `lines` (`usize`): Anzahl Zeilen, um die nach unten gescrollt wird.
    pub fn scroll_down(&mut self, lines: usize) {
        self.offset = self.offset.saturating_sub(lines);
    }

    /// Springt um eine Viewport-Höhe nach oben (Seite rauf).
    ///
    /// # Description
    /// Erhöht den Offset um `viewport` Zeilen, clamped auf das Maximum.
    ///
    /// # Arguments
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    pub fn page_up(&mut self, total_lines: usize, viewport: usize) {
        self.scroll_up(viewport, total_lines, viewport);
    }

    /// Springt um eine Viewport-Höhe nach unten (Seite runter).
    ///
    /// # Description
    /// Reduziert den Offset um `viewport` Zeilen.
    ///
    /// # Arguments
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    pub fn page_down(&mut self, viewport: usize) {
        self.scroll_down(viewport);
    }

    /// Springt an den Anfang der Historie (älteste Nachrichten).
    ///
    /// # Description
    /// Setzt den Offset auf den maximalen Wert `total_lines.saturating_sub(viewport)`.
    ///
    /// # Arguments
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    pub fn jump_to_top(&mut self, total_lines: usize, viewport: usize) {
        self.offset = total_lines.saturating_sub(viewport);
    }

    /// Springt an das Ende der Historie (neueste Nachrichten) und reaktiviert Auto-Follow.
    ///
    /// # Description
    /// Setzt Offset auf 0.
    pub fn jump_to_bottom(&mut self) {
        self.offset = 0;
    }

    /// Reagiert auf neue Inhalte in der Chat-Historie.
    ///
    /// # Description
    /// Wenn Auto-Follow aktiv ist (`offset = 0`), bleibt der Offset 0 (kein State-Change
    /// notwendig). Wenn der Nutzer manuell hochgescrollt hat (`offset > 0`), wird der
    /// Offset nicht verändert, damit die aktuelle Position erhalten bleibt.
    pub fn on_new_content(&mut self) {
        // Bei offset=0: Ansicht folgt bereits dem Ende, kein State-Change nötig.
        // Bei offset>0: Nutzer hat manuell gescrollt — offset bleibt unverändert.
    }

    /// Erzwingt Auto-Follow, unabhängig vom aktuellen Zustand.
    ///
    /// # Description
    /// Setzt `offset = 0`. Sinnvoll nach dem Absenden einer Nachricht
    /// (Enter/Submit), damit die neue Antwort direkt sichtbar ist.
    pub fn force_follow(&mut self) {
        self.offset = 0;
    }

    /// Berechnet den sichtbaren Zeilen-Range `[start, end)` für das Rendering.
    ///
    /// # Description
    /// - `end = total_lines - offset` (clamped auf `[0, total_lines]`)
    /// - `start = end.saturating_sub(viewport)`
    ///
    /// Der Aufrufer kann `lines[start..end]` direkt rendern.
    ///
    /// # Arguments
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    ///
    /// # Returns
    /// `(start, end)` als `(usize, usize)` wobei `start <= end <= total_lines`.
    #[cfg(test)]
    fn visible_range(&self, total_lines: usize, viewport: usize) -> (usize, usize) {
        let end = total_lines.saturating_sub(self.offset);
        let start = end.saturating_sub(viewport);
        (start, end)
    }

    /// Reagiert auf einen Tastatur-Event und aktualisiert den Scroll-Zustand entsprechend.
    ///
    /// # Description
    /// Konsumierte Keys:
    /// - `PageUp` → [`page_up`](ChatScroll::page_up)
    /// - `PageDown` → [`page_down`](ChatScroll::page_down)
    /// - `Shift+Up` → [`scroll_up`](ChatScroll::scroll_up) um 1
    /// - `Shift+Down` → [`scroll_down`](ChatScroll::scroll_down) um 1
    /// - `Ctrl+Home` → [`jump_to_top`](ChatScroll::jump_to_top)
    /// - `Ctrl+End` → [`jump_to_bottom`](ChatScroll::jump_to_bottom)
    /// - Alle anderen Keys → [`ScrollAction::Passthrough`]
    ///
    /// # Arguments
    /// - `key` (`KeyEvent`): Das zu verarbeitende Tastatur-Event.
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    ///
    /// # Returns
    /// [`ScrollAction::Redraw`] wenn der Event konsumiert wurde,
    /// [`ScrollAction::Passthrough`] wenn der Event weitergeleitet werden soll.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        total_lines: usize,
        viewport: usize,
    ) -> ScrollAction {
        match key.code {
            KeyCode::PageUp => {
                self.page_up(total_lines, viewport);
                ScrollAction::Redraw
            }
            KeyCode::PageDown => {
                self.page_down(viewport);
                ScrollAction::Redraw
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_up(1, total_lines, viewport);
                ScrollAction::Redraw
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_down(1);
                ScrollAction::Redraw
            }
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.jump_to_top(total_lines, viewport);
                ScrollAction::Redraw
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.jump_to_bottom();
                ScrollAction::Redraw
            }
            _ => ScrollAction::Passthrough,
        }
    }

    /// Reagiert auf ein Maus-Event (Mausrad) und aktualisiert den Scroll-Zustand.
    ///
    /// # Description
    /// - `MouseEventKind::ScrollUp` → [`scroll_up`](ChatScroll::scroll_up) um 3 Zeilen
    /// - `MouseEventKind::ScrollDown` → [`scroll_down`](ChatScroll::scroll_down) um 3 Zeilen
    /// - Andere Events → [`ScrollAction::Passthrough`]
    ///
    /// # Arguments
    /// - `ev` (`MouseEvent`): Das zu verarbeitende Maus-Event.
    /// - `total_lines` (`usize`): Gesamtanzahl aller Zeilen in der Historie.
    /// - `viewport` (`usize`): Höhe des sichtbaren Bereichs in Zeilen.
    ///
    /// # Returns
    /// [`ScrollAction::Redraw`] wenn das Event konsumiert wurde,
    /// [`ScrollAction::Passthrough`] sonst.
    pub fn handle_mouse(
        &mut self,
        ev: MouseEvent,
        total_lines: usize,
        viewport: usize,
    ) -> ScrollAction {
        match ev.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_up(3, total_lines, viewport);
                ScrollAction::Redraw
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(3);
                ScrollAction::Redraw
            }
            _ => ScrollAction::Passthrough,
        }
    }
}

impl Default for ChatScroll {
    /// Erstellt eine `ChatScroll`-Instanz mit Standardwerten (am Ende, Auto-Follow aktiv).
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_new_starts_at_tail() {
        let s = ChatScroll::new();
        assert!(s.is_at_tail(), "Neuer Scroll muss am Ende sein");
        assert!(s.is_at_tail(), "Auto-Follow muss initial aktiv sein");
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn test_scroll_up_leaves_tail() {
        let mut s = ChatScroll::new();
        s.scroll_up(5, 100, 20);
        assert_eq!(s.offset(), 5);
        assert!(
            !s.is_at_tail(),
            "Nach scroll_up darf Auto-Follow nicht aktiv sein"
        );
        assert!(!s.is_at_tail());
    }

    #[test]
    fn test_scroll_down_returns_to_tail() {
        let mut s = ChatScroll::new();
        s.scroll_up(5, 100, 20);
        s.scroll_down(5);
        assert!(
            s.is_at_tail(),
            "Nach scroll_down auf 0 muss is_at_tail true sein"
        );
        assert!(
            s.is_at_tail(),
            "Nach Rückkehr an Tail muss Auto-Follow wieder aktiv sein"
        );
    }

    #[test]
    fn test_page_up_moves_by_viewport() {
        let mut s = ChatScroll::new();
        s.page_up(100, 20);
        assert_eq!(s.offset(), 20, "page_up muss um Viewport-Höhe scrollen");
    }

    #[test]
    fn test_page_down_reduces_offset_by_viewport() {
        let mut s = ChatScroll::new();
        s.scroll_up(30, 100, 20);
        s.page_down(20);
        assert_eq!(
            s.offset(),
            10,
            "page_down muss Offset um Viewport-Höhe reduzieren"
        );
    }

    #[test]
    fn test_scroll_up_clamps_at_max() {
        let mut s = ChatScroll::new();
        // Versuche über das Maximum zu scrollen: total=100, viewport=20 → max=80
        s.scroll_up(200, 100, 20);
        assert_eq!(
            s.offset(),
            80,
            "offset darf total_lines - viewport nicht überschreiten"
        );
    }

    #[test]
    fn test_jump_to_top() {
        let mut s = ChatScroll::new();
        s.jump_to_top(100, 20);
        assert_eq!(
            s.offset(),
            80,
            "jump_to_top muss offset = total - viewport setzen"
        );
        assert!(!s.is_at_tail());
    }

    #[test]
    fn test_jump_to_bottom_resets() {
        let mut s = ChatScroll::new();
        s.scroll_up(30, 100, 20);
        s.jump_to_bottom();
        assert_eq!(s.offset(), 0, "jump_to_bottom muss offset auf 0 setzen");
        assert!(
            s.is_at_tail(),
            "jump_to_bottom muss Auto-Follow reaktivieren"
        );
    }

    #[test]
    fn test_on_new_content_when_following_stays_at_tail() {
        let mut s = ChatScroll::new();
        assert!(s.is_at_tail());
        s.on_new_content();
        assert_eq!(
            s.offset(),
            0,
            "Bei Auto-Follow muss on_new_content offset bei 0 halten"
        );
        assert!(s.is_at_tail());
    }

    #[test]
    fn test_on_new_content_when_scrolled_up_preserves_offset() {
        let mut s = ChatScroll::new();
        s.scroll_up(5, 100, 20);
        assert_eq!(s.offset(), 5);
        assert!(!s.is_at_tail());
        s.on_new_content();
        assert_eq!(
            s.offset(),
            5,
            "on_new_content darf bei Auto-Follow aus den Offset nicht ändern"
        );
        assert!(!s.is_at_tail());
    }

    #[test]
    fn test_visible_range_at_tail() {
        let s = ChatScroll::new();
        let (start, end) = s.visible_range(100, 20);
        assert_eq!((start, end), (80, 100));
    }

    #[test]
    fn test_visible_range_scrolled_up() {
        let mut s = ChatScroll::new();
        s.scroll_up(10, 100, 20);
        let (start, end) = s.visible_range(100, 20);
        assert_eq!((start, end), (70, 90));
    }

    #[test]
    fn test_visible_range_smaller_than_viewport() {
        let s = ChatScroll::new();
        let (start, end) = s.visible_range(5, 20);
        assert_eq!((start, end), (0, 5));
    }

    #[test]
    fn test_handle_key_pageup_consumes() {
        let mut s = ChatScroll::new();
        let action = s.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE), 100, 20);
        assert_eq!(action, ScrollAction::Redraw);
        assert!(s.offset() > 0, "PageUp muss offset erhöhen");
    }

    #[test]
    fn test_handle_key_unknown_passthrough() {
        let mut s = ChatScroll::new();
        let action = s.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE), 100, 20);
        assert_eq!(action, ScrollAction::Passthrough);
        assert_eq!(
            s.offset(),
            0,
            "Unbekannte Keys dürfen den State nicht ändern"
        );
    }

    #[test]
    fn test_handle_mouse_wheel_up() {
        let mut s = ChatScroll::new();
        let action = s.handle_mouse(mouse(MouseEventKind::ScrollUp), 100, 20);
        assert_eq!(action, ScrollAction::Redraw);
        assert_eq!(s.offset(), 3, "ScrollUp soll offset um 3 erhöhen");
    }

    #[test]
    fn test_force_follow_resets_from_scrolled_state() {
        let mut s = ChatScroll::new();
        s.scroll_up(5, 100, 20);
        assert_eq!(s.offset(), 5);
        assert!(!s.is_at_tail());
        s.force_follow();
        assert_eq!(s.offset(), 0, "force_follow muss offset auf 0 setzen");
        assert!(s.is_at_tail(), "force_follow muss Auto-Follow reaktivieren");
    }
}

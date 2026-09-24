//! Scroll-State-Verwaltung für die Chat-Historie-Ansicht.
//!
//! # Verantwortlichkeit
//! Dieses Modul kapselt ausschließlich den Scroll-Zustand der Chat-Ansicht
//! (und der Agenten-Detailspur, die dasselbe Modell nutzt).
//! Es enthält kein Rendering, keinen Zugriff auf History-Cells und keine
//! Kenntnis über den Inhalt der Nachrichten.
//!
//! Der Aufrufer besitzt die Cell-Liste und übergibt lediglich `total_lines`
//! (Gesamtzeilenanzahl) und `viewport` (sichtbare Höhe) für Clamping-Berechnungen.
//!
//! # „Folgen nur am Ende"
//! - Steht die Ansicht am Ende (`offset = 0`), folgt sie neuem Inhalt.
//! - Hat der Nutzer hochgescrollt (`offset > 0`), bleibt der sichtbare Text
//!   stehen: [`ChatScroll::sync_layout`] wird bei jedem Zeichnen mit der
//!   aktuellen Gesamtzeilenzahl aufgerufen und erhöht den Offset (gemessen vom
//!   Ende) um genau die Zahl der unten angehängten Zeilen. Gleichzeitig
//!   zählt es diese Zeilen als „neu, ungesehen" für den Hinweis
//!   [`ChatScroll::indicator_text`].
//! - Ändert sich nur die Viewport-Höhe (Freigabe-Dialog ersetzt den Composer,
//!   Terminal-Resize), bleibt beim Lesen die **oberste** sichtbare Zeile stehen.
//! - Eigenes Absenden ([`ChatScroll::force_follow`]) und
//!   [`ChatScroll::jump_to_bottom`] springen ans Ende und folgen wieder.
//!
//! # Schlüsseltypen
//! - [`ChatScroll`] — das zentrale State-Struct
//! - [`ScrollAction`] — Rückgabewert von Event-Handlern
//!
//! # Nebenläufigkeit
//! Der Zustand liegt in [`Cell`]s, damit das Rendering (das nur `&ChatApp`
//! sieht) den Anker nachführen kann. `ChatScroll` ist damit `Send`, aber
//! nicht `Sync` — wie der übrige TUI-Zustand, der nur im UI-Thread lebt.
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

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

/// Zuletzt beim Zeichnen gemessene Geometrie des Scroll-Bereichs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measure {
    /// Gesamtzeilen (nach Umbruch).
    total: usize,
    /// Sichtbare Zeilen.
    viewport: usize,
    /// Breite, mit der umbrochen wurde.
    width: u16,
}

/// State der Chat-Scroll-Position.
///
/// `offset` ist die Anzahl Zeilen VOM ENDE aus gemessen:
/// - `offset = 0` → am neuesten Ende (Standard, Auto-Follow neuer Nachrichten).
/// - `offset = N` → N Zeilen zurückgescrollt; der sichtbare Text bleibt bei
///   neuem Inhalt stehen (siehe [`ChatScroll::sync_layout`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatScroll {
    /// Zeilen vom Ende.
    offset: Cell<usize>,
    /// Seit dem Hochscrollen unten angehängte, noch nicht sichtbare Zeilen.
    unseen: Cell<usize>,
    /// Geometrie des letzten [`ChatScroll::sync_layout`]-Aufrufs.
    last: Cell<Option<Measure>>,
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
    /// # Returns
    /// Eine neue `ChatScroll`-Instanz mit Standardwerten (`offset = 0`).
    pub fn new() -> Self {
        Self {
            offset: Cell::new(0),
            unseen: Cell::new(0),
            last: Cell::new(None),
        }
    }

    /// Gibt den aktuellen Scroll-Offset zurück (Zeilen vom Ende gemessen).
    pub fn offset(&self) -> usize {
        self.offset.get()
    }

    /// `true`, solange die Ansicht am Ende steht und neuem Inhalt folgt.
    pub fn is_following(&self) -> bool {
        self.offset.get() == 0
    }

    /// Anzahl der seit dem Hochscrollen unten angehängten Zeilen, die noch
    /// unterhalb des sichtbaren Bereichs liegen (0 beim Folgen).
    pub fn unseen_lines(&self) -> usize {
        self.unseen.get()
    }

    /// Test-Alias für [`ChatScroll::is_following`].
    #[cfg(test)]
    fn is_at_tail(&self) -> bool {
        self.is_following()
    }

    /// Setzt den Offset und hält die Invariante `unseen <= offset`
    /// (ungesehen kann nur sein, was unterhalb der Ansicht liegt).
    fn set_offset(&self, offset: usize) {
        self.offset.set(offset);
        self.unseen.set(self.unseen.get().min(offset));
    }

    /// Größter sinnvoller Offset laut letzter Messung; ohne Messung
    /// unbegrenzt (das nächste [`ChatScroll::sync_layout`] kappt).
    fn cached_max_offset(&self) -> usize {
        self.last
            .get()
            .map_or(usize::MAX, |m| m.total.saturating_sub(m.viewport))
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
        self.set_offset(self.offset.get().saturating_add(lines).min(max_offset));
    }

    /// Wie [`ChatScroll::scroll_up`], aber gegen die zuletzt beim Zeichnen
    /// gemessene Geometrie begrenzt (für Ansichten, deren Tasten-Handler die
    /// Zeilenzahl nicht kennen, z. B. die Agenten-Detailspur).
    pub fn scroll_up_measured(&mut self, lines: usize) {
        let max_offset = self.cached_max_offset();
        self.set_offset(self.offset.get().saturating_add(lines).min(max_offset));
    }

    /// Scrollt um `lines` Zeilen nach unten (in Richtung neuerer Nachrichten).
    /// Erreicht der Offset 0, ist Auto-Follow wieder aktiv.
    pub fn scroll_down(&mut self, lines: usize) {
        self.set_offset(self.offset.get().saturating_sub(lines));
    }

    /// Springt um eine Viewport-Höhe nach oben (Seite rauf).
    pub fn page_up(&mut self, total_lines: usize, viewport: usize) {
        self.scroll_up(viewport, total_lines, viewport);
    }

    /// Springt um eine Viewport-Höhe nach unten (Seite runter).
    pub fn page_down(&mut self, viewport: usize) {
        self.scroll_down(viewport);
    }

    /// Springt an den Anfang der Historie (älteste Nachrichten).
    pub fn jump_to_top(&mut self, total_lines: usize, viewport: usize) {
        self.set_offset(total_lines.saturating_sub(viewport));
    }

    /// Wie [`ChatScroll::jump_to_top`] mit der zuletzt gemessenen Geometrie.
    pub fn jump_to_top_measured(&mut self) {
        self.set_offset(self.cached_max_offset());
    }

    /// Springt an das Ende der Historie (neueste Nachrichten) und reaktiviert Auto-Follow.
    pub fn jump_to_bottom(&mut self) {
        self.set_offset(0);
    }

    /// Frühere Benachrichtigung über neuen Inhalt — bewusst ein No-Op.
    ///
    /// # Description
    /// Der Anker wird nicht beim Anhängen, sondern beim Zeichnen in
    /// [`ChatScroll::sync_layout`] nachgeführt: nur dort ist die umbrochene
    /// Zeilenzahl bekannt, und so sind auch Inhalte erfasst, die ohne
    /// Anhänge-Aufruf wachsen (Streaming-Deltas, geteilte Zellen,
    /// Werkzeugergebnisse, Kind-Agenten-Updates).
    pub fn on_new_content(&mut self) {}

    /// Erzwingt Auto-Follow, unabhängig vom aktuellen Zustand.
    ///
    /// # Description
    /// Setzt `offset = 0`. Nach dem Absenden einer eigenen Nachricht
    /// (Enter/Submit), damit die neue Antwort direkt sichtbar ist.
    pub fn force_follow(&mut self) {
        self.set_offset(0);
    }

    /// Führt den Anker beim Zeichnen nach und liefert den gültigen Offset.
    ///
    /// # Description
    /// Vergleicht die aktuelle Geometrie mit der des letzten Aufrufs:
    /// - **Folgen** (`offset = 0`): nichts zu tun, die Ansicht bleibt am Ende.
    /// - **Hochgescrollt**, gleiche Breite: `offset += total - vorher_total`
    ///   (Wachstum unten wird als ungesehen gezählt; Schrumpfen unten zieht
    ///   den Offset nach, ohne dass Folgen stillschweigend wieder angeht).
    /// - **Hochgescrollt**, andere Breite (Neuumbruch): Offset proportional
    ///   skaliert — eine exakte Zeilen-Zuordnung gibt es nach Umbruch nicht.
    /// - **Hochgescrollt**, andere Viewport-Höhe (Freigabe-Dialog, Resize):
    ///   die oberste sichtbare Zeile bleibt stehen.
    ///
    /// Abschließend wird auf `total.saturating_sub(viewport)` gekappt; passt
    /// alles in den Viewport, folgt die Ansicht wieder.
    ///
    /// # Arguments
    /// - `total_lines` (`usize`): aktuelle Gesamtzeilen (nach Umbruch).
    /// - `viewport` (`usize`): aktuell sichtbare Zeilen.
    /// - `width` (`u16`): Umbruchbreite.
    ///
    /// # Returns
    /// Den Offset vom Ende, mit dem jetzt gezeichnet werden soll.
    pub fn sync_layout(&self, total_lines: usize, viewport: usize, width: u16) -> usize {
        let now = Measure {
            total: total_lines,
            viewport,
            width,
        };
        let prev = self.last.replace(Some(now));
        let mut offset = self.offset.get();
        let mut unseen = self.unseen.get();
        if offset > 0
            && let Some(prev) = prev
        {
            if prev.width == width {
                if total_lines >= prev.total {
                    let grown = total_lines - prev.total;
                    offset = offset.saturating_add(grown);
                    unseen = unseen.saturating_add(grown);
                } else {
                    let shrunk = prev.total - total_lines;
                    offset = offset.saturating_sub(shrunk).max(1);
                    unseen = unseen.saturating_sub(shrunk);
                }
            } else if prev.total > 0 {
                let scale = |value: usize| -> usize {
                    let scaled = (value as u128) * (total_lines as u128) / (prev.total as u128);
                    usize::try_from(scaled).unwrap_or(usize::MAX)
                };
                offset = scale(offset).max(1);
                unseen = scale(unseen);
            }
            // Oberste Zeile halten: start = total - offset - viewport.
            if viewport < prev.viewport {
                offset = offset.saturating_add(prev.viewport - viewport);
            } else {
                offset = offset.saturating_sub(viewport - prev.viewport).max(1);
            }
        }
        let max_offset = total_lines.saturating_sub(viewport);
        offset = offset.min(max_offset);
        self.offset.set(offset);
        self.unseen.set(unseen.min(offset));
        offset
    }

    /// Hinweistext, solange die Ansicht nicht folgt.
    ///
    /// # Arguments
    /// - `jump_key` (`&str`): Anzeigename der Taste, die ans Ende springt
    ///   (Chat: `"Strg+Ende"`, Agenten-Detail: `"G"`).
    ///
    /// # Returns
    /// `None` beim Folgen, sonst z. B. `"↓ 12 neue Zeilen · Strg+Ende springt ans Ende"`.
    pub fn indicator_text(&self, jump_key: &str) -> Option<String> {
        if self.is_following() {
            return None;
        }
        let head = match self.unseen_lines() {
            0 => "↑ hochgescrollt".to_owned(),
            1 => "↓ 1 neue Zeile".to_owned(),
            n => format!("↓ {n} neue Zeilen"),
        };
        Some(format!("{head} · {jump_key} springt ans Ende"))
    }

    /// Berechnet den sichtbaren Zeilen-Range `[start, end)` für das Rendering.
    #[cfg(test)]
    fn visible_range(&self, total_lines: usize, viewport: usize) -> (usize, usize) {
        let end = total_lines.saturating_sub(self.offset.get());
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
    /// - `End` (ohne Modifier) → [`jump_to_bottom`](ChatScroll::jump_to_bottom),
    ///   aber nur solange hochgescrollt ist (sonst Passthrough an den Composer)
    /// - Alle anderen Keys → [`ScrollAction::Passthrough`]
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
            KeyCode::End if key.modifiers.is_empty() && !self.is_following() => {
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

    // ── Folgen nur am Ende (sync_layout) ────────────────────────────────

    /// Erste sichtbare Zeile (Index von oben) für die aktuelle Geometrie.
    fn first_visible(s: &ChatScroll, total: usize, viewport: usize) -> usize {
        s.visible_range(total, viewport).0
    }

    #[test]
    fn append_while_scrolled_up_keeps_first_visible_line() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(10, 100, 20);
        let before = first_visible(&s, 100, 20);
        assert_eq!(before, 70);

        // Streaming/Werkzeugergebnisse hängen unten 7 Zeilen an.
        let offset = s.sync_layout(107, 20, 80);
        assert_eq!(offset, 17, "Offset vom Ende wächst um die neuen Zeilen");
        assert_eq!(
            first_visible(&s, 107, 20),
            before,
            "sichtbarer Text darf sich beim Anhängen nicht verschieben"
        );
        // Mehrere Frames hintereinander (Token für Token) bleiben stabil.
        s.sync_layout(108, 20, 80);
        s.sync_layout(130, 20, 80);
        assert_eq!(first_visible(&s, 130, 20), before);
        assert!(!s.is_following());
    }

    #[test]
    fn append_at_bottom_follows() {
        let s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        assert_eq!(s.sync_layout(150, 20, 80), 0);
        assert!(s.is_following());
        assert_eq!(s.visible_range(150, 20), (130, 150));
        assert_eq!(s.unseen_lines(), 0);
        assert_eq!(s.indicator_text("Strg+Ende"), None);
    }

    #[test]
    fn user_submit_resets_to_follow() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(30, 100, 20);
        s.sync_layout(120, 20, 80);
        assert!(!s.is_following());
        assert_eq!(s.unseen_lines(), 20);

        s.force_follow();
        assert!(s.is_following());
        assert_eq!(s.unseen_lines(), 0);
        assert_eq!(s.indicator_text("Strg+Ende"), None);
        // Danach folgt die Ansicht wieder neuem Inhalt.
        assert_eq!(s.sync_layout(140, 20, 80), 0);
    }

    #[test]
    fn indicator_counts_new_lines() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(10, 100, 20);
        assert_eq!(
            s.indicator_text("Strg+Ende").as_deref(),
            Some("↑ hochgescrollt · Strg+Ende springt ans Ende")
        );
        s.sync_layout(101, 20, 80);
        assert_eq!(
            s.indicator_text("Strg+Ende").as_deref(),
            Some("↓ 1 neue Zeile · Strg+Ende springt ans Ende")
        );
        s.sync_layout(113, 20, 80);
        assert_eq!(s.unseen_lines(), 13);
        assert_eq!(
            s.indicator_text("G").as_deref(),
            Some("↓ 13 neue Zeilen · G springt ans Ende")
        );
        // Herunterscrollen macht neue Zeilen sichtbar: höchstens so viele
        // bleiben ungesehen, wie unter der Ansicht liegen.
        s.scroll_down(15);
        assert_eq!(s.offset(), 8);
        assert_eq!(s.unseen_lines(), 8);
        s.jump_to_bottom();
        assert_eq!(s.unseen_lines(), 0);
        assert_eq!(s.indicator_text("G"), None);
    }

    #[test]
    fn viewport_shrink_keeps_top_line_while_reading() {
        // Ein Freigabe-Dialog ersetzt den Composer und verkleinert den
        // Verlauf von 20 auf 12 Zeilen: die oberste Zeile bleibt stehen.
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(10, 100, 20);
        let top = first_visible(&s, 100, 20);
        s.sync_layout(100, 12, 80);
        assert_eq!(first_visible(&s, 100, 12), top);
        // Dialog schließt: wieder derselbe Ausschnitt, Folgen bleibt aus.
        s.sync_layout(100, 20, 80);
        assert_eq!(first_visible(&s, 100, 20), top);
        assert_eq!(s.offset(), 10);
    }

    #[test]
    fn viewport_shrink_while_following_stays_at_bottom() {
        let s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        assert_eq!(s.sync_layout(100, 12, 80), 0);
        assert!(s.is_following());
    }

    #[test]
    fn shrink_below_view_does_not_silently_resume_following() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(2, 100, 20);
        // Live-Stream-Zeilen verschwinden (z. B. Cursorzeile `▍`).
        s.sync_layout(95, 20, 80);
        assert!(!s.is_following());
        assert_eq!(s.offset(), 1);
    }

    #[test]
    fn content_fitting_viewport_resumes_following() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(10, 100, 20);
        // `/clear` o. Ä.: alles passt in den Viewport.
        assert_eq!(s.sync_layout(5, 20, 80), 0);
        assert!(s.is_following());
    }

    #[test]
    fn width_change_scales_offset_and_clamps() {
        let mut s = ChatScroll::new();
        s.sync_layout(100, 20, 80);
        s.scroll_up(40, 100, 20);
        // Halbe Breite → doppelt so viele umbrochene Zeilen.
        assert_eq!(s.sync_layout(200, 20, 40), 80);
        assert!(!s.is_following());
    }

    #[test]
    fn plain_end_jumps_only_when_scrolled_up() {
        let mut s = ChatScroll::new();
        assert_eq!(
            s.handle_key(key(KeyCode::End, KeyModifiers::NONE), 100, 20),
            ScrollAction::Passthrough,
            "am Ende gehört End dem Composer"
        );
        s.scroll_up(5, 100, 20);
        assert_eq!(
            s.handle_key(key(KeyCode::End, KeyModifiers::NONE), 100, 20),
            ScrollAction::Redraw
        );
        assert!(s.is_following());
    }

    #[test]
    fn measured_helpers_clamp_against_last_layout() {
        let mut s = ChatScroll::new();
        // Ohne Messung unbegrenzt; das nächste Zeichnen kappt.
        s.scroll_up_measured(500);
        assert_eq!(s.sync_layout(100, 20, 80), 80);
        s.jump_to_bottom();
        s.scroll_up_measured(500);
        assert_eq!(s.offset(), 80);
        s.jump_to_bottom();
        s.jump_to_top_measured();
        assert_eq!(s.offset(), 80);
    }
}

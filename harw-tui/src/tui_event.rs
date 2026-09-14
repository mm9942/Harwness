//! Terminal-Eingabe-Ereignisse für die harw-tui.
//!
//! Dieses Modul definiert [`TuiEvent`], den zentralen Enum für alle
//! Terminaleingaben, die der async Event-Loop verarbeitet (Spec-Abschnitt 2.3,
//! SLICE 3). Der Enum bündelt Key-Presses, bracketed-Paste-Inhalte,
//! Resize-Signale und Frame-Draw-Anfragen in einem einzigen Typ.
//!
//! # Verantwortung
//! - Abbildung von [`crossterm::event::Event`] auf [`TuiEvent`] via
//!   [`from_crossterm`].
//! - `TuiEvent::Draw` wird **nicht** aus crossterm abgeleitet; es entsteht
//!   ausschließlich im `FrameRequester` (Abschnitt 2.5 der Spec).
//!
//! # Schlüsseltypen
//! - [`TuiEvent`] — Haupt-Enum
//! - [`from_crossterm`] — Konvertierungsfunktion
//!
//! # Nebenläufigkeit
//! `TuiEvent` ist `Send + Sync` (alle Felder sind es). Der Typ wird über einen
//! `tokio::sync::mpsc::UnboundedSender<TuiEvent>` an den Event-Loop übergeben.
//!
//! # Fehler
//! Dieses Modul erzeugt keine eigenen Fehler.
//!
//! # Beispiele
//! ```ignore
//! use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
//! use harw_tui::tui_event::{TuiEvent, from_crossterm};
//!
//! let key_event = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
//! let ct_event = Event::Key(KeyEvent { kind: KeyEventKind::Press, ..key_event });
//! assert!(matches!(from_crossterm(ct_event), Some(TuiEvent::Key(_))));
//! ```

use crossterm::event::{Event, KeyEventKind, MouseEventKind};

/// Alle Terminaleingabe-Ereignisse, die der async Event-Loop von harw-tui verarbeitet.
///
/// # Beschreibung
/// `TuiEvent` vereinheitlicht die vier Ereignisquellen des Select-Loops
/// (Spec-Abschnitt 2.1):
/// - Tastatureingaben via [`crossterm::event::KeyEvent`]
/// - Eingefügter Text via bracketed paste
/// - Terminalgröße-Änderungen
/// - Frame-Draw-Anfragen vom `FrameRequester`
///
/// Die Variante `Draw` entsteht **nicht** aus crossterm, sondern wird vom
/// `FrameRequester` über einen `mpsc::UnboundedSender<()>` ausgelöst.
///
/// # Concurrency
/// Implementiert `Send + Sync`; geeignet für `tokio::sync::mpsc`-Kanäle.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::tui_event::TuiEvent;
/// use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
///
/// let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
/// let event = TuiEvent::Key(key);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TuiEvent {
    /// Eine Tastatureingabe (nur `Press` und `Repeat`; `Release` wird gefiltert).
    Key(crossterm::event::KeyEvent),

    /// Text, der per Bracketed-Paste eingefügt wurde (z. B. API-Key via Ctrl+V).
    ///
    /// Voraussetzung: `crossterm::event::EnableBracketedPaste` muss im
    /// `TerminalGuard::enter()` aktiviert worden sein (Spec-Abschnitt 2.4).
    Paste(String),

    /// Das Terminal wurde in seiner Größe verändert.
    ///
    /// `w` = neue Breite in Spalten, `h` = neue Höhe in Zeilen.
    Resize(u16, u16),

    /// Ein Mausereignis, gebraucht für das Rad-Scrollen der Chat-Historie.
    ///
    /// Voraussetzung: `crossterm::event::EnableMouseCapture` muss im
    /// `TerminalGuard::enter()` aktiviert worden sein. Ohne Capture meldet das
    /// Terminal Radbewegungen nicht an die Anwendung.
    Mouse(crossterm::event::MouseEvent),

    /// Ein Frame soll gezeichnet werden.
    ///
    /// Diese Variante wird **nicht** von [`from_crossterm`] erzeugt. Sie wird
    /// ausschließlich vom `FrameRequester` (Spec-Abschnitt 2.5) via
    /// `mpsc::UnboundedSender<()>` ausgelöst und dann vom Event-Stream-Task
    /// als `TuiEvent::Draw` weitergeleitet.
    Draw,
}

/// Bildet ein [`crossterm::event::Event`] auf ein [`TuiEvent`] ab.
///
/// # Beschreibung
/// Diese Funktion filtert und konvertiert crossterm-Roh-Ereignisse:
/// - `Event::Key` mit `KeyEventKind::Press` oder `KeyEventKind::Repeat`
///   → `Some(TuiEvent::Key(...))`
/// - `Event::Key` mit `KeyEventKind::Release` → `None` (ignoriert)
/// - `Event::Paste(text)` → `Some(TuiEvent::Paste(text))`
/// - Rad-Ereignisse (`MouseEventKind::ScrollUp`/`ScrollDown`) → `Some(TuiEvent::Mouse(..))`
/// - `Event::Resize(w, h)` → `Some(TuiEvent::Resize(w, h))`
/// - Alle anderen Ereignisse (z. B. `FocusGained`, `FocusLost`, Klicks und Bewegungen) → `None`
///
/// `TuiEvent::Draw` kann über diese Funktion **nicht** erzeugt werden; es entsteht
/// ausschließlich im `FrameRequester`-Pfad (Spec-Abschnitt 2.3 und 2.5).
///
/// # Argumente
/// - `event` (`crossterm::event::Event`): Das rohe crossterm-Ereignis.
///
/// # Rückgabe
/// `Some(TuiEvent)` bei relevanten Ereignissen, `None` bei ignorierten.
///
/// # Fehler
/// Keine — die Funktion ist infallibel.
///
/// # Concurrency
/// Reine Funktion, thread-sicher ohne Locks.
///
/// # Beispiele
/// ```ignore
/// use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
/// use harw_tui::tui_event::{TuiEvent, from_crossterm};
///
/// let press = Event::Key(KeyEvent {
///     code: KeyCode::Char('x'),
///     modifiers: KeyModifiers::empty(),
///     kind: KeyEventKind::Press,
///     state: crossterm::event::KeyEventState::empty(),
/// });
/// assert!(matches!(from_crossterm(press), Some(TuiEvent::Key(_))));
///
/// let release = Event::Key(KeyEvent {
///     code: KeyCode::Char('x'),
///     modifiers: KeyModifiers::empty(),
///     kind: KeyEventKind::Release,
///     state: crossterm::event::KeyEventState::empty(),
/// });
/// assert_eq!(from_crossterm(release), None);
/// ```
pub(crate) fn from_crossterm(event: Event) -> Option<TuiEvent> {
    match event {
        Event::Key(key_event) => match key_event.kind {
            KeyEventKind::Press | KeyEventKind::Repeat => Some(TuiEvent::Key(key_event)),
            KeyEventKind::Release => None,
        },
        Event::Mouse(mouse_event)
            if matches!(
                mouse_event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) =>
        {
            Some(TuiEvent::Mouse(mouse_event))
        }
        Event::Mouse(_) => None,
        Event::Paste(text) => Some(TuiEvent::Paste(text)),
        Event::Resize(w, h) => Some(TuiEvent::Resize(w, h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent,
        MouseEventKind,
    };

    use super::*;

    fn make_key_event(kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
            kind,
            state: KeyEventState::empty(),
        })
    }

    /// Key-Press-Ereignis wird als `TuiEvent::Key` weitergeleitet.
    #[test]
    fn test_key_press_maps_to_some_key() {
        let event = make_key_event(KeyEventKind::Press);
        let result = from_crossterm(event);
        assert!(
            matches!(result, Some(TuiEvent::Key(k)) if k.code == KeyCode::Char('a')),
            "Key-Press muss als Some(TuiEvent::Key) zurückgegeben werden"
        );
    }

    /// Key-Repeat-Ereignis wird ebenfalls als `TuiEvent::Key` weitergeleitet.
    #[test]
    fn test_key_repeat_maps_to_some_key() {
        let event = make_key_event(KeyEventKind::Repeat);
        let result = from_crossterm(event);
        assert!(
            matches!(result, Some(TuiEvent::Key(_))),
            "Key-Repeat muss als Some(TuiEvent::Key) zurückgegeben werden"
        );
    }

    /// Key-Release-Ereignis wird ignoriert (gibt `None` zurück).
    #[test]
    fn test_key_release_maps_to_none() {
        let event = make_key_event(KeyEventKind::Release);
        let result = from_crossterm(event);
        assert_eq!(result, None, "Key-Release muss None zurückgeben");
    }

    #[test]
    fn test_mouse_wheel_maps_to_mouse_event() {
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(from_crossterm(event), Some(TuiEvent::Mouse(_))));
    }

    #[test]
    fn test_non_wheel_mouse_events_are_ignored() {
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(from_crossterm(event), None);
    }

    /// Resize-Ereignis wird als `TuiEvent::Resize` mit korrekten Abmessungen zurückgegeben.
    #[test]
    fn test_resize_maps_to_some_resize() {
        let event = Event::Resize(120, 40);
        let result = from_crossterm(event);
        assert_eq!(
            result,
            Some(TuiEvent::Resize(120, 40)),
            "Resize muss als Some(TuiEvent::Resize(120, 40)) zurückgegeben werden"
        );
    }

    /// Paste-Ereignis wird als `TuiEvent::Paste` mit dem Originaltext zurückgegeben.
    #[test]
    fn test_paste_maps_to_some_paste() {
        let text = "sk-abc123".to_owned();
        let event = Event::Paste(text.clone());
        let result = from_crossterm(event);
        assert_eq!(
            result,
            Some(TuiEvent::Paste(text)),
            "Paste muss als Some(TuiEvent::Paste) mit dem eingefügten Text zurückgegeben werden"
        );
    }

    /// Nicht-relevante Ereignisse (z. B. FocusGained) werden ignoriert.
    #[test]
    fn test_focus_gained_maps_to_none() {
        let result = from_crossterm(Event::FocusGained);
        assert_eq!(result, None, "FocusGained muss None zurückgeben");
    }
}

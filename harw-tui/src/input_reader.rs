//! Blockierender Terminal-Eingabe-Leser für die harw-tui (Spec-Abschnitt 2.4, SLICE 3).
//!
//! # Verantwortung
//! Dieses Modul besitzt genau einen Baustein: [`spawn_input_reader`]. Es startet
//! einen dedizierten OS-Thread, der `crossterm::event::read()` blockierend
//! aufruft, jedes Roh-Ereignis über [`crate::tui_event::from_crossterm`] in ein
//! [`TuiEvent`] übersetzt und in einen `tokio::sync::mpsc`-Kanal einspeist. So
//! bleibt der async `tokio::select!`-Loop frei von blockierender I/O.
//!
//! # Schlüsseltypen
//! - [`spawn_input_reader`] — Fabrik, die den Reader-Thread startet.
//!
//! # Fester Not-Aus (2× Ctrl+C)
//! Jedes Ereignis läuft **vor** der Weitergabe durch
//! [`crate::hard_kill::screen_event`]. Ein zweites Ctrl+C innerhalb von
//! [`crate::hard_kill::DOUBLE_PRESS_WINDOW`] löst hier, im Reader-Thread, den
//! Not-Aus aus — unabhängig davon, welches Fenster offen ist und ob die
//! Async-Schleife gerade antwortet. Siehe [`crate::hard_kill`].
//!
//! # Nebenläufigkeit
//! Der Reader läuft auf einem eigenen `std::thread`. Er hält einen
//! `UnboundedSender<TuiEvent>`; sobald der Empfänger (im Event-Loop) abgegeben
//! wird, schlägt das nächste `send` fehl und der Thread beendet sich. Ein noch
//! im `read()` blockierter Thread endet erst mit dem nächsten Terminal-Ereignis
//! (oder dem Prozess-Ende) — das ist für einen sauberen TUI-Exit unkritisch.
//!
//! # Fehler
//! Das Modul erzeugt keine eigenen Fehlertypen. Ein `read()`-Fehler oder ein
//! geschlossener Kanal beendet die Leseschleife still.
//!
//! # Beispiele
//! ```ignore
//! use tokio::sync::mpsc::unbounded_channel;
//! use harw_tui::input_reader::spawn_input_reader;
//!
//! let (tx, mut rx) = unbounded_channel();
//! let handle = spawn_input_reader(tx, hard_kill);
//! // ... im async-Loop: rx.recv().await verarbeiten ...
//! let _ = handle; // Thread endet, sobald rx abgegeben wird.
//! ```

use std::thread::{self, JoinHandle};
use std::time::Instant;

use tokio::sync::mpsc::UnboundedSender;

use crate::hard_kill::{CtrlCDetector, DOUBLE_PRESS_WINDOW, HardKill, screen_event};
use crate::tui_event::{TuiEvent, from_crossterm};

/// Startet den blockierenden Eingabe-Leser-Thread.
///
/// # Beschreibung
/// Spawnt einen OS-Thread, der in einer Endlosschleife `crossterm::event::read()`
/// aufruft. Jedes gelesene Ereignis wird via [`from_crossterm`] gefiltert:
/// relevante Ereignisse (Key-Press/Repeat, Maus, Paste, Resize) werden als
/// [`TuiEvent`] über `tx` gesendet; ignorierte Ereignisse (Key-Release, Fokus)
/// werden verworfen. Die Schleife endet, sobald `read()` einen Fehler liefert oder das
/// `send` fehlschlägt (Empfänger abgegeben).
///
/// # Argumente
/// - `tx` (`UnboundedSender<TuiEvent>`): Sende-Ende des Eingabe-Kanals; Ownership
///   wird an den Thread übergeben.
/// - `hard_kill` (`HardKill`): der Not-Aus, den ein Doppel-Ctrl+C auslöst.
///
/// # Rückgabe
/// Ein [`JoinHandle<()>`] auf den gestarteten Thread. Der Aufrufer muss ihn nicht
/// zwingend joinen — beim TUI-Exit reicht es, den Empfänger abzugeben.
///
/// # Nebenläufigkeit
/// Startet genau einen neuen `std::thread`. `crossterm::event::read()` blockiert,
/// bis ein Terminal-Ereignis vorliegt; deshalb ist die Leseschleife bewusst
/// vom async-Runtime getrennt.
///
/// # Beispiele
/// ```ignore
/// use tokio::sync::mpsc::unbounded_channel;
/// use harw_tui::input_reader::spawn_input_reader;
///
/// let (tx, _rx) = unbounded_channel();
/// let _handle = spawn_input_reader(tx, HardKill::live());
/// ```
pub(crate) fn spawn_input_reader(
    tx: UnboundedSender<TuiEvent>,
    hard_kill: HardKill,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut detector = CtrlCDetector::new(DOUBLE_PRESS_WINDOW);
        // `while let Ok(..)`: ein Lese-Fehler (z. B. geschlossenes stdin)
        // beendet die Schleife automatisch.
        while let Ok(event) = crossterm::event::read() {
            // Zuerst der Not-Aus, dann erst die Weitergabe: er darf nie hinter
            // einer vollen Warteschlange oder einem schluckenden Dialog stehen.
            screen_event(&mut detector, &hard_kill, &event, Instant::now());
            if let Some(tui_event) = from_crossterm(event) {
                // Fehlgeschlagenes Senden bedeutet: Empfänger ist weg → Loop beendet.
                if tx.send(tui_event).is_err() {
                    break;
                }
            }
        }
    })
}

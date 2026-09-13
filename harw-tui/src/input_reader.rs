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
//! let handle = spawn_input_reader(tx);
//! // ... im async-Loop: rx.recv().await verarbeiten ...
//! let _ = handle; // Thread endet, sobald rx abgegeben wird.
//! ```

use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::UnboundedSender;

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
/// let _handle = spawn_input_reader(tx);
/// ```
pub(crate) fn spawn_input_reader(tx: UnboundedSender<TuiEvent>) -> JoinHandle<()> {
    thread::spawn(move || {
        // `while let Ok(..)`: ein Lese-Fehler (z. B. geschlossenes stdin)
        // beendet die Schleife automatisch.
        while let Ok(event) = crossterm::event::read() {
            if let Some(tui_event) = from_crossterm(event) {
                // Fehlgeschlagenes Senden bedeutet: Empfänger ist weg → Loop beendet.
                if tx.send(tui_event).is_err() {
                    break;
                }
            }
        }
    })
}

//! Terminal ownership and RAII.

use super::TuiError;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EnableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout, Write as _};

/// RAII-Guard, der Raw-Mode, Bracketed-Paste und Alternate-Screen bei Drop zurückstellt.
///
/// # Beschreibung
/// Aktiviert Raw-Mode, `EnterAlternateScreen`, Bracketed-Paste und Maus-Capture und baut
/// ein ratatui-Fullscreen-Terminal. Das Mausrad wird als `MouseEvent` an den vorhandenen
/// Chat-Scrollpfad geliefert, statt vom Terminal in `Up`/`Down` für die Input-History
/// übersetzt zu werden. Shift+Mauszug bleibt terminalseitig für die Textauswahl verfügbar.
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; ausschließlich vom Renderer-Thread verwendet.
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Aktiviert Raw-Mode, Alternate-Screen, Bracketed-Paste und Maus-Capture.
    ///
    /// # Beschreibung
    /// Reihenfolge beim Aufbau:
    /// 1. Raw-Mode aktivieren.
    /// 2. `EnterAlternateScreen`, `EnableBracketedPaste` und `EnableMouseCapture` senden.
    /// 3. Backend + Fullscreen-`Terminal` bauen.
    ///
    /// Bei jedem Fehler nach Schritt 1 wird bereits aktivierter Zustand best-effort zurückgestellt.
    ///
    /// # Fehler
    /// [`TuiError::Io`], wenn Terminal-Setup fehlschlägt.
    pub(crate) fn enter() -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
        ) {
            let _ = crossterm::execute!(
                stdout,
                crossterm::event::DisableMouseCapture,
                DisableBracketedPaste,
                crossterm::terminal::LeaveAlternateScreen,
            );
            let _ = disable_raw_mode();
            return Err(TuiError::from(error));
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = crossterm::execute!(
                    io::stdout(),
                    crossterm::event::DisableMouseCapture,
                    DisableBracketedPaste,
                    crossterm::terminal::LeaveAlternateScreen,
                );
                let _ = disable_raw_mode();
                Err(TuiError::from(error))
            }
        }
    }

    /// Liefert eine veränderbare Referenz auf das zugrunde liegende Terminal.
    pub(super) fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// Stellt die vollständige TUI-Terminal-Ownership best-effort wieder her.
    ///
    /// # Beschreibung
    /// Host-Arbeit kann nicht nur Raw-Mode/Paste/Mouse-Capture verändern,
    /// sondern auch den Alternate-Screen verlassen. Passiert das, würde der
    /// nächste ratatui-Frame in den normalen tmux-Scrollback zeichnen und
    /// frühere Agenten-/Jobs-Docks als scheinbare Duplikate stehen lassen.
    /// Deshalb wird die komplette Terminal-Sequenz reassertiert und das
    /// ratatui-Terminal anschließend zu einem vollständigen Redraw gezwungen.
    /// Fehler bleiben best-effort: Self-Healing darf die TUI nicht beenden.
    pub(crate) fn reassert_terminal_modes(&mut self) {
        if let Err(error) = enable_raw_mode() {
            tracing::warn!(%error, "tui.terminal.reassert_raw_mode_failed");
        }
        if let Err(error) = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
        ) {
            tracing::warn!(%error, "tui.terminal.reassert_modes_failed");
        }
        if let Err(error) = self.terminal.clear() {
            tracing::warn!(%error, "tui.terminal.reassert_clear_failed");
        }
    }
}

/// Stellt Terminal-Raw-Mode, Bracketed-Paste und Alternate-Screen beim Drop zurück (Best-effort).
impl Drop for TerminalGuard {
    /// Deaktiviert Bracketed-Paste, verlässt den Alternate-Screen, deaktiviert Raw-Mode
    /// und blendet den Cursor wieder ein.
    ///
    /// # Beschreibung
    /// Läuft in umgekehrter Reihenfolge zur Aktivierung. Alle Aufrufe sind best-effort —
    /// Fehler werden verworfen, damit kein Panic im Drop ausgelöst wird.
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::event::DisableMouseCapture,
            DisableBracketedPaste,
            crossterm::terminal::LeaveAlternateScreen,
        );
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

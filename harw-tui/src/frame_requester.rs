//! Frame-Anforderungskanal für `harw-tui`.
//!
//! Verantwortungsbereich: Bereitstellung eines leichtgewichtigen Mechanismus,
//! mit dem beliebige Programmteile das Neuzeichnen eines TUI-Frames anfordern
//! können — entweder sofort oder nach einem konfigurierbaren Delay.
//!
//! Exportierte Typen: [`FrameRequester`], [`frame_channel`], [`MIN_FRAME_INTERVAL`].
//!
//! Nebenläufigkeitsmodell: `FrameRequester` ist `Clone` und `Send + Sync` (via
//! `tokio::sync::mpsc::UnboundedSender`). `schedule_frame_in` spawnt einen
//! `tokio`-Task auf dem aktuellen Runtime-Handle.
//!
//! Fehlertypen: keine — gesendete Frames werden mit `let _ = …` stille verworfen,
//! falls der Receiver bereits geschlossen wurde.
//!
//! Spec-Quelle: `docs/design/codex-tui-study/00-harw-tui-redesign-spec.md`,
//! Abschnitt 2.5 + SLICE 4.
//!
//! # Beispiele
//!
//! ```ignore
//! use harw_tui::frame_requester::{frame_channel, MIN_FRAME_INTERVAL};
//!
//! let (requester, mut rx) = frame_channel();
//! requester.schedule_frame();
//! assert!(rx.try_recv().is_ok());
//! ```

use std::time::Duration;

// ---------------------------------------------------------------------------
// Konstante
// ---------------------------------------------------------------------------

/// Mindestabstand zwischen zwei Draw-Frames (~120 FPS).
///
/// Ein späterer Scheduler-Task liest Anfragen vom `UnboundedReceiver` und
/// koalesziert sie: Er wartet mindestens `MIN_FRAME_INTERVAL` zwischen zwei
/// aufeinanderfolgenden `TuiEvent::Draw`-Sendungen, damit die CPU nicht durch
/// unnötige Redraws belastet wird.
///
/// Spec-Quelle: Abschnitt 2.5 der Redesign-Spec.
pub(crate) const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(8);

// ---------------------------------------------------------------------------
// Typen
// ---------------------------------------------------------------------------

/// Leichtgewichtiger Handle zum Anfordern eines TUI-Frame-Redraws.
///
/// # Beschreibung
/// Hält einen `UnboundedSender<()>` auf den Frame-Kanal. Jeder Klon des
/// `FrameRequester` teilt denselben Kanal; es spielt keine Rolle, welcher
/// Klon `schedule_frame` aufruft.
///
/// # Nebenläufigkeit
/// `Clone + Send + Sync`. Kann sicher aus mehreren Threads/Tasks gleichzeitig
/// verwendet werden, da `UnboundedSender` thread-safe ist.
///
/// Spec-Quelle: Abschnitt 2.5 + SLICE 4 der Redesign-Spec.
#[derive(Clone)]
pub(crate) struct FrameRequester {
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}

impl FrameRequester {
    /// Fordert sofort einen Frame-Redraw an.
    ///
    /// # Beschreibung
    /// Sendet `()` auf den internen Kanal. Fehler beim Senden (z. B. weil der
    /// Receiver bereits geschlossen wurde) werden stillschweigend ignoriert,
    /// da ein verlorenes Draw-Signal keine Datenbeschädigung verursacht.
    ///
    /// # Nebenläufigkeit
    /// Sicher aus beliebigen Threads/Tasks aufrufbar.
    ///
    /// Spec-Quelle: Abschnitt 2.5 der Redesign-Spec.
    pub(crate) fn schedule_frame(&self) {
        let _ = self.tx.send(());
    }
}

// ---------------------------------------------------------------------------
// Kanal-Konstruktor
// ---------------------------------------------------------------------------

/// Erzeugt einen neuen Frame-Kanal und gibt `(FrameRequester, Receiver)` zurück.
///
/// # Beschreibung
/// Legt einen unbounded MPSC-Kanal an. Der zurückgegebene `FrameRequester`
/// darf geklont und an beliebige Widgets / Tasks verteilt werden. Der
/// `UnboundedReceiver<()>` verbleibt beim Event-Loop-Task (Scheduler), der
/// Draw-Signale auf `MIN_FRAME_INTERVAL` koalesziert und dann
/// `TuiEvent::Draw` weiterleitet.
///
/// # Rückgabe
/// `(FrameRequester, tokio::sync::mpsc::UnboundedReceiver<()>)` — Sender-Handle
/// und zugehöriger Empfänger.
///
/// Spec-Quelle: Abschnitt 2.5 + SLICE 4 der Redesign-Spec.
pub(crate) fn frame_channel() -> (FrameRequester, tokio::sync::mpsc::UnboundedReceiver<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (FrameRequester { tx }, rx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Prüft, dass `schedule_frame` ein Signal sofort über `try_recv` lesbar macht.
    #[test]
    fn test_schedule_frame_liefert_signal() -> TestResult {
        // Tokio-Runtime wird für `frame_channel` (unbounded_channel) nicht
        // benötigt — nur für `tokio::spawn`. Der Kanal selbst ist sync-nutzbar.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Runtime"))?;
        let _guard = rt.enter();

        let (requester, mut rx) = frame_channel();
        requester.schedule_frame();

        assert_eq!(
            rx.try_recv(),
            Ok(()),
            "schedule_frame() muss sofort ein () auf den Kanal senden"
        );
        Ok(())
    }

    /// Prüft, dass ohne Aufruf kein Signal im Kanal liegt.
    #[test]
    fn test_kein_signal_ohne_schedule() -> TestResult {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Runtime"))?;
        let _guard = rt.enter();

        let (_requester, mut rx) = frame_channel();

        assert!(
            rx.try_recv().is_err(),
            "Ohne schedule_frame() darf kein Signal im Kanal sein"
        );
        Ok(())
    }

    /// Prüft, dass mehrere Klone desselben Requesters auf denselben Kanal senden.
    #[test]
    fn test_klone_teilen_kanal() -> TestResult {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(ctx("Runtime"))?;
        let _guard = rt.enter();

        let (requester, mut rx) = frame_channel();
        let requester2 = requester.clone();

        requester.schedule_frame();
        requester2.schedule_frame();

        assert_eq!(rx.try_recv(), Ok(()));
        assert_eq!(
            rx.try_recv(),
            Ok(()),
            "Beide Klone müssen je ein Signal senden"
        );
        assert!(rx.try_recv().is_err(), "Kein drittes Signal erwartet");
        Ok(())
    }
}

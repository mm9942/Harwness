//! Ereignis-Bus für harw-tui (Spec: `docs/design/tui-architecture.md`,
//! Abschnitt 2.2 und SLICE 3).
//!
//! Dieses Modul definiert den zentralen `HarwEvent`-Kanal, über den Widgets und
//! Hintergrund-Tasks Ereignisse an den Haupt-Event-Loop von `HarwApp` senden,
//! ohne direkten `&mut HarwApp`-Zugriff zu benötigen.
//!
//! # Exportierte Typen
//! - [`HarwEvent`] — Enum aller möglichen Anwendungsereignisse.
//! - [`HarwEventSender`] — Klonbarer Sender-Handle für den unbeschränkten Kanal.
//!
//! # Kanalmodell
//! Unbeschränkter MPSC-Kanal (`tokio::sync::mpsc::unbounded_channel`):
//! - Mehrere Sender (Widgets, Spawn-Tasks) — jeder hält eine geklonte
//!   [`HarwEventSender`]-Instanz.
//! - Ein Empfänger (`UnboundedReceiver<HarwEvent>`) im Haupt-Event-Loop.
//!
//! # Nebenläufigkeit
//! `HarwEvent` ist `Send + Sync`. `HarwEventSender` ist `Clone + Send`.
//! Sende-Fehler (Empfänger bereits abgegeben) werden still verworfen.
//!
//! # Beispiel
//! ```ignore
//! use harw_tui::events::{harw_event_channel, HarwEvent};
//!
//! #[tokio::main]
//! async fn main() {
//!     let (sender, mut receiver) = harw_event_channel();
//!     sender.send(HarwEvent::Submit("Hallo".to_owned()));
//!     if let Some(event) = receiver.recv().await {
//!         println!("{event:?}");
//!     }
//! }
//! ```

use tokio::sync::mpsc;

// ────────────────────────────────────────────────────────────────────────────
// HarwEvent
// ────────────────────────────────────────────────────────────────────────────

/// Alle Anwendungsereignisse, die über den `HarwEvent`-Bus übertragen werden.
///
/// # Beschreibung
/// Widgets oder Hintergrund-Tasks erzeugen Varianten dieses Enums und senden
/// sie über [`HarwEventSender::send`] an den zentralen `tokio::select!`-Loop
/// in `HarwApp::run`. Der Loop ordnet jedes Ereignis dem zuständigen
/// Verarbeitungszweig zu.
///
/// # Varianten
/// - [`Submit`](HarwEvent::Submit) — Nutzer hat Eingabe bestätigt (Enter).
/// - [`SystemMessage`](HarwEvent::SystemMessage) — Systemnachricht soll in
///   der Chat-Historie erscheinen (z. B. Fehlermeldung, Statushinweis).
/// - [`Command`](HarwEvent::Command) — eine abgeschickte `/command`-Zeile
///   (oder `!`-Shell/`#`-Note/`@`-Mention) soll asynchron über die
///   Operation-Adapter-Pipeline ausgeführt werden ([`crate::command_exec::execute_command`]).
/// - [`Quit`](HarwEvent::Quit) — Anwendung soll sauber beendet werden.
///
/// # Nebenläufigkeit
/// `HarwEvent` implementiert `Send + Sync` und kann sicher über Thread-Grenzen
/// und Tokio-Tasks hinweg verschickt werden.
///
/// # Beispiel
/// ```ignore
/// use harw_tui::events::HarwEvent;
///
/// let event = HarwEvent::Submit("Welche Uhrzeit ist es?".to_owned());
/// assert_eq!(event, HarwEvent::Submit("Welche Uhrzeit ist es?".to_owned()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HarwEvent {
    /// Der Nutzer hat seine Eingabe mit Enter bestätigt. Enthält den
    /// vollständigen Eingabetext.
    Submit(String),

    /// Eine Systemnachricht soll in der Chat-Historie angezeigt werden
    /// (z. B. Fehler, Statusinfo).
    SystemMessage(String),

    /// Eine abgeschickte `/command`-Zeile (oder `!`-Shell/`#`-Note/`@`-Mention)
    /// soll asynchron über [`crate::command_exec::execute_command`] ausgeführt
    /// werden. Enthält die unveränderte Rohzeile. `handle_key` bleibt dadurch
    /// synchron — die Ausführung selbst (inkl. `Operation::run`) läuft im
    /// asynchronen `run_loop`.
    Command(String),

    /// Fordert das saubere Beenden der Anwendung an.
    Quit,
}

// ────────────────────────────────────────────────────────────────────────────
// HarwEventSender
// ────────────────────────────────────────────────────────────────────────────

/// Klonbarer Sender-Handle für den `HarwEvent`-Kanal.
///
/// # Beschreibung
/// Kapselt einen `tokio::sync::mpsc::UnboundedSender<HarwEvent>`. Widgets und
/// Hintergrund-Tasks erhalten je eine geklonte Instanz und können darüber
/// Ereignisse an den Haupt-Event-Loop schicken, ohne direkten `&mut HarwApp`-
/// Zugriff zu benötigen.
///
/// Sende-Fehler (der Empfänger wurde bereits freigegeben) werden still
/// verworfen (`let _ = …`). Das ist bewusst: ein fehlgeschlagenes Senden
/// bedeutet, dass der Loop ohnehin bereits beendet ist.
///
/// # Nebenläufigkeit
/// `HarwEventSender` ist `Clone + Send` und kann bedenkenlos zwischen
/// Tokio-Tasks geteilt werden.
///
/// # Beispiel
/// ```ignore
/// use harw_tui::events::{harw_event_channel, HarwEvent};
///
/// let (sender, _rx) = harw_event_channel();
/// let sender2 = sender.clone(); // zweiter Handle für einen Widget
/// sender2.send(HarwEvent::Quit);
/// ```
pub(crate) struct HarwEventSender(mpsc::UnboundedSender<HarwEvent>);

impl HarwEventSender {
    /// Sendet ein [`HarwEvent`] an den Kanal-Empfänger.
    ///
    /// # Beschreibung
    /// Sende-Fehler (Empfänger bereits freigegeben) werden still verworfen.
    ///
    /// # Argumente
    /// - `event` (`HarwEvent`): Das zu sendende Ereignis; Ownership wird
    ///   übergeben.
    ///
    /// # Nebenläufigkeit
    /// Kann aus beliebigen Threads oder Tokio-Tasks aufgerufen werden.
    ///
    /// # Beispiel
    /// ```ignore
    /// use harw_tui::events::{harw_event_channel, HarwEvent};
    ///
    /// let (sender, _rx) = harw_event_channel();
    /// sender.send(HarwEvent::AssistantComplete);
    /// ```
    pub(crate) fn send(&self, event: HarwEvent) {
        let _ = self.0.send(event);
    }
}

impl Clone for HarwEventSender {
    /// Erzeugt einen weiteren Handle auf denselben Kanal.
    ///
    /// # Beschreibung
    /// Delegiert an den inneren `UnboundedSender::clone`. Alle Klone teilen
    /// denselben Kanal-Endpunkt; Nachrichten werden FIFO beim einzigen
    /// Empfänger eingereiht.
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Kanalfabrik
// ────────────────────────────────────────────────────────────────────────────

/// Erzeugt einen neuen unbeschränkten `HarwEvent`-Kanal.
///
/// # Beschreibung
/// Gibt ein Tupel aus [`HarwEventSender`] (Sender-Handle, klonbar) und
/// `tokio::sync::mpsc::UnboundedReceiver<HarwEvent>` (Empfänger, einmalig)
/// zurück. Der Sender wird an Widgets übergeben; der Empfänger wird im
/// `tokio::select!`-Loop von `HarwApp::run` verwendet.
///
/// # Gibt zurück
/// `(HarwEventSender, UnboundedReceiver<HarwEvent>)` — beide Hälften des
/// Kanals. Der Sender kann beliebig oft geklont werden; der Empfänger ist
/// einmalig.
///
/// # Nebenläufigkeit
/// Beide Kanalenden sind `Send`. Der Empfänger ist nicht `Sync`; er sollte
/// ausschließlich im Event-Loop-Task verwendet werden.
///
/// # Beispiel
/// ```ignore
/// use harw_tui::events::{harw_event_channel, HarwEvent};
///
/// #[tokio::main]
/// async fn main() {
///     let (tx, mut rx) = harw_event_channel();
///     tx.send(HarwEvent::Quit);
///     let received = rx.recv().await.expect("Kanal offen");
///     assert_eq!(received, HarwEvent::Quit);
/// }
/// ```
pub(crate) fn harw_event_channel() -> (HarwEventSender, mpsc::UnboundedReceiver<HarwEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (HarwEventSender(tx), rx)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Prüft, dass ein über `HarwEventSender::send` geschicktes
    /// `HarwEvent::Submit` unverändert beim Empfänger ankommt.
    #[tokio::test]
    async fn test_send_submit_empfangen() -> TestResult {
        let (sender, mut receiver) = harw_event_channel();
        let nachricht = "Hallo, Welt!".to_owned();

        sender.send(HarwEvent::Submit(nachricht.clone()));

        let empfangen = receiver
            .recv()
            .await
            .ok_or(TestError::Missing("Kanal sollte offen sein"))?;
        assert_eq!(empfangen, HarwEvent::Submit(nachricht));
        Ok(())
    }

    /// Prüft, dass ein geklonter `HarwEventSender` denselben Kanal verwendet:
    /// Nachrichten beider Sender landen beim selben Empfänger in FIFO-Reihenfolge.
    #[tokio::test]
    async fn test_clone_sender_gleicher_kanal() -> TestResult {
        let (sender1, mut receiver) = harw_event_channel();
        let sender2 = sender1.clone();

        sender1.send(HarwEvent::SystemMessage("erster".to_owned()));
        sender2.send(HarwEvent::Quit);

        let erste = receiver
            .recv()
            .await
            .ok_or(TestError::Missing("erste Nachricht"))?;
        let zweite = receiver
            .recv()
            .await
            .ok_or(TestError::Missing("zweite Nachricht"))?;

        assert_eq!(erste, HarwEvent::SystemMessage("erster".to_owned()));
        assert_eq!(zweite, HarwEvent::Quit);
        Ok(())
    }

    /// Prüft, dass ein Sende-Versuch nach Schließen des Empfängers nicht
    /// panikt (stilles Verwerfen via `let _ = …`).
    #[test]
    fn test_send_nach_receiver_drop_kein_panic() {
        let (sender, receiver) = harw_event_channel();
        drop(receiver);
        // Darf nicht paniken:
        sender.send(HarwEvent::Quit);
    }
}

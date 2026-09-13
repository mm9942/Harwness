//! Telegram-Anbindung des Audit-Spiegels aus `harw-secrets` (Knoten AW7-04).
//!
//! # Warum Telegram hier der zweite Weg ist — und wogegen er nicht schützt
//! [`harw_secrets::audit::mirror::MirrorTransport`] verlangt nur, dass der
//! Übertragungsweg **nicht derselbe** ist wie der, dessen Integrität er
//! bezeugen soll. Telegram läuft über einen fremden Server (Telegrams
//! Infrastruktur), authentifiziert über ein eigenes Zugangsdatum (Bot-Token),
//! nicht über Dateisystem- oder Prozesszugriff auf diesem Host. Das trennt
//! den Meldeweg von der naheliegendsten Angriffsklasse — lokale Manipulation
//! des Audit-Logs oder des Prozesses, der es schreibt.
//!
//! Es trennt ihn **nicht** von einem Angreifer, der auch die ausgehende
//! Netzwerkverbindung dieses Hosts kontrolliert (DNS-Hijack, umgeleiteter
//! Traffic, eine Firewall-Regel, die `api.telegram.org` sperrt) — dann bleibt
//! nur aus, dass Meldungen ankommen, beobachtbar für einen externen
//! Beobachter, der das Ausbleiben selbst überwacht (kein Totmann-Schalter
//! ist Teil dieser Datei). Es schützt ebenfalls nicht gegen eine
//! Kompromittierung des Bot-Tokens oder des Ziel-Chats selbst.
//!
//! # Was tatsächlich gebaut ist — und was bewusst minimal blieb
//! [`TelegramMirrorTransport`] rendert jede
//! [`harw_secrets::audit::mirror::MirrorEntry`] bzw.
//! [`harw_secrets::audit::mirror::ChainBreakAlert`] über deren eigene,
//! bereits inhaltsfreie `to_report_line()` und schickt den Text per
//! [`TelegramClient::send_message`] ab. Der eigentliche Versand läuft
//! **fire-and-forget** auf einem hereingereichten `tokio::runtime::Handle`:
//! `send_entry`/`send_break_alert` blockieren den Aufrufer nie auf eine
//! Netzwerkantwort (§ Harte Auflagen: ein ausgefallener Transport darf den
//! Host nicht hängen lassen), und ein Zustellfehler wird innerhalb der
//! gespawnten Aufgabe geloggt (`tracing::warn!`) statt still verworfen. Ein
//! bereits heruntergefahrenes Runtime-Handle lässt `Handle::spawn`
//! panicken; das wird hier mit `catch_unwind` abgefangen und als
//! [`harw_secrets::MirrorError`] zurückgegeben statt den Host mitzureißen.
//!
//! Bewusst **nicht** gebaut: **jeder** Test in dieser Datei. Jeder Aufruf von
//! `send_entry`/`send_break_alert` auf einem laufenden `Handle` plant einen
//! echten `TelegramClient::send_message`-Aufruf gegen `api.telegram.org` ein
//! — auch fire-and-forget auf einem Hintergrund-Task ist eine echte
//! Netzwerkverbindung, und diese Datei darf gemäß harter Auflage keine
//! herstellen. Ein Test, der stattdessen ein bereits heruntergefahrenes
//! `Handle` verwendet, wäre auf undokumentiertes, versionsabhängiges
//! Tokio-Verhalten angewiesen (ob `Handle::spawn` dabei tatsächlich
//! *panic*t oder die Aufgabe still verwirft, ist nicht Teil der öffentlichen
//! Zusicherung von `tokio::runtime::Handle::spawn`) und wäre damit potenziell
//! flakig, ohne dass `cargo test` hier ausgeführt werden darf, um das zu
//! verifizieren. Das `catch_unwind` in `TelegramMirrorTransport::spawn_send`
//! bleibt trotzdem in der Produktionslogik: es ist unabhängig von diesem
//! Verhalten immer sicher (ein no-op, falls `spawn` nicht paniert) und
//! deckt genau den Fall ab, den die harte Auflage verlangt — ein
//! ausgefallener Transport darf den Host nicht mitreißen. Die
//! verhaltensbezogenen Tests (Bruch wird erkannt, Meldung ist inhaltsfrei,
//! Zähler bewegt sich, ein Transport-Fehlschlag hängt den Aufrufer nicht auf)
//! liegen deshalb vollständig in `harw_secrets::audit::mirror` gegen
//! [`harw_secrets::audit::mirror::RecordingMirrorTransport`] — netzwerkfrei
//! und ohne dieses Verhalten zu erraten.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use harw_secrets::audit::mirror::{ChainBreakAlert, MirrorEntry, MirrorTransport};
use harw_secrets::error::{MirrorError, MirrorResult};
use tokio::runtime::Handle;

use crate::client::TelegramClient;

/// Sendet Audit-Spiegel-Meldungen fire-and-forget über einen Telegram-Chat.
///
/// # Description
/// Siehe Moduldoku für die Begründung des Wegs und seine Grenzen. Der
/// Bot-Token bleibt ausschließlich in [`TelegramClient`] (`SecretBox`) — diese
/// Struktur hält nur eine geteilte Referenz darauf.
pub struct TelegramMirrorTransport {
    client: Arc<TelegramClient>,
    chat_id: i64,
    message_thread_id: Option<i64>,
    handle: Handle,
}

impl TelegramMirrorTransport {
    /// Baut einen Telegram-Transport für den Audit-Spiegel.
    ///
    /// # Arguments
    /// - `client` (`Arc<TelegramClient>`): geteilter, token-haltender Client.
    /// - `chat_id` (`i64`): Telegram-Chat, an den Meldungen gehen.
    /// - `message_thread_id` (`Option<i64>`): optionales Forum-Thema.
    /// - `handle` (`tokio::runtime::Handle`): Runtime, auf der der
    ///   fire-and-forget-Versand läuft.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`TelegramMirrorTransport`].
    #[must_use]
    pub fn new(
        client: Arc<TelegramClient>,
        chat_id: i64,
        message_thread_id: Option<i64>,
        handle: Handle,
    ) -> Self {
        Self {
            client,
            chat_id,
            message_thread_id,
            handle,
        }
    }

    /// Spawnt den eigentlichen Versand fire-and-forget; fängt ein Panic von
    /// `Handle::spawn` ab (etwa bei bereits heruntergefahrenem Runtime),
    /// statt es den Host mitreißen zu lassen. Gibt bei Erfolg `Ok(())`
    /// zurück (der eigentliche Netzwerkausgang bleibt asynchron und wird nur
    /// innerhalb der gespawnten Aufgabe geloggt, siehe Moduldoku).
    fn spawn_send(&self, text: String) -> Result<(), String> {
        let client = Arc::clone(&self.client);
        let chat_id = self.chat_id;
        let message_thread_id = self.message_thread_id;
        let handle = self.handle.clone();

        catch_unwind(AssertUnwindSafe(|| {
            handle.spawn(async move {
                if let Err(error) = client
                    .send_message(chat_id, &text, message_thread_id, None)
                    .await
                {
                    tracing::warn!(%error, "audit-mirror telegram delivery failed");
                }
            });
        }))
        .map_err(|_| "telegram runtime handle unavailable for audit-mirror spawn".to_owned())
    }
}

impl MirrorTransport for TelegramMirrorTransport {
    fn send_entry(&self, entry: &MirrorEntry) -> MirrorResult<()> {
        self.spawn_send(entry.to_report_line())
            .map_err(|reason| MirrorError::EntryRejected { reason })
    }

    fn send_break_alert(&self, alert: &ChainBreakAlert) -> MirrorResult<()> {
        self.spawn_send(alert.to_report_line())
            .map_err(|reason| MirrorError::AlertRejected { reason })
    }
}

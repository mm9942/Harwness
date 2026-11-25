//! Ein Batch, ein Datagramm, alle Antworten: der Ablauf einer Operation
//! über einen austauschbaren Transport ([`Transport`], [`Session`]).
//!
//! # Verantwortungsbereich
//! [`Session::apply`] baut den Batch (Modul `batch`), räumt verspätete
//! Antworten früherer Operationen ab, sendet den Batch als **ein**
//! Datagramm und liest die Antworten, bis die Entscheidung steht (Modul
//! `reply`). Der Transport kennt nur Senden und Empfangen; der echte ist der
//! `NETLINK_NETFILTER`-Socket (Modul `socket`, nur Linux), die Tests nutzen
//! einen skriptbaren Kernel-Ersatz.
//!
//! # Lesen
//! Der Kernel verarbeitet einen Batch synchron im `sendto` des Aufrufers
//! (Netlink an den Kernel ruft dessen Empfangsfunktion direkt auf). Wenn
//! `sendto` zurückkehrt, liegen alle Antworten schon im Empfangspuffer
//! (Probe, Kernel 6.18: mit `MSG_DONTWAIT` sofort vollständig lesbar).
//! Gelesen wird trotzdem blockierend mit Frist (`SO_RCVTIMEO`, gesetzt beim
//! Öffnen): fehlt eine Antwort, endet das Warten nach der Frist mit
//! [`NftError::NoReply`] statt ewig. Ohne Warten (`MSG_DONTWAIT`) wird
//! gelesen, sobald sich Warten nicht mehr lohnt
//! (`crate::reply::ReplyTracker::wait_for_more`): nach dem ersten Fehler
//! (der Batch ist verworfen; was noch kommt, liegt schon im Puffer, und es
//! entscheidet der Fehler) und sobald jede Objektnachricht beantwortet ist —
//! außer der Kernel zeigt Batch-Bestätigungen und hat bisher alles mit `0`
//! beantwortet: dann ist die Bestätigung von `BATCH_END` zum Erfolg nötig,
//! und es wird weiter mit Frist gewartet. Steht die Entscheidung fest
//! (`crate::reply::ReplyTracker::is_settled`), endet das Lesen; übrige
//! Antworten räumt die nächste Operation ab.
//!
//! Was eine fehlende Antwort bedeutet, ist offen (verworfen oder verloren);
//! sie gilt immer als Fehler, nie als Erfolg. Ein Batch, den der Kernel
//! ohne Fehlermeldung verwirft, weil er `BATCH_END` nicht erreicht, zeigt
//! sich nur auf Kerneln mit Batch-Bestätigung als fehlende Bestätigung von
//! `BATCH_END`; ältere bestätigen dann jede Objektnachricht mit `0`. Dieser
//! Fall ist mit dem Kodierer unerreichbar: jeder Batch trägt `BATCH_END` im
//! selben Datagramm (Modul `batch`, „Flags und Bestätigung“).
//!
//! # Grenzen
//! - Empfangspuffer [`RECV_BUFFER_LEN`] (8 KiB): die längste Antwort ist
//!   eine Fehlerantwort mit der Kopie der längsten Nachricht (`NEWRULE` mit
//!   252-Zeichen-Suffix, 460 Byte) plus 20 Byte. Längere Datagramme werden
//!   über `MSG_TRUNC` erkannt und als [`NftError::MalformedReply`] gemeldet,
//!   nie still abgeschnitten.
//! - Höchstens `message_count + `[`MAX_EXTRA_DATAGRAMS`] Empfangsversuche
//!   je Operation und [`MAX_DRAIN_DATAGRAMS`] beim Abräumen.
//! - Datagramme, deren Absender nicht der Kernel ist (Port-ID ≠
//!   [`KERNEL_PORT_ID`]), werden übergangen. Unicast an diesen Socket
//!   verlangt ohnehin `CAP_NET_ADMIN` (`NETLINK_NETFILTER` erlaubt
//!   unprivilegiertes Senden nicht).
//!
//! # Sequenznummern
//! Jede Operation beginnt bei der Nummer nach dem Ende der vorigen
//! (`wrapping_add`); verspätete Antworten tragen deshalb Nummern außerhalb
//! des laufenden Batches und werden übergangen.
//!
//! # Nebenläufigkeit
//! Eine [`Session`] gehört genau einem Aufrufer (`&mut self`);
//! `crate::NftNetlink` serialisiert über einen `Mutex`.
//!
//! # Fehler
//! Siehe [`NftError`]: Transportfehler werden unverändert weitergereicht,
//! Antworten entscheidet `crate::reply::ReplyTracker`.

use crate::batch::{Batch, isolate_batch, release_all_batch, release_chains_batch};
use crate::error::{NftError, NftResult};
use crate::reply::{Intent, Outcome, ReplyTracker, parse_datagram};
use crate::rule::{CgroupRule, validate_chain_suffix};

/// Port-ID des Kernels als Absender (`nl_pid` = 0, netlink(7): „0 if the
/// destination is in the kernel“; Probe: jede Antwort kam von `(0, 0)`).
pub(crate) const KERNEL_PORT_ID: u32 = 0;

/// Größe des Empfangspuffers in Byte (siehe Moduldoku, „Grenzen“).
pub(crate) const RECV_BUFFER_LEN: usize = 8192;

/// Zusätzliche Empfangsversuche je Operation über die Nachrichtenzahl
/// hinaus (fremde Absender, verspätete Antworten, Unterbrechungen).
pub(crate) const MAX_EXTRA_DATAGRAMS: u32 = 16;

/// Höchstzahl verspäteter Datagramme, die vor dem Senden abgeräumt werden.
pub(crate) const MAX_DRAIN_DATAGRAMS: u32 = 64;

/// Wie ein Empfang wartet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecvMode {
    /// Blockierend bis zur Frist (`SO_RCVTIMEO`).
    Wait,
    /// Ohne Warten (`MSG_DONTWAIT`).
    Poll,
}

/// Ergebnis eines Empfangsversuchs.
// Ohne Linux erzeugt nur der Test-Kernel Empfangsergebnisse.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Received {
    /// Ein Datagramm: `len` Byte stehen im Puffer, `full_len` ist seine
    /// wahre Länge (`MSG_TRUNC`), `sender` die Port-ID des Absenders.
    Datagram {
        /// Bytes im Puffer.
        len: usize,
        /// Länge laut Kernel (größer als `len` = abgeschnitten).
        full_len: usize,
        /// Port-ID des Absenders (`None`: keine Netlink-Adresse).
        sender: Option<u32>,
    },
    /// Nichts da: bei [`RecvMode::Poll`] leer, bei [`RecvMode::Wait`] die
    /// Frist abgelaufen.
    Nothing,
    /// Durch ein Signal unterbrochen; erneut versuchen.
    Interrupted,
}

/// Senden und Empfangen von Netlink-Datagrammen an bzw. vom Kernel.
pub(crate) trait Transport {
    /// Sendet `datagram` als ein Datagramm an den Kernel.
    ///
    /// # Errors
    /// Transportfehler ([`NftError::Socket`], [`NftError::PartialSend`],
    /// [`NftError::NoBufferSpace`]).
    fn send(&mut self, datagram: &[u8]) -> NftResult<()>;

    /// Empfängt höchstens ein Datagramm in `buf`.
    ///
    /// # Errors
    /// [`NftError::NoBufferSpace`] (Antworten verworfen) oder
    /// [`NftError::Socket`].
    fn recv(&mut self, buf: &mut [u8], mode: RecvMode) -> NftResult<Received>;
}

/// Eine Änderung der eigenen Tabelle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation<'a> {
    /// cgroup isolieren.
    Isolate(&'a CgroupRule),
    /// Die beiden Ketten mit diesem Suffix entfernen.
    Release(&'a str),
    /// Die ganze Tabelle entfernen.
    ReleaseAll,
}

/// Ein Transport samt Sequenzzähler.
#[derive(Debug)]
pub(crate) struct Session<T> {
    transport: T,
    next_seq: u32,
}

impl<T: Transport> Session<T> {
    /// Baut die Sitzung; die erste Operation beginnt bei `first_seq`.
    // Ohne Linux baut nur der Test-Kernel eine Sitzung.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) const fn new(transport: T, first_seq: u32) -> Self {
        Self {
            transport,
            next_seq: first_seq,
        }
    }

    /// Der Transport (Tests prüfen damit, was gesendet wurde).
    #[cfg(test)]
    pub(crate) fn transport(&self) -> &T {
        &self.transport
    }

    /// Der Transport, veränderbar (Tests legen damit Antworten nach).
    #[cfg(test)]
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Führt `operation` als ein Batch aus.
    ///
    /// # Returns
    /// Das [`Outcome`], wenn der Kernel den Batch übernommen hat bzw. beim
    /// Aufheben nichts mehr da war.
    ///
    /// # Errors
    /// Eingabe- und Kodierfehler (vor dem Senden), Transportfehler und die
    /// Fehler der Antwortauswertung (Modul `reply`).
    pub(crate) fn apply(&mut self, operation: Operation<'_>) -> NftResult<Outcome> {
        let first_seq = self.next_seq;
        let (batch, intent) = match operation {
            Operation::Isolate(rule) => (isolate_batch(rule, first_seq)?, Intent::Isolate),
            Operation::Release(chain_suffix) => {
                validate_chain_suffix(chain_suffix)?;
                (
                    release_chains_batch(chain_suffix, first_seq)?,
                    Intent::Release,
                )
            }
            Operation::ReleaseAll => (release_all_batch(first_seq)?, Intent::Release),
        };
        self.next_seq = batch.end_seq().wrapping_add(1);
        let mut buf = vec![0_u8; RECV_BUFFER_LEN];
        self.drain(&mut buf)?;
        self.transport.send(batch.as_bytes())?;
        self.collect(&batch, intent, &mut buf)
    }

    // Räumt verspätete Antworten früherer Operationen ab (ohne Warten).
    // Ein `ENOBUFS` hier betrifft ebenfalls eine frühere Operation.
    fn drain(&mut self, buf: &mut [u8]) -> NftResult<()> {
        for _ in 0..MAX_DRAIN_DATAGRAMS {
            match self.transport.recv(buf, RecvMode::Poll) {
                Ok(Received::Nothing) => return Ok(()),
                Ok(Received::Datagram { .. } | Received::Interrupted)
                | Err(NftError::NoBufferSpace) => {}
                Err(other) => return Err(other),
            }
        }
        Err(NftError::MalformedReply {
            reason: "too many unsolicited replies",
        })
    }

    // Liest die Antworten auf `batch`, bis die Entscheidung steht.
    fn collect(&mut self, batch: &Batch, intent: Intent, buf: &mut [u8]) -> NftResult<Outcome> {
        let mut tracker = ReplyTracker::new(batch, intent);
        let mut mode = RecvMode::Wait;
        let attempts = batch.message_count().saturating_add(MAX_EXTRA_DATAGRAMS);
        for _ in 0..attempts {
            if tracker.is_settled() {
                break;
            }
            let (len, full_len) = match self.transport.recv(buf, mode)? {
                Received::Nothing => break,
                Received::Interrupted => continue,
                Received::Datagram {
                    sender: Some(KERNEL_PORT_ID),
                    len,
                    full_len,
                } => (len, full_len),
                // Nicht vom Kernel: übergehen.
                Received::Datagram { .. } => continue,
            };
            if full_len > len {
                return Err(NftError::MalformedReply {
                    reason: "reply larger than the receive buffer",
                });
            }
            let datagram = buf.get(..len).ok_or(NftError::MalformedReply {
                reason: "reply length beyond the receive buffer",
            })?;
            for ack in parse_datagram(datagram)? {
                tracker.record(ack)?;
            }
            mode = if tracker.wait_for_more() {
                RecvMode::Wait
            } else {
                RecvMode::Poll
            };
        }
        tracker.decide()
    }
}

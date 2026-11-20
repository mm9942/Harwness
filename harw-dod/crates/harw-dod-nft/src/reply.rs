//! Kernel-Antworten lesen und entscheiden: Netlink-Rahmen zerlegen
//! ([`parse_datagram`]) und die Antworten eines Batches auswerten
//! ([`ReplyTracker`]).
//!
//! # Verantwortungsbereich
//! Reiner Code auf Bytes, ohne Socket. Der Transport (Modul `transport`)
//! reicht jedes empfangene Datagramm hierher und fragt danach, ob die
//! Entscheidung steht und ob sich Warten noch lohnt.
//!
//! # Rahmen (`/usr/include/linux/netlink.h`)
//! - Ein Datagramm trägt eine oder mehrere Nachrichten; jede beginnt mit
//!   `struct nlmsghdr` (`:52-58`, [`crate::uapi::NLMSG_HDRLEN`] = 16 Byte,
//!   Host-Reihenfolge). Gültig ist eine Nachricht, wenn `nlmsg_len` den
//!   Kopf umfasst und nicht über das Datagramm hinausreicht (`NLMSG_OK`,
//!   `:106-108`); die nächste beginnt bei `NLMSG_ALIGN(nlmsg_len)` (`:99`),
//!   bei der letzten darf die Füllung fehlen (wie im Kernel: Länge auf den
//!   Rest gekappt).
//! - `NLMSG_ERROR` (`:113`) trägt `struct nlmsgerr` (`:119-131`): `int
//!   error` (`0` = Bestätigung, sonst `-errno`), danach eine Kopie des Kopfes
//!   der beantworteten Nachricht. Der Kernel setzt `nlmsg_seq` der Antwort
//!   gleich dem der beantworteten Nachricht; die Kopie muss denselben Wert
//!   tragen. Bei einem Fehler folgt der Kopie die ganze Nachricht (ohne
//!   `NETLINK_CAP_ACK`); dieser Teil wird nie gelesen, gespeichert oder
//!   gemeldet.
//! - Andere Nachrichtentypen werden übersprungen: ohne Multicast-Gruppe und
//!   ohne Abfrage schickt der Kernel diesem Socket nur `NLMSG_ERROR`.
//!
//! # Antwortformen des Kernels
//! Belegt am Quelltext von `net/netfilter/nfnetlink.c` (Zeilen: v6.18;
//! verglichen mit v6.9 bis v6.17), die Normalfälle zusätzlich mit Kernel 6.18
//! geprobt (Modul `batch`, „Flags und Bestätigung“). Wichtig:
//! `nfnetlink_rcv_skb_batch` zieht `BATCH_BEGIN` vom Puffer ab (`:642`), bevor
//! `nfnetlink_rcv_batch` läuft; `nlmsg_hdr(oskb)` ist dort deshalb die
//! **erste Objektnachricht** (`NEWTABLE`), nicht `BATCH_BEGIN`.
//!
//! - **Sofort gegen `BATCH_BEGIN`, danach nichts:** fehlendes
//!   `CAP_NET_ADMIN` (`EPERM`, `:661-662`, Probe), kein nf_tables
//!   (`EOPNOTSUPP`, `:406-419`), sonstige Abbrüche vor der Verarbeitung
//!   (`:385`, `:392`, `:426`).
//! - **Gesammelt, zugestellt nach Übernahme bzw. Verwerfen** (`:605`), in
//!   dieser Reihenfolge: ab Linux 6.10 die Bestätigung von `BATCH_BEGIN`
//!   (`:432-435`); je Objektnachricht `0` oder ihr Fehler (`:538-561`; ein
//!   Fehler verwirft den ganzen Batch, `:559-560`); ab 6.10 nach
//!   erfolgreicher Übernahme die Bestätigung von `BATCH_END` (`:583-586`).
//! - **Übernahme gescheitert:** der Fehler kommt sofort gegen die erste
//!   Objektnachricht (`:582`), danach die gesammelten Antworten ohne
//!   `BATCH_END` — die erste Objektnachricht bekommt also einen Fehler
//!   **und** eine `0`.
//! - **Sammeln gescheitert** (`ENOMEM`): die gesammelten Antworten werden
//!   verworfen, nur `ENOMEM` gegen die erste Objektnachricht kommt
//!   (`:549-551`).
//! - **Verschobene Bestätigung von `BATCH_BEGIN` (Linux 6.10 bis 6.17):**
//!   wiederholt der Kernel den Batch, weil er ein Modul nachladen musste
//!   (`-EAGAIN`, z. B. der erste `socket`-Ausdruck ohne geladenes
//!   `nft_socket`), sammelt er die Bestätigung von `BATCH_BEGIN` gegen die
//!   auslösende Nachricht: deren Sequenznummer und Typ, als **erste**
//!   Antwort, zusätzlich zu ihrer regulären. Nach einer Wiederholung wegen
//!   der Übernahme (`:577-579`) trifft sie `BATCH_END`, also vor den
//!   Objektnachrichten. v6.18 setzt den Kopf vorher auf `BATCH_BEGIN` zurück
//!   (`onlh`, `:379`, `:390`); v6.10 bis v6.17 nicht.
//! - **Ohne jede Antwort:** Abbruch bei einer kaputten Nachricht, einem
//!   zweiten `BATCH_BEGIN` oder einem tödlichen Signal (`:441`, `:454`,
//!   `:468`).
//!
//! # Entscheidungsregel
//! 1. Ein Fehler gegen `BATCH_BEGIN` entscheidet sofort: Fehler
//!    ([`batch_error`]).
//! 2. Jeder andere Fehler heißt: der Kernel hat den Batch verworfen. Es
//!    entscheidet der erste in Batch-Reihenfolge ([`object_error`]), auch
//!    wenn andere Antworten fehlen (Sammeln gescheitert). Fehler und `0` auf
//!    derselben Nachricht widersprechen sich nicht (gescheiterte Übernahme,
//!    verschobene Bestätigung); der Fehler zählt. Fehler **und** `0` auf der
//!    ersten Objektnachricht entscheiden sofort (gescheiterte Übernahme; es
//!    folgen nur noch Nullen). Ausnahme beim Aufheben:
//!    [`Outcome::AlreadyGone`] nur, wenn **jede** Löschnachricht `ENOENT`
//!    und keine `0` trägt und jede andere Objektnachricht fehlerfrei
//!    bestätigt ist. Meldet nur ein Teil der Löschnachrichten `ENOENT`, hat
//!    der Kernel die übrigen mit dem ganzen Batch zurückgerollt — die
//!    Isolation steht noch, das ist ein Fehler. Mit Kernel 6.18 nie
//!    beobachtet (jeder Batch legt vor dem Löschen an). Beim Isolieren gilt
//!    `ENOENT` immer als Fehler — eine Isolation darf nie still ausfallen.
//! 3. Ohne Fehler verlangt Erfolg eine `0` auf **jede** Objektnachricht.
//!    Fehlt eine: [`NftError::NoReply`] — nie Erfolg.
//! 4. Zeigt der Kernel Batch-Bestätigungen — eine `0` auf `BATCH_BEGIN`
//!    oder die verschobene (Regel 5) —, bestätigt er `BATCH_END` nach jeder
//!    erfolgreichen Übernahme. Dann verlangt Erfolg zusätzlich diese
//!    Bestätigung; ohne sie wurde der Batch verworfen
//!    ([`NftError::NoReply`]) — auch wenn jede Objektnachricht `0` trägt
//!    (Probe: Batch ohne `BATCH_END`). Ohne Batch-Bestätigungen (vor Linux
//!    6.10) gilt Regel 3 allein; kommt die Bestätigung von `BATCH_END`
//!    trotzdem, ist sie willkommen.
//! 5. Eine **zusätzliche** `0` — die zweite auf eine Objektnachricht oder
//!    eine auf `BATCH_END`, bevor jede Objektnachricht beantwortet ist — ist
//!    die verschobene Bestätigung von `BATCH_BEGIN`: höchstens eine je Batch
//!    und nie neben einer echten. Jede weitere ist
//!    [`NftError::MalformedReply`].
//! 6. Antworten mit Sequenznummern außerhalb des Batches (verspätete eines
//!    früheren Batches) werden übergangen; ein zweiter Fehler auf dieselbe
//!    Nachricht, eine zweite `0` auf `BATCH_BEGIN` oder eine Antwort mit
//!    fremdem Nachrichtentyp ist [`NftError::MalformedReply`].
//!
//! Keine Lockerung aus Regel 2 oder 5 kann Erfolg erzeugen: Erfolg verlangt,
//! dass keine Antwort einen Fehler trägt, jede Objektnachricht eine `0` hat
//! und — sobald der Kernel Batch-Bestätigungen zeigt — `BATCH_END` nach
//! allen Objektnachrichten bestätigt ist. Die verschobene `0` kann die
//! reguläre Antwort ihrer Nachricht nicht ersetzen, weil der Kernel beide in
//! derselben Liste sammelt und nur gemeinsam verwirft.
//!
//! # Fehlernummern
//! Gegen `BATCH_BEGIN`/`BATCH_END` (den ganzen Batch, [`batch_error`]):
//! `EPERM` → [`NftError::PermissionDenied`] (`nfnetlink_rcv` prüft dort
//! `CAP_NET_ADMIN`; Probe), `EOPNOTSUPP` → [`NftError::Unsupported`]
//! (Subsystem nf_tables fehlt), sonst wie bei Objektnachrichten.
//!
//! Gegen eine Objektnachricht ([`object_error`]) — auch die Fehler der
//! Übernahme und des Sammelns, die gegen die erste Objektnachricht kommen:
//! `ENOENT` → [`NftError::NotFound`], `EEXIST` → [`NftError::AlreadyExists`],
//! `ENOBUFS` → [`NftError::NoBufferSpace`], jede andere — auch `EPERM` und
//! `EOPNOTSUPP` — → [`NftError::Rejected`] mit der Nummer. Dort heißen
//! `EPERM` und `EOPNOTSUPP` nicht „Recht fehlt“ bzw. „nf_tables fehlt“:
//! `EPERM` meldet der Kernel, wenn `inet harw_warden` mit
//! `NFT_TABLE_F_OWNER` einem anderen Netlink-Socket gehört (Probe, Kernel
//! 6.18: `EPERM` auf `NEWTABLE` und `NEWCHAIN`, `BATCH_BEGIN` bestätigt);
//! `EOPNOTSUPP` u. a. bei einem Widerspruch der Owner-Flags in
//! `nf_tables_updtable` oder aus der Hardware-Auslagerung beim Übernehmen
//! (nach Kenntnis des Kernelquelltexts, nicht geprobt).
//!
//! # Nebenläufigkeit
//! Reine Funktionen und ein Zustand je Batch; kein Teilen.

use crate::batch::{Batch, MessageKind};
use crate::error::{NftError, NftResult};
use crate::uapi::{
    EEXIST, ENOBUFS, ENOENT, EOPNOTSUPP, EPERM, NLMSG_ALIGNTO, NLMSG_ERROR, NLMSG_HDRLEN,
    NLMSGERR_LEN,
};

/// Eine `NLMSG_ERROR`-Antwort, reduziert auf das, was die Entscheidung
/// braucht (keine Bytes der beantworteten Nachricht).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ack {
    /// `nlmsg_seq` der Antwort (= Sequenznummer der beantworteten
    /// Nachricht).
    pub(crate) seq: u32,
    /// `0` = Bestätigung, sonst die positive Fehlernummer.
    pub(crate) errno: i32,
    /// `nlmsg_type` der beantworteten Nachricht (aus der Kopie des Kopfes).
    pub(crate) echoed_type: u16,
}

/// Wofür ein Batch gebaut wurde; entscheidet, ob `ENOENT` auf
/// Löschnachrichten „schon fort“ heißen darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Intent {
    /// Isolieren: jeder Fehler ist ein Fehler.
    Isolate,
    /// Aufheben (eine cgroup oder alles).
    Release,
}

/// Ergebnis eines Batches, den der Kernel nicht abgelehnt hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Jede Objektnachricht ist bestätigt, keine Antwort trägt einen Fehler;
    /// zeigt der Kernel Batch-Bestätigungen (ab Linux 6.10), ist auch
    /// `BATCH_END` bestätigt.
    Applied,
    /// Aufheben: der Kernel meldete `ENOENT` auf jede Löschnachricht und
    /// keinen anderen Fehler.
    AlreadyGone,
}

// Kopf einer Netlink-Nachricht (Host-Reihenfolge).
#[derive(Debug, Clone, Copy)]
struct Header {
    len: u32,
    msg_type: u16,
    seq: u32,
}

impl Header {
    // Liest `struct nlmsghdr` (`netlink.h:52-58`); `nlmsg_flags` und
    // `nlmsg_pid` werden nicht gebraucht.
    fn parse(bytes: [u8; NLMSG_HDRLEN]) -> Self {
        let [
            l0,
            l1,
            l2,
            l3,
            t0,
            t1,
            _f0,
            _f1,
            s0,
            s1,
            s2,
            s3,
            _p0,
            _p1,
            _p2,
            _p3,
        ] = bytes;
        Self {
            len: u32::from_ne_bytes([l0, l1, l2, l3]),
            msg_type: u16::from_ne_bytes([t0, t1]),
            seq: u32::from_ne_bytes([s0, s1, s2, s3]),
        }
    }
}

// Fester Fehler für eine nicht wohlgeformte Antwort.
const fn malformed(reason: &'static str) -> NftError {
    NftError::MalformedReply { reason }
}

/// Zerlegt ein Datagramm und liefert seine `NLMSG_ERROR`-Antworten.
///
/// # Arguments
/// - `datagram` (`&[u8]`): die empfangenen Bytes (vollständig, nicht
///   abgeschnitten).
///
/// # Returns
/// Die Antworten in Datagramm-Reihenfolge (andere Typen übersprungen).
///
/// # Errors
/// [`NftError::MalformedReply`]: Kopf abgeschnitten, `nlmsg_len` kürzer als
/// der Kopf oder länger als der Rest, eine Fehlerantwort kürzer als
/// `struct nlmsgerr`, ein positiver oder nicht negierbarer Fehlercode oder
/// eine Kopie mit anderer Sequenznummer.
pub(crate) fn parse_datagram(datagram: &[u8]) -> NftResult<Vec<Ack>> {
    let mut acks = Vec::new();
    let mut rest = datagram;
    while !rest.is_empty() {
        let (head, _) = rest
            .split_first_chunk::<NLMSG_HDRLEN>()
            .ok_or(malformed("datagram ends inside a message header"))?;
        let header = Header::parse(*head);
        let len =
            usize::try_from(header.len).map_err(|_| malformed("nlmsg_len beyond the datagram"))?;
        if len < NLMSG_HDRLEN {
            return Err(malformed("nlmsg_len shorter than the message header"));
        }
        let (message, after) = rest
            .split_at_checked(len)
            .ok_or(malformed("nlmsg_len beyond the datagram"))?;
        if header.msg_type == NLMSG_ERROR {
            acks.push(parse_error(header, message)?);
        }
        // Nächste Nachricht bei NLMSG_ALIGN(len); fehlt die Füllung der
        // letzten, ist das Datagramm zu Ende.
        let padding = len
            .checked_next_multiple_of(NLMSG_ALIGNTO)
            .and_then(|aligned| aligned.checked_sub(len))
            .ok_or(malformed("nlmsg_len beyond the datagram"))?;
        rest = after.get(padding..).unwrap_or_default();
    }
    Ok(acks)
}

// Liest `struct nlmsgerr` einer `NLMSG_ERROR`-Nachricht.
fn parse_error(header: Header, message: &[u8]) -> NftResult<Ack> {
    let too_short = malformed("error reply shorter than struct nlmsgerr");
    // `struct nlmsgerr`: `int error`, dann die Kopie von `struct nlmsghdr`.
    let (nlmsgerr, _) = message
        .get(NLMSG_HDRLEN..)
        .and_then(<[u8]>::split_first_chunk::<NLMSGERR_LEN>)
        .ok_or(too_short)?;
    let (error, echoed) = nlmsgerr.split_first_chunk::<4>().ok_or(too_short)?;
    let (echoed, _) = echoed
        .split_first_chunk::<NLMSG_HDRLEN>()
        .ok_or(too_short)?;
    let error = i32::from_ne_bytes(*error);
    if error > 0 {
        return Err(malformed("positive error code"));
    }
    let errno = error
        .checked_neg()
        .ok_or(malformed("error code out of range"))?;
    let echoed = Header::parse(*echoed);
    if echoed.seq != header.seq {
        return Err(malformed("reply sequence differs from the echoed message"));
    }
    Ok(Ack {
        seq: header.seq,
        errno,
        echoed_type: echoed.msg_type,
    })
}

/// Fehler zu einer Fehlernummer gegen den ganzen Batch (`BATCH_BEGIN`,
/// `BATCH_END`; siehe Moduldoku, „Fehlernummern“).
pub(crate) fn batch_error(errno: i32) -> NftError {
    match errno {
        EPERM => NftError::PermissionDenied,
        EOPNOTSUPP => NftError::Unsupported,
        _ => object_error(errno),
    }
}

/// Fehler zu einer Fehlernummer gegen eine Objektnachricht; `EPERM` und
/// `EOPNOTSUPP` werden hier **nicht** als fehlendes Recht bzw. fehlendes
/// nf_tables gedeutet (siehe Moduldoku, „Fehlernummern“).
pub(crate) fn object_error(errno: i32) -> NftError {
    match errno {
        ENOENT => NftError::NotFound,
        EEXIST => NftError::AlreadyExists,
        ENOBUFS => NftError::NoBufferSpace,
        _ => NftError::Rejected { errno },
    }
}

// Antworten auf eine Nachricht des Batches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Slot {
    // Mit `0` bestätigt (eine zusätzliche `0` zählt
    // `ReplyTracker::shifted_begin_ack`, Regel 5).
    acked: bool,
    // Fehlernummer gegen diese Nachricht (höchstens eine, Regel 6).
    error: Option<i32>,
}

/// Sammelt die Antworten auf einen Batch und entscheidet.
///
/// # Description
/// Je Nachricht des Batches ein Platz (Index = Abstand zur ersten
/// Sequenznummer) mit „bestätigt“ und höchstens einer Fehlernummer, dazu
/// die verschobene Bestätigung von `BATCH_BEGIN` (Regel 5 der Moduldoku).
#[derive(Debug)]
pub(crate) struct ReplyTracker<'b> {
    batch: &'b Batch,
    intent: Intent,
    slots: Vec<Slot>,
    /// Die verschobene Bestätigung von `BATCH_BEGIN` (Linux 6.10 bis 6.17
    /// nach einer Wiederholung) ist eingetroffen.
    shifted_begin_ack: bool,
}

impl<'b> ReplyTracker<'b> {
    /// Beginnt die Auswertung für `batch`.
    pub(crate) fn new(batch: &'b Batch, intent: Intent) -> Self {
        Self {
            batch,
            intent,
            slots: vec![Slot::default(); batch.kinds().len()],
            shifted_begin_ack: false,
        }
    }

    /// Nimmt eine Antwort auf.
    ///
    /// # Errors
    /// [`NftError::MalformedReply`]: fremder Nachrichtentyp, zweiter Fehler
    /// auf dieselbe Nachricht, zweite Bestätigung von `BATCH_BEGIN` oder
    /// eine zusätzliche `0`, die nicht die verschobene Bestätigung von
    /// `BATCH_BEGIN` sein kann (Regeln 5 und 6 der Moduldoku).
    pub(crate) fn record(&mut self, ack: Ack) -> NftResult<()> {
        // Außerhalb des Batches: verspätete Antwort eines früheren Batches.
        let Some(kind) = self.batch.kind_of(ack.seq) else {
            return Ok(());
        };
        if ack.echoed_type != kind.nlmsg_type() {
            return Err(malformed("reply names a different message type"));
        }
        // Zustand des ganzen Batches, bevor der Platz geändert wird.
        let batch_acks = self.batch_acks();
        let early_end = kind == MessageKind::BatchEnd && !self.objects_answered();
        let shifted = self.shifted_begin_ack;
        let offset = usize::try_from(ack.seq.wrapping_sub(self.batch.first_seq()))
            .map_err(|_| malformed("reply sequence outside the batch"))?;
        let slot = self
            .slots
            .get_mut(offset)
            .ok_or(malformed("reply sequence outside the batch"))?;
        if ack.errno != 0 {
            return if slot.error.replace(ack.errno).is_some() {
                Err(malformed("duplicate reply"))
            } else {
                Ok(())
            };
        }
        if !early_end && !slot.acked {
            // Neben der verschobenen gibt es keine echte Bestätigung von
            // `BATCH_BEGIN` (Regel 5).
            if kind == MessageKind::BatchBegin && shifted {
                return Err(malformed("duplicate reply"));
            }
            slot.acked = true;
            return Ok(());
        }
        // Eine zusätzliche `0`: nur als verschobene Bestätigung von
        // `BATCH_BEGIN` erklärbar — höchstens einmal und nie neben der
        // echten (Regel 5). Eine zweite `0` auf `BATCH_BEGIN` selbst fällt
        // hierher, weil `batch_acks` dann schon gilt.
        if batch_acks {
            return Err(malformed(if early_end {
                "batch end confirmed before every message was answered"
            } else {
                "duplicate reply"
            }));
        }
        self.shifted_begin_ack = true;
        Ok(())
    }

    // Antworten mit ihrer Nachrichtenart, in Batch-Reihenfolge.
    fn answered(&self) -> impl Iterator<Item = (MessageKind, Slot)> + '_ {
        self.batch
            .kinds()
            .iter()
            .copied()
            .zip(self.slots.iter().copied())
    }

    // Antworten auf die Objektnachrichten, in Batch-Reihenfolge.
    fn objects(&self) -> impl Iterator<Item = (MessageKind, Slot)> + '_ {
        self.answered().filter(|(kind, _)| kind.is_object())
    }

    // Antworten auf `BATCH_BEGIN` bzw. `BATCH_END` (je Batch genau einmal).
    fn slot_of(&self, wanted: MessageKind) -> Slot {
        self.answered()
            .find(|(kind, _)| *kind == wanted)
            .map(|(_, slot)| slot)
            .unwrap_or_default()
    }

    // Ob der Kernel Batch-Nachrichten bestätigt (Regel 4): `BATCH_BEGIN`
    // bestätigt oder die verschobene Bestätigung eingetroffen.
    fn batch_acks(&self) -> bool {
        self.shifted_begin_ack || self.slot_of(MessageKind::BatchBegin).acked
    }

    // Ob irgendeine Antwort einen Fehler trägt (der Batch ist dann
    // verworfen, Regel 2).
    fn has_error(&self) -> bool {
        self.slots.iter().any(|slot| slot.error.is_some())
    }

    // Ob jede Objektnachricht eine Antwort hat (`0` oder Fehler).
    fn objects_answered(&self) -> bool {
        self.objects()
            .all(|(_, slot)| slot.acked || slot.error.is_some())
    }

    /// Ob sich blockierendes Warten (mit Frist) auf weitere Antworten lohnt.
    ///
    /// # Returns
    /// `true`, solange kein Fehler da ist und entweder eine Objektnachricht
    /// unbeantwortet ist oder nur noch die zum Erfolg nötige Bestätigung von
    /// `BATCH_END` fehlt (Regel 4). Nach einem Fehler nie: der Batch ist
    /// verworfen, der Kernel hat alles Weitere schon im `sendto` abgelegt,
    /// und eine fehlende Antwort ändert die Entscheidung nicht mehr.
    pub(crate) fn wait_for_more(&self) -> bool {
        if self.has_error() {
            return false;
        }
        if !self.objects_answered() {
            return true;
        }
        self.batch_acks() && !self.slot_of(MessageKind::BatchEnd).acked
    }

    /// Ob die Entscheidung feststeht.
    ///
    /// # Returns
    /// `true` bei einem Fehler gegen `BATCH_BEGIN` (Regel 1), bei Fehler
    /// **und** `0` auf der ersten Objektnachricht (gescheiterte Übernahme,
    /// Regel 2) und sobald `BATCH_END` bestätigt ist.
    pub(crate) fn is_settled(&self) -> bool {
        self.slot_of(MessageKind::BatchBegin).error.is_some()
            || self.slot_of(MessageKind::BatchEnd).acked
            || self
                .objects()
                .next()
                .is_some_and(|(_, slot)| slot.acked && slot.error.is_some())
    }

    // Aufheben: jede Löschnachricht meldet `ENOENT` und keine `0`, jede
    // andere Objektnachricht ist fehlerfrei bestätigt, `BATCH_END` trägt
    // keinen Fehler (Regel 2).
    fn nothing_left_to_delete(&self) -> bool {
        self.slot_of(MessageKind::BatchEnd).error.is_none()
            && self.objects().all(|(kind, slot)| {
                if kind.is_delete() {
                    slot.error == Some(ENOENT) && !slot.acked
                } else {
                    slot.acked && slot.error.is_none()
                }
            })
    }

    /// Entscheidet nach der Regel der Moduldoku.
    ///
    /// # Returns
    /// Das [`Outcome`], wenn der Kernel den Batch nicht abgelehnt hat.
    ///
    /// # Errors
    /// Den Fehler zur entscheidenden Fehlernummer — auch wenn andere
    /// Antworten fehlen — oder [`NftError::NoReply`] (ohne Fehler: Antwort
    /// auf eine Objektnachricht fehlt, oder die Bestätigung von `BATCH_END`
    /// fehlt, obwohl der Kernel Batch-Bestätigungen zeigt).
    pub(crate) fn decide(&self) -> NftResult<Outcome> {
        if let Some(errno) = self.slot_of(MessageKind::BatchBegin).error {
            return Err(batch_error(errno));
        }
        let first_error = self
            .answered()
            .filter(|(kind, _)| *kind != MessageKind::BatchBegin)
            .find_map(|(kind, slot)| slot.error.map(|errno| (kind, errno)));
        if let Some((kind, errno)) = first_error {
            if self.intent == Intent::Release && self.nothing_left_to_delete() {
                return Ok(Outcome::AlreadyGone);
            }
            return Err(if kind == MessageKind::BatchEnd {
                batch_error(errno)
            } else {
                object_error(errno)
            });
        }
        if !self.objects().all(|(_, slot)| slot.acked) {
            return Err(NftError::NoReply);
        }
        // Wer Batch-Nachrichten bestätigt, bestätigt `BATCH_END` nach jeder
        // erfolgreichen Übernahme; ohne sie ist der Batch verworfen (Regel
        // 4). Ältere Kernel: jede Objektnachricht fehlerfrei bestätigt.
        if self.slot_of(MessageKind::BatchEnd).acked || !self.batch_acks() {
            Ok(Outcome::Applied)
        } else {
            Err(NftError::NoReply)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ack, Intent, ReplyTracker, batch_error, object_error, parse_datagram};
    use crate::error::NftError;
    use crate::test_support::{TestResult, missing};
    use crate::{CgroupRule, isolate_batch};

    // Built from netlink.h independently of the parser:
    // nlmsghdr (len, type, flags, seq, pid) in host order.
    fn header(len: usize, msg_type: u16, seq: u32) -> TestResult<Vec<u8>> {
        let len = u32::try_from(len).map_err(|_| missing("len")())?;
        let mut out = Vec::new();
        out.extend_from_slice(&len.to_ne_bytes());
        out.extend_from_slice(&msg_type.to_ne_bytes());
        out.extend_from_slice(&0_u16.to_ne_bytes());
        out.extend_from_slice(&seq.to_ne_bytes());
        out.extend_from_slice(&4242_u32.to_ne_bytes());
        Ok(out)
    }

    // NLMSG_ERROR (netlink.h:113) with struct nlmsgerr (:119-131) and
    // `extra` bytes of echoed payload.
    fn error_message(seq: u32, error: i32, echoed_type: u16, extra: usize) -> TestResult<Vec<u8>> {
        let echoed_len = 16_usize.checked_add(extra).ok_or_else(missing("len"))?;
        let total = 20_usize
            .checked_add(echoed_len)
            .ok_or_else(missing("len"))?;
        let mut out = header(total, 2, seq)?;
        out.extend_from_slice(&error.to_ne_bytes());
        out.extend(header(echoed_len, echoed_type, seq)?);
        out.extend(std::iter::repeat_n(0xab_u8, extra));
        Ok(out)
    }

    fn reason(result: Result<Vec<Ack>, NftError>) -> Option<&'static str> {
        match result {
            Err(NftError::MalformedReply { reason }) => Some(reason),
            _ => None,
        }
    }

    // EPERM and EOPNOTSUPP mean "no CAP_NET_ADMIN" / "no nf_tables" only
    // against BATCH_BEGIN; against an object message they are plain
    // rejections. Numbers from asm-generic/errno-base.h and errno.h.
    #[test]
    fn test_errno_mapping_depends_on_the_answered_message() -> TestResult {
        assert_eq!(batch_error(1), NftError::PermissionDenied);
        assert_eq!(object_error(1), NftError::Rejected { errno: 1 });
        assert_eq!(batch_error(95), NftError::Unsupported);
        assert_eq!(object_error(95), NftError::Rejected { errno: 95 });
        let shared = [
            (2, NftError::NotFound),
            (17, NftError::AlreadyExists),
            (105, NftError::NoBufferSpace),
            (34, NftError::Rejected { errno: 34 }),
            (12, NftError::Rejected { errno: 12 }),
        ];
        for (errno, expected) in shared {
            assert_eq!(batch_error(errno), expected, "errno {errno}");
            assert_eq!(object_error(errno), expected, "errno {errno}");
        }
        Ok(())
    }

    // Settling and waiting for the failed-commit shape (nfnetlink.c v6.18
    // :582): ENOMEM against NEWTABLE (offset 1) at once, then the collected
    // zeros. Message types from nfnetlink.h:67 (BEGIN 0x10) and
    // nf_tables.h:111/:117 (NEWTABLE 0x0a00, NEWRULE 0x0a06).
    #[test]
    fn test_tracker_settles_on_error_and_ack_of_the_first_object_only() -> TestResult {
        let rule = CgroupRule::new(4242, 2, "6a6f62")?;
        let batch = isolate_batch(&rule, 100)?;
        let mut tracker = ReplyTracker::new(&batch, Intent::Isolate);
        assert!(tracker.wait_for_more());
        assert!(!tracker.is_settled());
        tracker.record(Ack {
            seq: 101,
            errno: 12,
            echoed_type: 0x0a00,
        })?;
        // After any error: never wait again; the error decides even though
        // replies are missing (failed collection, :549-551).
        assert!(!tracker.wait_for_more());
        assert!(!tracker.is_settled());
        assert_eq!(tracker.decide(), Err(NftError::Rejected { errno: 12 }));
        tracker.record(Ack {
            seq: 100,
            errno: 0,
            echoed_type: 0x10,
        })?;
        assert!(!tracker.is_settled());
        tracker.record(Ack {
            seq: 101,
            errno: 0,
            echoed_type: 0x0a00,
        })?;
        assert!(tracker.is_settled());
        assert_eq!(tracker.decide(), Err(NftError::Rejected { errno: 12 }));

        // A 0 and an error on a later object message (the shifted BEGIN ack
        // of 6.10 to 6.17 before NEWRULE's own error) do not settle.
        let mut tracker = ReplyTracker::new(&batch, Intent::Isolate);
        for errno in [0, 34] {
            tracker.record(Ack {
                seq: 104,
                errno,
                echoed_type: 0x0a06,
            })?;
        }
        assert!(!tracker.is_settled());
        assert!(!tracker.wait_for_more());
        assert_eq!(tracker.decide(), Err(NftError::Rejected { errno: 34 }));
        Ok(())
    }

    #[test]
    fn test_parses_an_acknowledgement() -> TestResult {
        let datagram = error_message(7, 0, 0x0a06, 0)?;
        assert_eq!(datagram.len(), 36);
        assert_eq!(
            parse_datagram(&datagram)?,
            [Ack {
                seq: 7,
                errno: 0,
                echoed_type: 0x0a06
            }]
        );
        Ok(())
    }

    #[test]
    fn test_parses_an_error_and_ignores_the_echoed_message() -> TestResult {
        // ERANGE (34) with the whole echoed NEWRULE behind the header copy.
        let datagram = error_message(104, -34, 0x0a06, 196)?;
        assert_eq!(
            parse_datagram(&datagram)?,
            [Ack {
                seq: 104,
                errno: 34,
                echoed_type: 0x0a06
            }]
        );
        Ok(())
    }

    #[test]
    fn test_parses_several_messages_and_skips_other_types() -> TestResult {
        let mut datagram = error_message(1, 0, 0x0a00, 0)?;
        datagram.extend(header(16, 1, 99)?); // NLMSG_NOOP (netlink.h:112)
        datagram.extend(header(20, 0x0a00, 98)?); // nf_tables message, 4 bytes body
        datagram.extend_from_slice(&[0, 0, 0, 0]);
        datagram.extend(error_message(2, -1, 0x10, 4)?);
        let acks = parse_datagram(&datagram)?;
        assert_eq!(
            acks,
            [
                Ack {
                    seq: 1,
                    errno: 0,
                    echoed_type: 0x0a00
                },
                Ack {
                    seq: 2,
                    errno: 1,
                    echoed_type: 0x10
                }
            ]
        );
        Ok(())
    }

    #[test]
    fn test_unaligned_messages_are_padded_and_the_last_may_lack_padding() -> TestResult {
        for extra in 0_usize..=12 {
            let mut datagram = error_message(1, -2, 0x0a05, extra)?;
            // NLMSG_ALIGN: pad the first message to 4 before the second.
            while !datagram.len().is_multiple_of(4) {
                datagram.push(0);
            }
            // The second (last) message keeps its unaligned length.
            datagram.extend(error_message(2, 0, 0x0a03, extra)?);
            let errnos: Vec<i32> = parse_datagram(&datagram)?
                .iter()
                .map(|ack| ack.errno)
                .collect();
            assert_eq!(errnos, [2, 0], "extra {extra}");
        }
        Ok(())
    }

    #[test]
    fn test_malformed_replies_are_rejected_with_a_fixed_reason() -> TestResult {
        let ack = error_message(5, 0, 0x0a00, 0)?;
        let with_len = |len: u32| -> TestResult<Vec<u8>> {
            let mut bytes = ack.clone();
            bytes
                .get_mut(..4)
                .ok_or_else(missing("nlmsg_len"))?
                .copy_from_slice(&len.to_ne_bytes());
            Ok(bytes)
        };
        let short_len = with_len(15)?;
        let long_len = with_len(40)?;
        let truncated_error = {
            let mut out = header(30, 2, 5)?;
            out.extend_from_slice(&[0; 14]);
            out
        };
        let positive = error_message(5, 3, 0x0a00, 0)?;
        let minimum = error_message(5, i32::MIN, 0x0a00, 0)?;
        let mut other_seq = error_message(5, 0, 0x0a00, 0)?;
        // The echoed header starts at 20; its seq at 20 + 8.
        if let Some(target) = other_seq.get_mut(28..32) {
            target.copy_from_slice(&6_u32.to_ne_bytes());
        }
        let cases: [(&[u8], &str); 7] = [
            (
                ack.get(..10).ok_or_else(missing("prefix"))?,
                "datagram ends inside a message header",
            ),
            (&short_len, "nlmsg_len shorter than the message header"),
            (&long_len, "nlmsg_len beyond the datagram"),
            (&truncated_error, "error reply shorter than struct nlmsgerr"),
            (&positive, "positive error code"),
            (&minimum, "error code out of range"),
            (&other_seq, "reply sequence differs from the echoed message"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(reason(parse_datagram(bytes)), Some(expected));
        }
        Ok(())
    }

    #[test]
    fn test_every_cut_is_either_a_message_boundary_or_malformed() -> TestResult {
        let mut datagram = error_message(1, 0, 0x0a00, 0)?;
        let boundary = datagram.len();
        datagram.extend(error_message(2, -2, 0x0a08, 28)?);
        for cut in 0..=datagram.len() {
            let bytes = datagram.get(..cut).ok_or_else(missing("cut"))?;
            let result = parse_datagram(bytes);
            let expected_messages = match cut {
                0 => Some(0),
                c if c == boundary => Some(1),
                c if c == datagram.len() => Some(2),
                _ => None,
            };
            match (expected_messages, result) {
                (Some(count), Ok(acks)) => assert_eq!(acks.len(), count, "cut {cut}"),
                (None, Err(NftError::MalformedReply { .. })) => {}
                (expected, other) => {
                    return Err(crate::test_support::TestError::Unexpected(format!(
                        "cut {cut}: expected {expected:?}, got {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }
}

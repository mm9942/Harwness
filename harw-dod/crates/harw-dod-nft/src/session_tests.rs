//! Tests des Ablaufs einer Operation (Modul `transport`) samt
//! Antwortauswertung (Modul `reply`) gegen einen skriptbaren Kernel-Ersatz.
//!
//! # Kernel-Ersatz
//! [`FakeKernel`] bildet den echten Kernel nach, soweit der Ablauf davon
//! abhängt: Antworten auf einen Batch liegen sofort nach `send` im
//! Empfangspuffer (der Kernel arbeitet synchron im `sendto`); ein leerer
//! Puffer heißt `Nothing` (bei `Wait` = Frist abgelaufen). Er zeichnet jedes
//! Senden und jeden Empfang samt Modus auf. Die Antwortformen der
//! Kernel-Generationen (ohne Bestätigung von Batch-Nachrichten, mit ihr, und
//! Linux 6.10 bis 6.17 mit verschobener Bestätigung von `BATCH_BEGIN`;
//! jeweils übernommen oder verworfen) beschreibt `Acks`; die gescheiterte
//! Übernahme `commit_failure`, das gescheiterte Sammeln
//! `collection_failure`. Belegt am Quelltext von `net/netfilter/nfnetlink.c`
//! (v6.9 bis v6.18, Modul `reply`, „Antwortformen des Kernels“); die Formen
//! ohne Verschiebung und ohne Fehler der Übernahme sind mit Kernel 6.18
//! geprobt.
//!
//! # Unabhängigkeit
//! Die Antworten entstehen hier aus den Header-Konstanten (erneut
//! abgeschrieben, `netlink.h`, `nfnetlink.h`, `nf_tables.h`), nicht aus dem
//! Leser. Die gesendeten Bytes vergleichen die Tests mit den reinen
//! Batch-Funktionen (deren Golden-Tests stehen in `golden_tests`).

use std::collections::VecDeque;

use crate::error::{NftError, NftResult, SocketOp};
use crate::reply::Outcome;
use crate::test_support::{TestError, TestResult, missing};
use crate::transport::{Operation, Received, RecvMode, Session, Transport};
use crate::{CgroupRule, isolate_batch, release_all_batch, release_batch};

// --- Header-Konstanten, erneut abgeschrieben --------------------------------------

/// `NLMSG_ERROR` (`netlink.h:113`).
const NLMSG_ERROR: u16 = 2;
/// `NLM_F_CAPPED` (`netlink.h:86`): an Bestätigungen gesetzt (Probe).
const CAPPED: u16 = 0x100;
/// `NFNL_MSG_BATCH_BEGIN` (`nfnetlink.h:67`).
const BEGIN: u16 = 0x10;
/// `NFNL_MSG_BATCH_END` (`nfnetlink.h:68`).
const END: u16 = 0x11;
/// `(10 << 8) | NFT_MSG_NEWTABLE` (`nf_tables.h:111`).
const NEWTABLE: u16 = 0x0a00;
/// `(10 << 8) | NFT_MSG_DELTABLE` (`nf_tables.h:113`).
const DELTABLE: u16 = 0x0a02;
/// `(10 << 8) | NFT_MSG_NEWCHAIN` (`nf_tables.h:114`).
const NEWCHAIN: u16 = 0x0a03;
/// `(10 << 8) | NFT_MSG_DELCHAIN` (`nf_tables.h:116`).
const DELCHAIN: u16 = 0x0a05;
/// `(10 << 8) | NFT_MSG_NEWRULE` (`nf_tables.h:117`).
const NEWRULE: u16 = 0x0a06;
/// `(10 << 8) | NFT_MSG_DELRULE` (`nf_tables.h:119`).
const DELRULE: u16 = 0x0a08;

/// Nachrichtentypen des Isolier-Batches in Sendereihenfolge.
const ISOLATE: [u16; 9] = [
    BEGIN, NEWTABLE, NEWCHAIN, DELRULE, NEWRULE, NEWCHAIN, DELRULE, NEWRULE, END,
];
/// Nachrichtentypen des Aufhebe-Batches.
const RELEASE: [u16; 9] = [
    BEGIN, NEWTABLE, NEWCHAIN, DELRULE, DELCHAIN, NEWCHAIN, DELRULE, DELCHAIN, END,
];
/// Nachrichtentypen des Alles-Aufheben-Batches.
const RELEASE_ALL: [u16; 4] = [BEGIN, NEWTABLE, DELTABLE, END];

/// Länge des Inhalts von `NEWTABLE` hinter dem Kopf (`nlmsg_len(nlh)`):
/// `struct nfgenmsg` (4) + `NFTA_TABLE_NAME` "harw_warden\0" (4 + 12) +
/// `NFTA_TABLE_FLAGS` (4 + 4). Ein Fehler gegen `NEWTABLE` kopiert ihn.
const NEWTABLE_PAYLOAD: usize = 28;

/// Fehlernummern (`asm-generic/errno-base.h`, `errno.h`).
const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const ENOMEM: i32 = 12;
const EEXIST: i32 = 17;
const ERANGE: i32 = 34;
const EOPNOTSUPP: i32 = 95;
const ENOBUFS: i32 = 105;

/// Port-ID, die der Kernel diesem Socket vergeben hätte.
const OUR_PORT: u32 = 3_141_592;
/// Erste Sequenznummer der Tests.
const FIRST: u32 = 1000;
/// Kettensuffix der Tests.
const SUFFIX: &str = "6a6f622d31";

// --- Antworten bauen ---------------------------------------------------------------

/// Eine `NLMSG_ERROR`-Antwort, wie `netlink_ack` sie baut: äußerer Kopf
/// (Sequenznummer der beantworteten Nachricht, unsere Port-ID), `int
/// error`, Kopie des Kopfes der beantworteten Nachricht und bei einem
/// Fehler `extra` Byte ihres Inhalts.
fn reply(seq: u32, errno: i32, echoed_type: u16, extra: usize) -> TestResult<Vec<u8>> {
    let echoed_len = 16_usize.checked_add(extra).ok_or_else(missing("len"))?;
    let total = 20_usize
        .checked_add(echoed_len)
        .ok_or_else(missing("len"))?;
    let total = u32::try_from(total).map_err(|_| missing("len")())?;
    let echoed_len = u32::try_from(echoed_len).map_err(|_| missing("len")())?;
    let error = errno.checked_neg().ok_or_else(missing("errno"))?;
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_ne_bytes());
    out.extend_from_slice(&NLMSG_ERROR.to_ne_bytes());
    out.extend_from_slice(&(if errno == 0 { CAPPED } else { 0 }).to_ne_bytes());
    out.extend_from_slice(&seq.to_ne_bytes());
    out.extend_from_slice(&OUR_PORT.to_ne_bytes());
    out.extend_from_slice(&error.to_ne_bytes());
    out.extend_from_slice(&echoed_len.to_ne_bytes());
    out.extend_from_slice(&echoed_type.to_ne_bytes());
    out.extend_from_slice(&0x0005_u16.to_ne_bytes()); // REQUEST | ACK
    out.extend_from_slice(&seq.to_ne_bytes());
    out.extend_from_slice(&0_u32.to_ne_bytes());
    out.extend(std::iter::repeat_n(0x5a_u8, extra));
    Ok(out)
}

fn seq_at(base: u32, offset: usize) -> TestResult<u32> {
    let offset = u32::try_from(offset).map_err(|_| missing("offset")())?;
    Ok(base.wrapping_add(offset))
}

fn seq(offset: usize) -> TestResult<u32> {
    seq_at(FIRST, offset)
}

/// Welche Batch-Bestätigungen der Ersatz-Kernel zusätzlich zu den
/// Antworten auf die Objektnachrichten schickt (Modul `reply`,
/// „Antwortformen des Kernels“).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acks {
    /// Vor Linux 6.10 (keine Behandlung von `NLM_F_ACK` an
    /// Batch-Nachrichten): nur die Objektnachrichten.
    ObjectsOnly,
    /// Ab 6.10, Batch übernommen: `BATCH_BEGIN` zuerst, dann die
    /// Objektnachrichten, `BATCH_END` zuletzt (Probe, Kernel 6.18).
    Committed,
    /// Ab 6.10, Batch verworfen: `BATCH_BEGIN` und die Objektnachrichten,
    /// kein `BATCH_END` (Probe, Kernel 6.18).
    Aborted,
    /// Linux 6.10 bis 6.17 nach einer Wiederholung, ausgelöst von der
    /// Nachricht am Abstand `.0`: die Bestätigung von `BATCH_BEGIN` trägt
    /// deren Sequenznummer und Typ (eine zusätzliche `0`, als erste
    /// Antwort); Batch übernommen.
    ShiftedCommitted(usize),
    /// Wie [`Acks::ShiftedCommitted`], aber verworfen (kein `BATCH_END`).
    ShiftedAborted(usize),
}

impl Acks {
    // Abstand der Nachricht, deren Sequenznummer und Typ die Bestätigung von
    // `BATCH_BEGIN` trägt (`None`: keine).
    const fn begin_ack_at(self) -> Option<usize> {
        match self {
            Self::ObjectsOnly => None,
            Self::Committed | Self::Aborted => Some(0),
            Self::ShiftedCommitted(at) | Self::ShiftedAborted(at) => Some(at),
        }
    }

    // Ob `BATCH_END` bestätigt wird (nur nach der Übernahme).
    const fn end_ack(self) -> bool {
        matches!(self, Self::Committed | Self::ShiftedCommitted(_))
    }
}

/// Antworten auf einen Batch ab Sequenznummer `base` mit den Typen
/// `types`: jede Objektnachricht bekommt `0` oder den Fehler aus `errors`
/// (Abstand → Fehlernummer); `acks` bestimmt die Bestätigungen von
/// `BATCH_BEGIN` und `BATCH_END`.
fn replies_at(
    base: u32,
    types: &[u16],
    errors: &[(usize, i32)],
    acks: Acks,
) -> TestResult<Vec<Step>> {
    let mut steps = Vec::new();
    let last = types.len().saturating_sub(1);
    if let Some(at) = acks.begin_ack_at() {
        let msg_type = types
            .get(at)
            .copied()
            .ok_or_else(missing("message carrying the BEGIN ack"))?;
        steps.push(Step::kernel(reply(seq_at(base, at)?, 0, msg_type, 0)?));
    }
    for (offset, msg_type) in types.iter().copied().enumerate() {
        if offset == 0 || offset == last {
            continue;
        }
        let errno = errors
            .iter()
            .find(|(at, _)| *at == offset)
            .map_or(0, |(_, errno)| *errno);
        let extra = if errno == 0 { 0 } else { 28 };
        steps.push(Step::kernel(reply(
            seq_at(base, offset)?,
            errno,
            msg_type,
            extra,
        )?));
    }
    if acks.end_ack() {
        steps.push(Step::kernel(reply(seq_at(base, last)?, 0, END, 0)?));
    }
    Ok(steps)
}

/// [`replies_at`] für Batches ab [`FIRST`].
fn replies(types: &[u16], errors: &[(usize, i32)], acks: Acks) -> TestResult<Vec<Step>> {
    replies_at(FIRST, types, errors, acks)
}

/// Gescheiterte Übernahme (`nfnetlink.c` v6.18 `:582`): der Fehler sofort
/// gegen die erste Objektnachricht (`NEWTABLE` an Abstand 1 — `BATCH_BEGIN`
/// ist dort schon abgezogen, `:642`), samt Kopie ihres Inhalts; danach die
/// gesammelten Antworten der verworfenen Form `acks`, alle `0`, ohne
/// `BATCH_END`.
fn commit_failure(types: &[u16], errno: i32, acks: Acks) -> TestResult<Vec<Step>> {
    let mut steps = collection_failure(errno)?;
    steps.extend(replies(types, &[], acks)?);
    Ok(steps)
}

/// Gescheitertes Sammeln (`nfnetlink.c` v6.18 `:549-551`): alle gesammelten
/// Antworten verworfen, nur dieser eine Fehler gegen die erste
/// Objektnachricht.
fn collection_failure(errno: i32) -> TestResult<Vec<Step>> {
    Ok(vec![Step::kernel(reply(
        seq(1)?,
        errno,
        NEWTABLE,
        NEWTABLE_PAYLOAD,
    )?)])
}

// --- Kernel-Ersatz -----------------------------------------------------------------

/// Ein Eintrag im Empfangspuffer des Ersatz-Kernels.
#[derive(Debug, Clone)]
enum Step {
    /// Ein Datagramm von `sender`; `full_len` überschreibt die wahre Länge.
    Datagram {
        bytes: Vec<u8>,
        sender: Option<u32>,
        full_len: Option<usize>,
    },
    /// `EINTR`.
    Interrupted,
    /// Ein Empfangsfehler.
    Fail(NftError),
}

impl Step {
    fn kernel(bytes: Vec<u8>) -> Self {
        Self::Datagram {
            bytes,
            sender: Some(0),
            full_len: None,
        }
    }

    fn from_sender(bytes: Vec<u8>, sender: Option<u32>) -> Self {
        Self::Datagram {
            bytes,
            sender,
            full_len: None,
        }
    }
}

/// Was der Ersatz-Kernel gesehen hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    Send,
    Recv(RecvMode),
}

#[derive(Debug, Default)]
struct FakeKernel {
    /// Empfangspuffer.
    queue: VecDeque<Step>,
    /// Antworten je `send`, in Reihenfolge.
    answers: VecDeque<Vec<Step>>,
    /// Fehler für das nächste `send`.
    send_error: Option<NftError>,
    /// Gesendete Datagramme.
    sent: Vec<Vec<u8>>,
    /// Aufrufe in Reihenfolge.
    events: Vec<Event>,
}

impl FakeKernel {
    fn answering(answers: Vec<Vec<Step>>) -> Self {
        Self {
            answers: answers.into(),
            ..Self::default()
        }
    }
}

impl Transport for FakeKernel {
    fn send(&mut self, datagram: &[u8]) -> NftResult<()> {
        self.events.push(Event::Send);
        if let Some(error) = self.send_error.take() {
            return Err(error);
        }
        self.sent.push(datagram.to_vec());
        if let Some(steps) = self.answers.pop_front() {
            self.queue.extend(steps);
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], mode: RecvMode) -> NftResult<Received> {
        self.events.push(Event::Recv(mode));
        match self.queue.pop_front() {
            None => Ok(Received::Nothing),
            Some(Step::Interrupted) => Ok(Received::Interrupted),
            Some(Step::Fail(error)) => Err(error),
            Some(Step::Datagram {
                bytes,
                sender,
                full_len,
            }) => {
                let len = bytes.len().min(buf.len());
                let target = buf.get_mut(..len).ok_or(NftError::Encoding {
                    reason: "fake buffer",
                })?;
                let source = bytes.get(..len).ok_or(NftError::Encoding {
                    reason: "fake bytes",
                })?;
                target.copy_from_slice(source);
                Ok(Received::Datagram {
                    len,
                    full_len: full_len.unwrap_or(bytes.len()),
                    sender,
                })
            }
        }
    }
}

fn rule() -> TestResult<CgroupRule> {
    CgroupRule::new(0x0102_0304_0506_0708, 3, SUFFIX).map_err(TestError::from)
}

// Fügt `step` an Stelle `at` ein (geprüft, ohne Panik).
fn inserted(mut steps: Vec<Step>, at: usize, step: Step) -> TestResult<Vec<Step>> {
    if at > steps.len() {
        return Err(TestError::Missing("insert position"));
    }
    steps.insert(at, step);
    Ok(steps)
}

// Entfernt den Eintrag an Stelle `at` (geprüft, ohne Panik).
fn removed(mut steps: Vec<Step>, at: usize) -> TestResult<Vec<Step>> {
    if at >= steps.len() {
        return Err(TestError::Missing("remove position"));
    }
    steps.remove(at);
    Ok(steps)
}

fn session(answers: Vec<Vec<Step>>) -> Session<FakeKernel> {
    Session::new(FakeKernel::answering(answers), FIRST)
}

fn isolate_with(answers: Vec<Step>) -> TestResult<(NftResult<Outcome>, Session<FakeKernel>)> {
    let rule = rule()?;
    let mut session = session(vec![answers]);
    let result = session.apply(Operation::Isolate(&rule));
    Ok((result, session))
}

fn release_with(answers: Vec<Step>) -> TestResult<NftResult<Outcome>> {
    let mut session = session(vec![answers]);
    Ok(session.apply(Operation::Release(SUFFIX)))
}

fn release_all_with(answers: Vec<Step>) -> TestResult<NftResult<Outcome>> {
    let mut session = session(vec![answers]);
    Ok(session.apply(Operation::ReleaseAll))
}

fn malformed(reason: &'static str) -> NftResult<Outcome> {
    Err(NftError::MalformedReply { reason })
}

// --- Erfolg ------------------------------------------------------------------------

#[test]
fn test_isolate_sends_the_encoded_batch_and_succeeds_with_every_ack() -> TestResult {
    let (result, session) = isolate_with(replies(&ISOLATE, &[], Acks::Committed)?)?;
    assert_eq!(result, Ok(Outcome::Applied));
    let kernel = session.transport();
    let expected = isolate_batch(&rule()?, FIRST)?;
    assert_eq!(kernel.sent, [expected.as_bytes().to_vec()]);
    // Drain (poll), send, BEGIN ack, seven object replies and the END ack
    // (all waiting: after a BEGIN ack and only zeros, END is required).
    let mut events = vec![Event::Recv(RecvMode::Poll), Event::Send];
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Wait), 9));
    assert_eq!(kernel.events, events);
    assert!(kernel.queue.is_empty());
    Ok(())
}

#[test]
fn test_success_without_batch_end_ack_on_older_kernels() -> TestResult {
    let (result, session) = isolate_with(replies(&ISOLATE, &[], Acks::ObjectsOnly)?)?;
    assert_eq!(result, Ok(Outcome::Applied));
    // Without a BEGIN ack no END ack is expected: after the last object
    // reply only one non-blocking read follows.
    let mut events = vec![Event::Recv(RecvMode::Poll), Event::Send];
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Wait), 7));
    events.push(Event::Recv(RecvMode::Poll));
    assert_eq!(session.transport().events, events);
    Ok(())
}

// --- Neuere Kernel: BATCH_BEGIN bestätigt, BATCH_END nötig -------------------------

#[test]
fn test_newer_kernel_without_batch_end_ack_is_never_success() -> TestResult {
    // Probe (kernel 6.18): a batch the kernel aborts without reaching
    // BATCH_END yields a BEGIN ack and a zero for every object message,
    // nothing else. That must not count as applied.
    let (result, session) = isolate_with(replies(&ISOLATE, &[], Acks::Aborted)?)?;
    assert_eq!(result, Err(NftError::NoReply));
    // It waited (with the timeout) for the END ack before giving up.
    let mut events = vec![Event::Recv(RecvMode::Poll), Event::Send];
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Wait), 9));
    assert_eq!(session.transport().events, events);

    // The same when only the END ack is missing from a committed answer.
    let mut answers = replies(&ISOLATE, &[], Acks::Committed)?;
    answers.pop().ok_or_else(missing("END ack"))?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, Err(NftError::NoReply));
    // Release and release-all follow the same rule.
    for acks in [Acks::Aborted, Acks::Committed] {
        let mut release_answers = replies(&RELEASE, &[], acks)?;
        let mut all_answers = replies(&RELEASE_ALL, &[], acks)?;
        if acks == Acks::Committed {
            release_answers.pop().ok_or_else(missing("END ack"))?;
            all_answers.pop().ok_or_else(missing("END ack"))?;
        }
        assert_eq!(release_with(release_answers)?, Err(NftError::NoReply));
        assert_eq!(release_all_with(all_answers)?, Err(NftError::NoReply));
    }
    Ok(())
}

#[test]
fn test_after_an_error_reading_never_waits() -> TestResult {
    // After an error the batch is aborted and END is never confirmed; what
    // else the kernel sends is already queued (it answers inside `sendto`).
    // From the error on, every read is non-blocking: the error is reported
    // without waiting out the timeout.
    let (result, session) = isolate_with(replies(&ISOLATE, &[(4, ERANGE)], Acks::Aborted)?)?;
    assert_eq!(result, Err(NftError::Rejected { errno: ERANGE }));
    // BEGIN ack and offsets 1 to 4 waiting; offsets 5 to 7 and the empty
    // read without waiting.
    let mut events = vec![Event::Recv(RecvMode::Poll), Event::Send];
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Wait), 5));
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Poll), 4));
    assert_eq!(session.transport().events, events);
    Ok(())
}

#[test]
fn test_duplicate_begin_ack_is_malformed() -> TestResult {
    let answers = replies(&ISOLATE, &[], Acks::Committed)?;
    let answers = inserted(answers, 1, Step::kernel(reply(FIRST, 0, BEGIN, 0)?))?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("duplicate reply"));
    Ok(())
}

#[test]
fn test_release_and_release_all_succeed_with_every_ack() -> TestResult {
    // The release batch uses FIRST..=FIRST+8, release-all starts at FIRST+9.
    let second = FIRST.wrapping_add(9);
    let mut session = session(vec![
        replies(&RELEASE, &[], Acks::Committed)?,
        replies_at(second, &RELEASE_ALL, &[], Acks::ObjectsOnly)?,
    ]);
    assert_eq!(
        session.apply(Operation::Release(SUFFIX)),
        Ok(Outcome::Applied)
    );
    assert_eq!(session.apply(Operation::ReleaseAll), Ok(Outcome::Applied));
    let release = release_batch(&rule()?, FIRST)?;
    let all = release_all_batch(second)?;
    assert_eq!(
        session.transport().sent,
        [release.as_bytes().to_vec(), all.as_bytes().to_vec()]
    );
    Ok(())
}

#[test]
fn test_several_acks_in_one_datagram_are_all_read() -> TestResult {
    let mut bytes = Vec::new();
    for step in replies(&RELEASE_ALL, &[], Acks::Committed)? {
        if let Step::Datagram { bytes: part, .. } = step {
            bytes.extend(part);
        }
    }
    assert_eq!(
        release_all_with(vec![Step::kernel(bytes)])?,
        Ok(Outcome::Applied)
    );
    Ok(())
}

#[test]
fn test_sequence_numbers_continue_across_operations_and_wrap() -> TestResult {
    let rule = rule()?;
    let start = u32::MAX.wrapping_sub(3);
    let mut session = Session::new(FakeKernel::default(), start);
    // No replies: both operations fail with NoReply, but the batches are
    // sent with consecutive sequence numbers.
    assert_eq!(
        session.apply(Operation::Isolate(&rule)),
        Err(NftError::NoReply)
    );
    assert_eq!(session.apply(Operation::ReleaseAll), Err(NftError::NoReply));
    let isolate = isolate_batch(&rule, start)?;
    assert_eq!(isolate.end_seq(), 4);
    let all = release_all_batch(5)?;
    assert_eq!(
        session.transport().sent,
        [isolate.as_bytes().to_vec(), all.as_bytes().to_vec()]
    );
    Ok(())
}

// --- Linux 6.10 bis 6.17: verschobene Bestätigung von BATCH_BEGIN -------------------

#[test]
fn test_shifted_begin_ack_after_a_module_autoload_still_succeeds() -> TestResult {
    // The first isolation after boot loads nft_socket: NEWRULE (offset 4)
    // returns -EAGAIN, the kernel replays the batch and, on 6.10 to 6.17,
    // queues the BEGIN ack against NEWRULE. NEWRULE then gets two zeros, the
    // first one before every other reply.
    let (result, session) = isolate_with(replies(&ISOLATE, &[], Acks::ShiftedCommitted(4))?)?;
    assert_eq!(result, Ok(Outcome::Applied));
    // Shifted ack, seven object replies and the END ack, all waiting (the
    // shifted ack makes the END ack necessary).
    let mut events = vec![Event::Recv(RecvMode::Poll), Event::Send];
    events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Wait), 9));
    assert_eq!(session.transport().events, events);
    // A replay after a failed commit validation shifts it onto END (:577-579),
    // ahead of the object replies.
    let (result, _) = isolate_with(replies(&ISOLATE, &[], Acks::ShiftedCommitted(8))?)?;
    assert_eq!(result, Ok(Outcome::Applied));
    for at in [2, 8] {
        let answers = replies(&RELEASE, &[], Acks::ShiftedCommitted(at))?;
        assert_eq!(release_with(answers)?, Ok(Outcome::Applied), "offset {at}");
    }
    for at in [1, 3] {
        let answers = replies(&RELEASE_ALL, &[], Acks::ShiftedCommitted(at))?;
        assert_eq!(
            release_all_with(answers)?,
            Ok(Outcome::Applied),
            "offset {at}"
        );
    }
    Ok(())
}

#[test]
fn test_shifted_begin_ack_still_requires_the_batch_end_ack() -> TestResult {
    // The shifted ack shows that the kernel acknowledges batch messages:
    // without the END ack the batch was not applied.
    for at in [1, 4, 7, 8] {
        let (result, _) = isolate_with(replies(&ISOLATE, &[], Acks::ShiftedAborted(at))?)?;
        assert_eq!(result, Err(NftError::NoReply), "offset {at}");
    }
    // Errors decide as usual, also on the message carrying the shifted ack.
    for errors in [[(7, ERANGE)], [(4, ERANGE)]] {
        let (result, _) = isolate_with(replies(&ISOLATE, &errors, Acks::ShiftedAborted(4))?)?;
        assert_eq!(
            result,
            Err(NftError::Rejected { errno: ERANGE }),
            "{errors:?}"
        );
    }
    Ok(())
}

#[test]
fn test_an_extra_ack_beside_a_real_begin_ack_or_a_second_one_is_malformed() -> TestResult {
    // Beside a real BEGIN ack (v6.18, or no replay) there is no shifted one,
    // before or after it.
    let extra = Step::kernel(reply(seq(4)?, 0, NEWRULE, 0)?);
    for at in [0, 1] {
        let answers = inserted(replies(&ISOLATE, &[], Acks::Committed)?, at, extra.clone())?;
        let (result, _) = isolate_with(answers)?;
        assert_eq!(result, malformed("duplicate reply"), "inserted at {at}");
    }
    // A real BEGIN ack after the shifted one (index 4 is NEWRULE's own 0).
    let begin = Step::kernel(reply(FIRST, 0, BEGIN, 0)?);
    let answers = inserted(replies(&ISOLATE, &[], Acks::ShiftedCommitted(4))?, 5, begin)?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("duplicate reply"));
    // A second extra zero.
    let extra = Step::kernel(reply(seq(2)?, 0, NEWCHAIN, 0)?);
    let answers = inserted(replies(&ISOLATE, &[], Acks::ShiftedCommitted(4))?, 1, extra)?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("duplicate reply"));
    Ok(())
}

// --- Fehler gegen den ganzen Batch ---------------------------------------------------

#[test]
fn test_eperm_on_batch_begin_is_permission_denied_and_stops_reading() -> TestResult {
    // Probe (kernel 6.18, unprivileged): exactly one EPERM against BEGIN.
    let mut answers = vec![Step::kernel(reply(FIRST, EPERM, BEGIN, 4)?)];
    // Anything after it must not be read in this operation.
    answers.extend(replies(&ISOLATE, &[], Acks::Committed)?);
    let (result, session) = isolate_with(answers)?;
    assert_eq!(result, Err(NftError::PermissionDenied));
    assert_eq!(
        session.transport().events,
        [
            Event::Recv(RecvMode::Poll),
            Event::Send,
            Event::Recv(RecvMode::Wait)
        ]
    );
    Ok(())
}

#[test]
fn test_commit_failure_is_reported_against_the_first_object_message() -> TestResult {
    // nfnetlink.c v6.18 :582: a failed commit is reported at once against
    // nlmsg_hdr(oskb) = NEWTABLE (BEGIN was pulled off at :642), then the
    // collected replies follow — NEWTABLE's own 0 included, no END ack.
    // EOPNOTSUPP there (e.g. from hardware offload) does not mean "no
    // nf_tables".
    let cases = [
        (ENOMEM, NftError::Rejected { errno: ENOMEM }),
        (EOPNOTSUPP, NftError::Rejected { errno: EOPNOTSUPP }),
    ];
    for acks in [Acks::ObjectsOnly, Acks::Aborted, Acks::ShiftedAborted(4)] {
        for (errno, expected) in cases {
            let (result, session) = isolate_with(commit_failure(&ISOLATE, errno, acks)?)?;
            assert_eq!(result, Err(expected), "errno {errno}, {acks:?}");
            // Waiting only for the error; then non-blocking reads until
            // NEWTABLE also has its 0, which settles it. The remaining zeros
            // (offsets 2 to 7) are left for the next operation's drain.
            let mut events = vec![
                Event::Recv(RecvMode::Poll),
                Event::Send,
                Event::Recv(RecvMode::Wait),
            ];
            let polls = if acks == Acks::ObjectsOnly { 1 } else { 2 };
            events.extend(std::iter::repeat_n(Event::Recv(RecvMode::Poll), polls));
            assert_eq!(session.transport().events, events, "{acks:?}");
            assert_eq!(session.transport().queue.len(), 6, "{acks:?}");
        }
    }
    // Never "already gone", whatever the commit errno.
    for acks in [Acks::ObjectsOnly, Acks::Aborted] {
        assert_eq!(
            release_with(commit_failure(&RELEASE, ENOENT, acks)?)?,
            Err(NftError::NotFound),
            "{acks:?}"
        );
        assert_eq!(
            release_all_with(commit_failure(&RELEASE_ALL, ENOENT, acks)?)?,
            Err(NftError::NotFound),
            "{acks:?}"
        );
    }
    Ok(())
}

#[test]
fn test_failed_reply_collection_reports_enomem_at_once() -> TestResult {
    // nfnetlink.c v6.18 :549-551: when queueing a reply fails, every
    // collected reply is dropped and only ENOMEM against NEWTABLE arrives.
    // That error decides (not NoReply), without waiting out the timeout.
    let (result, session) = isolate_with(collection_failure(ENOMEM)?)?;
    assert_eq!(result, Err(NftError::Rejected { errno: ENOMEM }));
    assert_eq!(
        session.transport().events,
        [
            Event::Recv(RecvMode::Poll),
            Event::Send,
            Event::Recv(RecvMode::Wait),
            Event::Recv(RecvMode::Poll)
        ]
    );
    assert_eq!(
        release_with(collection_failure(ENOMEM)?)?,
        Err(NftError::Rejected { errno: ENOMEM })
    );
    assert_eq!(
        release_all_with(collection_failure(ENOMEM)?)?,
        Err(NftError::Rejected { errno: ENOMEM })
    );
    Ok(())
}

#[test]
fn test_unsupported_subsystem_on_batch_begin() -> TestResult {
    let (result, _) = isolate_with(vec![Step::kernel(reply(FIRST, EOPNOTSUPP, BEGIN, 4)?)])?;
    assert_eq!(result, Err(NftError::Unsupported));
    Ok(())
}

// --- Fehler einzelner Nachrichten ----------------------------------------------------

#[test]
fn test_object_errors_map_to_typed_variants() -> TestResult {
    // EPERM and EOPNOTSUPP against an object message say nothing about
    // CAP_NET_ADMIN or nf_tables (those come against BEGIN): Rejected.
    let cases = [
        (EPERM, NftError::Rejected { errno: EPERM }),
        (ENOENT, NftError::NotFound),
        (EEXIST, NftError::AlreadyExists),
        (ENOBUFS, NftError::NoBufferSpace),
        (EOPNOTSUPP, NftError::Rejected { errno: EOPNOTSUPP }),
        (ERANGE, NftError::Rejected { errno: ERANGE }),
    ];
    for acks in [Acks::ObjectsOnly, Acks::Aborted] {
        for (errno, expected) in cases {
            // Both NEWRULE messages (offsets 4 and 7) fail, like level 256
            // did in the probe; every other object message is acknowledged.
            let answers = replies(&ISOLATE, &[(4, errno), (7, errno)], acks)?;
            let (result, _) = isolate_with(answers)?;
            assert_eq!(result, Err(expected), "errno {errno}, {acks:?}");
        }
    }
    Ok(())
}

#[test]
fn test_table_owned_by_another_socket_is_not_missing_privilege() -> TestResult {
    // Probe (kernel 6.18): `inet harw_warden` created with NFT_TABLE_F_OWNER
    // by another netlink socket; NEWTABLE and NEWCHAIN from ours get EPERM,
    // BEGIN is acknowledged, END is not. The warden holds CAP_NET_ADMIN, so
    // "insufficient privileges" would be the wrong diagnosis.
    let all_objects = |errno: i32| [1, 2, 3, 4, 5, 6, 7].map(|offset| (offset, errno));
    for errno in [EPERM, EOPNOTSUPP] {
        for acks in [Acks::ObjectsOnly, Acks::Aborted] {
            let first_only = [(1, errno)];
            let every = all_objects(errno);
            for errors in [first_only.as_slice(), every.as_slice()] {
                let (result, _) = isolate_with(replies(&ISOLATE, errors, acks)?)?;
                assert_eq!(result, Err(NftError::Rejected { errno }), "{acks:?}");
                assert_ne!(result, Err(NftError::PermissionDenied));
                assert_ne!(result, Err(NftError::Unsupported));
                // Releasing never reads it as "already gone" either.
                let release = release_with(replies(&RELEASE, errors, acks)?)?;
                assert_eq!(release, Err(NftError::Rejected { errno }), "{acks:?}");
            }
        }
    }
    Ok(())
}

#[test]
fn test_first_error_in_batch_order_decides() -> TestResult {
    let answers = replies(&ISOLATE, &[(2, EEXIST), (4, ENOENT)], Acks::ObjectsOnly)?;
    // Even when the later error (offset 4, index 3) arrives first.
    let later = answers.get(3).cloned().ok_or_else(missing("offset 4"))?;
    let answers = inserted(removed(answers, 3)?, 1, later)?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, Err(NftError::AlreadyExists));
    Ok(())
}

#[test]
fn test_isolate_never_treats_enoent_on_a_delete_as_success() -> TestResult {
    let (result, _) = isolate_with(replies(
        &ISOLATE,
        &[(3, ENOENT), (6, ENOENT)],
        Acks::ObjectsOnly,
    )?)?;
    assert_eq!(result, Err(NftError::NotFound));
    Ok(())
}

#[test]
fn test_release_treats_enoent_on_deletes_only_as_already_gone() -> TestResult {
    let only_deletes = [(3, ENOENT), (4, ENOENT), (6, ENOENT), (7, ENOENT)];
    // Newer kernels abort such a batch: BEGIN ack, no END ack.
    for acks in [Acks::ObjectsOnly, Acks::Aborted] {
        assert_eq!(
            release_with(replies(&RELEASE, &only_deletes, acks)?)?,
            Ok(Outcome::AlreadyGone),
            "{acks:?}"
        );
        assert_eq!(
            release_all_with(replies(&RELEASE_ALL, &[(2, ENOENT)], acks)?)?,
            Ok(Outcome::AlreadyGone),
            "{acks:?}"
        );
    }
    Ok(())
}

#[test]
fn test_release_with_enoent_on_only_some_deletes_is_not_found() -> TestResult {
    // Any error aborts the whole batch (NFNL_BATCH_FAILURE, nfnetlink.c
    // v6.18 :559-560): the deletes acknowledged with 0 were rolled back, so
    // their chain and its drop rule are still there. Never "already gone".
    let partial: [&[(usize, i32)]; 3] = [
        &[(7, ENOENT)],
        &[(3, ENOENT), (4, ENOENT)],
        &[(4, ENOENT), (6, ENOENT), (7, ENOENT)],
    ];
    for acks in [Acks::ObjectsOnly, Acks::Aborted] {
        for errors in partial {
            assert_eq!(
                release_with(replies(&RELEASE, errors, acks)?)?,
                Err(NftError::NotFound),
                "{errors:?}, {acks:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn test_release_enoent_on_a_create_is_an_error() -> TestResult {
    // NEWCHAIN (offset 2) failing makes the deletes fail too; never "gone".
    let errors = [(2, ENOENT), (3, ENOENT), (4, ENOENT)];
    assert_eq!(
        release_with(replies(&RELEASE, &errors, Acks::ObjectsOnly)?)?,
        Err(NftError::NotFound)
    );
    assert_eq!(
        release_all_with(replies(
            &RELEASE_ALL,
            &[(1, ENOENT), (2, ENOENT)],
            Acks::ObjectsOnly
        )?)?,
        Err(NftError::NotFound)
    );
    Ok(())
}

#[test]
fn test_release_with_mixed_delete_errors_reports_the_first() -> TestResult {
    let errors = [(4, ENOENT), (7, ERANGE)];
    assert_eq!(
        release_with(replies(&RELEASE, &errors, Acks::ObjectsOnly)?)?,
        Err(NftError::NotFound)
    );
    let errors = [(4, ERANGE), (7, ENOENT)];
    assert_eq!(
        release_with(replies(&RELEASE, &errors, Acks::ObjectsOnly)?)?,
        Err(NftError::Rejected { errno: ERANGE })
    );
    Ok(())
}

// --- Fehlende oder kaputte Antworten -------------------------------------------------

#[test]
fn test_no_reply_at_all_is_no_reply() -> TestResult {
    let (result, session) = isolate_with(Vec::new())?;
    assert_eq!(result, Err(NftError::NoReply));
    assert_eq!(
        session.transport().events,
        [
            Event::Recv(RecvMode::Poll),
            Event::Send,
            Event::Recv(RecvMode::Wait)
        ]
    );
    Ok(())
}

#[test]
fn test_a_missing_object_reply_is_no_reply_never_success() -> TestResult {
    for index in 0..7 {
        let answers = removed(replies(&ISOLATE, &[], Acks::ObjectsOnly)?, index)?;
        let (result, _) = isolate_with(answers)?;
        assert_eq!(result, Err(NftError::NoReply), "reply {index} missing");
    }
    // Newer kernels (BEGIN ack at index 0, END ack at index 8): without an
    // object reply the END ack comes too early, which no kernel does.
    for index in 1..=7 {
        let answers = removed(replies(&ISOLATE, &[], Acks::Committed)?, index)?;
        let (result, _) = isolate_with(answers)?;
        assert_eq!(
            result,
            malformed("batch end confirmed before every message was answered"),
            "reply {index} missing"
        );
    }
    Ok(())
}

#[test]
fn test_batch_end_ack_before_every_message_is_malformed() -> TestResult {
    let mut answers = replies(&ISOLATE, &[], Acks::Committed)?;
    let end = answers.pop().ok_or_else(missing("END ack"))?;
    let (result, _) = isolate_with(inserted(answers, 3, end)?)?;
    assert_eq!(
        result,
        malformed("batch end confirmed before every message was answered")
    );
    Ok(())
}

#[test]
fn test_duplicate_reply_is_malformed() -> TestResult {
    // A second error on the same message (index 3 is offset 4's error).
    let second_error = Step::kernel(reply(seq(4)?, ERANGE, NEWRULE, 28)?);
    let answers = replies(&ISOLATE, &[(4, ERANGE)], Acks::ObjectsOnly)?;
    let (result, _) = isolate_with(inserted(answers, 4, second_error)?)?;
    assert_eq!(result, malformed("duplicate reply"));
    // A third zero on one message: the second may be the shifted BEGIN ack
    // of 6.10 to 6.17, the third cannot.
    let zero = Step::kernel(reply(seq(1)?, 0, NEWTABLE, 0)?);
    let answers = replies(&ISOLATE, &[], Acks::ObjectsOnly)?;
    let answers = inserted(inserted(answers, 1, zero.clone())?, 1, zero)?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("duplicate reply"));
    // One extra zero without any BEGIN ack is the shifted one: it makes the
    // END ack necessary, which an ObjectsOnly answer lacks.
    let zero = Step::kernel(reply(seq(1)?, 0, NEWTABLE, 0)?);
    let answers = inserted(replies(&ISOLATE, &[], Acks::ObjectsOnly)?, 1, zero)?;
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, Err(NftError::NoReply));
    Ok(())
}

#[test]
fn test_reply_naming_another_message_type_is_malformed() -> TestResult {
    let mut answers = vec![Step::kernel(reply(seq(1)?, 0, NEWRULE, 0)?)];
    answers.extend(
        replies(&ISOLATE, &[], Acks::ObjectsOnly)?
            .into_iter()
            .skip(1),
    );
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("reply names a different message type"));
    Ok(())
}

#[test]
fn test_truncated_reply_is_malformed() -> TestResult {
    let bytes = reply(seq(4)?, ERANGE, NEWRULE, 28)?;
    let full_len = bytes.len().saturating_add(100);
    let answers = vec![Step::Datagram {
        bytes,
        sender: Some(0),
        full_len: Some(full_len),
    }];
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, malformed("reply larger than the receive buffer"));
    Ok(())
}

#[test]
fn test_unparsable_reply_is_malformed() -> TestResult {
    let (result, _) = isolate_with(vec![Step::kernel(vec![1, 2, 3])])?;
    assert_eq!(result, malformed("datagram ends inside a message header"));
    Ok(())
}

// --- Fremdes und Verspätetes ---------------------------------------------------------

#[test]
fn test_replies_from_other_senders_are_ignored() -> TestResult {
    // A forged EPERM from another port and one without netlink address.
    let forged = reply(FIRST, EPERM, BEGIN, 4)?;
    let mut answers = vec![
        Step::from_sender(forged.clone(), Some(4242)),
        Step::from_sender(forged, None),
    ];
    answers.extend(replies(&ISOLATE, &[], Acks::Committed)?);
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, Ok(Outcome::Applied));
    Ok(())
}

#[test]
fn test_replies_outside_the_batch_are_ignored() -> TestResult {
    let stale = reply(FIRST.wrapping_sub(1), ERANGE, NEWRULE, 28)?;
    let future = reply(FIRST.wrapping_add(9), EPERM, BEGIN, 4)?;
    let mut answers = vec![Step::kernel(stale), Step::kernel(future)];
    answers.extend(replies(&ISOLATE, &[], Acks::ObjectsOnly)?);
    let (result, _) = isolate_with(answers)?;
    assert_eq!(result, Ok(Outcome::Applied));
    Ok(())
}

#[test]
fn test_too_many_foreign_datagrams_end_in_no_reply() -> TestResult {
    let forged = reply(FIRST, EPERM, BEGIN, 4)?;
    let mut answers: Vec<Step> = (0..64)
        .map(|_| Step::from_sender(forged.clone(), Some(7)))
        .collect();
    answers.extend(replies(&ISOLATE, &[], Acks::Committed)?);
    let (result, session) = isolate_with(answers)?;
    assert_eq!(result, Err(NftError::NoReply));
    // 9 messages + 16 extra attempts after the send.
    let receives_after_send = session
        .transport()
        .events
        .iter()
        .skip_while(|event| **event != Event::Send)
        .filter(|event| matches!(event, Event::Recv(_)))
        .count();
    assert_eq!(receives_after_send, 25);
    Ok(())
}

#[test]
fn test_interrupted_receive_is_retried() -> TestResult {
    let mut answers = vec![Step::Interrupted];
    answers.extend(replies(&ISOLATE, &[], Acks::Committed)?);
    let (result, _) = isolate_with(inserted(answers, 4, Step::Interrupted)?)?;
    assert_eq!(result, Ok(Outcome::Applied));
    Ok(())
}

// --- Abräumen vor dem Senden ---------------------------------------------------------

#[test]
fn test_stale_replies_are_drained_before_sending() -> TestResult {
    let rule = rule()?;
    let mut session = session(vec![replies(&ISOLATE, &[], Acks::Committed)?]);
    // Leftovers of an earlier operation: an error with an old sequence
    // number, an overrun notice and an interruption.
    let stale = reply(FIRST.wrapping_sub(5), ERANGE, NEWRULE, 28)?;
    session.transport_mut().queue.extend([
        Step::kernel(stale),
        Step::Fail(NftError::NoBufferSpace),
        Step::Interrupted,
    ]);
    assert_eq!(
        session.apply(Operation::Isolate(&rule)),
        Ok(Outcome::Applied)
    );
    let events = &session.transport().events;
    assert_eq!(
        events.get(..5),
        Some(
            [
                Event::Recv(RecvMode::Poll),
                Event::Recv(RecvMode::Poll),
                Event::Recv(RecvMode::Poll),
                Event::Recv(RecvMode::Poll),
                Event::Send
            ]
            .as_slice()
        )
    );
    Ok(())
}

#[test]
fn test_endless_stale_replies_stop_before_sending() -> TestResult {
    let rule = rule()?;
    let mut session = session(vec![replies(&ISOLATE, &[], Acks::Committed)?]);
    let stale = reply(FIRST.wrapping_sub(5), 0, NEWRULE, 0)?;
    session
        .transport_mut()
        .queue
        .extend((0..100).map(|_| Step::kernel(stale.clone())));
    assert_eq!(
        session.apply(Operation::Isolate(&rule)),
        malformed("too many unsolicited replies")
    );
    assert!(session.transport().sent.is_empty());
    Ok(())
}

// --- Transportfehler und Eingaben ----------------------------------------------------

#[test]
fn test_send_error_is_returned_without_reading() -> TestResult {
    let rule = rule()?;
    let mut session = session(Vec::new());
    let error = NftError::Socket {
        op: SocketOp::Send,
        errno: EPERM,
    };
    session.transport_mut().send_error = Some(error);
    assert_eq!(session.apply(Operation::Isolate(&rule)), Err(error));
    assert_eq!(
        session.transport().events,
        [Event::Recv(RecvMode::Poll), Event::Send]
    );
    Ok(())
}

#[test]
fn test_receive_errors_are_returned() -> TestResult {
    let receive_error = NftError::Socket {
        op: SocketOp::Receive,
        errno: 9,
    };
    for error in [NftError::NoBufferSpace, receive_error] {
        let answers = inserted(
            replies(&ISOLATE, &[], Acks::Committed)?,
            2,
            Step::Fail(error),
        )?;
        let (result, _) = isolate_with(answers)?;
        assert_eq!(result, Err(error));
    }
    Ok(())
}

#[test]
fn test_release_validates_the_suffix_before_sending() -> TestResult {
    let mut session = session(Vec::new());
    for (suffix, reason) in [
        ("", "empty"),
        ("abc", "odd length"),
        ("XY", "not lowercase hex"),
        ("\u{202e}a", "not lowercase hex"),
    ] {
        assert_eq!(
            session.apply(Operation::Release(suffix)),
            Err(NftError::InvalidChainSuffix { reason })
        );
    }
    assert!(session.transport().events.is_empty());
    Ok(())
}

#[test]
fn test_a_failed_operation_still_advances_the_sequence() -> TestResult {
    // A failed operation still consumes its sequence range, so late replies
    // to it can never be mistaken for replies to the next batch.
    let rule = rule()?;
    let mut session = session(vec![vec![Step::kernel(reply(FIRST, EPERM, BEGIN, 4)?)]]);
    assert_eq!(
        session.apply(Operation::Isolate(&rule)),
        Err(NftError::PermissionDenied)
    );
    assert_eq!(session.apply(Operation::ReleaseAll), Err(NftError::NoReply));
    let second = session
        .transport()
        .sent
        .get(1)
        .ok_or_else(missing("second batch"))?;
    let expected = release_all_batch(FIRST.wrapping_add(9))?;
    if second.as_slice() != expected.as_bytes() {
        return Err(TestError::Unexpected(
            "second batch does not continue the sequence".to_owned(),
        ));
    }
    Ok(())
}

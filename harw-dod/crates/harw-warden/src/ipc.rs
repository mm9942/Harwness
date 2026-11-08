//! IPC-Empfangspfad: `SOCK_SEQPACKET` auf dem von systemd bereits gebundenen
//! und lauschenden Socket. `SO_PEERCRED` ist Pflicht und wird geprüft.
//!
//! # Warum `SO_PEERCRED`, nicht ein Token im Rumpf
//! `harw_dod_warden`s eigene Moduldoku (`lib.rs`) benennt die eigentliche
//! Vertrauensgrenze dieses Systems: ein `SOCK_SEQPACKET`-Unix-Socket mit
//! `SO_PEERCRED`. Ein Feld im Nachrichtenrumpf, das „ich bin der
//! Escalator" behauptet, wäre eine Behauptung der Gegenseite. `SO_PEERCRED`
//! liefert stattdessen die vom Kernel selbst festgestellte Identität des
//! verbundenen Prozesses (`pid`/`uid`/`gid`), gefüllt aus dem tatsächlichen
//! Verbindungsaufbau, nie aus fälschbaren Nutzdaten.
//!
//! # Peer-Allowlist (PL-100 WP 3.1, Bedrohung B1)
//! Nur die uid des Escalators ([`crate::peer::PeerGate`]) darf Anfragen
//! senden. Die Prüfung läuft in der Annahmeschleife, **bevor** ein Byte
//! gelesen und bevor ein Verbindungs-Thread gestartet wird: ein fremder
//! Peer kostet keinen Thread, seine Anfrage wird nie dekodiert. Er bekommt
//! die feste Antwort `peer-rejected` (nicht blockierend gesendet), der
//! Ablehnungszähler steigt. Ist `SO_PEERCRED` nicht lesbar, gilt der Peer
//! als fremd.
//!
//! # Immer eine Antwort (PL-100 WP 3.1)
//! Jede angenommene Verbindung, deren Peer nicht von sich aus ohne Anfrage
//! schließt, bekommt genau eine [`harw_dod_warden_proto::WardenReply`]:
//! - Peer abgewiesen → `peer-rejected`;
//! - Empfang gescheitert, Zeitlimit überschritten oder Nachricht zu groß →
//!   `malformed`;
//! - Anfrage nicht als v2 dekodierbar → `malformed` bzw. für jeden v1-Auftrag
//!   `unsupported-version` ([`crate::protocol::decode_request`]);
//! - kein Thread für die Verbindung (System lehnt ab) → `unavailable`;
//! - sonst das Ergebnis von `Warden::handle` — auch `execution-failed` und
//!   `unavailable` ([`harw_dod_warden::WardenOutcome::to_reply`] ist total).
//!
//! Nur ein Peer, der die Verbindung ohne Anfrage schließt (Datagramm der
//! Länge 0), bekommt nichts — es gibt niemanden mehr, der lesen könnte.
//!
//! # Ohne `unsafe`
//! `rustix::net` deckt `accept_with` (`accept4` mit `SOCK_CLOEXEC`),
//! `sockopt::socket_peercred`,
//! `sockopt::set_socket_timeout`, `recv` und `send` sicher ab (geprüft gegen
//! `rustix-1.1.5/src/net/`). `rustix::net::recv` liefert für einen
//! Array-Puffer `(Buf::Output, usize)`, kein Skalar.
//!
//! # Ein Austausch pro Verbindung
//! Jede angenommene Verbindung trägt genau eine Anfrage und genau eine
//! Antwort. Ein Peer, der nach dem Verbinden nichts sendet, wird nach
//! [`RECV_TIMEOUT`] mit `malformed` beantwortet und getrennt.
//!
//! # Nebenläufigkeit
//! [`serve_forever`] startet je zugelassener Verbindung einen Thread über
//! `std::thread::Builder::spawn` (liefert einen Fehler statt zu panicken).
//! Lehnt das System den Thread ab, antwortet die Annahmeschleife selbst mit
//! `unavailable` (nicht blockierend) und nimmt weiter an; die Verbindung
//! liegt dafür in einem `Arc<OwnedFd>`. `warden` und `gate` werden als
//! `Arc` geteilt; der `Warden` serialisiert seine Eingriffe selbst über eine
//! Sperre.
//!
//! # Logging
//! `warn` und höher tragen keine Pfade, uids oder Inhalte; die Peer-Identität
//! steht nur auf `info`/`debug`.

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use harw_dod_warden::Warden;
use harw_dod_warden_proto::{Denial, WardenReply};
use harw_macros::HarwError;

use crate::peer::PeerGate;
use crate::protocol::{decode_request, encode_reply, reply_for_denial};

/// Empfangspuffer-Größe je Nachricht. Reichlich bemessen für eine einzelne
/// serialisierte v2-Anfrage (Vorbild: `harw-sentinel::ipc::RECV_BUFFER_LEN`).
const RECV_BUFFER_LEN: usize = 16_384;

/// Wie lange ein zugelassener Peer nach dem Verbinden Zeit hat, seine Anfrage
/// zu senden.
pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// Fehler des IPC-Empfangspfads — je Verbindung, nicht prozessfatal (siehe
/// `crate::error`-Moduldoku für die Abgrenzung zu [`crate::error::WardenBinError`]).
///
/// # Description
/// Wie `harw-sentinel::ipc::IpcError` inhaltsfrei: kein
/// Betriebssystem-Fehlercode, kein Nachrichteninhalt in einer
/// `Display`-Meldung.
#[derive(Debug, HarwError)]
pub enum IpcError {
    /// Eine eingehende Verbindung konnte nicht angenommen werden.
    #[msg("failed to accept an ipc connection")]
    Accept,
    /// Ein Lesevorgang auf einer angenommenen Verbindung ist fehlgeschlagen
    /// (auch: Zeitlimit überschritten).
    #[msg("failed to receive a message from an ipc peer")]
    Recv,
    /// Eine empfangene Nachricht war größer als [`RECV_BUFFER_LEN`] und
    /// wurde vom Kernel abgeschnitten.
    #[msg("received an ipc message larger than the receive buffer; it was discarded")]
    Oversized,
    /// Eine Antwort ließ sich nicht als JSON kodieren.
    ///
    /// # Arguments
    /// - `0` (`serde_json::Error`): die zugrunde liegende Serde-Ursache.
    #[msg("failed to encode a warden reply for the ipc peer: {0}")]
    ReplyEncode(#[source] serde_json::Error),
    /// Das Senden einer Antwort an den Peer ist fehlgeschlagen.
    #[msg("failed to send a reply to an ipc peer")]
    Send,
}

/// Kernel-verbürgte Identität des verbundenen Peers (`SO_PEERCRED`).
///
/// # Description
/// Gelesen genau einmal, direkt nach `accept()` — der Kernel füllt diese
/// Werte aus dem verbindenden Prozess, nicht aus dessen (fälschbaren)
/// Nutzlast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Prozess-ID des Peers zum Zeitpunkt von `connect()`.
    pub pid: u32,
    /// Effektive Nutzer-ID des Peers.
    pub uid: u32,
    /// Effektive Gruppen-ID des Peers.
    pub gid: u32,
}

/// Nimmt Verbindungen auf dem von systemd übergebenen, bereits lauschenden
/// Socket entgegen, weist fremde Peers sofort ab und bedient jede
/// zugelassene Verbindung in einem eigenen Thread.
///
/// # Description
/// Endlosschleife über `accept()`; scheitert `accept()` (z. B. weil systemd
/// den Socket geschlossen hat), endet diese Funktion — der Aufrufer
/// (`crate::main::run`) behandelt das als
/// [`crate::error::WardenBinError::IpcAcceptLoopTerminated`].
///
/// # Arguments
/// - `listener` (`OwnedFd`): der von [`crate::systemd::acquire_listen_socket`]
///   gelieferte, bereits gebundene und lauschende Socket.
/// - `warden` (`Arc<Warden>`): der produktive Warden.
/// - `gate` (`Arc<PeerGate>`): die Peer-Allowlist.
///
/// # Concurrency
/// Startet je zugelassener Verbindung einen Thread
/// (`std::thread::Builder::spawn`); scheitert das, antwortet die Schleife
/// selbst mit `unavailable`.
pub fn serve_forever(listener: OwnedFd, warden: Arc<Warden>, gate: Arc<PeerGate>) {
    loop {
        // `SOCK_CLOEXEC`: ein `nft`-Kindprozess (Netfilter-Backend) erbt
        // keine Escalator-Verbindung.
        let connection = match rustix::net::accept_with(
            &listener,
            rustix::net::SocketFlags::CLOEXEC,
        ) {
            Ok(connection) => connection,
            Err(_) => {
                tracing::error!(error = %IpcError::Accept, "ipc accept failed; ceasing to serve");
                break;
            }
        };
        let Some(peer) = peer_credentials(&connection) else {
            gate.record_rejection();
            reject_peer(&connection, None, &gate);
            continue;
        };
        if !gate.admits(peer.uid) {
            reject_peer(&connection, Some(peer), &gate);
            continue;
        }
        tracing::info!(
            pid = peer.pid,
            uid = peer.uid,
            gid = peer.gid,
            "ipc connection from the escalator accepted"
        );
        spawn_connection_thread(Arc::new(connection), Arc::clone(&warden));
    }
}

// Startet den Verbindungs-Thread. Lehnt das System einen neuen Thread ab
// (`TasksMax`, `pids.max`, Speicher), bekommt der zugelassene Peer trotzdem
// eine Antwort (`unavailable`), und die Annahmeschleife läuft weiter —
// `std::thread::spawn` würde hier panicken und den Warden beenden.
fn spawn_connection_thread(connection: Arc<OwnedFd>, warden: Arc<Warden>) {
    let for_thread = Arc::clone(&connection);
    let spawned =
        std::thread::Builder::new().spawn(move || handle_connection(&for_thread, &warden));
    if spawned.is_err() {
        tracing::warn!("no thread for an admitted ipc connection; replied unavailable");
        reply_without_thread(&connection);
    }
}

/// Answers an admitted connection that could not get its own thread.
///
/// # Description
/// Antwortet `unavailable`, ohne die Anfrage zu lesen, nicht blockierend
/// (die Annahmeschleife darf nicht hängen).
fn reply_without_thread(connection: &OwnedFd) {
    if let Err(error) = send_reply(connection, &reply_for_denial(Denial::Unavailable), true) {
        tracing::debug!(error = %error, "could not deliver the unavailable reply");
    }
}

// Liest `SO_PEERCRED`; `None`, wenn der Kernel sie nicht liefert.
fn peer_credentials(connection: &OwnedFd) -> Option<PeerCredentials> {
    let creds = rustix::net::sockopt::socket_peercred(connection).ok()?;
    Some(PeerCredentials {
        pid: u32::try_from(creds.pid.as_raw_pid()).unwrap_or(0),
        uid: creds.uid.as_raw(),
        gid: creds.gid.as_raw(),
    })
}

// Fremder Peer: feste Antwort ohne Lesen, Zähler steht bereits.
fn reject_peer(connection: &OwnedFd, peer: Option<PeerCredentials>, gate: &PeerGate) {
    tracing::warn!(
        rejected_total = gate.rejected_total(),
        "ipc peer is not the escalator; request not read"
    );
    tracing::debug!(?peer, "rejected ipc peer identity");
    if let Err(error) = send_reply(connection, &reply_for_denial(Denial::PeerRejected), true) {
        tracing::debug!(error = %error, "could not deliver the peer-rejected reply");
    }
}

/// Bedient genau eine zugelassene Verbindung: eine Anfrage lesen, beantworten.
fn handle_connection(connection: &OwnedFd, warden: &Warden) {
    if let Err(error) = rustix::net::sockopt::set_socket_timeout(
        connection,
        rustix::net::sockopt::Timeout::Recv,
        Some(RECV_TIMEOUT),
    ) {
        tracing::debug!(error = %error, "could not set the receive timeout");
    }
    let mut buffer = [0u8; RECV_BUFFER_LEN];
    let reply = match recv_datagram(connection, &mut buffer) {
        Ok(None) => {
            tracing::debug!("ipc peer closed the connection without sending a request");
            return;
        }
        Ok(Some(request_bytes)) => respond(request_bytes, warden, jiff::Timestamp::now()),
        Err(error) => {
            tracing::warn!(error = %error, "failed to receive a well-formed request from the escalator");
            reply_for_denial(Denial::Malformed)
        }
    };
    if let Err(error) = send_reply(connection, &reply, false) {
        tracing::warn!(error = %error, "failed to send the warden reply to the escalator");
    }
}

/// Turns one received request into exactly one reply (no socket involved).
///
/// # Description
/// Dekodiert als v2 (v1 wird ausdrücklich abgelehnt) und reicht die Anfrage
/// unverändert an `Warden::handle`. Jeder Pfad ergibt eine Antwort.
pub fn respond(request_bytes: &[u8], warden: &Warden, now: jiff::Timestamp) -> WardenReply {
    match decode_request(request_bytes) {
        Ok(request) => warden.handle(&request, now).to_reply(),
        Err(error) => {
            let reason = error.as_denial();
            tracing::warn!(reason = %reason, error = %error, "warden request refused while decoding");
            reply_for_denial(reason)
        }
    }
}

/// Empfängt genau ein Datagramm in `buffer`.
///
/// # Description
/// `SOCK_SEQPACKET` liefert je `recv()` genau ein Datagramm. Ein Datagramm
/// der Länge `0` ist die geordnete Schließung durch den Peer, kein Fehler.
/// `copied` ist die Anzahl tatsächlich in `buffer` geschriebener Bytes,
/// `message_len` die ungekürzte Größe (`RecvFlags::TRUNC`).
///
/// # Returns
/// - `Some(bytes)`: die empfangene Nachricht (Teilstück von `buffer`).
/// - `None`: der Peer hat die Verbindung geordnet geschlossen.
///
/// # Errors
/// - [`IpcError::Recv`]: `recv()` scheitert (auch Zeitlimit).
/// - [`IpcError::Oversized`]: das Datagramm war größer als `buffer`.
fn recv_datagram<'a>(
    connection: &OwnedFd,
    buffer: &'a mut [u8; RECV_BUFFER_LEN],
) -> Result<Option<&'a [u8]>, IpcError> {
    use rustix::net::RecvFlags;

    let (copied, message_len) = rustix::net::recv(connection, &mut *buffer, RecvFlags::TRUNC)
        .map_err(|_| IpcError::Recv)?;
    if message_len == 0 {
        return Ok(None);
    }
    if message_len > RECV_BUFFER_LEN {
        return Err(IpcError::Oversized);
    }
    let received = buffer.get(..copied).ok_or(IpcError::Recv)?;
    Ok(Some(received))
}

/// Sendet eine bereits inhaltsfrei begrenzte Antwort an den Peer.
///
/// # Errors
/// - [`IpcError::ReplyEncode`]: Kodierung scheitert (praktisch unerreichbar).
/// - [`IpcError::Send`]: `send()` scheitert (`NOSIGNAL`: kein `SIGPIPE`;
///   bei `non_blocking` auch, wenn der Puffer voll ist).
fn send_reply(
    connection: &OwnedFd,
    reply: &WardenReply,
    non_blocking: bool,
) -> Result<(), IpcError> {
    use rustix::net::SendFlags;

    let bytes = encode_reply(reply).map_err(IpcError::ReplyEncode)?;
    let flags = if non_blocking {
        SendFlags::NOSIGNAL | SendFlags::DONTWAIT
    } else {
        SendFlags::NOSIGNAL
    };
    rustix::net::send(connection, &bytes, flags).map_err(|_| IpcError::Send)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;

    use super::{
        IpcError, handle_connection, reject_peer, reply_without_thread, respond,
        spawn_connection_thread,
    };
    use crate::peer::PeerGate;
    use crate::test_support::{TestError, TestResult, ctx, private_tempdir};
    use harw_dod_warden::{
        ActionAllowlist, DenyList, RecordedCall, RecordingAuditSink, RecordingExecutor,
        StaticTargetObserver, Warden, WardenParts, WardenPolicy, WardenState,
    };
    use harw_dod_warden_proto::{
        AuthorizationBody, CgroupKernelId, CgroupTarget, Denial, EscalationStage, KeyId, KeyRing,
        ProofKey, ProofPolicy, SignedAuthorization, WardenAction, WardenReply, WardenRequest,
        WardenResponse,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};
    use serde_json::{Value, json};

    const KEY: [u8; 32] = [0x11; 32];
    const NOW: i64 = 1_800_000_000;

    fn at(offset: i64) -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::from_second(NOW.saturating_add(offset)).map_err(ctx("timestamp"))
    }

    fn cgroup() -> TestResult<CgroupId> {
        CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))
    }

    fn warden(dir: &std::path::Path, executor: RecordingExecutor) -> TestResult<Warden> {
        let mut keys = KeyRing::new();
        keys.insert(
            KeyId::new("k1").map_err(ctx("key id"))?,
            ProofKey::from_bytes(KEY),
        )
        .map_err(ctx("ring"))?;
        let observer = StaticTargetObserver::new();
        observer.set(&cgroup()?, CgroupKernelId::new(4242).map_err(ctx("kid"))?);
        let policy = WardenPolicy::new(
            ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof"))?,
            ActionAllowlist::freeze_and_thaw(),
            DenyList::builtin(),
        );
        Ok(Warden::new(WardenParts {
            freezer: Box::new(executor.clone()),
            thawer: Box::new(executor.clone()),
            isolator: Box::new(executor.clone()),
            killer: Box::new(executor),
            observer: Box::new(observer),
            audit: Box::new(RecordingAuditSink::new()),
            keys,
            policy,
            state: WardenState::open(dir, at(0)?).map_err(ctx("state"))?,
        }))
    }

    fn freeze_request(nonce: u8) -> TestResult<WardenRequest> {
        signed_request(
            &WardenAction::FreezeCgroup { cgroup: cgroup()? },
            EscalationStage::RuleTriggered,
            nonce,
        )
    }

    // Gültig signierte Anfrage; `irreversible_ack` folgt der Aktion.
    fn signed_request(
        action: &WardenAction,
        stage: EscalationStage,
        nonce: u8,
    ) -> TestResult<WardenRequest> {
        let body = AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage,
            target: CgroupTarget::new(
                cgroup()?,
                CgroupKernelId::new(4242).map_err(ctx("kid"))?,
                at(-60)?,
            ),
            hold_secs: None,
            irreversible_ack: action.requires_irreversible_ack(),
            nonce: [nonce; 16],
            issued_at: at(0)?,
            expires_at: at(60)?,
        };
        Ok(WardenRequest::new(
            action.clone(),
            SignedAuthorization::sign(
                &ProofKey::from_bytes(KEY),
                &KeyId::new("k1").map_err(ctx("key id"))?,
                body,
            ),
        ))
    }

    // Ein lokales `SOCK_SEQPACKET`-Paar (kein Dateisystem, kein Netz).
    fn seqpacket_pair() -> TestResult<(OwnedFd, OwnedFd)> {
        rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .map_err(ctx("socketpair"))
    }

    // Liest genau eine Antwort vom Client-Ende.
    fn read_reply(client: &OwnedFd) -> TestResult<WardenReply> {
        let mut buffer = [0u8; 4096];
        let (copied, _) = rustix::net::recv(client, &mut buffer, rustix::net::RecvFlags::DONTWAIT)
            .map_err(ctx("reply is waiting"))?;
        let bytes = buffer
            .get(..copied)
            .ok_or(TestError::Missing("reply bytes"))?;
        serde_json::from_slice(bytes).map_err(ctx("reply decodes"))
    }

    fn reason(reply: &WardenReply) -> Option<Denial> {
        match reply.outcome {
            WardenResponse::Denied { reason } => Some(reason),
            WardenResponse::Executed { .. } => None,
        }
    }

    #[test]
    fn test_oversized_message_error_is_content_free() {
        assert_eq!(
            IpcError::Oversized.to_string(),
            "received an ipc message larger than the receive buffer; it was discarded"
        );
    }

    #[test]
    fn test_reply_encode_error_links_the_inner_serde_error() -> TestResult {
        use std::error::Error as _;

        let Err(json_err) = serde_json::from_str::<Value>("not json") else {
            return Err(TestError::Unexpected(
                "expected deliberately malformed JSON to fail parsing".into(),
            ));
        };
        let err = IpcError::ReplyEncode(json_err);
        assert!(err.source().is_some());
        Ok(())
    }

    #[test]
    fn test_valid_request_reaches_the_warden_and_replay_is_refused() -> TestResult {
        let dir = private_tempdir()?;
        let executor = RecordingExecutor::new();
        let warden = warden(dir.path(), executor.clone())?;
        let bytes = serde_json::to_vec(&freeze_request(1)?).map_err(ctx("encode"))?;
        let first = respond(&bytes, &warden, at(1)?);
        assert!(matches!(first.outcome, WardenResponse::Executed { .. }));
        assert_eq!(executor.calls(), vec![RecordedCall::Freeze(cgroup()?)]);
        assert_eq!(
            reason(&respond(&bytes, &warden, at(2)?)),
            Some(Denial::ProofMismatch)
        );
        Ok(())
    }

    #[test]
    fn test_every_failure_path_produces_a_reply() -> TestResult {
        let dir = private_tempdir()?;
        // Dekodieren: kein JSON, v1-Umschlag.
        let ok_warden = warden(dir.path(), RecordingExecutor::new())?;
        assert_eq!(
            reason(&respond(b"garbage", &ok_warden, at(1)?)),
            Some(Denial::Malformed)
        );
        let legacy = json!({ "finding": "finding-1", "request": { "action": {}, "proof": {} } });
        let legacy_bytes = serde_json::to_vec(&legacy).map_err(ctx("encode"))?;
        assert_eq!(
            reason(&respond(&legacy_bytes, &ok_warden, at(1)?)),
            Some(Denial::UnsupportedVersion)
        );
        // Verifikation: abgelaufen.
        let expired = serde_json::to_vec(&freeze_request(2)?).map_err(ctx("encode"))?;
        assert_eq!(
            reason(&respond(&expired, &ok_warden, at(500)?)),
            Some(Denial::ProofMismatch)
        );
        // Bindung: Aktion weicht von der signierten ab.
        let mut swapped = freeze_request(3)?;
        swapped.action = WardenAction::ThawCgroup { cgroup: cgroup()? };
        let swapped_bytes = serde_json::to_vec(&swapped).map_err(ctx("encode"))?;
        assert_eq!(
            reason(&respond(&swapped_bytes, &ok_warden, at(1)?)),
            Some(Denial::ProofMismatch)
        );
        // Policy: Kill ist korrekt signiert, aber nicht in der Allowlist.
        let kill = WardenAction::KillProcessTree { cgroup: cgroup()? };
        let kill_bytes = serde_json::to_vec(&signed_request(&kill, EscalationStage::Escalated, 5)?)
            .map_err(ctx("encode"))?;
        assert_eq!(
            reason(&respond(&kill_bytes, &ok_warden, at(1)?)),
            Some(Denial::PolicyDenied)
        );
        drop(ok_warden);
        // Ausführung scheitert: trotzdem eine Antwort.
        let failing = warden(dir.path(), RecordingExecutor::new_failing())?;
        let fresh = serde_json::to_vec(&freeze_request(4)?).map_err(ctx("encode"))?;
        assert_eq!(
            reason(&respond(&fresh, &failing, at(1)?)),
            Some(Denial::ExecutionFailed)
        );
        Ok(())
    }

    #[test]
    fn test_rejected_peer_gets_a_reply_and_its_request_stays_unread() -> TestResult {
        let (server, client) = seqpacket_pair()?;
        rustix::net::send(&client, b"{\"version\":2}", rustix::net::SendFlags::empty())
            .map_err(ctx("client sends"))?;
        let gate = PeerGate::new(4_000_000);
        assert!(!gate.admits(1000));
        reject_peer(&server, None, &gate);
        assert_eq!(reason(&read_reply(&client)?), Some(Denial::PeerRejected));
        // Der Warden hat nichts gelesen: die Anfrage liegt noch im Socket.
        let mut pending = [0u8; 64];
        let (unread, _) =
            rustix::net::recv(&server, &mut pending, rustix::net::RecvFlags::DONTWAIT)
                .map_err(ctx("request still queued"))?;
        assert_eq!(unread, 13);
        Ok(())
    }

    #[test]
    fn test_connection_without_a_thread_gets_an_unavailable_reply() -> TestResult {
        let (server, client) = seqpacket_pair()?;
        reply_without_thread(&server);
        assert_eq!(reason(&read_reply(&client)?), Some(Denial::Unavailable));
        Ok(())
    }

    #[test]
    fn test_spawned_connection_thread_answers_through_the_shared_descriptor() -> TestResult {
        let dir = private_tempdir()?;
        let warden = std::sync::Arc::new(warden(dir.path(), RecordingExecutor::new())?);
        let (server, client) = seqpacket_pair()?;
        let bytes = serde_json::to_vec(&freeze_request(9)?).map_err(ctx("encode"))?;
        rustix::net::send(&client, &bytes, rustix::net::SendFlags::empty())
            .map_err(ctx("client sends"))?;
        spawn_connection_thread(std::sync::Arc::new(server), warden);
        // Blockierend lesen (mit Zeitlimit), bis der Thread geantwortet hat.
        rustix::net::sockopt::set_socket_timeout(
            &client,
            rustix::net::sockopt::Timeout::Recv,
            Some(std::time::Duration::from_secs(10)),
        )
        .map_err(ctx("client timeout"))?;
        let mut buffer = [0u8; 4096];
        let (copied, _) = rustix::net::recv(&client, &mut buffer, rustix::net::RecvFlags::empty())
            .map_err(ctx("reply arrives"))?;
        let reply: WardenReply = serde_json::from_slice(
            buffer
                .get(..copied)
                .ok_or(TestError::Missing("reply bytes"))?,
        )
        .map_err(ctx("reply decodes"))?;
        // Die Anfrage ist mit `now` des Threads verarbeitet worden; ob sie
        // dabei abgelaufen ist, spielt hier keine Rolle — es kommt genau eine
        // Antwort.
        assert!(matches!(
            reply.outcome,
            WardenResponse::Executed { .. } | WardenResponse::Denied { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_connection_with_garbage_or_oversized_request_still_gets_a_reply() -> TestResult {
        let dir = private_tempdir()?;
        let warden = warden(dir.path(), RecordingExecutor::new())?;

        let (server, client) = seqpacket_pair()?;
        rustix::net::send(&client, b"garbage", rustix::net::SendFlags::empty())
            .map_err(ctx("send garbage"))?;
        handle_connection(&server, &warden);
        assert_eq!(reason(&read_reply(&client)?), Some(Denial::Malformed));

        let (server, client) = seqpacket_pair()?;
        let oversized = vec![b' '; 20_000];
        rustix::net::send(&client, &oversized, rustix::net::SendFlags::empty())
            .map_err(ctx("send oversized"))?;
        handle_connection(&server, &warden);
        assert_eq!(reason(&read_reply(&client)?), Some(Denial::Malformed));
        Ok(())
    }
}

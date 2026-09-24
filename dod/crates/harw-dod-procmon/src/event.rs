//! Formungsteil: das `payload` eines Prozessstart-Ereignisses deuten.
//!
//! # Verantwortungsbereich
//! `harw_dod_bpf::event::parse_raw_event` deutet nur den gemeinsamen,
//! art-unabhängigen Kopf eines Ringpuffer-Eintrags (`pid`, Zeitstempel,
//! `comm`) und liefert den Rest roh als [`harw_dod_bpf::RawBpfEvent::payload`]
//! zurück — welche Bedeutung dieser Rest hat, ist laut dortiger Moduldoku
//! „Sache der Sensor-Crate, die dieses Ereignis konsumiert". Dieses Modul ist
//! genau das für Prozessstart-Ereignisse: [`parse_exec_payload`] deutet
//! `payload` als [`ExecEvent`], orientiert am `sched_process_exec`-Tracepoint.
//!
//! Wie beim Formungsteil von `harw-dod-bpf` sind [`parse_exec_payload`] und
//! seine Hilfsfunktionen reine Funktionen auf `&[u8]` — kein Kernel, keine
//! Berechtigung, keine eBPF-Toolchain nötig, um sie zu testen.
//!
//! # Das Payload-Format (art-spezifisch, `sched_process_exec`-orientiert)
//! Der reale `sched_process_exec`-Tracepoint trägt `pid` (die ausführende
//! Prozess-ID) und einen `__data_loc`-kodierten `filename`-String mit
//! variablem Offset — kein `ppid`, kein `uid`. Diese Crate braucht aber
//! beides für [`ExecEvent`] (siehe dortige Doku für die Begründung, warum
//! `argv` nur als Digest erscheint) und **`parse_exec_payload` erhält
//! ausschließlich `payload: &[u8]`** — keinen Zugriff auf den gemeinsamen
//! Kopf von [`harw_dod_bpf::RawBpfEvent`]. Das art-spezifische
//! eBPF-Programm, das diesen Puffer erzeugt (nicht Teil dieser Lieferung,
//! siehe `harw-dod-bpf`-Crate-Moduldoku, Abschnitt „Abweichung vom Auftrag"),
//! schreibt deshalb **alle** für [`ExecEvent`] nötigen Felder redundant in
//! `payload` — auch `pid` und `comm`, die im gemeinsamen Kopf bereits
//! vorhanden sind. Diese Crate validiert die beiden Kopien **nicht**
//! gegeneinander; sie liest ausschließlich die art-spezifische Kopie in
//! `payload`, damit [`parse_exec_payload`] bei Bedarf auch auf einem
//! isoliert gespeicherten oder wiedergegebenen `payload`-Ausschnitt allein
//! funktioniert, ohne den ursprünglichen Ringpuffer-Kopf zu benötigen.
//!
//! Alle Felder außer `argv` sind **gepackt, ohne Auffüllbytes** — das
//! erzeugende eBPF-Programm schreibt sie Byte für Byte in dieser
//! Reihenfolge (kein `#[repr(C)]`-Struct, das der Compiler ausrichten
//! könnte). Die Offsets unten sind deshalb Summen der vorherigen
//! Feldbreiten, nicht das Ergebnis einer C-Struktur-Ausrichtung:
//!
//! | Byte-Bereich (relativ zu `payload`) | Feld       | Breite  | Deutung                                                        |
//! |-------------------------------------:|------------|--------:|------------------------------------------------------------------|
//! | `0..4`                                | `pid`      | 4       | `u32`, Little-Endian — die ausführende Prozess-ID                |
//! | `4..8`                                | `ppid`     | 4       | `u32`, Little-Endian — die Eltern-Prozess-ID                     |
//! | `8..12`                               | `uid`      | 4       | `u32`, Little-Endian — die effektive UID zum Exec-Zeitpunkt       |
//! | `12..28`                              | `comm`     | 16      | `TASK_COMM_LEN`, nullterminiert **oder** pufferfüllend            |
//! | `28..284`                             | `filename` | 256     | nullterminiert **oder** pufferfüllend (siehe unten zur Wahl)      |
//! | `284..`                               | `argv`     | variabel | roh, üblicherweise NUL-getrennt wie `/proc/<pid>/cmdline`; **nur zur Digestbildung gelesen, nie gespeichert** |
//!
//! `comm` benutzt dieselbe Breite (16 Byte) wie der gemeinsame Kopf von
//! `harw-dod-bpf` (Linux' `TASK_COMM_LEN`) — dieselbe Kernel-Konvention, statt
//! einer zweiten, frei erfundenen Breite. `filename` benutzt **256 Byte**,
//! nicht den Kernel-eigenen `PATH_MAX` (4096 Byte): das ist eine bewusste,
//! **von dieser Crate gewählte** Breite für ihr eigenes Wire-Format, keine
//! vom Kernel vorgegebene Zahl. Ein künftiges eBPF-Programm, das längere
//! Pfade unterstützen soll, ändert `FILENAME_LEN` und damit dieses Format
//! — bewusst dokumentiert, statt eine `PATH_MAX`-Zahl zu übernehmen, die für
//! Testzwecke unnötig groß wäre.
//!
//! `argv` hat **keine** eigene Längenangabe: es ist genau der Rest von
//! `payload` nach `filename`. Das genügt, weil `argv` ohnehin nur zur
//! Digestbildung gelesen wird ([`harw_types::ContentDigest::of`]) — eine
//! Trennung in einzelne Argumente ist für diese Crate nicht nötig, siehe
//! [`ExecEvent::argv_digest`].
//!
//! # `argv_digest`: warum nie die rohe Kommandozeile
//! `harw_dod_signals::EventKind::ProcessExec` trägt `path` und
//! `argv_digest`, **nicht** die rohe Kommandozeile — deren eigene Doku
//! begründet das so: eine Kommandozeile ist angreiferkontrolliert und
//! enthält regelmäßig Geheimnisse (Zugangstoken als Argument, eingebettete
//! Passwörter). Ein Digest belegt Gleichheit — zwei Ereignisse hatten
//! dieselbe Kommandozeile — ohne den Inhalt selbst zu transportieren oder
//! dauerhaft zu speichern. [`parse_exec_payload`] liest die `argv`-Bytes
//! genau einmal, ausschließlich um [`harw_types::ContentDigest::of`] darauf
//! aufzurufen; die gelesene Byte-Ansicht (`&[u8]`) ist eine geliehene
//! Teilansicht von `payload` und existiert nie als eigenständiger,
//! gespeicherter Wert — nach dieser Funktion gibt es keine Kopie der rohen
//! `argv`-Bytes mehr, weder in [`ExecEvent`] noch in [`crate::error::ProcmonError`].
//! Ein Test in diesem Modul verifiziert das strukturell über die
//! `Debug`-Ausgabe des Ergebnisses (siehe `test_argv_bytes_never_appear_*`
//! unten) — nicht nur behauptet in Prosa.
//!
//! # Byte-Leser: wiederverwendet, nicht neu geschrieben
//! [`parse_exec_payload`] liest ausschließlich über
//! `harw_dod_bpf::event::read_u32_le` und `harw_dod_bpf::event::read_fixed_c_str`
//! — dieselben Grundfunktionen, die `harw-dod-bpf` für den gemeinsamen Kopf
//! benutzt. Kein eigener Byte-Leser in dieser Crate.
//!
//! # Exportierte Typen und Funktionen
//! [`ExecEvent`], [`parse_exec_payload`].
//!
//! # Nebenläufigkeit
//! [`parse_exec_payload`] ist zustandslos und `Send + Sync`-frei aus
//! beliebig vielen Threads aufrufbar.
//!
//! # Fehler
//! [`crate::error::ProcmonError::MalformedEvent`] für jeden Puffer, der
//! kürzer als der feste Kopfbereich (`pid`/`ppid`/`uid`/`comm`/`filename`,
//! 284 Byte) ist — **inhaltsfrei**, siehe dortige Doku.
//!
//! # Examples
//! ```rust
//! use harw_dod_procmon::event::parse_exec_payload;
//!
//! let mut payload = Vec::new();
//! payload.extend_from_slice(&4_242u32.to_le_bytes()); // pid
//! payload.extend_from_slice(&1u32.to_le_bytes()); // ppid
//! payload.extend_from_slice(&0u32.to_le_bytes()); // uid
//!
//! let mut comm = [0u8; 16];
//! comm[..4].copy_from_slice(b"sshd");
//! payload.extend_from_slice(&comm);
//!
//! let mut filename = [0u8; 256];
//! filename[.."/usr/sbin/sshd".len()].copy_from_slice(b"/usr/sbin/sshd");
//! payload.extend_from_slice(&filename);
//!
//! payload.extend_from_slice(b"-D"); // argv: wird nur zum Digest gehasht, nie gespeichert
//!
//! let event = parse_exec_payload(&payload).expect("well-formed fixture buffer");
//! assert_eq!(event.pid, 4_242);
//! assert_eq!(event.comm, "sshd");
//! assert_eq!(event.filename, "/usr/sbin/sshd");
//! ```

use harw_dod_bpf::event::{read_fixed_c_str, read_u32_le};
use harw_dod_bpf::{TaskIdentity, WireEvent, WireEventType};
use harw_types::ContentDigest;

use crate::error::ProcmonError;

/// Offset von `pid` innerhalb von `payload`.
const PID_OFFSET: usize = 0;
/// Offset von `ppid` innerhalb von `payload`.
const PPID_OFFSET: usize = 4;
/// Offset von `uid` innerhalb von `payload`.
const UID_OFFSET: usize = 8;
/// Offset von `comm` innerhalb von `payload`.
const COMM_OFFSET: usize = 12;
/// Feste Breite des `comm`-Feldes (Linux' `TASK_COMM_LEN`, wie im
/// gemeinsamen Kopf von `harw-dod-bpf`).
const COMM_LEN: usize = 16;
/// Offset von `filename` innerhalb von `payload` (`COMM_OFFSET + COMM_LEN`).
const FILENAME_OFFSET: usize = COMM_OFFSET + COMM_LEN;
/// Feste Breite des `filename`-Feldes — eine von dieser Crate gewählte
/// Breite, nicht der Kernel-eigene `PATH_MAX` (siehe Moduldoku).
const FILENAME_LEN: usize = 256;
/// Offset von `argv` innerhalb von `payload` (`FILENAME_OFFSET + FILENAME_LEN`).
const ARGV_OFFSET: usize = FILENAME_OFFSET + FILENAME_LEN;

/// Ein aus dem `payload` eines Prozessstart-Ereignisses gedeutetes Ereignis.
///
/// # Description
/// Entsteht ausschließlich über [`parse_exec_payload`]. `argv_digest` belegt
/// Gleichheit zweier Kommandozeilen, ohne ihren Inhalt zu tragen — siehe
/// Moduldoku, Abschnitt „`argv_digest`: warum nie die rohe Kommandozeile".
///
/// # Errors
/// Keine eigenen Fehler — entsteht ausschließlich über [`parse_exec_payload`].
///
/// # Examples
/// ```rust
/// use harw_dod_procmon::ExecEvent;
/// use harw_types::ContentDigest;
///
/// let event = ExecEvent {
///     pid: 4_242,
///     ppid: 1,
///     uid: 0,
///     comm: "sshd".to_owned(),
///     filename: "/usr/sbin/sshd".to_owned(),
///     argv_digest: ContentDigest::of(b"unused in this example"),
/// };
/// assert_eq!(event.pid, 4_242);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ExecEvent {
    /// Die ausführende Prozess-ID.
    pub pid: u32,
    /// Die Eltern-Prozess-ID.
    pub ppid: u32,
    /// Die effektive UID zum Exec-Zeitpunkt.
    pub uid: u32,
    /// Der Prozessname (`comm`), gelesen über `read_fixed_c_str`.
    pub comm: String,
    /// Der Pfad des ausgeführten Programms.
    pub filename: String,
    /// Digest der Kommandozeile (`argv`) — **nie die Kommandozeile selbst**.
    /// Siehe Moduldoku für die Begründung.
    pub argv_digest: ContentDigest,
}

/// v1 never reads arguments or environment.  This is a positive state, not
/// a digest of an empty buffer (which would falsely look like evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgvCapture {
    NotCollected,
}

/// Whether the bounded executable path was captured.  A missing path is not
/// converted into an empty string: that would make a read failure look like
/// evidence of an executable with an empty path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutablePathCapture {
    Captured,
    PossiblyTruncated,
    Unavailable,
}

/// Versioned exec payload decoded from [`WireEventType::Exec`].  Task
/// identity is supplied by the common wire header, while the payload is
/// explicitly `comm[16] | path_len:u16-le | path[path_len]`; no padding and
/// no implicit C/Rust layout are involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecEventV1 {
    pub task: TaskIdentity,
    pub comm: String,
    pub path: Option<String>,
    pub path_capture: ExecutablePathCapture,
    pub argv: ArgvCapture,
}

/// A process exit carries identity only in v1.  It deliberately does not
/// repeat a path or argv from an earlier exec: a PID can be recycled and an
/// inferred association would be less honest than an absent field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessExitEventV1 {
    pub task: TaskIdentity,
}

/// Bit in the shared wire flags byte: the bounded executable path did not
/// fit in the BPF buffer.  It never applies to argv because argv is absent.
pub const EXEC_PATH_TRUNCATED: u8 = 0x01;
/// The kernel could not safely read the tracepoint-provided executable path.
/// It is a capture status, not an empty path.
pub const EXEC_PATH_UNAVAILABLE: u8 = 0x02;

/// Parse the versioned exec payload and reject a record of another kind.
pub fn parse_exec_v1(event: &WireEvent) -> Result<ExecEventV1, ProcmonError> {
    if event.event_type != WireEventType::Exec
        || event.payload.len() < 18
        || event.flags & !(EXEC_PATH_TRUNCATED | EXEC_PATH_UNAVAILABLE) != 0
        || event.flags & (EXEC_PATH_TRUNCATED | EXEC_PATH_UNAVAILABLE)
            == (EXEC_PATH_TRUNCATED | EXEC_PATH_UNAVAILABLE)
    {
        return Err(ProcmonError::MalformedEvent);
    }
    let comm = read_fixed_c_str(&event.payload, 0, 16).map_err(|_| ProcmonError::MalformedEvent)?;
    let path_len_bytes: [u8; 2] = event.payload[16..18]
        .try_into()
        .map_err(|_| ProcmonError::MalformedEvent)?;
    let path_len = usize::from(u16::from_le_bytes(path_len_bytes));
    let path_end = 18usize
        .checked_add(path_len)
        .ok_or(ProcmonError::MalformedEvent)?;
    if path_end != event.payload.len() {
        return Err(ProcmonError::MalformedEvent);
    }
    let (path, path_capture) = if event.flags & EXEC_PATH_UNAVAILABLE != 0 {
        if path_len != 0 {
            return Err(ProcmonError::MalformedEvent);
        }
        (None, ExecutablePathCapture::Unavailable)
    } else if event.flags & EXEC_PATH_TRUNCATED != 0 {
        (
            Some(String::from_utf8_lossy(&event.payload[18..path_end]).into_owned()),
            ExecutablePathCapture::PossiblyTruncated,
        )
    } else {
        (
            Some(String::from_utf8_lossy(&event.payload[18..path_end]).into_owned()),
            ExecutablePathCapture::Captured,
        )
    };
    Ok(ExecEventV1 {
        task: event.task,
        comm,
        path,
        path_capture,
        argv: ArgvCapture::NotCollected,
    })
}

/// Parse the empty v1 exit body.  The task/cgroup identity belongs to the
/// common header and is captured at `sched_process_exit` in task context.
pub fn parse_process_exit_v1(event: &WireEvent) -> Result<ProcessExitEventV1, ProcmonError> {
    if event.event_type != WireEventType::ProcessExit
        || !event.payload.is_empty()
        || event.flags != 0
    {
        return Err(ProcmonError::MalformedEvent);
    }
    Ok(ProcessExitEventV1 { task: event.task })
}

/// Deutet das `payload` eines Prozessstart-Ereignisses als [`ExecEvent`].
///
/// # Description
/// Liest die festen Felder (`pid`, `ppid`, `uid`, `comm`, `filename`) über
/// die Byte-Leser aus `harw-dod-bpf` und bildet den verbleibenden Rest
/// (`argv`) ausschließlich zu einem [`harw_types::ContentDigest`] — die
/// rohen `argv`-Bytes werden nach diesem Aufruf an keiner Stelle mehr
/// gehalten (siehe Moduldoku).
///
/// # Arguments
/// - `payload` (`&[u8]`): `harw_dod_bpf::RawBpfEvent::payload` eines
///   Prozessstart-Ereignisses — der art-spezifische Rest hinter dem
///   gemeinsamen Kopf, den `harw_dod_bpf::event::parse_raw_event` bereits
///   abgetrennt hat.
///
/// # Returns
/// Das gedeutete [`ExecEvent`].
///
/// # Errors
/// - [`ProcmonError::MalformedEvent`], wenn `payload` kürzer als der feste
///   Feldbereich ist (284 Byte: `pid` + `ppid` + `uid` + `comm` + `filename`).
///   **Inhaltsfrei:** die Fehlermeldung nennt nie die Länge oder den Inhalt
///   von `payload`, insbesondere nie ein Byte von `argv`.
///
/// # Examples
/// ```rust
/// use harw_dod_procmon::error::ProcmonError;
/// use harw_dod_procmon::event::parse_exec_payload;
///
/// assert!(matches!(parse_exec_payload(&[0u8; 4]), Err(ProcmonError::MalformedEvent)));
/// ```
pub fn parse_exec_payload(payload: &[u8]) -> Result<ExecEvent, ProcmonError> {
    let pid = read_u32_le(payload, PID_OFFSET).map_err(|_| ProcmonError::MalformedEvent)?;
    let ppid = read_u32_le(payload, PPID_OFFSET).map_err(|_| ProcmonError::MalformedEvent)?;
    let uid = read_u32_le(payload, UID_OFFSET).map_err(|_| ProcmonError::MalformedEvent)?;
    let comm = read_fixed_c_str(payload, COMM_OFFSET, COMM_LEN)
        .map_err(|_| ProcmonError::MalformedEvent)?;
    let filename = read_fixed_c_str(payload, FILENAME_OFFSET, FILENAME_LEN)
        .map_err(|_| ProcmonError::MalformedEvent)?;

    // `argv` ist der Rest von `payload` — eine geliehene Teilansicht, nie
    // kopiert oder gespeichert. `ContentDigest::of` hasht die Bytes und gibt
    // einen 32-Byte-Digest zurück; danach existiert keine Ansicht der rohen
    // `argv`-Bytes mehr.
    let argv = payload
        .get(ARGV_OFFSET..)
        .ok_or(ProcmonError::MalformedEvent)?;
    let argv_digest = ContentDigest::of(argv);

    Ok(ExecEvent {
        pid,
        ppid,
        uid,
        comm,
        filename,
        argv_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ARGV_OFFSET, ArgvCapture, COMM_LEN, EXEC_PATH_TRUNCATED, ExecutablePathCapture,
        FILENAME_LEN, parse_exec_payload, parse_exec_v1,
    };
    use crate::error::ProcmonError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_bpf::{TaskIdentity, WireEvent, WireEventType};
    use harw_types::ContentDigest;

    #[test]
    fn v1_exec_has_explicit_path_length_and_never_fabricates_argv_evidence() -> TestResult {
        // Wire layout (bpf/include/harw_dod_wire_v1.h, `hwd_emit_exec`):
        // `comm[16] | path_len:u16-le | path[path_len]`, where `path_len` is the
        // byte count without the C NUL and the payload ends exactly after the path.
        let path: &[u8] = b"/bin/bash";
        let path_len = u16::try_from(path.len()).map_err(ctx("test path fits in u16"))?;
        let mut payload = vec![0; 18];
        payload[..4].copy_from_slice(b"bash");
        payload[16..18].copy_from_slice(&path_len.to_le_bytes());
        payload.extend_from_slice(path);
        let event = WireEvent {
            event_type: WireEventType::Exec,
            flags: EXEC_PATH_TRUNCATED,
            ktime_ns: 1,
            sequence: 1,
            task: TaskIdentity {
                tgid: 1,
                pid: 2,
                ppid: 3,
                uid: 4,
                cgroup_id: 5,
            },
            payload,
        };
        let parsed = parse_exec_v1(&event).map_err(ctx("well-formed fixture buffer"))?;
        assert_eq!(parsed.path.as_deref(), Some("/bin/bash"));
        assert_eq!(
            parsed.path_capture,
            ExecutablePathCapture::PossiblyTruncated
        );
        assert_eq!(parsed.argv, ArgvCapture::NotCollected);
        Ok(())
    }

    #[test]
    fn v1_exit_keeps_the_exiting_tasks_identity_without_fabricating_exec_fields() -> TestResult {
        let event = WireEvent {
            event_type: WireEventType::ProcessExit,
            flags: 0,
            ktime_ns: 1,
            sequence: 2,
            task: TaskIdentity {
                tgid: 7,
                pid: 8,
                ppid: 6,
                uid: 1000,
                cgroup_id: 99,
            },
            payload: vec![],
        };
        let parsed =
            super::parse_process_exit_v1(&event).map_err(ctx("well-formed fixture buffer"))?;
        assert_eq!(parsed.task.pid, 8);
        assert_eq!(parsed.task.cgroup_id, 99);
        Ok(())
    }

    /// Baut einen wohlgeformten `payload`-Puffer aus seinen Feldern.
    ///
    /// `comm` und `filename` müssen bereits exakt [`COMM_LEN`] bzw.
    /// [`FILENAME_LEN`] Byte breit sein (mit Füllbytes oder vollständig
    /// ausfüllend) — genau wie ein reales eBPF-Programm ein festes Feld
    /// schreiben würde.
    fn build_payload(
        pid: u32,
        ppid: u32,
        uid: u32,
        comm: &[u8],
        filename: &[u8],
        argv: &[u8],
    ) -> Vec<u8> {
        assert_eq!(comm.len(), COMM_LEN);
        assert_eq!(filename.len(), FILENAME_LEN);

        let mut payload = Vec::with_capacity(ARGV_OFFSET + argv.len());
        payload.extend_from_slice(&pid.to_le_bytes());
        payload.extend_from_slice(&ppid.to_le_bytes());
        payload.extend_from_slice(&uid.to_le_bytes());
        payload.extend_from_slice(comm);
        payload.extend_from_slice(filename);
        payload.extend_from_slice(argv);
        payload
    }

    /// Ein `comm`/`filename`-Feld mit Inhalt am Anfang, nullterminiert,
    /// gefüllt mit Nullbytes bis zur geforderten Breite.
    fn nul_terminated_field(content: &[u8], width: usize) -> Vec<u8> {
        let mut field = vec![0u8; width];
        field[..content.len()].copy_from_slice(content);
        field
    }

    /// Ein `comm`/`filename`-Feld, das die geforderte Breite exakt und ohne
    /// jedes Nullbyte ausfüllt — der Fall ohne Terminator (siehe
    /// `harw_dod_bpf::event::read_fixed_c_str`-Doku).
    fn buffer_filling_field(fill: u8, width: usize) -> Vec<u8> {
        vec![fill; width]
    }

    #[test]
    fn test_parse_exec_payload_decodes_a_well_formed_buffer() -> TestResult {
        let comm = nul_terminated_field(b"sshd", COMM_LEN);
        let filename = nul_terminated_field(b"/usr/sbin/sshd", FILENAME_LEN);
        let payload = build_payload(4_242, 1, 0, &comm, &filename, b"-D\0-e\0none");

        let event = parse_exec_payload(&payload).map_err(ctx("well-formed payload must parse"))?;
        assert_eq!(event.pid, 4_242);
        assert_eq!(event.ppid, 1);
        assert_eq!(event.uid, 0);
        assert_eq!(event.comm, "sshd");
        assert_eq!(event.filename, "/usr/sbin/sshd");
        assert_eq!(event.argv_digest, ContentDigest::of(b"-D\0-e\0none"));
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_honors_byte_order_for_a_multi_byte_pid() -> TestResult {
        // `pid` und `ppid` über 255 brauchen mindestens zwei Bytes — ein
        // Wert unter 256 könnte eine vertauschte Byte-Reihenfolge nicht
        // erkennen (siehe `harw-dod-bpf`-Testmuster).
        let comm = nul_terminated_field(b"x", COMM_LEN);
        let filename = nul_terminated_field(b"/bin/x", FILENAME_LEN);
        let payload = build_payload(70_000, 300, 1_000, &comm, &filename, b"");

        let event = parse_exec_payload(&payload).map_err(ctx("well-formed payload must parse"))?;
        assert_eq!(event.pid, 70_000);
        assert_eq!(event.ppid, 300);
        assert_eq!(event.uid, 1_000);
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_comm_fills_field_completely_without_terminator() -> TestResult {
        let comm = buffer_filling_field(b'c', COMM_LEN);
        let filename = nul_terminated_field(b"/bin/true", FILENAME_LEN);
        let payload = build_payload(1, 0, 0, &comm, &filename, b"");

        let event = parse_exec_payload(&payload).map_err(ctx("well-formed payload must parse"))?;
        assert_eq!(event.comm.len(), COMM_LEN);
        assert_eq!(event.comm, "c".repeat(COMM_LEN));
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_filename_fills_field_completely_without_terminator() -> TestResult {
        let comm = nul_terminated_field(b"x", COMM_LEN);
        let filename = buffer_filling_field(b'f', FILENAME_LEN);
        let payload = build_payload(1, 0, 0, &comm, &filename, b"");

        let event = parse_exec_payload(&payload).map_err(ctx("well-formed payload must parse"))?;
        assert_eq!(event.filename.len(), FILENAME_LEN);
        assert_eq!(event.filename, "f".repeat(FILENAME_LEN));
        Ok(())
    }

    #[test]
    fn test_same_argv_yields_same_digest() -> TestResult {
        let comm = nul_terminated_field(b"a", COMM_LEN);
        let filename = nul_terminated_field(b"/bin/a", FILENAME_LEN);
        let a = parse_exec_payload(&build_payload(1, 0, 0, &comm, &filename, b"--flag=value"))
            .map_err(ctx("well-formed payload must parse"))?;
        let b = parse_exec_payload(&build_payload(2, 0, 0, &comm, &filename, b"--flag=value"))
            .map_err(ctx("well-formed payload must parse"))?;

        assert_eq!(a.argv_digest, b.argv_digest);
        Ok(())
    }

    #[test]
    fn test_different_argv_yields_different_digest() -> TestResult {
        let comm = nul_terminated_field(b"a", COMM_LEN);
        let filename = nul_terminated_field(b"/bin/a", FILENAME_LEN);
        let a = parse_exec_payload(&build_payload(1, 0, 0, &comm, &filename, b"--flag=one"))
            .map_err(ctx("well-formed payload must parse"))?;
        let b = parse_exec_payload(&build_payload(1, 0, 0, &comm, &filename, b"--flag=two"))
            .map_err(ctx("well-formed payload must parse"))?;

        assert_ne!(a.argv_digest, b.argv_digest);
        Ok(())
    }

    #[test]
    fn test_argv_bytes_never_appear_in_debug_output_of_the_result() -> TestResult {
        // Der wichtigste Test dieser Crate: eine Kommandozeile mit einem
        // eingebetteten Geheimnis darf an keiner Stelle des Ergebnisses
        // auftauchen — nicht als Feld, nicht über `Debug`. Geprüft über die
        // `Debug`-Ausgabe (Stellvertreter für „serialisiertes Ergebnis"),
        // nicht nur behauptet.
        let secret = b"--token=SuperSecretSharedToken123!";
        let comm = nul_terminated_field(b"curl", COMM_LEN);
        let filename = nul_terminated_field(b"/usr/bin/curl", FILENAME_LEN);
        let payload = build_payload(1, 0, 0, &comm, &filename, secret);

        let event = parse_exec_payload(&payload).map_err(ctx("well-formed payload must parse"))?;
        let debug_output = format!("{event:?}");

        assert!(!debug_output.contains("SuperSecretSharedToken123"));
        assert!(!debug_output.contains("--token="));
        // Auch der Digest selbst (Hex-Text) darf den Klartext nicht enthalten
        // — trivial für einen kryptografischen Hash, aber ein struktureller
        // Regressionsschutz, sollte sich das jemals ändern.
        assert!(
            !event
                .argv_digest
                .to_string()
                .contains("SuperSecretSharedToken123")
        );
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_too_short_buffer_returns_malformed_event_without_panicking()
    -> TestResult {
        let Err(err) = parse_exec_payload(&[1, 2, 3]) else {
            return Err(TestError::Unexpected(
                "a 3-byte buffer is far too short".into(),
            ));
        };
        assert!(matches!(err, ProcmonError::MalformedEvent));
        // Inhaltsfrei: die Meldung ist ein fester String, kann die Rohbytes
        // strukturell nicht enthalten.
        assert_eq!(err.to_string(), "process-exec payload buffer is malformed");
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_empty_buffer_returns_malformed_event_without_panicking() -> TestResult
    {
        let Err(err) = parse_exec_payload(&[]) else {
            return Err(TestError::Unexpected(
                "an empty buffer must not panic".into(),
            ));
        };
        assert!(matches!(err, ProcmonError::MalformedEvent));
        Ok(())
    }

    #[test]
    fn test_parse_exec_payload_one_byte_short_of_filename_returns_malformed_event() -> TestResult {
        // `ARGV_OFFSET - 1` Byte: das `filename`-Feld reicht gerade nicht
        // vollständig in den Puffer.
        let payload = vec![0u8; ARGV_OFFSET - 1];
        let Err(err) = parse_exec_payload(&payload) else {
            return Err(TestError::Unexpected(
                "truncated filename field must fail".into(),
            ));
        };
        assert!(matches!(err, ProcmonError::MalformedEvent));
        Ok(())
    }
}

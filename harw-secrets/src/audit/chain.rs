//! Hash-chaining over the audit event log (spec §4.2–§4.3). This is the one
//! place correctness matters most: any two serializations of "the same" event
//! that hash differently break the chain, so [`canonical_bytes`] is a fixed,
//! deterministic, length-prefixed encoding with no map types.
//!
//! Pure logic (canonical serialization, SHA-256 chaining, front-to-back
//! verification) is implemented in full over in-memory events.
//! `SecretStore::persist_audit_state` (`store.rs`) does the atomic
//! write-temp-then-rename persistence; [`load_and_verify_persisted_chain`]
//! is the matching on-disk reader, decoding exactly the byte layout that
//! writer produces rather than re-deriving a format of its own.
//!
//! # Warum ein separater Lader statt eines automatischen Ladens in `open`
//! `SecretStore::open`/`open_with_key_material` beginnen absichtlich mit
//! einer leeren `AuditLog` (§ Moduldoku in `store.rs`): die persistierte
//! Kette dient hier ausschließlich einer externen Manipulationsprüfung (z. B.
//! alle 15 Minuten durch das Gateway), nicht dem In-Prozess-Betrieb. Ein
//! automatisches Laden bei jedem `open` würde jede Geheimnisoperation mit dem
//! Einlesen und erneuten Hashen der kompletten Audit-Historie belasten, obwohl
//! kein Aufrufer im normalen Betrieb die persistierte Kette braucht — nur die
//! separate, seltene Prüfung tut das. Deshalb ist [`load_and_verify_persisted_chain`]
//! ein bewusst getrennter, ausdrücklich aufzurufender Weg
//! (`SecretStore::verify_persisted_audit_chain` in `store.rs`), keine
//! Erweiterung von `open`.
//!
//! # Die drei Fälle, die dieser Lader nie verwechselt
//! - **Nichts da** ([`PersistedChainStatus::Absent`]): `audit.log` existiert
//!   nicht. Das ist kein Fund — ein frisch angelegter Store, der noch nie
//!   `create`/`rotate`/`delete` durchlaufen hat, sieht genauso aus.
//! - **Unlesbar** (`Err(AuditError::Io(_))`): der Pfad ist keine reguläre
//!   Datei (Symlink/Verzeichnis werden abgelehnt, nie aufgelöst), oder die
//!   Bytes lassen sich nicht als dieses Format dekodieren (falsche Kennung,
//!   nicht unterstützte Version, abgeschnittenes oder überlanges
//!   längenpräfixiertes Feld, unbekanntes Actor-Diskriminanten-Byte, ungültiger
//!   `recorded_at`-Zeitstempel, Datenrest nach der angegebenen Ereigniszahl,
//!   oder eine redundante Zweitkodierung, die nicht zur Primärkodierung
//!   passt). Das sagt nichts darüber aus, ob die Historie, die die Datei
//!   *eigentlich* enthalten sollte, unversehrt wäre — die Datei lässt sich
//!   schlicht nicht als dieses Format lesen.
//! - **Manipuliert** (`Err(AuditError::ChainBroken { .. })`): die Datei lässt
//!   sich sauber als dieses Format parsen, aber der `prev_hash` eines
//!   Ereignisses stimmt nicht mit dem neu berechneten Hash seines Vorgängers
//!   überein. Nur das nennt dieser Lader einen tatsächlichen Kettenbruch.
//!
//! Bei jedem Zweifel meldet diese Funktion **nicht** `Intact`: ein
//! Parse-Fehler und ein gebrochener Verweis sind beides `Err`, nie ein
//! stillschweigendes „kein Problem gefunden".
//!
//! # Was dieser Lader nicht entdecken kann
//! Geprüft wird nur die innere Konsistenz: hängt die Datei, als Ganzes
//! gelesen, korrekt zusammen? Ein Angreifer, der `audit.log` **vollständig
//! und konsistent neu schreibt** — mit einer anderen, aber in sich stimmigen
//! Kette samt korrekter Hashes und einem bei der Genesis verankerten ersten
//! `prev_hash` — besteht diese Prüfung. Das zu erkennen braucht einen
//! externen Anker, gegen den die verifizierte Kette geprüft wird (ein
//! Spiegel, ein anderswo aufbewahrter signierter Checkpoint, ...); dieser
//! Lader hat keinen solchen Anker und behauptet auch keinen.

use std::path::Path;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::audit::event::{Actor, AuditEvent, AuditEventId, SubjectRef};
use crate::error::{AuditError, AuditResult};

/// Fixed 11-byte header identifying a persisted `audit.log` (§ store.rs
/// `persist_audit_state`). Owned here so the writer (`store.rs`) and this
/// reader share one literal definition instead of two copies that could
/// drift apart.
pub(crate) const AUDIT_MAGIC: &[u8] = b"HARW-AUDIT\0";

/// The only persisted audit-log format version this crate currently writes
/// or reads.
pub(crate) const AUDIT_FORMAT_VERSION: u32 = 1;

/// The genesis `prev_hash`: 32 zero bytes.
pub const GENESIS_HASH: [u8; 32] = [0u8; 32];

/// Append a `u64` big-endian length prefix followed by `bytes`.
fn write_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Deterministic, length-prefixed serialization of an event for hashing (§4.2).
/// Field order is fixed; there are no maps. `prev_hash` is included so altering
/// a predecessor changes every downstream hash.
#[must_use]
pub fn canonical_bytes(event: &AuditEvent) -> Vec<u8> {
    let mut out = Vec::new();

    // id: 16 raw UUID bytes (fixed width, no prefix needed).
    out.extend_from_slice(event.id.as_bytes());

    // actor: 1-byte discriminant then any payload.
    match &event.actor {
        Actor::Operator(name) => {
            out.push(0u8);
            write_len_prefixed(&mut out, name.as_bytes());
        }
        Actor::Agent { session_id } => {
            out.push(1u8);
            write_len_prefixed(&mut out, session_id.as_bytes());
        }
        Actor::System => out.push(2u8),
    }

    // action.
    write_len_prefixed(&mut out, event.action.as_bytes());

    // subjects: count then each (kind, id).
    out.extend_from_slice(&(event.subjects.len() as u64).to_be_bytes());
    for subject in &event.subjects {
        write_len_prefixed(&mut out, subject.kind.as_bytes());
        write_len_prefixed(&mut out, subject.id.as_bytes());
    }

    // recorded_at: whole seconds (i64) then sub-second nanos (i32), big-endian.
    out.extend_from_slice(&event.recorded_at.as_second().to_be_bytes());
    out.extend_from_slice(&event.recorded_at.subsec_nanosecond().to_be_bytes());

    // prev_hash: 32 raw bytes.
    out.extend_from_slice(&event.prev_hash);

    out
}

/// SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// An in-memory, append-only, hash-chained audit log.
#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvent>,
    head: [u8; 32],
}

impl AuditLog {
    /// A fresh log with a genesis chain head.
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            head: GENESIS_HASH,
        }
    }

    /// The current chain-head hash (SHA-256 of the last event, or genesis).
    #[must_use]
    pub fn chain_head(&self) -> [u8; 32] {
        self.head
    }

    /// Number of events appended.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The recorded events in append order.
    #[must_use]
    pub fn events(&self) -> &[AuditEvent] {
        &self.events
    }

    /// Append a new event linked to the current chain head, advancing the head
    /// (§4.2). The new event's `recorded_at` is `Timestamp::now()`.
    pub fn append(&mut self, actor: Actor, action: &str, subjects: Vec<SubjectRef>) -> &AuditEvent {
        let event = AuditEvent {
            id: AuditEventId::new(),
            actor,
            action: action.to_owned(),
            subjects,
            recorded_at: Timestamp::now(),
            prev_hash: self.head,
        };
        self.head = sha256(&canonical_bytes(&event));
        self.events.push(event);
        let last = self.events.len() - 1;
        &self.events[last]
    }

    /// Test-only reconstruction from a caller-supplied event list, without
    /// recomputing or validating `prev_hash` links. Lets `audit::mirror`'s
    /// tests synthesize a chain with a deliberately tampered predecessor hash
    /// without exposing mutable internals (`events`/`head` stay private) in
    /// production code.
    #[cfg(test)]
    pub(crate) fn from_raw_events_for_test(events: Vec<AuditEvent>) -> Self {
        let head = events
            .last()
            .map(|event| sha256(&canonical_bytes(event)))
            .unwrap_or(GENESIS_HASH);
        Self { events, head }
    }

    /// Walk the chain front-to-back, confirming each event's `prev_hash`
    /// matches the recomputed hash of its predecessor (§4.3 step 1). Reports the
    /// offending index on the first mismatch.
    pub fn verify(&self) -> AuditResult<()> {
        let mut prev = GENESIS_HASH;
        for (index, event) in self.events.iter().enumerate() {
            if event.prev_hash != prev {
                return Err(AuditError::ChainBroken {
                    index: index as u64,
                    expected: prev,
                    found: event.prev_hash,
                });
            }
            prev = sha256(&canonical_bytes(event));
        }
        Ok(())
    }
}

/// Outcome of loading and verifying the persisted audit chain (§4.3 step 1)
/// from disk, independent of any `AuditLog` a live [`crate::store::SecretStore`]
/// happens to hold in memory. See the module docs for what each variant does
/// and does not mean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistedChainStatus {
    /// No audit log file exists at the checked path. Not a chain break: a
    /// store that has never durably mutated anything looks exactly like
    /// this.
    Absent,
    /// The file was read, its framing parsed, and every event's `prev_hash`
    /// matched the recomputed hash of its predecessor.
    Intact {
        /// Number of events the persisted chain contains.
        event_count: u64,
        /// The verified chain-head hash ([`GENESIS_HASH`] when `event_count`
        /// is zero).
        chain_head: [u8; 32],
    },
}

/// Build a generic, content-free [`AuditError::Io`] for a framing/parse
/// failure. `reason` must never include event content, actor identity, or
/// key material — only a fixed description of which structural check failed.
fn malformed_persisted_chain(reason: &'static str) -> AuditError {
    AuditError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
}

/// Bounds-checked reader over the persisted audit log bytes. Every accessor
/// refuses immediately once fewer bytes remain than requested instead of
/// slicing out of range, so an attacker-controlled length prefix inside a
/// record can never panic this parser or drive an allocation past what the
/// file itself actually contains (every returned slice borrows straight out
/// of the input; nothing is copied until a fixed-size array is assembled).
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, len: usize) -> AuditResult<&'a [u8]> {
        if len > self.remaining() {
            return Err(malformed_persisted_chain("audit log record is truncated"));
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn take_u32_be(&mut self) -> AuditResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn take_u64_be(&mut self) -> AuditResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn take_i32_be(&mut self) -> AuditResult<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn take_i64_be(&mut self) -> AuditResult<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn take_hash(&mut self) -> AuditResult<[u8; 32]> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// A `u64`-length-prefixed field. The length is bounded by `take`
    /// against the bytes actually remaining, so a huge attacker-supplied
    /// length simply fails fast rather than allocating.
    fn take_len_prefixed(&mut self) -> AuditResult<&'a [u8]> {
        let len = self.take_u64_be()?;
        let len = usize::try_from(len).map_err(|_| {
            malformed_persisted_chain("audit log record length prefix is not addressable")
        })?;
        self.take(len)
    }
}

/// Read one record's actor discriminant and payload, matching
/// [`canonical_bytes`]'s encoding exactly. The decoded name/session id is
/// discarded (never turned into a `String`): verification only needs the
/// byte span consumed, not the actor's identity.
fn read_actor(cursor: &mut Cursor<'_>) -> AuditResult<()> {
    let tag = cursor.take(1)?[0];
    match tag {
        0 => {
            cursor.take_len_prefixed()?;
        }
        1 => {
            cursor.take_len_prefixed()?;
        }
        2 => {}
        _ => {
            return Err(malformed_persisted_chain(
                "audit log record has an unrecognized actor discriminant",
            ));
        }
    }
    Ok(())
}

/// Read one persisted record: [`canonical_bytes`]'s field layout (id, actor,
/// action, subjects, `recorded_at`, `prev_hash`) immediately followed by a
/// redundant second encoding of every field but the id, exactly as
/// `SecretStore::persist_audit_state` writes it. Returns the record's
/// `prev_hash` field and `sha256` of its primary (canonical) bytes — the two
/// values chain verification compares.
///
/// The redundant trailer is checked by raw byte comparison against the
/// primary encoding rather than re-parsed field by field: it must reproduce
/// `primary[16..]` exactly. Tampering that touches only the trailer — which
/// the hash-chain check alone would never see, since only the primary bytes
/// feed `sha256` — is caught here instead.
fn read_persisted_record(cursor: &mut Cursor<'_>) -> AuditResult<([u8; 32], [u8; 32])> {
    let start = cursor.pos;

    cursor.take(16)?; // id: opaque for verification purposes
    read_actor(cursor)?;
    cursor.take_len_prefixed()?; // action
    let subject_count = cursor.take_u64_be()?;
    for _ in 0..subject_count {
        cursor.take_len_prefixed()?; // subject kind
        cursor.take_len_prefixed()?; // subject id
    }
    let second = cursor.take_i64_be()?;
    let nanosecond = cursor.take_i32_be()?;
    Timestamp::new(second, nanosecond).map_err(|_| {
        malformed_persisted_chain("audit log record has an invalid recorded_at timestamp")
    })?;
    let prev_hash = cursor.take_hash()?;

    let end = cursor.pos;
    let primary = &cursor.data[start..end];
    let hash = sha256(primary);

    let trailer_len = end - start - 16;
    let trailer = cursor.take(trailer_len)?;
    if trailer != &primary[16..] {
        return Err(malformed_persisted_chain(
            "audit log record's redundant encoding does not match its primary encoding",
        ));
    }

    Ok((prev_hash, hash))
}

/// Lädt `path` (das durable `audit.log`, das ein [`crate::store::SecretStore`]
/// schreibt) und verifiziert dessen Hash-Kette lückenlos.
///
/// # Gelesenes Format
/// Liest genau das, was `SecretStore::persist_audit_state` schreibt: eine
/// feste 11-Byte-Kennung, eine big-endian `u32`-Formatversion (nur Version 1
/// wird akzeptiert), einen big-endian `u64`-Ereigniszähler, dann so viele
/// Datensätze. Jeder Datensatz ist [`canonical_bytes`]s Feldlayout (id, actor,
/// action, subjects, `recorded_at`, `prev_hash`) unmittelbar gefolgt von
/// einer redundanten Zweitkodierung aller Felder außer der id. Dieses Format
/// wird wörtlich aus dem Schreibpfad übernommen, nicht neu abgeleitet.
///
/// # Die drei Fälle
/// - **Nichts da** ([`PersistedChainStatus::Absent`]): die Datei existiert
///   nicht. Kein Fund — ein Store, der noch nie durabel mutiert hat, sieht
///   genauso aus.
/// - **Unlesbar** (`Err(`[`AuditError::Io`]`(_))`): der Pfad ist kein
///   reguläre Datei (ein Symlink oder Verzeichnis wird abgelehnt, nie
///   aufgelöst), oder die Bytes lassen sich nicht als dieses Format
///   dekodieren.
/// - **Manipuliert** (`Err(`[`AuditError::ChainBroken`]`{ .. })`): die Datei
///   parst sauber, aber der `prev_hash` eines Ereignisses stimmt nicht mit
///   dem neu berechneten Hash seines Vorgängers überein.
///
/// # Warum fail-closed
/// Kann diese Funktion nicht entscheiden, ob die Kette unversehrt ist,
/// meldet sie **nicht** [`PersistedChainStatus::Intact`]: ein Parse-Fehler
/// und ein gebrochener Verweis sind beides `Err`, nie ein stillschweigendes
/// „kein Problem gefunden".
///
/// # Was dies nicht entdecken kann
/// Geprüft wird nur die innere Konsistenz der Datei. Ein Angreifer, der
/// `audit.log` vollständig und konsistent neu schreibt — mit einer anderen,
/// aber in sich stimmigen, korrekt gehashten Kette — besteht diese Prüfung.
/// Das zu erkennen braucht einen externen Anker (Spiegel, extern
/// aufbewahrter Checkpoint, ...), den diese Funktion nicht hat und auch
/// nicht behauptet zu haben.
///
/// # Errors
/// - [`AuditError::Io`]: der Pfad ist keine reguläre Datei, oder seine Bytes
///   dekodieren nicht als dieses Format.
/// - [`AuditError::ChainBroken`]: die Datei dekodiert sauber, aber der
///   `prev_hash` eines Ereignisses stimmt nicht mit dem neu berechneten Hash
///   seines Vorgängers überein.
///
/// # Concurrency
/// Reine Berechnung über einmalig per `fs::read` gelesene Bytes; hält keine
/// Locks und startet keine Threads. Von jedem Thread aufrufbar, auch
/// parallel zu einem laufenden `SecretStore`, der dieselbe Datei mutiert —
/// der Read liefert eine Momentaufnahme, keine zerrissene Sicht.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_secrets::audit::chain::{PersistedChainStatus, load_and_verify_persisted_chain};
///
/// match load_and_verify_persisted_chain(Path::new("/var/lib/harw/secrets/audit.log")) {
///     Ok(PersistedChainStatus::Absent) => { /* nothing recorded yet */ }
///     Ok(PersistedChainStatus::Intact { event_count, .. }) => {
///         println!("audit chain intact over {event_count} events");
///     }
///     Err(error) => eprintln!("audit chain check failed: {error}"),
/// }
/// ```
pub fn load_and_verify_persisted_chain(path: &Path) -> AuditResult<PersistedChainStatus> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(malformed_persisted_chain(
                "audit log path exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedChainStatus::Absent);
        }
        Err(error) => return Err(AuditError::Io(error)),
    }

    let bytes = std::fs::read(path)?;
    let mut cursor = Cursor::new(&bytes);

    let magic = cursor.take(AUDIT_MAGIC.len())?;
    if magic != AUDIT_MAGIC {
        return Err(malformed_persisted_chain(
            "audit log file has an unrecognized header",
        ));
    }
    let version = cursor.take_u32_be()?;
    if version != AUDIT_FORMAT_VERSION {
        return Err(malformed_persisted_chain(
            "audit log file has an unsupported format version",
        ));
    }
    let event_count = cursor.take_u64_be()?;

    let mut chain_head = GENESIS_HASH;
    for index in 0..event_count {
        let (prev_hash, hash) = read_persisted_record(&mut cursor)?;
        if prev_hash != chain_head {
            return Err(AuditError::ChainBroken {
                index,
                expected: chain_head,
                found: prev_hash,
            });
        }
        chain_head = hash;
    }

    if cursor.remaining() != 0 {
        return Err(malformed_persisted_chain(
            "audit log file has trailing data after its recorded event count",
        ));
    }

    Ok(PersistedChainStatus::Intact {
        event_count,
        chain_head,
    })
}

#[cfg(test)]
mod persisted_chain_tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "harw-secrets-audit-chain-{label}-{}-{}",
            std::process::id(),
            AuditEventId::new().as_bytes()[0]
        ))
    }

    /// Mirrors `SecretStore::persist_audit_state`'s redundant trailer
    /// (`store.rs`) without depending on that module, so this test module
    /// can exercise the loader on synthetic byte buffers directly.
    fn append_trailer(out: &mut Vec<u8>, event: &AuditEvent) {
        match &event.actor {
            Actor::Operator(name) => {
                out.push(0);
                write_len_prefixed(out, name.as_bytes());
            }
            Actor::Agent { session_id } => {
                out.push(1);
                write_len_prefixed(out, session_id.as_bytes());
            }
            Actor::System => out.push(2),
        }
        write_len_prefixed(out, event.action.as_bytes());
        out.extend_from_slice(&(event.subjects.len() as u64).to_be_bytes());
        for subject in &event.subjects {
            write_len_prefixed(out, subject.kind.as_bytes());
            write_len_prefixed(out, subject.id.as_bytes());
        }
        out.extend_from_slice(&event.recorded_at.as_second().to_be_bytes());
        out.extend_from_slice(&event.recorded_at.subsec_nanosecond().to_be_bytes());
        out.extend_from_slice(&event.prev_hash);
    }

    fn encode_persisted_log(events: &[AuditEvent]) -> Vec<u8> {
        let mut out = Vec::from(AUDIT_MAGIC);
        out.extend_from_slice(&AUDIT_FORMAT_VERSION.to_be_bytes());
        out.extend_from_slice(&(events.len() as u64).to_be_bytes());
        for event in events {
            out.extend_from_slice(&canonical_bytes(event));
            append_trailer(&mut out, event);
        }
        out
    }

    /// Replace every occurrence of `needle` (a 32-byte hash) in `bytes` with
    /// `replacement`, returning how many occurrences were replaced.
    fn replace_hash_occurrences(
        bytes: &mut [u8],
        needle: [u8; 32],
        replacement: [u8; 32],
    ) -> usize {
        let mut replaced = 0;
        let mut i = 0;
        while i + 32 <= bytes.len() {
            if bytes[i..i + 32] == needle {
                bytes[i..i + 32].copy_from_slice(&replacement);
                replaced += 1;
                i += 32;
            } else {
                i += 1;
            }
        }
        replaced
    }

    fn two_event_log() -> AuditLog {
        let mut log = AuditLog::new();
        log.append(
            Actor::Operator("should-not-leak-operator".to_owned()),
            "secret.create-should-not-leak-action",
            vec![SubjectRef::new("secret", "aaaa")],
        );
        log.append(
            Actor::System,
            "secret.delete",
            vec![SubjectRef::new("secret", "aaaa")],
        );
        log
    }

    #[test]
    fn missing_file_is_reported_as_absent_not_a_break() {
        let path = temp_path("missing");

        assert!(matches!(
            load_and_verify_persisted_chain(&path),
            Ok(PersistedChainStatus::Absent)
        ));
    }

    #[test]
    fn intact_persisted_chain_loads_and_verifies() -> TestResult {
        let log = two_event_log();
        let path = temp_path("intact");
        std::fs::write(&path, encode_persisted_log(log.events()))?;

        let status = load_and_verify_persisted_chain(&path)?;

        assert_eq!(
            status,
            PersistedChainStatus::Intact {
                event_count: 2,
                chain_head: log.chain_head(),
            }
        );
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn tampered_prev_hash_is_reported_as_chain_broken_not_a_load_error() -> TestResult {
        let log = two_event_log();
        let path = temp_path("tampered");
        let mut bytes = encode_persisted_log(log.events());

        let event0_hash = sha256(&canonical_bytes(&log.events()[0]));
        let replaced = replace_hash_occurrences(&mut bytes, event0_hash, [0xEE; 32]);
        assert_eq!(
            replaced, 2,
            "the tampered hash must appear in both the primary and redundant encodings"
        );
        std::fs::write(&path, &bytes)?;

        let Err(error) = load_and_verify_persisted_chain(&path) else {
            return Err(TestError::Unexpected(
                "Err erwartet (tampered chain must fail)".into(),
            ));
        };
        assert!(matches!(error, AuditError::ChainBroken { index: 1, .. }));
        assert!(!error.to_string().contains("should-not-leak"));
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn truncated_file_is_a_load_error_not_a_crash_or_intact() -> TestResult {
        let log = two_event_log();
        let path = temp_path("truncated");
        let bytes = encode_persisted_log(log.events());
        let truncated = &bytes[..bytes.len() / 2];
        std::fs::write(&path, truncated)?;

        let Err(error) = load_and_verify_persisted_chain(&path) else {
            return Err(TestError::Unexpected(
                "Err erwartet (truncated file must fail)".into(),
            ));
        };
        assert!(matches!(error, AuditError::Io(_)));
        assert!(!error.to_string().contains("should-not-leak"));
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn bad_header_is_a_load_error() -> TestResult {
        let path = temp_path("bad-header");
        std::fs::write(&path, b"not-an-audit-log-file-at-all")?;

        assert!(matches!(
            load_and_verify_persisted_chain(&path),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn trailing_bytes_after_declared_event_count_are_a_load_error() -> TestResult {
        let log = two_event_log();
        let path = temp_path("trailing");
        let mut bytes = encode_persisted_log(log.events());
        bytes.extend_from_slice(b"trailing-garbage");
        std::fs::write(&path, &bytes)?;

        assert!(matches!(
            load_and_verify_persisted_chain(&path),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused_not_dereferenced() -> TestResult {
        let log = two_event_log();
        let target = temp_path("symlink-target");
        std::fs::write(&target, encode_persisted_log(log.events()))?;
        let link = temp_path("symlink-link");
        std::os::unix::fs::symlink(&target, &link)?;

        assert!(matches!(
            load_and_verify_persisted_chain(&link),
            Err(AuditError::Io(_))
        ));
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&link);
        Ok(())
    }

    #[test]
    fn empty_valid_log_is_intact_with_zero_events() -> TestResult {
        let path = temp_path("empty");
        std::fs::write(&path, encode_persisted_log(&[]))?;

        assert_eq!(
            load_and_verify_persisted_chain(&path)?,
            PersistedChainStatus::Intact {
                event_count: 0,
                chain_head: GENESIS_HASH,
            }
        );
        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

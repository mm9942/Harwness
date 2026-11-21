//! Eigenes, nur anhängendes Aktions-Ledger des Wardens mit Hash-Kette:
//! [`ActionLedger`], [`LedgerEntry`], [`LedgerRecord`] (PL-100 WP 3.1,
//! THREAT-MODEL §1 „Escalator- und Warden-Ledger“).
//!
//! # Zweck
//! Jeder Eingriff hinterlässt hier einen dauerhaften Eintrag, **bevor** er
//! wirkt (`attempt`), und danach sein Ergebnis (`executed`/`failed`; ein
//! signierter Thaw einer cgroup, die es nicht mehr gibt, endet mit
//! `stale-containment-cleared`: nur die Eindämmung wurde abgeräumt). Auch
//! der Hold-Timer schreibt seine Auftau-Versuche, verworfene Holds
//! (`hold-dropped`) und aufgegebene Holds (`hold-abandoned`) hierher. Das Ledger ist die Grundlage für den Abgleich
//! mit dem Escalator (FreezeStore) und den späteren S7-Spiegel.
//!
//! # Format und Hash-Kette
//! Datei `actions.v2.log` im StateDirectory, je Eintrag eine Zeile
//! `<64 Hexzeichen hash> <JSON>\n`. Das JSON ist ein [`LedgerRecord`] mit
//! fortlaufender `seq` (ab `0`) und `prev` (Hash des Vorgängers). Es gilt
//!
//! `hash = BLAKE3(LEDGER_DOMAIN ‖ prev (32 Bytes) ‖ JSON-Bytes der Zeile)`.
//!
//! Gehasht werden die tatsächlich geschriebenen JSON-Bytes, nicht eine
//! Neuserialisierung — die Prüfung hängt damit nicht davon ab, ob serde
//! beim Wiedereinlesen byte-gleich serialisiert. Der Vorgänger des ersten
//! Eintrags ist [`genesis_hash`], aus einem Domain-String abgeleitet und nie
//! aus Nullen.
//!
//! # Fail closed
//! - [`ActionLedger::open`] prüft die gesamte Kette (Hash, `seq`, `prev`);
//!   jeder Bruch verhindert den Start ([`WardenError::LedgerCorrupt`]).
//! - Eine abgerissene letzte Zeile (Absturz vor dem `fsync`) wird
//!   abgeschnitten; ihr Aufruf hat nie `Ok` geliefert, also hat der Warden
//!   die zugehörige Aktion auch nicht ausgeführt.
//! - Nach einem Schreibfehler ist das Ledger bis zum Neustart gesperrt; der
//!   Warden führt ohne vorherigen `attempt`-Eintrag nichts aus.
//!
//! # Nebenläufigkeit
//! Ein `Mutex` um Dateihandle, Kopf-Hash und Sequenz; ein vergifteter Mutex
//! ist [`WardenError::StatePoisoned`].
//!
//! # Examples
//! ```rust
//! use harw_dod_warden::action_ledger::{ActionLedger, genesis_hash};
//!
//! let Ok(dir) = tempfile::tempdir() else { return };
//! let Ok(ledger) = ActionLedger::open(dir.path()) else { return };
//! assert_eq!(ledger.head().ok(), Some(genesis_hash()));
//! assert_ne!(genesis_hash(), [0u8; 32]);
//! ```

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harw_dod_warden_proto::{
    CgroupTarget, EscalationStage, KeyId, VerifiedAuthorization, WardenAction,
};
use harw_types::{ApprovalActor, FindingId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{WardenError, WardenResult};
use crate::fsutil;
use crate::hex;
use crate::holds::HoldEntry;

/// Dateiname des Aktions-Ledgers im StateDirectory.
pub const ACTION_LEDGER_FILE: &str = "actions.v2.log";

/// Domain-Separator der Hash-Kette.
pub const LEDGER_DOMAIN: &[u8] = b"harw:warden-ledger:v2\0";

/// Domain-String, aus dem der Genesis-Vorgänger abgeleitet wird.
pub const LEDGER_GENESIS_DOMAIN: &[u8] = b"harw:warden-ledger:v2:genesis\0";

/// Returns the derived predecessor hash of the first record (never all-zero).
#[must_use]
pub fn genesis_hash() -> [u8; 32] {
    *blake3::hash(LEDGER_GENESIS_DOMAIN).as_bytes()
}

/// Art eines Ledger-Eintrags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LedgerEvent {
    /// Der Warden versucht die Aktion jetzt (geschrieben **vor** der Wirkung).
    Attempt,
    /// Die Aktion wurde ausgeführt.
    Executed,
    /// Die Ausführung ist fehlgeschlagen.
    Failed,
    /// Ein fälliger Hold wurde verworfen, weil das Ziel nicht mehr dasselbe
    /// ist (verschwunden oder Kernel-ID geändert) oder der Warden für die
    /// cgroup keine nicht freigegebene Eindämmung aus diesem Boot führt; es
    /// wurde nichts aufgetaut.
    HoldDropped,
    /// Der Hold-Timer hat das Auftauen nach
    /// [`crate::warden::MAX_HOLD_THAW_ATTEMPTS`] Fehlversuchen aufgegeben;
    /// die cgroup bleibt eingefroren, bis ein signierter Thaw sie freigibt.
    HoldAbandoned,
    /// Ergebnis eines signierten `ThawCgroup`, dessen cgroup es nicht mehr
    /// gibt (verschwunden oder unter dem Pfad ersetzt): der Warden hat nur
    /// den liegengebliebenen Eindämmungsmarker und -datensatz abgeräumt,
    /// `cgroup.freeze` blieb unberührt (Operator-Pfad, `warden.rs`). Die
    /// Antwort auf dem Socket lautet `executed`; dieser Eintrag ist der
    /// dauerhafte Hinweis, dass nichts aufgetaut wurde.
    StaleContainmentCleared,
}

/// Wodurch ein Eintrag ausgelöst wurde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LedgerOrigin {
    /// Eine geprüfte v2-Autorisierung des Escalators.
    Authorization {
        /// Befund der Autorisierung.
        finding_id: FindingId,
        /// Freigebender (immer ein lokaler Operator).
        approver: ApprovalActor,
        /// Schlüssel, mit dem geprüft wurde.
        key_id: KeyId,
        /// Verbrauchte Nonce (Hex).
        nonce: String,
        /// Signierte Stufe.
        stage: EscalationStage,
        /// Ausstellungszeit.
        issued_at: Timestamp,
        /// Ablaufzeit.
        expires_at: Timestamp,
        /// Signierte Haltedauer (`None` = bis zur Aufhebung).
        hold_secs: Option<u32>,
        /// Signierte Bestätigung „nicht umkehrbar“.
        irreversible_ack: bool,
    },
    /// Der Hold-Timer des Wardens (Ablauf eines signierten Holds).
    HoldTimer {
        /// Befund des ursprünglichen Freeze.
        finding_id: FindingId,
        /// Nonce des ursprünglichen Freeze (Hex).
        nonce: String,
    },
}

/// Inhalt eines Ledger-Eintrags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// Art des Eintrags.
    pub event: LedgerEvent,
    /// Die betroffene Aktion.
    pub action: WardenAction,
    /// Das Ziel-Tripel (Pfad, Kernel-ID, erste Beobachtung).
    pub target: CgroupTarget,
    /// Auslöser.
    pub origin: LedgerOrigin,
}

impl LedgerEntry {
    /// Builds an entry for a verified authorization.
    #[must_use]
    pub fn for_authorization(event: LedgerEvent, verified: &VerifiedAuthorization) -> Self {
        Self {
            event,
            action: verified.action().clone(),
            target: verified.target().clone(),
            origin: LedgerOrigin::Authorization {
                finding_id: verified.finding_id().clone(),
                approver: verified.approver().clone(),
                key_id: verified.key_id().clone(),
                nonce: hex::encode(verified.nonce()),
                stage: verified.stage(),
                issued_at: verified.issued_at(),
                expires_at: verified.expires_at(),
                hold_secs: verified.hold_secs(),
                irreversible_ack: verified.irreversible_ack(),
            },
        }
    }

    /// Builds an entry for the hold timer (the action is always a thaw).
    #[must_use]
    pub fn for_hold(event: LedgerEvent, hold: &HoldEntry) -> Self {
        Self {
            event,
            action: WardenAction::ThawCgroup {
                cgroup: hold.target.path.clone(),
            },
            target: hold.target.clone(),
            origin: LedgerOrigin::HoldTimer {
                finding_id: hold.finding_id.clone(),
                nonce: hold.nonce.clone(),
            },
        }
    }
}

/// Eine Ledger-Zeile (ohne ihren Hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRecord {
    /// Fortlaufende Nummer ab `0`.
    pub seq: u64,
    /// Hash des Vorgängers (Hex; beim ersten Eintrag [`genesis_hash`]).
    pub prev: String,
    /// Zeitpunkt des Eintrags (Warden-Uhr).
    pub at: Timestamp,
    /// Inhalt.
    pub entry: LedgerEntry,
}

// Kopf der Kette, Sequenz und Dateihandle unter einem Lock.
struct LedgerState {
    file: File,
    head: [u8; 32],
    next_seq: u64,
    locked: bool,
}

/// Das Aktions-Ledger im StateDirectory.
///
/// # Description
/// Siehe Moduldoku.
pub struct ActionLedger {
    state: Mutex<LedgerState>,
}

impl std::fmt::Debug for ActionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionLedger").finish_non_exhaustive()
    }
}

impl ActionLedger {
    /// Opens (or creates) the ledger in `dir` after verifying the whole chain.
    ///
    /// # Errors
    /// - [`WardenError::LedgerCorrupt`]: Hash, `seq` oder `prev` einer
    ///   vollständigen Zeile passt nicht oder die Zeile ist unlesbar.
    /// - [`WardenError::Io`]: Lesen, Abschneiden oder Öffnen scheitert.
    pub fn open(dir: &Path) -> WardenResult<Self> {
        let contents = fsutil::read_optional(dir, ACTION_LEDGER_FILE)?.unwrap_or_default();
        let verified = verify_contents(&contents)?;
        if verified.torn_tail {
            let path: PathBuf = dir.join(ACTION_LEDGER_FILE);
            let file = fsutil::open_state_file(&path, rustix::fs::OFlags::WRONLY)?;
            file.set_len(u64::try_from(verified.valid_len).unwrap_or(u64::MAX))?;
            file.sync_all()?;
        }
        let file = fsutil::open_append(dir, ACTION_LEDGER_FILE)?;
        fsutil::sync_dir(dir)?;
        Ok(Self {
            state: Mutex::new(LedgerState {
                file,
                head: verified.head,
                next_seq: verified.next_seq,
                locked: false,
            }),
        })
    }

    /// Appends `entry` durably and returns its sequence number.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    /// - [`WardenError::LedgerUnavailable`]: gesperrt nach einem früheren
    ///   Schreibfehler oder Sequenz erschöpft.
    /// - [`WardenError::Encode`]: JSON-Kodierung scheitert.
    /// - [`WardenError::Io`]: Schreiben/`fsync` scheitert (danach gesperrt).
    pub fn append(&self, at: Timestamp, entry: &LedgerEntry) -> WardenResult<u64> {
        let mut state = self.state.lock().map_err(|_| WardenError::StatePoisoned)?;
        if state.locked {
            return Err(WardenError::LedgerUnavailable {
                ledger: "action",
                reason: "locked after a write error",
            });
        }
        let seq = state.next_seq;
        let next_seq = seq.checked_add(1).ok_or(WardenError::LedgerUnavailable {
            ledger: "action",
            reason: "sequence exhausted",
        })?;
        let record = LedgerRecord {
            seq,
            prev: hex::encode(&state.head),
            at,
            entry: entry.clone(),
        };
        let json = serde_json::to_string(&record).map_err(WardenError::Encode)?;
        let hash = chain_hash(&state.head, json.as_bytes());
        let line = format!("{} {json}\n", hex::encode(&hash));
        if let Err(error) = fsutil::append_durably(&mut state.file, line.as_bytes()) {
            state.locked = true;
            return Err(WardenError::Io(error));
        }
        state.head = hash;
        state.next_seq = next_seq;
        Ok(seq)
    }

    /// Returns the hash of the newest record (or [`genesis_hash`] when empty).
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn head(&self) -> WardenResult<[u8; 32]> {
        Ok(self
            .state
            .lock()
            .map_err(|_| WardenError::StatePoisoned)?
            .head)
    }

    /// Returns the sequence number the next record will get.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn next_seq(&self) -> WardenResult<u64> {
        Ok(self
            .state
            .lock()
            .map_err(|_| WardenError::StatePoisoned)?
            .next_seq)
    }
}

/// Reads and verifies the ledger in `dir` and returns its records.
///
/// # Description
/// Dieselbe Prüfung wie [`ActionLedger::open`], ohne die Datei zu ändern;
/// eine abgerissene letzte Zeile wird ignoriert. Für Abgleich, Tests und den
/// späteren S7-Spiegel.
///
/// # Errors
/// - [`WardenError::LedgerCorrupt`] / [`WardenError::Io`]: wie bei `open`.
pub fn read_verified(dir: &Path) -> WardenResult<Vec<LedgerRecord>> {
    let contents = fsutil::read_optional(dir, ACTION_LEDGER_FILE)?.unwrap_or_default();
    Ok(verify_contents(&contents)?.records)
}

// Ergebnis einer Kettenprüfung.
struct VerifiedChain {
    records: Vec<LedgerRecord>,
    head: [u8; 32],
    next_seq: u64,
    valid_len: usize,
    torn_tail: bool,
}

// BLAKE3(LEDGER_DOMAIN ‖ prev ‖ json).
fn chain_hash(prev: &[u8; 32], json: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(LEDGER_DOMAIN);
    hasher.update(prev);
    hasher.update(json);
    *hasher.finalize().as_bytes()
}

// Prüft alle vollständigen Zeilen der Kette.
fn verify_contents(contents: &[u8]) -> WardenResult<VerifiedChain> {
    let (lines, tail) = fsutil::split_complete_lines(contents);
    let mut chain = VerifiedChain {
        records: Vec::with_capacity(lines.len()),
        head: genesis_hash(),
        next_seq: 0,
        valid_len: 0,
        torn_tail: tail.is_some(),
    };
    for (index, line) in lines.iter().enumerate() {
        let corrupt = WardenError::LedgerCorrupt {
            ledger: "action",
            line: index.saturating_add(1),
        };
        let Some((hash, record)) = parse_line(line, &chain.head) else {
            return Err(corrupt);
        };
        if record.seq != chain.next_seq || record.prev != hex::encode(&chain.head) {
            return Err(corrupt);
        }
        chain.head = hash;
        chain.next_seq = chain.next_seq.checked_add(1).ok_or(corrupt)?;
        chain.valid_len = chain.valid_len.saturating_add(line.len()).saturating_add(1);
        chain.records.push(record);
    }
    Ok(chain)
}

// Liest eine Zeile und prüft ihren Hash gegen `prev`.
fn parse_line(line: &[u8], prev: &[u8; 32]) -> Option<([u8; 32], LedgerRecord)> {
    let text = std::str::from_utf8(line).ok()?;
    let (hash_hex, json) = text.split_once(' ')?;
    let hash = hex::decode::<32>(hash_hex)?;
    if chain_hash(prev, json.as_bytes()) != hash {
        return None;
    }
    let record = serde_json::from_str::<LedgerRecord>(json).ok()?;
    Some((hash, record))
}

#[cfg(test)]
mod tests {
    use super::{
        ACTION_LEDGER_FILE, ActionLedger, LedgerEntry, LedgerEvent, LedgerOrigin, genesis_hash,
        read_verified,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestResult, ctx};
    use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget, WardenAction};
    use harw_types::{CgroupId, FindingId};
    use jiff::Timestamp;

    fn entry(event: LedgerEvent) -> TestResult<LedgerEntry> {
        let path = CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?;
        Ok(LedgerEntry {
            event,
            action: WardenAction::ThawCgroup {
                cgroup: path.clone(),
            },
            target: CgroupTarget::new(
                path,
                CgroupKernelId::new(4242).map_err(ctx("kernel id"))?,
                Timestamp::from_second(1_799_000_000).map_err(ctx("ts"))?,
            ),
            origin: LedgerOrigin::HoldTimer {
                finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding"))?,
                nonce: "42".repeat(16),
            },
        })
    }

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    #[test]
    fn test_genesis_is_derived_and_not_zero() {
        assert_ne!(genesis_hash(), [0u8; 32]);
        assert_eq!(
            genesis_hash(),
            *blake3::hash(b"harw:warden-ledger:v2:genesis\0").as_bytes()
        );
    }

    #[test]
    fn test_chain_survives_restart_and_continues() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        {
            let ledger = ActionLedger::open(dir.path()).map_err(ctx("open"))?;
            assert_eq!(
                ledger
                    .append(at(1)?, &entry(LedgerEvent::Attempt)?)
                    .map_err(ctx("a"))?,
                0
            );
            assert_eq!(
                ledger
                    .append(at(2)?, &entry(LedgerEvent::Executed)?)
                    .map_err(ctx("b"))?,
                1
            );
        }
        let reopened = ActionLedger::open(dir.path()).map_err(ctx("reopen"))?;
        assert_eq!(reopened.next_seq().map_err(ctx("seq"))?, 2);
        assert_eq!(
            reopened
                .append(at(3)?, &entry(LedgerEvent::Attempt)?)
                .map_err(ctx("c"))?,
            2
        );
        let records = read_verified(dir.path()).map_err(ctx("verify"))?;
        assert_eq!(records.len(), 3);
        assert_eq!(
            records.first().map(|r| r.prev.clone()),
            Some(crate::hex::encode(&genesis_hash()))
        );
        assert_eq!(
            records.iter().map(|r| r.entry.event).collect::<Vec<_>>(),
            [
                LedgerEvent::Attempt,
                LedgerEvent::Executed,
                LedgerEvent::Attempt
            ]
        );
        Ok(())
    }

    #[test]
    fn test_tampered_record_refuses_open() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        {
            let ledger = ActionLedger::open(dir.path()).map_err(ctx("open"))?;
            ledger
                .append(at(1)?, &entry(LedgerEvent::Attempt)?)
                .map_err(ctx("a"))?;
            ledger
                .append(at(2)?, &entry(LedgerEvent::Executed)?)
                .map_err(ctx("b"))?;
        }
        let path = dir.path().join(ACTION_LEDGER_FILE);
        let original = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        // Ereignis der ersten Zeile umschreiben: der Hash passt nicht mehr.
        let tampered = original.replacen("\"attempt\"", "\"executed\"", 1);
        assert_ne!(tampered, original);
        std::fs::write(&path, tampered).map_err(ctx("write"))?;
        assert!(matches!(
            ActionLedger::open(dir.path()),
            Err(WardenError::LedgerCorrupt { line: 1, .. })
        ));
        // Eine gelöschte Zeile bricht die Kette ebenfalls.
        let second_only: String = original.lines().skip(1).map(|l| format!("{l}\n")).collect();
        std::fs::write(path, second_only).map_err(ctx("write"))?;
        assert!(matches!(
            ActionLedger::open(dir.path()),
            Err(WardenError::LedgerCorrupt { line: 1, .. })
        ));
        Ok(())
    }

    #[test]
    fn test_torn_tail_is_cut_off_and_chain_continues() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        {
            let ledger = ActionLedger::open(dir.path()).map_err(ctx("open"))?;
            ledger
                .append(at(1)?, &entry(LedgerEvent::Attempt)?)
                .map_err(ctx("a"))?;
        }
        let path = dir.path().join(ACTION_LEDGER_FILE);
        let complete = std::fs::read_to_string(&path).map_err(ctx("read"))?;
        std::fs::write(&path, format!("{complete}deadbeef {{\"seq\":1")).map_err(ctx("write"))?;
        let reopened = ActionLedger::open(dir.path()).map_err(ctx("reopen torn"))?;
        assert_eq!(
            std::fs::read_to_string(path).map_err(ctx("read"))?,
            complete
        );
        assert_eq!(
            reopened
                .append(at(2)?, &entry(LedgerEvent::Executed)?)
                .map_err(ctx("b"))?,
            1
        );
        assert_eq!(read_verified(dir.path()).map_err(ctx("verify"))?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_stale_containment_clear_is_its_own_durable_outcome() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        {
            let ledger = ActionLedger::open(dir.path()).map_err(ctx("open"))?;
            ledger
                .append(at(1)?, &entry(LedgerEvent::Attempt)?)
                .map_err(ctx("a"))?;
            ledger
                .append(at(2)?, &entry(LedgerEvent::StaleContainmentCleared)?)
                .map_err(ctx("b"))?;
        }
        let text =
            std::fs::read_to_string(dir.path().join(ACTION_LEDGER_FILE)).map_err(ctx("read"))?;
        assert!(text.contains("\"stale-containment-cleared\""));
        let records = read_verified(dir.path()).map_err(ctx("verify"))?;
        assert_eq!(
            records.iter().map(|r| r.entry.event).collect::<Vec<_>>(),
            [LedgerEvent::Attempt, LedgerEvent::StaleContainmentCleared]
        );
        Ok(())
    }
}

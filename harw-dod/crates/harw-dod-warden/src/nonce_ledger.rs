//! Persistenter Replay-Schutz: [`FileNonceLedger`] (PL-100 WP 3.1,
//! Bedrohung B24, Invariante 3 „wiederholte Nonces auch nach einem Neustart“).
//!
//! # Zweck
//! Implementiert [`NonceLedger`] dauerhaft im StateDirectory des Wardens
//! (`/var/lib/harw-warden/nonces.v2.log`). Eine Nonce, die einmal
//! [`NonceLedger::check_and_record`] bestanden hat, wird bis
//! `expires_at + Sicherheitsabstand` abgelehnt — auch nach einem Neustart,
//! weil [`FileNonceLedger::open`] die Datei wieder einliest.
//!
//! # Format
//! Eine Zeile je Nonce: `<32 Hexzeichen> <keep_until>\n`, `keep_until` in
//! Unix-Sekunden (`expires_at` aufgerundet plus
//! [`NONCE_RETENTION_MARGIN_SECS`], dem Uhrenversatz-Deckel von Proof v2).
//! Jede Zeile wird mit einem einzigen `write` angehängt und vor `Ok` per
//! `fdatasync` dauerhaft gemacht. Beim Öffnen wird die Datei atomar neu
//! geschrieben (`fsutil.rs`); eine abgerissene letzte Zeile (Absturz vor dem
//! `fsync`) wird dabei verworfen — ihr Aufruf hat nie `Ok` geliefert.
//!
//! # Verworfen wird nur unter Druck (Uhrensprünge, B24)
//! Die Warden-Uhr ist die einzige Zeitquelle. Würde jeder Eintrag sofort
//! nach `keep_until` verworfen, ließe ein Vorwärtssprung der Uhr noch
//! gültige Nonces verfallen; nach der Rückkorrektur lägen die zugehörigen
//! Autorisierungen wieder in ihrem Zeitfenster und wären wiederholbar. Der
//! feste Sicherheitsabstand deckt das nicht. Deshalb gilt:
//!
//! - [`FileNonceLedger::open`] und [`FileNonceLedger::prune`] verwerfen erst,
//!   wenn mindestens [`prune_threshold`] (`capacity / 2`, Vorgabe 32 768)
//!   Einträge leben. Darunter verfällt keine Nonce, gleich was die Uhr sagt.
//! - Dann fallen nur abgelaufene Einträge weg, die **ältesten zuerst**
//!   (nach `keep_until`, also nach dem signierten `expires_at`), und nur bis
//!   auf [`prune_low_water`] (`capacity / 4`, Vorgabe 16 384). Die jüngsten
//!   Einträge — die einzigen, deren Autorisierungen nach einer
//!   Rückkorrektur der Uhr wieder gültig sein könnten — bleiben stehen,
//!   solange es nicht mehr jüngere gibt als diese Untergrenze. Das hieße
//!   über 16 000 angenommene Aufträge innerhalb einer Token-Lebensdauer.
//!
//! Der Preis ist Platz: bis zu `capacity / 2` Zeilen (rund 1,5 MiB) bleiben
//! länger als nötig stehen, auch über Neustarts. Ganz beseitigt erst eine
//! monotone, persistierte Zeitquelle die Abhängigkeit von der Uhr.
//!
//! # Grenzen und fail closed
//! - Höchstens `capacity` lebende Einträge (Vorgabe
//!   [`DEFAULT_NONCE_CAPACITY`]); ist der Ledger voll, wird jede neue Nonce
//!   mit [`ProofError::LedgerIo`] abgelehnt (Ablehnung `Unavailable`), nie
//!   ein alter Eintrag verdrängt.
//! - Nach einem Schreibfehler ist der Ledger bis zum Neustart gesperrt (die
//!   Datei könnte eine halbe Zeile enthalten).
//! - Jede beschädigte vollständige Zeile verhindert den Start
//!   ([`WardenError::LedgerCorrupt`]).
//!
//! # Nebenläufigkeit
//! Ein `Mutex` um Speicherabbild und Dateihandle; Prüfen und Eintragen sind
//! atomar. Ein vergifteter Mutex wird als [`ProofError::LedgerIo`]
//! gemeldet.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden::nonce_ledger::FileNonceLedger;
//! use harw_dod_warden_proto::NonceLedger;
//!
//! let Ok(dir) = tempfile::tempdir() else { return };
//! let now = jiff::Timestamp::UNIX_EPOCH;
//! let Ok(ledger) = FileNonceLedger::open(dir.path(), now, 16) else { return };
//! assert!(ledger.check_and_record(&[7; 16], now).is_ok());
//! drop(ledger);
//! // Neustart: dieselbe Nonce bleibt verbraucht.
//! let Ok(reopened) = FileNonceLedger::open(dir.path(), now, 16) else { return };
//! assert!(reopened.check_and_record(&[7; 16], now).is_err());
//! ```

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harw_dod_warden_proto::{MAX_CLOCK_SKEW_SECS_CAP, NonceLedger, ProofError};
use jiff::Timestamp;

use crate::error::{WardenError, WardenResult};
use crate::fsutil;
use crate::hex;

/// Dateiname des Nonce-Ledgers im StateDirectory.
pub const NONCE_LEDGER_FILE: &str = "nonces.v2.log";

/// Vorgabe der Höchstzahl lebender Nonces (TTL ≤ 120 s: weit über jedem
/// legitimen Aufkommen).
pub const DEFAULT_NONCE_CAPACITY: usize = 65_536;

/// Sicherheitsabstand nach `expires_at`, bevor ein Eintrag verworfen wird.
pub const NONCE_RETENTION_MARGIN_SECS: i64 = MAX_CLOCK_SKEW_SECS_CAP as i64;

// Speicherabbild und Dateihandle unter einem Lock.
struct NonceState {
    live: HashMap<[u8; 16], i64>,
    file: File,
    lines_in_file: usize,
    locked: bool,
}

/// Dauerhafter [`NonceLedger`] im StateDirectory.
///
/// # Description
/// Siehe Moduldoku.
pub struct FileNonceLedger {
    dir: PathBuf,
    capacity: usize,
    state: Mutex<NonceState>,
}

impl FileNonceLedger {
    /// Opens (or creates) the ledger in `dir`; under pressure drops the oldest entries expired before `now`.
    ///
    /// # Arguments
    /// - `dir` (`&Path`): das StateDirectory (muss existieren).
    /// - `now` (`Timestamp`): aktuelle Zeit (injiziert).
    /// - `capacity` (`usize`): Höchstzahl lebender Einträge (mindestens 1).
    ///
    /// # Errors
    /// - [`WardenError::LedgerCorrupt`]: eine vollständige Zeile ist
    ///   unlesbar.
    /// - [`WardenError::Io`]: Lesen, Neuschreiben oder Öffnen scheitert.
    /// - [`WardenError::LedgerUnavailable`]: `capacity == 0` oder die Datei
    ///   enthält mehr lebende Einträge als `capacity`.
    pub fn open(dir: &Path, now: Timestamp, capacity: usize) -> WardenResult<Self> {
        if capacity == 0 {
            return Err(WardenError::LedgerUnavailable {
                ledger: "nonce",
                reason: "capacity must be positive",
            });
        }
        let mut live: HashMap<[u8; 16], i64> = HashMap::new();
        if let Some(contents) = fsutil::read_optional(dir, NONCE_LEDGER_FILE)? {
            let (lines, _torn_tail) = fsutil::split_complete_lines(&contents);
            for (index, line) in lines.iter().enumerate() {
                let (nonce, keep_until) = parse_line(line).ok_or(WardenError::LedgerCorrupt {
                    ledger: "nonce",
                    line: index.saturating_add(1),
                })?;
                let slot = live.entry(nonce).or_insert(keep_until);
                *slot = (*slot).max(keep_until);
            }
        }
        if live.len() >= prune_threshold(capacity) {
            drop_oldest_expired(&mut live, now.as_second(), prune_low_water(capacity));
        }
        if live.len() > capacity {
            return Err(WardenError::LedgerUnavailable {
                ledger: "nonce",
                reason: "more live entries than the capacity",
            });
        }
        // Kompakte Fassung schreiben: verwirft eine abgerissene letzte Zeile
        // (und unter Druck Abgelaufenes), bevor wieder angehängt wird.
        fsutil::replace_file_atomically(dir, NONCE_LEDGER_FILE, &render(&live))?;
        let file = fsutil::open_append(dir, NONCE_LEDGER_FILE)?;
        let lines_in_file = live.len();
        Ok(Self {
            dir: dir.to_path_buf(),
            capacity,
            state: Mutex::new(NonceState {
                live,
                file,
                lines_in_file,
                locked: false,
            }),
        })
    }

    /// Drops entries whose retention ended before `now` once the ledger is under pressure.
    ///
    /// # Description
    /// Verwirft nur, wenn mindestens [`prune_threshold`] Einträge leben, und
    /// dann nur abgelaufene, die ältesten zuerst, bis höchstens
    /// [`prune_low_water`] übrig sind (siehe Moduldoku, „Verworfen wird nur
    /// unter Druck“). Schreibt die Datei neu, sobald sie deutlich mehr Zeilen
    /// als lebende Einträge hat.
    ///
    /// # Returns
    /// Wie viele Einträge entfernt wurden (`0` unterhalb der Schwelle).
    ///
    /// # Errors
    /// - [`ProofError::LedgerIo`]: Mutex vergiftet, Ledger gesperrt oder das
    ///   Neuschreiben scheitert (der Ledger ist danach gesperrt).
    pub fn prune(&self, now: Timestamp) -> Result<usize, ProofError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ledger_io("nonce ledger mutex poisoned"))?;
        if state.locked {
            return Err(ledger_io("nonce ledger is locked after a write error"));
        }
        let before = state.live.len();
        if before < prune_threshold(self.capacity) {
            return Ok(0);
        }
        let removed = drop_oldest_expired(
            &mut state.live,
            now.as_second(),
            prune_low_water(self.capacity),
        );
        // Neu schreiben, sobald die Datei deutlich mehr Zeilen als lebende
        // Einträge hat (amortisiert, nicht bei jedem Aufruf).
        let threshold = state.live.len().saturating_mul(2).saturating_add(1024);
        if state.lines_in_file > threshold {
            let rewritten =
                fsutil::replace_file_atomically(&self.dir, NONCE_LEDGER_FILE, &render(&state.live))
                    .and_then(|()| fsutil::open_append(&self.dir, NONCE_LEDGER_FILE));
            match rewritten {
                Ok(file) => {
                    state.file = file;
                    state.lines_in_file = state.live.len();
                }
                Err(error) => {
                    state.locked = true;
                    return Err(ProofError::LedgerIo(error));
                }
            }
        }
        Ok(removed)
    }

    /// Returns the number of live entries.
    ///
    /// # Errors
    /// - [`ProofError::LedgerIo`]: Mutex vergiftet.
    pub fn live_count(&self) -> Result<usize, ProofError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ledger_io("nonce ledger mutex poisoned"))?
            .live
            .len())
    }

    /// Returns the configured capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl std::fmt::Debug for FileNonceLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileNonceLedger")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl NonceLedger for FileNonceLedger {
    fn check_and_record(&self, nonce: &[u8; 16], expires_at: Timestamp) -> Result<(), ProofError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ledger_io("nonce ledger mutex poisoned"))?;
        if state.locked {
            return Err(ledger_io("nonce ledger is locked after a write error"));
        }
        if state.live.contains_key(nonce) {
            return Err(ProofError::NonceReplayed);
        }
        if state.live.len() >= self.capacity {
            return Err(ledger_io("nonce ledger is full"));
        }
        let keep_until = keep_until(expires_at);
        let line = format!("{} {keep_until}\n", hex::encode(nonce));
        if let Err(error) = fsutil::append_durably(&mut state.file, line.as_bytes()) {
            state.locked = true;
            return Err(ProofError::LedgerIo(error));
        }
        state.live.insert(*nonce, keep_until);
        state.lines_in_file = state.lines_in_file.saturating_add(1);
        Ok(())
    }
}

/// Returns the number of live entries from which expired ones are dropped.
///
/// # Description
/// `capacity / 2`, mindestens `1`. Darunter bleibt jeder Eintrag stehen,
/// gleich was die Uhr sagt (siehe Moduldoku).
///
/// # Examples
/// ```rust
/// use harw_dod_warden::nonce_ledger::{DEFAULT_NONCE_CAPACITY, prune_threshold};
///
/// assert_eq!(prune_threshold(DEFAULT_NONCE_CAPACITY), 32_768);
/// assert_eq!(prune_threshold(1), 1);
/// ```
#[must_use]
pub fn prune_threshold(capacity: usize) -> usize {
    (capacity / 2).max(1)
}

/// Returns how many live entries a prune under pressure keeps at least.
///
/// # Description
/// `capacity / 4`: so viele der jüngsten Einträge überstehen jedes
/// Verwerfen, auch wenn die Uhr sie für abgelaufen hält.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::nonce_ledger::{DEFAULT_NONCE_CAPACITY, prune_low_water};
///
/// assert_eq!(prune_low_water(DEFAULT_NONCE_CAPACITY), 16_384);
/// assert_eq!(prune_low_water(2), 0);
/// ```
#[must_use]
pub fn prune_low_water(capacity: usize) -> usize {
    capacity / 4
}

// Entfernt abgelaufene Einträge (`keep_until < cutoff`), die ältesten
// zuerst, bis höchstens `keep_at_least` übrig sind oder keiner mehr
// abgelaufen ist. Gibt die Zahl der entfernten Einträge zurück.
fn drop_oldest_expired(
    live: &mut HashMap<[u8; 16], i64>,
    cutoff: i64,
    keep_at_least: usize,
) -> usize {
    let mut expired: Vec<([u8; 16], i64)> = live
        .iter()
        .filter(|(_, keep_until)| **keep_until < cutoff)
        .map(|(nonce, keep_until)| (*nonce, *keep_until))
        .collect();
    expired.sort_unstable_by_key(|(_, keep_until)| *keep_until);
    let excess = live.len().saturating_sub(keep_at_least);
    let mut removed: usize = 0;
    for (nonce, _) in expired.into_iter().take(excess) {
        if live.remove(&nonce).is_some() {
            removed = removed.saturating_add(1);
        }
    }
    removed
}

// `expires_at` aufgerundet auf ganze Sekunden plus Sicherheitsabstand.
fn keep_until(expires_at: Timestamp) -> i64 {
    let rounded_up = if expires_at.subsec_nanosecond() > 0 {
        expires_at.as_second().saturating_add(1)
    } else {
        expires_at.as_second()
    };
    rounded_up.saturating_add(NONCE_RETENTION_MARGIN_SECS)
}

// Liest eine Zeile `<hex> <keep_until>`.
fn parse_line(line: &[u8]) -> Option<([u8; 16], i64)> {
    let text = std::str::from_utf8(line).ok()?;
    let (nonce, keep_until) = text.split_once(' ')?;
    Some((hex::decode::<16>(nonce)?, keep_until.parse::<i64>().ok()?))
}

// Kompakte Dateifassung aller lebenden Einträge.
fn render(live: &HashMap<[u8; 16], i64>) -> Vec<u8> {
    let mut out = String::with_capacity(live.len().saturating_mul(48));
    for (nonce, keep_until) in live {
        out.push_str(&hex::encode(nonce));
        out.push(' ');
        out.push_str(&keep_until.to_string());
        out.push('\n');
    }
    out.into_bytes()
}

// Ledger-Fehler mit festem Text.
fn ledger_io(message: &'static str) -> ProofError {
    ProofError::LedgerIo(std::io::Error::other(message))
}

#[cfg(test)]
mod tests {
    use super::{
        FileNonceLedger, NONCE_LEDGER_FILE, NONCE_RETENTION_MARGIN_SECS, prune_low_water,
        prune_threshold,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{NonceLedger, ProofError};
    use jiff::Timestamp;

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    #[test]
    fn test_replay_is_refused_before_and_after_restart() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        let expires = at(1_800_000_060)?;
        {
            let ledger = FileNonceLedger::open(dir.path(), now, 8).map_err(ctx("open"))?;
            ledger
                .check_and_record(&[1; 16], expires)
                .map_err(ctx("first use"))?;
            assert!(matches!(
                ledger.check_and_record(&[1; 16], expires),
                Err(ProofError::NonceReplayed)
            ));
        }
        let reopened = FileNonceLedger::open(dir.path(), now, 8).map_err(ctx("reopen"))?;
        assert!(matches!(
            reopened.check_and_record(&[1; 16], expires),
            Err(ProofError::NonceReplayed)
        ));
        reopened
            .check_and_record(&[2; 16], expires)
            .map_err(ctx("fresh nonce"))?;
        Ok(())
    }

    #[test]
    fn test_entries_survive_until_expiry_plus_margin_then_disappear_under_pressure() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        let expires = at(1_800_000_060)?;
        // Kapazität 2: ein lebender Eintrag erreicht die Schwelle.
        let ledger = FileNonceLedger::open(dir.path(), now, 2).map_err(ctx("open"))?;
        ledger
            .check_and_record(&[3; 16], expires)
            .map_err(ctx("record"))?;
        let still_kept = at(1_800_000_060_i64.saturating_add(NONCE_RETENTION_MARGIN_SECS))?;
        assert_eq!(ledger.prune(still_kept).map_err(ctx("prune"))?, 0);
        let gone = at(1_800_000_061_i64.saturating_add(NONCE_RETENTION_MARGIN_SECS))?;
        assert_eq!(ledger.prune(gone).map_err(ctx("prune"))?, 1);
        drop(ledger);
        // Auch das Wiedereinlesen verwirft den abgelaufenen Eintrag (die
        // Datei wurde nicht neu geschrieben; die Zeile steht noch darin).
        let reopened = FileNonceLedger::open(dir.path(), gone, 2).map_err(ctx("reopen"))?;
        assert_eq!(reopened.live_count().map_err(ctx("count"))?, 0);
        Ok(())
    }

    #[test]
    fn test_forward_clock_jump_never_drops_a_nonce_below_the_threshold() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        let expires = at(1_800_000_060)?;
        let jumped = at(1_900_000_000)?;
        assert_eq!(prune_threshold(8), 4);
        {
            let ledger = FileNonceLedger::open(dir.path(), now, 8).map_err(ctx("open"))?;
            ledger
                .check_and_record(&[4; 16], expires)
                .map_err(ctx("record"))?;
            // Die Uhr springt weit vor: nichts wird verworfen.
            assert_eq!(ledger.prune(jumped).map_err(ctx("prune"))?, 0);
            // Nach der Rückkorrektur ist die Nonce weiter verbraucht.
            assert!(matches!(
                ledger.check_and_record(&[4; 16], expires),
                Err(ProofError::NonceReplayed)
            ));
        }
        // Auch ein Neustart während des Sprungs verwirft sie nicht.
        let restarted = FileNonceLedger::open(dir.path(), jumped, 8).map_err(ctx("reopen"))?;
        assert_eq!(restarted.live_count().map_err(ctx("count"))?, 1);
        assert!(matches!(
            restarted.check_and_record(&[4; 16], expires),
            Err(ProofError::NonceReplayed)
        ));
        Ok(())
    }

    #[test]
    fn test_pressure_prune_drops_the_oldest_expired_and_keeps_the_newest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        // Kapazität 8: Schwelle 4, Untergrenze 2.
        assert_eq!((prune_threshold(8), prune_low_water(8)), (4, 2));
        let ledger = FileNonceLedger::open(dir.path(), now, 8).map_err(ctx("open"))?;
        for (nonce, offset) in [(1u8, 10i64), (2, 20), (3, 30), (4, 40)] {
            ledger
                .check_and_record(&[nonce; 16], at(1_800_000_000_i64.saturating_add(offset))?)
                .map_err(ctx("record"))?;
        }
        // Weit in der „Zukunft“ (etwa ein Vorwärtssprung): alle vier gelten
        // als abgelaufen, verworfen werden aber nur die zwei ältesten.
        assert_eq!(ledger.prune(at(1_900_000_000)?).map_err(ctx("prune"))?, 2);
        assert_eq!(ledger.live_count().map_err(ctx("count"))?, 2);
        for newest in [3u8, 4] {
            assert!(matches!(
                ledger.check_and_record(&[newest; 16], at(1_800_000_060)?),
                Err(ProofError::NonceReplayed)
            ));
        }
        ledger
            .check_and_record(&[1; 16], at(1_800_000_060)?)
            .map_err(ctx("oldest was dropped"))?;
        Ok(())
    }

    #[test]
    fn test_full_ledger_fails_closed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        let expires = at(1_800_000_060)?;
        let ledger = FileNonceLedger::open(dir.path(), now, 2).map_err(ctx("open"))?;
        ledger
            .check_and_record(&[1; 16], expires)
            .map_err(ctx("1"))?;
        ledger
            .check_and_record(&[2; 16], expires)
            .map_err(ctx("2"))?;
        match ledger.check_and_record(&[3; 16], expires) {
            Err(ProofError::LedgerIo(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "full ledger must fail closed, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_torn_tail_is_dropped_and_corrupt_line_refuses_start() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let now = at(1_800_000_000)?;
        let valid = format!("{} 1800000100\n", "11".repeat(16));
        std::fs::write(
            dir.path().join(NONCE_LEDGER_FILE),
            format!("{valid}{}", "22".repeat(8)),
        )
        .map_err(ctx("write torn ledger"))?;
        let ledger = FileNonceLedger::open(dir.path(), now, 8).map_err(ctx("open torn"))?;
        assert_eq!(ledger.live_count().map_err(ctx("count"))?, 1);
        drop(ledger);
        let rewritten =
            std::fs::read_to_string(dir.path().join(NONCE_LEDGER_FILE)).map_err(ctx("read"))?;
        assert_eq!(rewritten, valid);

        std::fs::write(
            dir.path().join(NONCE_LEDGER_FILE),
            format!("{valid}not-a-nonce 5\n"),
        )
        .map_err(ctx("write corrupt ledger"))?;
        assert!(matches!(
            FileNonceLedger::open(dir.path(), now, 8),
            Err(WardenError::LedgerCorrupt { line: 2, .. })
        ));
        Ok(())
    }
}

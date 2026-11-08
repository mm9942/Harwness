//! Persistente Hold-Timer: [`HoldStore`], [`HoldEntry`] (PL-100 WP 3.1,
//! Entscheidung O3, Bedrohung B13).
//!
//! # Zweck
//! Ein Freeze darf eine signierte Haltedauer tragen (`hold_secs`; `None` =
//! bis zur Aufhebung, O3). Läuft sie ab, taut der Warden die cgroup selbst
//! auf. Dieser Speicher hält die laufenden Holds im StateDirectory
//! (`holds.v2.json`), damit sie einen Neustart überstehen und danach wieder
//! scharf sind; ein während der Ausfallzeit fälliger Hold wird beim ersten
//! Durchlauf nach dem Start ausgeführt.
//!
//! # Der Hold-Timer taut nur auf
//! Ein [`HoldEntry`] beschreibt ausschließlich „diese cgroup zu diesem
//! Zeitpunkt auftauen“. Es gibt kein Feld, das eine Isolation, einen Kill
//! oder eine andere Aktion auslösen könnte; der Warden ruft beim Ablauf nur
//! seinen `CgroupThawer` auf (`warden.rs`, `expire_holds`).
//!
//! # Format
//! Ein JSON-Schnappschuss `{"version":2,"holds":[...]}`, bei jeder Änderung
//! atomar ersetzt (`fsutil.rs`). Je cgroup-Pfad höchstens ein Hold; ein
//! neuer Freeze desselben Pfads ersetzt ihn, ein Freeze ohne Hold, ein Thaw
//! oder ein Kill hebt ihn auf. Höchstens [`MAX_ACTIVE_HOLDS`] gleichzeitig.
//!
//! # Nebenläufigkeit
//! Ein `Mutex` um die Liste. Änderungen aus einer Anfrage ([`HoldStore::arm`],
//! [`HoldStore::cancel`]) werden zuerst dauerhaft geschrieben und erst danach
//! im Speicher übernommen; scheitert das Schreiben, bleibt der alte Stand
//! gültig (die Anfrage bekommt `unavailable`).
//!
//! # Buchführung des Hold-Timers auch ohne Schreibzugriff
//! [`HoldStore::reschedule`] und [`HoldStore::forget`] gehören dem Hold-Timer.
//! Sie übernehmen ihre Änderung **immer** im Speicher und melden nur, ob sie
//! auch dauerhaft geschrieben wurde. Sonst bliebe ein fälliger Hold bei
//! vollem Datenträger (oder einer fremden `holds.v2.json.tmp`) jede Sekunde
//! fällig: kein wachsender Abstand, keine Obergrenze der Versuche, jede
//! Sekunde neue Audit- und Ledger-Einträge. Der Speicherstand ist für den
//! laufenden Prozess maßgeblich; die nächste erfolgreiche Änderung schreibt
//! ihn vollständig. Bis dahin merkt sich der Speicher, dass er ungesichert
//! ist: dann schreibt auch ein [`HoldStore::cancel`] ohne Hold des Pfads die
//! Datei neu (scheitert das, scheitert `cancel`, und der Freeze, der damit
//! einen Hold aufheben wollte, wird nicht ausgeführt), und
//! [`HoldStore::flush`] schreibt den Stand nach (der Hold-Timer ruft das
//! regelmäßig auf). Sonst käme ein nur im Speicher entfernter Hold nach einem
//! Neustart zurück und taute einen späteren Freeze „bis zur Aufhebung“
//! derselben cgroup auf. Bleibt der Speicher bis zu einem Neustart
//! unschreibbar, gilt danach der zuletzt geschriebene Stand (ein Hold ist
//! dann früher fällig oder kommt zurück); der Hold-Timer taut dann nur auf,
//! wenn der Warden für die cgroup eine nicht freigegebene Eindämmung aus
//! diesem Boot führt (`warden.rs`, `expire_one`).
//!
//! # Examples
//! ```rust
//! use harw_dod_warden::holds::HoldStore;
//!
//! let Ok(dir) = tempfile::tempdir() else { return };
//! let Ok(store) = HoldStore::open(dir.path()) else { return };
//! assert_eq!(store.snapshot().map(|holds| holds.len()).ok(), Some(0));
//! ```

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harw_dod_warden_proto::CgroupTarget;
use harw_types::{CgroupId, FindingId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::{WardenError, WardenResult};
use crate::fsutil;

/// Dateiname des Hold-Speichers im StateDirectory.
pub const HOLDS_FILE: &str = "holds.v2.json";

/// Höchstzahl gleichzeitig laufender Holds.
pub const MAX_ACTIVE_HOLDS: usize = 256;

// Formatversion des Schnappschusses.
const HOLDS_FORMAT_VERSION: u16 = 2;

/// Ein laufender Hold: „`target` um `thaw_at` auftauen“.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldEntry {
    /// Das beim Freeze geprüfte Ziel-Tripel; vor dem Auftauen wird es
    /// erneut beobachtet (Kernel-ID-Wiederverwendung, B21).
    pub target: CgroupTarget,
    /// Fälligkeit.
    pub thaw_at: Timestamp,
    /// Befund des auslösenden Freeze.
    pub finding_id: FindingId,
    /// Nonce des auslösenden Freeze (Hex), für das Ledger.
    pub nonce: String,
    /// Bisher fehlgeschlagene Auftau-Versuche (`0` beim Stellen). Der
    /// Warden verlängert den Abstand je Versuch und gibt nach
    /// `warden::MAX_HOLD_THAW_ATTEMPTS` auf. Fehlt das Feld in der Datei,
    /// gilt `0`.
    #[serde(default)]
    pub attempts: u32,
}

// Dateiform des Schnappschusses.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldFile {
    version: u16,
    holds: Vec<HoldEntry>,
}

/// Persistenter Speicher der laufenden Holds.
///
/// # Description
/// Siehe Moduldoku.
#[derive(Debug)]
pub struct HoldStore {
    dir: PathBuf,
    state: Mutex<HoldState>,
}

// Die laufenden Holds und ob sie vom zuletzt geschriebenen Stand abweichen
// (ein Schreiben des Hold-Timers scheiterte; siehe Moduldoku).
#[derive(Debug, Default)]
struct HoldState {
    holds: Vec<HoldEntry>,
    unsaved: bool,
}

impl HoldStore {
    /// Opens the store in `dir`; a missing file means "no holds".
    ///
    /// # Errors
    /// - [`WardenError::LedgerCorrupt`]: Datei unlesbar, falsche Version,
    ///   doppelter Pfad oder mehr als [`MAX_ACTIVE_HOLDS`] Einträge.
    /// - [`WardenError::Io`]: Lesen scheitert.
    pub fn open(dir: &Path) -> WardenResult<Self> {
        let corrupt = || WardenError::LedgerCorrupt {
            ledger: "holds",
            line: 0,
        };
        let holds = match fsutil::read_optional(dir, HOLDS_FILE)? {
            None => Vec::new(),
            Some(bytes) => {
                let file: HoldFile = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
                if file.version != HOLDS_FORMAT_VERSION || file.holds.len() > MAX_ACTIVE_HOLDS {
                    return Err(corrupt());
                }
                let mut paths: Vec<&str> = file
                    .holds
                    .iter()
                    .map(|hold| hold.target.path.as_str())
                    .collect();
                paths.sort_unstable();
                paths.dedup();
                if paths.len() != file.holds.len() {
                    return Err(corrupt());
                }
                file.holds
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            state: Mutex::new(HoldState {
                holds,
                unsaved: false,
            }),
        })
    }

    /// Arms (or replaces) the hold for `entry.target.path`.
    ///
    /// # Errors
    /// - [`WardenError::LedgerUnavailable`]: bereits [`MAX_ACTIVE_HOLDS`]
    ///   Holds für andere Pfade.
    /// - [`WardenError::StatePoisoned`] / [`WardenError::Encode`] /
    ///   [`WardenError::Io`]: Sperre oder Schreiben scheitert (alter Stand
    ///   bleibt).
    pub fn arm(&self, entry: HoldEntry) -> WardenResult<()> {
        let mut state = self.lock()?;
        let mut next: Vec<HoldEntry> = state
            .holds
            .iter()
            .filter(|hold| hold.target.path != entry.target.path)
            .cloned()
            .collect();
        if next.len() >= MAX_ACTIVE_HOLDS {
            return Err(WardenError::LedgerUnavailable {
                ledger: "holds",
                reason: "full",
            });
        }
        next.push(entry);
        self.persist(&next)?;
        // Geschrieben ist der ganze Stand, auch ein vorher ungesicherter.
        state.holds = next;
        state.unsaved = false;
        Ok(())
    }

    /// Removes the hold for `path`; returns whether one existed.
    ///
    /// # Description
    /// Gibt es keinen Hold für `path`, schreibt die Methode nur, wenn der
    /// Stand im Speicher ungesichert ist (siehe Moduldoku): so kommt ein nur
    /// im Speicher entfernter Hold desselben Pfads nicht zurück.
    ///
    /// # Errors
    /// Wie [`Self::arm`] (Sperre, Kodierung, Schreiben).
    pub fn cancel(&self, path: &CgroupId) -> WardenResult<bool> {
        let mut state = self.lock()?;
        if !state.holds.iter().any(|hold| hold.target.path == *path) {
            if state.unsaved {
                self.sync(&mut state)?;
            }
            return Ok(false);
        }
        let next: Vec<HoldEntry> = state
            .holds
            .iter()
            .filter(|hold| hold.target.path != *path)
            .cloned()
            .collect();
        self.persist(&next)?;
        state.holds = next;
        state.unsaved = false;
        Ok(true)
    }

    /// Moves the due time of the hold for `path` and counts one failed thaw attempt.
    ///
    /// # Description
    /// Für den Hold-Timer nach einem fehlgeschlagenen Auftauen: setzt
    /// `thaw_at` und erhöht `attempts` (gesättigt) um eins. Die Änderung gilt
    /// im Speicher auch dann, wenn das Schreiben scheitert (siehe Moduldoku,
    /// „Buchführung des Hold-Timers auch ohne Schreibzugriff“).
    ///
    /// # Returns
    /// Ob ein Hold für `path` existierte.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet (nichts geändert).
    /// - [`WardenError::Encode`] / [`WardenError::Io`]: nur das Schreiben
    ///   scheiterte; der Speicherstand ist bereits geändert.
    pub fn reschedule(&self, path: &CgroupId, thaw_at: Timestamp) -> WardenResult<bool> {
        let mut state = self.lock()?;
        let Some(hold) = state
            .holds
            .iter_mut()
            .find(|hold| hold.target.path == *path)
        else {
            return Ok(false);
        };
        hold.thaw_at = thaw_at;
        hold.attempts = hold.attempts.saturating_add(1);
        self.sync(&mut state)?;
        Ok(true)
    }

    /// Removes the finished hold for `path` (hold timer only); memory first.
    ///
    /// # Description
    /// Für den Hold-Timer, nachdem ein fälliger Hold aufgetaut, verworfen
    /// oder aufgegeben wurde. Anders als [`Self::cancel`] wird der Hold im
    /// Speicher auch dann entfernt, wenn das Schreiben scheitert; er wird in
    /// diesem Prozess nicht wieder fällig (siehe Moduldoku).
    ///
    /// # Returns
    /// Ob ein Hold für `path` existierte.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet (nichts geändert).
    /// - [`WardenError::Encode`] / [`WardenError::Io`]: nur das Schreiben
    ///   scheiterte; der Hold ist im Speicher bereits entfernt.
    pub fn forget(&self, path: &CgroupId) -> WardenResult<bool> {
        let mut state = self.lock()?;
        let before = state.holds.len();
        state.holds.retain(|hold| hold.target.path != *path);
        if state.holds.len() == before {
            return Ok(false);
        }
        self.sync(&mut state)?;
        Ok(true)
    }

    /// Writes the in-memory holds if an earlier write of the hold timer failed.
    ///
    /// # Description
    /// Ohne ungesicherten Stand schreibt die Methode nichts (siehe
    /// Moduldoku, „Buchführung des Hold-Timers auch ohne Schreibzugriff“).
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    /// - [`WardenError::Encode`] / [`WardenError::Io`]: das Schreiben
    ///   scheitert erneut (der Stand bleibt ungesichert).
    pub fn flush(&self) -> WardenResult<()> {
        let mut state = self.lock()?;
        if !state.unsaved {
            return Ok(());
        }
        self.sync(&mut state)
    }

    /// Returns the holds due at `now` (`thaw_at <= now`), oldest first.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn due(&self, now: Timestamp) -> WardenResult<Vec<HoldEntry>> {
        let state = self.lock()?;
        let mut due: Vec<HoldEntry> = state
            .holds
            .iter()
            .filter(|hold| hold.thaw_at <= now)
            .cloned()
            .collect();
        due.sort_by_key(|hold| hold.thaw_at);
        Ok(due)
    }

    /// Returns all running holds.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn snapshot(&self) -> WardenResult<Vec<HoldEntry>> {
        Ok(self.lock()?.holds.clone())
    }

    // Die Sperre um den Stand.
    fn lock(&self) -> WardenResult<std::sync::MutexGuard<'_, HoldState>> {
        self.state.lock().map_err(|_| WardenError::StatePoisoned)
    }

    // Schreibt den Stand im Speicher und merkt sich, ob er damit gesichert ist.
    fn sync(&self, state: &mut HoldState) -> WardenResult<()> {
        let written = self.persist(&state.holds);
        state.unsaved = written.is_err();
        written
    }

    // Schreibt den Schnappschuss atomar.
    fn persist(&self, holds: &[HoldEntry]) -> WardenResult<()> {
        let file = HoldFile {
            version: HOLDS_FORMAT_VERSION,
            holds: holds.to_vec(),
        };
        let bytes = serde_json::to_vec(&file).map_err(WardenError::Encode)?;
        fsutil::replace_file_atomically(&self.dir, HOLDS_FILE, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{HOLDS_FILE, HoldEntry, HoldStore};
    use crate::error::WardenError;
    use crate::test_support::{TestResult, ctx};
    use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
    use harw_types::{CgroupId, FindingId};
    use jiff::Timestamp;

    fn hold(path: &str, thaw_at: i64) -> TestResult<HoldEntry> {
        Ok(HoldEntry {
            target: CgroupTarget::new(
                CgroupId::try_from_str(path).map_err(ctx("cgroup"))?,
                CgroupKernelId::new(77).map_err(ctx("kernel id"))?,
                Timestamp::from_second(1_700_000_000).map_err(ctx("ts"))?,
            ),
            thaw_at: Timestamp::from_second(thaw_at).map_err(ctx("ts"))?,
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding"))?,
            nonce: "01".repeat(16),
            attempts: 0,
        })
    }

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    #[test]
    fn test_holds_survive_restart_and_become_due() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        {
            let store = HoldStore::open(dir.path()).map_err(ctx("open"))?;
            store
                .arm(hold("harw.slice/a", 100)?)
                .map_err(ctx("arm a"))?;
            store
                .arm(hold("harw.slice/b", 200)?)
                .map_err(ctx("arm b"))?;
        }
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen"))?;
        assert_eq!(reopened.snapshot().map_err(ctx("snapshot"))?.len(), 2);
        assert!(reopened.due(at(99)?).map_err(ctx("due"))?.is_empty());
        let due = reopened.due(at(150)?).map_err(ctx("due"))?;
        assert_eq!(due, vec![hold("harw.slice/a", 100)?]);
        Ok(())
    }

    #[test]
    fn test_arm_replaces_and_cancel_removes_per_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = HoldStore::open(dir.path()).map_err(ctx("open"))?;
        store.arm(hold("harw.slice/a", 100)?).map_err(ctx("arm"))?;
        store
            .arm(hold("harw.slice/a", 300)?)
            .map_err(ctx("re-arm"))?;
        assert_eq!(
            store.snapshot().map_err(ctx("snapshot"))?,
            vec![hold("harw.slice/a", 300)?]
        );
        let path = CgroupId::try_from_str("harw.slice/a").map_err(ctx("cgroup"))?;
        assert!(
            store
                .reschedule(&path, at(400)?)
                .map_err(ctx("reschedule"))?
        );
        let mut retried = hold("harw.slice/a", 400)?;
        retried.attempts = 1;
        assert_eq!(store.due(at(400)?).map_err(ctx("due"))?, vec![retried]);
        assert!(store.cancel(&path).map_err(ctx("cancel"))?);
        assert!(!store.cancel(&path).map_err(ctx("cancel again"))?);
        drop(store);
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen"))?;
        assert!(reopened.snapshot().map_err(ctx("snapshot"))?.is_empty());
        Ok(())
    }

    // Eine fremde `holds.v2.json.tmp` (Symlink) lässt jedes Schreiben
    // scheitern (`O_NOFOLLOW`), auch für root.
    fn block_writes(dir: &std::path::Path) -> TestResult {
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join(format!("{HOLDS_FILE}.tmp")))
            .map_err(ctx("symlink tmp"))
    }

    #[test]
    fn test_timer_bookkeeping_applies_in_memory_when_writing_fails() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = HoldStore::open(dir.path()).map_err(ctx("open"))?;
        store
            .arm(hold("harw.slice/a", 100)?)
            .map_err(ctx("arm a"))?;
        store
            .arm(hold("harw.slice/b", 100)?)
            .map_err(ctx("arm b"))?;
        block_writes(dir.path())?;
        let a = CgroupId::try_from_str("harw.slice/a").map_err(ctx("cgroup"))?;
        let b = CgroupId::try_from_str("harw.slice/b").map_err(ctx("cgroup"))?;

        // Neuer Abstand und Versuchszähler gelten trotz Schreibfehler.
        assert!(store.reschedule(&a, at(400)?).is_err());
        let mut retried = hold("harw.slice/a", 400)?;
        retried.attempts = 1;
        assert_eq!(store.due(at(399)?).map_err(ctx("due"))?.len(), 1);
        assert!(
            store
                .snapshot()
                .map_err(ctx("snapshot"))?
                .contains(&retried)
        );
        // Ein erledigter Hold wird nicht wieder fällig.
        assert!(store.forget(&b).is_err());
        assert_eq!(store.due(at(10_000)?).map_err(ctx("due"))?.len(), 1);
        // Anfragepfade bleiben streng: ohne Schreiben keine Änderung.
        assert!(store.cancel(&a).is_err());
        assert_eq!(store.snapshot().map_err(ctx("snapshot"))?, vec![retried]);
        assert!(!store.forget(&b).map_err(ctx("forget twice"))?);

        // Nach einem Neustart gilt der zuletzt geschriebene Stand.
        drop(store);
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.snapshot().map_err(ctx("snapshot"))?,
            vec![hold("harw.slice/a", 100)?, hold("harw.slice/b", 100)?]
        );
        Ok(())
    }

    #[test]
    fn test_a_hold_forgotten_only_in_memory_never_comes_back() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = HoldStore::open(dir.path()).map_err(ctx("open"))?;
        store.arm(hold("harw.slice/a", 100)?).map_err(ctx("arm"))?;
        let path = CgroupId::try_from_str("harw.slice/a").map_err(ctx("cgroup"))?;
        block_writes(dir.path())?;
        assert!(store.forget(&path).is_err());
        // Solange der Stand ungesichert ist, scheitert auch ein `cancel` ohne
        // Hold des Pfads: der Freeze, der damit aufheben wollte, läuft nicht.
        assert!(store.cancel(&path).is_err());
        assert!(store.flush().is_err());
        let tmp = dir.path().join(format!("{HOLDS_FILE}.tmp"));
        std::fs::remove_file(&tmp).map_err(ctx("unblock writes"))?;
        // `cancel` schreibt den Stand nach, obwohl im Speicher nichts zu tun ist.
        assert!(!store.cancel(&path).map_err(ctx("cancel"))?);
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen"))?;
        assert!(reopened.snapshot().map_err(ctx("snapshot"))?.is_empty());
        drop(reopened);

        // Ebenso `flush` (Hold-Timer); ein gesicherter Stand schreibt nichts.
        store
            .arm(hold("harw.slice/b", 100)?)
            .map_err(ctx("arm b"))?;
        let b = CgroupId::try_from_str("harw.slice/b").map_err(ctx("cgroup"))?;
        block_writes(dir.path())?;
        assert!(store.forget(&b).is_err());
        std::fs::remove_file(&tmp).map_err(ctx("unblock again"))?;
        store.flush().map_err(ctx("flush"))?;
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen b"))?;
        assert!(reopened.snapshot().map_err(ctx("snapshot b"))?.is_empty());
        block_writes(dir.path())?;
        store.flush().map_err(ctx("flush without changes"))?;
        assert!(!store.cancel(&b).map_err(ctx("cancel saved"))?);
        Ok(())
    }

    #[test]
    fn test_forget_removes_and_persists() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = HoldStore::open(dir.path()).map_err(ctx("open"))?;
        store.arm(hold("harw.slice/a", 100)?).map_err(ctx("arm"))?;
        let path = CgroupId::try_from_str("harw.slice/a").map_err(ctx("cgroup"))?;
        assert!(store.forget(&path).map_err(ctx("forget"))?);
        assert!(!store.forget(&path).map_err(ctx("forget again"))?);
        drop(store);
        let reopened = HoldStore::open(dir.path()).map_err(ctx("reopen"))?;
        assert!(reopened.snapshot().map_err(ctx("snapshot"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_corrupt_snapshot_refuses_open() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(
            dir.path().join(HOLDS_FILE),
            b"{\"version\":2,\"holds\":[],\"x\":1}",
        )
        .map_err(ctx("write"))?;
        assert!(matches!(
            HoldStore::open(dir.path()),
            Err(WardenError::LedgerCorrupt { .. })
        ));
        std::fs::write(dir.path().join(HOLDS_FILE), b"{\"version\":1,\"holds\":[]}")
            .map_err(ctx("write"))?;
        assert!(matches!(
            HoldStore::open(dir.path()),
            Err(WardenError::LedgerCorrupt { .. })
        ));
        Ok(())
    }
}

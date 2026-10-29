//! Persistent, transaction-framed implementation of [`PlanStore`].
//!
//! A plan lives below `<root>/plans/<plan_id>/`: append-only `history.jsonl`
//! is authoritative; `rev-<n>.json` and `.seal` are periodic checkpoints.
//! Each new batch is one bounded, checksummed, LF-terminated WAL record.
//! Legacy per-event lines remain readable, including mixed legacy/new logs.
//! Only an unterminated EOF may be truncated during locked recovery. A
//! malformed complete record, digest mismatch or revision gap fails closed.
//!
//! # Commit and recovery
//! Snapshot/seal files are staged first. The checked publication hook runs
//! immediately before the WAL append. Success requires WAL and directory
//! synchronization; after that point checkpoint/compaction errors are
//! maintenance warnings, not rejected mutations. The initial record includes
//! the exact created plan (including tenant), so no checkpoint is needed for
//! crash recovery. A failed append is durably rolled back or returns
//! [`PlanError::CommitIndeterminate`] and fences this instance until reopening.
//! Create and catalog activation are separate records: activation failure
//! after Create also requires explicit recovery, never blind replay.
//!
//! # Concurrency
//! Lock order is caller JobLeaseGuard (when present), local write lock, then
//! nonblocking root OS lock. The publication callback must not reenter the
//! plan store or acquire another job lock. All fallible reads and mutations
//! refresh durable state under that root lock. Normal refresh reads only the
//! new WAL suffix; startup and checkpoint-less recovery scan full history.
//! `revision()` is only a cached hint, never a CAS/admission authority.
//!
//! # Trust and compatibility
//! The root directory, lock inode and existing WAL prefixes must remain under
//! trusted runtime ownership. Locks serialize cooperating new writers; older
//! binaries must not write the new format. This is not protection from a
//! process that can replace store files/locks. Digests and checkpoint seals
//! detect corruption, not an attacker who can recompute them. Plan IDs are
//! validated before constructing paths. Snapshot-only legacy stores remain
//! readable but cannot mutate until their audit history is restored. Legacy
//! partial batches cannot be reconstructed retroactively. Records, including
//! legacy ones, are bounded to 16 MiB; oversized records are rejected.
//!
//! The catalog `<root>/plan-index.json` stores selection, archival and
//! approvals. Corrupt inactive snapshots remain unavailable rather than being
//! overwritten. Reads may repair a torn tail and synchronize recovery; they
//! never invent missing committed events. Mutation semantics remain centralized
//! in [`crate::store::stage_actions`], shared with the in-memory implementation.

use std::io::{BufReader, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

use fs4::FileExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use tracing::{debug, info, warn};

use crate::actions::{PlanAction, PlanEvent};
use crate::catalog::{PLAN_INDEX_FILE, PlanApproval, PlanIndex, PlanMeta, PlanSummary};
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId};
use crate::mutation::apply_mutation;
use crate::store::{PlanRevision, PlanStore, check_batch_target, stage_actions};
use crate::types::{Plan, PlanNodeStatus};

/// Höchstens neun History-Ereignisse müssen nach einem Checkpoint nachgespielt werden.
const CHECKPOINT_INTERVAL: u64 = 10;

/// Integritätssiegel für Snapshot-Dateien (crate-privat, geteilt mit
/// `goal_store::FileGoalStore`).
///
/// # Format
/// `rev-<n>.seal` neben `rev-<n>.json`, JSON-Objekt [`SnapshotSeal`]:
/// - `digest` = BLAKE3 (`harw_types::ContentDigest`) der exakten Snapshot-Bytes,
/// - `chain` = BLAKE3 über `"harw-plan-seal:v1\0" ‖ prev_chain ‖ "\0" ‖ n ‖
///   "\0" ‖ digest`,
/// - `prev_revision`/`prev_chain` = letztes Kettenglied vor dieser Revision
///   (`None` beim ersten versiegelten Stand, z. B. nach einem Legacy-Plan).
///
/// # Prüfung beim Laden
/// Digest, Revision, Kettenwert und — falls vorhanden — der Kettenwert des
/// Vorgängersiegels müssen stimmen. Fehlt das Siegel in einem Verzeichnis, das
/// bereits Siegel enthält, ist das eine Manipulation. Ein Verzeichnis ganz ohne
/// Siegel gilt als Legacy-Stand: einmal `warn!`, der nächste Schreibvorgang
/// versiegelt.
///
/// # Grenzen
/// Ungeschlüsselt: erkennt Beschädigung und naive Manipulation, nicht einen
/// Angreifer mit Schreibrecht auf `HARW_HOME`.
pub(crate) mod seal {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Serialize};
    use tracing::warn;

    use crate::error::{PlanError, PlanResult};

    /// Aktuelle Formatversion.
    pub(crate) const SEAL_VERSION: u32 = 1;

    /// Domänentrenner des Kettenwerts.
    const CHAIN_DOMAIN: &str = "harw-plan-seal:v1";

    // Zähler für eindeutige Temp-Namen innerhalb eines Prozesses.
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Inhalt einer `rev-<n>.seal`-Datei.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub(crate) struct SnapshotSeal {
        /// Formatversion ([`SEAL_VERSION`]).
        pub(crate) version: u32,
        /// Revision des versiegelten Snapshots.
        pub(crate) revision: u64,
        /// Hex-Digest der Snapshot-Bytes.
        pub(crate) digest: String,
        /// Revision des vorherigen Kettenglieds.
        #[serde(default)]
        pub(crate) prev_revision: Option<u64>,
        /// Kettenwert des vorherigen Kettenglieds.
        #[serde(default)]
        pub(crate) prev_chain: Option<String>,
        /// Kettenwert dieses Siegels.
        pub(crate) chain: String,
    }

    /// Letztes Kettenglied eines Stores.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct SealLink {
        /// Revision des zuletzt versiegelten Snapshots.
        pub(crate) revision: u64,
        /// Dessen Kettenwert.
        pub(crate) chain: String,
    }

    /// Pfad des Siegels zu einem Snapshot (`rev-<n>.json` → `rev-<n>.seal`).
    pub(crate) fn seal_path(snapshot: &Path) -> PathBuf {
        snapshot.with_extension("seal")
    }

    /// Berechnet den Kettenwert.
    pub(crate) fn chain_value(prev_chain: Option<&str>, revision: u64, digest: &str) -> String {
        let material = format!(
            "{CHAIN_DOMAIN}\0{}\0{revision}\0{digest}",
            prev_chain.unwrap_or("")
        );
        harw_types::ContentDigest::of(material.as_bytes()).to_string()
    }

    /// Erzeugt Siegel-Bytes und neues Kettenglied für einen Snapshot.
    ///
    /// # Errors
    /// - [`PlanError::Serde`] wenn das Siegel nicht serialisiert werden kann.
    pub(crate) fn build(
        prev: Option<&SealLink>,
        revision: u64,
        snapshot: &[u8],
    ) -> PlanResult<(Vec<u8>, SealLink)> {
        let digest = harw_types::ContentDigest::of(snapshot).to_string();
        let prev_chain = prev.map(|link| link.chain.as_str());
        let chain = chain_value(prev_chain, revision, &digest);
        let seal = SnapshotSeal {
            version: SEAL_VERSION,
            revision,
            digest,
            prev_revision: prev.map(|link| link.revision),
            prev_chain: prev_chain.map(str::to_owned),
            chain: chain.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&seal)?;
        Ok((bytes, SealLink { revision, chain }))
    }

    /// Prüft, ob `dir` mindestens ein Siegel enthält.
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Lesefehler.
    pub(crate) fn directory_is_sealed(dir: &Path) -> PlanResult<bool> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("rev-") && name.ends_with(".seal"))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // Baut den Fehler für eine Siegelverletzung.
    fn mismatch(path: &Path, expected: impl Into<String>, actual: impl Into<String>) -> PlanError {
        PlanError::SealMismatch {
            path: path.display().to_string(),
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    // Liest und parst ein Siegel; jede Unlesbarkeit ist eine Siegelverletzung.
    fn read_seal(path: &Path) -> PlanResult<SnapshotSeal> {
        let bytes = std::fs::read(path)
            .map_err(|error| mismatch(path, "lesbares Siegel", error.to_string()))?;
        let seal: SnapshotSeal = serde_json::from_slice(&bytes)
            .map_err(|error| mismatch(path, "gültiges Siegel-JSON", error.to_string()))?;
        if seal.version != SEAL_VERSION {
            return Err(mismatch(
                path,
                format!("version {SEAL_VERSION}"),
                format!("version {}", seal.version),
            ));
        }
        Ok(seal)
    }

    /// Prüft das Siegel eines geladenen Snapshots.
    ///
    /// # Arguments
    /// - `snapshot_path` (`&Path`): Pfad des `rev-<n>.json`.
    /// - `revision` (`u64`): Revision laut Dateiname.
    /// - `bytes` (`&[u8]`): exakt die gelesenen Snapshot-Bytes.
    /// - `dir_sealed` (`bool`): enthält das Verzeichnis bereits Siegel?
    ///
    /// # Returns
    /// `Some(SealLink)` bei gültigem Siegel, `None` bei einem Legacy-Stand
    /// (Verzeichnis ohne jedes Siegel; es wird einmal gewarnt).
    ///
    /// # Errors
    /// - [`PlanError::SealMismatch`]: Siegel fehlt (in versiegeltem
    ///   Verzeichnis), ist unlesbar, trägt eine andere Revision, einen anderen
    ///   Digest oder Kettenwert, oder das Vorgängersiegel passt nicht.
    pub(crate) fn verify(
        snapshot_path: &Path,
        revision: u64,
        bytes: &[u8],
        dir_sealed: bool,
    ) -> PlanResult<Option<SealLink>> {
        let path = seal_path(snapshot_path);
        if !path.exists() {
            if dir_sealed {
                return Err(mismatch(&path, "Siegeldatei vorhanden", "fehlt"));
            }
            warn!(
                path = %snapshot_path.display(),
                "Legacy-Snapshot ohne Siegel; der nächste Schreibvorgang versiegelt"
            );
            return Ok(None);
        }

        let seal = read_seal(&path)?;
        if seal.revision != revision {
            return Err(mismatch(
                &path,
                format!("revision {revision}"),
                format!("revision {}", seal.revision),
            ));
        }
        let digest = harw_types::ContentDigest::of(bytes).to_string();
        if seal.digest != digest {
            return Err(mismatch(&path, seal.digest, digest));
        }
        let chain = chain_value(seal.prev_chain.as_deref(), revision, &seal.digest);
        if seal.chain != chain {
            return Err(mismatch(&path, seal.chain, chain));
        }
        match (seal.prev_revision, seal.prev_chain.as_deref()) {
            (None, None) => {}
            (Some(prev_revision), Some(prev_chain)) => {
                let prev_path = path.with_file_name(format!("rev-{prev_revision}.seal"));
                let prev = read_seal(&prev_path)?;
                if prev.chain != prev_chain {
                    return Err(mismatch(&prev_path, prev_chain, prev.chain));
                }
            }
            _ => {
                return Err(mismatch(
                    &path,
                    "prev_revision und prev_chain gemeinsam gesetzt",
                    "nur eines gesetzt",
                ));
            }
        }
        Ok(Some(SealLink { revision, chain }))
    }

    /// Synchronisierter, noch nicht sichtbarer Siegel-Write.
    pub(crate) struct StagedSeal {
        target: PathBuf,
        temporary: PathBuf,
    }

    impl StagedSeal {
        /// Veröffentlicht das Siegel per `rename` und synchronisiert das
        /// Verzeichnis.
        ///
        /// # Errors
        /// - [`PlanError::Io`] bei Fehlschlag (Temp-Datei wird entfernt).
        pub(crate) fn commit(self) -> PlanResult<()> {
            if let Err(error) = std::fs::rename(&self.temporary, &self.target) {
                // Best effort: das Aufräumen darf den eigentlichen Fehler nicht überdecken.
                let _ = std::fs::remove_file(&self.temporary);
                return Err(error.into());
            }
            if let Some(parent) = self.target.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        }

        /// Verwirft das gestagte Siegel.
        pub(crate) fn discard(self) {
            // Best effort: eine verwaiste Temp-Datei ist harmlos (`.tmp-`).
            let _ = std::fs::remove_file(self.temporary);
        }
    }

    /// Schreibt Siegel-Bytes synchronisiert in eine eindeutige Temp-Datei
    /// neben dem Ziel (`<name>.tmp-<pid>-<nanos>-<zähler>`).
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Schreibfehler.
    pub(crate) fn stage(target: &Path, bytes: &[u8]) -> PlanResult<StagedSeal> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("seal");
        let temporary = target.with_file_name(format!(
            "{name}.tmp-{}-{nonce}-{counter}",
            std::process::id()
        ));
        let result = (|| -> PlanResult<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            // Best effort: der Schreibfehler ist die relevante Meldung.
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(StagedSeal {
            target: target.to_path_buf(),
            temporary,
        })
    }
}

/// Ein Plan im Katalog samt History, Revisionsfolge und Siegelkette.
struct Slot {
    bootstrap: Option<Plan>,
    receipts: Vec<crate::parent_progress::ParentProgressReceipt>,
    /// Aktueller Planzustand (gecacht im RAM).
    plan: Plan,
    /// Event-History (gecacht).
    history: Vec<PlanEvent>,
    /// Nächste Revisionsnummer dieses Plans.
    next_revision: RevisionId,
    /// Letztes Glied der Siegelkette (`None` vor dem ersten Siegel).
    seal: Option<seal::SealLink>,
    /// Katalogdaten (Archiv, Freigabestand).
    meta: PlanMeta,
    /// Validated byte boundary in the append-only WAL.
    wal_offset: u64,
}

/// Interner Zustand des File-Stores.
struct Inner {
    /// Alle Pläne in Anlagereihenfolge.
    plans: Vec<Slot>,
    /// Der aktive Plan (Ziel aller Mutationen).
    active: Option<PlanId>,
    /// Wurzelverzeichnis.
    root: PathBuf,
}

impl Inner {
    // Der aktive Plan.
    fn active_slot(&self) -> Option<&Slot> {
        let active = self.active.as_ref()?;
        self.plans.iter().find(|slot| &slot.plan.id == active)
    }

    // Index des aktiven Plans.
    fn active_position(&self) -> Option<usize> {
        let active = self.active.as_ref()?;
        self.plans.iter().position(|slot| &slot.plan.id == active)
    }

    // Index des Plans `id` oder `PlanUnknown`.
    fn position_of(&self, id: &PlanId) -> PlanResult<usize> {
        self.plans
            .iter()
            .position(|slot| &slot.plan.id == id)
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    // Der Katalog-Index, wie er nach einer Änderung auf Platte gehört.
    fn index(&self) -> PlanIndex {
        let mut index = PlanIndex {
            active: self.active.as_ref().map(|id| id.as_str().to_owned()),
            ..PlanIndex::default()
        };
        for slot in &self.plans {
            index
                .plans
                .insert(slot.plan.id.as_str().to_owned(), slot.meta);
        }
        index
    }
}

/// Vollständig synchronisierter, aber noch nicht sichtbarer Snapshot-Write.
struct StagedWrite {
    target: PathBuf,
    temporary: PathBuf,
}

impl StagedWrite {
    /// Veröffentlicht die synchronisierte Temp-Datei erst nach dem dauerhaften
    /// History-Append und synchronisiert danach den Verzeichniseintrag.
    fn commit(self) -> PlanResult<()> {
        if let Err(error) = std::fs::rename(&self.temporary, &self.target) {
            // Best effort: der Rename-Fehler ist die relevante Meldung.
            let _ = std::fs::remove_file(&self.temporary);
            return Err(error.into());
        }
        FilePlanStore::sync_parent_directory(&self.target).map_err(|_| {
            PlanError::CommitIndeterminate {
                path: self.target.display().to_string(),
            }
        })
    }

    /// Verwirft den noch nicht sichtbaren Snapshot nach einem fehlgeschlagenen
    /// History-Append.
    fn discard(self) {
        // Best effort: eine verwaiste Temp-Datei ist harmlos.
        let _ = std::fs::remove_file(self.temporary);
    }
}

/// Persistenter `PlanStore`, der Pläne und History im Dateisystem ablegt.
///
/// # Description
/// Schreibt jeden Plan-Zustand als `<root>/plans/<plan_id>/rev-<n>.json` mit
/// Siegel `rev-<n>.seal`. Events werden append-only in
/// `<root>/plans/<plan_id>/history.jsonl` geschrieben. Each batch is one WAL
/// frame; checkpoints are periodic, not one per event.
///
/// # Concurrency
/// `Send + Sync` through the local lock plus a nonblocking root OS lock.
/// Fallible reads refresh under the same writer boundary as mutations.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::file_store::FilePlanStore;
/// use harw_plan::store::PlanStore;
/// use harw_plan::actions::PlanAction;
/// use harw_plan::ids::PlanId;
///
/// let store = FilePlanStore::new("/tmp/myplans").unwrap();
/// store.apply(PlanAction::Create {
///     plan_id: PlanId::parse("p-1").unwrap(),
///     goal: "Ziel".to_owned(),
/// }, "orchestrator").unwrap();
/// ```
pub struct FilePlanStore {
    inner: RwLock<Inner>,
    config: PlanToolConfig,
    indeterminate: AtomicBool,
}

/// Ergebnis von [`FilePlanStore::load`]: der aus `<root>/plans/` und
/// `<root>/plan-index.json` rekonstruierte Katalog.
struct LoadedCatalog {
    plans: Vec<Slot>,
    active: Option<PlanId>,
}

/// Ein aus seinem Verzeichnis geladener Plan (ohne Katalogdaten).
struct LoadedPlan {
    bootstrap: Option<Plan>,
    receipts: Vec<crate::parent_progress::ParentProgressReceipt>,
    plan: Plan,
    history: Vec<PlanEvent>,
    next_revision: RevisionId,
    seal: Option<seal::SealLink>,
    wal_offset: u64,
}

struct Published {
    seal: Option<seal::SealLink>,
    wal_offset: u64,
}

/// Local guard precedes the nonblocking OS lock. The descriptor is never shared.
struct LockedInner<'a> {
    inner: std::sync::RwLockWriteGuard<'a, Inner>,
    _file: RootLock,
}

/// Explicit unlock matters when another thread forks while this descriptor
/// is open: merely closing the parent descriptor can leave its inherited
/// open-file-description lock alive in the child until exec.
struct RootLock {
    file: std::fs::File,
}

impl Drop for RootLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            warn!(%error, "Plan store root unlock failed; closing descriptor");
        }
    }
}

impl Deref for LockedInner<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl DerefMut for LockedInner<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

/// Minimal boundary for deterministic short-write/fsync/rollback fault tests.
trait WalSink: Write {
    fn durable(&mut self) -> std::io::Result<()>;
    fn rollback(&mut self, original_len: u64) -> std::io::Result<()>;
}

struct FileWalSink<'a> {
    file: &'a mut std::fs::File,
    path: &'a Path,
}

impl Write for FileWalSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl WalSink for FileWalSink<'_> {
    fn durable(&mut self) -> std::io::Result<()> {
        self.file.sync_all()?;
        FilePlanStore::sync_parent_directory(self.path)
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
    fn rollback(&mut self, original_len: u64) -> std::io::Result<()> {
        self.file.set_len(original_len)?;
        self.durable()
    }
}

/// Success means durable commit; ordinary error means durable rollback;
/// indeterminate means neither can be established and the instance is fenced.
fn append_transaction(
    sink: &mut impl WalSink,
    bytes: &[u8],
    original_len: u64,
    path: &Path,
) -> PlanResult<()> {
    let result = sink
        .write_all(bytes)
        .and_then(|()| sink.flush())
        .and_then(|()| sink.durable());
    if let Err(error) = result {
        if sink.rollback(original_len).is_err() {
            return Err(PlanError::CommitIndeterminate {
                path: path.display().to_string(),
            });
        }
        return Err(error.into());
    }
    Ok(())
}

impl FilePlanStore {
    /// Erstellt einen neuen `FilePlanStore` mit dem angegebenen Wurzelverzeichnis.
    ///
    /// # Arguments
    /// - `root` (`impl AsRef<Path>`): Wurzelverzeichnis für alle Plandateien.
    ///
    /// # Errors
    /// - [`PlanError::Io`] wenn das Verzeichnis nicht erstellt werden kann.
    /// - [`PlanError::SealMismatch`] wenn der neueste Snapshot manipuliert ist.
    ///
    /// Dieser Kompatibilitätspfad verwendet explizit die aktivierten
    /// Standardwerte. Neue Aufrufer sollen [`Self::with_config`] verwenden,
    /// damit ihre Tool-Konfiguration an der Persistenzgrenze erzwungen wird.
    pub fn new(root: impl AsRef<Path>) -> PlanResult<Self> {
        Self::with_config(root, PlanToolConfig::enabled_defaults())
    }

    /// Erstellt einen neuen `FilePlanStore` und erzwingt die Plan-Tool-Konfiguration.
    ///
    /// Die Konfiguration wird vor dem Anlegen des Wurzelverzeichnisses geprüft.
    /// Ein bereits vorhandener Plan wird vor dem Exponieren des Stores gegen
    /// sein Integritätssiegel und das Knotenlimit validiert.
    ///
    /// # Errors
    /// - [`PlanError::Config`] wenn die Konfiguration deaktiviert oder ungültig ist.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] wenn Plandateien nicht lesbar sind.
    /// - [`PlanError::SealMismatch`] wenn das Siegel des neuesten Snapshots
    ///   nicht passt.
    /// - [`PlanError::InvalidId`] wenn der Snapshot eine andere ID trägt als
    ///   sein Verzeichnis.
    pub fn with_config(root: impl AsRef<Path>, config: PlanToolConfig) -> PlanResult<Self> {
        config.require_enabled().map_err(PlanError::Config)?;
        let root = if root.as_ref().is_absolute() {
            root.as_ref().to_path_buf()
        } else {
            std::env::current_dir()?.join(root)
        };
        Self::create_root_durably(&root)?;
        let _lock = Self::lock_root(&root)?;
        let loaded = Self::load(&root)?;
        std::fs::File::open(&root)?.sync_all()?;
        Self::sync_parent_directory(&root)?;
        // Wie bisher wird der aktive Plan gegen die Konfiguration geprüft;
        // inaktive Pläne erst, wenn sie per Mutation wieder angefasst werden.
        if let Some(active) = &loaded.active
            && let Some(slot) = loaded.plans.iter().find(|slot| &slot.plan.id == active)
        {
            config
                .validate_plan(&slot.plan)
                .map_err(PlanError::Config)?;
        }
        Ok(Self {
            inner: RwLock::new(Inner {
                plans: loaded.plans,
                active: loaded.active,
                root,
            }),
            config,
            indeterminate: AtomicBool::new(false),
        })
    }

    /// Lädt beim Start alle Pläne und den Katalog-Index aus dem Dateibaum.
    ///
    /// Die Dateinamen sind die durable Sequenzquelle. Berücksichtigt werden
    /// nur Verzeichnisse, deren Name eine gültige [`PlanId`] ist (andere werden
    /// mit `warn!` übersprungen) und die mindestens einen Snapshot enthalten.
    /// Aktiv ist der Plan aus `plan-index.json`; fehlt der Index
    /// (Altbestand, Migration), der Plan mit der höchsten Snapshot-Revision
    /// (Tie-Break: lexikografisch größere Plan-ID) — genau die Wahl des
    /// früheren Einzelplan-Stores.
    ///
    /// # Errors
    /// Lese-, Siegel- und ID-Fehler des **aktiven** Plans sowie ein
    /// unlesbarer Index. Fehler inaktiver Pläne werden mit `warn!`
    /// übersprungen.
    fn load(root: &Path) -> PlanResult<LoadedCatalog> {
        let plans_root = root.join("plans");
        let index = Self::read_index(root)?;
        if !plans_root.exists() {
            return Ok(LoadedCatalog {
                plans: Vec::new(),
                active: None,
            });
        }

        let candidates = Self::candidates(root)?;

        // Aktiver Plan: laut Index, sonst die Wahl des Einzelplan-Stores.
        let active = match &index {
            Some(index) => index
                .active_id()
                .filter(|id| candidates.iter().any(|(candidate, _, _)| candidate == id)),
            None => candidates
                .iter()
                .max_by(|(left_id, left_rev, _), (right_id, right_rev, _)| {
                    left_rev
                        .cmp(right_rev)
                        .then_with(|| left_id.as_str().cmp(right_id.as_str()))
                })
                .map(|(id, _, _)| id.clone()),
        };

        let mut plans = Vec::with_capacity(candidates.len());
        for (plan_id, file_revision, snapshot_path) in candidates {
            let is_active = active.as_ref() == Some(&plan_id);
            match Self::load_plan(root, &plan_id, file_revision, snapshot_path.as_deref()) {
                Ok(loaded) => {
                    let meta = index
                        .as_ref()
                        .map(|index| index.meta(&plan_id))
                        .unwrap_or_default();
                    plans.push(Slot {
                        plan: loaded.plan,
                        history: loaded.history,
                        bootstrap: loaded.bootstrap,
                        receipts: loaded.receipts,
                        next_revision: loaded.next_revision,
                        seal: loaded.seal,
                        meta,
                        wal_offset: loaded.wal_offset,
                    });
                }
                Err(error) if is_active => return Err(error),
                Err(error) => {
                    warn!(
                        plan_id = %plan_id,
                        error = %error,
                        "Inaktiver Plan ist nicht lesbar und wird übersprungen"
                    );
                }
            }
        }
        plans.sort_by(|left, right| {
            left.plan
                .created_at
                .cmp(&right.plan.created_at)
                .then_with(|| left.plan.id.as_str().cmp(right.plan.id.as_str()))
        });
        Ok(LoadedCatalog { plans, active })
    }

    fn candidates(root: &Path) -> PlanResult<Vec<(PlanId, RevisionId, Option<PathBuf>)>> {
        let plans_root = root.join("plans");
        // (Plan-ID, höchste Snapshot-Revision, Pfad dieses Snapshots)
        let mut candidates: Vec<(PlanId, RevisionId, Option<PathBuf>)> = Vec::new();
        for entry in std::fs::read_dir(&plans_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir_name = entry.file_name();
            let Some(plan_id) = dir_name.to_str().and_then(|name| PlanId::parse(name).ok()) else {
                warn!(
                    dir = %entry.path().display(),
                    "Plan-Verzeichnis mit ungültigem Namen wird ignoriert"
                );
                continue;
            };
            let mut newest: Option<(RevisionId, PathBuf)> = None;
            for file in std::fs::read_dir(entry.path())? {
                let file = file?;
                if !file.file_type()?.is_file() {
                    continue;
                }
                let Some(number) = file
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix("rev-"))
                    .and_then(|n| n.strip_suffix(".json"))
                    .and_then(|n| n.parse::<u64>().ok())
                else {
                    continue;
                };
                let revision = RevisionId::new(number);
                if newest
                    .as_ref()
                    .is_none_or(|(current, _)| revision > *current)
                {
                    newest = Some((revision, file.path()));
                }
            }
            if let Some((revision, path)) = newest {
                candidates.push((plan_id, revision, Some(path)));
            } else if entry.path().join("history.jsonl").is_file() {
                let wal = Self::read_wal(root, &plan_id, 0, RevisionId::new(0))?;
                // An interrupted first Create has no committed state. Repair
                // its torn tail, but do not register a phantom plan.
                if !wal.events.is_empty() {
                    candidates.push((plan_id, wal.last_revision, None));
                }
            }
        }
        Ok(candidates)
    }

    /// Lädt einen Plan aus seinem Verzeichnis: neuester Snapshot (gegen sein
    /// Siegel geprüft, muss die ID seines Verzeichnisses tragen) plus alle
    /// späteren History-Ereignisse.
    fn load_plan(
        root: &Path,
        plan_id: &PlanId,
        file_revision: RevisionId,
        snapshot_path: Option<&Path>,
    ) -> PlanResult<LoadedPlan> {
        let (snapshot, seal_link) = if let Some(path) = snapshot_path {
            let bytes = std::fs::read(path)?;
            let dir_sealed = Self::has_sealed_snapshot(&Self::plan_dir(root, plan_id)?)?;
            let link = seal::verify(path, file_revision.value(), &bytes, dir_sealed)?;
            let plan: Plan = serde_json::from_slice(&bytes)?;
            if &plan.id != plan_id || plan.revision != file_revision {
                return Err(Self::invalid_history("Snapshot identity/revision mismatch"));
            }
            (Some(plan), link)
        } else {
            (None, None)
        };
        let wal = Self::read_wal(root, plan_id, 0, RevisionId::new(0))?;
        if let Some(plan) = &snapshot {
            if !wal.events.is_empty() && !wal.committed_revisions.contains(&plan.revision) {
                return Err(Self::invalid_history(
                    "Snapshot is not a committed WAL boundary",
                ));
            }
        }
        let mut plan = snapshot
            .or(wal.bootstrap.clone())
            .ok_or_else(|| Self::invalid_history("WAL has no bootstrap or snapshot"))?;
        for event in &wal.events {
            if event.revision > plan.revision {
                apply_mutation(&mut plan, &event.action, &event.actor, event.applied_at);
                plan.updated_at = event.applied_at;
                plan.revision = event.revision;
            }
        }
        let next_revision = Self::next_revision(plan.revision)?;
        Ok(LoadedPlan {
            plan,
            history: wal.events,
            bootstrap: wal.bootstrap,
            receipts: wal.receipts,
            next_revision,
            seal: seal_link,
            wal_offset: wal.valid_len,
        })
    }

    /// A seal staged/published before its snapshot is only an orphan. It
    /// must not retroactively turn an older legacy snapshot into corruption.
    /// Any seal paired with a real snapshot still enables strict verification.
    fn has_sealed_snapshot(dir: &Path) -> PlanResult<bool> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "seal")
                && entry.path().with_extension("json").is_file()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn require_complete_history(slot: &Slot) -> PlanResult<()> {
        if slot.history.last().map(|event| event.revision) != Some(slot.plan.revision) {
            return Err(Self::invalid_history(
                "Snapshot without complete audit history is read-only; restore history before mutation",
            ));
        }
        Ok(())
    }

    fn invalid_history(message: &'static str) -> PlanError {
        std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
    }

    fn next_revision(revision: RevisionId) -> PlanResult<RevisionId> {
        revision
            .value()
            .checked_add(1)
            .map(RevisionId::new)
            .ok_or_else(|| Self::invalid_history("Plan revision exhausted"))
    }

    /// Must be called with the root lock. Repair only an unterminated EOF,
    /// never a malformed complete transaction; synchronize recovery before use.
    fn read_wal(
        root: &Path,
        id: &PlanId,
        offset: u64,
        base: RevisionId,
    ) -> PlanResult<crate::file_wal::WalRead> {
        let path = Self::history_path(root, id)?;
        let mut file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && offset == 0 => {
                return crate::file_wal::read(std::io::Cursor::new(Vec::<u8>::new()), id, base);
            }
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() < offset {
            return Err(Self::invalid_history(
                "Previously committed WAL was truncated",
            ));
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut wal = crate::file_wal::read(BufReader::new(&file), id, base)?;
        wal.valid_len += offset;
        if wal.torn_tail {
            file.set_len(wal.valid_len)?;
        }
        // Also resolves an earlier process's indeterminate write on reopen.
        if offset == 0 || wal.torn_tail || wal.valid_len > offset {
            file.sync_all()?;
            Self::sync_parent_directory(&path)?;
        }
        Ok(wal)
    }

    fn refresh(inner: &mut Inner) -> PlanResult<()> {
        let index = Self::read_index(&inner.root)?;
        // Resolve another writer's indeterminate catalog rename before the
        // freshly read selection/approvals become visible in this instance.
        std::fs::File::open(&inner.root)?.sync_all()?;
        let plans_root = inner.root.join("plans");
        if !plans_root.exists() {
            return if inner.plans.is_empty() {
                Ok(())
            } else {
                Err(Self::invalid_history(
                    "Previously loaded plan directory disappeared",
                ))
            };
        }
        let candidates = Self::candidates(&inner.root)?;
        for (id, revision, path) in &candidates {
            if let Some(slot) = inner.plans.iter_mut().find(|slot| &slot.plan.id == id) {
                let base = slot
                    .history
                    .last()
                    .map(|event| event.revision)
                    .unwrap_or(RevisionId::new(0));
                let wal = Self::read_wal(&inner.root, id, slot.wal_offset, base)?;
                for event in &wal.events {
                    if event.revision > slot.plan.revision {
                        apply_mutation(
                            &mut slot.plan,
                            &event.action,
                            &event.actor,
                            event.applied_at,
                        );
                        slot.plan.updated_at = event.applied_at;
                        slot.plan.revision = event.revision;
                    }
                }
                slot.next_revision = Self::next_revision(slot.plan.revision)?;
                slot.history.extend(wal.events);
                slot.receipts.extend(wal.receipts);
                slot.wal_offset = wal.valid_len;
            } else {
                let loaded = match Self::load_plan(&inner.root, id, *revision, path.as_deref()) {
                    Ok(loaded) => loaded,
                    Err(error)
                        if index.as_ref().and_then(PlanIndex::active_id).as_ref() != Some(id)
                            && index.is_some() =>
                    {
                        warn!(%error, plan_id = %id, "Unreadable inactive plan remains unavailable");
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                inner.plans.push(Slot {
                    plan: loaded.plan,
                    history: loaded.history,
                    bootstrap: loaded.bootstrap,
                    receipts: loaded.receipts,
                    next_revision: loaded.next_revision,
                    seal: loaded.seal,
                    meta: PlanMeta::default(),
                    wal_offset: loaded.wal_offset,
                });
            }
        }
        inner
            .plans
            .retain(|slot| candidates.iter().any(|(id, _, _)| id == &slot.plan.id));
        for slot in &mut inner.plans {
            slot.meta = index
                .as_ref()
                .map(|index| index.meta(&slot.plan.id))
                .unwrap_or_default();
        }
        inner.active = match index {
            Some(index) => index
                .active_id()
                .filter(|id| inner.plans.iter().any(|slot| &slot.plan.id == id)),
            None => candidates
                .iter()
                .max_by(|(a, ar, _), (b, br, _)| {
                    ar.cmp(br).then_with(|| a.as_str().cmp(b.as_str()))
                })
                .map(|(id, _, _)| id.clone()),
        };
        inner.plans.sort_by(|a, b| {
            a.plan
                .created_at
                .cmp(&b.plan.created_at)
                .then_with(|| a.plan.id.as_str().cmp(b.plan.id.as_str()))
        });
        Ok(())
    }

    /// `true`, wenn `dir` mindestens einen Snapshot `rev-<n>.json` enthält.
    ///
    /// Ein Verzeichnis ohne Snapshot (etwa Reste eines fehlgeschlagenen
    /// ersten `Create`) blockiert ein neues `Create` derselben ID nicht.
    fn dir_holds_snapshot(dir: &Path) -> PlanResult<bool> {
        if !dir.is_dir() {
            return Ok(false);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let is_snapshot = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("rev-"))
                .and_then(|rest| rest.strip_suffix(".json"))
                .is_some_and(|number| number.parse::<u64>().is_ok());
            if is_snapshot {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Pfad des Katalog-Index.
    fn index_path(root: &Path) -> PathBuf {
        root.join(PLAN_INDEX_FILE)
    }

    /// Liest `plan-index.json`; `None`, wenn es ihn (noch) nicht gibt.
    ///
    /// # Errors
    /// [`PlanError::Io`] / [`PlanError::Serde`] bei unlesbarem Index.
    fn read_index(root: &Path) -> PlanResult<Option<PlanIndex>> {
        let path = Self::index_path(root);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Schreibt `plan-index.json` atomar (Tmp-Datei + Rename + Verzeichnis-Sync).
    ///
    /// # Errors
    /// [`PlanError::Io`] / [`PlanError::Serde`] bei Schreibfehler.
    fn write_index(root: &Path, index: &PlanIndex) -> PlanResult<()> {
        let bytes = serde_json::to_vec_pretty(index)?;
        Self::stage_atomic_write(&Self::index_path(root), &bytes)?.commit()
    }

    /// Gibt das Plan-Verzeichnis zurück — erst nach Grammatikprüfung der ID.
    ///
    /// # Errors
    /// - [`PlanError::InvalidId`] wenn `plan_id` kein gültiges Pfadsegment ist.
    fn plan_dir(root: &Path, plan_id: &PlanId) -> PlanResult<PathBuf> {
        let checked = PlanId::parse(plan_id.as_str())?;
        Ok(root.join("plans").join(checked.as_str()))
    }

    /// Gibt den Planspeicherpfad für eine gegebene Revision zurück.
    fn plan_path(root: &Path, plan_id: &PlanId, revision: RevisionId) -> PlanResult<PathBuf> {
        Ok(Self::plan_dir(root, plan_id)?.join(format!("rev-{}.json", revision.value())))
    }

    /// Gibt den History-Pfad zurück.
    fn history_path(root: &Path, plan_id: &PlanId) -> PlanResult<PathBuf> {
        Ok(Self::plan_dir(root, plan_id)?.join("history.jsonl"))
    }

    /// Bereitet `content` als synchronisierte Temp-Datei vor, ohne den
    /// Zielpfad schon sichtbar zu verändern.
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Schreibfehler.
    fn stage_atomic_write(path: &Path, content: &[u8]) -> PlanResult<StagedWrite> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Tmp-Datei im gleichen Verzeichnis (rename atomar nur innerhalb desselben FS).
        // create_new verhindert, dass ein stale Tempfile eines anderen Prozesses
        // stillschweigend überschrieben wird.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let tmp_path = path.with_extension(format!("tmp-{}-{}", std::process::id(), nonce));
        let write_result = (|| -> PlanResult<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            f.write_all(content)?;
            f.flush()?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            // Best effort: der Schreibfehler ist die relevante Meldung.
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error);
        }
        Ok(StagedWrite {
            target: path.to_path_buf(),
            temporary: tmp_path,
        })
    }

    /// Synchronisiert das Verzeichnis eines Dateieintrags, damit ein zuvor
    /// synchronisiertes Rename bzw. Create nach einem Systemabsturz erhalten
    /// bleibt, soweit die Plattform Directory-Fsync unterstützt.
    fn sync_parent_directory(path: &Path) -> PlanResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    /// Appends one bounded transaction record after the publication check.
    ///
    /// # Errors
    /// An ordinary error means no committed append; an uncertain rollback
    /// returns `CommitIndeterminate`, requiring locked reopen and inspection.
    fn append_events_checked(
        root: &Path,
        candidate: &Plan,
        events: &[PlanEvent],
        receipt: Option<&crate::parent_progress::ParentProgressReceipt>,
        before_publish: &dyn Fn() -> PlanResult<()>,
    ) -> PlanResult<u64> {
        let path = Self::history_path(root, &candidate.id)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            // Publish both newly created directory links before acknowledging
            // the first WAL. fsync(file) alone does not make these durable.
            Self::sync_parent_directory(parent)?;
            if let Some(grandparent) = parent.parent() {
                Self::sync_parent_directory(grandparent)?;
            }
        }
        let buffer = match receipt {
            Some(receipt) => crate::file_wal::encode_progress(candidate, events, receipt)?,
            None => crate::file_wal::encode(candidate, events)?,
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let original_len = file.metadata()?.len();
        // Job lease is already held by the caller. No job lock is acquired
        // here. This is the final check before the authoritative WAL append.
        before_publish()?;
        append_transaction(
            &mut FileWalSink {
                file: &mut file,
                path: &path,
            },
            &buffer,
            original_len,
            &path,
        )?;
        Ok(original_len + buffer.len() as u64)
    }

    /// Persistiert Events und schreibt nur bei einem Checkpoint Snapshot plus
    /// Siegel. Der erste Zustand ist immer ein Checkpoint; danach gilt ein
    /// Intervall von zehn Revisionen oder ein terminaler Übergang.
    fn checkpoint_required(candidate: &Plan, events: &[PlanEvent]) -> bool {
        // `u64::is_multiple_of` was stabilized in Rust 1.87; this workspace's
        // `rust-version` is 1.85 (see root Cargo.toml), so the manual `% == 0`
        // form is kept intentionally instead of upgrading the MSRV.
        #[allow(clippy::manual_is_multiple_of)]
        let is_checkpoint_interval = candidate.revision.value() % CHECKPOINT_INTERVAL == 0;
        candidate.revision.value() == 1
            || is_checkpoint_interval
            || events.iter().any(|event| {
                matches!(
                    &event.action,
                    PlanAction::SetStatus {
                        status: PlanNodeStatus::Completed
                            | PlanNodeStatus::Superseded
                            | PlanNodeStatus::Invalidated,
                        ..
                    } | PlanAction::Invalidate { .. }
                        | PlanAction::Supersede { .. }
                )
            })
    }

    /// Entfernt nach einem erfolgreich veröffentlichten Checkpoint alle älteren
    /// Snapshots und Siegel. `history.jsonl` bleibt unverändert die vollständige
    /// Audit-Historie. Der Checkpoint beginnt bewusst eine neue Siegelkette;
    /// dadurch ist seine Integrität ohne bereits gelöschte Vorgänger prüfbar.
    fn compact_checkpoints(root: &Path, plan_id: &PlanId, keep: RevisionId) -> PlanResult<()> {
        let dir = Self::plan_dir(root, plan_id)?;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let revision = name
                .strip_prefix("rev-")
                .and_then(|value| {
                    value
                        .strip_suffix(".json")
                        .or_else(|| value.strip_suffix(".seal"))
                })
                .and_then(|value| value.parse::<u64>().ok());
            if revision.is_some_and(|revision| revision != keep.value()) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Self::sync_parent_directory(&dir.join("history.jsonl"))
    }

    fn publish(
        &self,
        root: &Path,
        candidate: &Plan,
        events: &[PlanEvent],
    ) -> PlanResult<Published> {
        self.publish_checked(root, candidate, events, &|| Ok(()))
    }

    fn publish_checked(
        &self,
        root: &Path,
        candidate: &Plan,
        events: &[PlanEvent],
        before_publish: &dyn Fn() -> PlanResult<()>,
    ) -> PlanResult<Published> {
        self.publish_progress_checked(root, candidate, events, None, before_publish)
    }
    fn publish_progress_checked(
        &self,
        root: &Path,
        candidate: &Plan,
        events: &[PlanEvent],
        receipt: Option<&crate::parent_progress::ParentProgressReceipt>,
        before_publish: &dyn Fn() -> PlanResult<()>,
    ) -> PlanResult<Published> {
        if !Self::checkpoint_required(candidate, events) {
            let wal_offset = self.record_outcome(Self::append_events_checked(
                root,
                candidate,
                events,
                receipt,
                before_publish,
            ))?;
            return Ok(Published {
                seal: None,
                wal_offset,
            });
        }
        let bytes = serde_json::to_vec_pretty(candidate)?;
        let plan_path = Self::plan_path(root, &candidate.id, candidate.revision)?;
        let (seal_bytes, link) = seal::build(None, candidate.revision.value(), &bytes)?;
        let staged_snapshot = Self::stage_atomic_write(&plan_path, &bytes)?;
        let staged_seal = match seal::stage(&seal::seal_path(&plan_path), &seal_bytes) {
            Ok(staged) => staged,
            Err(error) => {
                staged_snapshot.discard();
                return Err(error);
            }
        };
        let wal_offset = match self.record_outcome(Self::append_events_checked(
            root,
            candidate,
            events,
            receipt,
            before_publish,
        )) {
            Ok(offset) => offset,
            Err(error) => {
                staged_seal.discard();
                staged_snapshot.discard();
                return Err(error);
            }
        };
        // WAL is committed. A later checkpoint failure must not report the
        // mutation as rejected or leave RAM behind durable history.
        if let Err(error) = staged_seal.commit() {
            staged_snapshot.discard();
            warn!(%error, "Committed plan WAL; checkpoint seal maintenance failed");
            return Ok(Published {
                seal: None,
                wal_offset,
            });
        }
        if let Err(error) = staged_snapshot.commit() {
            warn!(%error, "Committed plan WAL; checkpoint snapshot maintenance failed");
            return Ok(Published {
                seal: None,
                wal_offset,
            });
        }
        if let Err(error) = Self::compact_checkpoints(root, &candidate.id, candidate.revision) {
            warn!(%error, "Committed plan WAL; checkpoint compaction failed");
        }
        Ok(Published {
            seal: Some(link),
            wal_offset,
        })
    }
}

impl FilePlanStore {
    fn create_root_durably(root: &Path) -> PlanResult<()> {
        let mut missing = Vec::new();
        for path in root.ancestors() {
            if path.try_exists()? {
                break;
            }
            missing.push(path.to_path_buf());
        }
        std::fs::create_dir_all(root)?;
        for path in missing.iter().rev() {
            std::fs::File::open(path)?.sync_all()?;
            Self::sync_parent_directory(path)?;
        }
        Ok(())
    }

    fn lock_root(root: &Path) -> PlanResult<RootLock> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".plan-store.lock"))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(RootLock { file }),
            Err(fs4::TryLockError::WouldBlock) => Err(PlanError::StoreContended),
            Err(fs4::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn read_inner(&self) -> PlanResult<LockedInner<'_>> {
        self.write_inner()
    }

    fn write_inner(&self) -> PlanResult<LockedInner<'_>> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock poisoned")))?;
        if self.indeterminate.load(Ordering::Acquire) {
            return Err(PlanError::StoreRecoveryRequired);
        }
        let file = Self::lock_root(&inner.root)?;
        Self::refresh(&mut inner)?;
        if let Some(slot) = inner.active_slot() {
            self.config
                .validate_plan(&slot.plan)
                .map_err(PlanError::Config)?;
        }
        Ok(LockedInner { inner, _file: file })
    }

    fn record_outcome<T>(&self, outcome: PlanResult<T>) -> PlanResult<T> {
        if matches!(&outcome, Err(PlanError::CommitIndeterminate { .. })) {
            self.indeterminate.store(true, Ordering::Release);
        }
        outcome
    }

    /// Wendet eine Katalogänderung an: RAM-Zustand ändern, neuen Index
    /// dauerhaft schreiben; scheitert das Schreiben, werden Katalogdaten und
    /// aktiver Plan auf den vorherigen Stand zurückgesetzt.
    fn commit_catalog(&self, inner: &mut Inner, change: impl FnOnce(&mut Inner)) -> PlanResult<()> {
        let previous_meta: Vec<PlanMeta> = inner.plans.iter().map(|slot| slot.meta).collect();
        let previous_active = inner.active.clone();
        change(inner);
        let index = inner.index();
        if let Err(error) = self.record_outcome(Self::write_index(&inner.root, &index)) {
            for (slot, meta) in inner.plans.iter_mut().zip(previous_meta) {
                slot.meta = meta;
            }
            inner.active = previous_active;
            return Err(error);
        }
        Ok(())
    }
}

/// Mandanten-fähiger Kern von `apply` (H12).
///
/// `tenant` wirkt nur auf `Create` (landet in [`Plan::tenant`]); jede
/// andere Aktion ignoriert ihn — der Mandant eines Plans ist nach dem
/// Anlegen unveränderlich.
impl FilePlanStore {
    fn apply_with_tenant(
        &self,
        action: PlanAction,
        tenant: Option<harw_types::TenantId>,
        actor: &str,
    ) -> PlanResult<PlanEvent> {
        let mut inner = self.write_inner()?;

        let now = OffsetDateTime::now_utc();

        // `Create` legt einen **weiteren** Plan an und macht ihn aktiv
        // (Runde 5, Teil P). Eine vergebene ID — auch die eines beim Start
        // geladenen oder archivierten Plans — wird abgelehnt: `Create`
        // überschreibt nie.
        if let PlanAction::Create { plan_id, goal } = &action {
            self.config
                .validate_action(&action, 0)
                .map_err(PlanError::Config)?;
            // Grammatik vor jedem Pfadzugriff (F-013/G-032).
            let plan_id = PlanId::parse(plan_id.as_str())?;
            if inner.position_of(&plan_id).is_ok() {
                return Err(PlanError::PlanExists { id: plan_id });
            }
            // Auch ein nicht ladbares Verzeichnis derselben ID (etwa ein
            // übersprungener, beschädigter Plan) wird nicht überschrieben.
            if Self::dir_holds_snapshot(&Self::plan_dir(&inner.root, &plan_id)?)? {
                return Err(PlanError::PlanExists { id: plan_id });
            }

            info!(plan_id = %plan_id, "Persistenten Plan erstellen");
            let revision = RevisionId::new(1);
            let plan = Plan {
                id: plan_id.clone(),
                revision,
                parent_revision: None,
                goal_statement: goal.clone(),
                goal_id: None,
                nodes: Vec::new(),
                created_at: now,
                updated_at: now,
                tenant,
            };
            let event = PlanEvent {
                revision,
                action,
                actor: actor.to_owned(),
                applied_at: now,
            };
            let published = self.publish(&inner.root, &plan, std::slice::from_ref(&event))?;
            inner.plans.push(Slot {
                plan: plan.clone(),
                history: vec![event.clone()],
                bootstrap: Some(plan.clone()),
                receipts: Vec::new(),
                next_revision: revision.next(),
                seal: published.seal,
                meta: PlanMeta::default(),
                wal_offset: published.wal_offset,
            });
            inner.active = Some(plan_id.clone());
            // Plan creation and catalog activation are separate durable
            // records. If activation fails, do not report full Create success
            // or let the next implicit apply silently target the old plan.
            let index = inner.index();
            if let Err(error) = Self::write_index(&inner.root, &index) {
                warn!(%error, %plan_id, "Plan committed but catalog activation failed");
                return self.record_outcome(Err(PlanError::CommitIndeterminate {
                    path: Self::index_path(&inner.root).display().to_string(),
                }));
            }
            return Ok(event);
        }

        let Some(position) = inner.active_position() else {
            return Err(PlanError::PlanNotFound);
        };
        let root = inner.root.clone();
        let Some(slot) = inner.plans.get_mut(position) else {
            return Err(PlanError::PlanNotFound);
        };
        Self::require_complete_history(slot)?;
        let revision = slot.next_revision;
        Self::next_revision(revision)?;
        // Validierung + Mutation auf einem Kandidaten — einzige Mutationsstelle,
        // geteilt mit `InMemoryPlanStore`. Sichtbar erst nach dem dauerhaften
        // History-Append.
        let (candidate, events) =
            stage_actions(&slot.plan, vec![action], actor, &self.config, revision, now)
                .map_err(|(_, error)| error)?;
        let published = self.publish(&root, &candidate, &events)?;
        let Some(event) = events.into_iter().next() else {
            return Err(PlanError::PlanNotFound);
        };
        debug!(revision = %revision, actor = actor, "Persistente Aktion angewendet");
        slot.next_revision = revision.next();
        slot.plan = candidate;
        if let Some(link) = published.seal {
            slot.seal = Some(link);
        }
        slot.wal_offset = published.wal_offset;
        slot.history.push(event.clone());
        Ok(event)
    }
}

impl PlanStore for FilePlanStore {
    fn reconcile_parent_progress(
        &self,
        admission: &crate::parent_progress::ParentAdmission,
    ) -> PlanResult<crate::parent_progress::ParentProgressView> {
        let inner = self.read_inner()?;
        let slot = inner.active_slot().ok_or(PlanError::PlanNotFound)?;
        crate::parent_progress::reconcile(
            slot.bootstrap.as_ref(),
            &slot.history,
            &slot.receipts,
            &slot.plan,
            admission,
            &self.config,
        )
    }
    fn commit_parent_progress_checked(
        &self,
        request: &crate::parent_progress::CollectSourcesProgress,
        before_publish: &dyn Fn() -> PlanResult<()>,
    ) -> PlanResult<crate::parent_progress::ParentProgressReceipt> {
        let mut inner = self.write_inner()?;
        self.config.require_enabled().map_err(PlanError::Config)?;
        let root = inner.root.clone();
        let position = inner.active_position().ok_or(PlanError::PlanNotFound)?;
        let slot = inner
            .plans
            .get_mut(position)
            .ok_or(PlanError::PlanNotFound)?;
        Self::require_complete_history(slot)?;
        let view = crate::parent_progress::reconcile(
            slot.bootstrap.as_ref(),
            &slot.history,
            &slot.receipts,
            &slot.plan,
            &request.admission,
            &self.config,
        )?;
        match crate::parent_progress::stage(&view, &slot.receipts, request, &self.config)? {
            crate::parent_progress::StagedProgress::Existing(receipt) => Ok(receipt),
            crate::parent_progress::StagedProgress::New(plan, events, receipt) => {
                let published = self.publish_progress_checked(
                    &root,
                    &plan,
                    &events,
                    Some(&receipt),
                    before_publish,
                )?;
                slot.next_revision = plan.revision.next();
                slot.plan = plan;
                if let Some(link) = published.seal {
                    slot.seal = Some(link);
                }
                slot.wal_offset = published.wal_offset;
                slot.history.extend(events);
                slot.receipts.push(receipt.clone());
                Ok(receipt)
            }
        }
    }
    fn current(&self) -> PlanResult<Plan> {
        let inner = self.read_inner()?;
        inner
            .active_slot()
            .map(|slot| slot.plan.clone())
            .ok_or(PlanError::PlanNotFound)
    }

    // Infallible compatibility API: cached hint only. Mutations use the
    // refreshed, fallible CAS path, never this value as authority.
    fn revision(&self) -> RevisionId {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match inner.active_slot() {
            Some(slot) => slot.plan.revision,
            None => RevisionId::new(0),
        }
    }

    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        self.apply_with_tenant(action, None, actor)
    }

    fn create_for_tenant(
        &self,
        plan_id: PlanId,
        goal: String,
        tenant: Option<harw_types::TenantId>,
        actor: &str,
    ) -> PlanResult<PlanEvent> {
        self.apply_with_tenant(PlanAction::Create { plan_id, goal }, tenant, actor)
    }

    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision> {
        self.apply_batch_checked(plan, actions, actor, expected_rev, &|| Ok(()))
    }

    fn apply_batch_checked(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
        before_publish: &dyn Fn() -> PlanResult<()>,
    ) -> PlanResult<PlanRevision> {
        let mut inner = self.write_inner()?;
        self.config.require_enabled().map_err(PlanError::Config)?;

        let Some(position) = inner.active_position() else {
            return Err(PlanError::PlanNotFound);
        };
        let root = inner.root.clone();
        let Some(slot) = inner.plans.get_mut(position) else {
            return Err(PlanError::PlanNotFound);
        };
        let current = check_batch_target(Some(&slot.plan), plan, expected_rev)?;
        if actions.is_empty() {
            before_publish()?;
            return Ok(PlanRevision {
                revision: current.revision,
                events: Vec::new(),
            });
        }

        Self::require_complete_history(slot)?;
        let count = u64::try_from(actions.len())
            .map_err(|_| Self::invalid_history("Batch revision overflow"))?;
        current
            .revision
            .value()
            .checked_add(count)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| Self::invalid_history("Batch revision overflow"))?;
        let now = OffsetDateTime::now_utc();
        let (candidate, events) = stage_actions(
            current,
            actions,
            actor,
            &self.config,
            slot.next_revision,
            now,
        )
        .map_err(|(index, source)| PlanError::BatchActionRejected {
            index,
            source: Box::new(source),
        })?;

        let published = self.publish_checked(&root, &candidate, &events, before_publish)?;
        let revision = candidate.revision;
        slot.next_revision = revision.next();
        slot.plan = candidate;
        if let Some(link) = published.seal {
            slot.seal = Some(link);
        }
        slot.wal_offset = published.wal_offset;
        slot.history.extend(events.iter().cloned());
        info!(
            plan_id = %plan,
            revision = %revision,
            count = events.len(),
            actor = actor,
            "Persistenter Batch atomar angewendet"
        );
        Ok(PlanRevision { revision, events })
    }

    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>> {
        let inner = self.read_inner()?;

        // Ohne aktiven Plan gibt es keine History.
        let Some(slot) = inner.active_slot() else {
            return Ok(Vec::new());
        };

        Ok(slot
            .history
            .iter()
            .filter(|event| since.is_none_or(|revision| event.revision >= revision))
            .cloned()
            .collect())
    }

    fn list_plans(&self) -> PlanResult<Vec<PlanSummary>> {
        let inner = self.read_inner()?;
        Ok(inner
            .plans
            .iter()
            .map(|slot| {
                let active = inner.active.as_ref() == Some(&slot.plan.id);
                PlanSummary::from_plan(&slot.plan, active, slot.meta)
            })
            .collect())
    }

    fn plan_by_id(&self, id: &PlanId) -> PlanResult<Plan> {
        let inner = self.read_inner()?;
        let position = inner.position_of(id)?;
        inner
            .plans
            .get(position)
            .map(|slot| slot.plan.clone())
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    fn plan_meta(&self, id: &PlanId) -> PlanResult<PlanMeta> {
        let inner = self.read_inner()?;
        let position = inner.position_of(id)?;
        inner
            .plans
            .get(position)
            .map(|slot| slot.meta)
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    fn switch_plan(&self, id: &PlanId, actor: &str) -> PlanResult<Plan> {
        let mut inner = self.write_inner()?;
        let position = inner.position_of(id)?;
        if let Some(slot) = inner.plans.get(position) {
            self.config
                .validate_plan(&slot.plan)
                .map_err(PlanError::Config)?;
        }
        let target = id.clone();
        self.commit_catalog(&mut inner, |inner| {
            if let Some(slot) = inner.plans.get_mut(position) {
                slot.meta.archived = false;
            }
            inner.active = Some(target);
        })?;
        info!(plan_id = %id, actor = actor, "Aktiven Plan gewechselt");
        inner
            .plans
            .get(position)
            .map(|slot| slot.plan.clone())
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    fn archive_plan(&self, id: &PlanId, actor: &str) -> PlanResult<()> {
        let mut inner = self.write_inner()?;
        let position = inner.position_of(id)?;
        let target = id.clone();
        self.commit_catalog(&mut inner, |inner| {
            if let Some(slot) = inner.plans.get_mut(position) {
                slot.meta.archived = true;
            }
            if inner.active.as_ref() == Some(&target) {
                inner.active = None;
            }
        })?;
        info!(plan_id = %id, actor = actor, "Plan archiviert");
        Ok(())
    }

    fn set_approval(&self, id: &PlanId, approval: PlanApproval, actor: &str) -> PlanResult<()> {
        let mut inner = self.write_inner()?;
        let position = inner.position_of(id)?;
        self.commit_catalog(&mut inner, |inner| {
            if let Some(slot) = inner.plans.get_mut(position) {
                slot.meta.approval = approval;
            }
        })?;
        info!(plan_id = %id, actor = actor, approval = approval.label(), "Freigabestand gesetzt");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PlanAction;
    use crate::config::{PlanToolConfig, PlanToolConfigError};
    use crate::ids::{PathOrSymbol, PlanId, TaskId};
    use crate::store::PlanStore;
    use crate::test_support::{TestError, TestResult};
    use crate::types::{PlanNode, PlanNodeKind, PlanNodeStatus};
    use tempfile::TempDir;
    use time::OffsetDateTime;

    fn make_store(dir: &TempDir) -> TestResult<FilePlanStore> {
        Ok(FilePlanStore::new(dir.path())?)
    }

    fn config_with_max_nodes(max_nodes: usize) -> PlanToolConfig {
        PlanToolConfig {
            max_nodes,
            ..PlanToolConfig::enabled_defaults()
        }
    }

    fn make_node(id: &str) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "obj".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{}.rs", id))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status: PlanNodeStatus::Draft,
            evidence: vec![],
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_persist_and_reload_roundtrip() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;

        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-1"),
                goal: "Persistenz-Ziel".to_owned(),
            },
            "orchestrator",
        )?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "a",
        )?;

        let plan = store.current()?;
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.goal_statement, "Persistenz-Ziel");

        // Nur der initiale Stand ist ein Checkpoint; der zweite Zustand wird
        // beim Neustart aus history.jsonl rekonstruiert.
        let checkpoint = dir.path().join("plans").join("p-1").join("rev-1.json");
        assert!(checkpoint.exists(), "initialer Checkpoint fehlt");
        assert!(
            !dir.path()
                .join("plans")
                .join("p-1")
                .join("rev-2.json")
                .exists()
        );
        drop(store);
        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.current()?.id, plan.id);
        assert_eq!(reloaded.current()?.nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn test_history_append_only() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;

        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-2"),
                goal: "History".to_owned(),
            },
            "o",
        )?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "a",
        )?;
        store.apply(
            PlanAction::SetStatus {
                id: TaskId::new("t1"),
                status: PlanNodeStatus::Ready,
                reason: None,
            },
            "a",
        )?;

        let hist = store.history(None)?;
        assert!(hist.len() >= 3, "History muss mindestens 3 Einträge haben");

        // History-Datei muss existieren und Zeilen enthalten
        let hist_path = dir.path().join("plans").join("p-2").join("history.jsonl");
        assert!(hist_path.exists(), "history.jsonl fehlt");
        let content = std::fs::read_to_string(&hist_path)?;
        let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(
            line_count >= 3,
            "history.jsonl hat zu wenig Zeilen: {}",
            line_count
        );
        Ok(())
    }

    #[test]
    fn test_staged_write_via_tmp_rename() -> TestResult {
        let dir = TempDir::new()?;
        let target = dir.path().join("test.json");

        FilePlanStore::stage_atomic_write(&target, b"{\"ok\":true}")?.commit()?;
        assert!(target.exists(), "Zieldatei fehlt nach staged write");

        // Keine staging-Datei darf nach dem Commit zurückbleiben.
        let leftovers = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0, "Tmp-Datei wurde nicht entfernt");

        // Inhalt korrekt
        let content = std::fs::read(&target)?;
        assert_eq!(content, b"{\"ok\":true}");
        Ok(())
    }

    #[test]
    fn test_restart_loads_plan_history_and_next_revision() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            store.apply(
                PlanAction::Create {
                    plan_id: PlanId::new("restart"),
                    goal: "restart-safe".to_owned(),
                },
                "orchestrator",
            )?;
            store.apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "worker",
            )?;
        }

        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.current()?.nodes.len(), 1);
        assert_eq!(reloaded.history(None)?.len(), 2);
        assert_eq!(
            reloaded.apply(PlanAction::Inspect, "worker")?.revision,
            RevisionId::new(3)
        );
        Ok(())
    }

    #[test]
    fn test_second_create_is_rejected_with_plan_exists() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-erst"),
                goal: "erster Plan".to_owned(),
            },
            "orchestrator",
        )?;

        // Runde 5, Teil P: nur dieselbe ID wird abgelehnt.
        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-erst"),
                goal: "darf nicht überschreiben".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-erst")),
            "ein zweites Create mit derselben ID muss abgelehnt werden, Ergebnis: {result:?}"
        );
        assert_eq!(store.current()?.id, PlanId::new("p-erst"));
        assert_eq!(store.current()?.goal_statement, "erster Plan");
        assert_eq!(
            store.history(None)?.len(),
            1,
            "kein Event für das abgelehnte Create"
        );
        Ok(())
    }

    #[test]
    fn test_create_after_reload_is_rejected_with_plan_exists() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            store.apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-durable"),
                    goal: "überlebt den Neustart".to_owned(),
                },
                "orchestrator",
            )?;
        }

        // Nach dem Neustart ist der Plan geladen — ein Create würde ihn sonst
        // still überschreiben.
        let reloaded = make_store(&dir)?;
        let result = reloaded.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-durable"),
                goal: "anderes Ziel".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-durable")),
            "Create nach Reload muss abgelehnt werden, Ergebnis: {result:?}"
        );
        assert_eq!(reloaded.current()?.goal_statement, "überlebt den Neustart");
        assert_eq!(reloaded.history(None)?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_failed_history_write_does_not_commit_memory_or_revision_file() -> TestResult {
        let dir = TempDir::new()?;
        let history_path = dir
            .path()
            .join("plans")
            .join("p-rollback")
            .join("history.jsonl");
        std::fs::create_dir_all(
            history_path
                .parent()
                .ok_or(TestError::Missing("history_path hat Elternverzeichnis"))?,
        )?;
        std::fs::create_dir(&history_path)?;
        let store = make_store(&dir)?;

        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-rollback"),
                goal: "must not commit".to_owned(),
            },
            "orchestrator",
        );
        assert!(result.is_err());
        assert_eq!(store.revision(), RevisionId::new(0));
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        assert!(
            !dir.path()
                .join("plans")
                .join("p-rollback")
                .join("rev-1.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn test_failed_history_append_does_not_publish_update_snapshot() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("update-rollback"),
                goal: "must keep revision one".to_owned(),
            },
            "orchestrator",
        )?;

        let history_path = dir
            .path()
            .join("plans")
            .join("update-rollback")
            .join("history.jsonl");
        std::fs::remove_file(&history_path)?;
        std::fs::create_dir(&history_path)?;

        let result = store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "worker",
        );

        assert!(result.is_err());
        assert_eq!(store.revision(), RevisionId::new(1));
        // Fallible reads no longer hide a broken authoritative WAL behind
        // a stale RAM snapshot.
        assert!(store.current().is_err());
        assert!(
            !dir.path()
                .join("plans")
                .join("update-rollback")
                .join("rev-2.json")
                .exists(),
            "ein fehlgeschlagener History-Append darf keinen Update-Snapshot veröffentlichen"
        );
        Ok(())
    }

    #[test]
    fn test_with_config_rejects_disabled_tool_before_creating_store_root() -> TestResult {
        let dir = TempDir::new()?;
        let root = dir.path().join("disabled-store");

        let result = FilePlanStore::with_config(&root, PlanToolConfig::default());

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::Disabled))
            ),
            "deaktiviertes Tool muss einen typisierten Konfigurationsfehler liefern"
        );
        assert!(
            !root.exists(),
            "deaktivierte Konfiguration darf kein Wurzelverzeichnis anlegen"
        );
        Ok(())
    }

    #[test]
    fn test_configured_store_rejects_node_limit_before_durable_write() -> TestResult {
        let dir = TempDir::new()?;
        let store = FilePlanStore::with_config(dir.path(), config_with_max_nodes(1))?;
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("node-limit"),
                goal: "enforce node limit".to_owned(),
            },
            "orchestrator",
        )?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "worker",
        )?;

        let result = store.apply(
            PlanAction::AddNode {
                node: make_node("t2"),
            },
            "worker",
        );

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::NodeLimitExceeded {
                    max_nodes: 1,
                    attempted_nodes: 2,
                }))
            ),
            "Knotenlimit muss AddNode mit einem typisierten Konfigurationsfehler ablehnen"
        );
        assert_eq!(store.current()?.nodes.len(), 1);
        assert_eq!(store.revision(), RevisionId::new(2));
        assert!(
            !dir.path()
                .join("plans")
                .join("node-limit")
                .join("rev-3.json")
                .exists(),
            "abgelehnte Aktion darf keinen Snapshot schreiben"
        );
        assert_eq!(store.history(None)?.len(), 2);
        Ok(())
    }

    // ── Pfad-Traversal (F-013, G-032) ──────────────────────────────────────

    #[test]
    fn test_create_with_traversal_id_writes_nothing_outside_store() -> TestResult {
        let dir = TempDir::new()?;
        let root = dir.path().join("store");
        let store = FilePlanStore::new(&root)?;

        for raw in ["../escape", "../../etc", "a/b", "/abs", ".."] {
            let result = store.apply(
                PlanAction::Create {
                    plan_id: PlanId::new(raw),
                    goal: "Ausbruch".to_owned(),
                },
                "model:x",
            );
            assert!(
                matches!(
                    result,
                    Err(PlanError::InvalidId {
                        field: "PlanId",
                        ..
                    })
                ),
                "{raw:?} war: {result:?}"
            );
        }
        assert!(!dir.path().join("escape").exists(), "kein Write außerhalb");
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        let entries = std::fs::read_dir(dir.path())?.count();
        assert_eq!(entries, 1, "nur das Store-Verzeichnis existiert");
        Ok(())
    }

    #[test]
    fn test_load_ignores_directories_with_invalid_plan_id() -> TestResult {
        let dir = TempDir::new()?;
        let bogus = dir.path().join("plans").join("Bad_Name");
        std::fs::create_dir_all(&bogus)?;
        std::fs::write(bogus.join("rev-9.json"), b"{}")?;

        let store = make_store(&dir)?;

        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        Ok(())
    }

    // ── Siegel (Integrität beim Laden) ─────────────────────────────────────

    fn seeded_store(dir: &TempDir, id: &str) -> TestResult<FilePlanStore> {
        let store = make_store(dir)?;
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new(id),
                goal: "Siegel".to_owned(),
            },
            "o",
        )?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "a",
        )?;
        Ok(store)
    }

    #[test]
    fn test_seal_is_written_and_reload_verifies() -> TestResult {
        let dir = TempDir::new()?;
        drop(seeded_store(&dir, "p-seal")?);
        let plan_dir = dir.path().join("plans").join("p-seal");
        assert!(plan_dir.join("rev-1.seal").exists());
        assert!(!plan_dir.join("rev-2.seal").exists());

        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.current()?.nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn test_manipulated_snapshot_is_rejected_on_load() -> TestResult {
        let dir = TempDir::new()?;
        drop(seeded_store(&dir, "p-tamper")?);
        let snapshot = dir.path().join("plans").join("p-tamper").join("rev-1.json");
        // Die Bytes ändern, ohne das passende Siegel neu zu berechnen.
        std::fs::write(&snapshot, b"{\"tampered\":true}")?;

        let result = FilePlanStore::new(dir.path());

        assert!(
            matches!(result, Err(PlanError::SealMismatch { .. })),
            "manipulierter Snapshot muss abgewiesen werden"
        );
        Ok(())
    }

    #[test]
    fn test_manipulated_checkpoint_seal_is_rejected_on_load() -> TestResult {
        let dir = TempDir::new()?;
        drop(seeded_store(&dir, "p-seal-tamper")?);
        let seal_path = dir
            .path()
            .join("plans")
            .join("p-seal-tamper")
            .join("rev-1.seal");
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&seal_path)?)?;
        value["chain"] = serde_json::json!("00");
        std::fs::write(&seal_path, serde_json::to_vec(&value)?)?;
        assert!(matches!(
            FilePlanStore::new(dir.path()),
            Err(PlanError::SealMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_checkpoint_without_seal_loads_as_legacy() -> TestResult {
        let dir = TempDir::new()?;
        drop(seeded_store(&dir, "p-legacy-seal")?);
        // A single retained checkpoint has no predecessor seal that could mark
        // the directory as sealed. This intentionally remains compatible with
        // pre-seal stores; the next checkpoint writes a fresh seal.
        std::fs::remove_file(
            dir.path()
                .join("plans")
                .join("p-legacy-seal")
                .join("rev-1.seal"),
        )?;
        assert!(FilePlanStore::new(dir.path()).is_ok());
        Ok(())
    }

    #[test]
    fn test_legacy_unsealed_plan_loads_and_is_sealed_on_next_write() -> TestResult {
        let dir = TempDir::new()?;
        drop(seeded_store(&dir, "p-legacy")?);
        let plan_dir = dir.path().join("plans").join("p-legacy");
        std::fs::remove_file(plan_dir.join("rev-1.seal"))?;

        let legacy = make_store(&dir)?;
        assert_eq!(legacy.current()?.nodes.len(), 1, "Legacy lädt");
        legacy.apply(
            PlanAction::AddNode {
                node: make_node("t2"),
            },
            "a",
        )?;
        // Checkpoints are periodic; the next checkpoint re-establishes sealing.
        for index in 3..=10 {
            legacy.apply(PlanAction::Inspect, "a")?;
            if index == 10 {
                assert!(
                    plan_dir.join("rev-10.seal").exists(),
                    "Checkpoint versiegelt"
                );
            }
        }

        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.current()?.nodes.len(), 2);
        Ok(())
    }

    #[test]
    fn test_checkpoint_compacts_snapshots_and_replays_history() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("compact"),
                goal: "checkpoint".to_owned(),
            },
            "o",
        )?;
        for _ in 2..=10 {
            store.apply(PlanAction::Inspect, "o")?;
        }
        let plan_dir = dir.path().join("plans").join("compact");
        assert!(plan_dir.join("rev-10.json").exists());
        assert!(plan_dir.join("rev-10.seal").exists());
        assert!(
            !plan_dir.join("rev-1.json").exists(),
            "alter Checkpoint wird kompakt entfernt"
        );
        store.apply(PlanAction::Inspect, "o")?;
        drop(store);
        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.revision(), RevisionId::new(11));
        assert_eq!(reloaded.history(None)?.len(), 11);
        Ok(())
    }

    // ── apply_batch (persistent) ───────────────────────────────────────────

    fn history_lines(dir: &TempDir, id: &str) -> TestResult<usize> {
        let content =
            std::fs::read_to_string(dir.path().join("plans").join(id).join("history.jsonl"))?;
        Ok(content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count())
    }

    #[test]
    fn checked_publication_rejects_after_staging_without_replayable_events() -> TestResult {
        let dir = TempDir::new()?;
        let store = seeded_store(&dir, "p-fenced")?;
        let before = store.current()?;
        let history = std::fs::read(dir.path().join("plans/p-fenced/history.jsonl"))?;
        let check = || -> PlanResult<()> {
            // Invalidation forces a checkpoint: both temporary files have
            // been written and fsynced before this authoritative boundary.
            let count = std::fs::read_dir(dir.path().join("plans/p-fenced"))?
                .collect::<Result<Vec<_>, _>>()?
                .len();
            assert!(count >= 5, "snapshot and seal must already be staged");
            Err(PlanError::Io(std::io::Error::other(
                "lease expired during staging",
            )))
        };
        assert!(
            store
                .apply_batch_checked(
                    &before.id,
                    vec![PlanAction::Invalidate {
                        ids: vec![TaskId::new("t1")],
                        condition: crate::types::InvalidationCondition::ManualInvalidate,
                    }],
                    "worker",
                    before.revision,
                    &check,
                )
                .is_err()
        );
        assert_eq!(store.revision(), before.revision);
        assert_eq!(
            std::fs::read(dir.path().join("plans/p-fenced/history.jsonl"))?,
            history
        );
        drop(store);
        assert_eq!(make_store(&dir)?.current()?.revision, before.revision);
        Ok(())
    }

    #[test]
    fn test_apply_batch_persists_one_snapshot_and_all_events() -> TestResult {
        let dir = TempDir::new()?;
        let store = seeded_store(&dir, "p-batch")?;

        let result = store.apply_batch(
            &PlanId::new("p-batch"),
            vec![
                PlanAction::AddNode {
                    node: make_node("t2"),
                },
                PlanAction::AddDependency {
                    child: TaskId::new("t2"),
                    parent: TaskId::new("t1"),
                },
            ],
            "controller",
            RevisionId::new(2),
        )?;

        assert_eq!(result.revision, RevisionId::new(4));
        assert_eq!(history_lines(&dir, "p-batch")?, 3); // One framed line for the two-action batch.
        let plan_dir = dir.path().join("plans").join("p-batch");
        assert!(!plan_dir.join("rev-4.json").exists());
        assert!(plan_dir.join("rev-1.json").exists());

        let reloaded = make_store(&dir)?;
        let plan = reloaded.current()?;
        assert_eq!(plan.revision, RevisionId::new(4));
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(reloaded.history(None)?.len(), 4);
        Ok(())
    }

    #[test]
    fn test_apply_batch_failure_in_third_action_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let store = seeded_store(&dir, "p-atomic")?;

        let result = store.apply_batch(
            &PlanId::new("p-atomic"),
            vec![
                PlanAction::AddNode {
                    node: make_node("t2"),
                },
                PlanAction::SetStatus {
                    id: TaskId::new("t2"),
                    status: PlanNodeStatus::Ready,
                    reason: None,
                },
                PlanAction::AddDependency {
                    child: TaskId::new("t2"),
                    parent: TaskId::new("missing"),
                },
            ],
            "controller",
            RevisionId::new(2),
        );

        assert!(
            matches!(
                &result,
                Err(PlanError::BatchActionRejected { index: 2, source })
                    if matches!(**source, PlanError::NodeMissing { .. })
            ),
            "war: {result:?}"
        );
        assert_eq!(store.revision(), RevisionId::new(2));
        assert_eq!(store.current()?.nodes.len(), 1);
        assert_eq!(history_lines(&dir, "p-atomic")?, 2);
        let plan_dir = dir.path().join("plans").join("p-atomic");
        for name in ["rev-3.json", "rev-4.json", "rev-5.json", "rev-5.seal"] {
            assert!(
                !plan_dir.join(name).exists(),
                "{name} darf nicht existieren"
            );
        }
        Ok(())
    }

    #[test]
    fn test_apply_batch_revision_conflict_writes_nothing() -> TestResult {
        let dir = TempDir::new()?;
        let store = seeded_store(&dir, "p-conflict")?;

        let result = store.apply_batch(
            &PlanId::new("p-conflict"),
            vec![PlanAction::Inspect],
            "controller",
            RevisionId::new(1),
        );

        assert!(matches!(result, Err(PlanError::RevisionConflict { .. })));
        assert_eq!(history_lines(&dir, "p-conflict")?, 2);
        Ok(())
    }

    // ── Runde 5, Teil P: Plan-Katalog ───────────────────────────────────────

    fn create(store: &FilePlanStore, id: &str, goal: &str) -> TestResult {
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new(id),
                goal: goal.to_owned(),
            },
            "orchestrator",
        )?;
        Ok(())
    }

    #[test]
    fn test_two_plans_in_a_row_survive_a_restart() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            create(&store, "plan-analyze", "automatische Analyse")?;
            store.apply(
                PlanAction::AddNode {
                    node: make_node("t-a"),
                },
                "op:analyze",
            )?;
            // Genau der Fall aus dem Transkript: ein Auto-Plan blockiert
            // das eigene `create` nicht mehr.
            create(&store, "crypt-guard-hardening-v1", "Härtung")?;
            assert_eq!(store.current()?.id, PlanId::new("crypt-guard-hardening-v1"));
            assert_eq!(store.revision(), RevisionId::new(1));
            assert!(
                dir.path()
                    .join("plans")
                    .join("crypt-guard-hardening-v1")
                    .join("rev-1.json")
                    .exists(),
                "jeder Plan beginnt mit eigenem Checkpoint rev-1"
            );
        }

        let reloaded = make_store(&dir)?;
        assert_eq!(
            reloaded.current()?.id,
            PlanId::new("crypt-guard-hardening-v1"),
            "der aktive Plan kommt aus plan-index.json"
        );
        let analyze = reloaded.plan_by_id(&PlanId::new("plan-analyze"))?;
        assert_eq!(analyze.nodes.len(), 1);
        assert_eq!(reloaded.list_plans()?.len(), 2);
        // Mutationen treffen nach dem Neustart den aktiven Plan.
        reloaded.apply(
            PlanAction::AddNode {
                node: make_node("t-h"),
            },
            "orchestrator",
        )?;
        assert_eq!(reloaded.current()?.revision, RevisionId::new(2));
        assert_eq!(
            reloaded
                .plan_by_id(&PlanId::new("plan-analyze"))?
                .nodes
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn test_switch_archive_and_list_are_persisted() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            create(&store, "p-a", "A")?;
            create(&store, "p-b", "B")?;
            store.switch_plan(&PlanId::new("p-a"), "human:test")?;
            store.archive_plan(&PlanId::new("p-b"), "human:test")?;
            store.set_approval(
                &PlanId::new("p-a"),
                crate::PlanApproval::Proposed,
                "model:test",
            )?;
        }
        let reloaded = make_store(&dir)?;
        assert_eq!(reloaded.current()?.id, PlanId::new("p-a"));
        let listed = reloaded.list_plans()?;
        let ids: Vec<&str> = listed.iter().map(|summary| summary.id.as_str()).collect();
        assert_eq!(ids, vec!["p-a", "p-b"]);
        let b = reloaded.plan_meta(&PlanId::new("p-b"))?;
        assert!(b.archived, "Archiv-Flag überlebt den Neustart");
        assert_eq!(
            reloaded.plan_meta(&PlanId::new("p-a"))?.approval,
            crate::PlanApproval::Proposed
        );
        assert!(
            dir.path()
                .join("plans")
                .join("p-b")
                .join("rev-1.json")
                .exists(),
            "archivieren löscht nichts"
        );

        reloaded.archive_plan(&PlanId::new("p-a"), "human:test")?;
        assert!(matches!(reloaded.current(), Err(PlanError::PlanNotFound)));
        drop(reloaded);
        let again = make_store(&dir)?;
        assert!(
            matches!(again.current(), Err(PlanError::PlanNotFound)),
            "kein aktiver Plan bleibt auch nach dem Neustart so"
        );
        again.switch_plan(&PlanId::new("p-b"), "human:test")?;
        assert!(!again.plan_meta(&PlanId::new("p-b"))?.archived);
        assert!(matches!(
            again.archive_plan(&PlanId::new("p-nix"), "human:test"),
            Err(PlanError::PlanUnknown { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_legacy_single_plan_store_migrates_without_data_loss() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            create(&store, "p-alt", "Altbestand")?;
            store.apply(
                PlanAction::AddNode {
                    node: make_node("t-1"),
                },
                "orchestrator",
            )?;
        }
        // Altbestand nachbauen: vor Teil P gab es keinen Index.
        let index_path = dir.path().join(crate::catalog::PLAN_INDEX_FILE);
        std::fs::remove_file(&index_path)?;

        let migrated = make_store(&dir)?;
        let plan = migrated.current()?;
        assert_eq!(plan.id, PlanId::new("p-alt"));
        assert_eq!(plan.nodes.len(), 1, "Knoten aus der History nachgespielt");
        assert_eq!(migrated.history(None)?.len(), 2);
        let meta = migrated.plan_meta(&PlanId::new("p-alt"))?;
        assert_eq!(
            meta,
            crate::PlanMeta::default(),
            "Altbestand gilt als bestätigt"
        );
        assert!(
            !index_path.exists(),
            "das Lesen eines Altbestands schreibt nichts"
        );

        // Das erste neue `create` klappt und schreibt den Index.
        create(&migrated, "p-neu", "neu")?;
        assert!(index_path.exists());
        assert_eq!(migrated.list_plans()?.len(), 2);
        Ok(())
    }

    #[test]
    fn test_legacy_store_with_two_plan_dirs_activates_the_newest() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            create(&store, "p-klein", "klein")?;
            create(&store, "p-gross", "groß")?;
            for id in ["t-1", "t-2"] {
                store.apply(
                    PlanAction::AddNode {
                        node: make_node(id),
                    },
                    "orchestrator",
                )?;
            }
            // Checkpoint erzwingen, damit p-gross die höchste Snapshot-
            // Revision trägt (Status-Terminal ist ein Checkpoint-Auslöser).
            store.apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t-1"),
                    status: PlanNodeStatus::Superseded,
                    reason: None,
                },
                "orchestrator",
            )?;
            store.switch_plan(&PlanId::new("p-klein"), "human:test")?;
        }
        std::fs::remove_file(dir.path().join(crate::catalog::PLAN_INDEX_FILE))?;

        let migrated = make_store(&dir)?;
        assert_eq!(
            migrated.current()?.id,
            PlanId::new("p-gross"),
            "ohne Index gilt die Wahl des Einzelplan-Stores (höchste Revision)"
        );
        assert!(migrated.plan_by_id(&PlanId::new("p-klein")).is_ok());
        Ok(())
    }

    #[test]
    fn test_corrupt_inactive_plan_is_skipped_but_not_overwritten() -> TestResult {
        let dir = TempDir::new()?;
        {
            let store = make_store(&dir)?;
            create(&store, "p-kaputt", "wird beschädigt")?;
            create(&store, "p-gut", "bleibt aktiv")?;
        }
        std::fs::write(
            dir.path().join("plans").join("p-kaputt").join("rev-1.json"),
            b"{ manipuliert",
        )?;
        let store = make_store(&dir)?;
        assert_eq!(store.current()?.id, PlanId::new("p-gut"));
        assert!(matches!(
            store.plan_by_id(&PlanId::new("p-kaputt")),
            Err(PlanError::PlanUnknown { .. })
        ));
        assert!(matches!(
            store.apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-kaputt"),
                    goal: "überschreiben?".to_owned(),
                },
                "orchestrator",
            ),
            Err(PlanError::PlanExists { .. })
        ));
        Ok(())
    }
    #[test]
    fn independent_instances_reject_stale_compare_and_swap() -> TestResult {
        let dir = TempDir::new()?;
        let first = make_store(&dir)?;
        let id = PlanId::new("multiwriter");
        first.apply(
            PlanAction::Create {
                plan_id: id.clone(),
                goal: "g".into(),
            },
            "owner",
        )?;
        let second = make_store(&dir)?;
        first.apply_batch(
            &id,
            vec![PlanAction::AddNode {
                node: make_node("one"),
            }],
            "one",
            RevisionId::new(1),
        )?;
        let stale = second.apply_batch(
            &id,
            vec![PlanAction::AddNode {
                node: make_node("two"),
            }],
            "two",
            RevisionId::new(1),
        );
        assert!(matches!(stale, Err(PlanError::RevisionConflict { .. })));
        assert_eq!(second.current()?.revision, RevisionId::new(2));
        second.apply_batch(
            &id,
            vec![PlanAction::AddNode {
                node: make_node("two"),
            }],
            "two",
            RevisionId::new(2),
        )?;
        assert_eq!(first.current()?.revision, RevisionId::new(3));
        assert_eq!(first.history(None)?.len(), 3);
        Ok(())
    }

    #[test]
    fn checkpoint_failure_after_wal_commit_is_successful_and_visible() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        let id = PlanId::new("checkpoint-failure");
        store.apply(
            PlanAction::Create {
                plan_id: id.clone(),
                goal: "g".into(),
            },
            "owner",
        )?;
        for n in 2..10 {
            store.apply(
                PlanAction::AddNode {
                    node: make_node(&format!("n{n}")),
                },
                "owner",
            )?;
        }
        let target = dir.path().join("plans/checkpoint-failure/rev-10.json");
        store.apply_batch_checked(
            &id,
            vec![PlanAction::AddNode {
                node: make_node("n10"),
            }],
            "owner",
            RevisionId::new(9),
            &|| {
                std::fs::create_dir(&target)?;
                Ok(())
            },
        )?;
        assert_eq!(store.current()?.revision, RevisionId::new(10));
        assert_eq!(make_store(&dir)?.current()?.revision, RevisionId::new(10));
        Ok(())
    }

    #[test]
    fn truncated_batch_never_recovers_a_prefix() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        let id = PlanId::new("torn-batch");
        store.apply(
            PlanAction::Create {
                plan_id: id.clone(),
                goal: "g".into(),
            },
            "owner",
        )?;
        let path = dir.path().join("plans/torn-batch/history.jsonl");
        let before = std::fs::read(&path)?;
        store.apply_batch(
            &id,
            vec![
                PlanAction::AddNode {
                    node: make_node("one"),
                },
                PlanAction::AddNode {
                    node: make_node("two"),
                },
            ],
            "owner",
            RevisionId::new(1),
        )?;
        let after = std::fs::read(&path)?;
        // Preserve the first complete JSONL record after Create, but not the
        // complete batch. Old per-event WAL accepts revision two here.
        let suffix = &after[before.len()..];
        let boundary = suffix
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or(TestError::Missing("newline"))?
            + 1;
        let cut = if boundary < suffix.len() {
            before.len() + boundary
        } else {
            after.len() - 1
        };
        std::fs::write(&path, &after[..cut])?;
        let recovered = make_store(&dir)?;
        assert_eq!(recovered.current()?.revision, RevisionId::new(1));
        Ok(())
    }
    #[test]
    fn bootstrap_recovers_exact_tenant_without_checkpoint() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        let tenant = harw_types::TenantId::new();
        store.create_for_tenant(
            PlanId::new("bootstrap"),
            "goal".into(),
            Some(tenant),
            "owner",
        )?;
        let expected = serde_json::to_value(store.current()?)?;
        std::fs::remove_file(dir.path().join("plans/bootstrap/rev-1.json"))?;
        std::fs::remove_file(dir.path().join("plans/bootstrap/rev-1.seal"))?;
        let recovered = make_store(&dir)?;
        assert_eq!(serde_json::to_value(recovered.current()?)?, expected);
        recovered.apply(PlanAction::Inspect, "owner")?;
        assert_eq!(make_store(&dir)?.current()?.revision, RevisionId::new(2));
        Ok(())
    }

    #[test]
    fn torn_first_create_is_uncommitted_and_id_can_be_retried() -> TestResult {
        for empty in [false, true] {
            let dir = TempDir::new()?;
            let store = make_store(&dir)?;
            create(&store, "first", "initial")?;
            let path = dir.path().join("plans/first/history.jsonl");
            let bytes = std::fs::read(&path)?;
            std::fs::write(
                &path,
                if empty {
                    &bytes[..0]
                } else {
                    &bytes[..bytes.len() / 2]
                },
            )?;
            std::fs::remove_file(dir.path().join("plans/first/rev-1.json"))?;
            std::fs::remove_file(dir.path().join("plans/first/rev-1.seal"))?;
            std::fs::remove_file(dir.path().join("plan-index.json"))?;
            let recovered = make_store(&dir)?;
            assert!(matches!(recovered.current(), Err(PlanError::PlanNotFound)));
            create(&recovered, "first", "retry")?;
            assert_eq!(make_store(&dir)?.current()?.goal_statement, "retry");
        }
        Ok(())
    }

    #[test]
    fn orphan_seal_does_not_invalidate_legacy_checkpoint_after_committed_batch() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        create(&store, "legacy-orphan", "g")?;
        std::fs::remove_file(dir.path().join("plans/legacy-orphan/rev-1.seal"))?;
        for _ in 2..10 {
            store.apply(PlanAction::Inspect, "owner")?;
        }
        let target = dir.path().join("plans/legacy-orphan/rev-10.json");
        store.apply_batch_checked(
            &PlanId::new("legacy-orphan"),
            vec![PlanAction::Inspect],
            "owner",
            RevisionId::new(9),
            &|| {
                std::fs::create_dir(&target)?;
                Ok(())
            },
        )?;
        assert_eq!(make_store(&dir)?.current()?.revision, RevisionId::new(10));
        Ok(())
    }

    #[test]
    fn snapshot_only_legacy_is_readable_but_cannot_append_unrecoverable_history() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        create(&store, "snapshot-only", "g")?;
        std::fs::remove_file(dir.path().join("plans/snapshot-only/history.jsonl"))?;
        let recovered = make_store(&dir)?;
        assert_eq!(recovered.current()?.revision, RevisionId::new(1));
        assert!(recovered.apply(PlanAction::Inspect, "owner").is_err());
        assert!(
            recovered
                .apply_batch(
                    &PlanId::new("snapshot-only"),
                    vec![PlanAction::Inspect],
                    "owner",
                    RevisionId::new(1)
                )
                .is_err()
        );
        assert_eq!(make_store(&dir)?.current()?.revision, RevisionId::new(1));
        Ok(())
    }

    #[test]
    fn independent_catalog_updates_do_not_lose_metadata() -> TestResult {
        let dir = TempDir::new()?;
        let first = make_store(&dir)?;
        create(&first, "a", "a")?;
        let second = make_store(&dir)?;
        create(&second, "b", "b")?;
        first.set_approval(&PlanId::new("a"), PlanApproval::Proposed, "owner")?;
        second.archive_plan(&PlanId::new("b"), "owner")?;
        assert_eq!(
            second.plan_meta(&PlanId::new("a"))?.approval,
            PlanApproval::Proposed
        );
        assert!(first.plan_meta(&PlanId::new("b"))?.archived);
        assert!(matches!(first.current(), Err(PlanError::PlanNotFound)));
        Ok(())
    }

    #[test]
    fn indeterminate_outcome_blocks_operations_until_locked_reopen() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        create(&store, "old", "old")?;
        let index = dir.path().join("plan-index.json");
        // Valid readable index cannot be sabotaged before refresh. Its parent
        // publication is instead exercised directly through the post-WAL
        // outcome handler; actual rename failure is covered for checkpoints.
        let outcome: PlanResult<()> = store.record_outcome(Err(PlanError::CommitIndeterminate {
            path: index.display().to_string(),
        }));
        assert!(matches!(
            outcome,
            Err(PlanError::CommitIndeterminate { .. })
        ));
        assert!(matches!(
            store.current(),
            Err(PlanError::StoreRecoveryRequired)
        ));
        assert!(matches!(
            store.apply(PlanAction::Inspect, "owner"),
            Err(PlanError::StoreRecoveryRequired)
        ));
        assert_eq!(make_store(&dir)?.current()?.revision, RevisionId::new(1));
        Ok(())
    }

    #[test]
    fn root_lock_is_nonblocking_across_independent_instances_and_processes() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        let lock = FilePlanStore::lock_root(dir.path())?;
        assert!(matches!(store.current(), Err(PlanError::StoreContended)));
        assert!(matches!(
            FilePlanStore::new(dir.path()),
            Err(PlanError::StoreContended)
        ));
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "file_store::tests::subprocess_lock_probe",
                "--nocapture",
            ])
            .env("HARW_PLAN_LOCK_PROBE_ROOT", dir.path())
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        drop(lock);
        create(&store, "released", "works")?;
        Ok(())
    }

    #[test]
    fn subprocess_lock_probe() -> TestResult {
        if let Some(root) = std::env::var_os("HARW_PLAN_LOCK_PROBE_ROOT") {
            assert!(matches!(
                FilePlanStore::new(root),
                Err(PlanError::StoreContended)
            ));
        }
        Ok(())
    }

    struct FaultSink {
        bytes: Vec<u8>,
        fail_write_at: Option<usize>,
        fail_sync_once: bool,
        fail_rollback: bool,
    }
    impl Write for FaultSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let count = self
                .fail_write_at
                .map(|at| at.saturating_sub(self.bytes.len()).min(bytes.len()))
                .unwrap_or(bytes.len());
            if count == 0 {
                return Err(std::io::Error::other("injected short write"));
            }
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl WalSink for FaultSink {
        fn durable(&mut self) -> std::io::Result<()> {
            if std::mem::take(&mut self.fail_sync_once) {
                Err(std::io::Error::other("injected fsync failure"))
            } else {
                Ok(())
            }
        }
        fn rollback(&mut self, len: u64) -> std::io::Result<()> {
            self.bytes.truncate(len as usize);
            if self.fail_rollback {
                return Err(std::io::Error::other("injected rollback fsync failure"));
            }
            self.durable()
        }
    }

    #[test]
    fn append_faults_are_rolled_back_or_marked_indeterminate() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        for fail_write_at in [Some(5), None] {
            for fail_rollback in [false, true] {
                let mut sink = FaultSink {
                    bytes: b"old".to_vec(),
                    fail_write_at,
                    fail_sync_once: fail_write_at.is_none(),
                    fail_rollback,
                };
                let result = append_transaction(&mut sink, b"transaction\n", 3, dir.path());
                assert_eq!(sink.bytes, b"old");
                if fail_rollback {
                    assert!(matches!(result, Err(PlanError::CommitIndeterminate { .. })));
                    let result = store.record_outcome(result);
                    assert!(matches!(result, Err(PlanError::CommitIndeterminate { .. })));
                    assert!(matches!(
                        store.current(),
                        Err(PlanError::StoreRecoveryRequired)
                    ));
                } else {
                    assert!(matches!(result, Err(PlanError::Io(_))));
                }
            }
        }
        Ok(())
    }
    #[test]
    fn real_legacy_event_lines_accept_new_framed_suffix() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        create(&store, "mixed", "g")?;
        store.apply(PlanAction::Inspect, "owner")?;
        let mut legacy = Vec::new();
        for event in store.history(None)? {
            serde_json::to_writer(&mut legacy, &event)?;
            legacy.push(b'\n');
        }
        std::fs::write(dir.path().join("plans/mixed/history.jsonl"), legacy)?;
        let reopened = make_store(&dir)?;
        reopened.apply(PlanAction::Inspect, "owner")?;
        let recovered = make_store(&dir)?;
        assert_eq!(recovered.history(None)?.len(), 3);
        assert_eq!(recovered.current()?.revision, RevisionId::new(3));
        Ok(())
    }

    #[test]
    fn snapshot_inside_framed_batch_is_rejected() -> TestResult {
        let dir = TempDir::new()?;
        let store = make_store(&dir)?;
        create(&store, "boundary", "g")?;
        store.apply_batch(
            &PlanId::new("boundary"),
            vec![PlanAction::Inspect, PlanAction::Inspect],
            "owner",
            RevisionId::new(1),
        )?;
        let mut partial = store.current()?;
        partial.revision = RevisionId::new(2);
        let path = dir.path().join("plans/boundary/rev-2.json");
        let bytes = serde_json::to_vec(&partial)?;
        let (seal, _) = seal::build(None, 2, &bytes)?;
        std::fs::write(&path, bytes)?;
        std::fs::write(path.with_extension("seal"), seal)?;
        assert!(FilePlanStore::new(dir.path()).is_err());
        Ok(())
    }
    #[test]
    fn missing_relative_root_and_ancestors_are_created_durably() -> TestResult {
        let cwd = std::env::current_dir()?;
        let temporary = TempDir::new_in(&cwd)?;
        let relative = temporary
            .path()
            .strip_prefix(&cwd)
            .map_err(|error| TestError::Unexpected(error.to_string()))?
            .join("nested/plans");
        assert!(!relative.is_absolute());
        let store = FilePlanStore::new(&relative)?;
        create(&store, "relative", "works")?;
        assert_eq!(
            FilePlanStore::new(relative)?.current()?.id,
            PlanId::new("relative")
        );
        Ok(())
    }

    #[test]
    fn refresh_enforces_local_node_limit_after_another_writer_grows_plan() -> TestResult {
        let dir = TempDir::new()?;
        let strict = FilePlanStore::with_config(dir.path(), config_with_max_nodes(1))?;
        create(&strict, "limits", "g")?;
        let wider = make_store(&dir)?;
        wider.apply_batch(
            &PlanId::new("limits"),
            vec![
                PlanAction::AddNode {
                    node: make_node("one"),
                },
                PlanAction::AddNode {
                    node: make_node("two"),
                },
            ],
            "owner",
            RevisionId::new(1),
        )?;
        assert!(matches!(
            strict.current(),
            Err(PlanError::Config(
                PlanToolConfigError::NodeLimitExceeded { .. }
            ))
        ));
        assert!(matches!(
            strict.apply(PlanAction::Inspect, "owner"),
            Err(PlanError::Config(
                PlanToolConfigError::NodeLimitExceeded { .. }
            ))
        ));
        assert_eq!(wider.current()?.revision, RevisionId::new(3));
        Ok(())
    }
}

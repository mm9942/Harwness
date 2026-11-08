//! Spool-Schreiber des Sentinels: eingefrorene Befunde mit begrenztem
//! Beleg-Ausschnitt, geschrieben auf einem eigenen Thread (PL-100 WP0.12).
//!
//! # Zweck
//! Jeder gemeldete Befund wird mit seinem Beleg-Ausschnitt zu einem
//! [`FindingRecord`] eingefroren (`harw_dod_sentinel::spool::slice_record`,
//! aufgerufen in `crate::findings`) und hier an den
//! [`FindingSpool`] übergeben. Die Exportzeile trägt den Digest dieses
//! Records (`record_digest`, `crate::export`); Triage und Escalator lesen den
//! Record später selbst aus dem Spool und prüfen ihn.
//!
//! # Eigener Schreibthread, begrenzte Warteschlange
//! [`FindingSpool::put`] schreibt atomar mit `fsync` und kann bei einer
//! langsamen Platte blockieren. Der Sammelthread soll davon nie aufgehalten
//! werden: [`SpoolWriter::submit`] legt den Record nur in eine begrenzte
//! Warteschlange ([`QUEUE_CAPACITY`], `std::sync::mpsc::sync_channel`), aus
//! der ein eigener Thread ([`WRITER_THREAD_NAME`]) schreibt. Ist die
//! Warteschlange voll, wird der Record **verworfen**, gezählt
//! (`dropped_total`) und `health.json` meldet den Spool als `degraded`
//! (`queue_overflow`); der Befund selbst wird trotzdem exportiert und
//! geloggt, nur ohne `record_digest`.
//!
//! # Ablage und Rechte
//! Der Spool liegt unter `<home>/`[`SPOOL_DIR_NAME`] — im Betrieb im
//! Zustandsverzeichnis `@STATEDIR@` (`0710`, Gruppe `harw-dod-readers`, die
//! nur hindurchgehen darf). [`prepare_spool_dir`] legt das Verzeichnis mit
//! [`SPOOL_DIR_MODE`] (`0700`) an und verengt ein bestehendes, das für Gruppe
//! oder andere zugänglich ist: Records tragen rohe Belege (Pfade, Ziele,
//! Prozessnamen) und sind für die Lesergruppe von harw nicht bestimmt. Das
//! Verzeichnis liegt unter `--home` und damit in der beschreibbaren
//! Landlock-Wurzel des Sentinels.
//!
//! # Aufbewahrung
//! [`sentinel_limits`]: Records höchstens
//! [`MAX_SLICE_RECORD_BYTES`] (64 KiB), Alter höchstens
//! [`DEFAULT_MAX_AGE`] (14 Tage), Summe höchstens [`MAX_SPOOL_BYTES`]
//! (1 GiB) — beides abgeglichen mit der Retention-Klasse `dod_spool` in
//! `harw-retention`. Weil der Sentinel keinen Konsum-Cursor kennt (Triage und
//! Escalator lesen getrennt), löscht der Sweep auch unkonsumierte Records
//! (`delete_unconsumed`): ein Record, der älter als 14 Tage ist, ist nicht
//! mehr triagierbar. Der Sweep läuft auf dem Schreibthread, angestoßen je
//! Zyklus über [`SpoolWriter::tick`] mit der injizierten Zykluszeit und
//! höchstens jede Minute (`FindingSpool::maybe_sweep`).
//!
//! # Gesundheit
//! [`SpoolStats`] sind atomare Zähler, die Schreibthread und Sammelthread
//! teilen; `crate::health_export` liest sie je Zyklus ([`SpoolCounters`]).
//! `SpoolFull`, `RecordTooLarge`, ein Schreibfehler, eine volle
//! Warteschlange und ein nicht geöffneter Spool erscheinen dort als
//! `degraded` mit Grund ([`SpoolFailure::label`]).
//!
//! # Journal
//! Auf `warn` stehen nur Gründe ([`SpoolFailure::label`]), nie Pfade oder
//! Inhalte; die Fehlermeldung mit Pfad steht auf `debug`. Eine anhaltende
//! Störung wird einmal gewarnt, Wiederholungen gehen auf `debug`.
//!
//! # Nebenläufigkeit
//! [`SpoolWriter`] lebt im Sammelthread (in `crate::findings::RuleCycle`);
//! der Schreibthread besitzt den [`FindingSpool`] allein. Beim Drop schließt
//! der [`SpoolWriter`] die Warteschlange und wartet, bis der Schreibthread
//! alles Eingereihte geschrieben hat (`--once` verliert so nichts).
//!
//! # Fehler
//! Keine nach außen: ein nicht zu öffnender Spool macht den Sentinel nicht
//! startunfähig ([`SpoolWriter::open`] liefert dann
//! [`SpoolWriter::unavailable`]), jeder Schreibfehler wird gezählt.
//!
//! # Examples
//! ```rust,ignore
//! let writer = SpoolWriter::open(&home);
//! if writer.submit(record) {
//!     // `record_digest` darf in die Exportzeile.
//! }
//! writer.tick(now);
//! ```

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::SystemTime;

use harw_dod_rules::finding::FindingRecord;
use harw_dod_sentinel::spool::{
    DEFAULT_MAX_AGE, DEFAULT_MAX_ENTRIES, FindingSpool, MAX_SLICE_RECORD_BYTES, SpoolError,
    SpoolId, SpoolLimits, SpoolResult, SweepReport,
};
use jiff::Timestamp;

/// Name des Spool-Verzeichnisses unter `--home`.
pub const SPOOL_DIR_NAME: &str = "spool";

/// Modus des Spool-Verzeichnisses: nur der Sentinel (`rwx------`).
pub const SPOOL_DIR_MODE: u32 = 0o700;

/// Plätze der Warteschlange zwischen Sammel- und Schreibthread.
pub const QUEUE_CAPACITY: usize = 64;

/// Obergrenze der Summe aller Records (1 GiB, gleich `max_bytes` der
/// Retention-Klasse `dod_spool`).
pub const MAX_SPOOL_BYTES: u64 = 1024 * 1024 * 1024;

/// Name des Schreibthreads.
pub const WRITER_THREAD_NAME: &str = "harw-spool-writer";

/// Die Grenzen des Sentinel-Spools (siehe Moduldoku, „Aufbewahrung").
///
/// # Returns
/// Records ≤ [`MAX_SLICE_RECORD_BYTES`], Alter ≤ [`DEFAULT_MAX_AGE`], Summe
/// ≤ [`MAX_SPOOL_BYTES`], Sweep wirksam, auch für unkonsumierte Records.
#[must_use]
pub const fn sentinel_limits() -> SpoolLimits {
    SpoolLimits {
        max_record_bytes: MAX_SLICE_RECORD_BYTES,
        max_entries: DEFAULT_MAX_ENTRIES,
        max_age: Some(DEFAULT_MAX_AGE),
        max_total_bytes: Some(MAX_SPOOL_BYTES),
        retention_apply: true,
        delete_unconsumed: true,
    }
}

/// Legt das Spool-Verzeichnis mit [`SPOOL_DIR_MODE`] an oder verengt ein
/// bestehendes auf den Eigentümer.
///
/// # Description
/// Fehlt `dir`, wird es (nicht rekursiv) mit `0700` angelegt. Besteht es und
/// ist es für Gruppe oder andere zugänglich, werden diese Bits entfernt (und
/// mit `info` gemeldet). Ein Symlink oder eine Nicht-Verzeichnis-Datei am
/// Pfad wird abgelehnt. Voraussetzung wie beim Spool selbst: das
/// Elternverzeichnis ist nur für den Sentinel beschreibbar.
///
/// # Errors
/// [`SpoolError::SymlinkRejected`], [`SpoolError::NotADirectory`] oder
/// [`SpoolError::Io`] (Anlegen, Prüfen, Verengen).
pub fn prepare_spool_dir(dir: &Path) -> SpoolResult<()> {
    let io_error = |op: &'static str| {
        move |source: io::Error| SpoolError::Io {
            op,
            path: dir.to_path_buf(),
            source,
        }
    };
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(SpoolError::SymlinkRejected {
            path: dir.to_path_buf(),
        }),
        Ok(meta) if !meta.is_dir() => Err(SpoolError::NotADirectory {
            path: dir.to_path_buf(),
        }),
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o7777;
            if mode & !SPOOL_DIR_MODE != 0 {
                let narrowed = mode & SPOOL_DIR_MODE;
                fs::set_permissions(dir, fs::Permissions::from_mode(narrowed))
                    .map_err(io_error("restrict the spool directory mode"))?;
                tracing::info!(
                    from = %format_args!("{mode:04o}"),
                    to = %format_args!("{narrowed:04o}"),
                    "finding spool directory narrowed to owner-only access"
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match DirBuilder::new().mode(SPOOL_DIR_MODE).create(dir) {
                Ok(()) => Ok(()),
                // Wettlauf mit einem anderen Anleger: `FindingSpool::open`
                // prüft danach, was tatsächlich dort liegt.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(io_error("create the spool directory")(error)),
            }
        }
        Err(error) => Err(io_error("inspect the spool directory")(error)),
    }
}

/// Warum ein Record den Spool nicht erreicht hat.
///
/// # Description
/// Der jüngste Grund steht in `health.json` als `spool.degrade_reason`.
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpoolFailure {
    /// Der Spool ließ sich beim Start nicht öffnen, oder der Schreibthread
    /// ist beendet.
    Unavailable,
    /// Die Warteschlange war voll; der Record wurde verworfen.
    QueueOverflow,
    /// Der Spool hält schon `max_entries` Records ([`SpoolError::SpoolFull`]).
    SpoolFull,
    /// Der Record überschreitet die Größengrenze
    /// ([`SpoolError::RecordTooLarge`]), auch schon beim Zuschneiden.
    RecordTooLarge,
    /// Jeder andere Fehler beim Erzeugen oder Schreiben eines Records.
    WriteFailed,
}

impl SpoolFailure {
    /// Stabile Kurzform für `health.json` und Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::QueueOverflow => "queue_overflow",
            Self::SpoolFull => "spool_full",
            Self::RecordTooLarge => "record_too_large",
            Self::WriteFailed => "write_failed",
        }
    }

    /// Ordnet einen Spool-Fehler seinem Grund zu.
    #[must_use]
    pub const fn of_error(error: &SpoolError) -> Self {
        match error {
            SpoolError::SpoolFull { .. } => Self::SpoolFull,
            SpoolError::RecordTooLarge { .. } => Self::RecordTooLarge,
            _ => Self::WriteFailed,
        }
    }

    // Kodierung für `SpoolStats::last_failure` (`0` = noch keiner).
    const fn code(self) -> u8 {
        match self {
            Self::Unavailable => 1,
            Self::QueueOverflow => 2,
            Self::SpoolFull => 3,
            Self::RecordTooLarge => 4,
            Self::WriteFailed => 5,
        }
    }

    // Umkehrung von `code`.
    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Unavailable),
            2 => Some(Self::QueueOverflow),
            3 => Some(Self::SpoolFull),
            4 => Some(Self::RecordTooLarge),
            5 => Some(Self::WriteFailed),
            _ => None,
        }
    }
}

/// Atomare Zähler des Spool-Schreibers, geteilt zwischen Sammel-,
/// Schreibthread und `crate::health_export`.
///
/// # Concurrency
/// `Send + Sync`; jeder Zähler ist für sich atomar, ein [`Self::snapshot`]
/// ist kein gemeinsamer Schnitt (für die Gesundheitsanzeige genügt das).
#[derive(Debug, Default)]
pub struct SpoolStats {
    // An den Schreibthread übergebene Records.
    enqueued: AtomicU64,
    // Geschriebene Records.
    written: AtomicU64,
    // Wegen voller Warteschlange verworfene Records.
    dropped: AtomicU64,
    // `SpoolFull` beim Schreiben.
    spool_full: AtomicU64,
    // `RecordTooLarge` beim Zuschneiden oder Schreiben.
    too_large: AtomicU64,
    // Alle anderen Fehler beim Erzeugen oder Schreiben.
    failed: AtomicU64,
    // Von der Aufbewahrung gelöschte Records.
    removed: AtomicU64,
    // Spool nicht geöffnet oder Schreibthread beendet.
    unavailable: AtomicBool,
    // Der jüngste Schreibversuch ist gescheitert.
    last_put_failed: AtomicBool,
    // `SpoolFailure::code` des jüngsten Grundes, `0` = keiner.
    last_failure: AtomicU8,
    // Die Warteschlange lief zuletzt über (begrenzt die Warnung).
    overflowing: AtomicBool,
    // Ein Sweep-Anstoß liegt in der Warteschlange.
    sweep_pending: AtomicBool,
}

/// Ein Stand der [`SpoolStats`] (`spool` in `health.json`).
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpoolCounters {
    /// An den Schreibthread übergebene Records.
    pub enqueued_total: u64,
    /// Geschriebene Records.
    pub written_total: u64,
    /// Wegen voller Warteschlange verworfene Records.
    pub dropped_total: u64,
    /// Wegen `SpoolFull` abgewiesene Records.
    pub spool_full_total: u64,
    /// Wegen ihrer Größe abgewiesene Records.
    pub too_large_total: u64,
    /// Aus anderen Gründen nicht erzeugte oder nicht geschriebene Records.
    pub failed_total: u64,
    /// Von der Aufbewahrung gelöschte Records.
    pub removed_total: u64,
    /// Der Spool ist nicht verfügbar.
    pub unavailable: bool,
    /// Der jüngste Schreibversuch ist gescheitert.
    pub last_put_failed: bool,
    /// Der jüngste Grund, aus dem ein Record den Spool nicht erreichte.
    pub last_failure: Option<SpoolFailure>,
}

impl SpoolCounters {
    /// Abgewiesene Records: `SpoolFull`, zu groß, sonstiger Fehler.
    #[must_use]
    pub const fn rejected_total(&self) -> u64 {
        self.spool_full_total
            .saturating_add(self.too_large_total)
            .saturating_add(self.failed_total)
    }
}

// Erhöht `counter` um `by`, sättigend (ein Überlauf ist nach 2^64
// Ereignissen unerreichbar, wird aber nicht still umgebrochen).
fn bump(counter: &AtomicU64, by: u64) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(by);
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

// `usize` → `u64` ohne `as`; auf allen Zielen verlustfrei.
fn count_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

impl SpoolStats {
    /// Der aktuelle Stand aller Zähler.
    #[must_use]
    pub fn snapshot(&self) -> SpoolCounters {
        SpoolCounters {
            enqueued_total: self.enqueued.load(Ordering::Relaxed),
            written_total: self.written.load(Ordering::Relaxed),
            dropped_total: self.dropped.load(Ordering::Relaxed),
            spool_full_total: self.spool_full.load(Ordering::Relaxed),
            too_large_total: self.too_large.load(Ordering::Relaxed),
            failed_total: self.failed.load(Ordering::Relaxed),
            removed_total: self.removed.load(Ordering::Relaxed),
            unavailable: self.unavailable.load(Ordering::Relaxed),
            last_put_failed: self.last_put_failed.load(Ordering::Relaxed),
            last_failure: SpoolFailure::from_code(self.last_failure.load(Ordering::Relaxed)),
        }
    }

    // Zählt `failure` und merkt es als jüngsten Grund.
    fn note_failure(&self, failure: SpoolFailure) {
        match failure {
            SpoolFailure::Unavailable => self.unavailable.store(true, Ordering::Relaxed),
            SpoolFailure::QueueOverflow => bump(&self.dropped, 1),
            SpoolFailure::SpoolFull => bump(&self.spool_full, 1),
            SpoolFailure::RecordTooLarge => bump(&self.too_large, 1),
            SpoolFailure::WriteFailed => bump(&self.failed, 1),
        }
        self.last_failure.store(failure.code(), Ordering::Relaxed);
    }

    // Verbucht das Ergebnis eines `put`; liefert, ob der Schreiber jetzt
    // gestört ist (begrenzt die Warnung auf den Übergang).
    fn record_put(&self, outcome: &SpoolResult<SpoolId>, was_failing: bool) -> bool {
        match outcome {
            Ok(_) => {
                bump(&self.written, 1);
                self.last_put_failed.store(false, Ordering::Relaxed);
                if was_failing {
                    tracing::info!("finding spool writes recovered");
                }
                false
            }
            Err(error) => {
                let failure = SpoolFailure::of_error(error);
                self.note_failure(failure);
                self.last_put_failed.store(true, Ordering::Relaxed);
                if was_failing {
                    tracing::debug!(
                        reason = failure.label(),
                        error = %error,
                        "finding spool write still failing"
                    );
                } else {
                    tracing::warn!(
                        reason = failure.label(),
                        "finding spool write failed; the finding is still exported and logged"
                    );
                    tracing::debug!(error = %error, "finding spool write failure detail");
                }
                true
            }
        }
    }

    // Verbucht das Ergebnis eines Aufbewahrungs-Sweeps.
    fn record_sweep(&self, outcome: &SpoolResult<Option<SweepReport>>) {
        match outcome {
            Ok(Some(report)) => {
                let removed = count_u64(report.removed);
                bump(&self.removed, removed);
                if removed > 0 {
                    tracing::info!(
                        removed,
                        kept = report.kept,
                        "finding spool retention removed aged records"
                    );
                }
                if report.deadline_hit || report.over_budget {
                    tracing::debug!(
                        deadline_hit = report.deadline_hit,
                        over_budget = report.over_budget,
                        "finding spool retention sweep incomplete"
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    reason = "retention_failed",
                    "finding spool retention sweep failed; retried on a later cycle"
                );
                tracing::debug!(error = %error, "finding spool retention failure detail");
            }
        }
    }
}

/// Nachricht an den Schreibthread.
enum SpoolMessage {
    /// Einen Record schreiben (geboxt: hält die Nachricht klein).
    Put(Box<FindingRecord>),
    /// Die Aufbewahrung zu diesem Zeitpunkt anwenden (höchstens jede
    /// Minute, `FindingSpool::maybe_sweep`).
    Sweep(SystemTime),
}

/// Übergibt Records über eine begrenzte Warteschlange an den Schreibthread
/// (siehe Moduldoku).
///
/// # Concurrency
/// Lebt im Sammelthread; `submit`, `tick` und `note_rejected` blockieren
/// nie. Der Drop schließt die Warteschlange und wartet auf den
/// Schreibthread.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug)]
pub struct SpoolWriter {
    // `None`: Spool nicht verfügbar.
    sender: Option<SyncSender<SpoolMessage>>,
    // Der Schreibthread; `None` ohne Spool.
    worker: Option<JoinHandle<()>>,
    // Mit `crate::health_export` geteilte Zähler.
    stats: Arc<SpoolStats>,
    // Obergrenze eines Records (aus den Spool-Grenzen).
    max_record_bytes: u64,
}

impl std::fmt::Debug for SpoolMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nie den Record-Inhalt (rohe Belege) ausgeben.
        match self {
            Self::Put(_) => f.write_str("Put(..)"),
            Self::Sweep(_) => f.write_str("Sweep(..)"),
        }
    }
}

impl SpoolWriter {
    /// Öffnet den Spool unter `<home>/`[`SPOOL_DIR_NAME`] und startet den
    /// Schreibthread.
    ///
    /// # Description
    /// [`prepare_spool_dir`], `FindingSpool::open_with_limits` mit
    /// [`sentinel_limits`], dann [`Self::spawn`]. Scheitert ein Schritt,
    /// wird das gewarnt (nur der Grund, der Pfad auf `debug`) und ein
    /// [`Self::unavailable`] Schreiber geliefert: der Sentinel läuft weiter,
    /// exportiert ohne `record_digest` und meldet den Spool als `degraded`.
    /// Muss **nach** der Landlock-Selbstbeschränkung laufen, damit der
    /// Schreibthread sie erbt.
    ///
    /// # Arguments
    /// - `home` (`&Path`): das Zustandsverzeichnis (`--home`).
    ///
    /// # Returns
    /// Den Schreiber; nie ein Fehler.
    #[must_use]
    pub fn open(home: &Path) -> Self {
        let dir = home.join(SPOOL_DIR_NAME);
        let opened = prepare_spool_dir(&dir)
            .and_then(|()| FindingSpool::open_with_limits(&dir, sentinel_limits()))
            .and_then(|spool| {
                Self::spawn(spool, QUEUE_CAPACITY).map_err(|source| SpoolError::Io {
                    op: "spawn the spool writer thread",
                    path: dir.clone(),
                    source,
                })
            });
        match opened {
            Ok(writer) => {
                tracing::info!(
                    dir = %dir.display(),
                    max_record_bytes = MAX_SLICE_RECORD_BYTES,
                    max_age_secs = DEFAULT_MAX_AGE.as_secs(),
                    queue = QUEUE_CAPACITY,
                    "finding spool enabled"
                );
                writer
            }
            Err(error) => {
                tracing::warn!(
                    reason = SpoolFailure::Unavailable.label(),
                    "finding spool could not be opened; findings are exported without record_digest"
                );
                tracing::debug!(error = %error, "finding spool open failure detail");
                Self::unavailable()
            }
        }
    }

    /// Startet den Schreibthread für einen bereits geöffneten Spool.
    ///
    /// # Arguments
    /// - `spool` (`FindingSpool`): geht an den Schreibthread über.
    /// - `capacity` (`usize`): Plätze der Warteschlange (mindestens 1).
    ///
    /// # Errors
    /// `io::Error`, wenn der Thread nicht startet.
    pub fn spawn(spool: FindingSpool, capacity: usize) -> io::Result<Self> {
        let (sender, receiver) = sync_channel(capacity.max(1));
        let stats = Arc::new(SpoolStats::default());
        bump(&stats.removed, count_u64(spool.open_report().removed));
        let max_record_bytes = spool.limits().max_record_bytes;
        let worker_stats = Arc::clone(&stats);
        let worker = std::thread::Builder::new()
            .name(WRITER_THREAD_NAME.to_owned())
            .spawn(move || run_worker(&spool, &receiver, &worker_stats))?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            stats,
            max_record_bytes,
        })
    }

    /// Ein Schreiber ohne Spool: jeder Record wird abgelehnt, die Zähler
    /// melden `unavailable`.
    #[must_use]
    pub fn unavailable() -> Self {
        let stats = Arc::new(SpoolStats::default());
        stats.note_failure(SpoolFailure::Unavailable);
        Self {
            sender: None,
            worker: None,
            stats,
            max_record_bytes: MAX_SLICE_RECORD_BYTES,
        }
    }

    /// Die geteilten Zähler (für `crate::health_export`).
    #[must_use]
    pub fn stats(&self) -> Arc<SpoolStats> {
        Arc::clone(&self.stats)
    }

    /// Obergrenze eines Records dieses Spools.
    #[must_use]
    pub const fn max_record_bytes(&self) -> u64 {
        self.max_record_bytes
    }

    /// Ob Records angenommen werden können.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.sender.is_some() && !self.stats.unavailable.load(Ordering::Relaxed)
    }

    /// Reiht `record` zum Schreiben ein, ohne zu blockieren.
    ///
    /// # Returns
    /// `true`, wenn der Record eingereiht wurde (sein Digest darf dann in
    /// die Exportzeile); `false` ohne Spool, bei voller Warteschlange
    /// (gezählt, `queue_overflow`) oder beendetem Schreibthread.
    #[must_use]
    pub fn submit(&self, record: FindingRecord) -> bool {
        let Some(sender) = self.sender.as_ref() else {
            return false;
        };
        match sender.try_send(SpoolMessage::Put(Box::new(record))) {
            Ok(()) => {
                bump(&self.stats.enqueued, 1);
                if self.stats.overflowing.swap(false, Ordering::Relaxed) {
                    tracing::info!("finding spool queue drains again");
                }
                true
            }
            Err(TrySendError::Full(_)) => {
                self.stats.note_failure(SpoolFailure::QueueOverflow);
                if self.stats.overflowing.swap(true, Ordering::Relaxed) {
                    tracing::debug!(
                        reason = SpoolFailure::QueueOverflow.label(),
                        "finding spool record dropped"
                    );
                } else {
                    tracing::warn!(
                        reason = SpoolFailure::QueueOverflow.label(),
                        capacity = QUEUE_CAPACITY,
                        "finding spool queue full; records are dropped and counted"
                    );
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                if !self.stats.unavailable.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        reason = SpoolFailure::Unavailable.label(),
                        "finding spool writer thread ended; records are no longer written"
                    );
                }
                self.stats.note_failure(SpoolFailure::Unavailable);
                false
            }
        }
    }

    /// Verbucht einen Record, der schon beim Erzeugen scheiterte (etwa
    /// `RecordTooLarge` beim Zuschneiden).
    pub fn note_rejected(&self, error: &SpoolError) {
        self.stats.note_failure(SpoolFailure::of_error(error));
    }

    /// Stößt die Aufbewahrung zum Zeitpunkt `now` an (je Zyklus).
    ///
    /// # Description
    /// Höchstens ein Anstoß liegt gleichzeitig in der Warteschlange; ist sie
    /// voll, folgt der nächste Versuch im nächsten Zyklus. Der Spool selbst
    /// sweept höchstens jede Minute.
    pub fn tick(&self, now: Timestamp) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        if self.stats.sweep_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if sender
            .try_send(SpoolMessage::Sweep(SystemTime::from(now)))
            .is_err()
        {
            self.stats.sweep_pending.store(false, Ordering::Release);
        }
    }

    /// Ein Schreiber ohne Thread, dessen Warteschlange der Test selbst
    /// leert.
    #[cfg(test)]
    fn detached(capacity: usize) -> (Self, Receiver<SpoolMessage>) {
        let (sender, receiver) = sync_channel(capacity.max(1));
        let writer = Self {
            sender: Some(sender),
            worker: None,
            stats: Arc::new(SpoolStats::default()),
            max_record_bytes: MAX_SLICE_RECORD_BYTES,
        };
        (writer, receiver)
    }
}

impl Drop for SpoolWriter {
    fn drop(&mut self) {
        // Erst die Warteschlange schließen: der Schreibthread schreibt alles
        // Eingereihte und endet dann.
        drop(self.sender.take());
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!(
                reason = SpoolFailure::Unavailable.label(),
                "finding spool writer thread panicked; queued records may be lost"
            );
        }
    }
}

// Schleife des Schreibthreads: endet, wenn alle Sender fort sind und die
// Warteschlange leer ist.
fn run_worker(spool: &FindingSpool, receiver: &Receiver<SpoolMessage>, stats: &SpoolStats) {
    let mut failing = false;
    while let Ok(message) = receiver.recv() {
        match message {
            SpoolMessage::Put(record) => {
                let outcome = spool.put(&record);
                failing = stats.record_put(&outcome, failing);
            }
            SpoolMessage::Sweep(now) => {
                stats.sweep_pending.store(false, Ordering::Release);
                stats.record_sweep(&spool.maybe_sweep(None, now));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::NetworkScope;
    use harw_dod_rules::rule::RuleContext;
    use harw_dod_rules::run_rules_checked;
    use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent, SecurityEvidence};
    use harw_types::SensorId;
    use std::time::Duration;

    /// `n` unterscheidbare Records aus echten Regelbefunden.
    fn records(n: usize) -> TestResult<Vec<FindingRecord>> {
        let events: Vec<SecurityEvent> = (0..n)
            .map(|i| SecurityEvent {
                sensor: SensorId::from_str("workspace-drift-0"),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::StructureDrift {
                    severity: DriftSeverity::Medium,
                    detail: format!("drift #{i}"),
                },
            })
            .collect();
        let evidence = SecurityEvidence::capture(Vec::new(), events, Timestamp::UNIX_EPOCH)
            .map_err(ctx("evidence encodes"))?;
        let scope = NetworkScope::empty();
        let ctx_full = RuleContext {
            now: Timestamp::UNIX_EPOCH,
            samples: &evidence.samples,
            events: &evidence.events,
            baselines: &[],
            network_scope: &scope,
            listener_history: None,
            rule_config: None,
        };
        let records: Vec<FindingRecord> = run_rules_checked(&ctx_full)
            .iter()
            .map(|finding| finding.record(evidence.clone()))
            .collect();
        if records.len() == n {
            Ok(records)
        } else {
            Err(TestError::Unexpected(format!(
                "{} findings for {n} drift events",
                records.len()
            )))
        }
    }

    fn open_spool(dir: &Path, limits: SpoolLimits) -> TestResult<FindingSpool> {
        prepare_spool_dir(dir).map_err(ctx("prepare spool dir"))?;
        FindingSpool::open_with_limits(dir, limits).map_err(ctx("open spool"))
    }

    fn page_len(dir: &Path) -> TestResult<usize> {
        let spool = FindingSpool::open(dir).map_err(ctx("reopen spool"))?;
        let (entries, _) = spool.page(None, 100).map_err(ctx("page"))?;
        Ok(entries.len())
    }

    #[test]
    fn test_sentinel_limits_bound_records_age_and_size() {
        let limits = sentinel_limits();
        assert_eq!(limits.max_record_bytes, 64 * 1024);
        assert_eq!(limits.max_age, Some(Duration::from_secs(14 * 86_400)));
        assert_eq!(limits.max_total_bytes, Some(MAX_SPOOL_BYTES));
        assert!(limits.retention_apply);
        assert!(
            limits.delete_unconsumed,
            "no consumer cursor in the sentinel"
        );
    }

    #[test]
    fn test_prepare_creates_an_owner_only_directory() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        prepare_spool_dir(&dir).map_err(ctx("prepare"))?;
        let mode = fs::metadata(&dir)
            .map_err(ctx("stat"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & !SPOOL_DIR_MODE, 0, "mode {mode:o} is owner-only");
        Ok(())
    }

    #[test]
    fn test_prepare_narrows_a_group_readable_directory() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        fs::create_dir(&dir).map_err(ctx("mkdir"))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).map_err(ctx("chmod"))?;
        prepare_spool_dir(&dir).map_err(ctx("prepare"))?;
        let mode = fs::metadata(&dir)
            .map_err(ctx("stat"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        Ok(())
    }

    #[test]
    fn test_prepare_refuses_a_symlink() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = root.path().join("real");
        fs::create_dir(&real).map_err(ctx("mkdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        std::os::unix::fs::symlink(&real, &dir).map_err(ctx("symlink"))?;
        assert!(matches!(
            prepare_spool_dir(&dir),
            Err(SpoolError::SymlinkRejected { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_open_under_a_file_home_is_unavailable_not_fatal() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = root.path().join("not-a-dir");
        fs::write(&home, b"").map_err(ctx("seed"))?;
        let writer = SpoolWriter::open(&home);
        assert!(!writer.is_available());
        let counters = writer.stats().snapshot();
        assert!(counters.unavailable);
        assert_eq!(counters.last_failure, Some(SpoolFailure::Unavailable));
        let record = records(1)?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("record"))?;
        assert!(!writer.submit(record), "no spool, no record");
        Ok(())
    }

    #[test]
    fn test_writer_thread_writes_every_queued_record_before_drop_returns() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        let writer = SpoolWriter::spawn(open_spool(&dir, sentinel_limits())?, QUEUE_CAPACITY)
            .map_err(ctx("spawn"))?;
        let stats = writer.stats();
        for record in records(3)? {
            assert!(writer.submit(record));
        }
        drop(writer);
        let counters = stats.snapshot();
        assert_eq!(counters.enqueued_total, 3);
        assert_eq!(counters.written_total, 3);
        assert_eq!(counters.rejected_total(), 0);
        assert!(!counters.last_put_failed);
        assert_eq!(page_len(&dir)?, 3);
        Ok(())
    }

    /// PL-100 WP0.12: eine volle Warteschlange verwirft, zählt und meldet
    /// den Grund — der Sammelthread blockiert nie.
    #[test]
    fn test_full_queue_drops_and_counts_the_record() -> TestResult {
        let (writer, receiver) = SpoolWriter::detached(1);
        let mut batch = records(2)?.into_iter();
        let first = batch.next().ok_or(TestError::Missing("record"))?;
        let second = batch.next().ok_or(TestError::Missing("record"))?;
        assert!(writer.submit(first));
        assert!(!writer.submit(second), "the queue holds one record");
        let counters = writer.stats().snapshot();
        assert_eq!(counters.enqueued_total, 1);
        assert_eq!(counters.dropped_total, 1);
        assert_eq!(counters.last_failure, Some(SpoolFailure::QueueOverflow));
        assert!(matches!(receiver.try_recv(), Ok(SpoolMessage::Put(_))));
        Ok(())
    }

    #[test]
    fn test_spool_full_is_counted_as_a_failed_put() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        let limits = SpoolLimits {
            max_entries: 1,
            ..sentinel_limits()
        };
        let writer =
            SpoolWriter::spawn(open_spool(&dir, limits)?, QUEUE_CAPACITY).map_err(ctx("spawn"))?;
        let stats = writer.stats();
        for record in records(2)? {
            assert!(writer.submit(record), "queued; the spool decides");
        }
        drop(writer);
        let counters = stats.snapshot();
        assert_eq!(counters.written_total, 1);
        assert_eq!(counters.spool_full_total, 1);
        assert_eq!(counters.rejected_total(), 1);
        assert!(counters.last_put_failed);
        assert_eq!(counters.last_failure, Some(SpoolFailure::SpoolFull));
        Ok(())
    }

    #[test]
    fn test_record_too_large_is_counted() {
        let (writer, _receiver) = SpoolWriter::detached(1);
        writer.note_rejected(&SpoolError::RecordTooLarge {
            len: 70_000,
            limit: MAX_SLICE_RECORD_BYTES,
        });
        let counters = writer.stats().snapshot();
        assert_eq!(counters.too_large_total, 1);
        assert_eq!(counters.last_failure, Some(SpoolFailure::RecordTooLarge));
    }

    #[test]
    fn test_tick_queues_at_most_one_sweep() {
        let (writer, receiver) = SpoolWriter::detached(4);
        writer.tick(Timestamp::UNIX_EPOCH);
        writer.tick(Timestamp::UNIX_EPOCH);
        assert!(matches!(receiver.try_recv(), Ok(SpoolMessage::Sweep(_))));
        assert!(receiver.try_recv().is_err(), "the second tick waits");
    }

    /// Die Aufbewahrung löscht auf dem Schreibthread Records, die älter als
    /// 14 Tage sind, auch unkonsumierte.
    #[test]
    fn test_tick_applies_the_fourteen_day_retention() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = root.path().join(SPOOL_DIR_NAME);
        let spool = open_spool(&dir, sentinel_limits())?;
        let old = SystemTime::now()
            .checked_sub(DEFAULT_MAX_AGE)
            .and_then(|edge| edge.checked_sub(Duration::from_secs(3_600)))
            .ok_or(TestError::Missing("time arithmetic"))?;
        for record in records(2)? {
            let id = spool.put(&record).map_err(ctx("put"))?;
            let path = dir.join(format!("{id}.json"));
            fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_modified(old))
                .map_err(ctx("age the record"))?;
        }
        let writer = SpoolWriter::spawn(spool, QUEUE_CAPACITY).map_err(ctx("spawn"))?;
        let stats = writer.stats();
        // Nach dem Sweep beim Öffnen ist der nächste erst eine Minute später dran.
        let later = SystemTime::now()
            .checked_add(Duration::from_secs(120))
            .ok_or(TestError::Missing("time arithmetic"))?;
        writer.tick(Timestamp::try_from(later).map_err(ctx("timestamp"))?);
        drop(writer);
        assert_eq!(stats.snapshot().removed_total, 2);
        assert_eq!(page_len(&dir)?, 0);
        Ok(())
    }

    #[test]
    fn test_failure_labels_are_stable_and_round_trip() {
        for (failure, label) in [
            (SpoolFailure::Unavailable, "unavailable"),
            (SpoolFailure::QueueOverflow, "queue_overflow"),
            (SpoolFailure::SpoolFull, "spool_full"),
            (SpoolFailure::RecordTooLarge, "record_too_large"),
            (SpoolFailure::WriteFailed, "write_failed"),
        ] {
            assert_eq!(failure.label(), label);
            assert_eq!(SpoolFailure::from_code(failure.code()), Some(failure));
        }
        assert_eq!(SpoolFailure::from_code(0), None);
    }
}

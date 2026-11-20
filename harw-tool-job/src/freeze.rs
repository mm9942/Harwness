//! Einfrieren und Auftauen der eigenen Job-cgroup für `job.suspend` /
//! `job.resume` (PL-100 WP 3.5).
//!
//! # Verantwortungsbereich
//! - [`JobFreezer`]: plattformneutrale Schnittstelle, über die der
//!   [`crate::JobManager`] die eigene cgroup eines Jobs einfriert bzw.
//!   auftaut. Sie bekommt nur den Blattnamen (`harw-job-<attempt>`) direkt
//!   unter ihrer delegierten Wurzel, nie einen Pfad.
//! - `CgroupFreezer` (nur Linux): setzt das über
//!   `harw_job_linux::cgroup::CgroupBackend::{freeze, thaw}` um
//!   (`cgroup.freeze` = 1 bzw. 0) und weigert sich, die cgroup einzufrieren,
//!   in der harw selbst läuft (oder die harw enthält).
//! - [`ContainmentHolds`]: Index der Eindämmungen, die ein Eigentümer
//!   außerhalb von harw (der Warden) auf cgroups hält, nach Kernel-ID;
//!   `MarkerDirHolds` (nur Linux) liest ihn aus Markerdateien unter
//!   [`CONTAINMENT_HOLDS_DIR`]; [`ContainmentLease`] ist die geteilte
//!   Sperre, unter der harw prüft und auftaut.
//! - [`FreezeError`]: typisierte, plattformneutrale Fehler.
//!
//! # Zielwahl
//! Das Ziel kommt nie aus Argumenten, auch nicht aus einer `finding_id`.
//! Der Manager nimmt ausschließlich den bewiesenen Pfad
//! [`crate::JobMeta::job_cgroup_path`] eines Jobs, den der Aufrufer steuern
//! darf (dieselbe Besitzprüfung wie `job.stop`), und reicht dessen
//! Blattnamen nur weiter, wenn der Elternpfad genau die delegierte Wurzel
//! des Freezers ist. Ohne Beweis oder bei einer anderen Wurzel wird nichts
//! eingefroren (fail closed).
//!
//! # Fremde Freezes
//! [`JobFreezer::freeze_requested`] (eigenes `cgroup.freeze`, sofort
//! sichtbar) und [`JobFreezer::is_frozen`] (Kernel-Sicht, auch über eine
//! Vorfahren-cgroup) zeigen dem Manager einen Freeze, den harw nicht
//! vermerkt hat (Operator, Warden). Den übernimmt harw nicht und hebt ihn
//! nie auf.
//!
//! # Eindämmung nach harws Anhalten
//! cgroup v2 hat je cgroup genau ein `cgroup.freeze`-Bit. Friert der Warden
//! die Job-cgroup ein, *nachdem* harw sie angehalten hat, teilen sich beide
//! dieses Bit; ein `job.resume` würde mit `cgroup.freeze` = 0 auch die
//! Eindämmung des Wardens aufheben. Getrennte Hierarchieebenen gehen nicht
//! ohne Umbau der Prozessplatzierung (die Job-Prozesse liegen direkt im
//! Blatt `harw-job-<attempt>`, und der Warden adressiert jede cgroup per
//! Pfad). Deshalb gilt koordiniertes Eigentum:
//! - Der Warden versucht einmal ohne Warten `flock(LOCK_EX|LOCK_NB)` auf
//!   das Indexverzeichnis, legt einen Marker
//!   `<CONTAINMENT_HOLDS_DIR>/<Kernel-ID>` an, merkt sich, ob
//!   `cgroup.freeze` schon `1` war, friert ein und gibt die Sperre frei;
//!   den Marker entfernt er erst nach seiner Freigabe (Vertrag bei
//!   [`CONTAINMENT_HOLDS_DIR`]).
//! - [`JobFreezer::containment_held`] liest diesen Index über die Kernel-ID
//!   der Job-cgroup (Inode ihres Verzeichnisses). Vor jedem Auftauen nimmt
//!   der Manager nach bestem Bemühen `flock(LOCK_SH)` auf dasselbe
//!   Verzeichnis ([`JobFreezer::containment_lease`], [`ContainmentLease`]),
//!   prüft, taut auf, prüft erneut und gibt die Sperre erst danach frei.
//!   Ein Warden, der das Protokoll einhält und die Sperre bekommt, kann
//!   dazwischen keine Eindämmung anlegen. Taucht der Marker trotzdem auf
//!   (ein Warden, der ohne Sperre einfriert), friert der Manager sofort
//!   wieder ein. Die Sperre ist nur eine Verengung des Fensters: jeder
//!   lokale Prozess kann das Verzeichnis sperren, deshalb wartet keine
//!   Seite länger darauf (sonst könnte jeder Nutzer `job.resume` bzw. die
//!   Eindämmung blockieren); ohne Sperre gilt allein die erneute Prüfung.
//!   Ein unlesbarer oder nicht vertrauenswürdiger Index heißt: nicht
//!   auftauen (fail closed); `job.suspend` friert dann trotzdem ein (die
//!   sichere Richtung).
//! - Fehlt das Laufzeitverzeichnis des Wardens (`/run/harw-warden`), gilt
//!   das nur dann als „kein Warden“, wenn auch keine Spur eines
//!   installierten Wardens existiert ([`CONTAINMENT_OWNER_EVIDENCE`]) und
//!   die Montage ihn nicht ausdrücklich erwartet
//!   (`MarkerDirHolds::owner_expected`); sonst fail closed.
//! - Ein Freeze in einer Vorfahren-cgroup wirkt unabhängig vom Bit des
//!   Blatts weiter; den kann harws Auftauen ohnehin nicht aufheben.
//!
//! # Produktion
//! Die Montage öffnet einen `CgroupFreezer` über dieselbe delegierte
//! Wurzel, unter der die Job-Runtime die Job-cgroups anlegt
//! (`[jobs] cgroup_root`), und übergibt ihn mit
//! [`crate::JobManager::set_freezer`]. Ohne Freezer lehnt `job.suspend`
//! typisiert ab. `CgroupFreezer::new`/`open` lesen den Eindämmungsindex des
//! Wardens unter [`CONTAINMENT_HOLDS_DIR`]; `CgroupFreezer::with_holds`
//! nimmt einen anderen [`ContainmentHolds`] (etwa
//! `MarkerDirHolds::default().owner_expected()`, wenn die
//! DoD-Konfiguration den Warden einschaltet).
//!
//! # Nebenläufigkeit
//! `Send + Sync`; jeder Aufruf ist ein kurzer, synchroner Zugriff auf eine
//! cgroupfs-Datei bzw. ein `lstat` im Eindämmungsindex. Nur
//! [`JobFreezer::containment_lease`] kann kurz blockierend warten
//! (höchstens etwa 100 ms, solange ein anderer Prozess die Sperre
//! exklusiv hält); asynchrone Aufrufer rufen den Manager deshalb über
//! `spawn_blocking` auf. Einfrieren ist im Kernel asynchron;
//! [`JobFreezer::is_frozen`] meldet den Stand.
//!
//! # Examples
//! ```rust,no_run
//! # #[cfg(target_os = "linux")]
//! # fn main() -> Result<(), harw_tool_job::FreezeError> {
//! use harw_tool_job::{CgroupFreezer, JobFreezer};
//!
//! let freezer = CgroupFreezer::open(std::path::Path::new("/sys/fs/cgroup/harw.slice"))?;
//! assert_eq!(freezer.delegated_root(), "/harw.slice");
//! # Ok(())
//! # }
//! # #[cfg(not(target_os = "linux"))]
//! # fn main() {}
//! ```

#[cfg(target_os = "linux")]
use harw_job_linux::CgroupError;
#[cfg(target_os = "linux")]
use harw_job_linux::cgroup::{CgroupBackend, CgroupV2Fs};
#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt as _;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;

/// Default containment index of the Warden: one marker file per cgroup the
/// Warden holds frozen, named by the cgroup's kernel id in decimal (the
/// inode of its cgroup-v2 directory), e.g.
/// `/run/harw-warden/holds/4294967298`.
///
/// # Description
/// The contract with the Warden:
/// - Containing: it tries `flock(LOCK_EX | LOCK_NB)` on this directory
///   exactly once, without waiting, and contains with or without the lock.
///   Under it (or without it) it creates the marker (a regular file, never
///   a symbolic link) *first*, then reads `cgroup.freeze` and records in
///   its ledger whether it already read `1`, then writes `cgroup.freeze` =
///   1, and only then releases the lock. harw holds `flock(LOCK_SH)` on the
///   same directory from its check until after its thaw when it gets it
///   ([`ContainmentLease`]). Any local process can open and lock the
///   directory, so the lock only narrows the race on both sides and
///   neither side waits long for it; harw's re-check after the thaw (the
///   marker comes before the freeze) is what holds without it.
/// - Releasing: if the bit already read `1` at containment (harw's
///   suspension or an operator's freeze came first), it removes only its
///   marker and leaves `cgroup.freeze` as it is; the earlier owner lifts
///   its own freeze (harw: `job.resume`). Otherwise it writes
///   `cgroup.freeze` = 0 first and removes the marker only *after* that.
///   So neither side lifts the other's freeze, in either order. Residual
///   race: if harw's freeze lands between the Warden's read of the bit and
///   its own freeze (after harw's check found no marker yet), the Warden
///   records `0` and its release lifts harw's suspension too; harw's record
///   then says "suspended" while the job runs (the safe side for
///   containment: `job.stop` kills instead of signalling).
/// - The directory belongs to the Warden (tmpfiles: `/run/harw-warden`
///   root-owned `0755`, `holds` owned by the Warden user, `0755`), so no
///   one else can add or remove markers. The marker's content is never
///   read. A stale marker (the Warden died between marker and release)
///   keeps `job.resume` refused until the Warden reconciles its markers at
///   startup or an operator clears it.
pub const CONTAINMENT_HOLDS_DIR: &str = "/run/harw-warden/holds";

/// Paths that show a containment owner (the Warden) is installed on this
/// host: its activation socket and its opt-in file (default `SYSCONFDIR`
/// of `harw-dod`).
///
/// # Description
/// `MarkerDirHolds` reads a missing runtime directory of the Warden
/// (`/run/harw-warden`) as "no Warden on this host, nothing held" only
/// while none of these paths exists. If one exists, or its absence cannot
/// be proven (e.g. no permission to look), the Warden is installed and the
/// missing index fails closed: half-deployed hosts (tmpfiles entry not yet
/// installed, a private `/run`) never let `job.resume` lift a containment.
pub const CONTAINMENT_OWNER_EVIDENCE: [&str; 2] =
    ["/run/harw/warden.sock", "/etc/harw-dod/warden.enable"];

/// How often `MarkerDirHolds` tries to take the shared lock of its index
/// before it hands out a contended lease ([`ContainmentLease::contended`]),
/// and the pause between two tries (about 100 ms in all; the Warden holds
/// its exclusive lock only for one marker and one freeze).
#[cfg(target_os = "linux")]
const LEASE_ATTEMPTS: u32 = 20;
/// Pause between two tries of [`LEASE_ATTEMPTS`].
#[cfg(target_os = "linux")]
const LEASE_RETRY: Duration = Duration::from_millis(5);

/// Why freezing or thawing a job cgroup failed.
///
/// # Description
/// Platform-neutral classes of `harw_job_linux::CgroupError`, without the
/// absolute path of the delegated root; the remaining text names at most a
/// cgroup beneath that root.
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::HarwError)]
pub enum FreezeError {
    /// The kernel or the backend lacks the interface (e.g. no cgroup v2
    /// freezer before Linux 5.2, or no Linux at all).
    #[msg("the cgroup freezer is not supported here ({feature})")]
    Unsupported {
        /// The missing interface.
        feature: &'static str,
    },
    /// The delegated cgroup root is not usable (not cgroup v2, missing or
    /// not delegated to this process).
    #[msg("the delegated cgroup root is not usable")]
    Unavailable,
    /// Permission denied on the job cgroup.
    #[msg("permission denied on the job cgroup")]
    PermissionDenied,
    /// The job cgroup does not exist (any more).
    #[msg("the job cgroup does not exist (any more)")]
    Gone,
    /// The leaf is not a single safe cgroup name.
    #[msg("invalid job cgroup name")]
    InvalidName,
    /// The target is the cgroup harw itself runs in, or one of its
    /// ancestors: freezing it would freeze harw.
    #[msg("refusing to freeze the cgroup harw itself runs in")]
    OwnCgroup,
    /// The containment index of the Warden ([`ContainmentHolds`]) cannot be
    /// read or is not trustworthy; whether a containment outside harw holds
    /// the cgroup is unknown, so `job.resume` thaws nothing (fail closed).
    #[msg("the containment index cannot be trusted ({reason})")]
    ContainmentIndexUnreadable {
        /// What is wrong with the index (fixed text, no path or id).
        reason: &'static str,
    },
    /// Any other failure (rendered cgroup error).
    #[msg("cgroup operation failed: {reason}")]
    Failed {
        /// What failed.
        reason: String,
    },
}

/// Freezes and thaws a job's own cgroup (`job.suspend` / `job.resume`).
///
/// # Description
/// Every method takes only the leaf name of a cgroup directly beneath
/// [`JobFreezer::delegated_root`]; the [`crate::JobManager`] derives it from
/// the job's proven cgroup ([`crate::JobMeta::job_cgroup_path`]) after the
/// ownership check. Implementations must never resolve anything outside
/// that root.
///
/// # Concurrency
/// `Send + Sync`; calls are short and synchronous.
pub trait JobFreezer: Send + Sync {
    /// The delegated root, hierarchy-relative as in `/proc/<pid>/cgroup`
    /// (e.g. `/harw.slice`). Only its direct children are ever touched.
    fn delegated_root(&self) -> &str;

    /// Freezes every process of the cgroup `leaf` (`cgroup.freeze` = 1).
    ///
    /// # Errors
    /// [`FreezeError`] when the cgroup is missing, not writable, not
    /// supported, or harw's own cgroup.
    fn freeze(&self, leaf: &str) -> Result<(), FreezeError>;

    /// Thaws the cgroup `leaf` (`cgroup.freeze` = 0).
    ///
    /// # Errors
    /// [`FreezeError`] when the cgroup is missing, not writable or not
    /// supported.
    fn thaw(&self, leaf: &str) -> Result<(), FreezeError>;

    /// The kernel view: whether every process of `leaf` has stopped
    /// (`cgroup.events` `frozen`); freezing is asynchronous. Also `true`
    /// while an ancestor of `leaf` is frozen.
    ///
    /// # Errors
    /// [`FreezeError::Unsupported`] without that probe, otherwise as
    /// [`JobFreezer::freeze`].
    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError>;

    /// Whether a freeze of `leaf` itself is requested (`cgroup.freeze`
    /// reads `1`), by harw or anyone else; `true` at once, before every
    /// process has stopped.
    ///
    /// # Description
    /// Together with [`JobFreezer::is_frozen`] this tells the
    /// [`crate::JobManager`] whether a freeze it did not record is in place
    /// (an operator, the Warden), which it then neither takes over nor
    /// lifts.
    ///
    /// # Errors
    /// [`FreezeError::Unsupported`] without that probe, otherwise as
    /// [`JobFreezer::freeze`].
    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError>;

    /// Whether a containment owner outside harw (the Warden) holds the
    /// cgroup `leaf`; its own `cgroup.freeze` then belongs (also) to that
    /// owner and harw must not thaw it.
    ///
    /// # Description
    /// cgroup v2 has one freeze bit per cgroup: harw's suspension and a
    /// Warden containment of the same cgroup share it, and the bit alone
    /// cannot tell them apart. The Warden therefore records every
    /// containment in an index keyed by the cgroup's kernel id before it
    /// freezes ([`ContainmentHolds`], [`CONTAINMENT_HOLDS_DIR`]). The
    /// [`crate::JobManager`] asks before every thaw of its own suspension
    /// and again right after it, both under
    /// [`JobFreezer::containment_lease`]; `job.suspend` asks before it
    /// freezes, so harw never takes over a bit the Warden is about to set.
    ///
    /// # Errors
    /// [`FreezeError::ContainmentIndexUnreadable`] when the index cannot be
    /// read or trusted, [`FreezeError::Unsupported`] without a kernel id,
    /// otherwise as [`JobFreezer::freeze`]. Before a thaw callers treat
    /// every error as "possibly held" and do not thaw; `job.suspend`
    /// freezes anyway (the safe direction).
    fn containment_held(&self, leaf: &str) -> Result<bool, FreezeError>;

    /// Takes the shared lease of the containment index for one
    /// check-thaw-recheck sequence ([`ContainmentLease`],
    /// [`ContainmentHolds::lease`]).
    ///
    /// # Description
    /// While a locked lease lives, a containment owner that honours the
    /// lock protocol of [`CONTAINMENT_HOLDS_DIR`] adds no containment, so a
    /// check before a thaw stays true until the thaw is done. The lock is
    /// best effort: if another process holds it for the whole retry budget,
    /// the lease is contended ([`ContainmentLease::is_contended`]) and the
    /// caller relies on its re-check after the thaw alone. Wrappers must
    /// delegate (there is deliberately no provided implementation).
    ///
    /// # Errors
    /// [`FreezeError::ContainmentIndexUnreadable`] when the index cannot be
    /// trusted or opened; callers then do not thaw.
    fn containment_lease(&self) -> Result<ContainmentLease, FreezeError>;
}

/// Shared lease on a containment index for one check-thaw-recheck sequence
/// of the [`crate::JobManager`] (PL-100 WP 3.5).
///
/// # Description
/// While it lives, `MarkerDirHolds` holds `flock(LOCK_SH)` on its index
/// directory; the containment owner tries `flock(LOCK_EX)` on the same
/// directory around writing its marker and freezing
/// ([`CONTAINMENT_HOLDS_DIR`]). Dropping the lease closes the descriptor
/// and so releases the lock. An index without locking, or a host without a
/// containment owner, hands out an unlocked lease
/// ([`ContainmentLease::unlocked`]); an index whose lock someone else
/// holds for the whole retry budget hands out a contended one
/// ([`ContainmentLease::contended`]).
///
/// # Concurrency
/// `Send + Sync`; hold it only for the few cgroupfs writes of one sequence.
#[derive(Debug, Default)]
#[must_use = "dropping the lease releases the lock of the containment index at once"]
pub struct ContainmentLease {
    // The index directory, locked shared; `None`: nothing is locked.
    lock: Option<std::fs::File>,
    // The index has a lock protocol, but its lock was not obtained.
    contended: bool,
}

impl ContainmentLease {
    /// A lease that locks nothing (an index without locking, or no
    /// containment owner on this host).
    pub fn unlocked() -> Self {
        Self::default()
    }

    /// A lease that locks nothing because the index's lock was not
    /// obtained: another process held it for the whole retry budget, or
    /// locking failed.
    ///
    /// # Description
    /// The lock is best effort. Any local process can open and lock the
    /// index directory, so refusing without it would let any user block
    /// every `job.resume` on the host. The caller goes on with check, thaw
    /// and re-check (and refreezes when the re-check finds a marker), the
    /// same guarantee it has against a containment owner that contains
    /// without the lock.
    pub fn contended() -> Self {
        Self {
            lock: None,
            contended: true,
        }
    }

    /// Whether the lease holds the shared lock of an index directory.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Whether the index has a lock that this lease could not obtain
    /// ([`ContainmentLease::contended`]).
    #[must_use]
    pub fn is_contended(&self) -> bool {
        self.contended
    }
}

/// Index of the containments an owner outside harw (the Warden) holds on
/// cgroups, keyed by the cgroup's kernel id (PL-100 WP 3.5).
///
/// # Description
/// The kernel id names a cgroup independently of paths and namespaces; on
/// cgroup v2 it is the inode of the cgroup directory, the id
/// `bpf_get_current_cgroup_id()` and the DoD probes report
/// (`harw_job_linux::cgroup::CgroupBackend::kernel_id`). The Warden
/// addresses cgroups by path; for its marker it derives the id as the
/// `st_ino` of `<cgroup root>/<cgroup>`.
///
/// # Concurrency
/// `Send + Sync`; a call is a short, synchronous lookup.
pub trait ContainmentHolds: Send + Sync + std::fmt::Debug {
    /// Whether a containment owner outside harw holds the cgroup with
    /// kernel id `kernel_id`.
    ///
    /// # Errors
    /// [`FreezeError::ContainmentIndexUnreadable`] when the index cannot be
    /// read or is not trustworthy; callers then fail closed (no thaw).
    fn is_held(&self, kernel_id: u64) -> Result<bool, FreezeError>;

    /// The shared lease of this index ([`ContainmentLease`]); the provided
    /// implementation locks nothing (an index without a lock protocol). A
    /// lock that is not obtained in time is no error but a contended lease
    /// ([`ContainmentLease::contended`]).
    ///
    /// # Errors
    /// [`FreezeError::ContainmentIndexUnreadable`] when the index cannot be
    /// trusted or opened.
    fn lease(&self) -> Result<ContainmentLease, FreezeError> {
        Ok(ContainmentLease::unlocked())
    }
}

/// Mode bits that let group or others write a directory, i.e. add or remove
/// containment markers.
#[cfg(target_os = "linux")]
const GROUP_OR_OTHER_WRITE: u32 = 0o022;

/// [`ContainmentHolds`] over the Warden's marker directory (Linux only;
/// default [`CONTAINMENT_HOLDS_DIR`]).
///
/// # Description
/// A cgroup is held when a marker `<dir>/<kernel id>` exists. Nothing is
/// ever followed or read: the runtime directory (the parent of `dir`), the
/// index `dir` and the marker are inspected with `lstat` only.
/// - Runtime directory missing: nothing is held only while no containment
///   owner is installed ([`CONTAINMENT_OWNER_EVIDENCE`], replaceable with
///   [`MarkerDirHolds::with_owner_evidence`]) and none is expected
///   ([`MarkerDirHolds::owner_expected`]); otherwise fail closed.
/// - The runtime directory must be a real directory owned by root and not
///   writable by group or others; `dir` must exist, be a real directory
///   not writable by group or others, and must not belong to this
///   process's own (non-root) user: a job of harw runs under that user and
///   could otherwise delete markers. Any other answer: the index cannot be
///   trusted ([`FreezeError::ContainmentIndexUnreadable`], fail closed).
/// - No marker: not held. A regular file owned by root or by the owner of
///   `dir`: held. Anything else (a symbolic link, a directory, a foreign
///   owner, an unreadable entry): untrusted, fail closed.
/// - [`ContainmentHolds::lease`] opens `dir` (and checks it is the
///   directory just inspected; an untrusted or replaced index fails
///   closed) and takes `flock(LOCK_SH)` on it, retrying for about 100 ms
///   while someone holds the exclusive lock. After that, or when locking
///   fails, it hands out a contended lease instead of failing: `dir` is
///   world-readable, so any local user could hold that lock forever.
///
/// # Examples
/// ```rust,no_run
/// # #[cfg(target_os = "linux")]
/// # fn main() -> Result<(), harw_tool_job::FreezeError> {
/// use harw_tool_job::{ContainmentHolds, MarkerDirHolds};
///
/// // The DoD configuration enables the Warden: a missing index never means
/// // "nothing held".
/// let holds = MarkerDirHolds::default().owner_expected();
/// let _lease = holds.lease()?;
/// if holds.is_held(4_294_967_298)? {
///     // The Warden holds this cgroup: never thaw it.
/// }
/// # Ok(())
/// # }
/// # #[cfg(not(target_os = "linux"))]
/// # fn main() {}
/// ```
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerDirHolds {
    dir: PathBuf,
    // What a missing runtime directory of the containment owner means.
    owner: OwnerPresence,
    // The uid that must own the runtime directory (the parent of `dir`):
    // root in production.
    runtime_owner: u32,
    // Whether an index owned by this process's own non-root uid is refused
    // (production: always).
    refuse_own_index: bool,
}

/// What a missing runtime directory of the containment owner means for
/// [`MarkerDirHolds`].
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnerPresence {
    /// Nothing is held unless one of these paths exists or cannot be ruled
    /// out: then the owner is installed and the missing index fails closed.
    Detect(Vec<PathBuf>),
    /// The owner is configured on this host: a missing index always fails
    /// closed.
    Expected,
}

#[cfg(target_os = "linux")]
impl MarkerDirHolds {
    /// Reads the marker directory `dir` (absolute; production:
    /// [`CONTAINMENT_HOLDS_DIR`]). A missing runtime directory of the
    /// Warden fails closed as soon as one of
    /// [`CONTAINMENT_OWNER_EVIDENCE`] exists.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            owner: OwnerPresence::Detect(
                CONTAINMENT_OWNER_EVIDENCE
                    .into_iter()
                    .map(PathBuf::from)
                    .collect(),
            ),
            runtime_owner: 0,
            refuse_own_index: true,
        }
    }

    /// The containment owner is configured on this host (e.g. the DoD
    /// configuration enables the Warden): a missing index always fails
    /// closed, no evidence is consulted.
    #[must_use]
    pub fn owner_expected(mut self) -> Self {
        self.owner = OwnerPresence::Expected;
        self
    }

    /// Detects an installed containment owner by `paths` instead of
    /// [`CONTAINMENT_OWNER_EVIDENCE`] (e.g. another `SYSCONFDIR`); undoes
    /// [`MarkerDirHolds::owner_expected`].
    ///
    /// # Arguments
    /// - `paths`: absolute paths; one that exists, or whose absence cannot
    ///   be proven, means the owner is installed.
    #[must_use]
    pub fn with_owner_evidence<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.owner = OwnerPresence::Detect(paths.into_iter().map(Into::into).collect());
        self
    }

    /// The marker directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Test seam: the runtime directory may belong to `uid` instead of
    /// root (temporary directories belong to the test user).
    #[cfg(test)]
    pub(crate) fn trusting_runtime_owner(mut self, uid: u32) -> Self {
        self.runtime_owner = uid;
        self
    }

    /// Test seam: an index owned by this process's own user is accepted
    /// (temporary directories belong to the test user).
    #[cfg(test)]
    pub(crate) fn allowing_own_index(mut self) -> Self {
        self.refuse_own_index = false;
        self
    }

    /// Whether a containment owner is (or may be) installed on this host.
    fn owner_installed(&self) -> bool {
        match &self.owner {
            OwnerPresence::Expected => true,
            OwnerPresence::Detect(evidence) => evidence.iter().any(|path| {
                !matches!(
                    fs::symlink_metadata(path),
                    Err(error) if error.kind() == io::ErrorKind::NotFound
                )
            }),
        }
    }

    /// The checked metadata of the index directory; `None` when no
    /// containment owner is installed on this host (nothing is held).
    ///
    /// # Errors
    /// [`FreezeError::ContainmentIndexUnreadable`] for every answer the
    /// type documentation does not accept.
    fn trusted_index(&self) -> Result<Option<fs::Metadata>, FreezeError> {
        let runtime_dir = self
            .dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| untrusted("the index directory has no parent"))?;
        let runtime = match fs::symlink_metadata(runtime_dir) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return if self.owner_installed() {
                    Err(untrusted(
                        "the runtime directory of an installed containment owner is missing",
                    ))
                } else {
                    Ok(None)
                };
            }
            Err(_) => return Err(untrusted("the runtime directory cannot be read")),
        };
        if !runtime.file_type().is_dir() {
            return Err(untrusted("the runtime directory is not a directory"));
        }
        if runtime.uid() != self.runtime_owner {
            return Err(untrusted("the runtime directory has a foreign owner"));
        }
        if runtime.mode() & GROUP_OR_OTHER_WRITE != 0 {
            return Err(untrusted(
                "the runtime directory is writable by group or others",
            ));
        }
        let index = match fs::symlink_metadata(&self.dir) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(untrusted("the index directory is missing"));
            }
            Err(_) => return Err(untrusted("the index directory cannot be read")),
        };
        if !index.file_type().is_dir() {
            return Err(untrusted("the index is not a directory"));
        }
        if index.mode() & GROUP_OR_OTHER_WRITE != 0 {
            return Err(untrusted(
                "the index directory is writable by group or others",
            ));
        }
        if self.refuse_own_index
            && index.uid() != 0
            && index.uid() == rustix::process::geteuid().as_raw()
        {
            return Err(untrusted("the index directory belongs to harw's own user"));
        }
        Ok(Some(index))
    }
}

#[cfg(target_os = "linux")]
impl Default for MarkerDirHolds {
    fn default() -> Self {
        Self::new(CONTAINMENT_HOLDS_DIR)
    }
}

/// [`FreezeError::ContainmentIndexUnreadable`] with `reason`.
#[cfg(target_os = "linux")]
fn untrusted(reason: &'static str) -> FreezeError {
    FreezeError::ContainmentIndexUnreadable { reason }
}

#[cfg(target_os = "linux")]
impl ContainmentHolds for MarkerDirHolds {
    fn is_held(&self, kernel_id: u64) -> Result<bool, FreezeError> {
        let Some(index) = self.trusted_index()? else {
            return Ok(false);
        };
        let marker = match fs::symlink_metadata(self.dir.join(kernel_id.to_string())) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(untrusted("a marker cannot be read")),
        };
        if !marker.file_type().is_file() {
            return Err(untrusted("a marker is not a regular file"));
        }
        if marker.uid() != 0 && marker.uid() != index.uid() {
            return Err(untrusted("a marker has a foreign owner"));
        }
        Ok(true)
    }

    fn lease(&self) -> Result<ContainmentLease, FreezeError> {
        let Some(index) = self.trusted_index()? else {
            return Ok(ContainmentLease::unlocked());
        };
        let dir = fs::File::open(&self.dir)
            .map_err(|_| untrusted("the index directory cannot be opened"))?;
        // `open` follows symbolic links: lock only the very directory that
        // was just inspected without following any.
        let opened = dir
            .metadata()
            .map_err(|_| untrusted("the index directory cannot be read"))?;
        if opened.dev() != index.dev() || opened.ino() != index.ino() {
            return Err(untrusted(
                "the index directory was replaced while it was opened",
            ));
        }
        for _ in 0..LEASE_ATTEMPTS {
            match dir.try_lock_shared() {
                Ok(()) => {
                    return Ok(ContainmentLease {
                        lock: Some(dir),
                        contended: false,
                    });
                }
                Err(fs::TryLockError::WouldBlock) => std::thread::sleep(LEASE_RETRY),
                // Best effort (`ContainmentLease::contended`): without the
                // lock the caller relies on its re-check alone.
                Err(fs::TryLockError::Error(_)) => return Ok(ContainmentLease::contended()),
            }
        }
        // Someone holds the exclusive lock (the Warden for longer than its
        // one marker and freeze, or any local user): do not wait for it.
        Ok(ContainmentLease::contended())
    }
}

/// The leaf of `path` when its parent is exactly `root` (both
/// hierarchy-relative); `None` otherwise, for the hierarchy root as `root`
/// and for an empty leaf.
pub(crate) fn leaf_beneath<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    if root.is_empty() || !root.starts_with('/') {
        return None;
    }
    let (parent, leaf) = path.rsplit_once('/')?;
    (parent == root && !leaf.is_empty()).then_some(leaf)
}

/// Whether the cgroup `own` lies at or beneath `target` (component-wise).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn is_within(own: &str, target: &str) -> bool {
    own.strip_prefix(target)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(target_os = "linux")]
impl From<CgroupError> for FreezeError {
    fn from(error: CgroupError) -> Self {
        match error {
            CgroupError::Unsupported { feature } => Self::Unsupported { feature },
            CgroupError::CgroupUnavailable { .. } => Self::Unavailable,
            CgroupError::PermissionDenied { .. } => Self::PermissionDenied,
            CgroupError::NotFound { .. } => Self::Gone,
            CgroupError::InvalidName { .. } => Self::InvalidName,
            other => Self::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// [`JobFreezer`] over a `harw_job_linux` cgroup backend rooted at the
/// delegated job root (Linux only).
///
/// # Description
/// Resolves the leaf with `CgroupBackend::reopen` (single safe path
/// component beneath the backend's root) and writes `cgroup.freeze` through
/// `CgroupBackend::freeze` / `thaw`. Before freezing it compares the target
/// with the cgroup this process runs in and refuses harw's own cgroup and
/// its ancestors ([`FreezeError::OwnCgroup`]).
/// [`JobFreezer::containment_held`] looks the leaf's kernel id
/// (`CgroupBackend::kernel_id`) up in its [`ContainmentHolds`]
/// (`MarkerDirHolds` over [`CONTAINMENT_HOLDS_DIR`] unless built with
/// `CgroupFreezer::with_holds`); [`JobFreezer::containment_lease`] takes
/// that index's shared lease. `thaw` itself does not check either: only the
/// [`crate::JobManager`] thaws, and it holds the lease and asks before and
/// after. `Debug` shows the root and the containment index.
///
/// # Examples
/// ```rust,no_run
/// # #[cfg(target_os = "linux")]
/// # fn main() -> Result<(), harw_tool_job::FreezeError> {
/// use harw_tool_job::{CgroupFreezer, JobFreezer};
///
/// let freezer = CgroupFreezer::open(std::path::Path::new("/sys/fs/cgroup/harw.slice"))?;
/// freezer.freeze("harw-job-work-1-e1")?;
/// freezer.thaw("harw-job-work-1-e1")?;
/// # Ok(())
/// # }
/// # #[cfg(not(target_os = "linux"))]
/// # fn main() {}
/// ```
#[cfg(target_os = "linux")]
pub struct CgroupFreezer {
    backend: Arc<dyn CgroupBackend>,
    root: String,
    // Containments held outside harw (the Warden), by kernel id.
    holds: Arc<dyn ContainmentHolds>,
}

#[cfg(target_os = "linux")]
impl CgroupFreezer {
    /// Wraps `backend`, whose delegated root is `root`, hierarchy-relative
    /// as in `/proc/<pid>/cgroup` (e.g. `/harw.slice`; for
    /// `harw_job_linux::cgroup::CgroupV2Fs` its `root_proc_path`). The
    /// Warden's containments are read from [`CONTAINMENT_HOLDS_DIR`]
    /// ([`MarkerDirHolds::default`]).
    #[must_use]
    pub fn new(backend: Arc<dyn CgroupBackend>, root: impl Into<String>) -> Self {
        Self::with_holds(backend, root, Arc::new(MarkerDirHolds::default()))
    }

    /// Like [`CgroupFreezer::new`], with the containment index `holds`
    /// instead of the Warden's default marker directory.
    ///
    /// # Arguments
    /// - `backend`: the cgroup backend rooted at the delegated job root.
    /// - `root`: that root, hierarchy-relative (e.g. `/harw.slice`).
    /// - `holds`: the index [`JobFreezer::containment_held`] consults and
    ///   [`JobFreezer::containment_lease`] locks.
    #[must_use]
    pub fn with_holds(
        backend: Arc<dyn CgroupBackend>,
        root: impl Into<String>,
        holds: Arc<dyn ContainmentHolds>,
    ) -> Self {
        Self {
            backend,
            root: root.into(),
            holds,
        }
    }

    /// Opens the delegated root `path` (absolute, on the cgroup v2 mount)
    /// with the in-house cgroupfs backend.
    ///
    /// # Description
    /// Like every `CgroupV2Fs::open` it enables the available controllers
    /// for children (best effort), which the job runtime does on the same
    /// root anyway.
    ///
    /// # Errors
    /// [`FreezeError::Unavailable`] when `path` is not a delegated, writable
    /// cgroup v2 directory; other classes of the underlying error.
    pub fn open(path: &Path) -> Result<Self, FreezeError> {
        let backend = CgroupV2Fs::open(path)?;
        let root = backend.root_proc_path().to_owned();
        Ok(Self::new(Arc::new(backend), root))
    }
}

#[cfg(target_os = "linux")]
impl fmt::Debug for CgroupFreezer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CgroupFreezer")
            .field("root", &self.root)
            .field("holds", &self.holds)
            .finish_non_exhaustive()
    }
}

/// The cgroup-v2 path this process runs in (hierarchy-relative), if known.
#[cfg(target_os = "linux")]
fn own_cgroup() -> Option<String> {
    harw_job_linux::proc::snapshot(std::process::id())
        .ok()
        .and_then(|snapshot| snapshot.cgroup_v2_path)
}

#[cfg(target_os = "linux")]
impl JobFreezer for CgroupFreezer {
    fn delegated_root(&self) -> &str {
        &self.root
    }

    fn freeze(&self, leaf: &str) -> Result<(), FreezeError> {
        let target = format!("{}/{leaf}", self.root.trim_end_matches('/'));
        // Defence in depth: the proof already binds the leaf to the job, but
        // harw must never freeze itself. An unknown own cgroup (no cgroup
        // v2 membership) cannot be the delegated job root.
        if own_cgroup().is_some_and(|own| is_within(&own, &target)) {
            return Err(FreezeError::OwnCgroup);
        }
        let group = self.backend.reopen(leaf)?;
        self.backend.freeze(&group)?;
        Ok(())
    }

    fn thaw(&self, leaf: &str) -> Result<(), FreezeError> {
        let group = self.backend.reopen(leaf)?;
        self.backend.thaw(&group)?;
        Ok(())
    }

    fn is_frozen(&self, leaf: &str) -> Result<bool, FreezeError> {
        let group = self.backend.reopen(leaf)?;
        Ok(self.backend.is_frozen(&group)?)
    }

    fn freeze_requested(&self, leaf: &str) -> Result<bool, FreezeError> {
        let group = self.backend.reopen(leaf)?;
        Ok(self.backend.freeze_requested(&group)?)
    }

    fn containment_held(&self, leaf: &str) -> Result<bool, FreezeError> {
        let group = self.backend.reopen(leaf)?;
        let kernel_id = self.backend.kernel_id(&group)?;
        self.holds.is_held(kernel_id)
    }

    fn containment_lease(&self) -> Result<ContainmentLease, FreezeError> {
        self.holds.lease()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leaf_beneath_accepts_only_direct_children_of_the_root() {
        assert_eq!(
            leaf_beneath("/harw.slice/harw-job-a", "/harw.slice"),
            Some("harw-job-a")
        );
        assert_eq!(
            leaf_beneath("/harw.slice/harw-job-a", "/harw.slice/"),
            Some("harw-job-a")
        );
        for (path, root) in [
            // Another root, a deeper path, the root itself.
            ("/harw.slice/harw-job-a", "/other.slice"),
            ("/harw.slice/x/harw-job-a", "/harw.slice"),
            ("/harw.slice", "/harw.slice"),
            // The hierarchy root and relative roots are never delegated.
            ("/harw-job-a", "/"),
            ("/harw-job-a", ""),
            ("/harw.slice/harw-job-a", "harw.slice"),
            // Empty leaf, relative path.
            ("/harw.slice/", "/harw.slice"),
            ("harw-job-a", "/harw.slice"),
        ] {
            assert_eq!(leaf_beneath(path, root), None, "{path:?} {root:?}");
        }
    }

    #[test]
    fn test_is_within_is_component_wise() {
        assert!(is_within(
            "/harw.slice/harw-job-a",
            "/harw.slice/harw-job-a"
        ));
        assert!(is_within(
            "/harw.slice/harw-job-a/inner",
            "/harw.slice/harw-job-a"
        ));
        assert!(!is_within(
            "/harw.slice/harw-job-ab",
            "/harw.slice/harw-job-a"
        ));
        assert!(!is_within("/harw.slice", "/harw.slice/harw-job-a"));
    }

    #[test]
    fn test_freeze_error_messages_are_stable() {
        assert_eq!(
            FreezeError::Unsupported {
                feature: "cgroup.freeze"
            }
            .to_string(),
            "the cgroup freezer is not supported here (cgroup.freeze)"
        );
        assert_eq!(
            FreezeError::OwnCgroup.to_string(),
            "refusing to freeze the cgroup harw itself runs in"
        );
        assert_eq!(
            FreezeError::Failed {
                reason: "boom".to_owned()
            }
            .to_string(),
            "cgroup operation failed: boom"
        );
        assert_eq!(
            FreezeError::ContainmentIndexUnreadable {
                reason: "a marker is not a regular file"
            }
            .to_string(),
            "the containment index cannot be trusted (a marker is not a regular file)"
        );
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::super::*;
        use crate::test_support::fake_cgroup::{
            FakeCgroup, FakeHolds, chmod, fake_kernel_id, marker_index, own_uid,
            trusting_test_user, write_marker,
        };
        use crate::test_support::{TestError, TestResult, ctx};
        use harw_job_linux::LinuxProcess;
        use harw_job_linux::cgroup::{CgroupHandle, CgroupSpec, CgroupStats};
        use std::os::unix::fs::MetadataExt as _;
        use std::path::PathBuf;

        /// Whether `result` is an untrusted-index error.
        fn is_untrusted<T>(result: &Result<T, FreezeError>) -> bool {
            matches!(result, Err(FreezeError::ContainmentIndexUnreadable { .. }))
        }

        /// The fixed reason of an untrusted-index error, if it is one.
        fn reason_of<T>(result: &Result<T, FreezeError>) -> Option<&'static str> {
            match result {
                Err(FreezeError::ContainmentIndexUnreadable { reason }) => Some(*reason),
                _ => None,
            }
        }

        #[test]
        fn test_marker_dir_holds_reads_markers_by_kernel_id() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            assert_eq!(holds.is_held(4_294_967_298), Ok(false));
            write_marker(&holds, 4_294_967_298)?;
            assert_eq!(holds.is_held(4_294_967_298), Ok(true));
            // Another cgroup is not held by that marker.
            assert_eq!(holds.is_held(4_294_967_299), Ok(false));
            Ok(())
        }

        /// Symbolic links are never followed: not as marker, not as index
        /// directory and not as the Warden's runtime directory; a directory
        /// is no marker. All of them fail closed instead of meaning "not
        /// held".
        #[test]
        fn test_marker_dir_holds_never_follows_symlinks() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            let target = temp.path().join("target-file");
            fs::write(&target, b"").map_err(ctx("write target"))?;
            chmod(&target, 0o644)?;
            std::os::unix::fs::symlink(&target, holds.dir().join("7"))
                .map_err(ctx("symlink marker"))?;
            assert!(is_untrusted(&holds.is_held(7)), "{:?}", holds.is_held(7));
            fs::create_dir(holds.dir().join("8")).map_err(ctx("directory marker"))?;
            assert!(is_untrusted(&holds.is_held(8)), "{:?}", holds.is_held(8));

            // The index itself as a symbolic link to a trusted directory,
            // inside a trusted runtime directory.
            let linked = temp.path().join("linked-warden");
            fs::create_dir(&linked).map_err(ctx("create linked runtime dir"))?;
            chmod(&linked, 0o755)?;
            std::os::unix::fs::symlink(holds.dir(), linked.join("holds"))
                .map_err(ctx("symlink index"))?;
            let via_link = trusting_test_user(MarkerDirHolds::new(linked.join("holds")));
            assert_eq!(
                reason_of(&via_link.is_held(9)),
                Some("the index is not a directory")
            );
            assert!(is_untrusted(&via_link.lease()), "{:?}", via_link.lease());

            // The runtime directory as a symbolic link to the trusted one.
            let runtime_link = temp.path().join("runtime-link");
            std::os::unix::fs::symlink(temp.path().join("harw-warden"), &runtime_link)
                .map_err(ctx("symlink runtime dir"))?;
            let via_runtime = trusting_test_user(MarkerDirHolds::new(runtime_link.join("holds")));
            assert_eq!(
                reason_of(&via_runtime.is_held(9)),
                Some("the runtime directory is not a directory")
            );
            Ok(())
        }

        /// An index or runtime directory others could change, a runtime
        /// directory of a foreign owner, a missing index while the Warden's
        /// runtime directory exists, and an index that is no directory all
        /// fail closed. Only a host without the Warden's runtime directory
        /// and without any evidence of a Warden has no holds.
        #[test]
        fn test_marker_dir_holds_fails_closed_on_an_untrusted_index() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            let runtime = temp.path().join("harw-warden");
            write_marker(&holds, 11)?;
            for mode in [0o775, 0o757, 0o777] {
                chmod(holds.dir(), mode)?;
                assert!(is_untrusted(&holds.is_held(11)), "index {mode:o}");
                assert!(is_untrusted(&holds.is_held(12)), "index {mode:o}");
                assert!(is_untrusted(&holds.lease()), "index {mode:o}");
            }
            chmod(holds.dir(), 0o755)?;
            for mode in [0o775, 0o757, 0o777] {
                chmod(&runtime, mode)?;
                assert!(is_untrusted(&holds.is_held(11)), "runtime {mode:o}");
                assert!(is_untrusted(&holds.lease()), "runtime {mode:o}");
            }
            chmod(&runtime, 0o755)?;
            assert_eq!(holds.is_held(11), Ok(true));

            // The runtime directory must belong to root (here: the test
            // user); any other owner fails closed.
            let foreign = holds.clone().trusting_runtime_owner(own_uid() ^ 1);
            assert_eq!(
                reason_of(&foreign.is_held(11)),
                Some("the runtime directory has a foreign owner")
            );

            // The Warden's runtime directory exists, its index does not.
            let missing = trusting_test_user(MarkerDirHolds::new(runtime.join("gone")));
            assert_eq!(
                reason_of(&missing.is_held(11)),
                Some("the index directory is missing")
            );
            // The index is a regular file.
            let file = runtime.join("file");
            fs::write(&file, b"").map_err(ctx("write index file"))?;
            let not_dir = trusting_test_user(MarkerDirHolds::new(&file));
            assert!(
                is_untrusted(&not_dir.is_held(11)),
                "{:?}",
                not_dir.is_held(11)
            );
            // No Warden runtime directory and no evidence of a Warden on
            // this host: nothing is held, the lease locks nothing.
            let absent = trusting_test_user(MarkerDirHolds::new(
                temp.path().join("no-warden").join("holds"),
            ));
            assert_eq!(absent.is_held(11), Ok(false));
            let unlocked = absent
                .lease()
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert!(!unlocked.is_locked());
            assert!(!unlocked.is_contended());
            // A relative index without a parent never counts as absent.
            let relative =
                trusting_test_user(MarkerDirHolds::new("harw-test-holds-without-parent"));
            assert!(
                is_untrusted(&relative.is_held(11)),
                "{:?}",
                relative.is_held(11)
            );
            Ok(())
        }

        /// Review round 1: a missing runtime directory is "no Warden" only
        /// without any evidence of an installed Warden and when none is
        /// expected. Its socket (or opt-in file), evidence that cannot be
        /// ruled out, or an expected Warden all fail closed: a half-deployed
        /// host never lets `job.resume` lift a containment.
        #[test]
        fn test_marker_dir_holds_absent_runtime_dir_with_a_warden_fails_closed() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let index = temp.path().join("no-warden").join("holds");
            let socket = temp.path().join("warden.sock");
            let holds = trusting_test_user(MarkerDirHolds::new(&index))
                .with_owner_evidence([socket.clone()]);
            assert_eq!(holds.is_held(11), Ok(false));
            // The Warden's socket exists (a regular file stands in for it).
            fs::write(&socket, b"").map_err(ctx("fake warden socket"))?;
            assert_eq!(
                reason_of(&holds.is_held(11)),
                Some("the runtime directory of an installed containment owner is missing")
            );
            assert!(is_untrusted(&holds.lease()), "{:?}", holds.lease());
            // Evidence whose absence cannot be proven counts as present.
            let file = temp.path().join("plain-file");
            fs::write(&file, b"").map_err(ctx("plain file"))?;
            let unprovable = trusting_test_user(MarkerDirHolds::new(&index))
                .with_owner_evidence([file.join("warden.enable")]);
            assert!(
                is_untrusted(&unprovable.is_held(11)),
                "{:?}",
                unprovable.is_held(11)
            );
            // An expected Warden needs no evidence.
            let expected = trusting_test_user(MarkerDirHolds::new(&index)).owner_expected();
            assert!(
                is_untrusted(&expected.is_held(11)),
                "{:?}",
                expected.is_held(11)
            );
            // Production default: the Warden's index, detected by its
            // socket and its opt-in file.
            assert_eq!(
                MarkerDirHolds::default(),
                MarkerDirHolds::new(CONTAINMENT_HOLDS_DIR)
                    .with_owner_evidence(CONTAINMENT_OWNER_EVIDENCE)
            );
            assert_ne!(
                MarkerDirHolds::default(),
                MarkerDirHolds::default().owner_expected()
            );
            Ok(())
        }

        /// Review round 1: an index directory that belongs to harw's own
        /// (non-root) user could be changed by any job of harw; in
        /// production it is refused. Root as owner is always accepted.
        #[test]
        fn test_marker_dir_holds_refuses_an_index_of_harws_own_user() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let relaxed = marker_index(temp.path())?;
            let strict = MarkerDirHolds::new(relaxed.dir())
                .trusting_runtime_owner(own_uid())
                .with_owner_evidence(Vec::<PathBuf>::new());
            if own_uid() == 0 {
                assert_eq!(strict.is_held(31), Ok(false));
            } else {
                assert_eq!(
                    reason_of(&strict.is_held(31)),
                    Some("the index directory belongs to harw's own user")
                );
                assert!(is_untrusted(&strict.lease()), "{:?}", strict.lease());
            }
            Ok(())
        }

        /// A marker owned by neither root nor the owner of the index fails
        /// closed. Changing the owner needs root; elsewhere the check is
        /// skipped.
        #[test]
        fn test_marker_dir_holds_refuses_a_foreign_marker_owner() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            let marker = write_marker(&holds, 21)?;
            let index_owner = fs::symlink_metadata(holds.dir())
                .map_err(ctx("stat index"))?
                .uid();
            if index_owner != 0 {
                return Ok(()); // Not root: cannot hand the marker to another uid.
            }
            // nobody (65534) is neither root nor the index owner (root).
            std::os::unix::fs::chown(&marker, Some(65_534), None).map_err(ctx("chown marker"))?;
            assert!(is_untrusted(&holds.is_held(21)), "{:?}", holds.is_held(21));
            Ok(())
        }

        /// The lease holds `flock(LOCK_SH)` on the index directory: the
        /// Warden's exclusive lock has to wait, other readers do not. While
        /// someone holds the exclusive lock (the Warden, or any local user:
        /// the directory is world-readable), the lease is contended after
        /// its bounded wait, never an error (review round 2: no host-wide
        /// `job.resume` lockout); once released, it is granted again.
        #[test]
        fn test_marker_dir_holds_lease_locks_the_index_shared() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            let lease = holds
                .lease()
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert!(lease.is_locked());
            assert!(!lease.is_contended());
            let warden = fs::File::open(holds.dir()).map_err(ctx("open index"))?;
            assert!(
                matches!(warden.try_lock(), Err(fs::TryLockError::WouldBlock)),
                "the Warden's exclusive lock must wait for the lease"
            );
            let reader = fs::File::open(holds.dir()).map_err(ctx("open index"))?;
            reader
                .try_lock_shared()
                .map_err(ctx("second shared lock"))?;
            drop(reader);
            drop(lease);
            warden
                .try_lock()
                .map_err(ctx("exclusive lock after the lease"))?;
            let busy = holds
                .lease()
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert!(busy.is_contended());
            assert!(!busy.is_locked());
            drop(busy);
            drop(warden);
            let again = holds
                .lease()
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert!(again.is_locked());
            assert!(!again.is_contended());
            Ok(())
        }

        /// `ContainmentLease` constructors: unlocked and contended leases
        /// lock nothing; only a contended one says its lock was not
        /// obtained.
        #[test]
        fn test_containment_lease_states() {
            let unlocked = ContainmentLease::unlocked();
            assert!(!unlocked.is_locked());
            assert!(!unlocked.is_contended());
            let contended = ContainmentLease::contended();
            assert!(!contended.is_locked());
            assert!(contended.is_contended());
        }

        /// `CgroupFreezer::new` (and so `open`) reads the Warden's default
        /// index: `Debug` shows it, so a regression to another index is
        /// visible.
        #[test]
        fn test_cgroup_freezer_new_reads_the_wardens_index() {
            let freezer = CgroupFreezer::new(Arc::new(FakeCgroup::default()), "/harw.slice");
            let shown = format!("{freezer:?}");
            let expected = format!("{:?}", MarkerDirHolds::default());
            assert!(shown.contains(&expected), "{shown}");
            assert!(shown.contains(CONTAINMENT_HOLDS_DIR), "{shown}");
        }

        /// End to end over a real marker directory: the Warden's marker for
        /// the leaf's kernel id holds exactly that cgroup, and the freezer's
        /// lease locks the index.
        #[test]
        fn test_cgroup_freezer_over_a_marker_directory() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let holds = marker_index(temp.path())?;
            write_marker(&holds, fake_kernel_id("harw-job-a"))?;
            let fake = Arc::new(FakeCgroup::default());
            let freezer = CgroupFreezer::with_holds(
                Arc::clone(&fake) as Arc<dyn CgroupBackend>,
                "/harw.slice",
                Arc::new(holds),
            );
            assert_eq!(freezer.containment_held("harw-job-a"), Ok(true));
            assert_eq!(freezer.containment_held("harw-job-b"), Ok(false));
            let lease = freezer
                .containment_lease()
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert!(lease.is_locked());
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
            Ok(())
        }

        /// `containment_held` looks the leaf's kernel id up in the holds of
        /// the freezer and never writes to the cgroup.
        #[test]
        fn test_cgroup_freezer_consults_the_holds_by_kernel_id() -> TestResult {
            let fake = Arc::new(FakeCgroup::default());
            let holds = Arc::new(FakeHolds::default());
            let freezer = CgroupFreezer::with_holds(
                Arc::clone(&fake) as Arc<dyn CgroupBackend>,
                "/harw.slice",
                Arc::clone(&holds) as Arc<dyn ContainmentHolds>,
            );
            assert_eq!(freezer.containment_held("harw-job-a"), Ok(false));
            holds.hold(fake_kernel_id("harw-job-a"));
            assert_eq!(freezer.containment_held("harw-job-a"), Ok(true));
            assert_eq!(freezer.containment_held("harw-job-b"), Ok(false));
            holds.set_unreadable(true);
            assert!(
                matches!(
                    freezer.containment_held("harw-job-b"),
                    Err(FreezeError::ContainmentIndexUnreadable { .. })
                ),
                "{:?}",
                freezer.containment_held("harw-job-b")
            );
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
            Ok(())
        }

        /// A backend without kernel ids cannot tell a containment apart:
        /// `containment_held` fails closed instead of answering `false`.
        #[test]
        fn test_cgroup_freezer_without_kernel_ids_fails_closed() {
            let freezer = CgroupFreezer::with_holds(
                Arc::new(NoKernelId),
                "/harw.slice",
                Arc::new(FakeHolds::default()),
            );
            assert!(
                matches!(
                    freezer.containment_held("harw-job-a"),
                    Err(FreezeError::Unsupported { .. })
                ),
                "{:?}",
                freezer.containment_held("harw-job-a")
            );
        }

        /// A backend that implements only the required methods (no
        /// `kernel_id`).
        struct NoKernelId;

        impl CgroupBackend for NoKernelId {
            fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
                Err(CgroupError::NotFound {
                    path: spec.name.clone(),
                })
            }

            fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
                Ok(CgroupHandle::new(
                    name.to_owned(),
                    format!("/harw.slice/{name}"),
                    None,
                ))
            }

            fn attach(
                &self,
                group: &CgroupHandle,
                _process: &LinuxProcess,
            ) -> Result<(), CgroupError> {
                Err(CgroupError::NotFound {
                    path: group.name().to_owned(),
                })
            }

            fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
                Err(CgroupError::NotFound {
                    path: group.name().to_owned(),
                })
            }

            fn freeze(&self, _group: &CgroupHandle) -> Result<(), CgroupError> {
                Ok(())
            }

            fn thaw(&self, _group: &CgroupHandle) -> Result<(), CgroupError> {
                Ok(())
            }

            fn kill(&self, _group: &CgroupHandle) -> Result<(), CgroupError> {
                Ok(())
            }

            fn stats(&self, _group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
                Ok(CgroupStats::default())
            }

            fn remove(&self, _group: CgroupHandle) -> Result<(), CgroupError> {
                Ok(())
            }
        }

        #[test]
        fn test_cgroup_errors_map_to_typed_classes() {
            let cases = [
                (
                    CgroupError::Unsupported {
                        feature: "cgroup.freeze",
                    },
                    FreezeError::Unsupported {
                        feature: "cgroup.freeze",
                    },
                ),
                (
                    CgroupError::CgroupUnavailable {
                        root: PathBuf::from("/sys/fs/cgroup/harw.slice"),
                        reason: harw_job_linux::CgroupUnavailableReason::NotDelegated,
                    },
                    FreezeError::Unavailable,
                ),
                (
                    CgroupError::PermissionDenied {
                        path: "harw-job-a".to_owned(),
                    },
                    FreezeError::PermissionDenied,
                ),
                (
                    CgroupError::NotFound {
                        path: "harw-job-a".to_owned(),
                    },
                    FreezeError::Gone,
                ),
                (
                    CgroupError::InvalidName {
                        name: "../x".to_owned(),
                    },
                    FreezeError::InvalidName,
                ),
            ];
            for (error, expected) in cases {
                assert_eq!(FreezeError::from(error), expected);
            }
            let busy = FreezeError::from(CgroupError::Busy {
                path: "harw-job-a".to_owned(),
            });
            assert!(
                matches!(&busy, FreezeError::Failed { reason } if reason.contains("harw-job-a")),
                "{busy:?}"
            );
            // The absolute root path never reaches the message.
            let unavailable = FreezeError::from(CgroupError::CgroupUnavailable {
                root: PathBuf::from("/sys/fs/cgroup/secret.slice"),
                reason: harw_job_linux::CgroupUnavailableReason::RootMissing,
            });
            assert!(!unavailable.to_string().contains("secret"));
        }

        /// `CgroupFreezer` goes through `CgroupBackend::{freeze, thaw}` with
        /// exactly the leaf it was given.
        #[test]
        fn test_cgroup_freezer_uses_backend_freeze_and_thaw() -> TestResult {
            let fake = Arc::new(FakeCgroup::default());
            let freezer =
                CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, "/harw.slice");
            assert_eq!(freezer.delegated_root(), "/harw.slice");
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(false));
            freezer
                .freeze("harw-job-a")
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert_eq!(freezer.is_frozen("harw-job-a"), Ok(true));
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(true));
            freezer
                .thaw("harw-job-a")
                .map_err(|err| TestError::Unexpected(err.to_string()))?;
            assert_eq!(freezer.is_frozen("harw-job-a"), Ok(false));
            assert_eq!(freezer.freeze_requested("harw-job-a"), Ok(false));
            assert_eq!(
                fake.calls(),
                vec![
                    ("freeze", "harw-job-a".to_owned()),
                    ("thaw", "harw-job-a".to_owned()),
                ]
            );
            Ok(())
        }

        #[test]
        fn test_cgroup_freezer_maps_backend_errors() {
            let fake = Arc::new(FakeCgroup::denying());
            let freezer =
                CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, "/harw.slice");
            assert_eq!(
                freezer.freeze("harw-job-a"),
                Err(FreezeError::PermissionDenied)
            );
            assert_eq!(
                freezer.thaw("harw-job-a"),
                Err(FreezeError::PermissionDenied)
            );
        }

        /// harw never freezes the cgroup it runs in: with the freezer rooted
        /// at the parent of this test's own cgroup, freezing its leaf is
        /// refused before the backend is asked.
        #[test]
        fn test_cgroup_freezer_refuses_its_own_cgroup() {
            let Some(own) = own_cgroup() else {
                return; // No cgroup v2 membership on this host.
            };
            let Some((parent, leaf)) = own.rsplit_once('/') else {
                return;
            };
            if parent.is_empty() || leaf.is_empty() {
                return; // Running in the hierarchy root itself.
            }
            let fake = Arc::new(FakeCgroup::default());
            let freezer = CgroupFreezer::new(Arc::clone(&fake) as Arc<dyn CgroupBackend>, parent);
            assert_eq!(freezer.freeze(leaf), Err(FreezeError::OwnCgroup));
            assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        }
    }
}

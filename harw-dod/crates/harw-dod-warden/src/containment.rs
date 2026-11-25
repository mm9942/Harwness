//! Eindämmungsmarker für harw und die Buchführung der Eindämmungen:
//! [`ContainmentMarkers`], [`MarkerDir`], [`MarkerCreation`],
//! [`ContainmentStore`], [`ContainmentEntry`] (PL-100, Owner-Review
//! 4505015305: `job.resume` darf nie einen Freeze des Wardens aufheben).
//!
//! # Zweck
//! cgroup v2 hat je cgroup genau ein `cgroup.freeze`-Bit. Hat harw einen
//! eigenen Job angehalten (`job.suspend`) und friert der Warden dieselbe
//! cgroup ein, teilen sich beide dieses Bit; ein `job.resume` höbe sonst auch
//! die Eindämmung des Wardens auf. Der Warden legt deshalb je eingedämmter
//! cgroup einen **Marker** an: eine leere reguläre Datei
//! `<holds>/<Kernel-ID dezimal>` (Vorgabe [`DEFAULT_HOLDS_DIR`]; die
//! Kernel-ID ist die Inode des cgroup-Verzeichnisses). harw liest diesen
//! Index vor jedem Auftauen (`harw_tool_job::MarkerDirHolds`, nur `lstat`)
//! und taut eine markierte cgroup nie auf.
//!
//! # Vertrag mit harw (Leserseite: `harw-tool-job`, `freeze.rs`)
//! - **Eindämmen** (`FreezeCgroup`): das Indexverzeichnis mit
//!   `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` öffnen
//!   ([`ContainmentMarkers::open_locked`]), **ein** Versuch
//!   `flock(LOCK_EX | LOCK_NB)` ohne Warten (bei Konkurrenz weiter: jeder
//!   lokale Nutzer kann das Verzeichnis sperren, Eindämmung geht vor), dann
//!   den Marker **zuerst** anlegen ([`MarkerDir::create`], `O_CREAT |
//!   O_EXCL | O_NOFOLLOW`, Modus `0644`), dann `cgroup.freeze` lesen und
//!   festhalten, ob es schon `1` war (`freeze_was_set`, in
//!   [`ContainmentStore`]), dann `cgroup.freeze` = 1 schreiben, dann die
//!   Sperre freigeben (Schließen des Deskriptors). Lässt sich der Marker
//!   nicht anlegen, friert der Warden trotzdem ein und meldet das.
//! - **Freigeben** (`ThawCgroup`, Hold-Timer): gehört das Bit nicht dem
//!   Warden (beim Eindämmen schon gesetzt, oder der Datensatz ist nicht
//!   abgeschlossen), wird **zuerst** der Datensatz dauerhaft als
//!   „freigegeben“ gebucht ([`ContainmentEntry::released`],
//!   [`ContainmentStore::record_release`]: erst schreiben, dann übernehmen;
//!   scheitert das Schreiben, wird nichts freigegeben), und `cgroup.freeze`
//!   bleibt stehen (der frühere Eigentümer hebt seinen Freeze selbst auf).
//!   Gehört das Bit dem Warden (`freeze_was_set` = `false`), setzt er zuerst
//!   `cgroup.freeze` = 0 und bucht **danach** die Freigabe
//!   ([`ContainmentStore::mark_released`]); ein Absturz dazwischen
//!   hinterlässt Bit `0`, und der Abgleich beim Start räumt ab. In beiden
//!   Fällen liegt die Buchung vor dem `executed` im Aktions-Ledger, vor dem
//!   Aufheben eines Holds und vor dem Entfernen des Markers
//!   ([`ContainmentMarkers::remove`]); zuletzt geht der Datensatz. Den Marker
//!   eines Datensatzes entfernt der Warden nur, wenn die Freigabe dauerhaft
//!   gebucht ist: ein Absturz danach lässt nie einen Marker neu entstehen (der
//!   Abgleich beim Start räumt einen freigegebenen Datensatz nur ab). Ist sie
//!   nur im Speicher gebucht oder lässt sich der Marker nicht entfernen,
//!   bleibt der Marker stehen (fail closed), und der Hold-Timer schreibt die
//!   Freigabe nach ([`ContainmentStore::flush`]) und entfernt ihn erneut.
//! - Marker von cgroups, die es nicht mehr gibt, entfernt der Warden
//!   (regelmäßiger Durchlauf des Hold-Timers, signierter Thaw eines
//!   verschwundenen Ziels als Operator-Pfad, nach einem Kill), siehe
//!   `warden.rs` und `reconcile.rs`.
//!
//! # Keine Symlinks, nur eigene Verzeichnisse
//! Das Indexverzeichnis wird nie über einen Symlink geöffnet
//! (`O_NOFOLLOW`), muss dem effektiven Nutzer des Wardens gehören und darf
//! weder für die Gruppe noch für andere schreibbar sein (harw verlangt
//! dasselbe vom Index und akzeptiert nur Marker von root oder vom Eigentümer
//! des Index). Alle Marker-Zugriffe laufen relativ zum geöffneten
//! Verzeichnis-Deskriptor (`openat`, `fstatat` ohne Symlink-Folgen,
//! `unlinkat`). Ein vorhandener Eintrag gleichen Namens
//! ([`MarkerCreation`]): eine reguläre Datei des Wardens gilt als Marker;
//! eine reguläre Datei eines anderen Eigentümers wird entfernt und neu
//! angelegt; alles andere (Symlink, Verzeichnis, Gerät) wird abgelehnt und
//! nie entfernt.
//!
//! # Datensatz vor Marker
//! [`ContainmentStore`] liegt im StateDirectory (`containment.v1.json`,
//! atomar ersetzt wie die Holds). Ein Freeze stellt seinen Datensatz
//! (Pfad, Kernel-ID, noch ohne `freeze_was_set`) **dauerhaft vor** dem
//! Marker; nach dem Lesen des Bits und vor dem Schreiben von
//! `cgroup.freeze` wird er mit `freeze_was_set` abgeschlossen; lässt sich
//! das nicht dauerhaft schreiben, friert der Warden nicht ein (die Anfrage
//! bekommt `unavailable`). So gibt es nach einem Absturz nie einen Marker,
//! dessen cgroup (Pfad und Kernel-ID) der Warden nicht kennt; ein nicht
//! abgeschlossener Datensatz zeigt einen Abbruch vor dem Freeze an, ein
//! abgeschlossener sagt, wem das Bit gehörte.
//!
//! # Boot-ID
//! Kernel-IDs von cgroups gelten nur innerhalb eines Boots; nach einem
//! Neustart des Hosts kann dieselbe Kernel-ID (oft sogar unter demselben
//! Pfad) eine ganz andere cgroup bezeichnen, und die Marker unter `/run` sind
//! weg. Der Speicher hält deshalb die Boot-ID
//! (`/proc/sys/kernel/random/boot_id`, [`current_boot_id`], gelesen beim
//! Öffnen, also vor Landlock) in seiner Datei fest. Datensätze eines anderen
//! Boots kommen beim Öffnen nicht in den laufenden Bestand, sondern zur Seite
//! ([`ContainmentStore::previous_boot`]); der Abgleich beim Start verwirft
//! und meldet sie. Ist eine der beiden Boot-IDs unbekannt (die laufende ist
//! nicht lesbar, oder die Datei trägt keine), lässt sich nicht zeigen, dass
//! die Datensätze aus diesem Boot stammen: sie kommen ebenfalls zur Seite
//! (fail closed). Holds tragen keine Boot-ID; ohne Datensatz taut der
//! Hold-Timer sie nicht auf (`warden.rs`, `expire_one`), und ein Marker
//! gleichen Namens bleibt als Marker ohne Datensatz stehen, bis ein
//! signierter Thaw ihn abräumt.
//!
//! # Nur im Speicher gebucht
//! Buchungen nach einem Eingriff ([`ContainmentStore::activate`],
//! [`ContainmentStore::mark_released`], [`ContainmentStore::restore`],
//! [`ContainmentStore::forget`], ...) gelten im Speicher auch dann, wenn das
//! Schreiben scheitert. Der Speicher merkt sich das: bis ein späteres
//! Schreiben gelingt (jedes schreibt den ganzen Bestand), ist sein Stand
//! ungesichert, und [`ContainmentStore::flush`] schreibt ihn nach. Eine nur
//! im Speicher gebuchte Freigabe entfernt nie einen Marker.
//!
//! # Abgleich beim Start
//! Marker überleben einen Neustart des Wardens (tmpfiles-Verzeichnis unter
//! `/run`, kein `RuntimeDirectory=`). Vor der ersten Anfrage gleicht
//! `crate::reconcile` Datensätze, Marker, Holds und cgroups ab; dafür listet
//! [`MarkerDir::scan`] das Indexverzeichnis über den geprüften Deskriptor
//! (`getdents64`), ohne einem Eintrag zu folgen.
//!
//! # Nebenläufigkeit
//! [`ContainmentMarkers`] ist zustandslos (jeder Aufruf öffnet das
//! Verzeichnis neu). [`MarkerDir`] hält den Deskriptor und damit die
//! Sperre, bis es fällt. [`ContainmentStore`] schützt seine Liste mit einem
//! `Mutex`; der Warden ruft alles unter seiner Eingriffssperre auf.
//!
//! # Fehler
//! [`WardenError::UntrustedFile`] (Verzeichnis oder Marker nicht
//! vertrauenswürdig), [`WardenError::Io`], [`WardenError::LedgerCorrupt`]
//! und [`WardenError::LedgerUnavailable`] (`"containment"`),
//! [`WardenError::StatePoisoned`], [`WardenError::Encode`].
//!
//! # Examples
//! ```rust
//! use std::os::unix::fs::PermissionsExt as _;
//!
//! use harw_dod_warden::containment::{ContainmentMarkers, MarkerCreation};
//! use harw_dod_warden_proto::CgroupKernelId;
//!
//! let Ok(run) = tempfile::tempdir() else { return };
//! let holds = run.path().join("holds");
//! let Ok(()) = std::fs::create_dir(&holds) else { return };
//! let Ok(()) = std::fs::set_permissions(&holds, std::fs::Permissions::from_mode(0o755)) else {
//!     return;
//! };
//! let Ok(kernel_id) = CgroupKernelId::new(4_294_967_298) else { return };
//! let markers = ContainmentMarkers::new(&holds);
//! let Ok(dir) = markers.open_locked() else { return };
//! assert_eq!(dir.create(kernel_id).ok(), Some(MarkerCreation::Created));
//! assert!(holds.join("4294967298").is_file());
//! drop(dir); // gibt die Sperre frei
//! assert_eq!(markers.remove(kernel_id).ok(), Some(true));
//! ```

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};

use crate::error::{WardenError, WardenResult};
use crate::fsutil;

/// Vorgabepfad des Indexverzeichnisses (gleich
/// `harw_tool_job::CONTAINMENT_HOLDS_DIR`; tmpfiles: `/run/harw-warden`
/// root `0755`, `holds` dem Warden-Nutzer, `0755`).
pub const DEFAULT_HOLDS_DIR: &str = "/run/harw-warden/holds";

/// Dateiname des Eindämmungsspeichers im StateDirectory.
pub const CONTAINMENT_FILE: &str = "containment.v1.json";

/// Höchstzahl gleichzeitig geführter Eindämmungen.
pub const MAX_CONTAINMENTS: usize = 1024;

/// Höchstzahl der Einträge, die [`MarkerDir::scan`] liest; weitere bleiben
/// ungelesen und unberührt ([`IndexScan::truncated`]).
pub const MAX_INDEX_ENTRIES: usize = 4096;

/// Quelle der Boot-ID des laufenden Kernels ([`current_boot_id`]).
pub const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

// Formatversion des Schnappschusses.
const CONTAINMENT_FORMAT_VERSION: u16 = 1;

// Längste zulässige Boot-ID (der Kernel liefert eine UUID mit 36 Zeichen).
const MAX_BOOT_ID_LEN: usize = 64;

// So viele Bytes liest `current_boot_id` höchstens (Boot-ID plus `\n`).
const MAX_BOOT_ID_READ: u64 = 80;

// Feste Namen für Fehlermeldungen (kein Pfad).
const HOLDS_DIR_WHAT: &str = "containment holds directory";
const MARKER_WHAT: &str = "containment marker";

/// Returns the marker file name of `kernel_id` (decimal, as harw looks it up).
///
/// # Examples
/// ```rust
/// use harw_dod_warden::containment::marker_name;
/// use harw_dod_warden_proto::CgroupKernelId;
///
/// let Ok(kernel_id) = CgroupKernelId::new(4_294_967_298) else { return };
/// assert_eq!(marker_name(kernel_id), "4294967298");
/// ```
#[must_use]
pub fn marker_name(kernel_id: CgroupKernelId) -> String {
    kernel_id.get().to_string()
}

/// Reads the boot id of the running kernel ([`BOOT_ID_PATH`]).
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Boot-ID“. Das Binary liest sie über
/// [`ContainmentStore::open`] vor Landlock und seccomp.
///
/// # Returns
/// Die Boot-ID (Hexziffern und `-`, höchstens 64 Zeichen) oder `None`, wenn
/// die Datei fehlt, unlesbar ist oder etwas anderes enthält.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::containment::current_boot_id;
///
/// if let Some(boot_id) = current_boot_id() {
///     assert!(boot_id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-'));
/// }
/// ```
#[must_use]
pub fn current_boot_id() -> Option<String> {
    let file = File::open(BOOT_ID_PATH).ok()?;
    let mut text = String::new();
    file.take(MAX_BOOT_ID_READ).read_to_string(&mut text).ok()?;
    parse_boot_id(&text)
}

// Prüft den Inhalt von `boot_id`: eine Zeile aus Hexziffern und `-` mit
// mindestens einer Ziffer, höchstens `MAX_BOOT_ID_LEN` Zeichen.
fn parse_boot_id(text: &str) -> Option<String> {
    let id = text.trim_end_matches('\n');
    let valid = !id.is_empty()
        && id.len() <= MAX_BOOT_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        && id.bytes().any(|byte| byte.is_ascii_hexdigit());
    valid.then(|| id.to_owned())
}

/// Was [`MarkerDir::scan`] im Indexverzeichnis vorfindet.
///
/// # Description
/// Ein Eintrag ist ein Marker, wenn sein Name die kanonische Dezimalform
/// einer Kernel-ID ist (genau der Name, den harw nachschlägt, siehe
/// [`marker_name`]) und unter ihm eine reguläre Datei liegt (`fstatat` ohne
/// Symlink-Folgen). Alles andere zählt als fremd: ein anderer Name (auch
/// `042` oder `0`), ein Symlink, ein Verzeichnis, ein Gerät. Fremde Einträge
/// werden nie verfolgt und nie entfernt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexScan {
    /// Kernel-IDs der gefundenen Marker, aufsteigend.
    pub markers: Vec<CgroupKernelId>,
    /// Zahl der fremden Einträge (ohne `.` und `..`).
    pub foreign: usize,
    /// Ob das Verzeichnis mehr als [`MAX_INDEX_ENTRIES`] Einträge hat; die
    /// übrigen wurden nicht gelesen.
    pub truncated: bool,
}

/// Was [`MarkerDir::create`] vorgefunden bzw. getan hat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerCreation {
    /// Der Marker wurde neu angelegt.
    Created,
    /// Ein Marker des Wardens (reguläre Datei des effektiven Nutzers) lag
    /// schon vor, etwa von einer früheren Eindämmung derselben cgroup.
    AlreadyPresent,
    /// Eine reguläre Datei eines anderen Eigentümers lag unter dem Namen; sie
    /// wurde entfernt und der Marker neu angelegt.
    Replaced,
}

/// Das Indexverzeichnis der Eindämmungsmarker (Produktion: [`DEFAULT_HOLDS_DIR`]).
///
/// # Description
/// Siehe Moduldoku. Der Konstruktor greift nicht aufs Dateisystem zu; jede
/// Operation öffnet und prüft das Verzeichnis neu, damit sie dasselbe
/// Verzeichnis sieht wie harw.
///
/// # Concurrency
/// Zustandslos, `Send + Sync`.
#[derive(Debug, Clone)]
pub struct ContainmentMarkers {
    dir: PathBuf,
}

impl ContainmentMarkers {
    /// Builds the marker index over `dir` (no filesystem access).
    ///
    /// # Arguments
    /// - `dir` (`impl Into<PathBuf>`): absoluter Pfad des Indexverzeichnisses.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Returns the index directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Opens and checks the index directory without locking it.
    ///
    /// # Errors
    /// Wie [`Self::open_locked`].
    pub fn open(&self) -> WardenResult<MarkerDir> {
        Ok(MarkerDir {
            dir: self.open_checked()?,
            locked: false,
        })
    }

    /// Opens and checks the index directory and tries `flock(LOCK_EX | LOCK_NB)` once.
    ///
    /// # Description
    /// Genau ein Versuch, kein Warten: schlägt er fehl (ein anderer Prozess
    /// hält die Sperre, oder `flock` ist nicht verfügbar), liefert die
    /// Methode trotzdem das geöffnete Verzeichnis mit
    /// [`MarkerDir::is_locked`] = `false`. Die Sperre gilt, bis das
    /// [`MarkerDir`] fällt.
    ///
    /// # Errors
    /// - [`WardenError::UntrustedFile`]: Symlink, kein Verzeichnis, fremder
    ///   Eigentümer oder Schreibrecht für Gruppe/andere.
    /// - [`WardenError::Io`]: das Verzeichnis fehlt oder lässt sich nicht
    ///   öffnen bzw. prüfen.
    pub fn open_locked(&self) -> WardenResult<MarkerDir> {
        let dir = self.open_checked()?;
        // Bewusst nur ein Versuch ohne Warten (Moduldoku, Vertrag mit harw).
        let locked = rustix::fs::flock(&dir, FlockOperation::NonBlockingLockExclusive).is_ok();
        Ok(MarkerDir { dir, locked })
    }

    /// Removes the marker of `kernel_id` (only a regular file, never through a link).
    ///
    /// # Returns
    /// `true`, wenn ein Marker entfernt wurde; `false`, wenn keiner da war.
    ///
    /// # Errors
    /// Wie [`Self::open`] und [`MarkerDir::remove`].
    pub fn remove(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        self.open()?.remove(kernel_id)
    }

    /// Returns whether a marker (regular file) of `kernel_id` exists.
    ///
    /// # Errors
    /// Wie [`Self::open`] und [`MarkerDir::is_present`].
    pub fn is_present(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        self.open()?.is_present(kernel_id)
    }

    // Öffnet das Verzeichnis ohne Symlink-Folgen und prüft Art, Eigentümer
    // und Modus am geöffneten Deskriptor.
    fn open_checked(&self) -> WardenResult<File> {
        let fd = rustix::fs::openat(
            rustix::fs::CWD,
            self.dir.as_path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|errno| {
            if errno == Errno::LOOP || errno == Errno::NOTDIR {
                untrusted(HOLDS_DIR_WHAT, "not a plain directory")
            } else {
                io_error(errno)
            }
        })?;
        let stat = rustix::fs::fstat(&fd).map_err(io_error)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(untrusted(HOLDS_DIR_WHAT, "not a plain directory"));
        }
        if stat.st_uid != rustix::process::geteuid().as_raw() {
            return Err(untrusted(
                HOLDS_DIR_WHAT,
                "owner is not the warden's effective user",
            ));
        }
        if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
            return Err(untrusted(HOLDS_DIR_WHAT, "writable by group or others"));
        }
        Ok(File::from(fd))
    }
}

/// Das geöffnete, geprüfte Indexverzeichnis (ggf. mit Sperre).
///
/// # Description
/// Alle Marker-Zugriffe laufen relativ zu diesem Deskriptor. Fällt der Wert,
/// wird der Deskriptor geschlossen und eine gehaltene `flock`-Sperre damit
/// freigegeben (`O_CLOEXEC`: ein `nft`-Kindprozess erbt sie nie).
///
/// # Concurrency
/// Gehört dem Aufrufer; nicht geteilt.
#[derive(Debug)]
pub struct MarkerDir {
    dir: File,
    locked: bool,
}

impl MarkerDir {
    /// Returns whether this handle holds the exclusive lock of the index.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Creates the marker of `kernel_id` (`O_CREAT | O_EXCL | O_NOFOLLOW`, mode `0644`).
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitt „Keine Symlinks, nur eigene Verzeichnisse“
    /// für einen schon vorhandenen Eintrag.
    ///
    /// # Returns
    /// Was vorgefunden bzw. getan wurde.
    ///
    /// # Errors
    /// - [`WardenError::UntrustedFile`]: unter dem Namen liegt keine
    ///   reguläre Datei (Symlink, Verzeichnis, ...); nichts wurde verändert.
    /// - [`WardenError::Io`]: Anlegen, Prüfen oder Entfernen scheitert.
    pub fn create(&self, kernel_id: CgroupKernelId) -> WardenResult<MarkerCreation> {
        let name = marker_name(kernel_id);
        match self.create_exclusive(&name) {
            Ok(()) => return Ok(MarkerCreation::Created),
            Err(errno) if errno == Errno::EXIST => {}
            Err(errno) => return Err(io_error(errno)),
        }
        let stat = rustix::fs::statat(&self.dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io_error)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(untrusted(MARKER_WHAT, "not a regular file"));
        }
        if stat.st_uid == rustix::process::geteuid().as_raw() {
            return Ok(MarkerCreation::AlreadyPresent);
        }
        // Eine reguläre Datei eines anderen Eigentümers: entfernen (`unlinkat`
        // folgt nie einem Symlink) und den eigenen Marker neu anlegen.
        rustix::fs::unlinkat(&self.dir, name.as_str(), AtFlags::empty()).map_err(io_error)?;
        self.create_exclusive(&name).map_err(io_error)?;
        Ok(MarkerCreation::Replaced)
    }

    /// Removes the marker of `kernel_id` if it is a regular file.
    ///
    /// # Returns
    /// `true`, wenn ein Marker entfernt wurde; `false`, wenn keiner da war.
    ///
    /// # Errors
    /// - [`WardenError::UntrustedFile`]: unter dem Namen liegt keine reguläre
    ///   Datei; sie wird nicht entfernt (und nie verfolgt).
    /// - [`WardenError::Io`]: Prüfen oder Entfernen scheitert.
    pub fn remove(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        let name = marker_name(kernel_id);
        if !self.is_present(kernel_id)? {
            return Ok(false);
        }
        match rustix::fs::unlinkat(&self.dir, name.as_str(), AtFlags::empty()) {
            Ok(()) => Ok(true),
            Err(errno) if errno == Errno::NOENT => Ok(false),
            Err(errno) => Err(io_error(errno)),
        }
    }

    /// Returns whether a marker (regular file) of `kernel_id` exists (`fstatat` without following links).
    ///
    /// # Errors
    /// - [`WardenError::UntrustedFile`]: unter dem Namen liegt keine reguläre
    ///   Datei.
    /// - [`WardenError::Io`]: die Prüfung scheitert.
    pub fn is_present(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        let name = marker_name(kernel_id);
        match rustix::fs::statat(&self.dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => Ok(true),
            Ok(_) => Err(untrusted(MARKER_WHAT, "not a regular file")),
            Err(errno) if errno == Errno::NOENT => Ok(false),
            Err(errno) => Err(io_error(errno)),
        }
    }

    /// Lists the markers in the index (`getdents64`, never following an entry).
    ///
    /// # Description
    /// Liest über einen eigenen Deskriptor (`openat(".")` relativ zum
    /// geprüften Verzeichnis, ohne `fcntl`; eine gehaltene Sperre bleibt
    /// bestehen) höchstens [`MAX_INDEX_ENTRIES`] Einträge. Siehe
    /// [`IndexScan`] dafür, was als Marker zählt.
    ///
    /// # Returns
    /// Die Marker (aufsteigend), die Zahl fremder Einträge und ob die Liste
    /// abgeschnitten wurde.
    ///
    /// # Errors
    /// - [`WardenError::Io`]: Öffnen, Lesen oder Prüfen eines Eintrags
    ///   scheitert.
    ///
    /// # Examples
    /// ```rust
    /// use std::os::unix::fs::PermissionsExt as _;
    ///
    /// use harw_dod_warden::containment::ContainmentMarkers;
    /// use harw_dod_warden_proto::CgroupKernelId;
    ///
    /// let Ok(run) = tempfile::tempdir() else { return };
    /// let holds = run.path().join("holds");
    /// let Ok(()) = std::fs::create_dir(&holds) else { return };
    /// let Ok(()) = std::fs::set_permissions(&holds, std::fs::Permissions::from_mode(0o755)) else {
    ///     return;
    /// };
    /// let Ok(kernel_id) = CgroupKernelId::new(42) else { return };
    /// let Ok(dir) = ContainmentMarkers::new(&holds).open() else { return };
    /// let Ok(_) = dir.create(kernel_id) else { return };
    /// let Ok(()) = std::fs::write(holds.join("notes"), b"") else { return };
    /// let Ok(scan) = dir.scan() else { return };
    /// assert_eq!(scan.markers, vec![kernel_id]);
    /// assert_eq!(scan.foreign, 1);
    /// ```
    pub fn scan(&self) -> WardenResult<IndexScan> {
        let listing = rustix::fs::openat(
            &self.dir,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)?;
        let entries = rustix::fs::Dir::new(listing).map_err(io_error)?;
        let mut scan = IndexScan::default();
        let mut seen: usize = 0;
        for entry in entries {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if seen >= MAX_INDEX_ENTRIES {
                scan.truncated = true;
                break;
            }
            seen = seen.saturating_add(1);
            let Some(kernel_id) = parse_marker_name(name) else {
                scan.foreign = scan.foreign.saturating_add(1);
                continue;
            };
            // Art des Eintrags über `fstatat` ohne Symlink-Folgen, nicht über
            // `d_type` (das manche Dateisysteme nicht liefern).
            match self.is_present(kernel_id) {
                Ok(true) => scan.markers.push(kernel_id),
                // Zwischen Auflisten und Prüfen entfernt.
                Ok(false) => {}
                Err(WardenError::UntrustedFile { .. }) => {
                    scan.foreign = scan.foreign.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
        scan.markers.sort_unstable();
        Ok(scan)
    }

    // `O_CREAT | O_EXCL` scheitert an jedem vorhandenen Eintrag, auch an einem
    // Symlink (`EEXIST`, ohne ihm zu folgen). Modus `0644` vor der umask
    // (harw liest nur Art und Eigentümer).
    fn create_exclusive(&self, name: &str) -> rustix::io::Result<()> {
        let marker = rustix::fs::openat(
            &self.dir,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        )?;
        drop(marker);
        Ok(())
    }
}

// Die Kernel-ID eines Markernamens: nur die kanonische Dezimalform (keine
// führende Null, kein Vorzeichen, nicht `0`), also genau `marker_name(id)`.
fn parse_marker_name(name: &[u8]) -> Option<CgroupKernelId> {
    if name.is_empty() || !name.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let text = std::str::from_utf8(name).ok()?;
    let kernel_id = CgroupKernelId::new(text.parse::<u64>().ok()?).ok()?;
    (marker_name(kernel_id) == text).then_some(kernel_id)
}

// `UntrustedFile` mit festen Texten.
fn untrusted(what: &'static str, reason: &'static str) -> WardenError {
    WardenError::UntrustedFile { what, reason }
}

// Ein `rustix`-Fehler als `Io`.
fn io_error(errno: Errno) -> WardenError {
    WardenError::Io(std::io::Error::from(errno))
}

/// Eine vom Warden geführte Eindämmung (ein Freeze und sein Marker).
///
/// # Description
/// `freeze_was_set` ist `None`, solange das Bit noch nicht gelesen ist
/// (Datensatz vor Marker, siehe Moduldoku); danach hält es fest, ob
/// `cgroup.freeze` beim Eindämmen schon `1` war. Nur bei `Some(false)`
/// gehört das Bit dem Warden ([`Self::owns_freeze_bit`]), und nur dann taut
/// das Freigeben auf. `released` wird beim Freigeben gesetzt (für ein
/// fremdes Bit vor allem anderen, für das eigene direkt nach dem Auftauen)
/// und muss dauerhaft stehen, **bevor** der Marker entfernt wird: bleibt der
/// Datensatz danach stehen (Marker nicht entfernbar, oder Absturz vor dem
/// Entfernen des Datensatzes), räumen der Hold-Timer und der Abgleich beim
/// Start nur noch den Marker und den Datensatz ab und legen nie einen Marker
/// neu an.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentEntry {
    /// Das signierte Ziel-Tripel; der Marker heißt nach `target.kernel_id`.
    pub target: CgroupTarget,
    /// Ob `cgroup.freeze` beim Eindämmen schon gesetzt war (`None`: noch
    /// nicht gelesen, die Eindämmung ist nicht abgeschlossen).
    pub freeze_was_set: Option<bool>,
    /// Ob die Freigabe schon lief und nur der Marker noch zu entfernen ist.
    #[serde(default)]
    pub released: bool,
}

impl ContainmentEntry {
    /// Builds the record a freeze writes before its marker (not completed yet).
    #[must_use]
    pub fn containing(target: CgroupTarget) -> Self {
        Self {
            target,
            freeze_was_set: None,
            released: false,
        }
    }

    /// Returns whether `freeze_was_set` is recorded (the containment got past its marker and read).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.freeze_was_set.is_some()
    }

    /// Returns whether the containment is in force (complete and not released yet).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.is_complete() && !self.released
    }

    /// Returns whether the warden wrote the freeze bit itself (`freeze_was_set == Some(false)`).
    ///
    /// # Description
    /// `false` auch für einen nicht abgeschlossenen Datensatz: der Warden
    /// schreibt `cgroup.freeze` = 1 erst, nachdem `freeze_was_set` feststeht;
    /// ein gesetztes Bit stammt dann von einem anderen Eigentümer.
    #[must_use]
    pub fn owns_freeze_bit(&self) -> bool {
        self.freeze_was_set == Some(false)
    }
}

// Dateiform des Schnappschusses; `boot_id` fehlt, wenn sie beim Schreiben
// unbekannt war.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContainmentFile {
    version: u16,
    #[serde(default)]
    boot_id: Option<String>,
    containments: Vec<ContainmentEntry>,
}

// Laufender Bestand, die beim Öffnen gefundenen Datensätze eines früheren
// (oder nicht nachweisbar desselben) Boots (siehe Moduldoku, „Boot-ID“) und
// ob der Bestand im Speicher vom zuletzt geschriebenen abweicht (siehe
// Moduldoku, „Nur im Speicher gebucht“).
#[derive(Debug, Default)]
struct Records {
    current: Vec<ContainmentEntry>,
    previous_boot: Vec<ContainmentEntry>,
    unsaved: bool,
}

/// Persistenter Speicher der Eindämmungen im StateDirectory (`containment.v1.json`).
///
/// # Description
/// Je Kernel-ID höchstens ein Datensatz. [`Self::record`] und
/// [`Self::record_release`] schreiben zuerst dauerhaft und übernehmen erst
/// danach (vor dem Eingriff: ohne Datensatz kein Marker; vor der Freigabe
/// eines fremden Bits: ohne dauerhafte Buchung keine Freigabe).
/// [`Self::activate`], [`Self::mark_released`], [`Self::restore`],
/// [`Self::forget`], [`Self::forget_many`] und
/// [`Self::discard_previous_boot`] buchen nach einem Eingriff und übernehmen
/// ihre Änderung **immer** im Speicher; sie melden nur, ob sie auch dauerhaft
/// geschrieben wurde (wie `HoldStore::forget`). Ein so ungesicherter Stand
/// bleibt markiert, bis ein Schreiben gelingt ([`Self::flush`]). Die Datei
/// trägt die Boot-ID des schreibenden Boots; Datensätze eines anderen oder
/// nicht nachweisbar desselben Boots liegen nach dem Öffnen nur in
/// [`Self::previous_boot`] (siehe Moduldoku, „Boot-ID“).
///
/// # Concurrency
/// Ein `Mutex` um beide Listen.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::containment::ContainmentStore;
///
/// let Ok(dir) = tempfile::tempdir() else { return };
/// let Ok(store) = ContainmentStore::open(dir.path()) else { return };
/// assert_eq!(store.snapshot().map(|entries| entries.len()).ok(), Some(0));
/// ```
#[derive(Debug)]
pub struct ContainmentStore {
    dir: PathBuf,
    boot_id: Option<String>,
    records: Mutex<Records>,
}

impl ContainmentStore {
    /// Opens the store in `dir` for the running boot ([`current_boot_id`]); a missing file means "no containments".
    ///
    /// # Errors
    /// Wie [`Self::open_at_boot`].
    pub fn open(dir: &Path) -> WardenResult<Self> {
        Self::open_at_boot(dir, current_boot_id())
    }

    /// Opens the store in `dir` for the boot `boot_id` (`None`: unknown).
    ///
    /// # Description
    /// Nur wenn die Datei dieselbe Boot-ID trägt wie `boot_id` (beide
    /// bekannt), kommen ihre Datensätze in den laufenden Bestand; sonst
    /// (andere Boot-ID, `boot_id` unbekannt, oder die Datei trägt keine)
    /// nach [`Self::previous_boot`] (fail closed, siehe Moduldoku,
    /// „Boot-ID“).
    ///
    /// # Errors
    /// - [`WardenError::LedgerCorrupt`]: Datei unlesbar, falsche Version,
    ///   doppelte Kernel-ID oder mehr als [`MAX_CONTAINMENTS`] Einträge.
    /// - [`WardenError::Io`]: Lesen scheitert (auch ein Symlink an Stelle der
    ///   Datei).
    pub fn open_at_boot(dir: &Path, boot_id: Option<String>) -> WardenResult<Self> {
        let corrupt = || WardenError::LedgerCorrupt {
            ledger: "containment",
            line: 0,
        };
        let (stored_boot, containments) = match fsutil::read_optional(dir, CONTAINMENT_FILE)? {
            None => (None, Vec::new()),
            Some(bytes) => {
                let file: ContainmentFile =
                    serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
                if file.version != CONTAINMENT_FORMAT_VERSION
                    || file.containments.len() > MAX_CONTAINMENTS
                {
                    return Err(corrupt());
                }
                let mut ids: Vec<u64> = file
                    .containments
                    .iter()
                    .map(|entry| entry.target.kernel_id.get())
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                if ids.len() != file.containments.len() {
                    return Err(corrupt());
                }
                (file.boot_id, file.containments)
            }
        };
        // Nur eine nachweislich gleiche Boot-ID macht Kernel-IDs vergleichbar.
        let same_boot = matches!(
            (&stored_boot, &boot_id),
            (Some(stored), Some(current)) if stored == current
        );
        let records = if same_boot {
            Records {
                current: containments,
                ..Records::default()
            }
        } else {
            Records {
                previous_boot: containments,
                ..Records::default()
            }
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            boot_id,
            records: Mutex::new(records),
        })
    }

    /// Returns the boot id this store writes into its file (`None`: unknown).
    #[must_use]
    pub fn boot_id(&self) -> Option<&str> {
        self.boot_id.as_deref()
    }

    /// Records (or replaces) the containment of `entry.target.kernel_id`; durable first.
    ///
    /// # Errors
    /// - [`WardenError::LedgerUnavailable`]: bereits [`MAX_CONTAINMENTS`]
    ///   Datensätze anderer Kernel-IDs (siehe [`Self::has_room_for`]).
    /// - [`WardenError::StatePoisoned`] / [`WardenError::Encode`] /
    ///   [`WardenError::Io`]: Sperre oder Schreiben scheitert (alter Stand
    ///   bleibt).
    pub fn record(&self, entry: ContainmentEntry) -> WardenResult<()> {
        let mut records = self.lock()?;
        let kernel_id = entry.target.kernel_id;
        let mut next: Vec<ContainmentEntry> = records
            .current
            .iter()
            .filter(|existing| existing.target.kernel_id != kernel_id)
            .cloned()
            .collect();
        if next.len() >= MAX_CONTAINMENTS {
            return Err(WardenError::LedgerUnavailable {
                ledger: "containment",
                reason: "full",
            });
        }
        next.push(entry);
        self.persist(&next)?;
        records.current = next;
        // Geschrieben ist der ganze Bestand, auch ein vorher ungesicherter.
        records.unsaved = false;
        Ok(())
    }

    /// Marks the containment of `kernel_id` as released; durable first (like [`Self::record`]).
    ///
    /// # Description
    /// Für die Freigabe eines Freeze, dessen Bit nicht dem Warden gehört
    /// (`freeze_was_set` ≠ `Some(false)`): sie wirkt nur über diese Buchung
    /// (kein `cgroup.freeze` = 0), und ein Absturz danach darf den Marker nie
    /// zurückbringen. Scheitert das Schreiben, bleibt der Speicher unverändert,
    /// und die Freigabe gilt als nicht gelaufen.
    ///
    /// # Returns
    /// Ob ein Datensatz für `kernel_id` existierte (ohne Datensatz wird nichts
    /// geschrieben).
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`] / [`WardenError::Encode`] /
    ///   [`WardenError::Io`]: Sperre oder Schreiben scheitert (alter Stand
    ///   bleibt).
    pub fn record_release(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        let mut records = self.lock()?;
        let mut next = records.current.clone();
        let Some(entry) = next
            .iter_mut()
            .find(|entry| entry.target.kernel_id == kernel_id)
        else {
            return Ok(false);
        };
        entry.released = true;
        self.persist(&next)?;
        records.current = next;
        records.unsaved = false;
        Ok(true)
    }

    /// Writes the in-memory records if an earlier write failed (see the module docs, "Nur im Speicher gebucht").
    ///
    /// # Description
    /// Ohne ungesicherten Stand schreibt die Methode nichts. Danach entspricht
    /// die Datei dem Bestand im Speicher, auch einer dort schon gebuchten
    /// Freigabe.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    /// - [`WardenError::Encode`] / [`WardenError::Io`]: das Schreiben
    ///   scheitert erneut (der Stand bleibt ungesichert).
    pub fn flush(&self) -> WardenResult<()> {
        let mut records = self.lock()?;
        if !records.unsaved {
            return Ok(());
        }
        self.sync(&mut records)
    }

    /// Returns whether [`Self::record`] has room for a record of `kernel_id`.
    ///
    /// # Returns
    /// `true`, wenn weniger als [`MAX_CONTAINMENTS`] Datensätze anderer
    /// Kernel-IDs bestehen.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn has_room_for(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        let others = self
            .lock()?
            .current
            .iter()
            .filter(|entry| entry.target.kernel_id != kernel_id)
            .count();
        Ok(others < MAX_CONTAINMENTS)
    }

    /// Completes the containment of `kernel_id` with `freeze_was_set`; memory first.
    ///
    /// # Returns
    /// Ob ein Datensatz für `kernel_id` existierte.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet (nichts geändert).
    /// - [`WardenError::Encode`] / [`WardenError::Io`]: nur das Schreiben
    ///   scheiterte; der Speicherstand ist bereits geändert.
    pub fn activate(&self, kernel_id: CgroupKernelId, freeze_was_set: bool) -> WardenResult<bool> {
        self.update(kernel_id, |entry| {
            entry.freeze_was_set = Some(freeze_was_set)
        })
    }

    /// Marks the containment of `kernel_id` as released (only its marker is left); memory first.
    ///
    /// # Description
    /// Die Freigabe eines eigenen Bits bucht das direkt nach dem Auftauen,
    /// vor dem Ledger und vor dem Entfernen des Markers (ein fremdes Bit
    /// bucht [`Self::record_release`]). Bleibt der Datensatz danach stehen
    /// (Marker nicht entfernbar, Absturz), entfernen der Hold-Timer und der
    /// Abgleich beim Start den Marker erneut und danach den Datensatz.
    /// Scheitert nur das Schreiben, gilt die Freigabe im Speicher, ist aber
    /// ungesichert: der Marker bleibt dann, bis [`Self::flush`] gelingt.
    ///
    /// # Returns
    /// Ob ein Datensatz für `kernel_id` existierte.
    ///
    /// # Errors
    /// Wie [`Self::activate`].
    pub fn mark_released(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        self.update(kernel_id, |entry| entry.released = true)
    }

    /// Puts back `previous` for `kernel_id` (or removes the record); memory first.
    ///
    /// # Description
    /// Für einen gescheiterten Freeze: der vorab gestellte Datensatz weicht
    /// dem vorherigen Stand.
    ///
    /// # Errors
    /// Wie [`Self::activate`].
    pub fn restore(
        &self,
        kernel_id: CgroupKernelId,
        previous: Option<ContainmentEntry>,
    ) -> WardenResult<()> {
        let mut records = self.lock()?;
        records
            .current
            .retain(|entry| entry.target.kernel_id != kernel_id);
        if let Some(previous) = previous {
            records.current.push(previous);
        }
        self.sync(&mut records)
    }

    /// Removes the record of `kernel_id`; memory first.
    ///
    /// # Returns
    /// Ob ein Datensatz für `kernel_id` existierte.
    ///
    /// # Errors
    /// Wie [`Self::activate`].
    pub fn forget(&self, kernel_id: CgroupKernelId) -> WardenResult<bool> {
        let mut records = self.lock()?;
        let before = records.current.len();
        records
            .current
            .retain(|entry| entry.target.kernel_id != kernel_id);
        if records.current.len() == before {
            return Ok(false);
        }
        self.sync(&mut records)?;
        Ok(true)
    }

    /// Removes the records of all `kernel_ids` with one write; memory first.
    ///
    /// # Description
    /// Für den Durchlauf des Hold-Timers, der viele Datensätze verschwundener
    /// cgroups auf einmal abräumen kann (ein Schreibvorgang statt einer je
    /// Datensatz).
    ///
    /// # Returns
    /// Wie viele Datensätze entfernt wurden.
    ///
    /// # Errors
    /// Wie [`Self::activate`].
    pub fn forget_many(&self, kernel_ids: &[CgroupKernelId]) -> WardenResult<usize> {
        let mut records = self.lock()?;
        let before = records.current.len();
        records
            .current
            .retain(|entry| !kernel_ids.contains(&entry.target.kernel_id));
        let removed = before.saturating_sub(records.current.len());
        if removed == 0 {
            return Ok(0);
        }
        self.sync(&mut records)?;
        Ok(removed)
    }

    /// Returns the record of `kernel_id`, if any (current boot only).
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn get(&self, kernel_id: CgroupKernelId) -> WardenResult<Option<ContainmentEntry>> {
        Ok(self
            .lock()?
            .current
            .iter()
            .find(|entry| entry.target.kernel_id == kernel_id)
            .cloned())
    }

    /// Returns all records of the current boot.
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn snapshot(&self) -> WardenResult<Vec<ContainmentEntry>> {
        Ok(self.lock()?.current.clone())
    }

    /// Returns the records found in a file of another (or not verifiably the same) boot (see the module docs, "Boot-ID").
    ///
    /// # Errors
    /// - [`WardenError::StatePoisoned`]: Mutex vergiftet.
    pub fn previous_boot(&self) -> WardenResult<Vec<ContainmentEntry>> {
        Ok(self.lock()?.previous_boot.clone())
    }

    /// Drops the records of another boot and rewrites the file for this boot; memory first.
    ///
    /// # Description
    /// Ohne Datensätze eines anderen Boots tut die Methode nichts.
    ///
    /// # Errors
    /// Wie [`Self::activate`].
    pub fn discard_previous_boot(&self) -> WardenResult<()> {
        let mut records = self.lock()?;
        if records.previous_boot.is_empty() {
            return Ok(());
        }
        records.previous_boot.clear();
        self.sync(&mut records)
    }

    // Ändert den Datensatz von `kernel_id` im Speicher und schreibt danach.
    fn update(
        &self,
        kernel_id: CgroupKernelId,
        change: impl FnOnce(&mut ContainmentEntry),
    ) -> WardenResult<bool> {
        let mut records = self.lock()?;
        let Some(entry) = records
            .current
            .iter_mut()
            .find(|entry| entry.target.kernel_id == kernel_id)
        else {
            return Ok(false);
        };
        change(entry);
        self.sync(&mut records)?;
        Ok(true)
    }

    // Die Sperre um den ganzen Bestand.
    fn lock(&self) -> WardenResult<std::sync::MutexGuard<'_, Records>> {
        self.records.lock().map_err(|_| WardenError::StatePoisoned)
    }

    // Schreibt den Bestand im Speicher und merkt sich, ob er damit gesichert
    // ist (jedes Schreiben schreibt den ganzen Bestand).
    fn sync(&self, records: &mut Records) -> WardenResult<()> {
        let written = self.persist(&records.current);
        records.unsaved = written.is_err();
        written
    }

    // Schreibt den Schnappschuss (mit der Boot-ID dieses Boots) atomar.
    fn persist(&self, entries: &[ContainmentEntry]) -> WardenResult<()> {
        let file = ContainmentFile {
            version: CONTAINMENT_FORMAT_VERSION,
            boot_id: self.boot_id.clone(),
            containments: entries.to_vec(),
        };
        let bytes = serde_json::to_vec(&file).map_err(WardenError::Encode)?;
        fsutil::replace_file_atomically(&self.dir, CONTAINMENT_FILE, &bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use super::{
        CONTAINMENT_FILE, ContainmentEntry, ContainmentMarkers, ContainmentStore, MAX_CONTAINMENTS,
        MarkerCreation, marker_name, parse_boot_id,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
    use harw_types::CgroupId;
    use jiff::Timestamp;

    const KERNEL_ID: u64 = 4_294_967_298;

    fn kernel_id(raw: u64) -> TestResult<CgroupKernelId> {
        CgroupKernelId::new(raw).map_err(ctx("kernel id"))
    }

    fn target(path: &str, raw: u64) -> TestResult<CgroupTarget> {
        Ok(CgroupTarget::new(
            CgroupId::try_from_str(path).map_err(ctx("cgroup"))?,
            kernel_id(raw)?,
            Timestamp::from_second(1_700_000_000).map_err(ctx("ts"))?,
        ))
    }

    // `<tmp>/holds` mit Modus 0755 (unabhängig von der umask), wie tmpfiles es
    // für den Warden-Nutzer anlegt.
    fn holds_dir(run: &tempfile::TempDir) -> TestResult<std::path::PathBuf> {
        let holds = run.path().join("holds");
        std::fs::create_dir(&holds).map_err(ctx("mkdir holds"))?;
        std::fs::set_permissions(&holds, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod holds"))?;
        Ok(holds)
    }

    // -- Marker ----------------------------------------------------------------

    #[test]
    fn test_marker_is_a_regular_file_harw_accepts() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let markers = ContainmentMarkers::new(&holds);
        let dir = markers.open_locked().map_err(ctx("open locked"))?;
        assert!(dir.is_locked());
        assert_eq!(
            dir.create(kernel_id(KERNEL_ID)?).map_err(ctx("create"))?,
            MarkerCreation::Created
        );
        // Dieselben Regeln wie `harw_tool_job::MarkerDirHolds::is_held`:
        // reguläre Datei, Eigentümer = Eigentümer des Index.
        let marker = std::fs::symlink_metadata(holds.join("4294967298")).map_err(ctx("lstat"))?;
        let index = std::fs::symlink_metadata(&holds).map_err(ctx("lstat index"))?;
        assert!(marker.file_type().is_file());
        assert_eq!(marker.uid(), index.uid());
        assert_eq!(marker.mode() & 0o022, 0);
        assert_eq!(marker.len(), 0);
        // Ein zweites Anlegen findet den eigenen Marker vor.
        assert_eq!(
            dir.create(kernel_id(KERNEL_ID)?)
                .map_err(ctx("create again"))?,
            MarkerCreation::AlreadyPresent
        );
        Ok(())
    }

    #[test]
    fn test_remove_takes_only_the_marker_and_reports_absence() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let markers = ContainmentMarkers::new(&holds);
        let id = kernel_id(KERNEL_ID)?;
        assert!(!markers.remove(id).map_err(ctx("remove absent"))?);
        assert!(!markers.is_present(id).map_err(ctx("absent"))?);
        markers
            .open()
            .map_err(ctx("open"))?
            .create(id)
            .map_err(ctx("create"))?;
        assert!(markers.is_present(id).map_err(ctx("present"))?);
        let other = kernel_id(77)?;
        markers
            .open()
            .map_err(ctx("open"))?
            .create(other)
            .map_err(ctx("create other"))?;
        assert!(markers.remove(id).map_err(ctx("remove"))?);
        assert!(!holds.join(marker_name(id)).exists());
        assert!(holds.join(marker_name(other)).exists());
        Ok(())
    }

    #[test]
    fn test_symlinked_holds_directory_is_refused() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = holds_dir(&run)?;
        let link = run.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        let markers = ContainmentMarkers::new(&link);
        assert!(matches!(
            markers.open_locked(),
            Err(WardenError::UntrustedFile { .. })
        ));
        assert!(markers.remove(kernel_id(KERNEL_ID)?).is_err());
        // Nichts wurde im echten Verzeichnis angelegt.
        let entries = std::fs::read_dir(&real).map_err(ctx("read dir"))?.count();
        assert_eq!(entries, 0);
        Ok(())
    }

    #[test]
    fn test_writable_or_missing_holds_directory_is_refused() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        std::fs::set_permissions(&holds, std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod 0777"))?;
        assert!(matches!(
            ContainmentMarkers::new(&holds).open_locked(),
            Err(WardenError::UntrustedFile { .. })
        ));
        let missing = ContainmentMarkers::new(run.path().join("missing"));
        let Err(error) = missing.open_locked() else {
            return Err(TestError::Unexpected("a missing index must fail".into()));
        };
        assert!(matches!(error, WardenError::Io(_)));
        let file = run.path().join("file");
        std::fs::write(&file, b"").map_err(ctx("write file"))?;
        assert!(matches!(
            ContainmentMarkers::new(&file).open_locked(),
            Err(WardenError::UntrustedFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_symlinked_or_directory_marker_is_never_followed_or_removed() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let outside = run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        let linked = kernel_id(KERNEL_ID)?;
        std::os::unix::fs::symlink(&outside, holds.join(marker_name(linked)))
            .map_err(ctx("symlink marker"))?;
        let dir_id = kernel_id(78)?;
        std::fs::create_dir(holds.join(marker_name(dir_id))).map_err(ctx("mkdir marker"))?;
        let markers = ContainmentMarkers::new(&holds);
        let dir = markers.open_locked().map_err(ctx("open"))?;
        for id in [linked, dir_id] {
            if !matches!(dir.create(id), Err(WardenError::UntrustedFile { .. })) {
                return Err(TestError::Unexpected(format!("{id} must be refused")));
            }
            if !matches!(markers.remove(id), Err(WardenError::UntrustedFile { .. })) {
                return Err(TestError::Unexpected(format!("{id} must not be removed")));
            }
            assert!(markers.is_present(id).is_err());
        }
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("read outside"))?,
            b"foreign"
        );
        assert!(
            std::fs::symlink_metadata(holds.join(marker_name(linked)))
                .map_err(ctx("lstat link"))?
                .file_type()
                .is_symlink()
        );
        assert!(holds.join(marker_name(dir_id)).is_dir());
        Ok(())
    }

    #[test]
    fn test_foreign_regular_file_is_replaced_by_an_own_marker() -> TestResult {
        // Eine fremde Eigentümerschaft lässt sich nur als root herstellen;
        // ohne root prüft der Test nichts.
        if rustix::process::geteuid().as_raw() != 0 {
            return Ok(());
        }
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let id = kernel_id(KERNEL_ID)?;
        let marker = holds.join(marker_name(id));
        std::fs::write(&marker, b"foreign").map_err(ctx("foreign file"))?;
        std::os::unix::fs::chown(&marker, Some(65_534), Some(65_534)).map_err(ctx("chown"))?;
        let dir = ContainmentMarkers::new(&holds)
            .open_locked()
            .map_err(ctx("open"))?;
        assert_eq!(
            dir.create(id).map_err(ctx("create"))?,
            MarkerCreation::Replaced
        );
        let replaced = std::fs::symlink_metadata(&marker).map_err(ctx("lstat"))?;
        assert!(replaced.file_type().is_file());
        assert_eq!(replaced.uid(), 0);
        assert_eq!(replaced.len(), 0);
        Ok(())
    }

    #[test]
    fn test_contended_lock_still_opens_the_index() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        // Ein anderer Teilnehmer (eigene Dateibeschreibung) hält die Sperre.
        let other = std::fs::File::open(&holds).map_err(ctx("open other"))?;
        other.lock().map_err(ctx("lock other"))?;
        let markers = ContainmentMarkers::new(&holds);
        let dir = markers.open_locked().map_err(ctx("open contended"))?;
        assert!(!dir.is_locked());
        assert_eq!(
            dir.create(kernel_id(KERNEL_ID)?).map_err(ctx("create"))?,
            MarkerCreation::Created
        );
        drop(dir);
        other.unlock().map_err(ctx("unlock other"))?;
        let relocked = markers.open_locked().map_err(ctx("open free"))?;
        assert!(relocked.is_locked());
        Ok(())
    }

    #[test]
    fn test_scan_lists_only_canonical_regular_markers_and_follows_nothing() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let markers = ContainmentMarkers::new(&holds);
        let dir = markers.open_locked().map_err(ctx("open locked"))?;
        for raw in [KERNEL_ID, 7] {
            dir.create(kernel_id(raw)?).map_err(ctx("create marker"))?;
        }
        // Fremde Einträge: nicht kanonische Namen, ein Nicht-Zahlname, ein
        // Symlink und ein Verzeichnis unter Kernel-ID-Namen.
        for name in ["0042", "0", "+5", "notes", "18446744073709551616"] {
            std::fs::write(holds.join(name), b"").map_err(ctx("foreign file"))?;
        }
        let outside = run.path().join("outside");
        std::fs::write(&outside, b"foreign").map_err(ctx("write outside"))?;
        std::os::unix::fs::symlink(&outside, holds.join("99")).map_err(ctx("symlink"))?;
        std::fs::create_dir(holds.join("100")).map_err(ctx("mkdir"))?;

        let scan = dir.scan().map_err(ctx("scan"))?;
        assert_eq!(scan.markers, vec![kernel_id(7)?, kernel_id(KERNEL_ID)?]);
        assert_eq!(scan.foreign, 7);
        assert!(!scan.truncated);
        // Die Sperre hält weiter, nichts wurde verändert oder verfolgt.
        assert!(dir.is_locked());
        assert_eq!(
            std::fs::read(&outside).map_err(ctx("read outside"))?,
            b"foreign"
        );
        assert!(holds.join("100").is_dir());
        assert!(holds.join("0042").is_file());
        // Ein zweiter Durchlauf über denselben Deskriptor sieht dasselbe.
        assert_eq!(dir.scan().map_err(ctx("scan again"))?, scan);
        Ok(())
    }

    #[test]
    fn test_scan_of_an_empty_index_finds_nothing() -> TestResult {
        let run = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let holds = holds_dir(&run)?;
        let scan = ContainmentMarkers::new(&holds)
            .open()
            .map_err(ctx("open"))?
            .scan()
            .map_err(ctx("scan"))?;
        assert_eq!(scan, super::IndexScan::default());
        Ok(())
    }

    // -- Datensätze ------------------------------------------------------------

    #[test]
    fn test_records_survive_restart_and_complete() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let id = kernel_id(KERNEL_ID)?;
        {
            let store =
                ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open"))?;
            store
                .record(ContainmentEntry::containing(target(
                    "harw.slice/a",
                    KERNEL_ID,
                )?))
                .map_err(ctx("record"))?;
            let pending = store.get(id).map_err(ctx("get"))?;
            assert!(pending.is_some_and(|entry| !entry.is_active()));
            assert!(store.activate(id, true).map_err(ctx("activate"))?);
        }
        let reopened =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("reopen"))?;
        let entry = reopened
            .get(id)
            .map_err(ctx("get"))?
            .ok_or(TestError::Missing("record after restart"))?;
        assert_eq!(entry.freeze_was_set, Some(true));
        assert!(reopened.forget(id).map_err(ctx("forget"))?);
        assert!(!reopened.forget(id).map_err(ctx("forget again"))?);
        drop(reopened);
        let empty = ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A))
            .map_err(ctx("reopen empty"))?;
        assert!(empty.snapshot().map_err(ctx("snapshot"))?.is_empty());
        assert!(empty.previous_boot().map_err(ctx("previous"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_restore_puts_back_the_previous_record() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = ContainmentStore::open(dir.path()).map_err(ctx("open"))?;
        let id = kernel_id(KERNEL_ID)?;
        let previous = ContainmentEntry {
            target: target("harw.slice/old", KERNEL_ID)?,
            freeze_was_set: Some(false),
            released: false,
        };
        store.record(previous.clone()).map_err(ctx("record old"))?;
        store
            .record(ContainmentEntry::containing(target(
                "harw.slice/new",
                KERNEL_ID,
            )?))
            .map_err(ctx("record new"))?;
        assert_eq!(store.snapshot().map_err(ctx("snapshot"))?.len(), 1);
        store
            .restore(id, Some(previous.clone()))
            .map_err(ctx("restore"))?;
        assert_eq!(store.snapshot().map_err(ctx("snapshot"))?, vec![previous]);
        store.restore(id, None).map_err(ctx("restore none"))?;
        assert!(store.snapshot().map_err(ctx("snapshot"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_bookkeeping_after_an_intervention_applies_in_memory_when_writing_fails() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = ContainmentStore::open(dir.path()).map_err(ctx("open"))?;
        let id = kernel_id(KERNEL_ID)?;
        store
            .record(ContainmentEntry::containing(target(
                "harw.slice/a",
                KERNEL_ID,
            )?))
            .map_err(ctx("record"))?;
        // Jedes Schreiben scheitert ab jetzt (`O_NOFOLLOW` auf der Zwischendatei).
        std::os::unix::fs::symlink(
            dir.path().join("elsewhere"),
            dir.path().join(format!("{CONTAINMENT_FILE}.tmp")),
        )
        .map_err(ctx("block writes"))?;
        assert!(store.activate(id, false).is_err());
        assert_eq!(
            store
                .get(id)
                .map_err(ctx("get"))?
                .and_then(|entry| entry.freeze_was_set),
            Some(false)
        );
        // Vor dem Eingriff bleibt es streng: ohne Schreiben kein neuer Datensatz.
        assert!(
            store
                .record(ContainmentEntry::containing(target("harw.slice/b", 9)?))
                .is_err()
        );
        assert!(store.get(kernel_id(9)?).map_err(ctx("get b"))?.is_none());
        assert!(store.forget(id).is_err());
        assert!(store.get(id).map_err(ctx("get"))?.is_none());
        Ok(())
    }

    #[test]
    fn test_corrupt_or_duplicate_snapshot_refuses_open() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::write(
            dir.path().join(CONTAINMENT_FILE),
            b"{\"version\":1,\"containments\":[],\"x\":1}",
        )
        .map_err(ctx("write"))?;
        assert!(matches!(
            ContainmentStore::open(dir.path()),
            Err(WardenError::LedgerCorrupt { .. })
        ));
        let entry = serde_json::to_value(ContainmentEntry::containing(target(
            "harw.slice/a",
            KERNEL_ID,
        )?))
        .map_err(ctx("encode"))?;
        let duplicate = serde_json::json!({ "version": 1, "containments": [entry.clone(), entry] });
        std::fs::write(
            dir.path().join(CONTAINMENT_FILE),
            serde_json::to_vec(&duplicate).map_err(ctx("encode file"))?,
        )
        .map_err(ctx("write duplicate"))?;
        assert!(matches!(
            ContainmentStore::open(dir.path()),
            Err(WardenError::LedgerCorrupt { .. })
        ));
        Ok(())
    }

    // -- Boot-ID, Freigabe-Markierung, Platz -------------------------------------

    // Zwei erfundene Boot-IDs im Format des Kernels.
    const BOOT_A: &str = "6c9a3f1e-2b47-4d8e-9a51-3e7f0c2d8b14";
    const BOOT_B: &str = "1f0e9d8c-7b6a-4c5d-8e3f-2a1b0c9d8e7f";

    fn boot(id: &str) -> Option<String> {
        Some(id.to_owned())
    }

    #[test]
    fn test_records_of_another_boot_are_set_apart_and_discarded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let id = kernel_id(KERNEL_ID)?;
        {
            let store =
                ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open a"))?;
            store
                .record(ContainmentEntry::containing(target(
                    "harw.slice/a",
                    KERNEL_ID,
                )?))
                .map_err(ctx("record"))?;
            assert!(store.activate(id, false).map_err(ctx("activate"))?);
        }
        // Derselbe Boot: der Datensatz gilt weiter.
        let same = ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("same"))?;
        assert_eq!(same.snapshot().map_err(ctx("snapshot"))?.len(), 1);
        assert!(same.previous_boot().map_err(ctx("previous"))?.is_empty());
        drop(same);

        // Ein anderer Boot: Kernel-IDs sind nicht vergleichbar.
        let next = ContainmentStore::open_at_boot(dir.path(), boot(BOOT_B)).map_err(ctx("next"))?;
        assert_eq!(next.boot_id(), Some(BOOT_B));
        assert!(next.snapshot().map_err(ctx("snapshot"))?.is_empty());
        assert!(next.get(id).map_err(ctx("get"))?.is_none());
        let previous = next.previous_boot().map_err(ctx("previous"))?;
        assert_eq!(previous.len(), 1);
        assert!(
            previous
                .first()
                .is_some_and(|entry| entry.target.kernel_id == id)
        );
        next.discard_previous_boot().map_err(ctx("discard"))?;
        assert!(next.previous_boot().map_err(ctx("previous"))?.is_empty());
        drop(next);

        // Die Datei gehört jetzt dem neuen Boot und ist leer.
        let file: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(CONTAINMENT_FILE)).map_err(ctx("read"))?,
        )
        .map_err(ctx("decode"))?;
        assert_eq!(
            file.get("boot_id").and_then(serde_json::Value::as_str),
            Some(BOOT_B)
        );
        let reopened =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_B)).map_err(ctx("reopen"))?;
        assert!(reopened.snapshot().map_err(ctx("snapshot"))?.is_empty());
        assert!(
            reopened
                .previous_boot()
                .map_err(ctx("previous"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_unknown_boot_id_sets_the_records_apart() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let id = kernel_id(KERNEL_ID)?;
        {
            let store = ContainmentStore::open_at_boot(dir.path(), None).map_err(ctx("open"))?;
            assert_eq!(store.boot_id(), None);
            store
                .record(ContainmentEntry::containing(target(
                    "harw.slice/a",
                    KERNEL_ID,
                )?))
                .map_err(ctx("record"))?;
            assert!(store.activate(id, false).map_err(ctx("activate"))?);
            // Im laufenden Prozess gilt der eigene Datensatz.
            assert!(store.get(id).map_err(ctx("get"))?.is_some());
        }
        // Eine Datei ohne Boot-ID beweist keinen Boot: weder mit bekannter
        // noch mit unbekannter Boot-ID kommt ihr Datensatz in den Bestand.
        for current in [boot(BOOT_A), None] {
            let store = ContainmentStore::open_at_boot(dir.path(), current).map_err(ctx("open"))?;
            assert!(store.snapshot().map_err(ctx("snapshot"))?.is_empty());
            assert!(store.get(id).map_err(ctx("get"))?.is_none());
            assert_eq!(store.previous_boot().map_err(ctx("previous"))?.len(), 1);
        }
        // Mit Boot-ID geschrieben, ohne bekannte Boot-ID gelesen: ebenso.
        {
            let store =
                ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open a"))?;
            store.discard_previous_boot().map_err(ctx("discard"))?;
            store
                .record(ContainmentEntry::containing(target(
                    "harw.slice/a",
                    KERNEL_ID,
                )?))
                .map_err(ctx("record a"))?;
        }
        let unknown = ContainmentStore::open_at_boot(dir.path(), None).map_err(ctx("unknown"))?;
        assert!(unknown.snapshot().map_err(ctx("snapshot"))?.is_empty());
        assert_eq!(unknown.previous_boot().map_err(ctx("previous"))?.len(), 1);
        drop(unknown);
        // Derselbe, bekannte Boot: der Datensatz gilt.
        let same = ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("same"))?;
        assert!(same.get(id).map_err(ctx("get"))?.is_some());
        assert!(same.previous_boot().map_err(ctx("previous"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_release_of_a_foreign_bit_is_durable_first_and_flush_writes_the_rest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open"))?;
        let id = kernel_id(KERNEL_ID)?;
        let other = kernel_id(9)?;
        for (path, raw) in [("harw.slice/a", KERNEL_ID), ("harw.slice/b", 9)] {
            store
                .record(ContainmentEntry::containing(target(path, raw)?))
                .map_err(ctx("record"))?;
        }
        assert!(store.activate(id, true).map_err(ctx("activate"))?);
        assert!(
            !store
                .record_release(kernel_id(77)?)
                .map_err(ctx("absent"))?
        );
        let tmp = dir.path().join(format!("{CONTAINMENT_FILE}.tmp"));
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &tmp)
            .map_err(ctx("block writes"))?;
        // Ohne Schreiben keine Freigabe, auch nicht im Speicher.
        assert!(store.record_release(id).is_err());
        assert!(
            store
                .get(id)
                .map_err(ctx("get"))?
                .is_some_and(|entry| entry.is_active())
        );
        // Eine Buchung nach einem Eingriff gilt im Speicher, bleibt aber
        // ungesichert, bis ein Schreiben gelingt.
        assert!(store.mark_released(other).is_err());
        assert!(store.flush().is_err());
        let stored = |store_dir: &std::path::Path| -> TestResult<Vec<ContainmentEntry>> {
            ContainmentStore::open_at_boot(store_dir, boot(BOOT_A))
                .map_err(ctx("reopen"))?
                .snapshot()
                .map_err(ctx("stored"))
        };
        assert!(stored(dir.path())?.iter().all(|entry| !entry.released));
        std::fs::remove_file(&tmp).map_err(ctx("unblock writes"))?;
        store.flush().map_err(ctx("flush"))?;
        let flushed = stored(dir.path())?;
        assert!(
            flushed
                .iter()
                .any(|entry| entry.target.kernel_id == other && entry.released)
        );
        assert!(
            flushed
                .iter()
                .any(|entry| entry.target.kernel_id == id && entry.is_active())
        );
        // Jetzt gelingt die strenge Buchung; ein gesicherter Stand schreibt
        // bei `flush` nichts.
        assert!(store.record_release(id).map_err(ctx("record release"))?);
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &tmp)
            .map_err(ctx("block writes again"))?;
        store.flush().map_err(ctx("flush without changes"))?;
        let released = stored(dir.path())?;
        assert!(released.iter().all(|entry| entry.released));
        assert_eq!(
            released
                .iter()
                .find(|entry| entry.target.kernel_id == id)
                .and_then(|entry| entry.freeze_was_set),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn test_boot_id_is_parsed_strictly() {
        assert_eq!(
            parse_boot_id(&format!("{BOOT_A}\n")).as_deref(),
            Some(BOOT_A)
        );
        assert_eq!(parse_boot_id(BOOT_B).as_deref(), Some(BOOT_B));
        let too_long = "a".repeat(65);
        for bad in [
            "",
            "\n",
            "---",
            "not-a-boot-id",
            "6c9a 3f1e",
            too_long.as_str(),
        ] {
            assert_eq!(parse_boot_id(bad), None, "{bad:?}");
        }
        // Die echte Boot-ID (falls lesbar) hat dieselbe Form.
        if let Some(current) = super::current_boot_id() {
            assert_eq!(parse_boot_id(&current).as_deref(), Some(current.as_str()));
        }
    }

    #[test]
    fn test_release_mark_survives_restart_and_keeps_freeze_was_set() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let id = kernel_id(KERNEL_ID)?;
        {
            let store =
                ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open"))?;
            store
                .record(ContainmentEntry::containing(target(
                    "harw.slice/a",
                    KERNEL_ID,
                )?))
                .map_err(ctx("record"))?;
            assert!(store.activate(id, true).map_err(ctx("activate"))?);
            let active = store
                .get(id)
                .map_err(ctx("get"))?
                .ok_or(TestError::Missing("record"))?;
            assert!(active.is_active());
            assert!(!active.owns_freeze_bit());
            assert!(store.mark_released(id).map_err(ctx("release"))?);
            assert!(
                !store
                    .mark_released(kernel_id(9)?)
                    .map_err(ctx("release absent"))?
            );
        }
        let reopened =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("reopen"))?;
        let entry = reopened
            .get(id)
            .map_err(ctx("get"))?
            .ok_or(TestError::Missing("released record"))?;
        assert!(entry.released);
        assert!(entry.is_complete());
        assert!(!entry.is_active());
        assert_eq!(entry.freeze_was_set, Some(true));
        // Ein nicht abgeschlossener Datensatz gibt nie ein eigenes Bit vor.
        let pending = ContainmentEntry::containing(target("harw.slice/b", 9)?);
        assert!(!pending.is_complete());
        assert!(!pending.owns_freeze_bit());
        Ok(())
    }

    #[test]
    fn test_a_full_store_has_no_room_for_a_new_kernel_id_only() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut entries = Vec::new();
        for index in 0..MAX_CONTAINMENTS {
            let raw = u64::try_from(index)
                .map_err(ctx("index"))?
                .saturating_add(10_000);
            entries.push(
                serde_json::to_value(ContainmentEntry::containing(target(
                    &format!("harw.slice/job-{raw}"),
                    raw,
                )?))
                .map_err(ctx("encode"))?,
            );
        }
        let file = serde_json::json!({ "version": 1, "boot_id": BOOT_A, "containments": entries });
        std::fs::write(
            dir.path().join(CONTAINMENT_FILE),
            serde_json::to_vec(&file).map_err(ctx("encode file"))?,
        )
        .map_err(ctx("write"))?;
        let store =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("open"))?;
        assert!(
            !store
                .has_room_for(kernel_id(KERNEL_ID)?)
                .map_err(ctx("room new"))?
        );
        assert!(matches!(
            store.record(ContainmentEntry::containing(target(
                "harw.slice/new",
                KERNEL_ID
            )?)),
            Err(WardenError::LedgerUnavailable { .. })
        ));
        // Ein Datensatz derselben Kernel-ID ersetzt nur.
        assert!(
            store
                .has_room_for(kernel_id(10_000)?)
                .map_err(ctx("room same"))?
        );
        // Viele auf einmal, mit einem Schreibvorgang.
        let gone = [
            kernel_id(10_000)?,
            kernel_id(10_001)?,
            kernel_id(KERNEL_ID)?,
        ];
        assert_eq!(store.forget_many(&gone).map_err(ctx("forget many"))?, 2);
        assert_eq!(store.forget_many(&gone).map_err(ctx("forget again"))?, 0);
        assert!(
            store
                .has_room_for(kernel_id(KERNEL_ID)?)
                .map_err(ctx("room after"))?
        );
        let reopened =
            ContainmentStore::open_at_boot(dir.path(), boot(BOOT_A)).map_err(ctx("reopen"))?;
        assert_eq!(
            reopened.snapshot().map_err(ctx("snapshot"))?.len(),
            MAX_CONTAINMENTS.saturating_sub(2)
        );
        Ok(())
    }
}

//! Die Ausführungs-Traits: [`CgroupFreezer`], [`CgroupThawer`],
//! [`NetworkIsolator`], [`ProcessTreeKiller`] — je einer pro
//! Aktionsfamilie von [`harw_dod_warden_proto::WardenAction`]
//! (`NetworkIsolator` deckt Isolieren und Aufheben der Isolation ab).
//!
//! # Verantwortungsbereich
//! Jede Aktion, die tatsächlich ins System eingreift, ist hinter einem
//! eigenen Trait gekapselt (Brief: „Kapsle jede hinter einen eigenen
//! Trait"; [`CgroupFreezer`] liest zusätzlich das Freeze-Bit).
//! [`crate::warden::Warden`] hält für jede Aktion genau
//! eine `Box<dyn Trait>`-Instanz und ruft nie direkt cgroup- oder
//! Netz-Mechanik auf. Das erlaubt zwei unabhängige Dinge:
//!
//! - **Tests ohne echte Wirkung**: [`RecordingExecutor`] implementiert alle
//!   vier Traits, friert nichts ein, tötet nichts, schneidet kein Netz ab —
//!   sie zeichnet nur auf, welche Methode mit welcher cgroup aufgerufen
//!   wurde. Klone teilen sich dasselbe Protokoll, sodass ein Test die
//!   Reihenfolge über alle vier Rollen hinweg prüfen kann (z. B. „der
//!   Hold-Timer taut auf, hebt aber keine Isolation auf"). Kein Test dieser
//!   Crate erzeugt ein echtes Einfrieren, Töten oder Netzabschneiden (Brief).
//! - **Ein Austausch der Netz-Mechanik ohne Umschreibung**: siehe Abschnitt
//!   „Warum `NetworkIsolator` keine echte Implementierung hat" unten.
//!
//! # Was `CgroupV2Executor` wirklich tut
//! [`CgroupV2Executor`] implementiert [`CgroupFreezer`], [`CgroupThawer`]
//! und [`ProcessTreeKiller`] über Schreibzugriffe auf die
//! cgroup-v2-Kontrolldateien (`cgroup.freeze`, `cgroup.kill`) unterhalb
//! eines konfigurierbaren Wurzelverzeichnisses (Produktionscode:
//! `/sys/fs/cgroup`; Tests: ein `tempfile`-Verzeichnis). Kein `unsafe`,
//! keine eigene cgroup-Crate: die cgroup-v2-Schnittstelle ist selbst schon
//! eine Textdatei. Gelesen wird nur `cgroup.freeze`
//! ([`CgroupFreezer::freeze_requested`]): beim Eindämmen hält der Warden
//! fest, ob das Bit schon gesetzt war (`containment.rs`, Vertrag mit harw).
//!
//! # Prüfung und Eingriff über denselben Deskriptor (B21)
//! Die Ausführungs-Traits bekommen das signierte Ziel-Tripel
//! ([`CgroupTarget`]), nicht nur den Pfad. [`CgroupV2Executor`] öffnet das
//! cgroup-Verzeichnis **einmal** (`openat` mit `O_DIRECTORY | O_NOFOLLOW |
//! O_CLOEXEC`, [`open_pinned_cgroup`]), vergleicht die Inode-Nummer des
//! geöffneten Deskriptors mit der signierten Kernel-ID und schreibt die
//! Kontrolldatei danach **relativ zu diesem Deskriptor** (`openat(dirfd,
//! "cgroup.kill")`). Wird die cgroup zwischen der Zielprüfung des Wardens und
//! dem Schreiben entfernt und unter demselben Pfad neu angelegt (z. B. ein
//! neu gestarteter Dienst mit gleichnamigem Scope), zeigt der Deskriptor
//! weiter auf die alte, entfernte cgroup: das Öffnen der Kontrolldatei
//! scheitert (`ENOENT`), statt die Nachfolgerin einzufrieren oder zu töten.
//! Eine abweichende Inode ergibt [`WardenError::TargetChanged`], ein
//! verschwundenes Ziel `Io(NotFound)` bzw. [`WardenError::TargetNotCgroup`];
//! der Warden meldet beides als `Denial::TargetChanged`.
//!
//! **Zusätzliche, selbst auferlegte Prüfung:** Bevor eine `CgroupId` in
//! einen Pfad eingebettet wird, prüft [`validate_path_segment`] jedes durch
//! `/` getrennte Segment
//! ([`crate::error::WardenError::InvalidCgroupId`] bei Verstoß). Ein Segment
//! darf nicht leer sein — damit fallen führender, abschließender und
//! doppelter `/` heraus; ein führender `/` ließe `Path::join` die Wurzel
//! ersetzen. Es darf weder `.` noch `..` sein und kein Steuerzeichen
//! (einschließlich NUL) enthalten. Mehrstufige Pfade wie
//! `harw.slice/job-1` sind erlaubt, weil Job-cgroups unterhalb eines Slice
//! liegen (siehe `harw_dod_warden_proto::ProofPolicy::allows_cgroup`,
//! dieselbe Segment-Grammatik). Eine Präfix-Politik erzwingt dieser Typ
//! nicht. Symlinks kann cgroupfs nicht enthalten, das Zielverzeichnis selbst
//! wird trotzdem mit `O_NOFOLLOW` geöffnet; in Produktion beschränkt
//! Landlock (`harw-warden`, `landlock.rs`) Schreibzugriffe auf die
//! cgroup-Wurzel. `harw-types` garantiert für `CgroupId` nur „nicht leer"
//! — keine Pfadsicherheit. Der Warden glaubt der Gegenseite nichts
//! (Crate-Moduldoku) — das gilt auch für Feldwerte innerhalb einer bereits
//! nachgeprüften und zulässigen Aktion.
//!
//! # Warum `NetworkIsolator` keine echte Implementierung hat
//! `harw-dod-netpolicy` (AW3-02) hat die Nahtstelle bereits richtig
//! geschnitten (`NetBackend::apply(&self, plan: &NetPlan)`) und verweist die
//! echte Mechanik ausdrücklich auf diesen Knoten (AW5-04a). Diese Crate
//! prüfte deshalb, ob `rustables` — die im Plan (`docs/design/build-history.md`)
//! genannte Kandidaten-Crate für die echte Netlink-Mechanik — in das
//! Abhängigkeitsbudget passt:
//!
//! - `rustables` taucht in `Cargo.lock` an keiner Stelle im Workspace auf
//!   (geprüft: `grep -n '^name = "rustables"' Cargo.lock` liefert nichts) —
//!   es wäre eine vollständig neue Abhängigkeitskette, nicht die
//!   Wiederverwendung einer bereits vorhandenen.
//! - Das Abhängigkeitsbudget dieser Crate ist bereits durch die mandatierte
//!   Grundausstattung von `harw-dod-warden-proto` selbst gesprengt (siehe
//!   `lib.rs`-Moduldoku, Abschnitt „Abhängigkeitsbudget") — jede zusätzliche
//!   Kette verschärft einen bereits dokumentierten Befund, statt einen neuen
//!   zu rechtfertigen.
//!
//! **Entscheidung: kein echtes Netz-Backend in dieser Crate.**
//! [`NetworkIsolator`] besteht hier nur aus dem Trait plus
//! [`RecordingExecutor`]; das produktive Backend liefert das Binary
//! `harw-warden` (`isolation.rs`). Ein echtes Backend implementiert
//! [`NetworkIsolator`] in einem eigenen Crate (genau wie
//! `harw_dod_netpolicy::NetBackend` es für `NetPlan` vorsieht) — das ist ein
//! Austausch der Implementierung, keine Umschreibung dieses Traits: die
//! Signatur `fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()>`
//! enthält kein backend-spezifisches Detail (kein Netlink-Handle, kein
//! `nft`-Prozess-Handle), genau wie `NetBackend::apply` keines enthält.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
use harw_types::CgroupId;
use rustix::fs::{Mode, OFlags};

use crate::error::{WardenError, WardenResult};

/// Friert eine cgroup ein (Aktion [`harw_dod_warden_proto::WardenAction::FreezeCgroup`]).
pub trait CgroupFreezer {
    /// Friert die angegebene cgroup ein.
    ///
    /// # Description
    /// Wird von [`crate::warden::Warden::handle`] ausschließlich nach
    /// erfolgreicher Nachprüfung aufgerufen. Die Implementierung muss die
    /// Kernel-ID von `target` am Ziel selbst prüfen und darf nur dann
    /// schreiben (siehe Moduldoku, „Prüfung und Eingriff über denselben
    /// Deskriptor“).
    ///
    /// # Arguments
    /// - `target` (`&CgroupTarget`): das signierte Ziel-Tripel.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`WardenError::TargetChanged`] / [`WardenError::TargetNotCgroup`] /
    ///   [`WardenError::Io`] (`NotFound`): das Ziel ist nicht mehr die
    ///   geprüfte cgroup.
    /// - [`WardenError::InvalidCgroupId`] / [`WardenError::Io`]: siehe
    ///   jeweilige Implementierung.
    fn freeze(&self, target: &CgroupTarget) -> WardenResult<()>;

    /// Reads whether the cgroup's own `cgroup.freeze` bit is already set.
    ///
    /// # Description
    /// Der Warden liest das Bit beim Eindämmen nach dem Marker und vor
    /// [`Self::freeze`] (`containment.rs`, Vertrag mit harw): war es schon
    /// `1`, gehört der Freeze einem früheren Eigentümer (harw `job.suspend`,
    /// ein Operator), und das Freigeben lässt es stehen. Gelesen wird nur das
    /// eigene Bit der cgroup, nicht der wirksame Zustand über Vorfahren. Die
    /// Implementierung prüft die Kernel-ID wie [`Self::freeze`] am Ziel
    /// selbst.
    ///
    /// # Arguments
    /// - `target` (`&CgroupTarget`): das signierte Ziel-Tripel.
    ///
    /// # Returns
    /// `true`, wenn `cgroup.freeze` gesetzt ist.
    ///
    /// # Errors
    /// Wie [`Self::freeze`]; dazu [`WardenError::Io`] (`InvalidData`) für
    /// einen Inhalt außer `0`/`1`.
    fn freeze_requested(&self, target: &CgroupTarget) -> WardenResult<bool>;
}

/// Taut eine eingefrorene cgroup auf (Aktion
/// [`harw_dod_warden_proto::WardenAction::ThawCgroup`] und Ablauf eines
/// Hold-Timers).
///
/// # Description
/// Hebt **nur** den Freeze auf, nie eine Netz-Isolation — dafür gibt es
/// [`NetworkIsolator::release_isolation`] (Aktion `LiftIsolation`). Der
/// Hold-Timer des Wardens ruft ausschließlich diesen Trait auf (PL-100 WP 3.1:
/// „der optionale Hold-Timer taut nur auf").
pub trait CgroupThawer {
    /// Taut die angegebene cgroup auf (idempotent).
    ///
    /// # Arguments
    /// - `target` (`&CgroupTarget`): das signierte (bzw. beim Hold
    ///   gespeicherte) Ziel-Tripel.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg, auch wenn die cgroup nicht eingefroren war.
    ///
    /// # Errors
    /// Wie [`CgroupFreezer::freeze`].
    fn thaw(&self, target: &CgroupTarget) -> WardenResult<()>;
}

/// Schneidet den Netzzugriff einer cgroup ab und hebt ihn wieder auf (Aktionen
/// [`harw_dod_warden_proto::WardenAction::IsolateNetwork`] und
/// [`harw_dod_warden_proto::WardenAction::LiftIsolation`]).
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Warum `NetworkIsolator` keine echte
/// Implementierung hat" — diese Crate liefert nur [`RecordingExecutor`].
/// Beide Methoden sind Pflicht: seit `LiftIsolation` eine eigene signierte
/// Aktion ist, darf kein Isolator ein Aufheben melden, das nichts getan hat.
pub trait NetworkIsolator {
    /// Isoliert die angegebene cgroup vom Netz.
    ///
    /// # Description
    /// Das nftables-Backend löst den Pfad beim Einfügen selbst auf; der
    /// Warden beobachtet das Ziel deshalb **nach** dieser Methode erneut und
    /// hebt die Isolation sofort wieder auf, wenn dort nicht mehr die
    /// signierte cgroup liegt (`warden.rs`).
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die zu isolierende cgroup.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// Implementierungsabhängig.
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()>;

    /// Hebt eine zuvor gesetzte Netz-Isolation wieder auf (idempotent).
    ///
    /// # Description
    /// Muss auch für eine bereits verschwundene cgroup funktionieren: ihre
    /// Regeln hängen am Pfad, nicht an einer lebenden cgroup.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die cgroup, deren Isolation aufgehoben wird.
    ///
    /// # Errors
    /// Implementierungsabhängig.
    fn release_isolation(&self, cgroup: &CgroupId) -> WardenResult<()>;
}

/// Beendet den Prozessbaum einer cgroup (Aktion
/// [`harw_dod_warden_proto::WardenAction::KillProcessTree`]). **Nicht
/// reversibel** (siehe `harw_dod_warden_proto::action`-Moduldoku).
pub trait ProcessTreeKiller {
    /// Beendet den Prozessbaum der angegebenen cgroup.
    ///
    /// # Arguments
    /// - `target` (`&CgroupTarget`): das signierte Ziel-Tripel.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// Wie [`CgroupFreezer::freeze`].
    fn kill(&self, target: &CgroupTarget) -> WardenResult<()>;
}

/// Weist eine cgroup-Kennung als sicheren relativen Pfad unterhalb der
/// cgroup-Wurzel aus.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Was `CgroupV2Executor` wirklich tut". Prüft
/// jedes durch `/` getrennte Segment einzeln und lehnt jede Kennung ab, die
/// die Wurzel ersetzen oder aus ihr herausführen würde. Mehrstufige Pfade
/// wie `harw.slice/job-1` sind zulässig; `a..b` ist ein gewöhnlicher Name.
/// Auch [`crate::observe::CgroupFsObserver`] nutzt diese Prüfung, bevor es
/// ein Ziel beobachtet.
///
/// # Errors
/// - [`WardenError::InvalidCgroupId`]: ein Segment ist leer (führender,
///   abschließender oder doppelter `/`), ein Segment ist `.` oder `..`,
///   oder ein Segment enthält ein Steuerzeichen (einschließlich NUL).
pub(crate) fn validate_path_segment(cgroup: &CgroupId) -> WardenResult<&str> {
    let value = cgroup.as_str();
    let valid = value.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !segment.chars().any(char::is_control)
    });
    if !valid {
        return Err(WardenError::InvalidCgroupId);
    }
    Ok(value)
}

/// Opens the cgroup directory at `path` once and checks its kernel id on the descriptor.
///
/// # Description
/// `openat(AT_FDCWD, path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`,
/// dann `fstat` am Deskriptor: die Inode-Nummer muss `expected` sein. Alles,
/// was danach relativ zu diesem Deskriptor geöffnet wird, gehört zu genau
/// dieser cgroup — auch wenn unter `path` inzwischen eine andere liegt.
///
/// # Arguments
/// - `path` (`&Path`): absoluter Pfad des cgroup-Verzeichnisses.
/// - `expected` (`CgroupKernelId`): die signierte Kernel-ID.
///
/// # Returns
/// Den geöffneten Verzeichnis-Deskriptor.
///
/// # Errors
/// - [`WardenError::Io`] (`NotFound`): das Ziel existiert nicht (mehr).
/// - [`WardenError::TargetNotCgroup`]: kein Verzeichnis oder ein Symlink.
/// - [`WardenError::TargetChanged`]: andere Kernel-ID.
/// - [`WardenError::Io`]: jeder andere Öffnungs- oder `fstat`-Fehler.
pub(crate) fn open_pinned_cgroup(path: &Path, expected: CgroupKernelId) -> WardenResult<File> {
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(pin_error)?;
    let dir = File::from(fd);
    let metadata = dir.metadata()?;
    if !metadata.is_dir() {
        return Err(WardenError::TargetNotCgroup);
    }
    if metadata.ino() != expected.get() {
        return Err(WardenError::TargetChanged);
    }
    Ok(dir)
}

// `ENOTDIR`/`ELOOP` (kein Verzeichnis bzw. Symlink unter `O_NOFOLLOW`) sind
// „kein cgroup-Verzeichnis“; alles andere bleibt ein I/O-Fehler (`ENOENT`
// → `NotFound`, also „verschwunden“).
fn pin_error(errno: rustix::io::Errno) -> WardenError {
    if errno == rustix::io::Errno::NOTDIR || errno == rustix::io::Errno::LOOP {
        WardenError::TargetNotCgroup
    } else {
        WardenError::Io(std::io::Error::from(errno))
    }
}

/// Echte cgroup-v2-Ausführung über Dateisystemschreibzugriffe.
///
/// # Description
/// Siehe Moduldoku, Abschnitte „Was `CgroupV2Executor` wirklich tut" und
/// „Prüfung und Eingriff über denselben Deskriptor". Adressiert cgroups in
/// beliebiger Tiefe unterhalb von `root`, z. B. `harw.slice/job-1` →
/// `<root>/harw.slice/job-1/cgroup.freeze`. Kein Test dieser Crate lässt
/// diesen Typ auf ein echtes `/sys/fs/cgroup` zugreifen — Tests übergeben
/// ein `tempfile`-Verzeichnis als `root`.
pub struct CgroupV2Executor {
    root: PathBuf,
}

impl CgroupV2Executor {
    /// Baut einen Ausführer, der unterhalb von `root` schreibt.
    ///
    /// # Arguments
    /// - `root` (`impl Into<PathBuf>`): das cgroup-v2-Wurzelverzeichnis
    ///   (Produktion: `/sys/fs/cgroup`; Tests: ein Tempdir).
    ///
    /// # Returns
    /// Einen neuen [`CgroupV2Executor`].
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    // Öffnet das Ziel, prüft die Kernel-ID am Deskriptor und schreibt
    // `value` in die Kontrolldatei relativ zu genau diesem Deskriptor.
    fn write_control_file(
        &self,
        target: &CgroupTarget,
        file_name: &str,
        value: &[u8],
    ) -> WardenResult<()> {
        let relative = validate_path_segment(&target.path)?;
        let dir = open_pinned_cgroup(&self.root.join(relative), target.kernel_id)?;
        let control = rustix::fs::openat(
            &dir,
            file_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            // Nur in Test-Fixtures entsteht die Datei neu; unter cgroupfs
            // existiert sie immer.
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|errno| WardenError::Io(std::io::Error::from(errno)))?;
        File::from(control).write_all(value)?;
        Ok(())
    }

    // Öffnet das Ziel wie `write_control_file` und liest `cgroup.freeze` über
    // denselben Deskriptor. Fehlt die Datei, gilt das Bit als nicht gesetzt:
    // unter cgroupfs heißt das, die gepinnte cgroup ist inzwischen entfernt,
    // und das anschließende Schreiben meldet sie als verschwunden; in
    // Test-Fixtures existiert die Datei erst nach dem ersten Schreiben.
    fn read_freeze_bit(&self, target: &CgroupTarget) -> WardenResult<bool> {
        let relative = validate_path_segment(&target.path)?;
        let dir = open_pinned_cgroup(&self.root.join(relative), target.kernel_id)?;
        let control = match rustix::fs::openat(
            &dir,
            "cgroup.freeze",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(control) => control,
            Err(errno) if errno == rustix::io::Errno::NOENT => return Ok(false),
            Err(errno) => return Err(WardenError::Io(std::io::Error::from(errno))),
        };
        let mut contents = Vec::new();
        File::from(control)
            .take(FREEZE_BIT_READ_LIMIT)
            .read_to_end(&mut contents)?;
        match contents.trim_ascii() {
            b"0" => Ok(false),
            b"1" => Ok(true),
            _ => Err(WardenError::Io(std::io::Error::from(
                std::io::ErrorKind::InvalidData,
            ))),
        }
    }
}

// Höchstens so viele Bytes von `cgroup.freeze` lesen (Inhalt: `0\n` oder `1\n`).
const FREEZE_BIT_READ_LIMIT: u64 = 16;

impl CgroupFreezer for CgroupV2Executor {
    fn freeze(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.write_control_file(target, "cgroup.freeze", b"1")
    }

    fn freeze_requested(&self, target: &CgroupTarget) -> WardenResult<bool> {
        self.read_freeze_bit(target)
    }
}

impl CgroupThawer for CgroupV2Executor {
    fn thaw(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.write_control_file(target, "cgroup.freeze", b"0")
    }
}

impl ProcessTreeKiller for CgroupV2Executor {
    fn kill(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.write_control_file(target, "cgroup.kill", b"1")
    }
}

/// Ein einzelner aufgezeichneter Aufruf an [`RecordingExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    /// [`CgroupFreezer::freeze`] wurde für diese cgroup aufgerufen.
    Freeze(CgroupId),
    /// [`CgroupThawer::thaw`] wurde für diese cgroup aufgerufen.
    Thaw(CgroupId),
    /// [`NetworkIsolator::isolate`] wurde für diese cgroup aufgerufen.
    Isolate(CgroupId),
    /// [`NetworkIsolator::release_isolation`] wurde für diese cgroup aufgerufen.
    LiftIsolation(CgroupId),
    /// [`ProcessTreeKiller::kill`] wurde für diese cgroup aufgerufen.
    Kill(CgroupId),
}

/// Aufzeichnende Testimplementierung aller vier Ausführungs-Traits.
///
/// # Description
/// Friert nichts ein, tötet nichts, schneidet kein Netz ab — zeichnet nur
/// auf, welche Methode mit welcher cgroup aufgerufen wurde. Klone teilen
/// dasselbe Protokoll (`Arc`), damit ein Test einen Klon je Rolle an den
/// Warden geben und die Reihenfolge aller Aufrufe an einer Stelle lesen kann.
/// [`Self::new_failing`] liefert einen Ausführer, der jeden Aufruf
/// aufzeichnet und danach fehlschlägt (Ausführungsfehler-Pfad).
/// [`CgroupFreezer::freeze_requested`] ist ein Lesezugriff ohne Wirkung: er
/// wird nicht aufgezeichnet, scheitert nie und meldet `false`, nach
/// [`Self::reporting_frozen`] `true` (ein schon gesetztes Bit, etwa von
/// harws `job.suspend`).
///
/// # Concurrency
/// Intern `Arc<Mutex<_>>`; `Send + Sync`. Ein vergifteter Mutex wird
/// übernommen (`into_inner`) — für eine reine Testimplementierung
/// akzeptabel.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::{CgroupFreezer, CgroupThawer, RecordedCall, RecordingExecutor};
/// use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
/// use harw_types::CgroupId;
///
/// let (Ok(cgroup), Ok(kernel_id)) =
///     (CgroupId::try_from_str("harw.slice/job-1"), CgroupKernelId::new(42))
/// else {
///     return;
/// };
/// let target = CgroupTarget::new(cgroup.clone(), kernel_id, jiff::Timestamp::UNIX_EPOCH);
/// let log = RecordingExecutor::new();
/// let freezer = log.clone();
/// assert!(freezer.freeze(&target).is_ok());
/// assert!(log.thaw(&target).is_ok());
/// assert_eq!(
///     log.calls(),
///     vec![RecordedCall::Freeze(cgroup.clone()), RecordedCall::Thaw(cgroup)]
/// );
/// ```
#[derive(Debug, Default, Clone)]
pub struct RecordingExecutor {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    fail: bool,
    reports_frozen: bool,
}

impl RecordingExecutor {
    /// Baut einen Ausführer, der jeden Aufruf aufzeichnet und `Ok(())`
    /// zurückgibt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Baut einen Ausführer, der jeden Aufruf aufzeichnet, aber
    /// [`WardenError::Io`] zurückgibt — für den
    /// Ausführungsfehler-Audit-Pfad.
    #[must_use]
    pub fn new_failing() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail: true,
            reports_frozen: false,
        }
    }

    /// Baut einen fehlschlagenden Ausführer, der dasselbe Protokoll wie
    /// `self` teilt.
    ///
    /// # Returns
    /// Einen Klon mit `fail = true`.
    #[must_use]
    pub fn failing_view(&self) -> Self {
        Self {
            calls: Arc::clone(&self.calls),
            fail: true,
            reports_frozen: self.reports_frozen,
        }
    }

    /// Baut einen Ausführer mit demselben Protokoll, dessen
    /// [`CgroupFreezer::freeze_requested`] ein schon gesetztes
    /// `cgroup.freeze` meldet.
    ///
    /// # Returns
    /// Einen Klon mit `reports_frozen = true`.
    #[must_use]
    pub fn reporting_frozen(&self) -> Self {
        Self {
            calls: Arc::clone(&self.calls),
            fail: self.fail,
            reports_frozen: true,
        }
    }

    /// Gibt alle bisher aufgezeichneten Aufrufe zurück, in Aufrufreihenfolge.
    ///
    /// # Returns
    /// Eine Kopie des Protokolls (geteilt über alle Klone).
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    // Zeichnet `call` auf und schlägt danach fehl, falls so konfiguriert.
    fn record(&self, call: RecordedCall) -> WardenResult<()> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call);
        if self.fail {
            return Err(WardenError::Io(std::io::Error::other(
                "aufgezeichneter Testfehler",
            )));
        }
        Ok(())
    }
}

impl CgroupFreezer for RecordingExecutor {
    fn freeze(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.record(RecordedCall::Freeze(target.path.clone()))
    }

    fn freeze_requested(&self, _target: &CgroupTarget) -> WardenResult<bool> {
        Ok(self.reports_frozen)
    }
}

impl CgroupThawer for RecordingExecutor {
    fn thaw(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.record(RecordedCall::Thaw(target.path.clone()))
    }
}

impl NetworkIsolator for RecordingExecutor {
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::Isolate(cgroup.clone()))
    }

    fn release_isolation(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.record(RecordedCall::LiftIsolation(cgroup.clone()))
    }
}

impl ProcessTreeKiller for RecordingExecutor {
    fn kill(&self, target: &CgroupTarget) -> WardenResult<()> {
        self.record(RecordedCall::Kill(target.path.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::{
        CgroupFreezer, CgroupThawer, CgroupV2Executor, NetworkIsolator, ProcessTreeKiller,
        RecordedCall, RecordingExecutor, validate_path_segment,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{CgroupKernelId, CgroupTarget};
    use harw_types::CgroupId;
    use jiff::Timestamp;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    // Ziel-Tripel mit beliebiger (nicht null) Kernel-ID, für Pfade ohne
    // Verzeichnis oder für die aufzeichnende Implementierung.
    fn target(id: &str, kernel_id: u64) -> TestResult<CgroupTarget> {
        Ok(CgroupTarget::new(
            cgroup(id)?,
            CgroupKernelId::new(kernel_id).map_err(ctx("kernel id"))?,
            Timestamp::UNIX_EPOCH,
        ))
    }

    // Legt `<root>/<id>` an und liefert das Ziel mit dessen echter Inode.
    fn live_target(root: &std::path::Path, id: &str) -> TestResult<CgroupTarget> {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).map_err(ctx("create cgroup dir"))?;
        let inode = std::fs::metadata(&dir).map_err(ctx("metadata"))?.ino();
        target(id, inode)
    }

    // -- validate_path_segment -------------------------------------------------

    #[test]
    fn test_validate_path_segment_accepts_plain_id() -> TestResult {
        let cg = cgroup("cgroup-1")?;
        assert_eq!(
            validate_path_segment(&cg).map_err(ctx("validate path segment"))?,
            "cgroup-1"
        );
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_accepts_nested_path() -> TestResult {
        let nested = cgroup("harw.slice/job-1")?;
        assert_eq!(
            validate_path_segment(&nested).map_err(ctx("validate nested path"))?,
            "harw.slice/job-1"
        );
        // `..` nur als ganzes Segment verboten, nicht als Teil eines Namens.
        let dotted = cgroup("a..b")?;
        assert_eq!(
            validate_path_segment(&dotted).map_err(ctx("validate dotted name"))?,
            "a..b"
        );
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_malformed_paths() -> TestResult {
        for id in [
            "harw.slice/../x",
            "harw.slice/./x",
            "/abs",
            "a//b",
            "a/",
            ".",
            "..",
            "a/b\nc",
        ] {
            let cg = cgroup(id)?;
            if !matches!(
                validate_path_segment(&cg),
                Err(WardenError::InvalidCgroupId)
            ) {
                return Err(TestError::Unexpected(format!("{id:?} must be rejected")));
            }
        }
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_parent_traversal() -> TestResult {
        let cg = cgroup("../etc")?;
        let Err(err) = validate_path_segment(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    #[test]
    fn test_validate_path_segment_rejects_embedded_nul() -> TestResult {
        let cg = cgroup("a\0b")?;
        let Err(err) = validate_path_segment(&cg) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    // -- CgroupV2Executor: keine echte Wirkung außerhalb eines Tempdirs -------

    #[test]
    fn test_cgroup_v2_executor_freeze_writes_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "cgroup-1")?;
        let executor = CgroupV2Executor::new(dir.path());

        executor.freeze(&job).map_err(ctx("freeze succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.freeze"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "1");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_thaw_writes_zero() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "cgroup-1")?;
        let executor = CgroupV2Executor::new(dir.path());

        executor.thaw(&job).map_err(ctx("thaw succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.freeze"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "0");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_kill_writes_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "cgroup-1")?;
        let executor = CgroupV2Executor::new(dir.path());

        executor.kill(&job).map_err(ctx("kill succeeds"))?;

        let written = std::fs::read_to_string(dir.path().join("cgroup-1").join("cgroup.kill"))
            .map_err(ctx("control file written"))?;
        assert_eq!(written, "1");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_rejects_traversal_before_touching_filesystem() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let Err(err) = executor.freeze(&target("../escape", 7)?) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::InvalidCgroupId));
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_freeze_writes_nested_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "harw.slice/job-1")?;
        let executor = CgroupV2Executor::new(dir.path());

        executor.freeze(&job).map_err(ctx("freeze succeeds"))?;

        let slice = dir.path().join("harw.slice");
        let written = std::fs::read_to_string(slice.join("job-1").join("cgroup.freeze"))
            .map_err(ctx("nested control file written"))?;
        assert_eq!(written, "1");
        // Der Eltern-Slice bleibt unberührt.
        assert!(!slice.join("cgroup.freeze").exists());
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_kill_writes_nested_control_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "harw.slice/job-1")?;
        let executor = CgroupV2Executor::new(dir.path());

        executor.kill(&job).map_err(ctx("kill succeeds"))?;

        let written = std::fs::read_to_string(
            dir.path()
                .join("harw.slice")
                .join("job-1")
                .join("cgroup.kill"),
        )
        .map_err(ctx("nested control file written"))?;
        assert_eq!(written, "1");
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_rejects_absolute_and_nested_traversal() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::fs::create_dir(dir.path().join("harw.slice")).map_err(ctx("create slice dir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        for id in ["/abs", "harw.slice/../escape"] {
            if !matches!(
                executor.freeze(&target(id, 7)?),
                Err(WardenError::InvalidCgroupId)
            ) {
                return Err(TestError::Unexpected(format!("{id:?} must be rejected")));
            }
        }
        // Abgelehnt, bevor irgendetwas geschrieben wurde.
        assert!(!dir.path().join("escape").exists());
        assert!(!dir.path().join("cgroup.freeze").exists());
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_reports_a_missing_directory_as_gone() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let Err(err) = executor.freeze(&target("does-not-exist", 7)?) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::Io(_)));
        assert!(err.is_target_gone());
        Ok(())
    }

    // -- Prüfung und Eingriff über denselben Deskriptor (B21) -------------------

    #[test]
    fn test_cgroup_v2_executor_refuses_a_kernel_id_mismatch_without_writing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let live = live_target(dir.path(), "harw.slice/job-1")?;
        let stale = target(
            "harw.slice/job-1",
            live.kernel_id.get().wrapping_add(1).max(1),
        )?;
        let executor = CgroupV2Executor::new(dir.path());

        for result in [
            executor.freeze(&stale),
            executor.thaw(&stale),
            executor.kill(&stale),
        ] {
            if !matches!(result, Err(WardenError::TargetChanged)) {
                return Err(TestError::Unexpected(format!(
                    "a stale kernel id must be refused, got {result:?}"
                )));
            }
        }
        let job = dir.path().join("harw.slice").join("job-1");
        assert!(!job.join("cgroup.freeze").exists());
        assert!(!job.join("cgroup.kill").exists());
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_refuses_a_recreated_cgroup_at_the_same_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let original = live_target(dir.path(), "harw.slice/job-1")?;
        let job = dir.path().join("harw.slice").join("job-1");
        // Ein Platzhalter hält die alte Inode belegt, damit die neu angelegte
        // cgroup sicher eine andere Inode-Nummer bekommt.
        let parked = dir.path().join("parked");
        std::fs::rename(&job, &parked).map_err(ctx("park original"))?;
        std::fs::create_dir(&job).map_err(ctx("recreate at the same path"))?;
        let executor = CgroupV2Executor::new(dir.path());

        let result = executor.kill(&original);
        assert!(matches!(result, Err(WardenError::TargetChanged)));
        assert!(!job.join("cgroup.kill").exists());
        assert!(!parked.join("cgroup.kill").exists());
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_treats_a_file_or_symlink_as_not_a_cgroup() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = live_target(dir.path(), "real")?;
        std::fs::write(dir.path().join("file"), b"x").map_err(ctx("write file"))?;
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link"))
            .map_err(ctx("symlink"))?;
        let executor = CgroupV2Executor::new(dir.path());

        for id in ["file", "link"] {
            let result = executor.freeze(&target(id, real.kernel_id.get())?);
            if !matches!(result, Err(WardenError::TargetNotCgroup)) {
                return Err(TestError::Unexpected(format!(
                    "{id} must not be followed, got {result:?}"
                )));
            }
        }
        assert!(!dir.path().join("real").join("cgroup.freeze").exists());
        Ok(())
    }

    // -- freeze_requested: das eigene Bit über denselben Deskriptor ------------

    #[test]
    fn test_cgroup_v2_executor_reads_the_freeze_bit() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = live_target(dir.path(), "harw.slice/job-1")?;
        let control = dir
            .path()
            .join("harw.slice")
            .join("job-1")
            .join("cgroup.freeze");
        let executor = CgroupV2Executor::new(dir.path());

        // Ohne Datei (Fixture vor dem ersten Schreiben): nicht gesetzt.
        assert!(!executor.freeze_requested(&job).map_err(ctx("absent"))?);
        std::fs::write(&control, b"1\n").map_err(ctx("write 1"))?;
        assert!(executor.freeze_requested(&job).map_err(ctx("set"))?);
        std::fs::write(&control, b"0\n").map_err(ctx("write 0"))?;
        assert!(!executor.freeze_requested(&job).map_err(ctx("clear"))?);
        std::fs::write(&control, b"frozen").map_err(ctx("write garbage"))?;
        let Err(error) = executor.freeze_requested(&job) else {
            return Err(TestError::Unexpected(
                "an unknown freeze value must be an error".into(),
            ));
        };
        assert!(matches!(error, WardenError::Io(_)));
        assert!(!error.is_target_replaced_or_gone());
        Ok(())
    }

    #[test]
    fn test_cgroup_v2_executor_reads_no_bit_of_a_replaced_target_or_through_a_link() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let live = live_target(dir.path(), "harw.slice/job-1")?;
        let stale = target(
            "harw.slice/job-1",
            live.kernel_id.get().wrapping_add(1).max(1),
        )?;
        let executor = CgroupV2Executor::new(dir.path());
        assert!(matches!(
            executor.freeze_requested(&stale),
            Err(WardenError::TargetChanged)
        ));
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"1").map_err(ctx("write outside"))?;
        std::os::unix::fs::symlink(
            &outside,
            dir.path()
                .join("harw.slice")
                .join("job-1")
                .join("cgroup.freeze"),
        )
        .map_err(ctx("symlink control file"))?;
        let linked = executor.freeze_requested(&live);
        assert!(linked.is_err(), "a linked control file must not be read");
        Ok(())
    }

    // -- RecordingExecutor: jede Aktion über ihren Trait, nie echte Wirkung ---

    #[test]
    fn test_recording_executor_reports_the_freeze_bit_without_recording() -> TestResult {
        let executor = RecordingExecutor::new();
        let job = target("cgroup-1", 7)?;
        assert!(!executor.freeze_requested(&job).map_err(ctx("read"))?);
        let frozen = executor.reporting_frozen();
        assert!(frozen.freeze_requested(&job).map_err(ctx("read frozen"))?);
        assert!(
            frozen
                .failing_view()
                .freeze_requested(&job)
                .map_err(ctx("failing view reads"))?
        );
        assert!(executor.calls().is_empty());
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_freeze_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let job = target("cgroup-1", 7)?;
        CgroupFreezer::freeze(&executor, &job).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Freeze(job.path)]);
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_thaw_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let job = target("cgroup-1", 7)?;
        CgroupThawer::thaw(&executor, &job).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Thaw(job.path)]);
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_isolate_and_lift_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let cg = cgroup("cgroup-1")?;
        NetworkIsolator::isolate(&executor, &cg).map_err(ctx("records, does not act"))?;
        NetworkIsolator::release_isolation(&executor, &cg).map_err(ctx("records, does not act"))?;
        assert_eq!(
            executor.calls(),
            vec![
                RecordedCall::Isolate(cg.clone()),
                RecordedCall::LiftIsolation(cg)
            ]
        );
        Ok(())
    }

    #[test]
    fn test_recording_executor_records_kill_via_trait() -> TestResult {
        let executor = RecordingExecutor::new();
        let job = target("cgroup-1", 7)?;
        ProcessTreeKiller::kill(&executor, &job).map_err(ctx("records, does not act"))?;
        assert_eq!(executor.calls(), vec![RecordedCall::Kill(job.path)]);
        Ok(())
    }

    #[test]
    fn test_failing_recording_executor_still_records_before_returning_err() -> TestResult {
        let executor = RecordingExecutor::new_failing();
        let job = target("cgroup-1", 7)?;
        let Err(err) = CgroupFreezer::freeze(&executor, &job) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, WardenError::Io(_)));
        assert_eq!(executor.calls(), vec![RecordedCall::Freeze(job.path)]);
        Ok(())
    }

    #[test]
    fn test_clones_and_failing_view_share_one_call_log() -> TestResult {
        let log = RecordingExecutor::new();
        let failing = log.failing_view();
        let job = target("cgroup-1", 7)?;
        CgroupFreezer::freeze(&log.clone(), &job).map_err(ctx("clone records"))?;
        assert!(CgroupThawer::thaw(&failing, &job).is_err());
        assert_eq!(
            log.calls(),
            vec![
                RecordedCall::Freeze(job.path.clone()),
                RecordedCall::Thaw(job.path)
            ]
        );
        Ok(())
    }
}

//! Landlock-Selbstbeschränkung beim Start — die zweite Entscheidung dieses
//! Knotens (AW2-19).
//!
//! # Warum der Sentinel sich selbst einsperrt
//! `harw-sentinel` hält keine Fähigkeit über das hinaus, was seine Sensoren
//! ohnehin lesen dürfen (siehe [`crate::sensors`]-Moduldoku) — trotzdem läuft
//! der Prozess mit den vollen Dateisystemrechten seines aufrufenden Nutzers,
//! solange nichts ihn einschränkt. [`restrict_self`] bindet den Prozess
//! selbst über [Landlock](https://docs.kernel.org/userspace-api/landlock.html)
//! auf genau die Verzeichnisse, die seine Sensoren tatsächlich lesen
//! (`--proc-root`, `--thermal-root`, `--blockio-root`, `--gpu-root`,
//! `--cgroup-root` und, nur wenn ausdrücklich angegeben, `--workspace-root`)
//! plus lesend-schreibend den Root-Space (Telemetrie-Sink, IPC-Socket) —
//! eine zweite, vom Sensor-Vertrag unabhängige Schranke, falls eine künftige
//! Änderung versehentlich einen Lesezugriff außerhalb des vorgesehenen
//! Bereichs einführt.
//!
//! # Keine Wurzel darf die Sandbox auf „/" weiten (PL-100 WP0.7)
//! Früher stand `--workspace-root` auf `.`, und weil die Unit kein
//! `WorkingDirectory=` setzte, wurde daraus `/` — eine Leseregel für das
//! ganze Dateisystem, mit der die Lese-Sandbox wirkungslos war.
//! [`prepare_roots`] prüft deshalb jede Wurzel, bevor sie eine Regel wird,
//! und weist ab:
//!
//! - **relative Pfade** — sie hingen vom Arbeitsverzeichnis ab;
//! - **die Dateisystemwurzel in jeder Schreibweise** — lexikalisch (`/`,
//!   `/.`, `/..`) und über die Identität (Gerät + Inode) des tatsächlich
//!   geöffneten Deskriptors, sodass auch ein Symlink auf `/` oder ein
//!   `…/..`-Umweg abgewiesen wird; geprüft wird genau der Deskriptor, der
//!   danach in die Regel eingeht (kein Zeitfenster zwischen Prüfung und
//!   Nutzung);
//! - **nicht prüfbare** Deskriptoren (Identität nicht lesbar) und **nicht
//!   öffenbare** Pfade.
//!
//! Dieselbe Funktion speist `--print-sandbox` (Ausgabe der wirksamen
//! Wurzeln ohne Selbstbeschränkung), damit die Host-Verifikation genau das
//! zeigt, was [`restrict_self`] durchsetzen würde. Die Unit setzt zusätzlich
//! `WorkingDirectory=/`, das aber nie mehr in eine Regel eingeht.
//!
//! # Die asymmetrische Entscheidung
//! Die drei privilegierten Binaries dieses Programms (`harw-probe-fs`,
//! `harw-probe-bpf`, ein künftiges `harw-probe-netlink`) brechen den Start
//! hart ab, wenn ihre jeweilige Kernelfähigkeit fehlt — für sie ist die
//! angeforderte Fähigkeit die ganze Existenzberechtigung des Prozesses. Für
//! **diesen** Prozess gilt das Gegenteil: er tut seine eigentliche Arbeit
//! (Sensoren abrufen, puffern) auch ohne Landlock vollständig weiter — nur
//! ohne die zusätzliche Schranke. Ein Sicherheitssammler, der auf einem
//! Host ohne Landlock-Unterstützung (Kernel < 5.13, oder eine gehärtete
//! Distribution mit deaktiviertem LSM-Stack) gar nicht erst startet, verliert
//! auf genau diesem Host jede Beobachtung, die er sonst geliefert hätte —
//! das ist der teurere Fehler. [`restrict_self`] gibt deshalb nie ein
//! `Result`, sondern ein [`SandboxOutcome`]: „fehlgeschlagen" ist ein
//! normaler, erwarteter Rückgabewert, kein `Err`.
//!
//! # Ohne `unsafe`
//! Jeder hier aufgerufene Baustein der `landlock`-Crate (`Ruleset`,
//! `RulesetCreated`, `PathBeneath`, `PathFd`, `.restrict_self()`) ist laut
//! ihrer eigenen Dokumentation eine sichere Funktion — die Crate existiert
//! ausdrücklich als „safe abstraction for the Landlock system calls". Diese
//! Implementierung wurde gegen die auf `docs.rs/landlock/0.4.7` nachgelesene
//! API geschrieben, aber **nie gegen einen echten Kernel oder Compiler
//! ausgeführt** — dieser Knoten darf kein `cargo` ausführen (zentrale,
//! sequenzielle Verifikation), analog zur Offenlegung in
//! `harw-dod-netlink/src/socket.rs` für `NetlinkAuditSource`.
//!
//! # `SensorDegraded` ohne polierbaren Sensor
//! Die Aufgabenstellung verlangt bei fehlender Landlock-Unterstützung „ein
//! `SensorDegraded`-Ereignis, nicht einen harten Startfehler". Diese Crate
//! baut deshalb [`landlock_degraded_event`] — ein echtes,
//! typisiertes `harw_dod_signals::SecurityEvent` mit
//! `EventKind::SensorDegraded { sensor: SensorId::from_str(`[`LANDLOCK_STATUS_SENSOR_ID`]`) }`,
//! unter einer Kennung, die **keinen** gepollten `harw_dod_signals::Sensor`
//! bezeichnet, sondern die Selbstbeschränkung dieses Binaries selbst.
//!
//! Dieses Ereignis erreichte lange **nicht** `Sentinel::buffer()`: weder
//! `harw_dod_cap::Capability` (ein geschlossenes 14-Varianten-Enum für genau
//! die elf geplanten Sensor-Crates, siehe dortige Moduldoku) noch
//! `harw_dod_sentinel::Sentinel` (dessen einziger Schreibweg in den Puffer
//! über `Sentinel::poll_all` auf ein registriertes `dyn Sensor` lief) boten
//! eine Erweiterung für ein Ereignis, das kein Sensor-Abruf ist. Eine
//! erfundene `Capability`-Variante nur für diesen Zweck hätte die Zusage
//! „eine Fähigkeit je Sensor-Crate" verwässert, ohne dass eine echte
//! Sensor-Crate dahinterstünde.
//!
//! Das ist inzwischen geschlossen: `harw_dod_sentinel::Sentinel::record_external_event`
//! ist der dafür vorgesehene zweite Schreibweg, neben `poll_all`. [`crate::run`]
//! baut dieses Ereignis vor der Konstruktion des `Sentinel` (Landlock läuft
//! vor `Sentinel::new`) und reicht es unmittelbar danach, vor dem ersten
//! Poll-Zyklus, über `record_external_event` nach — so erscheint es
//! garantiert im ersten `freeze()`, statt bei einem verpassten ersten
//! Zyklus verlorenzugehen (siehe `crate`-Moduldoku, Abschnitt „Der
//! Landlock-Fall entsteht vor dem `Sentinel`"). `tracing::warn!` bleibt
//! daneben bestehen: es bedient den Betreiber, der diesen Prozess in
//! Echtzeit über sein Log beobachtet, während der Puffer-/`freeze()`-Weg
//! das zitierfähige `SecurityEvidence`-Artefakt für eine spätere Prüfung
//! bedient — zwei verschiedene Leser für dasselbe Ereignis.
//!
//! # Nebenläufigkeit
//! [`restrict_self`] wird genau einmal beim Start aus dem Hauptthread
//! aufgerufen, bevor irgendein weiterer Thread (IPC-Empfangsschleife,
//! `Sentinel`-Sammelschleife) startet — eine Prozess-weite Landlock-Regel
//! gilt für alle Threads, die *danach* entstehen.

use std::path::{Component, Path, PathBuf};

use harw_dod_signals::{EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::Timestamp;
// `RulesetAttr`/`RulesetCreatedAttr`/`RestrictSelfAttr`/`Access`/`Compatible`
// werden nie namentlich benutzt, müssen aber im Geltungsbereich stehen,
// damit `.handle_access()`, `.add_rules()`, `.restrict_self()`,
// `.set_compatibility()` und `AccessFs::from_all`/`from_read` als
// Trait-Methoden auflösbar sind (dieselbe Notwendigkeit wie `use
// std::io::Write` für `.write_all()`). Ein nicht tatsächlich benötigter
// Trait-Import wäre höchstens eine ungenutzte-Import-Warnung, kein
// Kompilierfehler — das Gegenteil (ein fehlender Import) wäre einer, daher
// die bewusst vollständige Liste.
use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

/// Kennung, unter der dieses Binary seine eigene Landlock-Selbstbeschränkung
/// im Ereignisvokabular anspricht (siehe Moduldoku, Abschnitt
/// „`SensorDegraded` ohne pollbaren Sensor").
///
/// **Kein** `harw_types::SensorId` eines registrierten
/// `harw_dod_signals::Sensor` — [`crate::sensors::build_sensors`] vergibt
/// diese Kennung nie an einen echten Sensor.
pub const LANDLOCK_STATUS_SENSOR_ID: &str = "sentinel-landlock";

/// Eine Wurzel, die [`restrict_self`] in die Landlock-Regel aufnimmt.
#[derive(Debug, Clone)]
pub struct SandboxRoot {
    /// Der zu erlaubende Pfad.
    pub path: PathBuf,
    /// `true`, wenn dieser Pfad auch beschrieben werden muss (der
    /// Root-Space: Telemetrie-Sink, IPC-Socket). `false` für reine
    /// Sensor-Lesewurzeln (`/proc`, `/sys/class/thermal`, Workspace-Wurzel).
    pub writable: bool,
}

impl SandboxRoot {
    /// Baut eine nur lesbare Wurzel.
    #[must_use]
    pub fn read_only(path: PathBuf) -> Self {
        Self {
            path,
            writable: false,
        }
    }

    /// Baut eine lesend-schreibend erlaubte Wurzel.
    #[must_use]
    pub fn read_write(path: PathBuf) -> Self {
        Self {
            path,
            writable: true,
        }
    }
}

/// Ergebnis eines Selbstbeschränkungsversuchs.
///
/// # Description
/// Spiegelt `landlock::RulesetStatus` plus einen vierten Fall
/// (`Failed`) für jeden Schritt, der vor `restrict_self()` selbst
/// scheitert (z. B. `handle_access`/`create` liefern `Err`, obwohl
/// `CompatLevel::BestEffort` gesetzt ist — laut `landlock`-Dokumentation
/// praktisch unerreichbar, aber nicht per Typ ausgeschlossen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxOutcome {
    /// Jede angeforderte Regel wurde vom Kernel durchgesetzt.
    FullyEnforced,
    /// Ein Teil der angeforderten Regeln wurde durchgesetzt (älterer Kernel,
    /// niedrigere unterstützte ABI-Stufe).
    PartiallyEnforced,
    /// Der Kernel unterstützt Landlock nicht — die Regel wurde
    /// **überhaupt nicht** durchgesetzt. Der Fall, der laut Aufgabenstellung
    /// ein `SensorDegraded`-Ereignis auslöst, siehe [`landlock_degraded_event`].
    NotEnforced,
    /// Ein Aufbauschritt (Ruleset-Erstellung, Regelbindung,
    /// Selbstbeschränkung selbst) lieferte einen Fehler. Wird wie
    /// `NotEnforced` behandelt — der Prozess läuft ungeschützt weiter.
    Failed,
}

impl SandboxOutcome {
    /// Ob dieses Ergebnis das in der Aufgabenstellung verlangte
    /// `SensorDegraded`-Ereignis auslösen soll.
    #[must_use]
    pub const fn is_degraded(self) -> bool {
        matches!(self, Self::NotEnforced | Self::Failed)
    }
}

/// Warum [`prepare_roots`] eine Wurzel nicht in die Landlock-Regel aufnimmt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootRefusal {
    /// Der Pfad ist relativ und hinge vom Arbeitsverzeichnis ab.
    Relative,
    /// Der Pfad ist — in irgendeiner Schreibweise oder über einen Symlink —
    /// die Dateisystemwurzel `/`.
    FilesystemRoot,
    /// Der Pfad ließ sich öffnen, seine Identität (Gerät + Inode) aber
    /// nicht lesen; ohne sie ist „nicht `/`" nicht belegbar.
    Unverifiable,
    /// Der Pfad ließ sich nicht öffnen (fehlt, keine Berechtigung).
    Unavailable,
}

impl RootRefusal {
    /// Stabiler, kleingeschriebener Bezeichner für Log und
    /// `--print-sandbox`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Relative => "relative",
            Self::FilesystemRoot => "filesystem-root",
            Self::Unverifiable => "unverifiable",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Eine geprüfte, bereits geöffnete Wurzel — bereit für eine
/// `PathBeneath`-Regel.
///
/// # Description
/// Hält genau den Deskriptor, dessen Identität [`prepare_roots`] geprüft
/// hat; [`restrict_self`] baut die Regel aus eben diesem Deskriptor.
#[derive(Debug)]
pub struct PreparedRoot {
    /// Die angeforderte Wurzel.
    pub root: SandboxRoot,
    /// Der geprüfte Deskriptor (`O_PATH`).
    fd: PathFd,
}

/// Ergebnis von [`prepare_roots`].
#[derive(Debug, Default)]
pub struct PreparedRoots {
    /// Wurzeln, die eine Regel werden, in Eingabereihenfolge.
    pub accepted: Vec<PreparedRoot>,
    /// Abgewiesene Wurzeln mit Grund, in Eingabereihenfolge.
    pub refused: Vec<(SandboxRoot, RootRefusal)>,
}

/// Gerät und Inode einer Datei — die Identität, an der „ist `/`" gemessen
/// wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn identity_of(meta: &std::fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt as _;
    FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    }
}

/// Identität der Dateisystemwurzel dieses Prozesses (`None`, falls nicht
/// lesbar oder keine Unix-Plattform — dann bleibt nur die lexikalische
/// Prüfung).
#[cfg(unix)]
fn filesystem_root_identity() -> Option<FileIdentity> {
    std::fs::metadata("/").ok().map(|meta| identity_of(&meta))
}

#[cfg(not(unix))]
fn filesystem_root_identity() -> Option<FileIdentity> {
    None
}

/// Identität des bereits geöffneten Deskriptors (`fstat` auf einem Duplikat
/// des `O_PATH`-Deskriptors; seit Linux 3.6 erlaubt).
#[cfg(unix)]
fn fd_identity(fd: &PathFd) -> Option<FileIdentity> {
    use std::os::fd::AsFd as _;
    let owned = fd.as_fd().try_clone_to_owned().ok()?;
    let file = std::fs::File::from(owned);
    file.metadata().ok().map(|meta| identity_of(&meta))
}

#[cfg(not(unix))]
fn fd_identity(_fd: &PathFd) -> Option<FileIdentity> {
    None
}

/// Ob ein absoluter Pfad lexikalisch die Wurzel ist: keine einzige normale
/// Komponente (`/`, `/.`, `/..`, `//`).
fn is_lexical_filesystem_root(path: &Path) -> bool {
    !path
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
}

/// Prüft und öffnet eine einzelne Wurzel (siehe [`prepare_roots`]).
fn prepare_root(
    root: &SandboxRoot,
    filesystem_root: Option<FileIdentity>,
) -> Result<PathFd, RootRefusal> {
    if !root.path.is_absolute() {
        return Err(RootRefusal::Relative);
    }
    if is_lexical_filesystem_root(&root.path) {
        return Err(RootRefusal::FilesystemRoot);
    }
    let fd = PathFd::new(&root.path).map_err(|_| RootRefusal::Unavailable)?;
    let Some(filesystem_root) = filesystem_root else {
        // Ohne Vergleichswert (keine Unix-Plattform) bleibt es bei der
        // lexikalischen Prüfung oben.
        return Ok(fd);
    };
    match fd_identity(&fd) {
        Some(identity) if identity == filesystem_root => Err(RootRefusal::FilesystemRoot),
        Some(_) => Ok(fd),
        None => Err(RootRefusal::Unverifiable),
    }
}

/// Prüft und öffnet jede Wurzel, bevor sie eine Landlock-Regel wird.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Keine Wurzel darf die Sandbox auf „/"
/// weiten". Eine abgewiesene Wurzel wird auf `warn` nur mit Grund und
/// Schreibrecht gemeldet (Journal-Hygiene: kein Pfad auf `warn`), der Pfad
/// selbst auf `debug`. Kein Landlock-Aufruf — nur `open(O_PATH)` und
/// `fstat`, deshalb auch für `--print-sandbox` und in Tests nutzbar.
///
/// # Arguments
/// - `roots` (`&[SandboxRoot]`): die angeforderten Wurzeln.
///
/// # Returns
/// [`PreparedRoots`] mit den angenommenen (geöffneten) und den
/// abgewiesenen Wurzeln.
///
/// # Examples
/// ```rust,ignore
/// let prepared = prepare_roots(&[SandboxRoot::read_only(PathBuf::from("/"))]);
/// assert!(prepared.accepted.is_empty());
/// assert_eq!(prepared.refused[0].1, RootRefusal::FilesystemRoot);
/// ```
#[must_use]
pub fn prepare_roots(roots: &[SandboxRoot]) -> PreparedRoots {
    let filesystem_root = filesystem_root_identity();
    let mut prepared = PreparedRoots::default();
    for root in roots {
        match prepare_root(root, filesystem_root) {
            Ok(fd) => prepared.accepted.push(PreparedRoot {
                root: root.clone(),
                fd,
            }),
            Err(reason) => {
                tracing::warn!(
                    reason = reason.label(),
                    writable = root.writable,
                    "landlock root refused; it is not part of the ruleset"
                );
                tracing::debug!(
                    path = %root.path.display(),
                    reason = reason.label(),
                    "refused landlock root"
                );
                prepared.refused.push((root.clone(), reason));
            }
        }
    }
    prepared
}

fn classify(status: RestrictionStatus) -> SandboxOutcome {
    match status.ruleset {
        RulesetStatus::FullyEnforced => SandboxOutcome::FullyEnforced,
        RulesetStatus::PartiallyEnforced => SandboxOutcome::PartiallyEnforced,
        RulesetStatus::NotEnforced => SandboxOutcome::NotEnforced,
    }
}

/// Versucht, den aufrufenden Prozess über Landlock auf `roots` zu
/// beschränken.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (siehe
/// Moduldoku für die Begründung, warum dieser Prozess nie hart abbricht),
/// nimmt für jede von [`prepare_roots`] angenommene Wurzel eine
/// `PathBeneath`-Regel mit [`AccessFs::from_all`] (`writable`) oder
/// [`AccessFs::from_read`] (sonst) unter [`landlock::ABI::V1`] auf, und ruft
/// `restrict_self()` auf dem Ergebnis. Eine abgewiesene Wurzel (relativ,
/// `/` in irgendeiner Schreibweise, nicht prüfbar oder nicht öffenbar,
/// z. B. ein noch nicht angelegtes Verzeichnis) wird übersprungen und
/// geloggt — sie bricht den gesamten Versuch nicht ab und weitet ihn nie.
///
/// # Arguments
/// - `roots` (`&[SandboxRoot]`): die zu erlaubenden Pfade.
///
/// # Returns
/// Das erreichte [`SandboxOutcome`]. Nie ein `Result` — siehe Moduldoku.
///
/// # Concurrency
/// Nur vor dem Start weiterer Threads aufrufen (siehe Moduldoku).
///
/// # Examples
/// ```rust,no_run
/// use harw_sentinel::sandbox::{restrict_self, SandboxOutcome, SandboxRoot};
/// use std::path::PathBuf;
///
/// let outcome = restrict_self(&[SandboxRoot::read_only(PathBuf::from("/proc"))]);
/// if outcome.is_degraded() {
///     eprintln!("running without landlock sandboxing");
/// }
/// ```
#[must_use]
pub fn restrict_self(roots: &[SandboxRoot]) -> SandboxOutcome {
    let abi = ABI::V1;

    let ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    let ruleset = match ruleset.handle_access(AccessFs::from_all(abi)) {
        Ok(ruleset) => ruleset,
        Err(error) => {
            tracing::warn!(error = %error, "landlock handle_access failed; running without filesystem sandboxing");
            return SandboxOutcome::Failed;
        }
    };

    let created = match ruleset.create() {
        Ok(created) => created.set_compatibility(CompatLevel::BestEffort),
        Err(error) => {
            tracing::warn!(error = %error, "landlock ruleset create failed; running without filesystem sandboxing");
            return SandboxOutcome::Failed;
        }
    };

    let prepared = prepare_roots(roots);
    let rules: Vec<Result<PathBeneath<PathFd>, landlock::RulesetError>> = prepared
        .accepted
        .into_iter()
        .map(|prepared| Ok(rule_for(prepared, abi)))
        .collect();

    let created = match created.add_rules(rules) {
        Ok(created) => created,
        Err(error) => {
            tracing::warn!(error = %error, "landlock add_rules failed; running without filesystem sandboxing");
            return SandboxOutcome::Failed;
        }
    };

    match created.restrict_self() {
        Ok(status) => {
            // `no_new_privs` zuerst herauskopieren: `classify` nimmt
            // `status` als Wert entgegen, und ob `RestrictionStatus`
            // `Copy` ist, ist hier bewusst nicht vorausgesetzt.
            let no_new_privs = status.no_new_privs;
            let outcome = classify(status);
            tracing::info!(
                ?outcome,
                no_new_privs,
                "landlock self-restriction attempted"
            );
            outcome
        }
        Err(error) => {
            tracing::warn!(error = %error, "landlock restrict_self failed; running without filesystem sandboxing");
            SandboxOutcome::Failed
        }
    }
}

/// Baut die `PathBeneath`-Regel einer geprüften Wurzel aus genau dem
/// Deskriptor, den [`prepare_roots`] geprüft hat.
fn rule_for(prepared: PreparedRoot, abi: ABI) -> PathBeneath<PathFd> {
    let access = if prepared.root.writable {
        AccessFs::from_all(abi)
    } else {
        AccessFs::from_read(abi)
    };
    PathBeneath::new(prepared.fd, access)
}

/// Baut das `SecurityEvent`, das eine fehlende oder unvollständige
/// Landlock-Durchsetzung beim Start meldet.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „`SensorDegraded` ohne pollbaren Sensor", für
/// die Begründung, warum dieses Ereignis heute nur über `tracing`
/// beobachtbar gemacht wird und nicht in `Sentinel::buffer()` landet.
///
/// # Arguments
/// - `now` (`jiff::Timestamp`): injizierter Zeitpunkt des
///   Selbstbeschränkungsversuchs.
///
/// # Returns
/// Ein `SecurityEvent` mit `EventKind::SensorDegraded { sensor:
/// SensorId::from_str(`[`LANDLOCK_STATUS_SENSOR_ID`]`) }` und `actor: None`
/// (kein Akteur — dies ist ein Zustand des Prozesses selbst, kein
/// beobachtetes fremdes Handeln).
///
/// # Examples
/// ```rust
/// use harw_sentinel::sandbox::landlock_degraded_event;
/// use harw_dod_signals::EventKind;
///
/// let event = landlock_degraded_event(jiff::Timestamp::UNIX_EPOCH);
/// assert!(matches!(event.kind, EventKind::SensorDegraded { .. }));
/// ```
#[must_use]
pub fn landlock_degraded_event(now: Timestamp) -> SecurityEvent {
    let sensor = SensorId::from_str(LANDLOCK_STATUS_SENSOR_ID);
    SecurityEvent {
        sensor: sensor.clone(),
        observed_at: now,
        actor: None,
        kind: EventKind::SensorDegraded { sensor },
    }
}

/// Baut die Standard-Wurzelliste dieses Binaries für [`restrict_self`].
///
/// # Arguments
/// - `proc_root`, `thermal_root`, `blockio_root`, `gpu_root`, `cgroup_root`
///   (`&Path`): die Sensor-Lesewurzeln (siehe
///   [`crate::sensors::SensorRoots`]).
/// - `workspace_root` (`Option<&Path>`): die Workspace-Lesewurzel, nur wenn
///   `--workspace-root` ausdrücklich angegeben ist (PL-100 WP0.7); `None`
///   fügt keine Regel hinzu.
/// - `home` (`&Path`): der Root-Space — lesend-schreibend, weil der
///   Telemetrie-Sink und der IPC-Socket darunter liegen.
///
/// # Returns
/// Sechs bzw. sieben [`SandboxRoot`]s: die Lesewurzeln nur lesbar, `home`
/// (immer der letzte Eintrag) lesend-schreibend.
#[must_use]
pub fn default_roots(
    proc_root: &Path,
    thermal_root: &Path,
    workspace_root: Option<&Path>,
    blockio_root: &Path,
    gpu_root: &Path,
    cgroup_root: &Path,
    home: &Path,
) -> Vec<SandboxRoot> {
    let mut roots = vec![
        SandboxRoot::read_only(proc_root.to_path_buf()),
        SandboxRoot::read_only(thermal_root.to_path_buf()),
    ];
    if let Some(workspace_root) = workspace_root {
        roots.push(SandboxRoot::read_only(workspace_root.to_path_buf()));
    }
    roots.extend([
        SandboxRoot::read_only(blockio_root.to_path_buf()),
        SandboxRoot::read_only(gpu_root.to_path_buf()),
        SandboxRoot::read_only(cgroup_root.to_path_buf()),
        SandboxRoot::read_write(home.to_path_buf()),
    ]);
    roots
}

#[cfg(test)]
mod tests {
    use super::{
        LANDLOCK_STATUS_SENSOR_ID, RootRefusal, SandboxOutcome, SandboxRoot, default_roots,
        is_lexical_filesystem_root, landlock_degraded_event, prepare_roots,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::EventKind;
    use std::path::{Path, PathBuf};

    /// Der Grund, aus dem genau eine übergebene Wurzel abgewiesen wurde.
    fn refusal_of(root: SandboxRoot) -> TestResult<Option<RootRefusal>> {
        let prepared = prepare_roots(&[root]);
        match (prepared.accepted.len(), prepared.refused.first()) {
            (1, None) => Ok(None),
            (0, Some((_, reason))) => Ok(Some(*reason)),
            (accepted, _) => Err(TestError::Unexpected(format!(
                "expected exactly one decision, got {accepted} accepted and {} refused",
                prepared.refused.len()
            ))),
        }
    }

    #[test]
    fn test_sandbox_root_read_only_sets_writable_false() {
        let root = SandboxRoot::read_only(PathBuf::from("/proc"));
        assert!(!root.writable);
    }

    #[test]
    fn test_sandbox_root_read_write_sets_writable_true() {
        let root = SandboxRoot::read_write(PathBuf::from("/home/x/.harw"));
        assert!(root.writable);
    }

    #[test]
    fn test_default_roots_marks_only_home_as_writable() -> TestResult {
        let roots = default_roots(
            Path::new("/proc"),
            Path::new("/sys/class/thermal"),
            Some(Path::new("/srv/harw/ws")),
            Path::new("/sys/block"),
            Path::new("/sys/class/drm"),
            Path::new("/sys/fs/cgroup"),
            Path::new("/home/x/.harw"),
        );
        assert_eq!(roots.len(), 7);
        assert_eq!(roots.iter().filter(|r| r.writable).count(), 1);
        assert!(
            roots
                .last()
                .ok_or(TestError::Missing("seven roots"))?
                .writable
        );
        Ok(())
    }

    /// PL-100 WP0.7: ohne `--workspace-root` entsteht keine Workspace-Regel
    /// — insbesondere nie eine für das Arbeitsverzeichnis.
    #[test]
    fn test_default_roots_without_workspace_root_has_no_workspace_rule() -> TestResult {
        let roots = default_roots(
            Path::new("/proc"),
            Path::new("/sys/class/thermal"),
            None,
            Path::new("/sys/block"),
            Path::new("/sys/class/drm"),
            Path::new("/sys/fs/cgroup"),
            Path::new("/home/x/.harw"),
        );
        assert_eq!(roots.len(), 6);
        assert!(roots.iter().all(|root| root.path.is_absolute()));
        assert!(
            roots
                .last()
                .ok_or(TestError::Missing("six roots"))?
                .writable
        );
        Ok(())
    }

    #[test]
    fn test_lexical_filesystem_root_covers_every_spelling_without_a_name() {
        for root in ["/", "/.", "/..", "//", "/./.."] {
            assert!(is_lexical_filesystem_root(Path::new(root)), "{root}");
        }
        for not_root in ["/proc", "/sys/..", "/a/./b"] {
            assert!(
                !is_lexical_filesystem_root(Path::new(not_root)),
                "{not_root} has a normal component"
            );
        }
    }

    /// PL-100 WP0.7, Abnahme „Ruleset ≠ /": weder lesend noch schreibend
    /// wird `/` je eine Regel.
    #[test]
    fn test_prepare_roots_refuses_the_filesystem_root() -> TestResult {
        assert_eq!(
            refusal_of(SandboxRoot::read_only(PathBuf::from("/")))?,
            Some(RootRefusal::FilesystemRoot)
        );
        assert_eq!(
            refusal_of(SandboxRoot::read_write(PathBuf::from("/.")))?,
            Some(RootRefusal::FilesystemRoot)
        );
        Ok(())
    }

    /// Ein `…/..`-Umweg ist lexikalisch kein `/`; die Identitätsprüfung des
    /// geöffneten Deskriptors weist ihn trotzdem ab.
    #[test]
    fn test_prepare_roots_refuses_a_dot_dot_detour_to_the_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut detour = dir.path().to_path_buf();
        for _ in dir.path().components().skip(1) {
            detour.push("..");
        }
        assert_eq!(
            refusal_of(SandboxRoot::read_only(detour))?,
            Some(RootRefusal::FilesystemRoot)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_prepare_roots_refuses_a_symlink_to_the_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let link = dir.path().join("looks-harmless");
        std::os::unix::fs::symlink("/", &link).map_err(ctx("symlink"))?;
        assert_eq!(
            refusal_of(SandboxRoot::read_only(link))?,
            Some(RootRefusal::FilesystemRoot)
        );
        Ok(())
    }

    #[test]
    fn test_prepare_roots_refuses_relative_paths_including_the_working_directory() -> TestResult {
        for relative in [".", "proc", "./export"] {
            assert_eq!(
                refusal_of(SandboxRoot::read_only(PathBuf::from(relative)))?,
                Some(RootRefusal::Relative),
                "{relative}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_prepare_roots_refuses_a_missing_path_as_unavailable() -> TestResult {
        assert_eq!(
            refusal_of(SandboxRoot::read_only(PathBuf::from(
                "/nonexistent-harw-sentinel-landlock-root"
            )))?,
            Some(RootRefusal::Unavailable)
        );
        Ok(())
    }

    #[test]
    fn test_prepare_roots_accepts_an_existing_directory_and_keeps_order() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let roots = [
            SandboxRoot::read_only(PathBuf::from("/")),
            SandboxRoot::read_write(dir.path().to_path_buf()),
            SandboxRoot::read_only(PathBuf::from(".")),
        ];
        let prepared = prepare_roots(&roots);
        let accepted: Vec<(&Path, bool)> = prepared
            .accepted
            .iter()
            .map(|prepared| (prepared.root.path.as_path(), prepared.root.writable))
            .collect();
        assert_eq!(accepted, vec![(dir.path(), true)]);
        let refused: Vec<RootRefusal> = prepared.refused.iter().map(|(_, r)| *r).collect();
        assert_eq!(
            refused,
            vec![RootRefusal::FilesystemRoot, RootRefusal::Relative]
        );
        Ok(())
    }

    #[test]
    fn test_root_refusal_labels_are_stable() {
        assert_eq!(RootRefusal::Relative.label(), "relative");
        assert_eq!(RootRefusal::FilesystemRoot.label(), "filesystem-root");
        assert_eq!(RootRefusal::Unverifiable.label(), "unverifiable");
        assert_eq!(RootRefusal::Unavailable.label(), "unavailable");
    }

    #[test]
    fn test_landlock_degraded_event_carries_the_sentinel_landlock_sensor_id() -> TestResult {
        let event = landlock_degraded_event(jiff::Timestamp::UNIX_EPOCH);
        assert_eq!(event.sensor.as_str(), LANDLOCK_STATUS_SENSOR_ID);
        assert!(event.actor.is_none());
        let EventKind::SensorDegraded { sensor } = &event.kind else {
            return Err(TestError::Unexpected("expected SensorDegraded".to_owned()));
        };
        assert_eq!(sensor.as_str(), LANDLOCK_STATUS_SENSOR_ID);
        Ok(())
    }

    #[test]
    fn test_sandbox_outcome_is_degraded_matches_not_enforced_and_failed_only() {
        assert!(SandboxOutcome::NotEnforced.is_degraded());
        assert!(SandboxOutcome::Failed.is_degraded());
        assert!(!SandboxOutcome::FullyEnforced.is_degraded());
        assert!(!SandboxOutcome::PartiallyEnforced.is_degraded());
    }

    // Bewusst kein Test, der `restrict_self` tatsächlich aufruft: das würde
    // einen echten Landlock-Syscall auf dem Testhost auslösen — nach
    // Aufgabenstellung ausdrücklich untersagt ("binde kein echtes
    // Landlock"). Die reine Entscheidungslogik (`classify`, `is_degraded`,
    // `default_roots`, `prepare_roots`, `landlock_degraded_event`) ist oben
    // ohne einen einzigen Landlock-Aufruf abgedeckt; `prepare_roots` öffnet
    // nur `O_PATH`-Deskriptoren.
}

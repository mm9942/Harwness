//! Landlock-Selbstbeschränkung beim Start — hart, nicht degradierend.
//!
//! # Die Landlock-Asymmetrie
//! `harw-sentinel` (Knoten AW2-19, `src/sandbox.rs`) **degradiert** bei
//! fehlender Landlock-Unterstützung nur: es läuft ohne die zusätzliche
//! Schranke weiter, weil sein Berechtigungsumfang von vornherein klein ist
//! (es hält keine einzige erhöhte Fähigkeit) und ein Sicherheitssammler, der
//! gar nicht erst startet, jede Beobachtung verliert, die er sonst geliefert
//! hätte. Für **diese** Sonde gilt das Gegenteil: sie läuft mit `CAP_BPF`,
//! und Landlock ist hier nicht eine von mehreren Verteidigungslinien,
//! sondern die einzige, die den tatsächlichen Dateisystemzugriff dieses
//! Prozesses gegen sein volles Berechtigungsvermögen einschränkt. Ohne sie
//! zu starten hieße, die Verteidigung wegzulassen und trotzdem das Risiko
//! (eine erhöhte Kernel-Fähigkeit) zu tragen. [`enforce_fs_scope`] liefert
//! deshalb `Err`, sobald der erreichte Ausgang irgendetwas außer
//! vollständiger Durchsetzung ist — `crate::main::run` behandelt das als
//! harten Startfehler, kein Weiterlaufen. Das ist Entscheidung Nr. 4 des
//! Architekturberichts dieses Programms: der Sentinel degradiert, weil er
//! unprivilegiert ist; jede der privilegierten Sonden — `harw-probe-fs`,
//! diese Sonde, ein künftiges `harw-probe-netlink` — bricht stattdessen hart
//! ab. Die Asymmetrie ist Absicht, nicht Inkonsistenz.
//!
//! # Warum diese Sonde eigene Wurzeln statt eines `ReadScope` entgegennimmt
//! `harw-probe-fs::landlock::enforce_read_scope` nimmt einen
//! `harw_dod_cap::ReadScope` entgegen, weil seine Sensoren (fanotify auf
//! konfigurierten Verzeichnissen) einen echten, vom Betreiber gewählten
//! Lesebereich haben. Die Sensoren dieser Sonde lesen dagegen **nie** über
//! `handle.scope()`/`ReadScope` — sie lesen Kernel-Ringpuffer über den
//! gemeinsamen `harw_dod_bpf::RealBpfLoader`. [`enforce_fs_scope`] nimmt
//! deshalb eine einfache Liste erlaubter Wurzeln entgegen.
//!
//! # Welche Wurzeln: nur Kernel-Schnittstellen, nie das Objektverzeichnis
//! Die Wurzeln sind ausschließlich **Kernel-Schnittstellenverzeichnisse**,
//! die der Lade-, Anheft- und Lesepfad nach der Durchsetzung noch braucht
//! (`main::KERNEL_INTERFACE_ROOTS`, gefiltert auf vorhandene Verzeichnisse
//! von `main::fs_scope_roots`): `/proc/self`, `/sys/kernel/btf`,
//! `/sys/kernel/tracing`, `/sys/kernel/debug/tracing`,
//! `/sys/devices/system/cpu`. Das eBPF-Objektverzeichnis
//! (`--*-program-path`, `$HARW_DOD_BPF_DIR`, `/usr/local/lib/harw-dod/bpf`)
//! gehört **nicht** dazu: `main::run` liest alle vier Objekte vor dem Aufruf
//! dieser Funktion vollständig in den Speicher
//! (`crate::sensors::resolve_procmon_objects`/`resolve_flow_objects`,
//! `harw_dod_bpf::BpfProgramSource::Embedded`), danach öffnet dieser Prozess
//! keine Objektdatei mehr. Diese Datei selbst kennt keine Pfade; sie setzt
//! genau die übergebene Liste durch und nimmt nichts hinzu. Eine leere Liste
//! ist zulässig und ergibt die engste Regel (kein lesbares Dateisystemziel).
//!
//! Nur Verzeichnisse, nie Einzeldateien: die Regel verwendet
//! `AccessFs::from_read`, das auch Verzeichnisrechte (`ReadDir`) enthält;
//! auf einer Einzeldatei passten diese Rechte nicht (siehe
//! `main::KERNEL_INTERFACE_ROOTS`).
//!
//! # Die cgroup2-Wurzel (PL-97 E3c): bewusste, nur auflistende Erweiterung
//! Ist ein Profil mit `scope = "cgroups"` und `include_descendants = true`
//! aktiv, läuft nach `restrict_self` der Nachfahren-Rescan
//! (`crate::scope_rescan`): er listet die Verzeichnisse unter
//! `--cgroup-root` auf und liest ihre Kennungen per `lstat`. Nur dann nimmt
//! `main::fs_scope_roots` **eine** zusätzliche Wurzel auf: `--cgroup-root`
//! mit [`RootAccess::ListDirectories`] — ausschließlich
//! `AccessFs::ReadDir` (Verzeichnis öffnen und auflisten). Das ist eine
//! **bewusste** Erweiterung der Dateisystemsicht dieser Sonde auf die
//! cgroup2-Hierarchie unter `--cgroup-root`, und zwar die engste, die der
//! Rescan braucht:
//!
//! - kein `ReadFile`: keine Schnittstellendatei (`cgroup.procs` usw.) wird
//!   lesbar — der Walk liest keine Datei, `lstat` ist unter ABI V1 kein von
//!   Landlock behandeltes Recht;
//! - kein `Execute`: `AccessFs::from_read` der Kernel-Wurzeln enthielte es,
//!   diese Wurzel bekommt es ausdrücklich nicht;
//! - kein Schreibrecht: dieses Regelwerk vergibt nirgends eines;
//! - **keine neue Fähigkeit**: das Auflisten braucht weder `CAP_BPF` noch
//!   `CAP_PERFMON` noch irgendein anderes Privileg; die systemd-Unit hält
//!   den cgroup-Baum ohnehin schreibgeschützt (`ProtectControlGroups=yes`).
//!
//! Ein Host-Profil und ein cgroup-Profil ohne Nachfahren (kein Rescan)
//! erzeugen dagegen exakt den bisherigen Regelsatz: nur die
//! Kernel-Schnittstellen mit [`RootAccess::Read`]. `main::run` lehnt ein
//! `--cgroup-root` ab, das nicht absolut ist, `/` ist oder `..` enthält
//! (`crate::profile`), damit die Erweiterung nie den ganzen Baum erfasst.
//!
//! # Was die Regel beschränkt
//! Behandelt werden die lesenden Zugriffsrechte der ABI-Stufe V1
//! (`AccessFs::from_read`: Lesen von Dateien, Auflisten von Verzeichnissen,
//! Ausführen). Außerhalb der Wurzeln ist danach jeder lesende Zugriff
//! gesperrt, unterhalb einer Wurzel genau das, was ihr [`RootAccess`] nicht
//! gewährt; schreibende Rechte behandelt dieses Regelwerk nicht.
//!
//! # Warum diese Sonde Landlock bauen kann, obwohl der ursprüngliche Auftrag
//! das Gegenteil vermutete
//! Derselbe Beleg wie bei `harw-probe-fs` gilt hier unverändert: der
//! gleichzeitig gelandete Knoten AW2-19 (`harw-sentinel/src/sandbox.rs`) hat
//! bereits eine sichere, ohne `unsafe` auskommende Landlock-Bindung in
//! diesen Workspace eingebracht (`landlock = "0.4.7"`, laut deren eigener
//! Dokumentation eine „safe abstraction for the Landlock system calls").
//! Diese Sonde übernimmt dieselbe Abhängigkeit mit derselben, bereits im
//! Workspace begründeten Versionsangabe (siehe `Cargo.toml`-Kommentar).
//!
//! # Ohne `unsafe`
//! Wie `harw-sentinel/src/sandbox.rs` und `harw-probe-fs/src/landlock.rs`:
//! jeder hier aufgerufene Baustein der `landlock`-Crate (`Ruleset`,
//! `RulesetCreated`, `PathBeneath`, `PathFd`, `.restrict_self()`) ist laut
//! deren eigener Dokumentation eine sichere Funktion. Diese Implementierung
//! wurde gegen die auf `docs.rs/landlock/0.4.7` nachgelesene API
//! geschrieben, aber **nie gegen einen echten Kernel oder Compiler
//! ausgeführt** — dieser Knoten darf kein `cargo` ausführen und kein echtes
//! Landlock binden (zentrale, sequenzielle Verifikation; Aufgabenstellung:
//! „binde kein echtes Landlock").
//!
//! # Nur `FullyEnforced` zählt als Erfolg
//! `landlock::RulesetStatus` kennt drei Ausgänge: `FullyEnforced`,
//! `PartiallyEnforced` (älterer Kernel, niedrigere ABI-Stufe — ein Teil der
//! Regeln greift) und `NotEnforced` (der Kernel unterstützt Landlock
//! überhaupt nicht). Anders als `harw-sentinel`, das `PartiallyEnforced` als
//! brauchbaren Teilschutz akzeptiert, behandelt diese Sonde — wie
//! `harw-probe-fs` — jeden Ausgang außer `FullyEnforced` als „Landlock nicht
//! verfügbar". Für einen Prozess, dessen **einzige** Schranke Landlock ist,
//! ist ein teilweiser Schutz kein Schutz, dem man vertrauen sollte.
//!
//! # Fail-closed bei nicht öffenbaren Wurzeln
//! Lässt sich eine übergebene Wurzel nicht als `PathFd` öffnen (fehlt, keine
//! Berechtigung), wird sie aus der Regel ausgelassen und geloggt, statt den
//! gesamten Versuch abzubrechen — genau wie `harw-sentinel::sandbox` und
//! `harw-probe-fs::landlock`. Das ist die sichere Richtung: eine
//! ausgelassene Wurzel wird nach `restrict_self()` **unlesbar**, nie
//! zusätzlich erlaubt.
//!
//! # Exportierte Typen
//! [`FsRoot`], [`RootAccess`] und die Funktion [`enforce_fs_scope`].
//!
//! # Nebenläufigkeit
//! [`enforce_fs_scope`] muss vor jedem weiteren Thread aus dem Hauptthread
//! aufgerufen werden — eine Landlock-Regel gilt prozessweit für alle danach
//! entstehenden Threads.
//!
//! # Fehler
//! [`crate::error::ProbeError::LandlockUnavailable`], siehe oben.
//!
//! # Examples
//! ```rust,ignore
//! use crate::landlock::enforce_fs_scope;
//! use std::path::PathBuf;
//!
//! // Objekte sind zu diesem Zeitpunkt bereits eingelesen.
//! let roots = vec![
//!     FsRoot::read("/proc/self"),
//!     FsRoot::read("/sys/kernel/btf"),
//! ];
//! enforce_fs_scope(&roots)?;
//! ```

use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, BitFlags, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus,
    Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

use crate::error::ProbeError;

/// The rights granted beneath one Landlock root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootAccess {
    /// `AccessFs::from_read`: read files, list directories, execute (ABI V1).
    /// Used for the kernel interface directories (`main::KERNEL_INTERFACE_ROOTS`).
    Read,
    /// `AccessFs::ReadDir` only: open and list directories. Used for
    /// `--cgroup-root` while a descendant rescan is active (see the module
    /// docs, "Die cgroup2-Wurzel").
    ListDirectories,
}

impl RootAccess {
    /// The access rights a `PathBeneath` rule grants for this kind of root.
    ///
    /// # Arguments
    /// - `abi` (`landlock::ABI`): the ABI level of the ruleset.
    #[must_use]
    pub fn rights(self, abi: ABI) -> BitFlags<AccessFs> {
        match self {
            Self::Read => AccessFs::from_read(abi),
            Self::ListDirectories => AccessFs::from_read(abi) & AccessFs::ReadDir,
        }
    }
}

/// One root of the Landlock ruleset: a directory and the rights beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsRoot {
    /// The directory the rule applies to (and everything beneath it).
    pub path: PathBuf,
    /// The rights granted beneath `path`.
    pub access: RootAccess,
}

impl FsRoot {
    /// A kernel interface root with [`RootAccess::Read`].
    #[must_use]
    pub fn read(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            access: RootAccess::Read,
        }
    }

    /// A list-only root with [`RootAccess::ListDirectories`].
    #[must_use]
    pub fn list_directories(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            access: RootAccess::ListDirectories,
        }
    }
}

/// Erzwingt den übergebenen Dateisystem-Ausschnitt über Landlock, bevor
/// diese Sonde ein einziges Ereignis liest.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für jede
/// erreichbare Wurzel aus `roots` eine lesende `PathBeneath`-Regel unter
/// [`landlock::ABI::V1`] auf (mit den Rechten aus [`RootAccess::rights`])
/// und ruft `restrict_self()` auf. Nur
/// [`RulesetStatus::FullyEnforced`] gilt als Erfolg — auch bei einer leeren
/// `roots`-Liste (dann eine Regel, die kein Dateisystemziel erlaubt).
///
/// # Arguments
/// - `roots` (`&[FsRoot]`): die einzigen danach noch lesbaren Wurzeln —
///   Kernel-Schnittstellenverzeichnisse und, nur bei aktivem
///   Nachfahren-Rescan, `--cgroup-root` zum Auflisten
///   (`main::fs_scope_roots`), nie das eBPF-Objektverzeichnis (siehe
///   Moduldoku, Abschnitte „Welche Wurzeln" und „Die cgroup2-Wurzel"). Nicht
///   öffenbare Einträge werden ausgelassen (fail-closed).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`ProbeError::LandlockUnavailable`]: bei jedem Aufbaufehler
///   (`handle_access`, `create`, `add_rules`, `restrict_self`) und bei
///   jedem Ausgang außer `FullyEnforced`.
pub fn enforce_fs_scope(roots: &[FsRoot]) -> Result<(), ProbeError> {
    let abi = ABI::V1;

    let ruleset = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    let ruleset = ruleset
        .handle_access(AccessFs::from_read(abi))
        .map_err(|error| {
            tracing::error!(error = %error, "landlock handle_access failed; refusing to start");
            ProbeError::LandlockUnavailable
        })?;

    let created = ruleset
        .create()
        .map_err(|error| {
            tracing::error!(error = %error, "landlock ruleset create failed; refusing to start");
            ProbeError::LandlockUnavailable
        })?
        .set_compatibility(CompatLevel::BestEffort);

    let rules: Vec<Result<PathBeneath<PathFd>, landlock::RulesetError>> = roots
        .iter()
        .filter_map(|root| open_rule(&root.path, root.access.rights(abi)))
        .collect();

    let created = created.add_rules(rules).map_err(|error| {
        tracing::error!(error = %error, "landlock add_rules failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    let status: RestrictionStatus = created.restrict_self().map_err(|error| {
        tracing::error!(error = %error, "landlock restrict_self failed; refusing to start");
        ProbeError::LandlockUnavailable
    })?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        tracing::info!(root_count = roots.len(), "landlock fs scope fully enforced");
        Ok(())
    } else {
        tracing::error!(
            status = ?status.ruleset,
            "landlock did not fully enforce the fs scope; refusing to start"
        );
        Err(ProbeError::LandlockUnavailable)
    }
}

/// Baut, falls möglich, eine lesende `PathBeneath`-Regel für eine einzelne
/// Wurzel.
///
/// # Arguments
/// - `root` (`&Path`): die Wurzel.
/// - `rights` (`BitFlags<AccessFs>`): die gewährten Rechte
///   ([`RootAccess::rights`]).
///
/// # Returns
/// `None`, wenn der Pfad nicht als `PathFd` geöffnet werden kann (fehlt,
/// keine Berechtigung) — geloggt, aber kein Abbruch des gesamten Aufbaus
/// (siehe Moduldoku, Abschnitt „Fail-closed"). Sonst `Some(Ok(rule))`.
fn open_rule(
    root: &Path,
    rights: BitFlags<AccessFs>,
) -> Option<Result<PathBeneath<PathFd>, landlock::RulesetError>> {
    match PathFd::new(root) {
        Ok(fd) => Some(Ok(PathBeneath::new(fd, rights))),
        Err(error) => {
            tracing::warn!(
                path = %root.display(),
                error = %error,
                "landlock path unavailable while building the fs scope rule; this root will not be readable"
            );
            None
        }
    }
}

// Kein Test in dieser Datei ruft `enforce_fs_scope` (echtes `restrict_self`)
// oder `open_rule` (das `PathFd::new` aufruft) auf — dieselbe strikte
// Disziplin wie `harw-sentinel::sandbox` und `harw-probe-fs::landlock`
// ("binde kein echtes Landlock"). Geprüft wird nur die reine Abbildung
// `RootAccess::rights` (Bitmengen, kein Systemaufruf); die übrige
// Entscheidungslogik (Fail-closed bei nicht öffenbaren Wurzeln, „nur
// `FullyEnforced` zählt als Erfolg") ist in der Moduldoku begründet. Welche
// Wurzeln mit welchen Rechten entstehen, prüft `main::tests` über die reine
// Funktion `main::landlock_roots`.
#[cfg(test)]
mod tests {
    use landlock::{ABI, AccessFs, BitFlags};

    use super::{FsRoot, RootAccess};

    #[test]
    fn read_roots_keep_exactly_the_v1_read_rights() {
        assert_eq!(
            RootAccess::Read.rights(ABI::V1),
            AccessFs::from_read(ABI::V1)
        );
    }

    #[test]
    fn list_only_roots_grant_read_dir_and_nothing_else() {
        let rights = RootAccess::ListDirectories.rights(ABI::V1);
        assert_eq!(rights, BitFlags::from(AccessFs::ReadDir));
        assert!(!rights.contains(AccessFs::Execute));
        assert!(!rights.contains(AccessFs::ReadFile));
        assert!((rights & AccessFs::from_write(ABI::V1)).is_empty());
        // Nie mehr als das, was der Regelsatz überhaupt behandelt.
        assert_eq!(rights & AccessFs::from_read(ABI::V1), rights);
    }

    #[test]
    fn constructors_pick_the_matching_access() {
        assert_eq!(FsRoot::read("/proc/self").access, RootAccess::Read);
        assert_eq!(
            FsRoot::list_directories("/sys/fs/cgroup").access,
            RootAccess::ListDirectories
        );
    }
}

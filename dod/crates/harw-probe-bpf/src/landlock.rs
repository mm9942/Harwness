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
//! # Was die Regel beschränkt
//! Behandelt werden die lesenden Zugriffsrechte der ABI-Stufe V1
//! (`AccessFs::from_read`: Lesen von Dateien, Auflisten von Verzeichnissen,
//! Ausführen). Außerhalb der Wurzeln ist danach jeder lesende Zugriff
//! gesperrt; schreibende Rechte behandelt dieses Regelwerk nicht.
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
//! Keine — nur die Funktion [`enforce_fs_scope`].
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
//! let roots = vec![PathBuf::from("/proc/self"), PathBuf::from("/sys/kernel/btf")];
//! enforce_fs_scope(&roots)?;
//! ```

use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, RestrictionStatus, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

use crate::error::ProbeError;

/// Erzwingt den übergebenen Dateisystem-Ausschnitt über Landlock, bevor
/// diese Sonde ein einziges Ereignis liest.
///
/// # Description
/// Baut ein `landlock::Ruleset` mit `CompatLevel::BestEffort` (damit ein
/// älterer Kernel `PartiallyEnforced` statt eines harten `Err` aus der
/// `landlock`-Crate selbst liefert — diese Funktion entscheidet danach
/// selbst, siehe Moduldoku, dass auch das nicht genügt), nimmt für jede
/// erreichbare Wurzel aus `roots` eine lesende `PathBeneath`-Regel unter
/// [`landlock::ABI::V1`] auf und ruft `restrict_self()` auf. Nur
/// [`RulesetStatus::FullyEnforced`] gilt als Erfolg — auch bei einer leeren
/// `roots`-Liste (dann eine Regel, die kein Dateisystemziel erlaubt).
///
/// # Arguments
/// - `roots` (`&[std::path::PathBuf]`): die einzigen danach noch lesbaren
///   Wurzeln — ausschließlich Kernel-Schnittstellenverzeichnisse
///   (`main::fs_scope_roots`), nie das eBPF-Objektverzeichnis (siehe
///   Moduldoku, Abschnitt „Welche Wurzeln"). Nicht öffenbare Einträge werden
///   ausgelassen (fail-closed).
///
/// # Returns
/// `Ok(())` ausschließlich bei `RulesetStatus::FullyEnforced`.
///
/// # Errors
/// - [`ProbeError::LandlockUnavailable`]: bei jedem Aufbaufehler
///   (`handle_access`, `create`, `add_rules`, `restrict_self`) und bei
///   jedem Ausgang außer `FullyEnforced`.
pub fn enforce_fs_scope(roots: &[PathBuf]) -> Result<(), ProbeError> {
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
        .filter_map(|root| open_rule(root, abi))
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
/// # Returns
/// `None`, wenn der Pfad nicht als `PathFd` geöffnet werden kann (fehlt,
/// keine Berechtigung) — geloggt, aber kein Abbruch des gesamten Aufbaus
/// (siehe Moduldoku, Abschnitt „Fail-closed"). Sonst `Some(Ok(rule))`.
fn open_rule(root: &Path, abi: ABI) -> Option<Result<PathBeneath<PathFd>, landlock::RulesetError>> {
    match PathFd::new(root) {
        Ok(fd) => Some(Ok(PathBeneath::new(fd, AccessFs::from_read(abi)))),
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

// Bewusst kein `#[cfg(test)] mod tests` in dieser Datei: jeder denkbare Test
// müsste entweder `enforce_fs_scope` (echtes `restrict_self`) oder
// `open_rule` (das seinerseits `PathFd::new` aufruft) ausführen. Ein
// `PathFd::new`-Aufruf öffnet zwar nur einen gewöhnlichen Dateideskriptor und
// bindet keine Landlock-Regel, aber dieser Knoten hält sich an dieselbe,
// striktere Disziplin wie `harw-sentinel::sandbox` und
// `harw-probe-fs::landlock` (dort ebenfalls kein Test, der irgendeine
// Funktion der `landlock`-Crate aufruft) — nach Aufgabenstellung
// ausdrücklich untersagt ("binde kein echtes Landlock"). Die
// Entscheidungslogik dieser Datei (Fail-closed bei nicht öffenbaren
// Wurzeln, „nur `FullyEnforced` zählt als Erfolg") ist in der Moduldoku
// begründet statt durch einen Test belegt, der einen Kernel-Zustand
// voraussetzen würde.

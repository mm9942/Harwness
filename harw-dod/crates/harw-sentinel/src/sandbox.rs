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
//! (`--proc-root`, `--thermal-root`, `--workspace-root`, `--blockio-root`,
//! `--gpu-root`, `--cgroup-root`) plus lesend-
//! schreibend den Root-Space (Telemetrie-Sink, IPC-Socket) — eine zweite,
//! vom Sensor-Vertrag unabhängige Schranke, falls eine künftige Änderung
//! versehentlich einen Lesezugriff außerhalb des vorgesehenen Bereichs
//! einführt.
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

use std::path::{Path, PathBuf};

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
/// nimmt für jede erreichbare Wurzel eine `PathBeneath`-Regel mit
/// [`AccessFs::from_all`] (`writable`) oder [`AccessFs::from_read`]
/// (sonst) unter [`landlock::ABI::V1`] auf, und ruft `restrict_self()` auf
/// dem Ergebnis. Eine Wurzel, die beim Öffnen scheitert (z. B. ein noch
/// nicht angelegtes Verzeichnis), wird übersprungen und geloggt — sie
/// bricht den gesamten Versuch nicht ab.
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

    let rules: Vec<Result<PathBeneath<PathFd>, landlock::RulesetError>> = roots
        .iter()
        .filter_map(|root| open_rule(root, abi))
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

/// Baut, falls möglich, eine `PathBeneath`-Regel für eine einzelne Wurzel.
///
/// # Returns
/// `None`, wenn der Pfad nicht als `PathFd` geöffnet werden kann (fehlt,
/// keine Berechtigung) — geloggt, aber kein Abbruch. Sonst `Some(Ok(rule))`.
fn open_rule(
    root: &SandboxRoot,
    abi: ABI,
) -> Option<Result<PathBeneath<PathFd>, landlock::RulesetError>> {
    match PathFd::new(&root.path) {
        Ok(fd) => {
            let access = if root.writable {
                AccessFs::from_all(abi)
            } else {
                AccessFs::from_read(abi)
            };
            Some(Ok(PathBeneath::new(fd, access)))
        }
        Err(error) => {
            tracing::warn!(
                path = %root.path.display(),
                error = %error,
                "landlock path unavailable; skipping this rule"
            );
            None
        }
    }
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
/// - `proc_root`, `thermal_root`, `workspace_root`, `blockio_root`,
///   `gpu_root`, `cgroup_root` (`&Path`): die sechs Sensor-Lesewurzeln
///   (siehe [`crate::sensors::SensorRoots`]).
/// - `home` (`&Path`): der Root-Space — lesend-schreibend, weil der
///   Telemetrie-Sink und der IPC-Socket darunter liegen.
///
/// # Returns
/// Sieben [`SandboxRoot`]s: sechs nur lesbar, `home` lesend-schreibend.
#[must_use]
pub fn default_roots(
    proc_root: &Path,
    thermal_root: &Path,
    workspace_root: &Path,
    blockio_root: &Path,
    gpu_root: &Path,
    cgroup_root: &Path,
    home: &Path,
) -> Vec<SandboxRoot> {
    vec![
        SandboxRoot::read_only(proc_root.to_path_buf()),
        SandboxRoot::read_only(thermal_root.to_path_buf()),
        SandboxRoot::read_only(workspace_root.to_path_buf()),
        SandboxRoot::read_only(blockio_root.to_path_buf()),
        SandboxRoot::read_only(gpu_root.to_path_buf()),
        SandboxRoot::read_only(cgroup_root.to_path_buf()),
        SandboxRoot::read_write(home.to_path_buf()),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        LANDLOCK_STATUS_SENSOR_ID, SandboxOutcome, SandboxRoot, default_roots,
        landlock_degraded_event,
    };
    use crate::test_support::{TestError, TestResult};
    use harw_dod_signals::EventKind;
    use std::path::PathBuf;

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
            &PathBuf::from("/proc"),
            &PathBuf::from("/sys/class/thermal"),
            &PathBuf::from("."),
            &PathBuf::from("/sys/block"),
            &PathBuf::from("/sys/class/drm"),
            &PathBuf::from("/sys/fs/cgroup"),
            &PathBuf::from("/home/x/.harw"),
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
    // `default_roots`, `landlock_degraded_event`) ist oben ohne einen
    // einzigen Landlock-Aufruf abgedeckt.
}

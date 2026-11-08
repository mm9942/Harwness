//! Welche Container-Sensoren der Sentinel registriert (PL-100 WP1.8).
//!
//! # Verantwortungsbereich
//! [`plan`] übersetzt den wirksamen Schalter
//! ([`harw_dod_config::ContainerSensorMode`]: `--container-sensor` vor
//! `[sentinel] container_sensor` vor der Vorgabe `auto`) in die Liste der
//! [`PlannedLayout`]s, für die [`crate::sensors::build_sensors`] je einen
//! `harw_dod_container::ContainerSensor` baut, und der erkannten, nicht
//! unterstützten Anordnungen ([`UnsupportedLayout`]).
//!
//! # Bekannte Anordnungen
//! `harw_dod_container::ContainerSensor` liest die `*.scope`-Verzeichnisse
//! **eines** cgroup-Verzeichnisses. Zwei Anordnungen sind hier bekannt:
//!
//! | Anordnung | cgroup-Verzeichnis | Scope | Laufzeit-Socket (unter `/run`) |
//! |---|---|---|---|
//! | [`ContainerLayout::Machine`] (Podman als root, systemd-cgroup-Manager) | `machine.slice` | `libpod-<id>.scope` | `podman/podman.sock` |
//! | [`ContainerLayout::System`] (Docker, systemd-cgroup-Treiber) | `system.slice` | `docker-<id>.scope` | `docker.sock` |
//!
//! Nicht abgedeckt: rootless Podman (Scopes unter `user@<uid>.service`),
//! CRI-O/Kubernetes (verschachtelte `kubepods`-Slices) und der
//! `cgroupfs`-Treiber von Docker (keine `.scope`-Namen; erkannt und
//! gemeldet, siehe „Nicht unterstützte Anordnungen").
//!
//! # Registrierung je Schalter
//! `auto` und `on` registrieren **jede** bekannte Anordnung, `off` keine.
//! Eine Registrierung nur für beim Start erkannte Laufzeiten wäre stille
//! Blindheit (THREAT-MODEL B19): rootful Podman ist daemonlos (kein
//! `podman.socket` nötig), der Sentinel startet beim Boot vor jedem
//! Container, und jeder später in `machine.slice` gestartete Container
//! bliebe unbeobachtet. Ein Fehlalarm entsteht dadurch nicht: der Sensor
//! zählt nur Scopes mit Container-Präfix (`libpod-`, `docker-`, `crio-`)
//! und gültiger Container-Kennung, ein Host ohne Container liest also leer.
//!
//! # Fehlendes cgroup-Verzeichnis
//! systemd legt `machine.slice` erst mit dem ersten Container- oder
//! Maschinen-Scope an (und kann es nach dem letzten wieder entfernen). Ein
//! fehlendes Verzeichnis darf einen Sensor deshalb auf einer cgroup-v2-Wurzel
//! (`cgroup.controllers`) nie stilllegen — `SourceUnavailable` ist permanent,
//! `Degraded` im Sentinel endgültig, und jeder später gestartete Container
//! bliebe unbeobachtet. Ohne cgroup-v2-Wurzel kann kein Container-Sensor
//! Scopes sehen; das ist echte Blindheit und wird gemeldet („degraded
//! otherwise"). Je [`PlannedLayout`] gilt darum:
//!
//! | Schalter, Lage beim Start | [`MissingRoot`] | fehlendes Verzeichnis beim Poll |
//! |---|---|---|
//! | `auto`, cgroup-v2-Wurzel | `Empty` | leere Messung |
//! | `on`, cgroup-v2-Wurzel, Anordnung erkannt | `Empty` | leere Messung |
//! | `on`, Anordnung nicht erkannt | `Unavailable` | beeinträchtigt |
//! | `auto` oder `on` ohne cgroup-v2-Wurzel | `Unavailable` | beeinträchtigt |
//!
//! Mit `on` erklärt der Betreiber Container für erwartet: fehlt dann das
//! Verzeichnis einer Anordnung, deren Laufzeit beim Start weder per Socket
//! noch per laufendem Scope erkannt wurde, ist das eine Beeinträchtigung.
//! Erkannt heißt: einer ihrer Laufzeit-Sockets existiert **oder** ihr
//! cgroup-Verzeichnis enthält schon einen Scope mit ihrem Präfix (höchstens
//! [`MAX_SCAN_ENTRIES`] Einträge werden gelesen).
//!
//! # Nicht unterstützte Anordnungen
//! Docker mit `cgroupfs`-Treiber legt seine Container unter
//! `<cgroup>/docker/<id>` ab, ohne `.scope`-Namen: kein Container-Sensor
//! sieht sie. Existiert dieses Verzeichnis, nennt der Plan die Anordnung in
//! [`ContainerPlan::unsupported`] (bei `auto` und `on`); der Sentinel
//! reicht dafür einmal beim Start ein `SensorDegraded`-Ereignis
//! ([`UnsupportedLayout::degraded_event`]) nach — keine stille Blindheit
//! (THREAT-MODEL B19).
//!
//! # Erwartete Container
//! Der Sentinel kennt keine Liste der Container, die harw selbst gestartet
//! hat; jeder Sensor läuft deshalb mit leerer Erwartungsmenge und meldet
//! jeden Container-Scope nur als `container_unowned_<id>` — er liest dann
//! nichts weiter aus dessen Prozessen (Vertrauensregel von
//! `harw_dod_container`).
//!
//! # Nebenläufigkeit
//! [`plan`] läuft einmal beim Start, vor Landlock (die Laufzeit-Sockets, die
//! `on` prüft, liegen unter `/run`, keiner Landlock-Wurzel).
//!
//! # Fehler
//! Keine: ein nicht lesbares Verzeichnis oder ein fehlender Socket heißt
//! „nicht erkannt" (nur für `on` von Belang).
//!
//! # Examples
//! ```rust,ignore
//! use crate::container_plan::{plan, ContainerLayout};
//! use harw_dod_config::ContainerSensorMode;
//! use std::path::Path;
//!
//! let all = plan(ContainerSensorMode::On, Path::new("/sys/fs/cgroup"), Path::new("/run"));
//! let layouts: Vec<ContainerLayout> = all.layouts.iter().map(|planned| planned.layout).collect();
//! assert_eq!(layouts, ContainerLayout::ALL.to_vec());
//! ```

use std::path::Path;

use harw_dod_config::ContainerSensorMode;
use harw_dod_container::MissingRoot;
use harw_dod_signals::{EventKind, SecurityEvent};
use harw_types::SensorId;
use jiff::Timestamp;

/// Wurzel der Laufzeit-Sockets in Produktion.
pub const DEFAULT_RUN_ROOT: &str = "/run";

/// Höchstzahl der Verzeichniseinträge, die die Erkennung je Anordnung liest.
pub const MAX_SCAN_ENTRIES: usize = 4096;

/// Datei, an der die Erkennung eine cgroup-v2-Wurzel erkennt.
const CGROUP_V2_MARKER: &str = "cgroup.controllers";

/// Eine bekannte Anordnung von Container-Scopes im cgroup-Baum (siehe
/// Moduldoku).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerLayout {
    /// Podman als root: `machine.slice/libpod-<id>.scope`.
    Machine,
    /// Docker: `system.slice/docker-<id>.scope`.
    System,
}

impl ContainerLayout {
    /// Jede bekannte Anordnung, in Registrierungsreihenfolge.
    pub const ALL: [Self; 2] = [Self::Machine, Self::System];

    /// Die Sensor-Kennung des zugehörigen Container-Sensors.
    #[must_use]
    pub const fn sensor_id(self) -> &'static str {
        match self {
            Self::Machine => "container-machine",
            Self::System => "container-system",
        }
    }

    /// Das cgroup-Verzeichnis relativ zur cgroup-v2-Wurzel.
    #[must_use]
    pub const fn cgroup_parent(self) -> &'static str {
        match self {
            Self::Machine => "machine.slice",
            Self::System => "system.slice",
        }
    }

    /// Das Scope-Präfix, an dem die Erkennung (`on`) eine laufende Instanz
    /// sieht.
    #[must_use]
    pub const fn scope_prefix(self) -> &'static str {
        match self {
            Self::Machine => "libpod-",
            Self::System => "docker-",
        }
    }

    /// Die Laufzeit-Sockets relativ zu [`DEFAULT_RUN_ROOT`].
    #[must_use]
    pub const fn runtime_sockets(self) -> &'static [&'static str] {
        match self {
            Self::Machine => &["podman/podman.sock"],
            Self::System => &["docker.sock"],
        }
    }

    /// Stabiler Bezeichner für das Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Machine => "podman-machine-slice",
            Self::System => "docker-system-slice",
        }
    }
}

/// Eine zu registrierende Anordnung und die Bedeutung ihres fehlenden
/// cgroup-Verzeichnisses (siehe Moduldoku, „Fehlendes cgroup-Verzeichnis").
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedLayout {
    /// Die Anordnung.
    pub layout: ContainerLayout,
    /// Was ein fehlendes `cgroup_parent()` beim Poll bedeutet.
    pub missing_slice: MissingRoot,
}

impl PlannedLayout {
    /// Stabiler Bezeichner von [`Self::missing_slice`] für Journal und
    /// `--print-sandbox`: `on-demand` (leere Messung) oder `required`
    /// (beeinträchtigt).
    #[must_use]
    pub const fn availability_label(self) -> &'static str {
        match self.missing_slice {
            MissingRoot::Empty => "on-demand",
            MissingRoot::Unavailable => "required",
        }
    }
}

/// Eine erkannte, aber von keinem Container-Sensor lesbare Anordnung (siehe
/// Moduldoku, „Nicht unterstützte Anordnungen").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedLayout {
    /// Docker mit `cgroupfs`-Treiber: `<cgroup>/docker/<id>`.
    DockerCgroupfs,
}

impl UnsupportedLayout {
    /// Jede bekannte nicht unterstützte Anordnung.
    pub const ALL: [Self; 1] = [Self::DockerCgroupfs];

    /// Das Verzeichnis relativ zur cgroup-v2-Wurzel, an dem die Erkennung
    /// die Anordnung sieht.
    #[must_use]
    pub const fn cgroup_dir(self) -> &'static str {
        match self {
            Self::DockerCgroupfs => "docker",
        }
    }

    /// Die Kennung, unter der der Sentinel die Anordnung als
    /// `SensorDegraded` meldet. **Kein** registrierter Sensor (wie
    /// `crate::sandbox::LANDLOCK_STATUS_SENSOR_ID`).
    #[must_use]
    pub const fn status_sensor_id(self) -> &'static str {
        match self {
            Self::DockerCgroupfs => "container-docker-cgroupfs",
        }
    }

    /// Stabiler Bezeichner für das Journal.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DockerCgroupfs => "docker-cgroupfs",
        }
    }

    /// Das `SensorDegraded`-Ereignis, das der Sentinel einmal beim Start in
    /// den Ring nachreicht.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): Zeitstempel des Ereignisses.
    ///
    /// # Returns
    /// Ein Ereignis ohne Akteur; Quelle und betroffener Sensor sind
    /// [`Self::status_sensor_id`].
    #[must_use]
    pub fn degraded_event(self, now: Timestamp) -> SecurityEvent {
        let sensor = SensorId::from_str(self.status_sensor_id());
        SecurityEvent {
            sensor: sensor.clone(),
            observed_at: now,
            actor: None,
            kind: EventKind::SensorDegraded { sensor },
        }
    }

    // Ob das Verzeichnis der Anordnung unter `cgroup_root` existiert.
    fn detected(self, cgroup_root: &Path) -> bool {
        std::fs::symlink_metadata(cgroup_root.join(self.cgroup_dir()))
            .is_ok_and(|meta| meta.is_dir())
    }
}

/// Die Container-Anordnungen dieses Laufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPlan {
    /// Der wirksame Schalter.
    pub mode: ContainerSensorMode,
    /// Die zu registrierenden Anordnungen.
    pub layouts: Vec<PlannedLayout>,
    /// Erkannte, nicht unterstützte Anordnungen (je ein
    /// `SensorDegraded`-Ereignis beim Start).
    pub unsupported: Vec<UnsupportedLayout>,
}

impl ContainerPlan {
    /// Die Anordnungen ohne ihre Verfügbarkeitsregel (nur Tests).
    #[cfg(test)]
    #[must_use]
    pub fn layout_kinds(&self) -> Vec<ContainerLayout> {
        self.layouts.iter().map(|planned| planned.layout).collect()
    }
}

/// Bestimmt die zu registrierenden Container-Sensoren.
///
/// # Description
/// `off`: nichts. `auto` und `on`: jede Anordnung aus
/// [`ContainerLayout::ALL`] mit der Verfügbarkeitsregel der Moduldoku
/// („Fehlendes cgroup-Verzeichnis"); erkannte, nicht unterstützte
/// Anordnungen kommen in [`ContainerPlan::unsupported`].
///
/// # Arguments
/// - `mode` (`ContainerSensorMode`): der wirksame Schalter.
/// - `cgroup_root` (`&Path`): die cgroup-v2-Wurzel (`--cgroup-root`).
/// - `run_root` (`&Path`): die Wurzel der Laufzeit-Sockets
///   ([`DEFAULT_RUN_ROOT`] in Produktion).
///
/// # Returns
/// Den [`ContainerPlan`] (siehe Moduldoku, „Registrierung je Schalter",
/// „Fehlendes cgroup-Verzeichnis" und „Nicht unterstützte Anordnungen").
#[must_use]
pub fn plan(mode: ContainerSensorMode, cgroup_root: &Path, run_root: &Path) -> ContainerPlan {
    if mode == ContainerSensorMode::Off {
        return ContainerPlan {
            mode,
            layouts: Vec::new(),
            unsupported: Vec::new(),
        };
    }
    let cgroup_v2 = is_cgroup_v2_root(cgroup_root);
    let layouts = ContainerLayout::ALL
        .into_iter()
        .map(|layout| PlannedLayout {
            layout,
            missing_slice: missing_slice(mode, layout, cgroup_v2, cgroup_root, run_root),
        })
        .collect();
    let unsupported = UnsupportedLayout::ALL
        .into_iter()
        .filter(|layout| layout.detected(cgroup_root))
        .collect();
    ContainerPlan {
        mode,
        layouts,
        unsupported,
    }
}

// Die Verfügbarkeitsregel einer Anordnung (Tabelle der Moduldoku,
// „Fehlendes cgroup-Verzeichnis"); `off` kommt hier nicht an.
fn missing_slice(
    mode: ContainerSensorMode,
    layout: ContainerLayout,
    cgroup_v2: bool,
    cgroup_root: &Path,
    run_root: &Path,
) -> MissingRoot {
    if !cgroup_v2 {
        return MissingRoot::Unavailable;
    }
    match mode {
        ContainerSensorMode::Auto => MissingRoot::Empty,
        ContainerSensorMode::On if detected(layout, cgroup_root, run_root) => MissingRoot::Empty,
        ContainerSensorMode::On | ContainerSensorMode::Off => MissingRoot::Unavailable,
    }
}

// Ob `cgroup_root` eine cgroup-v2-Wurzel ist (`cgroup.controllers` ist eine
// Datei).
fn is_cgroup_v2_root(cgroup_root: &Path) -> bool {
    std::fs::symlink_metadata(cgroup_root.join(CGROUP_V2_MARKER)).is_ok_and(|meta| meta.is_file())
}

// Laufzeit-Socket vorhanden oder schon ein Scope mit dem Präfix der
// Anordnung im cgroup-Verzeichnis.
fn detected(layout: ContainerLayout, cgroup_root: &Path, run_root: &Path) -> bool {
    layout
        .runtime_sockets()
        .iter()
        .any(|socket| std::fs::symlink_metadata(run_root.join(socket)).is_ok())
        || has_scope_with_prefix(
            &cgroup_root.join(layout.cgroup_parent()),
            layout.scope_prefix(),
        )
}

// Ob `dir` (höchstens `MAX_SCAN_ENTRIES` Einträge) ein `<prefix>*.scope`
// enthält.
fn has_scope_with_prefix(dir: &Path, prefix: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .take(MAX_SCAN_ENTRIES)
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".scope"))
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use harw_dod_config::ContainerSensorMode;
    use harw_dod_container::MissingRoot;
    use harw_dod_signals::EventKind;
    use jiff::Timestamp;

    use super::{ContainerLayout, PlannedLayout, UnsupportedLayout, plan};
    use crate::test_support::{TestResult, ctx};

    struct Host {
        cgroup: tempfile::TempDir,
        run: tempfile::TempDir,
    }

    fn host() -> TestResult<Host> {
        Ok(Host {
            cgroup: tempfile::tempdir().map_err(ctx("cgroup tempdir"))?,
            run: tempfile::tempdir().map_err(ctx("run tempdir"))?,
        })
    }

    // Eine cgroup-v2-Wurzel (`cgroup.controllers`) im Fixture.
    fn mark_cgroup_v2(host: &Host) -> TestResult {
        fs::write(
            host.cgroup.path().join("cgroup.controllers"),
            b"cpu memory\n",
        )
        .map_err(ctx("cgroup.controllers"))
    }

    // Ein Laufzeit-Socket (Platzhalterdatei) unter dem Fixture-`/run`.
    fn socket(host: &Host, relative: &str) -> TestResult {
        let path = host.run.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(ctx("socket parent"))?;
        }
        fs::write(path, b"").map_err(ctx("socket stand-in"))
    }

    // Jede bekannte Anordnung mit derselben Verfügbarkeitsregel.
    fn every_layout(missing_slice: MissingRoot) -> Vec<PlannedLayout> {
        ContainerLayout::ALL
            .into_iter()
            .map(|layout| PlannedLayout {
                layout,
                missing_slice,
            })
            .collect()
    }

    /// Review R2 (B19): ohne Laufzeit-Socket und ohne Scope registriert
    /// `auto` auf einer cgroup-v2-Wurzel trotzdem jede Anordnung, und ein
    /// fehlendes Verzeichnis ist eine leere Messung (daemonloses Podman,
    /// Start vor jedem Container); `on` registriert jede Anordnung als
    /// `required`, `off` keine.
    #[test]
    fn test_modes_without_any_runtime() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        let (cgroup, run) = (host.cgroup.path(), host.run.path());
        let auto = plan(ContainerSensorMode::Auto, cgroup, run);
        assert_eq!(auto.layouts, every_layout(MissingRoot::Empty));
        assert!(
            auto.layouts
                .iter()
                .all(|planned| planned.availability_label() == "on-demand")
        );
        let on = plan(ContainerSensorMode::On, cgroup, run);
        assert_eq!(on.layouts, every_layout(MissingRoot::Unavailable));
        assert!(
            plan(ContainerSensorMode::Off, cgroup, run)
                .layouts
                .is_empty()
        );
        Ok(())
    }

    /// Ein Laufzeit-Socket ändert an `auto` nichts: jede Anordnung bleibt
    /// `on-demand`.
    #[test]
    fn test_auto_is_on_demand_with_or_without_a_runtime_socket() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        socket(&host, "docker.sock")?;
        assert_eq!(
            plan(
                ContainerSensorMode::Auto,
                host.cgroup.path(),
                host.run.path()
            )
            .layouts,
            every_layout(MissingRoot::Empty)
        );
        Ok(())
    }

    /// Review Runde 1: Podman-Socket ohne `machine.slice` (kein Container
    /// seit dem Boot) — der Sensor läuft und wird nie stillgelegt; mit `on`
    /// gilt für die erkannte Anordnung dasselbe, die nicht erkannte bleibt
    /// beeinträchtigt.
    #[test]
    fn test_a_socket_without_its_slice_is_on_demand_in_auto_and_on() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        socket(&host, "podman/podman.sock")?;
        let machine_on_demand = PlannedLayout {
            layout: ContainerLayout::Machine,
            missing_slice: MissingRoot::Empty,
        };
        let auto = plan(
            ContainerSensorMode::Auto,
            host.cgroup.path(),
            host.run.path(),
        );
        assert_eq!(auto.layouts, every_layout(MissingRoot::Empty));
        assert_eq!(machine_on_demand.availability_label(), "on-demand");
        let on = plan(ContainerSensorMode::On, host.cgroup.path(), host.run.path());
        assert_eq!(
            on.layouts,
            vec![
                machine_on_demand,
                PlannedLayout {
                    layout: ContainerLayout::System,
                    missing_slice: MissingRoot::Unavailable,
                },
            ]
        );
        Ok(())
    }

    /// Review R2 („degraded otherwise"): ohne cgroup-v2-Wurzel ist jede
    /// Anordnung `required` — mit und ohne erkannte Laufzeit, bei `auto`
    /// und `on`: ihr fehlendes Verzeichnis ist dann echte Blindheit und
    /// legt den Sensor beim ersten Poll als beeinträchtigt still.
    #[test]
    fn test_without_a_cgroup_v2_root_every_layout_is_required() -> TestResult {
        let host = host()?;
        for mode in [ContainerSensorMode::Auto, ContainerSensorMode::On] {
            let planned = plan(mode, host.cgroup.path(), host.run.path());
            assert_eq!(planned.layouts, every_layout(MissingRoot::Unavailable));
        }
        socket(&host, "podman/podman.sock")?;
        let planned = plan(
            ContainerSensorMode::Auto,
            host.cgroup.path(),
            host.run.path(),
        );
        assert_eq!(planned.layouts, every_layout(MissingRoot::Unavailable));
        assert_eq!(
            planned
                .layouts
                .first()
                .map(|layout| layout.availability_label()),
            Some("required")
        );
        Ok(())
    }

    /// Mit `on` genügt ein schon laufender Container (Scope mit dem
    /// Präfix) als Erkennung; ein fremder Scope oder ein Scope einer anderen
    /// Laufzeit nicht.
    #[test]
    fn test_on_detects_a_running_container_scope() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        let machine = host.cgroup.path().join("machine.slice");
        fs::create_dir_all(machine.join("systemd-nspawn@x.service")).map_err(ctx("unit"))?;
        fs::create_dir_all(host.cgroup.path().join("system.slice").join("sshd.service"))
            .map_err(ctx("system"))?;
        assert_eq!(
            plan(ContainerSensorMode::On, host.cgroup.path(), host.run.path()).layouts,
            every_layout(MissingRoot::Unavailable)
        );
        fs::create_dir_all(machine.join(format!("libpod-{}.scope", "a".repeat(64))))
            .map_err(ctx("libpod scope"))?;
        assert_eq!(
            plan(ContainerSensorMode::On, host.cgroup.path(), host.run.path()).layouts,
            vec![
                PlannedLayout {
                    layout: ContainerLayout::Machine,
                    missing_slice: MissingRoot::Empty,
                },
                PlannedLayout {
                    layout: ContainerLayout::System,
                    missing_slice: MissingRoot::Unavailable,
                },
            ]
        );
        Ok(())
    }

    /// Review Runde 1: Docker mit `cgroupfs`-Treiber (`<cgroup>/docker/<id>`)
    /// wird als nicht unterstützt gemeldet — bei `auto` und `on`, nie bei
    /// `off` — und ergibt ein `SensorDegraded` mit eigener Kennung.
    #[test]
    fn test_the_docker_cgroupfs_layout_is_reported_unsupported() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        socket(&host, "docker.sock")?;
        fs::create_dir_all(host.cgroup.path().join("docker").join("b".repeat(64)))
            .map_err(ctx("cgroupfs container"))?;
        for mode in [ContainerSensorMode::Auto, ContainerSensorMode::On] {
            let planned = plan(mode, host.cgroup.path(), host.run.path());
            assert_eq!(planned.unsupported, vec![UnsupportedLayout::DockerCgroupfs]);
        }
        assert!(
            plan(
                ContainerSensorMode::Off,
                host.cgroup.path(),
                host.run.path()
            )
            .unsupported
            .is_empty()
        );
        let event = UnsupportedLayout::DockerCgroupfs.degraded_event(Timestamp::UNIX_EPOCH);
        assert_eq!(event.sensor.as_str(), "container-docker-cgroupfs");
        assert!(event.actor.is_none());
        assert!(matches!(
            event.kind,
            EventKind::SensorDegraded { ref sensor } if sensor.as_str() == "container-docker-cgroupfs"
        ));
        Ok(())
    }

    /// Ohne `<cgroup>/docker` (systemd-Treiber) keine Meldung; eine Datei
    /// dieses Namens zählt nicht.
    #[test]
    fn test_no_unsupported_layout_without_the_cgroupfs_directory() -> TestResult {
        let host = host()?;
        mark_cgroup_v2(&host)?;
        socket(&host, "docker.sock")?;
        assert!(
            plan(
                ContainerSensorMode::Auto,
                host.cgroup.path(),
                host.run.path()
            )
            .unsupported
            .is_empty()
        );
        fs::write(host.cgroup.path().join("docker"), b"").map_err(ctx("plain file"))?;
        assert!(
            plan(ContainerSensorMode::On, host.cgroup.path(), host.run.path())
                .unsupported
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_layout_identifiers_are_distinct_and_stable() {
        assert_eq!(ContainerLayout::Machine.sensor_id(), "container-machine");
        assert_eq!(ContainerLayout::System.sensor_id(), "container-system");
        assert_eq!(ContainerLayout::Machine.cgroup_parent(), "machine.slice");
        assert_eq!(ContainerLayout::System.cgroup_parent(), "system.slice");
        assert_ne!(
            ContainerLayout::Machine.label(),
            ContainerLayout::System.label()
        );
        assert!(ContainerLayout::ALL.iter().all(
            |layout| layout.sensor_id() != UnsupportedLayout::DockerCgroupfs.status_sensor_id()
        ));
    }
}

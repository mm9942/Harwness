//! Sensor-Registrierung: die Liste, die `main` an `Sentinel::new` übergibt.
//!
//! # Die tragende Abnahme dieses Knotens
//! **Jeder hier registrierte Sensor muss
//! [`harw_dod_cap::Capability::class`] `== `[`harw_dod_cap::CapabilityClass::Unprivileged`]
//! tragen.** `harw-sentinel` ist das erste vollständig unprivilegierte
//! Binary des Ausbauprogramms — es hält keine einzige erhöhte Fähigkeit,
//! weil jede Fähigkeit in dem Sensor sitzt, der sie bindet, und dieses
//! Modul bindet ausschließlich Sensoren, deren Fähigkeit unprivilegiert ist.
//! `test_all_registered_sensors_are_unprivileged` ist der Test, der das
//! erzwingt.
//!
//! # Registrierte Sensoren
//! Von den elf ursprünglich geplanten Strom-A-Sensor-Crates sind bei diesem
//! Knoten neun gebaut **und** unprivilegiert einsetzbar. Die ersten vier
//! landeten bereits vor diesem Umbau; die letzten fünf — `harw-dod-memory`,
//! `harw-dod-blockio`, `harw-dod-netcounters`, `harw-dod-gpu`,
//! `harw-dod-cgroup` — waren zwar gebaut, getestet und fixture-geprüft,
//! wurden aber nie hier eingetragen: `build_sensors` trug eine
//! auskommentierte Einfügestelle genau dafür. Das ist mit diesem Umbau
//! geschlossen.
//!
//! | Crate | Sensor | Fähigkeit | Lesewurzel | Konstruktion |
//! |---|---|---|---|---|
//! | `harw-dod-listener` | [`harw_dod_listener::ListenerSensor`] | `ReadProcNet` | `proc_root` | `ListenerSensor::new(id, proc_root)` |
//! | `harw-dod-cpu` | [`harw_dod_cpu::CpuSensor`] | `ReadProcStat` | `proc_root` | `SensorHandle::new(id, Capability::ReadProcStat).bind(scope).into()` |
//! | `harw-dod-thermal` | [`harw_dod_thermal::ThermalSensor`] | `ReadSysfsThermal` | `thermal_root` | `SensorHandle::new(id, Capability::ReadSysfsThermal).bind(scope).into()` |
//! | `harw-dod-workspace` | [`harw_dod_workspace::WorkspaceDriftSensor`] | `ReadWorkspaceGraph` | `workspace_root` | `WorkspaceDriftSensor::new(handle, workspace_root, baseline)` |
//! | `harw-dod-memory` | [`harw_dod_memory::MemorySensor`] | `ReadProcMeminfo` | `proc_root` | `SensorHandle::new(id, Capability::ReadProcMeminfo).bind(scope).into()` |
//! | `harw-dod-blockio` | [`harw_dod_blockio::BlockioSensor`] | `ReadSysfsBlock` | `blockio_root` | `SensorHandle::new(id, Capability::ReadSysfsBlock).bind(scope).into()` |
//! | `harw-dod-netcounters` | [`harw_dod_netcounters::NetCountersSensor`] | `ReadProcNetDev` | `proc_root` | `SensorHandle::new(id, Capability::ReadProcNetDev).bind(scope).into()` |
//! | `harw-dod-gpu` | [`harw_dod_gpu::GpuSensor`] | `ReadSysfsDrm` | `gpu_root` | `SensorHandle::new(id, Capability::ReadSysfsDrm).bind(scope).into()` |
//! | `harw-dod-cgroup` | [`harw_dod_cgroup::CgroupSensor`] | `ReadCgroupV2` | `cgroup_root` | `SensorHandle::new(id, Capability::ReadCgroupV2).bind(scope).into()` |
//!
//! `harw-dod-memory` und `harw-dod-netcounters` lesen — wie `CpuSensor` und
//! `ListenerSensor` — unter `proc_root`, denn [`harw_dod_cap::Capability::probe`]
//! nennt für beide Fähigkeiten einen Pfad unter `/proc`
//! (`/proc/meminfo`, `/proc/net/dev`). `harw-dod-blockio`,
//! `harw-dod-gpu` und `harw-dod-cgroup` haben dagegen jeweils ihre eigene
//! `/sys`-Wurzel (`/sys/block`, `/sys/class/drm`, `/sys/fs/cgroup`) — genau
//! diese Zuordnung nennt [`harw_dod_cap::Capability::probe`] für
//! `ReadSysfsBlock`, `ReadSysfsDrm` und `ReadCgroupV2`, und
//! `harw-dod-blockio` wie `harw-dod-cgroup` machen diese Quellenwahl in
//! ihrer jeweiligen Crate-Dokumentation ausdrücklich davon abhängig. Jede
//! der drei bekommt deshalb eine eigene Wurzel in [`SensorRoots`] statt
//! einer gemeinsamen `/sys`-Wurzel mit den anderen.
//!
//! Jeder Eintrag der Vec in [`build_sensors`] ist genau eine Zeile — ein
//! neuer unprivilegierter Sensor braucht keine zweite Änderung an dieser
//! Datei.
//!
//! # Warum `harw-dod-authlog` fehlt, obwohl es gebaut ist
//! `harw-dod-authlog` ist gelandet, taucht hier aber bewusst **nicht** auf.
//! Sein `Sensor`-Adapter, [`harw_dod_authlog::AuthlogSensor`], braucht bei
//! der Konstruktion zusätzlich ein `Box<dyn harw_dod_authlog::AuthBackend>`
//! — genau wie [`harw_dod_workspace::WorkspaceDriftSensor`] eine
//! `Option<Inventory>` braucht, lässt sich das nicht aus einem bloßen
//! `SensorHandle` ableiten, was diese Registrierung ohnehin trägt (siehe
//! Tabelle oben: der `WorkspaceDriftSensor`-Eintrag ist deshalb kein
//! `.into()`, sondern ein eigener Konstruktoraufruf). Das ist aber nicht der
//! Grund für die Auslassung.
//!
//! Der eigentliche Grund: die einzige *produktive* `AuthBackend`-Quelle ist
//! [`harw_dod_authlog::AuditBackend`], und die trägt
//! `Capability::ReadAuditNetlink` — Klasse
//! [`harw_dod_cap::CapabilityClass::Netlink`], **nicht** unprivilegiert
//! (`harw_dod_netlink::socket::NetlinkAuditSource::open` braucht
//! `CAP_AUDIT_READ`). Ein Journal-Backend, das stattdessen
//! `Capability::ReadJournal` (Klasse `Unprivileged`) trüge, existiert laut
//! eigener Moduldokumentation von `harw-dod-authlog` bewusst nicht (siehe
//! dortigen Abschnitt „Journal-Backend: warum es hier nicht existiert").
//! [`harw_dod_authlog::FixtureAuthBackend`] wäre zwar unprivilegiert
//! konstruierbar, liefert aber nie echte Hostdaten — sie hier als
//! Produktionssensor zu registrieren wäre keine Sammelstelle, sondern eine
//! Attrappe, die eine funktionierende meldet.
//!
//! Anmeldeereignisse gehören damit strukturell **nicht** in dieses Binary:
//! ein künftiger `harw-probe-netlink` (Klasse `Netlink`, analog zu
//! `harw-probe-fs`/`harw-probe-bpf`) müsste `AuthlogSensor` mit
//! `AuditBackend` im eigenen, privilegierten Prozess pollen und die
//! resultierenden `SecurityEvent`s über den in [`crate::ipc`] beschriebenen
//! Empfangspfad an diesen Sentinel weiterreichen — nicht `harw-dod-authlog`
//! in dieses Binary hineinlinken.
//!
//! Aus demselben strukturellen Grund fehlen hier auch `harw-dod-fsmon`
//! (`Capability::WatchFilesystem`, Klasse `FileWatch`) und
//! `harw-dod-procmon` (`Capability::LoadBpfProgram`, Klasse `Bpf`): beide
//! sind gelandet, aber keiner ihrer Fähigkeiten ist
//! `CapabilityClass::Unprivileged`. Sie gehören in `harw-probe-fs` bzw.
//! `harw-probe-bpf`, nicht in diesen Sentinel.
//!
//! # Ein weiterer unprivilegierter Sensor, der noch aussteht:
//! `harw-dod-scanreport`
//! `harw-dod-scanreport` ist gelandet und trägt
//! `Capability::ReadScanReports` — Klasse `Unprivileged`, also grundsätzlich
//! für dieses Binary geeignet. Er ist hier bewusst **nicht** mit
//! registriert: er konstruiert nicht über `From<SensorHandle<Bound>>` wie
//! die neun oben, sondern über `ScanReportSensor::new(id, scope)` mit einer
//! eigenen Lesewurzel (`harw_home::paths::scan_reports_dir`, ein
//! Root-Space-Pfad, kein Sensor-Fixture-Pfad wie `proc_root`/`thermal_root`)
//! — eine vierte Wurzelquelle neben `SensorRoots`, die dieser Umbau nicht
//! mit umfasst. Seine Aufnahme ist damit eine offene Erweiterung eines
//! künftigen Knotens, keine Auslassung aus Privilegiengründen.
//!
//! # Nebenläufigkeit
//! [`build_sensors`] liest nur die übergebenen Pfade und die Systemuhr nie
//! selbst; jeder zurückgegebene `Arc<dyn Sensor>` ist `Send + Sync`
//! (Vertrag von `harw_dod_signals::Sensor`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_dod_cap::{Capability, ReadScope, SensorHandle};
use harw_dod_signals::Sensor;
use harw_dod_workspace::Inventory;
use harw_types::SensorId;

/// Wurzelverzeichnisse, aus denen die unprivilegierten Dateisystem-Sensoren
/// dieses Binaries lesen.
///
/// # Description
/// Eine Zeile je Sensor-Quelle statt loser `PathBuf`-Parameter an
/// [`build_sensors`] — siehe [`crate::cli::Cli`] für die zugehörigen
/// `--proc-root`/`--thermal-root`/`--workspace-root`/`--blockio-root`/
/// `--gpu-root`/`--cgroup-root`-Flags.
#[derive(Debug, Clone)]
pub struct SensorRoots {
    /// Wurzel für [`harw_dod_listener::ListenerSensor`],
    /// [`harw_dod_cpu::CpuSensor`], [`harw_dod_memory::MemorySensor`] und
    /// [`harw_dod_netcounters::NetCountersSensor`]. In Produktion `/proc` —
    /// [`harw_dod_cap::Capability::probe`] nennt für `ReadProcMeminfo` und
    /// `ReadProcNetDev` je einen Pfad unter `/proc`.
    pub proc_root: PathBuf,
    /// Wurzel für [`harw_dod_thermal::ThermalSensor`]. In Produktion
    /// `/sys/class/thermal`.
    pub thermal_root: PathBuf,
    /// Wurzel für [`harw_dod_workspace::WorkspaceDriftSensor`]: das
    /// Cargo-Workspace-Verzeichnis, dessen Struktur beobachtet wird.
    pub workspace_root: PathBuf,
    /// Wurzel für [`harw_dod_blockio::BlockioSensor`]. In Produktion
    /// `/sys/block` — die Quelle, die
    /// [`harw_dod_cap::Capability::ReadSysfsBlock`]`::probe()` nennt.
    pub blockio_root: PathBuf,
    /// Wurzel für [`harw_dod_gpu::GpuSensor`]. In Produktion
    /// `/sys/class/drm` — die Quelle, die
    /// [`harw_dod_cap::Capability::ReadSysfsDrm`]`::probe()` nennt.
    pub gpu_root: PathBuf,
    /// Wurzel für [`harw_dod_cgroup::CgroupSensor`]. In Produktion
    /// `/sys/fs/cgroup` — die Quelle, die
    /// [`harw_dod_cap::Capability::ReadCgroupV2`]`::probe()` nennt.
    pub cgroup_root: PathBuf,
    /// Der Root-Space für [`harw_dod_scanreport::ScanReportSensor`].
    ///
    /// Anders als die übrigen ist dies **keine** Sensorwurzel, sondern das
    /// HARW-Home: der Sensor löst daraus selbst
    /// [`harw_home::paths::scan_reports_dir`] auf. Ein noch nicht
    /// existierendes Berichtsverzeichnis ergibt eine leere Messung, keinen
    /// Fehler -- Fremdscanner laufen nicht auf jedem Host.
    pub home: PathBuf,
}

/// Versucht, eine Startaufnahme des Workspace-Inventars als
/// [`harw_dod_workspace::WorkspaceDriftSensor`]-Baseline zu bauen.
///
/// # Description
/// `WorkspaceDriftSensor` hält seine `baseline` unveränderlich ab der
/// Konstruktion (siehe dortige Moduldoku, Abschnitt „Warum der Sensor kein
/// eigenes Gedächtnis hat") — ohne diese Funktion wäre `baseline` für die
/// gesamte Prozesslaufzeit `None`, und der Sensor meldete nie eine Drift,
/// egal wie lange der Prozess läuft. Ein bei `main`-Start gelesenes
/// Inventar macht den Sensor stattdessen empfindlich für jede Abweichung,
/// die **während der Laufzeit dieses Prozesses** auftritt.
///
/// Es gibt noch keine Persistenz über einen Prozessneustart hinweg (kein
/// `harw_home::paths`-Eintrag für ein gespeichertes Inventar) — das wäre
/// eine sinnvolle Erweiterung eines künftigen Knotens, kein Rückschritt
/// dieses hier: `Inventory` ist bereits serde-serialisierbar
/// ([`harw_dod_workspace::Inventory`]-Dokumentation).
///
/// # Arguments
/// - `scope` (`&harw_dod_cap::ReadScope`): der an den Sensor gebundene
///   Lesebereich, unverändert an [`Inventory::read`] weitergereicht.
/// - `root` (`&std::path::Path`): das Workspace-Wurzelverzeichnis.
///
/// # Returns
/// `Some(inventory)` bei erfolgreicher Erstaufnahme, sonst `None` — ein
/// fehlendes Inventar degradiert den Sensor auf „meldet in diesem Lauf keine
/// Drift", startet den restlichen Prozess aber nicht neu und bricht ihn
/// nicht ab.
fn initial_workspace_baseline(scope: &ReadScope, root: &Path) -> Option<Inventory> {
    match Inventory::read(scope, root) {
        Ok(inventory) => Some(inventory),
        Err(error) => {
            tracing::warn!(
                error = %error,
                root = %root.display(),
                "workspace baseline snapshot failed; WorkspaceDriftSensor reports no drift this run"
            );
            None
        }
    }
}

/// Baut die vollständige Liste unprivilegierter Sensoren für diesen Prozess.
///
/// # Description
/// Siehe Moduldoku für die Tabelle der neun registrierten Sensoren und die
/// Begründung, warum `harw-dod-authlog`, `harw-dod-fsmon`,
/// `harw-dod-procmon` und (vorerst) `harw-dod-scanreport` hier nicht
/// erscheinen. Jeder Eintrag baut seinen eigenen `SensorHandle<Bound>`;
/// kein Sensor teilt einen Griff mit einem anderen.
///
/// # Arguments
/// - `roots` (`&SensorRoots`): die sechs Lesewurzeln dieses Prozesses.
///
/// # Returns
/// Ein `Vec<Arc<dyn Sensor>>`, bereit für `Sentinel::new`. Nie leer, solange
/// mindestens ein Sensorpfad existiert — [`Sentinel::poll_all`] behandelt
/// einen nicht vorhandenen Quellpfad ohnehin als
/// `SensorError::SourceUnavailable` je Sensor, nicht als Konstruktionsfehler
/// dieser Funktion.
///
/// # Examples
/// ```rust,no_run
/// use harw_sentinel::sensors::{build_sensors, SensorRoots};
/// use std::path::PathBuf;
///
/// let roots = SensorRoots {
///     proc_root: PathBuf::from("/proc"),
///     thermal_root: PathBuf::from("/sys/class/thermal"),
///     workspace_root: PathBuf::from("."),
///     blockio_root: PathBuf::from("/sys/block"),
///     gpu_root: PathBuf::from("/sys/class/drm"),
///     cgroup_root: PathBuf::from("/sys/fs/cgroup"),
///     home: PathBuf::from("/tmp/harw-home"),
/// };
/// let sensors = build_sensors(&roots);
/// assert_eq!(sensors.len(), 10);
/// ```
#[must_use]
pub fn build_sensors(roots: &SensorRoots) -> Vec<Arc<dyn Sensor>> {
    let listener: Arc<dyn Sensor> = Arc::new(harw_dod_listener::ListenerSensor::new(
        SensorId::from_str("listener-0"),
        roots.proc_root.clone(),
    ));

    let cpu_scope = ReadScope::from_roots([roots.proc_root.clone()]);
    let cpu_handle =
        SensorHandle::new(SensorId::from_str("cpu-0"), Capability::ReadProcStat).bind(cpu_scope);
    let cpu: Arc<dyn Sensor> = Arc::new(harw_dod_cpu::CpuSensor::from(cpu_handle));

    let thermal_scope = ReadScope::from_roots([roots.thermal_root.clone()]);
    let thermal_handle = SensorHandle::new(
        SensorId::from_str("thermal-0"),
        Capability::ReadSysfsThermal,
    )
    .bind(thermal_scope);
    let thermal: Arc<dyn Sensor> = Arc::new(harw_dod_thermal::ThermalSensor::from(thermal_handle));

    let workspace_scope = ReadScope::from_roots([roots.workspace_root.clone()]);
    let workspace_baseline = initial_workspace_baseline(&workspace_scope, &roots.workspace_root);
    let workspace_handle = SensorHandle::new(
        SensorId::from_str("workspace-drift-0"),
        Capability::ReadWorkspaceGraph,
    )
    .bind(workspace_scope);
    let workspace: Arc<dyn Sensor> = Arc::new(harw_dod_workspace::WorkspaceDriftSensor::new(
        workspace_handle,
        roots.workspace_root.clone(),
        workspace_baseline,
    ));

    let memory_scope = ReadScope::from_roots([roots.proc_root.clone()]);
    let memory_handle =
        SensorHandle::new(SensorId::from_str("memory-0"), Capability::ReadProcMeminfo)
            .bind(memory_scope);
    let memory: Arc<dyn Sensor> = Arc::new(harw_dod_memory::MemorySensor::from(memory_handle));

    let blockio_scope = ReadScope::from_roots([roots.blockio_root.clone()]);
    let blockio_handle =
        SensorHandle::new(SensorId::from_str("blockio-0"), Capability::ReadSysfsBlock)
            .bind(blockio_scope);
    let blockio: Arc<dyn Sensor> = Arc::new(harw_dod_blockio::BlockioSensor::from(blockio_handle));

    let netcounters_scope = ReadScope::from_roots([roots.proc_root.clone()]);
    let netcounters_handle = SensorHandle::new(
        SensorId::from_str("netcounters-0"),
        Capability::ReadProcNetDev,
    )
    .bind(netcounters_scope);
    let netcounters: Arc<dyn Sensor> = Arc::new(harw_dod_netcounters::NetCountersSensor::from(
        netcounters_handle,
    ));

    let gpu_scope = ReadScope::from_roots([roots.gpu_root.clone()]);
    let gpu_handle =
        SensorHandle::new(SensorId::from_str("gpu-0"), Capability::ReadSysfsDrm).bind(gpu_scope);
    let gpu: Arc<dyn Sensor> = Arc::new(harw_dod_gpu::GpuSensor::from(gpu_handle));

    let cgroup_scope = ReadScope::from_roots([roots.cgroup_root.clone()]);
    let cgroup_handle = SensorHandle::new(SensorId::from_str("cgroup-0"), Capability::ReadCgroupV2)
        .bind(cgroup_scope);
    let cgroup: Arc<dyn Sensor> = Arc::new(harw_dod_cgroup::CgroupSensor::from(cgroup_handle));

    // `ScanReportSensor` wird nicht über `From<SensorHandle<Bound>>` gebaut,
    // sondern löst seine Wurzel selbst aus dem HARW-Home auf -- deshalb der
    // eigene Konstruktor statt des Musters der übrigen sechs.
    let scanreport: Arc<dyn Sensor> = Arc::new(harw_dod_scanreport::ScanReportSensor::for_home(
        SensorId::from_str("scanreport-0"),
        &roots.home,
    ));

    vec![
        listener,
        scanreport,
        cpu,
        thermal,
        workspace,
        memory,
        blockio,
        netcounters,
        gpu,
        cgroup,
    ]
}

/// Meldet, ob jeder übergebene Sensor [`harw_dod_cap::CapabilityClass::Unprivileged`]
/// trägt.
///
/// # Description
/// Reine Prüffunktion ohne Seiteneffekt — Grundlage von
/// `test_all_registered_sensors_are_unprivileged` und, falls ein
/// künftiger Aufrufer das will, einer Laufzeitprüfung direkt nach
/// [`build_sensors`].
///
/// # Arguments
/// - `sensors` (`&[Arc<dyn Sensor>]`): die zu prüfende Sensorliste.
///
/// # Returns
/// `true`, wenn jeder Sensor `handle().capability().class() ==
/// CapabilityClass::Unprivileged` erfüllt; `false`, sobald einer davon
/// abweicht.
#[must_use]
pub fn all_unprivileged(sensors: &[Arc<dyn Sensor>]) -> bool {
    sensors.iter().all(|sensor| {
        sensor.handle().capability().class() == harw_dod_cap::CapabilityClass::Unprivileged
    })
}

#[cfg(test)]
mod tests {
    use super::{SensorRoots, all_unprivileged, build_sensors};
    use crate::test_support::{TestError, TestResult};
    use harw_dod_cap::{Capability, CapabilityClass};
    use std::path::PathBuf;

    fn roots() -> SensorRoots {
        SensorRoots {
            proc_root: PathBuf::from("/proc"),
            thermal_root: PathBuf::from("/sys/class/thermal"),
            workspace_root: PathBuf::from("."),
            blockio_root: PathBuf::from("/sys/block"),
            gpu_root: PathBuf::from("/sys/class/drm"),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            home: PathBuf::from("/sys/fs/cgroup"),
        }
    }

    #[test]
    fn test_build_sensors_registers_every_landed_unprivileged_sensor() {
        let sensors = build_sensors(&roots());
        // Zehn: listener, cpu, thermal, workspace, memory, blockio,
        // netcounters, gpu, cgroup, scanreport. Die Zahl steht hier
        // ausdrücklich und nicht als Untergrenze -- ein Sensor, der
        // stillschweigend herausfällt, ist genau der Befund, den die
        // Bottom-up-Analyse an dieser Stelle gefunden hat.
        assert_eq!(sensors.len(), 10);
    }

    #[test]
    fn test_all_registered_sensors_are_unprivileged() {
        let sensors = build_sensors(&roots());
        for sensor in &sensors {
            assert_eq!(
                sensor.handle().capability().class(),
                CapabilityClass::Unprivileged,
                "sensor {:?} is not unprivileged",
                sensor.handle().id()
            );
        }
        assert!(all_unprivileged(&sensors));
    }

    #[test]
    fn test_registered_sensor_ids_are_unique() {
        let sensors = build_sensors(&roots());
        let mut ids: Vec<&str> = sensors.iter().map(|s| s.handle().id().as_str()).collect();
        ids.sort_unstable();
        let mut deduped = ids.clone();
        deduped.dedup();
        assert_eq!(ids, deduped, "sensor ids must be unique: {ids:?}");
    }

    #[test]
    fn test_initial_workspace_baseline_none_when_root_is_unreadable() {
        let scope = harw_dod_cap::ReadScope::from_roots([PathBuf::from(
            "/nonexistent-harw-sentinel-test-root",
        )]);
        let baseline = super::initial_workspace_baseline(
            &scope,
            std::path::Path::new("/nonexistent-harw-sentinel-test-root"),
        );
        assert!(baseline.is_none());
    }

    #[test]
    fn test_all_unprivileged_on_empty_list_is_true() {
        let sensors: Vec<std::sync::Arc<dyn harw_dod_signals::Sensor>> = Vec::new();
        assert!(all_unprivileged(&sensors));
    }

    #[test]
    fn test_memory_sensor_appears_with_expected_id_and_capability() -> TestResult {
        let sensors = build_sensors(&roots());
        let memory = sensors
            .iter()
            .find(|s| s.handle().id().as_str() == "memory-0")
            .ok_or(TestError::Missing("memory sensor is registered"))?;
        assert_eq!(memory.handle().capability(), Capability::ReadProcMeminfo);
        Ok(())
    }

    #[test]
    fn test_blockio_sensor_appears_with_expected_id_and_capability() -> TestResult {
        let sensors = build_sensors(&roots());
        let blockio = sensors
            .iter()
            .find(|s| s.handle().id().as_str() == "blockio-0")
            .ok_or(TestError::Missing("blockio sensor is registered"))?;
        assert_eq!(blockio.handle().capability(), Capability::ReadSysfsBlock);
        Ok(())
    }

    #[test]
    fn test_netcounters_sensor_appears_with_expected_id_and_capability() -> TestResult {
        let sensors = build_sensors(&roots());
        let netcounters = sensors
            .iter()
            .find(|s| s.handle().id().as_str() == "netcounters-0")
            .ok_or(TestError::Missing("netcounters sensor is registered"))?;
        assert_eq!(
            netcounters.handle().capability(),
            Capability::ReadProcNetDev
        );
        Ok(())
    }

    #[test]
    fn test_gpu_sensor_appears_with_expected_id_and_capability() -> TestResult {
        let sensors = build_sensors(&roots());
        let gpu = sensors
            .iter()
            .find(|s| s.handle().id().as_str() == "gpu-0")
            .ok_or(TestError::Missing("gpu sensor is registered"))?;
        assert_eq!(gpu.handle().capability(), Capability::ReadSysfsDrm);
        Ok(())
    }

    #[test]
    fn test_cgroup_sensor_appears_with_expected_id_and_capability() -> TestResult {
        let sensors = build_sensors(&roots());
        let cgroup = sensors
            .iter()
            .find(|s| s.handle().id().as_str() == "cgroup-0")
            .ok_or(TestError::Missing("cgroup sensor is registered"))?;
        assert_eq!(cgroup.handle().capability(), Capability::ReadCgroupV2);
        Ok(())
    }

    #[test]
    fn test_build_sensors_survives_nonexistent_roots_without_panicking() {
        let roots = SensorRoots {
            proc_root: PathBuf::from("/nonexistent-harw-sentinel-proc"),
            thermal_root: PathBuf::from("/nonexistent-harw-sentinel-thermal"),
            workspace_root: PathBuf::from("/nonexistent-harw-sentinel-workspace"),
            blockio_root: PathBuf::from("/nonexistent-harw-sentinel-blockio"),
            gpu_root: PathBuf::from("/nonexistent-harw-sentinel-gpu"),
            cgroup_root: PathBuf::from("/nonexistent-harw-sentinel-cgroup"),
            home: PathBuf::from("/nonexistent-harw-sentinel-cgroup"),
        };
        let sensors = build_sensors(&roots);
        assert_eq!(sensors.len(), 10);
    }
}

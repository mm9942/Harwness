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
//! | `harw-dod-workspace` | [`harw_dod_workspace::WorkspaceDriftSensor`] | `ReadWorkspaceGraph` | `workspace_root` (nur mit `--workspace-root`) | `WorkspaceDriftSensor::new(handle, workspace_root, baseline)` |
//! | `harw-dod-memory` | [`harw_dod_memory::MemorySensor`] | `ReadProcMeminfo` | `proc_root` | `SensorHandle::new(id, Capability::ReadProcMeminfo).bind(scope).into()` |
//! | `harw-dod-blockio` | [`harw_dod_blockio::BlockioSensor`] | `ReadSysfsBlock` | `blockio_root` | `SensorHandle::new(id, Capability::ReadSysfsBlock).bind(scope).into()` |
//! | `harw-dod-netcounters` | [`harw_dod_netcounters::NetCountersSensor`] | `ReadProcNetDev` | `proc_root` | `SensorHandle::new(id, Capability::ReadProcNetDev).bind(scope).into()` |
//! | `harw-dod-gpu` | [`harw_dod_gpu::GpuSensor`] | `ReadSysfsDrm` | `gpu_root` | `SensorHandle::new(id, Capability::ReadSysfsDrm).bind(scope).into()` |
//! | `harw-dod-cgroup` | [`harw_dod_cgroup::CgroupSensor`] | `ReadCgroupV2` | `cgroup_root` | `SensorHandle::new(id, Capability::ReadCgroupV2).bind(scope).into()` |
//! | `harw-dod-container` | [`harw_dod_container::ContainerSensor`] (je geplanter Anordnung, PL-100 WP1.8) | `ReadContainerScopes` | `cgroup_root/<slice>` und `proc_root` | `ContainerSensor::new(handle, config)` |
//!
//! # sysfs-Klassenwurzeln über Alias-Wurzeln (PL-100 WP1.10)
//! `thermal_root`, `blockio_root` und `gpu_root` sind sysfs-Klassen-
//! verzeichnisse (`/sys/class/thermal`, `/sys/block`, `/sys/class/drm`),
//! deren Einträge auf jedem Linux Symlinks nach `/sys/devices/…` sind
//! (Befund F-005). Ein einfacher `ReadScope` auf der Klassenwurzel lehnte
//! jedes kanonische Ziel als `OutsideScope` ab — die drei Sensoren lasen auf
//! einem echten Host nichts. [`sysfs_class_scope`] baut deshalb für jede
//! Wurzel unter `/sys` eine `harw_dod_cap::scope::AliasRoot` (Zielpräfix
//! `/sys/devices`), und [`sysfs_alias_prefixes`] liefert diese Präfixe als
//! zusätzliche Landlock-Lesewurzeln (`crate::sandbox`), damit die
//! aufgelösten Lesezugriffe innerhalb der Sandbox bleiben. Eine Wurzel
//! außerhalb von `/sys` (Fixture-Verzeichnis im Test) bleibt eine einfache
//! Wurzel ohne Landlock-Zusatz.
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
//! Der Workspace-Sensor ist der einzige bedingte Eintrag: er wird nur
//! gebaut, wenn `--workspace-root` ausdrücklich angegeben ist
//! ([`SensorRoots::workspace_root`] ist dann `Some`). Ohne das Flag gibt es
//! weder den Sensor noch seine Landlock-Leseregel (PL-100 WP0.7: früher
//! stand die Wurzel auf `.` und weitete die Sandbox über das
//! Arbeitsverzeichnis auf `/`).
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

use harw_dod_cap::scope::AliasRoot;
use harw_dod_cap::{Capability, ReadScope, SensorHandle};
use harw_dod_container::{ContainerSensor, ContainerSensorConfig};
use harw_dod_signals::Sensor;
use harw_dod_workspace::Inventory;
use harw_types::SensorId;

use crate::container_plan::PlannedLayout;

/// Wurzel, unterhalb derer eine Sensorwurzel als sysfs-Klassenverzeichnis
/// gilt (siehe Moduldoku, „sysfs-Klassenwurzeln über Alias-Wurzeln").
const SYSFS_ROOT: &str = "/sys";

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
    /// Cargo-Workspace-Verzeichnis, dessen Struktur beobachtet wird. `None`
    /// (kein `--workspace-root`) registriert keinen Workspace-Sensor.
    pub workspace_root: Option<PathBuf>,
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
    /// Die Container-Anordnungen, für die je ein
    /// [`harw_dod_container::ContainerSensor`] unter `cgroup_root` läuft,
    /// samt Bedeutung ihres fehlenden cgroup-Verzeichnisses (PL-100 WP1.8;
    /// [`crate::container_plan::plan`]). Leer: kein Container-Sensor.
    pub containers: Vec<PlannedLayout>,
}

/// Der Lesebereich einer sysfs-Klassenwurzel (thermal, blockio, gpu).
///
/// # Description
/// Siehe Moduldoku, „sysfs-Klassenwurzeln über Alias-Wurzeln": unter `/sys`
/// eine Alias-Wurzel mit Zielpräfix `/sys/devices`, sonst (oder wenn die
/// Alias-Wurzel abgelehnt wird, etwa bei `/sys` selbst mit `..`) eine
/// einfache Wurzel.
///
/// # Arguments
/// - `root` (`&Path`): die Klassenwurzel.
///
/// # Returns
/// Den `ReadScope` für den Sensorgriff.
#[must_use]
pub fn sysfs_class_scope(root: &Path) -> ReadScope {
    match sysfs_alias(root) {
        Some(alias) => ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [alias]),
        None => ReadScope::from_roots([root.to_path_buf()]),
    }
}

/// Die Zielpräfixe der Alias-Wurzeln der übergebenen Klassenwurzeln, ohne
/// Duplikate — die zusätzlichen Landlock-Lesewurzeln (siehe Moduldoku).
///
/// # Arguments
/// - `class_roots` (`&[&Path]`): `thermal_root`, `blockio_root`, `gpu_root`.
///
/// # Returns
/// In Produktion genau `[/sys/devices]`; leer, wenn keine Wurzel unter
/// `/sys` liegt.
#[must_use]
pub fn sysfs_alias_prefixes(class_roots: &[&Path]) -> Vec<PathBuf> {
    let mut prefixes: Vec<PathBuf> = Vec::new();
    for alias in class_roots.iter().filter_map(|root| sysfs_alias(root)) {
        let prefix = alias.resolved_prefix().to_path_buf();
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    prefixes
}

// Die Alias-Wurzel einer Klassenwurzel unter `/sys` (`None` außerhalb oder
// wenn `AliasRoot` sie ablehnt).
fn sysfs_alias(root: &Path) -> Option<AliasRoot> {
    if !root.starts_with(SYSFS_ROOT) {
        return None;
    }
    AliasRoot::sysfs_class(root.to_path_buf()).ok()
}

/// Baut den Container-Sensor einer Anordnung (PL-100 WP1.8).
///
/// # Description
/// Lesebereich: das cgroup-Verzeichnis der Anordnung unter `cgroup_root`
/// und `proc_root` (beide schon Landlock-Lesewurzeln). Die Menge erwarteter
/// Container ist leer (siehe [`crate::container_plan`], „Erwartete
/// Container"): jeder Scope erscheint nur als `container_unowned_<id>`. Ein
/// fehlendes cgroup-Verzeichnis bedeutet, was der Plan festgelegt hat
/// ([`PlannedLayout::missing_slice`]; siehe [`crate::container_plan`],
/// „Fehlendes cgroup-Verzeichnis").
fn container_sensor(
    planned: PlannedLayout,
    cgroup_root: &Path,
    proc_root: &Path,
) -> Arc<dyn Sensor> {
    let layout = planned.layout;
    let scope_dir = cgroup_root.join(layout.cgroup_parent());
    let scope = ReadScope::from_roots([scope_dir.clone(), proc_root.to_path_buf()]);
    let handle = ContainerSensor::handle_for(SensorId::from_str(layout.sensor_id()), scope);
    Arc::new(ContainerSensor::new(
        handle,
        ContainerSensorConfig {
            cgroup_root: scope_dir,
            proc_root: proc_root.to_path_buf(),
            expected: Vec::new(),
            missing_root: planned.missing_slice,
        },
    ))
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

/// Baut den [`harw_dod_workspace::WorkspaceDriftSensor`] für `root`, samt
/// Startaufnahme als Baseline ([`initial_workspace_baseline`]).
fn workspace_sensor(root: &Path) -> Arc<dyn Sensor> {
    let scope = ReadScope::from_roots([root.to_path_buf()]);
    let baseline = initial_workspace_baseline(&scope, root);
    let handle = SensorHandle::new(
        SensorId::from_str("workspace-drift-0"),
        Capability::ReadWorkspaceGraph,
    )
    .bind(scope);
    Arc::new(harw_dod_workspace::WorkspaceDriftSensor::new(
        handle,
        root.to_path_buf(),
        baseline,
    ))
}

/// Baut die vollständige Liste unprivilegierter Sensoren für diesen Prozess.
///
/// # Description
/// Siehe Moduldoku für die Tabelle der registrierten Sensoren und die
/// Begründung, warum `harw-dod-authlog`, `harw-dod-fsmon`,
/// `harw-dod-procmon` und (vorerst) `harw-dod-scanreport` hier nicht
/// erscheinen. Jeder Eintrag baut seinen eigenen `SensorHandle<Bound>`;
/// kein Sensor teilt einen Griff mit einem anderen.
///
/// # Arguments
/// - `roots` (`&SensorRoots`): die Lesewurzeln und Container-Anordnungen
///   dieses Prozesses.
///
/// # Returns
/// Ein `Vec<Arc<dyn Sensor>>`, bereit für `Sentinel::new`: zehn Sensoren mit
/// `workspace_root`, neun ohne, dazu je Container-Anordnung einer. Nie leer, solange
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
///     workspace_root: None,
///     blockio_root: PathBuf::from("/sys/block"),
///     gpu_root: PathBuf::from("/sys/class/drm"),
///     cgroup_root: PathBuf::from("/sys/fs/cgroup"),
///     home: PathBuf::from("/tmp/harw-home"),
///     containers: Vec::new(),
/// };
/// let sensors = build_sensors(&roots);
/// assert_eq!(sensors.len(), 9);
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

    let thermal_scope = sysfs_class_scope(&roots.thermal_root);
    let thermal_handle = SensorHandle::new(
        SensorId::from_str("thermal-0"),
        Capability::ReadSysfsThermal,
    )
    .bind(thermal_scope);
    let thermal: Arc<dyn Sensor> = Arc::new(harw_dod_thermal::ThermalSensor::from(thermal_handle));

    let workspace = roots.workspace_root.as_deref().map(workspace_sensor);

    let memory_scope = ReadScope::from_roots([roots.proc_root.clone()]);
    let memory_handle =
        SensorHandle::new(SensorId::from_str("memory-0"), Capability::ReadProcMeminfo)
            .bind(memory_scope);
    let memory: Arc<dyn Sensor> = Arc::new(harw_dod_memory::MemorySensor::from(memory_handle));

    let blockio_scope = sysfs_class_scope(&roots.blockio_root);
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

    let gpu_scope = sysfs_class_scope(&roots.gpu_root);
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

    let mut sensors = vec![listener, scanreport, cpu, thermal];
    sensors.extend(workspace);
    sensors.extend([memory, blockio, netcounters, gpu, cgroup]);
    sensors.extend(
        roots
            .containers
            .iter()
            .map(|planned| container_sensor(*planned, &roots.cgroup_root, &roots.proc_root)),
    );
    sensors
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
    use super::{
        SensorRoots, all_unprivileged, build_sensors, sysfs_alias_prefixes, sysfs_class_scope,
    };
    use crate::container_plan::{ContainerLayout, PlannedLayout};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_cap::{Capability, CapabilityClass, ReadScope, SensorHandle};
    use harw_dod_container::MissingRoot;
    use harw_types::SensorId;
    use std::path::{Path, PathBuf};

    fn roots() -> SensorRoots {
        SensorRoots {
            proc_root: PathBuf::from("/proc"),
            thermal_root: PathBuf::from("/sys/class/thermal"),
            workspace_root: Some(PathBuf::from("/nonexistent-harw-sentinel-workspace")),
            blockio_root: PathBuf::from("/sys/block"),
            gpu_root: PathBuf::from("/sys/class/drm"),
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            home: PathBuf::from("/sys/fs/cgroup"),
            containers: Vec::new(),
        }
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

    /// PL-100 WP0.7: ohne `--workspace-root` kein Workspace-Sensor, die
    /// übrigen neun bleiben.
    #[test]
    fn test_build_sensors_without_workspace_root_skips_only_the_workspace_sensor() {
        let roots = SensorRoots {
            workspace_root: None,
            ..roots()
        };
        let sensors = build_sensors(&roots);
        assert_eq!(sensors.len(), 9);
        assert!(
            sensors
                .iter()
                .all(|sensor| sensor.handle().id().as_str() != "workspace-drift-0")
        );
        assert!(all_unprivileged(&sensors));
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
            workspace_root: Some(PathBuf::from("/nonexistent-harw-sentinel-workspace")),
            blockio_root: PathBuf::from("/nonexistent-harw-sentinel-blockio"),
            gpu_root: PathBuf::from("/nonexistent-harw-sentinel-gpu"),
            cgroup_root: PathBuf::from("/nonexistent-harw-sentinel-cgroup"),
            home: PathBuf::from("/nonexistent-harw-sentinel-cgroup"),
            containers: every_layout(MissingRoot::Unavailable),
        };
        let sensors = build_sensors(&roots);
        assert_eq!(sensors.len(), 12);
    }

    /// PL-100 WP1.8: je Container-Anordnung genau ein unprivilegierter
    /// Container-Sensor mit eigener Kennung.
    #[test]
    fn test_build_sensors_registers_one_container_sensor_per_layout() -> TestResult {
        let roots = SensorRoots {
            containers: every_layout(MissingRoot::Empty),
            ..roots()
        };
        let sensors = build_sensors(&roots);
        assert_eq!(sensors.len(), 12);
        assert!(all_unprivileged(&sensors));
        for layout in ContainerLayout::ALL {
            let sensor = sensors
                .iter()
                .find(|s| s.handle().id().as_str() == layout.sensor_id())
                .ok_or(TestError::Missing("container sensor is registered"))?;
            assert_eq!(
                sensor.handle().capability(),
                Capability::ReadContainerScopes
            );
            let dir = PathBuf::from("/sys/fs/cgroup").join(layout.cgroup_parent());
            assert!(sensor.handle().scope().allows(&dir.join("libpod-x.scope")));
            assert!(sensor.handle().scope().allows(Path::new("/proc/1/status")));
            assert!(
                !sensor
                    .handle()
                    .scope()
                    .allows(Path::new("/sys/fs/cgroup/user.slice"))
            );
        }
        Ok(())
    }

    /// PL-100 WP1.8: ohne Anordnung kein Container-Sensor.
    #[test]
    fn test_build_sensors_without_container_layouts_has_no_container_sensor() {
        let sensors = build_sensors(&roots());
        assert!(
            sensors
                .iter()
                .all(|sensor| sensor.handle().capability() != Capability::ReadContainerScopes)
        );
    }

    /// PL-100 WP1.10: die sysfs-Klassenwurzeln bekommen eine Alias-Wurzel
    /// mit Zielpräfix `/sys/devices`, eine Fixture-Wurzel nicht.
    #[test]
    fn test_sysfs_class_roots_use_alias_scopes_below_sys_only() -> TestResult {
        let scope = sysfs_class_scope(Path::new("/sys/class/thermal"));
        let alias = scope
            .alias_roots()
            .next()
            .ok_or(TestError::Missing("an alias root"))?;
        assert_eq!(alias.declared(), Path::new("/sys/class/thermal"));
        assert_eq!(alias.resolved_prefix(), Path::new("/sys/devices"));
        assert!(scope.allows(Path::new("/sys/class/thermal/thermal_zone0/temp")));
        assert!(!scope.allows(Path::new("/sys/devices/virtual/thermal/thermal_zone0/temp")));

        let fixture = sysfs_class_scope(Path::new("/srv/fixture/thermal"));
        assert_eq!(fixture.alias_roots().count(), 0);
        assert!(fixture.allows(Path::new("/srv/fixture/thermal/zone0/temp")));
        Ok(())
    }

    /// PL-100 WP1.10: die zusätzlichen Landlock-Lesewurzeln sind genau die
    /// Zielpräfixe, ohne Duplikate.
    #[test]
    fn test_sysfs_alias_prefixes_are_the_distinct_resolved_prefixes() {
        assert_eq!(
            sysfs_alias_prefixes(&[
                Path::new("/sys/class/thermal"),
                Path::new("/sys/block"),
                Path::new("/sys/class/drm"),
            ]),
            vec![PathBuf::from("/sys/devices")]
        );
        assert!(sysfs_alias_prefixes(&[Path::new("/srv/a"), Path::new("/sysfoo/b")]).is_empty());
    }

    /// PL-100 WP1.10, Regression F-005 über den Registrierungsweg: ein
    /// Klasseneintrag als Symlink in das Zielpräfix wird gelesen. Der Test
    /// baut die Alias-Wurzel mit Tempdir-Pfaden nach (`/sys` selbst ist im
    /// Test nicht beschreibbar) und liest über den Griff des gebauten
    /// Thermalsensors.
    #[cfg(unix)]
    #[test]
    fn test_alias_scope_reads_a_class_entry_symlinked_into_the_devices_tree() -> TestResult {
        use harw_dod_signals::Sensor as _;

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let base = std::fs::canonicalize(dir.path()).map_err(ctx("canonical tempdir"))?;
        let devices = base.join("devices");
        let class = base.join("class").join("thermal");
        let zone = devices
            .join("virtual")
            .join("thermal")
            .join("thermal_zone0");
        std::fs::create_dir_all(&zone).map_err(ctx("zone dir"))?;
        std::fs::create_dir_all(&class).map_err(ctx("class dir"))?;
        std::fs::write(zone.join("temp"), "42000\n").map_err(ctx("temp"))?;
        std::fs::write(zone.join("type"), "cpu\n").map_err(ctx("type"))?;
        std::os::unix::fs::symlink(
            "../../devices/virtual/thermal/thermal_zone0",
            class.join("thermal_zone0"),
        )
        .map_err(ctx("class symlink"))?;

        let plain = harw_dod_thermal::ThermalSensor::from(
            SensorHandle::new(
                SensorId::from_str("thermal-plain"),
                Capability::ReadSysfsThermal,
            )
            .bind(ReadScope::from_roots([class.clone()])),
        );
        let alias = harw_dod_cap::scope::AliasRoot::new(class.clone(), devices.clone())
            .map_err(ctx("alias root"))?;
        let aliased = harw_dod_thermal::ThermalSensor::from(
            SensorHandle::new(
                SensorId::from_str("thermal-alias"),
                Capability::ReadSysfsThermal,
            )
            .bind(ReadScope::from_roots_and_aliases(
                Vec::<PathBuf>::new(),
                [alias],
            )),
        );

        let plain_samples = plain
            .poll(jiff::Timestamp::UNIX_EPOCH)
            .map(|reading| reading.samples.len())
            .unwrap_or(0);
        assert_eq!(
            plain_samples, 0,
            "a plain class root rejects the symlink target"
        );
        let reading = aliased
            .poll(jiff::Timestamp::UNIX_EPOCH)
            .map_err(ctx("alias scope reads the zone"))?;
        assert_eq!(reading.samples.len(), 1);
        Ok(())
    }
}

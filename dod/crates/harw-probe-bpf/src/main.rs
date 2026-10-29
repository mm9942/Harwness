//! eBPF-Sonde, Push-Only — Knoten **AW7-01d**.
//!
//! # Zweck
//! Dieses Binary ist die eine privilegierte eBPF-Sonde des
//! Ausbauprogramms: es lädt die beiden geerbten eBPF-Programme
//! (Prozessstart über `harw-dod-procmon`, Verbindungszustand über
//! `harw-dod-flow`) über einen injizierten `harw_dod_bpf::BpfLoader`, formt
//! ihre Rohereignisse zu `harw_dod_signals::SecurityEvent`s und sendet sie
//! an `harw-sentinel`. Es hat **keinen** Empfangspfad — siehe Abschnitt
//! „Push-Only" unten.
//!
//! # Die Fähigkeiten: `CAP_BPF` und `CAP_PERFMON`
//! `harw_dod_cap::Capability::LoadBpfProgram`, Klasse
//! `harw_dod_cap::CapabilityClass::Bpf` — die einzige erhöhte Fähigkeit im
//! Vokabular dieses Programms, für die dieses Binary existiert. Beide
//! logischen Sensoren dieser Sonde (Prozessstart/-ende, Verbindung) tragen
//! exakt diese eine Fähigkeit. Der reale Anheftpfad
//! (`harw_dod_bpf::RealBpfLoader::load_contracts`) prüft zusätzlich
//! `CAP_PERFMON`, weil Tracepoint- und `fentry`-Anheftung auf aktuellen
//! Kerneln beides verlangen; fehlt eine der beiden, degradiert der
//! betroffene Sensor (siehe Abschnitt „Stand der eBPF-Bindung").
//!
//! # Push-Only: woran man die Eigenschaft erkennt, und wie sie erzwungen ist
//! Ein Prozess mit `CAP_BPF`, der Nachrichten **annimmt**, ist ein
//! Angriffsziel; einer, der nur **sendet**, ist keins. Diese Eigenschaft ist
//! hier keine Konvention, sondern eine Typ-Eigenschaft:
//! [`sink::EventSink`] hat genau eine Methode (`send`), die ausschließlich
//! Daten **entgegennimmt** — es gibt kein `poll_command`, kein
//! `set_watch_from`, keine Methode, über die der Sentinel diesem Prozess
//! etwas mitteilen könnte, und weder [`sink::SentinelSink`] noch irgendein
//! anderer Typ dieser Crate bindet einen Socket, der eingehende Verbindungen
//! annehmen könnte — es gibt nur `connect()`, nie eine
//! Lauschstellen-Funktion. `src/push_only_guard.rs` macht das
//! quelltextlich **prüfbar**, nicht nur behauptet: es durchsucht jede
//! Implementierungsdatei dieser Crate (außer sich selbst) nach den beiden
//! Bezeichnern, die einen Empfangspfad ausmachen würden — der
//! Unix-Socket-Empfangsfunktion und der Funktion, die einen Puffer bis zum
//! Streamende einliest. Ein späterer Leser bestätigt die Zusage, indem er
//! genau diese eine, kurze Datei liest, nicht den gesamten Quelltext dieser
//! Crate. Dieselbe Bauweise wie `harw-probe-fs/src/push_only_guard.rs` —
//! dasselbe Konsumentenpaar (privilegierte Sonde → unprivilegierter
//! Sentinel), dieselbe strukturelle Zusage, unabhängig für dieses Binary neu
//! aufgebaut, nicht importiert (jede der drei privilegierten Sonden dieses
//! Programms trägt ihre eigene, in ihrem eigenen Quelltext geprüfte Kopie).
//!
//! # Die Landlock-Asymmetrie
//! Ein unprivilegierter Sammler (`harw-sentinel`), der ohne Landlock läuft,
//! ist ein Sammler ohne zusätzliche Schranke — hinnehmbar, weil sein
//! Berechtigungsumfang von vornherein klein ist (er hält keine einzige
//! erhöhte Fähigkeit). Ein Prozess mit `CAP_BPF` ohne Schranke ist etwas
//! anderes: dort ist Landlock nicht eine von mehreren Verteidigungslinien,
//! sondern die einzige, die den tatsächlichen Dateisystemzugriff dieses
//! Prozesses gegen sein volles Berechtigungsvermögen einschränkt. [`run`]
//! ruft deshalb [`landlock::enforce_fs_scope`] vor jedem Lade- und
//! Anheftschritt auf und bricht bei jedem Ausgang außer vollständiger
//! Durchsetzung hart ab — Entscheidung Nr. 4 des Architekturberichts dieses
//! Programms, und dieselbe Asymmetrie, die `harw-probe-fs` bereits für sich
//! trifft. Siehe [`landlock`]-Moduldoku für die vollständige Begründung.
//!
//! Die Reihenfolge in [`run`] ist deshalb fest: aktives DoD-Profil laden
//! (fail closed, [`profile::load_active_profile`]) → Sentinel verbinden →
//! **die eBPF-Objekte der vom Profil gewählten Sensoren auflösen und
//! einlesen** ([`sensors::resolve_procmon_objects`],
//! [`sensors::resolve_flow_objects`]: die Bytes liegen danach als
//! `BpfProgramSource::Embedded` im Speicher) → den Scope des Profils
//! auflösen ([`profile::resolve_scope`], liest den cgroup-Baum) → Landlock
//! durchsetzen → laden und anheften. Das Objektverzeichnis
//! (`--*-program-path`, `$HARW_DOD_BPF_DIR`, `/usr/local/lib/harw-dod/bpf`)
//! und die Konfigurationsdatei sind damit **nicht** Teil des
//! Landlock-Ausschnitts. Lesbar bleiben nur die Kernel-Schnittstellen, die
//! der Lade-, Anheft- und Lesepfad selbst braucht
//! ([`KERNEL_INTERFACE_ROOTS`], siehe [`fs_scope_roots`]), und — nur bei
//! einem cgroup-Profil mit `include_descendants = true` — `--cgroup-root`
//! zum Auflisten (siehe [`landlock`]-Moduldoku, „Die cgroup2-Wurzel").
//!
//! # Das `SensorId`-Schema
//! `harw_dod_signals::SecurityEvent::sensor` ist die **einzige**
//! Herkunftsangabe, die beim Sentinel ankommt — jedes über
//! [`sink::EventSink::send`] geschobene Ereignis landet dort (über
//! `harw_dod_sentinel::Sentinel::record_external_event(&mut self, event:
//! SecurityEvent)`, den Weg, den die Gegenseite inzwischen für genau diesen
//! Zweck bekommen hat) ohne einen zusätzlichen Transportkanal, der die
//! Herkunft noch einmal trüge. Diese Sonde hostet **zwei** logische
//! Sensoren (Prozessstart, Verbindung), multiplext über **eine**
//! Push-Only-Verbindung — das Schema muss deshalb sowohl das Binary als
//! auch den internen Teilsensor benennen:
//!
//! - `probe-bpf-procmon-0` (Vorgabe, `--sensor-id-procmon`) für
//!   Prozessstart-Ereignisse.
//! - `probe-bpf-flow-0` (Vorgabe, `--sensor-id-flow`) für
//!   Verbindungs-Ereignisse.
//!
//! Muster: `<binary>-<teilsensor>-<instanz>`, angelehnt an das bereits im
//! Workspace etablierte `<art>-<n>` (`harw-sentinel`s `cpu-0`, `thermal-0`,
//! `listener-0`, `workspace-drift-0`, `sentinel-landlock`;
//! `harw-probe-fs`s `fsmon-0`), aber mit dem Präfix `probe-bpf-`, weil
//! **ein** Binary hier **zwei** unterscheidbare Ereignisarten über **eine**
//! flache `SensorId`-Namensfläche meldet — ohne das Präfix wären
//! `procmon-0`/`flow-0` mit einer künftigen eigenständigen Sensor-Crate
//! gleichen Namens verwechselbar. Beide Kennungen sind über
//! `--sensor-id-procmon`/`--sensor-id-flow` überschreibbar, falls ein
//! Betreiber mehrere Instanzen dieser Sonde nebeneinander betreibt (z. B.
//! je Netzwerk-Namensraum).
//!
//! # Zwei Ansteuerungspfade in einer Schleife
//! Geladene Sensoren werden nicht über `harw_dod_signals::Sensor::poll`
//! gelesen, sondern direkt über den gemeinsamen `RealBpfLoader`
//! ([`collect::drain_wire_once`]): der produktive Ladepfad
//! (`load_contracts`, `read_wire_events`, `loss_counters`) ist ein
//! inhärenter Teil von `RealBpfLoader`, nicht des `BpfLoader`-Traits, dessen
//! `load` für reale Objekte bewusst immer scheitert. Nur degradierte
//! Sensoren ([`sensors::UnavailableSensor`]) laufen weiter über den
//! generischen `Sensor`-Pfad ([`collect::run_once`]). [`collect_forever`]
//! bedient beide Pfade in derselben Runde. Das Urteil zur
//! Schnittstellen-Unstimmigkeit zwischen `harw-dod-procmon` und
//! `harw-dod-flow` steht in der [`sensors`]-Moduldoku.
//!
//! # Stand der eBPF-Bindung
//! Produktiv lädt diese Sonde über **einen** gemeinsamen
//! `harw_dod_bpf::RealBpfLoader` ([`real_loader::build_real_loader`]) je
//! Sensor einen vollständigen, transaktionalen Vertragssatz
//! ([`real_loader::load_sensor`]): `harw_dod_procmon::procmon_contracts`
//! (`exec.bpf.o`, `exit.bpf.o`) und `harw_dod_flow::flow_contracts`
//! (`tcp_v4_connect.bpf.o`, `tcp_v6_connect.bpf.o`), beide mit dem
//! `harw_dod_bpf::BpfScope` des aktiven Profils
//! ([`profile::ProbeScope::bpf_scope`]). Die geladenen Griffe werden als
//! [`collect::WireSource`] von [`collect::drain_wire_once`] gelesen
//! (v1-Wire-Ereignisse, Verlustzähler).
//!
//! # Das aktive DoD-Profil (PL-97 E3c)
//! Profil, Sensorwahl, Egress-CIDRs und Scope kommen ausschließlich aus
//! `harw-dod-config` ([`profile`]-Moduldoku): `--config` bzw. der feste
//! Systempfad, danach `resolve_active`. Jeder Lade-, Vertrauens-, Parse-
//! oder Auswahlfehler — auch eine fehlende Datei — beendet den Start mit
//! [`error::ProbeError::Config`]; es gibt keinen Rückfall auf eingebaute
//! Vorgaben und nie auf den Host-Scope.
//!
//! - `sensors`: `exec` lädt den Prozess-Sensor, `tcp-connect` den
//!   Verbindungs-Sensor; ein nicht gewählter Sensor wird weder aufgelöst
//!   noch geladen noch als degradiert gemeldet.
//! - `egress_allow_cidrs`: der `NetworkScope` des Verbindungs-Sensors
//!   ([`network_scope`]); leer meldet jede ausgehende Verbindung.
//! - `scope = "host"`: `BpfScope::Host`, kein Rescan, Landlock-Regelsatz
//!   unverändert — exakt das Verhalten vor E3c (die ausgelieferte
//!   `dod/config.toml` wählt genau dieses Profil mit beiden Sensoren).
//! - `scope = "cgroups"`: jede konfigurierte Wurzel wird vor Landlock unter
//!   `--cgroup-root` aufgelöst. Eine fehlende Wurzel wird geloggt und als
//!   degradierter Start gemeldet (genau ein `SensorDegraded` je geladenem
//!   Sensor, [`collect::report_scope_degraded`]), nie still verworfen; löst
//!   **keine** Wurzel auf, verweigert [`run`] das Anheften
//!   ([`error::ProbeError::CgroupScopeUnresolved`]).
//!
//! # Dynamischer cgroup-Scope (PL-97 E3b + E3c): verdrahtet
//! Ein Profil mit `scope = "cgroups"` und `include_descendants = true`
//! expandiert seine Wurzeln vor dem Anheften in die `SCOPE_CGROUP_IDS`-Map
//! (`cgroup_walk::walk_descendants` + `DescendantScope::plan`/`commit`) und
//! hält sie danach über [`scope_rescan::ScopeRescan`] aktuell: alle
//! [`scope_rescan::SCOPE_RESCAN_INTERVAL`] (2 s) werden die Wurzeln unter
//! `--cgroup-root` neu durchlaufen, neue Nachfahren eingefügt, verschwundene
//! zurückgezogen ([`collect::rescan_scope_once`]). Damit der Rescan nach
//! `restrict_self` auflisten kann, nimmt [`fs_scope_roots`] genau dann
//! `--cgroup-root` als nur auflistende Landlock-Wurzel auf.
//!
//! Stand der Folgeschritte:
//! - [x] E3b: Userspace-Mechanismus (Walk, Planer, inkrementelle
//!   Map-Aktualisierung, 2-s-Rescan, Abgleich teilweiser Aktualisierungen
//!   je Griff).
//! - [x] E3c: Profil auflösen, Scope vor Landlock bauen, Sensoren und CIDRs
//!   aus dem Profil, `--cgroup-root` als nur auflistende Landlock-Wurzel.
//! - [ ] Kernelseitiger Vorfahrenabgleich
//!   (`bpf_get_current_ancestor_cgroup_id` unter `dod/bpf/`, ABI-Bump): ein
//!   eigener Folgeschritt; er schlösse das Fenster von bis zu einem
//!   Rescan-Intervall, in dem eine brandneue cgroup noch nicht beobachtet
//!   wird.
//!
//! Degradierung statt Prozessende, je Sensor unabhängig:
//! - fehlt ein Objekt oder ist es ungültig, registriert [`run`] einen
//!   [`sensors::UnavailableSensor`] mit der Mängelliste;
//! - scheitert das Laden mit `AttachCapabilitiesUnavailable`,
//!   `CapabilityUnavailable` oder `InvalidProgramContract` (siehe
//!   [`degrades_sensor`]), ebenfalls — ein Host ohne die nötigen
//!   Fähigkeiten oder mit einem nicht vertragsgemäßen Objekt ist ein
//!   dokumentierter Betriebsfall, den der Sentinel als `SensorDegraded`
//!   sieht;
//! - jeder andere Ladefehler beendet den Prozess; bereits geladene Griffe
//!   werden vorher entladen.
//!
//! Endet die Sammelschleife (dauerhafter Fehler, z. B. Sentinel weg), werden
//! alle geladenen Objekte über [`collect::unload_all`] kontrolliert
//! entladen, bevor [`run`] den Fehler zurückgibt. Ein Prozessende per Signal
//! entlädt nicht explizit; die Kernel-Anheftungen hängen dann nur an den
//! Dateideskriptoren dieses Prozesses und verschwinden mit ihm.
//!
//! # Fehler
//! Kommandozeilenfehler sind `clap::Error`, direkt in [`main`] behandelt.
//! Jeder Fehler aus [`run`] ist ein [`error::ProbeError`] — siehe dessen
//! Moduldoku für die vollständige Varianteneinteilung.
//!
//! # Nebenläufigkeit
//! [`run`] läuft im Hauptthread; [`collect_forever`] startet keinen weiteren
//! Thread — degradierte Sensoren und Wire-Quellen werden sequenziell im
//! selben Thread bedient (siehe [`collect`]-Moduldoku).
//!
//! # Examples
//! ```text
//! $ harw-probe-bpf --log info \
//!     --config /etc/harw-dod/config.toml \
//!     --sentinel-socket /run/harw/sentinel.sock
//! ```

#![forbid(unsafe_code)]

mod cli;
mod collect;
mod error;
mod landlock;
mod profile;
mod real_loader;
mod scope_rescan;
mod sensors;
mod sink;

#[cfg(test)]
mod push_only_guard;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser as _;
use harw_authority::{EgressTarget, NetworkScope};
use harw_completions::CompletionsSubcommand;
use harw_dod_bpf::cgroup_walk::{HostCgroupFs, WalkLimits};
use harw_dod_bpf::{BpfError, BpfObjectContract, BpfProgramSource, BpfScope, RealBpfLoader};
use harw_dod_cap::Permanence;
use harw_dod_config::ResolvedObservationProfile;
use harw_dod_signals::Sensor;
use harw_types::SensorId;
use jiff::Timestamp;

use crate::landlock::FsRoot;
use cli::{Cli, LogLevel};
use collect::WireSource;
use error::ProbeError;
use profile::{ProbeScope, SensorSelection};
use scope_rescan::{SCOPE_RESCAN_INTERVAL, ScopeRescan};
use sensors::{ObjectUnavailable, ResolvedBpfObject, UnavailableSensor};
use sink::EventSink;

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das bei einem Fehler
/// oder `--help`/`--version` selbst `std::process::exit` aufriefe und diese
/// Funktion nie zu ihrem eigenen `ExitCode` zurückkehren ließe),
/// initialisiert `tracing` und delegiert an [`run`].
///
/// # Returns
/// `ExitCode::SUCCESS` nur, wenn [`run`] regulär zurückkehrt (heute nur,
/// wenn kein einziger Sensor registriert wäre — die Sammelschleife endet
/// sonst nur über einen Fehler). `ExitCode::FAILURE` bei jedem Fehlerpfad —
/// Kommandozeile, Landlock-Schranke, nicht degradierender Ladefehler,
/// fehlender Sentinel.
/// Der `completions`-Unterbefehl endet mit `ExitCode::SUCCESS` bzw.
/// `ExitCode::FAILURE`, bevor Socket, Landlock oder Sensoren angefasst werden.
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{err}");
                return ExitCode::SUCCESS;
            }
            eprint!("{err}");
            return ExitCode::FAILURE;
        }
    };

    init_tracing(cli.log);

    // `completions` läuft vor jedem Socket-, Landlock- oder BPF-Schritt.
    if let Some(CompletionsSubcommand::Completions(args)) = cli.command.as_ref() {
        return match run_completions(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                tracing::error!(error = %err, "harw-probe-bpf completions failed");
                eprintln!("harw-probe-bpf: {err}");
                ExitCode::FAILURE
            }
        };
    }

    // `clap` erzwingt `--sentinel-socket` ohne Unterbefehl bereits
    // (`required = true`); diese Prüfung ist nur die defensive Entpackung
    // des `Option` (siehe `cli`-Moduldoku).
    let Some(sentinel_socket) = cli.sentinel_socket.clone() else {
        let err = <Cli as clap::CommandFactory>::command().error(
            clap::error::ErrorKind::MissingRequiredArgument,
            "the following required argument was not provided: --sentinel-socket <PATH>",
        );
        eprint!("{err}");
        return ExitCode::FAILURE;
    };

    match run(cli, &sentinel_socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-probe-bpf exiting");
            eprintln!("harw-probe-bpf: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Initialisiert den globalen `tracing`-Subscriber.
///
/// # Description
/// Muss genau einmal aufgerufen werden, als erste Anweisung nach dem
/// Parsen der Kommandozeile. Die angeforderte Stufe wird unverändert
/// verwendet, nie stillschweigend reduziert — siehe [`cli::LogLevel`]-
/// Moduldoku für die Begründung, warum ein ungültiger Rohwert diese
/// Funktion nie erreicht.
///
/// # Arguments
/// - `level` (`cli::LogLevel`): die über `--log` angeforderte Stufe.
///
/// # Panics
/// Panics, falls bereits ein globaler Subscriber installiert ist (nur
/// erreichbar, wenn diese Funktion zweimal aufgerufen wird — ein
/// Programmierfehler).
fn init_tracing(level: LogLevel) {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_new(level.as_filter_directive()).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Führt den `completions`-Unterbefehl aus.
///
/// # Description
/// Schreibt das Completion-Skript nach stdout oder installiert bzw. entfernt
/// es (siehe `harw_completions::run_completions`). Berührt weder Socket
/// noch Landlock noch eBPF.
///
/// # Arguments
/// - `args` (`&harw_completions::CompletionsArgs`): die geparsten
///   Unterbefehls-Argumente.
///
/// # Errors
/// [`error::ProbeError::Completions`], wenn Erzeugung, Installation oder das
/// Schreiben nach stdout scheitert.
fn run_completions(args: &harw_completions::CompletionsArgs) -> Result<(), ProbeError> {
    harw_completions::run_completions(
        &mut <Cli as clap::CommandFactory>::command(),
        "harw-probe-bpf",
        args,
        &harw_completions::HomeEnv::from_process(),
        &mut std::io::stdout().lock(),
    )?;
    Ok(())
}

/// Kernel-Schnittstellen, die der Lade-, Anheft- und Lesepfad nach der
/// Landlock-Durchsetzung noch lesen muss.
///
/// # Description
/// `landlock::enforce_fs_scope` sperrt jeden lesenden Dateisystemzugriff
/// außerhalb der übergebenen Wurzeln. Das Laden selbst liest aber weiterhin
/// ein paar feste Kernel-Dateien (nicht das Objektverzeichnis — die Objekte
/// sind zu diesem Zeitpunkt bereits eingelesen):
/// - `/proc/self`: `CapEff` aus `/proc/self/status`
///   (`RealBpfLoader`s Fähigkeitsprüfung vor dem Anheften);
/// - `/sys/kernel/btf`: `vmlinux`-BTF für CO-RE-Relokationen und das
///   Laden der `fentry`-Programme;
/// - `/sys/kernel/tracing` und `/sys/kernel/debug/tracing`: Tracepoint-IDs
///   für das Anheften an `sched:*` (je nach Mount einer von beiden);
/// - `/sys/devices/system/cpu`: die möglichen CPUs, gebraucht beim Lesen
///   der per-CPU-Verlustzähler in der Sammelschleife.
///
/// Nur Verzeichnisse, nie Einzeldateien: eine `PathBeneath`-Regel auf einer
/// Datei vertrüge die Verzeichnisrechte aus `AccessFs::from_read` nicht.
const KERNEL_INTERFACE_ROOTS: [&str; 5] = [
    "/proc/self",
    "/sys/kernel/btf",
    "/sys/kernel/tracing",
    "/sys/kernel/debug/tracing",
    "/sys/devices/system/cpu",
];

/// Führt den eigentlichen Sondenbetrieb aus.
///
/// # Description
/// Reihenfolge (siehe Moduldoku, Abschnitt „Die Landlock-Asymmetrie"):
/// 1. Das aktive DoD-Profil laden ([`profile::load_active_profile`]) —
///    fail closed, bevor irgendein Socket, Objekt oder cgroup angefasst
///    wird.
/// 2. Sentinel verbinden — ein Dateisystempfad-Zugriff, der nach Landlock
///    scheitern würde (Muster: `harw-sentinel::main::run`,
///    `harw-probe-fs::main::run`).
/// 3. Die eBPF-Objekte der vom Profil gewählten Sensoren auflösen und
///    einlesen ([`sensors::resolve_procmon_objects`],
///    [`sensors::resolve_flow_objects`]) — **vor** Landlock, damit das
///    Objektverzeichnis nicht im Landlock-Ausschnitt stehen muss.
/// 4. Den Scope des Profils auflösen ([`resolve_profile_scope`]) — **vor**
///    Landlock, weil Wurzelauflösung und Nachfahren-Expansion den
///    cgroup-Baum lesen.
/// 5. Landlock durchsetzen (harter Abbruch bei jedem Ausgang außer
///    vollständiger Durchsetzung), mit den Wurzeln aus [`fs_scope_roots`].
/// 6. Eine gemeinsame reale Ladeschicht bauen und je gewähltem Sensor den
///    Vertragssatz mit dem Profil-Scope laden ([`setup_sensor`]); fehlende
///    Objekte oder ein degradierender Ladefehler registrieren einen
///    [`sensors::UnavailableSensor`]. War der Scope schon beim Start
///    unvollständig, geht genau ein `SensorDegraded` je geladenem Sensor an
///    den Sentinel.
/// 7. Die Sammelschleife betreiben ([`collect_forever`], mit Rescan nur für
///    einen Nachfahren-Scope); endet sie, werden alle geladenen Objekte
///    entladen.
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`error::ProbeError::Config`] für jede nicht ladbare, nicht
/// vertrauenswürdige oder ungültige Konfiguration;
/// [`error::ProbeError::CgroupRootInvalid`],
/// [`error::ProbeError::CgroupScopeUnresolved`] und
/// [`error::ProbeError::CgroupScopeRejected`] für einen nicht anheftbaren
/// cgroup-Scope (nie ein Rückfall auf den Host-Scope);
/// [`error::ProbeError::SentinelConnectFailed`] wenn der Sentinel nicht
/// erreichbar ist; [`error::ProbeError::LandlockUnavailable`] wenn Landlock
/// den Ausschnitt nicht vollständig durchsetzt;
/// [`error::ProbeError::BpfLoad`] wenn das Laden eines Sensors mit einem
/// nicht degradierenden Fehler scheitert (siehe [`degrades_sensor`]); jeder
/// dauerhafte Fehler der Sammelschleife.
fn run(cli: Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let profile = profile::load_active_profile(cli.config.as_deref())?;
    let selection = SensorSelection::from_profile(profile.sensors());

    let sink = sink::build_sentinel_sink(sentinel_socket)?;
    tracing::info!(path = %sentinel_socket.display(), "connected to sentinel");

    // Objekte vor Landlock einlesen: danach braucht dieser Prozess keinen
    // Lesezugriff auf das Objektverzeichnis mehr. Ein vom Profil nicht
    // gewählter Sensor wird gar nicht erst aufgelöst.
    let procmon_objects = selection.exec.then(|| {
        sensors::resolve_procmon_objects(
            cli.exec_program_path.as_deref(),
            cli.exit_program_path.as_deref(),
        )
    });
    let flow_objects = selection.tcp_connect.then(|| {
        sensors::resolve_flow_objects(
            cli.tcp_v4_program_path.as_deref(),
            cli.tcp_v6_program_path.as_deref(),
        )
    });

    // Scope vor Landlock festlegen: Wurzelauflösung und Nachfahren-Expansion
    // lesen den cgroup-Baum. Kein Rückfall auf den Host-Scope.
    let probe_scope = resolve_profile_scope(&profile, &cli.cgroup_root)?;
    let scope = probe_scope.bpf_scope();

    landlock::enforce_fs_scope(&fs_scope_roots(&probe_scope, &cli.cgroup_root))?;

    let loader = real_loader::build_real_loader()?;
    let mut degraded: Vec<Arc<dyn Sensor>> = Vec::new();
    let mut sources: Vec<WireSource> = Vec::new();

    if let Some(objects) = procmon_objects {
        setup_sensor(
            &loader,
            SensorId::from_str(cli.sensor_id_procmon.clone()),
            objects,
            |sensor, exec, exit| {
                harw_dod_procmon::procmon_contracts(sensor, exec, exit, scope.clone())
            },
            None,
        )?
        .register(&mut degraded, &mut sources);
    } else {
        tracing::info!(
            sensor = %cli.sensor_id_procmon,
            "exec sensor not selected by the active DoD profile; not loaded"
        );
    }

    if let Some(objects) = flow_objects {
        let flow = setup_sensor(
            &loader,
            SensorId::from_str(cli.sensor_id_flow.clone()),
            objects,
            |sensor, tcp_v4, tcp_v6| {
                harw_dod_flow::flow_contracts(sensor, tcp_v4, tcp_v6, scope.clone())
            },
            Some(network_scope(profile.egress_allow_cidrs())),
        );
        match flow {
            Ok(setup) => setup.register(&mut degraded, &mut sources),
            Err(err) => {
                // Bereits geladene Prozess-Objekte nicht angeheftet zurücklassen.
                collect::unload_all(&loader, &sources);
                return Err(err);
            }
        }
    } else {
        tracing::info!(
            sensor = %cli.sensor_id_flow,
            "tcp-connect sensor not selected by the active DoD profile; not loaded"
        );
    }

    tracing::info!(
        wire_sources = sources.len(),
        degraded_sensors = degraded.len(),
        scope = probe_scope.label(),
        "sensors registered"
    );

    if let Err(err) = report_degraded_start(&probe_scope, &sources, sink.as_ref()) {
        collect::unload_all(&loader, &sources);
        return Err(err);
    }

    let mut rescan = probe_scope.into_rescan(
        cli.cgroup_root.clone(),
        SCOPE_RESCAN_INTERVAL,
        WalkLimits::default(),
        Instant::now(),
    );
    let result = collect_forever(&loader, &degraded, &sources, sink.as_ref(), rescan.as_mut());
    collect::unload_all(&loader, &sources);
    tracing::info!("bpf objects unloaded after the collect loop ended");
    result
}

/// Resolves the attach-time scope of the active profile, before Landlock.
///
/// # Description
/// Wires PL-97 E3b into the probe (E3c); it replaces the former placeholder
/// `profile_descendant_scope()`, which always returned `None`. Delegates to
/// [`profile::resolve_scope`] with the host file system and the default walk
/// bounds: `scope = "host"` touches no file and yields `BpfScope::Host`; a
/// cgroup profile resolves every configured path below `cgroup_root`, and
/// with `include_descendants = true` expands the roots into a committed
/// `harw_dod_bpf::DescendantScope` that [`ProbeScope::into_rescan`] later
/// hands to the 2 s rescan. Logs the resolved scope once.
///
/// # Arguments
/// - `profile` (`&ResolvedObservationProfile`): the active profile.
/// - `cgroup_root` (`&Path`): `--cgroup-root`.
///
/// # Errors
/// [`ProbeError::CgroupRootInvalid`], [`ProbeError::CgroupScopeUnresolved`]
/// (no configured root resolved: never widened to the host) or
/// [`ProbeError::CgroupScopeRejected`].
fn resolve_profile_scope(
    profile: &ResolvedObservationProfile,
    cgroup_root: &Path,
) -> Result<ProbeScope, ProbeError> {
    let probe_scope = profile::resolve_scope(
        &HostCgroupFs,
        cgroup_root,
        profile.scope(),
        WalkLimits::default(),
    )?;
    let (roots, descendants) = match probe_scope.bpf_scope() {
        BpfScope::Host => (0, 0),
        BpfScope::Cgroups {
            groups,
            descendants,
            ..
        } => (groups.len(), descendants.len()),
    };
    tracing::info!(
        profile = profile.profile_id().as_str(),
        scope = probe_scope.label(),
        roots,
        descendants,
        unresolved_roots = probe_scope.unresolved_roots().len(),
        rescan = probe_scope.rescans(),
        "bpf scope resolved before attach"
    );
    Ok(probe_scope)
}

/// Reports a scope that was already incomplete at attach time.
///
/// # Description
/// A configured root that did not resolve, a full map, a depth-limited or
/// incomplete attach-time walk: logged once with the unavailable roots and
/// reported as exactly one `SensorDegraded` per loaded wire sensor
/// ([`collect::report_scope_degraded`]). The rescan takes this health as its
/// baseline, so the same state is not reported again. Without a wire source
/// there is nothing to report (degraded sensors report themselves).
///
/// # Returns
/// The number of reports sent.
///
/// # Errors
/// The error of [`EventSink::send`].
fn report_degraded_start(
    probe_scope: &ProbeScope,
    sources: &[WireSource],
    sink: &dyn EventSink,
) -> Result<usize, ProbeError> {
    let startup = probe_scope.startup_health();
    if !startup.is_degraded() || sources.is_empty() {
        return Ok(0);
    }
    let unresolved: Vec<String> = probe_scope
        .unresolved_roots()
        .iter()
        .map(|root| format!("{} ({})", root.path, root.error))
        .collect();
    tracing::warn!(
        scope = probe_scope.label(),
        unresolved_roots = ?unresolved,
        overflowed = startup.overflowed,
        depth_limited = startup.depth_limited,
        walk_incomplete = startup.incomplete,
        "bpf cgroup scope degraded at attach; reporting SensorDegraded to the sentinel"
    );
    collect::report_scope_degraded(sources, sink, Timestamp::now())
}

/// Ergebnis des Aufbaus eines Sensors: geladen oder degradiert.
enum SensorSetup {
    /// Alle Objekte des Sensors sind geladen und angeheftet.
    Wire(WireSource),
    /// Der Sensor meldet sich beim Sentinel als degradiert.
    Degraded(UnavailableSensor),
}

impl SensorSetup {
    /// Sortiert den Sensor in die passende Liste der Sammelschleife ein.
    ///
    /// # Arguments
    /// - `degraded` (`&mut Vec<Arc<dyn Sensor>>`): Sensoren für
    ///   [`collect::run_once`].
    /// - `sources` (`&mut Vec<WireSource>`): Quellen für
    ///   [`collect::drain_wire_once`].
    fn register(self, degraded: &mut Vec<Arc<dyn Sensor>>, sources: &mut Vec<WireSource>) {
        match self {
            Self::Wire(source) => sources.push(source),
            Self::Degraded(sensor) => degraded.push(Arc::new(sensor)),
        }
    }
}

/// Baut einen Sensor aus seinen aufgelösten Objekten.
///
/// # Description
/// Fehlen Objekte, entsteht ohne jeden Ladeversuch ein
/// [`sensors::UnavailableSensor`] mit der Mängelliste. Sonst baut `contracts`
/// den Vertragssatz (Reihenfolge wie die Objekte), und
/// [`real_loader::load_sensor`] lädt ihn transaktional. Ein Ladefehler, den
/// [`degrades_sensor`] als Betriebsfall einstuft, wird geloggt und ebenfalls
/// zu einem `UnavailableSensor`; jeder andere Ladefehler wird
/// zurückgegeben.
///
/// # Arguments
/// - `loader` (`&harw_dod_bpf::RealBpfLoader`): die gemeinsame Ladeschicht.
/// - `sensor` (`harw_types::SensorId`): die Kennung des Sensors.
/// - `objects`: das Ergebnis von [`sensors::resolve_procmon_objects`] bzw.
///   [`sensors::resolve_flow_objects`].
/// - `contracts`: baut aus Kennung und den beiden Objektquellen den
///   Vertragssatz, z. B. `harw_dod_procmon::procmon_contracts` mit dem
///   `BpfScope` des aktiven Profils.
/// - `net_scope` (`Option<harw_authority::NetworkScope>`): Melderegel für
///   Verbindungsereignisse aus den `egress_allow_cidrs` des Profils; `None`
///   für den Prozess-Sensor.
///
/// # Returns
/// [`SensorSetup::Wire`] oder [`SensorSetup::Degraded`].
///
/// # Errors
/// [`ProbeError::BpfLoad`] (bzw. jeder andere Fehler aus
/// [`real_loader::load_sensor`]), wenn der Ladefehler nicht degradierend ist.
fn setup_sensor(
    loader: &RealBpfLoader,
    sensor: SensorId,
    objects: Result<[ResolvedBpfObject; 2], Vec<ObjectUnavailable>>,
    contracts: impl FnOnce(SensorId, BpfProgramSource, BpfProgramSource) -> [BpfObjectContract; 2],
    net_scope: Option<NetworkScope>,
) -> Result<SensorSetup, ProbeError> {
    let [first, second] = match objects {
        Ok(objects) => objects,
        Err(missing) => {
            return Ok(SensorSetup::Degraded(UnavailableSensor::new(
                sensor, missing,
            )));
        }
    };
    let contracts = contracts(sensor.clone(), first.into_source(), second.into_source());
    match real_loader::load_sensor(loader, &contracts) {
        Ok(handles) => {
            tracing::info!(
                sensor = %sensor,
                programs = handles.len(),
                "bpf sensor loaded and attached"
            );
            Ok(SensorSetup::Wire(WireSource::new(
                sensor, handles, net_scope,
            )))
        }
        Err(ProbeError::BpfLoad(err)) if degrades_sensor(&err) => {
            tracing::warn!(
                sensor = %sensor,
                error = %err,
                "bpf sensor unavailable: loading or attaching failed"
            );
            Ok(SensorSetup::Degraded(UnavailableSensor::new(
                sensor,
                Vec::new(),
            )))
        }
        Err(err) => Err(err),
    }
}

/// Entscheidet, ob ein Ladefehler den Sensor nur degradiert.
///
/// # Description
/// Degradierend sind die dokumentierten Betriebsfälle: fehlende Fähigkeiten
/// (`AttachCapabilitiesUnavailable`, `CapabilityUnavailable`) und ein
/// Objekt, das dem v1-Vertrag nicht entspricht (`InvalidProgramContract`).
/// Alles andere (z. B. `ProgramLoadFailed`, `Io`) beendet den Prozess.
///
/// # Arguments
/// - `err` (`&harw_dod_bpf::BpfError`): der Ladefehler.
///
/// # Returns
/// `true`, wenn der Sensor als [`sensors::UnavailableSensor`] weiterläuft.
fn degrades_sensor(err: &BpfError) -> bool {
    matches!(
        err,
        BpfError::AttachCapabilitiesUnavailable
            | BpfError::CapabilityUnavailable
            | BpfError::InvalidProgramContract
    )
}

/// Die Sammelschleife: bedient degradierte Sensoren und Wire-Quellen im
/// Wechsel, bis ein dauerhafter Fehler auftritt.
///
/// # Description
/// Je Runde: [`collect::run_once`] über die degradierten Sensoren (liefert
/// deren einmalige `SensorDegraded`-Meldung, danach wartet jeder
/// `UnavailableSensor` selbst), dann [`collect::drain_wire_once`] über die
/// geladenen Quellen (mit eigenem Zeitbudget). Leere Listen werden
/// übersprungen. Liest die Systemuhr selbst — die Kompositionswurzel darf
/// das (Muster: `harw-probe-fs::collect::run_forever`).
///
/// # Arguments
/// - `loader` (`&harw_dod_bpf::RealBpfLoader`): die gemeinsame Ladeschicht.
/// - `degraded` (`&[Arc<dyn Sensor>]`): die degradierten Sensoren.
/// - `sources` (`&[WireSource]`): die geladenen Quellen.
/// - `sink` (`&dyn EventSink`): die Senke zum Sentinel.
/// - `rescan` (`Option<&mut ScopeRescan>`): der Rescan eines
///   Nachfahren-Scopes; je Runde vor dem Leeren der Ringpuffer bedient,
///   sobald er fällig ist ([`collect::rescan_scope_once`]).
///
/// # Returns
/// `Ok(())` nur, wenn beide Listen leer sind (dann gibt es nichts zu
/// sammeln); sonst kehrt diese Funktion nur über einen Fehler zurück.
///
/// # Errors
/// Der erste Fehler, den [`retry_transient`] nicht als vorübergehend
/// einstuft.
fn collect_forever(
    loader: &RealBpfLoader,
    degraded: &[Arc<dyn Sensor>],
    sources: &[WireSource],
    sink: &dyn EventSink,
    mut rescan: Option<&mut ScopeRescan>,
) -> Result<(), ProbeError> {
    if degraded.is_empty() && sources.is_empty() {
        tracing::warn!("no bpf sensor registered; nothing to collect");
        return Ok(());
    }
    loop {
        if !degraded.is_empty() {
            retry_transient(collect::run_once(degraded, sink, Timestamp::now()))?;
        }
        if !sources.is_empty() {
            if let Some(rescan) = rescan.as_deref_mut() {
                retry_transient(collect::rescan_scope_once(
                    rescan,
                    loader,
                    sources,
                    sink,
                    Instant::now(),
                ))?;
            }
            retry_transient(collect::drain_wire_once(loader, sources, sink))?;
        }
    }
}

/// Lässt einen vorübergehenden Sensorfehler durch, jeden anderen nicht.
///
/// # Arguments
/// - `result` (`Result<usize, ProbeError>`): das Ergebnis einer Runde.
///
/// # Returns
/// `Ok(())` bei Erfolg oder bei `ProbeError::Sensor` mit
/// `Permanence::Transient` (dann geloggt).
///
/// # Errors
/// Jeder andere Fehler, unverändert.
fn retry_transient(result: Result<usize, ProbeError>) -> Result<(), ProbeError> {
    match result {
        Ok(_) => Ok(()),
        Err(ProbeError::Sensor(err)) if err.permanence() == Permanence::Transient => {
            tracing::warn!(error = %err, "transient sensor error; retrying");
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Baut die Landlock-Wurzelliste dieses Prozesses.
///
/// # Description
/// Die vorhandenen Verzeichnisse aus [`KERNEL_INTERFACE_ROOTS`] — **nie**
/// das eBPF-Objektverzeichnis: die Objekte sind vor Landlock bereits
/// eingelesen (siehe [`run`]). Fehlende Verzeichnisse (z. B.
/// `/sys/kernel/debug/tracing` ohne debugfs) werden ausgelassen, statt bei
/// jedem Start eine Warnung aus `landlock::enforce_fs_scope` zu erzeugen;
/// ausgelassen heißt dort ohnehin unlesbar. Welche Wurzeln mit welchen
/// Rechten entstehen, entscheidet die reine Funktion [`landlock_roots`].
///
/// # Arguments
/// - `scope` (`&ProbeScope`): der aufgelöste Scope des aktiven Profils.
/// - `cgroup_root` (`&Path`): `--cgroup-root`.
///
/// # Returns
/// Siehe [`landlock_roots`].
fn fs_scope_roots(scope: &ProbeScope, cgroup_root: &Path) -> Vec<FsRoot> {
    let kernel_roots = KERNEL_INTERFACE_ROOTS
        .iter()
        .map(PathBuf::from)
        .filter(|root| root.is_dir());
    landlock_roots(kernel_roots, scope, cgroup_root)
}

/// Die Landlock-Wurzeln für einen Profil-Scope (rein, ohne Dateizugriff).
///
/// # Description
/// Jede Kernel-Schnittstelle als [`FsRoot::read`], in der übergebenen
/// Reihenfolge. Nur wenn der Scope nach dem Anheften neu durchlaufen wird
/// (`scope = "cgroups"` mit `include_descendants = true`,
/// [`ProbeScope::rescans`]), kommt **genau eine** weitere Wurzel hinzu:
/// `cgroup_root` als [`FsRoot::list_directories`] (nur `AccessFs::ReadDir`,
/// kein Lesen von Dateien, kein Ausführen, kein Schreiben; siehe
/// [`landlock`]-Moduldoku, „Die cgroup2-Wurzel"). Ein Host-Profil und ein
/// cgroup-Profil ohne Nachfahren ergeben exakt den Regelsatz vor E3c.
///
/// # Arguments
/// - `kernel_roots`: die vorhandenen Kernel-Schnittstellenverzeichnisse.
/// - `scope` (`&ProbeScope`): der aufgelöste Scope des aktiven Profils.
/// - `cgroup_root` (`&Path`): `--cgroup-root`.
///
/// # Returns
/// Die Wurzeln für [`landlock::enforce_fs_scope`].
fn landlock_roots(
    kernel_roots: impl IntoIterator<Item = PathBuf>,
    scope: &ProbeScope,
    cgroup_root: &Path,
) -> Vec<FsRoot> {
    let mut roots: Vec<FsRoot> = kernel_roots.into_iter().map(FsRoot::read).collect();
    if scope.rescans() {
        roots.push(FsRoot::list_directories(cgroup_root));
    }
    roots
}

/// Baut den `harw_authority::NetworkScope`, den die Verbindungs-Quelle
/// gegen jede beobachtete Verbindung prüft.
///
/// # Arguments
/// - `cidrs` (`&[ipnet::IpNet]`): erlaubte Zielnetze, die
///   `egress_allow_cidrs` des aktiven Profils.
///
/// # Returns
/// Einen `NetworkScope` aus den übergebenen Netzen. Ohne Angabe ein leerer
/// Scope, der jede ausgehende Verbindung meldet.
fn network_scope(cidrs: &[ipnet::IpNet]) -> NetworkScope {
    let targets: Vec<EgressTarget> = cidrs.iter().cloned().map(EgressTarget::Cidr).collect();
    NetworkScope::from_targets(targets)
}

#[cfg(test)]
mod tests {
    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten Socket verbinden, ein echtes Landlock-Ruleset binden und
    // echtes eBPF laden. Geprüft werden die reinen Entscheidungen dieser
    // Datei; Objektauflösung (`sensors.rs`), Laden (`real_loader.rs`) und
    // Sammeln (`collect.rs`) haben eigene Tests.

    use std::cell::Cell;
    use std::path::{Path, PathBuf};

    use harw_dod_bpf::cgroup_walk::{HostCgroupFs, WalkLimits};
    use harw_dod_bpf::{BpfError, RealBpfLoader};
    use harw_dod_cap::SensorError;
    use harw_types::SensorId;
    use ipnet::IpNet;
    use landlock::{ABI, AccessFs};

    use super::{
        KERNEL_INTERFACE_ROOTS, SensorSetup, degrades_sensor, fs_scope_roots, landlock_roots,
        network_scope, retry_transient, setup_sensor,
    };
    use crate::error::ProbeError;
    use crate::landlock::{FsRoot, RootAccess};
    use crate::profile::test_tree::{FakeTree, cgroup_scope};
    use crate::profile::{ProbeScope, resolve_scope};
    use crate::sensors::{
        BpfObjectKind, DEFAULT_BPF_OBJECT_DIR, ObjectUnavailable, ObjectUnavailableReason,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    const CGROUP_ROOT: &str = "/sys/fs/cgroup";

    fn kernel_roots() -> Vec<PathBuf> {
        KERNEL_INTERFACE_ROOTS.map(PathBuf::from).to_vec()
    }

    /// Der Regelsatz vor E3c: jede Kernel-Schnittstelle lesend, sonst nichts.
    fn kernel_only() -> Vec<FsRoot> {
        KERNEL_INTERFACE_ROOTS.map(FsRoot::read).to_vec()
    }

    /// Ein aufgelöster cgroup-Scope über einem gefälschten Baum.
    fn cgroup_probe_scope(tree: &FakeTree, include_descendants: bool) -> TestResult<ProbeScope> {
        tree.mkcg("/harw.slice")?;
        tree.mkcg("/harw.slice/harw-job-1")?;
        resolve_scope(
            &HostCgroupFs,
            tree.root(),
            &cgroup_scope(&["/harw.slice"], include_descendants)?,
            WalkLimits::default(),
        )
        .map_err(ctx("resolve the cgroup scope"))
    }

    #[test]
    fn test_fs_scope_roots_contains_only_kernel_interfaces_never_bpf_object_dirs() {
        let object_dir = Path::new(DEFAULT_BPF_OBJECT_DIR);
        for root in fs_scope_roots(&ProbeScope::host(), Path::new(CGROUP_ROOT)) {
            assert!(root.path.is_absolute());
            assert_eq!(root.access, RootAccess::Read);
            assert!(
                KERNEL_INTERFACE_ROOTS
                    .iter()
                    .any(|allowed| Path::new(allowed) == root.path)
            );
            assert!(!object_dir.starts_with(&root.path));
            assert!(!root.path.starts_with(object_dir));
        }
    }

    #[test]
    fn test_landlock_roots_for_a_host_profile_are_exactly_the_kernel_interfaces() {
        let roots = landlock_roots(kernel_roots(), &ProbeScope::host(), Path::new(CGROUP_ROOT));
        assert_eq!(roots, kernel_only());
        assert!(roots.iter().all(|root| !root.path.starts_with(CGROUP_ROOT)));
    }

    #[test]
    fn test_landlock_roots_for_a_fixed_cgroup_profile_are_unchanged() -> TestResult {
        // Ohne Nachfahren gibt es keinen Rescan nach `restrict_self`, also
        // auch keinen Grund, den cgroup-Baum lesbar zu lassen.
        let tree = FakeTree::new()?;
        let scope = cgroup_probe_scope(&tree, false)?;
        assert_eq!(
            landlock_roots(kernel_roots(), &scope, tree.root()),
            kernel_only()
        );
        Ok(())
    }

    #[test]
    fn test_landlock_roots_for_a_descendant_profile_add_one_list_only_cgroup_root() -> TestResult {
        let tree = FakeTree::new()?;
        let scope = cgroup_probe_scope(&tree, true)?;
        let roots = landlock_roots(kernel_roots(), &scope, tree.root());
        let mut expected = kernel_only();
        expected.push(FsRoot {
            path: tree.root().to_path_buf(),
            access: RootAccess::ListDirectories,
        });
        assert_eq!(roots, expected);

        let cgroup = roots.last().ok_or(TestError::Missing("cgroup root"))?;
        let rights = cgroup.access.rights(ABI::V1);
        assert!(!rights.contains(AccessFs::Execute));
        assert!(!rights.contains(AccessFs::ReadFile));
        assert!((rights & AccessFs::from_write(ABI::V1)).is_empty());
        assert!(rights.contains(AccessFs::ReadDir));
        Ok(())
    }

    /// Test-Senke: zeichnet jedes Ereignis auf, öffnet keinen Socket.
    #[derive(Default)]
    struct RecordingSink {
        sent: std::sync::Mutex<Vec<harw_dod_signals::SecurityEvent>>,
    }

    impl crate::sink::EventSink for RecordingSink {
        fn send(&self, event: &harw_dod_signals::SecurityEvent) -> Result<(), ProbeError> {
            self.sent
                .lock()
                .map_err(|_| ProbeError::SentinelSendFailed)?
                .push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn test_a_root_missing_at_start_is_reported_once_per_loaded_sensor() -> TestResult {
        let tree = FakeTree::new()?;
        tree.mkcg("/harw.slice")?;
        let degraded = resolve_scope(
            &HostCgroupFs,
            tree.root(),
            &cgroup_scope(&["/harw.slice", "/missing.slice"], true)?,
            WalkLimits::default(),
        )
        .map_err(ctx("partially resolvable scope"))?;
        let sources = ["probe-bpf-procmon-0", "probe-bpf-flow-0"].map(|sensor| {
            crate::collect::WireSource::new(SensorId::from_str(sensor), Vec::new(), None)
        });
        let sink = RecordingSink::default();
        let sent = super::report_degraded_start(&degraded, &sources, &sink)
            .map_err(ctx("report the degraded start"))?;
        assert_eq!(sent, 2);

        // Vollständig beobachteter Scope, Host-Scope oder keine geladene
        // Quelle: nichts zu melden.
        let healthy = cgroup_probe_scope(&FakeTree::new()?, true)?;
        assert_eq!(
            super::report_degraded_start(&healthy, &sources, &sink)
                .map_err(ctx("healthy scope"))?,
            0
        );
        assert_eq!(
            super::report_degraded_start(&ProbeScope::host(), &sources, &sink)
                .map_err(ctx("host scope"))?,
            0
        );
        assert_eq!(
            super::report_degraded_start(&degraded, &[], &sink).map_err(ctx("no source"))?,
            0
        );
        let reported = sink
            .sent
            .lock()
            .map_err(|_| TestError::Unexpected("recording sink poisoned".into()))?
            .len();
        assert_eq!(reported, 2);
        Ok(())
    }

    #[test]
    fn test_kernel_interface_roots_are_absolute_and_outside_the_object_dir() {
        let object_dir = Path::new(DEFAULT_BPF_OBJECT_DIR);
        for root in KERNEL_INTERFACE_ROOTS.map(PathBuf::from) {
            assert!(root.is_absolute());
            assert!(!object_dir.starts_with(&root));
        }
    }

    #[test]
    fn test_degrades_sensor_on_capability_and_contract_errors() {
        assert!(degrades_sensor(&BpfError::AttachCapabilitiesUnavailable));
        assert!(degrades_sensor(&BpfError::CapabilityUnavailable));
        assert!(degrades_sensor(&BpfError::InvalidProgramContract));
    }

    #[test]
    fn test_degrades_sensor_is_false_for_other_load_errors() {
        assert!(!degrades_sensor(&BpfError::ProgramLoadFailed));
        assert!(!degrades_sensor(&BpfError::UnknownHandle));
        assert!(!degrades_sensor(&BpfError::MalformedEvent));
    }

    #[test]
    fn test_setup_sensor_degrades_without_loading_when_objects_are_missing() -> TestResult {
        // Kein Kernelzugriff: bei fehlenden Objekten wird der Vertragssatz
        // gar nicht erst gebaut, `load_sensor` nie aufgerufen.
        let loader = RealBpfLoader::new();
        let contracts_built = Cell::new(false);
        let missing = vec![ObjectUnavailable {
            object: BpfObjectKind::Exec,
            path: PathBuf::from("/nonexistent/exec.bpf.o"),
            reason: ObjectUnavailableReason::Missing,
        }];
        let setup = setup_sensor(
            &loader,
            SensorId::from_str("probe-bpf-procmon-0"),
            Err(missing),
            |sensor, exec, exit| {
                contracts_built.set(true);
                harw_dod_procmon::procmon_contracts(
                    sensor,
                    exec,
                    exit,
                    harw_dod_bpf::BpfScope::Host,
                )
            },
            None,
        )
        .map_err(ctx("setup_sensor with missing objects"))?;
        assert!(matches!(setup, SensorSetup::Degraded(_)));
        assert!(!contracts_built.get());
        Ok(())
    }

    #[test]
    fn test_retry_transient_passes_success_and_transient_sensor_errors() -> TestResult {
        retry_transient(Ok(3)).map_err(ctx("success passes"))?;
        retry_transient(Err(ProbeError::Sensor(SensorError::MalformedSource)))
            .map_err(ctx("transient sensor error passes"))?;
        Ok(())
    }

    #[test]
    fn test_retry_transient_returns_permanent_and_non_sensor_errors() -> TestResult {
        match retry_transient(Err(ProbeError::Sensor(SensorError::OutsideScope))) {
            Err(ProbeError::Sensor(SensorError::OutsideScope)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        match retry_transient(Err(ProbeError::SentinelSendFailed)) {
            Err(ProbeError::SentinelSendFailed) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_network_scope_is_empty_without_configured_cidrs() {
        let scope = network_scope(&[]);
        assert!(!scope.allows_addr(std::net::IpAddr::from([127, 0, 0, 1])));
    }

    #[test]
    fn test_network_scope_allows_a_configured_cidr() -> TestResult {
        let cidrs = vec![
            "10.0.0.0/24"
                .parse::<IpNet>()
                .map_err(ctx("valid test CIDR literal"))?,
        ];
        let scope = network_scope(&cidrs);
        assert!(scope.allows_addr(std::net::IpAddr::from([10, 0, 0, 5])));
        assert!(!scope.allows_addr(std::net::IpAddr::from([203, 0, 113, 9])));
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

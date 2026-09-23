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
//! Die Reihenfolge in [`run`] ist deshalb fest: Sentinel verbinden →
//! **alle vier eBPF-Objekte auflösen und einlesen**
//! ([`sensors::resolve_procmon_objects`], [`sensors::resolve_flow_objects`]:
//! die Bytes liegen danach als `BpfProgramSource::Embedded` im Speicher) →
//! Landlock durchsetzen → laden und anheften. Das Objektverzeichnis
//! (`--*-program-path`, `$HARW_DOD_BPF_DIR`, `/usr/local/lib/harw-dod/bpf`)
//! ist damit **nicht** Teil des Landlock-Ausschnitts. Lesbar bleiben nur die
//! Kernel-Schnittstellen, die der Lade-, Anheft- und Lesepfad selbst braucht
//! ([`KERNEL_INTERFACE_ROOTS`], siehe [`fs_scope_roots`]).
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
//! (`tcp_v4_connect.bpf.o`, `tcp_v6_connect.bpf.o`), beide mit
//! `harw_dod_bpf::BpfScope::Host` — der einzige heute baubare Scope. Die
//! geladenen Griffe werden als [`collect::WireSource`] von
//! [`collect::drain_wire_once`] gelesen (v1-Wire-Ereignisse, Verlustzähler).
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
//!     --sentinel-socket /run/harw-sentinel.sock \
//!     --egress-allow-cidr 10.0.0.0/8
//! ```

#![forbid(unsafe_code)]

mod cli;
mod collect;
mod error;
mod landlock;
mod real_loader;
mod sensors;
mod sink;

#[cfg(test)]
mod push_only_guard;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser as _;
use harw_authority::{EgressTarget, NetworkScope};
use harw_completions::CompletionsSubcommand;
use harw_dod_bpf::{BpfError, BpfObjectContract, BpfProgramSource, BpfScope, RealBpfLoader};
use harw_dod_cap::Permanence;
use harw_dod_signals::Sensor;
use harw_types::SensorId;
use jiff::Timestamp;

use cli::{Cli, LogLevel};
use collect::WireSource;
use error::ProbeError;
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
/// 1. Sentinel verbinden — ein Dateisystempfad-Zugriff, der nach Landlock
///    scheitern würde (Muster: `harw-sentinel::main::run`,
///    `harw-probe-fs::main::run`).
/// 2. Alle vier eBPF-Objekte auflösen und einlesen
///    ([`sensors::resolve_procmon_objects`],
///    [`sensors::resolve_flow_objects`]) — **vor** Landlock, damit das
///    Objektverzeichnis nicht im Landlock-Ausschnitt stehen muss.
/// 3. Landlock durchsetzen (harter Abbruch bei jedem Ausgang außer
///    vollständiger Durchsetzung).
/// 4. Eine gemeinsame reale Ladeschicht bauen und je Sensor den
///    Vertragssatz laden ([`setup_sensor`]); fehlende Objekte oder ein
///    degradierender Ladefehler registrieren einen
///    [`sensors::UnavailableSensor`].
/// 5. Die Sammelschleife betreiben ([`collect_forever`]); endet sie, werden
///    alle geladenen Objekte entladen.
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`error::ProbeError::SentinelConnectFailed`] wenn der Sentinel nicht
/// erreichbar ist; [`error::ProbeError::LandlockUnavailable`] wenn Landlock
/// den Ausschnitt nicht vollständig durchsetzt;
/// [`error::ProbeError::BpfLoad`] wenn das Laden eines Sensors mit einem
/// nicht degradierenden Fehler scheitert (siehe [`degrades_sensor`]); jeder
/// dauerhafte Fehler der Sammelschleife.
fn run(cli: Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let sink = sink::build_sentinel_sink(sentinel_socket)?;
    tracing::info!(path = %sentinel_socket.display(), "connected to sentinel");

    // Objekte vor Landlock einlesen: danach braucht dieser Prozess keinen
    // Lesezugriff auf das Objektverzeichnis mehr.
    let procmon_objects = sensors::resolve_procmon_objects(
        cli.exec_program_path.as_deref(),
        cli.exit_program_path.as_deref(),
    );
    let flow_objects = sensors::resolve_flow_objects(
        cli.tcp_v4_program_path.as_deref(),
        cli.tcp_v6_program_path.as_deref(),
    );

    landlock::enforce_fs_scope(&fs_scope_roots())?;

    let loader = real_loader::build_real_loader()?;
    let mut degraded: Vec<Arc<dyn Sensor>> = Vec::new();
    let mut sources: Vec<WireSource> = Vec::new();

    setup_sensor(
        &loader,
        SensorId::from_str(cli.sensor_id_procmon.clone()),
        procmon_objects,
        |sensor, exec, exit| {
            harw_dod_procmon::procmon_contracts(sensor, exec, exit, BpfScope::Host)
        },
        None,
    )?
    .register(&mut degraded, &mut sources);

    let flow = setup_sensor(
        &loader,
        SensorId::from_str(cli.sensor_id_flow.clone()),
        flow_objects,
        |sensor, tcp_v4, tcp_v6| {
            harw_dod_flow::flow_contracts(sensor, tcp_v4, tcp_v6, BpfScope::Host)
        },
        Some(network_scope(&[])),
    );
    match flow {
        Ok(setup) => setup.register(&mut degraded, &mut sources),
        Err(err) => {
            // Bereits geladene Prozess-Objekte nicht angeheftet zurücklassen.
            collect::unload_all(&loader, &sources);
            return Err(err);
        }
    }

    tracing::info!(
        wire_sources = sources.len(),
        degraded_sensors = degraded.len(),
        "sensors registered"
    );

    let result = collect_forever(&loader, &degraded, &sources, sink.as_ref());
    collect::unload_all(&loader, &sources);
    tracing::info!("bpf objects unloaded after the collect loop ended");
    result
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
///   Vertragssatz, z. B. `harw_dod_procmon::procmon_contracts` mit
///   `BpfScope::Host`.
/// - `net_scope` (`Option<harw_authority::NetworkScope>`): Melderegel für
///   Verbindungsereignisse; `None` für den Prozess-Sensor.
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
/// Enthält ausschließlich die vorhandenen Verzeichnisse aus
/// [`KERNEL_INTERFACE_ROOTS`] — **nie** das eBPF-Objektverzeichnis: die
/// Objekte sind vor Landlock bereits eingelesen (siehe [`run`]). Fehlende
/// Verzeichnisse (z. B. `/sys/kernel/debug/tracing` ohne debugfs) werden
/// ausgelassen, statt bei jedem Start eine Warnung aus
/// `landlock::enforce_fs_scope` zu erzeugen; ausgelassen heißt dort ohnehin
/// unlesbar.
///
/// # Returns
/// Die vorhandenen Kernel-Schnittstellenverzeichnisse, in der Reihenfolge
/// von [`KERNEL_INTERFACE_ROOTS`].
fn fs_scope_roots() -> Vec<PathBuf> {
    KERNEL_INTERFACE_ROOTS
        .iter()
        .map(PathBuf::from)
        .filter(|root| root.is_dir())
        .collect()
}

/// Baut den `harw_authority::NetworkScope`, den die Verbindungs-Quelle
/// gegen jede beobachtete Verbindung prüft.
///
/// # Arguments
/// - `cidrs` (`&[ipnet::IpNet]`): erlaubte Zielnetze.
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

    use harw_dod_bpf::{BpfError, RealBpfLoader};
    use harw_dod_cap::SensorError;
    use harw_types::SensorId;
    use ipnet::IpNet;

    use super::{
        KERNEL_INTERFACE_ROOTS, SensorSetup, degrades_sensor, fs_scope_roots, network_scope,
        retry_transient, setup_sensor,
    };
    use crate::error::ProbeError;
    use crate::sensors::{
        BpfObjectKind, DEFAULT_BPF_OBJECT_DIR, ObjectUnavailable, ObjectUnavailableReason,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_fs_scope_roots_contains_only_kernel_interfaces_never_bpf_object_dirs() {
        let object_dir = Path::new(DEFAULT_BPF_OBJECT_DIR);
        for root in fs_scope_roots() {
            assert!(root.is_absolute());
            assert!(
                KERNEL_INTERFACE_ROOTS
                    .iter()
                    .any(|allowed| Path::new(allowed) == root)
            );
            assert!(!object_dir.starts_with(&root));
            assert!(!root.starts_with(object_dir));
        }
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

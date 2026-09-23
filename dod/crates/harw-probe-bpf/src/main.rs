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
use harw_dod_bpf::BpfProgramSource;
use harw_dod_signals::Sensor;
use harw_types::SensorId;

use cli::{Cli, LogLevel};
use error::ProbeError;

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das bei einem Fehler
/// oder `--help`/`--version` selbst `std::process::exit` aufriefe und diese
/// Funktion nie zu ihrem eigenen `ExitCode` zurückkehren ließe),
/// initialisiert `tracing` und delegiert an [`run`].
///
/// # Returns
/// `ExitCode::SUCCESS` nur, wenn [`run`] regulär zurückkehrt (heute
/// unerreichbar, siehe Moduldoku „Stand der eBPF-Bindung"). `ExitCode::FAILURE`
/// bei jedem Fehlerpfad — Kommandozeile, Landlock-Schranke, fehlender
/// eBPF-Loader, fehlender Sentinel.
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

/// Führt den eigentlichen Sondenbetrieb aus.
///
/// # Description
/// Reihenfolge: sich **zuerst** mit dem Sentinel verbinden (ein
/// Dateisystempfad-Zugriff, der nach einer Landlock-Regel scheitern könnte,
/// falls der Socket-Pfad nicht im gewährten Bereich liegt — Muster:
/// `harw-sentinel::main::run`, `harw-probe-fs::main::run`, die ihre eigenen
/// Sockets ebenfalls vor `restrict_self` binden bzw. verbinden), **dann**
/// über Landlock selbst einschränken (harter Abbruch bei jedem Ausgang
/// außer vollständiger Durchsetzung, siehe [`landlock`]-Moduldoku), **dann**
/// die reale eBPF-Ladeschicht aufbauen (siehe [`real_loader`]-Moduldoku:
/// das Konstruieren selbst greift weder auf den Kernel noch auf
/// Berechtigungen zu und schlägt deshalb nie fehl) und erst danach die
/// beiden Sensoren
/// registrieren und die Sammelschleife starten.
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`error::ProbeError::SentinelConnectFailed`] wenn der Sentinel nicht
/// erreichbar ist; [`error::ProbeError::LandlockUnavailable`] wenn Landlock
/// den Ausschnitt nicht vollständig durchsetzt;
/// [`error::ProbeError::BpfLoad`] wenn ein `load`-Aufruf auf dem realen
/// Ladeteil scheitert — etwa auf einem Host ohne `CAP_BPF` oder mit einem
/// Objekt, das dem in `RealBpfLoader`s Moduldoku beschriebenen Vertrag nicht
/// folgt.
fn run(cli: Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let sink = sink::build_sentinel_sink(sentinel_socket)?;
    tracing::info!(path = %sentinel_socket.display(), "connected to sentinel");

    let fs_roots = fs_scope_roots(&cli);
    landlock::enforce_fs_scope(&fs_roots)?;

    let procmon_source = program_source(cli.exec_program_path.clone());
    let procmon_loader = real_loader::build_real_loader()?;
    let procmon_sensor = sensors::build_procmon_sensor(
        procmon_loader,
        SensorId::from_str(cli.sensor_id_procmon.clone()),
        procmon_source,
    )?;

    let flow_source = program_source(cli.tcp_v4_program_path.clone());
    let flow_loader = real_loader::build_real_loader()?;
    let scope = network_scope(&[]);
    let flow_sensor = sensors::build_flow_sensor(
        flow_loader,
        SensorId::from_str(cli.sensor_id_flow.clone()),
        flow_source,
        scope,
    )?;

    let sensor_list: Vec<Arc<dyn Sensor>> = vec![Arc::new(procmon_sensor), Arc::new(flow_sensor)];
    tracing::info!(sensor_count = sensor_list.len(), "sensors registered");

    collect::run_forever(&sensor_list, sink.as_ref())
}

/// Baut die Landlock-Wurzelliste dieses Prozesses.
///
/// # Description
/// Beide Sensoren dieser Sonde lesen nie über einen `ReadScope` — ihre
/// einzige Quelle ist der injizierte `harw_dod_bpf::BpfLoader` (siehe
/// [`sensors`]-Moduldoku). Der einzige Dateisystemzugriff, den dieser
/// Prozess je braucht, sind die optionalen eBPF-Programmpfade; diese
/// Funktion sammelt deren Elternverzeichnisse. Ohne konfigurierte Pfade ist
/// das Ergebnis leer — die korrekte, maximal enge Voreinstellung.
///
/// # Arguments
/// - `cli` (`&cli::Cli`): die geparste Kommandozeile.
///
/// # Returns
/// Die Liste der Elternverzeichnisse konfigurierter Programmpfade, ohne
/// Duplikate zu entfernen (Landlock verkraftet doppelte Regeln
/// unproblematisch).
fn fs_scope_roots(cli: &Cli) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in [
        &cli.exec_program_path,
        &cli.exit_program_path,
        &cli.tcp_v4_program_path,
        &cli.tcp_v6_program_path,
    ]
    .into_iter()
    .flatten()
    {
        if let Some(parent) = path.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    roots
}

/// Baut die `harw_dod_bpf::BpfProgramSource` für einen optionalen
/// Programmpfad.
///
/// # Arguments
/// - `path` (`Option<std::path::PathBuf>`): der über `--procmon-program-path`
///   bzw. `--flow-program-path` konfigurierte Pfad.
///
/// # Returns
/// [`harw_dod_bpf::BpfProgramSource::Path`], wenn `path` gesetzt ist, sonst
/// ein leerer, eingebetteter Platzhalterrumpf
/// ([`harw_dod_bpf::BpfProgramSource::Embedded`]) — siehe
/// [`real_loader`]-Moduldoku für die Begründung, warum in dieser Lieferung
/// ohnehin kein realer Lader existiert, der einen Rumpf tatsächlich
/// verwenden würde.
fn program_source(path: Option<PathBuf>) -> BpfProgramSource {
    match path {
        Some(path) => BpfProgramSource::Path(path),
        None => BpfProgramSource::Embedded(std::borrow::Cow::Borrowed(&[])),
    }
}

/// Baut den `harw_authority::NetworkScope`, den [`sensors::FlowSensor`] gegen
/// jede beobachtete Verbindung prüft.
///
/// # Arguments
/// - `cli` (`&cli::Cli`): die geparste Kommandozeile.
///
/// # Returns
/// Einen `NetworkScope` aus den über `--egress-allow-cidr` konfigurierten
/// Netzen. Ohne Angabe ein leerer Scope, der jede ausgehende Verbindung
/// meldet — siehe [`cli`]-Moduldoku für die Begründung, warum
/// `Host`/`DnsSuffix`-Ziele hier nicht angeboten werden.
fn network_scope(cidrs: &[ipnet::IpNet]) -> NetworkScope {
    let targets: Vec<EgressTarget> = cidrs.iter().cloned().map(EgressTarget::Cidr).collect();
    NetworkScope::from_targets(targets)
}

#[cfg(test)]
mod tests {
    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten Socket verbinden und ein echtes Landlock-Ruleset binden
    // — beides nach Aufgabenstellung untersagt. Jede darin verkettete
    // Teilfunktion ist einzeln geprüft: `cli::Cli::try_parse_from` in
    // `cli.rs`, `sensors::build_procmon_sensor`/`build_flow_sensor` und
    // `FlowSensor::poll` in `sensors.rs`, `collect::run_once`/`run_forever`
    // in `collect.rs` (mit `FixtureBpfLoader`), `real_loader::build_real_loader`
    // in `real_loader.rs`. Zusammen decken sie jeden Schritt von `run` ab,
    // ohne dass ein Test dieser Crate einen echten Socket öffnet, ein
    // echtes Landlock bindet oder echtes eBPF lädt.

    use super::{fs_scope_roots, network_scope, program_source};
    use crate::test_support::{TestResult, ctx};
    use harw_dod_bpf::BpfProgramSource;
    use ipnet::IpNet;
    use std::path::PathBuf;

    fn minimal_cli() -> TestResult<super::Cli> {
        use clap::Parser as _;
        super::Cli::try_parse_from([
            "harw-probe-bpf",
            "--sentinel-socket",
            "/run/harw-sentinel.sock",
        ])
        .map_err(ctx("minimal valid arguments"))
    }

    #[test]
    fn test_program_source_defaults_to_embedded_placeholder_when_no_path_given() {
        assert!(matches!(
            program_source(None),
            BpfProgramSource::Embedded(_)
        ));
    }

    #[test]
    fn test_program_source_uses_path_when_given() {
        let path = PathBuf::from("/opt/harw/procmon.bpf.o");
        assert!(matches!(
            program_source(Some(path)),
            BpfProgramSource::Path(_)
        ));
    }

    #[test]
    fn test_fs_scope_roots_is_empty_without_configured_program_paths() -> TestResult {
        let cli = minimal_cli()?;
        assert!(fs_scope_roots(&cli).is_empty());
        Ok(())
    }

    #[test]
    fn test_fs_scope_roots_collects_parent_directories_of_configured_paths() -> TestResult {
        let mut cli = minimal_cli()?;
        cli.exec_program_path = Some(PathBuf::from("/opt/harw/bpf/exec.o"));
        cli.tcp_v4_program_path = Some(PathBuf::from("/opt/harw/bpf/flow.o"));

        let roots = fs_scope_roots(&cli);
        assert_eq!(
            roots,
            vec![
                PathBuf::from("/opt/harw/bpf"),
                PathBuf::from("/opt/harw/bpf")
            ]
        );
        Ok(())
    }

    #[test]
    fn test_network_scope_is_empty_without_configured_cidrs() -> TestResult {
        let _cli = minimal_cli()?;
        let scope = network_scope(&[]);
        assert!(!scope.allows_addr(std::net::IpAddr::from([127, 0, 0, 1])));
        Ok(())
    }

    #[test]
    fn test_network_scope_allows_a_configured_cidr() -> TestResult {
        let _cli = minimal_cli()?;
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

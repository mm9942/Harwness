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
//! `BpfProgramSource::Embedded` im Speicher) → **jedes eingelesene Objekt
//! gegen das root-eigene SHA-256-Manifest prüfen und jedes gewählte, aber
//! nicht auflösbare Objekt ablehnen**
//! ([`bpf_manifest::verify_resolved_objects`], `--bpf-manifest`,
//! [`bpf_manifest::ensure_objects_resolved`]) → den Scope
//! des Profils auflösen ([`profile::resolve_scope`], liest den cgroup-Baum)
//! → Landlock durchsetzen → laden und anheften. Das Objektverzeichnis
//! (`--*-program-path`, `$HARW_DOD_BPF_DIR`, `/usr/local/lib/harw-dod/bpf`),
//! das Manifest und die Konfigurationsdatei sind damit **nicht** Teil des
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
//!   `harw-dod/config.toml` wählt genau dieses Profil mit beiden Sensoren).
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
//!   (`bpf_get_current_ancestor_cgroup_id` unter `harw-dod/bpf/`, ABI-Bump): ein
//!   eigener Folgeschritt; er schlösse das Fenster von bis zu einem
//!   Rescan-Intervall, in dem eine brandneue cgroup noch nicht beobachtet
//!   wird.
//!
//! Degradierung statt Prozessende, je Sensor unabhängig:
//! - scheitert das Laden mit `AttachCapabilitiesUnavailable`,
//!   `CapabilityUnavailable` oder `InvalidProgramContract` (siehe
//!   [`degrades_sensor`]), registriert [`run`] einen
//!   [`sensors::UnavailableSensor`] — ein Host ohne die nötigen Fähigkeiten
//!   oder mit einem nicht vertragsgemäßen Objekt ist ein dokumentierter
//!   Betriebsfall, den der Sentinel als `SensorDegraded` sieht;
//! - **nur im Entwicklungsmodus** `--dev-allow-missing-bpf` (verlangt
//!   `--bpf-manifest`, steht nie in der systemd-Unit) ebenso ein gewählter
//!   Sensor, dessen Objekt fehlt oder ungültig ist (kein ELF, leer, zu
//!   groß), mit der Mängelliste ([`UnresolvedObjects::Degrade`]). Jedes
//!   aufgelöste Objekt wird auch dann gegen das Manifest geprüft: ein
//!   Teilbau (z. B. nur `exec.bpf.o`/`exit.bpf.o`) lädt seine vollständigen
//!   Sensoren, aber nie ungeprüfte Bytes;
//! - jeder andere Ladefehler beendet den Prozess; bereits geladene Griffe
//!   werden vorher entladen.
//!
//! Ausdrücklich **keine** Degradierung, sondern Prozessende vor jedem
//! Ladeschritt ([`error::ProbeError::BpfObjectIntegrity`], PL-100 Phase 4,
//! THREAT-MODEL B23, [`verify_selected_objects`]):
//! - ein vorhandenes Objekt, das die Manifestprüfung nicht besteht (kein
//!   `--bpf-manifest`, Manifest nicht root-eigen, schreibbar, ungültig oder
//!   ohne Eintrag, abweichender SHA-256-Digest);
//! - ein gesetztes `--bpf-manifest`, das fehlt, nicht vertrauenswürdig oder
//!   ungültig ist, auch wenn kein einziges Objekt aufgelöst ist;
//! - außerhalb des Entwicklungsmodus ein gewähltes Objekt, das sich nicht
//!   auflösen lässt (entfernt, leer, gekürzt, kein ELF, unlesbar): es
//!   liefert keine prüfbaren Bytes, und ein still degradierter Sensor ließe
//!   die Unit aktiv, aber diese Erkennung blind (Owner-Review zu PR #168).
//!
//! [`run`] meldet vorher jeden gewählten Sensor einmal als `SensorDegraded`
//! ([`report_integrity_refusal`]); `main` endet mit dem eigenen Exit-Status
//! [`EXIT_INTEGRITY_REFUSED`] (78, `EX_CONFIG`), den
//! `deploy/systemd/harw-probe-bpf.service` über `RestartPreventExitStatus=78`
//! vom Neustart ausnimmt: eine Ablehnung ist dauerhaft, kein vorübergehender
//! Absturz.
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

mod bpf_manifest;
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

use std::io::Write as _;
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
use sensors::{ObjectUnavailable, ResolvedBpfObject, UnavailableSensor, UnresolvedPair};
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
/// sonst nur über einen Fehler). [`EXIT_INTEGRITY_REFUSED`] (78), wenn die
/// Manifestprüfung der eBPF-Objekte den Start ablehnt
/// ([`exit_status_for`]). `ExitCode::FAILURE` bei jedem anderen Fehlerpfad —
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
            // A failed stderr write has no channel left to report it on.
            let _ = write!(std::io::stderr(), "{err}");
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
                // A failed stderr write has no channel left to report it on.
                let _ = writeln!(std::io::stderr(), "harw-probe-bpf: {err}");
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
        // A failed stderr write has no channel left to report it on.
        let _ = write!(std::io::stderr(), "{err}");
        return ExitCode::FAILURE;
    };

    match run(cli, &sentinel_socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let status = exit_status_for(&err);
            tracing::error!(error = %err, exit_status = status, "harw-probe-bpf exiting");
            // A failed stderr write has no channel left to report it on.
            let _ = writeln!(std::io::stderr(), "harw-probe-bpf: {err}");
            ExitCode::from(status)
        }
    }
}

/// Exit-Status, wenn die Manifestprüfung der eBPF-Objekte den Start ablehnt:
/// **78** (`EX_CONFIG` aus `sysexits.h`).
///
/// # Description
/// Eine Ablehnung durch [`bpf_manifest::verify_resolved_objects`] oder
/// [`bpf_manifest::ensure_objects_resolved`]
/// ([`ProbeError::BpfObjectIntegrity`]) ist dauerhaft: dasselbe Manifest und
/// dieselben (bzw. fehlenden) Objekte scheitern beim nächsten Start genauso.
/// Der eigene
/// Status erlaubt der systemd-Unit `RestartPreventExitStatus=78`, statt den
/// Start bis `StartLimitBurst` zu wiederholen und dabei jedes Mal erneut
/// `SensorDegraded` zu senden.
const EXIT_INTEGRITY_REFUSED: u8 = 78;

/// Exit-Status jedes anderen Fehlers aus [`run`]: **1**, derselbe Wert wie
/// `ExitCode::FAILURE` unter Linux.
const EXIT_RUN_FAILED: u8 = 1;

/// Der Exit-Status für einen Fehler aus [`run`].
///
/// # Arguments
/// - `err` (`&ProbeError`): der Fehler, mit dem [`run`] endete.
///
/// # Returns
/// [`EXIT_INTEGRITY_REFUSED`] für [`ProbeError::BpfObjectIntegrity`],
/// sonst [`EXIT_RUN_FAILED`].
fn exit_status_for(err: &ProbeError) -> u8 {
    if matches!(err, ProbeError::BpfObjectIntegrity { .. }) {
        EXIT_INTEGRITY_REFUSED
    } else {
        EXIT_RUN_FAILED
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
/// 4. Jedes eingelesene Objekt gegen das SHA-256-Manifest prüfen
///    ([`bpf_manifest::verify_resolved_objects`] mit `--bpf-manifest`) —
///    ebenfalls vor Landlock und an genau den Bytes, die später geladen
///    werden, auch das verwendbare Geschwister eines nicht aufgelösten
///    Objekts — und außerhalb von `--dev-allow-missing-bpf` jedes gewählte,
///    aber nicht aufgelöste Objekt ablehnen ([`verify_selected_objects`];
///    Schritte 3 und 4 zusammen: [`resolve_and_verify_objects`]). Bei einem
///    Verstoß geht je gewähltem Sensor ein `SensorDegraded` an den Sentinel
///    ([`report_integrity_refusal`]), und der Start endet ohne einen
///    einzigen Ladeschritt.
/// 5. Den Scope des Profils auflösen ([`resolve_profile_scope`]) — **vor**
///    Landlock, weil Wurzelauflösung und Nachfahren-Expansion den
///    cgroup-Baum lesen.
/// 6. Landlock durchsetzen (harter Abbruch bei jedem Ausgang außer
///    vollständiger Durchsetzung), mit den Wurzeln aus [`fs_scope_roots`].
/// 7. Eine gemeinsame reale Ladeschicht bauen und je gewähltem Sensor den
///    Vertragssatz mit dem Profil-Scope laden ([`setup_sensor`]); ein
///    degradierender Ladefehler (und nur im Entwicklungsmodus ein fehlendes
///    Objekt) registriert einen [`sensors::UnavailableSensor`]. War der
///    Scope schon beim Start
///    unvollständig, geht genau ein `SensorDegraded` je geladenem Sensor an
///    den Sentinel.
/// 8. Die Sammelschleife betreiben ([`collect_forever`], mit Rescan nur für
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
/// [`error::ProbeError::BpfObjectIntegrity`], wenn ein eingelesenes Objekt
/// oder das konfigurierte Manifest die Prüfung nicht besteht oder (außerhalb
/// von `--dev-allow-missing-bpf`) ein gewähltes Objekt nicht auflösbar ist;
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

    // Objekte vor Landlock einlesen (danach braucht dieser Prozess keinen
    // Lesezugriff auf das Objektverzeichnis mehr) und vor jedem Ladeschritt
    // prüfen (PL-100 Phase 4, THREAT-MODEL B23): jedes eingelesene Objekt
    // muss zu seinem Eintrag im root-eigenen SHA-256-Manifest passen, und
    // außerhalb des Entwicklungsmodus muss jedes gewählte Objekt auflösbar
    // sein. Ein Verstoß beendet den Start; vorher erfährt der Sentinel, dass
    // die gewählten Sensoren dieser Sonde blind sind.
    let SelectedObjects {
        procmon: procmon_objects,
        flow: flow_objects,
    } = resolve_and_verify_objects(
        &cli,
        selection,
        sink.as_ref(),
        Timestamp::now(),
        bpf_manifest::verify_resolved_objects,
    )?;

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

/// Ergebnis der Objektauflösung eines Sensors: beide Objekte oder jeder
/// Mangel samt verwendbarem Geschwisterobjekt (siehe
/// [`sensors::resolve_procmon_objects`], [`sensors::UnresolvedPair`]).
type SensorObjects = Result<[ResolvedBpfObject; 2], UnresolvedPair>;

/// Was mit einem gewählten Sensor geschieht, dessen Objekte sich nicht
/// auflösen lassen (entfernt, leer, gekürzt, kein ELF, unlesbar).
///
/// # Description
/// Owner-Review zu PR #168: ein solches Objekt darf die Integritätsprüfung
/// nicht umgehen. Produktiv ist es deshalb ein Startabbruch mit Exit-Status
/// 78; nur der ausdrückliche Entwicklungsmodus `--dev-allow-missing-bpf`
/// (den `clap` nur zusammen mit `--bpf-manifest` annimmt) behält die frühere
/// Degradierung des Sensors. Er lockert nur diese eine Regel: jedes
/// aufgelöste Objekt wird in beiden Fällen gegen das Manifest geprüft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnresolvedObjects {
    /// Produktion: [`bpf_manifest::ensure_objects_resolved`] lehnt den Start
    /// ab ([`bpf_manifest::IntegrityFailure::ObjectUnresolved`]).
    Refuse,
    /// Nur Entwicklung: der Sensor läuft als [`sensors::UnavailableSensor`]
    /// weiter ([`setup_sensor`]) und meldet einmal `SensorDegraded`.
    Degrade,
}

impl UnresolvedObjects {
    /// Die Behandlung für `--dev-allow-missing-bpf` und `--bpf-manifest`.
    ///
    /// # Description
    /// `clap` nimmt den Entwicklungsmodus nur zusammen mit `--bpf-manifest`
    /// an (`requires`); diese Funktion verlangt dasselbe noch einmal, damit
    /// ein Aufruf an `clap` vorbei nie ohne Manifest degradiert.
    ///
    /// # Arguments
    /// - `dev_allow_missing_bpf` (`bool`): ob der Entwicklungsmodus gesetzt
    ///   ist.
    /// - `manifest` (`Option<&Path>`): `--bpf-manifest`.
    ///
    /// # Returns
    /// [`Self::Degrade`] nur mit dem Entwicklungsmodus **und** einem
    /// Manifest, sonst [`Self::Refuse`].
    fn for_flags(dev_allow_missing_bpf: bool, manifest: Option<&Path>) -> Self {
        if dev_allow_missing_bpf && manifest.is_some() {
            Self::Degrade
        } else {
            Self::Refuse
        }
    }
}

/// Die aufgelösten Objekte der gewählten Sensoren nach bestandener
/// Integritätsprüfung ([`resolve_and_verify_objects`]).
struct SelectedObjects {
    /// Prozess-Sensor (`exec.bpf.o`, `exit.bpf.o`); `None`, wenn nicht
    /// gewählt. `Some(Err(_))` nur im Entwicklungsmodus.
    procmon: Option<SensorObjects>,
    /// Verbindungs-Sensor (`tcp_v4_connect.bpf.o`, `tcp_v6_connect.bpf.o`);
    /// `None`, wenn nicht gewählt. `Some(Err(_))` nur im Entwicklungsmodus.
    flow: Option<SensorObjects>,
}

/// Löst die Objekte der gewählten Sensoren auf und prüft sie vor Landlock
/// und vor jedem Ladeschritt (Schritte 3 und 4 aus [`run`]).
///
/// # Description
/// Je gewähltem Sensor [`sensors::resolve_procmon_objects`] bzw.
/// [`sensors::resolve_flow_objects`] mit den Pfaden aus `cli` (ein nicht
/// gewählter Sensor wird gar nicht erst aufgelöst), danach
/// [`verify_selected_objects`] mit `--bpf-manifest` und
/// `--dev-allow-missing-bpf`. Scheitert die Prüfung, geht vorher je
/// gewähltem Sensor genau ein `SensorDegraded` an den Sentinel
/// ([`report_integrity_refusal`]); [`run`] gibt den Fehler dann unverändert
/// zurück, `main` endet mit [`EXIT_INTEGRITY_REFUSED`].
///
/// # Arguments
/// - `cli` (`&Cli`): die geparste Kommandozeile.
/// - `selection` (`SensorSelection`): Sensorwahl des aktiven Profils.
/// - `sink` (`&dyn EventSink`): die Senke zum Sentinel.
/// - `now` (`Timestamp`): `observed_at` der Ablehnungsmeldungen.
/// - `verify`: die Manifestprüfung; produktiv
///   [`bpf_manifest::verify_resolved_objects`] (Anker `/`, Eigentümer
///   root), in Tests dieselbe Prüfung gegen ein Fixture-Manifest.
///
/// # Returns
/// Die aufgelösten Objekte beider Sensoren.
///
/// # Errors
/// [`ProbeError::BpfObjectIntegrity`] aus [`verify_selected_objects`].
///
/// # Concurrency
/// Blockierende Dateisystemzugriffe; nur beim Start im Hauptthread.
fn resolve_and_verify_objects(
    cli: &Cli,
    selection: SensorSelection,
    sink: &dyn EventSink,
    now: Timestamp,
    verify: impl FnOnce(Option<&Path>, &[&ResolvedBpfObject]) -> Result<usize, ProbeError>,
) -> Result<SelectedObjects, ProbeError> {
    let procmon = selection.exec.then(|| {
        sensors::resolve_procmon_objects(
            cli.exec_program_path.as_deref(),
            cli.exit_program_path.as_deref(),
        )
    });
    let flow = selection.tcp_connect.then(|| {
        sensors::resolve_flow_objects(
            cli.tcp_v4_program_path.as_deref(),
            cli.tcp_v6_program_path.as_deref(),
        )
    });
    let verified = verify_selected_objects(
        procmon.as_ref(),
        flow.as_ref(),
        cli.bpf_manifest.as_deref(),
        UnresolvedObjects::for_flags(cli.dev_allow_missing_bpf, cli.bpf_manifest.as_deref()),
        verify,
    );
    if let Err(err) = verified {
        let selected = selected_sensor_ids(selection, &cli.sensor_id_procmon, &cli.sensor_id_flow);
        let reported = report_integrity_refusal(&selected, sink, now);
        tracing::info!(
            reported_sensors = reported,
            "selected bpf sensors reported as degraded before refusing to load"
        );
        return Err(err);
    }
    Ok(SelectedObjects { procmon, flow })
}

/// Die Integritätsschranke über die Auflösung aller gewählten Sensoren.
///
/// # Description
/// Zuerst `verify` ([`bpf_manifest::verify_resolved_objects`]) über jedes
/// eingelesene Objekt ([`objects_to_verify`]), auch über das verwendbare
/// Geschwister eines nicht aufgelösten Objekts: ist `manifest` gesetzt, wird
/// das Manifest dabei immer gelesen und geprüft, auch wenn kein Objekt
/// aufgelöst ist, und ein manipuliertes Objekt wird immer vor einem
/// fehlenden gemeldet, über Sensoren hinweg wie innerhalb eines Sensors.
/// Danach die gewählten, aber nicht aufgelösten Objekte:
/// - [`UnresolvedObjects::Refuse`] (Produktion):
///   [`bpf_manifest::ensure_objects_resolved`] lehnt den Start ab;
/// - [`UnresolvedObjects::Degrade`] (nur `--dev-allow-missing-bpf` mit
///   `--bpf-manifest`): kein Abbruch, der Sensor degradiert später in
///   [`setup_sensor`]. Vor der Prüfung meldet genau eine Warnung, dass der
///   Entwicklungsmodus aktiv ist (mit der Zahl der nicht aufgelösten
///   Objekte, ohne Pfad). Die Manifestprüfung läuft unverändert: auch der
///   Entwicklungsmodus lädt nie ungeprüfte Bytes.
///
/// # Arguments
/// - `procmon` (`Option<&SensorObjects>`): Auflösung des Prozess-Sensors,
///   `None`, wenn nicht gewählt.
/// - `flow` (`Option<&SensorObjects>`): Auflösung des Verbindungs-Sensors,
///   `None`, wenn nicht gewählt.
/// - `manifest` (`Option<&Path>`): `--bpf-manifest`.
/// - `unresolved` ([`UnresolvedObjects`]): Behandlung nicht aufgelöster
///   Objekte.
/// - `verify`: die Manifestprüfung (siehe [`resolve_and_verify_objects`]).
///
/// # Returns
/// Die Zahl der gegen das Manifest geprüften Objekte.
///
/// # Errors
/// [`ProbeError::BpfObjectIntegrity`]: der erste Verstoß.
fn verify_selected_objects(
    procmon: Option<&SensorObjects>,
    flow: Option<&SensorObjects>,
    manifest: Option<&Path>,
    unresolved: UnresolvedObjects,
    verify: impl FnOnce(Option<&Path>, &[&ResolvedBpfObject]) -> Result<usize, ProbeError>,
) -> Result<usize, ProbeError> {
    let missing = unresolved_objects(procmon, flow);
    if unresolved == UnresolvedObjects::Degrade {
        tracing::warn!(
            unresolved_objects = missing.len(),
            "development mode (--dev-allow-missing-bpf) is active: selected bpf sensors with unresolved objects degrade instead of refusing to start; resolved objects are still verified against the manifest; never use this mode in production"
        );
    }
    let resolved = objects_to_verify(procmon, flow);
    let verified = verify(manifest, resolved.as_slice())?;
    if unresolved == UnresolvedObjects::Refuse {
        bpf_manifest::ensure_objects_resolved(missing.as_slice())?;
    }
    Ok(verified)
}

/// Jedes eingelesene Objekt aller gewählten Sensoren, für die
/// Manifestprüfung.
///
/// # Description
/// Ein vom Profil nicht gewählter Sensor (`None`) trägt nichts bei; er wird
/// weder aufgelöst noch geladen. Von einem gewählten Sensor zählen beide
/// Objekte bzw., wenn eines nicht aufgelöst ist, das verwendbare
/// Geschwister ([`sensors::UnresolvedPair::resolved`]): es wird nie geladen,
/// aber seine Bytes liegen im Speicher und werden geprüft, damit eine
/// Manipulation nicht hinter einem fehlenden Objekt verborgen bleibt. Nicht
/// aufgelöste Objekte selbst liefern keine Bytes; sie prüft
/// [`verify_selected_objects`] getrennt über [`unresolved_objects`].
///
/// # Arguments
/// - `procmon` (`Option<&SensorObjects>`): Auflösung des Prozess-Sensors.
/// - `flow` (`Option<&SensorObjects>`): Auflösung des Verbindungs-Sensors.
///
/// # Returns
/// Die Objekte in der Reihenfolge exec, exit, tcp_v4, tcp_v6 (soweit
/// eingelesen).
fn objects_to_verify<'a>(
    procmon: Option<&'a SensorObjects>,
    flow: Option<&'a SensorObjects>,
) -> Vec<&'a ResolvedBpfObject> {
    let mut objects: Vec<&'a ResolvedBpfObject> = Vec::with_capacity(4);
    for resolution in [procmon, flow].into_iter().flatten() {
        match resolution {
            Ok(pair) => objects.extend(pair.iter()),
            Err(unresolved) => objects.extend(unresolved.resolved.as_ref()),
        }
    }
    objects
}

/// Die Mängel aller gewählten Sensoren, deren Objekte nicht aufgelöst sind.
///
/// # Arguments
/// - `procmon` (`Option<&SensorObjects>`): Auflösung des Prozess-Sensors.
/// - `flow` (`Option<&SensorObjects>`): Auflösung des Verbindungs-Sensors.
///
/// # Returns
/// Jeder Mangel, Prozess- vor Verbindungs-Sensor; leer, wenn jeder gewählte
/// Sensor vollständig aufgelöst ist.
fn unresolved_objects<'a>(
    procmon: Option<&'a SensorObjects>,
    flow: Option<&'a SensorObjects>,
) -> Vec<&'a ObjectUnavailable> {
    [procmon, flow]
        .into_iter()
        .flatten()
        .filter_map(|objects| objects.as_ref().err())
        .flat_map(|unresolved| unresolved.missing.iter())
        .collect()
}

/// Die Kennungen der vom Profil gewählten Sensoren.
///
/// # Arguments
/// - `selection` (`SensorSelection`): Sensorwahl des aktiven Profils.
/// - `procmon` (`&str`): `--sensor-id-procmon`.
/// - `flow` (`&str`): `--sensor-id-flow`.
///
/// # Returns
/// Prozess-Sensor vor Verbindungs-Sensor, jeweils nur wenn gewählt.
fn selected_sensor_ids(selection: SensorSelection, procmon: &str, flow: &str) -> Vec<SensorId> {
    let mut sensors = Vec::with_capacity(2);
    if selection.exec {
        sensors.push(SensorId::from_str(procmon));
    }
    if selection.tcp_connect {
        sensors.push(SensorId::from_str(flow));
    }
    sensors
}

/// Meldet die gewählten Sensoren vor dem Abbruch wegen der Manifestprüfung
/// als degradiert.
///
/// # Description
/// Genau ein `EventKind::SensorDegraded` je verschiedener Kennung
/// ([`collect::sensor_degraded_events`]). Best effort: der Abbruch selbst
/// steht fest, ein Sendefehler wird deshalb nur auf `warn` protokolliert
/// (Kennung und Fehler, kein Pfad) und hält die übrigen Meldungen nicht auf.
///
/// # Arguments
/// - `sensors` (`&[SensorId]`): die gewählten Sensoren.
/// - `sink` (`&dyn EventSink`): die Senke zum Sentinel.
/// - `now` (`Timestamp`): `observed_at` der Meldungen.
///
/// # Returns
/// Die Zahl der erfolgreich gesendeten Meldungen.
fn report_integrity_refusal(sensors: &[SensorId], sink: &dyn EventSink, now: Timestamp) -> usize {
    let affected: Vec<&SensorId> = sensors.iter().collect();
    let mut sent = 0_usize;
    for event in collect::sensor_degraded_events(&affected, now) {
        match sink.send(&event) {
            Ok(()) => sent = sent.saturating_add(1),
            Err(err) => tracing::warn!(
                sensor = %event.sensor,
                error = %err,
                "could not report SensorDegraded before refusing to load bpf objects"
            ),
        }
    }
    sent
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
/// Fehlen Objekte (erreichbar nur im Entwicklungsmodus
/// `--dev-allow-missing-bpf`; produktiv hat [`verify_selected_objects`] den
/// Start dann schon abgelehnt), entsteht ohne jeden Ladeversuch ein
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
/// - `objects` ([`SensorObjects`]): das Ergebnis von
///   [`sensors::resolve_procmon_objects`] bzw.
///   [`sensors::resolve_flow_objects`]; bei `Err` wird nur
///   [`sensors::UnresolvedPair::missing`] verwendet, das Geschwister nie
///   geladen.
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
    objects: SensorObjects,
    contracts: impl FnOnce(SensorId, BpfProgramSource, BpfProgramSource) -> [BpfObjectContract; 2],
    net_scope: Option<NetworkScope>,
) -> Result<SensorSetup, ProbeError> {
    let [first, second] = match objects {
        Ok(objects) => objects,
        Err(unresolved) => {
            return Ok(SensorSetup::Degraded(UnavailableSensor::new(
                sensor,
                unresolved.missing,
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

    use std::borrow::Cow;
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    use clap::Parser as _;
    use harw_dod_bpf::cgroup_walk::{HostCgroupFs, WalkLimits};
    use harw_dod_bpf::{BpfError, BpfProgramSource, RealBpfLoader};
    use harw_dod_cap::SensorError;
    use harw_dod_signals::EventKind;
    use harw_types::SensorId;
    use ipnet::IpNet;
    use jiff::Timestamp;
    use landlock::{ABI, AccessFs};

    use super::{
        EXIT_INTEGRITY_REFUSED, EXIT_RUN_FAILED, KERNEL_INTERFACE_ROOTS, SelectedObjects,
        SensorObjects, SensorSetup, UnresolvedObjects, degrades_sensor, exit_status_for,
        fs_scope_roots, landlock_roots, network_scope, objects_to_verify, report_integrity_refusal,
        resolve_and_verify_objects, retry_transient, selected_sensor_ids, setup_sensor,
        unresolved_objects, verify_selected_objects,
    };
    use crate::bpf_manifest::IntegrityFailure;
    use crate::bpf_manifest::test_fixture::{Fixture, object_bytes};
    use crate::cli::Cli;
    use crate::error::ProbeError;
    use crate::landlock::{FsRoot, RootAccess};
    use crate::profile::test_tree::{FakeTree, cgroup_scope};
    use crate::profile::{ProbeScope, SensorSelection, resolve_scope};
    use crate::sensors::{
        BpfObjectKind, DEFAULT_BPF_OBJECT_DIR, ObjectUnavailable, ObjectUnavailableReason,
        ResolvedBpfObject, UnresolvedPair,
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

    /// Test-Senke, deren Senden immer scheitert.
    struct FailingSink;

    impl crate::sink::EventSink for FailingSink {
        fn send(&self, _event: &harw_dod_signals::SecurityEvent) -> Result<(), ProbeError> {
            Err(ProbeError::SentinelSendFailed)
        }
    }

    /// Ein aufgelöstes Fixture-Objekt (ELF-Kennung, keine Datei).
    fn resolved(object: BpfObjectKind) -> ResolvedBpfObject {
        ResolvedBpfObject {
            object,
            path: Path::new(DEFAULT_BPF_OBJECT_DIR).join(object.file_name()),
            source: BpfProgramSource::Embedded(Cow::Owned(vec![0x7f, b'E', b'L', b'F'])),
        }
    }

    /// Ein nicht aufgelöster Sensor: seine Mängel und, falls vorhanden, das
    /// verwendbare Geschwisterobjekt.
    fn unresolved_pair(
        resolved: Option<ResolvedBpfObject>,
        missing: Vec<ObjectUnavailable>,
    ) -> SensorObjects {
        Err(UnresolvedPair { resolved, missing })
    }

    #[test]
    fn test_objects_to_verify_takes_every_read_object_of_the_selected_sensors() {
        let procmon: SensorObjects =
            Ok([resolved(BpfObjectKind::Exec), resolved(BpfObjectKind::Exit)]);
        let flow_missing = unresolved_pair(
            Some(resolved(BpfObjectKind::TcpV6Connect)),
            vec![ObjectUnavailable {
                object: BpfObjectKind::TcpV4Connect,
                path: PathBuf::from("/nonexistent/tcp_v4_connect.bpf.o"),
                reason: ObjectUnavailableReason::Missing,
            }],
        );
        let flow_gone = unresolved_pair(
            None,
            vec![
                unavailable(
                    BpfObjectKind::TcpV4Connect,
                    ObjectUnavailableReason::Missing,
                ),
                unavailable(
                    BpfObjectKind::TcpV6Connect,
                    ObjectUnavailableReason::Missing,
                ),
            ],
        );
        let flow: SensorObjects = Ok([
            resolved(BpfObjectKind::TcpV4Connect),
            resolved(BpfObjectKind::TcpV6Connect),
        ]);

        let kinds = |objects: Vec<&ResolvedBpfObject>| -> Vec<BpfObjectKind> {
            objects.iter().map(|object| object.object).collect()
        };
        assert_eq!(
            kinds(objects_to_verify(Some(&procmon), Some(&flow))),
            vec![
                BpfObjectKind::Exec,
                BpfObjectKind::Exit,
                BpfObjectKind::TcpV4Connect,
                BpfObjectKind::TcpV6Connect,
            ]
        );
        // Nicht aufgelöste Objekte liefern keine Bytes (sie prüft
        // `verify_selected_objects` getrennt über `unresolved_objects`), ihr
        // eingelesenes Geschwister wird aber mitgeprüft.
        assert_eq!(
            kinds(objects_to_verify(Some(&procmon), Some(&flow_missing))),
            vec![
                BpfObjectKind::Exec,
                BpfObjectKind::Exit,
                BpfObjectKind::TcpV6Connect,
            ]
        );
        assert_eq!(
            kinds(objects_to_verify(Some(&procmon), Some(&flow_gone))),
            vec![BpfObjectKind::Exec, BpfObjectKind::Exit]
        );
        // Nicht gewählte Sensoren tragen nichts bei.
        assert!(objects_to_verify(None, Some(&flow_gone)).is_empty());
        assert!(objects_to_verify(None, None).is_empty());
    }

    /// Ein nicht aufgelöstes Objekt mit Fixture-Pfad.
    fn unavailable(object: BpfObjectKind, reason: ObjectUnavailableReason) -> ObjectUnavailable {
        ObjectUnavailable {
            object,
            path: Path::new(DEFAULT_BPF_OBJECT_DIR).join(object.file_name()),
            reason,
        }
    }

    #[test]
    fn test_unresolved_objects_collects_every_defect_of_the_selected_sensors() {
        let procmon_missing = unresolved_pair(
            None,
            vec![
                unavailable(BpfObjectKind::Exec, ObjectUnavailableReason::Missing),
                unavailable(BpfObjectKind::Exit, ObjectUnavailableReason::Empty),
            ],
        );
        let flow_corrupt = unresolved_pair(
            Some(resolved(BpfObjectKind::TcpV4Connect)),
            vec![unavailable(
                BpfObjectKind::TcpV6Connect,
                ObjectUnavailableReason::NotElf,
            )],
        );
        let flow: SensorObjects = Ok([
            resolved(BpfObjectKind::TcpV4Connect),
            resolved(BpfObjectKind::TcpV6Connect),
        ]);

        let defects =
            |missing: Vec<&ObjectUnavailable>| -> Vec<(BpfObjectKind, ObjectUnavailableReason)> {
                missing
                    .iter()
                    .map(|item| (item.object, item.reason))
                    .collect()
            };
        assert_eq!(
            defects(unresolved_objects(
                Some(&procmon_missing),
                Some(&flow_corrupt)
            )),
            vec![
                (BpfObjectKind::Exec, ObjectUnavailableReason::Missing),
                (BpfObjectKind::Exit, ObjectUnavailableReason::Empty),
                (BpfObjectKind::TcpV6Connect, ObjectUnavailableReason::NotElf),
            ]
        );
        // Vollständig aufgelöste und nicht gewählte Sensoren tragen nichts bei.
        assert!(unresolved_objects(None, Some(&flow)).is_empty());
        assert!(unresolved_objects(None, None).is_empty());
    }

    #[test]
    fn test_only_the_development_flag_with_a_manifest_degrades_unresolved_objects() {
        let manifest = Some(Path::new("/usr/local/lib/harw-dod/bpf/manifest.json"));
        assert_eq!(
            UnresolvedObjects::for_flags(false, manifest),
            UnresolvedObjects::Refuse
        );
        assert_eq!(
            UnresolvedObjects::for_flags(false, None),
            UnresolvedObjects::Refuse
        );
        assert_eq!(
            UnresolvedObjects::for_flags(true, manifest),
            UnresolvedObjects::Degrade
        );
        // An `clap` vorbei (das `requires` erzwingt): ohne Manifest nie
        // degradieren.
        assert_eq!(
            UnresolvedObjects::for_flags(true, None),
            UnresolvedObjects::Refuse
        );
    }

    #[test]
    fn test_verify_selected_objects_refuses_unresolved_objects_after_verification() -> TestResult {
        let procmon_missing = unresolved_pair(
            Some(resolved(BpfObjectKind::Exec)),
            vec![unavailable(
                BpfObjectKind::Exit,
                ObjectUnavailableReason::Missing,
            )],
        );
        let flow: SensorObjects = Ok([
            resolved(BpfObjectKind::TcpV4Connect),
            resolved(BpfObjectKind::TcpV6Connect),
        ]);
        let manifest = Some(Path::new("/usr/local/lib/harw-dod/bpf/manifest.json"));

        // Die Manifestprüfung sieht jedes eingelesene Objekt (auch das
        // Geschwister `exec` des fehlenden `exit`) und läuft zuerst; danach
        // lehnt die Produktion das fehlende Objekt ab.
        let seen = Cell::new(0_usize);
        let result = verify_selected_objects(
            Some(&procmon_missing),
            Some(&flow),
            manifest,
            UnresolvedObjects::Refuse,
            |manifest, objects| {
                assert!(manifest.is_some());
                seen.set(objects.len());
                Ok(objects.len())
            },
        );
        match result {
            Err(ProbeError::BpfObjectIntegrity {
                object: Some(BpfObjectKind::Exit),
                failure: IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            }) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        assert_eq!(seen.get(), 3);

        // Ein Verstoß der Manifestprüfung (manipuliertes Objekt) geht vor.
        match verify_selected_objects(
            Some(&procmon_missing),
            Some(&flow),
            None,
            UnresolvedObjects::Refuse,
            |_, _| {
                Err(ProbeError::BpfObjectIntegrity {
                    object: Some(BpfObjectKind::TcpV4Connect),
                    failure: IntegrityFailure::DigestMismatch,
                })
            },
        ) {
            Err(ProbeError::BpfObjectIntegrity {
                object: Some(BpfObjectKind::TcpV4Connect),
                failure: IntegrityFailure::DigestMismatch,
            }) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }

        // Entwicklungsmodus: das fehlende Objekt degradiert nur, die
        // Manifestprüfung läuft unverändert mit denselben Objekten.
        let verified = verify_selected_objects(
            Some(&procmon_missing),
            Some(&flow),
            manifest,
            UnresolvedObjects::Degrade,
            |manifest, objects| {
                assert!(manifest.is_some());
                Ok(objects.len())
            },
        )
        .map_err(ctx("development mode degrades"))?;
        assert_eq!(verified, 3);

        // Auch im Entwicklungsmodus beendet ein Verstoß der Manifestprüfung
        // den Start.
        match verify_selected_objects(
            Some(&procmon_missing),
            Some(&flow),
            manifest,
            UnresolvedObjects::Degrade,
            |_, _| {
                Err(ProbeError::BpfObjectIntegrity {
                    object: Some(BpfObjectKind::Exec),
                    failure: IntegrityFailure::DigestMismatch,
                })
            },
        ) {
            Err(ProbeError::BpfObjectIntegrity {
                object: Some(BpfObjectKind::Exec),
                failure: IntegrityFailure::DigestMismatch,
            }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_selected_sensor_ids_follow_the_profile_selection() {
        let procmon = SensorId::from_str("probe-bpf-procmon-0");
        let flow = SensorId::from_str("probe-bpf-flow-0");
        let select = |exec, tcp_connect| SensorSelection { exec, tcp_connect };
        assert_eq!(
            selected_sensor_ids(select(true, true), procmon.as_str(), flow.as_str()),
            vec![procmon.clone(), flow.clone()]
        );
        assert_eq!(
            selected_sensor_ids(select(true, false), procmon.as_str(), flow.as_str()),
            vec![procmon.clone()]
        );
        assert_eq!(
            selected_sensor_ids(select(false, true), procmon.as_str(), flow.as_str()),
            vec![flow.clone()]
        );
        assert!(
            selected_sensor_ids(select(false, false), procmon.as_str(), flow.as_str()).is_empty()
        );
    }

    #[test]
    fn test_integrity_refusal_reports_each_selected_sensor_once() -> TestResult {
        let sink = RecordingSink::default();
        let sensors = [
            "probe-bpf-procmon-0",
            "probe-bpf-flow-0",
            "probe-bpf-flow-0",
        ]
        .map(SensorId::from_str);
        assert_eq!(
            report_integrity_refusal(&sensors, &sink, Timestamp::UNIX_EPOCH),
            2
        );
        let sent = sink
            .sent
            .lock()
            .map_err(|_| TestError::Unexpected("recording sink poisoned".into()))?;
        assert_eq!(sent.len(), 2);
        for (event, expected) in sent.iter().zip(["probe-bpf-procmon-0", "probe-bpf-flow-0"]) {
            let expected = SensorId::from_str(expected);
            assert_eq!(event.sensor, expected);
            assert_eq!(event.observed_at, Timestamp::UNIX_EPOCH);
            assert!(event.actor.is_none());
            assert!(matches!(
                &event.kind,
                EventKind::SensorDegraded { sensor } if sensor == &expected
            ));
        }
        Ok(())
    }

    #[test]
    fn test_integrity_refusal_survives_a_failing_sink() {
        let sensors = [SensorId::from_str("probe-bpf-procmon-0")];
        assert_eq!(
            report_integrity_refusal(&sensors, &FailingSink, Timestamp::UNIX_EPOCH),
            0
        );
    }

    #[test]
    fn test_integrity_refusal_exits_with_its_own_status() {
        // Manifest- und Objektfehler gleichermaßen: 78 (`EX_CONFIG`), den
        // die Unit über `RestartPreventExitStatus=78` nicht neu startet.
        for (object, failure) in [
            (None, IntegrityFailure::ManifestNotConfigured),
            (None, IntegrityFailure::ManifestUntrusted),
            (None, IntegrityFailure::ManifestMalformed),
            (Some(BpfObjectKind::Exec), IntegrityFailure::EntryMissing),
            (
                Some(BpfObjectKind::TcpV6Connect),
                IntegrityFailure::DigestMismatch,
            ),
            (
                Some(BpfObjectKind::Exit),
                IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            ),
        ] {
            assert_eq!(
                exit_status_for(&ProbeError::BpfObjectIntegrity { object, failure }),
                EXIT_INTEGRITY_REFUSED
            );
        }
        assert_eq!(EXIT_INTEGRITY_REFUSED, 78);
    }

    #[test]
    fn test_other_run_errors_exit_with_the_generic_failure_status() {
        for err in [
            ProbeError::LandlockUnavailable,
            ProbeError::SentinelSendFailed,
            ProbeError::BpfLoad(BpfError::AttachCapabilitiesUnavailable),
            ProbeError::SentinelConnectFailed {
                path: "/run/harw/sentinel.sock".to_owned(),
            },
        ] {
            assert_eq!(exit_status_for(&err), EXIT_RUN_FAILED);
        }
        // Derselbe Wert wie `ExitCode::FAILURE` unter Linux, und nie der
        // Status, den die Unit vom Neustart ausnimmt.
        assert_eq!(EXIT_RUN_FAILED, 1);
        assert_ne!(EXIT_RUN_FAILED, EXIT_INTEGRITY_REFUSED);
    }

    #[test]
    fn test_systemd_unit_does_not_restart_after_an_integrity_refusal() {
        // Die Unit ist die Gegenseite von `exit_status_for`: sie muss genau
        // diesen Status vom Neustart ausnehmen und das Manifest übergeben.
        let unit = include_str!("../../../../deploy/systemd/harw-probe-bpf.service");
        let directive = format!("RestartPreventExitStatus={EXIT_INTEGRITY_REFUSED}");
        assert!(unit.lines().any(|line| line == directive), "{directive}");
        assert!(unit.lines().any(|line| line == "Restart=on-failure"));
        assert!(unit.lines().any(|line| {
            line.starts_with("ExecStart=")
                && line.contains(" --bpf-manifest @BPFDIR@/manifest.json")
        }));
        // Der Entwicklungsmodus steht nie in der Unit: ein entferntes oder
        // beschädigtes Objekt endet dort immer mit 78.
        assert!(
            unit.lines()
                .filter(|line| line.starts_with("ExecStart"))
                .all(|line| !line.contains("--dev-allow-missing-bpf"))
        );
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
        // Nur im Entwicklungsmodus erreichbar. Kein Kernelzugriff: bei
        // fehlenden Objekten wird der Vertragssatz gar nicht erst gebaut,
        // `load_sensor` nie aufgerufen.
        let loader = RealBpfLoader::new();
        let contracts_built = Cell::new(false);
        let missing = unresolved_pair(
            Some(resolved(BpfObjectKind::Exit)),
            vec![ObjectUnavailable {
                object: BpfObjectKind::Exec,
                path: PathBuf::from("/nonexistent/exec.bpf.o"),
                reason: ObjectUnavailableReason::Missing,
            }],
        );
        let setup = setup_sensor(
            &loader,
            SensorId::from_str("probe-bpf-procmon-0"),
            missing,
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
        // Das verwendbare Geschwister wird nie geladen; die Mängelliste
        // trägt nur das fehlende Objekt.
        let SensorSetup::Degraded(sensor) = setup else {
            return Err(TestError::Unexpected(
                "a sensor with a missing object must degrade".into(),
            ));
        };
        let defects: Vec<BpfObjectKind> = sensor.missing().iter().map(|item| item.object).collect();
        assert_eq!(defects, vec![BpfObjectKind::Exec]);
        assert!(!contracts_built.get());
        Ok(())
    }

    // Ende-zu-Ende-Tests der Integritätsschranke (Owner-Review zu PR #168):
    // echte Objektdateien unter einem Temporärverzeichnis, echte
    // Kommandozeile, echte Auflösung und dieselbe Manifestprüfung wie in der
    // Produktion — nur mit Anker und Eigentümer der Fixture statt `/` und
    // root (`Fixture::verify`). `run` selbst bleibt ungetestet (Socket,
    // Landlock, eBPF); es gibt den Fehler aus `resolve_and_verify_objects`
    // unverändert zurück, bevor `setup_sensor` einen Vertragssatz baut.

    /// Beide Sensoren gewählt, wie in der ausgelieferten Konfiguration.
    const BOTH: SensorSelection = SensorSelection {
        exec: true,
        tcp_connect: true,
    };

    /// Nur der Prozess-Sensor gewählt.
    const EXEC_ONLY: SensorSelection = SensorSelection {
        exec: true,
        tcp_connect: false,
    };

    /// Nur der Verbindungs-Sensor gewählt.
    const TCP_ONLY: SensorSelection = SensorSelection {
        exec: false,
        tcp_connect: true,
    };

    /// Die vorgegebenen Kennungen beider Sensoren.
    const BOTH_IDS: [&str; 2] = ["probe-bpf-procmon-0", "probe-bpf-flow-0"];

    /// Eine Kommandozeile wie die der systemd-Unit: explizite Objektpfade
    /// unter der Fixture, dazu `extra`.
    fn fixture_cli(fixture: &Fixture, extra: &[OsString]) -> TestResult<Cli> {
        let mut args = vec![
            OsString::from("harw-probe-bpf"),
            OsString::from("--sentinel-socket"),
            OsString::from("/run/harw/sentinel.sock"),
        ];
        for (flag, object) in [
            ("--exec-program-path", BpfObjectKind::Exec),
            ("--exit-program-path", BpfObjectKind::Exit),
            ("--tcp-v4-program-path", BpfObjectKind::TcpV4Connect),
            ("--tcp-v6-program-path", BpfObjectKind::TcpV6Connect),
        ] {
            args.push(OsString::from(flag));
            args.push(fixture.object_path(object).into_os_string());
        }
        args.extend_from_slice(extra);
        Cli::try_parse_from(args).map_err(ctx("fixture command line parses"))
    }

    /// `--bpf-manifest` mit dem Manifest der Fixture (Produktion).
    fn with_manifest(fixture: &Fixture) -> [OsString; 2] {
        [
            OsString::from("--bpf-manifest"),
            fixture.manifest_path().into_os_string(),
        ]
    }

    /// Der Entwicklungsmodus allein (ohne Manifest lehnt `clap` ihn ab).
    fn dev_mode() -> [OsString; 1] {
        [OsString::from("--dev-allow-missing-bpf")]
    }

    /// Der Entwicklungsmodus mit dem Manifest der Fixture.
    fn dev_mode_with_manifest(fixture: &Fixture) -> Vec<OsString> {
        with_manifest(fixture)
            .into_iter()
            .chain(dev_mode())
            .collect()
    }

    /// Hängt `-tampered` an ein Fixture-Objekt an: weiter ein ELF-Objekt,
    /// aber mit anderem Digest.
    fn tamper(fixture: &Fixture, object: BpfObjectKind) -> TestResult {
        let mut bytes = object_bytes(object);
        bytes.extend_from_slice(b"-tampered");
        std::fs::write(fixture.object_path(object), &bytes).map_err(ctx("tamper with an object"))
    }

    /// Entfernt Fixture-Objekte.
    fn remove_objects(fixture: &Fixture, objects: &[BpfObjectKind]) -> TestResult {
        for &object in objects {
            std::fs::remove_file(fixture.object_path(object)).map_err(ctx("remove an object"))?;
        }
        Ok(())
    }

    /// Schritte 3 und 4 aus `run` gegen die Fixture.
    fn gate(
        fixture: &Fixture,
        cli: &Cli,
        selection: SensorSelection,
        sink: &RecordingSink,
    ) -> Result<SelectedObjects, ProbeError> {
        resolve_and_verify_objects(
            cli,
            selection,
            sink,
            Timestamp::UNIX_EPOCH,
            |manifest, objects| fixture.verify(manifest, objects),
        )
    }

    /// Erwartet die Ablehnung `object`/`failure`, den Exit-Status 78 und je
    /// gewähltem Sensor (`reported`) genau ein `SensorDegraded`.
    fn expect_integrity_refusal(
        result: Result<SelectedObjects, ProbeError>,
        object: Option<BpfObjectKind>,
        failure: IntegrityFailure,
        sink: &RecordingSink,
        reported: &[&str],
    ) -> TestResult {
        let err = match result {
            Err(err) => err,
            Ok(_) => {
                return Err(TestError::Unexpected(format!(
                    "expected refusal {object:?}/{failure:?}, got loadable objects"
                )));
            }
        };
        match &err {
            ProbeError::BpfObjectIntegrity {
                object: actual_object,
                failure: actual_failure,
            } if *actual_object == object && *actual_failure == failure => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected refusal {object:?}/{failure:?}, got {other:?}"
                )));
            }
        }
        assert_eq!(exit_status_for(&err), EXIT_INTEGRITY_REFUSED);
        // Weder Suchpfad noch Fixture-Verzeichnis in der Meldung.
        assert!(!err.to_string().contains('/'), "{err}");
        let sent = sink
            .sent
            .lock()
            .map_err(|_| TestError::Unexpected("recording sink poisoned".into()))?;
        let sensors: Vec<&str> = sent.iter().map(|event| event.sensor.as_str()).collect();
        assert_eq!(sensors, reported);
        assert!(sent.iter().all(|event| matches!(
            &event.kind,
            EventKind::SensorDegraded { sensor } if sensor == &event.sensor
        )));
        Ok(())
    }

    /// Wie viele Meldungen die Senke aufgezeichnet hat.
    fn reported_count(sink: &RecordingSink) -> TestResult<usize> {
        Ok(sink
            .sent
            .lock()
            .map_err(|_| TestError::Unexpected("recording sink poisoned".into()))?
            .len())
    }

    #[test]
    fn test_gate_passes_objects_that_match_the_manifest() -> TestResult {
        let fixture = Fixture::trusted()?;
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
        assert!(!cli.dev_allow_missing_bpf);
        let sink = RecordingSink::default();
        let selected =
            gate(&fixture, &cli, BOTH, &sink).map_err(ctx("matching objects pass the gate"))?;
        assert!(matches!(selected.procmon, Some(Ok(_))));
        assert!(matches!(selected.flow, Some(Ok(_))));
        assert_eq!(reported_count(&sink)?, 0);
        Ok(())
    }

    #[test]
    fn test_removed_selected_object_refuses_to_start_with_status_78() -> TestResult {
        // Das Objekt wurde nach der Installation entfernt: früher degradierte
        // nur der Sensor, die Unit blieb aktiv und die Erkennung blind.
        let fixture = Fixture::trusted()?;
        std::fs::remove_file(fixture.object_path(BpfObjectKind::Exec))
            .map_err(ctx("remove the exec object"))?;
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            &sink,
            &BOTH_IDS,
        )
    }

    #[test]
    fn test_corrupt_or_truncated_selected_object_refuses_to_start_with_status_78() -> TestResult {
        let truncated_magic = object_bytes(BpfObjectKind::TcpV6Connect)
            .get(..2)
            .ok_or(TestError::Missing("two bytes of the fixture object"))?
            .to_vec();
        for (bytes, reason) in [
            (
                b"corrupted: no ELF header".to_vec(),
                ObjectUnavailableReason::NotElf,
            ),
            (truncated_magic, ObjectUnavailableReason::NotElf),
            (Vec::new(), ObjectUnavailableReason::Empty),
        ] {
            let fixture = Fixture::trusted()?;
            std::fs::write(fixture.object_path(BpfObjectKind::TcpV6Connect), &bytes)
                .map_err(ctx("corrupt the tcp_v6 object"))?;
            let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
            let sink = RecordingSink::default();
            expect_integrity_refusal(
                gate(&fixture, &cli, BOTH, &sink),
                Some(BpfObjectKind::TcpV6Connect),
                IntegrityFailure::ObjectUnresolved(reason),
                &sink,
                &BOTH_IDS,
            )?;
        }
        Ok(())
    }

    #[test]
    fn test_selected_object_with_other_elf_bytes_refuses_with_digest_mismatch() -> TestResult {
        let original = object_bytes(BpfObjectKind::Exit);
        let mut appended = original.clone();
        appended.extend_from_slice(b"-tampered");
        let truncated = original
            .get(..6)
            .ok_or(TestError::Missing("six bytes of the fixture object"))?
            .to_vec();
        for bytes in [appended, truncated] {
            let fixture = Fixture::trusted()?;
            std::fs::write(fixture.object_path(BpfObjectKind::Exit), &bytes)
                .map_err(ctx("tamper with the exit object"))?;
            let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
            let sink = RecordingSink::default();
            expect_integrity_refusal(
                gate(&fixture, &cli, BOTH, &sink),
                Some(BpfObjectKind::Exit),
                IntegrityFailure::DigestMismatch,
                &sink,
                &BOTH_IDS,
            )?;
        }
        Ok(())
    }

    #[test]
    fn test_all_selected_objects_missing_is_refused_instead_of_verifying_nothing() -> TestResult {
        // Früher: keine aufgelöste Datei, also `Ok(0)` ohne das Manifest zu
        // öffnen, und beide Sensoren still degradiert.
        let fixture = Fixture::trusted()?;
        for object in [
            BpfObjectKind::Exec,
            BpfObjectKind::Exit,
            BpfObjectKind::TcpV4Connect,
            BpfObjectKind::TcpV6Connect,
        ] {
            std::fs::remove_file(fixture.object_path(object)).map_err(ctx("remove an object"))?;
        }
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;

        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            &sink,
            &BOTH_IDS,
        )?;

        // Nur der Prozess-Sensor gewählt: dieselbe Ablehnung, gemeldet wird
        // nur er.
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, EXEC_ONLY, &sink),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            &sink,
            &["probe-bpf-procmon-0"],
        )?;

        // Ein konfiguriertes Manifest wird auch ohne aufgelöstes Objekt
        // gelesen: fehlt es, scheitert schon dessen Prüfung.
        std::fs::remove_file(fixture.manifest_path()).map_err(ctx("remove the manifest"))?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            None,
            IntegrityFailure::ManifestUnreadable(ErrorKind::NotFound),
            &sink,
            &BOTH_IDS,
        )
    }

    #[test]
    fn test_unresolved_object_without_manifest_or_development_mode_is_refused() -> TestResult {
        // Fail closed auch ohne `--bpf-manifest`: ohne den Entwicklungsmodus
        // degradiert kein gewählter Sensor still. Das eingelesene
        // Geschwister `exec.bpf.o` des fehlenden `exit.bpf.o` wird zuerst
        // geprüft und verlangt ein Manifest.
        let fixture = Fixture::trusted()?;
        remove_objects(&fixture, &[BpfObjectKind::Exit])?;
        let cli = fixture_cli(&fixture, &[])?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, EXEC_ONLY, &sink),
            None,
            IntegrityFailure::ManifestNotConfigured,
            &sink,
            &["probe-bpf-procmon-0"],
        )?;

        // Ohne ein einziges eingelesenes Objekt bleibt die Ablehnung des
        // fehlenden Objekts: auch dann kein stiller Start.
        remove_objects(&fixture, &[BpfObjectKind::Exec])?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, EXEC_ONLY, &sink),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            &sink,
            &["probe-bpf-procmon-0"],
        )
    }

    #[test]
    fn test_gate_ignores_missing_objects_of_an_unselected_sensor() -> TestResult {
        // Ein Profil, das nur `exec` wählt, scheitert nie an fehlenden
        // tcp-Objekten: ein nicht gewählter Sensor wird gar nicht aufgelöst.
        let fixture = Fixture::trusted()?;
        remove_objects(
            &fixture,
            &[BpfObjectKind::TcpV4Connect, BpfObjectKind::TcpV6Connect],
        )?;
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        let selected = gate(&fixture, &cli, EXEC_ONLY, &sink)
            .map_err(ctx("an unselected sensor's objects are ignored"))?;
        assert!(matches!(selected.procmon, Some(Ok(_))));
        assert!(selected.flow.is_none());
        assert_eq!(reported_count(&sink)?, 0);

        // Dasselbe umgekehrt: nur `tcp-connect` gewählt, Prozess-Objekte weg.
        let fixture = Fixture::trusted()?;
        remove_objects(&fixture, &[BpfObjectKind::Exec, BpfObjectKind::Exit])?;
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        let selected = gate(&fixture, &cli, TCP_ONLY, &sink)
            .map_err(ctx("an unselected sensor's objects are ignored"))?;
        assert!(selected.procmon.is_none());
        assert!(matches!(selected.flow, Some(Ok(_))));
        assert_eq!(reported_count(&sink)?, 0);
        Ok(())
    }

    #[test]
    fn test_tampered_object_is_reported_before_its_missing_sibling() -> TestResult {
        // Innerhalb eines Sensors: `exec.bpf.o` manipuliert, `exit.bpf.o`
        // entfernt. Gemeldet wird die Manipulation, nicht erst nach der
        // Reparatur von `exit.bpf.o` beim nächsten Start; in der Produktion
        // wie im Entwicklungsmodus.
        let fixture = Fixture::trusted()?;
        tamper(&fixture, BpfObjectKind::Exec)?;
        remove_objects(&fixture, &[BpfObjectKind::Exit])?;
        for extra in [
            with_manifest(&fixture).to_vec(),
            dev_mode_with_manifest(&fixture),
        ] {
            let cli = fixture_cli(&fixture, &extra)?;
            let sink = RecordingSink::default();
            expect_integrity_refusal(
                gate(&fixture, &cli, BOTH, &sink),
                Some(BpfObjectKind::Exec),
                IntegrityFailure::DigestMismatch,
                &sink,
                &BOTH_IDS,
            )?;
        }

        // Ohne Manipulation bleibt es bei der Ablehnung des fehlenden Objekts.
        let fixture = Fixture::trusted()?;
        remove_objects(&fixture, &[BpfObjectKind::Exit])?;
        let cli = fixture_cli(&fixture, &with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            Some(BpfObjectKind::Exit),
            IntegrityFailure::ObjectUnresolved(ObjectUnavailableReason::Missing),
            &sink,
            &BOTH_IDS,
        )
    }

    #[test]
    fn test_development_mode_requires_the_manifest() -> TestResult {
        // `clap` verlangt `--bpf-manifest` (`requires`); die Meldung nennt es.
        let fixture = Fixture::trusted()?;
        match fixture_cli(&fixture, &dev_mode()) {
            Err(TestError::Context { source, .. }) if source.contains("--bpf-manifest") => {}
            Err(other) => return Err(other),
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "--dev-allow-missing-bpf parsed without --bpf-manifest".into(),
                ));
            }
        }
        let cli = fixture_cli(&fixture, &dev_mode_with_manifest(&fixture))?;
        assert!(cli.dev_allow_missing_bpf);
        assert_eq!(cli.bpf_manifest, Some(fixture.manifest_path()));
        Ok(())
    }

    #[test]
    fn test_development_mode_loads_a_partial_build_and_degrades_only_the_unresolved_sensor()
    -> TestResult {
        // Teilbau: `exec`/`exit` gebaut, beide tcp-Objekte fehlen. Der
        // Prozess-Sensor passiert die Manifestprüfung, nur der
        // Verbindungs-Sensor degradiert.
        let fixture = Fixture::trusted()?;
        remove_objects(
            &fixture,
            &[BpfObjectKind::TcpV4Connect, BpfObjectKind::TcpV6Connect],
        )?;
        let cli = fixture_cli(&fixture, &dev_mode_with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        let selected =
            gate(&fixture, &cli, BOTH, &sink).map_err(ctx("development mode degrades"))?;
        assert!(matches!(selected.procmon, Some(Ok(_))));
        let flow = selected
            .flow
            .ok_or(TestError::Missing("the tcp-connect sensor's resolution"))?;
        let defects: Vec<(BpfObjectKind, ObjectUnavailableReason)> = match &flow {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "removed tcp objects must not resolve".into(),
                ));
            }
            Err(unresolved) => unresolved
                .missing
                .iter()
                .map(|item| (item.object, item.reason))
                .collect(),
        };
        assert_eq!(
            defects,
            vec![
                (
                    BpfObjectKind::TcpV4Connect,
                    ObjectUnavailableReason::Missing
                ),
                (
                    BpfObjectKind::TcpV6Connect,
                    ObjectUnavailableReason::Missing
                ),
            ]
        );
        // Kein Abbruch, also auch keine Ablehnungsmeldung: der degradierte
        // Sensor meldet sich selbst beim ersten `poll`.
        assert_eq!(reported_count(&sink)?, 0);

        // `setup_sensor` degradiert dann, ohne einen Vertragssatz zu bauen.
        let loader = RealBpfLoader::new();
        let contracts_built = Cell::new(false);
        let setup = setup_sensor(
            &loader,
            SensorId::from_str("probe-bpf-flow-0"),
            flow,
            |sensor, tcp_v4, tcp_v6| {
                contracts_built.set(true);
                harw_dod_flow::flow_contracts(sensor, tcp_v4, tcp_v6, harw_dod_bpf::BpfScope::Host)
            },
            Some(network_scope(&[])),
        )
        .map_err(ctx("setup_sensor in development mode"))?;
        assert!(matches!(setup, SensorSetup::Degraded(_)));
        assert!(!contracts_built.get());
        Ok(())
    }

    #[test]
    fn test_development_mode_never_skips_the_manifest_check() -> TestResult {
        // Teilbau mit manipuliertem `exit.bpf.o`: der Entwicklungsmodus
        // lockert nur die Regel für nicht aufgelöste Objekte.
        let fixture = Fixture::trusted()?;
        remove_objects(
            &fixture,
            &[BpfObjectKind::TcpV4Connect, BpfObjectKind::TcpV6Connect],
        )?;
        tamper(&fixture, BpfObjectKind::Exit)?;
        let cli = fixture_cli(&fixture, &dev_mode_with_manifest(&fixture))?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            Some(BpfObjectKind::Exit),
            IntegrityFailure::DigestMismatch,
            &sink,
            &BOTH_IDS,
        )?;

        // Ein fehlendes Manifest scheitert auch im Entwicklungsmodus, selbst
        // wenn kein einziges Objekt aufgelöst ist.
        let fixture = Fixture::trusted()?;
        remove_objects(
            &fixture,
            &[
                BpfObjectKind::Exec,
                BpfObjectKind::Exit,
                BpfObjectKind::TcpV4Connect,
                BpfObjectKind::TcpV6Connect,
            ],
        )?;
        let cli = fixture_cli(&fixture, &dev_mode_with_manifest(&fixture))?;
        std::fs::remove_file(fixture.manifest_path()).map_err(ctx("remove the manifest"))?;
        let sink = RecordingSink::default();
        expect_integrity_refusal(
            gate(&fixture, &cli, BOTH, &sink),
            None,
            IntegrityFailure::ManifestUnreadable(ErrorKind::NotFound),
            &sink,
            &BOTH_IDS,
        )
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

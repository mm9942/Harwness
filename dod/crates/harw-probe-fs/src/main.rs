//! fanotify-Sonde, Push-Only — Knoten AW4-02b.
//!
//! # Zweck
//! Dieses Binary ist die eine privilegierte Dateisystem-Sonde des
//! Ausbauprogramms: es überwacht die über `--scope-root` konfigurierten
//! Verzeichnisse auf Schreibzugriffe, formt jedes Ereignis über
//! `harw_dod_fsmon` und sendet es an `harw-sentinel`. Es hat **keinen**
//! Empfangspfad — siehe Abschnitt „Push-Only" unten.
//!
//! # Berechtigungsklasse
//! `harw_dod_cap::Capability::WatchFilesystem`, Klasse
//! `harw_dod_cap::CapabilityClass::FileWatch` — die eine Fähigkeit, für die
//! dieses Binary existiert. Dieser Prozess läuft produktiv mit
//! `CAP_SYS_ADMIN` — die einzige Capability, die `fanotify_mark` mit
//! `FAN_MARK_FILESYSTEM` und die beiden `FAN_UNLIMITED_*`-Flags erlaubt
//! (siehe unten, Abschnitt „Stand der fanotify-Bindung", und
//! [`source`]-Moduldoku) — und keine weitere.
//!
//! # Push-Only: woran man die Eigenschaft erkennt
//! Ein Prozess mit `CAP_SYS_ADMIN`, der Nachrichten **annimmt**, ist ein
//! Angriffsziel. Einer, der nur sendet, ist keins. Diese Eigenschaft ist
//! hier keine Konvention, sondern eine Typ-Eigenschaft:
//! [`sink::EventSink`] hat genau eine Methode (`send`), die ausschließlich
//! Daten **entgegennimmt** — es gibt kein `poll_command`, kein
//! `set_watch_from`, keine Methode, über die der Sentinel diesem Prozess
//! etwas mitteilen könnte. `src/push_only_guard.rs` macht das quelltextlich
//! prüfbar: es durchsucht jede Implementierungsdatei dieser Crate (außer
//! sich selbst) nach den beiden Bezeichnern, die einen Empfangspfad
//! ausmachen würden — der Unix-Socket-Empfangsfunktion und der Funktion,
//! die einen Puffer bis zum Streamende einliest. Ein späterer Leser
//! bestätigt die Zusage, indem er genau diese eine, kurze Datei liest,
//! nicht den gesamten Quelltext dieser Crate.
//!
//! # Die harte Landlock-Entscheidung
//! Ein unprivilegierter Sammler, der ohne Landlock läuft, ist ein Sammler
//! ohne zusätzliche Schranke — hinnehmbar, weil sein Berechtigungsumfang
//! von vornherein klein ist (siehe `harw-sentinel`, das ohne Landlock
//! **weiterlaufen** darf). Ein Prozess mit `CAP_SYS_ADMIN` ohne Schranke
//! ist etwas anderes: dort ist Landlock nicht eine von mehreren
//! Verteidigungslinien, sondern die einzige, die den deklarierten
//! `ReadScope` gegen das tatsächliche Berechtigungsvermögen des Prozesses
//! durchsetzt. Ohne sie zu starten hieße, die Verteidigung wegzulassen und
//! trotzdem das Risiko zu tragen. [`run`] ruft deshalb
//! [`landlock::enforce_read_scope`] vor jedem weiteren privilegierten
//! Schritt auf und bricht bei jedem Ausgang außer vollständiger
//! Durchsetzung hart ab — siehe [`landlock`]-Moduldoku für die Begründung,
//! warum das (anders als der ursprüngliche Auftrag dieses Knotens vermutet
//! hatte) heute tatsächlich **gebaut** ist, nicht nur gemeldet.
//!
//! # Stand der fanotify-Bindung: jetzt gebaut, über `nix`
//! Die für den Workspace gepinnte `rustix`-Fassung 1.1.4 listet
//! `fanotify_init`/`fanotify_mark` weiterhin ausdrücklich als *nicht
//! implementiert*, ohne auch nur eine unsichere Hülle — dieser Knoten hat
//! das an der Quelle erneut nachgeprüft (siehe [`source`]-Moduldoku). Ein
//! roher `libc`-Syscall über `unsafe` bleibt deshalb versperrt
//! (`[workspace.lints.rust] unsafe_code = "forbid"`). Die Recherche dieses
//! Knotens fand jedoch eine zweite, sichere Bindung: `nix::sys::fanotify`,
//! seit Fassung 0.28.0 im Baum, mit E/A-Sicherheit seit 0.30.0 (siehe
//! [`source`]-Moduldoku für Quellen und Fassungen). [`source::build_fanotify_source`]
//! initialisiert damit eine echte fanotify-Gruppe und markiert jede Wurzel
//! des konfigurierten `ReadScope`. Der Betrieb bleibt zusätzlich über
//! `harw_dod_fsmon::FixtureFsEventSource` möglich (siehe
//! [`collect`]-Modultests): jeder Teil der Sammel- und Sendelogik ist damit
//! sowohl ohne Kernel und ohne Berechtigungen als auch — sobald
//! `CAP_SYS_ADMIN` vorliegt — mit der echten Bindung lauffähig geprüft.
//!
//! # Was sich gegenüber dem ursprünglichen Auftrag geändert hat
//! Der ursprüngliche Auftrag dieses Knotens vermutete, Landlock-Bindung,
//! Sentinel-Sende-Senke **und** fanotify-Bindung seien „ohne `unsafe` nicht
//! erreichbar" und verlangte eine unabhängige Prüfung statt eines Glaubens
//! an diese Vermutung. Für Landlock und den Socket-Transport hatte der
//! gleichzeitig gelandete Knoten AW2-19 (`harw-sentinel`) bereits eine
//! sichere Landlock-Bindung (`landlock = "0.4.7"`) und einen sicheren
//! `SOCK_SEQPACKET`-Client (`rustix`, Feature `net`) in diesen Workspace
//! eingebracht; diese Sonde übernimmt dieselben, bereits begründeten
//! Abhängigkeiten (siehe `Cargo.toml`) statt eine überholte Vermutung
//! fortzuschreiben — siehe [`landlock`]- und [`sink`]-Moduldoku. Für
//! fanotify bestätigte eine erste Prüfung die Vermutung zunächst
//! (`rustix` 1.1.4 implementiert es nicht); eine zweite, in diesem Knoten
//! nachgeholte Recherche fand mit `nix::sys::fanotify` dann doch eine
//! sichere Bindung (siehe [`source`]-Moduldoku) — die Vermutung „ohne
//! `unsafe` nicht erreichbar" galt hier für die eine geprüfte Bibliothek,
//! nicht für den Mechanismus selbst. **Was unverändert bleibt:** kein
//! `unsafe` an keiner Stelle dieser Crate, keine Versionsangabe ohne
//! Begründung, kein echter Socket/Landlock-/fanotify-Aufruf in einem Test
//! dieser Crate.
//!
//! # Fehler
//! Kommandozeilenfehler sind `clap::Error`, direkt in [`main`] behandelt.
//! Jeder Fehler aus [`run`] ist ein [`error::ProbeError`] — siehe dessen
//! Moduldoku für die vollständige Varianteneinteilung.
//!
//! # Examples
//! ```text
//! $ harw-probe-fs --log info \
//!     --scope-root /srv/data \
//!     --sentinel-socket /run/harw-sentinel.sock
//! ```

#![forbid(unsafe_code)]

mod cli;
mod collect;
mod error;
mod landlock;
mod sink;
mod source;

#[cfg(test)]
mod push_only_guard;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use harw_completions::CompletionsSubcommand;
use harw_dod_cap::{Capability, ReadScope, SensorHandle};
use harw_dod_fsmon::FsMonSensor;
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
/// `ExitCode::SUCCESS` nur, wenn [`run`] regulär zurückkehrt — praktisch
/// unerreichbar, weil [`collect::run_forever`] nur über einen Fehler
/// zurückkehrt (siehe dessen Moduldoku), nicht weil die fanotify-Bindung
/// fehlte (siehe [`source`]-Moduldoku für deren aktuellen Stand).
/// `ExitCode::FAILURE` bei jedem Fehlerpfad — Kommandozeile,
/// Landlock-Schranke, fehlender Sentinel, nicht verfügbare fanotify-Quelle.
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

    // `completions` läuft vor jedem Socket-, Landlock- oder fanotify-Schritt.
    if let Some(CompletionsSubcommand::Completions(args)) = cli.command.as_ref() {
        return match run_completions(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                tracing::error!(error = %err, "harw-probe-fs completions failed");
                eprintln!("harw-probe-fs: {err}");
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
            tracing::error!(error = %err, "harw-probe-fs exiting");
            eprintln!("harw-probe-fs: {err}");
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
/// noch Landlock noch fanotify.
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
        "harw-probe-fs",
        args,
        &harw_completions::HomeEnv::from_process(),
        &mut std::io::stdout().lock(),
    )?;
    Ok(())
}

/// Führt den eigentlichen Sondenbetrieb aus.
///
/// # Description
/// Reihenfolge: den `ReadScope` aus `--scope-root` bauen, sich **zuerst**
/// mit dem Sentinel verbinden (ein Dateisystempfad-Zugriff, der nach einer
/// Landlock-Regel scheitern könnte, falls der Socket-Pfad nicht im
/// gewährten Bereich liegt — Muster: `harw-sentinel::main::run`, das seinen
/// IPC-Socket ebenfalls vor `restrict_self` bindet), **dann** über Landlock
/// selbst einschränken (harter Abbruch bei jedem Ausgang außer
/// vollständiger Durchsetzung, siehe [`landlock`]-Moduldoku), erst danach
/// die fanotify-Quelle aufbauen und die Sammelschleife starten.
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`error::ProbeError::SentinelConnectFailed`] wenn der Sentinel nicht
/// erreichbar ist; [`error::ProbeError::LandlockUnavailable`] wenn Landlock
/// den Bereich nicht vollständig durchsetzt;
/// [`error::ProbeError::FanotifySourceUnavailable`], wenn sich die
/// fanotify-Gruppe nicht initialisieren lässt oder keine Wurzel des
/// `ReadScope` markiert werden konnte (siehe [`source`]-Moduldoku) — z. B.
/// wenn der Prozess nicht mit `CAP_SYS_ADMIN` läuft.
fn run(cli: Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let scope = ReadScope::from_roots(cli.scope_roots.iter().cloned());

    let sink = sink::build_sentinel_sink(sentinel_socket)?;
    tracing::info!(path = %sentinel_socket.display(), "connected to sentinel");

    landlock::enforce_read_scope(&scope)?;

    let source = source::build_fanotify_source(&scope)?;
    let handle = SensorHandle::new(
        SensorId::from_str(cli.sensor_id.clone()),
        Capability::WatchFilesystem,
    )
    .bind(scope);
    let sensor = FsMonSensor::new(handle, source, cli.proc_root.clone());

    tracing::info!(sensor_id = %cli.sensor_id, "collection loop starting");
    collect::run_forever(&sensor, sink.as_ref())
}

#[cfg(test)]
mod tests {
    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten Socket verbinden, ein echtes Landlock-Ruleset binden und
    // eine echte fanotify-Gruppe öffnen — alles drei nach Aufgabenstellung
    // untersagt. Jede darin verkettete Teilfunktion ist einzeln geprüft:
    // `cli::Cli::try_parse_from` in `cli.rs`, `collect::run_once` in
    // `collect.rs` (mit `FixtureFsEventSource`), `sink::encode_event` in
    // `sink.rs`, die reinen Flag- und Pfadbausteine von
    // `source::build_fanotify_source` in `source.rs`. Zusammen decken sie
    // jeden Schritt von `run` ab, ohne dass ein Test dieser Crate einen
    // echten Socket öffnet, ein echtes Landlock bindet oder eine echte
    // fanotify-Gruppe öffnet.
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

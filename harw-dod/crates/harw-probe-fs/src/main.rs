//! fanotify-Sonde, Push-Only — Knoten AW4-02b.
//!
//! # Zweck
//! Dieses Binary ist die eine privilegierte Dateisystem-Sonde des
//! Ausbauprogramms: es überwacht die Persistenz-Orte des Hosts auf
//! Schreibzugriffe, formt jedes Ereignis über `harw_dod_fsmon` und sendet es
//! an `harw-sentinel`. Es hat **keinen** Empfangspfad — siehe Abschnitt
//! „Push-Only" unten.
//!
//! # Beobachtungsbereich aus `/etc` (PL-100 WP1.6)
//! Der Bereich kommt aus `[probe_fs]` der DoD-Konfiguration
//! (`/etc/harw-dod/config.toml` bzw. `--config`), sonst aus den eingebauten
//! Vorgaben, die die Orte der Regel `persistence-write` abdecken
//! (`/etc`, cron- und at-Spool, systemd/udev unter `/usr`, und unter
//! `/home`, `/var/home`, `/root`, `/var/roothome` nur `.ssh`, die
//! systemd-User-Units, Autostart, `environment.d` und die
//! Shell-Startdateien); `--scope-root`/`--home-root`/`--home-entry`
//! überschreiben ihn. Siehe [`watch`]. Damit gehört die Sonde zu
//! `harw-dod.target`. Wiederholte Schreibzugriffe desselben Prozesses auf
//! dieselbe Datei fasst `harw_dod_fsmon::coalesce` zusammen, ein Überlauf
//! der (begrenzten) Kernel-Warteschlange wird ein `SensorDegraded`, und eine
//! Wurzel oder ein Einhängepunkt unter ihr (Review R3) auf einem
//! Dateisystem, das die Sonde nicht markieren konnte, meldet sie beim Start
//! ebenfalls als `SensorDegraded` ([`collect::report_uncovered_scope`]).
//! Eine beim Start fehlende Wurzel bleibt bis zum nächsten Start
//! unbeobachtet; ihre Zahl steht auf `info` im Journal.
//!
//! # Berechtigungsklasse
//! `harw_dod_cap::Capability::WatchFilesystem`, Klasse
//! `harw_dod_cap::CapabilityClass::FileWatch` — die eine Fähigkeit, für die
//! dieses Binary existiert. Dieser Prozess läuft produktiv mit
//! `CAP_SYS_ADMIN` — die einzige Capability, die `fanotify_mark` mit
//! `FAN_MARK_FILESYSTEM` und `FAN_UNLIMITED_MARKS` erlaubt
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
//! [`landlock::enforce_read_scope`] vor dem ersten gelesenen Ereignis auf
//! und bricht bei jedem Ausgang außer vollständiger Durchsetzung hart ab.
//! Einzig `fanotify_init`/`fanotify_mark` laufen davor (Review R2): eine
//! Wurzel wie `/var/spool/cron` (`0700`) wird über ihren nächsten lesbaren
//! Vorfahren markiert, und der liegt außerhalb der Lesewurzeln. Die
//! Markierungen überdauern `restrict_self`; die Ereignis-Deskriptoren
//! öffnet der Kernel erst beim Lesen, also unter Landlock (siehe
//! [`source`]-Moduldoku). Siehe [`landlock`]-Moduldoku für die Begründung,
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
//!     --config /etc/harw-dod/config.toml \
//!     --sentinel-socket /run/harw/sentinel.sock
//! $ harw-probe-fs --scope-root /srv/data --sentinel-socket /run/harw/sentinel.sock
//! ```

#![forbid(unsafe_code)]

mod cli;
mod collect;
mod error;
mod landlock;
mod sink;
mod source;
mod watch;

#[cfg(test)]
mod push_only_guard;

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use harw_completions::CompletionsSubcommand;
use harw_dod_cap::{Capability, SensorHandle};
use harw_dod_fsmon::FsMonSensor;
use harw_types::SensorId;
use jiff::Timestamp;

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
                // Best effort: help/version text is diagnostic output only.
                let _ = write!(std::io::stdout().lock(), "{err}");
                return ExitCode::SUCCESS;
            }
            // Best effort: clap's usage text is diagnostic output only.
            let _ = write!(std::io::stderr().lock(), "{err}");
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
                report(&err);
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
        // Best effort: clap's usage text is diagnostic output only.
        let _ = write!(std::io::stderr().lock(), "{err}");
        return ExitCode::FAILURE;
    };

    match run(cli, &sentinel_socket) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-probe-fs exiting");
            report(&err);
            ExitCode::FAILURE
        }
    }
}

/// Writes one `harw-probe-fs:` diagnostic line to stderr, best effort.
///
/// Arguments: `err` is the error to print. Returns nothing; a failed stderr
/// write has no further channel to report on, so it is discarded on purpose.
fn report(err: &impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr().lock(), "harw-probe-fs: {err}");
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
/// Reihenfolge: die DoD-Konfiguration laden und den Beobachtungsbereich
/// auflösen ([`watch::load_probe_config`], [`watch::resolve_watch_plan`];
/// beides vor Landlock, weil `/etc/harw-dod` danach nicht mehr lesbar sein
/// muss), sich **dann** mit dem Sentinel verbinden (ein
/// Dateisystempfad-Zugriff, der nach einer Landlock-Regel scheitern könnte,
/// falls der Socket-Pfad nicht im gewährten Bereich liegt — Muster:
/// `harw-sentinel::main::run`, das seinen IPC-Socket ebenfalls vor
/// `restrict_self` bindet), **dann** die fanotify-Quelle aufbauen und ihre
/// Markierungen setzen (Review R2: eine nicht lesbare Wurzel wird über
/// ihren nächsten lesbaren Vorfahren außerhalb der Lesewurzeln markiert,
/// siehe [`source`]-Moduldoku), **dann** über Landlock selbst einschränken —
/// Lesebereich plus `--proc-root` für die loginuid (harter Abbruch bei jedem
/// Ausgang außer vollständiger Durchsetzung, siehe [`landlock`]-Moduldoku)
/// —, eine unvollständige Abdeckung einmal als `SensorDegraded` melden
/// ([`collect::report_uncovered_scope`]) und erst danach die Sammelschleife
/// mit dem Bereichsfilter starten. Bis zur Landlock-Schranke wird kein
/// einziges Ereignis gelesen.
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `sentinel_socket` (`&Path`): der bereits entpackte `--sentinel-socket`.
///
/// # Errors
/// [`error::ProbeError::Config`], wenn die DoD-Konfiguration unbrauchbar
/// ist; [`error::ProbeError::EmptyWatchScope`] ohne jede Wurzel;
/// [`error::ProbeError::SentinelConnectFailed`] wenn der Sentinel nicht
/// erreichbar ist; [`error::ProbeError::LandlockUnavailable`] wenn Landlock
/// den Bereich nicht vollständig durchsetzt;
/// [`error::ProbeError::FanotifySourceUnavailable`], wenn sich die
/// fanotify-Gruppe nicht initialisieren lässt oder kein Dateisystem des
/// `ReadScope` markiert werden konnte (siehe [`source`]-Moduldoku) — z. B.
/// wenn der Prozess nicht mit `CAP_SYS_ADMIN` läuft;
/// [`error::ProbeError::SentinelSendFailed`], wenn die Abdeckungsmeldung
/// den Sentinel nicht erreicht.
fn run(cli: Cli, sentinel_socket: &Path) -> Result<(), ProbeError> {
    let config = watch::load_probe_config(cli.config.as_deref())?;
    let plan = watch::resolve_watch_plan(
        &cli.scope_roots,
        &cli.home_roots,
        &cli.home_entries,
        config.as_ref().map(harw_dod_config::Config::probe_fs),
    )?;
    log_watch_plan(&plan, config.as_ref());
    let scope = plan.filter.read_scope();

    let sink = sink::build_sentinel_sink(sentinel_socket)?;
    tracing::info!(path = %sentinel_socket.display(), "connected to sentinel");

    // Vor Landlock: die Markierung einer nicht lesbaren Wurzel geht über
    // einen Vorfahren außerhalb der Lesewurzeln (siehe `source`-Moduldoku).
    let (source, marks) = source::build_fanotify_source(&scope)?;
    tracing::info!(
        marked_filesystems = marks.marked_filesystems,
        uncovered_roots = marks.uncovered_roots,
        uncovered_mounts = marks.uncovered_mounts,
        absent_roots = marks.absent_roots,
        "fanotify marks placed"
    );
    if marks.absent_roots > 0 {
        // Review R3: eine fehlende Wurzel bekommt weder Markierung noch
        // Landlock-Regel; entsteht sie später, braucht sie einen Neustart.
        tracing::info!(
            absent_roots = marks.absent_roots,
            "scope roots absent at start stay unobserved until the probe restarts"
        );
    }

    landlock::enforce_read_scope(&scope, std::slice::from_ref(&cli.proc_root))?;

    let sensor_id = SensorId::from_str(cli.sensor_id.clone());
    collect::report_uncovered_scope(sink.as_ref(), &sensor_id, &marks, Timestamp::now())?;
    let handle = SensorHandle::new(sensor_id, Capability::WatchFilesystem).bind(scope);
    let sensor =
        FsMonSensor::new(handle, source, cli.proc_root.clone()).with_watch_filter(plan.filter);

    tracing::info!(sensor_id = %cli.sensor_id, "collection loop starting");
    collect::run_forever(&sensor, sink.as_ref())
}

/// Protokolliert den aufgelösten Beobachtungsbereich (`info`, eine Zeile).
///
/// # Description
/// Die Wurzeln und Einträge sind administrativ konfigurierte Pfade ohne
/// Benutzernamen (siehe [`watch`]), dazu Herkunft je Liste und der Digest
/// der gelesenen Konfiguration (Abgleich mit Sentinel und BPF-Probe).
fn log_watch_plan(plan: &watch::WatchPlan, config: Option<&harw_dod_config::Config>) {
    tracing::info!(
        scope_roots = ?plan.filter.scope_roots(),
        scope_roots_source = plan.scope_roots_source.label(),
        home_roots = ?plan.filter.home_roots(),
        home_roots_source = plan.home_roots_source.label(),
        home_entries = plan.filter.home_entries().len(),
        home_entries_source = plan.home_entries_source.label(),
        config_digest = config.map(|config| config.config_digest().to_string()),
        "watch scope resolved"
    );
}

#[cfg(test)]
mod tests {
    // `main`/`run` selbst werden hier bewusst nicht getestet: `run` würde
    // einen echten Socket verbinden, ein echtes Landlock-Ruleset binden und
    // eine echte fanotify-Gruppe öffnen — alles drei nach Aufgabenstellung
    // untersagt. Jede darin verkettete Teilfunktion ist einzeln geprüft:
    // `cli::Cli::try_parse_from` in `cli.rs`, die Bereichsauflösung und das
    // Laden der Konfiguration in `watch.rs`, `collect::run_once` in
    // `collect.rs` (mit `FixtureFsEventSource`, auch mit Bereichsfilter),
    // `sink::encode_event` in `sink.rs`, die reinen Flag-, Pfad- und
    // Überlaufbausteine von `source::build_fanotify_source` in `source.rs`
    // (auch die Markierungsplanung und die Abdeckung), die
    // Abdeckungsmeldung in `collect.rs` und die Rechtewahl der
    // Landlock-Regel in `landlock.rs`. Zusammen
    // decken sie jeden Schritt von `run` ab, ohne dass ein Test dieser Crate
    // einen echten Socket öffnet, ein echtes Landlock bindet oder eine echte
    // fanotify-Gruppe öffnet.

    use clap::Parser as _;

    use crate::cli::Cli;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Die ausgelieferte Zielunit (kanonische Quelle `deploy/systemd/`).
    const DOD_TARGET: &str = include_str!("../../../../deploy/systemd/harw-dod.target");
    /// Die ausgelieferte Unit dieser Sonde.
    const PROBE_UNIT: &str = include_str!("../../../../deploy/systemd/harw-probe-fs.service");

    /// Alle Werte von `key` (Zeilen `key=…` außerhalb von Kommentaren).
    fn values<'a>(unit: &'a str, key: &str) -> Vec<&'a str> {
        unit.lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .filter(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim())
            .collect()
    }

    /// PL-100 WP1.6: die Sonde gehört zu `harw-dod.target` — das Ziel zieht
    /// sie hoch und ordnet sich nach ihr, die Unit hängt sich ein.
    #[test]
    fn test_probe_unit_is_a_member_of_the_dod_target() {
        let wants: Vec<&str> = values(DOD_TARGET, "Wants")
            .into_iter()
            .flat_map(str::split_whitespace)
            .collect();
        assert!(wants.contains(&"harw-probe-fs.service"), "{wants:?}");
        let after: Vec<&str> = values(DOD_TARGET, "After")
            .into_iter()
            .flat_map(str::split_whitespace)
            .collect();
        assert!(after.contains(&"harw-probe-fs.service"), "{after:?}");
        assert!(
            !wants.iter().any(|unit| unit.starts_with("harw-warden")),
            "the target never pulls in the Warden: {wants:?}"
        );
        assert_eq!(values(PROBE_UNIT, "WantedBy"), vec!["harw-dod.target"]);
        assert_eq!(values(PROBE_UNIT, "PartOf"), vec!["harw-dod.target"]);
    }

    /// PL-100 WP1.6: kein Platzhalter mehr — die Unit nennt die
    /// Konfiguration, darf sie lesen, sieht die Home-Wurzeln, und ihre
    /// Kommandozeile parst ohne `--scope-root`.
    #[test]
    fn test_probe_unit_reads_its_scope_from_the_config() -> TestResult {
        let exec = values(PROBE_UNIT, "ExecStart")
            .into_iter()
            .next()
            .ok_or(TestError::Missing("ExecStart"))?;
        assert!(!exec.contains("--scope-root"), "{exec}");
        assert!(!exec.contains("/srv/harw/watched"), "{exec}");
        let rendered = exec
            .replace("@LIBEXECDIR@/", "")
            .replace("@SYSCONFDIR@", "/etc/harw-dod");
        let cli =
            Cli::try_parse_from(rendered.split_whitespace()).map_err(ctx("unit ExecStart"))?;
        assert_eq!(
            cli.config,
            Some(std::path::PathBuf::from("/etc/harw-dod/config.toml"))
        );
        assert!(cli.scope_roots.is_empty() && cli.home_roots.is_empty());

        let groups: Vec<&str> = values(PROBE_UNIT, "SupplementaryGroups")
            .into_iter()
            .flat_map(str::split_whitespace)
            .collect();
        assert!(groups.contains(&"harw-dod-config"), "{groups:?}");
        assert_eq!(values(PROBE_UNIT, "ProtectHome"), vec!["read-only"]);
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

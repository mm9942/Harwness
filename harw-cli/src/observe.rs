//! Kompositionsstelle der Telemetrie-Sinks — schließt den Bottom-up-Befund
//! „`harw-observe-prom`/`harw-observe-otlp` haben null Produktionskonsumenten".
//!
//! # Verantwortungsbereich
//! Dieses Modul ist die **einzige** Stelle in `harw-cli`, die einen
//! [`harw_observe::RoutingSink`] baut und dabei entscheidet, welche
//! zusätzlichen Ziele (Prometheus, OTLP) neben dem immer aktiven
//! [`harw_observe_file::FileSink`] existieren. Es implementiert außerdem den
//! einen Adapter, den `harw-observe-otlp` bewusst nicht selbst mitbringt
//! (siehe dortige `clock`-Moduldoku „Keine Systemuhr"): [`SystemClock`], eine
//! echte Systemuhr über `jiff::Timestamp::now()`. Den Netzwerktransport
//! selbst liefert seit Knoten AW7-02 `harw_observe_otlp::HttpTransport` —
//! dieses Modul konstruiert ihn nur, an genau einer Stelle, außerhalb jeder
//! laufenden `tokio`-Runtime (siehe Abschnitt „Warum außerhalb der Runtime"
//! unten). Und es startet den periodischen Flush ([`PeriodicFlush`]).
//!
//! # Warum außerhalb der Runtime
//! `HttpTransport` baut sich intern eine eigene einthreadige `tokio`-Laufzeit
//! und überbrückt ihren synchronen `send_batch`/`flush`-Vertrag per
//! `Runtime::block_on` (siehe dortige Moduldoku). Ein `block_on`-Aufruf
//! **innerhalb** einer bereits laufenden `tokio`-Runtime panickt — kein
//! Fehlerwert, ein Absturz. [`build`] wird deshalb ausschließlich synchron
//! aufgerufen, **bevor** `gateway::run` seine eigene `tokio`-Runtime betritt
//! (`Builder::new_current_thread().build().block_on(supervise(..))`). Aus
//! async-Code heraus darf `flush()` nur über `tokio::task::spawn_blocking`
//! laufen, weil er bei aktivem OTLP über `block_on` zustellt; `record()`
//! berührt das Netz nie (OTLP puffert nur) und schreibt lediglich eine
//! JSONL-Zeile. Der periodische Flush läuft auf einem eigenen Thread außerhalb
//! jeder Runtime (siehe Abschnitt „Periodischer Flush").
//!
//! # Warum `security.*`/`warden.*` strukturell geschützt bleiben
//! [`build`] konstruiert den [`harw_observe::RoutingSink`] **immer** mit dem
//! [`FileSink`] als `default_sink` und hängt **nie** ein Exportziel an: der
//! einzige öffentliche Weg dorthin ist
//! [`harw_observe::SecurityExportGate::release`] mit einem
//! [`harw_observe::ExportApproval`] (die interne Registrierung
//! `approve_export` ist in `harw-observe` nur `pub(crate)`). Dieses Modul
//! öffnet das Tor nicht — es bleibt in seiner Voreinstellung geschlossen, und
//! es gibt hier bewusst keinen Konfigurationsschlüssel dafür. Ein Öffnen
//! bräuchte einen echten `ApprovalStore`-Beleg samt Audit-Ereignis (siehe
//! `harw_observe::routing`-Moduldoku, Abschnitte „Wie eine Bestätigung
//! aussieht" und „Grenze des Tors"). Prometheus- und OTLP-Sinks werden, wenn
//! überhaupt, ausschließlich unter dem Präfix `"app."` geroutet
//! ([`harw_observe::RoutingSink::route`]) — `security.*`/`warden.*` sind kein
//! Präfix von `"app."` und erreichen damit **nie** einen dieser beiden Sinks,
//! unabhängig davon, ob sie eingeschaltet sind. Das ist keine Konvention, die
//! man vergessen könnte: `RoutingSink::record` prüft geschützte Namensräume
//! vor jeder Routing-Tabelle (siehe dortige Moduldoku).
//!
//! # DoD-Null-Serien und das Produzenten-Handle
//! Nachdem **alle** Routen hängen, übergibt [`build`] den `RoutingSink` als
//! Wert an [`harw_observe::DispatchMetrics::install`] (Rolle
//! [`harw_observe::DodEmitter::Dispatch`]). Das emittiert die sieben
//! Null-Serien dieser Rolle (`dod_triage_verdicts_total`,
//! `human_overrides_total`, `unbound_verdict_total`,
//! `breaker_trips_total` je Partitionsart, `untriaged_backlog`) an **jedes**
//! Standardziel, nie an ein Exportziel, und liefert genau ein
//! Produzenten-Handle ([`TelemetrySinks::dod_metrics`]). Das Handle kennt nur
//! die Meldemethoden dieser Rolle; eine Sentinel- oder Warden-Meldung über
//! das Gateway ist ein Compile-Fehler.
//!
//! Warum nur diese sieben (Abweichung von der Owner-Vorgabe „alle neun",
//! Owner-Entscheidung offen): eine Null-Serie behauptet „Produzent
//! verdrahtet, bisher kein Ereignis". Das Gateway ist die Dispatch-Seite —
//! `harw-dod-watch` läuft nach README O5 als eigene User-Unit und meldet
//! über seine Gateway-Verbindung, die Verdict-Bindung und der Override-Pfad
//! laufen hier. `sensor_parse_errors_total` und `warden_unaudited_actions`
//! entstehen nur im Sentinel bzw. im Warden; deren Kompositionsstellen
//! installieren dort [`harw_observe::SensorMetrics`] bzw.
//! [`harw_observe::WardenMetrics`]. Emittierte das Gateway sie mit `0`,
//! sähe ein nie verdrahteter Sentinel oder Warden wie „nichts passiert"
//! aus. Aus demselben Grund registriert das Gateway den Verstoß-Nullzähler
//! [`harw_observe::WARDEN_UNAUDITED_VIOLATIONS`] **nicht** (es besitzt auch
//! keine `NullCounterRegistry`).
//!
//! **Offener Stand:** Solange die Produzenten (DodWatch-Meldungen über die
//! Gateway-Verbindung, `accept_security_verdict`, Override-Pfad)
//! [`TelemetrySinks::dod_metrics`] noch nicht aufrufen, bewegt im Gateway
//! nichts diese sieben Serien. Bis dahin belegt ihr Nullwert nur, dass die
//! Kompositionsstelle verdrahtet ist, nicht, dass ein Produzent angeschlossen
//! ist.
//!
//! Beim Aufbau flusht [`build`] nur den lokalen File-Sink; Prometheus ist
//! rein speicherresident. Der OTLP-Sink puffert die Nullwerte und stellt sie
//! mit dem ersten Takt des periodischen Flushs zu, damit der Prozessstart nie
//! an der Erreichbarkeit des Collectors hängt.
//!
//! # Periodischer Flush
//! `OtlpSink::record` puffert nur (Obergrenze `max_buffer`, danach werden
//! neue Punkte verworfen). Ohne laufenden Flush sähe das OTLP-Ziel Werte
//! erst beim Herunterfahren. [`build`] startet deshalb [`PeriodicFlush`]:
//! einen eigenen Thread `harw-telemetry-flush`, der alle
//! [`TELEMETRY_FLUSH_PERIOD`] den zusammengesetzten Sink flusht (erster Takt
//! nach einer Periode, nicht beim Start). Er läuft außerhalb jeder
//! `tokio`-Runtime, `HttpTransport::block_on` ist dort zulässig, und er
//! braucht keinen Zweig in `gateway::supervise`. [`TelemetrySinks`] besitzt
//! ihn; fällt der Wert (in `gateway::run` nach dem abschließenden
//! `flush()`), endet der Thread sofort und wird eingesammelt. Ein Collector,
//! der nie antwortet, hält höchstens diesen einen Thread und damit das
//! Einsammeln auf, bis `HttpTransport` Zeitgrenzen bekommt.
//!
//! **Nachlieferung an OTLP:** `OtlpSink::flush` leert seinen Puffer vor dem
//! Senden; ein gescheiterter Stapel ist verloren und erhöht nur
//! `send_failure_count`. Wären die DoD-Stände darin (typisch: Collector beim
//! Start noch nicht erreichbar), zeigte das OTLP-Ziel bis zum nächsten
//! Ereignis keine Serie — wieder „null" nicht von „nie verdrahtet"
//! unterscheidbar. Hatte ein Flush OTLP-Sendefehler, meldet der nächste Takt
//! deshalb vorher die aktuellen Stände aller sieben Serien erneut, nur an den
//! OTLP-Sink ([`harw_observe::DodMetrics::republish_to`]), und wiederholt das,
//! bis ein Flush ohne Sendefehler durchläuft. Kumulative Stände machen die
//! Wiederholung harmlos; File-Sink und Prometheus bekommen nichts doppelt.
//!
//! # Vorgabe: beide zusätzlichen Sinks aus
//! Ohne `--metrics-prometheus-port`/`--metrics-otlp-endpoint` bleibt
//! ausschließlich der File-Sink aktiv. Kein Endpunkt bindet, kein
//! `HttpTransport` wird gebaut, solange kein Operator eines der beiden Flags
//! setzt (`harw gateway --help` nennt beide Ziele).
//!
//! # `https://` wird verweigert, nicht stillschweigend heruntergestuft
//! `HttpTransport::new` lehnt einen `https://`-Endpunkt mit einem Fehler ab
//! (kein TLS in `harw-observe-otlp`, siehe dortige Moduldoku „TLS"). [`build`]
//! gibt diesen Fehler unverändert weiter: ein Startfehler bei einem falsch
//! konfigurierten Endpunkt ist besser als ein Sink, der still nichts sendet.
//!
//! # Nebenläufigkeit
//! Der zurückgegebene [`harw_observe::RoutingSink`] ist `Send + Sync` und
//! nach dem Aufbau unveränderlich (siehe dortige Moduldoku). Das
//! [`harw_observe::DispatchMetrics`]-Handle ist `Send + Sync` und
//! serialisiert seine Meldungen intern. [`SystemClock`] ist zustandslos.
//! [`PeriodicFlush`] flusht auf seinem eigenen Thread; ein gleichzeitiger
//! `flush()` eines anderen Aufrufers ist zulässig (die Sinks sperren selbst).
//!
//! # Fehler
//! [`build`] gibt [`TelemetryBuildError`] zurück. Ein Fehlschlag beim
//! Anlegen des Telemetrie-Verzeichnisses, beim Öffnen des File-Sinks, beim
//! Binden des Prometheus-Endpunkts, beim Bau des OTLP-`HttpTransport`s, beim
//! Installieren der DoD-Null-Serien oder beim Start des Flush-Threads bricht
//! den Aufbau ab, statt still auf `NullSink` zurückzufallen oder zu paniken.
//! Der Rest der Composition-Root (`gateway.rs`) arbeitet mit
//! `Result<_, String>`; dafür gibt es `From<TelemetryBuildError> for String`,
//! sodass `?` dort unverändert funktioniert.
//!
//! # Keine neue Fremdcrate
//! `harw_observe_otlp::HttpTransport` benutzt `hyper`/`hyper-util`/
//! `http-body-util`/`bytes`/`tokio` — dieselben Crates und Versionsangaben,
//! die `harw-mcp-server` und `harw-web` bereits im Baum auflösen. Dieses
//! Modul selbst zieht keine zusätzliche Abhängigkeit.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use harw_observe::{DispatchMetrics, ObserveError, RoutingSink, TelemetrySink};
use harw_observe_file::{FileSink, ObserveFileError};
use harw_observe_otlp::{HttpTransport, OtlpClock, OtlpConfig, OtlpError, OtlpSink, OtlpTransport};
use harw_observe_prom::{BindAddr, PromEndpoint, PromError};

use harw_retention::RetentionConfig;

use crate::cli::TelemetryArgs;

/// Obergrenze der `FileSink`-Rotationsdatei unter `<home>/telemetry`.
///
/// Ausreichend groß, um mehrere Betriebsstunden JSONL-Zeilen zu fassen, ohne
/// bei jedem Gateway-Start neu zu rotieren; endlich, damit ein vergessener
/// Prozess das Home-Verzeichnis nicht unbegrenzt füllt.
const FILE_SINK_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Abstand des periodischen Telemetrie-Flushs ([`PeriodicFlush`]).
///
/// # Description
/// Kurz genug, dass OTLP-Werte zeitnah ankommen und der OTLP-Puffer
/// (`max_buffer` = 10 000 Punkte) zwischen zwei Flushes nicht überläuft;
/// lang genug, dass das `fsync` des File-Sinks keine Last erzeugt.
pub(crate) const TELEMETRY_FLUSH_PERIOD: Duration = Duration::from_secs(30);

/// Obergrenze rotierter Telemetrie-Dateien aus der Retention-Klasse
/// `telemetry_rotated` (`[retention.telemetry_rotated] max_files`).
///
/// `None` (unbegrenzt), wenn die Klasse per Konfiguration abgeschaltet ist
/// oder kein `max_files` trägt.
pub(crate) fn telemetry_max_files(retention: &RetentionConfig) -> Option<usize> {
    let class = harw_retention::policy_for(retention, "telemetry_rotated")?;
    if !class.enabled {
        return None;
    }
    class.max_files.and_then(|n| usize::try_from(n).ok())
}

/// Ergebnis von [`build`]: der zusammengesetzte Sink, das
/// DoD-Produzenten-Handle, der periodische Flush und die Handles, deren
/// `Drop` einen laufenden Hintergrund-Endpunkt beendet.
///
/// # Description
/// `harw-cli` muss [`Self`] für die Lebensdauer des Prozesses halten —
/// [`PromEndpoint::bind`]s Hintergrund-Thread und der Thread des
/// [`PeriodicFlush`] stoppen und werden eingesammelt, sobald der Wert fällt
/// (siehe dortige Doku). Ein `TelemetrySinks`-Wert, der sofort verworfen
/// wird, beendet beide deshalb wieder, noch bevor sie etwas bedient haben —
/// das ist beabsichtigtes Verhalten (kein herrenloser Thread), kein Fehler.
/// `gateway::run` hält den Wert bis nach dem abschließenden `flush()`.
pub(crate) struct TelemetrySinks {
    /// Der zusammengesetzte Sink, an den Aufrufer Messwerte schicken.
    pub(crate) sink: Arc<dyn TelemetrySink>,
    /// Das eine Produzenten-Handle der Rolle
    /// [`harw_observe::DodEmitter::Dispatch`] in diesem Prozess, installiert
    /// über demselben `RoutingSink` wie [`Self::sink`]. Produzenten, die an
    /// das Gateway melden — Ereignisse, die `harw-dod-watch` über seine
    /// Gateway-Verbindung liefert (Breaker-Trips, Backlog, Verdicts),
    /// `accept_security_verdict` auf dem Wave-Pfad und der Override-Pfad —
    /// bekommen ein `Arc::clone` davon; nie ein zweites Handle bauen
    /// (getrennte Stände). Ein `harw-dod-watch` mit eigenem Sink darf
    /// dieselben Serien nicht zusätzlich emittieren. Die Methoden flushen
    /// nie, schreiben aber synchron in den File-Sink: aus async-Code über
    /// `tokio::task::spawn_blocking` aufrufen. Im OTLP-Ziel werden Stände
    /// mit dem nächsten Takt des [`PeriodicFlush`] sichtbar.
    // Noch ohne Produzenten im Gateway: bis `supervise` bzw. die
    // DodWatch-Anbindung `Arc::clone(&telemetry_sinks.dod_metrics)` an
    // `accept_security_verdict`, den Override-Pfad und die
    // DodWatch-Meldungen übergibt, liest nur der Kompositionstest das Feld.
    // Dann entfällt dieses `allow`.
    #[allow(dead_code)]
    pub(crate) dod_metrics: Arc<DispatchMetrics>,
    /// Der laufende periodische Flush; muss für die Prozesslaufzeit
    /// gehalten werden (siehe Typdoku).
    // Nur seines `Drop` wegen gehalten; Tests takten ihn zusätzlich direkt.
    #[allow(dead_code)]
    periodic_flush: PeriodicFlush,
    /// `Some(_)`, solange der Prometheus-Endpunkt gebunden ist; muss für die
    /// Prozesslaufzeit gehalten werden (siehe Typdoku).
    #[allow(dead_code)]
    prometheus_endpoint: Option<PromEndpoint>,
}

/// Fehler beim Aufbau der Telemetrie-Sinks in [`build`].
///
/// # Description
/// Jede Variante trägt den Fremdfehler als `source` und den Kontext, an dem
/// der Aufbau scheiterte. Die Texte entsprechen den früheren
/// `String`-Meldungen; `From<TelemetryBuildError> for String` hält die
/// Aufrufer mit `Result<_, String>` (`gateway::run`) unverändert.
#[derive(Debug)]
pub(crate) enum TelemetryBuildError {
    /// `<home>/telemetry` ließ sich nicht anlegen.
    TelemetryDir {
        /// Das Verzeichnis, das angelegt werden sollte.
        path: String,
        /// Der I/O-Fehler.
        source: std::io::Error,
    },
    /// Der File-Sink ließ sich nicht öffnen.
    FileSink {
        /// Das Verzeichnis des File-Sinks.
        path: String,
        /// Der Fehler aus `harw-observe-file`.
        source: ObserveFileError,
    },
    /// Der Prometheus-Endpunkt ließ sich nicht binden.
    Prometheus {
        /// Der angeforderte Loopback-Port.
        port: u16,
        /// Der Fehler aus `harw-observe-prom`.
        source: PromError,
    },
    /// Der OTLP-Transport ließ sich nicht bauen (z. B. `https://`).
    Otlp {
        /// Der konfigurierte Endpunkt.
        endpoint: String,
        /// Der Fehler aus `harw-observe-otlp`.
        source: OtlpError,
    },
    /// Die DoD-Null-Serien ließen sich nicht installieren (ungültige
    /// Registry oder unvollständig verteilt).
    DodZeroSeries {
        /// Der Fehler aus `harw_observe::DodMetrics::install`.
        source: ObserveError,
    },
    /// Der Thread des periodischen Flushs ließ sich nicht starten.
    FlushThread {
        /// Der I/O-Fehler aus `std::thread::Builder::spawn`.
        source: std::io::Error,
    },
}

impl std::fmt::Display for TelemetryBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TelemetryDir { path, source } => {
                write!(f, "Telemetrie-Verzeichnis {path}: {source}")
            }
            Self::FileSink { path, source } => write!(f, "File-Sink unter {path}: {source}"),
            Self::Prometheus { port, source } => {
                write!(f, "Prometheus-Endpunkt auf Port {port}: {source}")
            }
            Self::Otlp { endpoint, source } => write!(f, "OTLP-Endpunkt {endpoint}: {source}"),
            Self::DodZeroSeries { source } => write!(f, "DoD-Null-Serien: {source}"),
            Self::FlushThread { source } => write!(f, "Telemetrie-Flush-Thread: {source}"),
        }
    }
}

impl std::error::Error for TelemetryBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TelemetryDir { source, .. } => Some(source),
            Self::FileSink { source, .. } => Some(source),
            Self::Prometheus { source, .. } => Some(source),
            Self::Otlp { source, .. } => Some(source),
            Self::DodZeroSeries { source } => Some(source),
            Self::FlushThread { source } => Some(source),
        }
    }
}

// Brücke zur `Result<_, String>`-Fehlerform von `gateway::run`, damit
// `crate::observe::build(..)?` dort unverändert bleibt.
impl From<TelemetryBuildError> for String {
    fn from(error: TelemetryBuildError) -> Self {
        error.to_string()
    }
}

/// Baut den Composition-Root-Sink aus den `--metrics-*`-Flags von
/// `harw gateway`.
///
/// # Description
/// Muss außerhalb jeder laufenden `tokio`-Runtime aufgerufen werden, wenn
/// `--metrics-otlp-endpoint` gesetzt ist — siehe Moduldoc, Abschnitt „Warum
/// außerhalb der Runtime".
///
/// # Arguments
/// - `home` (`&Path`): HARW-Home; der File-Sink öffnet
///   [`harw_home::paths::telemetry_dir`] darunter.
/// - `args` (`&TelemetryArgs`): geparste `--metrics-prometheus-port`/
///   `--metrics-otlp-endpoint`-Flags.
/// - `retention` (`&RetentionConfig`): `[retention]`; Klasse
///   `telemetry_rotated` begrenzt die rotierten Dateien (Paar-Pruning).
///
/// # Returns
/// [`TelemetrySinks`] mit dem File-Sink als `default_sink`, optional
/// Prometheus/OTLP unter dem Präfix `"app."` geroutet (siehe Moduldoc), den
/// an alle Standardziele emittierten Null-Serien der Rolle
/// [`harw_observe::DodEmitter::Dispatch`] (im File-Sink bereits geflusht,
/// im OTLP-Sink gepuffert bis zum ersten Takt des periodischen Flushs), dem
/// Produzenten-Handle [`TelemetrySinks::dod_metrics`] und dem laufenden
/// [`PeriodicFlush`] (alle [`TELEMETRY_FLUSH_PERIOD`]). Das
/// `SecurityExportGate` bleibt geschlossen: kein Exportziel für
/// `security.*`/`warden.*`.
///
/// # Errors
/// [`TelemetryBuildError`], wenn das Telemetrie-Verzeichnis nicht angelegt
/// oder der File-Sink nicht geöffnet werden kann
/// ([`harw_observe_file::ObserveFileError`]), der Prometheus-Endpunkt nicht
/// binden kann ([`harw_observe_prom::PromError`]), der OTLP-Endpunkt keine
/// gültige `http://`-URI ist ([`harw_observe_otlp::OtlpError`] — dazu
/// zählt jedes `https://`, siehe Moduldoc), die DoD-Null-Serien sich
/// nicht installieren lassen ([`harw_observe::ObserveError`]) oder der
/// Flush-Thread nicht startet (`std::io::Error`).
///
/// # Concurrency
/// Synchroner Aufbau; der zurückgegebene Sink ist danach unveränderlich
/// und über `Arc` teilbar. Darf nicht aus einer laufenden `tokio`-Runtime
/// heraus aufgerufen werden (siehe Moduldoc), weil `HttpTransport::new` eine
/// eigene Runtime baut. Kontaktiert den OTLP-Collector nicht; das tut erst
/// der Flush-Thread, frühestens nach einer Periode.
pub(crate) fn build(
    home: &Path,
    args: &TelemetryArgs,
    retention: &RetentionConfig,
) -> Result<TelemetrySinks, TelemetryBuildError> {
    build_with_otlp_transport(
        home,
        args,
        retention,
        TELEMETRY_FLUSH_PERIOD,
        |config: &OtlpConfig| {
            HttpTransport::new(config)
                .map(|transport| Arc::new(transport) as Arc<dyn OtlpTransport>)
        },
    )
}

// Der eigentliche Aufbau von `build`. `otlp_transport` baut den Transport
// nur, wenn `--metrics-otlp-endpoint` gesetzt ist; in Produktion ist das
// `HttpTransport::new`, im Kompositionstest ein `RecordingTransport` (kein
// Netz). `flush_period` ist in Produktion `TELEMETRY_FLUSH_PERIOD`; Tests
// wählen eine Periode, die während des Tests nie abläuft, und takten den
// Flush selbst. Alles andere — Ziele, Routen, Null-Serien, Handle,
// Flush-Thread — ist identisch.
fn build_with_otlp_transport(
    home: &Path,
    args: &TelemetryArgs,
    retention: &RetentionConfig,
    flush_period: Duration,
    otlp_transport: impl FnOnce(&OtlpConfig) -> Result<Arc<dyn OtlpTransport>, OtlpError>,
) -> Result<TelemetrySinks, TelemetryBuildError> {
    let telemetry_dir = harw_home::paths::telemetry_dir(home);
    std::fs::create_dir_all(&telemetry_dir).map_err(|source| {
        TelemetryBuildError::TelemetryDir {
            path: telemetry_dir.display().to_string(),
            source,
        }
    })?;
    let file_sink: Arc<dyn TelemetrySink> = Arc::new(
        FileSink::open(&telemetry_dir, FILE_SINK_MAX_BYTES)
            .map_err(|source| TelemetryBuildError::FileSink {
                path: telemetry_dir.display().to_string(),
                source,
            })?
            .with_max_rotated_files(telemetry_max_files(retention)),
    );

    let mut routing = RoutingSink::new(Arc::clone(&file_sink));
    let mut extra_targets: Vec<Arc<dyn TelemetrySink>> = Vec::new();
    let mut prometheus_endpoint = None;
    let mut otlp_sink: Option<Arc<OtlpSink>> = None;

    if let Some(port) = args.metrics_prometheus_port {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(port))
            .map_err(|source| TelemetryBuildError::Prometheus { port, source })?;
        extra_targets.push(endpoint.sink() as Arc<dyn TelemetrySink>);
        prometheus_endpoint = Some(endpoint);
        tracing::info!(port, "observe.prometheus.bound");
    }

    if let Some(endpoint) = args.metrics_otlp_endpoint.as_deref() {
        let config = OtlpConfig {
            endpoint: endpoint.to_owned(),
            headers: Vec::new(),
            batch_size: 100,
            max_buffer: 10_000,
            resource_service_name: "harw".to_owned(),
        };
        let transport = otlp_transport(&config).map_err(|source| TelemetryBuildError::Otlp {
            endpoint: endpoint.to_owned(),
            source,
        })?;
        let clock: Arc<dyn OtlpClock> = Arc::new(SystemClock);
        let otlp = Arc::new(OtlpSink::new(transport, clock, config));
        extra_targets.push(Arc::clone(&otlp) as Arc<dyn TelemetrySink>);
        otlp_sink = Some(otlp);
        tracing::info!(endpoint, "observe.otlp.configured");
    }

    // Genau ein zusätzliches Ziel: direkt unter "app." routen. Zwei
    // zusätzliche Ziele: zu einem Fanout zusammenfassen, damit beide
    // dieselbe Route erreichen — `RoutingSink::route` bindet pro Präfix nur
    // ein Ziel (siehe dortige Doku zur Präzedenz).
    match extra_targets.len() {
        0 => {}
        1 => {
            if let Some(only) = extra_targets.into_iter().next() {
                routing.route("app.", only);
            }
        }
        _ => {
            routing.route(
                "app.",
                Arc::new(FanoutSink {
                    targets: extra_targets,
                }),
            );
        }
    }

    // Ab hier hängen alle Routen. `install` nimmt den Sink als Wert, danach
    // ist die Tabelle unveränderlich; die Null-Serien der Dispatch-Rolle
    // erreichen so jedes Ziel, das je Werte bekommt. Kein Exportziel ist
    // registriert (Tor geschlossen), und `install` bedient Exportziele
    // ohnehin nie.
    let (routing, dod_metrics) = DispatchMetrics::install(routing)
        .map_err(|source| TelemetryBuildError::DodZeroSeries { source })?;
    let dod_metrics = Arc::new(dod_metrics);
    // Nur das lokale Ziel flushen (Prometheus ist speicherresident). Ein
    // Flush über `routing` stellte auch den OTLP-Stapel zu und hinge damit
    // am Collector; OTLP bekommt die Nullwerte mit dem ersten Flush-Takt.
    file_sink.flush();
    tracing::info!(
        emitter = dod_metrics.emitter().as_str(),
        "observe.dod_zero_series.emitted"
    );

    let sink: Arc<dyn TelemetrySink> = routing;
    let otlp_repair = otlp_sink.map(|otlp| OtlpRepair {
        otlp,
        dod_metrics: Arc::clone(&dod_metrics),
    });
    let ticker = Arc::new(FlushTicker::new(Arc::clone(&sink), otlp_repair));
    let periodic_flush = PeriodicFlush::start(ticker, flush_period)
        .map_err(|source| TelemetryBuildError::FlushThread { source })?;

    Ok(TelemetrySinks {
        sink,
        dod_metrics,
        periodic_flush,
        prometheus_endpoint,
    })
}

/// Was ein Flush-Takt braucht, um verlorene DoD-Stände an OTLP
/// nachzuliefern (siehe Moduldoc, „Nachlieferung an OTLP").
#[derive(Debug)]
struct OtlpRepair {
    /// Der OTLP-Sink selbst (nicht der `RoutingSink`): Ziel der
    /// Nachlieferung und Quelle von `send_failure_count`.
    otlp: Arc<OtlpSink>,
    /// Das Produzenten-Handle, dessen Stände nachgeliefert werden.
    dod_metrics: Arc<DispatchMetrics>,
}

/// Ein Takt des periodischen Flushs.
///
/// # Description
/// Flusht den zusammengesetzten Sink. Ist OTLP aktiv, liefert der Takt
/// vorher die DoD-Stände an den OTLP-Sink nach, wenn der vorige Takt
/// OTLP-Sendefehler hatte, und merkt sich, ob dieser Flush welche hatte.
///
/// # Concurrency
/// `Send + Sync`; im Betrieb taktet nur der Thread des [`PeriodicFlush`].
#[derive(Debug)]
struct FlushTicker {
    /// Derselbe Sink wie [`TelemetrySinks::sink`].
    sink: Arc<dyn TelemetrySink>,
    /// `Some(_)` genau dann, wenn OTLP konfiguriert ist.
    otlp_repair: Option<OtlpRepair>,
    /// `true`, solange der zuletzt beobachtete Flush OTLP-Sendefehler hatte.
    republish_pending: AtomicBool,
}

impl FlushTicker {
    // Baut einen Takt ohne ausstehende Nachlieferung: die Nullwerte liegen
    // nach `install` ohnehin im OTLP-Puffer.
    fn new(sink: Arc<dyn TelemetrySink>, otlp_repair: Option<OtlpRepair>) -> Self {
        Self {
            sink,
            otlp_repair,
            republish_pending: AtomicBool::new(false),
        }
    }

    // Ein Takt: gegebenenfalls nachliefern, dann flushen, dann den Erfolg
    // des OTLP-Versands festhalten. Scheitert nie (Sinks scheitern still und
    // zählen selbst).
    fn tick(&self) {
        let Some(repair) = &self.otlp_repair else {
            self.sink.flush();
            return;
        };
        let otlp: &OtlpSink = &repair.otlp;
        let pending = self.republish_pending.load(Ordering::Relaxed);
        if pending {
            let series = repair.dod_metrics.republish_to(otlp);
            tracing::debug!(series, "observe.otlp.dod_totals_republished");
        }
        let failures_before = otlp.send_failure_count();
        self.sink.flush();
        let failed = otlp.send_failure_count() > failures_before;
        self.republish_pending.store(failed, Ordering::Relaxed);
        match (pending, failed) {
            (false, true) => tracing::warn!(republish_next_tick = true, "observe.otlp.send_failed"),
            (true, false) => tracing::info!("observe.otlp.send_recovered"),
            (false, false) | (true, true) => {}
        }
    }
}

/// Der laufende periodische Flush: ein eigener Thread, den `Drop` stoppt
/// und einsammelt.
///
/// # Description
/// Der Thread `harw-telemetry-flush` wartet je Takt eine Periode und ruft
/// dann [`FlushTicker::tick`]; der erste Flush kommt also nach einer
/// Periode, nie beim Start. Er läuft außerhalb jeder `tokio`-Runtime (siehe
/// Moduldoc, „Periodischer Flush"). `Drop` lässt den Stopp-Sender fallen,
/// was die Wartezeit sofort beendet, und wartet einen laufenden Flush ab.
///
/// # Concurrency
/// Der Takt ist über `Arc` mit dem Thread geteilt; Tests takten ihn
/// zusätzlich direkt (`tick_now`), ohne dass der Thread in einer langen
/// Periode dazwischenkommt.
#[derive(Debug)]
struct PeriodicFlush {
    // Teilt den Takt mit dem Thread; außerhalb von Tests nur dort benutzt.
    #[cfg_attr(not(test), allow(dead_code))]
    ticker: Arc<FlushTicker>,
    // Fällt der Sender, endet die Warteschleife des Threads sofort.
    stop: Option<mpsc::Sender<()>>,
    // Der Flush-Thread; `None` nach dem Einsammeln.
    thread: Option<JoinHandle<()>>,
}

impl PeriodicFlush {
    // Startet den Thread; `period == 0` wird auf eine Millisekunde
    // angehoben.
    fn start(ticker: Arc<FlushTicker>, period: Duration) -> Result<Self, std::io::Error> {
        let (stop, stop_signal) = mpsc::channel::<()>();
        let worker = Arc::clone(&ticker);
        let period = period.max(Duration::from_millis(1));
        let thread = std::thread::Builder::new()
            .name("harw-telemetry-flush".to_owned())
            .spawn(move || run_flush_loop(&worker, &stop_signal, period))?;
        Ok(Self {
            ticker,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    // Ein Takt sofort, auf dem aufrufenden Thread (nur Tests).
    #[cfg(test)]
    fn tick_now(&self) {
        self.ticker.tick();
    }
}

// Die Schleife des Flush-Threads: je Periode ein Takt, bis der Stopp-Sender
// fällt (oder ein Stopp gesendet wird).
fn run_flush_loop(ticker: &FlushTicker, stop_signal: &mpsc::Receiver<()>, period: Duration) {
    loop {
        match stop_signal.recv_timeout(period) {
            Err(RecvTimeoutError::Timeout) => ticker.tick(),
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl Drop for PeriodicFlush {
    fn drop(&mut self) {
        // Den Sender fallen lassen weckt `recv_timeout` sofort mit
        // `Disconnected`; danach den Thread einsammeln.
        drop(self.stop.take());
        let joined = self.thread.take().map(JoinHandle::join);
        if matches!(joined, Some(Err(_))) {
            tracing::warn!("observe.periodic_flush.thread_panicked");
        }
    }
}

/// Verteilt jeden Messwert unverändert an mehrere Ziel-Sinks.
///
/// # Description
/// Wird ausschließlich gebraucht, wenn sowohl Prometheus als auch OTLP
/// gleichzeitig aktiv sind — [`harw_observe::RoutingSink::route`] bindet pro
/// Präfix genau ein Ziel, dieser Typ bündelt mehrere dahinter.
#[derive(Debug)]
struct FanoutSink {
    targets: Vec<Arc<dyn TelemetrySink>>,
}

impl TelemetrySink for FanoutSink {
    fn record(
        &self,
        key: &harw_observe::MetricKey,
        value: harw_observe::MetricValue,
        labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
    ) {
        for target in &self.targets {
            target.record(key, value, labels);
        }
    }

    fn flush(&self) {
        for target in &self.targets {
            target.flush();
        }
    }

    fn name(&self) -> &'static str {
        "fanout"
    }
}

/// Echte Systemuhr für [`harw_observe_otlp::OtlpSink`] — siehe dortige
/// `clock`-Moduldoku, Abschnitt „Keine Systemuhr": die Crate selbst ruft nie
/// `jiff::Timestamp::now()` auf, das liefert diese Kompositionsstelle nach.
#[derive(Debug, Clone, Copy)]
struct SystemClock;

impl OtlpClock for SystemClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, some_or};
    use harw_observe::{
        BreakerPartitionKind, Cardinality, MetricKey, MetricKind, MetricValue, Unit,
    };
    use harw_observe_otlp::RecordingTransport;

    const KEY: MetricKey = MetricKey {
        name: "x",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    #[derive(Debug, Default)]
    struct RecordingSink {
        records: std::sync::Mutex<Vec<MetricKey>>,
    }

    impl TelemetrySink for RecordingSink {
        fn record(
            &self,
            key: &MetricKey,
            _value: MetricValue,
            _labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
        ) {
            self.records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(*key);
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[test]
    fn test_system_clock_returns_a_recent_timestamp() {
        // Kein Mock nötig: `jiff::Timestamp::now()` ist unfehlbar und die
        // einzige Aufgabe von `SystemClock` (siehe Moduldoc „Keine
        // Systemuhr" in `harw-observe-otlp`). Diese Behauptung prüft nur,
        // dass die Uhr überhaupt läuft, nicht ihre Genauigkeit.
        let before = jiff::Timestamp::now();
        let observed = SystemClock.now();
        let after = jiff::Timestamp::now();
        assert!(observed >= before && observed <= after);
    }

    #[test]
    fn test_fanout_sink_forwards_record_and_flush_to_every_target() {
        let a = Arc::new(RecordingSink::default());
        let b = Arc::new(RecordingSink::default());
        let fanout = FanoutSink {
            targets: vec![
                Arc::clone(&a) as Arc<dyn TelemetrySink>,
                Arc::clone(&b) as Arc<dyn TelemetrySink>,
            ],
        };

        fanout.record(&KEY, MetricValue::Count(1), &[]);

        assert_eq!(a.records.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        assert_eq!(b.records.lock().unwrap_or_else(|p| p.into_inner()).len(), 1);
        assert_eq!(fanout.name(), "fanout");
    }

    // Der wichtigste Test dieses Knotens: eine `security.*`-Metrik erreicht
    // den zusätzlichen (in der Praxis: OTLP- oder Prometheus-)Sink nie —
    // die Routing-Tabelle bindet zusätzliche Ziele ausschließlich unter dem
    // Präfix `"app."`, und `RoutingSink` liefert geschützte Namensräume nie
    // an einen gewöhnlichen `route()`-Eintrag aus (siehe
    // `harw_observe::routing`-Moduldoku). Dieser Test öffnet keinen echten
    // Socket: `additional_target` steht hier für "irgendein zusätzliches
    // Ziel" (Prometheus- oder OTLP-Sink), genau wie es [`build`] verdrahtet
    // — sein `record()` wird für `security.*` nie aufgerufen.
    #[test]
    fn test_security_metric_never_reaches_the_additional_target() {
        let file = Arc::new(RecordingSink::default());
        let additional_target = Arc::new(RecordingSink::default());
        let mut routing = RoutingSink::new(Arc::clone(&file) as Arc<dyn TelemetrySink>);
        routing.route(
            "app.",
            Arc::clone(&additional_target) as Arc<dyn TelemetrySink>,
        );

        const SECURITY_KEY: MetricKey = MetricKey {
            name: "security.finding_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        };
        routing.record(&SECURITY_KEY, MetricValue::Count(1), &[]);

        assert_eq!(
            additional_target
                .records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            0,
            "security.* must never reach the app.-routed additional sink"
        );
        assert_eq!(
            file.records.lock().unwrap_or_else(|p| p.into_inner()).len(),
            1,
            "security.* must durably reach the file sink"
        );
    }

    #[test]
    fn test_ordinary_app_metric_reaches_the_additional_target_via_route() {
        let file = Arc::new(RecordingSink::default());
        let target = Arc::new(RecordingSink::default());
        let mut routing = RoutingSink::new(Arc::clone(&file) as Arc<dyn TelemetrySink>);
        routing.route("app.", Arc::clone(&target) as Arc<dyn TelemetrySink>);

        const APP_KEY: MetricKey = MetricKey {
            name: "app.gateway_started_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        };
        routing.record(&APP_KEY, MetricValue::Count(1), &[]);

        assert_eq!(
            target
                .records
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            1
        );
    }

    #[test]
    fn test_build_without_any_flag_activates_only_the_file_sink() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(|error| TestError::Unexpected(format!("tempdir: {error}")))?;
        let args = TelemetryArgs {
            metrics_prometheus_port: None,
            metrics_otlp_endpoint: None,
        };

        let sinks = build(home.path(), &args, &RetentionConfig::default()).map_err(|error| {
            TestError::Unexpected(format!("build without flags must succeed: {error}"))
        })?;

        assert!(
            sinks.prometheus_endpoint.is_none(),
            "no --metrics-prometheus-port must bind no endpoint"
        );
        assert!(
            sinks.periodic_flush.ticker.otlp_repair.is_none(),
            "no --metrics-otlp-endpoint must arm no OTLP repair"
        );
        assert_eq!(sinks.sink.name(), "routing");
        Ok(())
    }

    #[test]
    fn test_telemetry_max_files_follows_class_default_and_config() {
        let mut cfg = RetentionConfig::default();
        assert_eq!(telemetry_max_files(&cfg), Some(20));
        cfg.telemetry_rotated.max_files = Some(3);
        assert_eq!(telemetry_max_files(&cfg), Some(3));
        cfg.telemetry_rotated.enabled = Some(false);
        assert_eq!(telemetry_max_files(&cfg), None);
    }

    // ── Kompositionstest: DoD-Null-Serien und Produzenten-Handle ────────

    /// Ein beobachteter Messpunkt eines Ziels: Name, `partition`-Label
    /// (falls vorhanden) und Zahlenwert.
    #[derive(Debug, Clone, PartialEq)]
    struct SeriesPoint {
        name: String,
        partition: Option<String>,
        value: f64,
    }

    /// Die sieben erwarteten Null-Serien der Dispatch-Rolle (Name,
    /// Partition), unabhängig von der Registry von Hand aufgezählt (5 Namen,
    /// `breaker_trips_total` × 3).
    const EXPECTED_ZERO_SERIES: [(&str, Option<&str>); 7] = [
        ("dod_triage_verdicts_total", None),
        ("human_overrides_total", None),
        ("unbound_verdict_total", None),
        ("breaker_trips_total", Some("uid")),
        ("breaker_trips_total", Some("cgroup")),
        ("breaker_trips_total", Some("sensor")),
        ("untriaged_backlog", None),
    ];

    /// Serien fremder Rollen (Sentinel, Warden): das Gateway emittiert sie
    /// nie.
    const FOREIGN_ROLE_SERIES: [&str; 2] =
        ["sensor_parse_errors_total", "warden_unaudited_actions"];

    const SECURITY_PROBE: MetricKey = MetricKey {
        name: "security.finding_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const OTLP_TEST_ENDPOINT: &str = "http://127.0.0.1:4318/v1/metrics";

    /// Eine Flush-Periode, die während eines Tests nie abläuft: der
    /// Flush-Thread läuft, taktet aber nicht dazwischen; die Tests takten
    /// selbst (`tick_now`).
    const TEST_FLUSH_PERIOD: Duration = Duration::from_secs(3600);

    // Baut die Produktions-Komposition mit OTLP über einen aufzeichnenden
    // Transport (kein Netz) und optional Prometheus auf einem freien
    // Loopback-Port.
    fn build_recorded(
        home: &Path,
        prometheus: bool,
    ) -> TestResult<(TelemetrySinks, Arc<RecordingTransport>)> {
        let args = TelemetryArgs {
            metrics_prometheus_port: prometheus.then_some(0),
            metrics_otlp_endpoint: Some(OTLP_TEST_ENDPOINT.to_owned()),
        };
        let recorder = Arc::new(RecordingTransport::new());
        let injected = Arc::clone(&recorder);
        let sinks = build_with_otlp_transport(
            home,
            &args,
            &RetentionConfig::default(),
            TEST_FLUSH_PERIOD,
            move |config: &OtlpConfig| {
                assert_eq!(config.endpoint, OTLP_TEST_ENDPOINT);
                Ok(injected as Arc<dyn OtlpTransport>)
            },
        )
        .map_err(|error| TestError::Unexpected(format!("composition build: {error}")))?;
        Ok((sinks, recorder))
    }

    // Liest alle Zeilen der aktiven JSONL-Datei des File-Sinks.
    fn file_points(home: &Path) -> TestResult<Vec<SeriesPoint>> {
        let path = harw_home::paths::telemetry_dir(home).join("active.jsonl");
        let text = std::fs::read_to_string(&path).map_err(ctx("read telemetry active.jsonl"))?;
        let mut points = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let row: serde_json::Value =
                serde_json::from_str(line).map_err(ctx("parse telemetry jsonl line"))?;
            let name = some_or(
                row.get("metric").and_then(serde_json::Value::as_str),
                "jsonl metric field",
            )?;
            let value = some_or(
                row.get("value").and_then(serde_json::Value::as_f64),
                "jsonl value field",
            )?;
            let partition = row
                .get("labels")
                .and_then(serde_json::Value::as_array)
                .and_then(|labels| {
                    labels.iter().find_map(|pair| {
                        let pair = pair.as_array()?;
                        if pair.first()?.as_str()? != "partition" {
                            return None;
                        }
                        pair.get(1)?.as_str().map(str::to_owned)
                    })
                });
            points.push(SeriesPoint {
                name: name.to_owned(),
                partition,
                value,
            });
        }
        Ok(points)
    }

    // Zerlegt die Prometheus-Textausgabe in Messpunkte (ohne `# TYPE`).
    fn prom_points(render: &str) -> TestResult<Vec<SeriesPoint>> {
        let mut points = Vec::new();
        for line in render
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let (series, value) = some_or(line.rsplit_once(' '), "prometheus sample value")?;
            let value: f64 = value
                .parse()
                .map_err(ctx("parse prometheus sample value"))?;
            let (name, partition) = match series.split_once('{') {
                Some((name, labels)) => (
                    name,
                    labels
                        .strip_prefix("partition=\"")
                        .and_then(|rest| rest.strip_suffix("\"}"))
                        .map(str::to_owned),
                ),
                None => (series, None),
            };
            points.push(SeriesPoint {
                name: name.to_owned(),
                partition,
                value,
            });
        }
        Ok(points)
    }

    // Das `partition`-Attribut eines OTLP-Datenpunkts, falls vorhanden.
    fn otlp_partition(point: &serde_json::Value) -> Option<String> {
        point
            .get("attributes")?
            .as_array()?
            .iter()
            .find_map(|attribute| {
                if attribute.get("key")?.as_str()? != "partition" {
                    return None;
                }
                attribute
                    .pointer("/value/stringValue")?
                    .as_str()
                    .map(str::to_owned)
            })
    }

    // Der Zahlenwert eines OTLP-Datenpunkts (`asInt` als String oder
    // `asDouble`).
    fn otlp_value(point: &serde_json::Value) -> Option<f64> {
        match point.get("asInt").and_then(serde_json::Value::as_str) {
            Some(int) => int.parse::<f64>().ok(),
            None => point.get("asDouble").and_then(serde_json::Value::as_f64),
        }
    }

    // Zerlegt alle zugestellten OTLP/JSON-Stapel in Messpunkte.
    fn otlp_points(batches: &[Vec<u8>]) -> TestResult<Vec<SeriesPoint>> {
        let empty: Vec<serde_json::Value> = Vec::new();
        let mut points = Vec::new();
        for batch in batches {
            let doc: serde_json::Value =
                serde_json::from_slice(batch).map_err(ctx("parse otlp batch"))?;
            let resources = doc
                .get("resourceMetrics")
                .and_then(serde_json::Value::as_array)
                .unwrap_or(&empty);
            for resource in resources {
                let scopes = resource
                    .get("scopeMetrics")
                    .and_then(serde_json::Value::as_array)
                    .unwrap_or(&empty);
                for scope in scopes {
                    let metrics = scope
                        .get("metrics")
                        .and_then(serde_json::Value::as_array)
                        .unwrap_or(&empty);
                    for metric in metrics {
                        let name = some_or(
                            metric.get("name").and_then(serde_json::Value::as_str),
                            "otlp metric name",
                        )?;
                        for section in ["sum", "gauge"] {
                            let data_points = metric
                                .get(section)
                                .and_then(|body| body.get("dataPoints"))
                                .and_then(serde_json::Value::as_array)
                                .unwrap_or(&empty);
                            for point in data_points {
                                points.push(SeriesPoint {
                                    name: name.to_owned(),
                                    partition: otlp_partition(point),
                                    value: some_or(otlp_value(point), "otlp data point value")?,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(points)
    }

    // Der zuletzt gemeldete Wert einer Serie in `points`.
    fn latest(points: &[SeriesPoint], name: &str, partition: Option<&str>) -> Option<f64> {
        points
            .iter()
            .rev()
            .find(|point| point.name == name && point.partition.as_deref() == partition)
            .map(|point| point.value)
    }

    // Vergleich zweier Messwerte ohne direkten `f64 ==`-Vergleich.
    fn same_value(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    // Prüft, dass `points` genau die sieben Null-Serien der Dispatch-Rolle
    // mit Wert 0 enthält (und damit keine Serie einer fremden Rolle).
    fn assert_exactly_the_zero_series(target: &'static str, points: &[SeriesPoint]) -> TestResult {
        if points.len() != EXPECTED_ZERO_SERIES.len() {
            return Err(TestError::Unexpected(format!(
                "{target}: expected exactly {} zero series, saw {} points",
                EXPECTED_ZERO_SERIES.len(),
                points.len()
            )));
        }
        for (name, partition) in EXPECTED_ZERO_SERIES {
            let Some(value) = latest(points, name, partition) else {
                return Err(TestError::Unexpected(format!(
                    "{target}: zero series {name} {partition:?} is missing"
                )));
            };
            if !same_value(value, 0.0) {
                return Err(TestError::Unexpected(format!(
                    "{target}: zero series {name} {partition:?} starts at {value}, not 0"
                )));
            }
        }
        Ok(())
    }

    // Prüft den zuletzt gemeldeten Wert einer Serie in einem Ziel.
    fn assert_latest(
        target: &'static str,
        points: &[SeriesPoint],
        name: &str,
        partition: Option<&str>,
        expected: f64,
    ) -> TestResult {
        match latest(points, name, partition) {
            Some(value) if same_value(value, expected) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "{target}: {name} {partition:?} expected {expected}, saw {other:?}"
            ))),
        }
    }

    // Prüft, dass keine Serie einer fremden Rolle in `points` auftaucht.
    fn assert_no_foreign_role_series(target: &'static str, points: &[SeriesPoint]) -> TestResult {
        for name in FOREIGN_ROLE_SERIES {
            if points.iter().any(|point| point.name == name) {
                return Err(TestError::Unexpected(format!(
                    "{target}: the gateway must not emit {name} (sentinel/warden role)"
                )));
            }
        }
        Ok(())
    }

    // Der wichtigste Test dieses Knotens (Owner-Review PR #168): der echte
    // Gateway-Aufbau mit allen drei Zielen (File, Prometheus auf einem
    // freien Loopback-Port, OTLP über einen aufzeichnenden Transport statt
    // HTTP) startet mit allen sieben Null-Serien der Dispatch-Rolle in jedem
    // Standardziel, ohne den OTLP-Collector beim Aufbau zu kontaktieren; der
    // erste Takt des Produktions-Flushs stellt sie an OTLP zu, und ein
    // echter Wert über das Produzenten-Handle erreicht jedes Ziel.
    // `security.*` erreicht bei geschlossenem Tor nur den File-Sink.
    #[test]
    fn test_gateway_composition_starts_with_all_zero_series_and_propagates_updates() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(|error| TestError::Unexpected(format!("tempdir: {error}")))?;
        let (sinks, recorder) = build_recorded(home.path(), true)?;
        let endpoint = some_or(sinks.prometheus_endpoint.as_ref(), "prometheus endpoint")?;
        let prom = endpoint.sink();

        // 1. Prozessstart: jede Serie der Rolle existiert in File und
        //    Prometheus mit Wert 0; der Aufbau hat den Collector nicht
        //    kontaktiert (kein Netz beim Start).
        let file_start = file_points(home.path())?;
        let prom_start = prom_points(&prom.render())?;
        assert_exactly_the_zero_series("file", &file_start)?;
        assert_exactly_the_zero_series("prometheus", &prom_start)?;
        assert_no_foreign_role_series("file", &file_start)?;
        assert_no_foreign_role_series("prometheus", &prom_start)?;
        assert!(
            recorder.sent_batches().is_empty(),
            "build must not contact the OTLP collector"
        );

        // 2. Erster Takt des Produktions-Flushs: OTLP bekommt die Nullwerte.
        sinks.periodic_flush.tick_now();
        let otlp_start = otlp_points(&recorder.sent_batches())?;
        assert_exactly_the_zero_series("otlp", &otlp_start)?;
        assert_no_foreign_role_series("otlp", &otlp_start)?;

        // 3. Echte Werte über das eine Produzenten-Handle (record_meta).
        sinks.dod_metrics.verdict_recorded();
        sinks.dod_metrics.breaker_trip(BreakerPartitionKind::Cgroup);
        sinks.dod_metrics.set_untriaged_backlog(4);
        // Eine geschützte Metrik über denselben Sink.
        sinks
            .sink
            .record(&SECURITY_PROBE, MetricValue::Count(1), &[]);
        // Nächster Takt des Produktions-Flushs (im Betrieb alle
        // `TELEMETRY_FLUSH_PERIOD` auf dem Flush-Thread).
        sinks.periodic_flush.tick_now();

        let file = file_points(home.path())?;
        let prometheus = prom_points(&prom.render())?;
        let otlp = otlp_points(&recorder.sent_batches())?;
        for (target, points) in [
            ("file", &file),
            ("prometheus", &prometheus),
            ("otlp", &otlp),
        ] {
            assert_latest(target, points, "dod_triage_verdicts_total", None, 1.0)?;
            assert_latest(target, points, "breaker_trips_total", Some("cgroup"), 1.0)?;
            assert_latest(target, points, "breaker_trips_total", Some("uid"), 0.0)?;
            assert_latest(target, points, "untriaged_backlog", None, 4.0)?;
            assert_latest(target, points, "human_overrides_total", None, 0.0)?;
            assert_no_foreign_role_series(target, points)?;
        }

        // 4. Tor geschlossen: `security.*` nur im File-Sink (durable),
        //    weder im Prometheus- noch im OTLP-Ziel.
        assert!(file.iter().any(|point| point.name == SECURITY_PROBE.name));
        assert!(
            !prometheus
                .iter()
                .any(|point| point.name == SECURITY_PROBE.name)
        );
        assert!(!otlp.iter().any(|point| point.name == SECURITY_PROBE.name));
        assert!(!prom.render().contains("security"));
        Ok(())
    }

    // Collector beim Start nicht erreichbar: `OtlpSink` verwirft den Stapel
    // mit den Nullwerten. Der Produktions-Flush liefert die aktuellen Stände
    // so lange an OTLP nach, bis ein Versand gelingt — danach nicht mehr.
    // File-Sink und Prometheus bekommen keine Wiederholung.
    #[test]
    fn test_gateway_composition_redelivers_otlp_zero_series_after_failed_sends() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(|error| TestError::Unexpected(format!("tempdir: {error}")))?;
        let (sinks, recorder) = build_recorded(home.path(), false)?;

        // Erster und zweiter Takt scheitern (Collector noch nicht da).
        recorder.fail_next_send(true);
        sinks.periodic_flush.tick_now();
        assert!(
            recorder.sent_batches().is_empty(),
            "the failed zero batch is dropped by OtlpSink"
        );
        recorder.fail_next_send(true);
        sinks.periodic_flush.tick_now();
        assert!(recorder.sent_batches().is_empty());

        // Dritter Takt: die sieben Serien kommen mit ihrem Stand (0) an.
        sinks.periodic_flush.tick_now();
        let batches = recorder.sent_batches();
        assert_eq!(batches.len(), 1, "exactly one redelivered batch");
        let otlp = otlp_points(&batches)?;
        assert_exactly_the_zero_series("otlp", &otlp)?;
        assert_no_foreign_role_series("otlp", &otlp)?;
        assert_exactly_the_zero_series("file", &file_points(home.path())?)?;

        // Nach einem erfolgreichen Versand keine Nachlieferung mehr: ein
        // echter Wert geht genau einmal hinaus ...
        sinks.dod_metrics.verdict_recorded();
        sinks.periodic_flush.tick_now();
        let batches = recorder.sent_batches();
        assert_eq!(batches.len(), 2);
        let last_batch = some_or(batches.last(), "last otlp batch")?;
        let last = otlp_points(std::slice::from_ref(last_batch))?;
        assert_eq!(last.len(), 1, "no republish after a successful send");
        assert_latest("otlp", &last, "dod_triage_verdicts_total", None, 1.0)?;
        // ... und ein Takt ohne neue Werte sendet nichts.
        sinks.periodic_flush.tick_now();
        assert_eq!(recorder.sent_batches().len(), 2);
        Ok(())
    }

    #[test]
    fn test_build_without_flags_emits_zero_series_and_handle_reaches_file_sink() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(|error| TestError::Unexpected(format!("tempdir: {error}")))?;
        let args = TelemetryArgs {
            metrics_prometheus_port: None,
            metrics_otlp_endpoint: None,
        };
        let sinks = build(home.path(), &args, &RetentionConfig::default())
            .map_err(|error| TestError::Unexpected(format!("build: {error}")))?;

        let start = file_points(home.path())?;
        assert_exactly_the_zero_series("file", &start)?;
        assert_no_foreign_role_series("file", &start)?;

        sinks.dod_metrics.human_override();
        sinks.dod_metrics.human_override();
        sinks.dod_metrics.unbound_verdict();
        let file = file_points(home.path())?;
        assert_latest("file", &file, "human_overrides_total", None, 2.0)?;
        assert_latest("file", &file, "unbound_verdict_total", None, 1.0)?;
        assert_latest("file", &file, "dod_triage_verdicts_total", None, 0.0)?;
        assert_no_foreign_role_series("file", &file)?;
        Ok(())
    }

    // Aufzeichnender Sink, der jeden `flush()` mit dem ausführenden Thread
    // festhält.
    #[derive(Debug, Default)]
    struct FlushThreadSink {
        flushes: std::sync::Mutex<Vec<std::thread::ThreadId>>,
    }

    impl FlushThreadSink {
        fn flush_threads(&self) -> Vec<std::thread::ThreadId> {
            self.flushes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    impl TelemetrySink for FlushThreadSink {
        fn record(
            &self,
            _key: &MetricKey,
            _value: MetricValue,
            _labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
        ) {
        }

        fn flush(&self) {
            self.flushes
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(std::thread::current().id());
        }

        fn name(&self) -> &'static str {
            "flush-thread"
        }
    }

    // Ein Takt ohne OTLP-Nachlieferung über `recorder`.
    fn plain_ticker(recorder: &Arc<FlushThreadSink>) -> Arc<FlushTicker> {
        Arc::new(FlushTicker::new(
            Arc::clone(recorder) as Arc<dyn TelemetrySink>,
            None,
        ))
    }

    #[test]
    fn test_flush_ticker_without_otlp_flushes_the_sink_once() {
        let recorder = Arc::new(FlushThreadSink::default());
        plain_ticker(&recorder).tick();
        assert_eq!(recorder.flush_threads().len(), 1);
    }

    #[test]
    fn test_periodic_flush_ticks_off_the_caller_thread_and_stops_on_drop() -> TestResult {
        let recorder = Arc::new(FlushThreadSink::default());
        // A zero period is raised to 1 ms and keeps ticking.
        let flush = PeriodicFlush::start(plain_ticker(&recorder), Duration::ZERO)
            .map_err(ctx("start flush thread"))?;
        let deadline = some_or(
            Instant::now().checked_add(Duration::from_secs(5)),
            "test deadline",
        )?;
        while recorder.flush_threads().len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(flush);

        let threads = recorder.flush_threads();
        assert!(threads.len() >= 2, "the thread flushes every period");
        assert!(
            threads
                .iter()
                .all(|thread| *thread != std::thread::current().id()),
            "the flush runs on its own thread"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            recorder.flush_threads().len(),
            threads.len(),
            "no flush after drop"
        );
        Ok(())
    }

    #[test]
    fn test_periodic_flush_waits_one_period_and_drop_stops_it_at_once() -> TestResult {
        let recorder = Arc::new(FlushThreadSink::default());
        let flush = PeriodicFlush::start(plain_ticker(&recorder), TEST_FLUSH_PERIOD)
            .map_err(ctx("start flush thread"))?;
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        drop(flush);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "drop must not wait for the period to elapse"
        );
        assert!(
            recorder.flush_threads().is_empty(),
            "the first tick comes after one period, never at start"
        );
        Ok(())
    }

    #[test]
    fn test_telemetry_flush_period_bounds_otlp_latency() {
        // Short enough for timely OTLP values and a bounded buffer, long
        // enough to keep the file sink's fsync cheap.
        let seconds = TELEMETRY_FLUSH_PERIOD.as_secs();
        assert!((1..=60).contains(&seconds), "{seconds} s");
    }

    #[test]
    fn test_build_rejects_https_otlp_with_a_typed_error() -> TestResult {
        let home = tempfile::tempdir()
            .map_err(|error| TestError::Unexpected(format!("tempdir: {error}")))?;
        let args = TelemetryArgs {
            metrics_prometheus_port: None,
            metrics_otlp_endpoint: Some("https://collector.example/v1/metrics".to_owned()),
        };
        let Err(error) = build(home.path(), &args, &RetentionConfig::default()) else {
            return Err(TestError::Unexpected(
                "an https:// OTLP endpoint must fail the build".to_owned(),
            ));
        };
        assert!(matches!(error, TelemetryBuildError::Otlp { .. }));
        assert!(std::error::Error::source(&error).is_some());
        let message = String::from(error);
        assert!(message.starts_with("OTLP-Endpunkt https://collector.example/v1/metrics: "));
        Ok(())
    }

    #[test]
    fn test_telemetry_build_error_display_for_zero_series_variant() {
        let incomplete = TelemetryBuildError::DodZeroSeries {
            source: ObserveError::IncompleteZeroSeries {
                emitted: 6,
                expected: 7,
            },
        };
        assert_eq!(
            incomplete.to_string(),
            "DoD-Null-Serien: zero series incomplete: 6 of 7 emitted to the default targets"
        );
        assert!(std::error::Error::source(&incomplete).is_some());

        let registry = TelemetryBuildError::DodZeroSeries {
            source: ObserveError::InvalidZeroSeries {
                name: "x_total",
                reason: "duplicate label set",
            },
        };
        assert_eq!(
            String::from(registry),
            "DoD-Null-Serien: zero series 'x_total' is invalid: duplicate label set"
        );
    }

    #[test]
    fn test_telemetry_build_error_display_for_flush_thread_variant() {
        let error = TelemetryBuildError::FlushThread {
            source: std::io::Error::other("no thread available"),
        };
        assert_eq!(
            error.to_string(),
            "Telemetrie-Flush-Thread: no thread available"
        );
        assert!(std::error::Error::source(&error).is_some());
    }
}

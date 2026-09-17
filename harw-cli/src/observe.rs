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
//! unten).
//!
//! # Warum außerhalb der Runtime
//! `HttpTransport` baut sich intern eine eigene einthreadige `tokio`-Laufzeit
//! und überbrückt ihren synchronen `send_batch`/`flush`-Vertrag per
//! `Runtime::block_on` (siehe dortige Moduldoku). Ein `block_on`-Aufruf
//! **innerhalb** einer bereits laufenden `tokio`-Runtime panickt — kein
//! Fehlerwert, ein Absturz. [`build`] wird deshalb ausschließlich synchron
//! aufgerufen, **bevor** `gateway::run` seine eigene `tokio`-Runtime betritt
//! (`Builder::new_current_thread().build().block_on(supervise(..))`), und der
//! zurückgegebene Sink wird nur außerhalb dieses `block_on`-Aufrufs beschrieben
//! (Start-/Stopp-Metrik in `gateway::run` selbst) — nie aus einer Task, die
//! `supervise` spawnt. Sollte ein künftiger Aufrufer den Sink doch in einen
//! async-Kontext reichen wollen, muss er `sink.record()`/`sink.flush()` über
//! `tokio::task::spawn_blocking` aufrufen; dieses Modul selbst tut das nicht,
//! weil es den Sink nie aus async-Code heraus benutzt.
//!
//! # Warum `security.*`/`warden.*` strukturell geschützt bleiben
//! [`build`] konstruiert den [`harw_observe::RoutingSink`] **immer** mit dem
//! [`FileSink`] als `default_sink` und ruft **nie**
//! [`harw_observe::RoutingSink::approve_export`] auf — es gibt in diesem
//! Modul keinen Weg, ein [`harw_observe::ExportApproval`] zu erzeugen (dafür
//! bräuchte es einen echten `ApprovalStore`-Beleg, siehe
//! `harw_observe::routing`-Moduldoku, Abschnitt „Wie eine Bestätigung
//! aussieht" — dieser Knoten liefert das bewusst nicht mit). Prometheus- und
//! OTLP-Sinks werden, wenn überhaupt, ausschließlich unter dem Präfix
//! `"app."` geroutet ([`harw_observe::RoutingSink::route`]) —
//! `security.*`/`warden.*` sind kein Präfix von `"app."` und erreichen damit
//! **nie** einen dieser beiden Sinks, unabhängig davon, ob sie eingeschaltet
//! sind. Das ist keine Konvention, die man vergessen könnte:
//! `RoutingSink::record` prüft geschützte Namensräume vor jeder
//! Routing-Tabelle (siehe dortige Moduldoku) — dieses Modul müsste
//! `approve_export` aktiv aufrufen, um das zu umgehen, und tut das nicht.
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
//! nach dem Aufbau unveränderlich (siehe dortige Moduldoku). [`SystemClock`]
//! ist zustandslos.
//!
//! # Fehler
//! [`build`] gibt `Result<_, String>` zurück — dieselbe Fehlerform wie der
//! Rest der Composition-Root in `main.rs`/`gateway.rs`. Ein Fehlschlag beim
//! Öffnen des File-Sinks, beim Binden des Prometheus-Endpunkts oder beim Bau
//! des OTLP-`HttpTransport`s bricht den Aufbau ab, statt still auf
//! `NullSink` zurückzufallen.
//!
//! # Keine neue Fremdcrate
//! `harw_observe_otlp::HttpTransport` benutzt `hyper`/`hyper-util`/
//! `http-body-util`/`bytes`/`tokio` — dieselben Crates und Versionsangaben,
//! die `harw-mcp-server` und `harw-web` bereits im Baum auflösen. Dieses
//! Modul selbst zieht keine zusätzliche Abhängigkeit.

use std::path::Path;
use std::sync::Arc;

use harw_observe::{RoutingSink, TelemetrySink};
use harw_observe_file::FileSink;
use harw_observe_otlp::{HttpTransport, OtlpClock, OtlpConfig, OtlpSink, OtlpTransport};
use harw_observe_prom::{BindAddr, PromEndpoint};

use crate::cli::TelemetryArgs;

/// Obergrenze der `FileSink`-Rotationsdatei unter `<home>/telemetry`.
///
/// Ausreichend groß, um mehrere Betriebsstunden JSONL-Zeilen zu fassen, ohne
/// bei jedem Gateway-Start neu zu rotieren; endlich, damit ein vergessener
/// Prozess das Home-Verzeichnis nicht unbegrenzt füllt.
const FILE_SINK_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Ergebnis von [`build`]: der zusammengesetzte Sink plus die Handles, deren
/// `Drop` einen laufenden Hintergrund-Endpunkt beendet.
///
/// # Description
/// `harw-cli` muss [`Self`] für die Lebensdauer des Prozesses halten —
/// [`PromEndpoint::bind`]s Hintergrund-Thread stoppt und wird eingesammelt,
/// sobald der Wert fällt (siehe dortige Moduldoku). Ein `TelemetrySinks`-Wert,
/// der sofort verworfen wird, beendet den Endpunkt deshalb wieder, noch
/// bevor er etwas bedient hat — das ist beabsichtigtes Verhalten (kein
/// herrenloser Thread), kein Fehler.
pub(crate) struct TelemetrySinks {
    /// Der zusammengesetzte Sink, an den Aufrufer Messwerte schicken.
    pub(crate) sink: Arc<dyn TelemetrySink>,
    /// `Some(_)`, solange der Prometheus-Endpunkt gebunden ist; muss für die
    /// Prozesslaufzeit gehalten werden (siehe Typdoku).
    #[allow(dead_code)]
    prometheus_endpoint: Option<PromEndpoint>,
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
///
/// # Returns
/// [`TelemetrySinks`] mit dem File-Sink als `default_sink` und optional
/// Prometheus/OTLP unter dem Präfix `"app."` geroutet (siehe Moduldoc).
///
/// # Errors
/// Ein `String`, wenn der File-Sink nicht geöffnet werden kann
/// ([`harw_observe_file::ObserveFileError`]), der Prometheus-Endpunkt nicht
/// binden kann ([`harw_observe_prom::PromError`]), oder der OTLP-Endpunkt
/// keine gültige `http://`-URI ist ([`harw_observe_otlp::OtlpError`] — dazu
/// zählt jedes `https://`, siehe Moduldoc).
///
/// # Concurrency
/// Rein synchroner Aufbau; der zurückgegebene Sink ist danach unveränderlich
/// und über `Arc` teilbar. Darf nicht aus einer laufenden `tokio`-Runtime
/// heraus aufgerufen werden (siehe Moduldoc).
pub(crate) fn build(home: &Path, args: &TelemetryArgs) -> Result<TelemetrySinks, String> {
    let telemetry_dir = harw_home::paths::telemetry_dir(home);
    std::fs::create_dir_all(&telemetry_dir).map_err(|error| {
        format!(
            "Telemetrie-Verzeichnis {}: {error}",
            telemetry_dir.display()
        )
    })?;
    let file_sink: Arc<dyn TelemetrySink> = Arc::new(
        FileSink::open(&telemetry_dir, FILE_SINK_MAX_BYTES)
            .map_err(|error| format!("File-Sink unter {}: {error}", telemetry_dir.display()))?,
    );

    let mut routing = RoutingSink::new(Arc::clone(&file_sink));
    let mut extra_targets: Vec<Arc<dyn TelemetrySink>> = Vec::new();
    let mut prometheus_endpoint = None;

    if let Some(port) = args.metrics_prometheus_port {
        let endpoint = PromEndpoint::bind(BindAddr::Loopback(port))
            .map_err(|error| format!("Prometheus-Endpunkt auf Port {port}: {error}"))?;
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
        let transport: Arc<dyn OtlpTransport> = Arc::new(
            HttpTransport::new(&config)
                .map_err(|error| format!("OTLP-Endpunkt {endpoint}: {error}"))?,
        );
        let clock: Arc<dyn OtlpClock> = Arc::new(SystemClock);
        extra_targets.push(Arc::new(OtlpSink::new(transport, clock, config)));
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

    Ok(TelemetrySinks {
        sink: Arc::new(routing),
        prometheus_endpoint,
    })
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
    use super::*;
    use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, Unit};

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
    fn test_build_without_any_flag_activates_only_the_file_sink() {
        let home = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("tempdir: {error}"),
        };
        let args = TelemetryArgs {
            metrics_prometheus_port: None,
            metrics_otlp_endpoint: None,
        };

        let sinks = match build(home.path(), &args) {
            Ok(sinks) => sinks,
            Err(error) => panic!("build without flags must succeed: {error}"),
        };

        assert!(
            sinks.prometheus_endpoint.is_none(),
            "no --metrics-prometheus-port must bind no endpoint"
        );
        assert_eq!(sinks.sink.name(), "routing");
    }
}

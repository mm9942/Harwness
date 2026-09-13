//! `OtlpSink`: puffert Messwerte und liefert sie stapelweise als
//! OTLP/JSON an einen [`crate::OtlpTransport`] aus.
//!
//! # Verantwortungsbereich
//! Trägt [`OtlpSink`] und den Nullzähler
//! [`OTLP_PROTECTED_NAMESPACE_BLOCKED`] — der dritte echte Implementierer
//! von [`harw_observe::TelemetrySink`] nach `harw-observe-file::FileSink`
//! und `harw-observe-prom::PromSink` (Vertrag A.3/A.6). Übersetzt jeden
//! `record()`-Aufruf in einen `crate::schema`-Datenpunkt, puffert ihn und
//! liefert ihn erst bei `flush()` stapelweise über den injizierten
//! [`crate::OtlpTransport`] aus — siehe Crate-Doc für die
//! vollständige Begründung von Pufferung, Überlaufverhalten und der
//! `security.*`/`warden.*`-Sperre.
//!
//! # Nebenläufigkeit
//! [`OtlpSink`] ist `Send + Sync + Debug` (Vertrag A.3). Der Puffer liegt
//! hinter einem `std::sync::Mutex` (`TelemetrySink::record` bekommt nur
//! `&self`), nach demselben Muster wie `harw-observe-file::FileSink` und
//! `harw-observe-prom::PromSink`; jeder Zähler liegt in einem eigenen
//! `AtomicU64`.
//!
//! # Fehler
//! Keine — `record()`/`flush()` geben `()` zurück (Vertrag A.3); jede
//! interne Ursache (Pufferüberlauf, nicht endlicher Wert, Zeitüberlauf,
//! Serialisierungs- oder Zustellfehler) wird gezählt statt propagiert.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use harw_observe::{
    Cardinality, FieldName, FieldValue, MetricKey, MetricKind, MetricValue, NullCounter, NullSink,
    TelemetrySink, Unit,
};

use crate::clock::OtlpClock;
use crate::config::OtlpConfig;
use crate::schema::{
    self, AGGREGATION_TEMPORALITY_CUMULATIVE, AGGREGATION_TEMPORALITY_DELTA, AnyValue,
    ExportMetricsServiceRequest, Gauge, Histogram, HistogramDataPoint, InstrumentationScope,
    KeyValue, Metric, NumberDataPoint, Resource, ResourceMetrics, ScopeMetrics, Sum,
};
use crate::transport::OtlpTransport;

const OTLP_PROTECTED_NAMESPACE_BLOCKED_KEY: MetricKey = MetricKey {
    name: "otlp_protected_namespace_blocked_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Nullzähler: wie oft ein Messpunkt aus einem geschützten Namensraum
/// (`security.*`/`warden.*`, [`harw_observe::PROTECTED_NAMESPACES`]) diesen
/// OTLP-Sink erreicht hat.
///
/// # Description
/// Erwarteter Stand im Betrieb: null. Siehe Crate-Doc, Abschnitt
/// „Verhältnis zur `security.*`-Routingregel" für die vollständige
/// Begründung, warum dieser Sink diese Sperre **selbst** durchsetzt, statt
/// sich ausschließlich auf `harw_observe::RoutingSink` zu verlassen.
/// Erhöht sich in [`OtlpSink::record`], **bevor** der Messpunkt gepuffert
/// wird — der Messpunkt erreicht in diesem Fall nie den internen Puffer und
/// wird nie an einen Collector ausgeliefert. Muss von der
/// Kompositionsstelle, die den Prozess zusammensetzt, in ihre
/// [`harw_observe::NullCounterRegistry`] aufgenommen werden, damit er in
/// der Nullzähler-Leiste (Knoten UI-04) erscheint — dieses Modul
/// registriert ihn nicht selbst (dieselbe Zurückhaltung wie
/// `harw_observe::routing::SECURITY_METRIC_LEAKED`).
///
/// # Examples
/// ```
/// use harw_observe_otlp::OTLP_PROTECTED_NAMESPACE_BLOCKED;
///
/// assert_eq!(OTLP_PROTECTED_NAMESPACE_BLOCKED.count(), 0);
/// ```
pub static OTLP_PROTECTED_NAMESPACE_BLOCKED: NullCounter = NullCounter::new(
    &OTLP_PROTECTED_NAMESPACE_BLOCKED_KEY,
    "security.* und warden.* erreichen diesen OTLP-Sink nie, unabhängig davon, wie er verkabelt \
     wurde",
);

// Ob `name` unter einen geschützten Namensraum fällt.
//
// `harw_observe::PROTECTED_NAMESPACES`-Einträge enden laut ihrer eigenen
// Dokumentation immer auf einen Punkt (`"security."`, `"warden."`); ein
// einfacher `starts_with`-Vergleich ist unter dieser Garantie bereits
// grenzsicher und liefert für genau diese beiden Konstanten dasselbe
// Ergebnis wie `harw_observe::routing`s allgemeinere (dort private)
// Punktgrenzen-Prüfung `namespace_matches` — siehe Crate-Doc, Abschnitt
// „Verhältnis zur `security.*`-Routingregel", für die Begründung, warum
// diese Crate die Prüfung eigenständig wiederholt, statt sich
// ausschließlich auf `RoutingSink` zu verlassen.
fn is_protected_namespace(name: &str) -> bool {
    harw_observe::PROTECTED_NAMESPACES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Ein gepufferter, noch nicht ausgelieferter Messpunkt.
#[derive(Debug, Clone)]
struct BufferedPoint {
    key: MetricKey,
    value: MetricValue,
    labels: Vec<(FieldName, FieldValue)>,
    timestamp: jiff::Timestamp,
}

/// Der veränderliche Teil von [`OtlpSink`], hinter einem `Mutex`, weil
/// `TelemetrySink::record` nur `&self` bekommt (Vertrag A.3).
#[derive(Debug, Default)]
struct OtlpState {
    buffer: VecDeque<BufferedPoint>,
}

// Sammelt die Datenpunkte eines einzelnen Metriknamens während des Aufbaus
// eines Stapels (siehe `OtlpSink::build_metrics`). Ein benannter Typ statt
// eines Tupels hält die Feldnamen lesbar und den Zugriff eindeutig.
struct MetricGroup {
    unit: Unit,
    sum_points: Vec<NumberDataPoint>,
    gauge_points: Vec<NumberDataPoint>,
    histogram_points: Vec<HistogramDataPoint>,
}

impl MetricGroup {
    fn new(unit: Unit) -> Self {
        Self {
            unit,
            sum_points: Vec::new(),
            gauge_points: Vec::new(),
            histogram_points: Vec::new(),
        }
    }

    // Baut die fertige `Metric` für diese Gruppe. Höchstens eines von
    // `sum`/`gauge`/`histogram` ist `Some`, außer im Fall eines
    // Emitter-Fehlers, der denselben Metriknamen unter mehreren
    // `MetricKind`-Werten verwendet — dieselbe Nicht-Garantie, die
    // `harw-observe-prom::PromSink` für denselben Fall bereits akzeptiert
    // (siehe dessen `record()`, das `series.kind` bei jedem Aufruf
    // überschreibt, statt Konsistenz über die Zeit zu erzwingen).
    fn into_metric(self, name: &'static str) -> Metric {
        Metric {
            name: name.to_owned(),
            unit: schema::unit_str(self.unit).to_owned(),
            sum: if self.sum_points.is_empty() {
                None
            } else {
                Some(Sum {
                    data_points: self.sum_points,
                    aggregation_temporality: AGGREGATION_TEMPORALITY_CUMULATIVE,
                    is_monotonic: true,
                })
            },
            gauge: if self.gauge_points.is_empty() {
                None
            } else {
                Some(Gauge {
                    data_points: self.gauge_points,
                })
            },
            histogram: if self.histogram_points.is_empty() {
                None
            } else {
                Some(Histogram {
                    data_points: self.histogram_points,
                    aggregation_temporality: AGGREGATION_TEMPORALITY_DELTA,
                })
            },
        }
    }
}

/// Ein [`harw_observe::TelemetrySink`], der Messwerte puffert und
/// stapelweise als OTLP/JSON ausliefert.
///
/// Siehe Moduldoc und Crate-Doc für Pufferstrategie, Überlaufverhalten und
/// die `security.*`/`warden.*`-Sperre.
#[derive(Debug)]
pub struct OtlpSink {
    transport: Arc<dyn OtlpTransport>,
    clock: Arc<dyn OtlpClock>,
    config: OtlpConfig,
    state: Mutex<OtlpState>,
    buffer_overflow_dropped: AtomicU64,
    non_finite_dropped: AtomicU64,
    timestamp_out_of_range_dropped: AtomicU64,
    send_failures: AtomicU64,
}

impl OtlpSink {
    /// Baut einen leeren `OtlpSink`.
    ///
    /// # Description
    /// Nimmt Transport und Zeitquelle als bereits fertige Implementierungen
    /// entgegen — dieser Sink baut keine eigene Netzverbindung und liest
    /// nirgends die Systemuhr (siehe Crate-Doc, Abschnitte „Puffern und
    /// Zustellen" und „Keine Systemuhr").
    ///
    /// # Arguments
    /// - `transport` (`Arc<dyn OtlpTransport>`): das Ziel für jeden Stapel.
    /// - `clock` (`Arc<dyn OtlpClock>`): die Zeitquelle für jeden
    ///   `record()`-Aufruf.
    /// - `config` (`OtlpConfig`): Stapel- und Puffergrößen sowie das
    ///   `service.name`-Ressourcenattribut.
    ///
    /// # Returns
    /// Einen `OtlpSink` ohne bisher gepufferte oder ausgelieferte
    /// Messpunkte.
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe_otlp::{FixedClock, OtlpConfig, OtlpSink, RecordingTransport};
    ///
    /// let config = OtlpConfig {
    ///     endpoint: "http://127.0.0.1:4318/v1/metrics".to_owned(),
    ///     headers: Vec::new(),
    ///     batch_size: 10,
    ///     max_buffer: 100,
    ///     resource_service_name: "harw-sentinel".to_owned(),
    /// };
    /// let sink = OtlpSink::new(
    ///     Arc::new(RecordingTransport::new()),
    ///     Arc::new(FixedClock::new(jiff::Timestamp::UNIX_EPOCH)),
    ///     config,
    /// );
    /// assert_eq!(sink.buffered_len(), 0);
    /// ```
    #[must_use]
    pub fn new(transport: Arc<dyn OtlpTransport>, clock: Arc<dyn OtlpClock>, config: OtlpConfig) -> Self {
        Self {
            transport,
            clock,
            config,
            state: Mutex::new(OtlpState::default()),
            buffer_overflow_dropped: AtomicU64::new(0),
            non_finite_dropped: AtomicU64::new(0),
            timestamp_out_of_range_dropped: AtomicU64::new(0),
            send_failures: AtomicU64::new(0),
        }
    }

    /// Anzahl noch nicht ausgelieferter, gepufferter Messpunkte.
    ///
    /// # Returns
    /// Die aktuelle Pufferlänge.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.lock_state().buffer.len()
    }

    /// Anzahl der wegen Pufferüberlauf verworfenen Messpunkte seit `new()`.
    ///
    /// # Returns
    /// Die kumulierte Zahl (siehe Crate-Doc, Abschnitt „Puffern und
    /// Zustellen": ein Messpunkt wird verworfen und gezählt, sobald der
    /// Puffer `max_buffer` erreicht, nie unbegrenzt aufgestaut).
    #[must_use]
    pub fn buffer_overflow_dropped_count(&self) -> u64 {
        self.buffer_overflow_dropped.load(Ordering::Relaxed)
    }

    /// Anzahl der wegen eines nicht endlichen Werts (`NaN`/`±Inf`)
    /// verworfenen Messpunkte seit `new()`.
    ///
    /// # Returns
    /// Die kumulierte Zahl. JSON kennt — anders als das
    /// Prometheus-Textformat mit seinen `NaN`/`+Inf`/`-Inf`-Literalen —
    /// keine Zahlendarstellung für nicht endliche Gleitkommawerte; ein
    /// solcher Datenpunkt wird deshalb verworfen statt einen ungültigen
    /// OTLP/JSON-Rumpf zu erzeugen.
    #[must_use]
    pub fn non_finite_dropped_count(&self) -> u64 {
        self.non_finite_dropped.load(Ordering::Relaxed)
    }

    /// Anzahl der wegen eines nicht in OTLPs `u64`-Nanosekundenfeld
    /// passenden Zeitstempels verworfenen Messpunkte seit `new()`.
    ///
    /// # Returns
    /// Die kumulierte Zahl (siehe `crate::schema::timestamp_to_unix_nanos`).
    #[must_use]
    pub fn timestamp_out_of_range_dropped_count(&self) -> u64 {
        self.timestamp_out_of_range_dropped.load(Ordering::Relaxed)
    }

    /// Anzahl der Stapel, deren Zustellung über den
    /// [`crate::OtlpTransport`] fehlgeschlagen ist, seit `new()`.
    ///
    /// # Returns
    /// Die kumulierte Zahl. Ein fehlgeschlagener Stapel gilt als verworfen
    /// (siehe [`crate::OtlpTransport::send_batch`]) — kein
    /// erneuter Zustellversuch.
    #[must_use]
    pub fn send_failure_count(&self) -> u64 {
        self.send_failures.load(Ordering::Relaxed)
    }

    fn lock_state(&self) -> MutexGuard<'_, OtlpState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // Nimmt bis zu `config.batch_size` Messpunkte vom Pufferkopf ab.
    fn drain_batch(&self) -> Vec<BufferedPoint> {
        let mut state = self.lock_state();
        let take = self.config.batch_size.max(1).min(state.buffer.len());
        state.buffer.drain(..take).collect()
    }

    // Baut den Ressourcen-/Scope-Rahmen für einen Stapel.
    fn build_resource_metrics(&self, metrics: Vec<Metric>) -> ExportMetricsServiceRequest {
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: AnyValue {
                            string_value: Some(self.config.resource_service_name.clone()),
                            ..AnyValue::default()
                        },
                    }],
                }),
                scope_metrics: vec![ScopeMetrics {
                    scope: Some(InstrumentationScope {
                        name: "harw-observe-otlp".to_owned(),
                    }),
                    metrics,
                }],
            }],
        }
    }

    // Baut die `Metric`-Liste eines Stapels aus den gepufferten Punkten,
    // nach Metrikname gruppiert (Byte-Reihenfolge, für deterministische
    // Ausgabe — dasselbe Muster wie `harw-observe-prom::PromSink::render`).
    // Verwirft und zählt nicht endliche Werte und nicht umrechenbare
    // Zeitstempel je Punkt, statt den ganzen Stapel zu verwerfen.
    fn build_metrics(&self, batch: Vec<BufferedPoint>) -> Vec<Metric> {
        let mut groups: BTreeMap<&'static str, MetricGroup> = BTreeMap::new();

        for point in batch {
            let nanos = match schema::timestamp_to_unix_nanos(point.timestamp) {
                Ok(nanos) => nanos,
                Err(_) => {
                    self.timestamp_out_of_range_dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let time_unix_nano = nanos.to_string();
            let attributes = schema::labels_to_attributes(&point.labels);
            let group = groups
                .entry(point.key.name)
                .or_insert_with(|| MetricGroup::new(point.key.unit));

            match point.key.kind {
                MetricKind::Counter => {
                    let (as_double, as_int) = schema::metric_value_to_number(point.value);
                    if let Some(d) = as_double {
                        if !d.is_finite() {
                            self.non_finite_dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                    group.sum_points.push(NumberDataPoint {
                        attributes,
                        start_time_unix_nano: Some(time_unix_nano.clone()),
                        time_unix_nano,
                        as_double,
                        as_int,
                    });
                }
                MetricKind::Gauge => {
                    let (as_double, as_int) = schema::metric_value_to_number(point.value);
                    if let Some(d) = as_double {
                        if !d.is_finite() {
                            self.non_finite_dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                    group.gauge_points.push(NumberDataPoint {
                        attributes,
                        start_time_unix_nano: None,
                        time_unix_nano,
                        as_double,
                        as_int,
                    });
                }
                MetricKind::Histogram => {
                    let value = schema::metric_value_scalar(point.value);
                    if !value.is_finite() {
                        self.non_finite_dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    group.histogram_points.push(HistogramDataPoint {
                        attributes,
                        start_time_unix_nano: time_unix_nano.clone(),
                        time_unix_nano,
                        count: "1".to_owned(),
                        sum: value,
                        min: value,
                        max: value,
                    });
                }
            }
        }

        groups
            .into_iter()
            .map(|(name, group)| group.into_metric(name))
            // Ein Name, dessen sämtliche Punkte verworfen wurden (nicht
            // endlicher Wert oder Zeitüberlauf, siehe oben), hinterlässt
            // eine Gruppe ohne jeden Datenpunkt — ein `Metric` ohne
            // `sum`/`gauge`/`histogram` trägt keine Information und würde
            // nur unnötig auf die Leitung gehen.
            .filter(|metric| metric.sum.is_some() || metric.gauge.is_some() || metric.histogram.is_some())
            .collect()
    }
}

impl TelemetrySink for OtlpSink {
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
        if is_protected_namespace(key.name) {
            // Absichtlich `&NullSink`: der Bericht über die Verletzung
            // selbst darf diesen (netzwerkexportierenden) Sink nicht
            // erreichen — siehe Crate-Doc und die Doku von
            // `OTLP_PROTECTED_NAMESPACE_BLOCKED`.
            OTLP_PROTECTED_NAMESPACE_BLOCKED.violated(&NullSink, &[]);
            return;
        }

        let timestamp = self.clock.now();
        let mut state = self.lock_state();
        if state.buffer.len() >= self.config.max_buffer {
            self.buffer_overflow_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        state.buffer.push_back(BufferedPoint {
            key: *key,
            value,
            labels: labels.to_vec(),
            timestamp,
        });
    }

    fn flush(&self) {
        loop {
            let batch = self.drain_batch();
            if batch.is_empty() {
                break;
            }
            let metrics = self.build_metrics(batch);
            if metrics.is_empty() {
                continue;
            }
            let request = self.build_resource_metrics(metrics);
            match schema::to_json_bytes(&request) {
                Ok(payload) => {
                    if self.transport.send_batch(&payload).is_err() {
                        self.send_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    self.send_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "otlp"
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harw_observe::{Cardinality, FieldValue, MetricKind, Unit};

    use super::*;
    use crate::clock::FixedClock;
    use crate::config::{HeaderEntry, OtlpConfig};
    use crate::transport::RecordingTransport;

    const COUNTER_NO_LABELS: MetricKey = MetricKey {
        name: "harw_admitted_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const CHILD_ADMITTED: MetricKey = MetricKey {
        name: "harw_child_admitted_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Bounded(64),
    };

    const GAUGE_NO_LABELS: MetricKey = MetricKey {
        name: "harw_queue_depth",
        kind: MetricKind::Gauge,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const LATENCY_HISTOGRAM: MetricKey = MetricKey {
        name: "harw_latency_seconds",
        kind: MetricKind::Histogram,
        unit: Unit::Seconds,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const SECURITY_FINDING: MetricKey = MetricKey {
        name: "security.finding_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const WARDEN_ESCALATION: MetricKey = MetricKey {
        name: "warden.escalation_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    fn test_config(batch_size: usize, max_buffer: usize) -> OtlpConfig {
        OtlpConfig {
            endpoint: "http://127.0.0.1:4318/v1/metrics".to_owned(),
            headers: vec![HeaderEntry {
                name: "X-Test".to_owned(),
                value: "1".to_owned(),
            }],
            batch_size,
            max_buffer,
            resource_service_name: "harw-sentinel".to_owned(),
        }
    }

    fn epoch_sink(batch_size: usize, max_buffer: usize) -> (Arc<RecordingTransport>, OtlpSink) {
        let transport = Arc::new(RecordingTransport::new());
        let sink = OtlpSink::new(
            Arc::clone(&transport) as Arc<dyn OtlpTransport>,
            Arc::new(FixedClock::new(jiff::Timestamp::UNIX_EPOCH)),
            test_config(batch_size, max_buffer),
        );
        (transport, sink)
    }

    fn parse(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes).expect("sink must emit valid JSON")
    }

    // ── Golden-Test ──────────────────────────────────────────────────────

    #[test]
    fn test_golden_fixture_matches_expected_otlp_json() {
        let (transport, sink) = epoch_sink(100, 100);

        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(42), &[]);
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(42),
            &[
                (harw_observe::field!("role"), FieldValue::Str("explorer")),
                (harw_observe::field!("clan"), FieldValue::Str("research")),
            ],
        );
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(7),
            &[
                (harw_observe::field!("role"), FieldValue::Str("guard")),
                (harw_observe::field!("clan"), FieldValue::Str("research")),
            ],
        );
        sink.record(&LATENCY_HISTOGRAM, MetricValue::Observation(1.23), &[]);
        sink.record(&GAUGE_NO_LABELS, MetricValue::Gauge(3.5), &[]);
        sink.flush();

        let sent = transport.sent_batches();
        assert_eq!(sent.len(), 1, "all five points fit in one batch");
        let actual = parse(&sent[0]);
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/golden_metrics_otlp.json"))
                .expect("fixture must be valid JSON");
        assert_eq!(actual, expected);
    }

    // ── Jede `MetricKind`-Variante fällt auf das richtige Konstrukt ──────

    #[test]
    fn test_counter_maps_to_sum_construct() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();

        let sent = transport.sent_batches();
        let value = parse(&sent[0]);
        let metric = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
        assert!(metric.get("sum").is_some());
        assert!(metric.get("gauge").is_none());
        assert!(metric.get("histogram").is_none());
    }

    #[test]
    fn test_gauge_maps_to_gauge_construct() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&GAUGE_NO_LABELS, MetricValue::Gauge(1.0), &[]);
        sink.flush();

        let sent = transport.sent_batches();
        let value = parse(&sent[0]);
        let metric = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
        assert!(metric.get("gauge").is_some());
        assert!(metric.get("sum").is_none());
        assert!(metric.get("histogram").is_none());
    }

    #[test]
    fn test_histogram_maps_to_histogram_construct() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&LATENCY_HISTOGRAM, MetricValue::Observation(2.0), &[]);
        sink.flush();

        let sent = transport.sent_batches();
        let value = parse(&sent[0]);
        let metric = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0];
        assert!(metric.get("histogram").is_some());
        assert!(metric.get("sum").is_none());
        assert!(metric.get("gauge").is_none());
    }

    // ── Labels/Attribute ─────────────────────────────────────────────────

    #[test]
    fn test_empty_labels_omit_the_attributes_field() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();

        let sent = transport.sent_batches();
        let value = parse(&sent[0]);
        let point = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]["dataPoints"][0];
        assert!(
            point.get("attributes").is_none(),
            "an empty label set must not appear as an empty `attributes` array either: {point}"
        );
    }

    #[test]
    fn test_labels_translate_to_sorted_attributes() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(1),
            &[
                (harw_observe::field!("role"), FieldValue::Str("guard")),
                (harw_observe::field!("clan"), FieldValue::Str("research")),
            ],
        );
        sink.flush();

        let sent = transport.sent_batches();
        let value = parse(&sent[0]);
        let attributes = &value["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]["sum"]["dataPoints"][0]["attributes"];
        assert_eq!(attributes[0]["key"], "clan");
        assert_eq!(attributes[0]["value"]["stringValue"], "research");
        assert_eq!(attributes[1]["key"], "role");
        assert_eq!(attributes[1]["value"]["stringValue"], "guard");
    }

    // ── Pufferüberlauf ───────────────────────────────────────────────────

    #[test]
    fn test_buffer_overflow_drops_and_counts_beyond_max_buffer() {
        let (_, sink) = epoch_sink(1000, 3);

        for _ in 0..5 {
            sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        }

        assert_eq!(sink.buffered_len(), 3, "the buffer never grows past max_buffer");
        assert_eq!(
            sink.buffer_overflow_dropped_count(),
            2,
            "the two points beyond max_buffer must be dropped and counted"
        );
    }

    // ── Stapelabgabe ─────────────────────────────────────────────────────

    #[test]
    fn test_batch_delivery_splits_into_multiple_batches_by_batch_size() {
        let (transport, sink) = epoch_sink(2, 100);

        for _ in 0..5 {
            sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        }
        sink.flush();

        // `harw_admitted_total` mit fünf gleich benannten Punkten fällt in
        // genau eine `Metric` je Stapel, aber der Puffer wird in Stapeln zu
        // höchstens zwei Punkten entleert — bei fünf Punkten macht das drei
        // Stapel (2 + 2 + 1).
        assert_eq!(transport.sent_batches().len(), 3);
        assert_eq!(sink.buffered_len(), 0, "flush() drains the whole buffer");
    }

    #[test]
    fn test_flush_with_empty_buffer_sends_nothing() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.flush();
        assert!(transport.sent_batches().is_empty());
    }

    // ── `security.*`/`warden.*`-Sperre ───────────────────────────────────

    // Beide Namensräume in einem Test geprüft, nicht in zweien: `cargo test`
    // führt Tests standardmäßig nebenläufig in Threads desselben Prozesses
    // aus, und `OTLP_PROTECTED_NAMESPACE_BLOCKED` ist ein einziger,
    // prozessweiter `static`-Zähler — zwei Tests, die ihn unabhängig
    // voneinander per „vorher/nachher"-Differenz prüfen, könnten sich
    // gegenseitig überholen und den jeweils anderen Test spuriös scheitern
    // lassen. Ein einziger Test, der beide Fälle nacheinander auslöst, hält
    // die Differenzprüfung race-frei (dasselbe Vorgehen wie
    // `harw_observe::routing`s eigener Test für `SECURITY_METRIC_LEAKED`,
    // der ebenfalls der einzige Berührungspunkt dieses Zählers in seiner
    // Testsuite ist).
    /// Serialisiert die Tests, die den prozessweiten Nullzähler
    /// [`OTLP_PROTECTED_NAMESPACE_BLOCKED`] lesen oder erhöhen — dieselbe
    /// Begründung, aus der `harw-core/src/context_budget.rs` seinen
    /// `TRUST_BLOCK_VIOLATION`-Test, `harw-secrets/src/audit/telemetry.rs`
    /// seinen `AUDIT_CHAIN_BREAK`-Test, `harw-knowledge/src/context_steward.rs`
    /// seine drei `STEWARD_*`-Tests und `harw-observe/src/routing.rs` seinen
    /// `SECURITY_METRIC_LEAKED`-Test hinter einem eigenen Lock serialisieren:
    /// ein `static NullCounter` ist prozessweit geteilt, und `cargo test`
    /// läuft standardmäßig mit mehreren Threads im selben Prozess.
    ///
    /// Eine gemeinsame Sperre mit `harw-observe`s `SECURITY_COUNTER_LOCK`
    /// ist hier **nicht** möglich: `harw-observe-otlp` hängt von
    /// `harw-observe` ab, nicht umgekehrt, und die beiden Zähler leben in
    /// getrennten Crates — deshalb ein eigenes, zweites Lock hier.
    ///
    /// Der Kommentar direkt oberhalb dieses Locks (Abschnitt
    /// „`security.*`/`warden.*`-Sperre") behauptet bereits, dieser Test sei
    /// der einzige Berührungspunkt des Zählers in dieser Testsuite — das
    /// macht eine reine Vorher/Nachher-Differenzmessung (`let before = …
    /// count();`) für sich genommen **trotzdem nicht verlässlich**: zwischen
    /// der Messung von `before` und der Zusicherung am Ende kann ein
    /// künftig hinzukommender, parallel laufender Test denselben Zähler
    /// erhöhen, und die Differenz stimmt dann nicht mehr. Ein solcher
    /// Kommentar ist keine dauerhafte Garantie — genau das ist bei den drei
    /// `STEWARD_*`-Zählern in `harw-knowledge` eingetreten: sie waren lange
    /// grün, bis neue Tests hinzukamen, die denselben Zähler lasen. Die
    /// Sperre steht **zusätzlich** zur Differenzmessung, nicht an ihrer
    /// Stelle.
    static OTLP_COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_security_and_warden_namespaces_never_reach_the_buffer_or_the_transport() {
        let _guard = OTLP_COUNTER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (transport, sink) = epoch_sink(10, 10);
        let before = OTLP_PROTECTED_NAMESPACE_BLOCKED.count();

        sink.record(&SECURITY_FINDING, MetricValue::Count(1), &[]);
        sink.record(&WARDEN_ESCALATION, MetricValue::Count(1), &[]);
        sink.flush();

        assert_eq!(sink.buffered_len(), 0, "a protected metric must never be buffered");
        assert!(
            transport.sent_batches().is_empty(),
            "a protected metric must never reach the transport"
        );
        assert_eq!(
            OTLP_PROTECTED_NAMESPACE_BLOCKED.count(),
            before + 2,
            "each of the two protected records must be counted exactly once"
        );
    }

    #[test]
    fn test_lookalike_namespace_without_dot_boundary_is_not_blocked() {
        const LOOKALIKE: MetricKey = MetricKey {
            name: "securityx.foo_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        };
        let (transport, sink) = epoch_sink(10, 10);

        sink.record(&LOOKALIKE, MetricValue::Count(1), &[]);
        sink.flush();

        assert_eq!(
            transport.sent_batches().len(),
            1,
            "securityx. is a distinct, unprotected namespace"
        );
    }

    // ── Nicht endliche Werte ─────────────────────────────────────────────

    #[test]
    fn test_non_finite_gauge_value_is_dropped_and_counted() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&GAUGE_NO_LABELS, MetricValue::Gauge(f64::NAN), &[]);
        sink.flush();

        assert!(transport.sent_batches().is_empty(), "a non-finite value must not reach JSON");
        assert_eq!(sink.non_finite_dropped_count(), 1);
    }

    #[test]
    fn test_non_finite_histogram_observation_is_dropped_and_counted() {
        let (transport, sink) = epoch_sink(10, 10);
        sink.record(&LATENCY_HISTOGRAM, MetricValue::Observation(f64::INFINITY), &[]);
        sink.flush();

        assert!(transport.sent_batches().is_empty());
        assert_eq!(sink.non_finite_dropped_count(), 1);
    }

    // ── Zeitüberlauf ─────────────────────────────────────────────────────

    #[test]
    fn test_timestamp_out_of_range_is_dropped_and_counted() {
        let transport = Arc::new(RecordingTransport::new());
        let sink = OtlpSink::new(
            Arc::clone(&transport) as Arc<dyn OtlpTransport>,
            Arc::new(FixedClock::new(jiff::Timestamp::MIN)),
            test_config(10, 10),
        );

        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();

        assert!(transport.sent_batches().is_empty());
        assert_eq!(sink.timestamp_out_of_range_dropped_count(), 1);
    }

    // ── Zustellfehler ────────────────────────────────────────────────────

    #[test]
    fn test_send_failure_is_counted_and_the_batch_is_not_retried() {
        let (transport, sink) = epoch_sink(10, 10);
        transport.fail_next_send(true);

        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();

        assert_eq!(sink.send_failure_count(), 1);
        assert!(
            transport.sent_batches().is_empty(),
            "a failed batch must not be retried on a later flush()"
        );

        sink.flush();
        assert_eq!(
            sink.send_failure_count(),
            1,
            "flush() on an already-empty buffer must not re-count the earlier failure"
        );
    }

    // ── Nebenläufigkeit / Grundeigenschaften ─────────────────────────────

    #[test]
    fn test_otlp_sink_name_is_otlp() {
        let (_, sink) = epoch_sink(10, 10);
        assert_eq!(sink.name(), "otlp");
    }

    #[test]
    fn test_otlp_sink_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<OtlpSink>();
    }

    #[test]
    fn test_otlp_sink_usable_through_arc_dyn_telemetry_sink() {
        let (_, sink) = epoch_sink(10, 10);
        let sink: Arc<dyn TelemetrySink> = Arc::new(sink);
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();
        assert_eq!(sink.name(), "otlp");
    }

    #[test]
    fn test_buffered_len_reflects_pending_points_until_flush() {
        let (_, sink) = epoch_sink(10, 10);
        assert_eq!(sink.buffered_len(), 0);
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        assert_eq!(sink.buffered_len(), 1);
        sink.flush();
        assert_eq!(sink.buffered_len(), 0);
    }
}

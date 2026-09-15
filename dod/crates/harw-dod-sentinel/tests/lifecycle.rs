//! Integrationstest: der volle Lebenszyklus einer `Sentinel`-Instanz, wie
//! ihn das Binary aus Knoten AW2-19 durchläuft — mehrere Sensoren mit
//! unterschiedlichem Schicksal, mehrere Abrufrunden, Telemetrie, Einfrieren
//! zu einem zitierfähigen Beleg.
//!
//! Nutzt ausschließlich die öffentliche API von `harw-dod-sentinel`
//! (`Sentinel`, `SentinelConfig`, `EvidenceBuffer`, `health`, `metrics`) und
//! zwei kleine, hier definierte Mock-Sensoren — kein `harw-dod-*`-Sensor,
//! keine reale Quelle.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_sentinel::health::{DegradeReason, RetryPolicy, SensorHealth};
use harw_dod_sentinel::{Sentinel, SentinelConfig};
use harw_dod_signals::{EventKind, HostSample, Sensor, SensorReading};
use harw_observe::{FieldName, FieldValue, MetricKey, MetricValue, TelemetrySink};
use harw_types::SensorId;
use jiff::{SignedDuration, Timestamp};

/// Sensor mit vorab festgelegten Ergebnissen (FIFO); nach Erschöpfung immer
/// `Ok(SensorReading::default())`.
#[derive(Debug)]
struct ScriptedSensor {
    handle: SensorHandle<Bound>,
    script: Mutex<VecDeque<Result<SensorReading, SensorError>>>,
    calls: AtomicUsize,
}

impl ScriptedSensor {
    fn new(
        id: &str,
        capability: Capability,
        script: Vec<Result<SensorReading, SensorError>>,
    ) -> Arc<Self> {
        let handle = SensorHandle::new(SensorId::from_str(id), capability)
            .bind(ReadScope::from_roots([std::path::PathBuf::from("/proc")]));
        Arc::new(Self {
            handle,
            script: Mutex::new(script.into()),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Sensor for ScriptedSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.script
            .lock()
            .expect("test mutex not poisoned")
            .pop_front()
            .unwrap_or_else(|| Ok(SensorReading::default()))
    }
}

/// Rückgabetyp-Koerzion `Arc<ScriptedSensor>` -> `Arc<dyn Sensor>`, ohne den
/// konkreten `Arc` beim Aufrufer zu verbrauchen (für `.calls()` danach).
fn as_sensor(mock: &Arc<ScriptedSensor>) -> Arc<dyn Sensor> {
    // Zwischenbindung nötig: `Arc::clone` leitet sein `T` sonst aus dem
    // Rückgabetyp ab und erwartet dann `&Arc<dyn Sensor>`. Erst mit
    // festgelegtem `T` greift die Unsizing-Koerzion an der Rückgabe.
    let cloned: Arc<ScriptedSensor> = Arc::clone(mock);
    cloned
}

fn sample(sensor: &str, metric: &'static str, value: f64, now: Timestamp) -> HostSample {
    HostSample {
        sensor: SensorId::from_str(sensor),
        observed_at: now,
        metric: std::borrow::Cow::Borrowed(metric),
        value,
    }
}

/// Ein Sink, der jeden `record()`-Aufruf mitschreibt, um am Ende zu prüfen,
/// dass die Sammelstelle tatsächlich Telemetrie emittiert.
/// Ein aufgezeichneter Aufruf: Name, Wert und die Felder, mit denen er kam.
type RecordedCall = (&'static str, MetricValue, Vec<(FieldName, FieldValue)>);

#[derive(Debug, Default)]
struct RecordingSink {
    records: Mutex<Vec<RecordedCall>>,
}

impl TelemetrySink for RecordingSink {
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
        self.records
            .lock()
            .expect("test mutex not poisoned")
            .push((key.name, value, labels.to_vec()));
    }

    fn flush(&self) {}

    fn name(&self) -> &'static str {
        "recording"
    }
}

impl RecordingSink {
    fn count_by_name(&self, name: &str) -> usize {
        self.records
            .lock()
            .expect("test mutex not poisoned")
            .iter()
            .filter(|(recorded_name, ..)| *recorded_name == name)
            .count()
    }
}

#[test]
fn test_full_lifecycle_healthy_transient_and_permanent_sensors() {
    let now0 = Timestamp::UNIX_EPOCH;

    // Sensor 1: liefert dauerhaft, ein Sample je Abruf.
    let healthy = ScriptedSensor::new(
        "healthy-0",
        Capability::ReadProcStat,
        vec![
            Ok(SensorReading {
                samples: vec![sample("healthy-0", "cpu_util_percent", 12.0, now0)],
                events: vec![],
            }),
            Ok(SensorReading {
                samples: vec![sample("healthy-0", "cpu_util_percent", 14.0, now0)],
                events: vec![],
            }),
        ],
    );

    // Sensor 2: ein vorübergehender Fehler, dann Erholung.
    let flaky = ScriptedSensor::new(
        "flaky-0",
        Capability::ReadSysfsThermal,
        vec![Err(SensorError::MalformedSource), Ok(SensorReading::default())],
    );

    // Sensor 3: sofort dauerhaft aufgegeben.
    let broken = ScriptedSensor::new("broken-0", Capability::ReadProcMeminfo, vec![Err(SensorError::OutsideScope)]);

    let sink = Arc::new(RecordingSink::default());
    let config = SentinelConfig::new(RetryPolicy::new(5, SignedDuration::from_secs(0)), 64, 64);
    let mut sentinel = Sentinel::new(
        vec![as_sensor(&healthy), as_sensor(&flaky), as_sensor(&broken)],
        Arc::clone(&sink) as Arc<dyn TelemetrySink>,
        config,
    );

    assert_eq!(sentinel.sensor_count(), 3);

    // Runde 1: healthy liefert, flaky scheitert transient, broken scheitert
    // permanent und wird sofort abgemeldet — mit Ereignis.
    let round1 = sentinel.poll_all(now0);
    assert_eq!(round1.samples.len(), 1);
    assert_eq!(round1.events.len(), 1);
    let EventKind::SensorDegraded { sensor } = &round1.events[0].kind else {
        panic!("expected SensorDegraded in round 1");
    };
    assert_eq!(sensor.as_str(), "broken-0");

    // Runde 2 (backoff = 0, also sofort fällig): healthy liefert erneut,
    // flaky erholt sich, broken wird nicht mehr abgerufen.
    let round2 = sentinel.poll_all(now0);
    assert_eq!(round2.samples.len(), 1);
    assert!(round2.events.is_empty());
    assert_eq!(broken.calls(), 1, "a degraded sensor must never be polled again");

    let snapshot = sentinel.health_snapshot();
    let health_of = |id: &str| {
        snapshot
            .iter()
            .find(|(sensor_id, _)| sensor_id.as_str() == id)
            .map(|(_, health)| health.clone())
            .expect("sensor present in snapshot")
    };
    assert_eq!(health_of("healthy-0"), SensorHealth::Bound);
    assert_eq!(health_of("flaky-0"), SensorHealth::Bound);
    assert_eq!(
        health_of("broken-0"),
        SensorHealth::Degraded {
            reason: DegradeReason::Permanent
        }
    );
    assert_eq!(sentinel.degraded_count(), 1);

    // Der Puffer hat beide Samples (aus Runde 1 und 2) und das eine
    // `SensorDegraded`-Event aus Runde 1 aufgenommen.
    assert_eq!(sentinel.buffer().sample_len(), 2);
    assert_eq!(sentinel.buffer().event_len(), 1);

    // Einfrieren ist deterministisch: zweimal ohne neue Einträge ergibt
    // denselben Beleg.
    let evidence_a = sentinel
        .freeze(now0)
        .expect("well-formed buffer content always encodes");
    let evidence_b = sentinel
        .freeze(now0)
        .expect("well-formed buffer content always encodes");
    assert_eq!(evidence_a, evidence_b);
    assert_eq!(evidence_a.samples.len(), 2);
    assert_eq!(evidence_a.events.len(), 1);

    // Telemetrie ist tatsächlich geflossen: mindestens ein Abruf je Runde
    // und Sensor, mindestens ein Fehler, genau eine Degradations-Kennzahl
    // je Runde, und ein Füllstand je Ring und Runde.
    assert!(sink.count_by_name("harw_sentinel_sensor_polls_total") >= 4);
    assert!(sink.count_by_name("harw_sentinel_sensor_errors_total") >= 2);
    assert_eq!(sink.count_by_name("harw_sentinel_sensors_degraded"), 2);
    assert_eq!(sink.count_by_name("harw_sentinel_buffer_utilization"), 4);
}

#[test]
fn test_ring_buffer_capacity_bounds_survive_a_full_lifecycle() {
    // Kapazität 1 je Ring: jede neue Runde verdrängt die vorherige.
    let sensor = ScriptedSensor::new(
        "loud-0",
        Capability::ReadProcNetDev,
        vec![
            Ok(SensorReading {
                samples: vec![sample("loud-0", "bytes_sent_total", 1.0, Timestamp::UNIX_EPOCH)],
                events: vec![],
            }),
            Ok(SensorReading {
                samples: vec![sample("loud-0", "bytes_sent_total", 2.0, Timestamp::UNIX_EPOCH)],
                events: vec![],
            }),
        ],
    );
    let mut sentinel = Sentinel::new(
        vec![as_sensor(&sensor)],
        Arc::new(harw_observe::NullSink),
        SentinelConfig::new(RetryPolicy::default(), 1, 1),
    );

    sentinel.poll_all(Timestamp::UNIX_EPOCH);
    sentinel.poll_all(Timestamp::UNIX_EPOCH);

    assert_eq!(sentinel.buffer().sample_len(), 1);
    let remaining: Vec<f64> = sentinel.buffer().samples().map(|s| s.value).collect();
    assert_eq!(remaining, vec![2.0]);
}

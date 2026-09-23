//! Metrikschlüssel dieser Crate: sieht man, ob die Sensorik überhaupt
//! arbeitet? (Knoten AW2-18)
//!
//! # Welche Frage jede Metrik beantwortet
//! - [`SENSOR_POLLS_TOTAL`]: Wird ein bestimmter Sensor tatsächlich
//!   abgerufen? Ein Sensor, dessen Zähler über eine ganze Betriebsdauer bei
//!   null steht, wird nie erreicht — unabhängig davon, ob er registriert
//!   wurde.
//! - [`SENSOR_ERRORS_TOTAL`]: Wie oft scheitert ein Abruf, und mit welcher
//!   [`harw_dod_cap::Permanence`]? Ein Sensor mit überwiegend `permanent`
//!   klassifizierten Fehlern nähert sich seiner Abmeldung; überwiegend
//!   `transient` ist ein Hinweis auf eine flatternde Quelle.
//! - [`SENSORS_DEGRADED`]: Wie viele Sensoren sind gerade abgemeldet? Der
//!   direkte Gegenwert zum [`harw_dod_signals::EventKind::SensorDegraded`]-Ereignis
//!   als Zeitreihe statt als Einzelmeldung.
//! - [`BUFFER_UTILIZATION`]: Wie voll ist welcher der beiden Ringe
//!   ([`crate::buffer::EvidenceBuffer`])? Ein Ring, der dauerhaft nahe `1.0`
//!   steht, verdrängt bei jedem neuen Eintrag einen alten — ein Hinweis, dass
//!   seine konfigurierte Kapazität (siehe `buffer`-Moduldoku) für die
//!   tatsächliche Last zu klein gewählt wurde.
//!
//! # Kardinalität
//! Ein Label je Sensor ist begrenzt: [`crate::Sentinel`] hält eine von
//! außen (Knoten AW2-19) übergebene, zur Konstruktionszeit feste Liste von
//! Sensoren — im Ausbauprogramm elf Crates, also elf mögliche Werte für das
//! Label `sensor`. [`SENSOR_POLLS_TOTAL`] deklariert deshalb
//! `cardinality = bounded(11)`; [`SENSOR_ERRORS_TOTAL`] kombiniert `sensor`
//! mit `permanence` (zwei Werte, siehe [`harw_dod_cap::Permanence`]) und
//! deklariert entsprechend `bounded(22)`. **Ein Label je Metrikname wäre es
//! nicht:** würde diese Crate stattdessen für jeden Sensor eine eigene
//! Metrik-Konstante erzeugen (`POLLS_THERMAL_TOTAL`,
//! `POLLS_PROC_STAT_TOTAL`, …), wüchse die Anzahl der Metriken mit jeder
//! neuen Sensor-Crate — eine Kardinalitätsexplosion auf der Ebene der
//! Metriknamen statt der Labelwerte, die dieselbe Zeitreihendatenbank aus
//! demselben Grund kippen lässt. Ein Label mit **fest begrenzter**
//! Wertemenge ist der Punkt, an dem diese Explosion strukturell
//! ausgeschlossen ist.
//!
//! `sensor` selbst trägt den dynamischen Namen der Sensorinstanz
//! ([`harw_types::SensorId`]) und ist deshalb kein `field!`-validierter
//! statischer Wert, sondern [`harw_observe::FieldValue::Owned`] — die
//! Kardinalitätsgrenze kommt aus der festen Sensorenliste dieser
//! Sentinel-Instanz, nicht aus einer geschlossenen Aufzählung im Code dieser
//! Crate (anders als `permanence` und `buffer`, die beide über ein
//! erschöpfendes `match` ohne Wildcard aus geschlossenen Enums abgeleitet
//! werden, siehe [`permanence_label`] und [`BufferKind::label`]).
//!
//! # Wie der Sink hereinkommt
//! [`crate::Sentinel`] hält einen `Arc<dyn TelemetrySink>` über seine
//! gesamte Lebensdauer (siehe `sentinel`-Moduldoku) und reicht ihn als
//! `&dyn TelemetrySink` an die `record_*`-Funktionen dieses Moduls durch.
//! Anders als `harw-plan-bridge::metrics` (kein `Option<&dyn TelemetrySink>`):
//! der Sentinel besitzt genau einen Lebenszyklus (poll-Schleife über seine
//! gesamte Laufzeit), an dem ein Sink immer vorhanden ist — Tests, die keine
//! Emission wollen, übergeben [`harw_observe::NullSink`].
//!
//! # Keine Inhalte in Labels
//! Jedes Label dieses Moduls ist entweder eine Sensorkennung
//! ([`harw_types::SensorId`] — ein vom Betreiber vergebener, stabiler Name,
//! keine Nutzlast) oder ein Wert aus einer der beiden hier geschlossenen
//! Zwei-Werte-Mengen (`permanence`, `buffer`). Nie ein Pfad, eine gelesene
//! Zeile oder ein Feldwert eines `HostSample`/`SecurityEvent` — dieselbe
//! Inhaltsfreiheit, die [`harw_dod_cap::error::SensorError`] für
//! Sensorfehler durchsetzt (siehe dortige Moduldoku), gilt hier für
//! Metrik-Labels.
//!
//! # Concurrency
//! Alle Funktionen dieses Moduls sind rein und zustandslos;
//! `TelemetrySink: Send + Sync` erlaubt Aufrufe aus beliebigen Threads
//! gleichzeitig. Keine globale Variable, kein `Mutex`.
//!
//! # Examples
//! ```rust
//! use harw_observe::NullSink;
//! use harw_types::SensorId;
//!
//! let sink = NullSink;
//! let id = SensorId::from_str("thermal-0");
//! harw_dod_sentinel::metrics::record_poll(&sink, &id);
//! harw_dod_sentinel::metrics::record_degraded_sensors(&sink, 0);
//! ```

use harw_dod_cap::Permanence;
use harw_observe::{FieldValue, MetricValue, TelemetrySink};
use harw_types::SensorId;

/// Label-Feldname für die Sensorkennung.
const SENSOR_LABEL: harw_observe::FieldName = harw_macros::field!("sensor");
/// Label-Feldname für die [`Permanence`]-Einordnung eines Fehlers.
const PERMANENCE_LABEL: harw_observe::FieldName = harw_macros::field!("permanence");
/// Label-Feldname für den betroffenen Ring ([`BufferKind`]).
const BUFFER_LABEL: harw_observe::FieldName = harw_macros::field!("buffer");

harw_macros::metrics! {
    /// Wie oft ein bestimmter Sensor tatsächlich abgerufen wurde (Label
    /// `sensor`; Kardinalität durch die feste, zur Konstruktionszeit
    /// übergebene Sensorenliste auf höchstens elf Werte begrenzt — siehe
    /// Moduldoku, Abschnitt "Kardinalität"). Ein übersprungener Abruf
    /// (Sensor `Degraded`, oder `Retrying` vor seinem `next_attempt`) zählt
    /// hier nicht mit — nur ein tatsächlicher `Sensor::poll`-Aufruf.
    SENSOR_POLLS_TOTAL: counter, unit = count, labels = ["sensor"], cardinality = bounded(11),
        name = "harw_sentinel_sensor_polls_total";
    /// Wie oft ein Abruf mit einem Fehler endete, aufgeschlüsselt nach
    /// Sensor und [`Permanence`] (`transient`/`permanent`).
    SENSOR_ERRORS_TOTAL: counter, unit = count, labels = ["sensor", "permanence"], cardinality = bounded(22),
        name = "harw_sentinel_sensor_errors_total";
    /// Aktuelle Zahl der Sensoren im Zustand
    /// [`harw_dod_sentinel::health::SensorHealth::Degraded`](crate::health::SensorHealth::Degraded).
    /// Kein Label: ein einzelner Wert je Sentinel-Instanz.
    SENSORS_DEGRADED: gauge, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_sensors_degraded";
    /// Füllstand eines der beiden Ringe aus [`crate::buffer::EvidenceBuffer`]
    /// (Label `buffer` = `samples` oder `events`), zwischen `0.0` (leer) und
    /// `1.0` (voll).
    BUFFER_UTILIZATION: gauge, unit = ratio, labels = ["buffer"], cardinality = bounded(2),
        name = "harw_sentinel_buffer_utilization";
}

/// Welcher der beiden Ringe aus [`crate::buffer::EvidenceBuffer`] gemeint
/// ist. Nur für die Beschriftung von [`BUFFER_UTILIZATION`] — kein Typ, den
/// `EvidenceBuffer` selbst kennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferKind {
    /// Der Sample-Ring.
    Samples,
    /// Der Event-Ring.
    Events,
}

impl BufferKind {
    /// Der Label-Wert dieser Ringart.
    ///
    /// Erschöpfendes `match` ohne Wildcard: eine dritte Ringart bricht diese
    /// Funktion beim Kompilieren, statt die deklarierte Kardinalitätsgrenze
    /// `bounded(2)` von [`BUFFER_UTILIZATION`] still zu verletzen.
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Samples => "samples",
            Self::Events => "events",
        }
    }
}

/// Ordnet eine [`Permanence`] ihrem Label-Wert für [`SENSOR_ERRORS_TOTAL`]
/// zu.
///
/// Erschöpfendes `match` ohne Wildcard, aus demselben Grund wie
/// [`BufferKind::label`].
const fn permanence_label(permanence: Permanence) -> &'static str {
    match permanence {
        Permanence::Transient => "transient",
        Permanence::Permanent => "permanent",
    }
}

/// Emittiert [`SENSOR_POLLS_TOTAL`] für einen tatsächlichen Abruf.
///
/// # Description
/// Aufrufstelle: [`crate::Sentinel::poll_all`], unmittelbar vor dem Aufruf
/// von [`harw_dod_signals::Sensor::poll`] auf `sensor` — ein übersprungener
/// Sensor (siehe [`crate::health::SensorHealth::is_due`]) erzeugt keine
/// Emission.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel des Messwerts.
/// - `sensor` (`&harw_types::SensorId`): die Kennung des abgerufenen
///   Sensors.
///
/// # Concurrency
/// Reine Funktion; `sink.record` ist aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_observe::NullSink;
/// use harw_types::SensorId;
///
/// let sink = NullSink;
/// harw_dod_sentinel::metrics::record_poll(&sink, &SensorId::from_str("thermal-0"));
/// ```
pub fn record_poll(sink: &dyn TelemetrySink, sensor: &SensorId) {
    sink.record(
        &SENSOR_POLLS_TOTAL,
        MetricValue::Count(1),
        &[(SENSOR_LABEL, FieldValue::Owned(sensor.as_str().to_owned()))],
    );
}

/// Emittiert [`SENSOR_ERRORS_TOTAL`] für einen fehlgeschlagenen Abruf.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel des Messwerts.
/// - `sensor` (`&harw_types::SensorId`): die Kennung des betroffenen
///   Sensors.
/// - `permanence` (`Permanence`): die Einordnung des aufgetretenen Fehlers
///   (`harw_dod_cap::error::SensorError::permanence`).
///
/// # Concurrency
/// Reine Funktion; `sink.record` ist aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_dod_cap::Permanence;
/// use harw_observe::NullSink;
/// use harw_types::SensorId;
///
/// let sink = NullSink;
/// harw_dod_sentinel::metrics::record_error(
///     &sink,
///     &SensorId::from_str("thermal-0"),
///     Permanence::Transient,
/// );
/// ```
pub fn record_error(sink: &dyn TelemetrySink, sensor: &SensorId, permanence: Permanence) {
    sink.record(
        &SENSOR_ERRORS_TOTAL,
        MetricValue::Count(1),
        &[
            (SENSOR_LABEL, FieldValue::Owned(sensor.as_str().to_owned())),
            (
                PERMANENCE_LABEL,
                FieldValue::Str(permanence_label(permanence)),
            ),
        ],
    );
}

/// Setzt [`SENSORS_DEGRADED`] auf die aktuelle Zahl abgemeldeter Sensoren.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel des Messwerts.
/// - `count` (`usize`): Anzahl der Sensoren im Zustand
///   [`crate::health::SensorHealth::Degraded`], aus
///   [`crate::Sentinel::degraded_count`].
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use harw_observe::NullSink;
///
/// let sink = NullSink;
/// harw_dod_sentinel::metrics::record_degraded_sensors(&sink, 2);
/// ```
pub fn record_degraded_sensors(sink: &dyn TelemetrySink, count: usize) {
    #[allow(clippy::cast_precision_loss)]
    sink.record(&SENSORS_DEGRADED, MetricValue::Gauge(count as f64), &[]);
}

/// Setzt [`BUFFER_UTILIZATION`] für einen der beiden Ringe.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel des Messwerts.
/// - `kind` (`BufferKind`): welcher Ring gemeint ist.
/// - `ratio` (`f64`): Füllstand zwischen `0.0` und `1.0`, aus
///   [`crate::buffer::EvidenceBuffer::sample_utilization`] bzw.
///   [`crate::buffer::EvidenceBuffer::event_utilization`].
///
/// # Concurrency
/// Reine Funktion.
///
/// # Examples
/// ```rust
/// use harw_dod_sentinel::metrics::BufferKind;
/// use harw_observe::NullSink;
///
/// let sink = NullSink;
/// harw_dod_sentinel::metrics::record_buffer_utilization(&sink, BufferKind::Events, 0.25);
/// ```
pub fn record_buffer_utilization(sink: &dyn TelemetrySink, kind: BufferKind, ratio: f64) {
    sink.record(
        &BUFFER_UTILIZATION,
        MetricValue::Gauge(ratio),
        &[(BUFFER_LABEL, FieldValue::Str(kind.label()))],
    );
}

#[cfg(test)]
mod tests {
    use super::{
        BUFFER_UTILIZATION, BufferKind, SENSOR_ERRORS_TOTAL, SENSOR_POLLS_TOTAL, SENSORS_DEGRADED,
        permanence_label, record_buffer_utilization, record_degraded_sensors, record_error,
        record_poll,
    };
    use crate::test_support::{TestResult, ctx};
    use harw_dod_cap::Permanence;
    use harw_observe::{FieldValue, MetricValue, TelemetrySink};
    use harw_types::SensorId;
    use std::sync::Mutex;

    /// Ein aufgezeichneter Aufruf: Name, Wert und die Felder dazu.
    type RecordedCall = (&'static str, MetricValue, Vec<(&'static str, FieldValue)>);

    #[derive(Debug, Default)]
    struct RecordingSink {
        records: Mutex<Vec<RecordedCall>>,
    }

    impl TelemetrySink for RecordingSink {
        fn record(
            &self,
            key: &harw_observe::MetricKey,
            value: MetricValue,
            labels: &[(harw_observe::FieldName, FieldValue)],
        ) {
            let labels = labels
                .iter()
                .map(|(name, value)| (name.as_str(), value.clone()))
                .collect();
            // `TelemetrySink::record` liefert laut Trait-Signatur (harw-observe,
            // außerhalb dieser Datei) `()` — kein `?` möglich. Ein vergifteter
            // Mutex wird hier statt eines Panics über `PoisonError::into_inner`
            // wiederhergestellt; ein Test-Double darf den Prozess nicht abbrechen.
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((key.name, value, labels));
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "recording"
        }
    }

    #[test]
    fn test_record_poll_emits_sensor_label() -> TestResult {
        let sink = RecordingSink::default();
        record_poll(&sink, &SensorId::from_str("thermal-0"));

        let records = sink
            .records
            .lock()
            .map_err(ctx("test mutex not poisoned"))?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, SENSOR_POLLS_TOTAL.name);
        assert_eq!(records[0].1, MetricValue::Count(1));
        assert_eq!(
            records[0].2,
            vec![("sensor", FieldValue::Owned("thermal-0".to_owned()))]
        );
        Ok(())
    }

    #[test]
    fn test_record_error_emits_sensor_and_permanence_labels() -> TestResult {
        let sink = RecordingSink::default();
        record_error(
            &sink,
            &SensorId::from_str("thermal-0"),
            Permanence::Permanent,
        );

        let records = sink
            .records
            .lock()
            .map_err(ctx("test mutex not poisoned"))?;
        assert_eq!(records[0].0, SENSOR_ERRORS_TOTAL.name);
        assert_eq!(
            records[0].2,
            vec![
                ("sensor", FieldValue::Owned("thermal-0".to_owned())),
                ("permanence", FieldValue::Str("permanent")),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_record_degraded_sensors_emits_gauge() -> TestResult {
        let sink = RecordingSink::default();
        record_degraded_sensors(&sink, 3);

        let records = sink
            .records
            .lock()
            .map_err(ctx("test mutex not poisoned"))?;
        assert_eq!(records[0].0, SENSORS_DEGRADED.name);
        assert_eq!(records[0].1, MetricValue::Gauge(3.0));
        assert!(records[0].2.is_empty());
        Ok(())
    }

    #[test]
    fn test_record_buffer_utilization_labels_by_kind() -> TestResult {
        let sink = RecordingSink::default();
        record_buffer_utilization(&sink, BufferKind::Events, 0.5);

        let records = sink
            .records
            .lock()
            .map_err(ctx("test mutex not poisoned"))?;
        assert_eq!(records[0].0, BUFFER_UTILIZATION.name);
        assert_eq!(records[0].1, MetricValue::Gauge(0.5));
        assert_eq!(records[0].2, vec![("buffer", FieldValue::Str("events"))]);
        Ok(())
    }

    #[test]
    fn test_permanence_label_is_exhaustive_and_lowercase() {
        assert_eq!(permanence_label(Permanence::Transient), "transient");
        assert_eq!(permanence_label(Permanence::Permanent), "permanent");
    }

    #[test]
    fn test_buffer_kind_labels_are_lowercase() {
        assert_eq!(BufferKind::Samples.label(), "samples");
        assert_eq!(BufferKind::Events.label(), "events");
    }

    #[test]
    fn test_all_declared_metrics_are_registered() {
        assert_eq!(super::ALL.len(), 4);
    }
}

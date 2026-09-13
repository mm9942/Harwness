//! Der Sink-Trait und die Voreinstellung `NullSink`.
//!
//! # Verantwortungsbereich
//! Trägt [`TelemetrySink`] und [`NullSink`] (Vertrag A.3,
//! `docs/aw-contract-master.md`). Kennt kein Backend — jeder echte Sink
//! (Datei, Prometheus, OTLP) lebt in einer eigenen Crate und implementiert
//! diesen Trait. `harw-observe-file` ist der erste echte Implementierer.
//!
//! # Nebenläufigkeit
//! `TelemetrySink: Send + Sync`; Implementierungen werden über
//! `Arc<dyn TelemetrySink>` geteilt und aus jedem Thread aufgerufen, ohne
//! dass der Aufrufer eine Sperre hält. Ein Sink, der internen Zustand
//! mutiert, muss diese Nebenläufigkeit selbst moderieren (z. B. über einen
//! `Mutex` oder atomare Zähler).
//!
//! # Fehler
//! `record` und `flush` geben `()` zurück (Vertrag A.3, zweite
//! Festlegung): ein Sink behandelt seine eigenen Fehler und zählt sie
//! selbst, statt die beobachtete Operation scheitern zu lassen.
//!
//! # Examples
//! ```
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, NullSink, TelemetrySink, Unit};
//!
//! const KEY: MetricKey = MetricKey {
//!     name: "noop",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! let sink = NullSink;
//! sink.record(&KEY, MetricValue::Count(1), &[]);
//! sink.flush();
//! assert_eq!(sink.name(), "null");
//! ```

use crate::field::{FieldName, FieldValue};
use crate::metric::{MetricKey, MetricValue};

/// Ein Ziel für Messwerte.
///
/// # Die drei Festlegungen
/// - **Receiver `&self`**: Sinks werden über `Arc<dyn TelemetrySink>`
///   geteilt und aus jedem Thread aufgerufen. `&mut self` verlangte einen
///   Mutex auf dem heißesten Pfad des Systems.
/// - **Rückgabe `()`**: Telemetrie darf die Operation, die sie beobachtet,
///   niemals scheitern lassen. Ein Sink behandelt seine Fehler selbst und
///   zählt sie über einen eigenen Zähler; ein `Result` hier bedeutete, dass
///   jeder Aufrufer eine Entscheidung treffen müsste, die er nicht treffen
///   kann.
/// - **Synchron**: der Kern hat Pfade ohne Runtime (`harw-sentinel` läuft
///   ohne `tokio`). Ein `async`-Trait schlösse sie aus.
pub trait TelemetrySink: Send + Sync + std::fmt::Debug {
    /// Nimmt einen Messwert entgegen.
    ///
    /// # Description
    /// Reine Beobachtungsschnittstelle: nimmt einen einzelnen Messpunkt
    /// entgegen und liefert `()`, egal ob das Schreiben gelingt. Fehler
    /// bleiben Sache der Implementierung (siehe die drei Festlegungen
    /// oben).
    ///
    /// # Arguments
    /// - `key` (`&MetricKey`): Name, Art, Einheit, Labelfelder und
    ///   Kardinalitätsgrenze der Messgröße.
    /// - `value` (`MetricValue`): der gemessene Wert.
    /// - `labels` (`&[(FieldName, FieldValue)]`): die konkreten
    ///   Label-Belegungen für diesen Aufruf.
    ///
    /// # Panics
    /// Implementierungen sollen niemals paniken; ein Fehler wird intern
    /// behandelt und gezählt.
    ///
    /// # Concurrency
    /// Wird aus beliebigen Threads gleichzeitig aufgerufen, ohne dass der
    /// Aufrufer eine Sperre hält. Implementierungen müssen parallele
    /// Aufrufe selbst absichern.
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]);

    /// Erzwingt das Ausschreiben gepufferter Werte.
    ///
    /// # Panics
    /// Implementierungen sollen niemals paniken; ein Fehler wird intern
    /// behandelt und gezählt.
    ///
    /// # Concurrency
    /// Wird aus beliebigen Threads gleichzeitig aufgerufen, ohne dass der
    /// Aufrufer eine Sperre hält.
    fn flush(&self);

    /// Name des Sinks für Diagnosezwecke.
    ///
    /// # Returns
    /// Ein kurzer, statischer Bezeichner, z. B. `"null"` oder `"file"`.
    fn name(&self) -> &'static str;
}

/// Ein Sink, der alles verwirft. Voreinstellung, damit Emission nie an
/// einer fehlenden Konfiguration scheitert.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl TelemetrySink for NullSink {
    fn record(&self, _key: &MetricKey, _value: MetricValue, _labels: &[(FieldName, FieldValue)]) {}

    fn flush(&self) {}

    fn name(&self) -> &'static str {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{Cardinality, MetricKind, Unit};
    use std::sync::Arc;

    const KEY: MetricKey = MetricKey {
        name: "x",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    #[test]
    fn test_null_sink_name_is_null() {
        assert_eq!(NullSink.name(), "null");
    }

    #[test]
    fn test_null_sink_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<NullSink>();
    }

    #[test]
    fn test_null_sink_usable_through_arc_dyn_telemetry_sink() {
        let sink: Arc<dyn TelemetrySink> = Arc::new(NullSink);
        sink.record(&KEY, MetricValue::Count(1), &[]);
        sink.flush();
        assert_eq!(sink.name(), "null");
    }

    #[test]
    fn test_null_sink_default_behaves_like_explicit_value() {
        // `NullSink` trägt keine `PartialEq`-Ableitung (Vertrag A.3
        // wörtlich übernommen); die Gleichwertigkeit von `default()` und
        // dem Literal wird daher über ihr beobachtbares Verhalten geprüft.
        // `default_constructed_unit_structs` ausdrücklich erlaubt: der
        // Vergleich von `NullSink` mit `NullSink::default()` **ist** der
        // Gegenstand dieses Tests. Clippys Rat, `default()` wegzulassen,
        // würde ihn seines Zwecks berauben.
        #[allow(clippy::default_constructed_unit_structs)]
        {
            assert_eq!(NullSink.name(), NullSink::default().name());
        }
    }
}

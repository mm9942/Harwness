//! Metrikschlüssel und -werte: Art, Einheit, Kardinalität und der
//! vollständige Schlüssel.
//!
//! # Verantwortungsbereich
//! Trägt [`MetricKind`], [`Unit`], [`Cardinality`], [`MetricKey`] und
//! [`MetricValue`] (Vertrag A.2, `docs/design/build-history.md`). Reine
//! Werttypen ohne Verhalten — die Emission übernimmt
//! [`crate::sink::TelemetrySink`].
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Copy` und ohne Interior Mutability; `MetricKey` trägt
//! nur `&'static`-Referenzen, ist also selbst über `const`-Deklarationen
//! hinweg beliebig zwischen Threads teilbar.
//!
//! # Fehler
//! Keine — reine Werttypen ohne fehlbare Konstruktion.
//!
//! # Examples
//! ```
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, Unit};
//!
//! const KEY: MetricKey = MetricKey {
//!     name: "jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//! let value = MetricValue::Count(1);
//! assert_eq!(KEY.kind, MetricKind::Counter);
//! assert!(matches!(value, MetricValue::Count(1)));
//! ```

use crate::field::FieldName;

/// Art einer Messgröße.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricKind {
    /// Monoton steigender Zähler.
    Counter,
    /// Momentaufnahme, die steigen und fallen kann.
    Gauge,
    /// Verteilung von Beobachtungen.
    Histogram,
}

/// Basiseinheit einer Messgröße. Geschlossen — eine neue Einheit ist eine
/// Entscheidung, kein freier String.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Reine Zählung ohne Einheit.
    Count,
    /// Bytes.
    Bytes,
    /// Sekunden.
    Seconds,
    /// Verhältnis, konventionell zwischen `0.0` und `1.0`.
    Ratio,
    /// Modell-Tokens.
    Tokens,
    /// Grad Celsius.
    Celsius,
}

/// Obergrenze der Labelkombinationen. Verhindert Kardinalitätsexplosion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cardinality {
    /// Höchstens so viele unterschiedliche Labelkombinationen.
    Bounded(u32),
    /// Genau eine Kombination — keine Label-Aufschlüsselung.
    Single,
}

/// Der vollständige Schlüssel einer Messgröße.
///
/// Nach AW3-04 (Prometheus-Sink mit Golden-Test) ist diese Form
/// eingefroren: jede Feldänderung wird danach zu einer
/// Golden-Test-Migration über alle bis dahin emittierten Metriken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MetricKey {
    /// Name der Messgröße, z. B. `"jobs_completed_total"`.
    pub name: &'static str,
    /// Art der Messgröße.
    pub kind: MetricKind,
    /// Basiseinheit.
    pub unit: Unit,
    /// Erlaubte Label-Feldnamen, in fester Reihenfolge.
    pub labels: &'static [FieldName],
    /// Obergrenze der Labelkombinationen.
    pub cardinality: Cardinality,
}

/// Der gemessene Wert.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricValue {
    /// Zählerstand.
    Count(u64),
    /// Momentanwert.
    Gauge(f64),
    /// Einzelbeobachtung für ein Histogramm.
    Observation(f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_LABELS: &[FieldName] = &[];

    #[test]
    fn test_metric_key_copy_and_eq() {
        const KEY_A: MetricKey = MetricKey {
            name: "x",
            kind: MetricKind::Gauge,
            unit: Unit::Bytes,
            labels: EMPTY_LABELS,
            cardinality: Cardinality::Single,
        };
        let key_b = KEY_A;
        assert_eq!(KEY_A, key_b);
    }

    #[test]
    fn test_metric_key_labels_preserve_order() {
        const LABELS: &[FieldName] = &[
            FieldName::from_static_unchecked("a"),
            FieldName::from_static_unchecked("b"),
        ];
        const KEY: MetricKey = MetricKey {
            name: "y",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: LABELS,
            cardinality: Cardinality::Single,
        };
        assert_eq!(KEY.labels[0].as_str(), "a");
        assert_eq!(KEY.labels[1].as_str(), "b");
    }

    #[test]
    fn test_metric_value_variants_distinct() {
        assert_ne!(MetricValue::Count(1), MetricValue::Gauge(1.0));
    }

    #[test]
    fn test_cardinality_bounded_holds_limit() {
        let c = Cardinality::Bounded(42);
        assert_eq!(c, Cardinality::Bounded(42));
    }

    #[test]
    fn test_unit_variants_are_distinguishable() {
        assert_ne!(Unit::Bytes, Unit::Seconds);
    }

    #[test]
    fn test_metric_kind_variants_are_distinguishable() {
        assert_ne!(MetricKind::Counter, MetricKind::Histogram);
    }
}

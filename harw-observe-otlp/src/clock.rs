//! Injizierbare Zeitquelle für Datenpunkt-Zeitstempel.
//!
//! # Verantwortungsbereich
//! Trägt [`OtlpClock`] und [`FixedClock`]. `harw-observe-otlp` ruft an
//! keiner Stelle `jiff::Timestamp::now()` auf (siehe Crate-Doc, Abschnitt
//! „Keine Systemuhr"): [`crate::OtlpSink`] bekommt seine Zeitquelle
//! bei der Konstruktion über dieses Trait gereicht. Der Grund ist
//! [`harw_observe::TelemetrySink::record`]s eingefrorene Signatur (Vertrag
//! A.3) — sie trägt keinen `timestamp`-Parameter, und `MetricValue` trägt
//! ebenfalls keinen. Ein Zeitstempel für einen Datenpunkt kann deshalb nur
//! auf einem dritten Weg entstehen: bei der Konstruktion des Sinks
//! „hereingereicht" (Auftrag AW7-02) statt zur Aufrufzeit aus der
//! Systemuhr gelesen. Die Kompositionsstelle (typischerweise `harw-cli`)
//! liefert eine echte Implementierung, die intern `jiff::Timestamp::now()`
//! aufruft — diese Crate selbst tut das nirgends, auch nicht als bequeme
//! Voreinstellung, um die Zusage wörtlich einzuhalten.
//!
//! # Nebenläufigkeit
//! [`OtlpClock`] verlangt `Send + Sync + Debug`, damit eine Implementierung
//! über `Arc<dyn OtlpClock>` geteilt und aus jedem Thread gleichzeitig
//! aufgerufen werden kann — dieselbe Erwartung wie an
//! [`harw_observe::TelemetrySink`]. [`FixedClock`] ist `Copy` und ohne
//! Interior Mutability.
//!
//! # Fehler
//! Keine — [`OtlpClock::now`] ist eine unfehlbare Abfrage.
//!
//! # Examples
//! ```
//! use harw_observe_otlp::{FixedClock, OtlpClock};
//!
//! let clock = FixedClock::new(jiff::Timestamp::UNIX_EPOCH);
//! assert_eq!(clock.now(), jiff::Timestamp::UNIX_EPOCH);
//! ```

use std::fmt;

/// Eine Zeitquelle für Zeitstempel auf OTLP-Datenpunkten.
///
/// Siehe Moduldoc für die Begründung, warum diese Crate selbst keine
/// systemuhrbasierte Implementierung mitbringt.
pub trait OtlpClock: Send + Sync + fmt::Debug {
    /// Der Zeitstempel, den [`crate::OtlpSink::record`] dem gerade
    /// eingehenden Messpunkt zuordnet.
    ///
    /// # Returns
    /// Den aktuellen Zeitstempel dieser Zeitquelle.
    ///
    /// # Concurrency
    /// Wird aus beliebigen Threads gleichzeitig aufgerufen, ohne dass der
    /// Aufrufer eine Sperre hält — dieselbe Erwartung wie
    /// [`harw_observe::TelemetrySink::record`].
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::{FixedClock, OtlpClock};
    ///
    /// let clock = FixedClock::new(jiff::Timestamp::UNIX_EPOCH);
    /// assert_eq!(clock.now(), jiff::Timestamp::UNIX_EPOCH);
    /// ```
    fn now(&self) -> jiff::Timestamp;
}

/// Eine feste Zeitquelle für deterministische Tests.
///
/// Liefert bei jedem Aufruf denselben, bei der Konstruktion übergebenen
/// Zeitstempel — die einzige Zeitquelle, die diese Crate selbst mitbringt
/// (siehe Moduldoc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(jiff::Timestamp);

impl FixedClock {
    /// Baut eine Zeitquelle, die immer `timestamp` liefert.
    ///
    /// # Arguments
    /// - `timestamp` (`jiff::Timestamp`): der stets zurückgegebene Wert.
    ///
    /// # Returns
    /// Eine [`FixedClock`] für `timestamp`.
    ///
    /// # Examples
    /// ```
    /// use harw_observe_otlp::FixedClock;
    ///
    /// let clock = FixedClock::new(jiff::Timestamp::UNIX_EPOCH);
    /// ```
    #[must_use]
    pub const fn new(timestamp: jiff::Timestamp) -> Self {
        Self(timestamp)
    }
}

impl OtlpClock for FixedClock {
    fn now(&self) -> jiff::Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixed_clock_always_returns_the_same_timestamp() {
        let ts = jiff::Timestamp::UNIX_EPOCH;
        let clock = FixedClock::new(ts);
        assert_eq!(clock.now(), ts);
        assert_eq!(clock.now(), ts);
    }

    #[test]
    fn test_fixed_clock_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + fmt::Debug>() {}
        assert_bounds::<FixedClock>();
    }

    #[test]
    fn test_fixed_clock_usable_through_dyn_otlp_clock() {
        let clock: Box<dyn OtlpClock> = Box::new(FixedClock::new(jiff::Timestamp::UNIX_EPOCH));
        assert_eq!(clock.now(), jiff::Timestamp::UNIX_EPOCH);
    }
}

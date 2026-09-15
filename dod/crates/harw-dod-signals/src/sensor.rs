//! Die eine Quelle für Hostbeobachtungen: `trait Sensor` und sein Ergebnistyp (Contract-Master §G).
//!
//! # Der eine Trait, an dem elf Crates gleichzeitig hängen
//! Jede Sensor-Crate implementiert [`Sensor`] genau einmal, für genau eine
//! Quelle und genau eine Fähigkeit ([`harw_dod_cap::Capability`]).
//!
//! # Was ein Sensor NICHT darf
//! - **Keine zweite Quelle.** Eine Sensor-Crate liest genau eine Quelle
//!   (eine `/sys`- oder `/proc`-Datei, einen Netlink-Socket, eine
//!   Log-Datei — nie mehrere). Wer zwei Quellen korrelieren will, gehört zu
//!   `harw-dod-rules`, nicht hierher.
//! - **Keine Kenntnis einer anderen Sensor-Crate.** Sensor-Crates hängen an
//!   `harw-dod-cap` und `harw-dod-signals`, nie aneinander. Es gibt keinen
//!   Mechanismus, mit dem ein Sensor das Ergebnis eines anderen Sensors
//!   liest.
//! - **Keinen Befund erzeugen.** `Finding` entsteht ausschließlich in
//!   `harw-dod-rules` (Contract-Master §G.1). Ein Sensor liefert
//!   [`SensorReading`] — Rohbeobachtungen, keine Bewertung.
//! - **Nie die Systemuhr lesen.** [`Sensor::poll`] bekommt `now` injiziert.
//!   Ein Sensor, der `std::time::SystemTime::now()` oder
//!   `jiff::Timestamp::now()` selbst aufruft, ist gegen kein Fixture mehr
//!   deterministisch prüfbar — und die Fixture-Prüfung „Determinismus" ist
//!   die wichtigste der sechs Harness-Prüfungen.
//! - **Kein `std::fs` außerhalb des `ReadScope`.** Jede Dateisystem-Lesung
//!   läuft über `harw_dod_cap::SensorHandle::scope()` bzw.
//!   `ReadScope::open`, nie über einen direkten `std::fs`-Aufruf auf einen
//!   selbst konstruierten Pfad. Der Lesebereich ist die einzige Instanz, die
//!   weiß, was erlaubt ist.
//!
//! # Verantwortungsbereich
//! Trägt [`SensorReading`] und `trait` [`Sensor`].
//!
//! # Nebenläufigkeit
//! `Sensor: Send + Sync`: der Sentinel hält Sensoren hinter `Arc` und ruft
//! sie aus einem Sammelthread auf. [`Sensor::poll`] nimmt `&self`, weil ein
//! Sensor keinen veränderlichen Zustand über einen Abruf hinaus führen darf
//! — zwei gleichzeitige `poll`-Aufrufe auf demselben Sensor müssen sicher
//! sein.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`], inhaltsfrei (Contract-Master §F). Diese
//! Crate definiert keinen eigenen Fehler für `poll` — der Trait ist wörtlich
//! aus dem Vertrag übernommen.
//!
//! # Examples
//! ```rust
//! use harw_dod_signals::{Sensor, SensorReading};
//!
//! fn poll_all(sensors: &[Box<dyn Sensor>], now: jiff::Timestamp) -> Vec<SensorReading> {
//!     sensors
//!         .iter()
//!         .filter_map(|sensor| sensor.poll(now).ok())
//!         .collect()
//! }
//!
//! let sensors: Vec<Box<dyn Sensor>> = vec![];
//! assert!(poll_all(&sensors, jiff::Timestamp::UNIX_EPOCH).is_empty());
//! ```

use crate::event::SecurityEvent;
use crate::sample::HostSample;

/// Was ein Sensor bei einem Abruf liefert.
///
/// # Description
/// Ein reiner Ergebnis-Container: die Samples und Events, die ein einzelner
/// [`Sensor::poll`]-Aufruf beobachtet hat. `Default` liefert einen leeren
/// Abruf — nützlich, wenn eine Quelle in einem Zyklus nichts zu melden hat.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::SensorReading;
///
/// let empty = SensorReading::default();
/// assert!(empty.samples.is_empty());
/// assert!(empty.events.is_empty());
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SensorReading {
    /// Die in diesem Abruf beobachteten Messwerte.
    pub samples: Vec<HostSample>,
    /// Die in diesem Abruf beobachteten Ereignisse.
    pub events: Vec<SecurityEvent>,
}

/// Eine Quelle für Hostbeobachtungen.
///
/// # Description
/// Siehe Moduldoku für die vollständige Liste dessen, was ein Sensor NICHT
/// darf. Jede Sensor-Crate implementiert diesen Trait genau einmal.
///
/// # Errors
/// [`harw_dod_cap::SensorError`], inhaltsfrei — kein Feldwert, kein Pfad,
/// keine gelesene Zeile erscheint darin.
///
/// # Concurrency
/// `Send + Sync`: der Sentinel hält Sensoren hinter `Arc` und ruft sie aus
/// einem Sammelthread auf. `poll` nimmt `&self`, weil ein Sensor keinen
/// veränderlichen Zustand über den Abruf hinaus führen darf — konkurrierende
/// `poll`-Aufrufe auf demselben Sensor müssen sicher sein. Objektsicher:
/// haltbar als `Box<dyn Sensor>` (der Sentinel hält heterogene Sensoren in
/// einer gemeinsamen Sammlung, siehe Tests unten).
///
/// # Examples
/// ```rust
/// use harw_dod_signals::{Sensor, SensorReading};
/// use harw_types::SensorId;
///
/// #[derive(Debug)]
/// struct AlwaysEmpty {
///     handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
/// }
///
/// impl Sensor for AlwaysEmpty {
///     fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound> {
///         &self.handle
///     }
///
///     fn poll(&self, _now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
///         Ok(SensorReading::default())
///     }
/// }
///
/// let scope = harw_dod_cap::ReadScope::from_roots([std::path::PathBuf::from("/proc/stat")]);
/// let handle = harw_dod_cap::SensorHandle::new(
///     SensorId::from_str("proc-stat"),
///     harw_dod_cap::Capability::ReadProcStat,
/// )
/// .bind(scope);
/// let sensor = AlwaysEmpty { handle };
/// let reading = sensor
///     .poll(jiff::Timestamp::UNIX_EPOCH)
///     .expect("mock sensor never fails");
/// assert!(reading.samples.is_empty());
/// ```
pub trait Sensor: Send + Sync + std::fmt::Debug {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den [`harw_dod_cap::SensorHandle`], der Kennung,
    /// Fähigkeit und Lesebereich dieses Sensors trägt.
    fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound>;

    /// Liest die Quelle einmal aus.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit. Ein Sensor liest nie
    ///   die Systemuhr selbst — sonst ist er gegen kein Fixture mehr
    ///   deterministisch prüfbar (siehe Moduldoku).
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit den in diesem Abruf beobachteten Samples
    /// und Events. Kann leer sein.
    ///
    /// # Errors
    /// [`harw_dod_cap::SensorError`], wenn die Quelle nicht lesbar ist,
    /// außerhalb des Lesebereichs liegt oder eine unerwartete Form hat.
    fn poll(
        &self,
        now: jiff::Timestamp,
    ) -> Result<SensorReading, harw_dod_cap::SensorError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    use harw_types::SensorId;

    #[derive(Debug)]
    struct MockSensor {
        handle: SensorHandle<harw_dod_cap::Bound>,
        reading: SensorReading,
    }

    impl Sensor for MockSensor {
        fn handle(&self) -> &SensorHandle<harw_dod_cap::Bound> {
            &self.handle
        }

        fn poll(
            &self,
            _now: jiff::Timestamp,
        ) -> Result<SensorReading, harw_dod_cap::SensorError> {
            Ok(self.reading.clone())
        }
    }

    fn mock_handle() -> SensorHandle<harw_dod_cap::Bound> {
        let scope = ReadScope::from_roots([std::path::PathBuf::from("/sys/class/thermal")]);
        SensorHandle::new(SensorId::from_str("mock-thermal"), Capability::ReadSysfsThermal)
            .bind(scope)
    }

    #[test]
    fn test_sensor_reading_default_is_empty() {
        let reading = SensorReading::default();
        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_mock_sensor_is_object_safe_as_boxed_trait_object() {
        let mock = MockSensor {
            handle: mock_handle(),
            reading: SensorReading::default(),
        };
        let boxed: Box<dyn Sensor> = Box::new(mock);

        let reading = boxed
            .poll(jiff::Timestamp::UNIX_EPOCH)
            .expect("mock sensor never fails");
        assert_eq!(reading, SensorReading::default());
        assert_eq!(boxed.handle().capability(), Capability::ReadSysfsThermal);
    }

    #[test]
    fn test_boxed_sensors_can_be_held_in_a_heterogeneous_collection() {
        let first = MockSensor {
            handle: mock_handle(),
            reading: SensorReading::default(),
        };
        let second = MockSensor {
            handle: mock_handle(),
            reading: SensorReading::default(),
        };
        let sensors: Vec<Box<dyn Sensor>> = vec![Box::new(first), Box::new(second)];
        assert_eq!(sensors.len(), 2);
    }
}

//! `FsMonSensor`: bindet Quelle, Bereich und Formung zu einem
//! `harw_dod_signals::Sensor` zusammen.
//!
//! # Verantwortungsbereich
//! [`FsMonSensor`] hält einen [`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`]
//! (Kennung, [`harw_dod_cap::Capability::WatchFilesystem`] — die eine
//! Fähigkeit dieser Crate — und den überwachten `ReadScope`) sowie eine
//! [`crate::raw::FsEventSource`]-Implementierung. `poll` liest eine Charge
//! roher Ereignisse und formt jedes über [`crate::shape::shape_event`] — die
//! Brücke zwischen dem generischen Sensor-Vokabular
//! (`harw_dod_signals::Sensor`) und dem fsmon-eigenen Formungsteil.
//!
//! # Nebenläufigkeit
//! `Sensor: Send + Sync + std::fmt::Debug`. `Box<dyn FsEventSource>` selbst
//! trägt kein `Debug` (der Trait aus [`crate::raw`] verlangt es bewusst
//! nicht — seine Fläche ist wörtlich durch den Vertrag dieses Knotens
//! festgelegt), deshalb implementiert [`FsMonSensor`] `Debug` von Hand und
//! zeigt nur Griff und `proc_root`.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`], gebildet aus
//! [`crate::error::FsMonError`] über dessen `From`-Implementierung.
//!
//! # Examples
//! ```rust
//! use std::path::{Path, PathBuf};
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_fsmon::raw::{FixtureFsEventSource, RawFsEvent};
//! use harw_dod_fsmon::sensor::FsMonSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//!
//! let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
//! let handle = SensorHandle::new(SensorId::from_str("fsmon-0"), Capability::WatchFilesystem)
//!     .bind(scope);
//! let source = FixtureFsEventSource::new(vec![RawFsEvent {
//!     mask: 0x08,
//!     pid: 1,
//!     uid: 0,
//!     fd_target: "/srv/data/report.csv".to_owned(),
//! }]);
//!
//! let sensor = FsMonSensor::new(handle, Box::new(source), PathBuf::from("/proc"));
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH).expect("Fixture-Quelle scheitert nie");
//! assert_eq!(reading.events.len(), 1);
//! ```

use std::path::PathBuf;
use std::time::Duration;

use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{Sensor, SensorReading};
use jiff::Timestamp;

use crate::raw::FsEventSource;
use crate::shape::shape_event;

/// Wie lange ein einzelner [`FsMonSensor::poll`]-Aufruf höchstens auf
/// Ereignisse der zugrunde liegenden Quelle wartet.
const POLL_TIMEOUT: Duration = Duration::from_millis(100);

/// Ein Dateisystem-Wächter-Sensor: Quelle, Bereich und Formung zusammen.
///
/// # Description
/// Siehe Moduldoku.
pub struct FsMonSensor {
    handle: SensorHandle<Bound>,
    source: Box<dyn FsEventSource>,
    proc_root: PathBuf,
}

impl std::fmt::Debug for FsMonSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsMonSensor")
            .field("handle", &self.handle)
            .field("proc_root", &self.proc_root)
            .finish()
    }
}

impl FsMonSensor {
    /// Baut einen `FsMonSensor` aus einem gebundenen Griff, einer
    /// Ereignisquelle und der Wurzel für die loginuid-Auflösung.
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): trägt
    ///   Kennung, [`harw_dod_cap::Capability::WatchFilesystem`] und den
    ///   überwachten `ReadScope`.
    /// - `source` (`Box<dyn `[`crate::raw::FsEventSource`]`>`): die Quelle
    ///   roher Ereignisse — in Produktion die fanotify-Bindung, in Tests
    ///   [`crate::raw::FixtureFsEventSource`].
    /// - `proc_root` (`std::path::PathBuf`): die Wurzel für die
    ///   loginuid-Auflösung (siehe [`crate::loginuid::resolve_loginuid`]).
    ///
    /// # Returns
    /// Einen einsatzbereiten `FsMonSensor`.
    ///
    /// # Errors
    /// Keine — der Konstruktor ist total.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::{Path, PathBuf};
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_fsmon::raw::FixtureFsEventSource;
    /// use harw_dod_fsmon::sensor::FsMonSensor;
    /// use harw_types::SensorId;
    ///
    /// let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
    /// let handle =
    ///     SensorHandle::new(SensorId::from_str("fsmon-0"), Capability::WatchFilesystem)
    ///         .bind(scope);
    /// let _sensor = FsMonSensor::new(
    ///     handle,
    ///     Box::new(FixtureFsEventSource::new(vec![])),
    ///     PathBuf::from("/proc"),
    /// );
    /// ```
    #[must_use]
    pub fn new(handle: SensorHandle<Bound>, source: Box<dyn FsEventSource>, proc_root: PathBuf) -> Self {
        Self {
            handle,
            source,
            proc_root,
        }
    }
}

impl Sensor for FsMonSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self.source.read_events(POLL_TIMEOUT)?;

        let mut events = Vec::with_capacity(raw_events.len());
        for raw in &raw_events {
            let shaped = shape_event(raw, self.handle.scope(), &self.proc_root, self.handle.id(), now)?;
            if let Some(event) = shaped {
                events.push(event);
            }
        }

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    use harw_dod_signals::{EventKind, Sensor};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::FsMonSensor;
    use crate::raw::{FixtureFsEventSource, RawFsEvent};

    fn handle_for(scope: ReadScope) -> SensorHandle<harw_dod_cap::Bound> {
        SensorHandle::new(SensorId::from_str("fsmon-test"), Capability::WatchFilesystem).bind(scope)
    }

    #[test]
    fn test_capability_is_watch_filesystem() {
        let handle = handle_for(ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]));
        assert_eq!(handle.capability(), Capability::WatchFilesystem);
    }

    #[test]
    fn test_poll_shapes_fixture_events_into_security_events() {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let source = FixtureFsEventSource::new(vec![RawFsEvent {
            mask: 0x08,
            pid: 1,
            uid: 0,
            fd_target: "/srv/data/report.csv".to_owned(),
        }]);
        let sensor = FsMonSensor::new(handle_for(scope), Box::new(source), PathBuf::from("/proc"));

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("Fixture-Quelle scheitert nie");

        assert_eq!(reading.samples.len(), 0);
        assert_eq!(reading.events.len(), 1);
        assert!(matches!(reading.events[0].kind, EventKind::FileWrite { .. }));
    }

    #[test]
    fn test_poll_drops_events_outside_scope() {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let source = FixtureFsEventSource::new(vec![RawFsEvent {
            mask: 0x08,
            pid: 1,
            uid: 0,
            fd_target: "/etc/passwd".to_owned(),
        }]);
        let sensor = FsMonSensor::new(handle_for(scope), Box::new(source), PathBuf::from("/proc"));

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("Formung selbst schlägt hier nicht fehl");

        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_poll_propagates_malformed_source_as_sensor_error() {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let source = FixtureFsEventSource::new(vec![RawFsEvent {
            mask: 0x01, // FAN_ACCESS: kein Schreibzugriff, nicht interpretierbar
            pid: 1,
            uid: 0,
            fd_target: "/srv/data/report.csv".to_owned(),
        }]);
        let sensor = FsMonSensor::new(handle_for(scope), Box::new(source), PathBuf::from("/proc"));

        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("unerwartete Ereignisform muss scheitern");

        assert!(matches!(err, harw_dod_cap::SensorError::MalformedSource));
    }

    #[test]
    fn test_sensor_is_usable_as_boxed_trait_object() {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let sensor = FsMonSensor::new(
            handle_for(scope),
            Box::new(FixtureFsEventSource::new(vec![])),
            PathBuf::from("/proc"),
        );
        let boxed: Box<dyn Sensor> = Box::new(sensor);
        assert_eq!(boxed.handle().capability(), Capability::WatchFilesystem);
    }
}

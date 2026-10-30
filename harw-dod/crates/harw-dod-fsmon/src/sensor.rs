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
//! [`crate::error::FsMonError`] über dessen `From`-Implementierung — nur für
//! Fehler beim Lesen der zugrunde liegenden Ereignisquelle selbst
//! ([`crate::raw::FsEventSource::read_events`]). Ein einzelnes Ereignis mit
//! nicht interpretierbarer Maske lässt [`FsMonSensor::poll`] seit dieser
//! Korrektur nicht mehr scheitern — es wird übersprungen, siehe dortige
//! Dokumentation.
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
    pub fn new(
        handle: SensorHandle<Bound>,
        source: Box<dyn FsEventSource>,
        proc_root: PathBuf,
    ) -> Self {
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

    /// Liest eine Charge roher Ereignisse und formt jedes einzeln.
    ///
    /// # Description
    /// Ein einzelnes Ereignis, dessen Maske [`crate::mask::interpret_mask`]
    /// nicht deuten kann (z. B. `FAN_ACCESS`/`FAN_OPEN` — laut
    /// `crate::mask`-Moduldoku bewusst nicht interpretierbar, aber je nach
    /// `fanotify`-Markierung des Bindungsteils durchaus lieferbar), lässt den
    /// **gesamten** Abruf nicht mehr scheitern: es wird übersprungen, die
    /// übrigen Ereignisse derselben Charge werden trotzdem geformt und
    /// gemeldet. `shape_event` kann innerhalb dieser Schleife ausschließlich
    /// [`crate::error::FsMonError::MalformedSource`] liefern — Lese- oder
    /// E/A-Fehler der Quelle selbst entstehen bereits beim vorangehenden
    /// `read_events`-Aufruf und werden dort weiterhin über `?` propagiert.
    /// Vorher hätte ein einzelnes, gewöhnliches Lese-/Öffnen-Ereignis
    /// irgendeines beobachteten Prozesses alle anderen, gültig geformten
    /// Schreibereignisse desselben Abrufs mit verworfen.
    ///
    /// # Errors
    /// [`SensorError`], wenn [`crate::raw::FsEventSource::read_events`]
    /// selbst scheitert. Eine einzelne nicht interpretierbare Ereignisform
    /// lässt diesen Abruf nicht mehr scheitern (siehe oben).
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self.source.read_events(POLL_TIMEOUT)?;

        let mut events = Vec::with_capacity(raw_events.len());
        for raw in &raw_events {
            match shape_event(
                raw,
                self.handle.scope(),
                &self.proc_root,
                self.handle.id(),
                now,
            ) {
                Ok(Some(event)) => events.push(event),
                Ok(None) => {}
                Err(_) => {
                    // Nicht interpretierbare Einzelform (siehe Doku oben) —
                    // überspringen statt den gesamten Abruf scheitern zu
                    // lassen.
                }
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
    use crate::test_support::{TestResult, ctx};

    fn handle_for(scope: ReadScope) -> SensorHandle<harw_dod_cap::Bound> {
        SensorHandle::new(
            SensorId::from_str("fsmon-test"),
            Capability::WatchFilesystem,
        )
        .bind(scope)
    }

    #[test]
    fn test_capability_is_watch_filesystem() {
        let handle = handle_for(ReadScope::from_roots(
            [Path::new("/srv/data").to_path_buf()],
        ));
        assert_eq!(handle.capability(), Capability::WatchFilesystem);
    }

    #[test]
    fn test_poll_shapes_fixture_events_into_security_events() -> TestResult {
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
            .map_err(ctx("Fixture-Quelle scheitert nie"))?;

        assert_eq!(reading.samples.len(), 0);
        assert_eq!(reading.events.len(), 1);
        assert!(matches!(
            reading.events[0].kind,
            EventKind::FileWrite { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_poll_drops_events_outside_scope() -> TestResult {
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
            .map_err(ctx("Formung selbst schlägt hier nicht fehl"))?;

        assert!(reading.events.is_empty());
        Ok(())
    }

    /// Vor dieser Korrektur ließ ein einzelnes nicht interpretierbares
    /// Ereignis den gesamten Abruf scheitern — jetzt wird es stillschweigend
    /// übersprungen, ohne dass `poll` einen Fehler zurückgibt.
    #[test]
    fn test_poll_skips_unrecognized_event_shape_instead_of_failing() -> TestResult {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let source = FixtureFsEventSource::new(vec![RawFsEvent {
            mask: 0x01, // FAN_ACCESS: kein Schreibzugriff, nicht interpretierbar
            pid: 1,
            uid: 0,
            fd_target: "/srv/data/report.csv".to_owned(),
        }]);
        let sensor = FsMonSensor::new(handle_for(scope), Box::new(source), PathBuf::from("/proc"));

        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "eine einzelne nicht interpretierbare Ereignisform darf den Abruf nicht scheitern lassen",
        ))?;

        assert!(reading.events.is_empty());
        Ok(())
    }

    /// Kernbeleg der Korrektur: ein nicht interpretierbares Ereignis darf
    /// nicht auch die anderen, gültig geformten Ereignisse derselben Charge
    /// mit verwerfen.
    #[test]
    fn test_poll_reports_valid_event_alongside_a_skipped_unrecognized_one() -> TestResult {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let source = FixtureFsEventSource::new(vec![
            RawFsEvent {
                mask: 0x01, // FAN_ACCESS: nicht interpretierbar, wird übersprungen
                pid: 1,
                uid: 0,
                fd_target: "/srv/data/other.csv".to_owned(),
            },
            RawFsEvent {
                mask: 0x08, // FAN_CLOSE_WRITE: gültig
                pid: 2,
                uid: 0,
                fd_target: "/srv/data/report.csv".to_owned(),
            },
        ]);
        let sensor = FsMonSensor::new(handle_for(scope), Box::new(source), PathBuf::from("/proc"));

        let reading = sensor.poll(Timestamp::UNIX_EPOCH).map_err(ctx(
            "das gültige Ereignis darf den Abruf nicht scheitern lassen",
        ))?;

        assert_eq!(
            reading.events.len(),
            1,
            "genau das gültige Ereignis muss ankommen, das übersprungene nicht"
        );
        assert!(matches!(
            reading.events[0].kind,
            EventKind::FileWrite { .. }
        ));
        Ok(())
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

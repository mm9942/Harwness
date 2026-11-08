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
//! # Überlauf, Filter und Zusammenfassen (PL-100 WP1.6)
//! [`FsMonSensor::poll`] verarbeitet jede Charge in dieser Reihenfolge:
//! 1. Ein rohes Ereignis mit [`crate::mask::FAN_Q_OVERFLOW`] (die Bindung
//!    reicht einen Warteschlangen-Überlauf mit dieser Maske weiter) wird zu
//!    genau einem `EventKind::SensorDegraded { sensor: <eigene Kennung> }`
//!    je Abruf — ein Überlauf heißt „Ereignisse unbekannter Zahl verloren",
//!    das ist Blindheit, kein Schreibzugriff.
//! 2. Jedes andere Ereignis wird geformt ([`crate::shape::shape_event`]).
//! 3. Ein optionaler [`crate::filter::WatchFilter`]
//!    ([`FsMonSensor::with_watch_filter`]) lässt unter Home-Wurzeln nur die
//!    konfigurierten Home-Einträge durch.
//! 4. Ein [`crate::coalesce::WriteCoalescer`] fasst Wiederholungen je
//!    (Pfad, pid) innerhalb von [`crate::coalesce::COALESCE_WINDOW`]
//!    zusammen.
//!
//! # Nebenläufigkeit
//! `Sensor: Send + Sync + std::fmt::Debug`. `Box<dyn FsEventSource>` selbst
//! trägt kein `Debug` (der Trait aus [`crate::raw`] verlangt es bewusst
//! nicht — seine Fläche ist wörtlich durch den Vertrag dieses Knotens
//! festgelegt), deshalb implementiert [`FsMonSensor`] `Debug` von Hand und
//! zeigt nur Griff, `proc_root` und Filter. Der Zustand des Zusammenfassers
//! liegt hinter einem `Mutex`; `poll(&self)` hält ihn nur für die Dauer
//! einer Charge, ein vergifteter `Mutex` wird übernommen (der Zustand ist
//! nach jedem Einzelschritt gültig).
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

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
use jiff::Timestamp;

use crate::coalesce::WriteCoalescer;
use crate::filter::WatchFilter;
use crate::mask::is_queue_overflow;
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
    // Optionaler Filter für Home-Wurzeln (`None`: jeder Pfad im Bereich).
    filter: Option<WatchFilter>,
    // Zusammenfassen wiederholter Schreibereignisse je (Pfad, pid).
    coalescer: Mutex<WriteCoalescer>,
}

impl std::fmt::Debug for FsMonSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsMonSensor")
            .field("handle", &self.handle)
            .field("proc_root", &self.proc_root)
            .field("filter", &self.filter)
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
            filter: None,
            coalescer: Mutex::new(WriteCoalescer::new()),
        }
    }

    /// Setzt den Filter für Home-Wurzeln (PL-100 WP1.6).
    ///
    /// # Description
    /// Ohne Filter reicht [`Sensor::poll`] jeden Pfad im `ReadScope` des
    /// Griffs weiter; mit Filter nur die Pfade, die
    /// [`WatchFilter::admits`] annimmt. Der `ReadScope` des Griffs sollte
    /// [`WatchFilter::read_scope`] sein.
    ///
    /// # Arguments
    /// - `filter` (`WatchFilter`): der Beobachtungsbereich der Sonde.
    ///
    /// # Returns
    /// Den Sensor mit Filter.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::PathBuf;
    /// use harw_dod_cap::{Capability, SensorHandle};
    /// use harw_dod_fsmon::filter::WatchFilter;
    /// use harw_dod_fsmon::raw::FixtureFsEventSource;
    /// use harw_dod_fsmon::sensor::FsMonSensor;
    /// use harw_types::SensorId;
    ///
    /// let filter = WatchFilter::new(
    ///     vec![PathBuf::from("/etc")],
    ///     vec![PathBuf::from("/home")],
    ///     vec![PathBuf::from(".ssh")],
    /// );
    /// let handle = SensorHandle::new(SensorId::from_str("fsmon-0"), Capability::WatchFilesystem)
    ///     .bind(filter.read_scope());
    /// let _sensor = FsMonSensor::new(
    ///     handle,
    ///     Box::new(FixtureFsEventSource::new(vec![])),
    ///     PathBuf::from("/proc"),
    /// )
    /// .with_watch_filter(filter);
    /// ```
    #[must_use]
    pub fn with_watch_filter(mut self, filter: WatchFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Zahl der seit dem Bau zusammengefassten (verworfenen) Wiederholungen.
    ///
    /// # Returns
    /// Den Zähler des [`WriteCoalescer`].
    #[must_use]
    pub fn coalesced_total(&self) -> u64 {
        self.coalescer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .coalesced_total()
    }

    // Das `SensorDegraded`-Ereignis dieses Sensors für einen
    // Warteschlangen-Überlauf.
    fn overflow_event(&self, now: Timestamp) -> SecurityEvent {
        SecurityEvent {
            sensor: self.handle.id().clone(),
            observed_at: now,
            actor: None,
            kind: EventKind::SensorDegraded {
                sensor: self.handle.id().clone(),
            },
        }
    }

    // Filter und Zusammenfassen für ein geformtes Schreibereignis.
    fn admits_write(
        &self,
        path: &str,
        pid: u32,
        now: Timestamp,
        coalescer: &mut WriteCoalescer,
    ) -> bool {
        if let Some(filter) = &self.filter
            && !filter.admits(Path::new(path))
        {
            return false;
        }
        coalescer.admit(path, pid, now)
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
    /// Vor dem Formen wird ein Warteschlangen-Überlauf erkannt (höchstens ein
    /// `SensorDegraded` je Abruf, an der Stelle des ersten Überlaufs), nach
    /// dem Formen filtert ein gesetzter [`WatchFilter`] und fasst der
    /// [`WriteCoalescer`] Wiederholungen zusammen (siehe Moduldoku).
    ///
    /// # Errors
    /// [`SensorError`], wenn [`crate::raw::FsEventSource::read_events`]
    /// selbst scheitert. Eine einzelne nicht interpretierbare Ereignisform
    /// lässt diesen Abruf nicht mehr scheitern (siehe oben).
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self.source.read_events(POLL_TIMEOUT)?;

        let mut events = Vec::with_capacity(raw_events.len());
        let mut overflow_reported = false;
        let mut coalescer = self
            .coalescer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for raw in &raw_events {
            if is_queue_overflow(raw.mask) {
                if !overflow_reported {
                    events.push(self.overflow_event(now));
                    overflow_reported = true;
                }
                continue;
            }
            match shape_event(
                raw,
                self.handle.scope(),
                &self.proc_root,
                self.handle.id(),
                now,
            ) {
                Ok(Some(event)) => {
                    let admitted = if let EventKind::FileWrite { path } = &event.kind {
                        self.admits_write(path, raw.pid, now, &mut coalescer)
                    } else {
                        true
                    };
                    if admitted {
                        events.push(event);
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    // Nicht interpretierbare Einzelform (siehe Doku oben) —
                    // überspringen statt den gesamten Abruf scheitern zu
                    // lassen.
                }
            }
        }
        drop(coalescer);

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
    use crate::filter::WatchFilter;
    use crate::mask::{FAN_CLOSE_WRITE, FAN_MODIFY, FAN_Q_OVERFLOW};
    use crate::raw::{FixtureFsEventSource, RawFsEvent};
    use crate::test_support::{TestError, TestResult, ctx};

    fn raw(mask: u64, pid: u32, path: &str) -> RawFsEvent {
        RawFsEvent {
            mask,
            pid,
            uid: 1000,
            fd_target: path.to_owned(),
        }
    }

    fn sensor_over(scope: ReadScope, events: Vec<RawFsEvent>) -> FsMonSensor {
        FsMonSensor::new(
            handle_for(scope),
            Box::new(FixtureFsEventSource::new(events)),
            PathBuf::from("/nonexistent-harw-fsmon-proc"),
        )
    }

    fn written_paths(events: &[harw_dod_signals::SecurityEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::FileWrite { path } => Some(path.as_str()),
                _ => None,
            })
            .collect()
    }

    // `UNIX_EPOCH + secs`.
    fn at(secs: i64) -> TestResult<Timestamp> {
        Timestamp::UNIX_EPOCH
            .checked_add(jiff::SignedDuration::from_secs(secs))
            .map_err(ctx("timestamp in range"))
    }

    /// PL-100 WP1.6: ein Warteschlangen-Überlauf wird genau ein
    /// `SensorDegraded` mit der eigenen Kennung — nie ein `FileWrite`, auch
    /// bei mehreren Überlauf-Markern in einer Charge; die übrigen Ereignisse
    /// derselben Charge kommen trotzdem an.
    #[test]
    fn test_poll_reports_a_queue_overflow_once_as_sensor_degraded() -> TestResult {
        let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
        let sensor = sensor_over(
            scope,
            vec![
                raw(FAN_Q_OVERFLOW, 0, ""),
                raw(FAN_CLOSE_WRITE, 2, "/srv/data/report.csv"),
                raw(FAN_Q_OVERFLOW, 0, ""),
            ],
        );

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("fixture never fails"))?;

        let degraded: Vec<&str> = reading
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::SensorDegraded { sensor } => Some(sensor.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(degraded, vec!["fsmon-test"]);
        let first = reading
            .events
            .first()
            .ok_or(TestError::Missing("the degraded event"))?;
        assert_eq!(first.sensor.as_str(), "fsmon-test");
        assert!(first.actor.is_none());
        assert_eq!(written_paths(&reading.events), vec!["/srv/data/report.csv"]);
        Ok(())
    }

    /// PL-100 WP1.6: wiederholte `FAN_MODIFY` desselben Prozesses auf
    /// denselben Pfad werden innerhalb des Fensters zusammengefasst — auch
    /// über zwei Abrufe; ein anderer Prozess und ein anderer Pfad nie, und
    /// nach dem Fenster kommt der Pfad wieder durch.
    #[test]
    fn test_poll_coalesces_repeated_modify_per_path_and_pid() -> TestResult {
        let scope = ReadScope::from_roots([Path::new("/etc").to_path_buf()]);
        let sensor = sensor_over(
            scope,
            vec![
                raw(FAN_MODIFY, 7, "/etc/cron.d/job"),
                raw(FAN_MODIFY, 7, "/etc/cron.d/job"),
                raw(FAN_MODIFY | FAN_CLOSE_WRITE, 7, "/etc/cron.d/job"),
                raw(FAN_MODIFY, 8, "/etc/cron.d/job"),
                raw(FAN_MODIFY, 7, "/etc/hosts"),
            ],
        );

        let first = sensor.poll(at(0)?).map_err(ctx("first poll"))?;
        assert_eq!(
            written_paths(&first.events),
            vec!["/etc/cron.d/job", "/etc/cron.d/job", "/etc/hosts"]
        );
        assert_eq!(sensor.coalesced_total(), 2);

        // Dieselbe Charge eine Sekunde später: alles noch im Fenster.
        let second = sensor.poll(at(1)?).map_err(ctx("second poll"))?;
        assert!(written_paths(&second.events).is_empty());

        // Nach dem Fenster kommt jeder Schlüssel genau einmal wieder durch.
        let third = sensor.poll(at(10)?).map_err(ctx("third poll"))?;
        assert_eq!(
            written_paths(&third.events),
            vec!["/etc/cron.d/job", "/etc/cron.d/job", "/etc/hosts"]
        );
        Ok(())
    }

    /// PL-100 WP1.6: unter einer Home-Wurzel kommen nur die Home-Einträge
    /// durch, unter einer vollständigen Wurzel alles.
    #[test]
    fn test_poll_with_watch_filter_forwards_only_home_entries_below_home_roots() -> TestResult {
        let filter = WatchFilter::new(
            vec![PathBuf::from("/etc")],
            vec![PathBuf::from("/home"), PathBuf::from("/root")],
            vec![PathBuf::from(".ssh"), PathBuf::from(".config/autostart")],
        );
        let sensor = sensor_over(
            filter.read_scope(),
            vec![
                raw(FAN_CLOSE_WRITE, 1, "/home/alice/.cache/big.bin"),
                raw(FAN_CLOSE_WRITE, 2, "/home/alice/.ssh/authorized_keys"),
                raw(FAN_CLOSE_WRITE, 3, "/root/.config/autostart/x.desktop"),
                raw(FAN_CLOSE_WRITE, 4, "/etc/ld.so.preload"),
                raw(FAN_CLOSE_WRITE, 5, "/var/log/syslog"),
            ],
        )
        .with_watch_filter(filter);

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("fixture never fails"))?;
        assert_eq!(
            written_paths(&reading.events),
            vec![
                "/home/alice/.ssh/authorized_keys",
                "/root/.config/autostart/x.desktop",
                "/etc/ld.so.preload"
            ]
        );
        Ok(())
    }

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

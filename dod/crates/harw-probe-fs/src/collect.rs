//! Sammelschleife: liest die konfigurierte Quelle aus und leitet jedes
//! geformte Ereignis an die Senke weiter.
//!
//! # Verantwortungsbereich
//! [`run_once`] führt genau eine Erfassungsrunde aus: `sensor.poll(now)`
//! liest die zugrunde liegende `harw_dod_fsmon::FsEventSource` einmal aus,
//! formt jedes Rohereignis über die Bibliothek
//! (`harw_dod_fsmon::shape_event`, intern in `FsMonSensor::poll`
//! aufgerufen) und verwirft dabei automatisch alles außerhalb des
//! konfigurierten `ReadScope` — diese Crate wiederholt diese Logik nicht
//! selbst, sie ruft nur `FsMonSensor::poll` auf und leitet dessen Ereignisse
//! an [`crate::sink::EventSink`] weiter. [`run_forever`] ruft [`run_once`]
//! wiederholt auf, bis ein dauerhafter Fehler auftritt.
//!
//! # Ohne Kernel testbar
//! Jeder Test dieses Moduls konstruiert seinen `FsMonSensor` mit einer
//! `harw_dod_fsmon::FixtureFsEventSource` — es gibt noch keine reale
//! fanotify-Bindung, gegen die getestet werden könnte (siehe
//! [`crate::source`]). [`run_once`] selbst unterscheidet nicht zwischen
//! einer echten und einer Fixture-Quelle; die gesamte Sammellogik ist
//! deshalb bereits heute vollständig geprüft, unabhängig vom Stand der
//! fanotify-Bindung.
//!
//! # Exportierte Typen
//! [`run_once`], [`run_forever`].
//!
//! # Nebenläufigkeit
//! Beide Funktionen nehmen `&FsMonSensor` und `&dyn EventSink` — keine
//! innere Veränderlichkeit auf dieser Ebene.
//!
//! # Fehler
//! [`crate::error::ProbeError`], durchgereicht aus `sensor.poll` (über
//! `harw_dod_cap::SensorError`) oder aus [`crate::sink::EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::collect::run_once;
//! ```

use harw_dod_cap::Permanence;
use harw_dod_fsmon::FsMonSensor;
use harw_dod_signals::Sensor;
use jiff::Timestamp;

use crate::error::ProbeError;
use crate::sink::EventSink;

/// Führt eine einzelne Erfassungsrunde aus.
///
/// # Description
/// Liest `sensor` genau einmal aus und sendet jedes dabei geformte Ereignis
/// über `sink`. Bricht beim ersten Sendefehler ab, ohne die verbleibenden
/// Ereignisse dieser Runde zu senden.
///
/// # Arguments
/// - `sensor` (`&harw_dod_fsmon::FsMonSensor`): der Sensor, dessen Quelle in
///   dieser Runde einmal ausgelesen wird.
/// - `sink` (`&dyn crate::sink::EventSink`): die Senke, an die jedes
///   geformte Ereignis dieser Runde gesendet wird.
/// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert an
///   `sensor.poll` weitergereicht.
///
/// # Returns
/// Die Anzahl der in dieser Runde gesendeten Ereignisse.
///
/// # Errors
/// - [`ProbeError::Sensor`]: wenn `sensor.poll` scheitert.
/// - Der Fehler von [`EventSink::send`]: wenn das Senden eines Ereignisses
///   scheitert.
pub fn run_once(sensor: &FsMonSensor, sink: &dyn EventSink, now: Timestamp) -> Result<usize, ProbeError> {
    let reading = sensor.poll(now)?;
    for event in &reading.events {
        sink.send(event)?;
    }
    Ok(reading.events.len())
}

/// Läuft unbegrenzt: ruft [`run_once`] wiederholt auf, bis ein dauerhafter
/// Fehler auftritt.
///
/// # Description
/// Ein Sensorfehler mit [`Permanence::Transient`] wird geloggt und die
/// Schleife läuft weiter; jeder andere Fehler (dauerhafte Sensorfehler,
/// jeder Sendefehler der Senke) beendet die Schleife. Liest die Systemuhr
/// selbst — das ist an dieser Stelle richtig, nicht in `FsMonSensor` selbst
/// (siehe `harw_dod_fsmon`-Moduldoku, Abschnitt „Push-only"/injizierte
/// Zeit): diese Funktion ist die Kompositionswurzel, die laut Konvention
/// dieses Workspace die Systemuhr lesen darf.
///
/// # Arguments
/// - `sensor` (`&harw_dod_fsmon::FsMonSensor`): der Sensor, der wiederholt
///   abgefragt wird.
/// - `sink` (`&dyn crate::sink::EventSink`): die Senke, an die jedes
///   geformte Ereignis gesendet wird.
///
/// # Returns
/// Diese Funktion kehrt nur über einen Fehler zurück; es gibt keinen
/// regulären `Ok`-Rückweg außer einem Abbruch von außen (Prozesssignal).
///
/// # Errors
/// Der erste dauerhafte Fehler aus [`run_once`].
pub fn run_forever(sensor: &FsMonSensor, sink: &dyn EventSink) -> Result<(), ProbeError> {
    loop {
        let now = Timestamp::now();
        match run_once(sensor, sink, now) {
            Ok(_) => {}
            Err(ProbeError::Sensor(err)) if err.permanence() == Permanence::Transient => {
                tracing::warn!(error = %err, "transient sensor error; retrying");
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    use harw_dod_fsmon::{FixtureFsEventSource, FsMonSensor, RawFsEvent};
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::run_once;
    use crate::error::ProbeError;
    use crate::sink::EventSink;

    /// Test-Senke: zeichnet jedes gesendete Ereignis auf, statt es zu
    /// übertragen. Öffnet keinen Socket.
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<SecurityEvent>>,
    }

    impl EventSink for RecordingSink {
        fn send(&self, event: &SecurityEvent) -> Result<(), ProbeError> {
            self.sent
                .lock()
                .expect("test mutex is never poisoned")
                .push(event.clone());
            Ok(())
        }
    }

    fn sensor_with(scope: ReadScope, events: Vec<RawFsEvent>) -> FsMonSensor {
        let handle = SensorHandle::new(SensorId::from_str("fsmon-test"), Capability::WatchFilesystem).bind(scope);
        FsMonSensor::new(handle, Box::new(FixtureFsEventSource::new(events)), PathBuf::from("/proc"))
    }

    #[test]
    fn test_run_once_forwards_every_event_from_the_source_to_the_sink() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        let sensor = sensor_with(
            scope,
            vec![RawFsEvent {
                mask: 0x08,
                pid: 42,
                uid: 1000,
                fd_target: "/srv/data/report.csv".to_owned(),
            }],
        );
        let sink = RecordingSink::default();

        let sent = run_once(&sensor, &sink, Timestamp::UNIX_EPOCH).expect("fixture source never fails");

        assert_eq!(sent, 1);
        let recorded = sink.sent.lock().expect("test mutex is never poisoned");
        assert_eq!(recorded.len(), 1);
        assert!(matches!(recorded[0].kind, EventKind::FileWrite { .. }));
    }

    #[test]
    fn test_run_once_drops_an_event_outside_the_read_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        let sensor = sensor_with(
            scope,
            vec![RawFsEvent {
                mask: 0x08,
                pid: 42,
                uid: 1000,
                fd_target: "/etc/passwd".to_owned(),
            }],
        );
        let sink = RecordingSink::default();

        let sent = run_once(&sensor, &sink, Timestamp::UNIX_EPOCH).expect("shaping itself does not fail here");

        assert_eq!(sent, 0);
        assert!(sink.sent.lock().expect("test mutex is never poisoned").is_empty());
    }

    #[test]
    fn test_run_once_forwards_zero_events_for_an_empty_fixture() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        let sensor = sensor_with(scope, vec![]);
        let sink = RecordingSink::default();

        let sent = run_once(&sensor, &sink, Timestamp::UNIX_EPOCH).expect("empty fixture never fails");
        assert_eq!(sent, 0);
    }

    #[test]
    fn test_run_once_forwarded_event_carries_no_file_content() {
        let dir = tempfile::tempdir().expect("tempdir for fixture target");
        let secret_path = dir.path().join("secret.txt");
        std::fs::write(&secret_path, b"TOP-SECRET-CONTENT-1234").expect("write fixture file");

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let sensor = sensor_with(
            scope,
            vec![RawFsEvent {
                mask: 0x08,
                pid: 42,
                uid: 1000,
                fd_target: secret_path.to_string_lossy().into_owned(),
            }],
        );
        let sink = RecordingSink::default();

        run_once(&sensor, &sink, Timestamp::UNIX_EPOCH).expect("fixture source never fails");

        let recorded = sink.sent.lock().expect("test mutex is never poisoned");
        assert_eq!(recorded.len(), 1);
        let json = serde_json::to_string(&recorded[0]).expect("SecurityEvent serializes");
        assert!(!json.contains("TOP-SECRET-CONTENT-1234"));
    }

    #[test]
    fn test_run_once_stops_at_the_first_sink_error() {
        struct FailingSink;
        impl EventSink for FailingSink {
            fn send(&self, _event: &SecurityEvent) -> Result<(), ProbeError> {
                Err(ProbeError::SentinelSendFailed)
            }
        }

        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        let sensor = sensor_with(
            scope,
            vec![RawFsEvent {
                mask: 0x08,
                pid: 42,
                uid: 1000,
                fd_target: "/srv/data/report.csv".to_owned(),
            }],
        );

        let err = run_once(&sensor, &FailingSink, Timestamp::UNIX_EPOCH)
            .expect_err("a sink failure must propagate");
        assert!(matches!(err, ProbeError::SentinelSendFailed));
    }
}

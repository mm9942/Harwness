//! Sammelschleife: pollt beide Sensoren gleichförmig und leitet jedes
//! geformte Ereignis an die Senke weiter.
//!
//! # Verantwortungsbereich
//! [`run_once`] führt genau eine Erfassungsrunde über eine Liste von
//! `harw_dod_signals::Sensor`-Trait-Objekten aus: `sensor.poll(now)` je
//! Sensor, jedes dabei geformte Ereignis wird an [`crate::sink::EventSink`]
//! weitergereicht. Diese Datei kennt weder `harw_dod_procmon::ProcmonSensor`
//! noch `crate::sensors::FlowSensor` namentlich — sie iteriert generisch
//! über `Vec<std::sync::Arc<dyn harw_dod_signals::Sensor>>`, genau die Form,
//! die `crate::sensors`-Moduldoku als das Ergebnis des dort gefällten
//! Urteils beschreibt: **eine** Sammelschleife für beide geerbten Quellen,
//! nicht zwei verschiedene. [`run_forever`] ruft [`run_once`] wiederholt
//! auf, bis ein dauerhafter Fehler auftritt.
//!
//! # Ohne Kernel, ohne Socket testbar
//! Jeder Test dieses Moduls konstruiert seine Sensoren entweder als reine
//! Mock-Implementierungen von `harw_dod_signals::Sensor` oder — für den
//! Test, der beide geerbten Quellen zugleich belegt — über
//! `crate::sensors::build_procmon_sensor`/`build_flow_sensor` mit je einer
//! eigenen `harw_dod_bpf::fixture::FixtureBpfLoader`-Instanz. Kein Test
//! dieses Moduls öffnet einen echten Socket oder bindet ein echtes
//! Landlock/eBPF.
//!
//! # Exportierte Typen
//! [`run_once`], [`run_forever`].
//!
//! # Nebenläufigkeit
//! Beide Funktionen nehmen `&[std::sync::Arc<dyn harw_dod_signals::Sensor>]`
//! und `&dyn crate::sink::EventSink` — keine innere Veränderlichkeit auf
//! dieser Ebene.
//!
//! # Fehler
//! [`crate::error::ProbeError`], durchgereicht aus `sensor.poll` (über
//! `harw_dod_cap::SensorError`) oder aus
//! [`crate::sink::EventSink::send`].
//!
//! # Examples
//! ```rust,ignore
//! use crate::collect::run_once;
//! ```

use std::sync::Arc;

use harw_dod_cap::Permanence;
use harw_dod_signals::Sensor;
use jiff::Timestamp;

use crate::error::ProbeError;
use crate::sink::EventSink;

/// Führt eine einzelne Erfassungsrunde über alle übergebenen Sensoren aus.
///
/// # Description
/// Pollt jeden Sensor in `sensors` genau einmal (in Reihenfolge der Liste)
/// und sendet jedes dabei geformte Ereignis über `sink`. Bricht beim ersten
/// Fehler ab — sowohl ein Sensorfehler als auch ein Sendefehler beenden
/// diese Runde sofort, ohne die verbleibenden Sensoren oder Ereignisse noch
/// zu bearbeiten.
///
/// # Arguments
/// - `sensors` (`&[std::sync::Arc<dyn harw_dod_signals::Sensor>]`): die
///   Sensoren, die in dieser Runde je einmal ausgelesen werden.
/// - `sink` (`&dyn crate::sink::EventSink`): die Senke, an die jedes
///   geformte Ereignis dieser Runde gesendet wird.
/// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert an jedes
///   `sensor.poll` weitergereicht.
///
/// # Returns
/// Die Anzahl der in dieser Runde gesendeten Ereignisse.
///
/// # Errors
/// - [`ProbeError::Sensor`]: wenn ein `sensor.poll` scheitert.
/// - Der Fehler von [`EventSink::send`]: wenn das Senden eines Ereignisses
///   scheitert.
pub fn run_once(
    sensors: &[Arc<dyn Sensor>],
    sink: &dyn EventSink,
    now: Timestamp,
) -> Result<usize, ProbeError> {
    let mut sent = 0usize;
    for sensor in sensors {
        let reading = sensor.poll(now)?;
        for event in &reading.events {
            sink.send(event)?;
            sent += 1;
        }
    }
    Ok(sent)
}

/// Läuft unbegrenzt: ruft [`run_once`] wiederholt auf, bis ein dauerhafter
/// Fehler auftritt.
///
/// # Description
/// Ein Sensorfehler mit `harw_dod_cap::Permanence::Transient` wird geloggt
/// und die Schleife läuft weiter; jeder andere Fehler (dauerhafte
/// Sensorfehler, jeder Sendefehler der Senke) beendet die Schleife. Liest
/// die Systemuhr selbst — das ist an dieser Stelle richtig, nicht in einem
/// Sensor selbst (siehe `harw_dod_signals::sensor`-Moduldoku, Abschnitt
/// „Nie die Systemuhr lesen"): diese Funktion ist die Kompositionswurzel,
/// die laut Konvention dieses Workspace die Systemuhr lesen darf (Muster:
/// `harw-probe-fs::collect::run_forever`, `harw-sentinel::main::poll_once`).
///
/// # Arguments
/// - `sensors` (`&[std::sync::Arc<dyn harw_dod_signals::Sensor>]`): die
///   Sensoren, die wiederholt abgefragt werden.
/// - `sink` (`&dyn crate::sink::EventSink`): die Senke, an die jedes
///   geformte Ereignis gesendet wird.
///
/// # Returns
/// Diese Funktion kehrt nur über einen Fehler zurück; es gibt keinen
/// regulären `Ok`-Rückweg außer einem Abbruch von außen (Prozesssignal).
///
/// # Errors
/// Der erste dauerhafte Fehler aus [`run_once`].
pub fn run_forever(sensors: &[Arc<dyn Sensor>], sink: &dyn EventSink) -> Result<(), ProbeError> {
    loop {
        let now = Timestamp::now();
        match run_once(sensors, sink, now) {
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
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use harw_dod_bpf::event::RawBpfEvent;
    use harw_dod_bpf::fixture::FixtureBpfLoader;
    use harw_dod_bpf::BpfProgramSource;
    use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
    use harw_sandbox::NetworkScope;
    use harw_types::{ContentDigest, SensorId};
    use jiff::Timestamp;

    use super::{run_forever, run_once};
    use crate::error::ProbeError;
    use crate::sensors::{build_flow_sensor, build_procmon_sensor};
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

    #[derive(Debug)]
    struct StaticSensor {
        handle: SensorHandle<Bound>,
        events: Vec<SecurityEvent>,
    }

    impl Sensor for StaticSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            Ok(SensorReading {
                samples: Vec::new(),
                events: self.events.clone(),
            })
        }
    }

    fn static_sensor(sensor_id: &str, events: Vec<SecurityEvent>) -> Arc<dyn Sensor> {
        let handle = SensorHandle::new(SensorId::from_str(sensor_id), Capability::LoadBpfProgram)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
        Arc::new(StaticSensor { handle, events })
    }

    fn sample_event(sensor_id: &str) -> SecurityEvent {
        SecurityEvent {
            sensor: SensorId::from_str(sensor_id),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::ProcessExec {
                path: "/usr/sbin/sshd".to_owned(),
                argv_digest: ContentDigest::of(b"unused in this test"),
            },
        }
    }

    #[test]
    fn test_run_once_forwards_every_event_from_every_sensor_to_the_sink() {
        let sensors = vec![
            static_sensor("probe-bpf-procmon-0", vec![sample_event("probe-bpf-procmon-0")]),
            static_sensor("probe-bpf-flow-0", vec![sample_event("probe-bpf-flow-0")]),
        ];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH).expect("static sensors never fail");

        assert_eq!(sent, 2);
        let recorded = sink.sent.lock().expect("test mutex is never poisoned");
        assert_eq!(recorded.len(), 2);
    }

    #[test]
    fn test_run_once_forwards_zero_events_for_sensors_with_no_reading() {
        let sensors = vec![static_sensor("probe-bpf-procmon-0", Vec::new())];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH).expect("static sensor never fails");
        assert_eq!(sent, 0);
    }

    #[test]
    fn test_run_once_stops_at_the_first_sink_error() {
        struct FailingSink;
        impl EventSink for FailingSink {
            fn send(&self, _event: &SecurityEvent) -> Result<(), ProbeError> {
                Err(ProbeError::SentinelSendFailed)
            }
        }

        let sensors = vec![static_sensor("probe-bpf-procmon-0", vec![sample_event("probe-bpf-procmon-0")])];

        let err = run_once(&sensors, &FailingSink, Timestamp::UNIX_EPOCH)
            .expect_err("a sink failure must propagate");
        assert!(matches!(err, ProbeError::SentinelSendFailed));
    }

    #[test]
    fn test_run_once_propagates_a_sensor_error() {
        #[derive(Debug)]
        struct FailingSensor {
            handle: SensorHandle<Bound>,
        }
        impl Sensor for FailingSensor {
            fn handle(&self) -> &SensorHandle<Bound> {
                &self.handle
            }
            fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
                Err(SensorError::OutsideScope)
            }
        }

        let handle = SensorHandle::new(SensorId::from_str("failing-0"), Capability::LoadBpfProgram)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
        let sensors: Vec<Arc<dyn Sensor>> = vec![Arc::new(FailingSensor { handle })];
        let sink = RecordingSink::default();

        let err = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH).expect_err("sensor error must propagate");
        assert!(matches!(err, ProbeError::Sensor(SensorError::OutsideScope)));
    }

    /// Ein Sensor, der zweimal einen transienten Fehler liefert und danach
    /// einen dauerhaften — belegt [`run_forever`]s Retry-Verhalten, ohne
    /// unbegrenzt zu laufen.
    #[derive(Debug)]
    struct FlakySensor {
        handle: SensorHandle<Bound>,
        call_count: AtomicUsize,
    }

    impl Sensor for FlakySensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }
        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(SensorError::MalformedSource)
            } else {
                Err(SensorError::OutsideScope)
            }
        }
    }

    #[test]
    fn test_run_forever_retries_transient_errors_and_stops_on_a_permanent_one() {
        let handle = SensorHandle::new(SensorId::from_str("flaky-0"), Capability::LoadBpfProgram)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
        let sensors: Vec<Arc<dyn Sensor>> = vec![Arc::new(FlakySensor {
            handle,
            call_count: AtomicUsize::new(0),
        })];
        let sink = RecordingSink::default();

        let err = run_forever(&sensors, &sink).expect_err("a permanent error must terminate the loop");
        assert!(matches!(err, ProbeError::Sensor(SensorError::OutsideScope)));
    }

    /// Der wichtigste Integrationstest dieser Crate: beide geerbten
    /// Formungscrates kommen durch dieselbe Sammelschleife, mit
    /// unterscheidbarer Herkunft (`SensorId`) — genau die Zusage aus
    /// `crate::sensors`-Moduldoku, mein Urteil zur
    /// Schnittstellen-Unstimmigkeit.
    #[test]
    fn test_run_once_carries_both_inherited_sources_through_with_distinguishable_sensor_ids() {
        let procmon_payload = {
            let mut payload = Vec::new();
            payload.extend_from_slice(&4_242u32.to_le_bytes()); // pid
            payload.extend_from_slice(&1u32.to_le_bytes()); // ppid
            payload.extend_from_slice(&0u32.to_le_bytes()); // uid
            payload.extend_from_slice(&[0u8; 16]); // comm
            let mut filename = [0u8; 256];
            filename[.."/usr/sbin/sshd".len()].copy_from_slice(b"/usr/sbin/sshd");
            payload.extend_from_slice(&filename);
            payload.extend_from_slice(b"-D");
            payload
        };
        let procmon_loader = FixtureBpfLoader::new(vec![RawBpfEvent {
            pid: 4_242,
            comm: String::new(),
            observed_at: Timestamp::UNIX_EPOCH,
            payload: procmon_payload,
        }]);

        let flow_payload = {
            let mut bytes = vec![0u8; 32];
            bytes[0..4].copy_from_slice(&100u32.to_le_bytes());
            bytes[4..8].copy_from_slice(&1_000u32.to_le_bytes());
            bytes[8] = 0; // TCP
            bytes[9] = 1; // ausgehend
            bytes[10] = 0; // IPv4
            bytes[12..14].copy_from_slice(&443u16.to_be_bytes());
            bytes[16..20].copy_from_slice(&[203, 0, 113, 9]);
            bytes
        };
        let flow_loader = FixtureBpfLoader::new(vec![RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at: Timestamp::UNIX_EPOCH,
            payload: flow_payload,
        }]);

        let placeholder = BpfProgramSource::Embedded(Cow::Borrowed(&[]));
        let procmon_sensor = build_procmon_sensor(
            Box::new(procmon_loader),
            SensorId::from_str("probe-bpf-procmon-0"),
            placeholder.clone(),
        )
        .expect("fixture loader with capability always succeeds");
        let flow_sensor = build_flow_sensor(
            Box::new(flow_loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder,
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");

        let sensors: Vec<Arc<dyn Sensor>> = vec![Arc::new(procmon_sensor), Arc::new(flow_sensor)];
        let sink = RecordingSink::default();

        let sent = run_once(&sensors, &sink, Timestamp::UNIX_EPOCH).expect("both fixture sensors never fail");
        assert_eq!(sent, 2);

        let recorded = sink.sent.lock().expect("test mutex is never poisoned");
        assert_eq!(recorded.len(), 2);

        let procmon_event = recorded
            .iter()
            .find(|event| event.sensor == SensorId::from_str("probe-bpf-procmon-0"))
            .expect("the procmon event carries the procmon sensor id");
        assert!(matches!(procmon_event.kind, EventKind::ProcessExec { .. }));

        let flow_event = recorded
            .iter()
            .find(|event| event.sensor == SensorId::from_str("probe-bpf-flow-0"))
            .expect("the flow event carries the flow sensor id");
        assert!(matches!(flow_event.kind, EventKind::EgressFlow { .. }));

        assert_ne!(procmon_event.sensor, flow_event.sensor);
    }
}

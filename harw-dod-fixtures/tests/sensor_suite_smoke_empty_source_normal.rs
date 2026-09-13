//! Integrationstest: `sensor_suite!` erzeugt acht grüne Tests für einen
//! Sensor, dessen Quelle legitim fehlen darf (Ausprägung
//! `EmptyScopeExpectation::EmptySourceIsNormal`, siehe Knoten K48).
//!
//! Gegenstück zu `tests/sensor_suite_smoke.rs`, das die Vorgabe-Ausprägung
//! `EmptyScopeExpectation::SourceUnavailableIsError` über das reale Makro
//! belegt. Dieser Test belegt die Gegenausprägung — analog zu
//! `harw-dod-gpu` auf einem Host ohne GPU: eine fehlende Quelle ist hier
//! kein Fehler, sondern `Ok(SensorReading::default())`.
//!
//! Fixtures unter `selftest-fixtures-empty-source-normal/` (siehe dort):
//! `typical/` (ein normaler Fall) und `malformed/not-a-number/`. Kein
//! `adversarial/` — diese Prüfung ist optional und wird bereits von
//! `tests/sensor_suite_smoke.rs` abgedeckt.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_fixtures::harness::EmptyScopeExpectation;
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Wie `ThermalLikeSensor` in `tests/sensor_suite_smoke.rs`, meldet eine
/// fehlende Quelle aber als `Ok(SensorReading::default())` statt
/// `Err(SourceUnavailable)` — die einzige Verhaltensdifferenz.
#[derive(Debug)]
struct OptionalSourceSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for OptionalSourceSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for OptionalSourceSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let Some(root) = self.handle.scope().roots().next() else {
            return Ok(SensorReading::default());
        };
        let path = root.join("value");
        match harw_dod_readfs::parse_i64(self.handle.scope(), &path) {
            Ok(value) => Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("optional_source_value"),
                    value: value as f64,
                }],
                events: vec![],
            }),
            Err(ReadFsError::Scope(SensorError::Io(io_err)))
                if io_err.kind() == std::io::ErrorKind::NotFound =>
            {
                // Verhaltensdifferenz zu ThermalLikeSensor: eine fehlende
                // Quelle ist hier der Normalfall, kein Fehler.
                Ok(SensorReading::default())
            }
            Err(ReadFsError::Scope(inner)) => Err(inner),
            Err(_) => Err(SensorError::MalformedSource),
        }
    }
}

harw_dod_fixtures::sensor_suite! {
    sensor: OptionalSourceSensor,
    capability: Capability::ReadSysfsDrm,
    max_cardinality: 1,
    fixtures: "selftest-fixtures-empty-source-normal",
    empty_scope: EmptyScopeExpectation::EmptySourceIsNormal,
}

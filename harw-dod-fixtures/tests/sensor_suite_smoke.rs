//! Integrationstest: `sensor_suite!` erzeugt acht grüne Tests für einen
//! korrekt gebauten Beispielsensor mit der Vorgabe-Ausprägung
//! `EmptyScopeExpectation::SourceUnavailableIsError` (Feld `empty_scope`
//! weggelassen — siehe `tests/sensor_suite_smoke_empty_source_normal.rs`
//! für die Gegenausprägung).
//!
//! Belegt die Behauptung aus der Crate-Moduldoku, dass ein Sensor, der die
//! sieben Prüfungen respektiert, tatsächlich grün durchläuft — über das
//! reale Makro (nicht über einen direkten Aufruf von `harness::assert_*`,
//! das `tests/harness_violations.rs` für die roten Zweige übernimmt).
//!
//! Fixtures unter `selftest-fixtures/` (siehe dort): `typical/` (ein
//! normaler Fall), `malformed/not-a-number/` und `adversarial/overflow/`.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Liest die erste Zeile der Datei `value` unterhalb der einzigen
/// Scope-Wurzel als `i64` und meldet sie unter einem statischen
/// Metriknamen — bewusst minimal, um alle sieben Prüfungen zu bestehen.
#[derive(Debug)]
struct ThermalLikeSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for ThermalLikeSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for ThermalLikeSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let Some(root) = self.handle.scope().roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let path = root.join("value");
        match harw_dod_readfs::parse_i64(self.handle.scope(), &path) {
            Ok(value) => Ok(SensorReading {
                samples: vec![HostSample {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    metric: Cow::Borrowed("fixture_selftest_value"),
                    value: value as f64,
                }],
                events: vec![],
            }),
            Err(ReadFsError::Scope(SensorError::Io(io_err)))
                if io_err.kind() == std::io::ErrorKind::NotFound =>
            {
                Err(SensorError::SourceUnavailable)
            }
            Err(ReadFsError::Scope(inner)) => Err(inner),
            Err(_) => Err(SensorError::MalformedSource),
        }
    }
}

harw_dod_fixtures::sensor_suite! {
    sensor: ThermalLikeSensor,
    capability: Capability::ReadSysfsThermal,
    max_cardinality: 4,
    fixtures: "selftest-fixtures",
}

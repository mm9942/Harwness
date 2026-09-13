// Diagnostic: `#[sensor(bogus = "x", ...)]` — unbekannter Attributschlüssel.
// Expected: compile_error! "unbekannter sensor-Schlüssel; erwartet: capability, id, metrics, error"
use harw_macros::SensorSource;

#[derive(SensorSource)]
#[sensor(
    capability = ReadSysfsThermal,
    id = "thermal",
    metrics = "harw_dod_thermal_",
    bogus = "x",
)]
struct ThermalSensor {
    handle: (),
    #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
    zones: (),
}

fn main() {}

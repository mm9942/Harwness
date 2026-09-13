// Diagnostic: `#[sensor(capability = Bogus)]` — unbekannte Capability.
// Expected: compile_error! "unbekannte capability `Bogus`; erwartet eine von: ..."
use harw_macros::SensorSource;

#[derive(SensorSource)]
#[sensor(
    capability = Bogus,
    id = "thermal",
    metrics = "harw_dod_thermal_",
)]
struct ThermalSensor {
    handle: (),
    #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
    zones: (),
}

fn main() {}

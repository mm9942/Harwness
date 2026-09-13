// Diagnostic: `#[derive(SensorSource)]` ohne Feld `handle`.
// Expected: compile_error! "SensorSource erfordert ein Feld `handle: ...`"
use harw_macros::SensorSource;

#[derive(SensorSource)]
#[sensor(
    capability = ReadSysfsThermal,
    id = "thermal",
    metrics = "harw_dod_thermal_",
)]
struct ThermalSensor {
    #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
    zones: (),
}

fn main() {}

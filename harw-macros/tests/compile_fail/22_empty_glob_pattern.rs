// Diagnostic: `#[source(glob = "")]` — leeres Glob-Muster.
// Expected: compile_error! "Glob-Muster darf nicht leer sein"
use harw_macros::SensorSource;

#[derive(SensorSource)]
#[sensor(
    capability = ReadSysfsThermal,
    id = "thermal",
    metrics = "harw_dod_thermal_",
)]
struct ThermalSensor {
    handle: (),
    #[source(glob = "", parse = "i64", metric = "temperature_celsius")]
    zones: (),
}

fn main() {}

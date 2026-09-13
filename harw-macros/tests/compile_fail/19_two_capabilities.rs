// Diagnostic: `#[sensor(capability = ..., capability = ...)]` — zwei
// Capabilities in einer Deklaration.
// Expected: compile_error! "`capability` doppelt angegeben"
use harw_macros::SensorSource;

#[derive(SensorSource)]
#[sensor(
    capability = ReadSysfsThermal,
    id = "thermal",
    metrics = "harw_dod_thermal_",
    capability = ReadProcStat,
)]
struct ThermalSensor {
    handle: (),
    #[source(glob = "thermal_zone*/temp", parse = "i64", metric = "temperature_celsius")]
    zones: (),
}

fn main() {}

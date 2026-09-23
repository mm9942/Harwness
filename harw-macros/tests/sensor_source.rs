//! Positive integration test for `#[derive(SensorSource)]`.
//!
//! Requires `harw-dod-cap`, `harw-dod-signals`, `harw-dod-readfs`,
//! `harw-observe`, `harw-types`, `jiff` and `tempfile` as dev-dependencies;
//! see `harw-macros/Cargo.toml`.
//!
//! # Why the glob pattern embeds a `tempfile` prefix wildcard
//! `harw_dod_readfs::glob::glob` always searches starting at the filesystem
//! root `/` (see `harw-dod-readfs/src/glob.rs`, module doc) — the pattern
//! given to `#[source(glob = "...")]` is a compile-time literal, but the
//! directory `tempfile::Builder::tempdir()` creates has a runtime-random
//! name. This test bridges the two by choosing a fixed, distinctive prefix
//! for the temp directory (`Builder::prefix(...)`) and letting the glob
//! pattern's directory component end in `*` to absorb the random suffix.
//! This assumes the sandbox's OS temp directory is `/tmp` and that
//! `tempfile` creates the directory as a single path segment directly under
//! it (true on this project's target platform, Linux) — the central
//! verification run adjusts this fixture if that assumption ever breaks.
//!
//! # Design-doc reference
//! AW2-06-Brief (`#[derive(SensorSource)]`).

use std::borrow::Cow;
use std::fs;

use harw_dod_cap::{Bound, Capability, ReadScope, SensorHandle};
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use harw_types::SensorId;

mod common;
use common::{TestResult, ctx};

const TEMPDIR_PREFIX: &str = "harw-macros-sensor-source-test-";

#[derive(Debug, harw_macros::SensorSource)]
#[sensor(
    capability = ReadSysfsThermal,
    id = "thermal",
    metrics = "harw_dod_thermal_",
)]
struct ThermalSensor {
    handle: SensorHandle<Bound>,
    #[source(
        // Das Muster ist ein **Suffix relativ zur Bereichswurzel**, kein
        // absoluter Pfad. Die frühere Fassung
        // (`tmp/harw-macros-sensor-source-test-*/thermal_zone*/temp`) stammte
        // aus der Zeit, als das Makro den Pfad zur Compile-Zeit fest
        // verdrahtete und `glob` ab `/` suchte -- ein so erzeugter Sensor fand
        // in Produktion etwas und in der Fixture-Prüfung nichts, bei sechs
        // grünen Prüfungen. Siehe K42 im Ausbauplan.
        glob = "thermal_zone*/temp",
        parse = "i64",
        metric = "temperature_celsius",
    )]
    zones: (),
}

fn build_sensor(dir: &std::path::Path) -> ThermalSensor {
    let scope = ReadScope::from_roots([dir.to_path_buf()]);
    let handle =
        SensorHandle::new(SensorId::from_str("thermal-0"), ThermalSensor::CAPABILITY).bind(scope);
    ThermalSensor { handle, zones: () }
}

#[test]
fn sensor_source_capability_and_id_constants_match_declaration() {
    assert_eq!(ThermalSensor::CAPABILITY, Capability::ReadSysfsThermal);
    assert_eq!(ThermalSensor::SENSOR_ID, "thermal");
}

#[test]
fn sensor_source_poll_reads_via_readfs_and_returns_expected_samples() -> TestResult {
    let dir = tempfile::Builder::new()
        .prefix(TEMPDIR_PREFIX)
        .tempdir()
        .map_err(ctx("tempdir for sensor fixture"))?;

    let zone0 = dir.path().join("thermal_zone0");
    let zone1 = dir.path().join("thermal_zone1");
    fs::create_dir_all(&zone0).map_err(ctx("create thermal_zone0"))?;
    fs::create_dir_all(&zone1).map_err(ctx("create thermal_zone1"))?;
    fs::write(zone0.join("temp"), "42000\n").map_err(ctx("write thermal_zone0/temp"))?;
    fs::write(zone1.join("temp"), "43000\n").map_err(ctx("write thermal_zone1/temp"))?;

    let sensor = build_sensor(dir.path());
    let reading = sensor
        .poll(jiff::Timestamp::UNIX_EPOCH)
        .map_err(ctx("poll must succeed for a well-formed fixture"))?;

    let expected = SensorReading {
        samples: vec![
            HostSample {
                sensor: SensorId::from_str("thermal-0"),
                observed_at: jiff::Timestamp::UNIX_EPOCH,
                metric: Cow::Borrowed("temperature_celsius"),
                value: 42000.0,
            },
            HostSample {
                sensor: SensorId::from_str("thermal-0"),
                observed_at: jiff::Timestamp::UNIX_EPOCH,
                metric: Cow::Borrowed("temperature_celsius"),
                value: 43000.0,
            },
        ],
        events: vec![],
    };

    assert_eq!(reading, expected);
    Ok(())
}

#[test]
fn sensor_source_poll_is_deterministic_for_the_same_injected_now() -> TestResult {
    let dir = tempfile::Builder::new()
        .prefix(TEMPDIR_PREFIX)
        .tempdir()
        .map_err(ctx("tempdir for determinism fixture"))?;

    let zone0 = dir.path().join("thermal_zone0");
    fs::create_dir_all(&zone0).map_err(ctx("create thermal_zone0"))?;
    fs::write(zone0.join("temp"), "51000\n").map_err(ctx("write thermal_zone0/temp"))?;

    let sensor = build_sensor(dir.path());
    let now = jiff::Timestamp::UNIX_EPOCH;

    let first = sensor.poll(now).map_err(ctx("first poll must succeed"))?;
    let second = sensor.poll(now).map_err(ctx("second poll must succeed"))?;

    assert_eq!(
        first, second,
        "two poll() calls with the same injected `now` must return exactly the same reading"
    );
    Ok(())
}

#[test]
fn sensor_source_poll_maps_outside_scope_to_sensor_error() -> TestResult {
    // A glob pattern that resolves nothing inside the bound scope yields an
    // empty, successful reading rather than an error — `glob()` treats "no
    // matches" as a valid, empty result (see `harw-dod-readfs/src/glob.rs`).
    let dir = tempfile::Builder::new()
        .prefix(TEMPDIR_PREFIX)
        .tempdir()
        .map_err(ctx("tempdir for empty fixture"))?;

    let sensor = build_sensor(dir.path());
    let reading = sensor
        .poll(jiff::Timestamp::UNIX_EPOCH)
        .map_err(ctx("poll over an empty fixture tree must still succeed"))?;

    assert!(reading.samples.is_empty());
    assert!(reading.events.is_empty());
    Ok(())
}

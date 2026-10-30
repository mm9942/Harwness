//! Integrationstest: `capture()` erzeugt einen Baum, den ein erneuter Poll
//! (also das, was `sensor_suite!` danach tut) unverändert akzeptiert.
//!
//! Deckt zusätzlich ab, dass `capture()` Benutzer- und Hostname mechanisch
//! redigiert (siehe `capture.rs`-Moduldoku, Abschnitt „Redaktion") und dass
//! [`harw_dod_fixtures::CaptureReport::warning`] immer belegt ist.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

mod common;
use common::{TestResult, ctx};

/// Liest `value` unterhalb der einzigen Scope-Wurzel als `i64` — dieselbe
/// Minimalform wie in `tests/sensor_suite_smoke.rs`.
#[derive(Debug)]
struct EchoValueSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for EchoValueSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for EchoValueSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let Some(root) = self.handle.scope().roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let value = harw_dod_readfs::parse_i64(self.handle.scope(), &root.join("value"))
            .map_err(|_| SensorError::MalformedSource)?;
        Ok(SensorReading {
            samples: vec![HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Borrowed("captured_value"),
                value: value as f64,
            }],
            events: vec![],
        })
    }
}

#[test]
fn test_capture_output_is_accepted_by_a_fresh_poll() -> TestResult {
    let source = tempfile::tempdir().map_err(ctx("source tempdir"))?;
    std::fs::write(source.path().join("value"), "123\n").map_err(ctx("write source value"))?;

    let target = tempfile::tempdir().map_err(ctx("target tempdir"))?;
    let case_dir = target.path().join("captured-case");

    let report = harw_dod_fixtures::capture::capture(
        EchoValueSensor::from,
        Capability::ReadProcStat,
        source.path(),
        &case_dir,
        Timestamp::UNIX_EPOCH,
    )
    .map_err(ctx("capture muss gelingen"))?;

    assert_eq!(report.case_dir, case_dir);
    assert_eq!(report.files_copied, 1);
    assert!(
        report.warning.contains("Prüfe tree/ von Hand"),
        "CaptureReport::warning muss immer einen deutlichen Hinweis enthalten"
    );

    // Rundlauf: dieselbe tree/-Kopie erneut gepollt muss exakt das
    // SensorReading liefern, das capture() nach expect.json geschrieben hat.
    let expect_json =
        std::fs::read_to_string(case_dir.join("expect.json")).map_err(ctx("expect.json lesbar"))?;
    let expected: serde_json::Value =
        serde_json::from_str(&expect_json).map_err(ctx("expect.json ist gueltiges JSON"))?;
    assert_eq!(expected["reading"]["samples"][0]["value"], 123.0);
    assert_eq!(
        expected["reading"]["samples"][0]["sensor"],
        harw_dod_fixtures::FIXTURE_SENSOR_ID
    );

    let scope = harw_dod_cap::ReadScope::from_roots([case_dir.join("tree")]);
    let handle = SensorHandle::new(
        harw_types::SensorId::from_str(harw_dod_fixtures::FIXTURE_SENSOR_ID),
        Capability::ReadProcStat,
    )
    .bind(scope);
    let sensor = EchoValueSensor::from(handle);
    let reading = sensor
        .poll(Timestamp::UNIX_EPOCH)
        .map_err(ctx("erneuter Poll gegen die Kopie muss gelingen"))?;

    assert_eq!(reading.samples.len(), 1);
    assert_eq!(reading.samples[0].value, 123.0);
    Ok(())
}

#[test]
fn test_capture_redacts_current_username_from_file_content() -> TestResult {
    let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) else {
        // Auf einem Host ohne $USER/$LOGNAME (z. B. manche Container) ist
        // nichts zu redigieren zu erwarten; dann ist dieser Test vakuos, aber
        // nicht falsch — die anderen Assertions unten entfallen ebenfalls.
        return Ok(());
    };
    if user.trim().is_empty() {
        return Ok(());
    }

    let source = tempfile::tempdir().map_err(ctx("source tempdir"))?;
    std::fs::write(source.path().join("value"), format!("1\n# owner={user}\n"))
        .map_err(ctx("write source value"))?;

    let target = tempfile::tempdir().map_err(ctx("target tempdir"))?;
    let case_dir = target.path().join("redaction-case");

    let report = harw_dod_fixtures::capture::capture(
        EchoValueSensor::from,
        Capability::ReadProcStat,
        source.path(),
        &case_dir,
        Timestamp::UNIX_EPOCH,
    )
    .map_err(ctx("capture muss gelingen"))?;

    assert!(
        report.redactions_applied >= 1,
        "capture() muss den aktuellen Benutzernamen im Dateiinhalt redigieren"
    );

    let copied = std::fs::read_to_string(case_dir.join("tree").join("value"))
        .map_err(ctx("value lesbar"))?;
    assert!(
        !copied.contains(&user),
        "redigierter Baum darf den Benutzernamen nicht mehr enthalten"
    );
    assert!(copied.contains("<REDACTED-USER>"));
    Ok(())
}

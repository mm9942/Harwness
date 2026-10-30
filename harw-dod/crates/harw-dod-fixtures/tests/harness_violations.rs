//! Integrationstest: jede der sieben Prüfungen (plus die Adversarial-
//! Prüfung) schlägt fehl, wenn der Sensor gegen sie verstößt. Deckt seit
//! Knoten K48 auch die beiden aus der ehemaligen Scope-Dichtheit-Prüfung
//! hervorgegangenen Prüfungen ab: Bereichsdichtheit
//! (`assert_scope_containment`, unbedingt für jeden Sensor) und Verhalten
//! bei leerer Quelle in beiden Ausprägungen
//! (`assert_scope_tightness`/`assert_empty_scope_is_ok`, je nach erklärter
//! `EmptyScopeExpectation`).
//!
//! Ruft `harw_dod_fixtures::harness::assert_*` direkt über die öffentliche
//! API auf (nicht über `sensor_suite!`, dessen generierte `#[test]`-
//! Funktionen selbst fehlschlagen würden statt einen prüfbaren Wert
//! zurückzugeben) und prüft den erwarteten `Err(HarnessViolation)` direkt
//! (Bible R087/R089: Prüf-Helfer dürfen nicht paniken — jede `assert_*`-
//! Funktion gibt seit diesem Umbau ein `Result` zurück, kein
//! `std::panic::catch_unwind` mehr nötig). Einzige Ausnahme:
//! [`PanicsOnAnythingSensor::poll`] paniert absichtlich, um den internen
//! `catch_unwind`-Pfad von `assert_adversarial_survives` selbst zu prüfen —
//! dort bleibt die Panik. `tests/sensor_suite_smoke.rs` und
//! `tests/sensor_suite_smoke_empty_source_normal.rs` belegen den grünen
//! Gegenpart über das reale Makro, für jede der beiden
//! `EmptyScopeExpectation`-Ausprägungen.

mod common;

use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{TestResult, ctx};
use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_fixtures::FIXTURE_SENSOR_ID;
use harw_dod_signals::{EventKind, HostSample, SecurityEvent, Sensor, SensorReading};
use jiff::Timestamp;

/// Schreibt einen normalen Fixture-Fall (`tree/value` + `expect.json`) mit
/// beliebigem, für die jeweilige Prüfung irrelevantem Erwartungswert — die
/// Testfälle hier interessieren sich nur dafür, *dass* die Prüfung
/// fehlschlägt, nicht am exakten erwarteten Inhalt.
fn write_normal_case(fixtures_root: &Path, name: &str) -> TestResult {
    let case = fixtures_root.join(name);
    let tree = case.join("tree");
    std::fs::create_dir_all(&tree).map_err(ctx("tree/ anlegen"))?;
    std::fs::write(tree.join("value"), "1\n").map_err(ctx("value schreiben"))?;
    let expect_json = format!(
        r#"{{"now":"1970-01-01T00:00:00Z","reading":{{"samples":[{{"sensor":"{FIXTURE_SENSOR_ID}","observed_at":"1970-01-01T00:00:00Z","metric":"placeholder","value":1.0}}],"events":[]}}}}"#
    );
    std::fs::write(case.join("expect.json"), expect_json).map_err(ctx("expect.json schreiben"))?;
    Ok(())
}

fn write_malformed_case(fixtures_root: &Path, name: &str) -> TestResult {
    let case = fixtures_root.join("malformed").join(name);
    std::fs::create_dir_all(&case).map_err(ctx("malformed dir anlegen"))?;
    std::fs::write(case.join("value"), "irrelevant\n").map_err(ctx("malformed value schreiben"))?;
    Ok(())
}

fn write_adversarial_case(fixtures_root: &Path, name: &str) -> TestResult {
    let case = fixtures_root.join("adversarial").join(name);
    std::fs::create_dir_all(&case).map_err(ctx("adversarial dir anlegen"))?;
    std::fs::write(case.join("value"), "x").map_err(ctx("adversarial value schreiben"))?;
    Ok(())
}

// ---- Prüfung 1: Determinismus ----

#[derive(Debug)]
struct BadDeterminismSensor {
    handle: SensorHandle<Bound>,
    counter: AtomicI64,
}

impl From<SensorHandle<Bound>> for BadDeterminismSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self {
            handle,
            counter: AtomicI64::new(0),
        }
    }
}

impl Sensor for BadDeterminismSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        // Verstoss: aendert sich bei jedem Aufruf, unabhaengig von `now`.
        let value = self.counter.fetch_add(1, Ordering::SeqCst) as f64;
        Ok(SensorReading {
            samples: vec![HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Borrowed("bad_counter"),
                value,
            }],
            events: vec![],
        })
    }
}

#[test]
fn test_determinism_check_fails_for_nondeterministic_sensor() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_normal_case(root.path(), "typical")?;

    let result = harw_dod_fixtures::harness::assert_determinism(
        BadDeterminismSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "nicht-deterministischer Sensor muss die Determinismus-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Prüfung 2: Inhaltsfreiheit ----

#[derive(Debug)]
struct BadContentFreeSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for BadContentFreeSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for BadContentFreeSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let Some(root) = self.handle.scope().roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let raw = harw_dod_readfs::read_to_string(self.handle.scope(), &root.join("value"))
            .map_err(|_| SensorError::SourceUnavailable)?;
        // Verstoss: der volle Rohtext der Quelle landet unveraendert im
        // Ereignisfeld.
        Ok(SensorReading {
            samples: vec![],
            events: vec![SecurityEvent {
                sensor: self.handle.id().clone(),
                observed_at: now,
                actor: None,
                kind: EventKind::StructureDrift {
                    severity: harw_dod_signals::DriftSeverity::Unknown,
                    detail: raw,
                },
            }],
        })
    }
}

#[test]
fn test_content_freedom_check_fails_for_leaking_sensor() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_normal_case(root.path(), "typical")?;

    let result = harw_dod_fixtures::harness::assert_content_freedom(
        BadContentFreeSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "ein Sensor, der Rohtext der Quelle uebernimmt, muss die Inhaltsfreiheits-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Prüfung 3: Scope-Dichtheit ----

#[derive(Debug)]
struct AlwaysOkSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for AlwaysOkSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for AlwaysOkSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        // Verstoss: ignoriert einen leeren ReadScope und liefert trotzdem
        // ein Ergebnis.
        Ok(SensorReading {
            samples: vec![HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Borrowed("always_one"),
                value: 1.0,
            }],
            events: vec![],
        })
    }
}

#[test]
fn test_scope_tightness_check_fails_for_scope_ignoring_sensor() {
    let result = harw_dod_fixtures::harness::assert_scope_tightness(
        AlwaysOkSensor::from,
        Capability::ReadProcStat,
    );
    assert!(
        result.is_err(),
        "ein Sensor, der bei leerem ReadScope Ok(...) liefert, muss die Scope-Dichtheit-Pruefung zum Scheitern bringen"
    );
}

// ---- Prüfung 4: Redaktion ----

#[derive(Debug)]
struct BadRedactionSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for BadRedactionSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for BadRedactionSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let root = self
            .handle
            .scope()
            .roots()
            .next()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        // Verstoss: der rohe, absolute Scope-Pfad landet unveraendert in
        // einem Pfadfeld.
        Ok(SensorReading {
            samples: vec![],
            events: vec![SecurityEvent {
                sensor: self.handle.id().clone(),
                observed_at: now,
                actor: None,
                kind: EventKind::FileWrite { path: root },
            }],
        })
    }
}

#[test]
fn test_redaction_check_fails_for_path_leaking_sensor() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_normal_case(root.path(), "typical")?;

    let result = harw_dod_fixtures::harness::assert_redaction(
        BadRedactionSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "ein Sensor, der den rohen Scope-Pfad emittiert, muss die Redaktions-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Prüfung 5: Kardinalität ----

#[derive(Debug)]
struct BadCardinalitySensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for BadCardinalitySensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for BadCardinalitySensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        // Verstoss: fuenfzig zur Laufzeit gebaute, verschiedene Metriknamen
        // statt eines festen, statischen Namens.
        let samples = (0..50)
            .map(|i| HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Owned(format!("device_{i}_metric")),
                value: f64::from(i),
            })
            .collect();
        Ok(SensorReading {
            samples,
            events: vec![],
        })
    }
}

#[test]
fn test_cardinality_check_fails_for_high_cardinality_sensor() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_normal_case(root.path(), "typical")?;

    let result = harw_dod_fixtures::harness::assert_cardinality(
        BadCardinalitySensor::from,
        Capability::ReadProcStat,
        root.path(),
        5,
    );
    assert!(
        result.is_err(),
        "ein Sensor mit fuenfzig Labelkombinationen muss die Kardinalitaets-Pruefung bei max_cardinality=5 zum Scheitern bringen"
    );
    Ok(())
}

// ---- Prüfung 6: Fehlerfall ----

#[derive(Debug)]
struct BadErrorMappingSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for BadErrorMappingSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for BadErrorMappingSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        // Verstoss: meldet nie MalformedSource, unabhaengig vom Inhalt.
        Ok(SensorReading::default())
    }
}

#[test]
fn test_error_case_check_fails_for_sensor_that_never_reports_malformed() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_malformed_case(root.path(), "any")?;

    let result = harw_dod_fixtures::harness::assert_error_case(
        BadErrorMappingSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "ein Sensor, der nie MalformedSource meldet, muss die Fehlerfall-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Zusaetzliche Pruefung: Adversarial ----

#[derive(Debug)]
struct PanicsOnAnythingSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for PanicsOnAnythingSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for PanicsOnAnythingSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        // Absichtliche Panik (bleibt bestehen): prueft den internen
        // std::panic::catch_unwind-Pfad von assert_adversarial_survives,
        // die diese Panik abfaengt und als Err(HarnessViolation) meldet.
        panic!("dieser Sensor paniert immer");
    }
}

#[test]
fn test_adversarial_check_fails_for_panicking_sensor() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_adversarial_case(root.path(), "weird")?;

    // Kein aeusseres catch_unwind mehr noetig: assert_adversarial_survives
    // faengt die Sensor-Panik bereits intern ab und liefert ein Err.
    let result = harw_dod_fixtures::harness::assert_adversarial_survives(
        PanicsOnAnythingSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "ein Sensor, der auf einem adversarial-Baum paniert, muss die Adversarial-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Pruefung 3a: Bereichsdichtheit (Containment) — K48 ----

#[derive(Debug)]
struct LeaksParentDirSensor {
    handle: SensorHandle<Bound>,
}

impl From<SensorHandle<Bound>> for LeaksParentDirSensor {
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for LeaksParentDirSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let Some(root) = self.handle.scope().roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let Some(parent) = root.parent() else {
            return Err(SensorError::SourceUnavailable);
        };
        // Verstoss: liest direkt ueber std::fs, am ReadScope vorbei, aus dem
        // uebergeordneten Verzeichnis der gegebenen Bereichswurzel.
        match std::fs::read_to_string(parent.join("value")) {
            Ok(content) => {
                let value: f64 = content.trim().parse().unwrap_or_default();
                Ok(SensorReading {
                    samples: vec![HostSample {
                        sensor: self.handle.id().clone(),
                        observed_at: now,
                        metric: Cow::Borrowed("leaked_value"),
                        value,
                    }],
                    events: vec![],
                })
            }
            Err(_) => Err(SensorError::SourceUnavailable),
        }
    }
}

#[test]
fn test_scope_containment_check_fails_for_sensor_reading_above_its_scope_root() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    write_normal_case(root.path(), "typical")?;

    let result = harw_dod_fixtures::harness::assert_scope_containment(
        LeaksParentDirSensor::from,
        Capability::ReadProcStat,
        root.path(),
    );
    assert!(
        result.is_err(),
        "ein Sensor, der Dateien oberhalb seiner Bereichswurzel liest, muss die Bereichsdichtheit-Pruefung zum Scheitern bringen"
    );
    Ok(())
}

// ---- Pruefung 3b, Ausprägung EmptySourceIsNormal — K48 ----

#[test]
fn test_empty_scope_is_ok_check_fails_for_sensor_that_reports_an_error() {
    // Keiner der oben definierten Sensoren meldet bei leerem ReadScope
    // einen Fehler (AlwaysOkSensor liefert immer Ok mit Daten) — daher hier
    // lokal ein minimaler Sensor, der genau das tut:
    #[derive(Debug)]
    struct ReportsErrorOnEmptyScopeSensor {
        handle: SensorHandle<Bound>,
    }
    impl From<SensorHandle<Bound>> for ReportsErrorOnEmptyScopeSensor {
        fn from(handle: SensorHandle<Bound>) -> Self {
            Self { handle }
        }
    }
    impl Sensor for ReportsErrorOnEmptyScopeSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }
        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            // Verstoss (gegen EmptySourceIsNormal): meldet immer einen
            // Fehler, auch wenn der Sensor "leere Quelle ist normal"
            // erklaeren wuerde.
            Err(SensorError::SourceUnavailable)
        }
    }

    let result = harw_dod_fixtures::harness::assert_empty_scope_is_ok(
        ReportsErrorOnEmptyScopeSensor::from,
        Capability::ReadProcStat,
    );
    assert!(
        result.is_err(),
        "ein Sensor, der 'leere Quelle ist normal' erklaert, aber bei leerem ReadScope einen Fehler meldet, muss die Pruefung zum Scheitern bringen"
    );
}

#[test]
fn test_empty_scope_is_ok_check_fails_for_sensor_that_ignores_scope_and_invents_values() {
    // AlwaysOkSensor (oben, fuer die Bereichsdichtheit-Pruefung 3a definiert)
    // ignoriert den ReadScope vollstaendig und liefert immer einen
    // erfundenen Wert — genau die Verstossklasse, die assert_empty_scope_is_ok
    // fuer die Ausprägung "leere Quelle ist normal" fangen muss.
    let result = harw_dod_fixtures::harness::assert_empty_scope_is_ok(
        AlwaysOkSensor::from,
        Capability::ReadProcStat,
    );
    assert!(
        result.is_err(),
        "ein Sensor, der bei leerem ReadScope erfundene Werte liefert, muss die Pruefung 'leere Quelle ist normal' zum Scheitern bringen"
    );
}

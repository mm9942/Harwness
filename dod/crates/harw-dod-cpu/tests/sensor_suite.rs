//! Integrationstest: `sensor_suite!` über die echten `fixtures/` dieser Crate.
//!
//! # Zweck
//! Verdrahtet [`harw_dod_cpu::CpuSensor`] gegen
//! `harw_dod_fixtures::sensor_suite!` und bindet damit die sechs
//! Pflichtprüfungen sowie die optionale Adversarial-Prüfung der
//! Fixture-Harness (Knoten AW2-05) ein — Determinismus, Inhaltsfreiheit,
//! Scope-Dichtheit, Redaktion, Kardinalität und Fehlerfall, alle gegen die
//! drei normalen Fälle (`single-core`, `multi-core`, `extra-columns`) und
//! die drei `malformed/`-Fälle unter `fixtures/`.
//!
//! Die zusätzlichen, sensorspezifischen Prüfungen aus dem Arbeitsauftrag
//! (Spaltenzuordnung aus der Zeilenmitte, Robustheit gegen zusätzliche
//! Spalten, kein Zeileninhalt in der Fehlermeldung) leben als
//! `#[cfg(test)]`-Unit-Tests direkt bei der privaten Funktion
//! `parse_cpu_line` in `src/sensor.rs`, weil sie deren Logik isoliert und
//! ohne Dateisystem-Umweg prüfen; diese Datei bindet nur die Harness ein.

use harw_dod_cap::Capability;
use harw_dod_cpu::sensor::MAX_CARDINALITY;
use harw_dod_cpu::CpuSensor;

harw_dod_fixtures::sensor_suite! {
    sensor: CpuSensor,
    capability: Capability::ReadProcStat,
    max_cardinality: MAX_CARDINALITY,
    fixtures: "fixtures",
}

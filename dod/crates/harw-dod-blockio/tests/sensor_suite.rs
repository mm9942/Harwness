//! Integrationstest: `sensor_suite!` über die echten `fixtures/` dieser Crate.
//!
//! # Zweck
//! Verdrahtet [`harw_dod_blockio::BlockioSensor`] gegen
//! `harw_dod_fixtures::sensor_suite!` und bindet damit die sechs
//! Pflichtprüfungen der Fixture-Harness (Knoten AW2-05) ein — Determinismus,
//! Inhaltsfreiheit, Scope-Dichtheit, Redaktion, Kardinalität und Fehlerfall —
//! gegen die drei normalen Fälle (`single-device`, `multi-device`,
//! `seventeen-fields`) und die drei `malformed/`-Fälle unter `fixtures/`.
//!
//! Die zusätzlichen, sensorspezifischen Prüfungen aus dem Arbeitsauftrag
//! (Spaltenzuordnung aus der Zeilenmitte, Robustheit gegen siebzehn Felder,
//! Rauschgeräte-Filterung, kein Zeileninhalt in der Fehlermeldung,
//! Determinismus) leben als `#[cfg(test)]`-Unit-Tests direkt bei der
//! privaten Logik in `src/sensor.rs`, weil sie diese isoliert und teils ohne
//! Dateisystem-Umweg prüfen; diese Datei bindet nur die Harness ein.

use harw_dod_blockio::BlockioSensor;
use harw_dod_blockio::sensor::MAX_CARDINALITY;
use harw_dod_cap::Capability;

harw_dod_fixtures::sensor_suite! {
    sensor: BlockioSensor,
    capability: Capability::ReadSysfsBlock,
    max_cardinality: MAX_CARDINALITY,
    fixtures: "fixtures",
}

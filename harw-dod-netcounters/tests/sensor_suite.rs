//! Integrationstest: `sensor_suite!` über die echten `fixtures/` dieser Crate.
//!
//! # Zweck
//! Verdrahtet [`harw_dod_netcounters::NetCountersSensor`] gegen
//! `harw_dod_fixtures::sensor_suite!` und bindet damit die sechs
//! Pflichtprüfungen der Fixture-Harness (Knoten AW2-05) ein — Determinismus,
//! Inhaltsfreiheit, Scope-Dichtheit, Redaktion, Kardinalität und Fehlerfall —
//! gegen die vier normalen Fälle (`single-interface`, `multi-interface`,
//! `extra-columns`, `many-interfaces`) und die drei `malformed/`-Fälle unter
//! `fixtures/`.
//!
//! Die zusätzlichen, sensorspezifischen Prüfungen aus dem Arbeitsauftrag
//! (Spaltenzuordnung aus der Zeilenmitte, Robustheit gegen zusätzliche
//! Spalten, Doppelpunkt-Trennung, Kardinalitätsdeckelung, Inhaltsfreiheit
//! von Adressen/Ports) leben als `#[cfg(test)]`-Unit-Tests direkt bei den
//! privaten Funktionen in `src/sensor.rs`, weil sie deren Logik isoliert und
//! größtenteils ohne Dateisystem-Umweg prüfen; diese Datei bindet nur die
//! Harness ein.

use harw_dod_cap::Capability;
use harw_dod_netcounters::sensor::MAX_CARDINALITY;
use harw_dod_netcounters::NetCountersSensor;

harw_dod_fixtures::sensor_suite! {
    sensor: NetCountersSensor,
    capability: Capability::ReadProcNetDev,
    max_cardinality: MAX_CARDINALITY,
    fixtures: "fixtures",
}

//! Integrationstest: `sensor_suite!` über die echten `fixtures/` dieser Crate.
//!
//! # Zweck
//! Verdrahtet [`harw_dod_memory::MemorySensor`] gegen
//! `harw_dod_fixtures::sensor_suite!` und bindet damit die sechs
//! Pflichtprüfungen der Fixture-Harness (Knoten AW2-05) ein — Determinismus,
//! Inhaltsfreiheit, Scope-Dichtheit, Redaktion, Kardinalität und Fehlerfall,
//! alle gegen die drei normalen Fälle (`typical-host`, `no-swap`,
//! `extra-unknown-lines`) und die drei `malformed/`-Fälle unter
//! `fixtures/`.
//!
//! Die zusätzlichen, sensorspezifischen Prüfungen aus dem Arbeitsauftrag
//! (Zuordnung eines Werts aus der Dateimitte, Robustheit gegen unbekannte
//! Zeilen, korrekte Behandlung einer Zeile ohne ` kB`, die konkrete
//! `kB`-→-Bytes-Umrechnung, inhaltsfreie `MalformedSource`-Meldung bei
//! nicht-numerischem Wert, Determinismus bei gleichem `now`) leben als
//! `#[cfg(test)]`-Unit-Tests direkt bei den privaten Funktionen
//! `select_known_fields` und `parse_meminfo_value` in `src/sensor.rs`, weil
//! sie deren Logik isoliert und ohne Dateisystem-Umweg prüfen; diese Datei
//! bindet nur die Harness ein.

use harw_dod_cap::Capability;
use harw_dod_memory::sensor::MAX_CARDINALITY;
use harw_dod_memory::MemorySensor;

harw_dod_fixtures::sensor_suite! {
    sensor: MemorySensor,
    capability: Capability::ReadProcMeminfo,
    max_cardinality: MAX_CARDINALITY,
    fixtures: "fixtures",
}

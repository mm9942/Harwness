//! JSONL-Sink mit Rotation und Prüfsumme.
//!
//! # Verantwortungsbereich
//! Der Standard-Sink des Sentinels und zugleich der erste echte
//! Implementierer von [`harw_observe::TelemetrySink`] — ein Trait ohne
//! echten Implementierer ist eine Vermutung, keine Zusage. Besitzt
//! [`FileSink`] und [`ObserveFileError`]; kennt kein anderes Backend.
//! Vertrag: `docs/aw-contract-master.md`, Abschnitt A.6.
//!
//! # Exportierte Typen
//! [`FileSink`], [`ObserveFileError`], [`ObserveFileResult`].
//!
//! # Nebenläufigkeit
//! [`FileSink`] ist `Send + Sync + Debug`; alle veränderlichen Felder
//! liegen hinter einem `std::sync::Mutex`, weil
//! [`harw_observe::TelemetrySink::record`] nur `&self` bekommt (Vertrag
//! A.3).
//!
//! # Fehler
//! [`ObserveFileError`] — Verzeichnis-/Dateizugriff beim Öffnen und beim
//! Rotieren. `record()`/`flush()` selbst geben `()` zurück; interne
//! Fehler werden gezählt ([`FileSink::write_error_count`]).
//!
//! # Examples
//! ```rust,no_run
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_file::FileSink;
//! use std::path::Path;
//!
//! let sink = FileSink::open(Path::new("/tmp/harw-telemetry-lib"), 1_048_576).unwrap();
//! const KEY: MetricKey = MetricKey {
//!     name: "example",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//! sink.record(&KEY, MetricValue::Count(1), &[]);
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entstand in
//! Knoten **AW0-01**; Ebene **L2** im Zielgraphen.

mod error;
mod sink;

pub use error::{ObserveFileError, ObserveFileResult};
pub use sink::FileSink;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

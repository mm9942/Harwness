//! Feldvertrag, Metrikschlüssel und Sink-Trait der Telemetrie.
//!
//! # Verantwortungsbereich
//! Besitzt [`FieldName`], [`FieldValue`], [`MetricKey`], [`MetricValue`],
//! [`TelemetrySink`], [`NullSink`], [`TraceContext`], [`NullCounter`],
//! [`NullCounterRegistry`] und [`ObserveError`]. Kennt kein Backend: jeder
//! Sink lebt in einer eigenen Crate (siehe `harw-observe-file`, der erste
//! echte Implementierer von [`TelemetrySink`]). Vertrag:
//! `docs/aw-contract-master.md`, Abschnitt A.
//!
//! # Exportierte Typen
//! [`FieldName`], [`FieldValue`], [`MetricKind`], [`Unit`],
//! [`Cardinality`], [`MetricKey`], [`MetricValue`], [`TelemetrySink`],
//! [`NullSink`], [`TraceContext`], [`Redacted`], [`Redact`],
//! [`NullCounter`], [`NullCounterRegistry`], [`ObserveError`],
//! [`ObserveResult`], [`RoutingSink`], [`ExportApproval`],
//! [`PROTECTED_NAMESPACES`], [`SECURITY_METRIC_LEAKED`]. Dazu das Makro
//! [`field!`] und die Funktion [`assert_all_zero`].
//!
//! # Nullzähler (Knoten AW1-07)
//! Neben Zählern für Arbeit trägt diese Crate die Mechanik für
//! **Nullzähler**: Zähler, deren erwarteter Wert im Betrieb null ist, weil
//! sie nicht Arbeit zählen, sondern Verstöße gegen eine Invariante
//! (`trust_block_violation`, `steward_ceiling_violation`, …). Eine
//! Invariante, die nur im Code steht, ist eine Behauptung; ein Nullzähler
//! macht sie beobachtbar — steigt er über null, ist die Invariante
//! verletzt, sichtbar im Betrieb statt erst in einem Post-Mortem. Siehe
//! [`null_counter`] für die Mechanik, die Begründung der
//! `Ordering::Relaxed`-Wahl und die Falle, die sie nicht auflösen kann: ein
//! nie erreichter Zähler sieht wie eine eingehaltene Invariante aus. Diese
//! Crate baut nur die Mechanik — die konkreten Zähler entstehen bei den
//! Knoten, die ihre jeweilige Invariante einführen (AW4-01, AW6-08,
//! AW7-05).
//!
//! # Routender Sink (Knoten AW5-06)
//! [`RoutingSink`] verteilt Messwerte nach ihrem Namensraum auf
//! verschiedene Ziel-Sinks und schützt zwei Namensräume dabei besonders:
//! [`PROTECTED_NAMESPACES`] (`security.*`, `warden.*`) gehen ohne
//! ausdrückliche Operator-Bestätigung ([`ExportApproval`]) ausschließlich
//! an den `default_sink` (im Betrieb der File-Sink) — nie an ein
//! gewöhnlich geroutetes Ziel wie einen Prometheus-Endpunkt. Der
//! zugehörige Nullzähler [`SECURITY_METRIC_LEAKED`] macht eine
//! Fehlkonfiguration, die das zu umgehen versucht, beobachtbar statt sie
//! stillschweigend zuzulassen. Details, insbesondere warum diese Crate
//! dafür bewusst keine Abhängigkeit auf `harw-sandbox` zieht, stehen im
//! [`routing`]-Moduldoc.
//!
//! # Nebenläufigkeit
//! Alle Werttypen sind `Copy`/`Clone` ohne Interior Mutability.
//! [`TelemetrySink`] verlangt `Send + Sync + Debug`: Implementierungen
//! werden über `Arc<dyn TelemetrySink>` geteilt und aus jedem Thread
//! gleichzeitig aufgerufen, ohne dass der Aufrufer eine Sperre hält.
//!
//! # Fehler
//! [`ObserveError`] ist der einzige Fehlertyp dieser Crate — erzeugt durch
//! die validierenden [`TraceContext`]-Konstruktoren sowie durch
//! [`assert_all_zero`], wenn ein Nullzähler über null steht.
//!
//! # Examples
//! ```
//! use harw_observe::{
//!     Cardinality, MetricKey, MetricKind, MetricValue, NullSink, TelemetrySink, Unit,
//! };
//!
//! const KEY: MetricKey = MetricKey {
//!     name: "jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! let sink = NullSink;
//! sink.record(&KEY, MetricValue::Count(1), &[]);
//! sink.flush();
//! ```
//!
//! # Stand
//! Gerüst aus Knoten AW0-00 (Workspace-Fundament). Der Inhalt entstand in
//! Knoten **AW0-01**; Ebene **L1** im Zielgraphen. Die Nullzähler-Mechanik
//! ([`null_counter`]) kam in Knoten **AW1-07** hinzu, der routende Sink
//! ([`routing`]) in Knoten **AW5-06**.

pub mod error;
pub mod field;
pub mod metric;
pub mod null_counter;
pub mod redact;
pub mod routing;
pub mod sink;
pub mod trace;

pub use error::{ObserveError, ObserveResult};
pub use field::{FieldName, FieldValue};
pub use metric::{Cardinality, MetricKey, MetricKind, MetricValue, Unit};
pub use null_counter::{NullCounter, NullCounterRegistry, assert_all_zero};
pub use redact::{Redact, Redacted};
pub use routing::{ExportApproval, PROTECTED_NAMESPACES, RoutingSink, SECURITY_METRIC_LEAKED};
pub use sink::{NullSink, TelemetrySink};
pub use trace::TraceContext;

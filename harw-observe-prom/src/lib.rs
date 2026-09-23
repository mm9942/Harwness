//! Prometheus-Textformat (Version 0.0.4) aus `harw_observe::MetricKey`,
//! selbst erzeugt, plus ein minimaler Loopback-/Unix-Socket-Endpunkt, über
//! den es abgeholt wird.
//!
//! # Verantwortungsbereich
//! Der zweite echte Implementierer von [`harw_observe::TelemetrySink`] nach
//! `harw-observe-file::FileSink` (Vertrag A.3/A.6,
//! `docs/aw-contract-master.md`; Knoten AW3-04). Besitzt [`PromSink`]
//! (Zustandshaltung + Textrendering, siehe `crate::sink`-Moduldoc für die
//! Ableitung aus `MetricKey`), [`PromEndpoint`] + [`BindAddr`]
//! (Socket-Bindung + minimaler HTTP-Antwortpfad, siehe
//! `crate::endpoint`-Moduldoc) und [`PromError`] (Vertrag §H.1). Kennt kein
//! anderes Exportformat und keinen anderen Pfad als `/metrics`.
//!
//! # Warum kein Prometheus-Client-Crate
//! Das Prometheus-Textformat ist rund dreißig Zeilen Code: eine
//! `# TYPE`-Zeile je Metrikname plus eine Beispielzeile je Label-Belegung
//! (siehe `crate::sink`-Moduldoc, `crate::format`-Moduldoc). Eine
//! Client-Bibliothek (z. B. `prometheus`) brächte einen eigenen
//! Registry-Begriff mit — eigene Zähler-/Gauge-/Histogram-Typen, eine
//! eigene Registrierung, eigene Label-Typen —, der neben
//! [`harw_observe::MetricKey`] stünde: **zwei** Wahrheiten über dieselbe
//! Sache (welche Metriken es gibt, welche Labels sie tragen), die
//! auseinanderdriften können, sobald eine davon geändert wird, ohne dass
//! die andere es merkt. `harw-observe-prom` liest ausschließlich aus
//! `MetricKey` (dem einen Vertrag) und rendert direkt in Text — keine
//! zweite Registrierung.
//!
//! # Warum nur Loopback
//! [`BindAddr`] hat keine Variante, die eine externe Adresse ausdrücken
//! kann — nur `Loopback(u16)` (immer `127.0.0.1:<port>`) und
//! `UnixSocket(PathBuf)` (per Bauart nicht netzwerkerreichbar). Das ist ein
//! Typ-Zwang, keine Laufzeitprüfung mit Ausweg: es gibt keinen Aufruf von
//! [`PromEndpoint::bind`], der auf `0.0.0.0` oder eine andere externe
//! Schnittstelle binden könnte, weil kein Wert von `BindAddr` das
//! ausdrückt. Ein Metrikendpunkt auf einer externen Adresse würde einem
//! Netzscanner unmoderiert Metriknamen, Label-Werte und Zählstände
//! verraten — die Innenstruktur des Systems; die Voreinstellung entscheidet
//! hier, was in der Praxis passiert. Details (inklusive der
//! Tiefenverteidigung [`PromError::NonLoopbackBind`]) stehen im
//! `crate::endpoint`-Moduldoc.
//!
//! # `MetricKey` ist ab diesem Knoten eingefroren
//! Der Golden-Test dieser Crate
//! (`tests/fixtures/golden_metrics.txt`, geprüft in
//! `crate::sink`-Testmodul) hält die Textausgabe für eine feste Menge an
//! `MetricKey`-Werten fest. **Jede künftige Änderung an `MetricKey`**
//! (neues Feld, geändertes Feld, geänderte Bedeutung eines Feldes) ist ab
//! jetzt keine lokale Änderung an `harw-observe` mehr — sie ist eine
//! Migration über jede Golden-Fixtur, die je auf `MetricKey`s heutige Form
//! aufbaut, hier eingeschlossen. Ein Knoten, der `MetricKey` anfasst
//! (z. B. um ein Beschreibungsfeld für `# HELP`-Zeilen zu ergänzen — siehe
//! `crate::sink`-Moduldoc, Abschnitt „Fehlendes Feld"), muss diese Fixtur
//! mit aktualisieren.
//!
//! # Exportierte Typen
//! [`PromSink`], [`PromEndpoint`], [`BindAddr`], [`PromError`],
//! [`PromResult`].
//!
//! # Nebenläufigkeit
//! [`PromSink`] ist `Send + Sync + Debug`: sein gesamter veränderliche
//! Zustand liegt hinter einem `std::sync::Mutex`, weil
//! `TelemetrySink::record` nur `&self` bekommt (Vertrag A.3).
//! [`PromEndpoint::bind`] spawnt genau einen `std::thread` pro Socket, ohne
//! `tokio` oder eine andere async-Runtime (Begründung im
//! `crate::endpoint`-Moduldoc); `Drop` stoppt und joint diesen Thread.
//!
//! # Fehler
//! [`PromError`] — ausschließlich aus [`PromEndpoint::bind`]. `PromSink`s
//! `record`/`flush` geben `()` zurück (Vertrag A.3); ein
//! `MetricKind::Histogram`-Schlüssel wird intern gezählt statt propagiert
//! (siehe `crate::sink`-Moduldoc, Abschnitt „Histogram-Entscheidung").
//!
//! # Examples
//! ```rust,no_run
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_prom::{BindAddr, PromEndpoint};
//!
//! const JOBS_COMPLETED: MetricKey = MetricKey {
//!     name: "harw_jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! let endpoint = PromEndpoint::bind(BindAddr::Loopback(9327))?;
//! endpoint.sink().record(&JOBS_COMPLETED, MetricValue::Count(1), &[]);
//! // `GET http://127.0.0.1:9327/metrics` liefert jetzt das Textformat.
//! # Ok::<(), harw_observe_prom::PromError>(())
//! ```
//!
//! # Stand
//! Inhalt entstand in Knoten **AW3-04** (Prometheus-Sink mit Golden-Test);
//! Ebene **L2** im Zielgraphen.

mod endpoint;
mod error;
mod format;
mod sink;

pub use endpoint::{BindAddr, PromEndpoint};
pub use error::{PromError, PromResult};
pub use sink::PromSink;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

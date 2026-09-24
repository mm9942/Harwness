//! `PromSink`: sammelt Messwerte im Speicher und rendert sie als
//! Prometheus-Textformat (Version 0.0.4).
//!
//! # Verantwortungsbereich
//! Trägt [`PromSink`], den zweiten echten Implementierer von
//! [`harw_observe::TelemetrySink`] nach `harw-observe-file::FileSink`
//! (Vertrag A.3/A.6, `docs/aw-contract-master.md`). Hält für jede
//! Kombination aus Metrikname und Label-Belegung den zuletzt gemeldeten
//! Wert im Speicher und rendert sie auf Abruf ([`PromSink::render`]) als
//! flaches Textdokument. Kennt kein Netzwerk — das übernimmt
//! [`crate::PromEndpoint`].
//!
//! # Ableitung aus `MetricKey`
//! Diese Crate erzeugt das Textformat **selbst**, ohne Prometheus-Client-
//! Bibliothek: das Format ist rund dreißig Zeilen Code (Escaping in
//! [`crate::format`] plus Zeilenbau hier), während eine Client-Bibliothek
//! einen eigenen Registry-Begriff neben [`harw_observe::MetricKey`]
//! mitbrächte — eine zweite Wahrheit über dieselbe Sache. Aus `MetricKey`
//! fließen `name` (wörtlich) und `kind` (auf `counter`/`gauge`/`histogram`)
//! in die Ausgabe; `unit` und `cardinality` fließen **nicht** ein:
//! `unit` steckt bereits konventionell im Namensbestandteil (von
//! `harw_macros::metrics!` zur Compile-Zeit erzwungen, siehe unten), ein
//! zweites Feld dafür wäre redundant; `cardinality` ist eine Obergrenze für
//! den Emittenten/die Registrierung, keine Ausgabeeigenschaft — dieser Sink
//! zählt Label-Kombinationen nicht gegen sie.
//!
//! **Fehlendes Feld:** `MetricKey` trägt keinen Beschreibungstext. Das
//! Prometheus-Textformat kennt eine optionale `# HELP <name> <text>`-Zeile
//! (siehe Beispiel im AW3-04-Auftrag); der einzige Ort, an dem ein solcher
//! Text heute existiert, ist der `///`-Doc-Kommentar vor einem
//! `metrics!`-Eintrag in `harw-macros` — der wird auf die erzeugte `pub
//! const` übertragen, aber nicht in den *Wert* von `MetricKey` eingebettet
//! und ist zur Laufzeit deshalb nicht mehr vorhanden. Dieser Sink erfindet
//! keinen Ersatztext und lässt die `# HELP`-Zeile deshalb bewusst weg
//! (nach der Prometheus-Spezifikation ist sie optional); nur `# TYPE`
//! erscheint. Ein späterer Knoten, der `# HELP` will, bräuchte ein
//! zusätzliches `&'static str`-Feld auf `MetricKey` — und **jede**
//! Feldänderung an `MetricKey` ist nach diesem Knoten eine
//! Golden-Test-Migration über alle bis dahin emittierten Metriken (siehe
//! unten).
//!
//! # `MetricKey` ist ab hier eingefroren
//! Der Golden-Test dieses Moduls (`test_render_matches_golden_fixture_*`,
//! Fixtur `tests/fixtures/golden_metrics.txt`) hält die Ausgabeform fest.
//! Jede künftige Änderung an `MetricKey` (neues Feld, geändertes Feld) ist
//! ab jetzt keine lokale Änderung mehr, sondern eine Migration über jede
//! Golden-Fixtur, die je auf `MetricKey`s heutige Form aufbaut.
//!
//! # Histogram-Entscheidung
//! `record()` lehnt einen `MetricKind::Histogram`-Schlüssel ab, statt einen
//! `_bucket`/`_sum`/`_count`-Dreiklang zu erfinden: ein korrektes Histogram
//! im Prometheus-Textformat braucht Bucket-Grenzen (`le="..."`), die
//! `MetricKey` nicht trägt und die dieser Knoten nicht hinzufügt (siehe
//! oben, „`MetricKey` ist ab hier eingefroren"). Ohne echte Grenzen wäre
//! jede Bucket-Struktur erfunden — eine irreführende Verteilung ist
//! schlimmer als eine fehlende Metrik. Der Aufruf wird gezählt
//! ([`PromSink::unsupported_histogram_count`]) statt zu paniken oder den
//! Aufrufer scheitern zu lassen (Vertrag A.3, zweite Festlegung); ein
//! künftiger Knoten mit echtem Histogram-Support fügt `MetricKey` das
//! nötige Feld hinzu und migriert alle Golden-Fixturen in einem Zug.
//!
//! # Nebenläufigkeit
//! [`PromSink`] ist `Send + Sync + Debug` (Vertrag A.3). Der gesamte
//! veränderliche Zustand liegt hinter einem `std::sync::Mutex`
//! (`TelemetrySink::record` bekommt nur `&self`), nach demselben Muster wie
//! `harw-observe-file::FileSink`; die Ablehnungszählung für Histogramme
//! liegt in einem separaten `AtomicU64`.
//!
//! # Fehler
//! Keine — `record()`/`flush()` geben `()` zurück (Vertrag A.3); interne
//! Ablehnungen (Histogramm) werden gezählt statt propagiert.
//!
//! # Examples
//! ```
//! use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
//! use harw_observe_prom::PromSink;
//!
//! const KEY: MetricKey = MetricKey {
//!     name: "harw_jobs_completed_total",
//!     kind: MetricKind::Counter,
//!     unit: Unit::Count,
//!     labels: &[],
//!     cardinality: Cardinality::Single,
//! };
//!
//! let sink = PromSink::new();
//! sink.record(&KEY, MetricValue::Count(3), &[]);
//! assert_eq!(sink.render(), "# TYPE harw_jobs_completed_total counter\nharw_jobs_completed_total 3\n");
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use harw_observe::{FieldName, FieldValue, MetricKey, MetricKind, MetricValue, TelemetrySink};

use crate::format::{escape_label_value, format_prom_float, metric_kind_str};

/// Eine sortierte Label-Belegung: `(Labelname, Labelwert)`-Paare, aufsteigend
/// nach Labelname. `Vec<(String, String)>` vergleicht lexikographisch
/// Element für Element, was [`PromState`] direkt als deterministischen
/// `BTreeMap`-Schlüssel nutzt — keine gesonderte `Ord`-Implementierung
/// nötig.
type LabelSet = Vec<(String, String)>;

/// Eine Metrikreihe: die Art (für die `# TYPE`-Zeile) und der zuletzt
/// gemeldete Wert je Label-Belegung.
#[derive(Debug)]
struct MetricSeries {
    kind: MetricKind,
    values: BTreeMap<LabelSet, MetricValue>,
}

/// Der veränderliche Zustand von [`PromSink`], hinter einem `Mutex`, weil
/// `TelemetrySink::record` nur `&self` bekommt (Vertrag A.3).
#[derive(Debug, Default)]
struct PromState {
    /// Metrikname → Reihe. `BTreeMap<&'static str, _>` sortiert nach
    /// Byte-Reihenfolge des Namens, was [`PromSink::render`] die
    /// geforderte deterministische Metrik-Sortierung ohne zusätzlichen
    /// Sortierschritt liefert.
    series: BTreeMap<&'static str, MetricSeries>,
}

/// Ein [`harw_observe::TelemetrySink`], der Messwerte im Speicher hält und
/// als Prometheus-Textformat (Version 0.0.4) rendert.
///
/// Siehe Moduldoc für die Designentscheidungen (kein Client-Crate,
/// Histogram-Ablehnung, `MetricKey`-Einfrierung).
#[derive(Debug, Default)]
pub struct PromSink {
    state: Mutex<PromState>,
    unsupported_histograms: AtomicU64,
}

impl PromSink {
    /// Baut einen leeren Sink.
    ///
    /// # Returns
    /// Einen `PromSink` ohne bisher gemeldete Messwerte.
    ///
    /// # Examples
    /// ```
    /// use harw_observe_prom::PromSink;
    ///
    /// let sink = PromSink::new();
    /// assert_eq!(sink.render(), "");
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // Nimmt die interne Sperre; bei Vergiftung (ein anderer Thread ist unter
    // Halten der Sperre panisch geworden) wird der zuletzt bekannte Zustand
    // trotzdem übernommen (Muster: `harw-observe-file::FileSink::lock_state`).
    fn lock_state(&self) -> MutexGuard<'_, PromState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Rendert den aktuellen Zustand als Prometheus-Textformat (Version
    /// 0.0.4).
    ///
    /// # Description
    /// Eine `# TYPE <name> <kind>`-Zeile je Metrikname (siehe Moduldoc,
    /// Abschnitt „Fehlendes Feld" zur bewusst fehlenden `# HELP`-Zeile),
    /// gefolgt von je einer Beispielzeile pro Label-Belegung dieser
    /// Metrik. Metriken erscheinen in Namens-Byte-Reihenfolge, Reihen
    /// innerhalb einer Metrik in Label-Byte-Reihenfolge — zwei Aufrufe mit
    /// demselben Zustand liefern byteidentischen Text.
    ///
    /// # Returns
    /// Das vollständige Textdokument; `""`, wenn noch nichts gemeldet
    /// wurde.
    ///
    /// # Examples
    /// ```
    /// use harw_observe::{Cardinality, MetricKey, MetricKind, MetricValue, TelemetrySink, Unit};
    /// use harw_observe_prom::PromSink;
    ///
    /// const KEY: MetricKey = MetricKey {
    ///     name: "harw_queue_depth",
    ///     kind: MetricKind::Gauge,
    ///     unit: Unit::Count,
    ///     labels: &[],
    ///     cardinality: Cardinality::Single,
    /// };
    /// let sink = PromSink::new();
    /// sink.record(&KEY, MetricValue::Gauge(3.5), &[]);
    /// assert_eq!(sink.render(), "# TYPE harw_queue_depth gauge\nharw_queue_depth 3.5\n");
    /// ```
    #[must_use]
    pub fn render(&self) -> String {
        let state = self.lock_state();
        let mut out = String::new();
        for (name, series) in &state.series {
            out.push_str("# TYPE ");
            out.push_str(name);
            out.push(' ');
            out.push_str(metric_kind_str(series.kind));
            out.push('\n');
            for (labels, value) in &series.values {
                out.push_str(name);
                push_label_set(&mut out, labels);
                out.push(' ');
                out.push_str(&metric_value_str(value));
                out.push('\n');
            }
        }
        out
    }

    /// Anzahl der `record()`-Aufrufe für einen `MetricKind::Histogram`-
    /// Schlüssel, die dieser Sink abgelehnt hat.
    ///
    /// # Returns
    /// Die kumulierte Ablehnungszahl seit `new()` (siehe Moduldoc,
    /// Abschnitt „Histogram-Entscheidung").
    ///
    /// # Examples
    /// ```
    /// use harw_observe_prom::PromSink;
    ///
    /// let sink = PromSink::new();
    /// assert_eq!(sink.unsupported_histogram_count(), 0);
    /// ```
    #[must_use]
    pub fn unsupported_histogram_count(&self) -> u64 {
        self.unsupported_histograms.load(Ordering::Relaxed)
    }
}

// Schreibt `{k1="v1",k2="v2"}` an `out`; schreibt nichts, wenn `labels` leer
// ist (eine Metrik ohne Labels hat keine Klammern in der Ausgabe).
fn push_label_set(out: &mut String, labels: &LabelSet) {
    if labels.is_empty() {
        return;
    }
    out.push('{');
    for (index, (name, value)) in labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(&escape_label_value(value));
        out.push('"');
    }
    out.push('}');
}

// Textform des gemessenen Werts. `Observation` ist eigentlich für
// Histogramme gedacht (die `record()` schon vorher ablehnt); trifft dieser
// Zweig dennoch zu (ein Aufrufer meldet `Observation` unter einem
// Counter-/Gauge-Schlüssel), wird der Zahlenwert trotzdem unverändert
// ausgegeben — dieser Sink prüft `key.kind` gegen den `MetricValue`-
// Varianten nicht gegenseitig, das ist Sache des Emittenten.
fn metric_value_str(value: &MetricValue) -> String {
    match value {
        MetricValue::Count(v) => v.to_string(),
        MetricValue::Gauge(v) | MetricValue::Observation(v) => format_prom_float(*v),
    }
}

// Textform eines `FieldValue` für einen Labelwert (vor dem Escaping).
fn field_value_to_label_string(value: &FieldValue) -> String {
    match value {
        FieldValue::Str(s) => (*s).to_owned(),
        FieldValue::Owned(s) => s.to_owned(),
        FieldValue::I64(v) => v.to_string(),
        FieldValue::U64(v) => v.to_string(),
        FieldValue::F64(v) => format_prom_float(*v),
        FieldValue::Bool(v) => v.to_string(),
    }
}

impl TelemetrySink for PromSink {
    fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
        if key.kind == MetricKind::Histogram {
            self.unsupported_histograms.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let mut label_set: LabelSet = labels
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), field_value_to_label_string(value)))
            .collect();
        label_set.sort_by(|a, b| a.0.cmp(&b.0));

        let mut state = self.lock_state();
        let series = state
            .series
            .entry(key.name)
            .or_insert_with(|| MetricSeries {
                kind: key.kind,
                values: BTreeMap::new(),
            });
        series.kind = key.kind;
        series.values.insert(label_set, value);
    }

    fn flush(&self) {
        // Rein speicherresident: es gibt keinen Puffer, der geleert werden
        // müsste. `PromEndpoint` liest `render()` bei jedem Scrape frisch.
    }

    fn name(&self) -> &'static str {
        "prom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_observe::{Cardinality, Unit};

    const COUNTER_NO_LABELS: MetricKey = MetricKey {
        name: "harw_admitted_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const GAUGE_NO_LABELS: MetricKey = MetricKey {
        name: "harw_queue_depth",
        kind: MetricKind::Gauge,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const BYTES_READ: MetricKey = MetricKey {
        name: "harw_bytes_read_total",
        kind: MetricKind::Counter,
        unit: Unit::Bytes,
        labels: &[],
        cardinality: Cardinality::Bounded(4),
    };

    const CHILD_ADMITTED: MetricKey = MetricKey {
        name: "harw_child_admitted_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Bounded(64),
    };

    const JOBS_COMPLETED: MetricKey = MetricKey {
        name: "harw_jobs_completed_total",
        kind: MetricKind::Counter,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Bounded(8),
    };

    const QUEUE_DEPTH: MetricKey = MetricKey {
        name: "harw_queue_depth",
        kind: MetricKind::Gauge,
        unit: Unit::Count,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    const LATENCY_HISTOGRAM: MetricKey = MetricKey {
        name: "harw_latency_seconds",
        kind: MetricKind::Histogram,
        unit: Unit::Seconds,
        labels: &[],
        cardinality: Cardinality::Single,
    };

    #[test]
    fn test_render_empty_sink_is_empty_string() {
        let sink = PromSink::new();
        assert_eq!(sink.render(), "");
    }

    #[test]
    fn test_record_then_render_shows_recorded_counter_value() {
        let sink = PromSink::new();
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(42), &[]);
        assert_eq!(
            sink.render(),
            "# TYPE harw_admitted_total counter\nharw_admitted_total 42\n"
        );
    }

    #[test]
    fn test_record_then_render_shows_recorded_gauge_value() {
        let sink = PromSink::new();
        sink.record(&GAUGE_NO_LABELS, MetricValue::Gauge(3.5), &[]);
        assert_eq!(
            sink.render(),
            "# TYPE harw_queue_depth gauge\nharw_queue_depth 3.5\n"
        );
    }

    #[test]
    fn test_record_with_labels_renders_sorted_label_pairs() {
        let sink = PromSink::new();
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(42),
            &[
                (harw_observe::field!("role"), FieldValue::Str("explorer")),
                (harw_observe::field!("clan"), FieldValue::Str("research")),
            ],
        );
        // Labels erscheinen alphabetisch (`clan` vor `role`), unabhängig von
        // der Reihenfolge, in der sie an `record()` übergeben wurden.
        assert_eq!(
            sink.render(),
            "# TYPE harw_child_admitted_total counter\nharw_child_admitted_total{clan=\"research\",role=\"explorer\"} 42\n"
        );
    }

    #[test]
    fn test_record_overwrites_previous_value_for_same_label_combination() {
        let sink = PromSink::new();
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(2), &[]);
        assert_eq!(
            sink.render(),
            "# TYPE harw_admitted_total counter\nharw_admitted_total 2\n"
        );
    }

    #[test]
    fn test_render_is_deterministic_across_repeated_calls() {
        let sink = PromSink::new();
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(9),
            &[
                (harw_observe::field!("clan"), FieldValue::Str("research")),
                (harw_observe::field!("role"), FieldValue::Str("guard")),
            ],
        );
        let first = sink.render();
        let second = sink.render();
        assert_eq!(
            first, second,
            "render() must be byte-identical across calls"
        );
    }

    #[test]
    fn test_record_histogram_key_is_rejected_and_counted() {
        let sink = PromSink::new();
        assert_eq!(sink.unsupported_histogram_count(), 0);

        sink.record(&LATENCY_HISTOGRAM, MetricValue::Observation(1.23), &[]);

        assert_eq!(sink.unsupported_histogram_count(), 1);
        assert_eq!(
            sink.render(),
            "",
            "a rejected histogram must not appear in the output"
        );
    }

    #[test]
    fn test_record_escapes_label_values_containing_quotes_backslashes_and_newlines() {
        let sink = PromSink::new();
        let raw = "line1\nquote\"and\\slash";
        sink.record(
            &BYTES_READ,
            MetricValue::Count(1024),
            &[(harw_observe::field!("path"), FieldValue::Str(raw))],
        );

        let output = sink.render();
        // Every logical output line ends at exactly one '\n': a raw newline
        // byte inside the label value would otherwise split the sample line
        // and corrupt every line after it for a scraping parser.
        assert_eq!(output.matches('\n').count(), output.lines().count());
        assert!(
            output.contains(r#"path="line1\nquote\"and\\slash""#),
            "{output}"
        );
    }

    #[test]
    fn test_render_matches_golden_fixture_for_mixed_metrics() {
        let sink = PromSink::new();

        sink.record(
            &BYTES_READ,
            MetricValue::Count(1024),
            &[(
                harw_observe::field!("path"),
                FieldValue::Str("line1\nquote\"and\\slash"),
            )],
        );
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(42),
            &[
                (harw_observe::field!("clan"), FieldValue::Str("research")),
                (harw_observe::field!("role"), FieldValue::Str("explorer")),
            ],
        );
        sink.record(
            &CHILD_ADMITTED,
            MetricValue::Count(7),
            &[
                (harw_observe::field!("clan"), FieldValue::Str("research")),
                (harw_observe::field!("role"), FieldValue::Str("guard")),
            ],
        );
        sink.record(
            &JOBS_COMPLETED,
            MetricValue::Count(10),
            &[(harw_observe::field!("status"), FieldValue::Str("ok"))],
        );
        sink.record(
            &JOBS_COMPLETED,
            MetricValue::Count(2),
            &[(harw_observe::field!("status"), FieldValue::Str("error"))],
        );
        sink.record(&QUEUE_DEPTH, MetricValue::Gauge(3.5), &[]);
        // A histogram record must be rejected and must not appear anywhere
        // in the golden output.
        sink.record(&LATENCY_HISTOGRAM, MetricValue::Observation(1.23), &[]);

        let expected = include_str!("../tests/fixtures/golden_metrics.txt");
        assert_eq!(sink.render(), expected);
        assert_eq!(sink.unsupported_histogram_count(), 1);
    }

    #[test]
    fn test_prom_sink_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<PromSink>();
    }

    #[test]
    fn test_prom_sink_name_is_prom() {
        assert_eq!(PromSink::new().name(), "prom");
    }

    #[test]
    fn test_prom_sink_usable_through_arc_dyn_telemetry_sink() {
        use std::sync::Arc;
        let sink: Arc<dyn TelemetrySink> = Arc::new(PromSink::new());
        sink.record(&COUNTER_NO_LABELS, MetricValue::Count(1), &[]);
        sink.flush();
        assert_eq!(sink.name(), "prom");
    }
}

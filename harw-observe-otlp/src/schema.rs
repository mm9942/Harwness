//! OTLP/JSON-Rumpf, selbst erzeugt: `ExportMetricsServiceRequest` und alle
//! darunterliegenden Strukturen, plus die reinen Abbildungsfunktionen von
//! `harw_observe`-Werttypen auf sie.
//!
//! # Verantwortungsbereich
//! Enthält ausschließlich crateinterne (`pub(crate)`) Typen und
//! zustandslose Abbildungsfunktionen ohne I/O — [`crate::OtlpSink`]
//! übernimmt Pufferung, Stapelbildung und die zählenden Entscheidungen
//! (verwerfen bei nicht endlichem Wert, bei Zeitüberlauf, bei Pufferüberlauf).
//! Kein Typ dieses Moduls ist `pub`: die OTel-Adapter-Isolation (Crate-Doc)
//! verlangt, dass kein Typ irgendeiner Fremdcrate — und, als Konsequenz
//! dieser Crate selbst als „Mauer", auch keiner der hier selbst erzeugten
//! OTLP-Formtypen — in einer öffentlichen Signatur dieser Crate erscheint.
//!
//! # Warum OTLP/JSON statt der `opentelemetry-otlp`-Familie oder `prost`
//! Drei realistische Wege standen zur Wahl:
//! - **(a) `opentelemetry-otlp` + `opentelemetry_sdk`**: die volle Familie —
//!   große, versionsgleichgeschaltete Fläche, genau das, wovor die
//!   OTel-Adapter-Isolation warnt.
//! - **(b) `prost` + `prost-types`** mit generierten oder handgeschriebenen
//!   OTLP-Protobuf-Typen: mittlere Fläche, aber `prost-build` braucht
//!   `protoc` als externes Werkzeug im Build — ein `build.rs` mit
//!   Systemabhängigkeit, verboten unter D3.
//! - **(c) OTLP/JSON**, das proto3-JSON-Mapping desselben
//!   `ExportMetricsServiceRequest`: nur `serde`/`serde_json` (bereits
//!   Workspace-Abhängigkeiten), kein `build.rs`, kein `protoc`. Erfüllt D5
//!   restlos.
//!
//! Diese Crate wählt **(c)**. Der Preis: ein größerer Rumpf auf der Leitung
//! als binäres Protobuf, und manche Collector-Konfigurationen akzeptieren
//! ausschließlich `application/x-protobuf`. Beides ist eine bewusste
//! Abwägung, keine Unkenntnis.
//!
//! # Quelle des Schemas
//! Feldnamen und -formen dieses Moduls sind gegen die offizielle
//! `open-telemetry/opentelemetry-proto`-Definition geprüft (abgerufen
//! 2026-09-01):
//! - `opentelemetry/proto/metrics/v1/metrics.proto` (`Sum`, `Gauge`,
//!   `Histogram`, `NumberDataPoint`, `HistogramDataPoint`, `Metric`,
//!   `ScopeMetrics`, `ResourceMetrics`, `AggregationTemporality`) —
//!   <https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/metrics/v1/metrics.proto>
//! - `opentelemetry/proto/common/v1/common.proto` (`AnyValue`, `KeyValue`,
//!   `InstrumentationScope`) —
//!   <https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/common/v1/common.proto>
//! - Das Beispiel-Rumpf-JSON, das die camelCase-Feldnamen und die
//!   String-Kodierung von `fixed64`/`int64`-Feldern bestätigt —
//!   <https://github.com/open-telemetry/opentelemetry-proto/blob/main/examples/metrics.json>
//!
//! proto3s kanonische JSON-Abbildung kodiert 64-Bit-Ganzzahlfelder
//! (`fixed64`, `int64`, `uint64`, `sfixed64`) als **Dezimalzeichenketten**,
//! nicht als JSON-Zahlen (Genauigkeitsverlust in manchen JSON-Parsern sonst
//! möglich) — deshalb sind `time_unix_nano`, `start_time_unix_nano`,
//! `count`, `bucket_counts` und `as_int`/`int_value` in diesem Modul
//! `String`-Felder, obwohl sie fachlich Ganzzahlen sind.
//!
//! # `unit`-Zuordnung ist keine UCUM-Kodierung
//! OTLPs `Metric.unit`-Feld ist konventionell ein UCUM-Code (`"s"`, `"By"`,
//! `"1"`, …); diese Crate kennt die UCUM-Tabelle nicht zuverlässig genug, um
//! sie zu raten (vgl. Auftrag: „Rate nicht" für das Schema selbst — dieselbe
//! Vorsicht gilt für einen externen Codestandard, den diese Crate nicht
//! verifiziert hat). [`unit_str`] liefert deshalb lesbare Wörter
//! (`"count"`, `"bytes"`, `"seconds"`, …), identisch zu
//! `harw-observe-file::FileSink`s `unit_str`. Ein späterer Knoten mit
//! geprüften UCUM-Codes kann diese Abbildung austauschen, ohne die übrige
//! Struktur anzufassen.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine, unveränderliche Werttypen ohne Interior
//! Mutability; alle Funktionen sind zustandslos.
//!
//! # Fehler
//! [`timestamp_to_unix_nanos`] liefert
//! [`crate::OtlpError::TimestampOutOfRange`]. [`to_json_bytes`]
//! liefert [`crate::OtlpError::Json`].

use serde::Serialize;

use harw_observe::{FieldName, FieldValue, MetricValue, Unit};

use crate::error::OtlpError;

/// `AggregationTemporality::AGGREGATION_TEMPORALITY_DELTA` (Protobuf-Wert
/// `1`): jeder Datenpunkt beschreibt eine unabhängige Beobachtung, keine
/// über ein Fenster aufsummierte Reihe.
pub(crate) const AGGREGATION_TEMPORALITY_DELTA: i32 = 1;
/// `AggregationTemporality::AGGREGATION_TEMPORALITY_CUMULATIVE` (Protobuf-Wert
/// `2`): der Datenpunkt trägt den seit Prozessstart aufgelaufenen Stand.
pub(crate) const AGGREGATION_TEMPORALITY_CUMULATIVE: i32 = 2;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportMetricsServiceRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<Resource>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Resource {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) attributes: Vec<KeyValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScopeMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<InstrumentationScope>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) metrics: Vec<Metric>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstrumentationScope {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Metric {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sum: Option<Sum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gauge: Option<Gauge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) histogram: Option<Histogram>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sum {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) data_points: Vec<NumberDataPoint>,
    pub(crate) aggregation_temporality: i32,
    pub(crate) is_monotonic: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Gauge {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) data_points: Vec<NumberDataPoint>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Histogram {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) data_points: Vec<HistogramDataPoint>,
    pub(crate) aggregation_temporality: i32,
}

/// Ein Zahlen-Datenpunkt (für `Sum` und `Gauge`).
///
/// `start_time_unix_nano` ist `None` für `Gauge`-Datenpunkte (ein
/// Momentanwert kennt keinen sinnvollen Fensterbeginn) und stets `Some` für
/// `Sum`-Datenpunkte, dort gleich `time_unix_nano` gesetzt: dieser Sink
/// verfolgt keinen natürlichen Startzeitpunkt eines Zählers (`record()`
/// liefert je Aufruf nur den aktuellen Stand, keine Zählerhistorie), und
/// `start_time_unix_nano` gleich `time_unix_nano` erfüllt das Feld, ohne
/// einen unbekannten früheren Zeitpunkt zu erfinden.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NumberDataPoint {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_time_unix_nano: Option<String>,
    pub(crate) time_unix_nano: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) as_double: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) as_int: Option<String>,
}

/// Ein Histogramm-Datenpunkt für eine einzelne Beobachtung
/// (`harw_observe::MetricValue::Observation`).
///
/// `bucket_counts`/`explicit_bounds` bleiben leer: `MetricKey` trägt keine
/// Bucket-Grenzen (dieselbe Lücke, aus der `harw-observe-prom::PromSink`
/// `MetricKind::Histogram` ganz ablehnt — siehe dessen Moduldoc, Abschnitt
/// „Histogram-Entscheidung"). Diese Crate lehnt nicht ab, sondern bildet
/// jede einzelne Beobachtung als einen eigenen Datenpunkt mit `count: "1"`
/// und `sum`/`min`/`max` gleich dem beobachteten Wert ab — korrekt im Sinne
/// des Schemas (kein erfundener Bucket), aber ohne echte
/// Verteilungsinformation; ein Collector sieht viele
/// Ein-Beobachtung-Histogramme statt einer aggregierten Verteilung.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistogramDataPoint {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) attributes: Vec<KeyValue>,
    pub(crate) start_time_unix_nano: String,
    pub(crate) time_unix_nano: String,
    pub(crate) count: String,
    pub(crate) sum: f64,
    pub(crate) min: f64,
    pub(crate) max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct KeyValue {
    pub(crate) key: String,
    pub(crate) value: AnyValue,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnyValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) string_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bool_value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) int_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) double_value: Option<f64>,
}

/// Rechnet einen `jiff::Timestamp` in OTLPs vorzeichenlose
/// 64-Bit-Nanosekunden seit der Epoche um (`fixed64`, siehe Moduldoc,
/// Abschnitt „Quelle des Schemas").
///
/// # Description
/// Nutzt `jiff::Timestamp::as_nanosecond`, das den vollen Wert als `i128`
/// liefert (jiffs interne Auflösung übersteigt `i64`-Nanosekunden), und
/// prüft die Umrechnung nach `u64` explizit über `TryFrom` statt über ein
/// stillschweigend überlaufendes `as`-Cast — genau die Stelle, vor der der
/// Auftrag warnt.
///
/// # Arguments
/// - `timestamp` (`jiff::Timestamp`): der umzurechnende Zeitstempel.
///
/// # Returns
/// Die Nanosekunden seit der Epoche als `u64`, wenn `timestamp` nicht vor
/// der Epoche liegt und `u64::MAX` nicht überschreitet (das entspricht etwa
/// dem Jahr 2554).
///
/// # Errors
/// - [`OtlpError::TimestampOutOfRange`]: `timestamp` liegt vor der Epoche
///   (negativer Nanosekundenwert) oder nach dem größten in `u64`
///   darstellbaren Nanosekundenwert.
///
/// # Examples
/// ```ignore
/// assert_eq!(timestamp_to_unix_nanos(jiff::Timestamp::UNIX_EPOCH), Ok(0));
/// assert!(timestamp_to_unix_nanos(jiff::Timestamp::MIN).is_err());
/// ```
pub(crate) fn timestamp_to_unix_nanos(timestamp: jiff::Timestamp) -> Result<u64, OtlpError> {
    let nanos: i128 = timestamp.as_nanosecond();
    u64::try_from(nanos).map_err(|_| OtlpError::TimestampOutOfRange { nanos })
}

/// Textform einer `harw_observe::Unit` (siehe Moduldoc, Abschnitt
/// „`unit`-Zuordnung ist keine UCUM-Kodierung").
pub(crate) fn unit_str(unit: Unit) -> &'static str {
    match unit {
        Unit::Count => "count",
        Unit::Bytes => "bytes",
        Unit::Seconds => "seconds",
        Unit::Ratio => "ratio",
        Unit::Tokens => "tokens",
        Unit::Celsius => "celsius",
    }
}

/// Rechnet einen `harw_observe::FieldValue` (ein Label) in ein
/// [`AnyValue`]-Attribut um.
///
/// # Description
/// `FieldValue::U64` wird nach `i64` konvertiert, wenn es passt (OTLPs
/// `AnyValue` kennt keine vorzeichenlose Ganzzahl); ein zu großer Wert
/// (`> i64::MAX`) fällt verlustbehaftet auf `double_value` zurück, statt den
/// gesamten Datenpunkt zu verwerfen — ein Label ist Diagnosekontext, kein
/// Messwert, ein gerundeter extremer Labelwert ist hier vertretbar.
pub(crate) fn field_value_to_any_value(value: &FieldValue) -> AnyValue {
    match value {
        FieldValue::Str(s) => AnyValue {
            string_value: Some((*s).to_owned()),
            ..AnyValue::default()
        },
        FieldValue::Owned(s) => AnyValue {
            string_value: Some(s.clone()),
            ..AnyValue::default()
        },
        FieldValue::I64(v) => AnyValue {
            int_value: Some(v.to_string()),
            ..AnyValue::default()
        },
        FieldValue::U64(v) => match i64::try_from(*v) {
            Ok(i) => AnyValue {
                int_value: Some(i.to_string()),
                ..AnyValue::default()
            },
            #[allow(clippy::cast_precision_loss)]
            Err(_) => AnyValue {
                double_value: Some(*v as f64),
                ..AnyValue::default()
            },
        },
        FieldValue::F64(v) => AnyValue {
            double_value: Some(*v),
            ..AnyValue::default()
        },
        FieldValue::Bool(v) => AnyValue {
            bool_value: Some(*v),
            ..AnyValue::default()
        },
    }
}

/// Baut die sortierte Attributliste aus Label-Belegungen.
///
/// # Description
/// Sortiert nach Labelname (Byte-Reihenfolge), damit zwei Aufrufe mit
/// derselben Label-Menge dieselbe Reihenfolge liefern — dieselbe
/// Determinismus-Anforderung wie `harw-observe-prom::PromSink`s
/// `LabelSet`-Sortierung.
pub(crate) fn labels_to_attributes(labels: &[(FieldName, FieldValue)]) -> Vec<KeyValue> {
    let mut attributes: Vec<KeyValue> = labels
        .iter()
        .map(|(name, value)| KeyValue {
            key: name.as_str().to_owned(),
            value: field_value_to_any_value(value),
        })
        .collect();
    attributes.sort_by(|a, b| a.key.cmp(&b.key));
    attributes
}

/// Der rohe Skalarwert eines `MetricValue`, unabhängig von der Variante.
///
/// # Description
/// Diese Crate prüft `key.kind` nicht gegenseitig gegen die
/// `MetricValue`-Variante — dieselbe Zurückhaltung wie
/// `harw-observe-prom::PromSink::record` (siehe dessen Moduldoc/Kommentar
/// zu `metric_value_str`): welche Variante zu welcher `MetricKind` passt,
/// ist Sache des Emittenten.
pub(crate) fn metric_value_scalar(value: MetricValue) -> f64 {
    match value {
        #[allow(clippy::cast_precision_loss)]
        MetricValue::Count(v) => v as f64,
        MetricValue::Gauge(v) | MetricValue::Observation(v) => v,
    }
}

/// Baut den `as_int`/`as_double`-Anteil eines [`NumberDataPoint`] aus einem
/// `MetricValue`.
///
/// # Description
/// `MetricValue::Count` nutzt `as_int`, wenn der Wert nach `i64` passt
/// (der Regelfall für jeden realistischen Zählerstand), sonst
/// verlustbehaftet `as_double` — dieselbe Abwägung wie
/// [`field_value_to_any_value`] für `FieldValue::U64`. `Gauge`/`Observation`
/// nutzen stets `as_double`.
///
/// # Returns
/// `(as_double, as_int)` — genau eines davon ist `Some`.
pub(crate) fn metric_value_to_number(value: MetricValue) -> (Option<f64>, Option<String>) {
    match value {
        MetricValue::Count(v) => match i64::try_from(v) {
            Ok(i) => (None, Some(i.to_string())),
            #[allow(clippy::cast_precision_loss)]
            Err(_) => (Some(v as f64), None),
        },
        MetricValue::Gauge(v) | MetricValue::Observation(v) => (Some(v), None),
    }
}

/// Serialisiert einen fertigen Stapel-Rumpf als OTLP/JSON-Bytes.
///
/// # Errors
/// - [`OtlpError::Json`]: die Serialisierung ist fehlgeschlagen (für die
///   ausschließlich hier vorkommenden, eigenen Werttypen praktisch nur bei
///   erschöpftem Speicher erreichbar).
pub(crate) fn to_json_bytes(request: &ExportMetricsServiceRequest) -> Result<Vec<u8>, OtlpError> {
    Ok(serde_json::to_vec(request)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_timestamp_to_unix_nanos_at_epoch_is_zero() -> TestResult {
        // `OtlpError` leitet bewusst kein `PartialEq` ab (Vertrag §H.1
        // nennt es nicht als Anforderung, und `serde_json::Error` in
        // `OtlpError::Json` trägt selbst keines) — deshalb wird hier der
        // entpackte `Ok`-Wert verglichen statt des ganzen `Result`.
        assert_eq!(
            timestamp_to_unix_nanos(jiff::Timestamp::UNIX_EPOCH).map_err(ctx("timestamp"))?,
            0
        );
        Ok(())
    }

    #[test]
    fn test_timestamp_to_unix_nanos_far_future_within_range_succeeds() -> TestResult {
        // ~ Jahr 2096, deutlich unter der u64-Nanosekundengrenze (~ Jahr 2554).
        let far_future = jiff::Timestamp::from_second(4_000_000_000).map_err(ctx("from_second"))?;
        let expected = far_future.as_nanosecond() as u64;
        assert_eq!(
            timestamp_to_unix_nanos(far_future).map_err(ctx("timestamp"))?,
            expected
        );
        Ok(())
    }

    #[test]
    fn test_timestamp_to_unix_nanos_before_epoch_is_out_of_range() -> TestResult {
        let before_epoch = jiff::Timestamp::from_second(-1).map_err(ctx("from_second"))?;
        let Err(err) = timestamp_to_unix_nanos(before_epoch) else {
            return Err(TestError::Unexpected(
                "timestamp before epoch must be rejected".to_string(),
            ));
        };
        assert!(matches!(err, OtlpError::TimestampOutOfRange { nanos } if nanos < 0));
        Ok(())
    }

    #[test]
    fn test_timestamp_to_unix_nanos_min_is_out_of_range() {
        assert!(timestamp_to_unix_nanos(jiff::Timestamp::MIN).is_err());
    }

    #[test]
    fn test_timestamp_to_unix_nanos_max_overflows_u64() -> TestResult {
        // `Timestamp::MAX` liegt weit jenseits des Jahres 2554 und passt
        // damit nicht mehr in ein u64-Nanosekundenfeld — der Überlauf muss
        // als Fehler sichtbar werden, nicht still umschlagen.
        let Err(err) = timestamp_to_unix_nanos(jiff::Timestamp::MAX) else {
            return Err(TestError::Unexpected(
                "timestamp far beyond u64 range must be rejected".to_string(),
            ));
        };
        assert!(
            matches!(err, OtlpError::TimestampOutOfRange { nanos } if nanos > i128::from(u64::MAX))
        );
        Ok(())
    }

    #[test]
    fn test_unit_str_maps_every_variant() {
        assert_eq!(unit_str(Unit::Count), "count");
        assert_eq!(unit_str(Unit::Bytes), "bytes");
        assert_eq!(unit_str(Unit::Seconds), "seconds");
        assert_eq!(unit_str(Unit::Ratio), "ratio");
        assert_eq!(unit_str(Unit::Tokens), "tokens");
        assert_eq!(unit_str(Unit::Celsius), "celsius");
    }

    #[test]
    fn test_field_value_to_any_value_str_and_owned() {
        assert_eq!(
            field_value_to_any_value(&FieldValue::Str("x")).string_value,
            Some("x".to_owned())
        );
        assert_eq!(
            field_value_to_any_value(&FieldValue::Owned("y".to_owned())).string_value,
            Some("y".to_owned())
        );
    }

    #[test]
    fn test_field_value_to_any_value_i64_and_u64_fitting() {
        assert_eq!(
            field_value_to_any_value(&FieldValue::I64(-3)).int_value,
            Some("-3".to_owned())
        );
        assert_eq!(
            field_value_to_any_value(&FieldValue::U64(7)).int_value,
            Some("7".to_owned())
        );
    }

    #[test]
    fn test_field_value_to_any_value_u64_overflow_falls_back_to_double() {
        let huge = u64::MAX;
        let any = field_value_to_any_value(&FieldValue::U64(huge));
        assert!(any.int_value.is_none());
        assert!(any.double_value.is_some());
    }

    #[test]
    fn test_field_value_to_any_value_f64_and_bool() {
        assert_eq!(
            field_value_to_any_value(&FieldValue::F64(1.5)).double_value,
            Some(1.5)
        );
        assert_eq!(
            field_value_to_any_value(&FieldValue::Bool(true)).bool_value,
            Some(true)
        );
    }

    #[test]
    fn test_labels_to_attributes_empty_input_is_empty_output() {
        assert!(labels_to_attributes(&[]).is_empty());
    }

    #[test]
    fn test_labels_to_attributes_sorts_by_key() {
        let labels = [
            (harw_observe::field!("role"), FieldValue::Str("guard")),
            (harw_observe::field!("clan"), FieldValue::Str("research")),
        ];
        let attributes = labels_to_attributes(&labels);
        assert_eq!(attributes[0].key, "clan");
        assert_eq!(attributes[1].key, "role");
    }

    #[test]
    fn test_metric_value_to_number_count_uses_as_int() {
        let (as_double, as_int) = metric_value_to_number(MetricValue::Count(42));
        assert_eq!(as_double, None);
        assert_eq!(as_int, Some("42".to_owned()));
    }

    #[test]
    fn test_metric_value_to_number_gauge_uses_as_double() {
        let (as_double, as_int) = metric_value_to_number(MetricValue::Gauge(3.5));
        assert_eq!(as_double, Some(3.5));
        assert_eq!(as_int, None);
    }

    #[test]
    fn test_to_json_bytes_produces_valid_json() -> TestResult {
        let request = ExportMetricsServiceRequest::default();
        let bytes = to_json_bytes(&request).map_err(ctx("to_json_bytes"))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(ctx("from_slice"))?;
        assert_eq!(value, serde_json::json!({}));
        Ok(())
    }
}

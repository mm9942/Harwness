//! Reine Textbausteine des Prometheus-Textformats (Version 0.0.4):
//! Label-Escaping, Sonderwert-Formatierung und die `kind`-zu-Text-Abbildung.
//!
//! # Verantwortungsbereich
//! Enthält ausschließlich zustandslose Funktionen ohne I/O; die
//! Zustandshaltung und das Zusammensetzen ganzer Zeilen übernimmt
//! [`crate::sink::PromSink`]. Aufgeteilt, damit die sicherheitsrelevante
//! Escaping-Regel isoliert und ohne den restlichen Sink-Zustand testbar
//! ist.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind reine, zustandslose Abbildungen ohne Interior
//! Mutability — beliebig nebenläufig aufrufbar.
//!
//! # Fehler
//! Keine — jede Funktion ist eine totale Abbildung ohne fehlbaren Fall.

use harw_observe::MetricKind;

/// Escaped einen Labelwert nach der Prometheus-Textformat-Regel.
///
/// # Description
/// Ein Labelwert steht im Textformat in doppelten Anführungszeichen; ohne
/// Escaping bricht ein `"`, `\` oder eine echte Zeilenumbruch-Byte im
/// Rohwert die erzeugte Ausgabezeile auf — und ein Labelwert kann aus einer
/// Quelle stammen, die ein Angreifer beeinflusst (z. B. ein Tool-Argument
/// oder ein Dateiname). Ersetzt deshalb, in dieser Reihenfolge über die
/// Eingabezeichen (nicht über die schon geschriebene Ausgabe, sonst würde
/// ein frisch eingefügter Backslash ein zweites Mal escaped): `\` → `\\`,
/// `"` → `\"`, echter Zeilenumbruch → die zwei Zeichen `\n`.
///
/// # Arguments
/// - `raw` (`&str`): der ungeprüfte, rohe Labelwert.
///
/// # Returns
/// Den escaped Text, bereit zwischen `"..."` in eine Ausgabezeile
/// eingesetzt zu werden.
///
/// # Examples
/// ```ignore
/// assert_eq!(escape_label_value("a\\b\"c\nd"), "a\\\\b\\\"c\\nd");
/// ```
pub(crate) fn escape_label_value(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Formatiert einen Gleitkommawert nach Prometheus-Konvention.
///
/// # Description
/// Prometheus-Parser erwarten für Sonderwerte die literalen Token `NaN`,
/// `+Inf` und `-Inf` statt Rusts Standard-`Display` (`NaN`, `inf`, `-inf`).
/// Jeder andere Wert nutzt Rusts kürzeste rundtrip-fähige
/// Dezimaldarstellung.
///
/// # Arguments
/// - `value` (`f64`): der zu formatierende Wert.
///
/// # Returns
/// Die Textdarstellung, wie sie hinter dem Metriknamen (bzw. hinter der
/// Label-Menge) in einer Ausgabezeile erscheint.
///
/// # Examples
/// ```ignore
/// assert_eq!(format_prom_float(3.5), "3.5");
/// assert_eq!(format_prom_float(f64::INFINITY), "+Inf");
/// ```
pub(crate) fn format_prom_float(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "+Inf".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Inf".to_owned()
    } else {
        value.to_string()
    }
}

/// Textform einer [`MetricKind`] für die `# TYPE`-Zeile.
///
/// # Arguments
/// - `kind` (`MetricKind`): die Art der Messgröße.
///
/// # Returns
/// `"counter"`, `"gauge"` oder `"histogram"`. Bleibt für `Histogram`
/// vollständig, obwohl [`crate::sink::PromSink`] Histogramm-Schlüssel nie
/// speichert (siehe dessen Moduldoc) — eine `match` über [`MetricKind`]
/// muss erschöpfend bleiben, unabhängig davon, welcher Aufrufer diesen Fall
/// tatsächlich erreicht.
pub(crate) fn metric_kind_str(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
        MetricKind::Histogram => "histogram",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_label_value_escapes_backslash_quote_and_newline() {
        assert_eq!(escape_label_value("a\\b\"c\nd"), r#"a\\b\"c\nd"#);
    }

    #[test]
    fn test_escape_label_value_leaves_plain_text_unchanged() {
        assert_eq!(escape_label_value("research"), "research");
    }

    #[test]
    fn test_escape_label_value_does_not_double_escape_generated_backslashes() {
        // Ein einzelner roher Backslash muss zu genau zwei Zeichen werden,
        // nicht zu vier: der Escaping-Schritt iteriert über die
        // *Eingabe*zeichen, nie über die schon geschriebene Ausgabe.
        assert_eq!(escape_label_value("\\"), r"\\");
    }

    #[test]
    fn test_escape_label_value_handles_only_newline() {
        assert_eq!(escape_label_value("\n"), r"\n");
    }

    #[test]
    fn test_format_prom_float_renders_plain_values_as_shortest_decimal() {
        assert_eq!(format_prom_float(3.5), "3.5");
        assert_eq!(format_prom_float(0.0), "0");
    }

    #[test]
    fn test_format_prom_float_renders_special_values_as_prometheus_tokens() {
        assert_eq!(format_prom_float(f64::NAN), "NaN");
        assert_eq!(format_prom_float(f64::INFINITY), "+Inf");
        assert_eq!(format_prom_float(f64::NEG_INFINITY), "-Inf");
    }

    #[test]
    fn test_metric_kind_str_maps_all_three_kinds() {
        assert_eq!(metric_kind_str(MetricKind::Counter), "counter");
        assert_eq!(metric_kind_str(MetricKind::Gauge), "gauge");
        assert_eq!(metric_kind_str(MetricKind::Histogram), "histogram");
    }
}

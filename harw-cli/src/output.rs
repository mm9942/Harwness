//! Ausgabeformat der CLI: menschenlesbarer Text oder maschinenlesbares JSON.
//!
//! # Beschreibung
//! Jeder Befehl, der `--json` unterstützt, gibt seine Ergebnisse über einen
//! [`Printer`] aus, statt selbst `println!` aufzurufen. Der [`Printer`] kennt
//! das gewählte [`OutputFormat`] und entscheidet, ob eine Tabelle, ein
//! Einzelwert oder das Ergebnis einer Operation als ausgerichteter Text oder
//! als formatiertes JSON auf der Standardausgabe landet.
//!
//! Befehle ohne JSON-Form rufen zu Beginn [`Printer::require_text`] auf; im
//! JSON-Modus bricht der Befehl dann mit einer klaren Fehlermeldung ab,
//! statt `--json` still zu ignorieren.
//!
//! # Nebenläufigkeit
//! [`Printer`] ist ein kleiner Werttyp ohne inneren Zustand. Jede Ausgabe
//! sperrt die Standardausgabe nur für die Dauer eines Aufrufs.
//!
//! # Fehler
//! Alle Ausgabefunktionen liefern `Err(String)` mit einer deutschen
//! Meldung, wenn die Standardausgabe nicht beschreibbar ist oder ein Wert
//! nicht als JSON serialisiert werden kann.

use std::io::Write;

use harw_operations::OpOutput;

/// Das gewählte Ausgabeformat eines Befehls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OutputFormat {
    /// Menschenlesbarer Text (Vorgabe).
    #[default]
    Text,
    /// Formatiertes JSON (`--json`).
    Json,
}

/// Gibt Befehlsergebnisse im gewählten [`OutputFormat`] aus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Printer {
    format: OutputFormat,
}

impl Printer {
    /// Baut einen Printer für das angegebene Format.
    ///
    /// # Argumente
    /// - `f` ([`OutputFormat`]): Text oder JSON.
    ///
    /// # Rückgabe
    /// Den Printer.
    #[must_use]
    pub fn new(f: OutputFormat) -> Self {
        Self { format: f }
    }

    /// Gibt eine Tabelle aus.
    ///
    /// # Beschreibung
    /// Text: Kopfzeile, Trennlinie und eine Zeile je Eintrag, jede Spalte auf
    /// die Breite ihres längsten Werts ausgerichtet (siehe
    /// [`render_table`]). JSON: ein Array aus Objekten, deren Schlüssel die
    /// Spaltenüberschriften sind. Zellen jenseits der letzten Überschrift
    /// werden in beiden Formaten ignoriert; fehlende Zellen gelten als leer.
    ///
    /// # Argumente
    /// - `headers` (`&[&str]`): die Spaltenüberschriften.
    /// - `rows` (`&[Vec<String>]`): die Zeilen.
    ///
    /// # Fehler
    /// `Err(String)`, wenn die Standardausgabe nicht beschreibbar ist.
    pub fn table(&self, headers: &[&str], rows: &[Vec<String>]) -> Result<(), String> {
        match self.format {
            OutputFormat::Text => write_stdout(&render_table(headers, rows)),
            OutputFormat::Json => write_json(&table_json(headers, rows)),
        }
    }

    /// Gibt einen Einzelwert aus.
    ///
    /// # Argumente
    /// - `text` (`&str`): die Textform (Text-Modus); ein abschließender
    ///   Zeilenumbruch wird ergänzt.
    /// - `json` (`serde_json::Value`): die JSON-Form (JSON-Modus).
    ///
    /// # Fehler
    /// `Err(String)`, wenn die Standardausgabe nicht beschreibbar ist.
    pub fn value(&self, text: &str, json: serde_json::Value) -> Result<(), String> {
        match self.format {
            OutputFormat::Text => write_stdout(&with_newline(text)),
            OutputFormat::Json => write_json(&json),
        }
    }

    /// Gibt das Ergebnis einer Operation aus.
    ///
    /// # Beschreibung
    /// Text: [`OpOutput::text`]. JSON: die strukturierte Nutzlast
    /// [`OpOutput::data`], falls die Operation eine liefert, sonst
    /// `{"text": …}`.
    ///
    /// # Argumente
    /// - `out` (`&OpOutput`): das Ergebnis.
    ///
    /// # Fehler
    /// `Err(String)`, wenn die Standardausgabe nicht beschreibbar ist.
    pub fn op_output(&self, out: &OpOutput) -> Result<(), String> {
        match self.format {
            OutputFormat::Text => write_stdout(&with_newline(&out.text)),
            OutputFormat::Json => write_json(&op_output_json(out)),
        }
    }

    /// Verlangt Textausgabe für einen Befehl ohne JSON-Form.
    ///
    /// # Argumente
    /// - `command` (`&str`): der Befehl, wie ihn die Person tippt (z. B.
    ///   `"config edit"`).
    ///
    /// # Fehler
    /// Im JSON-Modus `Err("`<command>` unterstützt --json nicht")`.
    pub fn require_text(&self, command: &str) -> Result<(), String> {
        match self.format {
            OutputFormat::Text => Ok(()),
            OutputFormat::Json => Err(format!("`{command}` unterstützt --json nicht")),
        }
    }
}

/// Rendert eine Tabelle als ausgerichteten Text.
///
/// # Beschreibung
/// Jede Spalte ist so breit wie ihr längster Wert (in Unicode-Zeichen,
/// Überschrift eingeschlossen); Spalten sind durch zwei Leerzeichen
/// getrennt, Zeilenenden tragen keine Leerzeichen. Unter der Kopfzeile steht
/// eine Trennlinie aus `-`. Ohne Überschriften ist das Ergebnis leer.
///
/// # Argumente
/// - `headers` (`&[&str]`): die Spaltenüberschriften.
/// - `rows` (`&[Vec<String>]`): die Zeilen; überzählige Zellen werden
///   ignoriert, fehlende gelten als leer.
///
/// # Rückgabe
/// Den Tabellentext, jede Zeile mit `\n` abgeschlossen.
#[must_use]
pub(crate) fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row.iter()) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_line(&mut out, &widths, headers.iter().copied());
    let separators: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    push_line(&mut out, &widths, separators.iter().map(String::as_str));
    for row in rows {
        let cells = (0..widths.len()).map(|index| row.get(index).map_or("", String::as_str));
        push_line(&mut out, &widths, cells);
    }
    out
}

/// Hängt eine ausgerichtete Tabellenzeile an `out` an.
fn push_line<'a>(out: &mut String, widths: &[usize], cells: impl Iterator<Item = &'a str>) {
    let mut line = String::new();
    for (index, (cell, width)) in cells.zip(widths.iter()).enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        line.push_str(cell);
        let pad = width.saturating_sub(cell.chars().count());
        line.extend(std::iter::repeat_n(' ', pad));
    }
    out.push_str(line.trim_end());
    out.push('\n');
}

/// JSON-Form einer Tabelle: ein Array aus Objekten `{überschrift: zelle}`.
fn table_json(headers: &[&str], rows: &[Vec<String>]) -> serde_json::Value {
    let items = rows
        .iter()
        .map(|row| {
            let object: serde_json::Map<String, serde_json::Value> = headers
                .iter()
                .enumerate()
                .map(|(index, header)| {
                    let cell = row.get(index).cloned().unwrap_or_default();
                    ((*header).to_owned(), serde_json::Value::String(cell))
                })
                .collect();
            serde_json::Value::Object(object)
        })
        .collect();
    serde_json::Value::Array(items)
}

/// JSON-Form eines Operationsergebnisses: `data` oder `{"text": …}`.
fn op_output_json(out: &OpOutput) -> serde_json::Value {
    match &out.data {
        Some(data) => data.clone(),
        None => serde_json::json!({ "text": out.text }),
    }
}

/// `text` mit genau einem abschließenden Zeilenumbruch (leer bleibt leer).
fn with_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_owned()
    } else {
        format!("{text}\n")
    }
}

/// Schreibt `text` unverändert auf die Standardausgabe.
fn write_stdout(text: &str) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("Ausgabe konnte nicht geschrieben werden: {error}"))
}

/// Schreibt `value` als formatiertes JSON mit abschließendem Zeilenumbruch.
fn write_json(value: &serde_json::Value) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)
        .map_err(|error| format!("JSON-Ausgabe fehlgeschlagen: {error}"))?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("Ausgabe konnte nicht geschrieben werden: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|cell| (*cell).to_owned()).collect()
    }

    #[test]
    fn render_table_aligns_columns_to_the_widest_cell() {
        let rendered = render_table(
            &["ID", "STATUS"],
            &[row(&["job-1", "läuft"]), row(&["j2", "abgeschlossen"])],
        );

        assert_eq!(
            rendered,
            "ID     STATUS\n\
             -----  -------------\n\
             job-1  läuft\n\
             j2     abgeschlossen\n"
        );
    }

    #[test]
    fn render_table_counts_unicode_characters_not_bytes() {
        let rendered = render_table(&["A", "B"], &[row(&["äöü", "x"])]);

        assert_eq!(rendered, "A    B\n---  -\näöü  x\n");
    }

    #[test]
    fn render_table_pads_missing_cells_and_ignores_extra_cells() {
        let rendered = render_table(&["A", "B"], &[row(&["1"]), row(&["2", "3", "überzählig"])]);

        assert_eq!(rendered, "A  B\n-  -\n1\n2  3\n");
    }

    #[test]
    fn render_table_without_rows_prints_header_and_separator() {
        assert_eq!(render_table(&["NAME"], &[]), "NAME\n----\n");
    }

    #[test]
    fn render_table_without_headers_is_empty() {
        assert_eq!(render_table(&[], &[row(&["x"])]), "");
    }

    #[test]
    fn table_json_maps_headers_to_cells() {
        let value = table_json(&["id", "status"], &[row(&["a", "ok"]), row(&["b"])]);

        assert_eq!(
            value,
            serde_json::json!([
                { "id": "a", "status": "ok" },
                { "id": "b", "status": "" }
            ])
        );
    }

    #[test]
    fn op_output_json_prefers_structured_data() {
        let with_data = OpOutput {
            text: "zwei Jobs".to_owned(),
            data: Some(serde_json::json!({ "jobs": 2 })),
        };
        let text_only = OpOutput::from("nur Text".to_owned());

        assert_eq!(op_output_json(&with_data), serde_json::json!({ "jobs": 2 }));
        assert_eq!(
            op_output_json(&text_only),
            serde_json::json!({ "text": "nur Text" })
        );
    }

    #[test]
    fn require_text_accepts_text_mode() {
        assert_eq!(
            Printer::new(OutputFormat::Text).require_text("config edit"),
            Ok(())
        );
    }

    #[test]
    fn require_text_rejects_json_mode_naming_the_command() {
        assert_eq!(
            Printer::new(OutputFormat::Json).require_text("config edit"),
            Err("`config edit` unterstützt --json nicht".to_owned())
        );
    }

    #[test]
    fn output_format_defaults_to_text() {
        assert_eq!(OutputFormat::default(), OutputFormat::Text);
    }

    #[test]
    fn with_newline_appends_exactly_one_newline() {
        assert_eq!(with_newline("a"), "a\n");
        assert_eq!(with_newline("a\n"), "a\n");
        assert_eq!(with_newline(""), "");
    }
}

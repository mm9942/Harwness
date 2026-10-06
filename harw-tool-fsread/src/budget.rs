//! Ausgabegrenzen und gemeinsame Ausgabe-Bausteine aller `PL-93`-Werkzeuge.
//!
//! # Verantwortung
//! - harte Obergrenzen ([`MAX_OUTPUT_BYTES`], [`ITEM_BUDGET`], [`HARD_MAX_ITEMS`]),
//! - [`Collector`]: sammelt JSON-Elemente, solange Anzahl und serialisierte
//!   Größe in das Budget passen, und merkt sich, ob gekürzt wurde,
//! - [`clip_text`]: zeichengrenzen-sicheres Kürzen von Text,
//! - [`ok`]/[`fail`]: einheitliche Ausgabeform (`tool`, `summary`, Daten).
//!
//! # Nebenläufigkeit
//! Zustandslos bzw. nur lokale Werte; alles `Send`.

use harw_tools::ToolOutput;
use serde_json::{Map, Value};

/// Obergrenze einer einzelnen Tool-Antwort (64 KiB).
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Budget für Listenelemente: Gesamtbudget abzüglich Reserve für Kopfdaten.
pub const ITEM_BUDGET: usize = MAX_OUTPUT_BYTES - 6 * 1024;

/// Absolute Obergrenze für Listeneinträge, egal was der Aufrufer verlangt.
pub const HARD_MAX_ITEMS: usize = 5_000;

/// Obergrenze für einen einzelnen Textblock (Dateiinhalt, Diff, Rohlog).
pub const MAX_TEXT_BYTES: usize = 48 * 1024;

/// Sammelt JSON-Elemente unter Anzahl- und Bytegrenze.
#[derive(Debug)]
pub struct Collector {
    items: Vec<Value>,
    used: usize,
    byte_budget: usize,
    max_items: usize,
    omitted: usize,
    stopped_by_bytes: bool,
}

impl Collector {
    /// Neuer Sammler. `max_items` wird auf [`HARD_MAX_ITEMS`] (und mindestens 1)
    /// begrenzt.
    #[must_use]
    pub fn new(max_items: usize) -> Self {
        Self::with_budget(max_items, ITEM_BUDGET)
    }

    /// Wie [`Collector::new`] mit eigenem Bytebudget.
    #[must_use]
    pub fn with_budget(max_items: usize, byte_budget: usize) -> Self {
        Self {
            items: Vec::new(),
            used: 0,
            byte_budget,
            max_items: max_items.clamp(1, HARD_MAX_ITEMS),
            omitted: 0,
            stopped_by_bytes: false,
        }
    }

    /// Nimmt `item` auf, falls Platz ist. `false` heißt: voll, der Aufrufer
    /// soll aufhören (das Element ist als ausgelassen gezählt).
    pub fn push(&mut self, item: Value) -> bool {
        if self.items.len() >= self.max_items {
            self.omitted += 1;
            return false;
        }
        let size = item.to_string().len() + 1;
        if self.used + size > self.byte_budget {
            self.omitted += 1;
            self.stopped_by_bytes = true;
            return false;
        }
        self.used += size;
        self.items.push(item);
        true
    }

    /// Anzahl der aufgenommenen Elemente.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true`, wenn nichts aufgenommen wurde.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// `true`, wenn mindestens ein Element nicht mehr hineinpasste.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.omitted > 0
    }

    /// Grund der Kürzung für das Feld `stopped`.
    #[must_use]
    pub fn stop_reason(&self) -> Option<&'static str> {
        if self.omitted == 0 {
            None
        } else if self.stopped_by_bytes {
            Some("output_limit")
        } else {
            Some("entry_limit")
        }
    }

    /// Gibt die gesammelten Elemente heraus.
    #[must_use]
    pub fn into_items(self) -> Vec<Value> {
        self.items
    }
}

/// Kürzt `text` auf höchstens `max_bytes` Bytes an einer Zeichengrenze.
///
/// Liefert den (ggf. gekürzten) Text und ob gekürzt wurde.
#[must_use]
pub fn clip_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// Wie [`clip_text`], aber für beliebige Bytes (ungültiges UTF-8 wird
/// ersetzt). Gekürzt wird vor der Umwandlung.
#[must_use]
pub fn clip_lossy(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    if bytes.len() <= max_bytes {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let text = String::from_utf8_lossy(&bytes[..max_bytes]).into_owned();
    (text, true)
}

/// Erfolgsausgabe: JSON-Objekt aus `data` plus `tool` und `summary`.
///
/// Ist `data` kein Objekt, wird es unter `data` abgelegt.
#[must_use]
pub fn ok(tool: &str, summary: impl Into<String>, data: Value) -> ToolOutput {
    let mut map = match data {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("data".to_owned(), other);
            map
        }
    };
    map.insert("tool".to_owned(), Value::String(tool.to_owned()));
    map.insert("summary".to_owned(), Value::String(summary.into()));
    ToolOutput::json(Value::Object(map))
}

/// Fehlerausgabe `"<tool>: <message>"` mit strukturiertem Log.
#[must_use]
pub fn fail(tool: &str, message: impl AsRef<str>) -> ToolOutput {
    let message = message.as_ref();
    tracing::debug!(tool, error = %message, "PL-93-Werkzeug abgelehnt");
    ToolOutput::error(format!("{tool}: {message}"))
}

/// Begrenzt einen Aufruferwert `requested` auf `1..=hard`, mit `default`.
#[must_use]
pub fn limit_or(requested: Option<usize>, default: usize, hard: usize) -> usize {
    requested
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(hard)
}

/// `Option<bool>` als Flag (`None` = `false`).
#[must_use]
pub fn flag(value: Option<bool>) -> bool {
    value.unwrap_or(false)
}

/// Prüft einen Auswahlwert gegen eine Allowlist.
///
/// # Errors
/// Meldung mit den erlaubten Werten.
pub fn choice<'a>(
    field: &str,
    value: Option<&'a str>,
    allowed: &[&str],
    default: &'a str,
) -> Result<&'a str, String> {
    let value = value.unwrap_or(default);
    if allowed.contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "invalid {field} '{}': expected one of {}",
            value.chars().take(32).collect::<String>(),
            allowed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use serde_json::json;

    #[test]
    fn collector_stops_at_item_limit_and_reports() -> TestResult {
        let mut collector = Collector::new(2);
        assert!(collector.push(json!("a")));
        assert!(collector.push(json!("b")));
        assert!(!collector.push(json!("c")));
        assert!(collector.truncated());
        assert_eq!(collector.stop_reason(), Some("entry_limit"));
        assert_eq!(collector.len(), 2);
        Ok(())
    }

    #[test]
    fn collector_stops_at_byte_budget() -> TestResult {
        let mut collector = Collector::with_budget(100, 20);
        assert!(collector.push(json!("0123456789")));
        assert!(!collector.push(json!("0123456789")));
        assert_eq!(collector.stop_reason(), Some("output_limit"));
        Ok(())
    }

    #[test]
    fn collector_clamps_hostile_item_limit() -> TestResult {
        let mut collector = Collector::with_budget(usize::MAX, usize::MAX);
        for _ in 0..HARD_MAX_ITEMS {
            assert!(collector.push(json!(1)));
        }
        assert!(!collector.push(json!(1)));
        Ok(())
    }

    #[test]
    fn clip_text_respects_char_boundaries() -> TestResult {
        let (clipped, was) = clip_text("aäb", 2);
        assert!(was);
        assert_eq!(clipped, "a");
        let (same, was) = clip_text("abc", 3);
        assert!(!was);
        assert_eq!(same, "abc");
        Ok(())
    }

    #[test]
    fn choice_rejects_unknown_value() -> TestResult {
        assert!(choice("sort", Some("bogus"), &["name", "size"], "name").is_err());
        assert_eq!(choice("sort", None, &["name", "size"], "name"), Ok("name"));
        Ok(())
    }

    #[test]
    fn limit_or_clamps() -> TestResult {
        assert_eq!(limit_or(None, 10, 50), 10);
        assert_eq!(limit_or(Some(0), 10, 50), 10);
        assert_eq!(limit_or(Some(10_000), 10, 50), 50);
        Ok(())
    }
}

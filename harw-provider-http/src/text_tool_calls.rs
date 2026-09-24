//! Text-eingebettete Tool-Call-Wiederherstellung für OpenAI-kompatible
//! Chat-Completions-Gateways (GLM-5.x / Kimi hinter einem Cloudflare
//! Worker), die Tool-Calls gelegentlich als **Text** in
//! `choices[0].message.content` senden statt strukturiert in
//! `message.tool_calls`. Spec-Quelle: Coding-Task-Brief
//! "GLM-5.x / Kimi text-embedded tool calls" (harw-provider-http, 2026-09-22).
//!
//! ## Verantwortung
//! Erkennt und parst `<tool_call>`-Marker-Text in mehreren konkurrierenden
//! Dialekten:
//! - `NAME{json}` — Argumente direkt als JSON-Objekt.
//! - `NAME({json})` — Klammer-Variante.
//! - `NAME<arg_key>k</arg_key><arg_value>v</arg_value>...` — wiederholte
//!   Schlüssel/Wert-Paare (Qwen-XML-Stil).
//! - `{"name":..,"arguments":{..}|"<json-string>"}` — Hermes/Qwen-
//!   Objektformat, `arguments` als Objekt oder JSON-kodierter String.
//!
//! Runde 7, Teil L7 — weitere Formate lokaler Modelle
//! ([`parse_alternative_tool_calls`], nur ohne `<tool_call>`-Marker):
//! - Mistral: `[TOOL_CALLS][{"name":..,"arguments":{..}}]` bzw.
//!   `[TOOL_CALLS]NAME[ARGS]{json}` (auch mehrfach).
//! - Llama 3.x: `<|python_tag|>{"name":..,"parameters":{..}}` (mehrere
//!   durch `;` getrennt, abschließendes `<|eom_id|>`/`<|eot_id|>` erlaubt).
//! - Blankes oder in ```` ``` ```` umzäuntes JSON, das **ausschließlich** aus
//!   einem Aufruf-Objekt (oder einem Array davon) mit bekanntem
//!   Werkzeugnamen besteht.
//!
//! Strippt außerdem führende `<think>...</think>`-Blöcke bzw. verwaiste
//! führende `</think>`-Marker (Gateway hat das Öffnungs-Tag verschluckt) aus
//! Content-Text — unabhängig davon, ob Tool-Calls gefunden werden.
//!
//! Fail-closed: jedes nicht erkennbare oder nicht angebotene
//! `<tool_call>`-Segment lässt die **gesamte** Erkennung scheitern (`None`)
//! — der Aufrufer (`interpret_chat` in `lib.rs`) lässt die Antwort dann
//! unverändert (roher Text bleibt die finale Antwort); es wird nie geraten.
//!
//! ## Nebenläufigkeit
//! Rein funktional, kein geteilter Zustand — alle Funktionen sind
//! `Send + Sync` durch reine Wertsemantik.
//!
//! ## Fehler
//! Kein eigener Error-Typ; Fehlschläge werden ausschließlich über `None`
//! signalisiert (siehe [`parse_text_tool_calls`]).
//!
//! # Examples
//! ```rust,ignore
//! let offered = ["fs.read"];
//! let content = "<tool_call>fs.read{\"path\":\"x\"}";
//! let (remaining, calls) = parse_text_tool_calls(content, &offered).unwrap();
//! assert_eq!(calls[0].name, "fs.read");
//! ```

use serde_json::Value;

/// Ein aus Text geparster Tool-Call: Name plus JSON-Argumente.
///
/// Wird ausschließlich von [`parse_text_tool_calls`] erzeugt; der Aufrufer
/// (`interpret_chat`) übersetzt daraus ein `harw_tools::ToolCall` mit einer
/// synthetischen `call_text_<n>`-ID.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedCall {
    pub(crate) name: String,
    pub(crate) arguments: Value,
}

/// Strippt einen führenden `<think>...</think>`-Block bzw. einen verwaisten
/// führenden `</think>`-Marker aus `content`.
///
/// # Description
/// Erkennt genau zwei Präfix-Formen am Anfang von `content`:
/// - `<think>TEXT</think>REST` → `(REST, Some(TEXT))`.
/// - `</think>REST` (kein Öffnungs-Tag — manche Gateways verschlucken es)
///   → `(REST, None)`: es gibt keinen Denktext zu übernehmen, nur der
///   Marker wird entfernt.
///
/// Ist `<think>` unterminiert (kein `</think>` im Rest gefunden) oder liegt
/// keine der beiden Formen vor, bleibt `content` unverändert — es wird
/// nicht geraten.
///
/// # Arguments
/// - `content` (`&str`): der rohe `message.content`-String, geliehen.
///
/// # Returns
/// `(String, Option<String>)` — verbleibender Text (führender Leerraum nach
/// einem entfernten Tag getrimmt) und optional der extrahierte Denktext, den
/// der Aufrufer in einen Reasoning-Slot verschieben kann, falls einer
/// existiert; ansonsten wird er verworfen.
pub(crate) fn strip_leading_think(content: &str) -> (String, Option<String>) {
    if let Some(after_open) = content.strip_prefix("<think>") {
        if let Some(end) = after_open.find("</think>") {
            let think_text = after_open[..end].trim();
            let remaining = after_open[end + "</think>".len()..].trim_start();
            return (remaining.to_owned(), non_empty(think_text));
        }
        return (content.to_owned(), None);
    }
    if let Some(after_close) = content.strip_prefix("</think>") {
        return (after_close.trim_start().to_owned(), None);
    }
    (content.to_owned(), None)
}

// Leerer/nur-Leerraum-Text wird als "kein Denktext" behandelt.
fn non_empty(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

/// Parst text-eingebettete Tool-Calls aus einer Chat-Completions-`content`-
/// Zeichenkette (GLM-5.x/Kimi-Gateway-Workaround).
///
/// # Description
/// Aktiviert sich nur, wenn `content` (nach Entfernen eines führenden
/// `<think>`-Blocks, siehe [`strip_leading_think`]) mindestens einen
/// `<tool_call>`-Marker enthält. Zerlegt an `<tool_call>`; das erste Stück
/// ist der sichtbare Resttext (getrimmt), jedes weitere Stück wird als ein
/// Tool-Call interpretiert (siehe Modul-Dokumentation für die vier
/// unterstützten Dialekte).
///
/// Alle Dialekte nutzen einen JSON-Präfix-Parser
/// (`serde_json::Deserializer::from_str(..).into_iter::<Value>()`, erstes
/// Element), sodass Müll nach dem eigentlichen JSON-Wert (ein schließendes
/// `)`, ein `</tool_call>`, ein fremdes `</arg_value>`, …) ignoriert wird,
/// statt einen Parse-Fehler auszulösen.
///
/// Ein Tool-Call wird nur akzeptiert, wenn sein Name exakt (byteweise) in
/// `offered` vorkommt — `offered` sind die für diesen Request tatsächlich
/// angebotenen, Wire-kodierten Tool-Namen (wie das Modell sie tatsächlich
/// gesehen hat). Scheitert irgendein Segment am Parsen oder nennt es einen
/// nicht angebotenen Namen, liefert die gesamte Funktion `None` zurück
/// (fail-closed, alles-oder-nichts) — der Aufrufer lässt die Antwort dann
/// unverändert.
///
/// # Arguments
/// - `content` (`&str`): der rohe `message.content`-String der Antwort.
/// - `offered` (`&[&str]`): die für diesen Request angebotenen (Wire-Form)
///   Tool-Namen.
///
/// # Returns
/// `Some((remaining_text, calls))` bei mindestens einem erfolgreich
/// geparsten, angebotenen Tool-Call; `None`, wenn kein `<tool_call>`-Marker
/// vorliegt oder irgendein Segment nicht vertrauenswürdig geparst werden
/// konnte.
pub(crate) fn parse_text_tool_calls(
    content: &str,
    offered: &[&str],
) -> Option<(String, Vec<ParsedCall>)> {
    let (content, _think_text) = strip_leading_think(content);
    if !content.contains("<tool_call>") {
        return None;
    }
    let mut parts = content.split("<tool_call>");
    let prefix = parts.next().unwrap_or_default().trim().to_owned();
    let mut calls = Vec::new();
    for segment in parts {
        calls.push(parse_one_segment(segment, offered)?);
    }
    if calls.is_empty() {
        return None;
    }
    Some((prefix, calls))
}

/// Runde 7, Teil L7: Parst Tool-Call-Formate lokaler Modelle, die ohne
/// `<tool_call>`-Marker auskommen (siehe Modul-Dokumentation).
///
/// # Description
/// Reihenfolge: Mistral-`[TOOL_CALLS]`, Llama-`<|python_tag|>`, zuletzt
/// blankes/umzäuntes JSON. Die Marker-Formate liefern den Text vor dem
/// Marker als sichtbaren Rest; das JSON-Format verlangt, dass die **gesamte**
/// Antwort (nach `<think>`-Bereinigung und ohne Code-Zaun) genau ein
/// Aufruf-Objekt oder ein Array solcher Objekte ist — JSON mitten im Text
/// wird nie als Aufruf gedeutet.
///
/// Fail-closed wie [`parse_text_tool_calls`]: ein nicht parsebares Segment,
/// ein nicht angebotener Name oder Argumente, die kein JSON-Objekt sind,
/// lassen die gesamte Erkennung scheitern (`None`).
///
/// # Arguments
/// - `content` (`&str`): der rohe `message.content`-String.
/// - `offered` (`&[&str]`): die angebotenen (Wire-Form) Tool-Namen.
///
/// # Returns
/// `Some((remaining_text, calls))` bei mindestens einem Aufruf, sonst `None`.
pub(crate) fn parse_alternative_tool_calls(
    content: &str,
    offered: &[&str],
) -> Option<(String, Vec<ParsedCall>)> {
    const MISTRAL: &str = "[TOOL_CALLS]";
    const LLAMA: &str = "<|python_tag|>";
    let (content, _think_text) = strip_leading_think(content);
    if content.contains("<tool_call>") {
        return None;
    }
    let (prefix, calls) = if let Some(index) = content.find(MISTRAL) {
        let calls = parse_mistral_calls(&content[index + MISTRAL.len()..])?;
        (content[..index].trim().to_owned(), calls)
    } else if let Some(index) = content.find(LLAMA) {
        let calls = parse_llama_calls(&content[index + LLAMA.len()..])?;
        (content[..index].trim().to_owned(), calls)
    } else {
        (String::new(), parse_bare_json_calls(&content)?)
    };
    if calls.is_empty()
        || calls
            .iter()
            .any(|call| !offered.contains(&call.name.as_str()))
    {
        return None;
    }
    Some((prefix, calls))
}

/// Mistral: ein oder mehrere `[TOOL_CALLS]`-Segmente, jedes entweder ein
/// JSON-Array/-Objekt von Aufrufen oder `NAME[ARGS]{json}`.
fn parse_mistral_calls(rest: &str) -> Option<Vec<ParsedCall>> {
    let mut calls = Vec::new();
    for segment in rest.split("[TOOL_CALLS]") {
        let segment = segment.trim();
        if segment.starts_with('[') || segment.starts_with('{') {
            calls.extend(calls_from_value(&parse_json_prefix(segment)?)?);
            continue;
        }
        let (name, arguments) = segment.split_once("[ARGS]")?;
        let name = name.trim();
        if name.is_empty() || !name.chars().all(is_tool_name_char) {
            return None;
        }
        let arguments = parse_json_prefix(arguments.trim_start())?;
        if !arguments.is_object() {
            return None;
        }
        calls.push(ParsedCall {
            name: name.to_owned(),
            arguments,
        });
    }
    Some(calls)
}

/// Llama 3.x: JSON-Aufrufe nach `<|python_tag|>`, durch `;` getrennt;
/// `<|eom_id|>`/`<|eot_id|>` beenden die Liste. Python-Code (eingebaute
/// Llama-Werkzeuge) ist kein JSON und scheitert daher fail-closed.
fn parse_llama_calls(rest: &str) -> Option<Vec<ParsedCall>> {
    let mut calls = Vec::new();
    let mut rest = rest.trim_start();
    loop {
        rest = rest.trim_start_matches(|c: char| c == ';' || c.is_whitespace());
        if rest.is_empty() || rest.starts_with("<|") {
            break;
        }
        let mut values = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        let value = values.next()?.ok()?;
        let consumed = values.byte_offset();
        calls.extend(calls_from_value(&value)?);
        rest = rest.get(consumed..)?;
    }
    Some(calls)
}

/// Blankes oder umzäuntes JSON, das vollständig aus Aufrufen besteht.
fn parse_bare_json_calls(content: &str) -> Option<Vec<ParsedCall>> {
    let trimmed = content.trim();
    let body = match trimmed.strip_prefix("```") {
        Some(fenced) => {
            let (_language, inner) = fenced.split_once('\n')?;
            inner.trim_end().strip_suffix("```")?.trim()
        }
        None => trimmed,
    };
    if !(body.starts_with('{') || body.starts_with('[')) {
        return None;
    }
    let value: Value = serde_json::from_str(body).ok()?;
    calls_from_value(&value)
}

/// Deutet ein JSON-Objekt oder -Array als Aufruf(e); jedes Objekt braucht
/// `name` und Argumente (`arguments` oder `parameters`, Objekt oder
/// JSON-kodierter Objekt-String; fehlend = leeres Objekt). Die
/// OpenAI-Form `{"type":"function","function":{..}}` wird ebenfalls
/// akzeptiert.
fn calls_from_value(value: &Value) -> Option<Vec<ParsedCall>> {
    match value {
        Value::Array(items) if !items.is_empty() => items.iter().map(call_from_object).collect(),
        Value::Object(_) => Some(vec![call_from_object(value)?]),
        _ => None,
    }
}

fn call_from_object(value: &Value) -> Option<ParsedCall> {
    let object = value.as_object()?;
    let object = match object.get("function") {
        Some(Value::Object(function)) => function,
        _ => object,
    };
    let name = object.get("name")?.as_str()?.to_owned();
    let arguments = match object.get("arguments").or_else(|| object.get("parameters")) {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).ok()?,
        Some(other) => other.clone(),
        None => Value::Object(serde_json::Map::new()),
    };
    if !arguments.is_object() {
        return None;
    }
    Some(ParsedCall { name, arguments })
}

/// Parst genau ein `<tool_call>`-Segment (der Text nach einem
/// `<tool_call>`-Marker, vor dem nächsten Marker oder dem Stringende) in
/// einen [`ParsedCall`].
///
/// Wählt anhand des ersten Nicht-Leerzeichen-Zeichens die Dialekt-Form
/// (siehe Modul-Dokumentation) und prüft danach zentral, ob der geparste
/// Name in `offered` steht.
fn parse_one_segment(segment: &str, offered: &[&str]) -> Option<ParsedCall> {
    let segment = segment.trim_start();
    let (name, arguments) = if segment.starts_with('{') {
        parse_hermes_segment(segment)?
    } else {
        let name_len = segment
            .find(|c: char| !is_tool_name_char(c))
            .unwrap_or(segment.len());
        if name_len == 0 {
            return None;
        }
        let name = segment[..name_len].to_owned();
        let rest = &segment[name_len..];
        let arguments = if rest.starts_with('{') {
            parse_json_prefix(rest)?
        } else if let Some(after_paren) = rest.strip_prefix('(') {
            parse_json_prefix(after_paren)?
        } else if rest.starts_with("<arg_key>") {
            parse_arg_kv_segment(rest)?
        } else {
            return None;
        };
        (name, arguments)
    };
    if !offered.contains(&name.as_str()) {
        return None;
    }
    Some(ParsedCall { name, arguments })
}

/// `true` für Zeichen, die in einem Tool-Namen vorkommen dürfen
/// (`fs.read`, `shell_exec`, `context-load`, …).
fn is_tool_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Parst genau einen JSON-Wert vom Anfang von `s`; nachfolgender Text (Müll,
/// schließende Klammern fremder Marker) wird stillschweigend ignoriert.
fn parse_json_prefix(s: &str) -> Option<Value> {
    let mut values = serde_json::Deserializer::from_str(s).into_iter::<Value>();
    values.next()?.ok()
}

/// Parst das Hermes/Qwen-Objektformat `{"name":..,"arguments":..}`.
///
/// `arguments` darf ein JSON-Objekt oder ein JSON-kodierter String sein;
/// fehlt das Feld, wird ein leeres Objekt angenommen.
fn parse_hermes_segment(segment: &str) -> Option<(String, Value)> {
    let value = parse_json_prefix(segment)?;
    let obj = value.as_object()?;
    let name = obj.get("name")?.as_str()?.to_owned();
    let arguments = match obj.get("arguments") {
        Some(Value::String(raw)) => serde_json::from_str(raw).ok()?,
        Some(other) => other.clone(),
        None => Value::Object(serde_json::Map::new()),
    };
    Some((name, arguments))
}

/// Parst wiederholte `<arg_key>k</arg_key><arg_value>v</arg_value>`-Paare zu
/// einem JSON-Objekt. Jedes `v` wird zuerst als JSON geparst (Zahlen,
/// Arrays, Objekte, `true`/`false`/`null`), sonst als roher String
/// übernommen.
fn parse_arg_kv_segment(mut rest: &str) -> Option<Value> {
    let mut map = serde_json::Map::new();
    loop {
        rest = rest.strip_prefix("<arg_key>")?;
        let key_end = rest.find("</arg_key>")?;
        let key = rest[..key_end].to_owned();
        rest = &rest[key_end + "</arg_key>".len()..];
        rest = rest.strip_prefix("<arg_value>")?;
        let value_end = rest.find("</arg_value>")?;
        let raw_value = &rest[..value_end];
        let value = serde_json::from_str::<Value>(raw_value)
            .unwrap_or_else(|_| Value::String(raw_value.to_owned()));
        map.insert(key, value);
        rest = &rest[value_end + "</arg_value>".len()..];
        if !rest.starts_with("<arg_key>") {
            break;
        }
    }
    Some(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_parse_text_tool_calls_example_1_brace_form_two_calls() -> TestResult {
        let content = concat!(
            "<tool_call>fs.grep{\"pattern\":\"enum Command\",\"paths\":[\"harw-cli/src/cli.rs\"]}",
            "<tool_call>fs.read{\"path\":\"harw-cli/src/main.rs\",\"max_bytes\":30000}"
        );
        let offered = ["fs.grep", "fs.read"];
        let (remaining, calls) = parse_text_tool_calls(content, &offered)
            .ok_or(TestError::Missing("must parse both calls"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "fs.grep");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"pattern": "enum Command", "paths": ["harw-cli/src/cli.rs"]})
        );
        assert_eq!(calls[1].name, "fs.read");
        assert_eq!(
            calls[1].arguments,
            serde_json::json!({"path": "harw-cli/src/main.rs", "max_bytes": 30000})
        );
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_example_2_paren_form_strips_stray_think_close() -> TestResult {
        let content = concat!(
            "</think><tool_call>fs.list({\"path\":\"dod/crates/harw-probe-bpf/src\"})",
            "<tool_call>fs.list({\"path\":\"dod/crates/harw-probe-fs/src\"})"
        );
        let offered = ["fs.list"];
        let (remaining, calls) = parse_text_tool_calls(content, &offered)
            .ok_or(TestError::Missing("must parse both calls"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "fs.list");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"path": "dod/crates/harw-probe-bpf/src"})
        );
        assert_eq!(
            calls[1].arguments,
            serde_json::json!({"path": "dod/crates/harw-probe-fs/src"})
        );
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_example_3_arg_key_value_form() -> TestResult {
        let content = "<tool_call>fs.read<arg_key>path</arg_key><arg_value>xtask/src/main.rs</arg_value></tool_call>";
        let offered = ["fs.read"];
        let (remaining, calls) = parse_text_tool_calls(content, &offered)
            .ok_or(TestError::Missing("must parse arg_key/arg_value form"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"path": "xtask/src/main.rs"})
        );
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_example_4_trailing_junk_after_json_is_ignored() -> TestResult {
        let content =
            "<tool_call>fs.read({\"path\":\"CHANGELOG.md\",\"max_bytes\":24000})</arg_value>";
        let offered = ["fs.read"];
        let (remaining, calls) = parse_text_tool_calls(content, &offered)
            .ok_or(TestError::Missing("must ignore trailing garbage"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert_eq!(
            calls[0].arguments,
            serde_json::json!({"path": "CHANGELOG.md", "max_bytes": 24000})
        );
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_hermes_object_form() -> TestResult {
        let content = r#"<tool_call>{"name":"fs.read","arguments":{"path":"x"}}</tool_call>"#;
        let offered = ["fs.read"];
        let (remaining, calls) = parse_text_tool_calls(content, &offered)
            .ok_or(TestError::Missing("must parse Hermes object form"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x"}));
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_hermes_object_form_with_string_arguments() -> TestResult {
        let content = r#"<tool_call>{"name":"fs.read","arguments":"{\"path\":\"x\"}"}</tool_call>"#;
        let offered = ["fs.read"];
        let (_remaining, calls) = parse_text_tool_calls(content, &offered).ok_or(
            TestError::Missing("must parse Hermes form with string-encoded arguments"),
        )?;
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x"}));
        Ok(())
    }

    #[test]
    fn test_parse_text_tool_calls_unoffered_name_returns_none() {
        let content = "<tool_call>shell.exec{\"cmd\":\"rm -rf /\"}";
        let offered = ["fs.read"];
        assert_eq!(parse_text_tool_calls(content, &offered), None);
    }

    #[test]
    fn test_parse_text_tool_calls_malformed_json_returns_none() {
        let content = "<tool_call>fs.read{not json}";
        let offered = ["fs.read"];
        assert_eq!(parse_text_tool_calls(content, &offered), None);
    }

    #[test]
    fn test_parse_text_tool_calls_one_bad_segment_fails_the_whole_batch() {
        let content = concat!(
            "<tool_call>fs.read{\"path\":\"a\"}",
            "<tool_call>shell.exec{\"cmd\":\"rm -rf /\"}"
        );
        let offered = ["fs.read"];
        assert_eq!(parse_text_tool_calls(content, &offered), None);
    }

    #[test]
    fn test_parse_text_tool_calls_no_marker_returns_none() {
        let content = "just a normal answer, no tool calls here";
        let offered = ["fs.read"];
        assert_eq!(parse_text_tool_calls(content, &offered), None);
    }

    #[test]
    fn test_parse_text_tool_calls_preserves_remaining_text_before_marker() -> TestResult {
        let content = "Here is my plan.\n<tool_call>fs.read{\"path\":\"a\"}";
        let offered = ["fs.read"];
        let (remaining, _calls) =
            parse_text_tool_calls(content, &offered).ok_or(TestError::Missing("must parse"))?;
        assert_eq!(remaining, "Here is my plan.");
        Ok(())
    }

    // Runde 7, Teil L7: weitere Formate lokaler Modelle.

    #[test]
    fn test_parse_alternative_mistral_array_form() -> TestResult {
        let content = r#"[TOOL_CALLS] [{"name": "fs.read", "arguments": {"path": "a.rs"}}]"#;
        let (remaining, calls) = parse_alternative_tool_calls(content, &["fs.read"])
            .ok_or(TestError::Missing("must parse Mistral array form"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.rs"}));
        Ok(())
    }

    #[test]
    fn test_parse_alternative_mistral_args_form_with_two_calls() -> TestResult {
        let content = concat!(
            "Ich lese beide Dateien.",
            "[TOOL_CALLS]fs.read[ARGS]{\"path\":\"a\"}",
            "[TOOL_CALLS]fs.list[ARGS]{\"path\":\"src\"}"
        );
        let (remaining, calls) = parse_alternative_tool_calls(content, &["fs.read", "fs.list"])
            .ok_or(TestError::Missing("must parse Mistral [ARGS] form"))?;
        assert_eq!(remaining, "Ich lese beide Dateien.");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].name, "fs.list");
        assert_eq!(calls[1].arguments, serde_json::json!({"path": "src"}));
        Ok(())
    }

    #[test]
    fn test_parse_alternative_llama_python_tag_form() -> TestResult {
        let content = concat!(
            "<|python_tag|>{\"name\": \"fs.read\", \"parameters\": {\"path\": \"a\"}}; ",
            "{\"name\": \"fs.read\", \"parameters\": {\"path\": \"b\"}}<|eom_id|>"
        );
        let (remaining, calls) = parse_alternative_tool_calls(content, &["fs.read"])
            .ok_or(TestError::Missing("must parse Llama python_tag form"))?;
        assert_eq!(remaining, "");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].arguments, serde_json::json!({"path": "b"}));
        Ok(())
    }

    #[test]
    fn test_parse_alternative_bare_and_fenced_json() -> TestResult {
        let bare = r#"{"name": "fs.read", "arguments": "{\"path\": \"x\"}"}"#;
        let (_, calls) = parse_alternative_tool_calls(bare, &["fs.read"])
            .ok_or(TestError::Missing("must parse bare JSON call"))?;
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "x"}));

        let fenced = "<think>ok</think>```json\n{\"name\": \"fs.list\", \"arguments\": {\"path\": \".\"}}\n```";
        let (_, calls) = parse_alternative_tool_calls(fenced, &["fs.list"])
            .ok_or(TestError::Missing("must parse fenced JSON call"))?;
        assert_eq!(calls[0].name, "fs.list");
        Ok(())
    }

    #[test]
    fn test_parse_alternative_is_fail_closed() {
        let offered = ["fs.read"];
        // Unbekannter Werkzeugname.
        assert_eq!(
            parse_alternative_tool_calls(
                r#"{"name":"shell.exec","arguments":{"cmd":"x"}}"#,
                &offered
            ),
            None
        );
        // JSON mitten im Text ist kein Aufruf.
        assert_eq!(
            parse_alternative_tool_calls(
                r#"Beispiel: {"name":"fs.read","arguments":{"path":"a"}} fertig."#,
                &offered
            ),
            None
        );
        // Llama-Python-Code ist kein JSON.
        assert_eq!(
            parse_alternative_tool_calls("<|python_tag|>fs.read(path=\"a\")", &offered),
            None
        );
        // Ein schlechtes Mistral-Segment kippt alle.
        assert_eq!(
            parse_alternative_tool_calls(
                "[TOOL_CALLS]fs.read[ARGS]{\"path\":\"a\"}[TOOL_CALLS]shell.exec[ARGS]{}",
                &offered
            ),
            None
        );
        // Argumente müssen ein Objekt sein.
        assert_eq!(
            parse_alternative_tool_calls(r#"{"name":"fs.read","arguments":[1,2]}"#, &offered),
            None
        );
        // Normale Textantwort.
        assert_eq!(
            parse_alternative_tool_calls("Alles erledigt.", &offered),
            None
        );
    }

    #[test]
    fn test_strip_leading_think_removes_paired_block_and_returns_text() {
        let content = "<think>reasoning here</think>The answer is 42.";
        let (remaining, think) = strip_leading_think(content);
        assert_eq!(remaining, "The answer is 42.");
        assert_eq!(think.as_deref(), Some("reasoning here"));
    }

    #[test]
    fn test_strip_leading_think_removes_stray_leading_close_tag() {
        let content = "</think>The answer is 42.";
        let (remaining, think) = strip_leading_think(content);
        assert_eq!(remaining, "The answer is 42.");
        assert_eq!(think, None);
    }

    #[test]
    fn test_strip_leading_think_leaves_untagged_content_untouched() {
        let content = "no think tags at all";
        let (remaining, think) = strip_leading_think(content);
        assert_eq!(remaining, "no think tags at all");
        assert_eq!(think, None);
    }

    #[test]
    fn test_strip_leading_think_leaves_unterminated_block_untouched() {
        let content = "<think>never closed";
        let (remaining, think) = strip_leading_think(content);
        assert_eq!(remaining, "<think>never closed");
        assert_eq!(think, None);
    }
}

//! `fsread.json` — `jq`-artiges Auslesen eines JSON-Pfads.
//!
//! Kein Filter-Interpreter: unterstützt wird ein **Pfadausdruck** der Form
//! `.a.b[0]["schlüssel mit leerzeichen"].c` (auch ohne führenden Punkt;
//! negative Indizes zählen vom Ende) und eine von vier Operationen: `get`
//! (Standard), `keys`, `length`, `type`. Ein fehlender Pfad ergibt
//! `found: false` (kein Fehler).
//!
//! # Grenzen
//! Datei höchstens 8 MiB, Pfadausdruck höchstens 512 Zeichen / 64 Segmente,
//! Wert in der Antwort höchstens 32 KiB; größere Werte liefern Art, Länge,
//! Schlüsselliste (bis 200) und eine 2-KiB-Vorschau, `truncated: true`.

use crate::blocking::{run_blocking, scoped, workspace_root};
use crate::budget::{choice, clip_text, ok};
use crate::io::{MAX_READ_BYTES, looks_binary, open_text, read_prefix};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// Name des Werkzeugs.
pub const TOOL: &str = "fsread.json";

/// Höchstlänge des Pfadausdrucks.
pub const MAX_QUERY_BYTES: usize = 512;

/// Höchstzahl Segmente.
pub const MAX_SEGMENTS: usize = 64;

/// Größter Wert, der vollständig zurückgegeben wird.
pub const MAX_VALUE_BYTES: usize = 32 * 1024;

/// Argumente für `fsread.json`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct JsonArgs {
    /// JSON file relative to the workspace root.
    pub path: String,
    /// Path expression such as '.a.b[0]', '.["key with space"]' or '.' for the whole document (default '.').
    #[serde(default)]
    pub query: Option<String>,
    /// Operation on the selected value: 'get' (default), 'keys', 'length' or 'type'.
    #[serde(default)]
    pub op: Option<String>,
}

/// Ein Pfadsegment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// Objektschlüssel.
    Key(String),
    /// Arrayindex (negativ = vom Ende).
    Index(i64),
}

/// Parst einen Pfadausdruck.
///
/// # Errors
/// Meldung mit Position bei ungültiger Syntax.
pub fn parse_query(query: &str) -> Result<Vec<Seg>, String> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(format!("query longer than {MAX_QUERY_BYTES} bytes"));
    }
    let chars: Vec<char> = query.trim().chars().collect();
    let mut segs = Vec::new();
    let mut i = 0usize;
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    while i < chars.len() {
        if segs.len() >= MAX_SEGMENTS {
            return Err(format!("query has more than {MAX_SEGMENTS} segments"));
        }
        match chars[i] {
            '.' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                if chars[i] == '"' {
                    let (key, next) = quoted(&chars, i)?;
                    segs.push(Seg::Key(key));
                    i = next;
                } else if chars[i] == '[' {
                    // `.[0]` — der Punkt gehört zur Klammer.
                } else {
                    let start = i;
                    while i < chars.len() && ident(chars[i]) {
                        i += 1;
                    }
                    if start == i {
                        return Err(format!("unexpected '{}' at position {i}", chars[i]));
                    }
                    segs.push(Seg::Key(chars[start..i].iter().collect()));
                }
            }
            '[' => {
                i += 1;
                if chars.get(i) == Some(&'"') {
                    let (key, next) = quoted(&chars, i)?;
                    i = next;
                    if chars.get(i) != Some(&']') {
                        return Err(format!("expected ']' at position {i}"));
                    }
                    i += 1;
                    segs.push(Seg::Key(key));
                } else {
                    let start = i;
                    if chars.get(i) == Some(&'-') {
                        i += 1;
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let digits: String = chars[start..i].iter().collect();
                    let index: i64 = digits.parse().map_err(|_| {
                        format!("invalid array index '{digits}' at position {start}")
                    })?;
                    if chars.get(i) != Some(&']') {
                        return Err(format!("expected ']' at position {i}"));
                    }
                    i += 1;
                    segs.push(Seg::Index(index));
                }
            }
            c if ident(c) && segs.is_empty() && i == 0 => {
                let start = i;
                while i < chars.len() && ident(chars[i]) {
                    i += 1;
                }
                segs.push(Seg::Key(chars[start..i].iter().collect()));
            }
            other => return Err(format!("unexpected '{other}' at position {i}")),
        }
    }
    Ok(segs)
}

/// Liest ein `"…"`-Segment ab `start` (auf dem öffnenden Anführungszeichen).
fn quoted(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '"' => return Ok((out, i + 1)),
            '\\' => {
                i += 1;
                match chars.get(i) {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    _ => return Err(format!("unsupported escape at position {i}")),
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Err("unterminated string in query".to_owned())
}

/// Wendet die Segmente auf `value` an.
#[must_use]
pub fn select<'a>(value: &'a Value, segs: &[Seg]) -> Option<&'a Value> {
    let mut current = value;
    for seg in segs {
        current = match (seg, current) {
            (Seg::Key(key), Value::Object(map)) => map.get(key)?,
            (Seg::Index(index), Value::Array(items)) => {
                let len = i64::try_from(items.len()).ok()?;
                let position = if *index < 0 { len + *index } else { *index };
                items.get(usize::try_from(position).ok()?)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn length_of(value: &Value) -> Option<usize> {
    match value {
        Value::Array(items) => Some(items.len()),
        Value::Object(map) => Some(map.len()),
        Value::String(text) => Some(text.chars().count()),
        _ => None,
    }
}

fn keys_of(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            Some(keys)
        }
        Value::Array(items) => Some((0..items.len()).map(|i| i.to_string()).collect()),
        _ => None,
    }
}

/// Führt `fsread.json` aus.
#[must_use]
pub fn run(root: &Path, args: &JsonArgs) -> ToolOutput {
    scoped(TOOL, root, |scope| {
        let op = choice(
            "op",
            args.op.as_deref(),
            &["get", "keys", "length", "type"],
            "get",
        )?;
        let query = args.query.as_deref().unwrap_or(".");
        let segs = parse_query(query)?;
        let (rel, mut file) = open_text(scope, &args.path)?;
        let shown = rel.display();
        let (bytes, more) = read_prefix(&mut file, MAX_READ_BYTES).map_err(|e| e.to_string())?;
        if more {
            return Err(format!("'{shown}' is larger than {MAX_READ_BYTES} bytes"));
        }
        if looks_binary(&bytes) {
            return Err(format!("'{shown}' looks like a binary file"));
        }
        let document: Value = serde_json::from_slice(&bytes).map_err(|e| {
            format!(
                "'{shown}' is not valid JSON: line {}, column {}: {}",
                e.line(),
                e.column(),
                e.classify_label()
            )
        })?;
        let Some(selected) = select(&document, &segs) else {
            return Ok(ok(
                TOOL,
                format!("{query}: not found"),
                json!({"path": shown, "query": query, "found": false, "value": null, "truncated": false}),
            ));
        };
        let mut data = json!({
            "path": shown, "query": query, "found": true,
            "type": type_name(selected), "truncated": false,
        });
        match op {
            "type" => data["value"] = json!(type_name(selected)),
            "length" => {
                let length = length_of(selected)
                    .ok_or_else(|| format!("{} has no length", type_name(selected)))?;
                data["value"] = json!(length);
            }
            "keys" => {
                let keys = keys_of(selected)
                    .ok_or_else(|| format!("{} has no keys", type_name(selected)))?;
                let total = keys.len();
                let shown_keys: Vec<String> = keys.into_iter().take(200).collect();
                data["truncated"] = json!(total > shown_keys.len());
                data["length"] = json!(total);
                data["value"] = json!(shown_keys);
            }
            _ => {
                let rendered = serde_json::to_string(selected).map_err(|e| e.to_string())?;
                if rendered.len() <= MAX_VALUE_BYTES {
                    data["value"] = selected.clone();
                } else {
                    let (preview, _) = clip_text(&rendered, 2048);
                    data["truncated"] = json!(true);
                    data["value"] = Value::Null;
                    data["preview"] = json!(preview);
                    data["serialized_bytes"] = json!(rendered.len());
                    if let Some(length) = length_of(selected) {
                        data["length"] = json!(length);
                    }
                    if let Some(keys) = keys_of(selected) {
                        data["keys"] = json!(keys.into_iter().take(200).collect::<Vec<_>>());
                    }
                }
            }
        }
        Ok(ok(TOOL, format!("{query}: {}", type_name(selected)), data))
    })
}

trait ClassifyLabel {
    fn classify_label(&self) -> &'static str;
}

impl ClassifyLabel for serde_json::Error {
    fn classify_label(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "I/O error",
            serde_json::error::Category::Syntax => "syntax error",
            serde_json::error::Category::Data => "data error",
            serde_json::error::Category::Eof => "unexpected end of input",
        }
    }
}

/// Liest einen Pfad aus einer JSON-Datei, `jq`-artig.
#[harw_macros::tool(
    name = "fsread.json",
    description = "Reads one value out of a workspace JSON file by path expression like jq's path access: '.a.b[0]', '.[\"key with space\"]', negative indexes, plus op=keys|length|type. Use when you need a single field of a large JSON file (package metadata, reports, configs) instead of running jq or reading the file. Returns JSON {found, type, value}; a missing path gives found=false. No filter language. File up to 8 MiB, value capped at 32 KiB with a preview beyond that.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fsread_json(
    context: &ToolExecutionContext,
    args: JsonArgs,
) -> Result<ToolOutput, ToolsError> {
    let root = workspace_root(context);
    run_blocking(TOOL, move || run(&root, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, TestError, TestResult, error_of, json_of};

    fn pq(query: &str) -> TestResult<Vec<Seg>> {
        parse_query(query).map_err(TestError::Unexpected)
    }

    fn q(fx: &Fixture, query: &str, op: Option<&str>) -> TestResult<Value> {
        json_of(run(
            &fx.ws,
            &JsonArgs {
                path: "d.json".to_owned(),
                query: Some(query.to_owned()),
                op: op.map(str::to_owned),
            },
        ))
    }

    fn doc() -> TestResult<Fixture> {
        let fx = Fixture::new()?;
        fx.write(
            "d.json",
            br#"{"name":"x","deps":{"a":"1","b":"2"},"list":[10,20,{"k":"v"}],"key with space":true,"n":null}"#,
        )?;
        Ok(fx)
    }

    #[test]
    fn parses_expressions() -> TestResult {
        assert_eq!(pq(".")?, vec![]);
        assert_eq!(pq("")?, vec![]);
        assert_eq!(
            pq(".a.b[0]")?,
            vec![Seg::Key("a".into()), Seg::Key("b".into()), Seg::Index(0)]
        );
        assert_eq!(pq("a.b")?, vec![Seg::Key("a".into()), Seg::Key("b".into())]);
        assert_eq!(
            pq(r#".["k v"][-1]"#)?,
            vec![Seg::Key("k v".into()), Seg::Index(-1)]
        );
        assert_eq!(
            pq(r#"."a b".c"#)?,
            vec![Seg::Key("a b".into()), Seg::Key("c".into())]
        );
        assert_eq!(pq(".[2].x")?, vec![Seg::Index(2), Seg::Key("x".into())]);
        for bad in [
            ".a..b",
            ".a[",
            ".a[x]",
            "[\"unterminated",
            ".a b",
            "a[1",
            ".a.[",
            ".!",
        ] {
            assert!(parse_query(bad).is_err(), "{bad} must fail");
        }
        assert!(parse_query(&"a.".repeat(MAX_SEGMENTS + 1)).is_err());
        assert!(parse_query(&"a".repeat(MAX_QUERY_BYTES + 1)).is_err());
        Ok(())
    }

    #[test]
    fn selects_values() -> TestResult {
        let fx = doc()?;
        assert_eq!(q(&fx, ".name", None)?["value"], "x");
        assert_eq!(q(&fx, ".deps.b", None)?["value"], "2");
        assert_eq!(q(&fx, ".list[1]", None)?["value"], 20);
        assert_eq!(q(&fx, ".list[-1].k", None)?["value"], "v");
        assert_eq!(q(&fx, r#".["key with space"]"#, None)?["value"], true);
        assert_eq!(q(&fx, ".n", None)?["found"], true);
        let missing = q(&fx, ".nope.x", None)?;
        assert_eq!(missing["found"], false);
        assert_eq!(q(&fx, ".list[9]", None)?["found"], false);
        assert_eq!(q(&fx, ".name.x", None)?["found"], false);
        Ok(())
    }

    #[test]
    fn operations() -> TestResult {
        let fx = doc()?;
        assert_eq!(q(&fx, ".deps", Some("keys"))?["value"], json!(["a", "b"]));
        assert_eq!(q(&fx, ".list", Some("length"))?["value"], 3);
        assert_eq!(q(&fx, ".list", Some("type"))?["value"], "array");
        assert_eq!(
            q(&fx, ".list", Some("keys"))?["value"],
            json!(["0", "1", "2"])
        );
        let bad = json_of(run(
            &fx.ws,
            &JsonArgs {
                path: "d.json".into(),
                query: Some(".name".into()),
                op: Some("keys".into()),
            },
        ));
        assert!(bad.is_err());
        Ok(())
    }

    #[test]
    fn large_values_get_a_preview() -> TestResult {
        let fx = Fixture::new()?;
        let big: Vec<u32> = (0..20_000).collect();
        fx.write(
            "d.json",
            serde_json::to_string(&json!({"big": big}))?.as_bytes(),
        )?;
        let value = q(&fx, ".big", None)?;
        assert_eq!(value["truncated"], true);
        assert!(value["value"].is_null());
        assert_eq!(value["length"], 20_000);
        assert!(value["preview"].as_str().is_some_and(|p| p.len() <= 2048));
        let small = q(&fx, ".big[3]", None)?;
        assert_eq!(small["value"], 3);
        Ok(())
    }

    #[test]
    fn invalid_json_binary_secret_escape_and_size() -> TestResult {
        let fx = Fixture::new()?;
        fx.plant_escapes()?;
        fx.write("bad.json", b"{not json")?;
        fx.write("bin.json", &[0, 1, 2])?;
        fx.write(".env", b"{}")?;
        fx.write("huge.json", &vec![b' '; 9 * 1024 * 1024])?;
        for path in [
            "bad.json",
            "bin.json",
            ".env",
            "huge.json",
            "link_file",
            "../outside/secret.txt",
            "missing.json",
        ] {
            let output = run(
                &fx.ws,
                &JsonArgs {
                    path: path.into(),
                    query: None,
                    op: None,
                },
            );
            error_of(output).map_err(|e| TestError::Unexpected(format!("{path}: {e}")))?;
        }
        Ok(())
    }
}

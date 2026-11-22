//! Parses the tool calls Qwen models write as `<tool_call>` markup.
//!
//! Two dialects are read: the XML form of the Qwen3.5 template
//! (`<function=NAME><parameter=KEY>VALUE</parameter></function>`) and the
//! Hermes JSON form (`{"name": ..., "arguments": {...}}`).

use serde_json::{Map, Value};

const TOOL_OPEN: &str = "<tool_call>";
const TOOL_CLOSE: &str = "</tool_call>";
const FUNCTION_OPEN: &str = "<function=";
const FUNCTION_CLOSE: &str = "</function>";
const PARAM_OPEN: &str = "<parameter=";
const PARAM_CLOSE: &str = "</parameter>";

/// One call the model asked for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedCall {
    pub name: String,
    pub arguments: Value,
}

/// Parses every `<tool_call>` block in `raw`. `tools` is the OpenAI-style tool
/// list given to the template; it tells which parameters are strings.
pub(crate) fn parse_tool_calls(raw: &str, tools: &[Value]) -> Vec<ParsedCall> {
    let mut calls = Vec::new();
    for segment in raw.split(TOOL_OPEN).skip(1) {
        let body = segment.split(TOOL_CLOSE).next().unwrap_or(segment).trim();
        let parsed = if body.starts_with('{') {
            parse_json_call(body)
        } else {
            parse_xml_call(body, tools)
        };
        if let Some(call) = parsed {
            calls.push(call);
        }
    }
    calls
}

fn parse_json_call(body: &str) -> Option<ParsedCall> {
    let value: Value = serde_json::from_str(body).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let arguments = match value.get("arguments").or_else(|| value.get("parameters")) {
        Some(Value::String(text)) => serde_json::from_str(text).ok()?,
        Some(other) => other.clone(),
        None => Value::Object(Map::new()),
    };
    Some(ParsedCall { name, arguments })
}

fn parse_xml_call(body: &str, tools: &[Value]) -> Option<ParsedCall> {
    let start = body.find(FUNCTION_OPEN)? + FUNCTION_OPEN.len();
    let rest = &body[start..];
    let name_end = rest.find('>')?;
    let name = rest[..name_end].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let after = &rest[name_end + 1..];
    let func_body = after.split(FUNCTION_CLOSE).next().unwrap_or(after);
    let mut args = Map::new();
    let mut cursor = func_body;
    while let Some(i) = cursor.find(PARAM_OPEN) {
        let rest = &cursor[i + PARAM_OPEN.len()..];
        let Some(key_end) = rest.find('>') else { break };
        let key = rest[..key_end].trim().to_owned();
        let value_part = &rest[key_end + 1..];
        let (raw_value, next) = match value_part.find(PARAM_CLOSE) {
            Some(j) => (&value_part[..j], &value_part[j + PARAM_CLOSE.len()..]),
            None => (value_part, ""),
        };
        if !key.is_empty() {
            args.insert(key.clone(), parse_value(raw_value, is_string_param(tools, &name, &key)));
        }
        cursor = next;
    }
    Some(ParsedCall {
        name,
        arguments: Value::Object(args),
    })
}

fn parse_value(raw: &str, as_string: bool) -> Value {
    let text = raw.strip_prefix('\n').unwrap_or(raw);
    let text = text.strip_suffix('\n').unwrap_or(text);
    if as_string {
        return Value::String(text.to_owned());
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|_| Value::String(text.to_owned()))
}

fn is_string_param(tools: &[Value], tool: &str, key: &str) -> bool {
    tools
        .iter()
        .filter_map(|t| t.get("function"))
        .find(|f| f.get("name").and_then(Value::as_str) == Some(tool))
        .and_then(|f| f.get("parameters")?.get("properties")?.get(key)?.get("type"))
        .and_then(Value::as_str)
        == Some("string")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_xml_dialect_with_typed_values() {
        let tools = vec![json!({"type":"function","function":{"name":"fs.read",
            "parameters":{"properties":{"path":{"type":"string"},"limit":{"type":"integer"}}}}})];
        let raw = "<tool_call>\n<function=fs.read>\n<parameter=path>\n123\n</parameter>\n\
                   <parameter=limit>\n10\n</parameter>\n</function>\n</tool_call>";
        let calls = parse_tool_calls(raw, &tools);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fs.read");
        assert_eq!(calls[0].arguments, json!({"path":"123","limit":10}));
    }

    #[test]
    fn parses_json_dialect() {
        let raw = "<tool_call>\n{\"name\":\"a\",\"arguments\":{\"x\":1}}\n</tool_call>";
        let calls = parse_tool_calls(raw, &[]);
        assert_eq!(calls, vec![ParsedCall { name: "a".into(), arguments: json!({"x":1}) }]);
    }
}

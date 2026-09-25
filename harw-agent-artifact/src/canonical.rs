//! Canonical JSON: the byte form of the artifact header.
//!
//! Rules (the whole definition; nothing else is canonicalized):
//! - Object keys are sorted by their UTF-8 bytes, ascending. The order does
//!   not depend on how the `serde_json::Value` was built or on whether some
//!   crate in the build enabled `serde_json/preserve_order`.
//! - No whitespace outside strings.
//! - Strings escape exactly `"`, `\`, and the control characters
//!   U+0000..=U+001F: `\b \f \n \r \t` by name, the rest as `\u00xx` with
//!   lowercase hex. Everything else, including non-ASCII and `/`, is written
//!   as raw UTF-8.
//! - Numbers are written as `serde_json::Number` displays them (integers
//!   plainly, floats in the shortest round-tripping form).
//!
//! This matches what `serde_json::to_vec` writes for a `Value` whose maps
//! are sorted, so `canonical_json(&serde_json::from_slice(c)?) == c` for
//! every canonical `c`. The reader relies on that to reject non-canonical
//! headers.

use serde_json::Value;

/// Serializes `value` as canonical JSON (sorted keys, no insignificant
/// whitespace). See the module documentation for the exact rules.
///
/// # Examples
/// ```
/// # use harw_agent_artifact::canonical_json;
/// let value = serde_json::json!({"b": 1, "a": [true, null, "x"]});
/// assert_eq!(canonical_json(&value), br#"{"a":[true,null,"x"],"b":1}"#);
/// ```
#[must_use]
pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, value);
    out
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => write_string(out, text),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_value(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_string(out, key);
                out.push(b':');
                write_value(out, item);
            }
            out.push(b'}');
        }
    }
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    for byte in text.bytes() {
        match byte {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x00..=0x1F => {
                out.extend_from_slice(b"\\u00");
                out.push(HEX[usize::from(byte >> 4)]);
                out.push(HEX[usize::from(byte & 0x0F)]);
            }
            // Multi-byte UTF-8 sequences only contain bytes >= 0x80, so
            // copying byte by byte keeps them intact.
            other => out.push(other),
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::canonical_json;
    use crate::test_support::{TestResult, ctx};
    use serde_json::json;

    #[test]
    fn test_canonical_json_sorts_keys_recursively() {
        let value = json!({"zeta": {"b": 2, "a": 1}, "alpha": [ {"y": 0, "x": 0} ], "Mid": null});
        assert_eq!(
            canonical_json(&value),
            br#"{"Mid":null,"alpha":[{"x":0,"y":0}],"zeta":{"a":1,"b":2}}"#
        );
    }

    #[test]
    fn test_canonical_json_ignores_insertion_order() {
        let mut first = serde_json::Map::new();
        first.insert("b".into(), json!(1));
        first.insert("a".into(), json!(2));
        let mut second = serde_json::Map::new();
        second.insert("a".into(), json!(2));
        second.insert("b".into(), json!(1));
        assert_eq!(
            canonical_json(&first.into()),
            canonical_json(&second.into())
        );
    }

    #[test]
    fn test_canonical_json_escapes_strings() {
        let value = json!("q\"b\\n\n\u{1}é/");
        assert_eq!(
            canonical_json(&value),
            "\"q\\\"b\\\\n\\n\\u0001é/\"".as_bytes()
        );
    }

    #[test]
    fn test_canonical_json_is_a_fixed_point_of_parse() -> TestResult {
        let value = json!({
            "name": "agent", "n": -12, "f": 1.5, "big": 18446744073709551615u64,
            "nested": {"list": [1, "two", false, null, {"k": "\u{7f}\t"}]}
        });
        let bytes = canonical_json(&value);
        let reparsed: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(ctx("canonical output parses"))?;
        assert_eq!(canonical_json(&reparsed), bytes);
        Ok(())
    }

    #[test]
    fn test_canonical_json_has_no_whitespace() {
        let value = json!({"a": [1, 2], "b": {"c": "d e"}});
        let bytes = canonical_json(&value);
        assert_eq!(bytes, br#"{"a":[1,2],"b":{"c":"d e"}}"#);
    }
}

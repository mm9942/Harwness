//! Return validators of the Agent IR v2 (`[return] validators`, #22 wave 1B).
//!
//! A child's IR may list cheap structural checks for its final answer. The
//! vocabulary is closed ([`harw_agent_dsl::ir_v2::ReturnValidator`]):
//! lowering rejects an unknown label with `HARW-RETURN-002`, and this module
//! runs every listed validator, in declaration order, on the **unabridged**
//! final answer before the return contract is evaluated.
//!
//! | Label | Check |
//! |---|---|
//! | `non-empty` | at least one non-whitespace character |
//! | `json` | valid JSON (a single surrounding ```` ``` ```` fence is tolerated) |
//! | `json-object` | like `json`, and the top-level value is an object |
//!
//! # Fail-closed
//! A label outside the vocabulary (only reachable through an IR that did not
//! pass `lower_v2`, e.g. a hand-built legacy IR) is a violation, never a
//! skipped check.
//!
//! # Content-free messages
//! A violation never quotes the child's answer: the answer may carry text an
//! attacker controls (see the security-verdict arm in `agent_tool.rs`).
//!
//! # Concurrency
//! Pure functions.

use harw_agent_dsl::ir_v2::ReturnValidator;
use serde_json::Value;

/// A failed return validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorViolation {
    /// The validator label as the IR names it.
    pub validator: String,
    /// Content-free reason.
    pub message: String,
}

impl ValidatorViolation {
    /// Model-visible form (structured, so the parent can react).
    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "error": self.message,
            "validator": self.validator,
        })
    }
}

impl std::fmt::Display for ValidatorViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Rückgabe-Validator '{}' verletzt: {}",
            self.validator, self.message
        )
    }
}

/// Runs `validators` (declaration order) on `text`.
///
/// # Errors
/// The first [`ValidatorViolation`]: a failed check or an unknown label.
pub fn run_return_validators(validators: &[String], text: &str) -> Result<(), ValidatorViolation> {
    for label in validators {
        let violation = |message: String| ValidatorViolation {
            validator: label.clone(),
            message,
        };
        let Some(validator) = ReturnValidator::parse(label) else {
            return Err(violation(format!(
                "unbekannter Validator (bekannt: {})",
                ReturnValidator::known_labels()
            )));
        };
        check(validator, text).map_err(violation)?;
    }
    Ok(())
}

/// One validator on one answer.
fn check(validator: ReturnValidator, text: &str) -> Result<(), String> {
    match validator {
        ReturnValidator::NonEmpty => {
            if text.trim().is_empty() {
                Err("die Kind-Antwort ist leer".to_owned())
            } else {
                Ok(())
            }
        }
        ReturnValidator::Json => parse_json(text).map(|_| ()),
        ReturnValidator::JsonObject => match parse_json(text)? {
            Value::Object(_) => Ok(()),
            other => Err(format!(
                "die Kind-Antwort ist JSON, aber kein Objekt (sondern {})",
                json_kind(&other)
            )),
        },
    }
}

/// Parses the answer as JSON, tolerating one surrounding code fence. The
/// parser error names line and column only, never the content.
fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(strip_fence(text.trim())).map_err(|error| {
        format!(
            "die Kind-Antwort ist kein gültiges JSON (Zeile {}, Spalte {})",
            error.line(),
            error.column()
        )
    })
}

/// Removes one surrounding ```` ```lang … ``` ```` fence.
fn strip_fence(text: &str) -> &str {
    let Some(inner) = text
        .strip_prefix("```")
        .and_then(|rest| rest.strip_suffix("```"))
    else {
        return text;
    };
    match inner.split_once('\n') {
        Some((info, body)) if !info.trim_start().starts_with(|c: char| c == '{' || c == '[') => {
            body.trim()
        }
        _ => inner.trim(),
    }
}

/// A short name of the JSON value kind.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "ein Wahrheitswert",
        Value::Number(_) => "eine Zahl",
        Value::String(_) => "eine Zeichenkette",
        Value::Array(_) => "ein Array",
        Value::Object(_) => "ein Objekt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn test_no_validators_accept_anything() {
        assert_eq!(run_return_validators(&[], ""), Ok(()));
    }

    #[test]
    fn test_non_empty_rejects_whitespace() {
        let validators = labels(&["non-empty"]);
        assert_eq!(run_return_validators(&validators, "Befund"), Ok(()));
        let violation = run_return_validators(&validators, " \n\t");
        assert!(violation.is_err_and(|violation| violation.validator == "non-empty"));
    }

    #[test]
    fn test_json_and_json_object() {
        let json = labels(&["json"]);
        assert_eq!(run_return_validators(&json, "[1, 2]"), Ok(()));
        assert_eq!(run_return_validators(&json, "```json\n{\"a\": 1}\n```"), Ok(()));
        assert!(run_return_validators(&json, "kein json").is_err());

        let object = labels(&["json-object"]);
        assert_eq!(run_return_validators(&object, "{\"a\": 1}"), Ok(()));
        let violation = run_return_validators(&object, "[1, 2]");
        assert!(violation.is_err_and(|violation| violation.message.contains("Array")));
    }

    #[test]
    fn test_validators_run_in_order_and_stop_at_the_first_violation() {
        let validators = labels(&["non-empty", "json-object"]);
        let violation = run_return_validators(&validators, "");
        assert!(violation.is_err_and(|violation| violation.validator == "non-empty"));
    }

    #[test]
    fn test_unknown_validator_fails_closed() {
        let violation = run_return_validators(&labels(&["redact.secrets"]), "x");
        assert!(violation.is_err_and(|violation| violation.validator == "redact.secrets"
            && violation.message.contains("unbekannter Validator")));
    }

    #[test]
    fn test_violations_never_quote_the_answer() {
        let secret = "GEHEIM-1234 kein json";
        let violation = run_return_validators(&labels(&["json"]), secret);
        assert!(violation.is_err_and(|violation| !violation.message.contains("GEHEIM")
            && !violation.to_json().to_string().contains("GEHEIM")));
    }
}

//! Gemeinsames Parsen von Tool-Argumenten mit einheitlichem Fehlertext.

use serde::de::DeserializeOwned;

use crate::output::ToolOutput;

/// Fehlerausgabe `"<tool>: <detail>"`.
#[must_use]
pub fn tool_failure(tool: &str, detail: impl std::fmt::Display) -> ToolOutput {
    ToolOutput::error(format!("{tool}: {detail}"))
}

/// Fehlerausgabe `"<tool>: ungültige Argumente: <error>"`.
#[must_use]
pub fn invalid_arguments(tool: &str, error: impl std::fmt::Display) -> ToolOutput {
    tool_failure(tool, format_args!("ungültige Argumente: {error}"))
}

/// Deserialisiert die Argumente eines Tool-Aufrufs.
///
/// # Errors
/// `ToolOutput::Error` mit `"<tool>: ungültige Argumente: <error>"`.
pub fn parse_args<T: DeserializeOwned>(
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<T, ToolOutput> {
    serde_json::from_value(arguments.clone()).map_err(|error| invalid_arguments(tool, error))
}

/// Wie [`parse_args`]; ein ganzes `null` ergibt `T::default()`.
///
/// # Errors
/// Wie [`parse_args`].
pub fn parse_args_or_default<T: DeserializeOwned + Default>(
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<T, ToolOutput> {
    if arguments.is_null() {
        return Ok(T::default());
    }
    parse_args(tool, arguments)
}

/// Wie [`parse_args`]; ein ganzes `null` gilt als leeres Objekt `{}`.
///
/// # Errors
/// Wie [`parse_args`].
pub fn parse_args_null_as_object<T: DeserializeOwned>(
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<T, ToolOutput> {
    if arguments.is_null() {
        return parse_args(tool, &serde_json::Value::Object(serde_json::Map::new()));
    }
    parse_args(tool, arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize, Default, PartialEq)]
    struct A {
        #[serde(default)]
        x: Option<u8>,
    }

    #[derive(Debug, Deserialize)]
    struct B {
        #[allow(dead_code)]
        y: u8,
    }

    #[test]
    fn error_text_is_stable() {
        let err = parse_args::<B>("t.x", &json!({})).err();
        let expected = serde_json::from_value::<B>(json!({}))
            .err()
            .map(|e| ToolOutput::error(format!("t.x: ungültige Argumente: {e}")));
        assert!(err.is_some());
        assert_eq!(format!("{err:?}"), format!("{expected:?}"));
    }

    #[test]
    fn null_handling() {
        assert_eq!(
            parse_args_or_default::<A>("t", &serde_json::Value::Null).ok(),
            Some(A::default())
        );
        assert!(parse_args::<A>("t", &serde_json::Value::Null).is_err());
        assert_eq!(
            parse_args_null_as_object::<A>("t", &serde_json::Value::Null).ok(),
            Some(A::default())
        );
        assert_eq!(
            parse_args::<A>("t", &json!({"x": 3})).ok(),
            Some(A { x: Some(3) })
        );
    }
}

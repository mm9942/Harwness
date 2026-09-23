//! JSON-RPC-2.0-inspiriertes Envelope-Format für Transport über Channels
//! (stdio, WebSocket, IPC).

use serde::{Deserialize, Serialize, de::Deserializer};
use serde_json::Value;

const CURRENT_PROTOCOL_MAJOR: u32 = 1;

/// Versionierter Protokoll-Header — in jede Envelope einbettbar.
#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    /// Semantische Major-Version. Breaking Changes → Increment.
    pub major: u32,
    /// Minor-Version für rückwärtskompatible Erweiterungen.
    pub minor: u32,
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            major: u32,
            minor: u32,
        }

        let fields = Fields::deserialize(deserializer)?;
        if fields.major != CURRENT_PROTOCOL_MAJOR {
            return Err(serde::de::Error::custom(format!(
                "unsupported protocol major version {}; expected {}",
                fields.major, CURRENT_PROTOCOL_MAJOR
            )));
        }

        Ok(Self {
            major: fields.major,
            minor: fields.minor,
        })
    }
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self { major: 1, minor: 0 }
    }
}

/// Liefert die feste JSON-RPC-Version `"2.0"`.
const fn jsonrpc_version() -> &'static str {
    "2.0"
}

/// Ausgehende Anfrage (Client → Agent).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    /// Immer `"2.0"`.
    #[serde(deserialize_with = "deserialize_jsonrpc_string")]
    pub jsonrpc: String,
    /// Korrelations-ID.
    pub id: String,
    /// z.B. `"turn.submit"`.
    pub method: String,
    pub params: Value,
    pub protocol: ProtocolVersion,
}

/// Direkte Antwort auf eine `RequestEnvelope` (Agent → Client).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "ResponseEnvelopeFields")]
pub struct ResponseEnvelope {
    /// Immer `"2.0"`.
    pub jsonrpc: String,
    /// Gleiche ID wie der Request.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelopeFields {
    #[serde(deserialize_with = "deserialize_jsonrpc_string")]
    jsonrpc: String,
    id: String,
    #[serde(default, deserialize_with = "deserialize_present_result")]
    result: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_present_error")]
    error: Option<WireError>,
}

impl TryFrom<ResponseEnvelopeFields> for ResponseEnvelope {
    type Error = &'static str;

    fn try_from(fields: ResponseEnvelopeFields) -> Result<Self, Self::Error> {
        match (fields.result, fields.error) {
            (Some(result), None) => Ok(Self {
                jsonrpc: fields.jsonrpc,
                id: fields.id,
                result: Some(result),
                error: None,
            }),
            (None, Some(error)) => Ok(Self {
                jsonrpc: fields.jsonrpc,
                id: fields.id,
                result: None,
                error: Some(error),
            }),
            _ => Err("response must contain exactly one of result or error"),
        }
    }
}

/// Asynchrone Benachrichtigung (Agent → Client), kein Request nötig.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationEnvelope {
    /// Immer `"2.0"`.
    #[serde(deserialize_with = "deserialize_jsonrpc_string")]
    pub jsonrpc: String,
    /// z.B. `"event.turn"`.
    pub method: String,
    pub params: Value,
    /// Submission-ID der auslösenden Anfrage, wenn bekannt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Top-Level-Nachricht: eine der drei Envelope-Varianten.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireMessage {
    Request(RequestEnvelope),
    Response(ResponseEnvelope),
    Notification(NotificationEnvelope),
}

fn deserialize_jsonrpc_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let version = String::deserialize(deserializer)?;
    if version != jsonrpc_version() {
        return Err(serde::de::Error::custom(format!(
            "jsonrpc must equal {:?}",
            jsonrpc_version()
        )));
    }
    Ok(version)
}

fn deserialize_present_result<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

fn deserialize_present_error<'de, D>(deserializer: D) -> Result<Option<WireError>, D::Error>
where
    D: Deserializer<'de>,
{
    WireError::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn inbound_envelopes_require_jsonrpc_2_0() -> TestResult {
        let request = r#"{"id":"1","method":"turn.submit","params":{}}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(request).is_err());

        let wrong = r#"{"jsonrpc":"1.0","id":"1","method":"turn.submit","params":{}}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(wrong).is_err());

        let notification = r#"{"jsonrpc":"2.0","method":"event.turn","params":{}}"#;
        let parsed = serde_json::from_str::<NotificationEnvelope>(notification)
            .map_err(ctx("Notification-Envelope parsen"))?;
        assert_eq!(parsed.jsonrpc, "2.0");
        Ok(())
    }

    #[test]
    fn inbound_envelopes_deny_unknown_fields() {
        let request =
            r#"{"jsonrpc":"2.0","id":"1","method":"turn.submit","params":{},"extra":true}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(request).is_err());

        let response = r#"{"jsonrpc":"2.0","id":"1","result":{},"extra":true}"#;
        assert!(serde_json::from_str::<ResponseEnvelope>(response).is_err());
    }

    #[test]
    fn responses_require_exactly_one_result_or_error() {
        for payload in [
            r#"{"jsonrpc":"2.0","id":"1"}"#,
            r#"{"jsonrpc":"2.0","id":"1","error":null}"#,
            r#"{"jsonrpc":"2.0","id":"1","result":{},"error":{"code":-1,"message":"failed"}}"#,
        ] {
            assert!(serde_json::from_str::<ResponseEnvelope>(payload).is_err());
        }

        let null_result = r#"{"jsonrpc":"2.0","id":"1","result":null}"#;
        assert!(serde_json::from_str::<ResponseEnvelope>(null_result).is_ok());
    }

    #[test]
    fn inbound_requests_require_a_supported_protocol_version() -> TestResult {
        let without_protocol = r#"{"jsonrpc":"2.0","id":"1","method":"turn.submit","params":{}}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(without_protocol).is_err());

        let supported = r#"{"jsonrpc":"2.0","id":"1","method":"turn.submit","params":{},"protocol":{"major":1,"minor":7}}"#;
        let parsed = serde_json::from_str::<RequestEnvelope>(supported).map_err(ctx(
            "Request-Envelope mit unterstützter Protokollversion parsen",
        ))?;
        assert_eq!(parsed.protocol.major, 1);
        assert_eq!(parsed.protocol.minor, 7);

        let unsupported = r#"{"jsonrpc":"2.0","id":"1","method":"turn.submit","params":{},"protocol":{"major":2,"minor":0}}"#;
        assert!(serde_json::from_str::<RequestEnvelope>(unsupported).is_err());
        Ok(())
    }
}

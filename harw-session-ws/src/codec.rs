//! Wire codec of the session WebSocket (W00 §3.2).
//!
//! One UTF-8 JSON [`WireMessage`] per text message. Binary messages are
//! reserved and refused. Malformed input is an error value, never a panic:
//! the connection driver maps every [`CodecError`] to a stable
//! [`CloseReason`] or a JSON-RPC error response.

use bytes::Bytes;
use harw_protocol::methods::NOTIF_EVENT_FRAME;
use harw_protocol::{
    FrameEnvelope, NotificationEnvelope, RequestEnvelope, ResponseEnvelope, WireError, WireMessage,
};
use serde::Serialize;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::Utf8Bytes;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::limits::WsLimits;

/// JSON-RPC version every envelope carries.
const JSONRPC: &str = "2.0";

/// Stable close codes and reasons of the session WebSocket.
///
/// Standard codes where RFC 6455 has one (normal, message too big, internal
/// error); everything else sits in the application range 4000-4999. Codes
/// are part of the wire contract and never change meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// Orderly close by either side.
    Normal,
    /// No `session.hello` within the hello timeout.
    HelloTimeout,
    /// A method other than `session.hello` arrived first.
    HelloRequired,
    /// A message violated the wire protocol.
    ProtocolError,
    /// An inbound message exceeded the size limit.
    MessageTooBig,
    /// A binary message arrived; binary is reserved.
    BinaryRefused,
    /// The client's authority was revoked.
    Revoked,
    /// The host is draining and closes connections.
    Draining,
    /// No inbound traffic within the idle timeout.
    IdleTimeout,
    /// The client kept exceeding the in-flight request limit.
    TooManyInFlight,
    /// An internal host failure.
    Internal,
}

impl CloseReason {
    /// Every reason, for exhaustive checks.
    pub const ALL: [Self; 11] = [
        Self::Normal,
        Self::HelloTimeout,
        Self::HelloRequired,
        Self::ProtocolError,
        Self::MessageTooBig,
        Self::BinaryRefused,
        Self::Revoked,
        Self::Draining,
        Self::IdleTimeout,
        Self::TooManyInFlight,
        Self::Internal,
    ];

    /// Numeric WebSocket close code.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Normal => 1000,
            Self::MessageTooBig => 1009,
            Self::Internal => 1011,
            Self::HelloTimeout => 4000,
            Self::HelloRequired => 4001,
            Self::ProtocolError => 4002,
            Self::BinaryRefused => 4003,
            Self::Revoked => 4004,
            Self::Draining => 4005,
            Self::IdleTimeout => 4006,
            Self::TooManyInFlight => 4007,
        }
    }

    /// Short, stable close reason text (well below the 123-byte limit).
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::HelloTimeout => "hello_timeout",
            Self::HelloRequired => "hello_required",
            Self::ProtocolError => "protocol_error",
            Self::MessageTooBig => "message_too_big",
            Self::BinaryRefused => "binary_refused",
            Self::Revoked => "revoked",
            Self::Draining => "draining",
            Self::IdleTimeout => "idle_timeout",
            Self::TooManyInFlight => "too_many_in_flight",
            Self::Internal => "internal",
        }
    }

    /// Close frame carrying this code and reason.
    #[must_use]
    pub fn to_close_frame(self) -> CloseFrame {
        CloseFrame {
            code: CloseCode::from(self.code()),
            reason: Utf8Bytes::from_static(self.reason()),
        }
    }
}

/// A decoded inbound WebSocket message.
#[derive(Debug)]
pub enum Inbound {
    Request(RequestEnvelope),
    Response(ResponseEnvelope),
    Notification(NotificationEnvelope),
    /// Ping payload; the caller answers with a pong carrying it.
    Ping(Bytes),
    Pong,
    Close,
}

/// Why an inbound message could not be decoded or an outbound one encoded.
#[derive(Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A binary message arrived; binary is reserved in v1.
    Binary,
    /// Not a valid wire message (bad JSON, wrong shape, raw frame, wrong
    /// notification method) or not encodable.
    Malformed(String),
    /// The text message exceeded [`WsLimits::max_message_bytes`].
    TooBig,
}

impl CodecError {
    /// Close reason for a connection that received this error.
    #[must_use]
    pub fn close_reason(&self) -> CloseReason {
        match self {
            Self::Binary => CloseReason::BinaryRefused,
            Self::Malformed(_) => CloseReason::ProtocolError,
            Self::TooBig => CloseReason::MessageTooBig,
        }
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binary => f.write_str("binary messages are not accepted"),
            Self::Malformed(detail) => write!(f, "malformed wire message: {detail}"),
            Self::TooBig => f.write_str("message exceeds the size limit"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Decodes one inbound WebSocket message.
///
/// # Errors
/// [`CodecError::Binary`] for binary messages, [`CodecError::TooBig`] for
/// text above [`WsLimits::max_message_bytes`], [`CodecError::Malformed`] for
/// anything that is not exactly one [`WireMessage`] and for raw frames.
pub fn decode(message: Message, limits: &WsLimits) -> Result<Inbound, CodecError> {
    match message {
        Message::Text(text) => {
            if text.len() > limits.max_message_bytes {
                return Err(CodecError::TooBig);
            }
            let wire = serde_json::from_str::<WireMessage>(text.as_str())
                .map_err(|error| CodecError::Malformed(error.to_string()))?;
            Ok(match wire {
                WireMessage::Request(request) => Inbound::Request(request),
                WireMessage::Response(response) => Inbound::Response(response),
                WireMessage::Notification(notification) => Inbound::Notification(notification),
            })
        }
        Message::Binary(_) => Err(CodecError::Binary),
        Message::Ping(payload) => Ok(Inbound::Ping(payload)),
        Message::Pong(_) => Ok(Inbound::Pong),
        Message::Close(_) => Ok(Inbound::Close),
        Message::Frame(_) => Err(CodecError::Malformed("raw frame".to_owned())),
    }
}

fn encode_text<T: Serialize>(value: &T) -> Result<Message, CodecError> {
    serde_json::to_string(value)
        .map(Message::text)
        .map_err(|error| CodecError::Malformed(format!("encode: {error}")))
}

/// Encodes a response as one text message.
///
/// # Errors
/// [`CodecError::Malformed`] if the envelope cannot be serialized.
pub fn encode_response(response: &ResponseEnvelope) -> Result<Message, CodecError> {
    encode_text(response)
}

/// Encodes a request as one text message.
///
/// # Errors
/// [`CodecError::Malformed`] if the envelope cannot be serialized.
pub fn encode_request(request: &RequestEnvelope) -> Result<Message, CodecError> {
    encode_text(request)
}

/// Encodes a session frame as an `event.frame` notification.
///
/// # Errors
/// [`CodecError::Malformed`] if the frame cannot be serialized.
pub fn encode_frame(envelope: &FrameEnvelope) -> Result<Message, CodecError> {
    let params = serde_json::to_value(envelope)
        .map_err(|error| CodecError::Malformed(format!("encode: {error}")))?;
    encode_text(&NotificationEnvelope {
        jsonrpc: JSONRPC.to_owned(),
        method: NOTIF_EVENT_FRAME.to_owned(),
        params,
        correlation_id: None,
    })
}

/// Error response with only `error` set.
#[must_use]
pub fn error_response(id: String, code: i32, message: impl Into<String>) -> ResponseEnvelope {
    ResponseEnvelope {
        jsonrpc: JSONRPC.to_owned(),
        id,
        result: None,
        error: Some(WireError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

/// Success response with only `result` set.
#[must_use]
pub fn ok_response(id: String, result: serde_json::Value) -> ResponseEnvelope {
    ResponseEnvelope {
        jsonrpc: JSONRPC.to_owned(),
        id,
        result: Some(result),
        error: None,
    }
}

/// Extracts the frame of an `event.frame` notification.
///
/// # Errors
/// [`CodecError::Malformed`] for any other method or params that are not a
/// [`FrameEnvelope`].
pub fn decode_frame_notification(
    notification: &NotificationEnvelope,
) -> Result<FrameEnvelope, CodecError> {
    if notification.method != NOTIF_EVENT_FRAME {
        return Err(CodecError::Malformed(format!(
            "expected {NOTIF_EVENT_FRAME}, got {}",
            notification.method
        )));
    }
    serde_json::from_value(notification.params.clone())
        .map_err(|error| CodecError::Malformed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::{Cursor, SessionFrame};
    use harw_types::SessionId;
    use tokio_tungstenite::tungstenite::protocol::frame::Frame;

    type TestResult<T = ()> = Result<T, String>;

    fn ctx<T, E: std::fmt::Display>(result: Result<T, E>, context: &'static str) -> TestResult<T> {
        result.map_err(|error| format!("{context}: {error}"))
    }

    fn text_of(message: &Message) -> TestResult<&str> {
        match message {
            Message::Text(text) => Ok(text.as_str()),
            other => Err(format!("expected text message, got {other:?}")),
        }
    }

    #[test]
    fn text_request_decodes() -> TestResult {
        let json = r#"{"jsonrpc":"2.0","id":"1","method":"session.hello","params":{},"protocol":{"major":1,"minor":1}}"#;
        let inbound = ctx(
            decode(Message::text(json), &WsLimits::default()),
            "decode request",
        )?;
        match inbound {
            Inbound::Request(request) => {
                assert_eq!(request.id, "1");
                assert_eq!(request.method, "session.hello");
                Ok(())
            }
            other => Err(format!("expected request, got {other:?}")),
        }
    }

    #[test]
    fn binary_is_refused() {
        let result = decode(
            Message::Binary(Bytes::from_static(b"{}")),
            &WsLimits::default(),
        );
        assert!(matches!(result, Err(CodecError::Binary)));
        assert_eq!(
            CodecError::Binary.close_reason(),
            CloseReason::BinaryRefused
        );
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        let limits = WsLimits::default();
        for payload in [
            "",
            "not json",
            "{",
            "null",
            "42",
            "[]",
            r#""string""#,
            "{}",
            r#"{"jsonrpc":"2.0"}"#,
            r#"{"jsonrpc":"1.0","id":"1","method":"m","params":{},"protocol":{"major":1,"minor":0}}"#,
            r#"{"jsonrpc":"2.0","id":"1","method":"m","params":{},"protocol":{"major":2,"minor":0}}"#,
            r#"{"jsonrpc":"2.0","id":"1","method":"m","params":{},"protocol":{"major":1,"minor":0},"actor":"root"}"#,
            r#"{"jsonrpc":"2.0","id":"1"}"#,
            r#"{"jsonrpc":"2.0","id":"1","result":{},"error":{"code":1,"message":"x"}}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
        ] {
            let result = decode(Message::text(payload), &limits);
            assert!(
                matches!(result, Err(CodecError::Malformed(_))),
                "payload {payload:?} gave {result:?}"
            );
        }
        let raw = decode(Message::Frame(Frame::ping(Vec::new())), &limits);
        assert!(matches!(raw, Err(CodecError::Malformed(_))));
        assert_eq!(
            CodecError::Malformed(String::new()).close_reason(),
            CloseReason::ProtocolError
        );
    }

    #[test]
    fn oversized_text_is_too_big() {
        let limits = WsLimits {
            max_message_bytes: 16,
            ..WsLimits::default()
        };
        let payload = format!(
            r#"{{"jsonrpc":"2.0","method":"{}","params":{{}}}}"#,
            "m".repeat(64)
        );
        let result = decode(Message::text(payload), &limits);
        assert!(matches!(result, Err(CodecError::TooBig)));
        assert_eq!(
            CodecError::TooBig.close_reason(),
            CloseReason::MessageTooBig
        );
    }

    #[test]
    fn control_messages_decode() -> TestResult {
        let limits = WsLimits::default();
        match ctx(
            decode(Message::Ping(Bytes::from_static(b"p")), &limits),
            "ping",
        )? {
            Inbound::Ping(payload) => assert_eq!(&payload[..], b"p"),
            other => return Err(format!("expected ping, got {other:?}")),
        }
        assert!(matches!(
            decode(Message::Pong(Bytes::new()), &limits),
            Ok(Inbound::Pong)
        ));
        assert!(matches!(
            decode(Message::Close(None), &limits),
            Ok(Inbound::Close)
        ));
        Ok(())
    }

    #[test]
    fn frame_notification_round_trips() -> TestResult {
        let envelope = FrameEnvelope {
            session_id: ctx(SessionId::try_from_str("s-1"), "session id")?,
            cursor: Cursor {
                generation: 2,
                durable: 7,
                live: 3,
            },
            frame: SessionFrame::Lagged {
                resume_from: Cursor::start(2),
            },
        };
        let message = ctx(encode_frame(&envelope), "encode frame")?;
        let inbound = ctx(decode(message, &WsLimits::default()), "decode frame")?;
        let notification = match inbound {
            Inbound::Notification(notification) => notification,
            other => return Err(format!("expected notification, got {other:?}")),
        };
        assert_eq!(notification.method, NOTIF_EVENT_FRAME);
        assert_eq!(notification.correlation_id, None);
        let back = ctx(decode_frame_notification(&notification), "frame params")?;
        assert_eq!(back.cursor, envelope.cursor);
        assert!(matches!(
            back.frame,
            SessionFrame::Lagged { resume_from } if resume_from == Cursor::start(2)
        ));
        Ok(())
    }

    #[test]
    fn frame_notification_requires_event_frame_method() {
        let notification = NotificationEnvelope {
            jsonrpc: JSONRPC.to_owned(),
            method: "event.turn".to_owned(),
            params: serde_json::json!({}),
            correlation_id: None,
        };
        assert!(matches!(
            decode_frame_notification(&notification),
            Err(CodecError::Malformed(_))
        ));
    }

    #[test]
    fn close_codes_are_distinct_and_stable() {
        for (index, reason) in CloseReason::ALL.iter().enumerate() {
            for other in CloseReason::ALL.iter().skip(index + 1) {
                assert_ne!(reason.code(), other.code(), "{reason:?} vs {other:?}");
                assert_ne!(reason.reason(), other.reason(), "{reason:?} vs {other:?}");
            }
            let code = reason.code();
            assert!(
                matches!(code, 1000 | 1009 | 1011 | 4000..=4999),
                "{reason:?} has code {code}"
            );
            assert!(reason.reason().len() <= 123);
            let frame = reason.to_close_frame();
            assert_eq!(u16::from(frame.code), code);
            assert_eq!(frame.reason.as_str(), reason.reason());
        }
        assert_eq!(CloseReason::Normal.code(), 1000);
        assert_eq!(CloseReason::MessageTooBig.code(), 1009);
        assert_eq!(CloseReason::Internal.code(), 1011);
    }

    #[test]
    fn error_response_serializes_with_only_error() -> TestResult {
        let response = error_response("9".to_owned(), -32001, "hello required");
        let message = ctx(encode_response(&response), "encode response")?;
        let value: serde_json::Value = ctx(
            serde_json::from_str(text_of(&message)?),
            "parse response json",
        )?;
        let object = value.as_object().ok_or("response is not an object")?;
        assert!(object.contains_key("error"));
        assert!(!object.contains_key("result"));
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "9");
        assert_eq!(value["error"]["code"], -32001);
        assert_eq!(value["error"]["message"], "hello required");
        Ok(())
    }

    #[test]
    fn ok_response_and_request_round_trip() -> TestResult {
        let limits = WsLimits::default();
        let response = ok_response("2".to_owned(), serde_json::json!({"ok": true}));
        let message = ctx(encode_response(&response), "encode ok response")?;
        match ctx(decode(message, &limits), "decode ok response")? {
            Inbound::Response(back) => {
                assert_eq!(back.id, "2");
                assert!(back.error.is_none());
                assert_eq!(back.result, Some(serde_json::json!({"ok": true})));
            }
            other => return Err(format!("expected response, got {other:?}")),
        }

        let request = RequestEnvelope {
            jsonrpc: JSONRPC.to_owned(),
            id: "3".to_owned(),
            method: "session.list".to_owned(),
            params: serde_json::json!({}),
            protocol: harw_protocol::ProtocolVersion::default(),
        };
        let message = ctx(encode_request(&request), "encode request")?;
        match ctx(decode(message, &limits), "decode request")? {
            Inbound::Request(back) => {
                assert_eq!(back.id, "3");
                assert_eq!(back.method, "session.list");
            }
            other => return Err(format!("expected request, got {other:?}")),
        }
        Ok(())
    }
}

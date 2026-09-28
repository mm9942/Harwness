//! Closed method table (W00 D5): one wire request → one typed
//! [`SessionPort`] call.
//!
//! There is no generic operation execution here. Every method of
//! [`harw_protocol::methods::SESSION_METHODS`] maps to exactly one port call
//! with strictly decoded parameters; anything else is `METHOD_NOT_FOUND`.
//! Parameters that fail to decode never reach the port.

use harw_protocol::methods::{
    METHOD_APPROVAL_RESP, METHOD_SESSION_ATTACH, METHOD_SESSION_CLOSE, METHOD_SESSION_CREATE,
    METHOD_SESSION_DETACH, METHOD_SESSION_HELLO, METHOD_SESSION_HISTORY, METHOD_SESSION_LIST,
    METHOD_SESSION_RESUME, METHOD_SESSION_SET_EFFORT, METHOD_SESSION_SET_MODE,
    METHOD_SESSION_SET_MODEL, METHOD_TURN_INTERRUPT, METHOD_TURN_SUBMIT,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, HelloParams, HistoryParams, HistoryResult,
    InterruptParams, ListResult, SessionRef, SetEffortParams, SetModeParams, SetModelParams,
    SubmitParams, error_codes,
};
use harw_protocol::{FrameSource, PortError, RequestEnvelope, ResponseEnvelope, SessionPort};
use harw_types::SessionId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::codec::{error_response, ok_response};

/// Outcome of one dispatched request.
pub enum Dispatched {
    /// Plain response.
    Response(ResponseEnvelope),
    /// `session.attach` succeeded: send the response, then start pumping
    /// `source`.
    Attached {
        response: ResponseEnvelope,
        session_id: SessionId,
        source: Box<dyn FrameSource>,
    },
    /// `session.detach` succeeded: stop pumping `session_id` after sending
    /// the response.
    Detached {
        response: ResponseEnvelope,
        session_id: SessionId,
    },
}

/// True for the only method allowed before hello.
#[must_use]
pub fn is_hello(method: &str) -> bool {
    method == METHOD_SESSION_HELLO
}

/// Decode the params, call the port and encode the result.
///
/// Decode errors answer `INVALID_PARAMS` without calling the port, an
/// unencodable result answers `INTERNAL`, an unknown method answers
/// `METHOD_NOT_FOUND` and a port error answers with [`PortError::code`].
pub async fn dispatch(port: &dyn SessionPort, request: RequestEnvelope) -> Dispatched {
    let RequestEnvelope {
        id, method, params, ..
    } = request;
    let response = match method.as_str() {
        METHOD_SESSION_HELLO => match decode::<HelloParams>(&id, params) {
            Ok(params) => encode(id, port.hello(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_LIST => {
            if is_empty_params(&params) {
                encode(
                    id,
                    port.list().await.map(|sessions| ListResult { sessions }),
                )
            } else {
                error_response(
                    id,
                    error_codes::INVALID_PARAMS,
                    "session.list takes no params",
                )
            }
        }
        METHOD_SESSION_CREATE => match decode::<CreateParams>(&id, params) {
            Ok(params) => encode(id, port.create(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_ATTACH => return attach(port, id, params).await,
        METHOD_SESSION_DETACH => return detach(port, id, params).await,
        METHOD_SESSION_HISTORY => match decode::<HistoryParams>(&id, params) {
            Ok(params) => encode(
                id,
                port.history(params)
                    .await
                    .map(|frames| HistoryResult { frames }),
            ),
            Err(response) => *response,
        },
        METHOD_TURN_SUBMIT => match decode::<SubmitParams>(&id, params) {
            Ok(params) => encode(id, port.submit(params).await),
            Err(response) => *response,
        },
        METHOD_TURN_INTERRUPT => match decode::<InterruptParams>(&id, params) {
            Ok(params) => encode(id, port.interrupt(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_RESUME => match decode::<SessionRef>(&id, params) {
            Ok(target) => encode(id, port.resume(target.session_id).await),
            Err(response) => *response,
        },
        METHOD_SESSION_CLOSE => match decode::<SessionRef>(&id, params) {
            Ok(target) => encode(id, port.close(target.session_id).await),
            Err(response) => *response,
        },
        METHOD_APPROVAL_RESP => match decode::<ApprovalRespondParams>(&id, params) {
            Ok(params) => encode(id, port.respond(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_SET_MODEL => match decode::<SetModelParams>(&id, params) {
            Ok(params) => encode(id, port.set_model(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_SET_MODE => match decode::<SetModeParams>(&id, params) {
            Ok(params) => encode(id, port.set_mode(params).await),
            Err(response) => *response,
        },
        METHOD_SESSION_SET_EFFORT => match decode::<SetEffortParams>(&id, params) {
            Ok(params) => encode(id, port.set_effort(params).await),
            Err(response) => *response,
        },
        _ => error_response(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        ),
    };
    Dispatched::Response(response)
}

/// `session.attach`: on success the caller starts pumping the frame source.
async fn attach(port: &dyn SessionPort, id: String, params: Value) -> Dispatched {
    let params = match decode::<AttachParams>(&id, params) {
        Ok(params) => params,
        Err(response) => return Dispatched::Response(*response),
    };
    let session_id = params.session_id.clone();
    match port.attach(params).await {
        Ok((ack, source)) => match serde_json::to_value(&ack) {
            Ok(result) => Dispatched::Attached {
                response: ok_response(id, result),
                session_id,
                source,
            },
            // The source is dropped: an attachment the client never learned
            // about must not start streaming.
            Err(error) => Dispatched::Response(internal(id, &error)),
        },
        Err(error) => Dispatched::Response(port_error(id, &error)),
    }
}

/// `session.detach`: on success the caller stops pumping `session_id`.
async fn detach(port: &dyn SessionPort, id: String, params: Value) -> Dispatched {
    let target = match decode::<SessionRef>(&id, params) {
        Ok(target) => target,
        Err(response) => return Dispatched::Response(*response),
    };
    let session_id = target.session_id;
    match port.detach(session_id.clone()).await {
        Ok(()) => Dispatched::Detached {
            response: ok_response(id, Value::Null),
            session_id,
        },
        Err(error) => Dispatched::Response(port_error(id, &error)),
    }
}

/// Strictly decode method params; a failure is the finished error response.
fn decode<P: DeserializeOwned>(id: &str, params: Value) -> Result<P, Box<ResponseEnvelope>> {
    serde_json::from_value(params).map_err(|error| {
        Box::new(error_response(
            id.to_owned(),
            error_codes::INVALID_PARAMS,
            format!("invalid params: {error}"),
        ))
    })
}

/// `null` or `{}`: the only params a parameterless method accepts.
fn is_empty_params(params: &Value) -> bool {
    match params {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Encode a port result; `()` becomes a `null` result.
fn encode<T: Serialize>(id: String, result: Result<T, PortError>) -> ResponseEnvelope {
    match result {
        Ok(value) => match serde_json::to_value(&value) {
            Ok(result) => ok_response(id, result),
            Err(error) => internal(id, &error),
        },
        Err(error) => port_error(id, &error),
    }
}

fn port_error(id: String, error: &PortError) -> ResponseEnvelope {
    error_response(id, error.code(), error.to_string())
}

fn internal(id: String, error: &serde_json::Error) -> ResponseEnvelope {
    error_response(
        id,
        error_codes::INTERNAL,
        format!("result encoding failed: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use harw_protocol::session_wire::{
        AttachAck, ClientCaps, Cursor, FrameEnvelope, HelloAck, RespondResult, SessionSummary,
        SubmitResult,
    };
    use harw_protocol::{PortFuture, ProtocolVersion};
    use serde_json::json;

    use super::*;

    type TestResult<T = ()> = Result<T, String>;

    fn ctx<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> TestResult<T> {
        result.map_err(|error| format!("{context}: {error}"))
    }

    fn request(method: &str, params: Value) -> RequestEnvelope {
        RequestEnvelope {
            jsonrpc: "2.0".to_owned(),
            id: "r-1".to_owned(),
            method: method.to_owned(),
            params,
            protocol: ProtocolVersion::default(),
        }
    }

    fn summary(session_id: &str) -> TestResult<SessionSummary> {
        ctx(
            serde_json::from_value(json!({
                "session_id": session_id,
                "title": null,
                "tenant": null,
                "state": { "state": "idle" },
                "attached": 1,
                "updated_at": "2026-01-01T00:00:00Z",
                "model": null,
            })),
            "summary",
        )
    }

    struct EmptySource;

    impl FrameSource for EmptySource {
        fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
            Box::pin(async { Ok(None) })
        }
    }

    /// Records every port call; `deny` makes every call fail with `Denied`.
    struct FakePort {
        calls: Mutex<Vec<&'static str>>,
        deny: bool,
        summary: SessionSummary,
    }

    impl FakePort {
        fn new(deny: bool) -> TestResult<Self> {
            Ok(Self {
                calls: Mutex::new(Vec::new()),
                deny,
                summary: summary("s-1")?,
            })
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }

        fn answer<T: Send + 'static>(&self, name: &'static str, value: T) -> PortFuture<'_, T> {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push(name);
            }
            let deny = self.deny;
            Box::pin(async move {
                if deny {
                    Err(PortError::Denied("caps".into()))
                } else {
                    Ok(value)
                }
            })
        }
    }

    impl SessionPort for FakePort {
        fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck> {
            self.answer(
                "hello",
                HelloAck {
                    wire_minor: params.wire_minor,
                    features: params.features,
                    host_epoch: 7,
                    granted: ClientCaps::OBSERVE,
                },
            )
        }
        fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
            self.answer("list", vec![self.summary.clone()])
        }
        fn create(&self, _params: CreateParams) -> PortFuture<'_, SessionSummary> {
            self.answer("create", self.summary.clone())
        }
        fn attach(
            &self,
            _params: AttachParams,
        ) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
            let ack = AttachAck {
                session: self.summary.clone(),
                granted: ClientCaps::OBSERVE,
                head: Cursor::default(),
                replay_from: Cursor::default(),
                host_epoch: 7,
            };
            let source: Box<dyn FrameSource> = Box::new(EmptySource);
            self.answer("attach", (ack, source))
        }
        fn detach(&self, _session: SessionId) -> PortFuture<'_, ()> {
            self.answer("detach", ())
        }
        fn history(&self, _params: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
            self.answer("history", Vec::new())
        }
        fn submit(&self, _params: SubmitParams) -> PortFuture<'_, SubmitResult> {
            self.answer("submit", SubmitResult::Accepted { position: 0 })
        }
        fn interrupt(&self, _params: InterruptParams) -> PortFuture<'_, ()> {
            self.answer("interrupt", ())
        }
        fn resume(&self, _session: SessionId) -> PortFuture<'_, ()> {
            self.answer("resume", ())
        }
        fn close(&self, _session: SessionId) -> PortFuture<'_, ()> {
            self.answer("close", ())
        }
        fn respond(&self, _params: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
            self.answer("respond", RespondResult::Resolved)
        }
        fn set_model(&self, _params: SetModelParams) -> PortFuture<'_, ()> {
            self.answer("set_model", ())
        }
        fn set_mode(&self, _params: SetModeParams) -> PortFuture<'_, ()> {
            self.answer("set_mode", ())
        }
        fn set_effort(&self, _params: SetEffortParams) -> PortFuture<'_, ()> {
            self.answer("set_effort", ())
        }
    }

    fn plain(dispatched: Dispatched) -> TestResult<ResponseEnvelope> {
        match dispatched {
            Dispatched::Response(response) => Ok(response),
            Dispatched::Attached { .. } => Err("unexpected Attached".into()),
            Dispatched::Detached { .. } => Err("unexpected Detached".into()),
        }
    }

    fn error_code(response: &ResponseEnvelope) -> Option<i32> {
        response.error.as_ref().map(|error| error.code)
    }

    #[test]
    fn only_hello_is_hello() {
        assert!(is_hello(METHOD_SESSION_HELLO));
        assert!(!is_hello(METHOD_SESSION_LIST));
        assert!(!is_hello("session.hello "));
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() -> TestResult {
        let port = FakePort::new(false)?;
        let response = plain(dispatch(&port, request("session.exec", json!({}))).await)?;
        assert_eq!(error_code(&response), Some(error_codes::METHOD_NOT_FOUND));
        assert_eq!(response.id, "r-1");
        assert!(port.calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn bad_params_are_invalid_params_and_never_reach_the_port() -> TestResult {
        let port = FakePort::new(false)?;
        let cases = [
            (METHOD_SESSION_HELLO, json!({ "client_label": 3 })),
            (METHOD_SESSION_ATTACH, json!({})),
            (METHOD_SESSION_DETACH, json!(null)),
            (
                METHOD_APPROVAL_RESP,
                json!({ "request_id": "a-1", "decision": "approved", "actor": "uid:0" }),
            ),
            (METHOD_SESSION_LIST, json!({ "tenant": "other" })),
        ];
        for (method, params) in cases {
            let response = plain(dispatch(&port, request(method, params)).await)?;
            assert_eq!(
                error_code(&response),
                Some(error_codes::INVALID_PARAMS),
                "{method}"
            );
        }
        assert!(port.calls().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn hello_round_trips_through_the_port() -> TestResult {
        let port = FakePort::new(false)?;
        let params = json!({ "client_label": "cli", "wire_minor": 1, "features": ["compact"] });
        let response = plain(dispatch(&port, request(METHOD_SESSION_HELLO, params)).await)?;
        assert!(response.error.is_none());
        let result = response.result.ok_or("missing result")?;
        let ack: HelloAck = ctx(serde_json::from_value(result), "decode ack")?;
        assert_eq!(ack.wire_minor, 1);
        assert_eq!(ack.features, vec!["compact".to_owned()]);
        assert_eq!(ack.granted, ClientCaps::OBSERVE);
        assert_eq!(port.calls(), vec!["hello"]);
        Ok(())
    }

    #[tokio::test]
    async fn attach_returns_attached_with_the_session_id() -> TestResult {
        let port = FakePort::new(false)?;
        let params = json!({ "session_id": "s-1" });
        match dispatch(&port, request(METHOD_SESSION_ATTACH, params)).await {
            Dispatched::Attached {
                response,
                session_id,
                mut source,
            } => {
                assert_eq!(session_id.as_str(), "s-1");
                let result = response.result.ok_or("missing result")?;
                let ack: AttachAck = ctx(serde_json::from_value(result), "decode ack")?;
                assert_eq!(ack.session.session_id.as_str(), "s-1");
                assert!(ctx(source.next().await, "source")?.is_none());
            }
            Dispatched::Response(response) => {
                return Err(format!("unexpected response {response:?}"));
            }
            Dispatched::Detached { .. } => return Err("unexpected Detached".into()),
        }
        assert_eq!(port.calls(), vec!["attach"]);
        Ok(())
    }

    #[tokio::test]
    async fn detach_returns_detached_with_a_null_result() -> TestResult {
        let port = FakePort::new(false)?;
        let params = json!({ "session_id": "s-1" });
        match dispatch(&port, request(METHOD_SESSION_DETACH, params)).await {
            Dispatched::Detached {
                response,
                session_id,
            } => {
                assert_eq!(session_id.as_str(), "s-1");
                assert!(response.error.is_none());
                assert_eq!(response.result, Some(Value::Null));
            }
            Dispatched::Response(response) => {
                return Err(format!("unexpected response {response:?}"));
            }
            Dispatched::Attached { .. } => return Err("unexpected Attached".into()),
        }
        assert_eq!(port.calls(), vec!["detach"]);
        Ok(())
    }

    #[tokio::test]
    async fn port_denied_maps_to_the_denied_code() -> TestResult {
        let port = FakePort::new(true)?;
        let attach = dispatch(
            &port,
            request(METHOD_SESSION_ATTACH, json!({ "session_id": "s-1" })),
        )
        .await;
        let response = plain(attach)?;
        assert_eq!(error_code(&response), Some(error_codes::DENIED));
        let detach = dispatch(
            &port,
            request(METHOD_SESSION_DETACH, json!({ "session_id": "s-1" })),
        )
        .await;
        let response = plain(detach)?;
        assert_eq!(error_code(&response), Some(error_codes::DENIED));
        let close = dispatch(
            &port,
            request(METHOD_SESSION_CLOSE, json!({ "session_id": "s-1" })),
        )
        .await;
        let response = plain(close)?;
        assert_eq!(error_code(&response), Some(error_codes::DENIED));
        assert_eq!(port.calls(), vec!["attach", "detach", "close"]);
        Ok(())
    }

    #[tokio::test]
    async fn list_accepts_null_and_empty_params() -> TestResult {
        let port = FakePort::new(false)?;
        for params in [Value::Null, json!({})] {
            let response = plain(dispatch(&port, request(METHOD_SESSION_LIST, params)).await)?;
            let result = response.result.ok_or("missing result")?;
            let list: ListResult = ctx(serde_json::from_value(result), "decode list")?;
            assert_eq!(list.sessions.len(), 1);
        }
        assert_eq!(port.calls(), vec!["list", "list"]);
        Ok(())
    }

    #[tokio::test]
    async fn unit_results_encode_as_null() -> TestResult {
        let port = FakePort::new(false)?;
        let params = json!({ "session_id": "s-1", "model": "m" });
        let response = plain(dispatch(&port, request(METHOD_SESSION_SET_MODEL, params)).await)?;
        assert!(response.error.is_none());
        assert_eq!(response.result, Some(Value::Null));
        assert_eq!(port.calls(), vec!["set_model"]);
        Ok(())
    }
}

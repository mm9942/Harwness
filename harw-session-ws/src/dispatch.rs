//! Closed method tables (W00 D5, R18 §2.1): one wire request → one typed
//! port call.
//!
//! There is no generic operation execution here. Every method of
//! [`harw_protocol::methods::SESSION_METHODS`] maps to exactly one
//! [`SessionPort`] call, every method of
//! [`harw_protocol::methods::TOOL_METHODS`] to one [`ToolPort`] call and
//! every method of [`harw_protocol::methods::GATEWAY_READ_METHODS`] /
//! [`harw_protocol::methods::GATEWAY_ADMIN_METHODS`] to one [`GatewayPort`]
//! call, with strictly decoded parameters; anything else is
//! `METHOD_NOT_FOUND`. Parameters that fail to decode never reach a port.
//!
//! R18 methods pass a cap gate first ([`required_cap`]): the caps the host
//! granted at hello must contain `tool_call`, `gateway_read` or
//! `gateway_admin`, else `DENIED` without calling the port. The gate is
//! defense in depth; the host admits every call again against the
//! connection's identity. A connection without a tool or gateway port
//! answers those methods `METHOD_NOT_FOUND`.

use std::sync::Arc;

use harw_protocol::methods::{
    GATEWAY_ADMIN_METHODS, GATEWAY_READ_METHODS, METHOD_APPROVAL_RESP,
    METHOD_GATEWAY_CONNECTIONS_LIST, METHOD_GATEWAY_CONNECTIONS_REVOKE, METHOD_GATEWAY_DRAIN,
    METHOD_GATEWAY_LISTENERS_LIST, METHOD_GATEWAY_LISTENERS_SET, METHOD_GATEWAY_SESSIONS_LIST,
    METHOD_GATEWAY_STATUS, METHOD_GATEWAY_TOOLS_GRANT, METHOD_GATEWAY_TOOLS_LIST,
    METHOD_GATEWAY_TOOLS_NARROW, METHOD_SESSION_ATTACH, METHOD_SESSION_CLOSE,
    METHOD_SESSION_CREATE, METHOD_SESSION_DETACH, METHOD_SESSION_HELLO, METHOD_SESSION_HISTORY,
    METHOD_SESSION_LIST, METHOD_SESSION_RESUME, METHOD_SESSION_SET_EFFORT, METHOD_SESSION_SET_MODE,
    METHOD_SESSION_SET_MODEL, METHOD_TOOL_CALL, METHOD_TOOL_CANCEL, METHOD_TOOL_LIST,
    METHOD_TURN_INTERRUPT, METHOD_TURN_SUBMIT, TOOL_METHODS,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachParams, CreateParams, GatewayDrainParams,
    GatewayListenerSetParams, GatewayRevokeParams, GatewayToolRightsParams, HelloParams,
    HistoryParams, HistoryResult, InterruptParams, ListResult, SessionRef, SetEffortParams,
    SetModeParams, SetModelParams, SubmitParams, ToolCallParams, ToolCancelParams, ToolListParams,
    error_codes,
};
use harw_protocol::{
    ClientCaps, FrameSource, GatewayPort, PortError, RequestEnvelope, ResponseEnvelope,
    SessionPort, ToolPort,
};
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

/// The ports one connection serves: the session port always, the R18 tool
/// and gateway ports when the host offers them to this connection.
#[derive(Clone)]
pub struct Ports {
    pub session: Arc<dyn SessionPort>,
    pub tools: Option<Arc<dyn ToolPort>>,
    pub gateway: Option<Arc<dyn GatewayPort>>,
}

impl std::fmt::Debug for Ports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ports")
            .field("tools", &self.tools.is_some())
            .field("gateway", &self.gateway.is_some())
            .finish_non_exhaustive()
    }
}

impl Ports {
    /// Session methods only (W00): `tool.*` and `gateway.*` answer
    /// `METHOD_NOT_FOUND`.
    #[must_use]
    pub fn session_only(session: Arc<dyn SessionPort>) -> Self {
        Self {
            session,
            tools: None,
            gateway: None,
        }
    }

    /// One port object serving all three tables (the host's connection).
    #[must_use]
    pub fn all<P>(port: Arc<P>) -> Self
    where
        P: SessionPort + ToolPort + GatewayPort + 'static,
    {
        Self {
            session: Arc::clone(&port) as Arc<dyn SessionPort>,
            tools: Some(Arc::clone(&port) as Arc<dyn ToolPort>),
            gateway: Some(port as Arc<dyn GatewayPort>),
        }
    }
}

/// The R18 cap a method needs at the dispatcher, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredCap {
    ToolCall,
    GatewayRead,
    GatewayAdmin,
}

impl RequiredCap {
    const fn name(self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::GatewayRead => "gateway_read",
            Self::GatewayAdmin => "gateway_admin",
        }
    }

    const fn is_granted(self, caps: ClientCaps) -> bool {
        match self {
            Self::ToolCall => caps.tool_call,
            Self::GatewayRead => caps.gateway_read,
            Self::GatewayAdmin => caps.gateway_admin,
        }
    }
}

/// Cap gate of an R18 method; `None` for session methods and unknown names.
#[must_use]
pub fn required_cap(method: &str) -> Option<RequiredCap> {
    if TOOL_METHODS.contains(&method) {
        Some(RequiredCap::ToolCall)
    } else if GATEWAY_READ_METHODS.contains(&method) {
        Some(RequiredCap::GatewayRead)
    } else if GATEWAY_ADMIN_METHODS.contains(&method) {
        Some(RequiredCap::GatewayAdmin)
    } else {
        None
    }
}

/// Route one request to the session, tool or gateway port. `granted` are
/// the caps from the successful hello (`ClientCaps::NONE` before it).
pub async fn dispatch_ports(
    ports: &Ports,
    granted: ClientCaps,
    request: RequestEnvelope,
) -> Dispatched {
    let Some(cap) = required_cap(&request.method) else {
        return dispatch(ports.session.as_ref(), request).await;
    };
    let RequestEnvelope {
        id, method, params, ..
    } = request;
    let served = match cap {
        RequiredCap::ToolCall => ports.tools.is_some(),
        RequiredCap::GatewayRead | RequiredCap::GatewayAdmin => ports.gateway.is_some(),
    };
    if !served {
        return Dispatched::Response(error_response(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        ));
    }
    if !cap.is_granted(granted) {
        return Dispatched::Response(error_response(
            id,
            error_codes::DENIED,
            format!("capability `{}` not granted", cap.name()),
        ));
    }
    let response = match (&ports.tools, &ports.gateway, cap) {
        (Some(tools), _, RequiredCap::ToolCall) => {
            dispatch_tool(tools.as_ref(), id, &method, params).await
        }
        (_, Some(gateway), RequiredCap::GatewayRead | RequiredCap::GatewayAdmin) => {
            dispatch_gateway(gateway.as_ref(), id, &method, params).await
        }
        _ => error_response(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        ),
    };
    Dispatched::Response(response)
}

/// `tool.*` (R18 D-A).
async fn dispatch_tool(
    port: &dyn ToolPort,
    id: String,
    method: &str,
    params: Value,
) -> ResponseEnvelope {
    match method {
        METHOD_TOOL_LIST => match decode::<ToolListParams>(&id, params) {
            Ok(params) => encode(id, port.list_tools(params).await),
            Err(response) => *response,
        },
        METHOD_TOOL_CALL => match decode::<ToolCallParams>(&id, params) {
            Ok(params) => encode(id, port.call_tool(params).await),
            Err(response) => *response,
        },
        METHOD_TOOL_CANCEL => match decode::<ToolCancelParams>(&id, params) {
            Ok(params) => encode(id, port.cancel_tool(params).await),
            Err(response) => *response,
        },
        _ => error_response(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        ),
    }
}

/// `gateway.*` (R18 D-B).
async fn dispatch_gateway(
    port: &dyn GatewayPort,
    id: String,
    method: &str,
    params: Value,
) -> ResponseEnvelope {
    let parameterless = matches!(
        method,
        METHOD_GATEWAY_STATUS
            | METHOD_GATEWAY_CONNECTIONS_LIST
            | METHOD_GATEWAY_SESSIONS_LIST
            | METHOD_GATEWAY_LISTENERS_LIST
            | METHOD_GATEWAY_TOOLS_LIST
    );
    if parameterless && !is_empty_params(&params) {
        return error_response(
            id,
            error_codes::INVALID_PARAMS,
            format!("{method} takes no params"),
        );
    }
    match method {
        METHOD_GATEWAY_STATUS => encode(id, port.status().await),
        METHOD_GATEWAY_CONNECTIONS_LIST => encode(id, port.connections().await),
        METHOD_GATEWAY_SESSIONS_LIST => encode(id, port.sessions().await),
        METHOD_GATEWAY_LISTENERS_LIST => encode(id, port.listeners().await),
        METHOD_GATEWAY_TOOLS_LIST => encode(id, port.tools().await),
        METHOD_GATEWAY_CONNECTIONS_REVOKE => match decode::<GatewayRevokeParams>(&id, params) {
            Ok(params) => encode(id, port.revoke_connection(params).await),
            Err(response) => *response,
        },
        METHOD_GATEWAY_DRAIN => match decode::<GatewayDrainParams>(&id, params) {
            Ok(params) => encode(id, port.drain(params).await),
            Err(response) => *response,
        },
        METHOD_GATEWAY_LISTENERS_SET => match decode::<GatewayListenerSetParams>(&id, params) {
            Ok(params) => encode(id, port.set_listener(params).await),
            Err(response) => *response,
        },
        METHOD_GATEWAY_TOOLS_GRANT => match decode::<GatewayToolRightsParams>(&id, params) {
            Ok(params) => encode(id, port.grant_tools(params).await),
            Err(response) => *response,
        },
        METHOD_GATEWAY_TOOLS_NARROW => match decode::<GatewayToolRightsParams>(&id, params) {
            Ok(params) => encode(id, port.narrow_tools(params).await),
            Err(response) => *response,
        },
        _ => error_response(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        ),
    }
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

/// Error response for a port error. A tool refusal carries its bare
/// `detail` as the message, so `PortError::from_code` on the client restores
/// the same value without double wrapping (R18 §10.3 P1).
fn port_error(id: String, error: &PortError) -> ResponseEnvelope {
    match error {
        PortError::ToolRefused { detail, .. } => error_response(id, error.code(), detail.clone()),
        _ => error_response(id, error.code(), error.to_string()),
    }
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
        AgentRole, AttachAck, Cursor, FrameEnvelope, GatewayConnectionsResult, GatewayListenerInfo,
        GatewayListenersResult, GatewayRevokeResult, GatewayStatus, GatewayToolRights,
        GatewayToolsResult, HelloAck, ListenerKind, RespondResult, SessionSummary, SubmitResult,
        ToolCallResultFrame, ToolListResult,
    };
    use harw_protocol::{
        PortFuture, ProtocolVersion, ResultTrust, ToolCallResult, ToolPlacement, ToolRefusal,
    };
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

    /// Records every port call; `deny` makes every call fail with `Denied`,
    /// `refusal` makes `tool.call` fail with that tool refusal.
    struct FakePort {
        calls: Mutex<Vec<&'static str>>,
        deny: bool,
        refusal: Option<ToolRefusal>,
        summary: SessionSummary,
    }

    impl FakePort {
        fn new(deny: bool) -> TestResult<Self> {
            Ok(Self {
                calls: Mutex::new(Vec::new()),
                deny,
                refusal: None,
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

    fn status() -> GatewayStatus {
        GatewayStatus {
            host_epoch: 7,
            node: None,
            draining: false,
            connections: 1,
            sessions: 1,
            running_turns: 0,
            listeners: 0,
            tools: 0,
            sandbox_available: false,
        }
    }

    fn rights() -> GatewayToolRights {
        GatewayToolRights {
            agent: "agent:uia".into(),
            role: AgentRole::UserInterface,
            tools: vec![],
        }
    }

    impl ToolPort for FakePort {
        fn list_tools(&self, _params: ToolListParams) -> PortFuture<'_, ToolListResult> {
            self.answer("list_tools", ToolListResult { tools: vec![] })
        }
        fn call_tool(&self, params: ToolCallParams) -> PortFuture<'_, ToolCallResultFrame> {
            if let Some(refusal) = self.refusal {
                if let Ok(mut calls) = self.calls.lock() {
                    calls.push("call_tool");
                }
                let detail = params.tool_name;
                return Box::pin(async move { Err(PortError::ToolRefused { refusal, detail }) });
            }
            self.answer(
                "call_tool",
                ToolCallResultFrame {
                    call_id: params.call_id,
                    result: ToolCallResult::success(json!("ok")),
                    placement: ToolPlacement::Gateway { node: None },
                    duration_ms: 1,
                    trust: ResultTrust::Untrusted,
                },
            )
        }
        fn cancel_tool(&self, _params: ToolCancelParams) -> PortFuture<'_, ()> {
            self.answer("cancel_tool", ())
        }
    }

    impl GatewayPort for FakePort {
        fn status(&self) -> PortFuture<'_, GatewayStatus> {
            self.answer("status", status())
        }
        fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult> {
            self.answer(
                "connections",
                GatewayConnectionsResult {
                    connections: vec![],
                },
            )
        }
        fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>> {
            self.answer("sessions", vec![self.summary.clone()])
        }
        fn listeners(&self) -> PortFuture<'_, GatewayListenersResult> {
            self.answer("listeners", GatewayListenersResult { listeners: vec![] })
        }
        fn tools(&self) -> PortFuture<'_, GatewayToolsResult> {
            self.answer(
                "tools",
                GatewayToolsResult {
                    tools: vec![],
                    grants: vec![],
                },
            )
        }
        fn revoke_connection(
            &self,
            _params: GatewayRevokeParams,
        ) -> PortFuture<'_, GatewayRevokeResult> {
            self.answer("revoke_connection", GatewayRevokeResult { revoked: true })
        }
        fn drain(&self, _params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus> {
            self.answer("drain", status())
        }
        fn set_listener(
            &self,
            params: GatewayListenerSetParams,
        ) -> PortFuture<'_, GatewayListenerInfo> {
            self.answer(
                "set_listener",
                GatewayListenerInfo {
                    name: params.name,
                    kind: ListenerKind::LocalUds,
                    address: "/run/harw.sock".into(),
                    enabled: params.enabled,
                },
            )
        }
        fn grant_tools(
            &self,
            _params: GatewayToolRightsParams,
        ) -> PortFuture<'_, GatewayToolRights> {
            self.answer("grant_tools", rights())
        }
        fn narrow_tools(
            &self,
            _params: GatewayToolRightsParams,
        ) -> PortFuture<'_, GatewayToolRights> {
            self.answer("narrow_tools", rights())
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

    fn call_params() -> Value {
        json!({
            "session_id": "s-1",
            "turn_id": "t-1",
            "call_id": "c-1",
            "tool_name": "fs.read",
            "arguments": {"path": "a"}
        })
    }

    fn r18(port: FakePort) -> (Arc<FakePort>, Ports) {
        let port = Arc::new(port);
        let ports = Ports::all(Arc::clone(&port));
        (port, ports)
    }

    #[test]
    fn r18_methods_map_to_their_caps() {
        assert_eq!(required_cap(METHOD_TOOL_CALL), Some(RequiredCap::ToolCall));
        assert_eq!(
            required_cap(METHOD_GATEWAY_STATUS),
            Some(RequiredCap::GatewayRead)
        );
        assert_eq!(
            required_cap(METHOD_GATEWAY_DRAIN),
            Some(RequiredCap::GatewayAdmin)
        );
        assert_eq!(required_cap(METHOD_SESSION_LIST), None);
        assert_eq!(required_cap("tool.exec"), None);
    }

    /// TG-11: an identity field in `tool.call` params is a decode error and
    /// never reaches the port; an unknown `tool.*` name is `METHOD_NOT_FOUND`.
    #[tokio::test]
    async fn tool_call_with_identity_field_never_reaches_the_port() -> TestResult {
        let (port, ports) = r18(FakePort::new(false)?);
        let mut params = call_params();
        params["principal"] = json!("agent:uia");
        let response = plain(
            dispatch_ports(
                &ports,
                ClientCaps::TOOL_CALL,
                request(METHOD_TOOL_CALL, params),
            )
            .await,
        )?;
        assert_eq!(error_code(&response), Some(error_codes::INVALID_PARAMS));
        let unknown = plain(
            dispatch_ports(
                &ports,
                ClientCaps::TOOL_CALL,
                request("tool.exec", json!({})),
            )
            .await,
        )?;
        assert_eq!(error_code(&unknown), Some(error_codes::METHOD_NOT_FOUND));
        assert!(port.calls().is_empty());
        let ok = plain(
            dispatch_ports(
                &ports,
                ClientCaps::TOOL_CALL,
                request(METHOD_TOOL_CALL, call_params()),
            )
            .await,
        )?;
        assert!(ok.error.is_none(), "{ok:?}");
        let frame: ToolCallResultFrame = ctx(
            serde_json::from_value(ok.result.ok_or("missing result")?),
            "frame",
        )?;
        assert_eq!(frame.placement, ToolPlacement::Gateway { node: None });
        assert_eq!(port.calls(), vec!["call_tool"]);
        Ok(())
    }

    /// TG-12: `gateway.*` reads need `gateway_read`, mutations
    /// `gateway_admin`, `tool.*` needs `tool_call`; the gate answers
    /// `DENIED` without calling the port.
    #[tokio::test]
    async fn r18_caps_are_gated_before_the_port() -> TestResult {
        let (port, ports) = r18(FakePort::new(false)?);
        let operate = ClientCaps::OPERATE;
        for (method, params) in [
            (METHOD_GATEWAY_STATUS, Value::Null),
            (METHOD_GATEWAY_DRAIN, json!({"retry_after_ms": 10})),
            (METHOD_TOOL_LIST, json!({"session_id": "s-1"})),
        ] {
            let response = plain(dispatch_ports(&ports, operate, request(method, params)).await)?;
            assert_eq!(error_code(&response), Some(error_codes::DENIED), "{method}");
        }
        let read_only = ClientCaps::GATEWAY_READ;
        let drain = plain(
            dispatch_ports(
                &ports,
                read_only,
                request(METHOD_GATEWAY_DRAIN, json!({"retry_after_ms": 10})),
            )
            .await,
        )?;
        assert_eq!(error_code(&drain), Some(error_codes::DENIED));
        assert!(port.calls().is_empty());

        let status = plain(
            dispatch_ports(&ports, read_only, request(METHOD_GATEWAY_STATUS, json!({}))).await,
        )?;
        assert!(status.error.is_none(), "{status:?}");
        let admin = ClientCaps::GATEWAY_ADMIN;
        let drained = plain(
            dispatch_ports(
                &ports,
                admin,
                request(METHOD_GATEWAY_DRAIN, json!({"retry_after_ms": 10})),
            )
            .await,
        )?;
        assert!(drained.error.is_none(), "{drained:?}");
        let sessions = plain(
            dispatch_ports(
                &ports,
                admin,
                request(METHOD_GATEWAY_SESSIONS_LIST, Value::Null),
            )
            .await,
        )?;
        let listed: Vec<SessionSummary> = ctx(
            serde_json::from_value(sessions.result.ok_or("missing result")?),
            "sessions",
        )?;
        assert_eq!(listed.len(), 1);
        assert_eq!(port.calls(), vec!["status", "drain", "sessions"]);
        Ok(())
    }

    #[tokio::test]
    async fn gateway_params_are_strict() -> TestResult {
        let (port, ports) = r18(FakePort::new(false)?);
        let admin = ClientCaps::GATEWAY_ADMIN;
        for (method, params) in [
            (METHOD_GATEWAY_STATUS, json!({"tenant": "t"})),
            (
                METHOD_GATEWAY_TOOLS_GRANT,
                json!({"agent": "a", "tools": [], "role": "user-interface"}),
            ),
            (
                METHOD_GATEWAY_CONNECTIONS_REVOKE,
                json!({"connection": 1, "reason": "r", "actor": "uid:0"}),
            ),
        ] {
            let response = plain(dispatch_ports(&ports, admin, request(method, params)).await)?;
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
    async fn session_only_ports_do_not_serve_r18_methods() -> TestResult {
        let port = Arc::new(FakePort::new(false)?);
        let ports = Ports::session_only(Arc::clone(&port) as Arc<dyn SessionPort>);
        let all = ClientCaps::ALL
            .with(ClientCaps::TOOL_CALL)
            .with(ClientCaps::GATEWAY_ADMIN);
        for method in [
            METHOD_TOOL_CALL,
            METHOD_GATEWAY_STATUS,
            METHOD_GATEWAY_DRAIN,
        ] {
            let response = plain(dispatch_ports(&ports, all, request(method, json!({}))).await)?;
            assert_eq!(
                error_code(&response),
                Some(error_codes::METHOD_NOT_FOUND),
                "{method}"
            );
        }
        // Session methods still work through the same entry point.
        let list =
            plain(dispatch_ports(&ports, all, request(METHOD_SESSION_LIST, Value::Null)).await)?;
        assert!(list.error.is_none());
        assert_eq!(port.calls(), vec!["list"]);
        Ok(())
    }

    /// A tool refusal travels as its own code with the bare detail, so the
    /// client restores exactly the same `PortError`.
    #[tokio::test]
    async fn tool_refusal_message_is_the_bare_detail() -> TestResult {
        for refusal in ToolRefusal::ALL {
            let mut fake = FakePort::new(false)?;
            fake.refusal = Some(refusal);
            let (_, ports) = r18(fake);
            let response = plain(
                dispatch_ports(
                    &ports,
                    ClientCaps::TOOL_CALL,
                    request(METHOD_TOOL_CALL, call_params()),
                )
                .await,
            )?;
            let error = response.error.ok_or("missing error")?;
            assert_eq!(error.code, refusal.code());
            assert_eq!(error.message, "fs.read");
            assert_eq!(
                PortError::from_code(error.code, error.message),
                PortError::ToolRefused {
                    refusal,
                    detail: "fs.read".into()
                }
            );
        }
        Ok(())
    }
}

//! End-to-end: HTTP/1 upgrade (hyper) → `harw.session.v1` WebSocket →
//! connection driver → session host → scripted driver, and back as frames.
//! R18: the same path serves `tool.*`/`gateway.*` for an agent principal.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::methods::{
    METHOD_GATEWAY_STATUS, METHOD_SESSION_ATTACH, METHOD_SESSION_CREATE, METHOD_SESSION_HELLO,
    METHOD_TOOL_CALL, METHOD_TOOL_LIST, METHOD_TURN_SUBMIT,
};
use harw_protocol::session_wire::{HelloAck, SessionSummary, ToolListResult, error_codes};
use harw_protocol::{
    ClientCaps, FrameEnvelope, PortError, ProtocolVersion, RequestEnvelope, ResponseEnvelope,
    SessionFrame, ToolRefusal, TurnEvent, WireMessage,
};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{
    AgentCredential, ClientIdentity, ConnectionId, HostConfig, NoGatewaySandbox, SessionHost,
    ToolGrant, ToolHost, caps_for_tier,
};
use harw_session_ws::conn::serve_connection_with;
use harw_session_ws::dispatch::Ports;
use harw_session_ws::{WsLimits, serve_connection, upgrade};
use harw_types::{
    ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
    SessionId, ThreadId, TrustZone, TurnId,
};
use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::watch;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct EchoDriver;

impl TurnDriver for EchoDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                turn_id,
                text: format!("echo: {}", input.text),
            }));
            Ok(TurnOutcome::Completed)
        })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

fn identity() -> ClientIdentity {
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "uid:1000",
            IngressSurface::Tui,
            PermissionTier::Operator,
        ),
        tenant: None,
        caps: caps_for_tier(PermissionTier::Operator),
        device: None,
        actor: ApprovalActor::Operator {
            id: "uid:1000".into(),
        },
        label: "laptop".into(),
        zone: TrustZone::Local,
        strength: AuthStrength::PeerCredential,
        connection: ConnectionId::next(),
        agent: None,
    }
}

/// Serve one HTTP/1 connection whose only route upgrades into the session
/// control plane for a fixed, already-authenticated identity.
fn spawn_server(io: tokio::io::DuplexStream, host: SessionHost) {
    spawn_server_for(io, host, identity(), false);
}

/// Like [`spawn_server`] for any identity; `r18` also serves the tool and
/// gateway tables of the host connection.
fn spawn_server_for(
    io: tokio::io::DuplexStream,
    host: SessionHost,
    identity: ClientIdentity,
    r18: bool,
) {
    tokio::spawn(async move {
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            let host = host.clone();
            let identity = identity.clone();
            async move {
                let limits = WsLimits::default();
                let upgraded = upgrade::upgrade_server(
                    request,
                    limits.tungstenite_config(),
                    move |ws| async move {
                        let Ok(port) = host.connect(identity) else {
                            return;
                        };
                        let (_tx, shutdown) = watch::channel(false);
                        if r18 {
                            let ports = Ports::all(Arc::new(port));
                            serve_connection_with(ws, ports, limits, shutdown).await;
                        } else {
                            serve_connection(ws, Arc::new(port), limits, shutdown).await;
                        }
                    },
                );
                let response = match upgraded {
                    Ok(response) => response.map(|_| http_body_util::Full::new(Bytes::new())),
                    Err(rejection) => upgrade::rejection_response(&rejection),
                };
                Ok::<_, std::convert::Infallible>(response)
            }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(io), service)
            .with_upgrades()
            .await;
    });
}

async fn client(
    io: tokio::io::DuplexStream,
    request: hyper::Request<Empty<Bytes>>,
) -> TestResult<hyper::Response<Incoming>> {
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    Ok(sender.send_request(request).await?)
}

fn request(id: &str, method: &str, params: serde_json::Value) -> TestResult<Message> {
    let envelope = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: id.into(),
        method: method.into(),
        params,
        protocol: ProtocolVersion::default(),
    };
    Ok(Message::text(serde_json::to_string(&envelope)?))
}

async fn next<S>(ws: &mut WebSocketStream<S>) -> TestResult<WireMessage>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .ok_or("closed")??;
        if let Message::Text(text) = message {
            return Ok(serde_json::from_str(text.as_str())?);
        }
    }
}

async fn call<S>(
    ws: &mut WebSocketStream<S>,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> TestResult<ResponseEnvelope>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    ws.send(request(id, method, params)?).await?;
    loop {
        if let WireMessage::Response(response) = next(ws).await? {
            if response.id == id {
                return Ok(response);
            }
        }
    }
}

fn open_host(dir: &std::path::Path) -> TestResult<SessionHost> {
    Ok(SessionHost::open(
        HostConfig::new(dir.to_path_buf()),
        Arc::new(EchoDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?)
}

#[tokio::test]
async fn upgrade_hello_create_attach_submit_round_trip() -> TestResult {
    let dir = tempfile::tempdir()?;
    let host = open_host(dir.path())?;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, host);

    let (request, key) = upgrade::client_request("localhost")?;
    let response = client(client_io, request).await?;
    upgrade::verify_client_response(&response, &key)?;
    let upgraded = hyper::upgrade::on(response).await?;
    let mut ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;

    let hello = call(
        &mut ws,
        "1",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "e2e", "wire_minor": 1}),
    )
    .await?;
    assert!(hello.error.is_none(), "{hello:?}");
    let created = call(
        &mut ws,
        "2",
        METHOD_SESSION_CREATE,
        serde_json::json!({"title": "phone"}),
    )
    .await?;
    let summary: SessionSummary = serde_json::from_value(created.result.ok_or("no result")?)?;
    let attached = call(
        &mut ws,
        "3",
        METHOD_SESSION_ATTACH,
        serde_json::json!({"session_id": summary.session_id.as_str()}),
    )
    .await?;
    assert!(attached.error.is_none(), "{attached:?}");
    let submitted = call(
        &mut ws,
        "4",
        METHOD_TURN_SUBMIT,
        serde_json::json!({
            "session_id": summary.session_id.as_str(),
            "text": "hi",
            "expect_head": {"generation": 0, "durable": 0, "live": 0},
            "client_msg_id": "m1"
        }),
    )
    .await?;
    assert!(submitted.error.is_none(), "{submitted:?}");

    for _ in 0..32 {
        if let WireMessage::Notification(notification) = next(&mut ws).await? {
            let frame: FrameEnvelope = serde_json::from_value(notification.params)?;
            if let SessionFrame::Turn(TurnEvent::AssistantDelta { text, .. }) = frame.frame {
                assert_eq!(text, "echo: hi");
                return Ok(());
            }
        }
    }
    Err("assistant delta never arrived".into())
}

#[tokio::test]
async fn wrong_subprotocol_is_refused_before_upgrade() -> TestResult {
    // WS-01
    let dir = tempfile::tempdir()?;
    let host = open_host(dir.path())?;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, host);
    let (mut request, _) = upgrade::client_request("localhost")?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        hyper::header::HeaderValue::from_static("chat"),
    );
    let response = client(client_io, request).await?;
    assert_eq!(response.status(), hyper::StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn browser_origin_is_refused() -> TestResult {
    // WS-04
    let dir = tempfile::tempdir()?;
    let host = open_host(dir.path())?;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server(server_io, host);
    let (mut request, _) = upgrade::client_request("localhost")?;
    request.headers_mut().insert(
        "origin",
        hyper::header::HeaderValue::from_static("https://evil.example"),
    );
    let response = client(client_io, request).await?;
    assert_eq!(response.status(), hyper::StatusCode::FORBIDDEN);
    Ok(())
}

/// R18 over the real transport: an agent principal negotiates minor 2 and
/// receives `tool_call`; `tool.list` answers its grant; a refused
/// `tool.call` carries the refusal code and the bare tool name, so the
/// client restores the typed refusal; `gateway.*` without the cap is
/// `DENIED`.
#[tokio::test]
async fn agent_tool_calls_over_the_websocket() -> TestResult {
    let dir = tempfile::tempdir()?;
    let host = SessionHost::open_with_tools(
        HostConfig::new(dir.path().to_path_buf()),
        Arc::new(EchoDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
        ToolHost::builder(Arc::new(NoGatewaySandbox)).build()?,
    )?;
    let credential = AgentCredential::new("cred-uia")?;
    host.agents()
        .enroll_uia(credential.clone(), "agent:uia", None, &ToolGrant::none())?;
    let resolved = host
        .agents()
        .resolve(&credential)
        .ok_or("agent not enrolled")?;
    let agent = ClientIdentity {
        caps: caps_for_tier(PermissionTier::Operator).with(ClientCaps::TOOL_CALL),
        label: "uia".into(),
        connection: ConnectionId::next(),
        agent: Some(resolved.principal),
        ..identity()
    };
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server_for(server_io, host, agent, true);
    let (request, key) = upgrade::client_request("localhost")?;
    let response = client(client_io, request).await?;
    upgrade::verify_client_response(&response, &key)?;
    let upgraded = hyper::upgrade::on(response).await?;
    let mut ws = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;

    let hello = call(
        &mut ws,
        "1",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "uia", "wire_minor": 2, "features": ["tools"]}),
    )
    .await?;
    let ack: HelloAck = serde_json::from_value(hello.result.ok_or("no hello result")?)?;
    assert!(ack.granted.tool_call);
    assert_eq!(ack.features, vec!["tools".to_owned()]);

    let created = call(&mut ws, "2", METHOD_SESSION_CREATE, serde_json::json!({})).await?;
    let summary: SessionSummary = serde_json::from_value(created.result.ok_or("no result")?)?;
    let session = summary.session_id.as_str();

    let listed = call(
        &mut ws,
        "3",
        METHOD_TOOL_LIST,
        serde_json::json!({"session_id": session}),
    )
    .await?;
    let tools: ToolListResult = serde_json::from_value(listed.result.ok_or("no list")?)?;
    assert!(tools.tools.is_empty());

    let refused = call(
        &mut ws,
        "4",
        METHOD_TOOL_CALL,
        serde_json::json!({
            "session_id": session,
            "turn_id": "t-1",
            "call_id": "c-1",
            "tool_name": "fs.read",
            "arguments": {}
        }),
    )
    .await?;
    let error = refused.error.ok_or("tool.call was not refused")?;
    assert_eq!(
        PortError::from_code(error.code, error.message),
        PortError::ToolRefused {
            refusal: ToolRefusal::UnknownTool,
            detail: "fs.read".into()
        }
    );

    let status = call(&mut ws, "5", METHOD_GATEWAY_STATUS, serde_json::Value::Null).await?;
    assert_eq!(
        status.error.map(|error| error.code),
        Some(error_codes::DENIED)
    );
    Ok(())
}

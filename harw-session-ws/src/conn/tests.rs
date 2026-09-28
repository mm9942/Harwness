//! Connection driver tests (W00 §9: WS-02, WS-03, WS-05, WS-06, WS-07,
//! REV-01 at the transport level) over an in-memory duplex socket.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::methods::{
    METHOD_SESSION_ATTACH, METHOD_SESSION_HELLO, METHOD_SESSION_LIST, METHOD_TURN_SUBMIT,
};
use harw_protocol::session_wire::{
    ApprovalRespondParams, AttachAck, AttachParams, CreateParams, HelloAck, HelloParams,
    HistoryParams, InterruptParams, RespondResult, SessionSummary, SetEffortParams, SetModeParams,
    SetModelParams, SubmitParams, SubmitResult, error_codes,
};
use harw_protocol::{
    ClientCaps, Cursor, FrameEnvelope, FrameSource, HostedState, PortError, PortFuture,
    ProtocolVersion, RequestEnvelope, ResponseEnvelope, SessionFrame, SessionPort, WireMessage,
};
use harw_types::SessionId;
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

use super::{ConnectionEnd, serve_connection};
use crate::codec::CloseReason;
use crate::limits::WsLimits;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct ChannelSource(mpsc::Receiver<FrameEnvelope>);

impl FrameSource for ChannelSource {
    fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
        Box::pin(async move { Ok(self.0.recv().await) })
    }
}

/// Port whose attach hands out test-controlled frame channels and whose
/// submit blocks until released.
struct FakePort {
    sources: Mutex<Vec<mpsc::Receiver<FrameEnvelope>>>,
    release: Arc<Notify>,
}

fn summary(session_id: SessionId) -> SessionSummary {
    SessionSummary {
        session_id,
        title: None,
        tenant: None,
        state: HostedState::Idle,
        attached: 1,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
        model: None,
    }
}

impl SessionPort for FakePort {
    fn hello(&self, params: HelloParams) -> PortFuture<'_, HelloAck> {
        Box::pin(async move {
            Ok(HelloAck {
                wire_minor: params.wire_minor.min(1),
                features: vec![],
                host_epoch: 1,
                granted: ClientCaps::OPERATE,
            })
        })
    }
    fn list(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn create(&self, _: CreateParams) -> PortFuture<'_, SessionSummary> {
        Box::pin(async { Err(PortError::Denied("no".into())) })
    }
    fn attach(&self, params: AttachParams) -> PortFuture<'_, (AttachAck, Box<dyn FrameSource>)> {
        Box::pin(async move {
            let receiver = self.sources.lock().await.pop().ok_or(PortError::NotFound)?;
            let ack = AttachAck {
                session: summary(params.session_id),
                granted: ClientCaps::OPERATE,
                head: Cursor::default(),
                replay_from: Cursor::default(),
                host_epoch: 1,
            };
            Ok((
                ack,
                Box::new(ChannelSource(receiver)) as Box<dyn FrameSource>,
            ))
        })
    }
    fn detach(&self, _: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn history(&self, _: HistoryParams) -> PortFuture<'_, Vec<FrameEnvelope>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn submit(&self, _: SubmitParams) -> PortFuture<'_, SubmitResult> {
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            release.notified().await;
            Ok(SubmitResult::Accepted { position: 0 })
        })
    }
    fn interrupt(&self, _: InterruptParams) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn resume(&self, _: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&self, _: SessionId) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn respond(&self, _: ApprovalRespondParams) -> PortFuture<'_, RespondResult> {
        Box::pin(async { Ok(RespondResult::Resolved) })
    }
    fn set_model(&self, _: SetModelParams) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn set_mode(&self, _: SetModeParams) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn set_effort(&self, _: SetEffortParams) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct Harness {
    client: WebSocketStream<tokio::io::DuplexStream>,
    server: tokio::task::JoinHandle<ConnectionEnd>,
    senders: Vec<mpsc::Sender<FrameEnvelope>>,
    release: Arc<Notify>,
    _shutdown: watch::Sender<bool>,
}

async fn harness(limits: WsLimits, attachments: usize, capacity: usize) -> Harness {
    // A small socket buffer: bytes already handed to the socket are outside
    // the multiplexer's control, so the fairness test must bound them.
    let (client_io, server_io) = tokio::io::duplex(8 * 1024);
    let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
    let server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for _ in 0..attachments {
        let (tx, rx) = mpsc::channel(capacity);
        senders.push(tx);
        receivers.push(rx);
    }
    receivers.reverse();
    let release = Arc::new(Notify::new());
    let port = Arc::new(FakePort {
        sources: Mutex::new(receivers),
        release: Arc::clone(&release),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(serve_connection(server, port, limits, shutdown_rx));
    Harness {
        client,
        server,
        senders,
        release,
        _shutdown: shutdown_tx,
    }
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

enum Received {
    Response(ResponseEnvelope),
    Frame(FrameEnvelope),
    Closed(Option<u16>),
}

async fn receive(client: &mut WebSocketStream<tokio::io::DuplexStream>) -> TestResult<Received> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await?
            .ok_or("stream ended")??;
        match message {
            Message::Text(text) => match serde_json::from_str::<WireMessage>(text.as_str())? {
                WireMessage::Response(response) => return Ok(Received::Response(response)),
                WireMessage::Notification(notification) => {
                    let frame = serde_json::from_value(notification.params)?;
                    return Ok(Received::Frame(frame));
                }
                WireMessage::Request(_) => return Err("server sent a request".into()),
            },
            Message::Close(frame) => {
                return Ok(Received::Closed(frame.map(|frame| u16::from(frame.code))));
            }
            _ => {}
        }
    }
}

async fn response(
    client: &mut WebSocketStream<tokio::io::DuplexStream>,
) -> TestResult<ResponseEnvelope> {
    loop {
        match receive(client).await? {
            Received::Response(response) => return Ok(response),
            Received::Frame(_) => {}
            Received::Closed(code) => return Err(format!("closed: {code:?}").into()),
        }
    }
}

async fn say_hello(client: &mut WebSocketStream<tokio::io::DuplexStream>) -> TestResult {
    client
        .send(request(
            "h",
            METHOD_SESSION_HELLO,
            serde_json::json!({"client_label": "t", "wire_minor": 1}),
        )?)
        .await?;
    let ack = response(client).await?;
    assert!(ack.error.is_none(), "{ack:?}");
    Ok(())
}

fn session(id: &str) -> TestResult<SessionId> {
    Ok(SessionId::try_from_str(id)?)
}

fn frame(session_id: &SessionId, n: u64) -> FrameEnvelope {
    FrameEnvelope {
        session_id: session_id.clone(),
        cursor: Cursor {
            generation: 0,
            durable: n,
            live: 0,
        },
        frame: SessionFrame::Heartbeat,
    }
}

#[tokio::test]
async fn calls_before_hello_are_refused() -> TestResult {
    // WS-02
    let mut h = harness(WsLimits::default(), 0, 1).await;
    h.client
        .send(request("1", METHOD_SESSION_LIST, serde_json::Value::Null)?)
        .await?;
    let refused = response(&mut h.client).await?;
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(error_codes::HELLO_REQUIRED)
    );
    say_hello(&mut h.client).await?;
    h.client
        .send(request("2", METHOD_SESSION_LIST, serde_json::Value::Null)?)
        .await?;
    assert!(response(&mut h.client).await?.error.is_none());
    Ok(())
}

#[tokio::test]
async fn missing_hello_times_out() -> TestResult {
    let limits = WsLimits {
        hello_timeout: Duration::from_millis(50),
        ..WsLimits::default()
    };
    let mut h = harness(limits, 0, 1).await;
    let closed = receive(&mut h.client).await?;
    assert!(
        matches!(closed, Received::Closed(Some(code)) if code == CloseReason::HelloTimeout.code())
    );
    assert_eq!(
        h.server.await?,
        ConnectionEnd::Closed(CloseReason::HelloTimeout)
    );
    Ok(())
}

#[tokio::test]
async fn binary_and_oversized_messages_close_the_connection() -> TestResult {
    // WS-03
    let mut h = harness(WsLimits::default(), 0, 1).await;
    h.client.send(Message::binary(vec![1_u8, 2, 3])).await?;
    assert!(matches!(
        receive(&mut h.client).await?,
        Received::Closed(Some(code)) if code == CloseReason::BinaryRefused.code()
    ));

    let limits = WsLimits {
        max_message_bytes: 64,
        ..WsLimits::default()
    };
    let mut h = harness(limits, 0, 1).await;
    h.client.send(Message::text("x".repeat(1024))).await?;
    assert!(matches!(
        receive(&mut h.client).await?,
        Received::Closed(Some(_))
    ));
    Ok(())
}

#[tokio::test]
async fn duplicate_in_flight_id_is_refused_and_client_responses_close() -> TestResult {
    // WS-05
    let mut h = harness(WsLimits::default(), 0, 1).await;
    say_hello(&mut h.client).await?;
    let params = serde_json::json!({
        "session_id": "s", "text": "x",
        "expect_head": {"generation": 0, "durable": 0, "live": 0},
        "client_msg_id": "m"
    });
    h.client
        .send(request("dup", METHOD_TURN_SUBMIT, params.clone())?)
        .await?;
    h.client
        .send(request("dup", METHOD_TURN_SUBMIT, params)?)
        .await?;
    let refused = response(&mut h.client).await?;
    assert_eq!(refused.id, "dup");
    assert_eq!(
        refused.error.map(|e| e.code),
        Some(error_codes::INVALID_PARAMS)
    );
    h.release.notify_one();
    assert!(response(&mut h.client).await?.error.is_none());

    let stray = ResponseEnvelope {
        jsonrpc: "2.0".into(),
        id: "never-sent".into(),
        result: Some(serde_json::Value::Null),
        error: None,
    };
    h.client
        .send(Message::text(serde_json::to_string(&stray)?))
        .await?;
    assert!(matches!(
        receive(&mut h.client).await?,
        Received::Closed(Some(code)) if code == CloseReason::ProtocolError.code()
    ));
    Ok(())
}

#[tokio::test]
async fn two_attachments_both_make_progress() -> TestResult {
    // WS-06
    let mut h = harness(WsLimits::default(), 2, 64).await;
    say_hello(&mut h.client).await?;
    let (a, b) = (session("a")?, session("b")?);
    for (id, s) in [("1", &a), ("2", &b)] {
        h.client
            .send(request(
                id,
                METHOD_SESSION_ATTACH,
                serde_json::json!({"session_id": s.as_str()}),
            )?)
            .await?;
        assert!(response(&mut h.client).await?.error.is_none());
    }
    for n in 0..5 {
        h.senders[0].send(frame(&a, n)).await?;
        h.senders[1].send(frame(&b, n)).await?;
    }
    let (mut seen_a, mut seen_b) = (0, 0);
    while seen_a < 5 || seen_b < 5 {
        if let Received::Frame(frame) = receive(&mut h.client).await? {
            if frame.session_id == a {
                seen_a += 1;
            } else if frame.session_id == b {
                seen_b += 1;
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn delta_flood_does_not_starve_a_response() -> TestResult {
    // WS-07
    let limits = WsLimits::default();
    let buffer = limits.attachment_buffer;
    let mut h = harness(limits, 1, 4096).await;
    say_hello(&mut h.client).await?;
    let flooded = session("flood")?;
    h.client
        .send(request(
            "a",
            METHOD_SESSION_ATTACH,
            serde_json::json!({"session_id": flooded.as_str()}),
        )?)
        .await?;
    assert!(response(&mut h.client).await?.error.is_none());
    for n in 0..4000 {
        h.senders[0].send(frame(&flooded, n)).await?;
    }
    h.client
        .send(request("q", METHOD_SESSION_LIST, serde_json::Value::Null)?)
        .await?;
    let mut frames_before = 0usize;
    loop {
        match receive(&mut h.client).await? {
            Received::Frame(_) => frames_before += 1,
            Received::Response(response) => {
                assert_eq!(response.id, "q");
                break;
            }
            Received::Closed(code) => return Err(format!("closed {code:?}").into()),
        }
    }
    assert!(
        frames_before <= buffer * 4 + 64,
        "response waited behind {frames_before} frames"
    );
    Ok(())
}

#[tokio::test]
async fn revoked_frame_closes_the_connection() -> TestResult {
    // REV-01 (transport side)
    let mut h = harness(WsLimits::default(), 1, 8).await;
    say_hello(&mut h.client).await?;
    let s = session("r")?;
    h.client
        .send(request(
            "a",
            METHOD_SESSION_ATTACH,
            serde_json::json!({"session_id": s.as_str()}),
        )?)
        .await?;
    assert!(response(&mut h.client).await?.error.is_none());
    h.senders[0]
        .send(FrameEnvelope {
            session_id: s.clone(),
            cursor: Cursor::default(),
            frame: SessionFrame::Revoked,
        })
        .await?;
    assert!(matches!(receive(&mut h.client).await?, Received::Frame(_)));
    assert!(matches!(
        receive(&mut h.client).await?,
        Received::Closed(Some(code)) if code == CloseReason::Revoked.code()
    ));
    assert_eq!(h.server.await?, ConnectionEnd::Closed(CloseReason::Revoked));
    Ok(())
}

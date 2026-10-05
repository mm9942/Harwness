//! Connecting, upgrade, hello and request correlation (S06).
//!
//! One [`RemoteConnection`] is one upgraded `harw.session.v1` WebSocket that
//! carries many sessions (R1). Layout:
//!
//! - A single supervisor task owns the socket. It reads and writes
//!   concurrently (the socket is split), so a large outbound message can never
//!   deadlock against inbound traffic.
//! - Callers register a pending request (unique id) in shared state, hand the
//!   envelope to the writer, and await a oneshot. The reader resolves it when
//!   the response with that id arrives, so any number of requests can be in
//!   flight and answered in any order.
//! - `event.frame` notifications are routed by session id to the
//!   [`RemoteFrames`] of that attachment. A response id the client never
//!   issued, a request from the host, or an undecodable message is a protocol
//!   violation and ends the connection (fail closed).
//! - When the connection ends, every pending request and every frame stream
//!   fails with the same typed [`PortError`]; later calls fail immediately
//!   with it too.
//!
//! Slow consumers (R6): each attachment has a bounded frame queue. When a
//! consumer lets it fill up, the queue ends with a [`SessionFrame::Lagged`]
//! marker whose `resume_from` is the cursor of the first frame that was not
//! queued, and the client stops delivering to that attachment. The reader
//! never blocks on a consumer, so the host is never back-pressured by it. The
//! client never invents a position: the marker carries a cursor the host sent
//! and re-attaching from it replays the dropped frames from the transcript.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::{SinkExt, StreamExt};
use harw_node_transport::{LocalNode, NodeTransportClient, NodeVerifier, UpgradeError, empty_body};
use harw_protocol::methods::{METHOD_SESSION_HELLO, NOTIF_EVENT_FRAME};
use harw_protocol::session_wire::{HelloAck, HelloParams, SESSION_WIRE_MINOR};
use harw_protocol::{
    ClientCaps, FrameEnvelope, FrameSource, PortError, PortFuture, ProtocolVersion,
    RequestEnvelope, ResponseEnvelope, SessionFrame,
};
use harw_session_ws::codec::{Inbound, decode, decode_frame_notification, encode_request};
use harw_session_ws::{Role, WebSocketStream, WsLimits, upgrade};
use harw_types::{NodeId, SessionId};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::RemoteError;

/// Frames queued per attachment before the consumer is declared lagging.
const FRAME_QUEUE_CAP: usize = 1024;

/// Requests buffered between callers and the writer.
const OUTBOUND_BUFFER: usize = 64;

/// Slack kept for the request envelope when checking the outbound size cap.
const ENVELOPE_SLACK: usize = 512;

/// Where a host is reached. An endpoint is a locator only; identity comes
/// from the transport (`SO_PEERCRED` on the host side for UDS, the pinned
/// node key for the node transport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Local AF_UNIX socket path.
    Unix(PathBuf),
    /// Own-cloud node.
    Node(NodeEndpoint),
}

/// Own-cloud node locator plus the node identity it must prove.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEndpoint {
    /// Where to dial (not identity).
    pub addr: SocketAddr,
    /// Node that must authenticate there (pinned key via the verifier).
    pub expected_node: NodeId,
}

/// Options shared by every connect path.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// Presence label sent in `session.hello` (audit/display only).
    pub client_label: String,
    /// Caps to request; `None` asks for the full ceiling (can only narrow).
    pub requested_caps: Option<ClientCaps>,
    /// Client-side WebSocket limits.
    pub limits: WsLimits,
}

impl ConnectOptions {
    /// Defaults for `client_label`.
    #[must_use]
    pub fn new(client_label: impl Into<String>) -> Self {
        Self {
            client_label: client_label.into(),
            requested_caps: None,
            limits: WsLimits::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// What a successful response changes in the attachment map. Applied by the
/// reader in the same step that resolves the request, so no frame sent after
/// the response can arrive before the subscription exists.
pub(crate) enum Effect {
    None,
    Subscribe(SessionId, Arc<Subscription>),
    Unsubscribe(SessionId),
}

struct Pending {
    reply: oneshot::Sender<Result<Value, PortError>>,
    effect: Effect,
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Pending>,
    subs: HashMap<SessionId, Arc<Subscription>>,
    /// Set once; after that no request is accepted.
    end: Option<PortError>,
}

struct Shared {
    state: Mutex<State>,
    next_id: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            next_id: AtomicU64::new(1),
        }
    }

    fn end_error(&self) -> PortError {
        lock(&self.state)
            .end
            .clone()
            .unwrap_or_else(|| PortError::Transport("connection closed".to_owned()))
    }

    /// Resolve the request a response answers. An id the client did not issue
    /// (or already answered) is a protocol violation.
    fn on_response(&self, response: ResponseEnvelope) -> Result<(), PortError> {
        let ResponseEnvelope {
            id, result, error, ..
        } = response;
        let (pending, replaced) = {
            let mut state = lock(&self.state);
            let Some(pending) = state.pending.remove(&id) else {
                return Err(PortError::Protocol(format!(
                    "response for unknown request id {id:?}"
                )));
            };
            let mut replaced = None;
            if error.is_none() {
                match &pending.effect {
                    Effect::Subscribe(session, sub) => {
                        replaced = state.subs.insert(session.clone(), Arc::clone(sub));
                    }
                    Effect::Unsubscribe(session) => replaced = state.subs.remove(session),
                    Effect::None => {}
                }
            }
            (pending, replaced)
        };
        if let Some(old) = replaced {
            old.close(StreamEnd::Clean);
        }
        let outcome = match (error, result) {
            (Some(error), _) => Err(PortError::from_code(error.code, error.message)),
            (None, result) => Ok(result.unwrap_or(Value::Null)),
        };
        // The caller may have gone away; that is not an error here.
        let _ = pending.reply.send(outcome);
        Ok(())
    }

    /// Route one frame to its attachment. Frames of sessions without a
    /// subscription (detached, never attached) are dropped.
    fn on_frame(&self, envelope: FrameEnvelope) {
        let session = envelope.session_id.clone();
        let sub = lock(&self.state).subs.get(&session).cloned();
        let Some(sub) = sub else {
            tracing::trace!(session = %session.as_str(), "frame for a session without attachment");
            return;
        };
        if sub.push(envelope) == Push::Closed {
            let mut state = lock(&self.state);
            if state
                .subs
                .get(&session)
                .is_some_and(|current| Arc::ptr_eq(current, &sub))
            {
                state.subs.remove(&session);
            }
        }
    }

    /// Fail one request that was never sent.
    fn fail_request(&self, id: &str, error: PortError) {
        let pending = lock(&self.state).pending.remove(id);
        if let Some(pending) = pending {
            let _ = pending.reply.send(Err(error));
        }
    }

    /// The connection is over: fail everything, once.
    fn finish(&self, error: &PortError) {
        let (pending, subs) = {
            let mut state = lock(&self.state);
            if state.end.is_some() {
                return;
            }
            state.end = Some(error.clone());
            (
                std::mem::take(&mut state.pending),
                std::mem::take(&mut state.subs),
            )
        };
        for (_, pending) in pending {
            let _ = pending.reply.send(Err(error.clone()));
        }
        for (_, sub) in subs {
            sub.close(StreamEnd::Error(error.clone()));
        }
    }
}

// ---------------------------------------------------------------------------
// Attachment frame queue
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum StreamEnd {
    Clean,
    Error(PortError),
}

#[derive(Debug, PartialEq, Eq)]
enum Push {
    Open,
    Closed,
}

#[derive(Default)]
struct SubState {
    queue: VecDeque<FrameEnvelope>,
    end: Option<StreamEnd>,
    consumer_gone: bool,
}

/// Bounded frame queue of one attachment.
pub(crate) struct Subscription {
    state: Mutex<SubState>,
    notify: Notify,
}

impl Subscription {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SubState::default()),
            notify: Notify::new(),
        })
    }

    fn push(&self, envelope: FrameEnvelope) -> Push {
        let mut state = lock(&self.state);
        if state.consumer_gone || state.end.is_some() {
            return Push::Closed;
        }
        let result = if state.queue.len() >= FRAME_QUEUE_CAP {
            // Lossy for the live stream only: mark where the consumer must
            // resume from (a cursor the host sent) and stop delivering.
            let marker = FrameEnvelope {
                session_id: envelope.session_id.clone(),
                cursor: envelope.cursor,
                frame: SessionFrame::Lagged {
                    resume_from: envelope.cursor,
                },
            };
            state.queue.push_back(marker);
            state.end = Some(StreamEnd::Clean);
            Push::Closed
        } else {
            let terminal = matches!(
                envelope.frame,
                SessionFrame::Lagged { .. }
                    | SessionFrame::Revoked
                    | SessionFrame::HostDraining { .. }
            );
            state.queue.push_back(envelope);
            if terminal {
                state.end = Some(StreamEnd::Clean);
                Push::Closed
            } else {
                Push::Open
            }
        };
        drop(state);
        self.notify.notify_one();
        result
    }

    fn close(&self, end: StreamEnd) {
        let mut state = lock(&self.state);
        if state.end.is_none() {
            state.end = Some(end);
        }
        drop(state);
        self.notify.notify_one();
    }

    async fn next(&self) -> Result<Option<FrameEnvelope>, PortError> {
        loop {
            {
                let mut state = lock(&self.state);
                if let Some(frame) = state.queue.pop_front() {
                    return Ok(Some(frame));
                }
                match state.end.clone() {
                    Some(StreamEnd::Clean) => return Ok(None),
                    Some(StreamEnd::Error(error)) => {
                        // Report the failure once; afterwards the stream is over.
                        state.end = Some(StreamEnd::Clean);
                        return Err(error);
                    }
                    None => {}
                }
            }
            self.notify.notified().await;
        }
    }
}

/// The frames of one attachment, as a [`FrameSource`].
pub struct RemoteFrames {
    sub: Arc<Subscription>,
}

impl fmt::Debug for RemoteFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RemoteFrames")
    }
}

impl RemoteFrames {
    pub(crate) fn new(sub: Arc<Subscription>) -> Self {
        Self { sub }
    }
}

impl FrameSource for RemoteFrames {
    fn next(&mut self) -> PortFuture<'_, Option<FrameEnvelope>> {
        Box::pin(self.sub.next())
    }
}

impl Drop for RemoteFrames {
    fn drop(&mut self) {
        // The reader stops delivering to an attachment nobody reads.
        lock(&self.sub.state).consumer_gone = true;
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// An upgraded, hello-completed connection. Owned by [`crate::RemotePort`].
pub struct RemoteConnection {
    shared: Arc<Shared>,
    out: mpsc::Sender<RequestEnvelope>,
    hello: HelloAck,
    max_message_bytes: usize,
}

impl fmt::Debug for RemoteConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConnection")
            .field("hello", &self.hello)
            .finish_non_exhaustive()
    }
}

impl RemoteConnection {
    /// The host's `session.hello` acknowledgement for this connection.
    #[must_use]
    pub fn hello_ack(&self) -> &HelloAck {
        &self.hello
    }

    /// Send one request and await its response (correlated by id).
    pub(crate) async fn request(
        &self,
        method: &'static str,
        params: Value,
        effect: Effect,
    ) -> Result<Value, PortError> {
        let params_len = serde_json::to_vec(&params)
            .map_err(|error| PortError::Protocol(format!("encode params: {error}")))?
            .len();
        if params_len.saturating_add(ENVELOPE_SLACK) > self.max_message_bytes {
            return Err(PortError::Protocol(format!(
                "request exceeds the {} byte message limit",
                self.max_message_bytes
            )));
        }
        let id = format!("c{}", self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = oneshot::channel();
        {
            let mut state = lock(&self.shared.state);
            if let Some(error) = &state.end {
                return Err(error.clone());
            }
            state.pending.insert(id.clone(), Pending { reply, effect });
        }
        let envelope = RequestEnvelope {
            jsonrpc: "2.0".to_owned(),
            id: id.clone(),
            method: method.to_owned(),
            params,
            protocol: ProtocolVersion::default(),
        };
        if self.out.send(envelope).await.is_err() {
            self.shared.fail_request(&id, self.shared.end_error());
            return Err(self.shared.end_error());
        }
        match answer.await {
            Ok(outcome) => outcome,
            Err(_) => Err(self.shared.end_error()),
        }
    }

    /// [`Self::request`] with typed params and result.
    pub(crate) async fn call<P, R>(
        &self,
        method: &'static str,
        params: &P,
        effect: Effect,
    ) -> Result<R, PortError>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let params = serde_json::to_value(params)
            .map_err(|error| PortError::Protocol(format!("encode params: {error}")))?;
        let result = self.request(method, params, effect).await?;
        serde_json::from_value(result)
            .map_err(|error| PortError::Protocol(format!("undecodable {method} result: {error}")))
    }
}

/// Take over an upgraded client socket: start the supervisor and complete
/// `session.hello`.
async fn start<S>(
    ws: WebSocketStream<S>,
    options: &ConnectOptions,
) -> Result<RemoteConnection, RemoteError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let limits = options.limits;
    let shared = Arc::new(Shared::new());
    let (out, mut out_rx) = mpsc::channel::<RequestEnvelope>(OUTBOUND_BUFFER);
    let (mut sink, mut stream) = ws.split();

    let task_shared = Arc::clone(&shared);
    tokio::spawn(async move {
        let reader = async {
            loop {
                let Some(item) = stream.next().await else {
                    break PortError::Transport("host closed the connection".to_owned());
                };
                let message = match item {
                    Ok(message) => message,
                    Err(error) => break PortError::Transport(format!("websocket: {error}")),
                };
                let close_detail = message.is_close().then(|| format!("{message:?}"));
                match decode(message, &limits) {
                    Ok(Inbound::Response(response)) => {
                        if let Err(error) = task_shared.on_response(response) {
                            break error;
                        }
                    }
                    Ok(Inbound::Notification(notification)) => {
                        if notification.method == NOTIF_EVENT_FRAME {
                            match decode_frame_notification(&notification) {
                                Ok(envelope) => task_shared.on_frame(envelope),
                                Err(error) => break PortError::Protocol(error.to_string()),
                            }
                        } else {
                            tracing::debug!(
                                method = %notification.method,
                                "ignoring unknown notification"
                            );
                        }
                    }
                    Ok(Inbound::Request(_)) => {
                        break PortError::Protocol("host sent a request".to_owned());
                    }
                    Ok(Inbound::Ping(_) | Inbound::Pong) => {}
                    Ok(Inbound::Close) => {
                        break PortError::Transport(format!(
                            "host closed the connection: {}",
                            close_detail.unwrap_or_default()
                        ));
                    }
                    Err(error) => break PortError::Protocol(error.to_string()),
                }
            }
        };
        let writer = async {
            loop {
                let Some(request) = out_rx.recv().await else {
                    let _ = sink.close().await;
                    break PortError::Transport("connection closed locally".to_owned());
                };
                match encode_request(&request) {
                    Ok(message) => {
                        if let Err(error) = sink.send(message).await {
                            break PortError::Transport(format!("websocket: {error}"));
                        }
                    }
                    Err(error) => {
                        task_shared
                            .fail_request(&request.id, PortError::Protocol(error.to_string()));
                    }
                }
            }
        };
        let end = tokio::select! {
            end = reader => end,
            end = writer => end,
        };
        task_shared.finish(&end);
    });

    let mut connection = RemoteConnection {
        shared,
        out,
        hello: HelloAck {
            wire_minor: 0,
            features: Vec::new(),
            host_epoch: 0,
            granted: ClientCaps::NONE,
        },
        max_message_bytes: limits.max_message_bytes,
    };
    let params = HelloParams {
        client_label: options.client_label.clone(),
        wire_minor: SESSION_WIRE_MINOR,
        features: Vec::new(),
        requested_caps: options.requested_caps,
    };
    connection.hello = connection
        .call::<_, HelloAck>(METHOD_SESSION_HELLO, &params, Effect::None)
        .await
        .map_err(|error| RemoteError::Connect(format!("session.hello failed: {error}")))?;
    Ok(connection)
}

fn upgrade_error(error: UpgradeError) -> RemoteError {
    match error {
        UpgradeError::NotImplemented(what) => RemoteError::NotImplemented(what),
        other => RemoteError::Connect(format!("upgrade: {other}")),
    }
}

/// Upgrade over any duplex byte stream (already connected and, where the
/// transport needs it, authenticated) and complete `session.hello`.
///
/// [`connect_unix`] is this over a `UnixStream`; tests use an in-memory
/// duplex.
///
/// # Errors
/// [`RemoteError::Connect`] when the HTTP upgrade is refused, the host picks
/// another subprotocol, hello fails or the whole attempt exceeds
/// `options.limits.hello_timeout`.
pub async fn connect_io<I>(io: I, options: ConnectOptions) -> Result<RemoteConnection, RemoteError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let limits = options.limits;
    let attempt = async {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(|error| RemoteError::Connect(format!("http handshake: {error}")))?;
        tokio::spawn(async move {
            let _ = connection.with_upgrades().await;
        });
        let (request, key) = upgrade::client_request("localhost")
            .map_err(|error| RemoteError::Connect(format!("upgrade request: {error}")))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| RemoteError::Connect(format!("upgrade request: {error}")))?;
        verify(&response, &key)?;
        let upgraded = hyper::upgrade::on(response)
            .await
            .map_err(|error| RemoteError::Connect(format!("upgrade: {error}")))?;
        let ws = WebSocketStream::from_raw_socket(
            TokioIo::new(upgraded),
            Role::Client,
            Some(limits.tungstenite_config()),
        )
        .await;
        start(ws, &options).await
    };
    match tokio::time::timeout(limits.hello_timeout, attempt).await {
        Ok(result) => result,
        Err(_) => Err(RemoteError::Connect(
            "timed out upgrading and completing session.hello".to_owned(),
        )),
    }
}

fn verify<B>(response: &http::Response<B>, key: &str) -> Result<(), RemoteError> {
    upgrade::verify_client_response(response, key).map_err(|rejection| {
        RemoteError::Connect(format!(
            "host did not accept the session upgrade (HTTP {}): {rejection}",
            response.status()
        ))
    })
}

/// Connect to a local host socket, upgrade and complete `session.hello`.
///
/// # Errors
/// [`RemoteError::Connect`] when the socket cannot be reached, the upgrade is
/// refused or hello fails.
pub async fn connect_unix(
    path: PathBuf,
    options: ConnectOptions,
) -> Result<RemoteConnection, RemoteError> {
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|error| RemoteError::Connect(format!("connect {}: {error}", path.display())))?;
    connect_io(stream, options).await
}

/// Connect to an own-cloud node over the authenticated node transport,
/// upgrade (via `NodeTransportClient::upgrade`, S02) and complete
/// `session.hello`.
///
/// The pinned node identity is verified by the node transport before any
/// upgraded stream exists; no provider credential is sent.
///
/// # Errors
/// [`RemoteError::Connect`] for transport, identity or upgrade failures;
/// [`RemoteError::NotImplemented`] while the node transport upgrade (S02) is
/// still a stub.
pub async fn connect_node(
    endpoint: NodeEndpoint,
    local: &LocalNode,
    verifier: &dyn NodeVerifier,
    options: ConnectOptions,
) -> Result<RemoteConnection, RemoteError> {
    let limits = options.limits;
    let attempt = async {
        let client =
            NodeTransportClient::connect(endpoint.addr, local, &endpoint.expected_node, verifier)
                .await
                .map_err(|error| RemoteError::Connect(format!("node transport: {error}")))?;
        let (request, key) = upgrade::client_request("localhost")
            .map_err(|error| RemoteError::Connect(format!("upgrade request: {error}")))?;
        let request = request.map(|_| empty_body());
        let upgraded = client.upgrade(request).await.map_err(upgrade_error)?;
        verify(&upgraded.response, &key)?;
        let ws = WebSocketStream::from_raw_socket(
            upgraded.io,
            Role::Client,
            Some(limits.tungstenite_config()),
        )
        .await;
        start(ws, &options).await
    };
    match tokio::time::timeout(limits.hello_timeout, attempt).await {
        Ok(result) => result,
        Err(_) => Err(RemoteError::Connect(
            "timed out upgrading and completing session.hello".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_socket_is_a_typed_connect_error() {
        let result = connect_unix(
            PathBuf::from("/nonexistent/harw.sock"),
            ConnectOptions::new("t"),
        )
        .await;
        assert!(matches!(result, Err(RemoteError::Connect(_))));
    }

    fn heartbeat(session: &SessionId, live: u32) -> FrameEnvelope {
        FrameEnvelope {
            session_id: session.clone(),
            cursor: harw_protocol::Cursor {
                generation: 0,
                durable: 0,
                live,
            },
            frame: SessionFrame::Heartbeat,
        }
    }

    /// A consumer that lets the queue fill gets a `Lagged` marker at the first
    /// frame that did not fit, then the stream ends (R6).
    #[tokio::test]
    async fn full_frame_queue_ends_with_a_lagged_marker() -> Result<(), String> {
        let session = SessionId::try_from_str("s-1").map_err(|error| error.to_string())?;
        let sub = Subscription::new();
        let total = u32::try_from(FRAME_QUEUE_CAP).map_err(|error| error.to_string())?;
        for live in 0..total {
            assert_eq!(sub.push(heartbeat(&session, live)), Push::Open);
        }
        // The next frame does not fit: marker, then closed.
        assert_eq!(sub.push(heartbeat(&session, total)), Push::Closed);
        assert_eq!(sub.push(heartbeat(&session, total + 1)), Push::Closed);

        for live in 0..total {
            let frame = sub.next().await.map_err(|error| error.to_string())?;
            let frame = frame.ok_or("stream ended early")?;
            assert_eq!(frame.cursor.live, live);
        }
        let marker = sub.next().await.map_err(|error| error.to_string())?;
        let marker = marker.ok_or("no marker")?;
        match marker.frame {
            SessionFrame::Lagged { resume_from } => assert_eq!(resume_from.live, total),
            other => return Err(format!("expected Lagged, got {other:?}")),
        }
        assert!(
            sub.next()
                .await
                .map_err(|error| error.to_string())?
                .is_none()
        );
        Ok(())
    }

    /// A failed connection is reported to a stream once, then it is over.
    #[tokio::test]
    async fn connection_failure_is_reported_once() -> Result<(), String> {
        let sub = Subscription::new();
        sub.close(StreamEnd::Error(PortError::Transport("gone".into())));
        let first = sub.next().await;
        assert!(
            matches!(&first, Err(PortError::Transport(detail)) if detail == "gone"),
            "{first:?}"
        );
        let second = sub.next().await;
        assert!(matches!(second, Ok(None)), "{second:?}");
        Ok(())
    }
}

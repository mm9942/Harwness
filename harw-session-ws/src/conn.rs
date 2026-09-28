//! Connection driver (W00 §3.2–§3.4, §4).
//!
//! One task per WebSocket connection. It owns the socket and runs a single
//! `select!` loop over:
//! 1. shutdown (drain),
//! 2. inbound messages (requests → spawned dispatch tasks),
//! 3. finished dispatches (responses; always ahead of frames),
//! 4. attachment frames (fair: every attachment holds at most
//!    [`WsLimits::attachment_buffer`] frames in the shared outbound channel,
//!    so a delta flood on one session cannot starve another session or an
//!    RPC response — WS-06, WS-07),
//! 5. keep-alive ping and idle timeout.
//!
//! Hello gate: until a `session.hello` succeeds, every other method gets
//! `HELLO_REQUIRED` without reaching the port, and the connection closes
//! when hello does not succeed within [`WsLimits::hello_timeout`].
//!
//! R18: the caps granted in the hello acknowledgement are kept for the
//! dispatcher's cap gate on `tool.*`/`gateway.*` ([`crate::dispatch::required_cap`]);
//! an acknowledgement that does not decode grants nothing (fail closed).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use harw_protocol::session_wire::{HelloAck, error_codes};
use harw_protocol::{
    ClientCaps, FrameEnvelope, FrameSource, RequestEnvelope, SessionFrame, SessionPort,
};
use harw_types::SessionId;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::codec::{CloseReason, Inbound, decode, encode_frame, encode_response, error_response};
use crate::dispatch::{Dispatched, Ports, dispatch_ports, is_hello};
use crate::limits::WsLimits;

/// How a connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionEnd {
    /// This side closed the connection for `reason`.
    Closed(CloseReason),
    /// The peer closed the connection or the stream ended.
    PeerClosed,
    /// Reading or writing the socket failed.
    Transport(String),
}

/// One frame travelling from an attachment pump to the writer. The permit is
/// the attachment's buffer slot; it is released once the frame is written.
struct Outbound {
    session: SessionId,
    epoch: u64,
    envelope: FrameEnvelope,
    _slot: OwnedSemaphorePermit,
}

struct Attachment {
    epoch: u64,
    pump: JoinHandle<()>,
}

/// A finished dispatch plus the request id it answers.
struct Finished {
    request_id: String,
    dispatched: Dispatched,
}

/// Drive one upgraded WebSocket connection against `port` until it ends.
///
/// `port` is already bound to the connection's authenticated identity; this
/// function never looks at identity. `shutdown` flipping to `true` closes the
/// connection with [`CloseReason::Draining`]. Session methods only; use
/// [`serve_connection_with`] to also serve `tool.*` and `gateway.*`.
pub async fn serve_connection<S>(
    ws: WebSocketStream<S>,
    port: Arc<dyn SessionPort>,
    limits: WsLimits,
    shutdown: watch::Receiver<bool>,
) -> ConnectionEnd
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_connection_with(ws, Ports::session_only(port), limits, shutdown).await
}

/// Like [`serve_connection`], serving every table `ports` offers (R18).
pub async fn serve_connection_with<S>(
    mut ws: WebSocketStream<S>,
    ports: Ports,
    limits: WsLimits,
    mut shutdown: watch::Receiver<bool>,
) -> ConnectionEnd
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let in_flight = Arc::new(Semaphore::new(limits.max_in_flight.max(1)));
    let (done_tx, mut done_rx) = mpsc::channel::<Finished>(limits.response_buffer.max(1));
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Outbound>();

    let mut attachments: HashMap<SessionId, Attachment> = HashMap::new();
    let mut in_flight_ids: HashSet<String> = HashSet::new();
    let mut next_epoch: u64 = 0;
    let mut hello_done = false;
    let mut hello_pending: Option<String> = None;
    let mut granted = ClientCaps::NONE;

    let hello_deadline = Instant::now() + limits.hello_timeout;
    let mut last_inbound = Instant::now();
    let mut ping = interval(limits.ping_interval);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ping.reset();

    let end = loop {
        let idle_deadline = last_inbound + limits.idle_timeout;
        tokio::select! {
            biased;

            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break close(&mut ws, CloseReason::Draining).await;
                }
            }

            () = sleep_until(hello_deadline), if !hello_done => {
                break close(&mut ws, CloseReason::HelloTimeout).await;
            }

            () = sleep_until(idle_deadline) => {
                break close(&mut ws, CloseReason::IdleTimeout).await;
            }

            inbound = ws.next() => {
                let Some(inbound) = inbound else { break ConnectionEnd::PeerClosed };
                let message = match inbound {
                    Ok(message) => message,
                    Err(error) => break ConnectionEnd::Transport(error.to_string()),
                };
                last_inbound = Instant::now();
                match decode(message, &limits) {
                    Ok(Inbound::Request(request)) => {
                        let outcome = admit_request(
                            request,
                            &ports,
                            granted,
                            &limits,
                            &in_flight,
                            &done_tx,
                            &mut in_flight_ids,
                            &mut hello_pending,
                            hello_done,
                            attachments.len(),
                        );
                        if let Some(response) = outcome
                            && let Err(end) = send(&mut ws, encode_response(&response)).await
                        {
                            break end;
                        }
                    }
                    // v1 servers send no requests, so any response or
                    // notification from the client is a protocol violation
                    // (WS-05: unknown response id → fail closed).
                    Ok(Inbound::Response(_) | Inbound::Notification(_)) => {
                        break close(&mut ws, CloseReason::ProtocolError).await;
                    }
                    Ok(Inbound::Ping(_) | Inbound::Pong) => {}
                    Ok(Inbound::Close) => break ConnectionEnd::PeerClosed,
                    Err(error) => break close(&mut ws, error.close_reason()).await,
                }
            }

            finished = done_rx.recv() => {
                let Some(finished) = finished else { break ConnectionEnd::Transport("dispatch channel closed".into()) };
                in_flight_ids.remove(&finished.request_id);
                if hello_pending.as_deref() == Some(finished.request_id.as_str()) {
                    hello_pending = None;
                    if let Dispatched::Response(response) = &finished.dispatched {
                        hello_done = response.error.is_none();
                        granted = granted_caps(response);
                    }
                }
                let result = match finished.dispatched {
                    Dispatched::Response(response) => send(&mut ws, encode_response(&response)).await,
                    Dispatched::Attached { response, session_id, source } => {
                        let sent = send(&mut ws, encode_response(&response)).await;
                        if sent.is_ok() {
                            next_epoch += 1;
                            if let Some(old) = attachments.insert(
                                session_id.clone(),
                                Attachment {
                                    epoch: next_epoch,
                                    pump: spawn_pump(session_id, next_epoch, source, frame_tx.clone(), limits.attachment_buffer),
                                },
                            ) {
                                old.pump.abort();
                            }
                        }
                        sent
                    }
                    Dispatched::Detached { response, session_id } => {
                        if let Some(old) = attachments.remove(&session_id) {
                            old.pump.abort();
                        }
                        send(&mut ws, encode_response(&response)).await
                    }
                };
                if let Err(end) = result {
                    break end;
                }
            }

            frame = frame_rx.recv() => {
                let Some(outbound) = frame else { continue };
                let current = attachments.get(&outbound.session).map(|attachment| attachment.epoch);
                if current != Some(outbound.epoch) {
                    // Frame of a detached or replaced attachment.
                    continue;
                }
                let terminal = match &outbound.envelope.frame {
                    SessionFrame::Revoked => Some(CloseReason::Revoked),
                    SessionFrame::HostDraining { .. } => Some(CloseReason::Draining),
                    _ => None,
                };
                if let Err(end) = send(&mut ws, encode_frame(&outbound.envelope)).await {
                    break end;
                }
                if let Some(reason) = terminal {
                    break close(&mut ws, reason).await;
                }
            }

            _ = ping.tick() => {
                if let Err(error) = ws.send(Message::Ping(Default::default())).await {
                    break ConnectionEnd::Transport(error.to_string());
                }
            }
        }
    };

    for (session, attachment) in attachments.drain() {
        attachment.pump.abort();
        // Best effort: the host also cleans up when the stream is dropped.
        let _ = tokio::time::timeout(limits.hello_timeout, ports.session.detach(session)).await;
    }
    end
}

/// Caps from a successful hello acknowledgement; anything else grants
/// nothing.
fn granted_caps(response: &harw_protocol::ResponseEnvelope) -> ClientCaps {
    if response.error.is_some() {
        return ClientCaps::NONE;
    }
    response
        .result
        .clone()
        .and_then(|result| serde_json::from_value::<HelloAck>(result).ok())
        .map_or(ClientCaps::NONE, |ack| ack.granted)
}

/// Check one request against the hello gate, duplicate ids and the
/// in-flight bound. Returns an immediate error response, or `None` when a
/// dispatch task was spawned.
#[allow(clippy::too_many_arguments)]
fn admit_request(
    request: RequestEnvelope,
    ports: &Ports,
    granted: ClientCaps,
    limits: &WsLimits,
    in_flight: &Arc<Semaphore>,
    done_tx: &mpsc::Sender<Finished>,
    in_flight_ids: &mut HashSet<String>,
    hello_pending: &mut Option<String>,
    hello_done: bool,
    attached: usize,
) -> Option<harw_protocol::ResponseEnvelope> {
    let id = request.id.clone();
    if in_flight_ids.contains(&id) {
        return Some(error_response(
            id,
            error_codes::INVALID_PARAMS,
            "duplicate request id while in flight",
        ));
    }
    let hello = is_hello(&request.method);
    if hello && (hello_done || hello_pending.is_some()) {
        return Some(error_response(
            id,
            error_codes::INVALID_PARAMS,
            "session.hello already sent",
        ));
    }
    if !hello && !hello_done {
        return Some(error_response(
            id,
            error_codes::HELLO_REQUIRED,
            "session.hello required first",
        ));
    }
    if request.method == harw_protocol::methods::METHOD_SESSION_ATTACH
        && attached >= limits.max_attachments
    {
        return Some(error_response(
            id,
            error_codes::BUSY,
            "too many attached sessions",
        ));
    }
    let Ok(permit) = Arc::clone(in_flight).try_acquire_owned() else {
        return Some(error_response(
            id,
            error_codes::BUSY,
            "too many requests in flight",
        ));
    };
    in_flight_ids.insert(id.clone());
    if hello {
        *hello_pending = Some(id.clone());
    }
    let ports = ports.clone();
    let done_tx = done_tx.clone();
    tokio::spawn(async move {
        let dispatched = dispatch_ports(&ports, granted, request).await;
        // Detach of an unknown session still answers; the writer ignores it.
        let _ = done_tx
            .send(Finished {
                request_id: id,
                dispatched,
            })
            .await;
        drop(permit);
    });
    None
}

/// Forward frames of one attachment into the shared outbound channel. Each
/// frame first takes one of `buffer` slots, so this attachment never holds
/// more than `buffer` frames ahead of the writer.
fn spawn_pump(
    session: SessionId,
    epoch: u64,
    mut source: Box<dyn FrameSource>,
    frame_tx: mpsc::UnboundedSender<Outbound>,
    buffer: usize,
) -> JoinHandle<()> {
    let slots = Arc::new(Semaphore::new(buffer.max(1)));
    tokio::spawn(async move {
        loop {
            let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
                break;
            };
            match source.next().await {
                Ok(Some(envelope)) => {
                    let outbound = Outbound {
                        session: session.clone(),
                        epoch,
                        envelope,
                        _slot: slot,
                    };
                    if frame_tx.send(outbound).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
    })
}

async fn send<S>(
    ws: &mut WebSocketStream<S>,
    message: Result<Message, crate::codec::CodecError>,
) -> Result<(), ConnectionEnd>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let message = match message {
        Ok(message) => message,
        Err(error) => {
            tracing::warn!(?error, "session ws: dropping unencodable message");
            return Ok(());
        }
    };
    ws.send(message)
        .await
        .map_err(|error| ConnectionEnd::Transport(error.to_string()))
}

async fn close<S>(ws: &mut WebSocketStream<S>, reason: CloseReason) -> ConnectionEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = ws.close(Some(reason.to_close_frame())).await;
    ConnectionEnd::Closed(reason)
}

#[cfg(test)]
mod tests;

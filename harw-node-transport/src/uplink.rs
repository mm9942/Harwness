//! DoD uplink (WP-15): a read-only security event stream from a node to the
//! central SecurityHub.
//!
//! * **Read-only.** Events flow node → hub only. The receiving side answers
//!   with a single status for the whole stream; there is no command channel
//!   back through the uplink, and nothing here can reach the Warden or a
//!   sensor (masterplan §20: no local DoD TCB widening).
//! * **Identity from the channel, never the payload.** Events carry no node
//!   id. The receiver attributes them to the [`crate::AuthenticatedPeer`]
//!   of the connection.
//! * **Wire format.** One JSON object per line (`\n`-terminated) in a
//!   streaming HTTP/1 body: `POST` [`UPLINK_PATH`] with content type
//!   [`UPLINK_CONTENT_TYPE`].
//! * **Bounded.** The sender refuses oversized events and never blocks (a
//!   full queue drops the event with [`UplinkError::Backpressure`]); the
//!   receiver enforces per-line, per-stream byte and event-count limits
//!   before parsing, so an oversized line is never buffered past the limit.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::client::{BoxError, NodeBody};
use crate::error::{TransportError, UplinkError};

/// Request path of the uplink stream.
pub const UPLINK_PATH: &str = "/v1/dod/uplink";

/// Content type of the uplink stream.
pub const UPLINK_CONTENT_TYPE: &str = "application/x-ndjson";

/// Severity of a finding summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    /// Informational.
    Info,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Critical.
    Critical,
}

/// Coarse health of the sending node's DoD stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    /// Sentinel and sensors report normally.
    Ok,
    /// Some sensors are missing or lagging.
    Degraded,
    /// The local DoD stack is not functional.
    Failing,
}

/// One uplink event.
///
/// Deliberately a summary: raw sensor records stay on the node. No field
/// names the sending node — that comes from the authenticated channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UplinkEvent {
    /// A triaged Sentinel finding.
    FindingSummary {
        /// Node-local finding id.
        finding_id: String,
        /// Rule that produced it.
        rule_id: String,
        /// Triage severity.
        severity: FindingSeverity,
        /// One-line human summary.
        summary: String,
        /// When the finding was raised (Unix ms).
        observed_at_unix_ms: u64,
    },
    /// Liveness beat of the node's DoD stack.
    HealthBeat {
        /// Monotonic per-stream sequence number.
        sequence: u64,
        /// Health at the time of the beat.
        status: HealthStatus,
        /// When the beat was produced (Unix ms).
        observed_at_unix_ms: u64,
    },
}

/// Size limits of one uplink stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UplinkLimits {
    /// Largest encoded event line, including the trailing `\n`.
    pub max_line_bytes: usize,
    /// Most events per stream.
    pub max_events: u64,
    /// Most bytes per stream.
    pub max_stream_bytes: u64,
}

impl Default for UplinkLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 16 * 1024,
            max_events: 100_000,
            max_stream_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Encodes one event as a JSON line, refusing it if it exceeds
/// `max_line_bytes`.
///
/// # Errors
/// [`UplinkError::LineTooLong`]; [`UplinkError::Malformed`] if serde fails.
pub fn encode_line(event: &UplinkEvent, max_line_bytes: usize) -> Result<Bytes, UplinkError> {
    let mut line = serde_json::to_vec(event).map_err(|err| UplinkError::Malformed {
        line: 0,
        reason: err.to_string(),
    })?;
    line.push(b'\n');
    if line.len() > max_line_bytes {
        return Err(UplinkError::LineTooLong {
            actual: line.len(),
            limit: max_line_bytes,
        });
    }
    Ok(Bytes::from(line))
}

/// Sender side of the uplink, as seen by the local SecurityHub/Sentinel
/// bridge.
pub trait DodUplink: Send + Sync {
    /// Queues `event` for the uplink without blocking.
    ///
    /// # Errors
    /// [`UplinkError::LineTooLong`] for oversized events,
    /// [`UplinkError::Backpressure`] when the queue is full (the event is
    /// dropped), [`UplinkError::Closed`] when the stream is gone.
    fn publish(&self, event: &UplinkEvent) -> Result<(), UplinkError>;
}

/// Queue-backed [`DodUplink`] feeding an [`UplinkBody`].
#[derive(Debug, Clone)]
pub struct UplinkSender {
    queue: mpsc::Sender<Bytes>,
    max_line_bytes: usize,
}

impl DodUplink for UplinkSender {
    fn publish(&self, event: &UplinkEvent) -> Result<(), UplinkError> {
        let line = encode_line(event, self.max_line_bytes)?;
        self.queue.try_send(line).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => UplinkError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => UplinkError::Closed,
        })
    }
}

/// Streaming HTTP body draining an [`UplinkSender`]'s queue. Ends when every
/// sender clone is dropped.
#[derive(Debug)]
pub struct UplinkBody {
    queue: mpsc::Receiver<Bytes>,
}

impl Body for UplinkBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.queue
            .poll_recv(cx)
            .map(|line| line.map(|bytes| Ok(Frame::data(bytes))))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

/// Creates a sender/body pair with a queue of `capacity` events (at least 1).
#[must_use]
pub fn uplink_channel(capacity: usize, limits: &UplinkLimits) -> (UplinkSender, UplinkBody) {
    let (queue, receiver) = mpsc::channel(capacity.max(1));
    (
        UplinkSender {
            queue,
            max_line_bytes: limits.max_line_bytes,
        },
        UplinkBody { queue: receiver },
    )
}

/// Builds the `POST` [`UPLINK_PATH`] request streaming `body`.
///
/// # Errors
/// [`TransportError::Request`] if the request can not be built.
pub fn uplink_request(body: UplinkBody) -> Result<http::Request<NodeBody>, TransportError> {
    Ok(http::Request::builder()
        .method(http::Method::POST)
        .uri(UPLINK_PATH)
        .header(http::header::CONTENT_TYPE, UPLINK_CONTENT_TYPE)
        .body(body.map_err(|never| -> BoxError { match never {} }).boxed())?)
}

/// What a receiver accepted from one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UplinkStats {
    /// Events delivered to the callback.
    pub events: u64,
    /// Body bytes read.
    pub bytes: u64,
}

/// Reads an uplink stream, handing each event to `on_event` in order.
///
/// Limits are checked on raw bytes before any JSON parsing. On error,
/// events already delivered stay delivered; the caller answers with
/// [`UplinkError::http_status`].
///
/// # Errors
/// Limit violations, malformed or truncated lines, body read failures.
pub async fn receive_uplink<B, F>(
    body: B,
    limits: &UplinkLimits,
    mut on_event: F,
) -> Result<UplinkStats, UplinkError>
where
    B: Body,
    B::Error: std::fmt::Display,
    F: FnMut(UplinkEvent),
{
    let mut body = std::pin::pin!(body);
    let mut pending: Vec<u8> = Vec::new();
    let mut stats = UplinkStats::default();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| UplinkError::Body(err.to_string()))?;
        let Ok(mut data) = frame.into_data() else {
            continue; // trailers carry nothing for us
        };
        let chunk_len = u64::try_from(data.remaining()).unwrap_or(u64::MAX);
        stats.bytes = stats.bytes.saturating_add(chunk_len);
        if stats.bytes > limits.max_stream_bytes {
            return Err(UplinkError::StreamTooLarge {
                limit: limits.max_stream_bytes,
            });
        }
        // Feed at most one line limit at a time so `pending` never grows
        // beyond twice the line limit, whatever the transport chunk size.
        while data.has_remaining() {
            let chunk = data.chunk();
            let take = chunk.len().min(limits.max_line_bytes.max(1));
            pending.extend_from_slice(&chunk[..take]);
            data.advance(take);
            drain_lines(&mut pending, limits, &mut stats, &mut on_event)?;
        }
    }
    if !pending.is_empty() {
        return Err(UplinkError::TruncatedLine);
    }
    Ok(stats)
}

fn drain_lines<F>(
    pending: &mut Vec<u8>,
    limits: &UplinkLimits,
    stats: &mut UplinkStats,
    on_event: &mut F,
) -> Result<(), UplinkError>
where
    F: FnMut(UplinkEvent),
{
    let mut start = 0usize;
    while let Some(offset) = pending[start..].iter().position(|b| *b == b'\n') {
        let end = start + offset + 1; // exclusive, includes '\n'
        let line_len = end - start;
        if line_len > limits.max_line_bytes {
            return Err(UplinkError::LineTooLong {
                actual: line_len,
                limit: limits.max_line_bytes,
            });
        }
        if stats.events >= limits.max_events {
            return Err(UplinkError::TooManyEvents {
                limit: limits.max_events,
            });
        }
        let line_no = stats.events + 1;
        let event: UplinkEvent =
            serde_json::from_slice(&pending[start..end - 1]).map_err(|err| {
                UplinkError::Malformed {
                    line: line_no,
                    reason: err.to_string(),
                }
            })?;
        stats.events = line_no;
        on_event(event);
        start = end;
    }
    pending.drain(..start);
    if pending.len() > limits.max_line_bytes {
        return Err(UplinkError::LineTooLong {
            actual: pending.len(),
            limit: limits.max_line_bytes,
        });
    }
    Ok(())
}

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
//! * **Ids, enums and counters only (PL-100 I4).** No event field carries
//!   free text: sensor strings (process names, paths, peers) and model
//!   output stay on the node. Every string field is an identifier with a
//!   fixed ASCII grammar, validated on construction *and* on decode
//!   ([`UplinkFindingId`], [`UplinkRuleId`], [`UplinkDedupKey`]); a value
//!   outside the grammar makes the whole line malformed. The
//!   [`UplinkError::Malformed`] reason names only the error class and the
//!   column, never a byte of the rejected line. Under the PL-99
//!   hub these events are the inner payload of the node uplink; the hub
//!   relays them opaquely.
//! * **Wire format.** One JSON object per line (`\n`-terminated) in a
//!   streaming HTTP/1 body: `POST` [`UPLINK_PATH`] with content type
//!   [`UPLINK_CONTENT_TYPE`].
//! * **Bounded.** Every event of this schema encodes to at most
//!   [`MAX_EVENT_LINE_BYTES`]. The sender refuses oversized events and never
//!   blocks (a full queue drops the event with [`UplinkError::Backpressure`]);
//!   the receiver enforces per-line, per-stream byte and event-count limits
//!   before parsing, so an oversized line is never buffered past the limit.
//!
//! # Event versions
//! The stream path stays `/v1/dod/uplink`; each event carries its schema
//! generation in its wire tag (`"type"`), and decoding rejects unknown tags
//! and unknown fields (`deny_unknown_fields`). Changing the field set of an
//! event therefore means a new tag, never an in-place change, and readers
//! learn a new tag before any writer emits it (PL-100 I11, reader-first).
//!
//! | Rust variant | Wire tag | Fields |
//! |---|---|---|
//! | [`UplinkEvent::FindingSummary`] | `finding_summary_v2` | `finding_id`, `rule_id`, `severity`, `hardness`, `kind`, `dedup_key`, `occurrences`, `observed_at_unix_ms`, optional `verdict` |
//! | [`UplinkEvent::HealthBeat`] | `health_beat_v2` | `sequence`, `status`, `observed_at_unix_ms`, `sensors_ok`, `sensors_degraded`, `untriaged_backlog`, `breaker_trips` |
//!
//! Generation 1 (`finding_summary` with a free-text `summary`,
//! `health_beat` without counters) is no longer accepted: its `summary`
//! violated I4, and no producer of it ever shipped (no caller of
//! [`DodUplink::publish`] existed). A generation-1 line is refused as
//! [`UplinkError::Malformed`].

use std::convert::Infallible;
use std::fmt;
use std::num::NonZeroU64;
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

/// Upper bound of one encoded event line, including the trailing `\n`, for
/// every event of this schema.
///
/// Every field is bounded (fixed-length ids, at most
/// [`MAX_RULE_ID_BYTES`] for the rule id, enums, integers), so the largest
/// possible [`UplinkEvent::FindingSummary`] encodes to 445 bytes and the
/// largest [`UplinkEvent::HealthBeat`] to 253 bytes. A receiver that only
/// accepts this schema can set [`UplinkLimits::max_line_bytes`] to this
/// value.
pub const MAX_EVENT_LINE_BYTES: usize = 512;

/// Longest accepted [`UplinkRuleId`], in bytes (the same bound the
/// `harw-dod-client` export reader applies, `MAX_RULE_ID_BYTES`).
pub const MAX_RULE_ID_BYTES: usize = 128;

/// Length of an [`UplinkDedupKey`]: lowercase hex of 16 bytes (export v3
/// `dedup_key`).
pub const DEDUP_KEY_HEX_LEN: usize = 32;

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

/// How directly a finding is backed by observations.
///
/// Mirrors `harw_dod_signals::Hardness` and the export v3 `hardness`
/// (`harw_dod_client::findings::FindingHardness`), spelled like the export.
/// Ordered `Observed < Correlated < Inferred`: the larger, the more indirect
/// the evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingHardness {
    /// Observed directly, without linking or inference.
    Observed,
    /// Linked from several observations.
    Correlated,
    /// Concluded beyond the observations.
    Inferred,
}

/// Why a finding exists.
///
/// Mirrors `harw_dod_rules::FindingKind` and the export v3 `kind`
/// (`harw_dod_client::findings::FindingKind`), spelled like the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A rule fired (may be escalated).
    RuleTriggered,
    /// A deviation from a baseline that is not established yet (notice
    /// only).
    Anomaly,
}

/// Triage outcome bound to a finding.
///
/// Mirrors `harw_dod_rules::Verdict` *after* the clamp in `triage_record`:
/// a model's `Confirmed` arrives as [`FindingVerdict::NeedsReview`];
/// [`FindingVerdict::Confirmed`] only ever comes from a human decision
/// (PL-100 I1). On the receiving side a verdict annotates, it never
/// decides or suppresses anything (I3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingVerdict {
    /// Confirmed by a human; worth escalating.
    Confirmed,
    /// A false alarm.
    FalsePositive,
    /// Unclear; needs a human review before anything escalates.
    NeedsReview,
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

/// An uplink identifier violates its grammar.
///
/// Names the field and the rule, never the rejected value: the value may
/// be attacker-influenced and must not reach logs or the peer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UplinkFieldError {
    field: &'static str,
    rule: &'static str,
}

impl UplinkFieldError {
    /// Name of the offending field (`finding_id`, `rule_id`, `dedup_key`).
    #[must_use]
    pub fn field(&self) -> &'static str {
        self.field
    }

    /// The grammar the value had to satisfy.
    #[must_use]
    pub fn rule(&self) -> &'static str {
        self.rule
    }
}

impl fmt::Display for UplinkFieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid uplink field `{}`: {}", self.field, self.rule)
    }
}

impl fmt::Debug for UplinkFieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for UplinkFieldError {}

/// `true` for a canonical UUIDv4 as `uuid::Uuid::new_v4()` renders it (and
/// `harw_types::FindingId::new` mints it): 36 characters, lowercase hex,
/// hyphens at 8/13/18/23, version nibble `4`, RFC 4122 variant. Rejects
/// the nil UUID (version nibble `0`), uppercase, braced and simple forms.
fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `true` for a rule id: 1..=[`MAX_RULE_ID_BYTES`] bytes of ASCII
/// alphanumerics, `-`, `_`, `.`, `:` (the `harw-dod-client` export grammar).
fn is_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RULE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// `true` for exactly [`DEDUP_KEY_HEX_LEN`] lowercase hex characters.
fn is_dedup_key(value: &str) -> bool {
    value.len() == DEDUP_KEY_HEX_LEN
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Defines a string identifier that is validated by `$check` on
/// construction and on deserialization, and serializes as a plain string.
macro_rules! uplink_id {
    (
        $(#[$doc:meta])*
        $name:ident, field = $field:literal, rule = $rule:literal, check = $check:ident
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Validates `value` as a `", $field, "` (", $rule, ").")]
            ///
            /// # Errors
            /// [`UplinkFieldError`] when `value` violates the grammar; the
            /// value itself is not echoed.
            pub fn new(value: impl Into<String>) -> Result<Self, UplinkFieldError> {
                let value = value.into();
                if $check(&value) {
                    Ok(Self(value))
                } else {
                    Err(UplinkFieldError {
                        field: $field,
                        rule: $rule,
                    })
                }
            }

            /// The validated value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

uplink_id! {
    /// Node-local finding id: the canonical lowercase UUIDv4 the Sentinel
    /// mints (export v3 `finding_id`, PL-100 I4).
    UplinkFindingId,
    field = "finding_id",
    rule = "canonical lowercase UUIDv4",
    check = is_uuid_v4
}

uplink_id! {
    /// Id of the rule that produced a finding (`egress-flow`, ...).
    UplinkRuleId,
    field = "rule_id",
    rule = "1 to 128 bytes of ASCII alphanumerics, '-', '_', '.', ':'",
    check = is_rule_id
}

uplink_id! {
    /// Throttling key of a finding (export v3 `dedup_key`): repeats of one
    /// finding share it.
    UplinkDedupKey,
    field = "dedup_key",
    rule = "32 lowercase hex characters",
    check = is_dedup_key
}

/// One uplink event.
///
/// Deliberately a summary of ids, enums and counters: raw sensor records
/// and every free text stay on the node (PL-100 I4). No field names the
/// sending node — that comes from the authenticated channel. See the
/// module docs for the wire tags and their versioning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UplinkEvent {
    /// A Sentinel finding (wire tag `finding_summary_v2`).
    #[serde(rename = "finding_summary_v2")]
    FindingSummary {
        /// Node-local finding id.
        finding_id: UplinkFindingId,
        /// Rule that produced it.
        rule_id: UplinkRuleId,
        /// Severity.
        severity: FindingSeverity,
        /// How directly observations back the finding.
        hardness: FindingHardness,
        /// Why the finding exists.
        kind: FindingKind,
        /// Throttling key; repeats of one finding share it.
        dedup_key: UplinkDedupKey,
        /// How often `dedup_key` fired so far (`>= 1`).
        occurrences: NonZeroU64,
        /// When the finding was observed (event time, Unix ms).
        observed_at_unix_ms: u64,
        /// Triage outcome bound to the finding, if any. Omitted on the wire
        /// when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verdict: Option<FindingVerdict>,
    },
    /// Liveness beat of the node's DoD stack (wire tag `health_beat_v2`).
    #[serde(rename = "health_beat_v2")]
    HealthBeat {
        /// Monotonic per-stream sequence number.
        sequence: u64,
        /// Health at the time of the beat.
        status: HealthStatus,
        /// When the beat was produced (Unix ms).
        observed_at_unix_ms: u64,
        /// Sensors reporting normally.
        sensors_ok: u32,
        /// Sensors that are missing, lagging or dropping data.
        sensors_degraded: u32,
        /// Findings waiting for triage (the `untriaged_backlog` metric).
        untriaged_backlog: u64,
        /// Dispatch breaker trips since the dispatcher started (the
        /// `breaker_trips_total` metric, summed over partitions).
        breaker_trips: u64,
    },
}

harw_types::limits_struct! {
    /// Size limits of one uplink stream.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UplinkLimits {
        /// Largest encoded event line, including the trailing `\n`.
        pub max_line_bytes: usize = DEFAULT_MAX_LINE_BYTES = 16 * 1024,
        /// Most events per stream.
        pub max_events: u64 = DEFAULT_MAX_EVENTS = 100_000,
        /// Most bytes per stream.
        pub max_stream_bytes: u64 = DEFAULT_MAX_STREAM_BYTES = 64 * 1024 * 1024,
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

/// The [`UplinkError::Malformed`] reason for a line that failed to decode,
/// built without any byte of the line.
///
/// serde_json's own message quotes the input verbatim for an unknown tag,
/// an unknown enum value (`severity`, `hardness`, `kind`, `verdict`,
/// `status`) or an unknown field name, and JSON escapes are decoded, so it
/// can carry raw bidi or control characters up to the line limit. The
/// reason reaches logs and, through the receiver's error response, the
/// peer; like [`UplinkFieldError`] it therefore names only the error class
/// and the column (PL-100 I4).
fn decode_failure_reason(err: &serde_json::Error) -> String {
    let class = match err.classify() {
        serde_json::error::Category::Io => "i/o failure",
        serde_json::error::Category::Syntax => "invalid JSON",
        serde_json::error::Category::Data => "not an event of this uplink schema",
        serde_json::error::Category::Eof => "incomplete JSON",
    };
    format!("{class} at column {}", err.column())
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
                    reason: decode_failure_reason(&err),
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

//! Harness-native ingress/egress payloads and the admission verdict (spec §2.3).
//!
//! `InboundEvent` is the normalized wire event; `OutboundContent` is what the
//! harness wants rendered; `ChannelSendOp` is the channel-native result of
//! `render_outbound`; `Admission` is the pre-session gate verdict. All are
//! transport-agnostic — Telegram/Slack/email specifics never leak in here.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, PeerId, ThreadRef};

/// The acting human inside a shared (group) session (§3.3).
///
/// Carried as a per-message attribute for policy/audit; it never participates
/// in `SessionKey` derivation, keeping "who is in the room" separate from
/// "which conversation this is".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderRef {
    /// Channel-local identity of the actual sender (e.g. Telegram user id).
    pub id: String,
    /// Optional human-friendly label for audit surfaces.
    pub display_name: Option<String>,
}

/// A reference to an inbound attachment prior to download/validation (§2.4, §3.7).
///
/// Holds only the channel-reported metadata; the reported MIME is advisory and
/// must be re-derived by content sniffing before the bytes reach `TaskPackage`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    /// Opaque channel-side handle (e.g. Telegram `file_id`).
    pub remote_id: String,
    /// Channel-reported MIME string — advisory only, never trusted.
    pub reported_mime: Option<String>,
    /// Channel-reported byte size, if known before download.
    pub declared_size: Option<u64>,
    /// Original filename, if the channel supplied one.
    pub filename: Option<String>,
}

/// A normalized inbound event addressed at a would-be `SessionKey` (§2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEvent {
    /// Configured channel binding the event arrived on.
    pub channel: ChannelId,
    /// Peer/session-partition identity (user in DM, chat id in a group, §3.3).
    pub peer: PeerId,
    /// Optional channel-local sub-conversation (forum topic, thread_ts, …).
    pub thread: Option<ThreadRef>,
    /// Actual sender inside a shared session, for policy/audit only (§3.3).
    pub sender: Option<SenderRef>,
    /// Message text, if any.
    pub text: Option<String>,
    /// Whether the event carries an explicit bot mention / trigger (§3.4).
    pub mentioned: bool,
    /// Attachment references pending download + hygiene validation (§2.4).
    pub attachments: Vec<AttachmentRef>,
    /// Raw channel event id for dedup/audit/replay (e.g. Telegram `update_id`, §5).
    pub raw_event_id: Option<String>,
    /// Wall-clock time the event entered the adapter.
    pub received_at: Timestamp,
}

/// One action offered on an approval prompt (§4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAction {
    /// Human-visible button/label text (e.g. "Approve").
    pub label: String,
    /// Protocol decision token this action maps to (e.g. "approve" / "deny").
    pub decision: String,
}

/// A harness approval request to render as inline actions or reply-text (§4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    /// Correlates the rendered prompt back to `ApprovalRequest.id`.
    pub request_id: String,
    /// Short human-readable description of what is being approved.
    pub summary: String,
    /// Declared risk tier (e.g. "elevated"), surfaced verbatim.
    pub risk: String,
    /// Available decisions, one rendered action each.
    pub actions: Vec<ApprovalAction>,
}

/// Harness-native outbound content to be rendered by an adapter (§2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutboundContent {
    /// Canonical-markdown message body (adapter downgrades per capabilities).
    Message { markdown: String },
    /// A progressive status update keyed for amend-in-place rendering (§3.6).
    StatusUpdate {
        status_key: String,
        markdown: String,
    },
    /// An approval request rendered as inline actions or reply-text (§4.2).
    Approval(ApprovalPrompt),
}

/// One inline action attached to a channel-native send op (§4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineAction {
    /// Button label shown to the user.
    pub label: String,
    /// Opaque payload echoed back on tap, correlated against the approvals table.
    pub callback_payload: String,
}

/// A single channel-native send operation produced by `render_outbound` (§3.6, §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelSendOp {
    /// Send a (possibly chunk of a) message, optionally with inline actions.
    SendMessage {
        text: String,
        inline_actions: Vec<InlineAction>,
    },
    /// Amend a previously sent status message identified by its key (§3.6).
    EditMessage { status_key: String, text: String },
}

/// Why an inbound event was rejected at admission (§2.3, §3.2, §3.4, §3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Peer/chat is not on the applicable allowlist.
    NotAllowlisted,
    /// A `require_mention` group message lacked a trigger (§3.4).
    NoMention,
    /// Inbound rate window exceeded for this peer (§3.5).
    RateLimited,
    /// The peer's tenant binding was revoked (§3.2).
    Revoked,
    /// Any other adapter-specific rejection, carrying a short reason.
    Other(String),
}

/// Why an inbound event was deferred rather than admitted or rejected (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferralReason {
    /// Peer is unpaired; route into the session-free onboarding flow (§3.2).
    Onboarding,
}

/// The admission-gate verdict computed before any session is created (§2.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission {
    /// The event may proceed to session resolution and dispatch.
    Admitted,
    /// The event is dropped with a (loggable) reason; no session touched.
    Rejected(RejectionReason),
    /// The event is held pending an out-of-band step (e.g. pairing approval).
    Deferred(DeferralReason),
}

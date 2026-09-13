//! The `ChannelAdapter` trait, `ChannelCapabilities`, and the shared,
//! transport-agnostic message-chunking utility (spec §2.3, §3.6, §7).
//!
//! The trait is named after behavior (ingress + egress + capability
//! declaration), not transport, and is deliberately synchronous: `run_ingress`
//! owns its own blocking transport loop (§6.2), so no async runtime is imposed
//! on the core. Capability-driven chunking is a *shared mechanism* (§7); the
//! actual per-channel markdown escaping/flattening lives in each binding.

use std::sync::mpsc::Sender;

use crate::event::{Admission, ChannelSendOp, InboundEvent, OutboundContent};
use crate::ids::SessionKey;

/// Declared markdown richness a channel can render (§2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownSupport {
    /// Plain text only; all markup is stripped on downgrade.
    None,
    /// Bold, italic, code spans, code blocks, links (Telegram MarkdownV2-safe subset, §3.6).
    BasicV1,
    /// `BasicV1` plus tables rendered natively.
    ExtendedTables,
    /// Full native markdown rendering.
    Native,
}

/// Declared thread/sub-conversation support a channel exposes (§2.3, §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadSupport {
    /// No sub-conversations; everything is one flat stream.
    None,
    /// Flat reply chains without first-class thread objects.
    Flat,
    /// Native forum-style threads (Telegram topics, Slack `thread_ts`).
    Native,
}

/// Declared attachment limits a channel enforces (§2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSupport {
    /// Hard per-attachment byte ceiling (0 = attachments unsupported).
    pub max_size_bytes: u64,
    /// Maximum attachments accepted from a single inbound message.
    pub max_count_per_message: usize,
    /// Allowed MIME kinds (validated by sniffing, not the reported string, §2.4).
    pub allowed_kinds: Vec<String>,
}

/// Static, load-time description of what a channel binding can render/accept (§2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCapabilities {
    /// Markdown tier the binding renders; downgrade target for richer content.
    pub markdown: MarkdownSupport,
    /// Maximum message length (channel's own unit, e.g. ~4096 for Telegram).
    pub max_message_len: usize,
    /// Attachment intake limits.
    pub attachments: AttachmentSupport,
    /// Whether a previously sent message can be amended (§3.6 status edits).
    pub edits: bool,
    /// Whether reactions are supported.
    pub reactions: bool,
    /// Thread/sub-conversation support tier.
    pub threads: ThreadSupport,
    /// Whether inline buttons/keyboards are available for approvals (§4.2).
    pub inline_actions: bool,
}

impl ChannelCapabilities {
    /// Splits `text` into chunks that each fit `max_message_len` (§3.6 step 3).
    ///
    /// Convenience wrapper over [`chunk_text`] bound to this channel's limit.
    #[must_use]
    pub fn chunk(&self, text: &str) -> Vec<String> {
        chunk_text(text, self.max_message_len)
    }
}

/// Behavior every channel binding implements (§2.3).
///
/// `Send + Sync` so a binding can be driven from a dedicated ingress thread
/// while its capabilities/render paths are used elsewhere.
pub trait ChannelAdapter: Send + Sync {
    /// Binding-specific error type; maps into `ChannelError` at the pipeline edge.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Static, load-time description of what this channel can render/accept.
    fn capabilities(&self) -> ChannelCapabilities;

    /// Maps a wire event to a harness session address; deterministic given durable
    /// pairing state (a binding may consult the durable pairing index). Stable.
    fn derive_session_key(&self, event: &InboundEvent) -> Result<SessionKey, Self::Error>;

    /// Admission gate run before an event reaches the session runtime (§2.3).
    fn admit(&self, event: &InboundEvent, key: &SessionKey) -> Admission;

    /// Render outbound content into zero or more channel-native send ops,
    /// downgrading unsupported constructs per [`capabilities`](Self::capabilities).
    /// Total: always returns something renderable rather than failing (§2.3).
    fn render_outbound(&self, content: &OutboundContent) -> Vec<ChannelSendOp>;

    /// Long-running ingress loop pushing events onto `sink` (§6.2).
    fn run_ingress(&self, sink: Sender<InboundEvent>) -> Result<(), Self::Error>;
}

/// Splits `text` at paragraph/sentence boundaries into chunks of at most
/// `max_chars` Unicode scalar values each (§3.6 step 3).
///
/// Shared, transport-agnostic mechanism (§7): boundaries are preferred in the
/// order paragraph (`\n\n`) -> line (`\n`) -> sentence (`. `) -> hard split at a
/// char boundary, so a chunk boundary never lands inside a multi-byte character.
/// `max_chars == 0` (or text already within budget) yields the input unsplit.
#[must_use]
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 || count_chars(text) <= max_chars {
        return vec![text.to_owned()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for segment in split_segments(text) {
        let seg_len = count_chars(segment);

        if seg_len > max_chars {
            // Oversized atom: flush the buffer, then hard-split the atom.
            if current_len > 0 {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            for piece in hard_split(segment, max_chars) {
                chunks.push(piece);
            }
            continue;
        }

        if current_len + seg_len > max_chars && current_len > 0 {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current.push_str(segment);
        current_len += seg_len;
    }

    if current_len > 0 {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Counts Unicode scalar values in `s`.
fn count_chars(s: &str) -> usize {
    s.chars().count()
}

/// Splits `text` into boundary-preserving segments (paragraph > line > sentence),
/// each retaining its trailing delimiter so re-joining is lossless.
fn split_segments(text: &str) -> Vec<&str> {
    let mut segments: Vec<&str> = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut idx = 0usize;

    while idx < bytes.len() {
        let boundary_end = paragraph_boundary(bytes, idx)
            .or_else(|| line_boundary(bytes, idx))
            .or_else(|| sentence_boundary(bytes, idx));

        if let Some(end) = boundary_end {
            segments.push(&text[start..end]);
            start = end;
            idx = end;
        } else {
            idx += 1;
        }
    }

    if start < text.len() {
        segments.push(&text[start..]);
    }
    segments
}

/// Returns the byte offset just past a `\n\n` paragraph break at `idx`, if any.
fn paragraph_boundary(bytes: &[u8], idx: usize) -> Option<usize> {
    if bytes.get(idx) == Some(&b'\n') && bytes.get(idx + 1) == Some(&b'\n') {
        Some(idx + 2)
    } else {
        None
    }
}

/// Returns the byte offset just past a single `\n` line break at `idx`, if any.
fn line_boundary(bytes: &[u8], idx: usize) -> Option<usize> {
    if bytes.get(idx) == Some(&b'\n') {
        Some(idx + 1)
    } else {
        None
    }
}

/// Returns the byte offset just past a `. ` sentence break at `idx`, if any.
fn sentence_boundary(bytes: &[u8], idx: usize) -> Option<usize> {
    if bytes.get(idx) == Some(&b'.') && bytes.get(idx + 1) == Some(&b' ') {
        Some(idx + 2)
    } else {
        None
    }
}

/// Hard-splits an oversized segment into `max_chars`-sized pieces, always on a
/// char boundary (never mid-scalar-value).
fn hard_split(segment: &str, max_chars: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut len = 0usize;
    for ch in segment.chars() {
        buf.push(ch);
        len += 1;
        if len == max_chars {
            pieces.push(std::mem::take(&mut buf));
            len = 0;
        }
    }
    if len > 0 {
        pieces.push(buf);
    }
    pieces
}

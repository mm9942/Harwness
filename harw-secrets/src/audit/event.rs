//! Append-only audit event record (spec §4.1). Records are immutable once
//! written and chained (§4.2) so tampering with an old record is detectable
//! even by a party with only read access. Events never carry raw secret bytes,
//! full message content, or anything that would itself need to be a secret
//! (§4.4).

use jiff::Timestamp;
use uuid::Uuid;

/// Identifies one audit event. UUIDv7 so ids are naturally time-ordered, which
/// lets verification sanity-check ordering independent of the hash chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AuditEventId(Uuid);

impl AuditEventId {
    /// Mint a fresh time-ordered (UUIDv7) event id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Borrow the raw 16-byte UUID representation (used in `canonical_bytes`).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl Default for AuditEventId {
    fn default() -> Self {
        Self::new()
    }
}

/// Who or what performed the audited action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Actor {
    /// A human operator identified by their configured operator name.
    Operator(String),
    /// An agent acting autonomously within a session.
    Agent { session_id: String },
    /// The harness itself (startup, scheduled maintenance, key rotation).
    System,
}

/// A single reference to the entity an event acted upon — kept generic so the
/// audit layer need not know every domain type (secret ids, tool names, channel
/// ids, plugin names, ...).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectRef {
    /// Namespace of the referenced entity, e.g. `"secret"`, `"channel"`.
    pub kind: String,
    /// Stable identifier within that namespace.
    pub id: String,
}

impl SubjectRef {
    /// Build a subject reference from a namespace and an id.
    #[must_use]
    pub fn new(kind: &str, id: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            id: id.to_owned(),
        }
    }
}

/// One append-only audit record.
#[derive(Clone, Debug)]
pub struct AuditEvent {
    /// Time-ordered event id.
    pub id: AuditEventId,
    /// Who performed the action.
    pub actor: Actor,
    /// Dot-separated `namespace.verb`, e.g. `"secret.access"`,
    /// `"approval.grant"`, `"channel.command"`, `"plugin.install"`.
    pub action: String,
    /// Entities the action touched.
    pub subjects: Vec<SubjectRef>,
    /// When the event was recorded.
    pub recorded_at: Timestamp,
    /// SHA-256 of the previous event's canonical bytes (all-zero for genesis).
    pub prev_hash: [u8; 32],
}

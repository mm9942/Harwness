//! Identity re-exports and the transport-agnostic `SessionKey` (spec §2.1, §2.2).
//!
//! The channel-local identity newtypes are owned by `harw-types` (zero-dep
//! fan-in) and only re-exported here — never redefined — so every crate refers
//! to the same type. `SessionKey` is the structural conversation address every
//! adapter derives; the session store resolves it to a `SessionId`.

pub use harw_types::{ChannelId, PeerId, TenantId, ThreadRef};

use serde::{Deserialize, Serialize};

/// Field separator for a `SessionKey`'s canonical string form. Chosen to not
/// occur in the id newtypes' typical value space so the join is unambiguous.
const KEY_SEP: char = '\u{1f}';

/// Structural conversation address: `(tenant, channel, peer, thread)` (§2.2).
///
/// Two inbound events with an identical tuple resolve to the same session;
/// changing any field is by definition a different conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Harness-side tenant this peer is bound to (from pairing, §3.2).
    pub tenant: TenantId,
    /// Configured channel binding the event arrived on.
    pub channel: ChannelId,
    /// Channel-local peer identity (or group/chat id for group sessions, §3.3).
    pub peer: PeerId,
    /// Optional channel-local sub-conversation (forum topic, thread_ts, …).
    pub thread: Option<ThreadRef>,
}

impl SessionKey {
    /// Builds a session key from its four structural components.
    #[must_use]
    pub fn new(
        tenant: TenantId,
        channel: ChannelId,
        peer: PeerId,
        thread: Option<ThreadRef>,
    ) -> Self {
        Self {
            tenant,
            channel,
            peer,
            thread,
        }
    }

    /// Deterministic canonical string form of the key (§2.2).
    ///
    /// Pure and stable across process restarts: identical tuples always produce
    /// an identical string, so replays and recovery-marker reconciliation are
    /// byte-for-byte reproducible. A `None` thread renders as an empty segment,
    /// keeping "root/lobby" distinct from any populated thread ref.
    #[must_use]
    pub fn canonical_key(&self) -> String {
        let thread = self.thread.as_ref().map(ThreadRef::as_str).unwrap_or("");
        let mut out = String::with_capacity(
            self.tenant.as_str().len()
                + self.channel.as_str().len()
                + self.peer.as_str().len()
                + thread.len()
                + 3,
        );
        out.push_str(self.tenant.as_str());
        out.push(KEY_SEP);
        out.push_str(self.channel.as_str());
        out.push(KEY_SEP);
        out.push_str(self.peer.as_str());
        out.push(KEY_SEP);
        out.push_str(thread);
        out
    }
}

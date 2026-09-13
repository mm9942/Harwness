//! Channel-agnostic pairing-code primitive, backed by `harw-session-store` (§3.2).
//!
//! Pairing codes are single-use, short-TTL, and scoped to one `ChannelId`, so a
//! code issued by one binding cannot be redeemed against another. The pure
//! code-generation, expiry, and validation logic is implemented in full here;
//! durable issue/approve persistence journals through `harw-session-store`'s
//! append-only log (currently deferred upstream) so the same primitive is
//! reusable by every future channel.

use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::SessionId;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::error::{ChannelError, ChannelResult};
use crate::ids::{ChannelId, PeerId, TenantId, ThreadRef};

/// Crockford base32 alphabet (no `I`, `L`, `O`, `U`) for human-readable codes.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Number of entropy bytes consumed to render one 8-character pairing code.
const ENTROPY_BYTES: usize = 5;

/// A generated, channel-scoped, expiring pairing code (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    /// The rendered code, e.g. `"7F2K-9QRT"`.
    pub code: String,
    /// The channel binding this code is valid against (scope, §3.2).
    pub channel: ChannelId,
    /// Peer that requested pairing (bound on approval).
    pub peer: PeerId,
    /// When the code was issued.
    pub issued_at: Timestamp,
    /// When the code stops being redeemable.
    pub expires_at: Timestamp,
}

impl PairingCode {
    /// Default code lifetime in seconds (15 minutes, §3.2).
    pub const DEFAULT_TTL_SECS: i64 = 15 * 60;

    /// The default pairing-code time-to-live.
    #[must_use]
    pub fn default_ttl() -> SignedDuration {
        SignedDuration::from_secs(Self::DEFAULT_TTL_SECS)
    }

    /// Generates a code for `(channel, peer)` valid for `ttl` from `now` (§3.2).
    ///
    /// Pure aside from the caller-supplied entropy; the caller must pass at
    /// least [`ENTROPY_BYTES`] cryptographically-random bytes (shorter input is
    /// zero-padded). Fails only if `now + ttl` overflows the timestamp range.
    pub fn generate(
        channel: &ChannelId,
        peer: &PeerId,
        entropy: &[u8],
        now: Timestamp,
        ttl: SignedDuration,
    ) -> ChannelResult<Self> {
        let expires_at = now.checked_add(ttl).map_err(|e| ChannelError::Time {
            detail: e.to_string(),
        })?;
        Ok(Self {
            code: Self::format_code(entropy),
            channel: channel.clone(),
            peer: peer.clone(),
            issued_at: now,
            expires_at,
        })
    }

    /// Renders `entropy` as an 8-character Crockford-base32 code `"XXXX-XXXX"`.
    ///
    /// Deterministic and infallible: uses the first [`ENTROPY_BYTES`] bytes,
    /// zero-padding if fewer are supplied.
    #[must_use]
    pub fn format_code(entropy: &[u8]) -> String {
        let mut seed = [0u8; ENTROPY_BYTES];
        for (slot, byte) in seed.iter_mut().zip(entropy.iter()) {
            *slot = *byte;
        }

        // 5 bytes = 40 bits -> eight 5-bit base32 symbols.
        let mut acc: u64 = 0;
        for byte in seed {
            acc = (acc << 8) | u64::from(byte);
        }

        let mut out = String::with_capacity(9);
        for i in 0..8 {
            let shift = 35 - i * 5;
            let symbol = ((acc >> shift) & 0b1_1111) as usize;
            out.push(CROCKFORD[symbol] as char);
            if i == 3 {
                out.push('-');
            }
        }
        out
    }

    /// Whether the code is past its expiry at `now`.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }

    /// Validates a presented `code` against this record (§3.2).
    ///
    /// # Errors
    /// - [`ChannelError::PairingInvalid`]: wrong channel scope or code mismatch.
    /// - [`ChannelError::PairingExpired`]: the code's TTL has elapsed.
    pub fn validate(&self, channel: &ChannelId, code: &str, now: Timestamp) -> ChannelResult<()> {
        if &self.channel != channel || self.code != code {
            return Err(ChannelError::PairingInvalid {
                channel: channel.clone(),
                code: code.to_owned(),
            });
        }
        if self.is_expired(now) {
            return Err(ChannelError::PairingExpired {
                code: code.to_owned(),
            });
        }
        Ok(())
    }
}

/// The authoritative `PeerId -> TenantId` binding written on approval (§3.2 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRecord {
    /// Channel binding the pairing applies to.
    pub channel: ChannelId,
    /// Newly-bound peer.
    pub peer: PeerId,
    /// Tenant the peer is now authorized as.
    pub tenant: TenantId,
    /// When the binding was established.
    pub bound_at: Timestamp,
}

/// Issues and approves pairing codes, journaling them through the session store.
///
/// The pure code lifecycle lives on [`PairingCode`]; this type owns durability.
/// Persistence keys pairing events under a per-channel synthetic session so they
/// share the append-only journal's guarantees (§3.2 "same durability as session
/// events"). The append/rewrite bodies are deferred upstream today, so
/// [`issue`](Self::issue) / [`approve`](Self::approve) surface that deferral via
/// the propagated `SessionStoreError`.
pub struct PairingRegistry {
    store: TranscriptStore,
}

impl PairingRegistry {
    /// Builds a registry over an existing transcript store.
    #[must_use]
    pub fn new(store: TranscriptStore) -> Self {
        Self { store }
    }

    /// Borrows the backing transcript store.
    #[must_use]
    pub fn store(&self) -> &TranscriptStore {
        &self.store
    }

    /// Synthetic session id partitioning one channel's pairing journal entries.
    #[must_use]
    pub fn pairing_session_id(channel: &ChannelId) -> SessionId {
        SessionId::from_str(format!("pairing:{channel}"))
    }

    /// Generates a pairing code for `(channel, peer)` and journals it (§3.2 step 2).
    ///
    /// # Errors
    /// - [`ChannelError::Time`]: expiry timestamp overflow.
    /// - [`ChannelError::SessionStore`]: persistence failure (deferred upstream).
    /// - [`ChannelError::Serde`]: payload serialization failure.
    pub fn issue(
        &self,
        channel: &ChannelId,
        peer: &PeerId,
        entropy: &[u8],
        now: Timestamp,
    ) -> ChannelResult<PairingCode> {
        let code = PairingCode::generate(channel, peer, entropy, now, PairingCode::default_ttl())?;
        let record = Self::journal_record(channel, now, "pairing.code.issued", &code)?;
        self.store.append(&record)?;
        Ok(code)
    }

    /// Records the authoritative `PeerId -> TenantId` binding on approval (§3.2 step 3).
    ///
    /// # Errors
    /// - [`ChannelError::SessionStore`]: persistence failure (deferred upstream).
    /// - [`ChannelError::Serde`]: payload serialization failure.
    pub fn approve(
        &self,
        code: &PairingCode,
        tenant: &TenantId,
        now: Timestamp,
    ) -> ChannelResult<PairingRecord> {
        let binding = PairingRecord {
            channel: code.channel.clone(),
            peer: code.peer.clone(),
            tenant: tenant.clone(),
            bound_at: now,
        };
        let record = Self::journal_record(&code.channel, now, "pairing.bound", &binding)?;
        self.store.append(&record)?;
        Ok(binding)
    }

    /// Builds a `Lifecycle` transcript record wrapping a serializable pairing payload.
    fn journal_record<T: Serialize>(
        channel: &ChannelId,
        now: Timestamp,
        event: &str,
        payload: &T,
    ) -> ChannelResult<TranscriptRecord> {
        let body = serde_json::json!({
            "event": event,
            "channel": channel.as_str(),
            "data": serde_json::to_value(payload)?,
        });
        Ok(TranscriptRecord::new(
            Self::pairing_session_id(channel),
            ThreadRef::from_str(""),
            0,
            now,
            RecordKind::Lifecycle,
            body,
        ))
    }
}

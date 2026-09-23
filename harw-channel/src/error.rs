//! Transport-agnostic error type for the channel layer (spec §6.3).
//!
//! `#[derive(HarwError)]` emits `Display`, `std::error::Error`, the `#[from]`
//! conversions, and the `ChannelResult<T>` alias. No `anyhow`/`thiserror`.
//! Binding-specific error enums (e.g. `TelegramChannelError`) map *into* this
//! uniform type via `From` where the core dispatch pipeline needs one shape,
//! preserving binding detail as `source()`.

use harw_macros::HarwError;

use crate::ids::{ChannelId, PeerId};

/// Failure modes of the transport-agnostic channel core (admission, pairing,
/// rate limiting, capability negotiation, and propagated store/serde/io errors).
#[derive(Debug, HarwError)]
pub enum ChannelError {
    /// Admission gate denied an inbound event before it reached the runtime (§2.3).
    #[msg("inbound event denied by admission gate on channel '{channel}': {reason}")]
    AdmissionDenied { channel: ChannelId, reason: String },

    /// A pairing code was unknown, malformed, or scoped to a different channel (§3.2).
    #[msg("pairing code '{code}' is invalid for channel '{channel}'")]
    PairingInvalid { channel: ChannelId, code: String },

    /// A pairing code was presented after its short TTL elapsed (§3.2).
    #[msg("pairing code '{code}' has expired")]
    PairingExpired { code: String },

    /// A durable pairing code was already consumed by an earlier redemption; it
    /// is single-use and cannot be redeemed again (§3.2).
    #[msg("pairing code '{code}' for channel '{channel}' was already redeemed")]
    PairingAlreadyRedeemed { channel: ChannelId, code: String },

    /// Revocation was requested for a peer that has no durable pairing binding
    /// on this channel (§3.2).
    #[msg("no pairing binding for peer '{peer}' on channel '{channel}' to revoke")]
    PairingBindingNotFound { channel: ChannelId, peer: PeerId },

    /// A concurrent redemption/claim currently holds the durable pairing lock for
    /// this resource; the caller lost the single-winner race (§3.2).
    #[msg(
        "pairing resource '{resource}' on channel '{channel}' is locked by a concurrent operation"
    )]
    PairingContended {
        channel: ChannelId,
        resource: String,
    },

    /// A peer exceeded the configured inbound rate window (§3.5).
    #[msg("peer '{peer}' on channel '{channel}' exceeded rate limit ({limit} per {window_secs}s)")]
    RateLimited {
        channel: ChannelId,
        peer: PeerId,
        limit: u32,
        window_secs: u64,
    },

    /// Outbound content requested a construct the channel does not support (§2.3).
    #[msg("channel '{channel}' does not support capability '{capability}'")]
    CapabilityUnsupported {
        channel: ChannelId,
        capability: String,
    },

    /// No `PeerId -> TenantId` binding exists yet; onboarding/pairing required (§2.2, §3.2).
    #[msg("peer '{peer}' on channel '{channel}' is not paired to a tenant")]
    Unpaired { channel: ChannelId, peer: PeerId },

    /// Timestamp arithmetic (e.g. pairing expiry) overflowed the representable range.
    #[msg("timestamp arithmetic failed: {detail}")]
    Time { detail: String },

    /// Bot-token / webhook-secret storage error propagated from `harw-secrets` (§1a).
    #[msg("secret store error: {0}")]
    #[from]
    Secrets(harw_secrets::SecretsError),

    /// Pairing/journal persistence error propagated from `harw-session-store` (§3.2).
    #[msg("session store error: {0}")]
    #[from]
    SessionStore(harw_session_store::SessionStoreError),

    /// I/O error in the channel layer (transport-agnostic paths only).
    #[msg("i/o error in channel layer: {0}")]
    #[from]
    Io(std::io::Error),

    /// (De)serialization error for channel-layer payloads.
    #[msg("serialization error in channel layer: {0}")]
    #[from]
    Serde(serde_json::Error),

    /// The channel's inbound transport cannot deliver events right now: the
    /// binding was built without ingress, another runner already owns the
    /// receiver, or the runtime sink/ingress lock is gone. This is a
    /// configuration/lifecycle condition of a configured channel, not a
    /// missing feature.
    #[msg("ingress for channel '{channel}' is unavailable: {reason}")]
    IngressUnavailable { channel: ChannelId, reason: String },

    /// The channel binding deliberately offers no path for the requested
    /// operation (e.g. launching a sandboxed worker from an approved work
    /// request). `operation` is a stable, static identifier suitable for
    /// matching; `detail` names the affected object for the operator.
    #[msg("channel '{channel}' does not support operation '{operation}': {detail}")]
    OperationUnsupported {
        channel: ChannelId,
        operation: &'static str,
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn ingress_unavailable_display_names_channel_and_reason() -> TestResult {
        let error = ChannelError::IngressUnavailable {
            channel: ChannelId::from_str("telegram"),
            reason: "ingress is not configured".to_owned(),
        };
        let rendered = error.to_string();
        if rendered != "ingress for channel 'telegram' is unavailable: ingress is not configured" {
            return Err(TestError::Unexpected(rendered));
        }
        if std::error::Error::source(&error).is_some() {
            return Err(TestError::Unexpected(
                "IngressUnavailable must not carry a source".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn operation_unsupported_display_names_operation_and_detail() -> TestResult {
        let error = ChannelError::OperationUnsupported {
            channel: ChannelId::from_str("telegram"),
            operation: "sandboxed-launch",
            detail: "work request 'w-1'".to_owned(),
        };
        let rendered = error.to_string();
        if rendered
            != "channel 'telegram' does not support operation 'sandboxed-launch': work request 'w-1'"
        {
            return Err(TestError::Unexpected(rendered));
        }
        if !matches!(
            error,
            ChannelError::OperationUnsupported {
                operation: "sandboxed-launch",
                ..
            }
        ) {
            return Err(TestError::Unexpected(
                "operation identifier must be matchable".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn io_error_converts_and_exposes_source() -> TestResult {
        let error = ChannelError::from(std::io::Error::other("disk gone"));
        let source = std::error::Error::source(&error)
            .ok_or(TestError::Missing("source of ChannelError::Io"))?;
        if source.to_string() != "disk gone" {
            return Err(TestError::Unexpected(source.to_string()));
        }
        if error.to_string() != "i/o error in channel layer: disk gone" {
            return Err(TestError::Unexpected(error.to_string()));
        }
        Ok(())
    }
}

//! `harw-types` — Phase-0-Vokabular des Harness.
//!
//! Reine serde-Daten: ID-Newtypes, `AgentRole`, `RiskLevel`, `ReviewDecision`,
//! `MessagePhase`, `TokenUsage`, `ReasoningEffort`. Keine `tokio`-, `reqwest`-
//! oder Provider-Abhängigkeiten. Höchste Stabilität, maximaler Fan-in.
//!
//! ID values coming from configuration, wire data, or other untrusted sources
//! must use each type's `try_from_str`, `parse`, or `FromStr` implementation.
//! These validated paths reject empty and whitespace-only values and preserve
//! the existing infallible constructors for compatibility with current callers.

#![forbid(unsafe_code)]

pub mod confidence;
pub mod digest;
pub mod error;
pub mod ids;
pub mod impact;
pub mod provider_ids;
pub mod reasoning;
pub mod roles;
pub mod usage;

pub use confidence::Confidence;
pub use digest::ContentDigest;
pub use error::{ImpactAssessmentError, InvalidDigest, InvalidId};
pub use ids::{
    ActionId, ApprovalActor, BaselineId, CgroupId, ChannelId, FindingId, HostId, ItemId, PeerId,
    SensorId, SessionId, TenantId, ThreadId, ThreadRef, ToolCallId, TurnId, WorkId, WorkspaceId,
};
pub use impact::{ImpactAssessment, ImpactConfidence, ImpactDomain, ImpactSeverity};
pub use provider_ids::{AgentName, CustomerId, ModelId, ModelName, ProviderId, ProviderName};
pub use reasoning::ReasoningEffort;
pub use roles::{AgentRole, MessagePhase, ReviewDecision, RiskLevel};
pub use usage::TokenUsage;

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_public_fallible_id_api_rejects_blank {
        ($id_type:ty) => {
            assert!(<$id_type>::try_from_str("").is_err());
            assert!(<$id_type>::parse(" \t\n").is_err());
            assert!("".parse::<$id_type>().is_err());
        };
    }

    #[test]
    fn public_id_types_offer_blank_rejecting_constructors() {
        assert_public_fallible_id_api_rejects_blank!(SessionId);
        assert_public_fallible_id_api_rejects_blank!(ThreadId);
        assert_public_fallible_id_api_rejects_blank!(TurnId);
        assert_public_fallible_id_api_rejects_blank!(ToolCallId);
        assert_public_fallible_id_api_rejects_blank!(ItemId);
        assert_public_fallible_id_api_rejects_blank!(WorkId);
        assert_public_fallible_id_api_rejects_blank!(ChannelId);
        assert_public_fallible_id_api_rejects_blank!(PeerId);
        assert_public_fallible_id_api_rejects_blank!(TenantId);
        assert_public_fallible_id_api_rejects_blank!(WorkspaceId);
        assert_public_fallible_id_api_rejects_blank!(ThreadRef);
        assert_public_fallible_id_api_rejects_blank!(ProviderId);
        assert_public_fallible_id_api_rejects_blank!(ProviderName);
        assert_public_fallible_id_api_rejects_blank!(ModelId);
        assert_public_fallible_id_api_rejects_blank!(ModelName);
        assert_public_fallible_id_api_rejects_blank!(AgentName);
        assert_public_fallible_id_api_rejects_blank!(CustomerId);
    }

    #[test]
    fn public_id_types_preserve_non_blank_values() {
        assert_eq!(SessionId::parse(" session ").unwrap().as_str(), " session ");
        assert_eq!(ProviderId::parse(" openai ").unwrap().as_str(), " openai ");
    }
}

#[cfg(test)]
mod aw0_03_tests {
    use super::*;

    macro_rules! assert_public_fallible_id_api_rejects_blank {
        ($id_type:ty) => {
            assert!(<$id_type>::try_from_str("").is_err());
            assert!(<$id_type>::parse(" \t\n").is_err());
            assert!("".parse::<$id_type>().is_err());
        };
    }

    #[test]
    fn test_new_public_id_types_reject_blank_values() {
        assert_public_fallible_id_api_rejects_blank!(FindingId);
        assert_public_fallible_id_api_rejects_blank!(SensorId);
        assert_public_fallible_id_api_rejects_blank!(ActionId);
        assert_public_fallible_id_api_rejects_blank!(BaselineId);
        assert_public_fallible_id_api_rejects_blank!(HostId);
        assert_public_fallible_id_api_rejects_blank!(CgroupId);
    }

    #[test]
    fn test_content_digest_reexported_and_usable_from_crate_root() {
        let digest = ContentDigest::of(b"crate root reexport check");
        assert_eq!(digest.to_string().len(), 64);
    }
}

//! `harw-types` — Phase-0-Vokabular des Harness.
//!
//! Reine serde-Daten: ID-Newtypes, `AgentRole`, `RiskLevel`, `ReviewDecision`,
//! `MessagePhase`, `TokenUsage`, `ReasoningEffort`, `Principal`/`PermissionTier`,
//! die Infrastruktur-Identität `SecurityContext`/`TrustZone`/`AuthStrength`,
//! die Wanduhr-Abstraktion `Clock`/`SystemClock`.
//! Keine `tokio`-, `reqwest`- oder Provider-Abhängigkeiten. Höchste Stabilität,
//! maximaler Fan-in.
//!
//! ID values coming from configuration, wire data, or other untrusted sources
//! must use each type's `try_from_str`, `parse`, or `FromStr` implementation.
//! These validated paths reject empty and whitespace-only values and preserve
//! the existing infallible constructors for compatibility with current callers.

#![forbid(unsafe_code)]

pub mod cancel;
pub mod clock;
pub mod confidence;
pub mod digest;
pub mod error;
pub mod ids;
pub mod impact;
pub mod limits;
pub mod principal;
pub mod provider_ids;
pub mod reasoning;
pub mod roles;
pub mod security;
pub mod usage;

#[cfg(test)]
mod test_support;

pub use clock::{Clock, SystemClock};
pub use confidence::Confidence;
pub use digest::ContentDigest;
pub use error::{ImpactAssessmentError, InvalidDigest, InvalidId};
pub use ids::{
    ActionId, ApprovalActor, ApprovalId, BaselineId, CgroupId, ChannelId, DeviceId, FindingId,
    HostId, ItemId, NodeId, PeerId, SecurityContextId, SensorId, ServiceIdentityId, SessionId,
    TenantId, ThreadId, ThreadRef, ToolCallId, TurnId, WorkId, WorkspaceId,
};
pub use impact::{ImpactAssessment, ImpactConfidence, ImpactDomain, ImpactSeverity};
pub use principal::{IngressSurface, PermissionTier, Principal, PrincipalKind};
pub use provider_ids::{AgentName, CustomerId, ModelId, ModelName, ProviderId, ProviderName};
pub use reasoning::ReasoningEffort;
pub use roles::{
    APPROVAL_TIMEOUT_REASON, AgentRole, DEFAULT_APPROVAL_TIMEOUT, MessagePhase, ReviewDecision,
    RiskLevel,
};
pub use security::{
    AuthStrength, SecurityClaims, SecurityContext, SecurityContextError, SecurityContextIssuer,
    SecurityContextSummary, TrustZone,
};
pub use usage::TokenUsage;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        TestResult, assert_blank_id_apis_rejected, assert_blank_id_apis_rejected_with_try_from,
    };

    #[test]
    fn public_id_types_offer_blank_rejecting_constructors() {
        assert_blank_id_apis_rejected_with_try_from!(SessionId);
        assert_blank_id_apis_rejected_with_try_from!(ThreadId);
        assert_blank_id_apis_rejected_with_try_from!(TurnId);
        assert_blank_id_apis_rejected_with_try_from!(ToolCallId);
        assert_blank_id_apis_rejected_with_try_from!(ItemId);
        assert_blank_id_apis_rejected_with_try_from!(WorkId);
        assert_blank_id_apis_rejected_with_try_from!(ChannelId);
        assert_blank_id_apis_rejected_with_try_from!(PeerId);
        assert_blank_id_apis_rejected_with_try_from!(TenantId);
        assert_blank_id_apis_rejected_with_try_from!(WorkspaceId);
        assert_blank_id_apis_rejected_with_try_from!(ThreadRef);
        assert_blank_id_apis_rejected_with_try_from!(ApprovalId);
        assert_blank_id_apis_rejected!(ProviderId);
        assert_blank_id_apis_rejected!(ProviderName);
        assert_blank_id_apis_rejected!(ModelId);
        assert_blank_id_apis_rejected!(ModelName);
        assert_blank_id_apis_rejected!(AgentName);
        assert_blank_id_apis_rejected!(CustomerId);
    }

    #[test]
    fn public_id_types_preserve_non_blank_values() -> TestResult {
        assert_eq!(SessionId::parse(" session ")?.as_str(), " session ");
        assert_eq!(ProviderId::parse(" openai ")?.as_str(), " openai ");
        Ok(())
    }
}

#[cfg(test)]
mod aw0_03_tests {
    use super::*;
    use crate::test_support::assert_blank_id_apis_rejected_with_try_from;

    #[test]
    fn test_new_public_id_types_reject_blank_values() {
        assert_blank_id_apis_rejected_with_try_from!(FindingId);
        assert_blank_id_apis_rejected_with_try_from!(SensorId);
        assert_blank_id_apis_rejected_with_try_from!(ActionId);
        assert_blank_id_apis_rejected_with_try_from!(BaselineId);
        assert_blank_id_apis_rejected_with_try_from!(HostId);
        assert_blank_id_apis_rejected_with_try_from!(CgroupId);
        assert_blank_id_apis_rejected_with_try_from!(NodeId);
        assert_blank_id_apis_rejected_with_try_from!(DeviceId);
        assert_blank_id_apis_rejected_with_try_from!(ServiceIdentityId);
        assert_blank_id_apis_rejected_with_try_from!(SecurityContextId);
    }

    #[test]
    fn test_content_digest_reexported_and_usable_from_crate_root() {
        let digest = ContentDigest::of(b"crate root reexport check");
        assert_eq!(digest.to_string().len(), 64);
    }
}

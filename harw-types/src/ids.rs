//! ID-Newtypes für den Harness.
//!
//! Jede ID ist ein `#[serde(transparent)]`-Newtype über `String`, sodass sie
//! auf der Wire-Ebene als reiner String erscheint — kein Objekt-Wrapper.
//! Höchste Stabilität: diese Typen ändern sich fast nie.

pub use crate::error::InvalidId;
use serde::{Deserialize, Serialize, de::Deserializer};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

fn validate_id<S>(id_type: &'static str, value: S) -> Result<String, InvalidId>
where
    S: Into<String>,
{
    let value = value.into();
    if value.trim().is_empty() {
        Err(InvalidId::new(id_type))
    } else {
        Ok(value)
    }
}

macro_rules! newtype_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash,
            Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Erzeugt eine neue, zufällige ID (UUID v4).
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            /// Konstruiert eine ID aus einem bestehenden, nicht-leeren String.
            pub fn try_from_str(s: impl Into<String>) -> Result<Self, InvalidId> {
                validate_id(stringify!($name), s).map(Self)
            }

            /// Alias für [`Self::try_from_str`] für Parser-orientierte Call-Sites.
            pub fn parse(s: impl Into<String>) -> Result<Self, InvalidId> {
                Self::try_from_str(s)
            }

            /// Konstruiert eine ID aus einem bestehenden String.
            ///
            /// Dieser Konstruktor bleibt aus Kompatibilitätsgründen bestehen.
            /// Neue Eingaben aus untrusted Quellen müssen [`Self::try_from_str`]
            /// oder [`FromStr::from_str`] verwenden.
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrowt den inneren String-Slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from_str(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = InvalidId;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_from_str(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = InvalidId;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_from_str(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from_str(value).map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

newtype_id!(
    /// Lebenszeit einer Agent-Verbindung.
    SessionId
);
newtype_id!(
    /// Persistente Konversations-Kette über mehrere Turns.
    ThreadId
);
newtype_id!(
    /// Einzelner Request-Response-Zyklus.
    TurnId
);
newtype_id!(
    /// Eindeutige Tool-Invokation innerhalb eines Turns.
    ToolCallId
);
newtype_id!(
    /// Einzelnes `TurnItem` (Message, Reasoning, …).
    ItemId
);

// --- Phase-1 fan-in IDs (shared by the new crates) ---------------------------
// Deliberately live here in the zero-dependency `harw-types` crate so that
// `harw-job-runtime`, `harw-channel*`, `harw-knowledge` and `harw-secrets` all
// reference the *same* newtype rather than each minting an incompatible copy.

newtype_id!(
    /// Governance handle for a unit of governed background work
    /// (`harw-job-runtime::Job`). Time-ordered where the caller uses UUID v7.
    WorkId
);
newtype_id!(
    /// Identifies a configured ingress channel instance (e.g. one Telegram bot).
    ChannelId
);
newtype_id!(
    /// Identifies a remote peer on a channel (e.g. a Telegram chat/user id,
    /// stored as an opaque string to stay channel-agnostic).
    PeerId
);
newtype_id!(
    /// Tenant / workspace isolation boundary shared across channels and stores.
    TenantId
);
newtype_id!(
    /// Configured workspace alias resolved server-side before work starts.
    WorkspaceId
);
newtype_id!(
    /// Identifies one pending or resolved `ApprovalRequest`
    /// (`harw-protocol::approvals`, interaction-contract.md §4.1). Distinct
    /// from [`WorkId`]: an `ApprovalRequest` is addressed at exactly one
    /// `WorkId`, but the approval decision point itself — which may be one
    /// of several raised against the same `WorkId` over its lifetime — has
    /// its own identity.
    ApprovalId
);
newtype_id!(
    /// Stable reference to a persisted conversation thread as seen from outside
    /// `harw-core` (provenance links, pairing records, transcript back-refs).
    ThreadRef
);

/// Trusted identity allowed to answer an approval prompt. This belongs to the
/// channel/session authority plane, never to model-generated tool arguments.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApprovalActor {
    /// A local authenticated operator, identified by the runtime's identity
    /// provider rather than a display name supplied in chat text.
    Operator { id: String },
    /// A remote peer bound to one configured ingress channel.
    ChannelPeer { channel: ChannelId, peer: PeerId },
}

// --- AW0-03 detection/response fan-in IDs ------------------------------------
// Deliberately live here in the zero-dependency `harw-types` crate for the same
// reason as the Phase-1 fan-in IDs above: `harw-dod-signals`, `harw-dod-cap`,
// `harw-lens-types`, `harw-context`, and their siblings all need the *same*
// newtype rather than each minting an incompatible copy.

newtype_id!(
    /// Identifies a single finding record produced by a detection or scanning
    /// engine (a policy violation, an anomaly, a rule match, ...).
    FindingId
);
newtype_id!(
    /// Identifies one configured sensor/detector instance that produces
    /// findings or telemetry.
    SensorId
);
newtype_id!(
    /// Identifies a single response or remediation action taken against a
    /// finding (isolate, kill, quarantine, ...).
    ActionId
);
newtype_id!(
    /// Identifies a baseline snapshot used as the reference point for drift
    /// and anomaly comparisons.
    BaselineId
);
newtype_id!(
    /// Identifies a physical or virtual host monitored or acted upon by the
    /// harness.
    HostId
);
newtype_id!(
    /// Identifies a Linux control group (cgroup).
    ///
    /// # Kernel ID reuse
    ///
    /// The kernel reuses cgroup IDs after the originating cgroup is removed
    /// and enough time or ID-space pressure has passed. A `CgroupId` is
    /// therefore **not** a permanently unique identifier on its own: a
    /// consumer that treats it as such can mistake a freshly created cgroup
    /// for the old one that used to hold this ID, silently attributing new
    /// activity to a stale record. Callers that need durable uniqueness (for
    /// example, keying a `FreezeStore` entry) must pair a `CgroupId` with
    /// additional disambiguating context, such as a creation timestamp or the
    /// host's boot id.
    CgroupId
);

// --- H1 infrastructure identity IDs (crypto masterplan v2 §13/§25/§34) -------
// Shared by the Auth/Crypto Hub, NetSec, SecurityHub, `harw-web` and the fleet
// transport. They live here for the same fan-in reason as the blocks above: no
// crate may plumb a raw `String` where one of these identities is meant.

newtype_id!(
    /// Identifies one Harwness node (a runtime installation that holds its own
    /// node-identity key and can join a fleet).
    NodeId
);
newtype_id!(
    /// Identifies an enrolled end-user device (laptop, phone, hardware token
    /// holder) from which a principal authenticates.
    DeviceId
);
newtype_id!(
    /// Identifies a non-human service identity (daemon, gateway, worker) that
    /// authenticates with its own key rather than on behalf of a user.
    ServiceIdentityId
);
newtype_id!(
    /// Identifies one issued [`crate::security::SecurityContext`]. Used as the
    /// short-lived context reference across process/network boundaries
    /// (masterplan §25) — the reference alone never grants anything.
    SecurityContextId
);

#[cfg(test)]
mod tests {
    use super::{
        ApprovalId, ChannelId, ItemId, PeerId, SessionId, TenantId, ThreadId, ThreadRef,
        ToolCallId, TurnId, WorkId, WorkspaceId,
    };
    use crate::test_support::{TestResult, assert_blank_id_apis_rejected_with_try_from};

    #[test]
    fn every_id_type_rejects_empty_and_whitespace_only_values_from_fallible_apis() -> TestResult {
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

        assert_eq!(SessionId::try_from_str(" session ")?.as_str(), " session ");
        Ok(())
    }

    #[test]
    fn string_parser_and_legacy_constructor_are_available() -> TestResult {
        assert_eq!("session".parse::<SessionId>()?.as_str(), "session");
        assert_eq!(SessionId::from_str("legacy").as_str(), "legacy");
        Ok(())
    }

    #[test]
    fn deserialization_rejects_empty_and_whitespace_only_values() -> TestResult {
        assert!(serde_json::from_str::<ThreadRef>(r#"""#).is_err());
        assert!(serde_json::from_str::<ThreadRef>(r#""  \t\n""#).is_err());
        assert_eq!(
            serde_json::from_str::<ThreadRef>(r#""thread-1""#)?.as_str(),
            "thread-1"
        );
        Ok(())
    }
}

#[cfg(test)]
mod aw0_03_ids_tests {
    use super::{ActionId, BaselineId, CgroupId, FindingId, HostId, SensorId};
    use crate::test_support::{TestResult, assert_blank_id_apis_rejected_with_try_from};

    #[test]
    fn test_new_id_types_reject_empty_and_whitespace_only_values() {
        assert_blank_id_apis_rejected_with_try_from!(FindingId);
        assert_blank_id_apis_rejected_with_try_from!(SensorId);
        assert_blank_id_apis_rejected_with_try_from!(ActionId);
        assert_blank_id_apis_rejected_with_try_from!(BaselineId);
        assert_blank_id_apis_rejected_with_try_from!(HostId);
        assert_blank_id_apis_rejected_with_try_from!(CgroupId);
    }

    #[test]
    fn test_as_str_returns_what_went_in() -> TestResult {
        assert_eq!(FindingId::try_from_str("finding-1")?.as_str(), "finding-1");
        assert_eq!(SensorId::try_from_str("sensor-1")?.as_str(), "sensor-1");
        assert_eq!(ActionId::try_from_str("action-1")?.as_str(), "action-1");
        assert_eq!(
            BaselineId::try_from_str("baseline-1")?.as_str(),
            "baseline-1"
        );
        assert_eq!(HostId::try_from_str("host-1")?.as_str(), "host-1");
        assert_eq!(CgroupId::try_from_str("cgroup-1")?.as_str(), "cgroup-1");
        Ok(())
    }

    #[test]
    fn test_serde_roundtrip_for_new_id_types() -> TestResult {
        let finding = FindingId::try_from_str("finding-42")?;
        let json = serde_json::to_string(&finding)?;
        assert_eq!(json, "\"finding-42\"");
        let round_tripped: FindingId = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, finding);

        let sensor = SensorId::try_from_str("sensor-42")?;
        let round_tripped: SensorId = serde_json::from_str(&serde_json::to_string(&sensor)?)?;
        assert_eq!(round_tripped, sensor);

        let action = ActionId::try_from_str("action-42")?;
        let round_tripped: ActionId = serde_json::from_str(&serde_json::to_string(&action)?)?;
        assert_eq!(round_tripped, action);

        let baseline = BaselineId::try_from_str("baseline-42")?;
        let round_tripped: BaselineId = serde_json::from_str(&serde_json::to_string(&baseline)?)?;
        assert_eq!(round_tripped, baseline);

        let host = HostId::try_from_str("host-42")?;
        let round_tripped: HostId = serde_json::from_str(&serde_json::to_string(&host)?)?;
        assert_eq!(round_tripped, host);

        let cgroup = CgroupId::try_from_str("cgroup-42")?;
        let round_tripped: CgroupId = serde_json::from_str(&serde_json::to_string(&cgroup)?)?;
        assert_eq!(round_tripped, cgroup);

        assert!(serde_json::from_str::<CgroupId>("\"\"").is_err());
        Ok(())
    }
}

#[cfg(test)]
mod h1_infrastructure_ids_tests {
    use super::{DeviceId, NodeId, SecurityContextId, ServiceIdentityId};
    use crate::test_support::{TestResult, assert_blank_id_apis_rejected_with_try_from};

    #[test]
    fn test_infrastructure_id_types_reject_empty_and_whitespace_only_values() {
        assert_blank_id_apis_rejected_with_try_from!(NodeId);
        assert_blank_id_apis_rejected_with_try_from!(DeviceId);
        assert_blank_id_apis_rejected_with_try_from!(ServiceIdentityId);
        assert_blank_id_apis_rejected_with_try_from!(SecurityContextId);
    }

    #[test]
    fn test_infrastructure_id_types_serde_roundtrip() -> TestResult {
        let node = NodeId::try_from_str("node-1")?;
        assert_eq!(serde_json::to_string(&node)?, "\"node-1\"");
        let round_tripped: NodeId = serde_json::from_str(&serde_json::to_string(&node)?)?;
        assert_eq!(round_tripped, node);

        let device = DeviceId::try_from_str("device-1")?;
        let round_tripped: DeviceId = serde_json::from_str(&serde_json::to_string(&device)?)?;
        assert_eq!(round_tripped, device);

        let service = ServiceIdentityId::try_from_str("svc-1")?;
        let round_tripped: ServiceIdentityId =
            serde_json::from_str(&serde_json::to_string(&service)?)?;
        assert_eq!(round_tripped, service);

        let context = SecurityContextId::try_from_str("ctx-1")?;
        let round_tripped: SecurityContextId =
            serde_json::from_str(&serde_json::to_string(&context)?)?;
        assert_eq!(round_tripped, context);

        assert!(serde_json::from_str::<NodeId>("\" \"").is_err());
        assert!(serde_json::from_str::<DeviceId>("\"\"").is_err());
        assert!(serde_json::from_str::<ServiceIdentityId>("\"\"").is_err());
        assert!(serde_json::from_str::<SecurityContextId>("\"\"").is_err());
        Ok(())
    }
}

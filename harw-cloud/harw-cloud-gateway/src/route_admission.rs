//! Root-Hub route admission, not a listener, directory, or authentication provider.
//!
//! A future composition supplies current device policy from authenticated
//! connection state, the durable session owner fence, and separately observed
//! node liveness. Matching a cache record alone cannot authorize forwarding.
//! Neither this module nor a routing decision establishes device identity,
//! exchanges credentials, or grants a destination session capability.

use std::{fmt, num::NonZeroU64};

/// Action requested by a client. Neither action permits creating or moving an owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachKind {
    /// Read an existing session without resuming or mutating it.
    Observe,
    /// Attach with session-control intent, still subject to owner-side policy.
    Control,
}

/// Connection authority resolved by the trusted composition, not request fields.
///
/// Construct only after authenticating the enrolled device and evaluating the
/// exact session's current policy. This is an in-process input, not serializable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerGrant {
    pub tenant: String,
    pub device_id: String,
    pub authenticated_node: String,
    pub policy_revision: u64,
    pub session_id: String,
    pub may_observe: bool,
    pub may_control: bool,
}

/// The authoritative current record read from durable storage on each admission.
///
/// The tenant is part of the session reference. Generation is directory-issued;
/// an owner epoch is meaningful only within its exact node and host instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerFence {
    pub tenant: String,
    pub session_id: String,
    pub node_id: String,
    pub host_instance: String,
    pub generation: NonZeroU64,
    pub owner_epoch: u64,
}

impl OwnerFence {
    fn complete(&self) -> bool {
        [&self.tenant, &self.session_id, &self.node_id, &self.host_instance]
            .into_iter()
            .all(|field| !field.trim().is_empty())
    }
}

/// Current report from a node authenticated by the existing node transport.
///
/// This is separate from the durable owner record. The composition must resolve
/// freshness and enrolled/active state; a stale, revoked, draining, or unknown
/// report must not be marked eligible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerObservation {
    pub reported_fence: OwnerFence,
    pub eligible: bool,
}

/// Rejection reason for the trusted operator/diagnostic surface.
///
/// A public ingress must not expose a foreign tenant's owner or distinguish an
/// unauthorized session from a nonexistent one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteRefusal {
    Forbidden,
    OwnerUnavailable,
    FenceMismatch,
}

impl fmt::Display for RouteRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Forbidden => "route is not permitted",
            Self::OwnerUnavailable => "session owner is unavailable",
            Self::FenceMismatch => "session owner changed",
        })
    }
}

impl std::error::Error for RouteRefusal {}

/// Opaque, non-transferable decision for in-process routing only.
///
/// There is deliberately no wire format, public constructor, or capability
/// minting path. The owner must re-read authority and authorize the originating
/// device before attaching. Do not forward a caller-supplied URL or target.
#[derive(Debug, Eq, PartialEq)]
pub struct RouteAdmission {
    fence: OwnerFence,
    grant: CallerGrant,
    kind: AttachKind,
}

impl RouteAdmission {
    /// Resolve this node ID through the enrolled node registry, not request input.
    pub fn destination_node(&self) -> &str {
        &self.fence.node_id
    }

    pub fn session_id(&self) -> &str {
        &self.fence.session_id
    }

    pub(crate) fn destination_instance(&self) -> &str {
        &self.fence.host_instance
    }

    #[cfg(all(unix, any(feature = "hub-owner-local-host", feature = "hub-owner-process-proof")))]
    pub(crate) fn observation_context(&self) -> Option<(&OwnerFence, &CallerGrant)> {
        (self.kind == AttachKind::Observe).then_some((&self.fence, &self.grant))
    }

    /// Check the original decision against fresh destination-side state just
    /// before attachment. This does not replace the owner's normal admission,
    /// connection revocation, or operation checks.
    pub fn revalidate_at_owner(
        &self,
        destination_node: &str,
        destination_instance: &str,
        grant: Option<&CallerGrant>,
        current: Option<&OwnerFence>,
    ) -> Result<(), RouteRefusal> {
        self.validate_caller(grant)?;
        let current = current.ok_or(RouteRefusal::OwnerUnavailable)?;
        if current != &self.fence
            || destination_node != current.node_id
            || destination_instance != current.host_instance
        {
            return Err(RouteRefusal::FenceMismatch);
        }
        Ok(())
    }

    pub(crate) fn validate_caller<'a>(
        &self,
        grant: Option<&'a CallerGrant>,
    ) -> Result<&'a CallerGrant, RouteRefusal> {
        let grant = authorized_grant(grant, &self.fence.session_id, self.kind)?;
        if grant == &self.grant {
            Ok(grant)
        } else {
            Err(RouteRefusal::Forbidden)
        }
    }
}

pub(crate) fn authorized_grant<'a>(
    grant: Option<&'a CallerGrant>,
    session_id: &str,
    kind: AttachKind,
) -> Result<&'a CallerGrant, RouteRefusal> {
    grant
        .filter(|grant| {
            !grant.tenant.trim().is_empty()
                && !grant.device_id.trim().is_empty()
                && !grant.authenticated_node.trim().is_empty()
                && !session_id.trim().is_empty()
                && grant.session_id == session_id
                && match kind {
                    AttachKind::Observe => grant.may_observe,
                    AttachKind::Control => grant.may_control,
                }
        })
        .ok_or(RouteRefusal::Forbidden)
}

/// Select only the durable owner under the caller's current session grant.
///
/// `durable` must come from the generation authority, not a node observation
/// or cached lookup. Pass `None` when a current authoritative record is
/// absent or unavailable. An observed owner never becomes the durable owner
/// merely by reporting a newer generation. There is no fallback or mutation.
pub fn admit_route(
    session_id: &str,
    kind: AttachKind,
    grant: Option<&CallerGrant>,
    durable: Option<&OwnerFence>,
    observed: Option<&OwnerObservation>,
) -> Result<RouteAdmission, RouteRefusal> {
    let grant = authorized_grant(grant, session_id, kind)?;
    let durable = durable
        .filter(|fence| fence.complete())
        .ok_or(RouteRefusal::OwnerUnavailable)?;
    if durable.tenant != grant.tenant || durable.session_id != session_id {
        return Err(RouteRefusal::Forbidden);
    }
    let observed = observed
        .filter(|observation| observation.eligible)
        .ok_or(RouteRefusal::OwnerUnavailable)?;
    if &observed.reported_fence != durable {
        return Err(RouteRefusal::FenceMismatch);
    }
    Ok(RouteAdmission {
        fence: durable.clone(),
        grant: grant.clone(),
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CallerGrant, OwnerFence, OwnerObservation) {
        let grant = CallerGrant {
            tenant: "tenant-a".into(),
            device_id: "device-a".into(),
            authenticated_node: "phone-node-a".into(),
            policy_revision: 4,
            session_id: "session-a".into(),
            may_observe: true,
            may_control: false,
        };
        let fence = OwnerFence {
            tenant: grant.tenant.clone(),
            session_id: grant.session_id.clone(),
            node_id: "node-a".into(),
            host_instance: "instance-a".into(),
            generation: NonZeroU64::MIN,
            owner_epoch: 17,
        };
        let observed = OwnerObservation {
            reported_fence: fence.clone(),
            eligible: true,
        };
        (grant, fence, observed)
    }

    #[test]
    fn denies_anonymous_other_tenant_or_control_without_grant() {
        let (mut grant, fence, observed) = fixture();
        for (kind, candidate) in [
            (AttachKind::Observe, None),
            (AttachKind::Control, Some(&grant)),
        ] {
            assert_eq!(
                admit_route("session-a", kind, candidate, Some(&fence), Some(&observed)),
                Err(RouteRefusal::Forbidden)
            );
        }
        grant.tenant = "tenant-b".into();
        assert_eq!(
            admit_route("session-a", AttachKind::Observe, Some(&grant), Some(&fence), Some(&observed)),
            Err(RouteRefusal::Forbidden)
        );
    }

    #[test]
    fn cache_does_not_fill_missing_fence_or_override_current_owner() {
        let (grant, fence, mut observed) = fixture();
        let route = |durable, live| admit_route(
            "session-a", AttachKind::Observe, Some(&grant), durable, live
        );
        assert_eq!(route(None, Some(&observed)), Err(RouteRefusal::OwnerUnavailable));
        observed.reported_fence.host_instance = "new-instance".into();
        assert_eq!(route(Some(&fence), Some(&observed)), Err(RouteRefusal::FenceMismatch));
        observed.reported_fence = fence.clone();
        observed.eligible = false;
        assert_eq!(route(Some(&fence), Some(&observed)), Err(RouteRefusal::OwnerUnavailable));
    }

    #[test]
    fn destination_rechecks_identity_policy_and_fence_after_routing() -> Result<(), RouteRefusal> {
        let (mut grant, mut fence, observed) = fixture();
        let decision = admit_route(
            "session-a", AttachKind::Observe, Some(&grant), Some(&fence), Some(&observed),
        )?;
        assert_eq!(decision.destination_node(), "node-a");
        assert_eq!(decision.session_id(), "session-a");
        decision.revalidate_at_owner("node-a", "instance-a", Some(&grant), Some(&fence))?;
        assert_eq!(
            decision.revalidate_at_owner("node-b", "instance-a", Some(&grant), Some(&fence)),
            Err(RouteRefusal::FenceMismatch)
        );
        fence.owner_epoch += 1;
        assert_eq!(
            decision.revalidate_at_owner("node-a", "instance-a", Some(&grant), Some(&fence)),
            Err(RouteRefusal::FenceMismatch)
        );
        grant.policy_revision += 1;
        assert_eq!(
            decision.revalidate_at_owner("node-a", "instance-a", Some(&grant), Some(&fence)),
            Err(RouteRefusal::Forbidden)
        );
        Ok(())
    }
}

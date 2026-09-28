//! Client identity and admission (W00 §2.4 "Client identity", PL-65 §2.4,
//! §4.1).
//!
//! A [`ClientIdentity`] is built by a listener from transport facts
//! (`SO_PEERCRED` on the local socket, `AuthenticatedPeer` on the node
//! transport). No request payload can replace any of its fields.

use std::sync::atomic::{AtomicU64, Ordering};

use harw_protocol::ClientCaps;
use harw_types::{
    ApprovalActor, AuthStrength, DeviceId, PermissionTier, Principal, TenantId, TrustZone,
};

use crate::error::HostError;

/// Process-unique id of one client connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    /// A fresh, process-unique connection id.
    #[must_use]
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Capability ceiling for a permission tier (PL-65 §2.4 table).
#[must_use]
pub const fn caps_for_tier(tier: PermissionTier) -> ClientCaps {
    match tier {
        PermissionTier::Observer => ClientCaps::OBSERVE,
        PermissionTier::Operator => ClientCaps::OPERATE,
        PermissionTier::Maintainer | PermissionTier::Owner => ClientCaps::ALL,
    }
}

/// Everything the host knows about a caller. Built by a listener, immutable
/// afterwards.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
    /// Authenticated principal (local uid or `device:<id>`).
    pub principal: Principal,
    /// Tenant scope. Mandatory (`Some`) for remote callers.
    pub tenant: Option<TenantId>,
    /// Capability ceiling from tier, device record and security context.
    pub caps: ClientCaps,
    /// Enrolled device, for remote callers.
    pub device: Option<DeviceId>,
    /// Host-derived approval actor. Never taken from the wire.
    pub actor: ApprovalActor,
    /// Display label for presence.
    pub label: String,
    pub zone: TrustZone,
    pub strength: AuthStrength,
    pub connection: ConnectionId,
}

impl ClientIdentity {
    /// True for callers that came through a remote transport.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self.zone, TrustZone::Remote | TrustZone::Cluster)
    }

    /// Validate structural invariants before the identity is used: remote
    /// callers must carry a tenant (an unscoped caller would see every
    /// tenant's sessions).
    pub fn validate(&self) -> Result<(), HostError> {
        if self.is_remote() && self.tenant.is_none() {
            return Err(HostError::Denied("remote caller without tenant".into()));
        }
        Ok(())
    }

    /// Human-readable actor label used in `ApprovalResolved.by` and
    /// `RespondResult::AlreadyResolved.by`.
    #[must_use]
    pub fn actor_label(&self) -> String {
        actor_label(&self.actor)
    }
}

/// Render an approval actor as a stable label.
#[must_use]
pub fn actor_label(actor: &ApprovalActor) -> String {
    match actor {
        ApprovalActor::Operator { id } => format!("operator:{id}"),
        ApprovalActor::ChannelPeer { channel, peer } => {
            format!("{}:{}", channel.as_str(), peer.as_str())
        }
    }
}

/// Tenant admission: a caller sees a session only when the tenants match.
/// An unscoped local caller (`None`) sees unscoped sessions and, like
/// `harw_operations::context::tenant_admits(None, _)`, every tenant; remote
/// callers never reach this with `None` (see [`ClientIdentity::validate`]).
#[must_use]
pub fn tenant_admits(caller: Option<&TenantId>, record: Option<&TenantId>) -> bool {
    match (caller, record) {
        (None, _) => true,
        (Some(caller), Some(record)) => caller == record,
        (Some(_), None) => false,
    }
}

/// One capability a call needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Observe,
    Steer,
    Approve,
    Control,
}

impl Need {
    /// True when `caps` contains this capability.
    #[must_use]
    pub const fn is_granted(self, caps: ClientCaps) -> bool {
        match self {
            Self::Observe => caps.observe,
            Self::Steer => caps.steer,
            Self::Approve => caps.approve,
            Self::Control => caps.control,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Steer => "steer",
            Self::Approve => "approve",
            Self::Control => "control",
        }
    }
}

/// Admit a call against a session: tenant first (answered as `NotFound` so a
/// foreign session's existence does not leak), then the capability from the
/// connection's granted caps.
pub fn admit(
    identity: &ClientIdentity,
    granted: ClientCaps,
    record_tenant: Option<&TenantId>,
    need: Need,
) -> Result<(), HostError> {
    if !tenant_admits(identity.tenant.as_ref(), record_tenant) {
        return Err(HostError::NotFound);
    }
    if !need.is_granted(granted) {
        return Err(HostError::Denied(format!(
            "capability `{}` not granted",
            need.name()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_identity {
    use harw_types::{
        ApprovalActor, AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
        TenantId, TrustZone,
    };

    use super::{ClientIdentity, ConnectionId, caps_for_tier};

    /// A local operator identity for tests.
    pub(crate) fn local(uid: u32, tier: PermissionTier) -> ClientIdentity {
        ClientIdentity {
            principal: Principal::trusted_ingress(
                PrincipalKind::Human,
                format!("uid:{uid}"),
                IngressSurface::Tui,
                tier,
            ),
            tenant: None,
            caps: caps_for_tier(tier),
            device: None,
            actor: ApprovalActor::Operator {
                id: format!("uid:{uid}"),
            },
            label: format!("local-{uid}"),
            zone: TrustZone::Local,
            strength: AuthStrength::PeerCredential,
            connection: ConnectionId::next(),
        }
    }

    /// A tenant-scoped local identity for tests.
    pub(crate) fn scoped(uid: u32, tier: PermissionTier, tenant: &TenantId) -> ClientIdentity {
        ClientIdentity {
            tenant: Some(tenant.clone()),
            ..local(uid, tier)
        }
    }
}

#[cfg(test)]
mod tests {
    use harw_types::{PermissionTier, TenantId, TrustZone};

    use super::test_identity::{local, scoped};
    use super::*;

    #[test]
    fn remote_identity_without_tenant_is_refused() {
        let mut identity = local(1000, PermissionTier::Operator);
        identity.zone = TrustZone::Remote;
        assert!(identity.validate().is_err());
    }

    #[test]
    fn foreign_tenant_reads_as_not_found() -> Result<(), harw_types::InvalidId> {
        let a = TenantId::try_from_str("a")?;
        let b = TenantId::try_from_str("b")?;
        let caller = scoped(1, PermissionTier::Owner, &a);
        assert_eq!(
            admit(&caller, ClientCaps::ALL, Some(&b), Need::Observe),
            Err(HostError::NotFound)
        );
        assert_eq!(
            admit(&caller, ClientCaps::ALL, Some(&a), Need::Observe),
            Ok(())
        );
        assert_eq!(
            admit(&caller, ClientCaps::ALL, None, Need::Observe),
            Err(HostError::NotFound)
        );
        Ok(())
    }

    #[test]
    fn missing_capability_is_denied() {
        let caller = local(1, PermissionTier::Observer);
        assert!(matches!(
            admit(&caller, ClientCaps::OBSERVE, None, Need::Steer),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn tier_ceilings_follow_the_table() {
        assert_eq!(caps_for_tier(PermissionTier::Observer), ClientCaps::OBSERVE);
        assert_eq!(caps_for_tier(PermissionTier::Operator), ClientCaps::OPERATE);
        assert_eq!(caps_for_tier(PermissionTier::Maintainer), ClientCaps::ALL);
    }
}

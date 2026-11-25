//! Host-internal authenticated membership lookup. It neither grants a
//! capability nor decides access to another device's data.

use harw_types::{DeviceId, NodeId, TenantId};

use crate::{ClientIdentity, HostError};

/// Current, exact local registry evidence as of one read. This is not a
/// bearer grant, a serializable token, or a substitute for live revalidation.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedUserMembership {
    pub user_id: String,
    pub tenant: TenantId,
    pub device: DeviceId,
    pub node: NodeId,
    pub key_fingerprint: [u8; 32],
    pub issued_revision: u64,
}

impl VerifiedUserMembership {
    pub(crate) fn matches(&self, identity: &ClientIdentity) -> bool {
        identity.tenant.as_ref() == Some(&self.tenant)
            && identity.device.as_ref() == Some(&self.device)
            && identity.authenticated_node.as_ref() == Some(&self.node)
            && identity.authenticated_key_fingerprint == Some(self.key_fingerprint)
    }
}

/// Implemented only by trusted listener composition over the mounted
/// profile. Inputs are immutable host-provided identity, never request JSON.
pub trait UserMembershipResolver: Send + Sync {
    fn resolve_current(&self, identity: &ClientIdentity) -> Result<Option<VerifiedUserMembership>, HostError>;

    /// Recheck an exact previously authenticated pending binding, without
    /// treating it as a connection credential or finding a member by label.
    fn binding_is_current(&self, _binding: &VerifiedUserMembership) -> Result<bool, HostError> {
        Ok(false)
    }
}

//! Shared helpers for unit tests.

use harw_session_host::ClientIdentity;
use harw_types::PermissionTier;

/// A valid local identity labelled `label`.
pub(crate) fn identity(label: &str) -> ClientIdentity {
    let mut identity = crate::local::local_identity(1000, PermissionTier::Operator);
    identity.label = label.to_owned();
    identity
}

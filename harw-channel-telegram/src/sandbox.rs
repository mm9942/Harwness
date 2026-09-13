//! Intersection-only capability reduction for Telegram-originated sessions.

use std::collections::BTreeSet;

use harw_sandbox::{Permission, PermissionSet};

/// Conservative channel profile applied after the upstream policy decision.
///
/// The profile has no allow/grant mutation: [`reduce`](Self::reduce) can only
/// intersect a pre-existing policy result. This keeps Telegram a capability
/// perimeter rather than a parallel authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSandbox {
    denied: BTreeSet<Permission>,
}

impl TelegramSandbox {
    /// The default remote-surface reduction.
    #[must_use]
    pub fn reduced_default() -> Self {
        Self {
            denied: [
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::ReadSecrets,
                Permission::ManagePlugins,
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Intersects a policy-authorized set with this Telegram restriction.
    #[must_use]
    pub fn reduce<I>(&self, upstream: I) -> PermissionSet
    where
        I: IntoIterator<Item = Permission>,
    {
        PermissionSet::from_policy(
            upstream
                .into_iter()
                .filter(|capability| !self.denied.contains(capability)),
        )
    }
}

//! Intersection-only capability reduction for Telegram-originated sessions.

use std::collections::BTreeSet;

use harw_authority::{Permission, PermissionSet};

/// Conservative channel profile applied after the upstream policy decision.
///
/// The profile has no allow/grant mutation: [`reduce`](Self::reduce) can only
/// intersect a pre-existing policy result with a fixed allowlist. This keeps
/// Telegram a capability perimeter rather than a parallel authority, and
/// means a newly added [`Permission`] variant is denied by default instead
/// of passing through unnoticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSandbox {
    allowed: BTreeSet<Permission>,
}

impl TelegramSandbox {
    /// The default remote-surface reduction.
    #[must_use]
    pub fn reduced_default() -> Self {
        Self {
            allowed: [Permission::ReadWorkspace, Permission::NetworkAccess]
                .into_iter()
                .collect(),
        }
    }

    /// Intersects a policy-authorized set with this Telegram allowlist.
    #[must_use]
    pub fn reduce<I>(&self, upstream: I) -> PermissionSet
    where
        I: IntoIterator<Item = Permission>,
    {
        PermissionSet::from_policy(
            upstream
                .into_iter()
                .filter(|capability| self.allowed.contains(capability)),
        )
    }
}

#[cfg(test)]
mod tests {
    use harw_authority::{Permission, PermissionSet};

    use super::TelegramSandbox;
    use crate::test_support::TestResult;

    /// The default profile must keep exactly the allowlisted permissions,
    /// even when upstream grants every current and future [`Permission`]
    /// variant (guards against the denylist-style regression where a new
    /// variant such as `ReadCargoRegistry` passed through unnoticed).
    #[test]
    fn reduced_default_allows_only_read_workspace_and_network() -> TestResult {
        let expected =
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::NetworkAccess]);

        let reduced = TelegramSandbox::reduced_default().reduce(Permission::ALL);

        assert_eq!(reduced, expected);
        assert!(!reduced.contains(Permission::ReadCargoRegistry));
        Ok(())
    }
}

//! Explicit authority to inspect one locally selected profile's device registry.
//! This is not a tenant identity or service-mutation grant.

use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};

/// Read-only local profile administration, minted at a trusted TUI boundary.
///
/// No wire representation or default constructor exists. Runtime composition
/// additionally restricts mounting this capability to its local TUI entry and
/// cloud read operations. An explicit caller tenant always narrows visibility.
#[derive(Debug, Clone, Copy)]
pub struct LocalProfileOperator {
    _private: (),
}

impl LocalProfileOperator {
    /// Mint only for the authenticated local human operator of the TUI.
    #[must_use]
    pub fn from_principal(principal: &Principal) -> Option<Self> {
        (principal.kind() == PrincipalKind::Human
            && principal.surface() == IngressSurface::Tui
            && principal.tier() >= PermissionTier::Operator)
            .then_some(Self { _private: () })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_profile_capability_requires_local_human_operator() {
        for (kind, surface, tier, allowed) in [
            (
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Operator,
                true,
            ),
            (
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Maintainer,
                true,
            ),
            (
                PrincipalKind::Model,
                IngressSurface::Child,
                PermissionTier::Operator,
                false,
            ),
            (
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Owner,
                true,
            ),
            (
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Observer,
                false,
            ),
            (
                PrincipalKind::Human,
                IngressSurface::Gateway,
                PermissionTier::Owner,
                false,
            ),
            (
                PrincipalKind::Model,
                IngressSurface::Tui,
                PermissionTier::Owner,
                false,
            ),
        ] {
            assert_eq!(
                LocalProfileOperator::from_principal(&Principal::trusted_ingress(
                    kind, "operator", surface, tier
                ))
                .is_some(),
                allowed
            );
        }
    }
}

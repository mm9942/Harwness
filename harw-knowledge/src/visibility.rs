//! `VisibilityScope` and the local agent-identity newtypes it references.
//!
//! Spec: `docs/design/knowledge-surfaces.md` §7. One scope enum is applied to
//! every surface (memory, palace, diary, dream, workbench, kanban). The rule
//! that generalizes all seven §7 rows: *read follows the scope; write
//! additionally requires being the sole author, being inside the granted set,
//! or going through the surface's own promotion/lease mechanism.*
//!
//! `AgentId` and `AgentRoleRef` are defined here because `harw-types` does not
//! (yet) export them and §7/§8.1 reference them unqualified. Full cross-tree
//! enforcement (SelfOnly identity match, DescendantTree spawn-tree walk) is
//! `harw-policy`'s job; this crate implements only the locally-decidable
//! predicates and fails closed when the supplied caller context cannot prove
//! visibility.

use serde::{Deserialize, Serialize};

id_newtype!(
    /// A concrete agent identity (the author/holder of an artifact or lease).
    AgentId
);
id_newtype!(
    /// A reference to an agent *role* (used in grants and worker-lane binding).
    AgentRoleRef
);

/// Visibility scope applied uniformly to every knowledge surface (§7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityScope {
    /// Only the authoring agent identity can read/write.
    SelfOnly,
    /// The authoring agent and every descendant it spawns (subagent tree).
    DescendantTree,
    /// An explicit allowlist of agent/role identities.
    ExplicitlyGranted(Vec<AgentRoleRef>),
    /// Only a human operator (never an agent) can read/write.
    OperatorOnly,
}

impl VisibilityScope {
    /// Returns `true` when this scope is operator-restricted.
    #[must_use]
    pub fn is_operator_only(&self) -> bool {
        matches!(self, Self::OperatorOnly)
    }

    /// Returns `true` when `role` appears in an `ExplicitlyGranted` allowlist.
    #[must_use]
    pub fn grants_role(&self, role: &AgentRoleRef) -> bool {
        matches!(self, Self::ExplicitlyGranted(roles) if roles.contains(role))
    }

    /// Conservative read check used by recall/index filtering (§2.3).
    ///
    /// `OperatorOnly` artifacts are visible only to an `OperatorOnly` caller.
    /// The current caller context contains no agent identity, descendant-tree
    /// proof, or granted-role identity, so it cannot prove access to any other
    /// restricted scope. Those scopes therefore fail closed until a richer
    /// caller context is provided by the policy layer.
    #[must_use]
    pub fn visible_to_caller(&self, caller: &VisibilityScope) -> bool {
        match self {
            Self::OperatorOnly => caller.is_operator_only(),
            Self::SelfOnly | Self::DescendantTree | Self::ExplicitlyGranted(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentRoleRef, VisibilityScope};

    #[test]
    fn restricted_scopes_fail_closed_without_identity_context() {
        let callers = [
            VisibilityScope::SelfOnly,
            VisibilityScope::DescendantTree,
            VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]),
            VisibilityScope::OperatorOnly,
        ];

        let restricted_scopes = [
            VisibilityScope::SelfOnly,
            VisibilityScope::DescendantTree,
            VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]),
        ];

        for visibility in &restricted_scopes {
            for caller in &callers {
                assert!(
                    !visibility.visible_to_caller(caller),
                    "{visibility:?} must fail closed for {caller:?}"
                );
            }
        }
    }

    #[test]
    fn operator_only_visibility_remains_limited_to_operator_callers() {
        let visibility = VisibilityScope::OperatorOnly;

        assert!(visibility.visible_to_caller(&VisibilityScope::OperatorOnly));
        assert!(!visibility.visible_to_caller(&VisibilityScope::SelfOnly));
    }
}

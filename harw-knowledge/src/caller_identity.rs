//! Identity-aware visibility predicate (PL-90 W03d-2).
//!
//! [`VisibilityScope::visible_to_caller`] has no agent identity to work with,
//! so it fails closed for `SelfOnly`, `DescendantTree` and `ExplicitlyGranted`.
//! This module supplies the richer caller context ([`CallerIdentity`]) and a
//! pure predicate ([`is_visible`]) that can actually decide those scopes.
//!
//! Nothing here performs I/O and nothing here verifies identities: the caller
//! identity (in particular `role` and `ancestors`) is trusted as supplied by
//! admission in `harw-policy`. A caller must never construct it from
//! agent-controlled input.

use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

/// Who is asking, as established by admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerIdentity {
    /// The concrete identity of the calling agent.
    pub agent_id: AgentId,
    /// The role the caller was admitted under, if any.
    pub role: Option<AgentRoleRef>,
    /// The caller's spawn ancestors, nearest parent first. Trusted from
    /// admission.
    pub ancestors: Vec<AgentId>,
    /// `true` when the caller is a human operator rather than an agent.
    pub operator: bool,
}

impl CallerIdentity {
    /// Create an agent caller with no role, no ancestors and no operator rights.
    #[must_use]
    pub fn new(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            role: None,
            ancestors: Vec::new(),
            operator: false,
        }
    }

    /// Set the role the caller was admitted under.
    #[must_use]
    pub fn with_role(mut self, role: AgentRoleRef) -> Self {
        self.role = Some(role);
        self
    }

    /// Set the spawn ancestors, nearest parent first.
    #[must_use]
    pub fn with_ancestors(mut self, ancestors: Vec<AgentId>) -> Self {
        self.ancestors = ancestors;
        self
    }

    /// Mark the caller as a human operator.
    #[must_use]
    pub fn as_operator(mut self) -> Self {
        self.operator = true;
        self
    }
}

/// The authorship and visibility scope of one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactOwner {
    /// The agent that authored the artifact.
    pub author: AgentId,
    /// The visibility scope the author assigned to it.
    pub scope: VisibilityScope,
}

/// Decide whether `caller` may read the artifact described by `owner`.
///
/// Rules (all fail closed):
/// - A caller whose agent id is blank (empty after trimming) is always denied.
/// - `OperatorOnly`: visible only when `caller.operator` is set.
/// - `SelfOnly`: visible only when the caller is the author.
/// - `DescendantTree`: visible to the author and to every descendant of the
///   author, i.e. when the author appears in `caller.ancestors`. The relation
///   is directional: a child sees its ancestor's tree, but a parent does not
///   see its child's tree, and a **sibling never sees a sibling's tree**,
///   because a sibling is not the author and the author is not one of its
///   ancestors (only their common parent is).
/// - `ExplicitlyGranted`: visible when the caller's role is in the allowlist.
///   A caller without a role is denied.
///
/// `SelfOnly` and `DescendantTree` additionally deny when the author id is
/// blank, so an unattributed artifact never matches by accident.
///
/// The scope enum has no public/workspace-wide variant today, so there is no
/// arm that mirrors a permissive `visible_to_caller` result; should one be
/// added, this match stops compiling and must be decided explicitly.
#[must_use]
pub fn is_visible(owner: &ArtifactOwner, caller: &CallerIdentity) -> bool {
    if caller.agent_id.as_str().trim().is_empty() {
        return false;
    }
    let author_known = !owner.author.as_str().trim().is_empty();
    match &owner.scope {
        VisibilityScope::OperatorOnly => caller.operator,
        VisibilityScope::SelfOnly => author_known && caller.agent_id == owner.author,
        VisibilityScope::DescendantTree => {
            author_known
                && (caller.agent_id == owner.author || caller.ancestors.contains(&owner.author))
        }
        VisibilityScope::ExplicitlyGranted(roles) => caller
            .role
            .as_ref()
            .is_some_and(|role| roles.contains(role)),
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactOwner, CallerIdentity, is_visible};
    use crate::visibility::{AgentId, AgentRoleRef, VisibilityScope};

    fn owner(author: &str, scope: VisibilityScope) -> ArtifactOwner {
        ArtifactOwner {
            author: AgentId::new(author),
            scope,
        }
    }

    #[test]
    fn self_sees_self() {
        let caller = CallerIdentity::new(AgentId::new("a"));
        assert!(is_visible(&owner("a", VisibilityScope::SelfOnly), &caller));
        assert!(is_visible(
            &owner("a", VisibilityScope::DescendantTree),
            &caller
        ));
    }

    #[test]
    fn sibling_does_not_see_sibling_artifact() {
        // `parent` spawned both `a` and `b`.
        let b = CallerIdentity::new(AgentId::new("b")).with_ancestors(vec![AgentId::new("parent")]);
        assert!(!is_visible(&owner("a", VisibilityScope::SelfOnly), &b));
        assert!(!is_visible(
            &owner("a", VisibilityScope::DescendantTree),
            &b
        ));
    }

    #[test]
    fn child_sees_parent_descendant_tree() {
        let child = CallerIdentity::new(AgentId::new("child"))
            .with_ancestors(vec![AgentId::new("parent"), AgentId::new("root")]);
        assert!(is_visible(
            &owner("parent", VisibilityScope::DescendantTree),
            &child
        ));
        assert!(is_visible(
            &owner("root", VisibilityScope::DescendantTree),
            &child
        ));
        assert!(!is_visible(
            &owner("parent", VisibilityScope::SelfOnly),
            &child
        ));
    }

    #[test]
    fn parent_does_not_see_child_descendant_tree() {
        let parent =
            CallerIdentity::new(AgentId::new("parent")).with_ancestors(vec![AgentId::new("root")]);
        assert!(!is_visible(
            &owner("child", VisibilityScope::DescendantTree),
            &parent
        ));
    }

    #[test]
    fn granted_role_sees_ungranted_role_does_not() {
        let scope = VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]);
        let granted = CallerIdentity::new(AgentId::new("x")).with_role(AgentRoleRef::new("reader"));
        let ungranted =
            CallerIdentity::new(AgentId::new("y")).with_role(AgentRoleRef::new("writer"));
        let roleless = CallerIdentity::new(AgentId::new("z"));
        assert!(is_visible(&owner("a", scope.clone()), &granted));
        assert!(!is_visible(&owner("a", scope.clone()), &ungranted));
        assert!(!is_visible(&owner("a", scope), &roleless));
    }

    #[test]
    fn blank_caller_id_is_denied() {
        let scopes = [
            VisibilityScope::SelfOnly,
            VisibilityScope::DescendantTree,
            VisibilityScope::OperatorOnly,
            VisibilityScope::ExplicitlyGranted(vec![AgentRoleRef::new("reader")]),
        ];
        for scope in scopes {
            let caller = CallerIdentity::new(AgentId::new("  "))
                .with_role(AgentRoleRef::new("reader"))
                .as_operator();
            assert!(!is_visible(&owner("  ", scope.clone()), &caller));
            assert!(!is_visible(&owner("a", scope), &caller));
        }
    }

    #[test]
    fn operator_only_requires_operator_flag() {
        let scope = VisibilityScope::OperatorOnly;
        let agent = CallerIdentity::new(AgentId::new("a"));
        let operator = CallerIdentity::new(AgentId::new("op")).as_operator();
        assert!(!is_visible(&owner("a", scope.clone()), &agent));
        assert!(is_visible(&owner("a", scope), &operator));
    }

    #[test]
    fn forged_role_does_not_help_self_only() {
        let attacker = CallerIdentity::new(AgentId::new("mallory"))
            .with_role(AgentRoleRef::new("a"))
            .with_ancestors(vec![AgentId::new("root")]);
        assert!(!is_visible(
            &owner("a", VisibilityScope::SelfOnly),
            &attacker
        ));
    }
}

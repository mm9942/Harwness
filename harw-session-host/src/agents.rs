//! Agent principal registry (R18 §3, §4.1).
//!
//! Maps a gateway-issued agent credential to an [`AgentPrincipal`] and holds
//! the live tool grant of every agent. Listeners resolve a credential here
//! when they build a [`crate::ClientIdentity`]; the host re-reads the live
//! principal on every `tool.call`, so a grant change or a revocation takes
//! effect for the next call without reconnecting.
//!
//! Rules:
//! - Only the UIA is enrolled with a grant of its own, bounded by the
//!   configured UIA ceiling.
//! - Every other agent is a delegate: its grant is a subset of its parent's
//!   grant ([`AgentPrincipal::delegate`]), it carries its parent's tenant and
//!   may narrow its parent's gateway sandbox further ([`Delegation::sandbox`]).
//! - `grant` replaces a grant within the ceiling (parent grant or UIA
//!   ceiling); `narrow` removes names. Both cascade: every descendant is cut
//!   to `descendant ∩ parent` immediately.
//! - `revoke` revokes the agent and all its descendants. A revoked agent
//!   never becomes active again.
//!
//! The credential format per transport is W04/W05 work; here a credential is
//! an opaque, non-empty secret string that is never logged.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use harw_authority::PermissionRequest;
use harw_protocol::AgentRole;
use harw_protocol::session_wire::GatewayToolRights;
use harw_types::TenantId;

use crate::error::HostError;
use crate::identity::{AgentPrincipal, ToolGrant, tenant_admits};

/// Opaque, gateway-issued agent credential. Never printed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AgentCredential(String);

impl AgentCredential {
    /// Wrap a credential secret; blank secrets are refused.
    pub fn new(secret: impl Into<String>) -> Result<Self, HostError> {
        let secret = secret.into();
        if secret.trim().is_empty() {
            return Err(HostError::Denied("empty agent credential".into()));
        }
        Ok(Self(secret))
    }
}

impl fmt::Debug for AgentCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentCredential(<redacted>)")
    }
}

/// A delegation request of a parent agent for one child agent.
#[derive(Clone, Debug)]
pub struct Delegation {
    /// Gateway-issued id of the child agent principal.
    pub child_id: String,
    /// Role of the child; never [`AgentRole::UserInterface`] or `Unknown`.
    pub role: AgentRole,
    /// Tools for the child; must lie within the parent's grant.
    pub tools: ToolGrant,
    /// Further narrowing of the parent's gateway sandbox for this child
    /// (`None`: same sandbox as the parent). The resulting spec always
    /// satisfies `child.ensure_child_of(parent)`.
    pub sandbox: Option<PermissionRequest>,
}

/// A resolved, active agent: what a listener needs to build an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAgent {
    pub principal: AgentPrincipal,
    /// Tenant of the agent (the UIA's tenant for every delegate).
    pub tenant: Option<TenantId>,
}

/// Live state of one agent as the host sees it on a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentState {
    pub(crate) principal: AgentPrincipal,
    pub(crate) tenant: Option<TenantId>,
    pub(crate) revoked: bool,
}

#[derive(Clone, Debug)]
struct AgentEntry {
    principal: AgentPrincipal,
    tenant: Option<TenantId>,
    sandbox: Option<PermissionRequest>,
    revoked: bool,
}

#[derive(Debug, Default)]
struct Agents {
    by_id: BTreeMap<String, AgentEntry>,
    by_credential: HashMap<AgentCredential, String>,
}

impl Agents {
    fn entry(&self, id: &str) -> Result<&AgentEntry, HostError> {
        self.by_id.get(id).ok_or(HostError::NotFound)
    }

    fn active(&self, id: &str) -> Result<&AgentEntry, HostError> {
        let entry = self.entry(id)?;
        if entry.revoked {
            return Err(HostError::Revoked);
        }
        Ok(entry)
    }

    fn register(
        &mut self,
        credential: AgentCredential,
        entry: AgentEntry,
    ) -> Result<AgentPrincipal, HostError> {
        let id = entry.principal.id.clone();
        if self.by_id.contains_key(&id) {
            return Err(HostError::Denied(format!(
                "agent principal `{id}` already exists"
            )));
        }
        if self.by_credential.contains_key(&credential) {
            return Err(HostError::Denied("agent credential already in use".into()));
        }
        let principal = entry.principal.clone();
        self.by_credential.insert(credential, id.clone());
        self.by_id.insert(id, entry);
        Ok(principal)
    }

    /// Ids of every descendant of `id`, parents before children.
    fn descendants(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut frontier: Vec<String> = vec![id.to_owned()];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(id.to_owned());
        while let Some(parent) = frontier.pop() {
            for (child, entry) in &self.by_id {
                if entry.principal.parent.as_deref() == Some(parent.as_str())
                    && seen.insert(child.clone())
                {
                    out.push(child.clone());
                    frontier.push(child.clone());
                }
            }
        }
        out
    }

    /// Cut every descendant of `id` to `descendant ∩ parent`.
    fn cascade(&mut self, id: &str) {
        for child in self.descendants(id) {
            let parent_grant = self
                .by_id
                .get(&child)
                .and_then(|entry| entry.principal.parent.clone())
                .and_then(|parent| self.by_id.get(&parent))
                .map(|entry| entry.principal.tools.clone())
                .unwrap_or_default();
            if let Some(entry) = self.by_id.get_mut(&child) {
                entry.principal.tools = entry.principal.tools.intersect(&parent_grant);
            }
        }
    }
}

/// In-memory agent principal registry (R18 §3).
pub struct AgentRegistry {
    uia_ceiling: ToolGrant,
    agents: Mutex<Agents>,
}

impl fmt::Debug for AgentRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentRegistry")
            .field("uia_ceiling", &self.uia_ceiling)
            .finish_non_exhaustive()
    }
}

impl AgentRegistry {
    /// An empty registry whose UIA grants are bounded by `uia_ceiling`.
    #[must_use]
    pub fn new(uia_ceiling: ToolGrant) -> Self {
        Self {
            uia_ceiling,
            agents: Mutex::new(Agents::default()),
        }
    }

    /// The configured UIA ceiling.
    #[must_use]
    pub fn uia_ceiling(&self) -> &ToolGrant {
        &self.uia_ceiling
    }

    fn lock(&self) -> Result<MutexGuard<'_, Agents>, HostError> {
        self.agents
            .lock()
            .map_err(|_| HostError::Storage("agent registry poisoned".into()))
    }

    /// Enroll a User Interface Agent with `tools`, which must lie within the
    /// UIA ceiling (a name outside is refused, never dropped).
    pub fn enroll_uia(
        &self,
        credential: AgentCredential,
        id: impl Into<String>,
        tenant: Option<TenantId>,
        tools: &ToolGrant,
    ) -> Result<AgentPrincipal, HostError> {
        let principal = AgentPrincipal {
            id: id.into(),
            role: AgentRole::UserInterface,
            parent: None,
            tools: self.uia_ceiling.narrow_to(tools)?,
        };
        principal.validate()?;
        self.lock()?.register(
            credential,
            AgentEntry {
                principal,
                tenant,
                sandbox: None,
                revoked: false,
            },
        )
    }

    /// Delegate a child of the active agent `parent` (R18 §4.1). The child
    /// carries the parent's tenant; its grant must lie within the parent's.
    pub fn delegate(
        &self,
        parent: &str,
        credential: AgentCredential,
        delegation: Delegation,
    ) -> Result<AgentPrincipal, HostError> {
        let mut agents = self.lock()?;
        let parent_entry = agents.active(parent)?.clone();
        if delegation.role == AgentRole::UserInterface {
            return Err(HostError::Denied(
                "a user interface agent cannot be delegated".into(),
            ));
        }
        let child = parent_entry.principal.delegate(
            delegation.child_id,
            delegation.role,
            &delegation.tools,
        )?;
        agents.register(
            credential,
            AgentEntry {
                principal: child,
                tenant: parent_entry.tenant,
                sandbox: delegation.sandbox,
                revoked: false,
            },
        )
    }

    /// The active agent behind `credential`; `None` when unknown or revoked.
    #[must_use]
    pub fn resolve(&self, credential: &AgentCredential) -> Option<ResolvedAgent> {
        let agents = self.lock().ok()?;
        let id = agents.by_credential.get(credential)?;
        let entry = agents.active(id).ok()?;
        Some(ResolvedAgent {
            principal: entry.principal.clone(),
            tenant: entry.tenant.clone(),
        })
    }

    /// Live state of `id` (`NotFound` when never enrolled).
    pub(crate) fn state(&self, id: &str) -> Result<AgentState, HostError> {
        let agents = self.lock()?;
        let entry = agents.entry(id)?;
        Ok(AgentState {
            principal: entry.principal.clone(),
            tenant: entry.tenant.clone(),
            revoked: entry.revoked,
        })
    }

    /// True when `id` is enrolled and revoked. Unknown ids are not revoked
    /// here; admission refuses them separately.
    #[must_use]
    pub fn is_revoked(&self, id: &str) -> bool {
        self.lock()
            .map(|agents| agents.by_id.get(id).is_some_and(|entry| entry.revoked))
            .unwrap_or(true)
    }

    /// Sandbox narrowing requests from the UIA down to `id` (UIA first).
    pub(crate) fn sandbox_chain(&self, id: &str) -> Result<Vec<PermissionRequest>, HostError> {
        let agents = self.lock()?;
        let mut chain = Vec::new();
        let mut current = Some(id.to_owned());
        let mut hops = 0usize;
        while let Some(agent) = current {
            let entry = agents.entry(&agent)?;
            if let Some(request) = &entry.sandbox {
                chain.push(request.clone());
            }
            current = entry.principal.parent.clone();
            hops += 1;
            if hops > agents.by_id.len() {
                return Err(HostError::Denied("agent delegation chain loops".into()));
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// The delegation chain of `id`: `id` first, then its parent, …, up to
    /// the UIA (R18 §4: session binding). `NotFound` for an unknown agent.
    pub(crate) fn lineage(&self, id: &str) -> Result<Vec<String>, HostError> {
        let agents = self.lock()?;
        let mut chain = Vec::new();
        let mut current = Some(id.to_owned());
        while let Some(agent) = current {
            let entry = agents.entry(&agent)?;
            current = entry.principal.parent.clone();
            chain.push(agent);
            if chain.len() > agents.by_id.len() {
                return Err(HostError::Denied("agent delegation chain loops".into()));
            }
        }
        Ok(chain)
    }

    /// `gateway.tools.grant`: replace the grant of `id` within its ceiling
    /// (parent grant, or the UIA ceiling), then cut every descendant.
    pub fn grant(&self, id: &str, tools: &ToolGrant) -> Result<AgentPrincipal, HostError> {
        let mut agents = self.lock()?;
        let entry = agents.active(id)?;
        let ceiling = match entry.principal.parent.as_deref() {
            Some(parent) => agents.entry(parent)?.principal.tools.clone(),
            None => self.uia_ceiling.clone(),
        };
        let granted = ceiling.narrow_to(tools)?;
        let principal = match agents.by_id.get_mut(id) {
            Some(entry) => {
                entry.principal.tools = granted;
                entry.principal.clone()
            }
            None => return Err(HostError::NotFound),
        };
        agents.cascade(id);
        Ok(principal)
    }

    /// `gateway.tools.narrow`: remove `tools` from the grant of `id`, then
    /// cut every descendant.
    pub fn narrow(&self, id: &str, tools: &ToolGrant) -> Result<AgentPrincipal, HostError> {
        let mut agents = self.lock()?;
        agents.active(id)?;
        let principal = match agents.by_id.get_mut(id) {
            Some(entry) => {
                entry.principal.tools = entry.principal.tools.without(tools);
                entry.principal.clone()
            }
            None => return Err(HostError::NotFound),
        };
        agents.cascade(id);
        Ok(principal)
    }

    /// Revoke `id` and every descendant. Returns the ids revoked by this
    /// call (empty when all were revoked already).
    pub fn revoke(&self, id: &str) -> Result<Vec<String>, HostError> {
        let mut agents = self.lock()?;
        agents.entry(id)?;
        let mut targets = vec![id.to_owned()];
        targets.extend(agents.descendants(id));
        let mut revoked = Vec::new();
        for target in targets {
            if let Some(entry) = agents.by_id.get_mut(&target) {
                if !entry.revoked {
                    entry.revoked = true;
                    revoked.push(target);
                }
            }
        }
        Ok(revoked)
    }

    /// Tenant of `id` (for admission of `gateway.tools.*`).
    pub(crate) fn tenant_of(&self, id: &str) -> Result<Option<TenantId>, HostError> {
        Ok(self.lock()?.entry(id)?.tenant.clone())
    }

    /// Wire view of the active agents visible to `caller`'s tenant.
    pub fn rights(&self, caller: Option<&TenantId>) -> Vec<GatewayToolRights> {
        let Ok(agents) = self.lock() else {
            return Vec::new();
        };
        agents
            .by_id
            .values()
            .filter(|entry| !entry.revoked && tenant_admits(caller, entry.tenant.as_ref()))
            .map(|entry| rights_of(&entry.principal))
            .collect()
    }
}

/// Wire view of one principal's grant.
#[must_use]
pub fn rights_of(principal: &AgentPrincipal) -> GatewayToolRights {
    GatewayToolRights {
        agent: principal.id.clone(),
        role: principal.role,
        tools: principal.tools.to_names(),
    }
}

#[cfg(test)]
mod tests {
    use harw_authority::Permission;

    use super::*;

    type TestResult<T = ()> = Result<T, HostError>;

    fn cred(secret: &str) -> TestResult<AgentCredential> {
        AgentCredential::new(secret)
    }

    fn registry() -> AgentRegistry {
        AgentRegistry::new(ToolGrant::from_names(["fs.read", "fs.write", "shell.exec"]))
    }

    fn delegation(id: &str, tools: &[&str]) -> Delegation {
        Delegation {
            child_id: id.into(),
            role: AgentRole::Worker,
            tools: ToolGrant::from_names(tools.iter().copied()),
            sandbox: None,
        }
    }

    #[test]
    fn credentials_are_redacted_and_never_blank() -> TestResult {
        assert!(AgentCredential::new("  ").is_err());
        let credential = cred("s3cret")?;
        assert!(!format!("{credential:?}").contains("s3cret"));
        Ok(())
    }

    #[test]
    fn uia_grant_is_bounded_by_the_ceiling() -> TestResult {
        let agents = registry();
        assert!(matches!(
            agents.enroll_uia(
                cred("c0")?,
                "agent:uia",
                None,
                &ToolGrant::from_names(["web.fetch"])
            ),
            Err(HostError::Denied(_))
        ));
        let uia = agents.enroll_uia(
            cred("c1")?,
            "agent:uia",
            None,
            &ToolGrant::from_names(["fs.read"]),
        )?;
        assert!(uia.is_uia());
        let resolved = agents.resolve(&cred("c1")?).ok_or(HostError::NotFound)?;
        assert_eq!(resolved.principal, uia);
        assert!(agents.resolve(&cred("unknown")?).is_none());
        // Ids and credentials are unique.
        assert!(
            agents
                .enroll_uia(cred("c2")?, "agent:uia", None, &ToolGrant::none())
                .is_err()
        );
        assert!(
            agents
                .enroll_uia(cred("c1")?, "agent:other", None, &ToolGrant::none())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn delegates_inherit_tenant_and_stay_within_the_parent() -> TestResult {
        let agents = registry();
        let tenant = TenantId::from_str("t");
        agents.enroll_uia(
            cred("u")?,
            "agent:uia",
            Some(tenant.clone()),
            &ToolGrant::from_names(["fs.read", "fs.write"]),
        )?;
        assert!(matches!(
            agents.delegate(
                "agent:uia",
                cred("w0")?,
                delegation("agent:w0", &["shell.exec"])
            ),
            Err(HostError::Denied(_))
        ));
        let worker = agents.delegate(
            "agent:uia",
            cred("w1")?,
            Delegation {
                sandbox: Some(PermissionRequest::from_permissions([
                    Permission::ReadWorkspace,
                ])),
                ..delegation("agent:w1", &["fs.read"])
            },
        )?;
        assert_eq!(worker.parent.as_deref(), Some("agent:uia"));
        let resolved = agents.resolve(&cred("w1")?).ok_or(HostError::NotFound)?;
        assert_eq!(resolved.tenant, Some(tenant));
        assert_eq!(agents.sandbox_chain("agent:w1")?.len(), 1);
        assert!(agents.sandbox_chain("agent:uia")?.is_empty());
        // A delegate cannot be a UIA, and unknown parents are not found.
        let mut uia_child = delegation("agent:x", &[]);
        uia_child.role = AgentRole::UserInterface;
        assert!(agents.delegate("agent:uia", cred("x")?, uia_child).is_err());
        assert_eq!(
            agents.delegate("agent:nope", cred("y")?, delegation("agent:y", &[])),
            Err(HostError::NotFound)
        );
        Ok(())
    }

    #[test]
    fn narrowing_cascades_to_every_descendant() -> TestResult {
        let agents = registry();
        agents.enroll_uia(
            cred("u")?,
            "agent:uia",
            None,
            &ToolGrant::from_names(["fs.read", "fs.write"]),
        )?;
        agents.delegate(
            "agent:uia",
            cred("c")?,
            delegation("agent:c", &["fs.read", "fs.write"]),
        )?;
        agents.delegate("agent:c", cred("g")?, delegation("agent:g", &["fs.write"]))?;
        agents.narrow("agent:uia", &ToolGrant::from_names(["fs.write"]))?;
        assert_eq!(
            agents.state("agent:c")?.principal.tools.to_names(),
            vec!["fs.read".to_owned()]
        );
        assert!(agents.state("agent:g")?.principal.tools.is_empty());
        // Re-granting the parent never widens a descendant.
        agents.grant("agent:uia", &ToolGrant::from_names(["fs.read", "fs.write"]))?;
        assert_eq!(
            agents.state("agent:c")?.principal.tools.to_names(),
            vec!["fs.read".to_owned()]
        );
        // A child grant stays within its parent.
        assert!(matches!(
            agents.grant("agent:c", &ToolGrant::from_names(["shell.exec"])),
            Err(HostError::Denied(_))
        ));
        Ok(())
    }

    #[test]
    fn revocation_cascades_and_is_final() -> TestResult {
        let agents = registry();
        agents.enroll_uia(
            cred("u")?,
            "agent:uia",
            None,
            &ToolGrant::from_names(["fs.read"]),
        )?;
        agents.delegate("agent:uia", cred("c")?, delegation("agent:c", &["fs.read"]))?;
        let revoked = agents.revoke("agent:uia")?;
        assert_eq!(revoked.len(), 2);
        assert!(agents.is_revoked("agent:c"));
        assert!(agents.resolve(&cred("c")?).is_none());
        assert_eq!(
            agents.grant("agent:uia", &ToolGrant::none()),
            Err(HostError::Revoked)
        );
        assert!(agents.revoke("agent:uia")?.is_empty());
        assert!(agents.rights(None).is_empty());
        Ok(())
    }

    #[test]
    fn rights_are_filtered_by_tenant() -> TestResult {
        let agents = registry();
        let a = TenantId::from_str("a");
        let b = TenantId::from_str("b");
        agents.enroll_uia(cred("a")?, "agent:a", Some(a.clone()), &ToolGrant::none())?;
        agents.enroll_uia(cred("b")?, "agent:b", Some(b), &ToolGrant::none())?;
        assert_eq!(agents.rights(None).len(), 2);
        let visible = agents.rights(Some(&a));
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible.first().map(|rights| rights.agent.as_str()),
            Some("agent:a")
        );
        Ok(())
    }
}

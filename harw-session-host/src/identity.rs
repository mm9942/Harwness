//! Client identity and admission (W00 §2.4 "Client identity", PL-65 §2.4,
//! §4.1).
//!
//! A [`ClientIdentity`] is built by a listener from transport facts
//! (`SO_PEERCRED` on the local socket, `AuthenticatedPeer` on the node
//! transport). No request payload can replace any of its fields.
//!
//! R18 (contract `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`
//! §3) adds the agent principal: a connection is either a human/device
//! caller (`agent: None`) or an [`AgentPrincipal`] with role, parent and
//! [`ToolGrant`]. Only an agent principal can be admitted to `tool.*`
//! ([`admit_tool_call`]); a human identity carrying the `tool_call` cap is a
//! listener bug and fails [`ClientIdentity::validate`].

use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_protocol::session_wire::{AgentRole, PrincipalSummary};
use harw_protocol::{ClientCaps, ToolRefusal};
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

/// Gateway ceiling for the R18 gateway caps of a human tier (R18 §6):
/// maintainers read, owners read and administrate. Never contains
/// `tool_call`; agents get that only through [`AgentPrincipal`].
#[must_use]
pub const fn gateway_caps_for_tier(tier: PermissionTier) -> ClientCaps {
    match tier {
        PermissionTier::Observer | PermissionTier::Operator => ClientCaps::NONE,
        PermissionTier::Maintainer => ClientCaps::GATEWAY_READ,
        PermissionTier::Owner => ClientCaps::GATEWAY_ADMIN,
    }
}

/// The set of tool names an agent principal may call (R18 §3, §4).
///
/// Exact names only, no patterns: an empty grant admits nothing. Grants
/// only ever narrow along the delegation chain ([`ToolGrant::narrow_to`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolGrant {
    tools: BTreeSet<String>,
}

impl ToolGrant {
    /// The empty grant.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// A grant of exactly `names` (trimmed; blank names are dropped).
    #[must_use]
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            tools: names
                .into_iter()
                .map(|name| name.as_ref().trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect(),
        }
    }

    /// True when `tool` is granted.
    #[must_use]
    pub fn contains(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    /// Granted names in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(String::as_str)
    }

    /// Number of granted tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when nothing is granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// True when every granted tool is also in `ceiling`.
    #[must_use]
    pub fn is_within(&self, ceiling: &Self) -> bool {
        self.tools.is_subset(&ceiling.tools)
    }

    /// The grant for a delegate: exactly `requested`, which must lie within
    /// `self`. A name outside `self` is refused, never silently dropped.
    pub fn narrow_to(&self, requested: &Self) -> Result<Self, ToolGrantError> {
        match requested.tools.difference(&self.tools).next() {
            Some(tool) => Err(ToolGrantError::OutsideCeiling { tool: tool.clone() }),
            None => Ok(requested.clone()),
        }
    }

    /// `self` without `removed` (`gateway.tools.narrow`).
    #[must_use]
    pub fn without(&self, removed: &Self) -> Self {
        Self {
            tools: self.tools.difference(&removed.tools).cloned().collect(),
        }
    }

    /// Tools granted in both `self` and `other` (cascading a narrowed
    /// parent grant onto a descendant).
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            tools: self.tools.intersection(&other.tools).cloned().collect(),
        }
    }

    /// Granted names as an owned, sorted list (wire form).
    #[must_use]
    pub fn to_names(&self) -> Vec<String> {
        self.tools.iter().cloned().collect()
    }
}

/// Why a tool grant could not be formed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolGrantError {
    /// A requested tool is outside the ceiling (parent grant or configured
    /// UIA ceiling).
    OutsideCeiling { tool: String },
}

impl fmt::Display for ToolGrantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideCeiling { tool } => {
                write!(f, "tool `{tool}` is outside the delegating grant")
            }
        }
    }
}

impl std::error::Error for ToolGrantError {}

impl From<ToolGrantError> for HostError {
    fn from(error: ToolGrantError) -> Self {
        Self::Denied(error.to_string())
    }
}

/// An agent process connected to the gateway (R18 §3).
///
/// Built by a listener from a gateway-issued agent credential, never from a
/// payload. The tenant is [`ClientIdentity::tenant`]; a delegate always
/// carries its parent's tenant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPrincipal {
    /// Gateway-issued, stable agent principal id.
    pub id: String,
    /// Organizational role. Only [`AgentRole::UserInterface`] holds tool
    /// rights of its own; every other role reaches tools only by UIA
    /// delegation.
    pub role: AgentRole,
    /// Agent principal id of the delegating parent. `None` exactly for the
    /// UIA.
    pub parent: Option<String>,
    /// Tools this principal may call.
    pub tools: ToolGrant,
}

impl AgentPrincipal {
    /// True for the User Interface Agent.
    #[must_use]
    pub fn is_uia(&self) -> bool {
        self.role == AgentRole::UserInterface
    }

    /// Structural invariants: known role, the UIA has no parent, every
    /// other role has one (tools only via UIA delegation).
    pub fn validate(&self) -> Result<(), HostError> {
        if self.id.trim().is_empty() {
            return Err(HostError::Denied("agent principal without id".into()));
        }
        match (self.role, self.parent.as_deref()) {
            (AgentRole::Unknown, _) => Err(HostError::Denied(
                "agent principal with unknown role".into(),
            )),
            (AgentRole::UserInterface, None) => Ok(()),
            (AgentRole::UserInterface, Some(_)) => Err(HostError::Denied(
                "user interface agent must not have a parent principal".into(),
            )),
            (_, None) => Err(HostError::Denied(
                "delegated agent principal without parent".into(),
            )),
            (_, Some(parent)) if parent == self.id => Err(HostError::Denied(
                "agent principal is its own parent".into(),
            )),
            (_, Some(_)) => Ok(()),
        }
    }

    /// Delegate a narrower principal to a child (R18 §4): the child's grant
    /// must lie within this grant, and a known child role is required.
    pub fn delegate(
        &self,
        child_id: impl Into<String>,
        child_role: AgentRole,
        requested: &ToolGrant,
    ) -> Result<Self, HostError> {
        let child = Self {
            id: child_id.into(),
            role: child_role,
            parent: Some(self.id.clone()),
            tools: self.tools.narrow_to(requested)?,
        };
        child.validate()?;
        Ok(child)
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
    /// R18: the agent principal behind this connection; `None` for a human
    /// or device caller.
    pub agent: Option<AgentPrincipal>,
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
        match &self.agent {
            Some(agent) => agent.validate()?,
            None if self.caps.tool_call => {
                return Err(HostError::Denied(
                    "tool_call cap on a caller without agent principal".into(),
                ));
            }
            None => {}
        }
        Ok(())
    }

    /// How `gateway.connections.list` shows this caller.
    #[must_use]
    pub fn principal_summary(&self) -> PrincipalSummary {
        match &self.agent {
            Some(agent) => PrincipalSummary::Agent {
                agent: agent.id.clone(),
                role: agent.role,
                parent: agent.parent.clone(),
            },
            None => PrincipalSummary::Device {
                device: self.device.clone(),
            },
        }
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
    /// R18: `tool.*`.
    ToolCall,
    /// R18: read-only `gateway.*`.
    GatewayRead,
    /// R18: mutating `gateway.*`.
    GatewayAdmin,
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
            Self::ToolCall => caps.tool_call,
            Self::GatewayRead => caps.gateway_read,
            Self::GatewayAdmin => caps.gateway_admin,
        }
    }

    /// `Ok` when `caps` contains this capability, else `Denied` naming it.
    /// For calls that are not bound to a session (`gateway.*`).
    pub fn require(self, caps: ClientCaps) -> Result<(), HostError> {
        if self.is_granted(caps) {
            Ok(())
        } else {
            Err(HostError::Denied(format!(
                "capability `{}` not granted",
                self.name()
            )))
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Steer => "steer",
            Self::Approve => "approve",
            Self::Control => "control",
            Self::ToolCall => "tool_call",
            Self::GatewayRead => "gateway_read",
            Self::GatewayAdmin => "gateway_admin",
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
    need.require(granted)
}

/// Admit a `tool.call`/`tool.list` against a session (R18 §4), in this
/// order, fail closed at the first failing step:
///
/// 1. tenant (answered as `NotFound`, like [`admit`]);
/// 2. `tool_call` cap;
/// 3. agent principal present and valid (a human caller never runs tools
///    through the gateway);
/// 4. `tool` is served by the gateway (`known`), else
///    [`ToolRefusal::UnknownTool`];
/// 5. `tool` is in the principal's grant, else [`ToolRefusal::NotGranted`].
///
/// `tool = None` admits `tool.list` (steps 1-3 only). Host state checks
/// (draining, sandbox availability, duplicate call id) follow in the tool
/// host; none of them ever falls back to host execution.
pub fn admit_tool_call<'a>(
    identity: &'a ClientIdentity,
    granted: ClientCaps,
    record_tenant: Option<&TenantId>,
    tool: Option<(&str, bool)>,
) -> Result<&'a AgentPrincipal, HostError> {
    admit(identity, granted, record_tenant, Need::ToolCall)?;
    let agent = identity
        .agent
        .as_ref()
        .ok_or_else(|| HostError::Denied("tool calls require an agent principal".into()))?;
    agent.validate()?;
    if let Some((name, known)) = tool {
        if !known {
            return Err(HostError::ToolRefused {
                refusal: ToolRefusal::UnknownTool,
                detail: name.to_owned(),
            });
        }
        if !agent.tools.contains(name) {
            return Err(HostError::ToolRefused {
                refusal: ToolRefusal::NotGranted,
                detail: name.to_owned(),
            });
        }
    }
    Ok(agent)
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
            agent: None,
        }
    }

    /// A tenant-scoped local identity for tests.
    pub(crate) fn scoped(uid: u32, tier: PermissionTier, tenant: &TenantId) -> ClientIdentity {
        ClientIdentity {
            tenant: Some(tenant.clone()),
            ..local(uid, tier)
        }
    }

    /// A local UIA agent identity with `tools` granted, for tests.
    pub(crate) fn uia(uid: u32, tools: &[&str]) -> ClientIdentity {
        let base = local(uid, PermissionTier::Operator);
        ClientIdentity {
            caps: base.caps.with(harw_protocol::ClientCaps::TOOL_CALL),
            agent: Some(super::AgentPrincipal {
                id: format!("agent:uia-{uid}"),
                role: harw_protocol::AgentRole::UserInterface,
                parent: None,
                tools: super::ToolGrant::from_names(tools.iter().copied()),
            }),
            ..base
        }
    }
}

#[cfg(test)]
mod tests {
    use harw_types::{PermissionTier, TenantId, TrustZone};

    use super::test_identity::{self, local, scoped};
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

    fn tool_error(result: Result<&AgentPrincipal, HostError>) -> Option<ToolRefusal> {
        match result {
            Err(HostError::ToolRefused { refusal, .. }) => Some(refusal),
            _ => None,
        }
    }

    #[test]
    fn human_caller_is_never_admitted_to_tools() {
        let human = local(1, PermissionTier::Owner);
        // Without the cap: denied by capability.
        assert!(matches!(
            admit_tool_call(&human, ClientCaps::ALL, None, None),
            Err(HostError::Denied(_))
        ));
        // Even a forged cap does not help: no agent principal.
        let forged = ClientCaps::ALL.with(ClientCaps::TOOL_CALL);
        assert!(matches!(
            admit_tool_call(&human, forged, None, Some(("fs.read", true))),
            Err(HostError::Denied(_))
        ));
        let mut listener_bug = local(1, PermissionTier::Owner);
        listener_bug.caps = forged;
        assert!(listener_bug.validate().is_err());
    }

    #[test]
    fn uia_admission_checks_unknown_then_grant() {
        let identity = test_identity::uia(7, &["fs.read", "gateway.status"]);
        assert_eq!(identity.validate(), Ok(()));
        let caps = identity.caps;
        assert!(admit_tool_call(&identity, caps, None, None).is_ok());
        assert!(admit_tool_call(&identity, caps, None, Some(("fs.read", true))).is_ok());
        assert_eq!(
            tool_error(admit_tool_call(
                &identity,
                caps,
                None,
                Some(("fs.nope", false))
            )),
            Some(ToolRefusal::UnknownTool)
        );
        assert_eq!(
            tool_error(admit_tool_call(
                &identity,
                caps,
                None,
                Some(("shell.exec", true))
            )),
            Some(ToolRefusal::NotGranted)
        );
        // Granted caps without `tool_call` (requested narrower at hello).
        assert!(matches!(
            admit_tool_call(
                &identity,
                ClientCaps::OPERATE,
                None,
                Some(("fs.read", true))
            ),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn tool_admission_checks_tenant_first() -> Result<(), harw_types::InvalidId> {
        let a = TenantId::try_from_str("a")?;
        let b = TenantId::try_from_str("b")?;
        let mut identity = test_identity::uia(8, &["fs.read"]);
        identity.tenant = Some(a);
        let caps = identity.caps;
        assert_eq!(
            admit_tool_call(&identity, caps, Some(&b), Some(("fs.read", true))).map(|_| ()),
            Err(HostError::NotFound)
        );
        Ok(())
    }

    #[test]
    fn agent_principal_structure_is_validated() {
        let uia = AgentPrincipal {
            id: "agent:uia".into(),
            role: AgentRole::UserInterface,
            parent: None,
            tools: ToolGrant::from_names(["fs.read"]),
        };
        assert_eq!(uia.validate(), Ok(()));
        let with_parent = AgentPrincipal {
            parent: Some("agent:x".into()),
            ..uia.clone()
        };
        assert!(with_parent.validate().is_err());
        let orphan = AgentPrincipal {
            role: AgentRole::Worker,
            ..uia.clone()
        };
        assert!(orphan.validate().is_err());
        let unknown = AgentPrincipal {
            role: AgentRole::Unknown,
            ..uia.clone()
        };
        assert!(unknown.validate().is_err());
        let own_parent = AgentPrincipal {
            role: AgentRole::Worker,
            parent: Some("agent:uia".into()),
            ..uia
        };
        assert!(own_parent.validate().is_err());
    }

    #[test]
    fn delegation_only_narrows() -> Result<(), HostError> {
        let uia = AgentPrincipal {
            id: "agent:uia".into(),
            role: AgentRole::UserInterface,
            parent: None,
            tools: ToolGrant::from_names(["fs.read", "fs.write", "shell.exec"]),
        };
        let worker = uia.delegate(
            "agent:w1",
            AgentRole::Worker,
            &ToolGrant::from_names(["fs.read"]),
        )?;
        assert_eq!(worker.parent.as_deref(), Some("agent:uia"));
        assert!(worker.tools.is_within(&uia.tools));
        assert!(worker.tools.contains("fs.read"));
        assert!(!worker.tools.contains("shell.exec"));
        // A grandchild cannot regain what the child lost.
        assert!(matches!(
            worker.delegate(
                "agent:w2",
                AgentRole::Worker,
                &ToolGrant::from_names(["shell.exec"])
            ),
            Err(HostError::Denied(_))
        ));
        // Unknown child roles are refused.
        assert!(
            uia.delegate("agent:u", AgentRole::Unknown, &ToolGrant::none())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn tool_grant_set_operations() {
        let grant = ToolGrant::from_names([" fs.read ", "", "fs.write", "fs.read"]);
        assert_eq!(grant.len(), 2);
        assert_eq!(
            grant.names().collect::<Vec<_>>(),
            vec!["fs.read", "fs.write"]
        );
        let narrowed = grant.without(&ToolGrant::from_names(["fs.write", "shell.exec"]));
        assert_eq!(narrowed.names().collect::<Vec<_>>(), vec!["fs.read"]);
        let common = grant.intersect(&ToolGrant::from_names(["fs.write", "shell.exec"]));
        assert_eq!(common.to_names(), vec!["fs.write".to_owned()]);
        assert_eq!(
            grant.narrow_to(&ToolGrant::from_names(["web.fetch"])),
            Err(ToolGrantError::OutsideCeiling {
                tool: "web.fetch".into()
            })
        );
        assert!(ToolGrant::none().is_empty());
        assert!(ToolGrant::none().is_within(&grant));
    }

    #[test]
    fn principal_summary_distinguishes_agents_from_devices() {
        let human = local(1, PermissionTier::Operator);
        assert!(matches!(
            human.principal_summary(),
            PrincipalSummary::Device { device: None }
        ));
        let agent = test_identity::uia(2, &[]);
        assert!(matches!(
            agent.principal_summary(),
            PrincipalSummary::Agent {
                role: AgentRole::UserInterface,
                parent: None,
                ..
            }
        ));
    }

    #[test]
    fn gateway_tier_ceilings_never_grant_tool_call() {
        for tier in [
            PermissionTier::Observer,
            PermissionTier::Operator,
            PermissionTier::Maintainer,
            PermissionTier::Owner,
        ] {
            let ceiling = caps_for_tier(tier).with(gateway_caps_for_tier(tier));
            assert!(!ceiling.tool_call);
        }
        assert!(gateway_caps_for_tier(PermissionTier::Maintainer).gateway_read);
        assert!(!gateway_caps_for_tier(PermissionTier::Maintainer).gateway_admin);
        assert!(gateway_caps_for_tier(PermissionTier::Owner).gateway_admin);
        assert_eq!(
            gateway_caps_for_tier(PermissionTier::Operator),
            ClientCaps::NONE
        );
    }

    #[test]
    fn tier_ceilings_follow_the_table() {
        assert_eq!(caps_for_tier(PermissionTier::Observer), ClientCaps::OBSERVE);
        assert_eq!(caps_for_tier(PermissionTier::Operator), ClientCaps::OPERATE);
        assert_eq!(caps_for_tier(PermissionTier::Maintainer), ClientCaps::ALL);
    }
}

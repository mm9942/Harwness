//! Policy engine: kernel-attested peer identity → trusted [`SecurityContext`].
//!
//! # Model (masterplan v2 §13, §15, §25, §32)
//! The SecurityHub answers "WHO + WHERE → SHOULD" for local callers. *Who* is
//! never taken from the request: it is the `SO_PEERCRED` uid of the connected
//! socket peer, looked up in a server-side [`SecurityPolicy`]. The policy
//! fixes, per uid, the principal (kind/id/surface/tier), the tenant, the
//! workspace scope, the trust-zone ceiling, the authentication strength and a
//! TTL cap.
//!
//! A caller may send a [`ContextRequest`] that asks for a **narrower**
//! context — a specific workspace when its rule is unscoped, a farther (less
//! trusted) trust zone, a shorter TTL. It can never ask for anything broader:
//!
//! | Request field | Allowed | Denied |
//! |---|---|---|
//! | `workspace` | rule unscoped → any workspace; rule scoped to `W` → `W` | rule scoped to `W` → any other workspace |
//! | `trust_zone` | equal to or farther than the rule's zone | closer (more trusted) than the rule's zone |
//! | `ttl_secs` | `1..` — silently capped at the rule/policy cap | `0` |
//!
//! Principal, tenant, service identity, node and auth strength are not
//! request fields at all; `deny_unknown_fields` turns an attempt to send them
//! into a `400`.
//!
//! # Auth strength
//! The hub only ever verifies a local peer credential. A rule may therefore
//! declare at most [`AuthStrength::PeerCredential`]; validation rejects
//! anything stronger, because the hub would be asserting a verification it
//! never performed.
//!
//! # The issuer
//! [`PolicyEngine`] owns the process's only [`SecurityContextIssuer`]. It is
//! created once at startup (see [`crate::server`]) and moved in here; the
//! engine is not `Clone`, so the capability cannot be duplicated.
//!
//! # `PermissionTier` is not cryptographic authorization (§32)
//! The tier in the rule is copied into the principal for downstream
//! operation-class checks. The engine itself never widens anything based on
//! the tier.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use harw_types::{
    AuthStrength, IngressSurface, NodeId, PermissionTier, Principal, PrincipalKind, SecurityClaims,
    SecurityContext, SecurityContextError, SecurityContextIssuer, ServiceIdentityId, TenantId,
    TrustZone, WorkspaceId,
};

/// Default TTL of an issued context when the request names none.
pub const DEFAULT_TTL_SECS: u64 = 300;
/// Default policy-wide TTL cap.
pub const DEFAULT_MAX_TTL_SECS: u64 = 900;

fn default_ttl_secs() -> u64 {
    DEFAULT_TTL_SECS
}

fn default_max_ttl_secs() -> u64 {
    DEFAULT_MAX_TTL_SECS
}

fn default_trust_zone() -> TrustZone {
    TrustZone::Local
}

fn default_auth_strength() -> AuthStrength {
    AuthStrength::PeerCredential
}

/// Server-side policy: which local uid is which principal, and what it may
/// receive.
///
/// Parsed from TOML with `deny_unknown_fields` at every level.
///
/// ```rust
/// use harw_security_hub::policy::SecurityPolicy;
///
/// let policy = SecurityPolicy::from_toml_str(r#"
///     default_ttl_secs = 300
///     max_ttl_secs = 900
///     verifier_uids = [990]
///
///     [[peers]]
///     uid = 1000
///     tenant = "acme"
///     trust_zone = "local"
///     principal = { kind = "human", id = "alice", surface = "cli", tier = "owner" }
/// "#)?;
/// assert_eq!(policy.peers.len(), 1);
/// # Ok::<(), harw_security_hub::error::HubError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityPolicy {
    /// TTL used when a request names none (capped like any other TTL).
    #[serde(default = "default_ttl_secs")]
    pub default_ttl_secs: u64,
    /// Policy-wide TTL cap; must not exceed [`SecurityContext::MAX_TTL`].
    #[serde(default = "default_max_ttl_secs")]
    pub max_ttl_secs: u64,
    /// Peers (other daemons) allowed to verify and revoke *any* context via
    /// `GET`/`DELETE /v1/contexts/{id}`. A context's own requester may
    /// always verify and revoke it.
    #[serde(default)]
    pub verifier_uids: Vec<u32>,
    /// One rule per local uid.
    #[serde(default)]
    pub peers: Vec<PeerRule>,
}

/// Mapping of one local uid to a principal and its context ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRule {
    /// Effective uid as reported by `SO_PEERCRED`.
    pub uid: u32,
    /// The principal this uid authenticates as.
    pub principal: PrincipalRule,
    /// Tenant scope of every context issued to this uid.
    #[serde(default)]
    pub tenant: Option<TenantId>,
    /// Workspace scope; `None` means unscoped (a request may narrow it).
    #[serde(default)]
    pub workspace: Option<WorkspaceId>,
    /// Service identity, for daemon peers.
    #[serde(default)]
    pub service_identity: Option<ServiceIdentityId>,
    /// Trust-zone ceiling: the closest zone this peer may be placed in.
    #[serde(default = "default_trust_zone")]
    pub trust_zone: TrustZone,
    /// Authentication strength asserted; at most `peer_credential`.
    #[serde(default = "default_auth_strength")]
    pub auth_strength: AuthStrength,
    /// Per-peer TTL cap (never above the policy-wide cap).
    #[serde(default)]
    pub max_ttl_secs: Option<u64>,
}

/// The principal part of a [`PeerRule`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalRule {
    /// Principal kind.
    pub kind: PrincipalKind,
    /// Stable principal id.
    pub id: String,
    /// Ingress surface recorded on the principal.
    pub surface: IngressSurface,
    /// Operation-class tier (not cryptographic authorization, §32).
    pub tier: PermissionTier,
}

impl SecurityPolicy {
    /// Parses and validates a policy from TOML text.
    ///
    /// # Errors
    /// [`crate::error::HubError::ConfigParse`] for malformed TOML or unknown
    /// fields, [`crate::error::HubError::ConfigInvalid`] when
    /// [`Self::validate`] fails.
    pub fn from_toml_str(text: &str) -> Result<Self, crate::error::HubError> {
        let policy: Self = toml::from_str(text)
            .map_err(|error| crate::error::HubError::ConfigParse(error.to_string()))?;
        policy
            .validate()
            .map_err(crate::error::HubError::ConfigInvalid)?;
        Ok(policy)
    }

    /// Checks the semantic invariants the type system cannot express.
    ///
    /// # Errors
    /// A human-readable reason for the first violated invariant.
    pub fn validate(&self) -> Result<(), String> {
        let max_allowed = u64::try_from(SecurityContext::MAX_TTL.as_secs()).unwrap_or(0);
        if self.max_ttl_secs == 0 || self.max_ttl_secs > max_allowed {
            return Err(format!(
                "max_ttl_secs must be in 1..={max_allowed}, got {}",
                self.max_ttl_secs
            ));
        }
        if self.default_ttl_secs == 0 {
            return Err("default_ttl_secs must be positive".to_owned());
        }
        let mut seen = BTreeSet::new();
        for rule in &self.peers {
            if !seen.insert(rule.uid) {
                return Err(format!("duplicate peer rule for uid {}", rule.uid));
            }
            if rule.principal.id.trim().is_empty() {
                return Err(format!("peer uid {}: principal id is blank", rule.uid));
            }
            if rule.auth_strength > AuthStrength::PeerCredential {
                return Err(format!(
                    "peer uid {}: auth_strength above peer_credential is never verified by this hub",
                    rule.uid
                ));
            }
            match rule.max_ttl_secs {
                Some(cap) if cap == 0 || cap > self.max_ttl_secs => {
                    return Err(format!(
                        "peer uid {}: max_ttl_secs must be in 1..={}",
                        rule.uid, self.max_ttl_secs
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Kernel-attested identity of the connected socket peer (`SO_PEERCRED`).
///
/// Read once, directly after `accept()`. This is the *only* input to the
/// "who" part of a policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Effective uid.
    pub uid: u32,
    /// Effective gid.
    pub gid: u32,
    /// Process id at `connect()` time, if the platform reports it.
    pub pid: Option<u32>,
}

impl PeerIdentity {
    /// Builds a peer identity from credential values.
    #[must_use]
    pub const fn new(uid: u32, gid: u32, pid: Option<u32>) -> Self {
        Self { uid, gid, pid }
    }
}

/// What a caller may ask for: only narrowing, never broadening.
///
/// `deny_unknown_fields`: sending `tenant`, `principal`, `auth_strength` or
/// any other field is a malformed request, not an ignored hint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {
    /// Narrow to this workspace.
    #[serde(default)]
    pub workspace: Option<WorkspaceId>,
    /// Place the context in this (equal or farther) trust zone.
    #[serde(default)]
    pub trust_zone: Option<TrustZone>,
    /// Requested lifetime; capped, never an error unless `0`.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// Why the policy engine refused to issue a context.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyDenied {
    /// No rule exists for the peer's uid.
    UnknownPeer {
        /// The peer's uid.
        uid: u32,
    },
    /// The requested workspace is outside the rule's workspace scope.
    WorkspaceNotPermitted,
    /// The requested trust zone is closer (more trusted) than the ceiling.
    ZoneBroadening {
        /// Requested zone.
        requested: TrustZone,
        /// The rule's ceiling.
        ceiling: TrustZone,
    },
    /// The requested TTL is zero.
    InvalidTtl,
    /// The issuer rejected the claims.
    Issue(SecurityContextError),
}

impl PolicyDenied {
    /// Stable machine-readable code for the HTTP error body.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownPeer { .. } => "unknown_peer",
            Self::WorkspaceNotPermitted => "workspace_not_permitted",
            Self::ZoneBroadening { .. } => "zone_broadening",
            Self::InvalidTtl => "invalid_ttl",
            Self::Issue(_) => "issue_failed",
        }
    }
}

impl fmt::Display for PolicyDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPeer { uid } => write!(f, "no policy rule for peer uid {uid}"),
            Self::WorkspaceNotPermitted => f.write_str("requested workspace is not permitted"),
            Self::ZoneBroadening { requested, ceiling } => write!(
                f,
                "requested trust zone {requested:?} is broader than the ceiling {ceiling:?}"
            ),
            Self::InvalidTtl => f.write_str("requested ttl must be positive"),
            Self::Issue(error) => write!(f, "context could not be issued: {error}"),
        }
    }
}

impl std::error::Error for PolicyDenied {}

/// Evaluates requests against a [`SecurityPolicy`] and mints contexts with
/// the hub's single [`SecurityContextIssuer`].
///
/// Deliberately not `Clone` (it owns the issuer).
#[derive(Debug)]
pub struct PolicyEngine {
    issuer: SecurityContextIssuer,
    node: Option<NodeId>,
    default_ttl_secs: u64,
    max_ttl_secs: u64,
    verifiers: BTreeSet<u32>,
    rules: HashMap<u32, PeerRule>,
}

impl PolicyEngine {
    /// Builds the engine, taking ownership of the issuer.
    ///
    /// # Arguments
    /// - `policy`: the validated policy.
    /// - `issuer`: the hub's only issuer capability.
    /// - `node`: this host's node id, bound into every context, if known.
    ///
    /// # Errors
    /// The reason from [`SecurityPolicy::validate`].
    pub fn new(
        policy: SecurityPolicy,
        issuer: SecurityContextIssuer,
        node: Option<NodeId>,
    ) -> Result<Self, String> {
        policy.validate()?;
        let SecurityPolicy {
            default_ttl_secs,
            max_ttl_secs,
            verifier_uids,
            peers,
        } = policy;
        Ok(Self {
            issuer,
            node,
            default_ttl_secs,
            max_ttl_secs,
            verifiers: verifier_uids.into_iter().collect(),
            rules: peers.into_iter().map(|rule| (rule.uid, rule)).collect(),
        })
    }

    /// Name of the issuer recorded on every context.
    #[must_use]
    pub fn issuer_name(&self) -> &str {
        self.issuer.name()
    }

    /// `true` if a rule exists for `uid`.
    #[must_use]
    pub fn is_known_peer(&self, uid: u32) -> bool {
        self.rules.contains_key(&uid)
    }

    /// Principal id the policy assigns to `uid`, if any.
    #[must_use]
    pub fn principal_id_for(&self, uid: u32) -> Option<&str> {
        self.rules.get(&uid).map(|rule| rule.principal.id.as_str())
    }

    /// `true` if `uid` may verify/revoke any context.
    #[must_use]
    pub fn is_verifier(&self, uid: u32) -> bool {
        self.verifiers.contains(&uid)
    }

    /// Evaluates `request` for `peer` at the current system time.
    ///
    /// # Errors
    /// See [`Self::evaluate_at`].
    pub fn evaluate(
        &self,
        peer: PeerIdentity,
        request: ContextRequest,
    ) -> Result<SecurityContext, PolicyDenied> {
        self.evaluate_at(peer, request, Timestamp::now())
    }

    /// Evaluates `request` for `peer` and issues a context valid from `now`.
    ///
    /// # Errors
    /// A [`PolicyDenied`] naming the first failed check.
    pub fn evaluate_at(
        &self,
        peer: PeerIdentity,
        request: ContextRequest,
        now: Timestamp,
    ) -> Result<SecurityContext, PolicyDenied> {
        let rule = self
            .rules
            .get(&peer.uid)
            .ok_or(PolicyDenied::UnknownPeer { uid: peer.uid })?;

        let cap = rule
            .max_ttl_secs
            .unwrap_or(self.max_ttl_secs)
            .min(self.max_ttl_secs);
        let requested_ttl = request.ttl_secs.unwrap_or(self.default_ttl_secs);
        if requested_ttl == 0 {
            return Err(PolicyDenied::InvalidTtl);
        }
        let ttl_secs =
            i64::try_from(requested_ttl.min(cap)).map_err(|_| PolicyDenied::InvalidTtl)?;

        let workspace = match (&rule.workspace, request.workspace) {
            (None, requested) => requested,
            (Some(scoped), None) => Some(scoped.clone()),
            (Some(scoped), Some(requested)) if *scoped == requested => Some(requested),
            (Some(_), Some(_)) => return Err(PolicyDenied::WorkspaceNotPermitted),
        };

        let trust_zone = match request.trust_zone {
            None => rule.trust_zone,
            Some(requested) if requested >= rule.trust_zone => requested,
            Some(requested) => {
                return Err(PolicyDenied::ZoneBroadening {
                    requested,
                    ceiling: rule.trust_zone,
                });
            }
        };

        let principal = Principal::trusted_ingress(
            rule.principal.kind,
            rule.principal.id.clone(),
            rule.principal.surface,
            rule.principal.tier,
        );
        let mut claims = SecurityClaims::new(principal, trust_zone, rule.auth_strength);
        claims.tenant = rule.tenant.clone();
        claims.workspace = workspace;
        claims.node = self.node.clone();
        claims.service_identity = rule.service_identity.clone();

        SecurityContext::issue(
            &self.issuer,
            claims,
            now,
            SignedDuration::from_secs(ttl_secs),
        )
        .map_err(PolicyDenied::Issue)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextRequest, PeerIdentity, PolicyDenied, PolicyEngine, SecurityPolicy};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{
        AuthStrength, NodeId, PermissionTier, PrincipalKind, SecurityContextIssuer, TrustZone,
        WorkspaceId,
    };
    use jiff::{SignedDuration, Timestamp};

    const POLICY: &str = r#"
        default_ttl_secs = 300
        max_ttl_secs = 900
        verifier_uids = [990]

        [[peers]]
        uid = 1000
        tenant = "acme"
        trust_zone = "local"
        principal = { kind = "human", id = "alice", surface = "cli", tier = "owner" }

        [[peers]]
        uid = 1001
        tenant = "acme"
        workspace = "ws-a"
        trust_zone = "host"
        max_ttl_secs = 60
        service_identity = "svc-worker"
        principal = { kind = "operation", id = "worker", surface = "job_worker", tier = "operator" }
    "#;

    fn engine() -> TestResult<PolicyEngine> {
        let policy = SecurityPolicy::from_toml_str(POLICY).map_err(ctx("policy parses"))?;
        let issuer = SecurityContextIssuer::new_for_hub("security-hub").map_err(ctx("issuer"))?;
        let node = NodeId::try_from_str("node-1").map_err(ctx("node id"))?;
        PolicyEngine::new(policy, issuer, Some(node)).map_err(ctx("engine"))
    }

    fn now() -> TestResult<Timestamp> {
        Timestamp::from_second(1_700_000_000).map_err(ctx("timestamp"))
    }

    fn peer(uid: u32) -> PeerIdentity {
        PeerIdentity::new(uid, uid, Some(4242))
    }

    fn ws(name: &str) -> TestResult<WorkspaceId> {
        WorkspaceId::try_from_str(name).map_err(ctx("workspace id"))
    }

    fn expect_denied(
        result: Result<harw_types::SecurityContext, PolicyDenied>,
    ) -> TestResult<PolicyDenied> {
        match result {
            Ok(context) => Err(TestError::Unexpected(format!(
                "expected denial, got context {}",
                context.id()
            ))),
            Err(denied) => Ok(denied),
        }
    }

    #[test]
    fn test_unknown_uid_is_denied() -> TestResult {
        let engine = engine()?;
        let denied =
            expect_denied(engine.evaluate_at(peer(4711), ContextRequest::default(), now()?))?;
        assert_eq!(denied, PolicyDenied::UnknownPeer { uid: 4711 });
        assert_eq!(denied.code(), "unknown_peer");
        Ok(())
    }

    #[test]
    fn test_known_uid_gets_rule_claims_and_default_ttl() -> TestResult {
        let engine = engine()?;
        let at = now()?;
        let context = engine
            .evaluate_at(peer(1000), ContextRequest::default(), at)
            .map_err(ctx("alice gets a context"))?;
        assert_eq!(context.principal_id(), "alice");
        assert_eq!(context.principal_kind(), PrincipalKind::Human);
        assert_eq!(context.principal().tier(), PermissionTier::Owner);
        assert_eq!(context.tenant().map(|t| t.as_str()), Some("acme"));
        assert_eq!(context.workspace(), None);
        assert_eq!(context.node().map(|n| n.as_str()), Some("node-1"));
        assert_eq!(context.trust_zone(), TrustZone::Local);
        assert_eq!(context.auth_strength(), AuthStrength::PeerCredential);
        assert_eq!(context.issuer(), "security-hub");
        assert_eq!(context.expires_at(), at + SignedDuration::from_secs(300));
        Ok(())
    }

    #[test]
    fn test_unscoped_rule_allows_narrowing_to_workspace() -> TestResult {
        let engine = engine()?;
        let request = ContextRequest {
            workspace: Some(ws("ws-z")?),
            ..ContextRequest::default()
        };
        let context = engine
            .evaluate_at(peer(1000), request, now()?)
            .map_err(ctx("narrowing allowed"))?;
        assert_eq!(context.workspace().map(|w| w.as_str()), Some("ws-z"));
        Ok(())
    }

    #[test]
    fn test_scoped_rule_keeps_own_workspace_and_denies_other() -> TestResult {
        let engine = engine()?;
        let same = engine
            .evaluate_at(
                peer(1001),
                ContextRequest {
                    workspace: Some(ws("ws-a")?),
                    ..ContextRequest::default()
                },
                now()?,
            )
            .map_err(ctx("same workspace allowed"))?;
        assert_eq!(same.workspace().map(|w| w.as_str()), Some("ws-a"));

        let implicit = engine
            .evaluate_at(peer(1001), ContextRequest::default(), now()?)
            .map_err(ctx("implicit workspace"))?;
        assert_eq!(implicit.workspace().map(|w| w.as_str()), Some("ws-a"));
        assert_eq!(
            implicit.service_identity().map(|s| s.as_str()),
            Some("svc-worker")
        );

        let denied = expect_denied(engine.evaluate_at(
            peer(1001),
            ContextRequest {
                workspace: Some(ws("ws-b")?),
                ..ContextRequest::default()
            },
            now()?,
        ))?;
        assert_eq!(denied, PolicyDenied::WorkspaceNotPermitted);
        Ok(())
    }

    #[test]
    fn test_zone_narrowing_allowed_and_broadening_denied() -> TestResult {
        let engine = engine()?;
        // Farther zone than the ceiling = less trust = narrowing.
        let farther = engine
            .evaluate_at(
                peer(1001),
                ContextRequest {
                    trust_zone: Some(TrustZone::Remote),
                    ..ContextRequest::default()
                },
                now()?,
            )
            .map_err(ctx("farther zone allowed"))?;
        assert_eq!(farther.trust_zone(), TrustZone::Remote);

        let denied = expect_denied(engine.evaluate_at(
            peer(1001),
            ContextRequest {
                trust_zone: Some(TrustZone::Local),
                ..ContextRequest::default()
            },
            now()?,
        ))?;
        assert_eq!(
            denied,
            PolicyDenied::ZoneBroadening {
                requested: TrustZone::Local,
                ceiling: TrustZone::Host,
            }
        );
        Ok(())
    }

    #[test]
    fn test_ttl_is_capped_by_policy_and_rule() -> TestResult {
        let engine = engine()?;
        let at = now()?;
        let policy_capped = engine
            .evaluate_at(
                peer(1000),
                ContextRequest {
                    ttl_secs: Some(86_400),
                    ..ContextRequest::default()
                },
                at,
            )
            .map_err(ctx("policy cap"))?;
        assert_eq!(
            policy_capped.expires_at(),
            at + SignedDuration::from_secs(900)
        );

        let rule_capped = engine
            .evaluate_at(
                peer(1001),
                ContextRequest {
                    ttl_secs: Some(600),
                    ..ContextRequest::default()
                },
                at,
            )
            .map_err(ctx("rule cap"))?;
        assert_eq!(rule_capped.expires_at(), at + SignedDuration::from_secs(60));

        let shorter = engine
            .evaluate_at(
                peer(1000),
                ContextRequest {
                    ttl_secs: Some(10),
                    ..ContextRequest::default()
                },
                at,
            )
            .map_err(ctx("shorter ttl"))?;
        assert_eq!(shorter.expires_at(), at + SignedDuration::from_secs(10));

        let denied = expect_denied(engine.evaluate_at(
            peer(1000),
            ContextRequest {
                ttl_secs: Some(0),
                ..ContextRequest::default()
            },
            at,
        ))?;
        assert_eq!(denied, PolicyDenied::InvalidTtl);
        Ok(())
    }

    #[test]
    fn test_request_rejects_broadening_fields() {
        // Tenant/principal/auth strength are not request fields at all.
        for body in [
            r#"{"tenant":"other"}"#,
            r#"{"auth_strength":"hardware_backed"}"#,
            r#"{"principal":"root"}"#,
        ] {
            assert!(
                serde_json::from_str::<ContextRequest>(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn test_policy_rejects_unknown_fields_and_bad_invariants() {
        let cases = [
            // unknown top-level field
            "unknown = 1",
            // unknown rule field
            r#"[[peers]]
               uid = 1
               bogus = true
               principal = { kind = "human", id = "a", surface = "cli", tier = "owner" }"#,
            // cap above MAX_TTL
            "max_ttl_secs = 100000",
            // duplicate uid
            r#"[[peers]]
               uid = 1
               principal = { kind = "human", id = "a", surface = "cli", tier = "owner" }
               [[peers]]
               uid = 1
               principal = { kind = "human", id = "b", surface = "cli", tier = "owner" }"#,
            // auth strength the hub never verifies
            r#"[[peers]]
               uid = 1
               auth_strength = "mutual_tls"
               principal = { kind = "human", id = "a", surface = "cli", tier = "owner" }"#,
            // blank principal id
            r#"[[peers]]
               uid = 1
               principal = { kind = "human", id = " ", surface = "cli", tier = "owner" }"#,
            // per-rule cap above policy cap
            r#"max_ttl_secs = 60
               [[peers]]
               uid = 1
               max_ttl_secs = 61
               principal = { kind = "human", id = "a", surface = "cli", tier = "owner" }"#,
        ];
        for case in cases {
            assert!(SecurityPolicy::from_toml_str(case).is_err(), "{case}");
        }
    }

    #[test]
    fn test_verifier_and_known_peer_lookup() -> TestResult {
        let engine = engine()?;
        assert!(engine.is_verifier(990));
        assert!(!engine.is_verifier(1000));
        assert!(engine.is_known_peer(1000));
        assert!(!engine.is_known_peer(990));
        assert_eq!(engine.issuer_name(), "security-hub");
        Ok(())
    }
}

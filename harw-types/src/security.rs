//! Infrastructure identity vocabulary: [`TrustZone`], [`AuthStrength`] and the
//! trusted, short-lived [`SecurityContext`] (crypto masterplan v2 §13, §14,
//! §25, §32, wave H1).
//!
//! # Trusted context vs. wire summary
//!
//! A [`SecurityContext`] is the *authoritative* statement "the Auth/Crypto Hub
//! verified this principal, from this trust zone, with this authentication
//! strength, for this tenant/workspace/node/device, until `expires_at`". It
//! therefore follows the same rules as [`Principal`]:
//!
//! - all fields are private; there is no struct-literal construction,
//! - there is **no** `Deserialize` — a context can never be rebuilt from wire,
//!   configuration, HTTP headers or model output,
//! - the only constructor is [`SecurityContext::issue`], which requires a
//!   [`SecurityContextIssuer`].
//!
//! [`SecurityContextSummary`] is the opposite: a plain, `Deserialize`-able
//! data record for display, logging and UI wire payloads. It is
//! **non-authoritative**: nothing may make an authorization decision from a
//! summary, and there is deliberately no conversion from a summary back into a
//! [`SecurityContext`]. Across process or network boundaries the masterplan
//! (§25) requires an authenticated representation or a short-lived context
//! reference ([`SecurityContextId`]) that the receiver resolves against the
//! hub — never a summary taken at face value.
//!
//! # Who may issue
//!
//! Rust cannot restrict a public constructor to particular downstream crates
//! without sealing across crate boundaries, so [`SecurityContextIssuer`] has an
//! explicit, greppable constructor, [`SecurityContextIssuer::new_for_hub`].
//! By contract only `harw-security-hub` (the Auth/Crypto Hub) and the local
//! peer-identity resolver in the `harw-web` authentication path (§14) hold an
//! issuer. Any other call site of `new_for_hub` is an architecture violation
//! and must be rejected in review. The issuer is neither `Clone` nor
//! `Serialize`/`Deserialize`, and an issued context records only the issuer's
//! *name*, never the issuer itself, so holding a context never confers the
//! ability to mint new ones.
//!
//! # `PermissionTier` is not cryptographic authorization (§32)
//!
//! The [`Principal`]'s [`crate::PermissionTier`] answers "may this caller
//! invoke this *class* of operation?". A `SecurityContext` adds the
//! dimensions the hub needs to answer "may this caller act on *this* key in
//! *this* tenant from *this* zone?". Both checks are required; an `Owner`
//! tier implies nothing about trust zone or authentication strength.
//!
//! # Key versions (decision D4)
//!
//! There is intentionally **no** `KeyGeneration` type here. Harwness keeps its
//! existing `KeyVersion` (`u32`, starting at `0`); the CryptGuard service's
//! `KeyVersion` is a `NonZeroU32` starting at `1`. The mapping is fixed:
//!
//! ```text
//! CryptGuard KeyVersion (NonZeroU32) = Harwness KeyVersion (u32, starts 0) + 1
//! ```
//!
//! Conversions belong to the adapter crate that talks to CryptGuard, not to
//! this vocabulary crate.

use std::fmt;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, NodeId, SecurityContextId, ServiceIdentityId, TenantId, WorkspaceId};
use crate::principal::{PermissionTier, Principal, PrincipalKind};

/// Network/ingress trust zone a request originated from.
///
/// # Ordering
/// Variants are ordered from the closest, most trusted zone (`Local`) to the
/// farthest (`Remote`). A policy that allows "at most `Host`" accepts `Local`
/// and `Host` — see [`TrustZone::is_within`].
///
/// # Serialization
/// `snake_case`: `"local"`, `"host"`, `"cluster"`, `"remote"`.
///
/// ```rust
/// use harw_types::TrustZone;
///
/// assert!(TrustZone::Local < TrustZone::Remote);
/// assert!(TrustZone::Host.is_within(TrustZone::Cluster));
/// assert!(!TrustZone::Remote.is_within(TrustZone::Host));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustZone {
    /// Same process or a local Unix socket on the same user session.
    Local,
    /// Another process or user on the same host.
    Host,
    /// Another node of the same fleet/cluster.
    Cluster,
    /// Anything beyond the cluster (remote browser, external gateway).
    Remote,
}

impl TrustZone {
    /// `true` if this zone is at most as far out as `max_zone`.
    #[must_use]
    pub fn is_within(self, max_zone: TrustZone) -> bool {
        self <= max_zone
    }
}

/// Strength of the authentication that established a [`SecurityContext`].
///
/// # Ordering
/// Totally ordered from weakest (`None`) to strongest (`HardwareBacked`);
/// see [`AuthStrength::at_least`].
///
/// # Serialization
/// `snake_case`: `"none"`, `"peer_credential"`, `"token"`, `"mutual_tls"`,
/// `"hardware_backed"`.
///
/// ```rust
/// use harw_types::AuthStrength;
///
/// assert!(AuthStrength::MutualTls.at_least(AuthStrength::Token));
/// assert!(!AuthStrength::PeerCredential.at_least(AuthStrength::Token));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStrength {
    /// Unauthenticated.
    None,
    /// Kernel-attested local peer credential (`SO_PEERCRED`).
    PeerCredential,
    /// Bearer/session token verified by the hub.
    Token,
    /// Mutually authenticated TLS (or an equivalent key-bound channel).
    MutualTls,
    /// Key material held in hardware (TPM, secure enclave, security key).
    HardwareBacked,
}

impl AuthStrength {
    /// `true` if this strength is equal to or stronger than `required`.
    #[must_use]
    pub fn at_least(self, required: AuthStrength) -> bool {
        self >= required
    }
}

/// Error returned when an issuer or a [`SecurityContext`] cannot be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityContextError {
    /// The issuer name must not be empty or whitespace-only.
    BlankIssuerName,
    /// The principal id carried in the claims must not be blank.
    BlankPrincipalId,
    /// The TTL must be strictly positive.
    NonPositiveTtl,
    /// The TTL exceeds [`SecurityContext::MAX_TTL`].
    TtlTooLong {
        /// Maximum allowed TTL in whole seconds.
        max_seconds: i64,
    },
    /// `issued_at + ttl` is not representable as a timestamp.
    ExpiryOutOfRange,
}

impl fmt::Display for SecurityContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankIssuerName => f.write_str("security context issuer name must not be blank"),
            Self::BlankPrincipalId => {
                f.write_str("security context principal id must not be blank")
            }
            Self::NonPositiveTtl => f.write_str("security context ttl must be positive"),
            Self::TtlTooLong { max_seconds } => {
                write!(f, "security context ttl exceeds maximum of {max_seconds}s")
            }
            Self::ExpiryOutOfRange => {
                f.write_str("security context expiry is out of timestamp range")
            }
        }
    }
}

impl std::error::Error for SecurityContextError {}

/// Capability to mint [`SecurityContext`]s.
///
/// Only the Auth/Crypto Hub (`harw-security-hub`) and the local peer-identity
/// resolver on the `harw-web` authentication path may hold one; see the
/// [module docs](self). Deliberately not `Clone`, not `Serialize` and not
/// `Deserialize`.
///
/// ```rust,compile_fail
/// use harw_types::SecurityContextIssuer;
///
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<SecurityContextIssuer>();
/// ```
///
/// ```rust,compile_fail
/// use harw_types::SecurityContextIssuer;
///
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<SecurityContextIssuer>();
/// ```
#[derive(Debug)]
pub struct SecurityContextIssuer {
    name: String,
}

impl SecurityContextIssuer {
    /// Creates the issuer capability for the Auth/Crypto Hub or the local
    /// peer-identity resolver.
    ///
    /// **Architecture contract:** calling this anywhere else is a violation;
    /// see the [module docs](self).
    ///
    /// # Errors
    /// [`SecurityContextError::BlankIssuerName`] if `name` is blank.
    pub fn new_for_hub(name: impl Into<String>) -> Result<Self, SecurityContextError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(SecurityContextError::BlankIssuerName);
        }
        Ok(Self { name })
    }

    /// Name recorded as `issuer` on every context this issuer mints.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Verified facts the issuer turns into a [`SecurityContext`].
///
/// Claims are plain input to [`SecurityContext::issue`] and grant nothing on
/// their own. They are not `Deserialize` either, because [`Principal`] is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityClaims {
    /// The authenticated principal (from a trusted ingress).
    pub principal: Principal,
    /// Tenant the principal acts in, if tenant-scoped.
    pub tenant: Option<TenantId>,
    /// Workspace the principal acts in, if workspace-scoped.
    pub workspace: Option<WorkspaceId>,
    /// Node the request is bound to, if any.
    pub node: Option<NodeId>,
    /// Enrolled device the principal authenticated from, if any.
    pub device: Option<DeviceId>,
    /// Service identity, when the principal is a non-human service.
    pub service_identity: Option<ServiceIdentityId>,
    /// Ingress trust zone of the request.
    pub trust_zone: TrustZone,
    /// Strength of the authentication the hub verified.
    pub auth_strength: AuthStrength,
}

impl SecurityClaims {
    /// Claims with only the mandatory facts; all optional scopes are `None`.
    #[must_use]
    pub fn new(principal: Principal, trust_zone: TrustZone, auth_strength: AuthStrength) -> Self {
        Self {
            principal,
            tenant: None,
            workspace: None,
            node: None,
            device: None,
            service_identity: None,
            trust_zone,
            auth_strength,
        }
    }
}

/// Trusted, short-lived security context minted after authentication
/// (masterplan §13, §25).
///
/// Fields are private and there is no `Deserialize`; the only constructor is
/// [`SecurityContext::issue`]. See the [module docs](self) for the difference
/// to [`SecurityContextSummary`].
///
/// ```rust
/// use harw_types::{
///     AuthStrength, IngressSurface, PermissionTier, Principal, PrincipalKind,
///     SecurityClaims, SecurityContext, SecurityContextIssuer, TrustZone,
/// };
/// use jiff::{SignedDuration, Timestamp};
///
/// fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
///     let principal = Principal::trusted_ingress(
///         PrincipalKind::Human,
///         "alice",
///         IngressSurface::Web,
///         PermissionTier::Owner,
///     );
///     let claims = SecurityClaims::new(principal, TrustZone::Local, AuthStrength::PeerCredential);
///     let now = Timestamp::from_second(1_700_000_000)?;
///     let ctx = SecurityContext::issue(&issuer, claims, now, SignedDuration::from_mins(5))?;
///
///     assert_eq!(ctx.issuer(), "auth-hub");
///     assert!(ctx.permits(AuthStrength::PeerCredential, TrustZone::Host));
///     assert!(!ctx.permits(AuthStrength::MutualTls, TrustZone::Host));
///     assert!(ctx.is_expired(now + SignedDuration::from_mins(5)));
///     Ok(())
/// }
/// ```
///
/// A context cannot be deserialized:
///
/// ```rust,compile_fail
/// use harw_types::SecurityContext;
///
/// let _ = serde_json::from_str::<SecurityContext>("{}");
/// ```
///
/// A context cannot be built with a struct literal (fields are private):
///
/// ```rust,compile_fail
/// use harw_types::{
///     AuthStrength, Principal, SecurityContext, SecurityContextId, TrustZone,
/// };
/// use jiff::Timestamp;
///
/// fn forge(principal: Principal, id: SecurityContextId, at: Timestamp) -> SecurityContext {
///     SecurityContext {
///         id,
///         principal,
///         tenant: None,
///         workspace: None,
///         node: None,
///         device: None,
///         service_identity: None,
///         trust_zone: TrustZone::Local,
///         auth_strength: AuthStrength::HardwareBacked,
///         issued_at: at,
///         expires_at: at,
///         issuer: String::from("forged"),
///     }
/// }
/// ```
///
/// Fields are not publicly readable or writable:
///
/// ```rust,compile_fail
/// use harw_types::{AuthStrength, SecurityContext};
///
/// fn upgrade(ctx: &mut SecurityContext) {
///     ctx.auth_strength = AuthStrength::HardwareBacked;
/// }
/// ```
///
/// A deserialized summary is not a context:
///
/// ```rust,compile_fail
/// use harw_types::{SecurityContext, SecurityContextSummary};
///
/// fn needs_context(_: SecurityContext) {}
/// let summary: SecurityContextSummary = todo!();
/// needs_context(summary);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecurityContext {
    id: SecurityContextId,
    principal: Principal,
    tenant: Option<TenantId>,
    workspace: Option<WorkspaceId>,
    node: Option<NodeId>,
    device: Option<DeviceId>,
    service_identity: Option<ServiceIdentityId>,
    trust_zone: TrustZone,
    auth_strength: AuthStrength,
    issued_at: Timestamp,
    expires_at: Timestamp,
    issuer: String,
}

impl SecurityContext {
    /// Upper bound for a context's lifetime. Contexts are meant to be
    /// short-lived (§25); longer sessions re-authenticate and re-issue.
    pub const MAX_TTL: SignedDuration = SignedDuration::from_hours(24);

    /// Mints a new context with a fresh [`SecurityContextId`], valid from
    /// `now` until `now + ttl`.
    ///
    /// # Errors
    /// - [`SecurityContextError::BlankPrincipalId`] if the principal id is blank,
    /// - [`SecurityContextError::NonPositiveTtl`] if `ttl <= 0`,
    /// - [`SecurityContextError::TtlTooLong`] if `ttl > MAX_TTL`,
    /// - [`SecurityContextError::ExpiryOutOfRange`] if `now + ttl` overflows.
    pub fn issue(
        issuer: &SecurityContextIssuer,
        claims: SecurityClaims,
        now: Timestamp,
        ttl: SignedDuration,
    ) -> Result<SecurityContext, SecurityContextError> {
        if claims.principal.id().trim().is_empty() {
            return Err(SecurityContextError::BlankPrincipalId);
        }
        if !ttl.is_positive() {
            return Err(SecurityContextError::NonPositiveTtl);
        }
        if ttl > Self::MAX_TTL {
            return Err(SecurityContextError::TtlTooLong {
                max_seconds: Self::MAX_TTL.as_secs(),
            });
        }
        let expires_at = now
            .checked_add(ttl)
            .map_err(|_| SecurityContextError::ExpiryOutOfRange)?;
        let SecurityClaims {
            principal,
            tenant,
            workspace,
            node,
            device,
            service_identity,
            trust_zone,
            auth_strength,
        } = claims;
        Ok(SecurityContext {
            id: SecurityContextId::new(),
            principal,
            tenant,
            workspace,
            node,
            device,
            service_identity,
            trust_zone,
            auth_strength,
            issued_at: now,
            expires_at,
            issuer: issuer.name.clone(),
        })
    }

    /// Context identity; usable as a short-lived reference across boundaries.
    #[must_use]
    pub fn id(&self) -> &SecurityContextId {
        &self.id
    }

    /// The authenticated principal.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Shortcut for `principal().id()`.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        self.principal.id()
    }

    /// Shortcut for `principal().kind()`.
    #[must_use]
    pub fn principal_kind(&self) -> PrincipalKind {
        self.principal.kind()
    }

    /// Tenant scope, if any.
    #[must_use]
    pub fn tenant(&self) -> Option<&TenantId> {
        self.tenant.as_ref()
    }

    /// Workspace scope, if any.
    #[must_use]
    pub fn workspace(&self) -> Option<&WorkspaceId> {
        self.workspace.as_ref()
    }

    /// Bound node, if any.
    #[must_use]
    pub fn node(&self) -> Option<&NodeId> {
        self.node.as_ref()
    }

    /// Enrolled device, if any.
    #[must_use]
    pub fn device(&self) -> Option<&DeviceId> {
        self.device.as_ref()
    }

    /// Service identity, if any.
    #[must_use]
    pub fn service_identity(&self) -> Option<&ServiceIdentityId> {
        self.service_identity.as_ref()
    }

    /// Ingress trust zone.
    #[must_use]
    pub fn trust_zone(&self) -> TrustZone {
        self.trust_zone
    }

    /// Verified authentication strength.
    #[must_use]
    pub fn auth_strength(&self) -> AuthStrength {
        self.auth_strength
    }

    /// Issue time.
    #[must_use]
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Expiry time (exclusive).
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Name of the issuer that minted this context.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// `true` once `now >= expires_at`.
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expires_at
    }

    /// Static strength/zone check: `auth_strength >= min_strength` and the
    /// context's trust zone lies within `max_zone`.
    ///
    /// Does **not** look at time; use [`Self::permits_at`] on a request path.
    #[must_use]
    pub fn permits(&self, min_strength: AuthStrength, max_zone: TrustZone) -> bool {
        self.auth_strength.at_least(min_strength) && self.trust_zone.is_within(max_zone)
    }

    /// [`Self::permits`] plus validity at `now`: `issued_at <= now < expires_at`.
    #[must_use]
    pub fn permits_at(
        &self,
        now: Timestamp,
        min_strength: AuthStrength,
        max_zone: TrustZone,
    ) -> bool {
        now >= self.issued_at && !self.is_expired(now) && self.permits(min_strength, max_zone)
    }

    /// Non-authoritative display summary of this context.
    #[must_use]
    pub fn summary(&self) -> SecurityContextSummary {
        SecurityContextSummary {
            id: self.id.clone(),
            principal_id: self.principal.id().to_owned(),
            principal_kind: self.principal.kind(),
            permission_tier: self.principal.tier(),
            tenant: self.tenant.clone(),
            workspace: self.workspace.clone(),
            node: self.node.clone(),
            device: self.device.clone(),
            service_identity: self.service_identity.clone(),
            trust_zone: self.trust_zone,
            auth_strength: self.auth_strength,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
            issuer: self.issuer.clone(),
        }
    }
}

/// Non-authoritative, wire-friendly view of a [`SecurityContext`] for display,
/// logs and UI payloads.
///
/// Unlike [`SecurityContext`] this **is** `Deserialize` and has public
/// fields — precisely because it grants nothing. Never authorize from a
/// summary and never convert one back into a context; see the
/// [module docs](self).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityContextSummary {
    /// Context identity.
    pub id: SecurityContextId,
    /// Principal id.
    pub principal_id: String,
    /// Principal kind.
    pub principal_kind: PrincipalKind,
    /// Principal's operation tier (display only).
    pub permission_tier: PermissionTier,
    /// Tenant scope.
    pub tenant: Option<TenantId>,
    /// Workspace scope.
    pub workspace: Option<WorkspaceId>,
    /// Bound node.
    pub node: Option<NodeId>,
    /// Enrolled device.
    pub device: Option<DeviceId>,
    /// Service identity.
    pub service_identity: Option<ServiceIdentityId>,
    /// Ingress trust zone.
    pub trust_zone: TrustZone,
    /// Authentication strength.
    pub auth_strength: AuthStrength,
    /// Issue time.
    pub issued_at: Timestamp,
    /// Expiry time (exclusive).
    pub expires_at: Timestamp,
    /// Issuer name.
    pub issuer: String,
}

#[cfg(test)]
mod tests {
    use super::{
        AuthStrength, SecurityClaims, SecurityContext, SecurityContextError, SecurityContextIssuer,
        SecurityContextSummary, TrustZone,
    };
    use crate::ids::{DeviceId, NodeId, ServiceIdentityId, TenantId, WorkspaceId};
    use crate::principal::{IngressSurface, PermissionTier, Principal, PrincipalKind};
    use crate::test_support::{TestError, TestResult, ctx};
    use jiff::{SignedDuration, Timestamp};

    const ALL_STRENGTHS: [AuthStrength; 5] = [
        AuthStrength::None,
        AuthStrength::PeerCredential,
        AuthStrength::Token,
        AuthStrength::MutualTls,
        AuthStrength::HardwareBacked,
    ];
    const ALL_ZONES: [TrustZone; 4] = [
        TrustZone::Local,
        TrustZone::Host,
        TrustZone::Cluster,
        TrustZone::Remote,
    ];

    fn principal() -> Principal {
        Principal::trusted_ingress(
            PrincipalKind::Human,
            "alice",
            IngressSurface::Web,
            PermissionTier::Owner,
        )
    }

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    fn issue_with(
        zone: TrustZone,
        strength: AuthStrength,
        now: Timestamp,
        ttl: SignedDuration,
    ) -> TestResult<SecurityContext> {
        let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
        let claims = SecurityClaims::new(principal(), zone, strength);
        Ok(SecurityContext::issue(&issuer, claims, now, ttl)?)
    }

    #[test]
    fn test_issue_populates_fields_and_expires_at_ttl() -> TestResult {
        let now = at(1_700_000_000)?;
        let ttl = SignedDuration::from_mins(5);
        let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
        let mut claims = SecurityClaims::new(principal(), TrustZone::Host, AuthStrength::Token);
        claims.tenant = Some(TenantId::try_from_str("tenant-1")?);
        claims.workspace = Some(WorkspaceId::try_from_str("ws-1")?);
        claims.node = Some(NodeId::try_from_str("node-1")?);
        claims.device = Some(DeviceId::try_from_str("device-1")?);
        claims.service_identity = Some(ServiceIdentityId::try_from_str("svc-1")?);

        let context = SecurityContext::issue(&issuer, claims, now, ttl)?;

        assert!(!context.id().as_str().trim().is_empty());
        assert_eq!(context.principal_id(), "alice");
        assert_eq!(context.principal_kind(), PrincipalKind::Human);
        assert_eq!(context.principal().tier(), PermissionTier::Owner);
        assert_eq!(context.tenant().map(TenantId::as_str), Some("tenant-1"));
        assert_eq!(context.workspace().map(WorkspaceId::as_str), Some("ws-1"));
        assert_eq!(context.node().map(NodeId::as_str), Some("node-1"));
        assert_eq!(context.device().map(DeviceId::as_str), Some("device-1"));
        assert_eq!(
            context.service_identity().map(ServiceIdentityId::as_str),
            Some("svc-1")
        );
        assert_eq!(context.trust_zone(), TrustZone::Host);
        assert_eq!(context.auth_strength(), AuthStrength::Token);
        assert_eq!(context.issuer(), "auth-hub");
        assert_eq!(context.issued_at(), now);
        assert_eq!(context.expires_at(), now + ttl);

        assert!(!context.is_expired(now));
        assert!(!context.is_expired(now + SignedDuration::from_secs(299)));
        assert!(context.is_expired(now + ttl));
        assert!(context.is_expired(now + SignedDuration::from_mins(6)));
        Ok(())
    }

    #[test]
    fn test_issue_assigns_fresh_context_ids() -> TestResult {
        let now = at(1_700_000_000)?;
        let ttl = SignedDuration::from_mins(1);
        let first = issue_with(TrustZone::Local, AuthStrength::Token, now, ttl)?;
        let second = issue_with(TrustZone::Local, AuthStrength::Token, now, ttl)?;
        assert_ne!(first.id(), second.id());
        Ok(())
    }

    #[test]
    fn test_issue_rejects_invalid_ttl_and_blank_inputs() -> TestResult {
        let now = at(1_700_000_000)?;
        let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
        let claims = SecurityClaims::new(principal(), TrustZone::Local, AuthStrength::Token);

        assert_eq!(
            SecurityContext::issue(&issuer, claims.clone(), now, SignedDuration::ZERO),
            Err(SecurityContextError::NonPositiveTtl)
        );
        assert_eq!(
            SecurityContext::issue(&issuer, claims.clone(), now, SignedDuration::from_secs(-1)),
            Err(SecurityContextError::NonPositiveTtl)
        );
        assert_eq!(
            SecurityContext::issue(
                &issuer,
                claims.clone(),
                now,
                SecurityContext::MAX_TTL + SignedDuration::from_secs(1)
            ),
            Err(SecurityContextError::TtlTooLong {
                max_seconds: 24 * 60 * 60
            })
        );
        assert!(SecurityContext::issue(&issuer, claims, now, SecurityContext::MAX_TTL).is_ok());

        let blank = Principal::trusted_ingress(
            PrincipalKind::Human,
            " ",
            IngressSurface::Web,
            PermissionTier::Owner,
        );
        let blank_claims = SecurityClaims::new(blank, TrustZone::Local, AuthStrength::Token);
        assert_eq!(
            SecurityContext::issue(&issuer, blank_claims, now, SignedDuration::from_mins(1)),
            Err(SecurityContextError::BlankPrincipalId)
        );

        assert_eq!(
            SecurityContextIssuer::new_for_hub(" \t").map(|issuer| issuer.name().to_owned()),
            Err(SecurityContextError::BlankIssuerName)
        );
        Ok(())
    }

    #[test]
    fn test_issue_rejects_expiry_beyond_timestamp_range() -> TestResult {
        let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
        let claims = SecurityClaims::new(principal(), TrustZone::Local, AuthStrength::Token);
        assert_eq!(
            SecurityContext::issue(
                &issuer,
                claims,
                Timestamp::MAX,
                SignedDuration::from_secs(1)
            ),
            Err(SecurityContextError::ExpiryOutOfRange)
        );
        Ok(())
    }

    #[test]
    fn test_auth_strength_is_totally_ordered_weakest_to_strongest() {
        for window in ALL_STRENGTHS.windows(2) {
            assert!(window[0] < window[1]);
        }
        for (i, have) in ALL_STRENGTHS.iter().enumerate() {
            for (j, required) in ALL_STRENGTHS.iter().enumerate() {
                assert_eq!(have.at_least(*required), i >= j, "{have:?} vs {required:?}");
            }
        }
    }

    #[test]
    fn test_trust_zone_is_ordered_local_to_remote() {
        for window in ALL_ZONES.windows(2) {
            assert!(window[0] < window[1]);
        }
        for (i, zone) in ALL_ZONES.iter().enumerate() {
            for (j, max_zone) in ALL_ZONES.iter().enumerate() {
                assert_eq!(
                    zone.is_within(*max_zone),
                    i <= j,
                    "{zone:?} vs {max_zone:?}"
                );
            }
        }
    }

    #[test]
    fn test_permits_matrix_over_all_strengths_and_zones() -> TestResult {
        let now = at(1_700_000_000)?;
        let ttl = SignedDuration::from_mins(5);
        for (si, have_strength) in ALL_STRENGTHS.iter().enumerate() {
            for (zi, have_zone) in ALL_ZONES.iter().enumerate() {
                let context = issue_with(*have_zone, *have_strength, now, ttl)?;
                for (ri, min_strength) in ALL_STRENGTHS.iter().enumerate() {
                    for (mi, max_zone) in ALL_ZONES.iter().enumerate() {
                        let expected = si >= ri && zi <= mi;
                        assert_eq!(
                            context.permits(*min_strength, *max_zone),
                            expected,
                            "have {have_strength:?}/{have_zone:?}, need {min_strength:?}/{max_zone:?}"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_permits_at_respects_validity_window() -> TestResult {
        let now = at(1_700_000_000)?;
        let ttl = SignedDuration::from_mins(5);
        let context = issue_with(TrustZone::Local, AuthStrength::MutualTls, now, ttl)?;
        let (strength, zone) = (AuthStrength::Token, TrustZone::Host);

        assert!(context.permits_at(now, strength, zone));
        assert!(context.permits_at(now + SignedDuration::from_secs(299), strength, zone));
        assert!(!context.permits_at(now + ttl, strength, zone));
        assert!(!context.permits_at(now - SignedDuration::from_secs(1), strength, zone));
        assert!(!context.permits_at(now, AuthStrength::HardwareBacked, zone));
        Ok(())
    }

    #[test]
    fn test_trust_zone_and_auth_strength_serde_snake_case() -> TestResult {
        let zones = [
            (TrustZone::Local, "\"local\""),
            (TrustZone::Host, "\"host\""),
            (TrustZone::Cluster, "\"cluster\""),
            (TrustZone::Remote, "\"remote\""),
        ];
        for (zone, json) in zones {
            assert_eq!(serde_json::to_string(&zone)?, json);
            assert_eq!(serde_json::from_str::<TrustZone>(json)?, zone);
        }
        let strengths = [
            (AuthStrength::None, "\"none\""),
            (AuthStrength::PeerCredential, "\"peer_credential\""),
            (AuthStrength::Token, "\"token\""),
            (AuthStrength::MutualTls, "\"mutual_tls\""),
            (AuthStrength::HardwareBacked, "\"hardware_backed\""),
        ];
        for (strength, json) in strengths {
            assert_eq!(serde_json::to_string(&strength)?, json);
            assert_eq!(serde_json::from_str::<AuthStrength>(json)?, strength);
        }
        assert!(serde_json::from_str::<TrustZone>("\"Local\"").is_err());
        assert!(serde_json::from_str::<AuthStrength>("\"root\"").is_err());
        Ok(())
    }

    #[test]
    fn test_summary_round_trips_and_mirrors_context() -> TestResult {
        let now = at(1_700_000_000)?;
        let issuer = SecurityContextIssuer::new_for_hub("auth-hub")?;
        let mut claims =
            SecurityClaims::new(principal(), TrustZone::Cluster, AuthStrength::MutualTls);
        claims.tenant = Some(TenantId::try_from_str("tenant-1")?);
        claims.node = Some(NodeId::try_from_str("node-1")?);
        let context = SecurityContext::issue(&issuer, claims, now, SignedDuration::from_mins(5))?;

        let summary = context.summary();
        assert_eq!(&summary.id, context.id());
        assert_eq!(summary.principal_id, "alice");
        assert_eq!(summary.principal_kind, PrincipalKind::Human);
        assert_eq!(summary.permission_tier, PermissionTier::Owner);
        assert_eq!(summary.trust_zone, TrustZone::Cluster);
        assert_eq!(summary.auth_strength, AuthStrength::MutualTls);
        assert_eq!(summary.expires_at, context.expires_at());
        assert_eq!(summary.issuer, "auth-hub");
        assert_eq!(summary.workspace, None);

        let json = serde_json::to_string(&summary)?;
        let round_tripped: SecurityContextSummary = serde_json::from_str(&json)?;
        assert_eq!(round_tripped, summary);

        // The trusted context serializes to the same field layout for the
        // shared fields (one-way: it can never be deserialized back).
        let context_json = serde_json::to_value(&context)?;
        let trust_zone = context_json
            .get("trust_zone")
            .ok_or(TestError::Missing("trust_zone"))?;
        assert_eq!(trust_zone, "cluster");
        Ok(())
    }

    #[test]
    fn test_summary_rejects_blank_ids_on_the_wire() -> TestResult {
        let now = at(1_700_000_000)?;
        let context = issue_with(
            TrustZone::Local,
            AuthStrength::Token,
            now,
            SignedDuration::from_mins(1),
        )?;
        let mut value = serde_json::to_value(context.summary())?;
        let object = value
            .as_object_mut()
            .ok_or(TestError::Unexpected(String::from(
                "summary is not an object",
            )))?;
        object.insert(String::from("id"), serde_json::Value::from(" "));
        assert!(serde_json::from_value::<SecurityContextSummary>(value).is_err());
        Ok(())
    }
}

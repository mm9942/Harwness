//! Authority primitives for HARW.
//!
//! The public surface intentionally separates requests and snapshots from a
//! granted [`AuthorityContext`].  The implementation is added below together
//! with its internal algebra and bootstrap tests.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use harw_types::{TenantId, WorkspaceId};
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
mod test_support;

/// The complete, deliberately fixed permission vocabulary.
///
/// New variants must be added to [`Self::ALL`] explicitly.  The exhaustive
/// algebra test relies on that list so a new permission never becomes granted
/// simply because it was omitted from a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadWorkspace,
    WriteWorkspace,
    ExecuteProcess,
    NetworkAccess,
    ReadSecrets,
    ManagePlugins,
    ReadCargoRegistry,
}

impl Permission {
    /// Every permission, in the sole canonical bit/algebra order.
    pub const ALL: [Self; 7] = [
        Self::ReadWorkspace,
        Self::WriteWorkspace,
        Self::ExecuteProcess,
        Self::NetworkAccess,
        Self::ReadSecrets,
        Self::ManagePlugins,
        Self::ReadCargoRegistry,
    ];
}

/// Freely constructible, serializable request or upper bound.
///
/// A request has no authority of its own.  It becomes a grant only after a
/// [`PolicyBootstrap`] evaluates it against a trusted policy, or after an
/// existing [`AuthorityContext`] restricts itself with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    requested: BTreeSet<Permission>,
    #[serde(default)]
    network_scope: NetworkScope,
}

impl PermissionRequest {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_permissions(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self {
            requested: permissions.into_iter().collect(),
            network_scope: NetworkScope::empty(),
        }
    }

    /// Sets the requested network upper bound.  This remains only a request.
    #[must_use]
    pub fn with_network_scope(mut self, network_scope: NetworkScope) -> Self {
        self.network_scope = network_scope;
        self
    }

    #[must_use]
    pub fn contains(&self, permission: Permission) -> bool {
        self.requested.contains(&permission)
    }

    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.requested.iter().copied()
    }

    #[must_use]
    pub fn network_scope(&self) -> &NetworkScope {
        &self.network_scope
    }

    fn is_within(&self, permissions: &PermissionSet, network_scope: &NetworkScope) -> bool {
        self.requested.is_subset(&permissions.granted)
            && self.network_scope.is_subset_of(network_scope)
    }
}

/// Granted permissions.  Its representation and all constructors for nonempty
/// grants are private to this crate, except for [`Self::from_policy`], which
/// is intended for code-defined ceilings that are not derived from a
/// deserialized policy.
///
/// This type deliberately implements neither `Deserialize`, `Default`,
/// `FromIterator`, nor a public policy constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSet {
    granted: BTreeSet<Permission>,
}

impl PermissionSet {
    /// A harmless, capability-free grant.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            granted: BTreeSet::new(),
        }
    }

    /// A publicly constructible set of permissions for code-defined ceilings.
    ///
    /// The caller vouches that the passed permissions form a safe upper bound.
    /// This is used by [`harw_core::mode::InteractionMode::permission_ceiling`]
    /// to express mode-specific ceilings without exposing a generic grant
    /// constructor.
    #[must_use]
    pub fn from_policy(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self::from_validated_policy(permissions)
    }

    #[must_use]
    pub fn contains(&self, permission: Permission) -> bool {
        self.granted.contains(&permission)
    }

    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.granted.iter().copied()
    }

    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.granted.is_subset(&parent.granted)
    }

    fn restrict(&self, request: &PermissionRequest) -> Self {
        Self {
            granted: self
                .granted
                .intersection(&request.requested)
                .copied()
                .collect(),
        }
    }

    fn from_validated_policy(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self {
            granted: permissions.into_iter().collect(),
        }
    }

    #[cfg(test)]
    fn from_test_mask(mask: u8) -> Self {
        Self::from_test_permissions(
            Permission::ALL
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(index, permission)| {
                    ((mask & (1 << index)) != 0).then_some(permission)
                }),
        )
    }

    #[cfg(test)]
    fn from_test_permissions(permissions: impl IntoIterator<Item = Permission>) -> Self {
        Self::from_validated_policy(permissions)
    }
}

/// A DNS name or address range permitted by a network scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EgressTarget {
    /// Exactly one DNS host, without subdomains.
    Host(String),
    /// A DNS suffix and its subdomains, matched at a dot boundary.
    DnsSuffix(String),
    /// An IP address range.
    Cidr(IpNet),
    /// Any public DNS name ([`is_public_dns_name`]): never an IP literal,
    /// never `localhost`, a single-label or a reserved local name. Resolved
    /// addresses are still classified by the egress resolver, which admits
    /// only public address classes for such names. Used for the open
    /// research web (`[network].research_web = "open"`); wire form
    /// [`PUBLIC_DNS_WIRE`].
    PublicDns,
}

/// Wire form of [`EgressTarget::PublicDns`] (never a valid host name).
pub const PUBLIC_DNS_WIRE: &str = "*public-dns";

/// Reserved or local-only DNS suffixes that [`is_public_dns_name`] rejects.
const NON_PUBLIC_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "localdomain",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "private",
    "arpa",
    "test",
    "invalid",
    "example",
    "onion",
];

/// Whether `host` is a public DNS name that the open research web may reach.
///
/// Rejects IP literals (IPv4, IPv6 with or without brackets), empty labels,
/// single-label names (search-domain expansion could reach internal hosts),
/// characters outside `[a-z0-9-.]` and every name at or below a reserved or
/// local-only suffix ([`NON_PUBLIC_SUFFIXES`], e.g. `localhost`, `local`,
/// `internal`, `home.arpa`). This is a name check only; the egress resolver
/// additionally rejects every non-public resolved address.
///
/// # Examples
/// ```rust
/// use harw_authority::is_public_dns_name;
///
/// assert!(is_public_dns_name("www.bund.de"));
/// assert!(is_public_dns_name("Docs.RS."));
/// assert!(!is_public_dns_name("127.0.0.1"));
/// assert!(!is_public_dns_name("[::1]"));
/// assert!(!is_public_dns_name("localhost"));
/// assert!(!is_public_dns_name("printer.local"));
/// assert!(!is_public_dns_name("intranet"));
/// ```
#[must_use]
pub fn is_public_dns_name(host: &str) -> bool {
    let host = normalize_host(host);
    let host = host.strip_suffix('.').unwrap_or(&host);
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<std::net::IpAddr>().is_ok() || host.contains(':') {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
    {
        return false;
    }
    // Ein rein numerisches letztes Label ist keine TLD (verkürzte IPv4-Formen).
    if labels
        .last()
        .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    !NON_PUBLIC_SUFFIXES
        .iter()
        .any(|suffix| host_matches_suffix(suffix, host))
}

impl Serialize for EgressTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Host(host) => serializer.serialize_str(&format!("={host}")),
            Self::DnsSuffix(suffix) => serializer.serialize_str(suffix),
            Self::Cidr(net) => serializer.collect_str(net),
            Self::PublicDns => serializer.serialize_str(PUBLIC_DNS_WIRE),
        }
    }
}

impl<'de> Deserialize<'de> for EgressTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if raw == PUBLIC_DNS_WIRE {
            return Ok(Self::PublicDns);
        }
        if let Some(host) = raw.strip_prefix('=') {
            return Ok(Self::Host(host.to_owned()));
        }
        if let Ok(net) = raw.parse::<IpNet>() {
            return Ok(Self::Cidr(net));
        }
        Ok(Self::DnsSuffix(raw))
    }
}

/// Central, deny-by-default network scope.
///
/// It can be constructed from configuration as a request, but a granted
/// context only obtains it through policy evaluation.  Its only derivation
/// operation is intersection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkScope {
    #[serde(rename = "allow_hosts")]
    allowed: BTreeSet<EgressTarget>,
}

impl NetworkScope {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_hosts(hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed: hosts
                .into_iter()
                .map(|host| normalize_host(&host))
                .filter(|host| !host.is_empty())
                .map(EgressTarget::DnsSuffix)
                .collect(),
        }
    }

    #[must_use]
    pub fn from_targets(targets: impl IntoIterator<Item = EgressTarget>) -> Self {
        Self {
            allowed: targets.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    pub fn hosts(&self) -> impl Iterator<Item = &str> + '_ {
        self.allowed.iter().filter_map(|target| match target {
            EgressTarget::Host(host) | EgressTarget::DnsSuffix(host) => Some(host.as_str()),
            EgressTarget::Cidr(_) | EgressTarget::PublicDns => None,
        })
    }

    /// `true`, wenn der Scope jeden öffentlichen DNS-Namen zulässt
    /// ([`EgressTarget::PublicDns`], offenes Recherche-Netz).
    #[must_use]
    pub fn allows_public_dns(&self) -> bool {
        self.allowed.contains(&EgressTarget::PublicDns)
    }

    pub fn targets(&self) -> impl Iterator<Item = &EgressTarget> + '_ {
        self.allowed.iter()
    }

    #[must_use]
    pub fn allows(&self, host: &str) -> bool {
        let host = normalize_host(host);
        !host.is_empty()
            && self
                .allowed
                .iter()
                .any(|target| target.matches_normalized_host(&host))
    }

    #[must_use]
    pub fn allows_addr(&self, addr: std::net::IpAddr) -> bool {
        self.allowed.iter().any(|target| target.matches_addr(addr))
    }

    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        let mut allowed = BTreeSet::new();
        for left in &self.allowed {
            for right in &other.allowed {
                if let Some(target) = intersect_targets(left, right) {
                    allowed.insert(target);
                }
            }
        }
        Self { allowed }
    }

    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        self.allowed.iter().all(|child| {
            parent
                .allowed
                .iter()
                .any(|candidate| target_is_subset(child, candidate))
        })
    }
}

impl EgressTarget {
    #[must_use]
    pub fn matches_host(&self, host: &str) -> bool {
        let host = normalize_host(host);
        !host.is_empty() && self.matches_normalized_host(&host)
    }

    fn matches_normalized_host(&self, host: &str) -> bool {
        match self {
            Self::Host(allowed) => host_matches_exact(allowed, host),
            Self::DnsSuffix(allowed) => host_matches_suffix(allowed, host),
            Self::Cidr(_) => false,
            Self::PublicDns => is_public_dns_name(host),
        }
    }

    #[must_use]
    pub fn matches_addr(&self, addr: std::net::IpAddr) -> bool {
        matches!(self, Self::Cidr(net) if net.contains(&addr))
    }
}

/// Compares DNS suffixes at a label boundary.  This is the only implementation
/// of this security-sensitive comparison in the authority core.
#[must_use]
pub fn host_matches_suffix(allowed: &str, host: &str) -> bool {
    let allowed = allowed.strip_suffix('.').unwrap_or(allowed);
    let host = host.strip_suffix('.').unwrap_or(host);
    if allowed.is_empty() || host.is_empty() {
        return false;
    }
    match (
        allowed.parse::<std::net::IpAddr>(),
        host.parse::<std::net::IpAddr>(),
    ) {
        (Ok(allowed_ip), Ok(host_ip)) => return allowed_ip == host_ip,
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => return false,
        (Err(_), Err(_)) => {}
    }
    if host.eq_ignore_ascii_case(allowed) || host.len() <= allowed.len() {
        return host.eq_ignore_ascii_case(allowed);
    }
    let start = host.len() - allowed.len();
    let Some(suffix) = host.get(start..) else {
        return false;
    };
    suffix.eq_ignore_ascii_case(allowed) && host.as_bytes().get(start - 1) == Some(&b'.')
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn host_matches_exact(allowed: &str, host: &str) -> bool {
    let allowed = allowed.strip_suffix('.').unwrap_or(allowed);
    let host = host.strip_suffix('.').unwrap_or(host);
    !allowed.is_empty() && host.eq_ignore_ascii_case(allowed)
}

fn intersect_targets(left: &EgressTarget, right: &EgressTarget) -> Option<EgressTarget> {
    match (left, right) {
        (EgressTarget::Host(a), EgressTarget::Host(b)) if a == b => {
            Some(EgressTarget::Host(a.clone()))
        }
        (EgressTarget::DnsSuffix(a), EgressTarget::DnsSuffix(b)) if a == b => {
            Some(EgressTarget::DnsSuffix(a.clone()))
        }
        (EgressTarget::Host(host), EgressTarget::DnsSuffix(suffix))
        | (EgressTarget::DnsSuffix(suffix), EgressTarget::Host(host))
            if host_matches_suffix(suffix, host) =>
        {
            Some(EgressTarget::Host(host.clone()))
        }
        (EgressTarget::Cidr(a), EgressTarget::Cidr(b)) if a == b || a.contains(b) => {
            Some(EgressTarget::Cidr(*b))
        }
        (EgressTarget::Cidr(a), EgressTarget::Cidr(b)) if b.contains(a) => {
            Some(EgressTarget::Cidr(*a))
        }
        (EgressTarget::PublicDns, EgressTarget::PublicDns) => Some(EgressTarget::PublicDns),
        (
            EgressTarget::PublicDns,
            name @ (EgressTarget::Host(host) | EgressTarget::DnsSuffix(host)),
        )
        | (
            name @ (EgressTarget::Host(host) | EgressTarget::DnsSuffix(host)),
            EgressTarget::PublicDns,
        ) if is_public_dns_name(host) => Some(name.clone()),
        _ => None,
    }
}

fn target_is_subset(child: &EgressTarget, parent: &EgressTarget) -> bool {
    match (child, parent) {
        (EgressTarget::Host(a), EgressTarget::Host(b)) => a == b,
        (EgressTarget::DnsSuffix(a), EgressTarget::DnsSuffix(b)) => a == b,
        (EgressTarget::Host(host), EgressTarget::DnsSuffix(suffix)) => {
            host_matches_suffix(suffix, host)
        }
        (EgressTarget::Cidr(child), EgressTarget::Cidr(parent)) => {
            child == parent || parent.contains(child)
        }
        (EgressTarget::PublicDns, EgressTarget::PublicDns) => true,
        (EgressTarget::Host(host) | EgressTarget::DnsSuffix(host), EgressTarget::PublicDns) => {
            is_public_dns_name(host)
        }
        _ => false,
    }
}

/// Resolved, canonical workspace identity.  Constructing a binding requires a
/// registry built from trusted operator configuration; wire payloads cannot
/// create a grant merely by deserializing this scope value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    tenant: TenantId,
    workspace: WorkspaceId,
    canonical_root: PathBuf,
}

impl WorkspaceBinding {
    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    pub fn resolve_existing(&self, relative: &Path) -> AuthorityResult<PathBuf> {
        let candidate = self.join_relative(relative)?;
        let canonical = candidate
            .canonicalize()
            .map_err(|error| AuthorityError::Io {
                path: candidate,
                reason: error.to_string(),
            })?;
        self.ensure_contained(canonical)
    }

    pub fn resolve_for_create(&self, relative: &Path) -> AuthorityResult<PathBuf> {
        let candidate = self.join_relative(relative)?;
        let parent = candidate
            .parent()
            .ok_or_else(|| AuthorityError::InvalidRelativePath {
                path: relative.to_path_buf(),
            })?;
        let canonical_parent = parent.canonicalize().map_err(|error| AuthorityError::Io {
            path: parent.to_path_buf(),
            reason: error.to_string(),
        })?;
        let contained_parent = self.ensure_contained(canonical_parent)?;
        let name = candidate
            .file_name()
            .ok_or_else(|| AuthorityError::InvalidRelativePath {
                path: relative.to_path_buf(),
            })?;
        Ok(contained_parent.join(name))
    }

    fn join_relative(&self, relative: &Path) -> AuthorityResult<PathBuf> {
        if relative.as_os_str().is_empty()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::Prefix(_) | Component::RootDir | Component::ParentDir
                )
            })
        {
            return Err(AuthorityError::InvalidRelativePath {
                path: relative.to_path_buf(),
            });
        }
        Ok(self.canonical_root.join(relative))
    }

    fn ensure_contained(&self, canonical: PathBuf) -> AuthorityResult<PathBuf> {
        if canonical.starts_with(&self.canonical_root) {
            Ok(canonical)
        } else {
            Err(AuthorityError::PathEscapesWorkspace {
                path: canonical,
                workspace: self.workspace.clone(),
            })
        }
    }
}

/// A configuration-derived workspace registration.  It has no authority until
/// the registry resolves it and a bootstrap subsequently issues a context.
#[derive(Debug, Clone)]
pub struct WorkspaceRegistration {
    pub tenant: TenantId,
    pub workspace: WorkspaceId,
    pub root: PathBuf,
}

/// Registry of configured workspace identities.
#[derive(Debug, Clone)]
pub struct WorkspaceRegistry {
    bindings: BTreeMap<(String, String), WorkspaceBinding>,
}

impl WorkspaceRegistry {
    pub fn build(
        harness_root: &Path,
        registrations: impl IntoIterator<Item = WorkspaceRegistration>,
    ) -> AuthorityResult<Self> {
        let harness_root = harness_root
            .canonicalize()
            .map_err(|error| AuthorityError::Io {
                path: harness_root.to_path_buf(),
                reason: error.to_string(),
            })?;
        if !harness_root.is_dir() {
            return Err(AuthorityError::NotDirectory { path: harness_root });
        }

        let mut bindings = BTreeMap::new();
        for registration in registrations {
            let candidate = if registration.root.is_absolute() {
                registration.root
            } else {
                harness_root.join(registration.root)
            };
            let canonical_root = candidate
                .canonicalize()
                .map_err(|error| AuthorityError::Io {
                    path: candidate,
                    reason: error.to_string(),
                })?;
            if !canonical_root.is_dir() {
                return Err(AuthorityError::NotDirectory {
                    path: canonical_root,
                });
            }
            if !canonical_root.starts_with(&harness_root) {
                return Err(AuthorityError::WorkspaceEscapesHarness {
                    path: canonical_root,
                });
            }

            let key = (
                registration.tenant.as_str().to_owned(),
                registration.workspace.as_str().to_owned(),
            );
            let binding = WorkspaceBinding {
                tenant: registration.tenant,
                workspace: registration.workspace,
                canonical_root,
            };
            if bindings.insert(key.clone(), binding).is_some() {
                return Err(AuthorityError::DuplicateWorkspaceBinding {
                    tenant: key.0,
                    workspace: key.1,
                });
            }
        }
        Ok(Self { bindings })
    }

    pub fn resolve(
        &self,
        tenant: &TenantId,
        workspace: &WorkspaceId,
    ) -> AuthorityResult<WorkspaceBinding> {
        self.bindings
            .get(&(tenant.as_str().to_owned(), workspace.as_str().to_owned()))
            .cloned()
            .ok_or_else(|| AuthorityError::WorkspaceNotBound {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
            })
    }
}

/// The source class of a trusted operator policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySourceKind {
    OperatorHome,
    System,
}

/// Fixed, independently verifiable policy locations.
///
/// There is intentionally no `from_path`: accepting a caller-controlled path
/// would turn any freely constructed TOML into a root grant source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    OperatorHome,
    System,
}

impl PolicySource {
    pub const SYSTEM_POLICY_PATH: &'static str = "/etc/harw/authority.toml";
    pub const OPERATOR_POLICY_RELATIVE_PATH: &'static str = ".harw/authority.toml";

    #[must_use]
    pub const fn kind(self) -> PolicySourceKind {
        match self {
            Self::OperatorHome => PolicySourceKind::OperatorHome,
            Self::System => PolicySourceKind::System,
        }
    }

    fn path(self) -> AuthorityResult<PathBuf> {
        match self {
            Self::System => Ok(PathBuf::from(Self::SYSTEM_POLICY_PATH)),
            Self::OperatorHome => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .ok_or(AuthorityError::OperatorHomeUnavailable)?;
                Ok(home.join(Self::OPERATOR_POLICY_RELATIVE_PATH))
            }
        }
    }
}

/// Origin metadata bound to every context and copied to snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityOrigin {
    Policy {
        source: PolicySourceKind,
        policy_digest: String,
    },
    /// A code-defined, process-local authority ceiling used while assembling
    /// built-in runtime entry points.
    Runtime,
    /// Ursprung einer über [`SandboxSpec::from_resolved_for_test`] oder ein
    /// crate-internes Testfixture gebauten Sandbox. Gilt bewusst nicht als
    /// Policy- oder Runtime-Ursprung.
    #[cfg(any(test, feature = "test-support"))]
    TestOnly,
    /// Harness-eigene, rein lesende Sicht auf ein vom Harness befülltes
    /// Verzeichnis (z. B. die Unterlagen-Kopie eines Matrix-Sitzes unter
    /// `<profile>/knowledge/matrix/<scenario>/<run>/materials/<seat>/`).
    ///
    /// Einziger Ursprung, dessen Kontext in [`AuthorityContext::is_subset_of`]
    /// einen *anderen* Workspace als der Parent tragen darf — und auch das nur
    /// mit Rechten ⊆ `{ReadWorkspace}` ∩ Parent-Rechte und leerem Netz-Scope.
    /// Entsteht ausschließlich über [`SandboxSpec::harness_read_view`].
    HarnessReadView,
}

impl AuthorityOrigin {
    #[must_use]
    pub fn source(&self) -> Option<PolicySourceKind> {
        match self {
            Self::Policy { source, .. } => Some(*source),
            Self::Runtime | Self::HarnessReadView => None,
            #[cfg(any(test, feature = "test-support"))]
            Self::TestOnly => None,
        }
    }

    #[must_use]
    pub fn policy_digest(&self) -> Option<&str> {
        match self {
            Self::Policy { policy_digest, .. } => Some(policy_digest),
            Self::Runtime | Self::HarnessReadView => None,
            #[cfg(any(test, feature = "test-support"))]
            Self::TestOnly => None,
        }
    }
}

/// An immutable grant.  It is neither deserializable nor directly
/// constructible outside this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    workspace: WorkspaceBinding,
    permissions: PermissionSet,
    network_scope: NetworkScope,
    origin: AuthorityOrigin,
}

impl AuthorityContext {
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceBinding {
        &self.workspace
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionSet {
        &self.permissions
    }

    #[must_use]
    pub fn network_scope(&self) -> &NetworkScope {
        &self.network_scope
    }

    #[must_use]
    pub fn origin(&self) -> &AuthorityOrigin {
        &self.origin
    }

    /// Derives a child context by intersecting every permission and network
    /// scope dimension with a non-authoritative request.
    #[must_use]
    pub fn restrict(&self, request: &PermissionRequest) -> Self {
        Self {
            workspace: self.workspace.clone(),
            permissions: self.permissions.restrict(request),
            network_scope: self.network_scope.intersection(&request.network_scope),
            origin: self.origin.clone(),
        }
    }

    /// Prüft, ob dieser Kontext eine echte Teilmenge von `parent` ist.
    ///
    /// # Beschreibung
    /// Regelfall: gleicher Workspace, Rechte ⊆ Parent-Rechte, Netz-Scope ⊆
    /// Parent-Scope.
    ///
    /// Einzige, bewusst schmale Ausnahme: ein Kontext mit Ursprung
    /// [`AuthorityOrigin::HarnessReadView`] darf an einen *anderen* Workspace
    /// gebunden sein, sofern (a) derselbe Tenant, (b) Rechte ⊆
    /// `{ReadWorkspace}` ∩ Parent-Rechte und (c) ein leerer Netz-Scope
    /// vorliegen. Begründung: solche Sichten sind harness-eigene, rein
    /// lesende Kopien (Matrix-Unterlagen), in die der Harness nur zulässige
    /// Dateien kopiert; Schreiben, Ausführen und Netz sind damit
    /// ausgeschlossen, und Lesen setzt voraus, dass schon der Parent lesen
    /// darf. Jeder andere Ursprung verlangt weiterhin denselben Workspace
    /// (fail-closed).
    ///
    /// # Returns
    /// `true` genau dann, wenn eine der beiden Regeln greift.
    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        if self.workspace == parent.workspace {
            return self.permissions.is_subset_of(&parent.permissions)
                && self.network_scope.is_subset_of(&parent.network_scope);
        }
        matches!(self.origin, AuthorityOrigin::HarnessReadView)
            && self.workspace.tenant == parent.workspace.tenant
            && self
                .permissions
                .is_subset_of(&harness_read_view_permissions(&parent.permissions))
            && self.network_scope.is_empty()
    }

    #[must_use]
    pub fn snapshot(&self) -> AuthoritySnapshot {
        AuthoritySnapshot {
            workspace: self.workspace.clone(),
            request: PermissionRequest::from_permissions(self.permissions.iter())
                .with_network_scope(self.network_scope.clone()),
            origin: self.origin.clone(),
        }
    }
}

/// Rechte-Obergrenze einer Harness-Lesesicht: `{ReadWorkspace}` ∩ `parent`.
fn harness_read_view_permissions(parent: &PermissionSet) -> PermissionSet {
    parent.restrict(&PermissionRequest::from_permissions([
        Permission::ReadWorkspace,
    ]))
}

/// Serializable, non-authoritative display/persistence data.
///
/// A snapshot cannot become a grant by deserialization; callers must pass it to
/// [`PolicyBootstrap::reissue`] with a freshly resolved workspace binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritySnapshot {
    workspace: WorkspaceBinding,
    request: PermissionRequest,
    origin: AuthorityOrigin,
}

impl AuthoritySnapshot {
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceBinding {
        &self.workspace
    }

    #[must_use]
    pub fn request(&self) -> &PermissionRequest {
        &self.request
    }

    #[must_use]
    pub fn origin(&self) -> &AuthorityOrigin {
        &self.origin
    }
}

/// A frozen, backend-facing view of an [`AuthorityContext`].
///
/// This is deliberately not serializable.  The v1 authority core carries one
/// canonical workspace root only; extra roots require a later, policy-bound
/// extension instead of a mutable side channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    authority: AuthorityContext,
}

impl SandboxSpec {
    /// Creates a sandbox specification from a trusted runtime profile.
    ///
    /// Runtime profiles are compiled into the binary; dynamic callers must
    /// instead obtain an [`AuthorityContext`] from [`PolicyBootstrap`].
    #[must_use]
    pub fn from_resolved(workspace: WorkspaceBinding, permissions: PermissionSet) -> Self {
        Self {
            authority: AuthorityContext {
                workspace,
                permissions,
                network_scope: NetworkScope::empty(),
                origin: AuthorityOrigin::Runtime,
            },
        }
    }

    /// Wie [`Self::from_resolved`], aber mit einem code-definierten Netz-Scope
    /// (der Egress-Allowlist der Konfiguration).
    ///
    /// # Beschreibung
    /// Nur für Runtime-Einstiegsprofile gedacht, deren Profil
    /// `NetworkAccess` trägt. Der Aufrufer ist dafür verantwortlich, dass der
    /// Scope ausschließlich aus der konfigurierten Allowlist stammt; ein
    /// leerer Scope bedeutet „kein Host“ (fail-closed).
    #[must_use]
    pub fn from_resolved_with_network(
        workspace: WorkspaceBinding,
        permissions: PermissionSet,
        network_scope: NetworkScope,
    ) -> Self {
        Self {
            authority: AuthorityContext {
                workspace,
                permissions,
                network_scope,
                origin: AuthorityOrigin::Runtime,
            },
        }
    }

    #[must_use]
    pub fn from_authority(authority: AuthorityContext) -> Self {
        Self { authority }
    }

    /// Nur für Tests abhängiger Crates: baut eine Sandbox-Spezifikation mit
    /// einem beliebigen, auch nicht-leeren `NetworkScope`, hinter dem Feature
    /// `test-support` verriegelt.
    ///
    /// # Description
    /// Entspricht ansonsten genau [`Self::from_resolved`], erlaubt aber
    /// zusätzlich einen explizit übergebenen [`NetworkScope`] statt des dort
    /// fest verdrahteten `NetworkScope::empty()`. Das ist außerhalb dieses
    /// Konstruktors nicht erreichbar: `AuthorityContext::restrict` bleibt eine
    /// reine Schnittmengenoperation (leer ∩ irgendwas = leer, unverändert),
    /// und `PolicyBootstrap::issue`/`reissue` bleiben die einzigen
    /// produktiven Wege zu einem nicht-leeren Scope — beide akzeptieren
    /// ausschließlich die zwei fest verdrahteten Policy-Pfade
    /// (`PolicySource::SYSTEM_POLICY_PATH`,
    /// `PolicySource::OPERATOR_POLICY_RELATIVE_PATH`). Diese Funktion dient
    /// ausschließlich dazu, abhängigen Crates (z. B. `harw-tools`,
    /// `harw-core-bridge`) zu erlauben, in ihren eigenen Tests eine Sandbox
    /// mit einer konkreten Host-Allow-Liste aufzubauen — etwa um
    /// `harw_tools::sandbox_guard::require_host_access` oder die
    /// Authority-Reduzierer in `harw-core-bridge` gegen echte Hosts zu
    /// prüfen. `test-support` steht ausschließlich in
    /// `[dev-dependencies]`-Positionen und ist wegen `resolver = "2"` nie im
    /// Build eines produktiven Konsumenten aktiv (siehe `Cargo.toml`).
    ///
    /// # Arguments
    /// - `workspace` (`WorkspaceBinding`): reale, kanonisierte Bindung —
    ///   üblicherweise über `WorkspaceRegistry::build`/`resolve` gewonnen.
    /// - `permissions` (`PermissionSet`): die zu gewährenden Rechte.
    /// - `network_scope` (`NetworkScope`): der zu erzwingende Host-Scope,
    ///   z. B. über [`NetworkScope::from_hosts`] gebaut.
    ///
    /// # Returns
    /// Eine `SandboxSpec` mit genau den übergebenen Feldwerten und
    /// `AuthorityOrigin::TestOnly`.
    ///
    /// # Panics
    /// Nie.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn from_resolved_for_test(
        workspace: WorkspaceBinding,
        permissions: PermissionSet,
        network_scope: NetworkScope,
    ) -> Self {
        Self {
            authority: AuthorityContext {
                workspace,
                permissions,
                network_scope,
                origin: AuthorityOrigin::TestOnly,
            },
        }
    }

    /// Baut eine harness-eigene, rein lesende Sicht auf `view` als Kind von
    /// `parent`.
    ///
    /// # Beschreibung
    /// Für Verzeichnisse, die der Harness selbst befüllt und in die er nur
    /// zulässige Dateien kopiert (Unterlagen-Kopie eines Matrix-Sitzes).
    /// Das Ergebnis trägt Rechte = `{ReadWorkspace}` ∩ Parent-Rechte, einen
    /// leeren Netz-Scope und den Ursprung [`AuthorityOrigin::HarnessReadView`];
    /// nur damit besteht es [`Self::ensure_child_of`] trotz abweichendem
    /// Workspace (siehe [`AuthorityContext::is_subset_of`]).
    ///
    /// # Arguments
    /// - `parent` (`&SandboxSpec`): Sandbox des Aufrufers, Obergrenze der Sicht.
    /// - `view` (`WorkspaceBinding`): kanonisierte Bindung des Sicht-Verzeichnisses.
    ///
    /// # Returns
    /// Die Sicht-Sandbox.
    ///
    /// # Errors
    /// - [`AuthorityError::ReadViewWithoutReadWorkspace`], wenn der Parent
    ///   kein `ReadWorkspace` hält (die Sicht wäre rechtelos und nutzlos).
    /// - [`AuthorityError::ReadViewTenantMismatch`], wenn `view` zu einem
    ///   anderen Tenant gehört als der Parent-Workspace.
    pub fn harness_read_view(parent: &Self, view: WorkspaceBinding) -> AuthorityResult<Self> {
        let permissions = harness_read_view_permissions(parent.permissions());
        if !permissions.contains(Permission::ReadWorkspace) {
            return Err(AuthorityError::ReadViewWithoutReadWorkspace);
        }
        if view.tenant != parent.workspace().tenant {
            return Err(AuthorityError::ReadViewTenantMismatch);
        }
        let spec = Self {
            authority: AuthorityContext {
                workspace: view,
                permissions,
                network_scope: NetworkScope::empty(),
                origin: AuthorityOrigin::HarnessReadView,
            },
        };
        spec.ensure_child_of(parent)?;
        Ok(spec)
    }

    #[must_use]
    pub fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    #[must_use]
    pub fn workspace(&self) -> &WorkspaceBinding {
        self.authority.workspace()
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionSet {
        self.authority.permissions()
    }

    #[must_use]
    pub fn network_scope(&self) -> &NetworkScope {
        self.authority.network_scope()
    }

    #[must_use]
    pub fn restrict(&self, request: &PermissionRequest) -> Self {
        Self {
            authority: self.authority.restrict(request),
        }
    }

    pub fn ensure_child_of(&self, parent: &Self) -> AuthorityResult<()> {
        if !self.authority.is_subset_of(&parent.authority) {
            return Err(AuthorityError::ChildAuthorityEscalation);
        }
        Ok(())
    }
}

/// Parses and validates exactly one trusted policy source, then issues grants
/// only by cutting a request against that policy.
#[derive(Debug, Clone)]
pub struct PolicyBootstrap {
    source: PolicySourceKind,
    permissions: PermissionSet,
    network_scope: NetworkScope,
    policy_digest: String,
}

impl PolicyBootstrap {
    pub fn load(source: PolicySource) -> AuthorityResult<Self> {
        let path = source.path()?;
        let bytes = read_trusted_policy(source, &path)?;
        Self::from_policy_bytes(source.kind(), &bytes)
    }

    #[must_use]
    pub fn source(&self) -> PolicySourceKind {
        self.source
    }

    #[must_use]
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    /// Issues a context only when the entire request is within this bootstrap's
    /// already validated policy.  The workspace binding is part of the grant.
    pub fn issue(
        &self,
        workspace: WorkspaceBinding,
        request: &PermissionRequest,
    ) -> AuthorityResult<AuthorityContext> {
        if !request.is_within(&self.permissions, &self.network_scope) {
            return Err(AuthorityError::RequestExceedsPolicy {
                source: self.source,
            });
        }
        Ok(AuthorityContext {
            workspace,
            permissions: PermissionSet::from_validated_policy(request.iter()),
            network_scope: request.network_scope.clone(),
            origin: AuthorityOrigin::Policy {
                source: self.source,
                policy_digest: self.policy_digest.clone(),
            },
        })
    }

    /// Re-evaluates a persisted snapshot against the current trusted policy.
    /// No old grant is restored directly.
    pub fn reissue(
        &self,
        workspace: WorkspaceBinding,
        snapshot: &AuthoritySnapshot,
    ) -> AuthorityResult<AuthorityContext> {
        if workspace != snapshot.workspace {
            return Err(AuthorityError::SnapshotWorkspaceMismatch);
        }
        if snapshot.origin.source() != Some(self.source) {
            return Err(AuthorityError::SnapshotSourceMismatch {
                expected: self.source,
                actual: snapshot.origin.source(),
            });
        }
        self.issue(workspace, &snapshot.request)
    }

    fn from_policy_bytes(source: PolicySourceKind, bytes: &[u8]) -> AuthorityResult<Self> {
        let text = std::str::from_utf8(bytes).map_err(|error| AuthorityError::PolicyDecode {
            reason: error.to_string(),
        })?;
        let raw: RawPolicy =
            toml::from_str(text).map_err(|error| AuthorityError::PolicyDecode {
                reason: error.to_string(),
            })?;
        if raw.schema_version != 1 {
            return Err(AuthorityError::UnsupportedPolicySchema {
                found: raw.schema_version,
            });
        }
        Ok(Self {
            source,
            permissions: PermissionSet::from_validated_policy(raw.permissions),
            network_scope: NetworkScope::from_targets(raw.network_targets),
            policy_digest: blake3::hash(bytes).to_hex().to_string(),
        })
    }

    #[cfg(test)]
    fn from_test_toml(text: &str) -> AuthorityResult<Self> {
        Self::from_policy_bytes(PolicySourceKind::OperatorHome, text.as_bytes())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    schema_version: u32,
    #[serde(default)]
    permissions: Vec<Permission>,
    #[serde(default)]
    network_targets: Vec<EgressTarget>,
}

fn read_trusted_policy(source: PolicySource, path: &Path) -> AuthorityResult<Vec<u8>> {
    let symlink_metadata = fs::symlink_metadata(path).map_err(|error| AuthorityError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if symlink_metadata.file_type().is_symlink() {
        return Err(AuthorityError::PolicySymlink {
            path: path.to_path_buf(),
        });
    }
    let canonical = path.canonicalize().map_err(|error| AuthorityError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if canonical != path {
        return Err(AuthorityError::PolicyPathNotCanonical {
            path: path.to_path_buf(),
            canonical,
        });
    }
    #[cfg(unix)]
    if source == PolicySource::System {
        validate_system_policy_ancestors(path)?;
    }

    let mut file = fs::File::open(path).map_err(|error| AuthorityError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let metadata = file.metadata().map_err(|error| AuthorityError::Io {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    validate_policy_metadata(source, path, &metadata)?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| AuthorityError::Io {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
    Ok(bytes)
}

#[cfg(unix)]
fn validate_policy_metadata(
    source: PolicySource,
    path: &Path,
    metadata: &fs::Metadata,
) -> AuthorityResult<()> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_file() {
        return Err(AuthorityError::PolicyNotRegular {
            path: path.to_path_buf(),
        });
    }
    let mode = metadata.mode();
    if mode & 0o022 != 0 {
        return Err(AuthorityError::PolicyWritableByGroupOrWorld {
            path: path.to_path_buf(),
        });
    }
    if source == PolicySource::System && metadata.uid() != 0 {
        return Err(AuthorityError::SystemPolicyNotRootOwned {
            path: path.to_path_buf(),
            uid: metadata.uid(),
        });
    }
    Ok(())
}

/// The system policy's whole path is part of its trust boundary. A correctly
/// owned leaf below a group-writable directory is replaceable and therefore not
/// a trusted root source.
#[cfg(unix)]
fn validate_system_policy_ancestors(path: &Path) -> AuthorityResult<()> {
    use std::os::unix::fs::MetadataExt;

    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(ancestor).map_err(|error| AuthorityError::Io {
            path: ancestor.to_path_buf(),
            reason: error.to_string(),
        })?;
        let mode = metadata.mode();
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || mode & 0o022 != 0
        {
            return Err(AuthorityError::SystemPolicyAncestorUntrusted {
                path: ancestor.to_path_buf(),
                uid: metadata.uid(),
                mode,
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_policy_metadata(
    _source: PolicySource,
    path: &Path,
    metadata: &fs::Metadata,
) -> AuthorityResult<()> {
    if metadata.is_file() {
        Ok(())
    } else {
        Err(AuthorityError::PolicyNotRegular {
            path: path.to_path_buf(),
        })
    }
}

/// Errors returned by authority and scope validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityError {
    Io {
        path: PathBuf,
        reason: String,
    },
    NotDirectory {
        path: PathBuf,
    },
    WorkspaceEscapesHarness {
        path: PathBuf,
    },
    DuplicateWorkspaceBinding {
        tenant: String,
        workspace: String,
    },
    WorkspaceNotBound {
        tenant: TenantId,
        workspace: WorkspaceId,
    },
    InvalidRelativePath {
        path: PathBuf,
    },
    PathEscapesWorkspace {
        path: PathBuf,
        workspace: WorkspaceId,
    },
    ChildAuthorityEscalation,
    /// Eine Harness-Lesesicht verlangt `ReadWorkspace` beim Parent.
    ReadViewWithoutReadWorkspace,
    /// Eine Harness-Lesesicht muss zum Tenant des Parent-Workspace gehören.
    ReadViewTenantMismatch,
    OperatorHomeUnavailable,
    PolicySymlink {
        path: PathBuf,
    },
    PolicyPathNotCanonical {
        path: PathBuf,
        canonical: PathBuf,
    },
    PolicyNotRegular {
        path: PathBuf,
    },
    PolicyWritableByGroupOrWorld {
        path: PathBuf,
    },
    SystemPolicyNotRootOwned {
        path: PathBuf,
        uid: u32,
    },
    SystemPolicyAncestorUntrusted {
        path: PathBuf,
        uid: u32,
        mode: u32,
    },
    PolicyDecode {
        reason: String,
    },
    UnsupportedPolicySchema {
        found: u32,
    },
    RequestExceedsPolicy {
        source: PolicySourceKind,
    },
    SnapshotWorkspaceMismatch,
    SnapshotSourceMismatch {
        expected: PolicySourceKind,
        actual: Option<PolicySourceKind>,
    },
}

pub type AuthorityResult<T> = Result<T, AuthorityError>;

impl fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } => {
                write!(f, "authority I/O at '{}': {reason}", path.display())
            }
            Self::NotDirectory { path } => {
                write!(f, "authority path is not a directory: '{}'", path.display())
            }
            Self::WorkspaceEscapesHarness { path } => {
                write!(f, "workspace escapes harness root: '{}'", path.display())
            }
            Self::DuplicateWorkspaceBinding { tenant, workspace } => {
                write!(
                    f,
                    "duplicate workspace binding '{workspace}' for tenant '{tenant}'"
                )
            }
            Self::WorkspaceNotBound { tenant, workspace } => {
                write!(
                    f,
                    "workspace '{workspace}' is not bound to tenant '{tenant}'"
                )
            }
            Self::InvalidRelativePath { path } => {
                write!(f, "invalid workspace-relative path: '{}'", path.display())
            }
            Self::PathEscapesWorkspace { path, workspace } => {
                write!(
                    f,
                    "path '{}' escapes workspace '{workspace}'",
                    path.display()
                )
            }
            Self::ChildAuthorityEscalation => {
                write!(f, "child authority is not a subset of its parent")
            }
            Self::ReadViewWithoutReadWorkspace => {
                write!(f, "harness read view requires read_workspace on the parent")
            }
            Self::ReadViewTenantMismatch => {
                write!(f, "harness read view belongs to a different tenant")
            }
            Self::OperatorHomeUnavailable => {
                write!(f, "operator home is unavailable or not absolute")
            }
            Self::PolicySymlink { path } => {
                write!(f, "policy path must not be a symlink: '{}'", path.display())
            }
            Self::PolicyPathNotCanonical { path, canonical } => write!(
                f,
                "policy path '{}' resolves to a different path '{}'",
                path.display(),
                canonical.display()
            ),
            Self::PolicyNotRegular { path } => {
                write!(f, "policy is not a regular file: '{}'", path.display())
            }
            Self::PolicyWritableByGroupOrWorld { path } => write!(
                f,
                "policy must not be writable by group or world: '{}'",
                path.display()
            ),
            Self::SystemPolicyNotRootOwned { path, uid } => write!(
                f,
                "system policy '{}' is not root-owned (uid {uid})",
                path.display()
            ),
            Self::SystemPolicyAncestorUntrusted { path, uid, mode } => write!(
                f,
                "system policy ancestor '{}' is not a root-owned, non-group/world-writable directory (uid {uid}, mode {mode:o})",
                path.display()
            ),
            Self::PolicyDecode { reason } => {
                write!(f, "trusted authority policy is invalid: {reason}")
            }
            Self::UnsupportedPolicySchema { found } => {
                write!(f, "unsupported authority policy schema version {found}")
            }
            Self::RequestExceedsPolicy { source } => {
                write!(
                    f,
                    "permission request exceeds the trusted {source:?} policy"
                )
            }
            Self::SnapshotWorkspaceMismatch => {
                write!(f, "authority snapshot workspace does not match")
            }
            Self::SnapshotSourceMismatch { expected, actual } => write!(
                f,
                "authority snapshot source {actual:?} does not match bootstrap {expected:?}"
            ),
        }
    }
}

impl std::error::Error for AuthorityError {}

/// A grant cannot be constructed directly because its fields are private.
///
/// ```rust,compile_fail
/// use harw_authority::{Permission, PermissionSet};
/// use std::collections::BTreeSet;
///
/// let granted = BTreeSet::from([Permission::ExecuteProcess]);
/// let _ = PermissionSet { granted };
/// ```
///
/// Grants cannot be deserialized either.
///
/// ```rust,compile_fail
/// use harw_authority::PermissionSet;
///
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<PermissionSet>();
/// ```
///
/// A serializable snapshot is also intentionally not a grant.
///
/// ```rust,compile_fail
/// use harw_authority::{AuthorityContext, AuthoritySnapshot};
///
/// fn needs_grant(_: AuthorityContext) {}
/// let snapshot: AuthoritySnapshot = todo!();
/// needs_grant(snapshot);
/// ```
const _COMPILE_FAIL_INTENT: () = ();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_dns_name_rejects_literals_local_and_single_label_names() {
        for public in [
            "www.bund.de",
            "docs.rs",
            "Example.ORG.",
            "a-b.co.uk",
            "xn--bcher-kva.de",
        ] {
            assert!(is_public_dns_name(public), "{public}");
        }
        for local in [
            "",
            "localhost",
            "api.localhost",
            "printer.local",
            "svc.internal",
            "router.home.arpa",
            "intranet",
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "::1",
            "[::1]",
            "0x7f.1",
            "1.2.3",
            "a..b",
            "evil.com:80",
            "user@evil.com",
        ] {
            assert!(!is_public_dns_name(local), "{local}");
        }
    }

    #[test]
    fn public_dns_target_matches_names_but_never_addresses() {
        let open = NetworkScope::from_targets([EgressTarget::PublicDns]);
        assert!(open.allows_public_dns());
        assert!(open.allows("www.destatis.de"));
        assert!(!open.allows("localhost"));
        assert!(!open.allows("127.0.0.1"));
        assert!(!open.allows_addr(std::net::IpAddr::from([10, 0, 0, 1])));
        assert!(open.hosts().next().is_none());
    }

    #[test]
    fn public_dns_intersection_never_widens_either_side() {
        let open = NetworkScope::from_targets([EgressTarget::PublicDns]);
        let listed = NetworkScope::from_hosts(["docs.rs".to_owned(), "printer.local".to_owned()]);
        let narrowed = open.intersection(&listed);
        assert!(narrowed.allows("docs.rs"));
        assert!(!narrowed.allows("printer.local"));
        assert!(!narrowed.allows_public_dns());
        assert!(narrowed.is_subset_of(&open));
        assert!(narrowed.is_subset_of(&listed));
        assert!(
            !open.is_subset_of(&listed),
            "offen ist nie Teilmenge einer Liste"
        );
        assert!(listed.intersection(&NetworkScope::empty()).is_empty());
        assert_eq!(open.intersection(&open), open);
        let both = NetworkScope::from_targets([
            EgressTarget::PublicDns,
            EgressTarget::DnsSuffix("docs.rs".to_owned()),
        ]);
        assert!(open.is_subset_of(&both));
    }

    #[test]
    fn public_dns_target_round_trips_through_its_wire_form() {
        let open = NetworkScope::from_targets([
            EgressTarget::PublicDns,
            EgressTarget::DnsSuffix("docs.rs".to_owned()),
        ]);
        let encoded = toml::to_string(&open).unwrap_or_default();
        assert!(encoded.contains(PUBLIC_DNS_WIRE), "{encoded}");
        let back: NetworkScope = toml::from_str(&encoded).unwrap_or_default();
        assert_eq!(back, open);
    }

    #[test]
    fn every_permission_request_restricts_every_parent_set() {
        for parent_mask in 0_u8..(1 << Permission::ALL.len()) {
            let parent = PermissionSet::from_test_mask(parent_mask);
            for request_mask in 0_u8..(1 << Permission::ALL.len()) {
                let request = PermissionRequest::from_permissions(mask_permissions(request_mask));
                let child = parent.restrict(&request);

                assert!(child.is_subset_of(&parent));
                for (index, permission) in Permission::ALL.iter().copied().enumerate() {
                    let expected =
                        (parent_mask & (1 << index)) != 0 && (request_mask & (1 << index)) != 0;
                    assert_eq!(child.contains(permission), expected);
                }
            }
        }
    }

    #[test]
    fn a_request_is_not_a_grant() {
        let request = PermissionRequest::from_permissions([Permission::ExecuteProcess]);
        assert!(request.contains(Permission::ExecuteProcess));
        assert!(!PermissionSet::empty().contains(Permission::ExecuteProcess));
    }

    #[test]
    fn bootstrap_rejects_requests_outside_its_validated_policy() -> test_support::TestResult {
        let bootstrap = PolicyBootstrap::from_test_toml(
            r#"
schema_version = 1
permissions = ["read_workspace"]
network_targets = ["docs.rs"]
"#,
        )
        .map_err(test_support::ctx("test policy is structurally valid"))?;
        let workspace = test_workspace();

        let request = PermissionRequest::from_permissions([
            Permission::ReadWorkspace,
            Permission::ExecuteProcess,
        ]);
        assert!(matches!(
            bootstrap.issue(workspace, &request),
            Err(AuthorityError::RequestExceedsPolicy { .. })
        ));
        Ok(())
    }

    #[test]
    fn snapshot_requires_re_evaluation_and_cannot_restore_old_policy_rights()
    -> test_support::TestResult {
        let permissive = PolicyBootstrap::from_test_toml(
            r#"
schema_version = 1
permissions = ["read_workspace", "execute_process"]
network_targets = []
"#,
        )
        .map_err(test_support::ctx("test policy is structurally valid"))?;
        let context = permissive
            .issue(
                test_workspace(),
                &PermissionRequest::from_permissions([
                    Permission::ReadWorkspace,
                    Permission::ExecuteProcess,
                ]),
            )
            .map_err(test_support::ctx("policy grants request"))?;
        let snapshot = context.snapshot();

        let restrictive = PolicyBootstrap::from_test_toml(
            r#"
schema_version = 1
permissions = ["read_workspace"]
network_targets = []
"#,
        )
        .map_err(test_support::ctx("test policy is structurally valid"))?;
        assert!(matches!(
            restrictive.reissue(test_workspace(), &snapshot),
            Err(AuthorityError::RequestExceedsPolicy { .. })
        ));
        Ok(())
    }

    #[test]
    fn network_and_workspace_scopes_only_reduce() {
        let parent = test_context(
            [Permission::ReadWorkspace, Permission::NetworkAccess],
            NetworkScope::from_hosts(["docs.rs".to_owned(), "crates.io".to_owned()]),
        );
        let child = parent.restrict(
            &PermissionRequest::from_permissions([Permission::NetworkAccess]).with_network_scope(
                NetworkScope::from_hosts(["docs.rs".to_owned(), "evil.example".to_owned()]),
            ),
        );

        assert!(child.is_subset_of(&parent));
        assert!(child.network_scope().allows("api.docs.rs"));
        assert!(!child.network_scope().allows("evil.example"));
        assert!(!child.permissions().contains(Permission::ReadWorkspace));
    }

    fn view_workspace() -> WorkspaceBinding {
        WorkspaceBinding {
            tenant: harw_types::TenantId::from_str("test-tenant"),
            workspace: harw_types::WorkspaceId::from_str("matrix-run-seat"),
            canonical_root: std::env::temp_dir().join("materials").join("seat"),
        }
    }

    fn sandbox_with(
        workspace: WorkspaceBinding,
        permissions: impl IntoIterator<Item = Permission>,
        network_scope: NetworkScope,
        origin: AuthorityOrigin,
    ) -> SandboxSpec {
        SandboxSpec {
            authority: AuthorityContext {
                workspace,
                permissions: PermissionSet::from_test_permissions(permissions),
                network_scope,
                origin,
            },
        }
    }

    fn full_parent() -> SandboxSpec {
        sandbox_with(
            test_workspace(),
            Permission::ALL,
            NetworkScope::from_hosts(["docs.rs".to_owned()]),
            AuthorityOrigin::TestOnly,
        )
    }

    #[test]
    fn harness_read_view_is_read_only_child_of_parent() -> test_support::TestResult {
        let parent = full_parent();
        let view = SandboxSpec::harness_read_view(&parent, view_workspace())?;

        assert_eq!(view.workspace(), &view_workspace());
        assert_eq!(
            view.permissions(),
            &PermissionSet::from_test_permissions([Permission::ReadWorkspace])
        );
        assert!(view.network_scope().is_empty());
        assert_eq!(view.authority().origin(), &AuthorityOrigin::HarnessReadView);
        view.ensure_child_of(&parent)?;
        // Eine weitere Einschränkung der Sicht bleibt Kind der Sicht.
        view.restrict(&PermissionRequest::empty())
            .ensure_child_of(&view)?;
        Ok(())
    }

    #[test]
    fn harness_read_view_rejects_parent_without_read_workspace() {
        let parent = sandbox_with(
            test_workspace(),
            [Permission::WriteWorkspace, Permission::ExecuteProcess],
            NetworkScope::empty(),
            AuthorityOrigin::TestOnly,
        );
        assert_eq!(
            SandboxSpec::harness_read_view(&parent, view_workspace()),
            Err(AuthorityError::ReadViewWithoutReadWorkspace)
        );
        // Auch ein von Hand gebauter Sicht-Kontext besteht nicht.
        let forged = sandbox_with(
            view_workspace(),
            [Permission::ReadWorkspace],
            NetworkScope::empty(),
            AuthorityOrigin::HarnessReadView,
        );
        assert_eq!(
            forged.ensure_child_of(&parent),
            Err(AuthorityError::ChildAuthorityEscalation)
        );
    }

    #[test]
    fn harness_read_view_rejects_other_tenant() {
        let other = WorkspaceBinding {
            tenant: harw_types::TenantId::from_str("other-tenant"),
            ..view_workspace()
        };
        assert_eq!(
            SandboxSpec::harness_read_view(&full_parent(), other.clone()),
            Err(AuthorityError::ReadViewTenantMismatch)
        );
        let forged = sandbox_with(
            other,
            [Permission::ReadWorkspace],
            NetworkScope::empty(),
            AuthorityOrigin::HarnessReadView,
        );
        assert!(forged.ensure_child_of(&full_parent()).is_err());
    }

    #[test]
    fn harness_read_view_with_more_than_read_is_rejected() {
        let parent = full_parent();
        for extra in [
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ] {
            let child = sandbox_with(
                view_workspace(),
                [Permission::ReadWorkspace, extra],
                NetworkScope::empty(),
                AuthorityOrigin::HarnessReadView,
            );
            assert_eq!(
                child.ensure_child_of(&parent),
                Err(AuthorityError::ChildAuthorityEscalation),
                "{extra:?}"
            );
        }
    }

    #[test]
    fn harness_read_view_with_network_scope_is_rejected() {
        let child = sandbox_with(
            view_workspace(),
            [Permission::ReadWorkspace],
            NetworkScope::from_hosts(["docs.rs".to_owned()]),
            AuthorityOrigin::HarnessReadView,
        );
        assert_eq!(
            child.ensure_child_of(&full_parent()),
            Err(AuthorityError::ChildAuthorityEscalation)
        );
    }

    #[test]
    fn ordinary_child_with_other_workspace_is_still_rejected() {
        let parent = full_parent();
        for origin in [AuthorityOrigin::Runtime, AuthorityOrigin::TestOnly] {
            let child = sandbox_with(
                view_workspace(),
                [Permission::ReadWorkspace],
                NetworkScope::empty(),
                origin.clone(),
            );
            assert_eq!(
                child.ensure_child_of(&parent),
                Err(AuthorityError::ChildAuthorityEscalation),
                "{origin:?}"
            );
        }
        let rights_free = sandbox_with(
            view_workspace(),
            std::iter::empty(),
            NetworkScope::empty(),
            AuthorityOrigin::Runtime,
        );
        assert!(rights_free.ensure_child_of(&parent).is_err());
    }

    fn mask_permissions(mask: u8) -> impl Iterator<Item = Permission> {
        Permission::ALL
            .iter()
            .copied()
            .enumerate()
            .filter_map(move |(index, permission)| {
                ((mask & (1 << index)) != 0).then_some(permission)
            })
    }

    fn test_workspace() -> WorkspaceBinding {
        WorkspaceBinding {
            tenant: harw_types::TenantId::from_str("test-tenant"),
            workspace: harw_types::WorkspaceId::from_str("test-workspace"),
            canonical_root: std::env::temp_dir(),
        }
    }

    fn test_context(
        permissions: impl IntoIterator<Item = Permission>,
        network_scope: NetworkScope,
    ) -> AuthorityContext {
        AuthorityContext {
            workspace: test_workspace(),
            permissions: PermissionSet::from_test_permissions(permissions),
            network_scope,
            origin: AuthorityOrigin::TestOnly,
        }
    }
}

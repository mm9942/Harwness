//! Identity mapping: `AuthenticatedPeer` -> host `ClientIdentity` (S08).
//!
//! Never from the wire payload (W00 D4, ID-02, ID-04, ID-05): the mapper
//! reads only the already-authenticated node id and the device registry. The
//! tenant and the cap ceiling are host-fixed per device; remote identities
//! always carry a tenant (`ClientIdentity::validate` refuses otherwise).
//!
//! A valid node key proves *who* connects, never *what they may do*: the
//! [`PermissionTier`] comes only from the host-written registry record of
//! that device. A key without a record, with a revoked record, with an
//! ambiguous record or with an unreadable record is [`ListenerError::UnknownPeer`]
//! (default deny, nothing is guessed).
//!
//! # Registry format
//!
//! `<state_dir>/node-devices.conf`, one record per line, `#` starts a comment:
//!
//! ```text
//! node_id|device_id|tenant|tier|status|label
//! ```
//!
//! `tier` is `observer|operator|maintainer|owner`, `status` is `active` or
//! `revoked`. Any line that does not parse is ignored (never repaired into a
//! grant). The file is re-read on every mapping, so a revocation written by
//! [`DeviceRegistry::mark_revoked`] takes effect for the next handshake
//! without a restart.

use std::fs;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use harw_node_transport::AuthenticatedPeer;
use harw_session_host::{ClientIdentity, ConnectionId, caps_for_tier, gateway_caps_for_tier};
use harw_types::{
    ApprovalActor, AuthStrength, DeviceId, IngressSurface, NodeId, PermissionTier, Principal,
    PrincipalKind, TenantId, TrustZone,
};

use crate::ListenerError;

/// File name of the device registry inside the state directory.
pub const REGISTRY_FILE: &str = "node-devices.conf";

/// Boxed future of one mapping.
pub type MapFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ClientIdentity, ListenerError>> + Send + 'a>>;

/// Maps an authenticated node peer to the identity the host admits against.
pub trait IdentityMapper: Send + Sync {
    /// Resolve `peer` for the new `connection`. Unknown, revoked or
    /// unenrolled peers fail with [`ListenerError::UnknownPeer`] (default
    /// deny); the result's tenant, caps, device and actor are host-derived.
    fn map(&self, peer: &AuthenticatedPeer, connection: ConnectionId) -> MapFuture<'_>;
}

/// One enrolled device of the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRecord {
    /// Node key identity the device authenticates with.
    pub node_id: NodeId,
    /// Enrolled device id.
    pub device: DeviceId,
    /// Host-fixed tenant.
    pub tenant: TenantId,
    /// Host-fixed permission tier (the cap ceiling derives from it).
    pub tier: PermissionTier,
    /// Revoked records never map.
    pub revoked: bool,
    /// Display label for presence.
    pub label: String,
    /// Per-device opt-in for `approval.respond` from a remote device
    /// (PL-68 §13). Off unless the registry line says `approve`; a tier alone
    /// never grants it.
    pub approve_optin: bool,
}

impl DeviceRecord {
    /// Parse one registry line; `None` when blank, a comment or malformed.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let (node, device, tenant, tier, status, label, approve_optin) = match fields.as_slice() {
            [node, device, tenant, tier, status, label] => {
                (node, device, tenant, tier, status, label, false)
            }
            // The only accepted seventh field is the literal opt-in marker.
            [node, device, tenant, tier, status, label, "approve"] => {
                (node, device, tenant, tier, status, label, true)
            }
            _ => return None,
        };
        let tier = match *tier {
            "observer" => PermissionTier::Observer,
            "operator" => PermissionTier::Operator,
            "maintainer" => PermissionTier::Maintainer,
            "owner" => PermissionTier::Owner,
            _ => return None,
        };
        let revoked = match *status {
            "active" => false,
            "revoked" => true,
            _ => return None,
        };
        Some(Self {
            node_id: NodeId::try_from_str(*node).ok()?,
            device: DeviceId::try_from_str(*device).ok()?,
            tenant: TenantId::try_from_str(*tenant).ok()?,
            tier,
            revoked,
            label: (*label).to_owned(),
            approve_optin,
        })
    }

    /// The registry line of this record.
    #[must_use]
    pub fn render(&self) -> String {
        let tier = match self.tier {
            PermissionTier::Observer => "observer",
            PermissionTier::Operator => "operator",
            PermissionTier::Maintainer => "maintainer",
            PermissionTier::Owner => "owner",
        };
        format!(
            "{}|{}|{}|{tier}|{}|{}{}",
            self.node_id.as_str(),
            self.device.as_str(),
            self.tenant.as_str(),
            if self.revoked { "revoked" } else { "active" },
            self.label,
            if self.approve_optin { "|approve" } else { "" }
        )
    }
}

/// The device registry file under a state directory.
#[derive(Clone, Debug)]
pub struct DeviceRegistry {
    path: PathBuf,
}

impl DeviceRegistry {
    /// Registry of `state_dir`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(REGISTRY_FILE),
        }
    }

    /// Append `record` (host-side enrollment; not reachable from the wire).
    ///
    /// # Errors
    /// [`ListenerError::Io`] when the file cannot be written.
    pub fn enroll(&self, record: &DeviceRecord) -> Result<(), ListenerError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io)?;
        writeln!(file, "{}", record.render()).map_err(io)
    }

    /// Parsed records. A missing file is an empty registry.
    ///
    /// # Errors
    /// [`ListenerError::Io`] when the file exists but cannot be read.
    pub fn records(&self) -> Result<Vec<DeviceRecord>, ListenerError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(text.lines().filter_map(DeviceRecord::parse).collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(io(error)),
        }
    }

    /// The single record of `node`. `None` when there is no record or more
    /// than one (ambiguity fails closed).
    ///
    /// # Errors
    /// [`ListenerError::Io`] when the registry cannot be read.
    pub fn record_for(&self, node: &NodeId) -> Result<Option<DeviceRecord>, ListenerError> {
        let mut matching = self
            .records()?
            .into_iter()
            .filter(|record| &record.node_id == node);
        let first = matching.next();
        Ok(match (first, matching.next()) {
            (Some(record), None) => Some(record),
            _ => None,
        })
    }

    /// Mark every record of `device` revoked, keeping all other lines
    /// byte-for-byte. Returns how many records changed.
    ///
    /// # Errors
    /// [`ListenerError::Io`] when the registry cannot be read or rewritten.
    pub fn mark_revoked(&self, device: &DeviceId) -> Result<usize, ListenerError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io(error)),
        };
        let mut changed = 0;
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            match DeviceRecord::parse(line) {
                Some(mut record) if &record.device == device && !record.revoked => {
                    record.revoked = true;
                    changed += 1;
                    out.push_str(&record.render());
                }
                _ => out.push_str(line),
            }
            out.push('\n');
        }
        if changed > 0 {
            let tmp = self.path.with_extension("conf.tmp");
            fs::write(&tmp, out).map_err(io)?;
            fs::rename(&tmp, &self.path).map_err(io)?;
        }
        Ok(changed)
    }
}

fn io(error: std::io::Error) -> ListenerError {
    ListenerError::Io(error.to_string())
}

/// Mapper backed by the machine's device registry.
#[derive(Debug)]
pub struct RegistryIdentityMapper {
    registry: DeviceRegistry,
}

impl RegistryIdentityMapper {
    /// Mapper reading the device registry under `state_dir`.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            registry: DeviceRegistry::new(state_dir),
        }
    }

    fn resolve(
        &self,
        peer: &AuthenticatedPeer,
        connection: ConnectionId,
    ) -> Result<ClientIdentity, ListenerError> {
        // Unreadable registry: deny, do not surface the path or the cause.
        let record = match self.registry.record_for(&peer.node_id) {
            Ok(Some(record)) => record,
            Ok(None) => return Err(ListenerError::UnknownPeer),
            Err(error) => {
                tracing::warn!(%error, "device registry unreadable; refusing peer");
                return Err(ListenerError::UnknownPeer);
            }
        };
        if record.revoked {
            return Err(ListenerError::UnknownPeer);
        }
        Ok(identity_of(&record, connection))
    }
}

/// The host-derived identity of an unrevoked record. Every field comes from
/// the record or from constants of the remote profile.
fn identity_of(record: &DeviceRecord, connection: ConnectionId) -> ClientIdentity {
    let id = format!("device:{}", record.device.as_str());
    let mut caps = caps_for_tier(record.tier).with(gateway_caps_for_tier(record.tier));
    // A tier is not a key authorization: remote approval needs the explicit
    // per-device opt-in, whatever the tier says.
    caps.approve = caps.approve && record.approve_optin;
    ClientIdentity {
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            id.clone(),
            IngressSurface::Gateway,
            record.tier,
        ),
        tenant: Some(record.tenant.clone()),
        caps,
        device: Some(record.device.clone()),
        actor: ApprovalActor::Operator { id },
        label: record.label.clone(),
        zone: TrustZone::Remote,
        strength: AuthStrength::MutualTls,
        connection,
        agent: None,
    }
}

impl IdentityMapper for RegistryIdentityMapper {
    fn map(&self, peer: &AuthenticatedPeer, connection: ConnectionId) -> MapFuture<'_> {
        let result = self.resolve(peer, connection);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_lines_never_parse_into_a_grant() {
        for line in [
            "",
            "# comment",
            "n|d|t|owner|active",
            "n|d|t|root|active|x",
            "n|d|t|owner|maybe|x",
            "n|d||owner|active|x",
        ] {
            assert!(DeviceRecord::parse(line).is_none(), "{line}");
        }
    }

    #[test]
    fn record_round_trips() {
        let line = "node-a|dev-1|acme|operator|active|phone";
        let parsed = DeviceRecord::parse(line);
        assert_eq!(
            parsed.as_ref().map(DeviceRecord::render).as_deref(),
            Some(line)
        );
    }
}

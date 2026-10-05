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
            write_atomic(&self.path, &out).map_err(io)?;
        }
        Ok(changed)
    }

    /// Path of the registry file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Like [`Self::records`], but also reports every line the parser
    /// discards (neither blank, nor a comment, nor a valid record) with its
    /// 1-based line number. The loading rules do not change: such a line
    /// still grants nothing; this only makes the silent drop visible.
    ///
    /// # Errors
    /// [`ListenerError::Io`] when the file exists but cannot be read.
    pub fn scan(&self) -> Result<RegistryScan, ListenerError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RegistryScan::default());
            }
            Err(error) => return Err(io(error)),
        };
        let mut scan = RegistryScan::default();
        for (index, line) in text.lines().enumerate() {
            if let Some(record) = DeviceRecord::parse(line) {
                scan.records.push(record);
            } else {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    scan.rejected.push(RejectedLine {
                        line: index + 1,
                        text: trimmed.to_owned(),
                    });
                }
            }
        }
        Ok(scan)
    }

    /// Set the tier of the one record of `device`, rewriting only that line
    /// (all other lines, comments and line endings stay byte-for-byte) via a
    /// temporary file and `rename` (mode 0600 on Unix). Nothing is written
    /// when the device is unknown, ambiguous (several records), revoked, or
    /// already holds `tier`.
    ///
    /// # Errors
    /// [`TierChangeError`] as described above or on I/O failure.
    pub fn set_tier(
        &self,
        device: &DeviceId,
        tier: PermissionTier,
    ) -> Result<TierChange, TierChangeError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(TierChangeError::UnknownDevice);
            }
            Err(error) => return Err(TierChangeError::Io(error.to_string())),
        };
        let mut hits: Vec<(usize, DeviceRecord)> = Vec::new();
        for (index, line) in text.split_inclusive('\n').enumerate() {
            if let Some(record) = DeviceRecord::parse(line) {
                if &record.device == device {
                    hits.push((index, record));
                }
            }
        }
        let (index, before) = match hits.len() {
            0 => return Err(TierChangeError::UnknownDevice),
            1 => hits.remove(0),
            count => return Err(TierChangeError::Ambiguous { count }),
        };
        if before.revoked {
            return Err(TierChangeError::Revoked);
        }
        let mut after = before.clone();
        after.tier = tier;
        if before.tier == tier {
            return Ok(TierChange {
                before,
                after,
                written: false,
            });
        }
        let mut out = String::with_capacity(text.len() + 16);
        for (i, raw) in text.split_inclusive('\n').enumerate() {
            if i == index {
                out.push_str(&after.render());
                // Keep this line's own ending (`\n`, `\r\n` or none).
                out.push_str(&raw[raw.trim_end_matches(['\n', '\r']).len()..]);
            } else {
                out.push_str(raw);
            }
        }
        write_atomic(&self.path, &out).map_err(|error| TierChangeError::Io(error.to_string()))?;
        Ok(TierChange {
            before,
            after,
            written: true,
        })
    }
}

/// One line of the registry file the parser discards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedLine {
    /// 1-based line number in the file.
    pub line: usize,
    /// The trimmed line text.
    pub text: String,
}

/// Parsed records plus the lines the parser discarded
/// ([`DeviceRegistry::scan`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryScan {
    /// Records exactly as [`DeviceRegistry::records`] returns them.
    pub records: Vec<DeviceRecord>,
    /// Non-blank, non-comment lines that did not parse.
    pub rejected: Vec<RejectedLine>,
}

/// Result of [`DeviceRegistry::set_tier`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierChange {
    /// The record before the change.
    pub before: DeviceRecord,
    /// The record after the change.
    pub after: DeviceRecord,
    /// `false` when the tier already matched and nothing was written.
    pub written: bool,
}

/// Why [`DeviceRegistry::set_tier`] changed nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TierChangeError {
    /// No record of that device.
    #[error("no record for this device")]
    UnknownDevice,
    /// Several records of that device; nothing is guessed.
    #[error("{count} records match this device")]
    Ambiguous {
        /// Number of matching records.
        count: usize,
    },
    /// The record is revoked.
    #[error("the record is revoked")]
    Revoked,
    /// The registry could not be read or written.
    #[error("io: {0}")]
    Io(String),
}

/// Write `contents` to `path` through a sibling temporary file and `rename`,
/// so a reader sees the old or the new file, never a mix. The temporary file
/// is created exclusively with mode 0600 on Unix and removed on failure.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("conf.tmp");
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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

#[cfg(test)]
mod registry_tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const FILE: &str = "# my devices\n\
        node-a|dev-1|acme|observer|active|phone\n\
        \n\
        garbage line\n\
        node-b|dev-2|acme|operator|active|laptop|approve\n\
        # trailing comment\n";

    fn registry(text: &str) -> TestResult<(tempfile::TempDir, DeviceRegistry)> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join(REGISTRY_FILE), text)?;
        let registry = DeviceRegistry::new(dir.path());
        Ok((dir, registry))
    }

    fn device(raw: &str) -> TestResult<DeviceId> {
        Ok(DeviceId::try_from_str(raw)?)
    }

    #[test]
    fn scan_reports_discarded_lines_with_their_numbers() -> TestResult {
        let (_dir, registry) = registry(FILE)?;
        let scan = registry.scan()?;
        assert_eq!(scan.records.len(), 2);
        assert_eq!(
            scan.rejected,
            vec![RejectedLine {
                line: 4,
                text: "garbage line".to_owned()
            }]
        );
        // The loader itself is unchanged: same two records, nothing repaired.
        assert_eq!(registry.records()?, scan.records);
        Ok(())
    }

    #[test]
    fn scan_of_a_missing_file_is_empty() -> TestResult {
        let dir = tempfile::tempdir()?;
        let scan = DeviceRegistry::new(dir.path()).scan()?;
        assert_eq!(scan, RegistryScan::default());
        Ok(())
    }

    #[test]
    fn set_tier_changes_only_the_target_line() -> TestResult {
        let (_dir, registry) = registry(FILE)?;
        let change = registry.set_tier(&device("dev-1")?, PermissionTier::Maintainer)?;
        assert!(change.written);
        assert_eq!(change.before.tier, PermissionTier::Observer);
        assert_eq!(change.after.tier, PermissionTier::Maintainer);
        let expected = FILE.replace(
            "node-a|dev-1|acme|observer|active|phone",
            "node-a|dev-1|acme|maintainer|active|phone",
        );
        assert_eq!(std::fs::read_to_string(registry.path())?, expected);
        Ok(())
    }

    #[test]
    fn set_tier_keeps_the_approve_optin_and_crlf_endings() -> TestResult {
        let (_dir, registry) =
            registry("n|d|t|operator|active|x|approve\r\nn2|d2|t|observer|active|y\r\n")?;
        registry.set_tier(&device("d")?, PermissionTier::Observer)?;
        assert_eq!(
            std::fs::read_to_string(registry.path())?,
            "n|d|t|observer|active|x|approve\r\nn2|d2|t|observer|active|y\r\n"
        );
        Ok(())
    }

    #[test]
    fn set_tier_refuses_unknown_ambiguous_and_revoked_without_writing() -> TestResult {
        let text = "n1|dup|t|observer|active|a\nn2|dup|t|observer|active|b\nn3|gone|t|observer|revoked|c\n";
        let (_dir, registry) = registry(text)?;
        assert_eq!(
            registry.set_tier(&device("nope")?, PermissionTier::Owner),
            Err(TierChangeError::UnknownDevice)
        );
        assert_eq!(
            registry.set_tier(&device("dup")?, PermissionTier::Owner),
            Err(TierChangeError::Ambiguous { count: 2 })
        );
        assert_eq!(
            registry.set_tier(&device("gone")?, PermissionTier::Owner),
            Err(TierChangeError::Revoked)
        );
        assert_eq!(std::fs::read_to_string(registry.path())?, text);
        Ok(())
    }

    #[test]
    fn set_tier_to_the_same_tier_writes_nothing() -> TestResult {
        let (_dir, registry) = registry(FILE)?;
        let change = registry.set_tier(&device("dev-1")?, PermissionTier::Observer)?;
        assert!(!change.written);
        assert_eq!(std::fs::read_to_string(registry.path())?, FILE);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn writes_are_atomic_and_mode_0600() -> TestResult {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let (dir, registry) = registry(FILE)?;
        // A world-readable file must come out owner-only after a rewrite.
        std::fs::set_permissions(registry.path(), std::fs::Permissions::from_mode(0o644))?;
        let inode_before = std::fs::metadata(registry.path())?.ino();
        registry.set_tier(&device("dev-1")?, PermissionTier::Operator)?;
        let meta = std::fs::metadata(registry.path())?;
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_ne!(
            meta.ino(),
            inode_before,
            "replaced by rename, not rewritten in place"
        );
        assert!(!dir.path().join("node-devices.conf.tmp").exists());

        // Same for revoke.
        std::fs::set_permissions(registry.path(), std::fs::Permissions::from_mode(0o644))?;
        assert_eq!(registry.mark_revoked(&device("dev-2")?)?, 1);
        assert_eq!(
            std::fs::metadata(registry.path())?.permissions().mode() & 0o777,
            0o600
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_original_untouched() -> TestResult {
        let (dir, registry) = registry(FILE)?;
        // A directory squatting on the temporary name makes the write fail.
        std::fs::create_dir(dir.path().join("node-devices.conf.tmp"))?;
        let result = registry.set_tier(&device("dev-1")?, PermissionTier::Owner);
        assert!(matches!(result, Err(TierChangeError::Io(_))), "{result:?}");
        assert_eq!(std::fs::read_to_string(registry.path())?, FILE);
        Ok(())
    }

    #[test]
    fn revoke_is_idempotent_and_reaches_record_for() -> TestResult {
        let (_dir, registry) = registry(FILE)?;
        let node = NodeId::try_from_str("node-b")?;
        assert_eq!(registry.record_for(&node)?.map(|r| r.revoked), Some(false));
        assert_eq!(registry.mark_revoked(&device("dev-2")?)?, 1);
        assert_eq!(registry.mark_revoked(&device("dev-2")?)?, 0);
        assert_eq!(registry.record_for(&node)?.map(|r| r.revoked), Some(true));
        // Round trip: the file written by the library still parses; the
        // untouched garbage line is still reported, comments survive.
        let scan = registry.scan()?;
        assert_eq!(scan.records.len(), 2);
        assert_eq!(scan.rejected.len(), 1);
        assert!(std::fs::read_to_string(registry.path())?.contains("# my devices"));
        Ok(())
    }
}

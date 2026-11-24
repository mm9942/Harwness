//! Local evidence of one fresh hosted record's confirmed durable creation.
//!
//! This is not placement, an observation capability, or evidence that a
//! previous writer stopped. No network handler accepts inputs here.

use std::num::NonZeroU64;
use std::path::Path;
#[cfg(feature = "owner-creation-receipts")]
use std::sync::Arc;

use harw_types::{NodeId, SessionId, TenantId};
use serde::{Deserialize, Serialize};

use crate::{HostError, record::{HostedSessionRecord, RecordStore}};

const DOMAIN: &[u8] = b"HARW-OWNER-CREATION-V1\0";
const DIRECTORY: &str = "owner-creation-receipts-v1";
/// Maximum private receipt envelope size, in bytes.
pub const MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES: usize = 16 * 1024;
const MAX_RECEIPT_BYTES: u64 = MAX_OWNER_CREATION_RECEIPT_WIRE_BYTES as u64;
pub const ML_DSA_65_PUBLIC_KEY_LEN: usize = 1952;
pub const ML_DSA_65_SIGNATURE_LEN: usize = 3309;

fn unavailable() -> HostError {
    HostError::Denied("owner creation evidence is unavailable".into())
}

fn valid_topology_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
        })
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.starts_with('.')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
        })
}

#[must_use]
pub fn owner_creation_key_fingerprint(public_key: &[u8]) -> Option<[u8; 32]> {
    (public_key.len() == ML_DSA_65_PUBLIC_KEY_LEN && public_key.iter().any(|&b| b != 0))
        .then(|| *blake3::hash(public_key).as_bytes())
}

/// Exact producer and record identity, never supplied by the remote creation RPC.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerCreationClaims {
    pub node: NodeId,
    pub instance: String,
    pub tenant: TenantId,
    pub session: SessionId,
    /// Actual local host epoch, independent of any Hub generation.
    pub epoch: NonZeroU64,
    pub namespace_id: [u8; 32],
    pub key_fingerprint: [u8; 32],
}

impl OwnerCreationClaims {
    /// Domain-separated canonical ML-DSA-65 input, never JSON serialization.
    ///
    /// # Errors
    /// Refuses malformed or overlong strings and zero identity pins.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, HostError> {
        let fields = [
            self.node.as_str(), &self.instance, self.tenant.as_str(), self.session.as_str(),
        ];
        if !fields[..3].iter().all(|value| valid_topology_id(value))
            || !valid_session_id(fields[3]) || self.namespace_id == [0; 32]
            || self.key_fingerprint == [0; 32]
        {
            return Err(unavailable());
        }
        let mut payload = Vec::from(DOMAIN);
        for field in fields {
            let len = u16::try_from(field.len()).map_err(|_| unavailable())?;
            payload.extend_from_slice(&len.to_be_bytes());
            payload.extend_from_slice(field.as_bytes());
        }
        payload.extend_from_slice(&self.epoch.get().to_be_bytes());
        payload.extend_from_slice(&self.namespace_id);
        payload.extend_from_slice(&self.key_fingerprint);
        Ok(payload)
    }
}

/// Private detached creation evidence. Deserialization is not verification.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerCreationReceipt {
    claims: OwnerCreationClaims,
    signature: Vec<u8>,
}

impl OwnerCreationReceipt {
    #[must_use]
    pub fn claims(&self) -> &OwnerCreationClaims {
        &self.claims
    }

    #[must_use]
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    fn agrees_with(&self, record: &HostedSessionRecord) -> bool {
        self.signature.len() == ML_DSA_65_SIGNATURE_LEN
            && self.claims.signing_bytes().is_ok()
            && record.fresh_human_origin && record.owner_agent.is_none()
            && record.session_id == self.claims.session
            && record.tenant.as_ref() == Some(&self.claims.tenant)
            && record.host_epoch == self.claims.epoch.get()
            && !record.state.is_terminal()
    }
}

/// An independently provisioned creation-only signing key plus its exact
/// operator-selected producer scope. No raw or general signing method.
#[cfg(feature = "owner-creation-receipts")]
pub struct OwnerCreationAuthority {
    node: NodeId,
    instance: String,
    namespace_id: [u8; 32],
    key_fingerprint: [u8; 32],
    key: aws_lc_rs::signature::PqdsaKeyPair,
}

#[cfg(feature = "owner-creation-receipts")]
impl OwnerCreationAuthority {
    /// Import an operator-supplied dedicated seed and independent public pin.
    /// The caller remains responsible for zeroizing the original seed.
    ///
    /// # Errors
    /// Refuses invalid producer scope, malformed markers, or a key mismatch.
    pub fn from_seed(
        node: NodeId,
        instance: String,
        namespace_id: [u8; 32],
        seed: &[u8; 32],
        expected_public_key: &[u8],
    ) -> Result<Self, HostError> {
        use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};

        if !valid_topology_id(node.as_str()) || !valid_topology_id(&instance)
            || namespace_id == [0; 32] || *seed == [0; 32]
        {
            return Err(unavailable());
        }
        let key_fingerprint = owner_creation_key_fingerprint(expected_public_key)
            .ok_or_else(unavailable)?;
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, seed).map_err(|_| unavailable())?;
        if key.public_key().as_ref() != expected_public_key {
            return Err(unavailable());
        }
        Ok(Self { node, instance, namespace_id, key_fingerprint, key })
    }

    fn matches(&self, claims: &OwnerCreationClaims) -> bool {
        self.node == claims.node && self.instance == claims.instance
            && self.namespace_id == claims.namespace_id
            && self.key_fingerprint == claims.key_fingerprint
    }
}

#[cfg(feature = "owner-creation-receipts")]
pub(crate) struct InstalledOwnerCreation {
    pub(crate) gate: tokio::sync::Mutex<()>,
    authority: OwnerCreationAuthority,
    guard_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
}

#[cfg(feature = "owner-creation-receipts")]
impl InstalledOwnerCreation {
    pub(crate) fn new(
        authority: OwnerCreationAuthority,
        guard_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, HostError> {
        if !guard_is_live() { return Err(unavailable()); }
        Ok(Self { gate: tokio::sync::Mutex::new(()), authority, guard_is_live })
    }

    pub(crate) fn check_live(&self) -> Result<(), HostError> {
        (self.guard_is_live)().then_some(()).ok_or_else(unavailable)
    }

    pub(crate) fn verifies(&self, receipt: &OwnerCreationReceipt) -> bool {
        use aws_lc_rs::signature::{KeyPair, ML_DSA_65, UnparsedPublicKey};
        self.authority.matches(receipt.claims())
            && receipt.claims.signing_bytes().is_ok_and(|payload| {
                UnparsedPublicKey::new(&ML_DSA_65, self.authority.key.public_key().as_ref())
                    .verify(&payload, receipt.signature()).is_ok()
            })
    }

    /// Only called by the host immediately after confirmed publication. The
    /// wire caller cannot supply these fields or invoke this signer directly.
    pub(crate) fn persist_committed(
        &self, records: &RecordStore, record: &HostedSessionRecord, epoch: u64,
    ) -> Result<(), HostError> {
        use std::fs::{File, TryLockError};
        use std::os::fd::AsFd;
        use std::os::unix::fs::MetadataExt;

        self.check_live()?;
        if !record.fresh_human_origin || record.owner_agent.is_some() || record.generation != 0
            || record.host_epoch != epoch || record.state.is_terminal()
            || records.load_for_owner_observation(&record.session_id)?.as_ref() != Some(record)
        {
            return Err(unavailable());
        }
        // RecordStore created its root lazily. Ensure the root entry as
        // well as the record itself is durable before signing the fact.
        let root = File::from(harw_fsutil::open_dir_nofollow(records.root())?);
        harw_fsutil::ensure_owned_directory(&root)?;
        if root.metadata()?.mode() & 0o077 != 0 { return Err(unavailable()); }
        let host = File::open(records.root().parent().ok_or_else(unavailable)?)?;
        harw_fsutil::ensure_owned_directory(&host)?;
        if host.metadata()?.mode() & 0o077 != 0 { return Err(unavailable()); }
        root.sync_all()?;
        host.sync_all()?;
        self.check_live()?;
        let claims = OwnerCreationClaims {
            node: self.authority.node.clone(),
            instance: self.authority.instance.clone(),
            tenant: record.tenant.clone().ok_or_else(unavailable)?,
            session: record.session_id.clone(),
            epoch: NonZeroU64::new(epoch).ok_or_else(unavailable)?,
            namespace_id: self.authority.namespace_id,
            key_fingerprint: self.authority.key_fingerprint,
        };
        let mut signature = vec![0; ML_DSA_65_SIGNATURE_LEN];
        let written = self.authority.key.sign(&claims.signing_bytes()?, &mut signature)
            .map_err(|_| unavailable())?;
        if written != signature.len() { return Err(unavailable()); }
        let receipt = OwnerCreationReceipt { claims, signature };
        if !receipt.agrees_with(record) || record.generation != 0
            || records.load_for_owner_observation(&record.session_id)?.as_ref() != Some(record)
        {
            return Err(unavailable());
        }
        self.check_live()?;
        if harw_fsutil::mkdir_at(root.as_fd(), Path::new(DIRECTORY), 0o700)? {
            root.sync_all()?;
        }
        let directory = harw_fsutil::open_dir_at(root.as_fd(), Path::new(DIRECTORY))?;
        harw_fsutil::ensure_owned_directory(&directory)?;
        if directory.metadata()?.mode() & 0o077 != 0 { return Err(unavailable()); }
        // Committed sidecars are write-once. A short directory lock excludes
        // the separate local reader until the no-replace/fsync operation ends.
        let lock = harw_fsutil::open_beneath(directory.as_fd(), Path::new(".lock"),
            harw_fsutil::OpenMode { create: true, ..harw_fsutil::OpenMode::read_write() })?;
        harw_fsutil::ensure_private_regular(&lock)?;
        if lock.metadata()?.nlink() != 1 { return Err(unavailable()); }
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => unavailable(),
            TryLockError::Error(error) => error.into(),
        })?;
        if !harw_fsutil::same_file_at(directory.as_fd(), Path::new(".lock"), &lock)? {
            return Err(unavailable());
        }
        let data = serde_json::to_vec(&receipt).map_err(|_| unavailable())?;
        if data.len() as u64 > MAX_RECEIPT_BYTES { return Err(unavailable()); }
        self.check_live()?;
        harw_session_store::store::persist_noclobber(
            &records.root().join(DIRECTORY).join(name(&record.session_id)?), &data,
        )?;
        Ok(())
    }
}

fn name(session: &SessionId) -> Result<String, HostError> {
    valid_session_id(session.as_str()).then(|| format!("{}.json", session.as_str()))
        .ok_or_else(unavailable)
}

/// Read only an exact local sidecar and its corresponding hosted row. The
/// caller must provide a trusted private state directory and independently
/// verify the returned signature and pins. This is not a liveness test.
///
/// # Errors
/// Refuses absent or inconsistent state, malformed evidence and unsafe files.
pub fn read_owner_creation_receipt(
    state_dir: &Path, tenant: &TenantId, session: &SessionId,
) -> Result<OwnerCreationReceipt, HostError> {
    use std::fs::{File, TryLockError};
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;

    if !state_dir.is_absolute() || !valid_topology_id(tenant.as_str()) { return Err(unavailable()); }
    let name = name(session)?;
    let records = RecordStore::new(state_dir);
    let root = File::from(harw_fsutil::open_dir_nofollow(records.root())?);
    harw_fsutil::ensure_owned_directory(&root)?;
    if root.metadata()?.mode() & 0o077 != 0 { return Err(unavailable()); }
    let directory = harw_fsutil::open_dir_at(root.as_fd(), Path::new(DIRECTORY))?;
    harw_fsutil::ensure_owned_directory(&directory)?;
    if directory.metadata()?.mode() & 0o077 != 0 { return Err(unavailable()); }
    let lock = harw_fsutil::open_beneath(directory.as_fd(), Path::new(".lock"), harw_fsutil::OpenMode::read_only())?;
    harw_fsutil::ensure_private_regular(&lock)?;
    if lock.metadata()?.nlink() != 1 { return Err(unavailable()); }
    lock.try_lock_shared().map_err(|error| match error {
        TryLockError::WouldBlock => unavailable(),
        TryLockError::Error(error) => error.into(),
    })?;
    if !harw_fsutil::same_file_at(directory.as_fd(), Path::new(".lock"), &lock)? {
        return Err(unavailable());
    }
    let name = Path::new(&name);
    let file = harw_fsutil::open_beneath(directory.as_fd(), name, harw_fsutil::OpenMode::read_only())?;
    harw_fsutil::ensure_private_regular(&file)?;
    let metadata = file.metadata()?;
    if metadata.nlink() != 1 || !(1..=MAX_RECEIPT_BYTES).contains(&metadata.len()) {
        return Err(unavailable());
    }
    let mut data = Vec::new();
    (&file).take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 != metadata.len()
        || !harw_fsutil::same_file_at(directory.as_fd(), name, &file)?
        || !harw_fsutil::same_file_at(root.as_fd(), Path::new(DIRECTORY), &directory)?
    {
        return Err(unavailable());
    }
    let receipt: OwnerCreationReceipt = serde_json::from_slice(&data).map_err(|_| unavailable())?;
    let record = records.load_for_owner_observation(session)?.ok_or_else(unavailable)?;
    if receipt.claims.tenant != *tenant || !receipt.agrees_with(&record) {
        return Err(unavailable());
    }
    Ok(receipt)
}

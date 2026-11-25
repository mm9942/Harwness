//! Empty work only. Any uncertain operation remains bound to its one reserved target.
use super::*;
use std::fs::{File, TryLockError};
use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::Path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use harw_protocol::session_wire::{ManualCloudWorkCommandV1, ManualCloudWorkReplyV1};
use crate::record::{CreateOutcome, CreatorExecutionBinding};

const DIRECTORY: &str = "manual-cloud-work-v1";
const MAX_RECEIPTS: usize = 1024;
const MAX_RECEIPT_BYTES: u64 = 8192;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    key: String,
    profile_root: PathBuf,
    authority: [u8; 32],
    target: SessionId,
}

impl HostConnection {
    pub(super) async fn run_manual_cloud_work(
        &self, work: ManualCloudWorkCommandV1,
    ) -> Result<ManualCloudWorkReplyV1, HostError> {
        let capabilities = self.granted()?;
        if !self.manual_cloud_work.load(Ordering::SeqCst) || self.host.is_draining()
            || !crate::cloud_assistant::CloudAssistant::admits(&self.identity, capabilities)
        {
            return Err(HostError::Denied("manual Cloud work is unavailable".into()));
        }
        let (operation, create) = match work {
            ManualCloudWorkCommandV1::CreateEmpty { operation_id } => (operation_id, true),
            ManualCloudWorkCommandV1::Resolve { operation_id } => (operation_id, false),
        };
        if !(1..=128).contains(&operation.len())
            || !operation.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(HostError::Protocol("invalid manual work operation ID".into()));
        }
        let share = self.steering_share()?;
        let root = share.index.sessions_root().to_owned();
        let mut record = self.new_record(SessionId::new(), None, None, self.record_tenant());
        record.fresh_human_origin = true;
        let (key, authority) = receipt_key(&self.identity, &operation)?;
        record.manual_cloud_work_receipt = Some(key.clone());
        let host = self.host.clone();
        let identity = self.identity.clone();
        let result = tokio::task::spawn_blocking(move || {
            let profile_root = std::fs::canonicalize(root)?;
            execute(&host, &identity, key, authority, profile_root, record, create)
        }).await.map_err(|_| storage())??;
        self.granted()?;
        Ok(result)
    }
}

fn execute(
    host: &HostInner, identity: &ClientIdentity, key: String, authority: [u8; 32], profile_root: PathBuf,
    candidate: HostedSessionRecord, create: bool,
) -> Result<ManualCloudWorkReplyV1, HostError> {
    ensure_live(host, identity)?;
    let records = host.records.root();
    let receipts = records.join(DIRECTORY);
    if !create && !receipts.try_exists()? {
        return Ok(ManualCloudWorkReplyV1::Missing);
    }
    if create {
        let mut dir = std::fs::DirBuilder::new();
        dir.mode(0o700);
        match dir.create(&receipts) {
            Ok(()) => File::from(harw_fsutil::open_dir_nofollow(records)?).sync_all()?,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
    }
    let directory = File::from(harw_fsutil::open_dir_nofollow(&receipts)?);
    harw_fsutil::ensure_owned_directory(&directory)?;
    if directory.metadata()?.mode() & 0o077 != 0 { return Err(storage()); }
    let guard = harw_fsutil::open_beneath(
        directory.as_fd(), Path::new(".lock"),
        harw_fsutil::OpenMode { create, ..harw_fsutil::OpenMode::read_write() },
    );
    // Resolve is read-only and never creates a lock/file or a session.
    let guard = match guard {
        Ok(file) => file,
        Err(error) if !create && error.kind() == ErrorKind::NotFound => return Ok(ManualCloudWorkReplyV1::Missing),
        Err(error) => return Err(error.into()),
    };
    private_file(&guard)?;
    if create { guard.try_lock() } else { guard.try_lock_shared() }.map_err(|error| match error {
        TryLockError::WouldBlock => HostError::Busy("manual work receipt is busy".into()),
        TryLockError::Error(error) => error.into(),
    })?;
    ensure_live(host, identity)?;
    let name = PathBuf::from(format!("{key}.json"));
    if let Some(existing) = read_intent(&directory, &name)? {
        if existing.key != key || existing.authority != authority || existing.profile_root != profile_root {
            return Err(HostError::Denied("manual work operation is bound to another profile".into()));
        }
        return resolve(host, identity, &existing, &candidate);
    }
    if !create { return Ok(ManualCloudWorkReplyV1::Missing); }

    if std::fs::read_dir(&receipts)?.take(MAX_RECEIPTS + 2).count() >= MAX_RECEIPTS + 1 {
        return Err(HostError::Busy("manual work receipt store is full".into()));
    }
    let intent = Intent { key, authority, profile_root, target: candidate.session_id.clone() };
    let bytes = serde_json::to_vec(&intent).map_err(|_| storage())?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES { return Err(storage()); }
    // The intent is the point of no retry: once synced, any later process may
    // only resolve it. A crash before publication yields Unknown, not a 2nd create.
    harw_session_store::store::persist_noclobber(&receipts.join(name), &bytes)?;
    directory.sync_all()?;
    ensure_live(host, identity)?;
    match host.records.create_if_absent(&candidate)? {
        CreateOutcome::Created => resolve(host, identity, &intent, &candidate),
        CreateOutcome::Existing(_) => Err(storage()),
    }
}

fn resolve(
    host: &HostInner, identity: &ClientIdentity, intent: &Intent, ownership: &HostedSessionRecord,
) -> Result<ManualCloudWorkReplyV1, HostError> {
    let Some(record) = host.records.load(&intent.target)? else {
        return Ok(ManualCloudWorkReplyV1::OutcomeUnknown);
    };
    // Title, time, state, and project may change normally; they are not proof.
    if record.session_id != intent.target
        || record.manual_cloud_work_receipt.as_deref() != Some(intent.key.as_str())
        || record.tenant != ownership.tenant || record.owner_principal != ownership.owner_principal
        || record.creator_execution.as_ref().and_then(|c| c.binding.as_ref())
            != ownership.creator_execution.as_ref().and_then(|c| c.binding.as_ref())
        || record.owner_agent.is_some()
    {
        return Err(HostError::Denied("manual work target does not match its receipt".into()));
    }
    ensure_live(host, identity)?;
    let slot = host.slot(&intent.target)?;
    let state = slot.lock()?;
    Ok(ManualCloudWorkReplyV1::Created { session: host.summary(&state) })
}

fn read_intent(root: &File, name: &Path) -> Result<Option<Intent>, HostError> {
    let f = match harw_fsutil::open_beneath(root.as_fd(), name, harw_fsutil::OpenMode::read_only()) {
        Ok(f) => f,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    private_file(&f)?;
    let size = f.metadata()?.len();
    if size == 0 || size > MAX_RECEIPT_BYTES { return Err(storage()); }
    let mut bytes = Vec::new();
    f.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size { return Err(storage()); }
    serde_json::from_slice(&bytes).map(Some).map_err(|_| storage())
}

fn private_file(file: &File) -> Result<(), HostError> {
    harw_fsutil::ensure_private_regular(file)?;
    if file.metadata()?.nlink() != 1 { return Err(storage()); }
    Ok(())
}

fn ensure_live(host: &HostInner, identity: &ClientIdentity) -> Result<(), HostError> {
    if host.is_revoked(identity) { return Err(HostError::Revoked); }
    if host.is_draining() { return Err(HostError::Denied("host draining".into())); }
    Ok(())
}

fn receipt_key(identity: &ClientIdentity, operation: &str) -> Result<(String, [u8; 32]), HostError> {
    // Credential rotation in the same principal scope must hit the old
    // receipt and deny; it is not a new operation namespace.
    let name = serde_json::to_vec(&(identity.principal.id(), &identity.tenant, operation))
        .map_err(|_| storage())?;
    let credential = serde_json::to_vec(&(
        &identity.actor, CreatorExecutionBinding::from_identity(identity),
    )).map_err(|_| storage())?;
    let mut hash = Sha256::new();
    hash.update(b"harw/manual-cloud-work/intent/v1\0");
    hash.update(name);
    let key = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let mut authority = Sha256::new();
    authority.update(b"harw/manual-cloud-work/authority/v1\0");
    authority.update(credential);
    Ok((key, authority.finalize().into()))
}

fn storage() -> HostError {
    HostError::Storage("manual Cloud work state unavailable".into())
}

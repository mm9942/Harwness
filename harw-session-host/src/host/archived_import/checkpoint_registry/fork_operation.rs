//! Durable idempotency and exact archive publication. Only the final journal
//! install makes the target visible. A recovered intent owns one target and
//! its deterministic seed identity; this path never calls a turn driver.

use std::fs::{File, TryLockError};

use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};

use super::*;

const FORK_STORE: &str = "archive-forks-v1";
const DISPLAYED_FORK_STORE: &str = "displayed-forks-v1";
const MAX_FORK_OPERATIONS: usize = 1024;
const HASH_DOMAIN: &[u8] = b"harw/archive-fork/operation-v1\0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredForkV1 {
    issuer: IssuerV1,
    scope: ScopeV1,
    request: ForkInclusiveV1,
    target: ArchiveDestination,
    complete: bool,
}

impl StoredForkV1 {
    fn matches(&self, issuer: &IssuerV1, scope: &ScopeV1, params: &ForkInclusiveV1) -> bool {
        &self.issuer == issuer && &self.scope == scope && &self.request == params
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDisplayedForkV1 {
    issuer: IssuerV1,
    scope: ScopeV1,
    source: SessionId,
    selection_digest: [u8; 32],
    target: SessionId,
    complete: bool,
}

impl HostConnection {
    /// One published child per exact scope and displayed source selection.
    /// A request cannot reserve a second target even if an ACK was lost.
    pub(in crate::host) async fn create_displayed_once(
        &self,
        params: CreateParams,
    ) -> Result<SessionSummary, HostError> {
        let fork = params.fork_from.as_ref().ok_or(HostError::NotFound)?;
        let displayed = fork.displayed.as_ref().ok_or(HostError::NotFound)?;
        if params.title.is_some() || params.workspace.is_some()
            || !(1..=harw_protocol::session_wire::DISPLAYED_FORK_MAX_ROWS).contains(&displayed.rows.len())
            || displayed.head.durable == 0
            || displayed.rows.last().is_none_or(|r| r.item_id != fork.before_item)
            || displayed.rows.iter().any(|r| {
                !(1..=128).contains(&r.item_id.len())
                    || !r.item_id.bytes().all(|b| b.is_ascii_graphic())
            })
        {
            return Err(HostError::Denied("invalid exact displayed fork selection".into()));
        }
        let source = fork.session_id.clone();
        let source_scope = self.fork_source_scope(&source, true)?;
        let source_slot = self.observed_slot(&source)?;
        {
            let state = source_slot.lock()?;
            self.check_displayed_fork_attachment(&state)?;
            if state.record.generation != displayed.head.generation
                || source_scope.generation != displayed.head.generation
            {
                return Err(HostError::Denied("displayed fork source generation changed".into()));
            }
        }
        // The live cursor can change during reconnect, and a later row beyond
        // the selection does not make this the user's second logical fork.
        // Actual source/head validation is still required before the first write.
        let encoded = serde_json::to_vec(&(
            "harw/displayed-fork/selection-v1",
            &source,
            displayed.head.generation,
            &displayed.rows,
        )).map_err(|_| HostError::Protocol("displayed fork proof cannot be encoded".into()))?;
        let digest = blake3::hash(&encoded);
        let issuer = IssuerV1::authenticated(&self.identity);
        let scope = ScopeV1::from(&source_scope);
        let root = self.host.records.root().join(DISPLAYED_FORK_STORE);
        let key = operation_key(&issuer, &scope.root, digest.to_hex().as_str())?;
        let _lock = lock_operations(&root)?;
        let path = root.join(format!("{key}.json"));
        if let Some(prior) = read_operation::<StoredDisplayedForkV1>(&path)? {
            if prior.issuer != issuer || prior.scope != scope || prior.source != source
                || prior.selection_digest != *digest.as_bytes() || prior.target == source
            {
                return Err(HostError::Denied("displayed fork receipt scope does not match".into()));
            }
            if !prior.complete {
                return Err(HostError::Busy("The prior fork is unconfirmed; no second chat was created".into()));
            }
            let child = self.host.records.load(&prior.target)?.ok_or(HostError::NotFound)?;
            if child.tenant != self.identity.tenant || child.owner_principal != issuer.principal
                || child.creator_execution.as_ref().and_then(|creator| creator.binding.as_ref())
                    != Some(&issuer.binding)
            {
                return Err(HostError::NotFound);
            }
            self.profile_target(&prior.target)?;
            let slot = self.observed_slot(&prior.target)?;
            if self.fork_source_scope(&source, true)? != source_scope {
                return Err(HostError::NotFound);
            }
            self.check_displayed_fork_attachment(&source_slot.lock()?)?;
            let state = slot.lock()?;
            return Ok(self.host.summary(&state));
        }

        let prepared = self.prepare_displayed_fork(fork).await?;
        if self.fork_source_scope(&source, true)? != source_scope || prepared.2 != source_scope.root {
            return Err(HostError::Denied("displayed fork source scope changed".into()));
        }
        let mut receipt = StoredDisplayedForkV1 {
            issuer,
            scope,
            source,
            selection_digest: *digest.as_bytes(),
            target: SessionId::new(),
            complete: false,
        };
        if self.host.records.load(&receipt.target)?.is_some() {
            return Err(HostError::Storage("reserved fork destination is already in use".into()));
        }
        reserve_operation(&root, &path, &receipt)?;
        let summary = self.create_prepared(params, receipt.target.clone(), Some(prepared)).await?;
        if summary.session_id != receipt.target {
            return Err(HostError::Storage("fork returned the wrong destination".into()));
        }
        receipt.complete = true;
        finish_operation(&path, &receipt)?;
        Ok(summary)
    }

    pub(in crate::host) async fn fork_inclusive_wire(
        &self,
        params: ForkInclusiveV1,
    ) -> Result<SessionSummary, HostError> {
        self.require_checkpoint_feature()?;
        if !valid_operation_id(&params.client_operation_id) {
            return Err(HostError::Protocol("invalid archive operation ID".into()));
        }
        let (_, ticket) = self
            .authorized_checkpoint(&params.session_id, &params.checkpoint_id)
            .await?;
        let issuer = IssuerV1::authenticated(&self.identity);
        let scope = ScopeV1::from(&ticket.scope);
        let root = self.host.records.root().join(FORK_STORE);
        let key = operation_key(&issuer, &scope.root, &params.client_operation_id)?;

        // Re-check the retained proof for first execution and replay, independently
        // of a restarted writer's empty cache. A missing proof is never a new fork.
        let source = ticket.source.clone();
        let source_root = ticket.scope.root.clone();
        let checkpoint = ticket.revision.clone();
        let displayed = params.displayed_prefix.clone();
        if displayed.is_some() && (!checkpoint.is_retained_predecessor()
            || !self.archive_retained_rewrites.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(HostError::Denied("exact displayed fork requires a retained predecessor".into()));
        }
        let row = ArchivedRowV1 {
            sequence: params.row.sequence,
            id: params.row.item_id.clone(),
        };
        let (prefix, fence) = tokio::task::spawn_blocking(move || {
            let (mut prefix, fence) = export_inclusive_guarded_v1(
                &TranscriptStore::new(&source_root),
                &source,
                &checkpoint,
                &row,
            )
            .map_err(archive_error)?;
            if let Some(proof) = &displayed {
                prefix.restrict_to_displayed(proof).map_err(archive_error)?;
            }
            Ok::<_, HostError>((prefix, fence))
        })
        .await
        .map_err(|_| HostError::Storage("archive fork reader stopped".into()))??;
        self.validate_archive_ticket(&ticket)?;
        let lock = lock_operations(&root)?;
        let path = root.join(format!("{key}.json"));
        let mut images_checked = false;
        let mut operation = match read_operation::<StoredForkV1>(&path)? {
            Some(operation) if operation.matches(&issuer, &scope, &params) => operation,
            Some(_) => {
                return Err(HostError::Denied(
                    "archive operation ID belongs to a different revision or row".into(),
                ));
            }
            None => {
                // Don't fill the durable intent store with unsupported or
                // unowned source images.
                validate_archive_media(self.host.driver.as_ref(), &prefix)?;
                images_checked = true;
                self.validate_archive_ticket(&ticket)?;
                let operation = StoredForkV1 {
                    issuer,
                    scope,
                    request: params,
                    target: ArchiveDestination {
                        id: SessionId::new(),
                        transaction: SessionId::new(),
                        at: jiff::Timestamp::now(),
                    },
                    complete: false,
                };
                reserve_operation(&root, &path, &operation)?;
                operation
            }
        };
        self.validate_archive_ticket(&ticket)?;
        if let Some(summary) = self.recover_archive_target(&operation, &ticket)? {
            if !operation.complete {
                operation.complete = true;
                finish_operation(&path, &operation)?;
            }
            return Ok(summary);
        }
        if operation.complete {
            // The target was removed by a later action; a retry is never
            // permission to create another copy of this revision.
            return Err(HostError::NotFound);
        }
        if !images_checked {
            validate_archive_media(self.host.driver.as_ref(), &prefix)?;
        }
        let summary = self.commit_archive_to(&ticket, prefix, fence, &operation.target, None)?;
        operation.complete = true;
        finish_operation(&path, &operation)?;
        drop(lock);
        Ok(summary)
    }

    fn recover_archive_target(
        &self,
        operation: &StoredForkV1,
        ticket: &IssuedArchiveTicket,
    ) -> Result<Option<SessionSummary>, HostError> {
        let target = &operation.target.id;
        let Some(record) = self.host.records.load(target)? else {
            return Ok(None);
        };
        if record.tenant != ticket.scope.tenant
            || record.owner_principal != ticket.principal
            || record
                .creator_execution
                .as_ref()
                .and_then(|creator| creator.binding.as_ref())
                != Some(&ticket.binding)
        {
            return Err(HostError::NotFound);
        }
        let store = TranscriptStore::new(&ticket.scope.root);
        let Ok(mut reader) = store.reader(target) else {
            return Ok(None);
        };
        if !operation.complete {
            let owned = reader.next().transpose()?.is_some_and(|header| {
                header.session_id == *target
                    && header.sequence == 0
                    && header.kind == RecordKind::Lifecycle
                    && header.payload["marker"].as_str() == Some(ARCHIVE_PUBLICATION_MARKER)
                    && header.payload["transaction"].as_str()
                        == Some(operation.target.transaction.as_str())
            });
            if !owned {
                return Err(HostError::Storage(
                    "archive target cannot be matched to its publication".into(),
                ));
            }
        }
        self.validate_archive_ticket(ticket)?;
        Ok(Some(self.host.summary(&SlotState::new(
            record,
            self.host.config.arbiter,
            0,
            false,
        ))))
    }
}

fn valid_operation_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn operation_key(issuer: &IssuerV1, root: &Path, operation: &str) -> Result<String, HostError> {
    let value = serde_json::to_vec(&(issuer, root, operation))
        .map_err(|_| HostError::Storage("archive operation scope cannot be encoded".into()))?;
    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    hasher.update(value);
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn lock_operations(root: &Path) -> Result<File, HostError> {
    if !private_dir(root, true)? {
        return Err(HostError::Storage(
            "archive operation directory unavailable".into(),
        ));
    }
    let lock = harw_fsutil::open_nofollow(
        &root.join(".lock"),
        harw_fsutil::OpenMode {
            create: true,
            ..harw_fsutil::OpenMode::read_write()
        },
    )?;
    if !lock
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o077 == 0)
    {
        return Err(HostError::Storage(
            "archive operation lock is not private".into(),
        ));
    }
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => {
            HostError::Busy("archive publication is busy; retry this operation ID".into())
        }
        TryLockError::Error(error) => HostError::from(error),
    })?;
    Ok(lock)
}

fn read_operation<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, HostError> {
    let file = match harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_RECEIPT_BYTES
    {
        return Err(HostError::Storage(
            "archive operation record is invalid".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(HostError::Storage(
            "archive operation record is too large".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| HostError::Storage("archive operation record is not readable".into()))
}

fn operation_bytes(operation: &impl Serialize) -> Result<Vec<u8>, HostError> {
    let bytes = serde_json::to_vec(operation)
        .map_err(|_| HostError::Storage("archive operation cannot be encoded".into()))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(HostError::Denied(
            "archive operation exceeds storage limit".into(),
        ));
    }
    Ok(bytes)
}

fn reserve_operation(root: &Path, path: &Path, operation: &impl Serialize) -> Result<(), HostError> {
    let mut count = 0_usize;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
            count += 1;
            if count >= MAX_FORK_OPERATIONS {
                return Err(HostError::Busy("archive operation store is full".into()));
            }
        }
    }
    harw_session_store::store::persist_noclobber(path, &operation_bytes(operation)?)?;
    let parent = root
        .parent()
        .ok_or_else(|| HostError::Storage("archive operation has no parent".into()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn finish_operation(path: &Path, operation: &impl Serialize) -> Result<(), HostError> {
    harw_fsutil::write_atomic(
        path,
        &operation_bytes(operation)?,
        harw_fsutil::AtomicWriteOptions::private(),
    )?;
    Ok(())
}

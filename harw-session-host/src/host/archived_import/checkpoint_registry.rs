//! V5 host-owned checkpoint issue/describe. No edit, import, or payload export.
//! Private records bind an opaque ID to authenticated actor and exact source
//! scope. A receipt is not a bearer token and never bypasses current ACL.

use std::io::{ErrorKind, Read as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

use harw_protocol::session_wire::{ArchivePositionV1, ArchiveRewriteCauseV1};
use serde::{Deserialize, Serialize};

use super::*;
use crate::archived_fork::{
    capture_previous_rewrite_v1, page_checkpoint_v1, verify_checkpoint_v1,
};

const MAX_RECEIPTS: usize = 512;
const MAX_RECEIPT_BYTES: u64 = 16 * 1024;
const STORE_DIR: &str = "archive-checkpoints-v1";
// Older binaries may reject new proofs. Keep their registry readable
// after a binary rollback, including when issuing normal current captures.
const RETAINED_STORE_DIR: &str = "archive-retained-rewrites-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCheckpointV1 {
    info: ArchiveCheckpointInfoV1,
    issuer: IssuerV1,
    scope: ScopeV1,
    proof: ArchivedCheckpointV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuerV1 {
    principal: String,
    tenant: Option<TenantId>,
    actor: ApprovalActor,
    binding: CreatorExecutionBinding,
}

impl IssuerV1 {
    fn authenticated(identity: &ClientIdentity) -> Self {
        Self {
            principal: identity.principal.id().to_owned(),
            tenant: identity.tenant.clone(),
            actor: identity.actor.clone(),
            binding: CreatorExecutionBinding::from_identity(identity),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeV1 {
    root: PathBuf,
    thread: harw_types::ThreadRef,
    tenant: Option<TenantId>,
    generation: u32,
    owner: String,
    project: Option<(String, PathBuf)>,
}

impl From<&ArchiveSourceScope> for ScopeV1 {
    fn from(scope: &ArchiveSourceScope) -> Self {
        Self {
            root: scope.root.clone(),
            thread: scope.thread.clone(),
            tenant: scope.tenant.clone(),
            generation: scope.generation,
            owner: scope.owner.clone(),
            project: scope
                .project
                .as_ref()
                .map(|project| (project.project_key.clone(), project.project_root.clone())),
        }
    }
}

impl HostConnection {
    pub(in crate::host) async fn issue_checkpoint_wire(
        &self,
        params: ArchiveCheckpointIssueV1,
    ) -> Result<ArchiveCheckpointInfoV1, HostError> {
        self.require_checkpoint_feature()?;
        let scope = self.archive_source_scope(&params.session_id)?;
        let expected =
            self.checked_wire_position(&params.session_id, scope.generation, params.expected)?;
        let ticket = self
            .issue_archive_ticket(&params.session_id, expected)
            .await?;
        self.validate_archive_ticket(&ticket)?;
        self.checked_wire_position(&params.session_id, ticket.scope.generation, params.expected)?;

        let issuer = IssuerV1::authenticated(&self.identity);
        let candidate = StoredCheckpointV1 {
            info: ArchiveCheckpointInfoV1 {
                session_id: ticket.source.clone(),
                checkpoint_id: SessionId::new().as_str().to_owned(),
                revision: params.expected,
            },
            issuer,
            scope: ScopeV1::from(&ticket.scope),
            proof: ticket.revision.clone(),
        };
        let host = Arc::clone(&self.host);
        let identity = self.identity.clone();
        let info = tokio::task::spawn_blocking(move || {
            let _guard = host.archive_checkpoint_storage.lock().map_err(|_| {
                HostError::Storage("archive checkpoint store is unavailable".into())
            })?;
            if host.is_revoked(&identity) {
                return Err(HostError::Revoked);
            }
            persist_receipt(&host.records.root().join(STORE_DIR), &candidate)
        })
        .await
        .map_err(|_| HostError::Storage("archive checkpoint writer stopped".into()))??;
        self.validate_archive_ticket(&ticket)?;
        Ok(info)
    }

    pub(in crate::host) async fn previous_checkpoint_wire(
        &self,
        params: ArchiveCheckpointPreviousV1,
    ) -> Result<Option<ArchiveRetainedRevisionV1>, HostError> {
        self.require_checkpoint_feature()?;
        if !self.archive_retained_rewrites.load(Ordering::SeqCst) {
            return Err(HostError::Denied(
                "archive retained rewrite feature was not granted".into(),
            ));
        }
        let scope = self.archive_source_scope(&params.session_id)?;
        let published = self
            .host
            .driver
            .archive_published_head(&params.session_id)?
            .ok_or_else(|| HostError::Busy("archive has no writer-verified horizon".into()))?;
        let before = params
            .before_sequence
            .map(|value| NonZeroU64::new(value).filter(|value| *value <= published)
                 .ok_or_else(|| HostError::Protocol("invalid archive continuation".into())))
            .transpose()?;
        let source = params.session_id;
        let scan_source = source.clone();
        let root = scope.root.clone();
        let retained = tokio::task::spawn_blocking(move || {
            capture_previous_rewrite_v1(&TranscriptStore::new(&root), &scan_source, published, before)
        })
        .await
        .map_err(|_| HostError::Storage("retained archive reader stopped".into()))?
        .map_err(|error| match error {
            ArchivedForkError::Changed => HostError::Busy("archive journal moved; reload".into()),
            error => archive_error(error),
        })?;
        if self.archive_source_scope(&source)? != scope {
            return Err(HostError::NotFound);
        }
        let Some((boundary, proof)) = retained else {
            return Ok(None);
        };
        if proof.thread() != &scope.thread
            || self.host.driver.archive_published_head(&source)?
                .is_none_or(|head| head < boundary.committed_next)
        {
            return Err(HostError::Busy("archive journal moved; reload".into()));
        }
        let candidate = StoredCheckpointV1 {
            info: ArchiveCheckpointInfoV1 {
                session_id: source.clone(),
                checkpoint_id: SessionId::new().as_str().to_owned(),
                revision: ArchivePositionV1 {
                    generation: scope.generation,
                    durable_next: boundary.before_sequence.get(),
                },
            },
            issuer: IssuerV1::authenticated(&self.identity),
            scope: ScopeV1::from(&scope),
            proof,
        };
        let host = Arc::clone(&self.host);
        let identity = self.identity.clone();
        let checkpoint = tokio::task::spawn_blocking(move || {
            let _guard = host.archive_checkpoint_storage.lock().map_err(|_| {
                HostError::Storage("archive checkpoint store is unavailable".into())
            })?;
            if host.is_revoked(&identity) {
                return Err(HostError::Revoked);
            }
            persist_receipt(&host.records.root().join(RETAINED_STORE_DIR), &candidate)
        })
        .await
        .map_err(|_| HostError::Storage("archive checkpoint writer stopped".into()))??;
        if self.archive_source_scope(&source)? != scope {
            return Err(HostError::NotFound);
        }
        Ok(Some(ArchiveRetainedRevisionV1 {
            checkpoint,
            before_sequence: boundary.before_sequence.get(),
            recorded_at: boundary.recorded_at,
            cause: if boundary.explicit_edit {
                ArchiveRewriteCauseV1::UserEdit
            } else {
                ArchiveRewriteCauseV1::HistoryRewrite
            },
        }))
    }

    pub(in crate::host) async fn describe_checkpoint_wire(
        &self,
        params: ArchiveCheckpointDescribeV1,
    ) -> Result<ArchiveCheckpointInfoV1, HostError> {
        let (info, ticket) = self
            .authorized_checkpoint(&params.session_id, &params.checkpoint_id)
            .await?;
        let source = ticket.source.clone();
        let root = ticket.scope.root.clone();
        let proof = ticket.revision.clone();
        tokio::task::spawn_blocking(move || {
            verify_checkpoint_v1(&TranscriptStore::new(&root), &source, &proof)
                .map_err(archive_error)
        })
        .await
        .map_err(|_| HostError::Storage("archive checkpoint reader stopped".into()))??;
        self.validate_archive_ticket(&ticket)?;
        Ok(info)
    }

    pub(in crate::host) async fn page_checkpoint_wire(
        &self,
        params: ArchiveCheckpointPageV1,
    ) -> Result<ArchiveCheckpointRowsV1, HostError> {
        let (info, ticket) = self
            .authorized_checkpoint(&params.session_id, &params.checkpoint_id)
            .await?;
        let source = ticket.source.clone();
        let root = ticket.scope.root.clone();
        let proof = ticket.revision.clone();
        let (rows, next_before_sequence) = tokio::task::spawn_blocking(move || {
            page_checkpoint_v1(
                &TranscriptStore::new(&root),
                &source,
                &proof,
                params.before_sequence,
                params.limit.unwrap_or(40),
            )
            .map_err(archive_error)
        })
        .await
        .map_err(|_| HostError::Storage("archive row reader stopped".into()))??;
        self.validate_archive_ticket(&ticket)?;
        Ok(ArchiveCheckpointRowsV1 {
            session_id: info.session_id,
            checkpoint_id: info.checkpoint_id,
            rows,
            next_before_sequence,
        })
    }

    pub(super) async fn authorized_checkpoint(
        &self,
        session: &SessionId,
        checkpoint_id: &str,
    ) -> Result<(ArchiveCheckpointInfoV1, IssuedArchiveTicket), HostError> {
        self.require_checkpoint_feature()?;
        if !safe_id(checkpoint_id) {
            return Err(HostError::NotFound);
        }
        let scope = self.archive_source_scope(session)?;
        let root = self.host.records.root().to_path_buf();
        let allow_retained = self.archive_retained_rewrites.load(Ordering::SeqCst);
        let issuer = IssuerV1::authenticated(&self.identity);
        let expected_scope = ScopeV1::from(&scope);
        let source = session.clone();
        let checkpoint_id = checkpoint_id.to_owned();
        let stored = tokio::task::spawn_blocking(move || {
            let stored = match load_receipt(&root.join(STORE_DIR), &checkpoint_id)? {
                Some(stored) => stored,
                None if allow_retained => load_receipt(
                    &root.join(RETAINED_STORE_DIR),
                    &checkpoint_id,
                )?
                .ok_or(HostError::NotFound)?,
                None => return Err(HostError::NotFound),
            };
            if stored.info.session_id != source
                || stored.info.checkpoint_id != checkpoint_id
                || stored.issuer != issuer
                || stored.scope != expected_scope
                || stored.proof.thread() != &expected_scope.thread
            {
                return Err(HostError::NotFound);
            }
            Ok::<_, HostError>(stored)
        })
        .await
        .map_err(|_| HostError::Storage("archive checkpoint reader stopped".into()))??;
        let ticket = IssuedArchiveTicket {
            source: session.clone(),
            revision: stored.proof,
            scope,
            principal: stored.issuer.principal,
            requester_tenant: stored.issuer.tenant,
            actor: stored.issuer.actor,
            binding: stored.issuer.binding,
            epoch: self.host.epoch,
        };
        self.validate_archive_ticket(&ticket)?;
        Ok((stored.info, ticket))
    }

    fn require_checkpoint_feature(&self) -> Result<(), HostError> {
        self.granted()?;
        if !self.archive_checkpoints.load(Ordering::SeqCst) {
            return Err(HostError::Denied(
                "archive checkpoint feature was not granted".into(),
            ));
        }
        Ok(())
    }

    fn checked_wire_position(
        &self,
        source: &SessionId,
        current_generation: u32,
        expected: ArchivePositionV1,
    ) -> Result<NonZeroU64, HostError> {
        let current = self
            .host
            .driver
            .archive_published_head(source)?
            .ok_or_else(|| HostError::Denied("no writer-verified archive revision".into()))?;
        if expected.generation != current_generation || expected.durable_next != current.get() {
            return Err(HostError::Busy(
                "observed transcript changed; reload before capturing".into(),
            ));
        }
        Ok(current)
    }
}

fn safe_id(id: &str) -> bool {
    (32..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

fn private_dir(path: &Path, create: bool) -> Result<bool, HostError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0 => Ok(true),
        Ok(_) => Err(HostError::Storage(
            "archive checkpoint directory is not private".into(),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound && create => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            private_dir(path, false)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn load_receipt(dir: &Path, id: &str) -> Result<Option<StoredCheckpointV1>, HostError> {
    if !safe_id(id) || !private_dir(dir, false)? {
        return Ok(None);
    }
    let path = dir.join(format!("{id}.json"));
    let file = match harw_fsutil::open_nofollow(&path, harw_fsutil::OpenMode::read_only()) {
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
            "archive checkpoint record is invalid".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(HostError::Storage(
            "archive checkpoint record is too large".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| HostError::Storage("archive checkpoint record cannot be restored".into()))
}

fn persist_receipt(
    dir: &Path,
    candidate: &StoredCheckpointV1,
) -> Result<ArchiveCheckpointInfoV1, HostError> {
    if !safe_id(&candidate.info.checkpoint_id) || !private_dir(dir, true)? {
        return Err(HostError::Storage(
            "archive checkpoint store is unavailable".into(),
        ));
    }
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| safe_id(id))
        else {
            continue;
        };
        count += 1;
        if count > MAX_RECEIPTS {
            return Err(HostError::Busy("archive checkpoint store is full".into()));
        }
        if let Some(existing) = load_receipt(dir, id)?
            && existing.info.checkpoint_id == id
            && existing.info.session_id == candidate.info.session_id
            && existing.info.revision == candidate.info.revision
            && existing.issuer == candidate.issuer
            && existing.scope == candidate.scope
            && existing.proof == candidate.proof
        {
            return Ok(existing.info);
        }
    }
    if count >= MAX_RECEIPTS {
        return Err(HostError::Busy("archive checkpoint store is full".into()));
    }
    let bytes = serde_json::to_vec(candidate)
        .map_err(|_| HostError::Storage("archive checkpoint record cannot be encoded".into()))?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(HostError::Denied(
            "archive checkpoint scope exceeds limit".into(),
        ));
    }
    let path = dir.join(format!("{}.json", candidate.info.checkpoint_id));
    harw_session_store::store::persist_noclobber(&path, &bytes)?;
    // The root is already durable for normal host records. On first mkdir
    // also sync the parent, so a completed receipt survives host restart.
    let parent = dir
        .parent()
        .ok_or_else(|| HostError::Storage("archive store has no parent".into()))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(candidate.info.clone())
}

mod fork_operation;

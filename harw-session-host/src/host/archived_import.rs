//! Internal host-owned, scope-checked inclusive archive import.
//! No new wire methods, client-supplied roles, model, runtime open, or tools.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::ApprovalActor;
use serde_json::json;

use super::*;
use crate::archived_fork::{
    ArchivedCheckpointV1, ArchivedForkError, ArchivedPrefixV1, ArchivedRowV1,
    ArchivedSourceFenceV1, capture_checkpoint_v1, export_inclusive_guarded_v1,
};
use crate::index::ResolvedProject;
use crate::record::{
    ARCHIVE_PUBLICATION_MARKER, ArchivePublicationIntent, CreatorExecutionBinding,
};

const MAX_ARCHIVE_MEDIA: usize = 50;
const MAX_ARCHIVE_MEDIA_BYTES: u64 = 16 * 1024 * 1024;

/// Non-serializable server-owned capability. No caller can use a replay
/// cursor or construct this type from device-provided JSON.
pub(super) struct IssuedArchiveTicket {
    source: SessionId,
    revision: ArchivedCheckpointV1,
    scope: ArchiveSourceScope,
    principal: String,
    requester_tenant: Option<TenantId>,
    actor: ApprovalActor,
    binding: CreatorExecutionBinding,
    epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArchiveSourceScope {
    root: PathBuf,
    thread: harw_types::ThreadRef,
    tenant: Option<TenantId>,
    generation: u32,
    owner: String,
    project: Option<ResolvedProject>,
}

impl HostConnection {
    /// Current authenticated source proof for a specific archived media read.
    /// Page/fork/preview all use the same server-owned receipt and ACL.
    pub(super) async fn archive_preview_checkpoint(
        &self,
        source: &SessionId,
        checkpoint_id: &str,
    ) -> Result<ArchivedCheckpointV1, HostError> {
        let (_, ticket) = self.authorized_checkpoint(source, checkpoint_id).await?;
        self.validate_archive_ticket(&ticket)?;
        Ok(ticket.revision)
    }

    /// Issue an exact retained-prefix capability using an existing active
    /// source writer's post-sync fence; a position alone never proves custody.
    pub(super) async fn issue_archive_ticket(
        &self,
        session: &SessionId,
        revision: NonZeroU64,
    ) -> Result<IssuedArchiveTicket, HostError> {
        let scope = self.archive_source_scope(session)?;
        let published = self
            .host
            .driver
            .archive_published_head(session)?
            .ok_or_else(|| HostError::Denied("archive has no verified writer checkpoint".into()))?;
        if revision > published {
            return Err(HostError::Denied(
                "archive revision is not committed".into(),
            ));
        }
        let source = session.clone();
        let root = scope.root.clone();
        let checkpoint = tokio::task::spawn_blocking(move || {
            capture_checkpoint_v1(&TranscriptStore::new(&root), &source, revision)
        })
        .await
        .map_err(|_| HostError::Storage("archive checkpoint reader stopped".into()))?
        .map_err(archive_error)?;
        if checkpoint.thread() != &scope.thread
            || self.archive_source_scope(session)? != scope
            || self
                .host
                .driver
                .archive_published_head(session)?
                .is_none_or(|head| head < revision)
        {
            return Err(HostError::Denied("archive source scope changed".into()));
        }
        Ok(IssuedArchiveTicket {
            source: session.clone(),
            revision: checkpoint,
            scope,
            principal: self.identity.principal.id().to_owned(),
            requester_tenant: self.identity.tenant.clone(),
            actor: self.identity.actor.clone(),
            binding: CreatorExecutionBinding::from_identity(&self.identity),
            epoch: self.host.epoch,
        })
    }

    pub(super) async fn import_archive_ticket(
        &self,
        ticket: &IssuedArchiveTicket,
        row: &ArchivedRowV1,
        title: Option<&str>,
    ) -> Result<SessionSummary, HostError> {
        self.validate_archive_ticket(ticket)?;
        let source = ticket.source.clone();
        let root = ticket.scope.root.clone();
        let checkpoint = ticket.revision.clone();
        let row = row.clone();
        let driver = Arc::clone(&self.host.driver);
        let (prefix, source_fence) = tokio::task::spawn_blocking(move || {
            let (prefix, source_fence) = export_inclusive_guarded_v1(
                &TranscriptStore::new(&root),
                &source,
                &checkpoint,
                &row,
            )
            .map_err(archive_error)?;
            // Verify only source-member refs in this driver's prompt store,
            // not the process-global reader nor any client-supplied refs.
            validate_archive_media(driver.as_ref(), &prefix)?;
            Ok::<_, HostError>((prefix, source_fence))
        })
        .await
        .map_err(|_| HostError::Storage("archive snapshot reader stopped".into()))??;
        self.validate_archive_ticket(ticket)?;
        self.commit_archive_destination(ticket, prefix, source_fence, title)
    }

    fn archive_source_scope(&self, id: &SessionId) -> Result<ArchiveSourceScope, HostError> {
        self.fork_source_scope(id, false)
    }

    // Direct Fork also supported an attached read-only profile-view source.
    // This does not adopt it or grant archive/checkpoint authority.
    fn fork_source_scope(&self, id: &SessionId, allow_profile_view: bool) -> Result<ArchiveSourceScope, HostError> {
        self.granted()?;
        if self.host.is_draining() || self.identity.agent.is_some() {
            return Err(HostError::Denied(
                "archive unavailable for this caller".into(),
            ));
        }
        let share = self.profile_target(id)?;
        let source = self.observed_slot(id)?;
        let (tenant, generation, owner) = {
            let state = source.lock()?;
            if (!allow_profile_view && state.ephemeral) || state.record.tenant != self.identity.tenant {
                return Err(HostError::NotFound);
            }
            (
                state.record.tenant.clone(),
                state.record.generation,
                state.record.owner_principal.clone(),
            )
        };
        let binding = self.host.driver.archive_journal_binding(id)?;
        let root = std::fs::canonicalize(&binding.root)?;
        if root != std::fs::canonicalize(share.index.sessions_root())? {
            return Err(HostError::Denied(
                "archive transcript store does not match source profile".into(),
            ));
        }
        let project = match meta::load(&root, id)? {
            Some(source_meta) if source_meta.project_key.is_some() => {
                let key = source_meta
                    .project_key
                    .filter(|key| !key.is_empty())
                    .ok_or(HostError::NotFound)?;
                // resolve_project can create a directory on both supported
                // backends. Archive issue/describe must not recreate a removed
                // project; workdir proves it already exists in this profile.
                let workdir = share.index.workdir(id)?.ok_or(HostError::NotFound)?;
                if source_meta.project_root.as_deref() != Some(workdir.as_path()) {
                    return Err(HostError::Denied(
                        "archive source project does not match profile".into(),
                    ));
                }
                Some(ResolvedProject {
                    project_key: key.clone(),
                    project_root: workdir,
                    label: key,
                })
            }
            _ => None,
        };
        Ok(ArchiveSourceScope {
            root,
            thread: binding.thread,
            tenant,
            generation,
            owner,
            project,
        })
    }

    fn validate_archive_ticket(&self, ticket: &IssuedArchiveTicket) -> Result<(), HostError> {
        self.granted()?;
        if ticket.epoch != self.host.epoch
            || ticket.principal != self.identity.principal.id()
            || ticket.requester_tenant != self.identity.tenant
            || ticket.actor != self.identity.actor
            || ticket.binding != CreatorExecutionBinding::from_identity(&self.identity)
            || self.archive_source_scope(&ticket.source)? != ticket.scope
        {
            return Err(HostError::NotFound);
        }
        Ok(())
    }

    fn commit_archive_destination(
        &self,
        ticket: &IssuedArchiveTicket,
        prefix: ArchivedPrefixV1,
        source_fence: ArchivedSourceFenceV1,
        title: Option<&str>,
    ) -> Result<SessionSummary, HostError> {
        self.commit_archive_to(
            ticket,
            prefix,
            source_fence,
            &ArchiveDestination {
                id: SessionId::new(),
                transaction: SessionId::new(),
                at: jiff::Timestamp::now(),
            },
            title,
        )
    }

    fn commit_archive_to(
        &self,
        ticket: &IssuedArchiveTicket,
        prefix: ArchivedPrefixV1,
        source_fence: ArchivedSourceFenceV1,
        destination: &ArchiveDestination,
        title: Option<&str>,
    ) -> Result<SessionSummary, HostError> {
        let target = destination.id.clone();
        let mut record = self.new_record(target.clone(), title, None, ticket.scope.tenant.clone());
        record.created_at = destination.at;
        record.updated_at = destination.at;

        let binding = self.host.driver.archive_journal_binding(&target)?;
        if std::fs::canonicalize(binding.root)? != ticket.scope.root {
            return Err(HostError::Denied(
                "archive target store does not match source profile".into(),
            ));
        }
        let store = TranscriptStore::new(&ticket.scope.root);
        let transcript = store.transcript_path(&target)?;
        let project_meta = meta::meta_path(&ticket.scope.root, &target);
        if path_exists(&transcript)? {
            return Err(HostError::Storage(
                "archive destination already exists".into(),
            ));
        }
        let transaction = &destination.transaction;
        let first_user = prefix.first_user_text();
        let seed = prefix.into_seed();
        let count = seed.len();
        let mut journal = Vec::new();
        let mut sequence = 0_u64;
        let mut append = |recorded_at, kind, payload| -> Result<(), HostError> {
            let row = TranscriptRecord::new(
                target.clone(),
                binding.thread.clone(),
                sequence,
                recorded_at,
                kind,
                payload,
            );
            let line = row.to_jsonl_line()?;
            if line.len() > harw_session_store::store::MAX_RECORD_BYTES {
                return Err(HostError::Denied(
                    "archive seed record exceeds limit".into(),
                ));
            }
            journal.extend_from_slice(line.as_bytes());
            sequence += 1;
            Ok(())
        };
        let now = record.created_at;
        append(
            now,
            RecordKind::Lifecycle,
            json!({
                "marker": ARCHIVE_PUBLICATION_MARKER,
                "transaction": transaction,
                "source": ticket.source,
                "source_checkpoint": ticket.revision,
            }),
        )?;
        append(
            now,
            RecordKind::Lifecycle,
            json!({"marker":"history_replace_started","item_count":count}),
        )?;
        for (timestamp, row) in seed {
            append(
                timestamp,
                RecordKind::Item,
                serde_json::to_value(row)
                    .map_err(|_| HostError::Storage("archive seed cannot be encoded".into()))?,
            )?;
        }
        append(
            now,
            RecordKind::Lifecycle,
            json!({"marker":"history_replaced","item_count":count}),
        )?;

        let intent = ArchivePublicationIntent::new(transcript, &journal);
        // A retry under the operation-store lock may remove only its own
        // byte-identical hidden stage. Never touch a published transcript.
        if let Some(staged) = self.host.records.load_archive_intent(&target)? {
            if staged != intent || path_exists(intent.transcript())? {
                return Err(HostError::Storage(
                    "archive staging belongs to another publication".into(),
                ));
            }
            self.host.records.remove_staged_archive_record(&target)?;
            match std::fs::remove_file(&project_meta) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            std::fs::File::open(&ticket.scope.root)?.sync_all()?;
            self.host.records.remove_archive_intent(&target)?;
        }
        if path_exists(&project_meta)? {
            return Err(HostError::Storage(
                "unowned archive project metadata already exists".into(),
            ));
        }
        self.host
            .records
            .reserve_archive_publication(&target, &intent)?;
        let mut cleanup = ArchiveStage {
            records: &self.host.records,
            target: &target,
            meta_path: &project_meta,
            record_staged: false,
            metadata_staged: false,
            preserve_intent: false,
            published: false,
        };
        cleanup.metadata_staged = true;
        meta::update(&ticket.scope.root, &target, |meta| {
            if let Some(project) = &ticket.scope.project {
                meta.cwd = Some(project.project_root.clone());
                meta.project_root = Some(project.project_root.clone());
                meta.project_key = Some(project.project_key.clone());
            }
            meta.title = record.title.clone();
            if meta.title.is_some() {
                meta.title_source = TitleSource::Manual;
            }
            meta.last_origin = Some(LastOrigin::Gateway);
            meta.first_user_message = first_user;
        })?;
        match self.host.records.create_if_absent(&record) {
            Ok(CreateOutcome::Created) => cleanup.record_staged = true,
            Ok(CreateOutcome::Existing(_)) => {
                return Err(HostError::Storage(
                    "archive destination already exists".into(),
                ));
            }
            Err(error) => {
                // Link/persist may have succeeded before directory sync
                // failed. Keep the invisibility gate in place if uncertain.
                cleanup.preserve_intent = true;
                return Err(error);
            }
        }
        self.validate_archive_ticket(ticket)?;
        if !source_fence
            .still_current(&store, &ticket.source)
            .map_err(archive_error)?
        {
            return Err(archive_error(ArchivedForkError::Changed));
        }
        // Readers cannot see the hosted record without the exact journal
        // prefix, nor a legacy profile row before the complete .jsonl is
        // installed. All data and the checked project are synced before that
        // final atomic visibility point. No live core/model is created.
        if let Err(error) =
            harw_session_store::store::persist_noclobber(intent.transcript(), &journal)
        {
            if intent.is_published(&target) {
                cleanup.published = true;
                return Err(HostError::Storage(
                    "archive publication outcome uncertain; do not automatically retry".into(),
                ));
            }
            return Err(error.into());
        }
        cleanup.published = true;
        if let Err(error) = self.host.records.remove_archive_intent(&target) {
            tracing::warn!(%error, "archive committed; could not remove publication intent");
        }
        Ok(self.host.summary(&SlotState::new(
            record,
            self.host.config.arbiter,
            sequence,
            false,
        )))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveDestination {
    id: SessionId,
    transaction: SessionId,
    at: jiff::Timestamp,
}

struct ArchiveStage<'a> {
    records: &'a RecordStore,
    target: &'a SessionId,
    meta_path: &'a Path,
    record_staged: bool,
    metadata_staged: bool,
    preserve_intent: bool,
    published: bool,
}

impl Drop for ArchiveStage<'_> {
    fn drop(&mut self) {
        if self.published || self.preserve_intent {
            return;
        }
        if self.record_staged
            && self
                .records
                .remove_staged_archive_record(self.target)
                .is_err()
        {
            // Never unhide a staged record after failed cleanup.
            return;
        }
        if self.metadata_staged {
            match std::fs::remove_file(self.meta_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return,
            }
            if self
                .meta_path
                .parent()
                .and_then(|parent| std::fs::File::open(parent).ok())
                .is_none_or(|root| root.sync_all().is_err())
            {
                return;
            }
        }
        let _ = self.records.remove_archive_intent(self.target);
    }
}

fn path_exists(path: &Path) -> Result<bool, HostError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn validate_archive_media(
    driver: &dyn TurnDriver,
    prefix: &ArchivedPrefixV1,
) -> Result<(), HostError> {
    let mut seen = HashSet::new();
    let mut bytes = 0_u64;
    for media in prefix.media_refs() {
        if !seen.insert(media) {
            continue;
        }
        let store = driver
            .archive_media_store()
            .ok_or_else(|| HostError::Denied("archive media store is not available".into()))?;
        bytes = bytes.saturating_add(media.bytes());
        if seen.len() > MAX_ARCHIVE_MEDIA
            || bytes > MAX_ARCHIVE_MEDIA_BYTES
            || media.bytes() > store.limits().max_bytes
        {
            return Err(HostError::Denied(
                "archive media exceeds import limit".into(),
            ));
        }
        if !store.contains(media) {
            return Err(HostError::Denied(
                "archive image is not available in the source store".into(),
            ));
        }
    }
    Ok(())
}

fn archive_error(error: ArchivedForkError) -> HostError {
    match error {
        ArchivedForkError::WrongSource => HostError::NotFound,
        ArchivedForkError::Changed => {
            HostError::Denied("archived revision changed or is no longer retained".into())
        }
        _ => HostError::Denied("archive is not an exact supported retained revision".into()),
    }
}

mod checkpoint_registry;

#[cfg(test)]
mod tests;

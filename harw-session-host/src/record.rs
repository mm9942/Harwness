//! Durable hosted session records (PL-65 §2.2).
//!
//! One JSON file per hosted session under `<state_dir>/hosted-sessions/`,
//! written atomically with mode `0600` into a `0700` directory. The same
//! directory holds the `epoch` file: a decimal counter the host bumps once per
//! start, so records carry the epoch of the host that last owned them.
//!
//! Session ids become file names, so every id is checked against a strict
//! charset before it touches the filesystem (no separators, no leading dot).

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};

use harw_protocol::HostedState;
use harw_types::{
    ApprovalActor, AuthStrength, DeviceId, IngressSurface, PermissionTier, PrincipalKind,
    SessionId, TenantId, TrustZone,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::HostError;

/// Name of the directory below the state dir that holds the records.
const DIR_NAME: &str = "hosted-sessions";
/// Name of the host epoch counter file inside the record directory.
const EPOCH_FILE: &str = "epoch";
/// File extension of a record file.
const RECORD_EXT: &str = "json";
/// Longest session id accepted as a file name.
const MAX_ID_LEN: usize = 128;
/// This opt-in metadata-only observation refuses oversized legacy rows.
const MAX_OWNER_OBSERVATION_RECORD_BYTES: u64 = 256 * 1024;

/// Stable provenance pinned at creation, used only as a matching constraint.
/// It never reconstructs an authenticated principal or grants execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatorExecutionBinding {
    pub kind: PrincipalKind,
    pub surface: IngressSurface,
    pub device: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated_node: Option<harw_types::NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated_key_fingerprint: Option<[u8; 32]>,
    pub zone: TrustZone,
    pub strength: AuthStrength,
}

impl CreatorExecutionBinding {
    #[must_use]
    pub fn from_identity(identity: &crate::identity::ClientIdentity) -> Self {
        Self {
            kind: identity.principal.kind(),
            surface: identity.principal.surface(),
            device: identity.device.clone(),
            authenticated_node: identity.authenticated_node.clone(),
            authenticated_key_fingerprint: identity.authenticated_key_fingerprint,
            zone: identity.zone,
            strength: identity.strength,
        }
    }
}

/// Creator-derived request ceiling and actor, recorded for future shared-host admission.
///
/// This persisted value is not authority. Before any execution, a trusted
/// policy must revalidate the creator and intersect this ceiling with current
/// host and device restrictions. Legacy records without it cannot establish
/// a creator's execution authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatorExecutionRequest {
    /// Missing in early/legacy records: cannot establish creator authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<CreatorExecutionBinding>,
    pub maximum_tier: PermissionTier,
    pub approval_actor: ApprovalActor,
}

/// Private evidence about acceptance of an exact history operation. A reservation
/// cannot prove that the turn ran; committed proves the prompt landed, not that
/// model generation succeeded. Never deserialize this type from the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactRewriteReceipt {
    pub id: String,
    pub principal: String,
    pub binding: CreatorExecutionBinding,
    pub fingerprint: [u8; 32],
    pub original_generation: u32,
    pub original_durable: u64,
    pub committed: bool,
}

/// Durable metadata of one hosted session.
///
/// No `deny_unknown_fields`: records evolve additively, and an older host
/// must still read a record a newer host wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedSessionRecord {
    pub session_id: SessionId,
    pub tenant: Option<TenantId>,
    pub owner_principal: String,
    /// Provenance asserted by the host only for a fresh, unseeded human session.
    /// An adopted, forked, imported or legacy record does not gain title-read authority.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fresh_human_origin: bool,
    /// Creator-derived request; never sufficient to authorize execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_execution: Option<CreatorExecutionRequest>,
    /// Durable marker proving an exact manual empty-work publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_cloud_work_receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_rewrites: Vec<ExactRewriteReceipt>,
    pub workspace: Option<String>,
    pub title: Option<String>,
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub state: HostedState,
    /// Epoch of the host process that last owned this session.
    pub host_epoch: u64,
    pub generation: u32,
    /// R18 §4: agent principal that created the session (`None`: created by
    /// a human/device caller). Gateway tool calls into the session are
    /// admitted only for this agent and its delegates, and for the agents
    /// in [`Self::tool_agents`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent: Option<String>,
    /// R18 §4: agent principals the host composition bound to the session
    /// explicitly (`SessionHost::bind_tool_agent`), each with its delegates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_agents: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl HostedSessionRecord {
    /// Revalidate a creator request against a freshly authenticated identity.
    ///
    /// This returns a narrowing only; composition must also enforce all other
    /// live policy, workspace and device restrictions. Legacy records and
    /// identity or actor mismatches fail closed.
    pub fn revalidate_creator_execution(
        &self,
        creator: &crate::identity::ClientIdentity,
        host_maximum: PermissionTier,
    ) -> Result<CreatorExecutionRequest, crate::error::HostError> {
        creator.validate()?;
        let request = self.creator_execution.as_ref().ok_or_else(|| {
            crate::error::HostError::Denied(
                "legacy session lacks a creator execution request".into(),
            )
        })?;
        if request.binding.as_ref() != Some(&CreatorExecutionBinding::from_identity(creator))
            || self.owner_principal != creator.principal.id()
            || self.tenant != creator.tenant
            || self.owner_agent.as_deref() != creator.agent.as_ref().map(|agent| agent.id.as_str())
            || request.approval_actor != creator.actor
        {
            return Err(crate::error::HostError::Denied(
                "session creator identity or approval actor no longer matches".into(),
            ));
        }
        Ok(CreatorExecutionRequest {
            binding: request.binding.clone(),
            maximum_tier: request
                .maximum_tier
                .min(creator.principal.tier())
                .min(host_maximum),
            approval_actor: creator.actor.clone(),
        })
    }

    /// True when an agent whose delegation chain is `lineage` (the agent
    /// first, then its parent, …, up to the UIA) may run gateway tools in
    /// this session: some agent of the chain created the session or was
    /// bound to it. Fail closed: an empty chain is never admitted.
    #[must_use]
    pub fn admits_tool_agent(&self, lineage: &[String]) -> bool {
        lineage.iter().any(|agent| {
            self.owner_agent.as_deref() == Some(agent.as_str()) || self.tool_agents.contains(agent)
        })
    }
}

/// Outcome of [`RecordStore::create_if_absent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateOutcome {
    /// The record was written by this call.
    Created,
    /// Another caller was faster; this is its record, left unchanged.
    Existing(Box<HostedSessionRecord>),
}

pub(crate) const ARCHIVE_PUBLICATION_MARKER: &str = "archived_fork_publication_v1";

/// A private publication intent is written before the hosted record. Both
/// normal readers and restart recovery keep that record invisible until the
/// exact complete destination journal prefix is atomically installed.
#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchivePublicationIntent {
    transcript: PathBuf,
    bytes: u64,
    sha256: [u8; 32],
}

impl ArchivePublicationIntent {
    pub(crate) fn new(transcript: PathBuf, seed: &[u8]) -> Self {
        Self {
            transcript,
            bytes: seed.len() as u64,
            sha256: Sha256::digest(seed).into(),
        }
    }

    pub(crate) fn transcript(&self) -> &Path {
        &self.transcript
    }

    pub(crate) fn is_published(&self, id: &SessionId) -> bool {
        let expected = format!("{}.jsonl", id.as_str());
        if !self.transcript.is_absolute()
            || self.transcript.file_name().and_then(|name| name.to_str()) != Some(expected.as_str())
            || self.bytes == 0
            || self.bytes > 3 * 1024 * 1024
        {
            return false;
        }
        let Ok(file) =
            harw_fsutil::open_nofollow(&self.transcript, harw_fsutil::OpenMode::read_only())
        else {
            return false;
        };
        if !file
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.len() >= self.bytes)
        {
            return false;
        }
        let mut prefix = Vec::new();
        if file.take(self.bytes).read_to_end(&mut prefix).is_err() {
            return false;
        }
        let digest: [u8; 32] = Sha256::digest(&prefix).into();
        prefix.len() as u64 == self.bytes && digest == self.sha256
    }
}

/// File-backed store of [`HostedSessionRecord`]s.
#[derive(Clone, Debug)]
pub struct RecordStore {
    root: PathBuf,
}

impl RecordStore {
    /// A store rooted at `<state_dir>/hosted-sessions`. Nothing is created
    /// until the first write.
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(DIR_NAME),
        }
    }

    /// The record directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(feature = "owner-creation-receipts")]
    pub(crate) fn require_absent(&self, id: &SessionId) -> Result<(), HostError> {
        match std::fs::symlink_metadata(self.record_path(id)?) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(HostError::Denied("isolated writer record already exists".into())),
        }
    }

    /// Write `record` atomically, replacing any previous version.
    pub fn save(&self, record: &HostedSessionRecord) -> Result<(), HostError> {
        let path = self.record_path(&record.session_id)?;
        self.ensure_root()?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| {
            HostError::Storage(format!("encode hosted session record: {error}"))
        })?;
        harw_fsutil::write_atomic(&path, &bytes, harw_fsutil::AtomicWriteOptions::private())?;
        Ok(())
    }

    /// Write `record` only when no record of this session exists yet
    /// (O_EXCL semantics via `persist_noclobber`: private temp file, no-replace
    /// link into place, parent fsync). An existing record is loaded and
    /// returned as [`CreateOutcome::Existing`]; other failures are errors as
    /// in [`RecordStore::save`].
    pub fn create_if_absent(
        &self,
        record: &HostedSessionRecord,
    ) -> Result<CreateOutcome, HostError> {
        let path = self.record_path(&record.session_id)?;
        self.ensure_root()?;
        let bytes = serde_json::to_vec_pretty(record).map_err(|error| {
            HostError::Storage(format!("encode hosted session record: {error}"))
        })?;
        match harw_session_store::store::persist_noclobber(&path, &bytes) {
            Ok(()) => Ok(CreateOutcome::Created),
            Err(harw_session_store::SessionStoreError::PersistTargetExists { .. }) => {
                match self.load(&record.session_id)? {
                    Some(existing) => Ok(CreateOutcome::Existing(Box::new(existing))),
                    None => Err(HostError::Storage(format!(
                        "{}: record vanished during create",
                        path.display()
                    ))),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Remove an unpublished/failed creation record. A missing file is benign.
    pub fn remove(&self, id: &SessionId) -> Result<(), HostError> {
        match std::fs::remove_file(self.record_path(id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn reserve_archive_publication(
        &self,
        id: &SessionId,
        intent: &ArchivePublicationIntent,
    ) -> Result<(), HostError> {
        let record = self.record_path(id)?;
        if std::fs::symlink_metadata(record).is_ok() {
            return Err(HostError::Storage(
                "archive destination already exists".into(),
            ));
        }
        self.ensure_root()?;
        let bytes = serde_json::to_vec(intent).map_err(|_| {
            HostError::Storage("archive publication intent cannot be encoded".into())
        })?;
        harw_session_store::store::persist_noclobber(&self.archive_intent_path(id)?, &bytes)?;
        Ok(())
    }

    /// Persist deletion of the hidden record before dropping its gate. A crash
    /// must never resurrect an unpublished record without its intent.
    pub(crate) fn remove_staged_archive_record(&self, id: &SessionId) -> Result<(), HostError> {
        self.remove(id)?;
        std::fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    pub(crate) fn remove_archive_intent(&self, id: &SessionId) -> Result<(), HostError> {
        match std::fs::remove_file(self.archive_intent_path(id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn archive_intent_path(&self, id: &SessionId) -> Result<PathBuf, HostError> {
        validate_file_id(id.as_str())?;
        Ok(self.root.join(format!("{}.archive-intent", id.as_str())))
    }

    pub(crate) fn load_archive_intent(
        &self,
        id: &SessionId,
    ) -> Result<Option<ArchivePublicationIntent>, HostError> {
        let path = self.archive_intent_path(id)?;
        let file = match harw_fsutil::open_nofollow(&path, harw_fsutil::OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            return Err(HostError::Storage(
                "archive publication intent is not a file".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(8 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 {
            return Err(HostError::Storage(
                "archive publication intent is too large".into(),
            ));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| HostError::Storage("archive publication intent is malformed".into()))
    }

    fn archive_publication_ready(&self, id: &SessionId) -> Result<bool, HostError> {
        Ok(self
            .load_archive_intent(id)?
            .is_none_or(|intent| intent.is_published(id)))
    }

    /// Read the record of `id`; `Ok(None)` when there is none.
    pub fn load(&self, id: &SessionId) -> Result<Option<HostedSessionRecord>, HostError> {
        let path = self.record_path(id)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record = decode(&bytes, &path)?;
        if record.session_id != *id {
            return Err(HostError::Storage(format!(
                "{}: record belongs to session `{}`",
                path.display(),
                record.session_id
            )));
        }
        Ok(self.archive_publication_ready(id)?.then_some(record))
    }

    /// Read one bounded durable row for owner status only. Do not use the
    /// normal archive publication verifier here: that would read a transcript
    /// prefix. An outstanding archive intent or concurrently replaced row is
    /// not eligible, even when its contents would otherwise authorize access.
    pub(crate) fn load_for_owner_observation(
        &self,
        id: &SessionId,
    ) -> Result<Option<HostedSessionRecord>, HostError> {
        use harw_fsutil::{
            OpenMode, ensure_owned_directory, ensure_private_regular, entry_type_at,
            open_beneath, open_dir_nofollow, same_file_at,
        };

        let record_path = self.record_path(id)?;
        let intent_path = self.archive_intent_path(id)?;
        let record = record_path.file_name().ok_or_else(|| {
            HostError::Storage("owner observation record has no filename".into())
        })?;
        let intent = intent_path.file_name().ok_or_else(|| {
            HostError::Storage("owner observation intent has no filename".into())
        })?;
        let dir = match open_dir_nofollow(&self.root) {
            Ok(fd) => File::from(fd),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure_owned_directory(&dir)?;
        let file = match open_beneath(dir.as_fd(), Path::new(record), OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure_private_regular(&file)?;
        if file.metadata()?.len() > MAX_OWNER_OBSERVATION_RECORD_BYTES {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        (&file).take(MAX_OWNER_OBSERVATION_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_OWNER_OBSERVATION_RECORD_BYTES
            || entry_type_at(dir.as_fd(), Path::new(intent))?.is_some()
            || !same_file_at(dir.as_fd(), Path::new(record), &file)?
        {
            return Ok(None);
        }
        let row = decode(&bytes, &record_path)?;
        Ok((row.session_id == *id).then_some(row))
    }

    /// Every readable record, sorted by `created_at`, then id. A missing
    /// directory is an empty store; a corrupt file is skipped with a warning
    /// so one bad record does not hide all others.
    pub fn list(&self) -> Result<Vec<HostedSessionRecord>, HostError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            // `DirEntry::file_type` does not follow symlinks: only plain
            // files count as records.
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(RECORD_EXT) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_file_id(stem).is_err() {
                tracing::warn!(path = %path.display(), "skipping record with invalid file name");
                continue;
            }
            let record = match std::fs::read(&path)
                .map_err(HostError::from)
                .and_then(|bytes| decode(&bytes, &path))
            {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "skipping unreadable hosted session record");
                    continue;
                }
            };
            if record.session_id.as_str() != stem {
                tracing::warn!(
                    path = %path.display(),
                    session = %record.session_id,
                    "skipping hosted session record stored under a foreign file name"
                );
                continue;
            }
            if self
                .archive_publication_ready(&record.session_id)
                .unwrap_or(false)
            {
                records.push(record);
            }
        }
        records.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.session_id.as_str().cmp(b.session_id.as_str()))
        });
        Ok(records)
    }

    /// Bump the persistent host epoch and return the new value (first call on
    /// a fresh store returns 1).
    pub fn next_epoch(&self) -> Result<u64, HostError> {
        let path = self.root.join(EPOCH_FILE);
        let current = match std::fs::read_to_string(&path) {
            Ok(text) => text.trim().parse::<u64>().map_err(|error| {
                HostError::Storage(format!("{}: corrupt epoch: {error}", path.display()))
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let next = current
            .checked_add(1)
            .ok_or_else(|| HostError::Storage(format!("{}: epoch overflow", path.display())))?;
        self.ensure_root()?;
        harw_fsutil::write_atomic(
            &path,
            format!("{next}\n").as_bytes(),
            harw_fsutil::AtomicWriteOptions::private(),
        )?;
        Ok(next)
    }

    /// Restart recovery (PL-65 §2.2 "Reclaim and restart recovery"): no turn
    /// survives a host restart, so every `Running` or `Queued` session becomes
    /// `Interrupted`, and every record is claimed for `epoch`. Returns the
    /// ids that were interrupted.
    pub fn recover_after_restart(&self, epoch: u64) -> Result<Vec<SessionId>, HostError> {
        let mut interrupted = Vec::new();
        for mut record in self.list()? {
            let mut changed = false;
            if matches!(
                record.state,
                HostedState::Running | HostedState::Queued { .. }
            ) {
                record.state = HostedState::Interrupted;
                interrupted.push(record.session_id.clone());
                changed = true;
            }
            if record.host_epoch != epoch {
                record.host_epoch = epoch;
                changed = true;
            }
            if changed {
                self.save(&record)?;
            }
        }
        Ok(interrupted)
    }

    /// `<root>/<id>.json` after validating `id` as a file name.
    fn record_path(&self, id: &SessionId) -> Result<PathBuf, HostError> {
        validate_file_id(id.as_str())?;
        Ok(self.root.join(format!("{}.{RECORD_EXT}", id.as_str())))
    }

    /// Create the record directory (mode `0700` on unix).
    fn ensure_root(&self) -> Result<(), HostError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.root)?;
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&self.root)?;
        }
        Ok(())
    }
}

/// Accept `id` as a file name only when it is 1..=128 chars of
/// `[A-Za-z0-9._-]` and does not start with a dot (rules out `.`, `..` and
/// hidden files).
fn validate_file_id(id: &str) -> Result<(), HostError> {
    let valid_chars = id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if id.is_empty() || id.len() > MAX_ID_LEN || !valid_chars || id.starts_with('.') {
        return Err(HostError::Protocol(format!(
            "session id `{}` is not a valid record name",
            id.escape_debug()
        )));
    }
    Ok(())
}

/// Decode one record file.
fn decode(bytes: &[u8], path: &Path) -> Result<HostedSessionRecord, HostError> {
    serde_json::from_slice(bytes)
        .map_err(|error| HostError::Storage(format!("{}: corrupt record: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn record(
        id: &str,
        state: HostedState,
        created_second: i64,
    ) -> Result<HostedSessionRecord, Box<dyn std::error::Error>> {
        let created_at = jiff::Timestamp::from_second(created_second)?;
        Ok(HostedSessionRecord {
            session_id: SessionId::try_from_str(id)?,
            tenant: None,
            owner_principal: "uid:1000".into(),
            fresh_human_origin: false,
            creator_execution: None,
            manual_cloud_work_receipt: None,
            exact_rewrites: Vec::new(),
            workspace: Some("/work".into()),
            title: Some(format!("session {id}")),
            created_at,
            updated_at: created_at,
            state,
            host_epoch: 0,
            generation: 1,
            owner_agent: None,
            tool_agents: Vec::new(),
        })
    }

    #[test]
    fn creator_execution_revalidation_refuses_legacy_and_identity_mismatch() -> TestResult {
        let mut session = record("creator", HostedState::Idle, 100)?;
        let creator = crate::identity::test_identity::local(1000, PermissionTier::Operator);
        assert!(
            session
                .revalidate_creator_execution(&creator, PermissionTier::Owner)
                .is_err()
        );
        session.creator_execution = Some(CreatorExecutionRequest {
            binding: Some(CreatorExecutionBinding::from_identity(&creator)),
            maximum_tier: PermissionTier::Owner,
            approval_actor: creator.actor.clone(),
        });
        let narrowed = session.revalidate_creator_execution(&creator, PermissionTier::Observer)?;
        assert_eq!(narrowed.maximum_tier, PermissionTier::Observer);
        assert_eq!(narrowed.approval_actor, creator.actor);
        let other = crate::identity::test_identity::local(1001, PermissionTier::Owner);
        assert!(
            session
                .revalidate_creator_execution(&other, PermissionTier::Owner)
                .is_err()
        );
        let mut actor_changed = creator;
        actor_changed.actor = ApprovalActor::Operator {
            id: "other".to_owned(),
        };
        assert!(
            session
                .revalidate_creator_execution(&actor_changed, PermissionTier::Owner)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn creator_execution_rejects_changed_provenance() -> TestResult {
        let mut session = record("creator", HostedState::Idle, 100)?;
        let creator = crate::identity::test_identity::local(1000, PermissionTier::Operator);
        session.creator_execution = Some(CreatorExecutionRequest {
            binding: Some(CreatorExecutionBinding::from_identity(&creator)),
            maximum_tier: creator.principal.tier(),
            approval_actor: creator.actor.clone(),
        });
        let mut changed = creator.clone();
        changed.principal = harw_types::Principal::trusted_ingress(
            harw_types::PrincipalKind::Human,
            creator.principal.id(),
            harw_types::IngressSurface::Cli,
            creator.principal.tier(),
        );
        assert!(
            session
                .revalidate_creator_execution(&changed, PermissionTier::Owner)
                .is_err()
        );
        changed = creator.clone();
        changed.device = Some(DeviceId::try_from_str("other-device")?);
        assert!(
            session
                .revalidate_creator_execution(&changed, PermissionTier::Owner)
                .is_err()
        );
        changed = creator.clone();
        changed.authenticated_node = Some(harw_types::NodeId::try_from_str("different-node")?);
        assert!(
            session
                .revalidate_creator_execution(&changed, PermissionTier::Owner)
                .is_err()
        );
        changed = creator.clone();
        changed.authenticated_key_fingerprint = Some([7; 32]);
        assert!(
            session
                .revalidate_creator_execution(&changed, PermissionTier::Owner)
                .is_err()
        );
        session
            .creator_execution
            .as_mut()
            .ok_or("missing request")?
            .binding = None;
        assert!(
            session
                .revalidate_creator_execution(&creator, PermissionTier::Owner)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn creator_execution_pins_its_peer_key_without_pinning_other_peers() -> TestResult {
        let mut session = record("creator", HostedState::Idle, 100)?;
        let mut creator = crate::identity::test_identity::local(1000, PermissionTier::Operator);
        creator.authenticated_node = Some(harw_types::NodeId::try_from_str("same-node")?);
        creator.authenticated_key_fingerprint = Some([3; 32]);
        session.creator_execution = Some(CreatorExecutionRequest {
            binding: Some(CreatorExecutionBinding::from_identity(&creator)),
            maximum_tier: creator.principal.tier(),
            approval_actor: creator.actor.clone(),
        });
        let mut fresh = creator.clone();
        fresh.connection = crate::identity::ConnectionId::next();
        // Reopening with the same verified peer key remains eligible, even if
        // unrelated trusted peers were added to the verifier's key store.
        assert!(
            session
                .revalidate_creator_execution(&fresh, PermissionTier::Owner)
                .is_ok()
        );
        fresh.authenticated_key_fingerprint = Some([4; 32]);
        assert!(
            session
                .revalidate_creator_execution(&fresh, PermissionTier::Owner)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn creator_execution_request_round_trips_and_legacy_stays_absent() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let mut session = record("creator", HostedState::Idle, 100)?;
        assert!(
            serde_json::to_value(&session)?
                .get("creator_execution")
                .is_none()
        );
        session.creator_execution = Some(CreatorExecutionRequest {
            binding: None,
            maximum_tier: PermissionTier::Operator,
            approval_actor: ApprovalActor::Operator {
                id: "device:one".to_owned(),
            },
        });
        store.save(&session)?;
        assert_eq!(store.load(&session.session_id)?, Some(session));
        let legacy = record("legacy", HostedState::Idle, 101)?;
        store.save(&legacy)?;
        assert_eq!(
            store
                .load(&legacy.session_id)?
                .and_then(|r| r.creator_execution),
            None
        );
        Ok(())
    }

    #[test]
    fn records_without_tool_agents_keep_their_shape_and_admit_no_agent() -> TestResult {
        let plain = record("s-9", HostedState::Idle, 100)?;
        let json = serde_json::to_value(&plain)?;
        assert!(json.get("owner_agent").is_none());
        assert!(json.get("tool_agents").is_none());
        assert!(!plain.admits_tool_agent(&["agent:uia".to_owned()]));
        assert!(!plain.admits_tool_agent(&[]));

        let mut owned = plain.clone();
        owned.owner_agent = Some("agent:uia".into());
        owned.tool_agents = vec!["agent:other".into()];
        let chain = ["agent:w1".to_owned(), "agent:uia".to_owned()];
        assert!(owned.admits_tool_agent(&chain));
        assert!(owned.admits_tool_agent(&["agent:other".to_owned()]));
        assert!(!owned.admits_tool_agent(&["agent:uia-b".to_owned()]));
        let decoded: HostedSessionRecord = serde_json::from_value(serde_json::to_value(&owned)?)?;
        assert_eq!(decoded, owned);
        Ok(())
    }

    #[test]
    fn save_then_load_round_trips() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let mut original = record("s-1", HostedState::Queued { depth: 2 }, 100)?;
        original.tenant = Some(TenantId::try_from_str("acme")?);
        store.save(&original)?;
        assert_eq!(store.load(&original.session_id)?, Some(original.clone()));
        assert!(store.root().join("s-1.json").is_file());
        Ok(())
    }

    #[test]
    fn missing_record_loads_as_none() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        assert_eq!(store.load(&SessionId::try_from_str("absent")?)?, None);
        assert!(store.list()?.is_empty());
        Ok(())
    }

    #[test]
    fn path_traversal_ids_are_rejected() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let long = "a".repeat(MAX_ID_LEN + 1);
        for bad in ["../x", "a/b", ".hidden", ".", "..", long.as_str()] {
            let id = SessionId::try_from_str(bad)?;
            assert!(
                matches!(store.load(&id), Err(HostError::Protocol(_))),
                "{bad}"
            );
            let mut rec = record("ok", HostedState::Idle, 1)?;
            rec.session_id = id;
            assert!(
                matches!(store.save(&rec), Err(HostError::Protocol(_))),
                "{bad}"
            );
        }
        // Empty ids cannot come from `try_from_str`; the public field still
        // allows one.
        let empty = SessionId(String::new());
        assert!(matches!(store.load(&empty), Err(HostError::Protocol(_))));
        assert!(!dir.path().join("x.json").exists());
        assert!(!store.root().exists());
        Ok(())
    }

    #[test]
    fn corrupt_record_is_a_storage_error_on_load() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.save(&record("good", HostedState::Idle, 1)?)?;
        std::fs::write(store.root().join("good.json"), b"{ not json")?;
        assert!(matches!(
            store.load(&SessionId::try_from_str("good")?),
            Err(HostError::Storage(_))
        ));
        Ok(())
    }

    #[test]
    fn list_skips_corrupt_files_and_sorts() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.save(&record("late", HostedState::Idle, 300)?)?;
        store.save(&record("b-early", HostedState::Idle, 100)?)?;
        store.save(&record("a-early", HostedState::Idle, 100)?)?;
        std::fs::write(store.root().join("broken.json"), b"{ not json")?;
        std::fs::write(store.root().join("notes.txt"), b"ignored")?;
        store.next_epoch()?;
        let ids: Vec<String> = store
            .list()?
            .into_iter()
            .map(|rec| rec.session_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["a-early", "b-early", "late"]);
        Ok(())
    }

    #[test]
    fn next_epoch_increments_across_instances() -> TestResult {
        let dir = tempfile::tempdir()?;
        assert_eq!(RecordStore::new(dir.path()).next_epoch()?, 1);
        assert_eq!(RecordStore::new(dir.path()).next_epoch()?, 2);
        assert_eq!(RecordStore::new(dir.path()).next_epoch()?, 3);
        Ok(())
    }

    #[test]
    fn corrupt_epoch_is_a_storage_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.next_epoch()?;
        std::fs::write(store.root().join(EPOCH_FILE), b"not a number")?;
        assert!(matches!(store.next_epoch(), Err(HostError::Storage(_))));
        Ok(())
    }

    #[test]
    fn recovery_interrupts_running_and_queued_only() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.save(&record("run", HostedState::Running, 1)?)?;
        store.save(&record("queue", HostedState::Queued { depth: 1 }, 2)?)?;
        store.save(&record("idle", HostedState::Idle, 3)?)?;
        let mut interrupted = store.recover_after_restart(7)?;
        interrupted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(
            interrupted,
            [
                SessionId::try_from_str("queue")?,
                SessionId::try_from_str("run")?
            ]
        );
        for rec in store.list()? {
            assert_eq!(rec.host_epoch, 7);
            let expected = if rec.session_id.as_str() == "idle" {
                HostedState::Idle
            } else {
                HostedState::Interrupted
            };
            assert_eq!(rec.state, expected, "{}", rec.session_id);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directory_are_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.save(&record("mode", HostedState::Idle, 1)?)?;
        store.next_epoch()?;
        let mode = |path: &Path| -> io::Result<u32> {
            Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
        };
        assert_eq!(mode(&store.root().join("mode.json"))?, 0o600);
        assert_eq!(mode(&store.root().join(EPOCH_FILE))?, 0o600);
        assert_eq!(mode(store.root())?, 0o700);
        Ok(())
    }

    #[test]
    fn create_if_absent_creates_once() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let rec = record("c-1", HostedState::Idle, 10)?;
        assert_eq!(store.create_if_absent(&rec)?, CreateOutcome::Created);
        assert_eq!(store.load(&rec.session_id)?, Some(rec));
        Ok(())
    }

    #[test]
    fn second_create_returns_existing_unchanged() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let first = record("c-2", HostedState::Idle, 10)?;
        let mut second = record("c-2", HostedState::Queued { depth: 1 }, 20)?;
        second.title = Some("other".into());
        assert_eq!(store.create_if_absent(&first)?, CreateOutcome::Created);
        assert_eq!(
            store.create_if_absent(&second)?,
            CreateOutcome::Existing(Box::new(first.clone()))
        );
        assert_eq!(store.load(&first.session_id)?, Some(first));
        Ok(())
    }

    #[test]
    fn concurrent_create_has_one_winner() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        let mut handles = Vec::new();
        for n in 0..8_i64 {
            let store = store.clone();
            let mut rec = record("c-3", HostedState::Idle, 10 + n)?;
            rec.title = Some(format!("writer {n}"));
            handles.push(std::thread::spawn(move || {
                store
                    .create_if_absent(&rec)
                    .map(|outcome| (rec.title.clone(), outcome))
            }));
        }
        let mut created = Vec::new();
        let mut existing = Vec::new();
        for handle in handles {
            let (title, outcome) = handle.join().map_err(|_| "thread panicked")??;
            match outcome {
                CreateOutcome::Created => created.push(title),
                CreateOutcome::Existing(rec) => existing.push(rec.title),
            }
        }
        assert_eq!(created.len(), 1);
        assert_eq!(existing.len(), 7);
        assert!(existing.iter().all(|title| *title == created[0]));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn created_file_is_private() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let store = RecordStore::new(dir.path());
        store.create_if_absent(&record("c-4", HostedState::Idle, 1)?)?;
        let mode = std::fs::metadata(store.root().join("c-4.json"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        Ok(())
    }
}

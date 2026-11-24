//! Read only the selected attachment in the authoritative session/profile.
//! A transfer never accepts a filesystem path, URL, or client-supplied digest.
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use harw_media::{MediaError, MediaSource};
use harw_protocol::{
    MediaRef,
    items::{ContentPart, TurnItem},
    session_wire::{
        MAX_MEDIA_GET_BYTES, MEDIA_PREVIEW_MAX_BYTES, MEDIA_PREVIEW_PART_BYTES,
        MediaGetParams, MediaGetResult, SessionMediaModeV1, SessionMediaPreviewReplyV1 as Reply,
        SessionMediaPreviewV1, SessionMediaSourceV1,
    },
};
use harw_session_store::{
    RecordKind, SessionStoreError, TranscriptReader, TranscriptStore,
};
use harw_types::{ItemId, SessionId, TenantId, ThreadRef};
use tokio::sync::{Semaphore, SemaphorePermit};

use super::{HostConnection, HostError, HostedState, Need, Slot, admit};
use crate::archived_fork::{
    ArchivedCheckpointV1, ArchivedForkError, ArchivedSourceFenceV1, media_in_checkpoint_v1,
};

const MAX_SCAN_BYTES: u64 = 24 * 1024 * 1024;
const MAX_SCAN_RECORDS: usize = 32_768;
const MAX_ACTIVE_TRANSFERS: usize = 4;
const TRANSFER_LIFETIME: Duration = Duration::from_secs(90);
static ADMISSION: Semaphore = Semaphore::const_new(MAX_ACTIVE_TRANSFERS);

pub(super) struct PreviewState {
    negotiated: AtomicBool,
    request: tokio::sync::Mutex<()>,
    lease: Arc<Mutex<Option<Transfer>>>,
}

impl PreviewState {
    pub(super) fn new() -> Self {
        Self {
            negotiated: AtomicBool::new(false),
            request: tokio::sync::Mutex::new(()),
            lease: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn set_negotiated(&self, negotiated: bool) {
        self.negotiated.store(negotiated, Ordering::Release);
    }
}

struct Scope {
    slot: Arc<Slot>,
    generation: u32,
    tenant: Option<TenantId>,
    owner: String,
    ephemeral: bool,
    root: std::path::PathBuf,
    thread: ThreadRef,
    published_next: Option<u64>,
    active: bool,
}

impl Scope {
    fn same_authority(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot)
            && self.generation == other.generation
            && self.tenant == other.tenant
            && self.owner == other.owner
            && self.ephemeral == other.ephemeral
            && self.root == other.root
            && self.thread == other.thread
    }

    fn same_canonical_revision(&self, other: &Self) -> bool {
        self.published_next == other.published_next
            && (self.published_next.is_some() || (!self.active && !other.active))
    }
}

enum Fence {
    Canonical(TranscriptReader),
    Archive(ArchivedSourceFenceV1),
}

impl Fence {
    fn valid(&self, root: &std::path::Path, session: &SessionId) -> bool {
        let store = TranscriptStore::new(root);
        match self {
            Self::Canonical(reader) => store.snapshot_is_current(session, reader).unwrap_or(false),
            Self::Archive(fence) => fence.still_current(&store, session).unwrap_or(false),
        }
    }
}

struct Image {
    media: MediaRef,
    fence: Fence,
}

struct Transfer {
    source: SessionMediaPreviewV1,
    scope: Scope,
    checkpoint: Option<ArchivedCheckpointV1>,
    image: Image,
    bytes: Vec<u8>,
    next: usize,
    expires: tokio::time::Instant,
    ticket: String,
    _permit: SemaphorePermit<'static>,
}

impl Transfer {
    fn reply(&mut self) -> Reply {
        let offset = self.next;
        self.next = (offset + MEDIA_PREVIEW_PART_BYTES).min(self.bytes.len());
        Reply::Part {
            mime: self.image.media.format().mime().to_owned(),
            total_bytes: self.bytes.len() as u32,
            offset: offset as u32,
            data_b64: STANDARD.encode(&self.bytes[offset..self.next]),
            continuation: (self.next < self.bytes.len()).then(|| self.ticket.clone()),
        }
    }

    fn matches(&self, params: &SessionMediaPreviewV1) -> bool {
        self.source.session_id == params.session_id
            && self.source.source == params.source
            && self.source.attachment_index == params.attachment_index
            && self.source.mode == params.mode
            && params.continuation.as_deref() == Some(self.ticket.as_str())
            && tokio::time::Instant::now() < self.expires
    }
}

enum Selector<'a> {
    Occurrence(&'a ItemId, usize),
    Legacy(&'a MediaRef),
}

impl HostConnection {
    fn media_scope(&self, session: &SessionId) -> Result<Scope, HostError> {
        if self.identity.agent.is_some() {
            return Err(HostError::Denied("media is unavailable for this client".into()));
        }
        // Previews are for an already observed chat. Do not accidentally
        // kick off the uncapped profile replay/head discovery for a new ID.
        let granted = self.granted()?;
        let slot = self.host.loaded_slot(session).ok_or(HostError::NotFound)?;
        let share = match self.host.share() {
            Some(_) => Some(self.host.share_for(self.identity.tenant.as_ref())
                .ok_or(HostError::NotFound)?),
            None => None,
        };
        let (generation, tenant, owner, ephemeral, active) = {
            let state = slot.lock()?;
            (
                state.record.generation,
                state.record.tenant.clone(),
                state.record.owner_principal.clone(),
                state.ephemeral,
                matches!(state.record.state, HostedState::Running | HostedState::Queued { .. })
                    || state.parked,
            )
        };
        admit(&self.identity, granted, tenant.as_ref(), Need::Observe)?;
        let binding = self.host.driver.archive_journal_binding(session)?;
        let root = std::fs::canonicalize(&binding.root)?;
        if let Some(share) = share
            && root != std::fs::canonicalize(share.index.sessions_root())?
        {
            return Err(HostError::Denied("media source is outside this profile".into()));
        }
        if ephemeral {
            let share = share.ok_or(HostError::NotFound)?;
            let store = TranscriptStore::new(&root);
            let metadata = std::fs::symlink_metadata(store.transcript_path(session)?)?;
            if !metadata.is_file() || metadata.len() > MAX_SCAN_BYTES
                || share.index.row(session)?.is_none()
            {
                return Err(HostError::NotFound);
            }
        }
        let published_next = self.host.driver.archive_published_head(session)?
            .map(std::num::NonZeroU64::get);
        Ok(Scope {
            slot, generation, tenant, owner, ephemeral, root, thread: binding.thread,
            published_next, active,
        })
    }

    async fn media_checkpoint(
        &self,
        request: &SessionMediaPreviewV1,
    ) -> Result<Option<ArchivedCheckpointV1>, HostError> {
        match &request.source {
            SessionMediaSourceV1::Canonical { .. } => Ok(None),
            SessionMediaSourceV1::Archive { checkpoint_id, .. } => self
                .archive_preview_checkpoint(&request.session_id, checkpoint_id).await.map(Some),
        }
    }

    pub(super) async fn preview_session_media(
        &self, params: SessionMediaPreviewV1,
    ) -> Result<Reply, HostError> {
        self.granted()?;
        if !self.media_preview.negotiated.load(Ordering::Acquire) {
            return Ok(Reply::Unsupported);
        }
        let Ok(_serial) = self.media_preview.request.try_lock() else {
            return Ok(Reply::Busy);
        };
        let scope = self.media_scope(&params.session_id)?;
        let checkpoint = self.media_checkpoint(&params).await?;

        if params.continuation.is_some() {
            let mut lease = self.media_preview.lease.lock()
                .map_err(|_| HostError::Storage("media preview state unavailable".into()))?;
            let Some(transfer) = lease.as_mut() else {
                return Ok(Reply::Busy);
            };
            if !transfer.matches(&params)
                || transfer.checkpoint != checkpoint
                || !transfer.scope.same_authority(&scope)
            {
                return Ok(Reply::Busy);
            }
            if (checkpoint.is_none() && !transfer.scope.same_canonical_revision(&scope))
                || !transfer.image.fence.valid(&scope.root, &params.session_id)
            {
                *lease = None;
                return Ok(Reply::Busy);
            }
            let reply = transfer.reply();
            if transfer.next == transfer.bytes.len() {
                *lease = None;
            }
            return Ok(reply);
        }

        // A fresh request may replace a previous transfer on this same
        // connection. Mobile serializes requests for a visible image.
        self.media_preview.lease.lock()
            .map_err(|_| HostError::Storage("media preview state unavailable".into()))?.take();
        let Ok(permit) = ADMISSION.try_acquire() else { return Ok(Reply::Busy); };
        let driver = Arc::clone(&self.host.driver);
        let read_scope = scope;
        let query = params.clone();
        let read_checkpoint = checkpoint.clone();
        let (scope, image, bytes, permit) = match tokio::task::spawn_blocking(move || {
            let image = resolve_preview(&read_scope, &query, read_checkpoint.as_ref())?;
            if image.media.bytes() > MEDIA_PREVIEW_MAX_BYTES as u64 {
                return Err(Reply::Unreadable);
            }
            let bytes = if query.mode == SessionMediaModeV1::Thumbnail
                && image.media.bytes() > MEDIA_PREVIEW_PART_BYTES as u64
            {
                None
            } else {
                let store = driver.archive_media_store().ok_or(Reply::Unsupported)?;
                Some(store.load(&image.media).map_err(media_failure)?)
            };
            Ok::<_, Reply>((read_scope, image, bytes, permit))
        }).await.map_err(|_| HostError::Storage("media preview reader stopped".into()))? {
            Ok(resolved) => resolved,
            Err(status) => {
                self.media_scope(&params.session_id)?;
                self.media_checkpoint(&params).await?;
                return Ok(status);
            }
        };
        let now = self.media_scope(&params.session_id)?;
        if !scope.same_authority(&now)
            || (checkpoint.is_none() && !scope.same_canonical_revision(&now))
            || checkpoint != self.media_checkpoint(&params).await?
            || !image.fence.valid(&scope.root, &params.session_id)
        {
            return Ok(Reply::Busy);
        }
        let Some(bytes) = bytes else { return Ok(Reply::OpenRequired); };
        let ticket = SessionId::new().as_str().to_owned();
        let expires = tokio::time::Instant::now() + TRANSFER_LIFETIME;
        let mut transfer = Transfer {
            source: params,
            scope, checkpoint, image, bytes, next: 0, expires, ticket: ticket.clone(),
            _permit: permit,
        };
        let reply = transfer.reply();
        if transfer.next < transfer.bytes.len() {
            *self.media_preview.lease.lock()
                .map_err(|_| HostError::Storage("media preview state unavailable".into()))? =
                Some(transfer);
            let slot = Arc::downgrade(&self.media_preview.lease);
            tokio::spawn(async move {
                tokio::time::sleep_until(expires).await;
                if let Some(slot) = slot.upgrade()
                    && let Ok(mut lease) = slot.lock()
                    && lease.as_ref().is_some_and(|lease| lease.ticket == ticket)
                {
                    *lease = None;
                }
            });
        }
        Ok(reply)
    }

    /// Keep the existing v1 media.get shape and size, but never treat an
    /// arbitrary known digest as a capability beyond this transcript.
    pub(super) async fn read_legacy_session_media(
        &self,
        params: MediaGetParams,
    ) -> Result<MediaGetResult, HostError> {
        let scope = self.media_scope(&params.session_id)?;
        if params.media.bytes() > MAX_MEDIA_GET_BYTES {
            return Err(HostError::Protocol("image too large for media.get".into()));
        }
        let permit = ADMISSION.try_acquire()
            .map_err(|_| HostError::Busy("media reader busy".into()))?;
        let driver = Arc::clone(&self.host.driver);
        let query = params.clone();
        let (scope, image, bytes) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let image = scan_canonical(&scope, &query.session_id, Selector::Legacy(&query.media))
                .map_err(|failure| match failure {
                    Reply::Busy => HostError::Busy("media source changed".into()),
                    Reply::Missing => HostError::NotFound,
                    _ => HostError::Denied("no verified media in this session".into()),
                })?;
            let store = driver.archive_media_store().ok_or(HostError::NotImplemented("session media"))?;
            let bytes = store.load(&image.media).map_err(|err| match err {
                MediaError::NotFound => HostError::NotFound,
                _ => HostError::Storage("session media is unreadable".into()),
            })?;
            Ok::<_, HostError>((scope, image, bytes))
        }).await.map_err(|_| HostError::Storage("media reader stopped".into()))??;
        let now = self.media_scope(&params.session_id)?;
        if !scope.same_authority(&now) || !scope.same_canonical_revision(&now)
            || !image.fence.valid(&scope.root, &params.session_id)
        {
            return Err(HostError::Busy("media source changed".into()));
        }
        Ok(MediaGetResult {
            mime: image.media.format().mime().to_owned(),
            data_b64: STANDARD.encode(bytes),
        })
    }
}

fn media_failure(error: MediaError) -> Reply {
    match error {
        MediaError::NotFound => Reply::Missing,
        _ => Reply::Unreadable,
    }
}

fn resolve_preview(
    scope: &Scope,
    query: &SessionMediaPreviewV1,
    checkpoint: Option<&ArchivedCheckpointV1>,
) -> Result<Image, Reply> {
    let index = usize::try_from(query.attachment_index).map_err(|_| Reply::Missing)?;
    match (&query.source, checkpoint) {
        (SessionMediaSourceV1::Canonical { item_id }, None) => {
            scan_canonical(scope, &query.session_id, Selector::Occurrence(item_id, index))
        }
        (SessionMediaSourceV1::Archive { row, .. }, Some(proof)) => {
            let (media, fence) = media_in_checkpoint_v1(
                &TranscriptStore::new(&scope.root), &query.session_id, proof, row, index,
            ).map_err(|err| match err {
                ArchivedForkError::ClickNotVisible | ArchivedForkError::Unavailable => Reply::Missing,
                ArchivedForkError::Changed => Reply::Busy,
                _ => Reply::Unreadable,
            })?;
            Ok(Image { media, fence: Fence::Archive(fence) })
        }
        _ => Err(Reply::Unreadable),
    }
}

fn scan_canonical(scope: &Scope, source: &SessionId, selector: Selector<'_>) -> Result<Image, Reply> {
    if scope.published_next.is_none() && scope.active {
        return Err(Reply::Busy);
    }
    if scope.published_next.is_some_and(|head| head > MAX_SCAN_RECORDS as u64) {
        return Err(Reply::Unreadable);
    }
    let store = TranscriptStore::new(&scope.root);
    let mut reader = store.snapshot_reader(source, MAX_SCAN_BYTES).map_err(|err| match err {
        SessionStoreError::NotFound { .. } => Reply::Missing,
        _ => Reply::Unreadable,
    })?;
    // Only retain a candidate for the selected row, not every decoded
    // message. Keep markers for positional legacy replace-footer semantics.
    let mut matches: Vec<Option<Option<MediaRef>>> = Vec::new();
    let mut frame: Option<(usize, usize)> = None;
    let mut previous_len = 0;
    let mut seen_occurrence = None;
    let mut conflicting_occurrence = false;
    let mut seen = 0;
    let records = reader.by_ref();
    while scope.published_next.is_none_or(|head| seen < head) {
        let Some(record) = records.next() else { break; };
        if seen >= MAX_SCAN_RECORDS as u64 {
            return Err(Reply::Unreadable);
        }
        let record = record.map_err(|_| Reply::Unreadable)?;
        if record.sequence != seen || &record.session_id != source {
            return Err(Reply::Unreadable);
        }
        seen += 1;
        if record.thread != scope.thread {
            return Err(Reply::Unreadable);
        }
        let marker = (record.kind == RecordKind::Lifecycle)
            .then(|| record.payload.get("marker").and_then(serde_json::Value::as_str))
            .flatten();
        match marker {
            Some("history_replace_started") => {
                if frame.replace((matches.len(), frame_count(&record.payload)?)).is_some() {
                    return Err(Reply::Unreadable);
                }
            }
            Some("history_replaced") => {
                let count = frame_count(&record.payload)?;
                if let Some((before, expected)) = frame.take() {
                    if count != expected || matches.len().saturating_sub(before) != count {
                        return Err(Reply::Unreadable);
                    }
                } else if matches.len().saturating_sub(previous_len) < count {
                    return Err(Reply::Unreadable);
                }
                matches.drain(..matches.len() - count);
                previous_len = matches.len();
            }
            _ if record.kind == RecordKind::Item => {
                let item = serde_json::from_value::<TurnItem>(record.payload)
                    .map_err(|_| Reply::Unreadable)?;
                let selected = candidate(item, &selector);
                // A legacy UI can hold a stale ItemId. Reused IDs may be
                // copied by a rewrite, but a differing slot must never be
                // substituted as the previously shown occurrence.
                if matches!(selector, Selector::Occurrence(_, _))
                    && let Some(media) = &selected
                {
                    if let Some(old) = &seen_occurrence {
                        conflicting_occurrence |= old != media;
                    } else {
                        seen_occurrence = Some(media.clone());
                    }
                }
                matches.push(selected);
            }
            _ if frame.is_some() => return Err(Reply::Unreadable),
            _ => {}
        }
    }
    if frame.is_some() || scope.published_next.is_some_and(|head| head != seen) {
        return Err(Reply::Busy);
    }
    if !store.snapshot_is_current(source, &reader).map_err(|_| Reply::Unreadable)? {
        return Err(Reply::Busy);
    }
    let mut candidates = matches.into_iter().flatten();
    let found = candidates.next().ok_or(Reply::Missing)?;
    if conflicting_occurrence
        || (matches!(selector, Selector::Occurrence(_, _)) && candidates.next().is_some())
    {
        return Err(Reply::Unreadable);
    }
    let media = found.ok_or(Reply::Missing)?;
    Ok(Image { media, fence: Fence::Canonical(reader) })
}

fn frame_count(payload: &serde_json::Value) -> Result<usize, Reply> {
    payload.get("item_count").and_then(serde_json::Value::as_u64)
        .filter(|count| *count <= MAX_SCAN_RECORDS as u64)
        .map(|count| count as usize)
        .ok_or(Reply::Unreadable)
}

fn candidate(item: TurnItem, selector: &Selector<'_>) -> Option<Option<MediaRef>> {
    let (id, parts) = match item {
        TurnItem::UserMessage(message) => (message.id, Some(message.content)),
        TurnItem::AssistantMessage(message) => (message.id, None),
        TurnItem::Reasoning(message) => (message.id, None),
        TurnItem::ToolCall(message) => (message.id, None),
        TurnItem::ToolResult(message) => (message.id, None),
        TurnItem::Error(message) => (message.id, None),
    };
    let mut media = parts.into_iter().flatten().filter_map(|part| match part {
        ContentPart::Media { media, .. } => Some(media),
        _ => None,
    });
    match selector {
        Selector::Occurrence(target, index) if &id == *target => Some(media.nth(*index)),
        Selector::Occurrence(_, _) => None,
        Selector::Legacy(reference) => media.find(|media| media == *reference).map(Some),
    }
}

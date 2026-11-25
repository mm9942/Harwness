//! Read-only retained revision foundation for an inclusive archived fork.
//!
//! A replay cursor is not a revision ID. This V1 checkpoint commits to the
//! exact decoded record prefix, including order, source/thread, payload,
//! original journal timestamps and replacement frames. It is **not** an
//! authentication token. A later owner-host adapter must check tenant/profile
//! authority and use checkpoints it issued or loads from trusted metadata;
//! never deserialize a device request here and treat it as authority.
//!
//! A checkpoint covers a complete framed revision retained on disk. Physical
//! deletion/truncation/rewrite, old unframed replacement and ambiguity fail
//! closed. Timestamps describe the exact record occurrence, not the first time
//! an ItemId appeared in the source. No session, media bytes, model or tool
//! is opened or started by this module.

use std::collections::HashSet;
use std::num::NonZeroU64;

use harw_protocol::items::{ContentPart, TurnItem};
use harw_protocol::media::{ImageDetail, MediaRef};
use harw_protocol::session_wire::{
    ArchiveRowLocatorV1, ArchiveVerifiedRowV1, ArchiveVisibleRoleV1, ArchivedDisplayedPrefixV1,
    DisplayedForkRoleV1, DisplayedForkRowV1, DISPLAYED_FORK_MAX_ROWS, archived_displayed_prefix_v1,
    displayed_fork_body_v1,
};
use harw_session_store::{
    RecordKind, SessionStoreError, TranscriptReader, TranscriptRecord, TranscriptStore,
};
use harw_types::{ItemId, SessionId, ThreadRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const MAX_SCAN_BYTES: u64 = 24 * 1024 * 1024;
const MAX_RECORDS: u64 = 32_768;
const MAX_EXPORT_ROWS: usize = 512;
const MAX_EXPORT_BYTES: usize = 2 * 1024 * 1024;
const START: &str = "history_replace_started";
const END: &str = "history_replaced";
const DIGEST_DOMAIN: &[u8] = b"harw/archive-prefix/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchiveFormat {
    ArchivedPrefixV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchivedCheckpointV1 {
    format: ArchiveFormat,
    source: SessionId,
    thread: ThreadRef,
    committed_next: NonZeroU64,
    digest: [u8; 32],
    /// If present, output from just before this START, proven by the whole
    /// successor frame ending at committed_next. Old receipts omit this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before_rewrite: Option<NonZeroU64>,
}

impl ArchivedCheckpointV1 {
    pub(crate) fn thread(&self) -> &ThreadRef {
        &self.thread
    }

    pub(crate) fn is_retained_predecessor(&self) -> bool {
        self.before_rewrite.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchivedRowV1 {
    pub(crate) sequence: u64,
    pub(crate) id: ItemId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ArchivedPrefixV1 {
    format: ArchiveFormat,
    source: SessionId,
    through: u64,
    rows: Vec<ExportRowV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ExportRowV1 {
    id: ItemId,
    source_sequence: u64,
    source_recorded_at: jiff::Timestamp,
    message: ExportMessageV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum ExportMessageV1 {
    User { content: Vec<ExportUserPartV1> },
    Assistant { text: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ExportUserPartV1 {
    Text {
        text: String,
    },
    StoredMedia {
        media: MediaRef,
        detail: Option<ImageDetail>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchivedForkError {
    WrongSource,
    ExceedsLimit,
    Unavailable,
    Changed,
    InvalidJournal,
    MissingFrame,
    UnverifiedRewrite,
    ClickNotVisible,
}

/// Make a checkpoint using a verified post-sync sequence from the source
/// writer or trusted persisted revision metadata, not file size/replay head.
pub(crate) fn capture_checkpoint_v1(
    store: &TranscriptStore,
    authorized_source: &SessionId,
    published_committed_next: NonZeroU64,
) -> Result<ArchivedCheckpointV1, ArchivedForkError> {
    let revision = read_revision(store, authorized_source, published_committed_next, None)?;
    Ok(ArchivedCheckpointV1 {
        format: ArchiveFormat::ArchivedPrefixV1,
        source: authorized_source.clone(),
        thread: revision.thread,
        committed_next: published_committed_next,
        digest: revision.digest,
        before_rewrite: None,
    })
}

/// Metadata from a verified, fully closed retained replacement. Timestamp is
/// the actual END record time, not wall time inferred from the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetainedRewriteV1 {
    pub(crate) before_sequence: NonZeroU64,
    pub(crate) committed_next: NonZeroU64,
    pub(crate) recorded_at: jiff::Timestamp,
    pub(crate) explicit_edit: bool,
}

/// Locate the latest fully verified predecessor below an exclusive
/// boundary. An older generic replacement remains useful, but must not be
/// labeled as an edit. A scan is never authority to turn the physical file
/// end into a durable writer cursor.
fn find_retained_rewrite_v1(
    store: &TranscriptStore,
    source: &SessionId,
    published_next: NonZeroU64,
    older_than: Option<NonZeroU64>,
) -> Result<Option<(RetainedRewriteV1, ArchivedSourceFenceV1)>, ArchivedForkError> {
    let mut latest = None;
    let mut on_rewrite = |rewrite: RetainedRewriteV1| {
        if older_than.is_none_or(|before| rewrite.before_sequence < before) {
            latest = Some(rewrite);
        }
    };
    match scan_revision(store, source, published_next, None, Some(&mut on_rewrite)) {
        Ok(revision) => Ok(latest.map(|rewrite| (rewrite, ArchivedSourceFenceV1(revision.reader)))),
        Err(ArchivedForkError::MissingFrame) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Capture the discovered predecessor, not the replacement contents. A
/// detected candidate is not enough: re-read its exact boundary and obtain
/// an immutable digest through the closed successor frame.
fn capture_retained_rewrite_v1(
    store: &TranscriptStore,
    source: &SessionId,
    boundary: RetainedRewriteV1,
) -> Result<ArchivedCheckpointV1, ArchivedForkError> {
    let mut verified = None;
    let revision = scan_revision(
        store,
        source,
        boundary.committed_next,
        Some(boundary.before_sequence),
        Some(&mut |observed| {
            if observed.committed_next == boundary.committed_next {
                verified = Some(observed);
            }
        }),
    )?;
    if verified != Some(boundary) {
        return Err(ArchivedForkError::Changed);
    }
    Ok(ArchivedCheckpointV1 {
        format: ArchiveFormat::ArchivedPrefixV1,
        source: source.clone(),
        thread: revision.thread,
        committed_next: boundary.committed_next,
        digest: revision.digest,
        before_rewrite: Some(boundary.before_sequence),
    })
}

/// Discover and seal one predecessor from a single stable physical journal
/// version under the writer's published horizon. No locks on the model,
/// session, or physical append path are taken.
pub(crate) fn capture_previous_rewrite_v1(
    store: &TranscriptStore,
    source: &SessionId,
    published_next: NonZeroU64,
    older_than: Option<NonZeroU64>,
) -> Result<Option<(RetainedRewriteV1, ArchivedCheckpointV1)>, ArchivedForkError> {
    let Some((boundary, fence)) = find_retained_rewrite_v1(store, source, published_next, older_than)?
    else {
        return Ok(None);
    };
    let checkpoint = capture_retained_rewrite_v1(store, source, boundary)?;
    if !fence.still_current(store, source)? {
        return Err(ArchivedForkError::Changed);
    }
    Ok(Some((boundary, checkpoint)))
}

/// Revalidate the exact proven revision without returning any row data.
pub(crate) fn verify_checkpoint_v1(
    store: &TranscriptStore,
    authorized_source: &SessionId,
    checkpoint: &ArchivedCheckpointV1,
) -> Result<(), ArchivedForkError> {
    if authorized_source != &checkpoint.source {
        return Err(ArchivedForkError::WrongSource);
    }
    let revision = read_revision(
        store,
        authorized_source,
        checkpoint.committed_next,
        checkpoint.before_rewrite,
    )?;
    if revision.digest != checkpoint.digest || revision.thread != checkpoint.thread {
        return Err(ArchivedForkError::Changed);
    }
    Ok(())
}

/// Return only verified user/assistant display rows. A page is in source
/// order; its exclusive continuation is the first returned exact sequence.
/// Images are counts only. The source scoped importer owns media validation.
pub(crate) fn page_checkpoint_v1(
    store: &TranscriptStore,
    source: &SessionId,
    checkpoint: &ArchivedCheckpointV1,
    before: Option<u64>,
    limit: u16,
) -> Result<(Vec<ArchiveVerifiedRowV1>, Option<u64>), ArchivedForkError> {
    const MAX_PAGE_BYTES: usize = 256 * 1024;
    if source != &checkpoint.source {
        return Err(ArchivedForkError::WrongSource);
    }
    let row_horizon = checkpoint.before_rewrite.unwrap_or(checkpoint.committed_next).get();
    let before = before.unwrap_or(row_horizon);
    if before > row_horizon || !(1..=64).contains(&limit) {
        return Err(ArchivedForkError::ExceedsLimit);
    }
    let revision = read_revision(
        store,
        source,
        checkpoint.committed_next,
        checkpoint.before_rewrite,
    )?;
    if revision.digest != checkpoint.digest || revision.thread != checkpoint.thread {
        return Err(ArchivedForkError::Changed);
    }
    let mut rows: Vec<ArchiveVerifiedRowV1> = Vec::new();
    let mut bytes = 0_usize;
    let mut next = None;
    for row in revision
        .items
        .into_iter()
        .rev()
        .filter(|item| item.sequence < before)
        .filter_map(RecordedItem::into_visible_row)
        .map(ExportRowV1::into_wire_row)
    {
        let size = serde_json::to_vec(&row)
            .map_err(|_| ArchivedForkError::InvalidJournal)?
            .len();
        if rows.len() == usize::from(limit) || bytes.saturating_add(size) > MAX_PAGE_BYTES {
            next = Some(
                rows.last()
                    .ok_or(ArchivedForkError::ExceedsLimit)?
                    .locator
                    .sequence,
            );
            break;
        }
        bytes += size;
        rows.push(row);
    }
    rows.reverse();
    Ok((rows, next))
}

impl ExportRowV1 {
    fn into_wire_row(self) -> ArchiveVerifiedRowV1 {
        let (role, text_parts, image_count) = match self.message {
            ExportMessageV1::User { content } => {
                let mut image_count = 0_u32;
                let mut text_parts = Vec::new();
                for part in content {
                    match part {
                        ExportUserPartV1::Text { text } => text_parts.push(text),
                        ExportUserPartV1::StoredMedia { .. } => {
                            image_count = image_count.saturating_add(1)
                        }
                    }
                }
                (ArchiveVisibleRoleV1::User, text_parts, image_count)
            }
            ExportMessageV1::Assistant { text } => (ArchiveVisibleRoleV1::Assistant, text, 0),
        };
        ArchiveVerifiedRowV1 {
            locator: ArchiveRowLocatorV1 {
                sequence: self.source_sequence,
                item_id: self.id,
            },
            role,
            text_parts,
            recorded_at: self.source_recorded_at,
            image_count,
        }
    }
}

/// Resolve one exact stored attachment from a host-issued verified row.
pub(crate) fn media_in_checkpoint_v1(
    store: &TranscriptStore,
    source: &SessionId,
    checkpoint: &ArchivedCheckpointV1,
    row: &ArchiveRowLocatorV1,
    attachment_index: usize,
) -> Result<(MediaRef, ArchivedSourceFenceV1), ArchivedForkError> {
    if source != &checkpoint.source {
        return Err(ArchivedForkError::WrongSource);
    }
    let revision = read_revision(
        store, source, checkpoint.committed_next, checkpoint.before_rewrite,
    )?;
    if revision.digest != checkpoint.digest || revision.thread != checkpoint.thread {
        return Err(ArchivedForkError::Changed);
    }
    let media = revision.items.iter()
        .find(|item| item.sequence == row.sequence && item.id() == &row.item_id)
        .and_then(|item| match &item.item {
            TurnItem::UserMessage(message) => message.content.iter().filter_map(|part| match part {
                ContentPart::Media { media, .. } => Some(media),
                _ => None,
            }).nth(attachment_index).cloned(),
            _ => None,
        })
        .ok_or(ArchivedForkError::ClickNotVisible)?;
    Ok((media, ArchivedSourceFenceV1(revision.reader)))
}

/// Verify a previously trusted checkpoint and include its exact clicked row.
/// Stored media are returned only as references found in user rows of this
/// source prefix. An eventual destination must prove its own authorized
/// access before importing blobs. There is no media/load API in this module.
pub(crate) fn export_inclusive_v1(
    store: &TranscriptStore,
    authorized_source: &SessionId,
    checkpoint: &ArchivedCheckpointV1,
    clicked: &ArchivedRowV1,
) -> Result<ArchivedPrefixV1, ArchivedForkError> {
    export_inclusive_guarded_v1(store, authorized_source, checkpoint, clicked)
        .map(|(prefix, _)| prefix)
}

/// The opened bounded byte horizon is already validated by the archive scan.
/// Before publication, confirm the scoped journal still matches that inode
/// and its metadata without holding a source writer or model lock.
pub(crate) struct ArchivedSourceFenceV1(TranscriptReader);

impl ArchivedSourceFenceV1 {
    pub(crate) fn still_current(
        &self,
        store: &TranscriptStore,
        source: &SessionId,
    ) -> Result<bool, ArchivedForkError> {
        store
            .snapshot_is_current(source, &self.0)
            .map_err(|_| ArchivedForkError::Unavailable)
    }
}

pub(crate) fn export_inclusive_guarded_v1(
    store: &TranscriptStore,
    authorized_source: &SessionId,
    checkpoint: &ArchivedCheckpointV1,
    clicked: &ArchivedRowV1,
) -> Result<(ArchivedPrefixV1, ArchivedSourceFenceV1), ArchivedForkError> {
    if authorized_source != &checkpoint.source {
        return Err(ArchivedForkError::WrongSource);
    }
    if clicked.sequence >= checkpoint.before_rewrite.unwrap_or(checkpoint.committed_next).get() {
        return Err(ArchivedForkError::ClickNotVisible);
    }
    let revision = read_revision(
        store,
        authorized_source,
        checkpoint.committed_next,
        checkpoint.before_rewrite,
    )?;
    if revision.digest != checkpoint.digest || revision.thread != checkpoint.thread {
        return Err(ArchivedForkError::Changed);
    }
    let through = revision
        .items
        .iter()
        .position(|item| item.sequence == clicked.sequence && item.id() == &clicked.id)
        .ok_or(ArchivedForkError::ClickNotVisible)?;
    let mut rows = Vec::new();
    let mut encoded_row_bytes = 0_usize;
    for record in revision.items.into_iter().take(through + 1) {
        if let Some(row) = record.into_visible_row() {
            let encoded =
                serde_json::to_vec(&row).map_err(|_| ArchivedForkError::InvalidJournal)?;
            encoded_row_bytes = encoded_row_bytes.saturating_add(encoded.len());
            if rows.len() >= MAX_EXPORT_ROWS || encoded_row_bytes > MAX_EXPORT_BYTES {
                return Err(ArchivedForkError::ExceedsLimit);
            }
            rows.push(row);
        }
    }
    if rows
        .last()
        .is_none_or(|row| row.source_sequence != clicked.sequence)
    {
        return Err(ArchivedForkError::ClickNotVisible);
    }
    let result = ArchivedPrefixV1 {
        format: ArchiveFormat::ArchivedPrefixV1,
        source: authorized_source.clone(),
        through: clicked.sequence,
        rows,
    };
    if serde_json::to_vec(&result)
        .map_err(|_| ArchivedForkError::InvalidJournal)?
        .len()
        > MAX_EXPORT_BYTES
    {
        return Err(ArchivedForkError::ExceedsLimit);
    }
    Ok((result, ArchivedSourceFenceV1(revision.reader)))
}

impl ArchivedPrefixV1 {
    /// History pagination can hide older rows. Seed only the exact contiguous
    /// displayed suffix through the independently verified clicked archive row.
    pub(crate) fn restrict_to_displayed(
        &mut self,
        proof: &ArchivedDisplayedPrefixV1,
    ) -> Result<(), ArchivedForkError> {
        let count = usize::from(proof.row_count);
        if !(1..=DISPLAYED_FORK_MAX_ROWS).contains(&count) {
            return Err(ArchivedForkError::ExceedsLimit);
        }
        let start = self.rows.len().checked_sub(count).ok_or(ArchivedForkError::Changed)?;
        let mut visible = Vec::with_capacity(count);
        for row in &self.rows[start..] {
            let mut refs = Vec::new();
            let (role, text) = match &row.message {
                ExportMessageV1::User { content } => {
                    let mut parts = Vec::with_capacity(content.len());
                    for part in content {
                        match part {
                            ExportUserPartV1::Text { text } => parts.push(text.as_str()),
                            ExportUserPartV1::StoredMedia { media, .. } => {
                                refs.push(media.clone());
                                parts.push("[image]");
                            }
                        }
                    }
                    (DisplayedForkRoleV1::User, parts.join("\n"))
                }
                ExportMessageV1::Assistant { text } => {
                    (DisplayedForkRoleV1::Assistant, text.join("\n"))
                }
            };
            let body = displayed_fork_body_v1(&text, &refs)
                .map_err(|_| ArchivedForkError::InvalidJournal)?;
            visible.push(DisplayedForkRowV1 {
                item_id: row.id.as_str().to_owned(),
                role,
                body_digest: *blake3::hash(&body).as_bytes(),
            });
        }
        let encoded = archived_displayed_prefix_v1(&visible)
            .map_err(|_| ArchivedForkError::InvalidJournal)?;
        if blake3::hash(&encoded).as_bytes() != &proof.digest {
            return Err(ArchivedForkError::Changed);
        }
        self.rows.drain(..start);
        Ok(())
    }

    /// Only references directly found on already scope-checked source user rows.
    pub(crate) fn media_refs(&self) -> impl Iterator<Item = &MediaRef> {
        self.rows
            .iter()
            .flat_map(|row| match &row.message {
                ExportMessageV1::User { content } => content.iter(),
                ExportMessageV1::Assistant { .. } => [].iter(),
            })
            .filter_map(|part| match part {
                ExportUserPartV1::StoredMedia { media, .. } => Some(media),
                ExportUserPartV1::Text { .. } => None,
            })
    }

    pub(crate) fn first_user_text(&self) -> Option<String> {
        self.rows.iter().find_map(|row| match &row.message {
            ExportMessageV1::User { content } => Some(
                content
                    .iter()
                    .find_map(|part| match part {
                        ExportUserPartV1::Text { text } => Some(text.chars().take(200).collect()),
                        ExportUserPartV1::StoredMedia { .. } => None,
                    })
                    .unwrap_or_default(),
            ),
            ExportMessageV1::Assistant { .. } => None,
        })
    }

    pub(crate) fn into_seed(self) -> impl ExactSizeIterator<Item = (jiff::Timestamp, TurnItem)> {
        self.rows.into_iter().map(|row| {
            let item = match row.message {
                ExportMessageV1::User { content } => {
                    TurnItem::UserMessage(harw_protocol::items::UserMessageItem {
                        id: row.id,
                        content: content
                            .into_iter()
                            .map(|part| match part {
                                ExportUserPartV1::Text { text } => ContentPart::Text { text },
                                ExportUserPartV1::StoredMedia { media, detail } => {
                                    ContentPart::Media { media, detail }
                                }
                            })
                            .collect(),
                    })
                }
                ExportMessageV1::Assistant { text } => {
                    TurnItem::AssistantMessage(harw_protocol::items::AssistantMessageItem {
                        id: row.id,
                        content: text
                            .into_iter()
                            .map(|text| ContentPart::Text { text })
                            .collect(),
                        phase: None,
                    })
                }
            };
            (row.source_recorded_at, item)
        })
    }
}

struct RecordedItem {
    sequence: u64,
    recorded_at: jiff::Timestamp,
    item: TurnItem,
}

impl RecordedItem {
    fn id(&self) -> &ItemId {
        match &self.item {
            TurnItem::UserMessage(item) => &item.id,
            TurnItem::AssistantMessage(item) => &item.id,
            TurnItem::Reasoning(item) => &item.id,
            TurnItem::ToolCall(item) => &item.id,
            TurnItem::ToolResult(item) => &item.id,
            TurnItem::Error(item) => &item.id,
        }
    }

    fn has_visible_row(&self) -> bool {
        match &self.item {
            TurnItem::UserMessage(user) => user.content.iter().any(|part| {
                matches!(part, ContentPart::Text { .. } | ContentPart::Media { .. })
            }),
            TurnItem::AssistantMessage(assistant) => assistant
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text { .. })),
            _ => false,
        }
    }

    fn into_visible_row(self) -> Option<ExportRowV1> {
        let (id, message) = match self.item {
            TurnItem::UserMessage(user) => {
                let content: Vec<_> = user
                    .content
                    .into_iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(ExportUserPartV1::Text { text }),
                        ContentPart::Media { media, detail } => {
                            Some(ExportUserPartV1::StoredMedia { media, detail })
                        }
                        ContentPart::ImageUrl { .. } => None,
                    })
                    .collect();
                if content.is_empty() {
                    return None;
                }
                (user.id, ExportMessageV1::User { content })
            }
            TurnItem::AssistantMessage(assistant) => {
                let text: Vec<_> = assistant
                    .content
                    .into_iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() {
                    return None;
                }
                (assistant.id, ExportMessageV1::Assistant { text })
            }
            _ => return None,
        };
        Some(ExportRowV1 {
            id,
            source_sequence: self.sequence,
            source_recorded_at: self.recorded_at,
            message,
        })
    }
}

struct Replacement {
    start: u64,
    explicit_target: Option<ItemId>,
    expected: usize,
    items: Vec<RecordedItem>,
}

struct Revision {
    digest: [u8; 32],
    thread: ThreadRef,
    items: Vec<RecordedItem>,
    reader: TranscriptReader,
}

fn read_revision(
    store: &TranscriptStore,
    source: &SessionId,
    horizon: NonZeroU64,
    before_rewrite: Option<NonZeroU64>,
) -> Result<Revision, ArchivedForkError> {
    scan_revision(store, source, horizon, before_rewrite, None)
}

fn scan_revision(
    store: &TranscriptStore,
    source: &SessionId,
    horizon: NonZeroU64,
    before_rewrite: Option<NonZeroU64>,
    mut on_rewrite: Option<&mut dyn FnMut(RetainedRewriteV1)>,
) -> Result<Revision, ArchivedForkError> {
    if horizon.get() > MAX_RECORDS {
        return Err(ArchivedForkError::ExceedsLimit);
    }
    let mut reader =
        store
            .snapshot_reader(source, MAX_SCAN_BYTES)
            .map_err(|error| match error {
                SessionStoreError::NotFound { .. } => ArchivedForkError::Unavailable,
                _ => ArchivedForkError::InvalidJournal,
            })?;
    let mut items: Vec<RecordedItem> = Vec::new();
    let mut group: Option<Replacement> = None;
    let mut predecessor = None;
    let mut thread: Option<ThreadRef> = None;
    let mut saw_frame = false;
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(horizon.get().to_be_bytes());
    for sequence in 0..horizon.get() {
        let record = reader
            .next()
            .ok_or(ArchivedForkError::Unavailable)?
            .map_err(|_| ArchivedForkError::InvalidJournal)?;
        if &record.session_id != source || record.sequence != sequence {
            return Err(ArchivedForkError::InvalidJournal);
        }
        let known = thread.get_or_insert_with(|| record.thread.clone());
        if known.as_str() != record.thread.as_str() {
            return Err(ArchivedForkError::InvalidJournal);
        }
        hash_record(&mut hasher, &record)?;
        let marker = (record.kind == RecordKind::Lifecycle)
            .then(|| {
                record
                    .payload
                    .get("marker")
                    .and_then(serde_json::Value::as_str)
            })
            .flatten();
        match marker {
            Some(START) => {
                if group.is_some() {
                    return Err(ArchivedForkError::InvalidJournal);
                }
                group = Some(Replacement {
                    start: sequence,
                    explicit_target: record
                        .payload
                        .get("rewind_target_item_id")
                        .map(|value| {
                            value
                                .as_str()
                                .and_then(|target| ItemId::try_from_str(target).ok())
                                .ok_or(ArchivedForkError::InvalidJournal)
                        })
                        .transpose()?,
                    expected: frame_item_count(&record)?,
                    items: Vec::new(),
                });
            }
            Some(END) => {
                let finished = group.take().ok_or(ArchivedForkError::UnverifiedRewrite)?;
                if finished.expected != frame_item_count(&record)?
                    || finished.items.len() != finished.expected
                {
                    return Err(ArchivedForkError::InvalidJournal);
                }
                let boundary = NonZeroU64::new(finished.start);
                if let Some((on_rewrite, start)) = on_rewrite.as_deref_mut().zip(boundary)
                    && items.iter().any(RecordedItem::has_visible_row)
                    && unique_item_ids(&items)
                {
                    on_rewrite(RetainedRewriteV1 {
                        before_sequence: start,
                        committed_next: NonZeroU64::new(sequence + 1)
                            .ok_or(ArchivedForkError::InvalidJournal)?,
                        recorded_at: record.recorded_at,
                        explicit_edit: verified_rewind_target(&items, &finished),
                    });
                }
                if before_rewrite.is_some() && before_rewrite == boundary {
                    // The selected frame must be the last record group in
                    // the fingerprint, never a merely asserted interior cut.
                    if sequence + 1 != horizon.get() {
                        return Err(ArchivedForkError::InvalidJournal);
                    }
                    predecessor = Some(std::mem::take(&mut items));
                }
                items = finished.items;
                saw_frame = true;
            }
            _ if record.kind == RecordKind::Item => {
                let item = RecordedItem {
                    sequence,
                    recorded_at: record.recorded_at,
                    item: serde_json::from_value(record.payload)
                        .map_err(|_| ArchivedForkError::InvalidJournal)?,
                };
                if let Some(group) = &mut group {
                    if group.items.len() >= group.expected {
                        return Err(ArchivedForkError::InvalidJournal);
                    }
                    group.items.push(item);
                } else {
                    items.push(item);
                }
            }
            _ if group.is_some() => return Err(ArchivedForkError::InvalidJournal),
            _ => {}
        }
    }
    if group.is_some() {
        return Err(ArchivedForkError::InvalidJournal);
    }
    if !saw_frame {
        return Err(ArchivedForkError::MissingFrame);
    }
    if before_rewrite.is_some() {
        items = predecessor.ok_or(ArchivedForkError::InvalidJournal)?;
    }
    if !unique_item_ids(&items) {
        return Err(ArchivedForkError::InvalidJournal);
    }
    // Later *existing* records are outside the fingerprint. A concurrent
    // mutation changes pinned file metadata; fail instead of mixing snapshots.
    match store.snapshot_is_current(source, &reader) {
        Ok(true) => {}
        Ok(false) => return Err(ArchivedForkError::Changed),
        Err(_) => return Err(ArchivedForkError::Unavailable),
    }
    Ok(Revision {
        digest: hasher.finalize().into(),
        thread: thread.ok_or(ArchivedForkError::InvalidJournal)?,
        items,
        reader,
    })
}

fn unique_item_ids(items: &[RecordedItem]) -> bool {
    let mut seen = HashSet::with_capacity(items.len());
    items.iter().all(|item| seen.insert(item.id()))
}

fn verified_rewind_target(previous: &[RecordedItem], replacement: &Replacement) -> bool {
    let Some(target) = &replacement.explicit_target else { return false; };
    let Some(index) = previous.iter().position(|item| {
        matches!(&item.item, TurnItem::UserMessage(user) if &user.id == target)
    }) else { return false; };
    // Legacy edits committed the prefix alone, then appended. Exact edits
    // publish the same prefix plus a fresh replacement user in one frame.
    match replacement.items.len().checked_sub(index) {
        Some(0) => {}
        Some(1) if matches!(replacement.items.last(), Some(RecordedItem { item: TurnItem::UserMessage(user), .. })
            if previous.iter().all(|old| old.id() != &user.id)) => {}
        _ => return false,
    }
    previous[..index].iter().zip(&replacement.items).all(|(old, new)| {
        match (serde_json::to_value(&old.item), serde_json::to_value(&new.item)) {
            (Ok(old), Ok(new)) => old == new,
            _ => false,
        }
    })
}

fn frame_item_count(record: &TranscriptRecord) -> Result<usize, ArchivedForkError> {
    record
        .payload
        .get("item_count")
        .and_then(serde_json::Value::as_u64)
        .filter(|count| *count <= MAX_RECORDS)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(ArchivedForkError::InvalidJournal)
}

fn hash_record(hasher: &mut Sha256, record: &TranscriptRecord) -> Result<(), ArchivedForkError> {
    let mut value = serde_json::to_value(record).map_err(|_| ArchivedForkError::InvalidJournal)?;
    // Be stable even if Cargo feature unification enables preserve_order.
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| ArchivedForkError::InvalidJournal)?;
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

#[cfg(test)]
mod tests;

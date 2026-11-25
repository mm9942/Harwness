//! Exact source attachment reads; no raw MediaRef or alternate backend lookup.
use std::{sync::{Arc, Weak}, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use harw_protocol::session_wire::{
    ArchiveRowLocatorV1, MEDIA_PREVIEW_MAX_BYTES, MEDIA_PREVIEW_PART_BYTES,
    SessionMediaModeV1, SessionMediaPreviewReplyV1 as Reply, SessionMediaPreviewV1,
    SessionMediaSourceV1,
};
use harw_types::{ItemId, SessionId};

use super::archive::{MobileArchiveRowLocator, MobileArchiveScope};
use super::{BackendKind, Link, MobileCore, MobileError, ServerProfile, features, parse_session_id};

static PREVIEW_ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
const READ_DEADLINE: Duration = Duration::from_secs(85);

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileMediaPreviewMode {
    Thumbnail,
    VisiblePreview,
    Viewer,
}

impl From<MobileMediaPreviewMode> for SessionMediaModeV1 {
    fn from(mode: MobileMediaPreviewMode) -> Self {
        match mode {
            MobileMediaPreviewMode::Thumbnail => Self::Thumbnail,
            MobileMediaPreviewMode::VisiblePreview => Self::VisiblePreview,
            MobileMediaPreviewMode::Viewer => Self::Viewer,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileMediaPreview {
    Found { mime: String, data_b64: String },
    OpenRequired,
    Missing,
    Unsupported,
    Denied,
    Unreadable,
    Offline,
    Busy,
}

/// In-process witness of an already attached source. Not a bearer token,
/// address, serializable ID or an authority grant. Holding a weak reference
/// prevents pointer ABA without keeping an old transport alive.
#[derive(uniffi::Object)]
pub struct MobileMediaScope {
    session: SessionId,
    profile_epoch: u64,
    link: Weak<Link>,
}

#[uniffi::export]
impl MobileCore {
    /// Capture before deferring work: validate the caller's selected server
    /// authority and the attached source without making a request.
    pub fn media_preview_scope(
        &self,
        expected_profile: Option<ServerProfile>,
        session_id: String,
    ) -> Result<Arc<MobileMediaScope>, MobileError> {
        let session = parse_session_id(session_id)?;
        let expected = expected_profile
            .map(super::ValidatedProfile::new)
            .transpose()?;
        let state = self.shared.state();
        let link = state.active()?;
        let same_authority = match (link.backend.kind(), expected) {
            (BackendKind::Local, None) => true,
            (BackendKind::Remote, Some(expected)) => state.profile.as_ref()
                .is_some_and(|actual| actual.same_authority(&expected)
                    && actual.profile.server_node_id == expected.profile.server_node_id),
            _ => false,
        };
        if !same_authority {
            return Err(MobileError::Disconnected);
        }
        if !link.backend.granted().observe {
            return Err(MobileError::Denied { detail: "Session media requires Observe".into() });
        }
        state.session(&session)?.require_live()?;
        Ok(Arc::new(MobileMediaScope {
            session, profile_epoch: state.profile_epoch, link: Arc::downgrade(&link),
        }))
    }

    /// Read only through the captured link. Never re-select an active
    /// gateway on the worker thread after a UI/profile change.
    pub fn media_preview(
        &self,
        scope: Arc<MobileMediaScope>,
        item_id: String,
        attachment_index: u32,
        mode: MobileMediaPreviewMode,
    ) -> MobileMediaPreview {
        let Ok(item_id) = ItemId::try_from_str(item_id) else {
            return MobileMediaPreview::Unreadable;
        };
        let link = {
            let state = self.shared.state();
            let current = match state.active() {
                Ok(link) => link,
                Err(error) => return preview_error(error),
            };
            if state.profile_epoch != scope.profile_epoch
                || !Weak::ptr_eq(&scope.link, &Arc::downgrade(&current))
                || !state.session(&scope.session).is_ok_and(|it| it.require_live().is_ok())
            {
                return MobileMediaPreview::Offline;
            }
            current
        };
        self.read_media_preview(link, SessionMediaPreviewV1 {
            session_id: scope.session.clone(),
            source: SessionMediaSourceV1::Canonical { item_id },
            attachment_index,
            mode: mode.into(),
            continuation: None,
        })
    }

    /// Archive rows use a current, reauthorized server-issued checkpoint;
    /// never silently substitute the live row with the same displayed text.
    pub fn archive_media_preview(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
        row: MobileArchiveRowLocator,
        attachment_index: u32,
        mode: MobileMediaPreviewMode,
    ) -> MobileMediaPreview {
        let Ok(session_id) = parse_session_id(session_id) else {
            return MobileMediaPreview::Unreadable;
        };
        let Ok(item_id) = ItemId::try_from_str(row.item_id) else {
            return MobileMediaPreview::Unreadable;
        };
        let link = match self.archive_link(&scope) {
            Ok(link) => link,
            Err(error) => return preview_error(error),
        };
        self.read_media_preview(link, SessionMediaPreviewV1 {
            session_id,
            source: SessionMediaSourceV1::Archive {
                checkpoint_id,
                row: ArchiveRowLocatorV1 { sequence: row.sequence, item_id },
            },
            attachment_index,
            mode: mode.into(),
            continuation: None,
        })
    }
}

impl MobileCore {
    fn read_media_preview(
        &self,
        link: Arc<Link>,
        mut params: SessionMediaPreviewV1,
    ) -> MobileMediaPreview {
        if !link.backend.features().iter().any(|it| it == features::SESSION_MEDIA_PREVIEW_V1) {
            return MobileMediaPreview::Unsupported;
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let Ok(_permit) = PREVIEW_ADMISSION.try_acquire() else {
                return Ok(MobileMediaPreview::Busy);
            };
            let result = tokio::time::timeout(READ_DEADLINE, async {
                let mut payload = Vec::<u8>::new();
                let mut current: Option<(String, usize)> = None;
                loop {
                    if !shared.state().same_link(&link) {
                        return Ok(MobileMediaPreview::Offline);
                    }
                    let reply = link.request(
                        link.backend.port().session_media_preview(params.clone()),
                    ).await?;
                    if !shared.state().same_link(&link) {
                        return Ok(MobileMediaPreview::Offline);
                    }
                    let Reply::Part {
                        mime, total_bytes, offset, data_b64, continuation,
                    } = reply else {
                        return Ok(match reply {
                            Reply::OpenRequired if current.is_none() => MobileMediaPreview::OpenRequired,
                            Reply::Missing => MobileMediaPreview::Missing,
                            Reply::Unsupported => MobileMediaPreview::Unsupported,
                            Reply::Busy => MobileMediaPreview::Busy,
                            _ => MobileMediaPreview::Unreadable,
                        });
                    };
                    let total = total_bytes as usize;
                    if total == 0 || total > MEDIA_PREVIEW_MAX_BYTES
                        || offset as usize != payload.len()
                        || data_b64.len() > MEDIA_PREVIEW_PART_BYTES.div_ceil(3) * 4
                        || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp")
                        || (params.mode == SessionMediaModeV1::Thumbnail
                            && (total > MEDIA_PREVIEW_PART_BYTES || continuation.is_some()))
                        || current.as_ref().is_some_and(|(kind, size)| kind != &mime || *size != total)
                    {
                        return Ok(MobileMediaPreview::Unreadable);
                    }
                    let bytes = match STANDARD.decode(data_b64) {
                        Ok(bytes) if !bytes.is_empty() && bytes.len() <= MEDIA_PREVIEW_PART_BYTES => bytes,
                        _ => return Ok(MobileMediaPreview::Unreadable),
                    };
                    if payload.len().saturating_add(bytes.len()) > total {
                        return Ok(MobileMediaPreview::Unreadable);
                    }
                    if current.is_none() {
                        payload.reserve_exact(total);
                        current = Some((mime.clone(), total));
                    }
                    payload.extend_from_slice(&bytes);
                    match continuation {
                        Some(ticket) if bytes.len() == MEDIA_PREVIEW_PART_BYTES
                            && payload.len() < total
                            && valid_ticket(&ticket)
                            && params.continuation.as_ref().is_none_or(|old| old == &ticket) =>
                        {
                            params.continuation = Some(ticket);
                        }
                        None if payload.len() == total => {
                            if !shared.state().same_link(&link) {
                                return Ok(MobileMediaPreview::Offline);
                            }
                            return Ok(MobileMediaPreview::Found {
                                mime,
                                data_b64: STANDARD.encode(payload),
                            });
                        }
                        _ => return Ok(MobileMediaPreview::Unreadable),
                    }
                }
            }).await;
            match result {
                Ok(value) => value,
                Err(_) => Ok(MobileMediaPreview::Busy),
            }
        }).unwrap_or_else(preview_error)
    }
}

fn valid_ticket(ticket: &str) -> bool {
    (32..=64).contains(&ticket.len())
        && ticket.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

fn preview_error(error: MobileError) -> MobileMediaPreview {
    match error {
        MobileError::Busy { .. } => MobileMediaPreview::Busy,
        MobileError::Denied { .. } | MobileError::Revoked => MobileMediaPreview::Denied,
        MobileError::Disconnected | MobileError::Connect { .. } | MobileError::Transport { .. } =>
            MobileMediaPreview::Offline,
        _ => MobileMediaPreview::Unreadable,
    }
}

//! Native bridge for negotiated exact archive rows and durable inclusive forks.

use harw_protocol::session_wire::{
    ARCHIVE_CHECKPOINT_WIRE_MINOR, ArchiveCheckpointDescribeV1, ArchiveCheckpointInfoV1,
    ArchiveCheckpointIssueV1, ArchiveCheckpointPageV1, ArchiveCheckpointPreviousV1,
    ArchiveCheckpointRowsV1, ArchivePositionV1, ArchiveRetainedRevisionV1, ArchiveRewriteCauseV1,
    ArchiveRowLocatorV1, ArchiveVisibleRoleV1, ArchivedDisplayedPrefixV1, ForkInclusiveV1,
    SessionSummary, archived_displayed_prefix_v1,
};
use harw_types::ItemId;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileArchiveBackend {
    Local,
    Cloud,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchiveScope {
    pub backend: MobileArchiveBackend,
    pub authority_id: String,
    pub profile_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchiveCheckpoint {
    pub session_id: String,
    pub checkpoint_id: String,
    pub generation: u32,
    pub durable_next: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileArchiveRewriteCause {
    UserEdit,
    HistoryRewrite,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchiveRetainedRevision {
    pub checkpoint: MobileArchiveCheckpoint,
    pub before_sequence: u64,
    pub recorded_at: String,
    pub cause: MobileArchiveRewriteCause,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchiveRowLocator {
    pub sequence: u64,
    pub item_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileArchiveRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchiveRow {
    pub locator: MobileArchiveRowLocator,
    pub role: MobileArchiveRole,
    pub text_parts: Vec<String>,
    pub recorded_at: String,
    pub image_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MobileArchivePage {
    pub session_id: String,
    pub checkpoint_id: String,
    pub rows: Vec<MobileArchiveRow>,
    pub next_before_sequence: Option<u64>,
}

impl From<ArchiveCheckpointInfoV1> for MobileArchiveCheckpoint {
    fn from(value: ArchiveCheckpointInfoV1) -> Self {
        Self {
            session_id: value.session_id.as_str().to_owned(),
            checkpoint_id: value.checkpoint_id,
            generation: value.revision.generation,
            durable_next: value.revision.durable_next,
        }
    }
}

impl From<ArchiveRetainedRevisionV1> for MobileArchiveRetainedRevision {
    fn from(value: ArchiveRetainedRevisionV1) -> Self {
        Self {
            checkpoint: value.checkpoint.into(),
            before_sequence: value.before_sequence,
            recorded_at: value.recorded_at.to_string(),
            cause: match value.cause {
                ArchiveRewriteCauseV1::UserEdit => MobileArchiveRewriteCause::UserEdit,
                ArchiveRewriteCauseV1::HistoryRewrite => MobileArchiveRewriteCause::HistoryRewrite,
            },
        }
    }
}

impl From<ArchiveCheckpointRowsV1> for MobileArchivePage {
    fn from(value: ArchiveCheckpointRowsV1) -> Self {
        Self {
            session_id: value.session_id.as_str().to_owned(),
            checkpoint_id: value.checkpoint_id,
            rows: value
                .rows
                .into_iter()
                .map(|row| MobileArchiveRow {
                    locator: MobileArchiveRowLocator {
                        sequence: row.locator.sequence,
                        item_id: row.locator.item_id.as_str().to_owned(),
                    },
                    role: match row.role {
                        ArchiveVisibleRoleV1::User => MobileArchiveRole::User,
                        ArchiveVisibleRoleV1::Assistant => MobileArchiveRole::Assistant,
                    },
                    text_parts: row.text_parts,
                    recorded_at: row.recorded_at.to_string(),
                    image_count: row.image_count,
                })
                .collect(),
            next_before_sequence: value.next_before_sequence,
        }
    }
}

#[uniffi::export]
impl MobileCore {
    /// Available only on the currently authenticated backend after hello.
    pub fn archive_scope(&self) -> Option<MobileArchiveScope> {
        let state = self.shared.state();
        let link = state.active().ok()?;
        current_scope(&state, &link)
    }

    /// Only when this exact scope negotiated retained predecessor discovery.
    pub fn archive_previous_available(&self, scope: MobileArchiveScope) -> bool {
        self.archive_link(&scope).is_ok_and(|link| previous_supported(&link))
    }

    /// Fetch one verified predecessor from the authenticated source journal.
    /// A null reply is terminal for this traversal, not a local-cache fallback.
    pub fn archive_checkpoint_previous(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        before_sequence: Option<u64>,
    ) -> Result<Option<MobileArchiveRetainedRevision>, MobileError> {
        let session = parse_session_id(session_id)?;
        if before_sequence == Some(0) {
            return Err(session_error("Invalid archive continuation"));
        }
        let link = self.archive_link(&scope)?;
        if !previous_supported(&link) {
            return Err(MobileError::Denied {
                detail: "This backend does not offer retained archive discovery".into(),
            });
        }
        let params = ArchiveCheckpointPreviousV1 {
            session_id: session.clone(),
            before_sequence,
        };
        let result = self.archive_read_on(link, move |port| port.archive_checkpoint_previous(params))?;
        if let Some(previous) = &result
            && (previous.checkpoint.session_id != session
                || previous.checkpoint.checkpoint_id.is_empty()
                || previous.before_sequence == 0
                || previous.checkpoint.revision.durable_next != previous.before_sequence
                || before_sequence.is_some_and(|older_than| previous.before_sequence >= older_than))
        {
            return Err(session_error("Retained archive did not match the requested source"));
        }
        Ok(result.map(MobileArchiveRetainedRevision::from))
    }

    pub fn archive_checkpoint_issue(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        generation: u32,
        durable_next: u64,
    ) -> Result<MobileArchiveCheckpoint, MobileError> {
        let session = parse_session_id(session_id)?;
        let params = ArchiveCheckpointIssueV1 {
            session_id: session.clone(),
            expected: ArchivePositionV1 {
                generation,
                durable_next,
            },
        };
        let result =
            self.archive_read(&scope, move |port| port.archive_checkpoint_issue(params))?;
        if result.session_id != session
            || result.revision
                != (ArchivePositionV1 {
                    generation,
                    durable_next,
                })
        {
            return Err(session_error(
                "Archive checkpoint did not match the requested source revision",
            ));
        }
        Ok(result.into())
    }

    pub fn archive_checkpoint_describe(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
    ) -> Result<MobileArchiveCheckpoint, MobileError> {
        let session = parse_session_id(session_id)?;
        let params = ArchiveCheckpointDescribeV1 {
            session_id: session.clone(),
            checkpoint_id: checkpoint_id.clone(),
        };
        let result =
            self.archive_read(&scope, move |port| port.archive_checkpoint_describe(params))?;
        ensure_reply_scope(
            &session,
            &checkpoint_id,
            &result.session_id,
            &result.checkpoint_id,
        )?;
        Ok(result.into())
    }

    pub fn archive_checkpoint_page(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
        before_sequence: Option<u64>,
        limit: Option<u16>,
    ) -> Result<MobileArchivePage, MobileError> {
        let session = parse_session_id(session_id)?;
        let params = ArchiveCheckpointPageV1 {
            session_id: session.clone(),
            checkpoint_id: checkpoint_id.clone(),
            before_sequence,
            limit,
        };
        let result = self.archive_read(&scope, move |port| port.archive_checkpoint_page(params))?;
        ensure_reply_scope(
            &session,
            &checkpoint_id,
            &result.session_id,
            &result.checkpoint_id,
        )?;
        Ok(result.into())
    }

    /// Sends only a verified source locator and stable operation ID. Never
    /// retries internally; an unknown outcome is retried using the same ID.
    pub fn fork_inclusive(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
        row: MobileArchiveRowLocator,
        client_operation_id: String,
    ) -> Result<MobileSession, MobileError> {
        self.send_archive_fork(scope, session_id, checkpoint_id, row, client_operation_id, None)
    }

    /// Exact retained-arrow fork; no client-provided text or media is imported.
    pub fn fork_retained_displayed(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
        row: MobileArchiveRowLocator,
        client_operation_id: String,
        displayed: Vec<crate::TranscriptEntry>,
    ) -> Result<MobileSession, MobileError> {
        let rows = super::displayed_fork_rows(displayed)?;
        if rows.last().is_none_or(|last| last.item_id != row.item_id) {
            return Err(MobileError::Denied {
                detail: "The selected archive row does not match the displayed prefix".into(),
            });
        }
        let encoded = archived_displayed_prefix_v1(&rows)
            .map_err(|_| session_error("The displayed archive prefix could not be encoded"))?;
        let proof = ArchivedDisplayedPrefixV1 {
            row_count: u16::try_from(rows.len())
                .map_err(|_| session_error("The displayed archive prefix exceeds its bound"))?,
            digest: *blake3::hash(&encoded).as_bytes(),
        };
        self.send_archive_fork(scope, session_id, checkpoint_id, row, client_operation_id, Some(proof))
    }
}

impl MobileCore {
    fn send_archive_fork(
        &self,
        scope: MobileArchiveScope,
        session_id: String,
        checkpoint_id: String,
        row: MobileArchiveRowLocator,
        client_operation_id: String,
        displayed_prefix: Option<ArchivedDisplayedPrefixV1>,
    ) -> Result<MobileSession, MobileError> {
        let source = parse_session_id(session_id)?;
        let item_id = ItemId::try_from_str(row.item_id)
            .map_err(|_| session_error("Invalid archive row ID"))?;
        if !(1..=128).contains(&client_operation_id.len())
            || !client_operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(session_error("Invalid archive operation ID"));
        }
        let link = self.archive_link(&scope)?;
        let params = ForkInclusiveV1 {
            session_id: source.clone(),
            checkpoint_id,
            row: ArchiveRowLocatorV1 {
                sequence: row.sequence,
                item_id,
            },
            client_operation_id,
            displayed_prefix,
        };
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let reply = link
                .request(link.backend.port().fork_inclusive(params))
                .await;
            let same_link = shared.state().same_link(&link);
            let destination = normalize_fork_reply(reply, same_link, &source)?;
            shared.notifier.sessions();
            Ok(destination.into())
        })
    }

    pub(super) fn archive_link(&self, expected: &MobileArchiveScope) -> Result<Arc<Link>, MobileError> {
        let state = self.shared.state();
        let link = state.active()?;
        let actual = current_scope(&state, &link).ok_or_else(|| MobileError::Denied {
            detail: "This backend does not offer verified archive forks".into(),
        })?;
        if &actual != expected {
            return Err(MobileError::Denied {
                detail: "Archive source connection changed".into(),
            });
        }
        Ok(link)
    }

    fn archive_read<T, F>(&self, scope: &MobileArchiveScope, call: F) -> Result<T, MobileError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, T> + Send + 'static,
    {
        let link = self.archive_link(scope)?;
        self.archive_read_on(link, call)
    }

    fn archive_read_on<T, F>(&self, link: Arc<Link>, call: F) -> Result<T, MobileError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, T> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            let response = link.request(call(link.backend.port())).await?;
            if !shared.state().same_link(&link) {
                return Err(MobileError::Denied {
                    detail: "Archive source connection changed".into(),
                });
            }
            Ok(response)
        })
    }
}

fn previous_supported(link: &Link) -> bool {
    link.backend.ack().features.iter().any(|name| name == features::ARCHIVE_RETAINED_REWRITE_V1)
}

fn current_scope(state: &CoreState, link: &Arc<Link>) -> Option<MobileArchiveScope> {
    let caps = link.backend.granted();
    if !caps.observe
        || !caps.steer
        || link.backend.ack().wire_minor < ARCHIVE_CHECKPOINT_WIRE_MINOR
        || !link
            .backend
            .features()
            .iter()
            .any(|name| name == features::ARCHIVE_CHECKPOINT_V1)
    {
        return None;
    }
    let (backend, authority_id) = match link.backend.kind() {
        BackendKind::Local => (
            MobileArchiveBackend::Local,
            "embedded-local-host".to_owned(),
        ),
        BackendKind::Remote => {
            let profile = &state.profile.as_ref()?.profile;
            let mut hash = blake3::Hasher::new();
            hash.update(b"harw-mobile/archive-authority/v1\0");
            for public in [
                &profile.server_node_id,
                &profile.server_public_key_hex,
                &profile.endpoint,
            ] {
                hash.update(&(public.len() as u64).to_be_bytes());
                hash.update(public.as_bytes());
            }
            (
                MobileArchiveBackend::Cloud,
                hash.finalize().to_hex().to_string(),
            )
        }
    };
    Some(MobileArchiveScope {
        backend,
        authority_id,
        profile_epoch: state.profile_epoch,
    })
}

fn ensure_reply_scope(
    source: &SessionId,
    checkpoint: &str,
    reply_source: &SessionId,
    reply_checkpoint: &str,
) -> Result<(), MobileError> {
    if source == reply_source && checkpoint == reply_checkpoint {
        Ok(())
    } else {
        Err(session_error(
            "Archive reply does not match its source checkpoint",
        ))
    }
}

fn normalize_fork_reply(
    reply: Result<SessionSummary, MobileError>,
    same_link: bool,
    source: &SessionId,
) -> Result<SessionSummary, MobileError> {
    let unknown = || MobileError::Ambiguous {
        detail:
            "Archive fork outcome is unknown; retry this same operation ID on the original backend"
                .into(),
    };
    match reply {
        Ok(result) if same_link && &result.session_id != source => Ok(result),
        Ok(_) => Err(unknown()),
        Err(
            MobileError::Disconnected
            | MobileError::Transport { .. }
            | MobileError::Internal { .. }
            | MobileError::Ambiguous { .. },
        ) => Err(unknown()),
        Err(definite) => Err(definite),
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    fn session(id: &SessionId) -> SessionSummary {
        SessionSummary {
            session_id: id.clone(),
            title: None,
            tenant: None,
            state: HostedState::Idle,
            attached: 0,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            model: None,
        }
    }

    #[test]
    fn archive_result_preserves_definite_errors_and_requires_same_operation_on_uncertainty() {
        let source = SessionId::new();
        for err in [
            MobileError::Disconnected,
            MobileError::Transport {
                detail: "timeout after dispatch".into(),
            },
            MobileError::Ambiguous {
                detail: "another transport ambiguity".into(),
            },
            MobileError::Internal {
                detail: "worker gone".into(),
            },
        ] {
            assert!(matches!(
                normalize_fork_reply(Err(err), true, &source),
                Err(MobileError::Ambiguous { .. })
            ));
        }
        assert!(matches!(
            normalize_fork_reply(Err(MobileError::Revoked), true, &source),
            Err(MobileError::Revoked)
        ));
        assert!(matches!(
            normalize_fork_reply(
                Err(MobileError::Busy {
                    detail: "try same ID".into()
                }),
                true,
                &source
            ),
            Err(MobileError::Busy { .. })
        ));
        assert!(matches!(
            normalize_fork_reply(
                Err(MobileError::Denied {
                    detail: "operation conflict".into()
                }),
                true,
                &source
            ),
            Err(MobileError::Denied { .. })
        ));
    }

    #[test]
    fn archive_result_wont_select_source_or_stale_backend() {
        let source = SessionId::new();
        let target = SessionId::new();
        assert!(matches!(
            normalize_fork_reply(Ok(session(&target)), false, &source),
            Err(MobileError::Ambiguous { .. })
        ));
        assert!(matches!(
            normalize_fork_reply(Ok(session(&source)), true, &source),
            Err(MobileError::Ambiguous { .. })
        ));
        assert!(
            matches!(normalize_fork_reply(Ok(session(&target)), true, &source), Ok(value) if value.session_id == target)
        );
    }
}

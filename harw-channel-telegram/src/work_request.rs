//! Typed `WorkRequest` lifecycle for the `/request /review /approve /deny
//! /cancel` grammar (docs/design/telegram-sandbox-work-requests.md).
//!
//! # Responsibility
//! This module owns the *typed* boundary between a parsed
//! [`harw_channel_telegram_transport::TelegramCommand`] and an eventual
//! sandboxed worker launch: it resolves a closed-grammar workspace alias
//! through the authoritative `harw_authority::WorkspaceRegistry` (never
//! trusting the alias string directly as a filesystem selector), mints a
//! [`WorkId`], and durably tracks the request through its lifecycle states.
//! It deliberately does **not** own launching the worker itself — see
//! [`launch_sandboxed_worker`]'s doc comment for why and what remains.
//!
//! # Key types
//! - [`WorkRequestRecord`] — the durable, typed request.
//! - [`WorkRequestState`] — its closed lifecycle.
//! - [`WorkRequestStore`] — a small file-backed durable store keyed by
//!   [`WorkId`], modelled after `harw_channel::PairingStore`'s atomic
//!   temp-file-plus-rename persistence but with a single in-process
//!   [`Mutex`] instead of per-key `fs4` advisory locks: this binding's
//!   production wiring drives one Telegram binding's admitted events through
//!   a single dedicated thread (see `harw-cli/src/gateway.rs`'s
//!   `start_telegram_long_poll`), so there is no cross-process writer to
//!   arbitrate.
//!
//! # Invariant
//! Only the closed `/request <workspace-alias> <role> <task>` grammar
//! (already validated by
//! `harw_channel_telegram_transport::mapping::parse_command`) can select a
//! workspace, and only through [`WorkspaceRegistry::resolve`] against the
//! requester's *paired* tenant. Free-form message text, attachments, and
//! callback payloads never reach [`WorkRequestStore::submit`]'s alias
//! parameter.
//!
//! # Concurrency
//! [`WorkRequestStore`] is `Clone` (`Arc`-backed) and `Send + Sync`; every
//! mutation is serialized through its internal [`Mutex`].
//!
//! # Errors
//! All fallible paths return [`TelegramChannelResult`]; see
//! [`TelegramChannelError`]'s `WorkspaceUnresolved`, `InvalidWorkRequestRole`,
//! `WorkRequestNotFound`, `WorkRequestInvalidTransition`,
//! `LaunchNotYetAvailable`, `Io`, and `Serde` variants.
//!
//! # Audit
//! Every admitted `/request /review /approve /deny /cancel` invocation
//! appends a `channel.command` record to a `harw_session_store::TranscriptStore`
//! journal under this store's root (§5 "Full audit of inbound commands"),
//! the same durable append-only mechanism `harw_channel::PairingStore`
//! already uses for pairing lifecycle events. This is **not** the
//! `harw-secrets` cryptographic audit chain (hash-chained, tamper-evident,
//! used for secret create/rotate/delete): `harw-cli/src/gateway.rs`
//! deliberately holds no writable handle to that chain (see its
//! `audit_chain_scheduler` doc comment — this gateway process never calls
//! `create`/`rotate`/`delete` against a `SecretStore`, precisely to avoid
//! holding decrypted KEK material outside a secret's own lifecycle). Mirroring
//! that chain's tamper-evidence for channel commands would need a write path
//! added at `harw-cli/src/secret_store.rs`, outside this change's file scope.
//! A journal write failure is logged (`tracing::error!`) but never fails the
//! command it audits — the durable lifecycle transition already succeeded by
//! the time the journal is written.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use harw_authority::WorkspaceRegistry;
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::{ChannelId, PeerId, SessionId, TenantId, ThreadRef, WorkId, WorkspaceId};

use crate::error::{TelegramChannelError, TelegramChannelResult};

/// Closed lifecycle of a Telegram-originated work request
/// (docs/design/telegram-sandbox-work-requests.md's integration order + the
/// `/review /approve /deny /cancel` grammar).
///
/// # Variants
/// - `Requested` — durably persisted by `/request`, not yet previewed.
/// - `UnderReview` — `/review` rendered the resolved workspace/role/sandbox
///   preview; the immutable request digest an `/approve` acts on is fixed
///   from this point.
/// - `Approved` — `/approve` accepted the reviewed request. Does not imply a
///   worker has launched — see [`launch_sandboxed_worker`].
/// - `Denied` — terminal: `/deny` rejected the request.
/// - `Cancelled` — terminal: `/cancel` withdrew the request (by the
///   requester or an operator) before or after approval, but always before
///   launch, since no launch path exists yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkRequestState {
    Requested,
    UnderReview,
    Approved,
    Denied,
    Cancelled,
}

impl WorkRequestState {
    /// A short, stable label for error messages and audit strings.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::UnderReview => "under_review",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A durable, typed Telegram work request
/// (docs/design/telegram-sandbox-work-requests.md, "Typed request boundary").
///
/// The task text itself is never stored here — only its normalized digest —
/// mirroring the audit rule elsewhere in this codebase that a durable record
/// keeps a content hash rather than a second copy of possibly-sensitive
/// message bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRequestRecord {
    pub work_id: WorkId,
    pub channel: ChannelId,
    /// The actual acting human (`SenderRef::id` when present), never a
    /// group's `PeerId` — a work request must be attributable to one person.
    pub requester: PeerId,
    /// The requester's *paired* tenant at request time (never derived from
    /// message content).
    pub tenant: TenantId,
    /// The canonical workspace identity, resolved once through
    /// [`WorkspaceRegistry::resolve`] at submission time.
    pub workspace: WorkspaceId,
    pub role: String,
    /// SHA-256 hex digest of the normalized (trimmed) task text.
    pub task_digest: String,
    /// The Telegram `update_id` that created this request, for replay/audit
    /// correlation (§5).
    pub source_update_id: String,
    pub state: WorkRequestState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Launches the sandboxed worker for an `Approved` work request.
///
/// # Description
/// `harw-channel-telegram`/`harw-channel-telegram-transport` deliberately
/// have no job/worker submission path today: `harw-cli/src/gateway.rs`'s
/// Telegram consumer drives an ordinary, toolless
/// `harw_core::run_turn` per admitted message (no sandbox, no workspace
/// mount), and the durable job-worker/plan-node launch path
/// (`harw-cli/src/job_worker.rs`, `harw_plan_bridge`) is a separate
/// subsystem this crate does not depend on and this task's file scope does
/// not include. Wiring an approved [`WorkRequestRecord`] to an actual
/// `harw-sandbox`/`bwrap` launch (docs/design/telegram-sandbox-work-requests.md
/// §"Worker filesystem and process isolation") is follow-up work outside
/// this change; this function exists so that follow-up has one, clearly
/// named integration point instead of a TODO scattered across the lifecycle
/// handlers below.
///
/// # Errors
/// Always returns [`TelegramChannelError::LaunchNotYetAvailable`].
pub fn launch_sandboxed_worker(record: &WorkRequestRecord) -> TelegramChannelResult<()> {
    Err(TelegramChannelError::LaunchNotYetAvailable {
        work_id: record.work_id.clone(),
    })
}

/// Normalizes and hashes task text for durable storage (never the raw text).
fn task_digest(task: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(task.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The closed role-argument grammar `/request` already enforces at parse
/// time (`is_atom` in `harw_channel_telegram_transport::mapping`); re-checked
/// here so this module never trusts a caller that bypasses that parser.
fn is_valid_role(role: &str) -> bool {
    !role.is_empty() && !role.chars().any(char::is_whitespace)
}

// Hex-encodes a `WorkId` into a filesystem-safe filename. `WorkId::new()`
// values are UUIDs (already filename-safe), but a caller could in principle
// construct one from arbitrary non-empty text via `try_from_str`; encoding
// defensively keeps this store's on-disk layout independent from that.
fn record_filename(work_id: &WorkId) -> String {
    let mut out = String::with_capacity(work_id.as_str().len() * 2);
    for byte in work_id.as_str().as_bytes() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> TelegramChannelResult<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TelegramChannelError::from(error)),
    }
}

fn persist_json<T: Serialize>(path: &Path, value: &T) -> TelegramChannelResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TelegramChannelError::from(std::io::Error::other(
            "work-request record path has no parent",
        ))
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temp.as_file_mut(), value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| TelegramChannelError::from(error.error))?;
    Ok(())
}

/// Durable, file-backed store for [`WorkRequestRecord`]s. See the module
/// docs for why this uses one in-process [`Mutex`] rather than `fs4`
/// cross-process advisory locks.
#[derive(Debug, Clone)]
pub struct WorkRequestStore {
    root: PathBuf,
    guard: Arc<Mutex<()>>,
}

impl WorkRequestStore {
    /// Creates a store rooted at `root` (created lazily on first write).
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            guard: Arc::new(Mutex::new(())),
        }
    }

    fn path(&self, work_id: &WorkId) -> PathBuf {
        self.root
            .join("records")
            .join(record_filename(work_id))
            .with_extension("json")
    }

    /// Reads the current record for `work_id`, or `Ok(None)` if unknown.
    pub fn get(&self, work_id: &WorkId) -> TelegramChannelResult<Option<WorkRequestRecord>> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        read_json(&self.path(work_id))
    }

    /// Builds and durably persists a new `Requested` work request (§"Typed
    /// request boundary").
    ///
    /// # Arguments
    /// - `channel`/`requester`/`tenant`/`source_update_id`: trusted context
    ///   the admission pipeline already resolved for this event.
    /// - `workspace_alias` (`&str`): the closed-grammar alias from `/request`
    ///   — resolved through `workspaces`, never trusted directly.
    /// - `role` (`&str`): the closed-grammar role atom from `/request`.
    /// - `task` (`&str`): untrusted task text; only its normalized digest is
    ///   persisted.
    /// - `workspaces` (`&WorkspaceRegistry`): the authoritative registry;
    ///   resolution failure fails closed.
    ///
    /// # Errors
    /// - [`TelegramChannelError::InvalidWorkRequestRole`]: `role` is empty or
    ///   contains whitespace.
    /// - [`TelegramChannelError::WorkspaceUnresolved`]: the alias does not
    ///   resolve for `tenant` in `workspaces` (unknown, disabled,
    ///   cross-tenant, or otherwise unbound alias — fails closed per the
    ///   design doc's acceptance gates).
    /// - [`TelegramChannelError::Io`] / [`TelegramChannelError::Serde`]:
    ///   persistence failure.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &self,
        channel: &ChannelId,
        requester: &PeerId,
        tenant: &TenantId,
        workspace_alias: &str,
        role: &str,
        task: &str,
        source_update_id: &str,
        workspaces: &WorkspaceRegistry,
        now: Timestamp,
    ) -> TelegramChannelResult<WorkRequestRecord> {
        if !is_valid_role(role) {
            return Err(TelegramChannelError::InvalidWorkRequestRole {
                role: role.to_owned(),
            });
        }
        let workspace_id = WorkspaceId::from_str(workspace_alias);
        workspaces
            .resolve(tenant, &workspace_id)
            .map_err(|source| TelegramChannelError::WorkspaceUnresolved {
                alias: workspace_alias.to_owned(),
                tenant: tenant.clone(),
                source,
            })?;

        let record = WorkRequestRecord {
            work_id: WorkId::new(),
            channel: channel.clone(),
            requester: requester.clone(),
            tenant: tenant.clone(),
            workspace: workspace_id,
            role: role.to_owned(),
            task_digest: task_digest(task),
            source_update_id: source_update_id.to_owned(),
            state: WorkRequestState::Requested,
            created_at: now,
            updated_at: now,
        };

        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        persist_json(&self.path(&record.work_id), &record)?;
        drop(_guard);
        self.record_command_audit("request", now, &record);
        Ok(record)
    }

    /// Atomically transitions `work_id` from one of `expected` states to
    /// `next`, or fails without mutating anything.
    fn transition(
        &self,
        work_id: &WorkId,
        expected: &[WorkRequestState],
        next: WorkRequestState,
        now: Timestamp,
    ) -> TelegramChannelResult<WorkRequestRecord> {
        let _guard = self.guard.lock().unwrap_or_else(|p| p.into_inner());
        let path = self.path(work_id);
        let mut record: WorkRequestRecord =
            read_json(&path)?.ok_or_else(|| TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            })?;
        if !expected.contains(&record.state) {
            return Err(TelegramChannelError::WorkRequestInvalidTransition {
                work_id: work_id.clone(),
                from: record.state.as_str(),
                action: transition_action(next),
            });
        }
        record.state = next;
        record.updated_at = now;
        persist_json(&path, &record)?;
        Ok(record)
    }

    /// `/review <work-id>`: renders the resolved-workspace/role preview and
    /// transitions `Requested -> UnderReview` (idempotent if already under
    /// review).
    pub fn review(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = if let Some(existing) = self.get(work_id)? {
            if existing.state == WorkRequestState::UnderReview {
                existing
            } else {
                self.transition(
                    work_id,
                    &[WorkRequestState::Requested],
                    WorkRequestState::UnderReview,
                    now,
                )?
            }
        } else {
            return Err(TelegramChannelError::WorkRequestNotFound {
                work_id: work_id.clone(),
            });
        };
        self.record_command_audit("review", now, &record);
        Ok(format!(
            "Review {}: workspace='{}' role='{}' state={}",
            record.work_id,
            record.workspace,
            record.role,
            record.state.as_str()
        ))
    }

    /// `/approve <work-id>`: accepts a reviewed request. Never launches a
    /// worker itself — see [`launch_sandboxed_worker`].
    pub fn approve(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = self.transition(
            work_id,
            &[WorkRequestState::UnderReview],
            WorkRequestState::Approved,
            now,
        )?;
        self.record_command_audit("approve", now, &record);
        match launch_sandboxed_worker(&record) {
            Ok(()) => Ok(format!("Approved {}", record.work_id)),
            Err(TelegramChannelError::LaunchNotYetAvailable { work_id }) => Ok(format!(
                "Approved {work_id}, but sandboxed launch is not yet available"
            )),
            Err(other) => Err(other),
        }
    }

    /// `/deny <work-id>`: terminally rejects a request that has not yet been
    /// approved.
    pub fn deny(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = self.transition(
            work_id,
            &[WorkRequestState::Requested, WorkRequestState::UnderReview],
            WorkRequestState::Denied,
            now,
        )?;
        self.record_command_audit("deny", now, &record);
        Ok(format!("Denied {}", record.work_id))
    }

    /// `/cancel <work-id>`: terminally withdraws a request in any
    /// non-terminal state. Safe even after `/approve`, since no launch path
    /// exists yet to race against.
    pub fn cancel(&self, work_id: &WorkId, now: Timestamp) -> TelegramChannelResult<String> {
        let record = self.transition(
            work_id,
            &[
                WorkRequestState::Requested,
                WorkRequestState::UnderReview,
                WorkRequestState::Approved,
            ],
            WorkRequestState::Cancelled,
            now,
        )?;
        self.record_command_audit("cancel", now, &record);
        Ok(format!("Cancelled {}", record.work_id))
    }

    /// Appends a `channel.command` record to this store's audit journal
    /// (see module docs' "Audit" section). Best-effort: a journal write
    /// failure is logged, never returned, since the durable lifecycle
    /// mutation this audits has already succeeded.
    fn record_command_audit(&self, action: &'static str, now: Timestamp, record: &WorkRequestRecord) {
        let body = serde_json::json!({
            "event": "channel.command",
            "action": action,
            "work_id": record.work_id.as_str(),
            "channel": record.channel.as_str(),
            "requester": record.requester.as_str(),
            "tenant": record.tenant.as_str(),
            "workspace": record.workspace.as_str(),
            "role": record.role,
            "task_digest": record.task_digest,
            "source_update_id": record.source_update_id,
            "state": record.state.as_str(),
        });
        let transcript_record = TranscriptRecord::new(
            command_audit_session_id(&record.channel),
            ThreadRef::from_str(""),
            0,
            now,
            RecordKind::Lifecycle,
            body,
        );
        if let Err(error) =
            TranscriptStore::new(&self.root.join("journal")).append(&transcript_record)
        {
            tracing::error!(
                work_id = %record.work_id,
                action,
                error = %error,
                "channel.command audit journal write failed"
            );
        }
    }
}

// Deterministic, portable journal-session id for a channel's `channel.command`
// audit trail (mirrors `harw_channel::pairing_store`'s hex-encoded session-id
// convention so arbitrary channel identifiers cannot introduce path
// separators into the transcript store's filenames).
fn command_audit_session_id(channel: &ChannelId) -> SessionId {
    let mut hex = String::with_capacity(channel.as_str().len() * 2);
    for byte in channel.as_str().as_bytes() {
        hex.push_str(&format!("{byte:02x}"));
    }
    SessionId::from_str(format!("channel-command-audit-{hex}"))
}

fn transition_action(next: WorkRequestState) -> &'static str {
    match next {
        WorkRequestState::Requested => "reset to requested",
        WorkRequestState::UnderReview => "reviewed",
        WorkRequestState::Approved => "approved",
        WorkRequestState::Denied => "denied",
        WorkRequestState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_authority::{WorkspaceRegistration, WorkspaceRegistry};

    fn registry(tenant: &TenantId, workspace: &WorkspaceId, root: &Path) -> WorkspaceRegistry {
        WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: workspace.clone(),
                root: PathBuf::from("."),
            }],
        )
        .expect("registry builds")
    }

    #[test]
    fn submit_resolves_workspace_and_persists_requested_state() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path());
        let store = WorkRequestStore::new(store_dir.path());

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "  fix the failing test  ",
                "42",
                &registry,
                Timestamp::now(),
            )
            .expect("submit succeeds");

        assert_eq!(record.state, WorkRequestState::Requested);
        assert_eq!(record.workspace, workspace);
        assert_eq!(record.task_digest, task_digest("fix the failing test"));
        assert_eq!(
            store.get(&record.work_id).unwrap().unwrap().work_id,
            record.work_id
        );
    }

    #[test]
    fn submit_fails_closed_for_unresolved_workspace_alias() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let registry = WorkspaceRegistry::build(harness.path(), []).expect("empty registry");
        let store = WorkRequestStore::new(store_dir.path());

        let result = store.submit(
            &ChannelId::from_str("telegram:ops"),
            &PeerId::from_str("100"),
            &tenant,
            "unknown-room",
            "implementer",
            "task",
            "1",
            &registry,
            Timestamp::now(),
        );

        assert!(matches!(
            result,
            Err(TelegramChannelError::WorkspaceUnresolved { alias, .. }) if alias == "unknown-room"
        ));
    }

    #[test]
    fn submit_rejects_whitespace_role() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path());
        let store = WorkRequestStore::new(store_dir.path());

        let result = store.submit(
            &ChannelId::from_str("telegram:ops"),
            &PeerId::from_str("100"),
            &tenant,
            "ops-room",
            "not a role",
            "task",
            "1",
            &registry,
            Timestamp::now(),
        );

        assert!(matches!(
            result,
            Err(TelegramChannelError::InvalidWorkRequestRole { role }) if role == "not a role"
        ));
    }

    #[test]
    fn full_lifecycle_review_approve_reports_launch_not_yet_available() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path());
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .expect("submit");

        let review = store.review(&record.work_id, now).expect("review");
        assert!(review.contains("ops-room"));
        assert_eq!(
            store.get(&record.work_id).unwrap().unwrap().state,
            WorkRequestState::UnderReview
        );

        let approve = store.approve(&record.work_id, now).expect("approve");
        assert!(approve.contains("not yet available"));
        assert_eq!(
            store.get(&record.work_id).unwrap().unwrap().state,
            WorkRequestState::Approved
        );
    }

    #[test]
    fn deny_after_approve_is_an_invalid_transition() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path());
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .expect("submit");
        store.review(&record.work_id, now).expect("review");
        store.approve(&record.work_id, now).expect("approve");

        let result = store.deny(&record.work_id, now);
        assert!(matches!(
            result,
            Err(TelegramChannelError::WorkRequestInvalidTransition { from, .. }) if from == "approved"
        ));
    }

    #[test]
    fn cancel_after_approve_succeeds() {
        let harness = tempfile::tempdir().expect("harness dir");
        let store_dir = tempfile::tempdir().expect("store dir");
        let tenant = TenantId::from_str("ops");
        let workspace = WorkspaceId::from_str("ops-room");
        let registry = registry(&tenant, &workspace, harness.path());
        let store = WorkRequestStore::new(store_dir.path());
        let now = Timestamp::now();

        let record = store
            .submit(
                &ChannelId::from_str("telegram:ops"),
                &PeerId::from_str("100"),
                &tenant,
                "ops-room",
                "implementer",
                "task",
                "1",
                &registry,
                now,
            )
            .expect("submit");
        store.review(&record.work_id, now).expect("review");
        store.approve(&record.work_id, now).expect("approve");

        let cancelled = store.cancel(&record.work_id, now).expect("cancel");
        assert!(cancelled.starts_with("Cancelled"));
        assert_eq!(
            store.get(&record.work_id).unwrap().unwrap().state,
            WorkRequestState::Cancelled
        );
    }

    #[test]
    fn unknown_work_id_is_reported_not_found() {
        let store_dir = tempfile::tempdir().expect("store dir");
        let store = WorkRequestStore::new(store_dir.path());
        let missing = WorkId::from_str("missing-work-id");

        assert!(matches!(
            store.review(&missing, Timestamp::now()),
            Err(TelegramChannelError::WorkRequestNotFound { .. })
        ));
    }

    #[test]
    fn launch_sandboxed_worker_reports_not_yet_available() {
        let record = WorkRequestRecord {
            work_id: WorkId::new(),
            channel: ChannelId::from_str("telegram:ops"),
            requester: PeerId::from_str("100"),
            tenant: TenantId::from_str("ops"),
            workspace: WorkspaceId::from_str("ops-room"),
            role: "implementer".to_owned(),
            task_digest: task_digest("task"),
            source_update_id: "1".to_owned(),
            state: WorkRequestState::Approved,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };

        assert!(matches!(
            launch_sandboxed_worker(&record),
            Err(TelegramChannelError::LaunchNotYetAvailable { work_id }) if work_id == record.work_id
        ));
    }
}

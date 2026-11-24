//! Purpose-specific bridge. A scope is bound to one negotiated remote
//! transport and the selected pinned profile; it never targets a newer link.

use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

use harw_protocol::session_wire::{
    ArchiveParams, CloudAssistantRecallSubmit, CloudAssistantSharedCommandV1,
    CloudAssistantSharedSnapshotV1, CloudAssistantSharedStatusV1, CloudAssistantSnapshot,
    CloudAssistantStatus, CloudAssistantSubmit, CloudAssistantTurn, RenameParams,
    SetProjectParams,
    features,
};

use super::{
    BackendKind, ConnectionPhase, CoreState, Link, MobileCore, MobileError, PortFuture,
    REQUEST_TIMEOUT, ServerProfile, SessionPort, ValidatedProfile, parse_session_id,
};

const MAX_PROMPT_BYTES: usize = 8 * 1024;
const MAX_RECALL_TURNS: usize = 4;
const MAX_SHARED_DEVICES: usize = 7;
const MAX_SHARED_TURNS: usize = 64;
const MAX_SHARED_HISTORY_BYTES: usize = 48 * 1024;
const MAX_SHARED_ANSWER_BYTES: usize = 16 * 1024;
// The hosted model has a 75 s deadline. Allow time for an onion route and the
// gateway's durable response without weakening the default for ordinary calls.
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(120);

/// This handle does not prolong the lifetime of a retired transport.
#[derive(uniffi::Object)]
pub struct CloudAssistantAccess {
    link: Weak<Link>,
    profile: ServerProfile,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum MobileCloudAssistantStatus {
    Unprovisioned,
    Ready,
    Revoked,
}

#[derive(uniffi::Record)]
pub struct MobileCloudAssistantTurn {
    pub client_message_id: String,
    pub text: String,
    pub answer: String,
}

#[derive(uniffi::Record)]
pub struct MobileCloudAssistantSnapshot {
    pub status: MobileCloudAssistantStatus,
    pub turns: Vec<MobileCloudAssistantTurn>,
}

impl From<CloudAssistantSnapshot> for MobileCloudAssistantSnapshot {
    fn from(snapshot: CloudAssistantSnapshot) -> Self {
        let status = match snapshot.status {
            CloudAssistantStatus::Unprovisioned => MobileCloudAssistantStatus::Unprovisioned,
            CloudAssistantStatus::Ready => MobileCloudAssistantStatus::Ready,
            CloudAssistantStatus::Revoked => MobileCloudAssistantStatus::Revoked,
        };
        let turns = if matches!(status, MobileCloudAssistantStatus::Ready) {
            snapshot
                .turns
                .into_iter()
                .map(|turn| MobileCloudAssistantTurn {
                    client_message_id: turn.client_message_id,
                    text: turn.text,
                    answer: turn.answer,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self { status, turns }
    }
}

/// Different from the existing device room; never serializable as a Local or
/// ordinary session command.
#[derive(uniffi::Enum)]
pub enum MobileCloudAssistantSharedCommand {
    Status,
    Provision,
    RequestJoin,
    Approve { request_id: String },
    WithdrawOrRemove { request_id: String },
    RevokeRoom,
    Submit { client_message_id: String, text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MobileCloudAssistantSharedStatus {
    Unprovisioned,
    NotGranted,
    Pending,
    Owner,
    Participant,
    Revoked,
}

#[derive(uniffi::Record)]
pub struct MobileCloudAssistantSharedDevice {
    pub request_id: String,
    pub device_id: String,
    pub node_id: String,
    pub key_fingerprint_hex: String,
    pub pending: bool,
}

#[derive(uniffi::Record)]
pub struct MobileCloudAssistantSharedSnapshot {
    pub status: MobileCloudAssistantSharedStatus,
    pub turns: Vec<MobileCloudAssistantTurn>,
    pub own_request_id: Option<String>,
    pub devices: Vec<MobileCloudAssistantSharedDevice>,
}

#[uniffi::export]
impl MobileCore {
    /// Reads the native hello grant without an outgoing request.
    pub fn cloud_assistant_access(&self, profile: ServerProfile) -> Option<Arc<CloudAssistantAccess>> {
        let profile = ValidatedProfile::new(profile).ok()?.profile;
        let state = self.shared.state();
        let link = state.active().ok()?;
        enabled(&state, &link, &profile).then(|| {
            Arc::new(CloudAssistantAccess {
                link: Arc::downgrade(&link),
                profile,
            })
        })
    }

    /// Local check even if a fast reconnect's status transitions were conflated on Android.
    pub fn cloud_assistant_access_current(&self, access: Arc<CloudAssistantAccess>) -> bool {
        access
            .link
            .upgrade()
            .is_some_and(|link| enabled(&self.shared.state(), &link, &access.profile))
    }

    /// Local capability probe for this exact owner; no remote read or fallback.
    pub fn cloud_assistant_recall_supported(&self, access: Arc<CloudAssistantAccess>) -> bool {
        access.link.upgrade().is_some_and(|link| {
            enabled(&self.shared.state(), &link, &access.profile)
                && link.backend.features().iter().any(|feature| feature == features::CLOUD_ASSISTANT_RECALL_V1)
        })
    }

    /// Local probe only; no new connection, server lookup, or fallback.
    pub fn cloud_assistant_shared_supported(&self, access: Arc<CloudAssistantAccess>) -> bool {
        access.link.upgrade().is_some_and(|link| {
            shared_enabled(&self.shared.state(), &link, &access.profile)
        })
    }

    /// Full BLAKE3 fingerprint of this runtime's immutable public enrollment
    /// key. It uses the same bytes as gateway node pinning, not returned join
    /// metadata. This is for independent comparison by the device owner.
    pub fn cloud_assistant_shared_device_fingerprint(
        &self, access: Arc<CloudAssistantAccess>,
    ) -> Option<String> {
        if !self.cloud_assistant_shared_supported(Arc::clone(&access)) { return None; }
        let public_key = hex::decode(self.enrollment().public_key_hex).ok()?;
        let fingerprint = blake3::hash(&public_key).to_hex().to_string();
        self.cloud_assistant_shared_supported(access).then_some(fingerprint)
    }

    /// Typed, separate shared room request on the one originally admitted link.
    /// Definite refusal never calls the device room or starts a replacement.
    pub fn cloud_assistant_shared(
        &self,
        access: Arc<CloudAssistantAccess>,
        command: MobileCloudAssistantSharedCommand,
    ) -> Result<MobileCloudAssistantSharedSnapshot, MobileError> {
        let link = access.link.upgrade().ok_or(MobileError::Disconnected)?;
        {
            let state = self.shared.state();
            if !enabled(&state, &link, &access.profile) {
                return Err(MobileError::Disconnected);
            }
            if !shared_enabled(&state, &link, &access.profile) {
                return Err(MobileError::Denied { detail: "Shared Cloud Assistant not negotiated".into() });
            }
        }
        let request = shared_command(command)?;
        let mutation = !matches!(request, CloudAssistantSharedCommandV1::Status);
        let timeout = if matches!(request, CloudAssistantSharedCommandV1::Submit { .. }) {
            SUBMIT_TIMEOUT
        } else { REQUEST_TIMEOUT };
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            if !shared_enabled(&shared.state(), &link, &access.profile) {
                return Err(MobileError::Disconnected);
            }
            let result = link.request_with_revocation_policy(
                timeout, link.backend.port().cloud_assistant_shared(request), false,
            ).await;
            let reply = match result {
                Err(MobileError::Disconnected | MobileError::Transport { .. } | MobileError::Internal { .. })
                    if mutation => return Err(shared_ambiguous()),
                any => any?,
            };
            if !shared_enabled(&shared.state(), &link, &access.profile) {
                return Err(if mutation { shared_ambiguous() } else { MobileError::Disconnected });
            }
            match shared_snapshot(reply) {
                Err(_) if mutation => Err(shared_ambiguous()),
                output => output,
            }
        })
    }

    pub fn cloud_assistant_status(
        &self,
        access: Arc<CloudAssistantAccess>,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError> {
        self.assistant_call(access, REQUEST_TIMEOUT, |port| port.cloud_assistant_status())
    }

    pub fn cloud_assistant_provision(
        &self,
        access: Arc<CloudAssistantAccess>,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError> {
        self.assistant_call(access, REQUEST_TIMEOUT, |port| port.cloud_assistant_provision())
    }

    pub fn cloud_assistant_revoke(
        &self,
        access: Arc<CloudAssistantAccess>,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError> {
        self.assistant_call(access, REQUEST_TIMEOUT, |port| port.cloud_assistant_revoke())
    }

    /// Human-initiated, typed session management. No model action invokes this.
    pub fn cloud_assistant_rename_session(
        &self,
        access: Arc<CloudAssistantAccess>,
        session_id: String,
        title: String,
    ) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
            return Err(MobileError::Denied {
                detail: "Invalid session title".into(),
            });
        }
        let title = title.to_owned();
        self.assistant_session_change(access, move |port| {
            port.rename(RenameParams { session_id, title })
        })
    }

    /// Archivieren is reversible on the ordinary gateway, not deletion.
    pub fn cloud_assistant_archive_session(
        &self,
        access: Arc<CloudAssistantAccess>,
        session_id: String,
    ) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        self.assistant_session_change(access, move |port| {
            port.archive(ArchiveParams { session_id, archived: true })
        })
    }

    pub fn cloud_assistant_assign_existing_project(
        &self,
        access: Arc<CloudAssistantAccess>,
        session_id: String,
        project_key: String,
    ) -> Result<(), MobileError> {
        let session_id = parse_session_id(session_id)?;
        super::validate_existing_project_key(&project_key)?;
        self.assistant_request(access, REQUEST_TIMEOUT, true, move |port| {
            port.set_existing_project(SetProjectParams { session_id, project_key })
        })
    }

    pub fn cloud_assistant_submit(
        &self,
        access: Arc<CloudAssistantAccess>,
        client_message_id: String,
        text: String,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError> {
        validate_message(&client_message_id, &text)?;
        self.assistant_call(access, SUBMIT_TIMEOUT, move |port| {
            port.cloud_assistant_submit(CloudAssistantSubmit { client_message_id, text })
        })
    }

    /// The selected IDs are resolved only by the authenticated room owner.
    /// Retired/unsupported owners cannot implicitly send a plain message.
    pub fn cloud_assistant_submit_with_recall(
        &self,
        access: Arc<CloudAssistantAccess>,
        client_message_id: String,
        text: String,
        recall_message_ids: Vec<String>,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError> {
        validate_message(&client_message_id, &text)?;
        if !(1..=MAX_RECALL_TURNS).contains(&recall_message_ids.len())
            || recall_message_ids.iter().enumerate().any(|(idx, id)| {
                !valid_id(id) || recall_message_ids[..idx].contains(id)
            })
        {
            return Err(MobileError::Denied { detail: "Invalid Cloud Assistant recall selection".into() });
        }
        if !self.cloud_assistant_access_current(Arc::clone(&access)) {
            return Err(MobileError::Disconnected);
        }
        if !self.cloud_assistant_recall_supported(Arc::clone(&access)) {
            return Err(MobileError::Denied { detail: "Cloud Assistant recall was not negotiated".into() });
        }
        self.assistant_call(access, SUBMIT_TIMEOUT, move |port| {
            port.cloud_assistant_submit_with_recall(CloudAssistantRecallSubmit {
                client_message_id, text, recall_message_ids,
            })
        })
    }
}

fn shared_enabled(state: &CoreState, link: &Arc<Link>, profile: &ServerProfile) -> bool {
    enabled(state, link, profile)
        && link.backend.features().iter().any(|f| f == features::CLOUD_ASSISTANT_SHARED_V1)
}

fn shared_ambiguous() -> MobileError {
    MobileError::Ambiguous { detail: "Shared Cloud Assistant: the operation might have reached this room; reload before retrying".into() }
}

fn shared_command(input: MobileCloudAssistantSharedCommand) -> Result<CloudAssistantSharedCommandV1, MobileError> {
    use CloudAssistantSharedCommandV1 as Wire;
    use MobileCloudAssistantSharedCommand as Mobile;
    Ok(match input {
        Mobile::Status => Wire::Status,
        Mobile::Provision => Wire::Provision,
        Mobile::RequestJoin => Wire::RequestJoin,
        Mobile::Approve { request_id } | Mobile::WithdrawOrRemove { request_id }
            if !valid_request_id(&request_id) =>
        {
            return Err(MobileError::Denied { detail: "Invalid shared-room request ID".into() });
        }
        Mobile::Approve { request_id } => Wire::Approve { request_id },
        Mobile::WithdrawOrRemove { request_id } => Wire::WithdrawOrRemove { request_id },
        Mobile::RevokeRoom => Wire::RevokeRoom,
        Mobile::Submit { client_message_id, text } => {
            validate_message(&client_message_id, &text)?;
            Wire::Submit { client_message_id, text }
        }
    })
}

fn stable_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn nonzero_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| b != b'0')
}

fn valid_request_id(id: &str) -> bool { nonzero_lower_hex(id, 32) }

fn valid_turn(turn: &CloudAssistantTurn) -> bool {
    validate_message(&turn.client_message_id, &turn.text).is_ok()
        && !turn.answer.trim().is_empty()
        && turn.answer.len() <= MAX_SHARED_ANSWER_BYTES
}

fn shared_snapshot(wire: CloudAssistantSharedSnapshotV1) -> Result<MobileCloudAssistantSharedSnapshot, MobileError> {
    use CloudAssistantSharedStatusV1 as Wire;
    use MobileCloudAssistantSharedStatus as Mobile;
    let invalid = || MobileError::Session { detail: "Invalid shared Cloud Assistant snapshot".into() };
    let owner = wire.status == Wire::Owner;
    let participant = wire.status == Wire::Participant;
    let pending = wire.status == Wire::Pending;
    if (pending || participant) != wire.own_request_id.is_some()
        || wire.own_request_id.as_deref().is_some_and(|id| !valid_request_id(id))
        || (!owner && !wire.devices.is_empty())
        || (!(owner || participant) && !wire.turns.is_empty())
        || wire.devices.len() > MAX_SHARED_DEVICES || wire.turns.len() > MAX_SHARED_TURNS
    {
        return Err(invalid());
    }
    {
        let mut requests = HashSet::new();
        let mut devices = HashSet::new();
        let mut turns = HashSet::new();
        if wire.devices.iter().any(|row| {
            !valid_request_id(&row.request_id) || !stable_id(&row.device_id) || !stable_id(&row.node_id)
                || !nonzero_lower_hex(&row.key_fingerprint_hex, 64)
                || !requests.insert(row.request_id.as_str()) || !devices.insert(row.device_id.as_str())
        }) || wire.turns.iter().any(|turn| !valid_turn(turn) || !turns.insert(turn.client_message_id.as_str()))
            || wire.turns.iter().map(|turn| turn.text.len() + turn.answer.len()).sum::<usize>() > MAX_SHARED_HISTORY_BYTES
        {
            return Err(invalid());
        }
    }
    Ok(MobileCloudAssistantSharedSnapshot {
        status: match wire.status {
            Wire::Unprovisioned => Mobile::Unprovisioned,
            Wire::NotGranted => Mobile::NotGranted,
            Wire::Pending => Mobile::Pending,
            Wire::Owner => Mobile::Owner,
            Wire::Participant => Mobile::Participant,
            Wire::Revoked => Mobile::Revoked,
        },
        turns: wire.turns.into_iter().map(|turn| MobileCloudAssistantTurn {
            client_message_id: turn.client_message_id, text: turn.text, answer: turn.answer,
        }).collect(),
        own_request_id: wire.own_request_id,
        devices: wire.devices.into_iter().map(|row| MobileCloudAssistantSharedDevice {
            request_id: row.request_id, device_id: row.device_id, node_id: row.node_id,
            key_fingerprint_hex: row.key_fingerprint_hex, pending: row.pending,
        }).collect(),
    })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn validate_message(id: &str, text: &str) -> Result<(), MobileError> {
    if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES || !valid_id(id) {
        return Err(MobileError::Denied { detail: "Invalid cloud assistant message".into() });
    }
    Ok(())
}

impl MobileCore {
    fn assistant_call<F>(
        &self,
        access: Arc<CloudAssistantAccess>,
        timeout: Duration,
        request: F,
    ) -> Result<MobileCloudAssistantSnapshot, MobileError>
    where
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, CloudAssistantSnapshot>
            + Send
            + 'static,
    {
        self.assistant_request(access, timeout, false, request).map(Into::into)
    }

    fn assistant_session_change<F>(&self, access: Arc<CloudAssistantAccess>, request: F) -> Result<(), MobileError>
    where
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, harw_protocol::session_wire::SessionRow>
            + Send
            + 'static,
    {
        // The ordinary host checks the actual session, tenant and share mode
        // before making a change. Keep ordinary row contents on the Rust side.
        self.assistant_request(access, REQUEST_TIMEOUT, true, request).map(|_| ())
    }

    fn assistant_request<T, F>(
        &self,
        access: Arc<CloudAssistantAccess>,
        timeout: Duration,
        require_projects: bool,
        request: F,
    ) -> Result<T, MobileError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a dyn SessionPort) -> PortFuture<'a, T> + Send + 'static,
    {
        let link = access.link.upgrade().ok_or(MobileError::Disconnected)?;
        if !enabled(&self.shared.state(), &link, &access.profile) {
            return Err(MobileError::Disconnected);
        }
        if require_projects && !super::link_supports_projects(&link) {
            return Err(MobileError::Denied {
                detail: "Host has not granted session management".into(),
            });
        }
        let shared = Arc::clone(&self.shared);
        self.run(async move {
            // The link cancellation token also fences an operation if a switch
            // happens after admission. This call is never attempted on a new link.
            let result = link
                .request_with_timeout(timeout, request(link.backend.port()))
                .await?;
            if !enabled(&shared.state(), &link, &access.profile) {
                return Err(MobileError::Disconnected);
            }
            Ok(result)
        })
    }
}

fn enabled(state: &CoreState, link: &Arc<Link>, profile: &ServerProfile) -> bool {
    let caps = link.backend.granted();
    state.phase == ConnectionPhase::Connected
        && state.same_link(link)
        && link.backend.kind() == BackendKind::Remote
        && caps.observe
        && caps.steer
        && state.profile.as_ref().is_some_and(|selected| &selected.profile == profile)
        && link.backend.features().iter().any(|feature| feature == features::CLOUD_ASSISTANT_V1)
}

//! Separate, opt-in room for a verified tenant-local user. The legacy device
//! room is never opened as a source or used as a fallback.

use harw_protocol::session_wire::{
    CloudAssistantSharedCommandV1 as Command, CloudAssistantSharedDeviceV1 as Device,
    CloudAssistantSharedSnapshotV1 as Snapshot, CloudAssistantSharedStatusV1 as Status,
};

use harw_types::{DeviceId, NodeId};

use crate::membership::VerifiedUserMembership;

use super::*;

const DOMAIN: &[u8] = b"harw.cloud.assistant.shared-user.v1";
const MAX_SHARED_DEVICES: usize = 7;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeviceProof {
    device: String,
    node: String,
    fingerprint: [u8; 32],
    issued_revision: u64,
}

impl DeviceProof {
    fn of(member: &VerifiedUserMembership) -> Self {
        Self {
            device: member.device.as_str().to_owned(),
            node: member.node.as_str().to_owned(),
            fingerprint: member.key_fingerprint,
            issued_revision: member.issued_revision,
        }
    }

    fn same_user_binding(&self, actor: &VerifiedUserMembership) -> Result<VerifiedUserMembership> {
        Ok(VerifiedUserMembership {
            user_id: actor.user_id.clone(),
            tenant: actor.tenant.clone(),
            device: DeviceId::try_from_str(self.device.clone()).map_err(|_| storage())?,
            node: NodeId::try_from_str(self.node.clone()).map_err(|_| storage())?,
            key_fingerprint: self.fingerprint,
            issued_revision: self.issued_revision,
        })
    }

    fn valid(&self) -> bool {
        stable_id(&self.device) && stable_id(&self.node)
            && self.fingerprint != [0; 32] && self.issued_revision > 0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Join {
    id: String,
    device: DeviceProof,
    pending: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SharedRoom {
    owner: DeviceProof,
    joins: Vec<Join>,
}

impl SharedRoom {
    fn owner(&self, proof: &DeviceProof) -> bool {
        &self.owner == proof
    }

    pub(super) fn admits(&self, proof: &DeviceProof) -> bool {
        self.owner(proof) || self.joins.iter().any(|join| !join.pending && &join.device == proof)
    }

    pub(super) fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut devices = HashSet::from([self.owner.device.as_str()]);
        if !self.owner.valid() || self.joins.len() > MAX_SHARED_DEVICES
            || !self.joins.iter().all(|join| {
                valid_request_id(&join.id) && join.device.valid()
                    && ids.insert(join.id.as_str()) && devices.insert(join.device.device.as_str())
            })
        {
            return Err(storage());
        }
        Ok(())
    }

    fn new_join(&mut self, proof: &DeviceProof) -> Result<()> {
        if self.owner(proof) || self.joins.iter().any(|join| &join.device == proof) {
            return Ok(());
        }
        if self.owner.device == proof.device
            || self.joins.iter().any(|join| join.device.device == proof.device)
        {
            return Err(HostError::Denied("A different membership for this device must be removed explicitly".into()));
        }
        if self.joins.len() >= MAX_SHARED_DEVICES {
            return Err(HostError::Busy("Shared Cloud Assistant device limit reached".into()));
        }
        for _ in 0..8 {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| storage())?;
            if nonce == [0; 16] { continue; }
            let id = hex(&nonce);
            if self.joins.iter().all(|join| join.id != id) {
                self.joins.push(Join { id, device: proof.clone(), pending: true });
                return Ok(());
            }
        }
        Err(storage())
    }
}

#[derive(Clone)]
pub(super) struct SharedAccess {
    pub(super) proof: DeviceProof,
    member: VerifiedUserMembership,
    pub(super) revalidate_member: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    validate_pending: Arc<dyn Fn(&VerifiedUserMembership) -> Result<bool> + Send + Sync>,
}

impl CloudAssistant {
    /// Convert an already admitted transport into a fresh shared-room
    /// grant. The current membership is supplied by trusted host code, not
    /// the caller. The underlying active connection still must be authorized.
    pub(crate) fn shared_grant(
        &self, mut base: CloudAssistantGrant, member: &VerifiedUserMembership,
        revalidate: impl Fn() -> Result<()> + Send + Sync + 'static,
        validate_pending: impl Fn(&VerifiedUserMembership) -> Result<bool> + Send + Sync + 'static,
    ) -> Result<CloudAssistantGrant> {
        base.require_live(self)?;
        let proof = DeviceProof::of(member);
        if !proof.valid() || !stable_id(&member.user_id) || !stable_id(member.tenant.as_str()) {
            return Err(scope_denied());
        }
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        for field in [self.profile.as_str(), member.tenant.as_str(), member.user_id.as_str()] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        base.scope = digest.finalize().into();
        base.shared = Some(SharedAccess {
            proof,
            member: member.clone(),
            revalidate_member: Arc::new(revalidate),
            validate_pending: Arc::new(validate_pending),
        });
        Ok(base)
    }

    pub(crate) async fn shared_command(
        self: &Arc<Self>, grant: CloudAssistantGrant, command: Command,
    ) -> Result<Snapshot> {
        let command = match command {
            Command::Submit { client_message_id, text } => {
                self.submit(grant.clone(), CloudAssistantSubmit { client_message_id, text }).await?;
                Command::Status
            }
            other => other,
        };
        let disk_grant = grant.clone();
        let snapshot = self.disk(move |store| store.shared_action(&disk_grant, command)).await?;
        grant.require_live(self)?;
        Ok(snapshot)
    }

    pub(super) async fn confirm_shared_admission(
        self: &Arc<Self>, grant: &CloudAssistantGrant,
    ) -> Result<()> {
        let grant = grant.clone();
        self.disk(move |store| {
            grant.require_authorized(store)?;
            let _ = store.ready_room(&grant)?;
            Ok(())
        }).await
    }

    pub(super) async fn wait_shared_authority_loss(
        self: &Arc<Self>, grant: &CloudAssistantGrant,
    ) -> HostError {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Err(error) = self.confirm_shared_admission(grant).await {
                return error;
            }
        }
    }

    fn shared_action(&self, grant: &CloudAssistantGrant, command: Command) -> Result<Snapshot> {
        grant.require_authorized(self)?;
        let access = grant.shared.as_ref().ok_or_else(scope_denied)?;
        let proof = &access.proof;
        let mut room = self.load(&grant.scope)?;
        if matches!(command, Command::Provision) && room.is_none() {
            let new_room = Room {
                revoked: false, attempts: Vec::new(), turns: Vec::new(), recalls: Vec::new(),
                shared: Some(SharedRoom { owner: proof.clone(), joins: Vec::new() }),
            };
            self.save(grant, &new_room, None)?;
            grant.require_authorized(self)?;
            return view(Some(new_room), proof);
        }
        let read = matches!(command, Command::Status | Command::Provision);
        let Some(room_data) = room.as_mut() else {
            return if read { view(None, proof) } else { Err(HostError::NotFound) };
        };
        let acl = room_data.shared.as_mut().ok_or_else(storage)?;
        if read { grant.require_authorized(self)?; return view(room, proof); }
        if room_data.revoked { return Err(HostError::Revoked); }
        let mut changed = true;
        match command {
            Command::RequestJoin => {
                let before = acl.joins.len();
                acl.new_join(proof)?;
                changed = acl.joins.len() != before;
            }
            Command::Approve { request_id } => {
                if !acl.owner(proof) { return Err(scope_denied()); }
                let join = lookup(&mut acl.joins, &request_id)?;
                if !(access.validate_pending)(&join.device.same_user_binding(&access.member)?)? {
                    return Err(HostError::Denied("The requested device membership is no longer current".into()));
                }
                changed = join.pending;
                join.pending = false;
            }
            Command::WithdrawOrRemove { request_id } => {
                let owner = acl.owner(proof);
                let join = lookup(&mut acl.joins, &request_id)?;
                if !owner && &join.device != proof { return Err(scope_denied()); }
                let retired = join.device.clone();
                acl.joins.retain(|entry| entry.id != request_id);
                self.cancel_shared_device(grant.scope, &retired);
            }
            Command::RevokeRoom => {
                if !acl.owner(proof) { return Err(scope_denied()); }
                room_data.revoked = true;
                self.cancel_scope(grant.scope);
            }
            Command::Status | Command::Provision | Command::Submit { .. } =>
                return Err(HostError::Protocol("Unexpected shared Cloud Assistant operation".into())),
        }
        if changed { self.save(grant, room_data, None)?; }
        grant.require_authorized(self)?;
        view(room, proof)
    }
}

fn lookup<'a>(joins: &'a mut [Join], request_id: &str) -> Result<&'a mut Join> {
    if !valid_request_id(request_id) {
        return Err(HostError::Protocol("Invalid shared Cloud Assistant request ID".into()));
    }
    joins.iter_mut().find(|entry| entry.id == request_id).ok_or(HostError::NotFound)
}

fn view(room: Option<Room>, proof: &DeviceProof) -> Result<Snapshot> {
    let Some(room) = room else {
        return Ok(Snapshot { status: Status::Unprovisioned, turns: Vec::new(), own_request_id: None, devices: Vec::new() });
    };
    let acl = room.shared.ok_or_else(storage)?;
    let own = acl.joins.iter().find(|entry| &entry.device == proof)
        .map(|entry| (entry.pending, entry.id.clone()));
    let status = if room.revoked { Status::Revoked }
        else if acl.owner(proof) { Status::Owner }
        else if let Some((pending, _)) = &own { if *pending { Status::Pending } else { Status::Participant } }
        else { Status::NotGranted };
    let (turns, own_request_id, devices) = match status {
        Status::Owner => {
            let devices = acl.joins.into_iter().map(|entry| Device {
                request_id: entry.id,
                device_id: entry.device.device,
                node_id: entry.device.node,
                key_fingerprint_hex: hex(&entry.device.fingerprint),
                pending: entry.pending,
            }).collect();
            (room.turns, None, devices)
        }
        Status::Participant => (room.turns, own.map(|(_, id)| id), Vec::new()),
        Status::Pending => (Vec::new(), own.map(|(_, id)| id), Vec::new()),
        _ => (Vec::new(), None, Vec::new()),
    };
    Ok(Snapshot { status, turns, own_request_id, devices })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stable_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn valid_request_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && id.bytes().any(|b| b != b'0')
}

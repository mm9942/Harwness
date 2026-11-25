//! Separate authenticated Cloud Assistant store and model boundary. No session
//! port, tool registry, filesystem contexts, jobs, or local mobile data.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, open_beneath, open_dir_nofollow, open_nofollow, write_atomic_with_outcome,
};
use harw_protocol::{
    ClientCaps,
    session_wire::{
        CloudAssistantRecallSubmit, CloudAssistantSnapshot, CloudAssistantStatus,
        CloudAssistantSubmit, CloudAssistantTurn,
    },
};
use harw_types::{AuthStrength, PrincipalKind, TrustZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use zeroize::Zeroizing;

use crate::{ClientIdentity, ConnectionId, HostError, driver::DriverFuture};

mod shared;

const DOMAIN: &[u8] = b"harw.cloud.assistant.v1";
const MAGIC: &[u8; 8] = b"HCAS\x01\0\0\0";
const MAX_FILE: usize = 384 * 1024;
const MAX_ROOM_BYTES: usize = 48 * 1024;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_ANSWER_BYTES: usize = 16 * 1024;
const MAX_TURNS: usize = 64;
const MAX_ATTEMPTS: usize = 128;
const MAX_CONCURRENT_TURNS: usize = 4;
const MAX_CONTEXT_BYTES: usize = 16 * 1024;
const MAX_RECALLED_BYTES: usize = 8 * 1024;
const MAX_RECALLED_TURNS: usize = 4;

type Result<T> = std::result::Result<T, HostError>;
type Scope = [u8; 32];

/// Receives a bounded chronological context selected in the same room, plus
/// this message. Production does not give it tools, sessions or other stores.
pub trait CloudAssistantModel: Send + Sync {
    fn answer<'a>(&'a self, turns: &'a [CloudAssistantTurn], text: &'a str) -> DriverFuture<'a, String>;
}

pub struct CloudAssistant {
    root: PathBuf,
    directory: File,
    _owner: File,
    profile: String,
    cipher: XChaCha20Poly1305,
    transaction: Mutex<()>,
    authority: Arc<()>,
    active: Mutex<HashMap<Scope, ActiveTurn>>,
    limit: tokio::sync::Semaphore,
    model: Arc<dyn CloudAssistantModel>,
}

/// Ephemeral proof of one admitted transport and one mounted, fingerprint-bound
/// device room. It is not a user/account identifier and cannot be passed via RPC.
#[derive(Clone)]
pub(crate) struct CloudAssistantGrant {
    scope: Scope,
    connection: ConnectionId,
    authority: Arc<()>,
    reauthorize: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    shared: Option<shared::SharedAccess>,
}

impl CloudAssistantGrant {
    fn require_live(&self, store: &CloudAssistant) -> Result<()> {
        if !Arc::ptr_eq(&self.authority, &store.authority) {
            return Err(scope_denied());
        }
        (self.reauthorize)()
    }

    /// The registry can perform bounded filesystem I/O; call on a blocking worker only.
    fn require_authorized(&self, store: &CloudAssistant) -> Result<()> {
        self.require_live(store)?;
        if let Some(shared) = &self.shared {
            (shared.revalidate_member)()?;
        }
        self.require_live(store)
    }
}

struct ActiveTurn {
    connection: ConnectionId,
    revoke: watch::Sender<bool>,
    shared_device: Option<shared::DeviceProof>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Room {
    revoked: bool,
    attempts: Vec<String>,
    turns: Vec<CloudAssistantTurn>,
    // Only references, never another copy of recalled text. Legacy records omit this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recalls: Vec<RecallAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shared: Option<shared::SharedRoom>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallAttempt {
    message_id: String,
    selected: Vec<String>,
}

enum Prepared {
    Done(CloudAssistantSnapshot),
    Run(Vec<CloudAssistantTurn>),
}

fn storage() -> HostError {
    HostError::Storage("Cloud Assistant could not verify or commit its room".into())
}

fn scope_denied() -> HostError {
    HostError::Denied("Cloud Assistant requires an enrolled remote device and active profile".into())
}

impl CloudAssistant {
    /// The explicit credential is one private 32-byte random key, distinct
    /// from the Cloud Library key. Never generate or infer it on first use.
    pub fn open(
        root: PathBuf,
        key_file: &Path,
        profile: String,
        model: Arc<dyn CloudAssistantModel>,
    ) -> Result<Self> {
        if !root.is_absolute() || !key_file.is_absolute() || profile.is_empty() || profile.len() > 128 {
            return Err(storage());
        }
        let key_file = open_nofollow(key_file, OpenMode::read_only()).map_err(|_| storage())?;
        check_private(&key_file)?;
        if key_file.metadata().map_err(|_| storage())?.len() != 32 {
            return Err(storage());
        }
        let mut key = Zeroizing::new(Vec::with_capacity(33));
        key_file.take(33).read_to_end(&mut key).map_err(|_| storage())?;
        if key.len() != 32 || key.iter().all(|&b| b == 0) {
            return Err(storage());
        }
        let cipher = XChaCha20Poly1305::new_from_slice(&key).map_err(|_| storage())?;
        let directory = File::from(open_dir_nofollow(&root).map_err(|_| storage())?);
        ensure_owned_directory(&directory).map_err(|_| storage())?;
        if directory.metadata().map_err(|_| storage())?.mode() & 0o077 != 0 {
            return Err(storage());
        }
        let owner = open_beneath(directory.as_fd(), Path::new(".owner.lock"), OpenMode {
            create: true, ..OpenMode::read_write()
        }).map_err(|_| storage())?;
        check_private(&owner)?;
        owner.try_lock().map_err(|_| storage())?;
        Ok(Self {
            root, directory, _owner: owner, profile, cipher, model,
            transaction: Mutex::new(()), authority: Arc::new(()),
            active: Mutex::new(HashMap::new()),
            limit: tokio::sync::Semaphore::new(MAX_CONCURRENT_TURNS),
        })
    }

    pub(crate) fn admits(identity: &ClientIdentity, caps: ClientCaps) -> bool {
        Self::owner_fields(identity, caps).is_some()
    }

    fn owner_fields(identity: &ClientIdentity, caps: ClientCaps) -> Option<[&str; 4]> {
        if identity.zone != TrustZone::Remote
            || identity.strength != AuthStrength::MutualTls
            || identity.authenticated_key_fingerprint.is_none_or(|key| key == [0; 32])
            || identity.agent.is_some()
            || identity.principal.kind() != PrincipalKind::Human
            || !caps.observe || !caps.steer {
            return None;
        }
        let device = identity.device.as_ref()?.as_str();
        let principal = identity.principal.id();
        if principal.strip_prefix("device:") != Some(device) {
            return None;
        }
        let scope = [
            identity.tenant.as_ref()?.as_str(),
            principal,
            device,
            identity.authenticated_node.as_ref()?.as_str(),
        ];
        scope.iter().all(|s| !s.is_empty() && s.len() <= 256).then_some(scope)
    }

    fn scope(&self, identity: &ClientIdentity, caps: ClientCaps) -> Result<Scope> {
        let fields = Self::owner_fields(identity, caps).ok_or_else(scope_denied)?;
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        digest.update(identity.authenticated_key_fingerprint.ok_or_else(scope_denied)?);
        for field in std::iter::once(self.profile.as_str()).chain(fields) {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        Ok(digest.finalize().into())
    }

    pub(crate) fn grant(
        &self, identity: &ClientIdentity, caps: ClientCaps,
        reauthorize: impl Fn() -> Result<()> + Send + Sync + 'static,
    ) -> Result<CloudAssistantGrant> {
        let grant = CloudAssistantGrant {
            scope: self.scope(identity, caps)?,
            connection: identity.connection,
            authority: Arc::clone(&self.authority),
            reauthorize: Arc::new(reauthorize),
            shared: None,
        };
        grant.require_live(self)?;
        Ok(grant)
    }

    pub(crate) async fn status(self: &Arc<Self>, grant: CloudAssistantGrant) -> Result<CloudAssistantSnapshot> {
        let disk_grant = grant.clone();
        let result = self.disk(move |store| {
            disk_grant.require_live(store)?;
            Ok(snapshot(store.load(&disk_grant.scope)?))
        }).await?;
        grant.require_live(self)?;
        Ok(result)
    }

    pub(crate) async fn provision(self: &Arc<Self>, grant: CloudAssistantGrant) -> Result<CloudAssistantSnapshot> {
        let disk_grant = grant.clone();
        let result = self.disk(move |store| {
            disk_grant.require_live(store)?;
            if let Some(room) = store.load(&disk_grant.scope)? {
                return Ok(snapshot(Some(room)));
            }
            let room = Room {
                revoked: false,
                attempts: Vec::new(),
                turns: Vec::new(),
                recalls: Vec::new(),
                shared: None,
            };
            store.save(&disk_grant, &room, None)?;
            Ok(snapshot(Some(room)))
        }).await?;
        grant.require_live(self)?;
        Ok(result)
    }

    pub(crate) async fn revoke(self: &Arc<Self>, grant: CloudAssistantGrant) -> Result<CloudAssistantSnapshot> {
        let disk_grant = grant.clone();
        let result = self.disk(move |store| {
            disk_grant.require_live(store)?;
            let mut room = store.load(&disk_grant.scope)?.ok_or(HostError::NotFound)?;
            disk_grant.require_live(store)?;
            // Cancel only this sealed room, even if the following tombstone
            // write is uncertain. The caller gets an error, not false success.
            store.cancel_scope(disk_grant.scope);
            if !room.revoked {
                room.revoked = true;
                store.save(&disk_grant, &room, None)?;
            }
            Ok(snapshot(Some(room)))
        }).await?;
        grant.require_live(self)?;
        Ok(result)
    }

    pub(crate) async fn submit(
        self: &Arc<Self>,
        grant: CloudAssistantGrant,
        params: CloudAssistantSubmit,
    ) -> Result<CloudAssistantSnapshot> {
        self.submit_pinned(grant, params, Vec::new()).await
    }

    pub(crate) async fn submit_with_recall(
        self: &Arc<Self>,
        grant: CloudAssistantGrant,
        params: CloudAssistantRecallSubmit,
    ) -> Result<CloudAssistantSnapshot> {
        let CloudAssistantRecallSubmit { client_message_id, text, mut recall_message_ids } = params;
        if !(1..=MAX_RECALLED_TURNS).contains(&recall_message_ids.len()) {
            return Err(HostError::Protocol("Invalid Cloud Assistant recall selection".into()));
        }
        recall_message_ids.sort_unstable();
        if !valid_recall_ids(&recall_message_ids) {
            return Err(HostError::Protocol("Invalid Cloud Assistant recall selection".into()));
        }
        self.submit_pinned(
            grant,
            CloudAssistantSubmit { client_message_id, text },
            recall_message_ids,
        ).await
    }

    async fn submit_pinned(
        self: &Arc<Self>,
        grant: CloudAssistantGrant,
        params: CloudAssistantSubmit,
        selected: Vec<String>,
    ) -> Result<CloudAssistantSnapshot> {
        grant.require_live(self)?;
        if !valid_id(&params.client_message_id) || !valid_text(&params.text, MAX_MESSAGE_BYTES) {
            return Err(HostError::Protocol("Invalid Cloud Assistant message".into()));
        }
        let _slot = self.limit.try_acquire().map_err(|_| busy())?;
        let mut active = self.claim(&grant)?;
        let CloudAssistantSubmit { client_message_id: id, text } = params;
        let prepared = self.disk({
            let (id, text) = (id.clone(), text.clone());
            let disk_grant = grant.clone();
            let revoked = active.revoked.clone();
            move |store| store.prepare(&disk_grant, &revoked, id, &text, selected)
        }).await?;
        grant.require_live(self)?;
        active.require_live()?;
        let turns = match prepared {
            Prepared::Done(snapshot) => return Ok(snapshot),
            Prepared::Run(turns) => turns,
        };
        if grant.shared.is_some() {
            self.confirm_shared_admission(&grant).await?;
        }
        let answer = tokio::select! {
            biased;
            _ = active.revoked.wait_for(|revoked| *revoked) => {
                grant.require_live(self)?;
                return Err(HostError::Revoked);
            },
            error = self.wait_shared_authority_loss(&grant), if grant.shared.is_some() => return Err(error),
            result = tokio::time::timeout(
                std::time::Duration::from_secs(75),
                // Don't construct the adapter future at all if already cancelled.
                async { self.model.answer(&turns, &text).await },
            ) => result.map_err(|_| HostError::Driver("Cloud Assistant model timed out".into()))??,
        };
        if !valid_text(&answer, MAX_ANSWER_BYTES) {
            return Err(HostError::Driver("Cloud Assistant model returned no complete answer".into()));
        }
        grant.require_live(self)?;
        active.require_live()?;
        let disk_grant = grant.clone();
        let revoked = active.revoked.clone();
        let result = self.disk(move |store| {
            disk_grant.require_authorized(store)?;
            require_running(Some(&revoked))?;
            let mut room = store.ready_room(&disk_grant)?;
            if room.attempts.last() != Some(&id) {
                return Err(storage());
            }
            room.turns.push(CloudAssistantTurn { client_message_id: id, text, answer });
            store.save(&disk_grant, &room, Some(&revoked))?;
            Ok(snapshot(Some(room)))
        }).await?;
        grant.require_live(self)?;
        active.require_live()?;
        Ok(result)
    }

    fn claim(&self, grant: &CloudAssistantGrant) -> Result<InFlight<'_>> {
        let mut active = self.active.lock().map_err(|_| storage())?;
        grant.require_live(self)?;
        if active.contains_key(&grant.scope) {
            return Err(busy());
        }
        let (revoke, revoked) = watch::channel(false);
        active.insert(grant.scope, ActiveTurn {
            connection: grant.connection,
            revoke,
            shared_device: grant.shared.as_ref().map(|access| access.proof.clone()),
        });
        Ok(InFlight { store: self, scope: grant.scope, revoked })
    }

    fn cancel_scope(&self, scope: Scope) {
        if let Ok(active) = self.active.lock() && let Some(turn) = active.get(&scope) {
            turn.revoke.send_replace(true);
        }
    }

    fn cancel_shared_device(&self, scope: Scope, device: &shared::DeviceProof) {
        if let Ok(active) = self.active.lock()
            && let Some(turn) = active.get(&scope)
            && turn.shared_device.as_ref() == Some(device)
        {
            turn.revoke.send_replace(true);
        }
    }

    pub(crate) fn cancel_connection(&self, connection: ConnectionId) {
        if let Ok(active) = self.active.lock() {
            for turn in active.values().filter(|turn| turn.connection == connection) {
                turn.revoke.send_replace(true);
            }
        }
    }

    pub(crate) fn cancel_all(&self) {
        if let Ok(active) = self.active.lock() {
            for turn in active.values() {
                turn.revoke.send_replace(true);
            }
        }
    }

    async fn disk<T: Send + 'static>(
        self: &Arc<Self>,
        op: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = store.transaction.lock().map_err(|_| storage())?;
            op(&store)
        }).await.map_err(|_| storage())?
    }

    fn prepare(
        &self, grant: &CloudAssistantGrant, revoked: &watch::Receiver<bool>,
        id: String, text: &str, selected: Vec<String>,
    ) -> Result<Prepared> {
        grant.require_authorized(self)?;
        require_running(Some(revoked))?;
        let mut room = self.ready_room(grant)?;
        let previous_selection = room.recalls.iter().find(|receipt| receipt.message_id == id)
            .map_or(&[][..], |receipt| receipt.selected.as_slice());
        if room.attempts.contains(&id) && previous_selection != selected.as_slice() {
            return Err(HostError::Protocol("Cloud Assistant message ID reused with different recall".into()));
        }
        if let Some(turn) = room.turns.iter().find(|turn| turn.client_message_id == id) {
            if turn.text != text {
                return Err(HostError::Protocol("Cloud Assistant message ID reused".into()));
            }
            return Ok(Prepared::Done(snapshot(Some(room))));
        }
        if room.attempts.contains(&id) {
            // A timeout, disconnect or uncertain persistence cannot imply a
            // second billable request. The user may explicitly send again.
            return Err(HostError::Busy("Cloud Assistant delivery was not confirmed; reload before sending again".into()));
        }
        if room.turns.len() >= MAX_TURNS || room.attempts.len() >= MAX_ATTEMPTS
            || room_bytes(&room) + text.len() + MAX_ANSWER_BYTES > MAX_ROOM_BYTES {
            return Err(HostError::Storage("Cloud Assistant room reached its retention limit".into()));
        }
        // Resolve authority and capacity before writing an attempt or invoking the provider.
        let context = model_context(&room.turns, &selected)?;
        if !selected.is_empty() {
            room.recalls.push(RecallAttempt { message_id: id.clone(), selected });
        }
        room.attempts.push(id);
        self.save(grant, &room, Some(revoked))?;
        Ok(Prepared::Run(context))
    }

    fn ready_room(&self, grant: &CloudAssistantGrant) -> Result<Room> {
        let room = self.load(&grant.scope)?.ok_or(HostError::NotFound)?;
        if room.revoked { return Err(HostError::Revoked); }
        match (&room.shared, &grant.shared) {
            (None, None) => Ok(room),
            (Some(acl), Some(access)) if acl.admits(&access.proof) => Ok(room),
            _ => Err(HostError::Denied("Cloud Assistant room access not granted".into())),
        }
    }

    fn path(scope: &Scope) -> PathBuf {
        let hex: String = scope.iter().map(|b| format!("{b:02x}")).collect();
        PathBuf::from(format!("{hex}.sealed"))
    }

    fn aad(scope: &Scope) -> Vec<u8> {
        [MAGIC.as_slice(), scope.as_slice()].concat()
    }

    fn load(&self, scope: &Scope) -> Result<Option<Room>> {
        let file = match open_beneath(self.directory.as_fd(), &Self::path(scope), OpenMode::read_only()) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage()),
        };
        check_private(&file)?;
        let length = file.metadata().map_err(|_| storage())?.len();
        if !(48..=MAX_FILE as u64).contains(&length) {
            return Err(storage());
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take(MAX_FILE as u64 + 1).read_to_end(&mut bytes).map_err(|_| storage())?;
        if bytes.len() as u64 != length {
            return Err(storage());
        }
        let (nonce, wire) = bytes.strip_prefix(MAGIC)
            .and_then(|bytes| bytes.split_at_checked(24)).ok_or_else(storage)?;
        let nonce: [u8; 24] = nonce.try_into().map_err(|_| storage())?;
        let plain = Zeroizing::new(self.cipher.decrypt(
            &XNonce::from(nonce),
            Payload { msg: wire, aad: &Self::aad(scope) },
        ).map_err(|_| storage())?);
        let room: Room = serde_json::from_slice(&plain).map_err(|_| storage())?;
        validate_room(&room)?;
        Ok(Some(room))
    }

    fn save(
        &self, grant: &CloudAssistantGrant, room: &Room, revoked: Option<&watch::Receiver<bool>>,
    ) -> Result<()> {
        grant.require_authorized(self)?;
        require_running(revoked)?;
        if room.shared.is_some() != grant.shared.is_some() {
            return Err(scope_denied());
        }
        let scope = &grant.scope;
        validate_room(room)?;
        let plain = Zeroizing::new(serde_json::to_vec(room).map_err(|_| storage())?);
        let mut nonce = [0; 24];
        getrandom::fill(&mut nonce).map_err(|_| storage())?;
        let wire = self.cipher.encrypt(
            &XNonce::from(nonce),
            Payload { msg: &plain, aad: &Self::aad(scope) },
        ).map_err(|_| storage())?;
        let record = [MAGIC.as_slice(), nonce.as_slice(), &wire].concat();
        if record.len() > MAX_FILE {
            return Err(storage());
        }
        let pinned = self.directory.metadata().map_err(|_| storage())?;
        let opened = File::from(open_dir_nofollow(&self.root).map_err(|_| storage())?);
        ensure_owned_directory(&opened).map_err(|_| storage())?;
        let actual = opened.metadata().map_err(|_| storage())?;
        if (pinned.dev(), pinned.ino()) != (actual.dev(), actual.ino()) || actual.mode() & 0o077 != 0 {
            return Err(storage());
        }
        grant.require_authorized(self)?;
        require_running(revoked)?;
        match write_atomic_with_outcome(&self.root.join(Self::path(scope)), &record, AtomicWriteOptions::private())
            .map_err(|_| storage())? {
            AtomicWriteOutcome::Committed => self.directory.sync_all().map_err(|_| storage()),
            AtomicWriteOutcome::DurabilityUncertain(_) => Err(storage()),
        }
    }
}

use std::os::fd::AsFd;

fn busy() -> HostError {
    HostError::Busy("Cloud Assistant is busy".into())
}

struct InFlight<'a> {
    store: &'a CloudAssistant,
    scope: Scope,
    revoked: watch::Receiver<bool>,
}

fn require_running(revoked: Option<&watch::Receiver<bool>>) -> Result<()> {
    if revoked.is_some_and(|signal| *signal.borrow()) {
        Err(HostError::Revoked)
    } else {
        Ok(())
    }
}

impl InFlight<'_> {
    fn require_live(&self) -> Result<()> {
        require_running(Some(&self.revoked))
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.store.active.lock() {
            active.remove(&self.scope);
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_text(text: &str, max_bytes: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max_bytes
        && !text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

fn room_bytes(room: &Room) -> usize {
    room.turns.iter().map(|turn| turn.text.len() + turn.answer.len()).sum()
}

fn valid_recall_ids(sorted: &[String]) -> bool {
    (1..=MAX_RECALLED_TURNS).contains(&sorted.len())
        && sorted.iter().all(|id| valid_id(id))
        && sorted.windows(2).all(|pair| pair[0] < pair[1])
}

fn turn_bytes(turn: &CloudAssistantTurn) -> usize {
    turn.text.len() + turn.answer.len()
}

/// Preselect explicit same-room pairs, then fill the remaining budget with the
/// ordinary contiguous latest tail. The model gets one chronological union.
fn model_context(turns: &[CloudAssistantTurn], selected: &[String]) -> Result<Vec<CloudAssistantTurn>> {
    let mut admitted = HashSet::new();
    let mut bytes = 0usize;
    for chosen in selected {
        let Some((ordinal, turn)) = turns.iter().enumerate().find(|(_, turn)| turn.client_message_id == *chosen) else {
            return Err(HostError::Denied("Cloud Assistant recall selection is not available in this room".into()));
        };
        bytes += turn_bytes(turn);
        if bytes > MAX_RECALLED_BYTES {
            return Err(HostError::Protocol("Cloud Assistant recall selection exceeds its context budget".into()));
        }
        admitted.insert(ordinal);
    }
    for (ordinal, turn) in turns.iter().enumerate().rev() {
        if admitted.contains(&ordinal) { continue; }
        let n = turn_bytes(turn);
        if n > MAX_CONTEXT_BYTES - bytes { break; }
        bytes += n;
        admitted.insert(ordinal);
    }
    Ok(turns.iter().enumerate().filter(|(ordinal, _)| admitted.contains(ordinal))
        .map(|(_, turn)| turn.clone()).collect())
}

fn validate_room(room: &Room) -> Result<()> {
    if let Some(acl) = &room.shared { acl.validate()?; }
    let ids: HashSet<_> = room.attempts.iter().map(String::as_str).collect();
    let mut completed = HashSet::new();
    if room.turns.len() > MAX_TURNS || room.attempts.len() > MAX_ATTEMPTS
        || ids.len() != room.attempts.len() || room_bytes(room) > MAX_ROOM_BYTES
        || !room.attempts.iter().all(|id| valid_id(id))
        || !room.turns.iter().all(|turn| {
            let id = turn.client_message_id.as_str();
            ids.contains(id) && completed.insert(id) && valid_text(&turn.text, MAX_MESSAGE_BYTES)
                && valid_text(&turn.answer, MAX_ANSWER_BYTES)
        }) {
        return Err(storage());
    }
    let mut recall_receipts = HashSet::new();
    if room.recalls.len() > room.attempts.len() || !room.recalls.iter().all(|receipt| {
        if !recall_receipts.insert(receipt.message_id.as_str()) || !valid_recall_ids(&receipt.selected) {
            return false;
        }
        let Some(at) = room.attempts.iter().position(|id| *id == receipt.message_id) else { return false; };
        receipt.selected.iter().all(|chosen| {
            completed.contains(chosen.as_str()) && room.attempts[..at].contains(chosen)
        })
    }) {
        return Err(storage());
    }
    Ok(())
}

fn snapshot(room: Option<Room>) -> CloudAssistantSnapshot {
    let (status, turns) = match room {
        Some(room) if !room.revoked => (CloudAssistantStatus::Ready, room.turns),
        Some(_) => (CloudAssistantStatus::Revoked, Vec::new()),
        None => (CloudAssistantStatus::Unprovisioned, Vec::new()),
    };
    CloudAssistantSnapshot { status, turns }
}

fn check_private(file: &File) -> Result<()> {
    ensure_private_regular(file).map_err(|_| storage())?;
    if file.metadata().map_err(|_| storage())?.nlink() != 1 {
        return Err(storage());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Echo(AtomicUsize);
    impl CloudAssistantModel for Echo {
        fn answer<'a>(
            &'a self, _: &'a [CloudAssistantTurn], _: &'a str,
        ) -> DriverFuture<'a, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok("Only this scoped room".into()) })
        }
    }

    #[tokio::test]
    async fn explicit_provision_replay_and_revoke_survive_reopen()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("rooms");
        std::fs::create_dir(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let key = dir.path().join("credential");
        std::fs::write(&key, [8; 32])?;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
        let model = Arc::new(Echo(AtomicUsize::new(0)));
        let open = || {
            let erased: Arc<dyn CloudAssistantModel> = model.clone();
            CloudAssistant::open(root.clone(), &key, "profile".into(), erased).map(Arc::new)
        };
        let assistant = open()?;
        let owner = [1; 32];
        let other = [2; 32];
        let grant = |store: &CloudAssistant, scope| CloudAssistantGrant {
            scope,
            connection: ConnectionId(1),
            authority: Arc::clone(&store.authority),
            reauthorize: Arc::new(|| Ok(())),
            shared: None,
        };
        let submit = || CloudAssistantSubmit {
            client_message_id: "message-one".into(),
            text: "Private message".into(),
        };
        assert_eq!(assistant.status(grant(&assistant, owner)).await?.status, CloudAssistantStatus::Unprovisioned);
        assert!(matches!(assistant.submit(grant(&assistant, owner), submit()).await, Err(HostError::NotFound)));
        assert_eq!(assistant.provision(grant(&assistant, owner)).await?.status, CloudAssistantStatus::Ready);
        for _ in 0..2 {
            assert_eq!(assistant.submit(grant(&assistant, owner), submit()).await?.turns.len(), 1);
        }
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        assert!(assistant.status(grant(&assistant, other)).await?.turns.is_empty());
        assert!(!std::fs::read(root.join(CloudAssistant::path(&owner)))?
            .windows("Private message".len()).any(|window| window == b"Private message"));
        assert_eq!(assistant.revoke(grant(&assistant, owner)).await?.status, CloudAssistantStatus::Revoked);
        drop(assistant);

        let assistant = open()?;
        let revoked = assistant.provision(grant(&assistant, owner)).await?;
        assert_eq!(revoked.status, CloudAssistantStatus::Revoked);
        assert!(revoked.turns.is_empty());
        assert!(matches!(assistant.submit(grant(&assistant, owner), submit()).await, Err(HostError::Revoked)));
        assert_eq!(assistant.status(grant(&assistant, other)).await?.status, CloudAssistantStatus::Unprovisioned);
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
        Ok(())
    }
}

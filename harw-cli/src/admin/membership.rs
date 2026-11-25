//! Explicit local Administrative membership issuance, independent of Gateway
//! access policy. A membership is never accepted from a peer, label, or login.

use std::collections::HashSet;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use harw_fsutil::{
    AtomicWriteOptions, AtomicWriteOutcome, OpenMode, ensure_owned_directory,
    ensure_private_regular, open_beneath, open_dir_nofollow, write_atomic_with_outcome,
};
use harw_node_listener::{DeviceRecord, DeviceRegistry};
use harw_session_host::{
    ClientIdentity, HostError,
    membership::{UserMembershipResolver, VerifiedUserMembership},
};
use harw_types::{AuthStrength, DeviceId, NodeId, PrincipalKind, TenantId, TrustZone};
use serde::{Deserialize, Serialize};

use crate::cli::AdminMembershipAction;

const FORMAT: u8 = 1;
const FILE: &str = "user-device-membership-v1.json";
const LOCK: &str = "user-device-membership-v1.lock";
const MAX_POLICY_BYTES: usize = 1024 * 1024;
const MAX_STORE_BYTES: u64 = 256 * 1024;
const MAX_BINDINGS: usize = 256;

type Result<T> = std::result::Result<T, String>;

/// The gateway-owned typed identifiers and full peer pin, not account claims
/// supplied by an enrolled device. Store pathname comes only from the profile.
#[derive(Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceBinding {
    device: DeviceId,
    node: NodeId,
    tenant: TenantId,
    fingerprint: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    user: String,
    device: DeviceBinding,
    issued_revision: u64,
    revoked_revision: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    revision: u64,
    bindings: Vec<Binding>,
}

pub(crate) struct Registry {
    path: PathBuf,
    dir: File,
}

fn registry_error() -> String {
    "Die Mitgliedschaftsdatei konnte nicht sicher gelesen oder geschrieben werden.".into()
}

fn policy_error() -> String {
    "Genau ein aktives Gerät mit einem gültigen Gateway-Pin im aktuellen Profil wird benötigt.".into()
}

fn safe_id(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn valid_user(s: &str) -> bool {
    s.strip_prefix("u_").is_some_and(|id| id.len() == 32
        && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && id.bytes().any(|b| b != b'0'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fingerprint(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Der vollständige 64-stellige Schlüsselfingerabdruck ist erforderlich.".into());
    }
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| policy_error())?;
    }
    if out == [0; 32] {
        return Err(policy_error());
    }
    Ok(out)
}

impl DeviceBinding {
    fn parse(device: String, node: String, tenant: String, fingerprint_hex: &str) -> Result<Self> {
        if ![device.as_str(), node.as_str(), tenant.as_str()].into_iter().all(safe_id) {
            return Err("Ungültige Geräte-, Node- oder Mandanten-ID.".into());
        }
        Ok(Self {
            device: DeviceId::try_from_str(device).map_err(|_| policy_error())?,
            node: NodeId::try_from_str(node).map_err(|_| policy_error())?,
            tenant: TenantId::try_from_str(tenant).map_err(|_| policy_error())?,
            fingerprint: fingerprint(fingerprint_hex)?,
        })
    }
}

impl State {
    fn validate(&self) -> Result<()> {
        if self.version != FORMAT || self.bindings.len() > MAX_BINDINGS {
            return Err(registry_error());
        }
        let mut tuples = HashSet::new();
        let mut active_devices = HashSet::new();
        let mut user_tenants = std::collections::HashMap::new();
        let mut revisions = HashSet::new();
        for row in &self.bindings {
            let key = &row.device;
            if !valid_user(&row.user)
                || ![key.device.as_str(), key.node.as_str(), key.tenant.as_str()].into_iter().all(safe_id)
                || key.fingerprint == [0; 32] || !tuples.insert(key)
                || row.issued_revision == 0 || !revisions.insert(row.issued_revision)
                || row.issued_revision > self.revision
            {
                return Err(registry_error());
            }
            let tenant = user_tenants.entry(row.user.as_str()).or_insert(&key.tenant);
            if *tenant != &key.tenant {
                return Err(registry_error());
            }
            if let Some(revoked) = row.revoked_revision {
                if revoked <= row.issued_revision || revoked > self.revision || !revisions.insert(revoked) {
                    return Err(registry_error());
                }
            } else if !active_devices.insert(&key.device) {
                return Err(registry_error());
            }
        }
        if u64::try_from(revisions.len()).ok() != Some(self.revision) {
            return Err(registry_error());
        }
        Ok(())
    }

    fn next_revision(&self) -> Result<u64> {
        self.revision.checked_add(1).ok_or_else(registry_error)
    }

    fn mint_user(&self) -> Result<String> {
        for _ in 0..8 {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).map_err(|_| registry_error())?;
            if nonce == [0; 16] { continue; }
            let minted = format!("u_{}", hex(&nonce));
            if self.bindings.iter().all(|row| row.user != minted) {
                return Ok(minted);
            }
        }
        Err(registry_error())
    }
}

impl Registry {
    pub(crate) fn open(path: PathBuf) -> Result<Self> {
        let dir = File::from(open_dir_nofollow(&path).map_err(|_| registry_error())?);
        ensure_owned_directory(&dir).map_err(|_| registry_error())?;
        if dir.metadata().map_err(|_| registry_error())?.mode() & 0o077 != 0 {
            return Err(registry_error());
        }
        Ok(Self { path, dir })
    }

    fn require_pinned_path(&self) -> Result<()> {
        let current = Self::open(self.path.clone())?;
        let a = self.dir.metadata().map_err(|_| registry_error())?;
        let b = current.dir.metadata().map_err(|_| registry_error())?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(registry_error());
        }
        Ok(())
    }

    fn private_file(file: &File) -> Result<()> {
        ensure_private_regular(file).map_err(|_| registry_error())?;
        if file.metadata().map_err(|_| registry_error())?.nlink() != 1 {
            return Err(registry_error());
        }
        Ok(())
    }

    fn lock(&self) -> Result<File> {
        let file = open_beneath(self.dir.as_fd(), Path::new(LOCK), OpenMode {
            create: true, ..OpenMode::read_write()
        }).map_err(|_| registry_error())?;
        Self::private_file(&file)?;
        file.try_lock().map_err(|_| "Die lokale Mitgliedschaftsverwaltung ist belegt.".to_owned())?;
        Ok(file)
    }

    /// Read without creating files or mutating the directory. A missing
    /// stable writer lock with an existing store is not trustworthy.
    fn read_lock(&self) -> std::result::Result<Option<File>, HostError> {
        let file = match open_beneath(self.dir.as_fd(), Path::new(LOCK), OpenMode::read_only()) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return match open_beneath(self.dir.as_fd(), Path::new(FILE), OpenMode::read_only()) {
                    Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
                    _ => Err(member_storage()),
                };
            }
            Err(_) => return Err(member_storage()),
        };
        Self::private_file(&file).map_err(|_| member_storage())?;
        file.try_lock_shared().map_err(|_| HostError::Busy("User membership store is busy".into()))?;
        Ok(Some(file))
    }

    fn load(&self) -> Result<State> {
        let file = match open_beneath(self.dir.as_fd(), Path::new(FILE), OpenMode::read_only()) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound =>
                return Ok(State { version: FORMAT, revision: 0, bindings: Vec::new() }),
            Err(_) => return Err(registry_error()),
        };
        Self::private_file(&file)?;
        let len = file.metadata().map_err(|_| registry_error())?.len();
        if !(2..=MAX_STORE_BYTES).contains(&len) { return Err(registry_error()); }
        let mut bytes = Vec::new();
        file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes).map_err(|_| registry_error())?;
        if u64::try_from(bytes.len()).ok() != Some(len) { return Err(registry_error()); }
        let state: State = serde_json::from_slice(&bytes).map_err(|_| registry_error())?;
        state.validate()?;
        Ok(state)
    }

    fn save(&self, state: &State) -> Result<()> {
        state.validate()?;
        let wire = serde_json::to_vec(state).map_err(|_| registry_error())?;
        if u64::try_from(wire.len()).map_err(|_| registry_error())? > MAX_STORE_BYTES {
            return Err(registry_error());
        }
        self.require_pinned_path()?;
        let uncertain = || "Der Schreibstatus ist unklar. Prüfe die Mitgliedschaft erneut; nichts automatisch wiederholen.".to_owned();
        match write_atomic_with_outcome(&self.path.join(FILE), &wire, AtomicWriteOptions::private())
            .map_err(|_| registry_error())? {
            AtomicWriteOutcome::Committed => self.dir.sync_all().map_err(|_| uncertain()),
            AtomicWriteOutcome::DurabilityUncertain(_) => Err(uncertain()),
        }
    }

    /// Mirrors current Gateway input sources; it grants neither a successful
    /// handshake nor privilege. A future live consumer must additionally
    /// match the actual authenticated transport's node and key fingerprint.
    fn current_device(&self, device: &DeviceId) -> Result<DeviceBinding> {
        self.current_policy(device).map(|(binding, _)| binding)
    }

    fn current_policy(&self, device: &DeviceId) -> Result<(DeviceBinding, DeviceRecord)> {
        self.require_pinned_path()?;
        let registry = DeviceRegistry::new(&self.path);
        let scan = registry.scan_bounded(MAX_POLICY_BYTES).map_err(|_| policy_error())?;
        if !scan.rejected.is_empty() { return Err(policy_error()); }
        let mut matches = scan.records.iter().filter(|row| &row.device == device);
        let row = matches.next().ok_or_else(policy_error)?;
        if row.revoked || matches.next().is_some()
            || scan.records.iter().filter(|other| other.node_id == row.node_id).count() != 1
        {
            return Err(policy_error());
        }
        let DeviceRecord { device, node_id, tenant, .. } = row;
        if ![device.as_str(), node_id.as_str(), tenant.as_str()].into_iter().all(safe_id) {
            return Err(policy_error());
        }
        let peers = crate::session_serve_remote::read_peer_pins(&self.path.join(crate::session_listener::PEERS_FILE))
            .map_err(|_| policy_error())?;
        let mut keys = crate::session_listener::peer_key_fingerprints(&peers);
        let fingerprint = keys.remove(node_id.as_str()).filter(|key| *key != [0; 32])
            .ok_or_else(policy_error)?;
        self.require_pinned_path()?;
        Ok((DeviceBinding { device: device.clone(), node: node_id.clone(), tenant: tenant.clone(), fingerprint }, row.clone()))
    }

    fn issue(&self, expected: DeviceBinding, user: Option<String>) -> Result<(String, u64)> {
        let _writer = self.lock()?;
        if self.current_device(&expected.device)? != expected { return Err(policy_error()); }
        let mut state = self.load()?;
        if let Some(row) = state.bindings.iter().find(|row| row.device == expected) {
            if row.revoked_revision.is_none() && user.as_ref() == Some(&row.user) {
                return Ok((row.user.clone(), state.revision));
            }
            return Err("Für diese exakte Gerätebindung besteht bereits ein Eintrag oder Widerruf.".into());
        }
        if state.bindings.len() >= MAX_BINDINGS
            || state.bindings.iter().any(|row| row.revoked_revision.is_none() && row.device.device == expected.device)
        {
            return Err("Mitgliedschaftsgrenze erreicht oder das Gerät ist schon anders gebunden.".into());
        }
        let user = match user {
            Some(id) if valid_user(&id) && state.bindings.iter()
                .any(|row| row.user == id && row.device.tenant == expected.tenant) => id,
            Some(_) => return Err("Keine lokal ausgestellte Benutzer-ID für diesen Mandanten.".into()),
            None => state.mint_user()?,
        };
        let revision = state.next_revision()?;
        state.revision = revision;
        state.bindings.push(Binding { user: user.clone(), device: expected.clone(), issued_revision: revision, revoked_revision: None });
        // A concurrent external enrollment change is never silently adopted.
        if self.current_device(&expected.device)? != expected { return Err(policy_error()); }
        self.save(&state)?;
        Ok((user, revision))
    }

    fn revoke(&self, expected: &DeviceBinding, user: &str) -> Result<u64> {
        let _writer = self.lock()?;
        let mut state = self.load()?;
        let index = state.bindings.iter().position(|row| row.user == user && &row.device == expected)
            .ok_or_else(|| "Keine exakt passende ausgestellte Mitgliedschaft.".to_owned())?;
        // Still allowed after the original device or pin was retired.
        if state.bindings[index].revoked_revision.is_some() { return Ok(state.revision); }
        let revision = state.next_revision()?;
        state.revision = revision;
        state.bindings[index].revoked_revision = Some(revision);
        self.save(&state)?;
        Ok(revision)
    }
}

fn member_storage() -> HostError {
    HostError::Storage("User membership authority unavailable".into())
}

impl UserMembershipResolver for Registry {
    fn resolve_current(&self, identity: &ClientIdentity)
        -> std::result::Result<Option<VerifiedUserMembership>, HostError>
    {
        if identity.zone != TrustZone::Remote || identity.strength != AuthStrength::MutualTls
            || identity.agent.is_some() || identity.principal.kind() != PrincipalKind::Human
        {
            return Err(HostError::Denied("User membership requires enrolled remote transport".into()));
        }
        let device = identity.device.as_ref().ok_or(HostError::Revoked)?;
        self.require_pinned_path().map_err(|_| member_storage())?;
        let lock = self.read_lock()?;
        // Rebuild the policy from the same trusted inputs as Gateway auth,
        // but use the actual handshake fingerprint, never a new pin as proof.
        let authorize = || -> std::result::Result<DeviceBinding, HostError> {
            let (binding, record) = self.current_policy(device).map_err(|_| HostError::Revoked)?;
            if !record.authorizes_identity(identity)
                || identity.authenticated_key_fingerprint != Some(binding.fingerprint)
            {
                return Err(HostError::Revoked);
            }
            Ok(binding)
        };
        let admitted = authorize()?;
        let Some(_reader) = lock else { return Ok(None); };
        let state = self.load().map_err(|_| member_storage())?;
        let Some(row) = state.bindings.iter()
            .find(|row| row.device == admitted && row.revoked_revision.is_none())
        else { return Ok(None); };
        if authorize()? != admitted { return Err(HostError::Revoked); }
        Ok(Some(VerifiedUserMembership {
            user_id: row.user.clone(),
            tenant: admitted.tenant,
            device: admitted.device,
            node: admitted.node,
            key_fingerprint: admitted.fingerprint,
            issued_revision: row.issued_revision,
        }))
    }

    fn binding_is_current(&self, binding: &VerifiedUserMembership) -> std::result::Result<bool, HostError> {
        self.require_pinned_path().map_err(|_| member_storage())?;
        let Some(_reader) = self.read_lock()? else { return Ok(false); };
        let current = || -> Result<DeviceBinding> { self.current_device(&binding.device) };
        let Ok(enrolled) = current() else { return Ok(false); };
        if enrolled.node != binding.node || enrolled.tenant != binding.tenant
            || enrolled.fingerprint != binding.key_fingerprint
        {
            return Ok(false);
        }
        let state = self.load().map_err(|_| member_storage())?;
        let exists = state.bindings.iter().any(|row| {
            row.revoked_revision.is_none() && row.issued_revision == binding.issued_revision
                && row.user == binding.user_id && row.device == enrolled
        });
        Ok(exists && current().is_ok_and(|now| now == enrolled))
    }
}

/// Hub observe admission uses the existing profile's live, exact membership
/// authority. Session permission is deliberately left to the separate pinned
/// policy; this callback proves neither a session right nor a handshake.
#[cfg(feature = "direct-owner-placement")]
impl harw_cloud_gateway::owner_session_grants::DirectObserveMembershipSource for Registry {
    fn resolve_current(
        &self,
        caller: &harw_node_transport::AuthenticatedPeer,
        verified_key_fingerprint: [u8; 32],
    ) -> Option<harw_cloud_gateway::owner_session_grants::DirectObserveMembership> {
        use std::num::NonZeroU64;
        use harw_cloud_gateway::owner_session_grants::DirectObserveMembership;

        if verified_key_fingerprint == [0; 32] || !safe_id(caller.node_id.as_str()) {
            return None;
        }
        self.require_pinned_path().ok()?;
        let _reader = self.read_lock().ok()??;
        let scan = DeviceRegistry::new(&self.path).scan_bounded(MAX_POLICY_BYTES).ok()?;
        if !scan.rejected.is_empty() {
            return None;
        }
        let mut rows = scan.records.iter().filter(|row| row.node_id == caller.node_id);
        let enrolled = rows.next()?;
        if enrolled.revoked || rows.next().is_some() {
            return None;
        }
        let admitted = self.current_device(&enrolled.device).ok()?;
        if admitted.node != caller.node_id
            || admitted.tenant != enrolled.tenant
            || admitted.fingerprint != verified_key_fingerprint
        {
            return None;
        }
        let state = self.load().ok()?;
        let mut active = state.bindings.iter()
            .filter(|row| row.device == admitted && row.revoked_revision.is_none());
        let binding = active.next()?;
        if active.next().is_some() || self.current_device(&admitted.device).ok()? != admitted {
            return None;
        }
        Some(DirectObserveMembership {
            tenant: admitted.tenant.as_str().to_owned(),
            device: admitted.device.as_str().to_owned(),
            authenticated_node: admitted.node.as_str().to_owned(),
            key_fingerprint: verified_key_fingerprint,
            issued_revision: NonZeroU64::new(binding.issued_revision)?,
        })
    }
}

pub(super) fn run(home: &Path, action: AdminMembershipAction) -> Result<()> {
    let store = Registry::open(crate::session_serve_remote::remote_state_dir(home)?)?;
    match action {
        AdminMembershipAction::Inspect { device } => {
            if !safe_id(&device) { return Err(policy_error()); }
            let id = DeviceId::try_from_str(device).map_err(|_| policy_error())?;
            let state = store.load()?;
            let enrolled = match store.current_device(&id) {
                Ok(exact) => {
                    println!("Gerät: {} · Node: {} · Mandant: {}",
                        exact.device.as_str(), exact.node.as_str(), exact.tenant.as_str());
                    println!("Vollständiger BLAKE3-Pin: {}", hex(&exact.fingerprint));
                    Some(exact)
                }
                Err(_) if state.bindings.iter().any(|row| row.device.device == id) => {
                    println!("Aktuelle Gateway-Gerätebindung nicht bestätigt. Nur gespeicherte Mitgliedschaften:");
                    // Do not present any saved row as a current grant in this branch.
                    None
                }
                Err(error) => return Err(error),
            };
            let mut present = false;
            for row in state.bindings.iter().filter(|row| row.device.device == id) {
                present = true;
                let membership = if row.revoked_revision.is_some() {
                    "widerrufen"
                } else if enrolled.as_ref() == Some(&row.device) {
                    "ausgestellt; aktuelle Gerätebindung stimmt überein"
                } else {
                    "ausgestellt; aktuelle Gerätebindung nicht bestätigt"
                };
                println!("{} · Node {} · Mandant {} · Pin {} · {membership}",
                    row.user, row.device.node.as_str(), row.device.tenant.as_str(), hex(&row.device.fingerprint));
            }
            if !present {
                println!("Keine ausgestellte Mitgliedschaft für dieses Gerät.");
            }
        }
        AdminMembershipAction::Issue { device, node, tenant, key_fingerprint, user } => {
            let exact = DeviceBinding::parse(device, node, tenant, &key_fingerprint)?;
            let (subject, revision) = store.issue(exact, user)?;
            super::audit::record(home, "device_membership_issue", "cli", serde_json::json!({"revision": revision}));
            println!("Ausgestellte Benutzer-ID: {subject} · Revision {revision}. Keine Gateway-Berechtigung geändert.");
        }
        AdminMembershipAction::Revoke { device, node, tenant, key_fingerprint, user } => {
            if !valid_user(&user) { return Err("Ungültige ausgestellte Benutzer-ID.".into()); }
            let exact = DeviceBinding::parse(device, node, tenant, &key_fingerprint)?;
            let revision = store.revoke(&exact, &user)?;
            super::audit::record(home, "device_membership_revoke", "cli", serde_json::json!({"revision": revision}));
            println!("Mitgliedschaft widerrufen · Revision {revision}. Die Geräteanmeldung ist davon getrennt.");
        }
    }
    Ok(())
}

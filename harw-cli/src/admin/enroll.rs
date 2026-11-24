//! Enrollment-Handshake: Nonces, ausstehende Anfragen, Besitznachweis,
//! Eintrag in Registry und Pin-Datei sowie das öffentliche Server-Profil.
//!
//! # Beschreibung
//! Das Handy holt ein einmaliges Nonce (`GET /api/enroll/nonce`) und sendet
//! seine öffentlichen Felder zusammen mit einer ML-DSA-65-Signatur über
//! [`harw_node_transport::enrollment_proof_message`] (Besitznachweis, derselbe
//! Verifizierer wie der Transport: [`PinnedPeers`]). Erst ein angemeldeter
//! Admin kann die Anfrage freigeben. Nonces sind einmalig und kurzlebig,
//! Anfragen laufen nach 10 Minuten ab. Nur der bestandene Besitznachweis
//! wird als beschränkter privater Pending-Zustand über Neustarts bewahrt;
//! Nonces sowie Browser-Anmeldungen bleiben flüchtig.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harw_node_listener::{DeviceRecord, DeviceRegistry};
use harw_node_transport::{
    ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeVerifier as _, PinnedPeers,
    enrollment_proof_message,
};
use harw_types::{DeviceId, NodeId, PermissionTier, TenantId};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::account::{ensure_admin_dir, write_private_atomic};
use crate::session_listener::PEERS_FILE;

/// Lebensdauer einer Anfrage.
const REQUEST_TTL_SECS: u64 = 600;
const REQUEST_TTL: Duration = Duration::from_secs(REQUEST_TTL_SECS);
const REQUEST_TTL_MS: u64 = REQUEST_TTL_SECS * 1_000;
/// Lebensdauer eines Nonces.
const NONCE_TTL: Duration = Duration::from_secs(120);
const MAX_NONCES: usize = 512;
const MAX_PENDING: usize = 20;
const MAX_PENDING_PER_IP: usize = 3;
const MAX_LABEL_CHARS: usize = 64;
const PENDING_FILE: &str = "enrollment-pending-v1.json";
const PENDING_LOCK: &str = "enrollment-pending.lock";
const PENDING_VERSION: u32 = 1;
const MAX_PENDING_BYTES: usize = 128 * 1024;

fn unix_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .filter(|now| *now != 0)
}

/// Eine ausstehende, bereits bewiesene Anfrage.
#[derive(Clone)]
pub(crate) struct Pending {
    pub id: String,
    pub node_id: String,
    pub public_key: Vec<u8>,
    pub label: String,
    pub ip: String,
    created_ms: u64,
    deadline: Instant,
    pub(crate) approval: Option<RecordedApproval>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApprovalPhase {
    Applying,
    Completed,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedApproval {
    pub(crate) tier: PermissionTier,
    pub(crate) approve_optin: bool,
    phase: ApprovalPhase,
}

pub(crate) enum ApprovalStart {
    Missing,
    DifferentDecision,
    Applying(Pending),
    Recorded(Pending),
}

impl Pending {
    /// Kurzer BLAKE3-Fingerabdruck des Schlüssels (wie `harw device list`).
    pub(crate) fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }

    fn remaining(&self) -> Duration {
        let Some(now) = unix_ms().filter(|now| *now >= self.created_ms) else {
            return Duration::ZERO;
        };
        let wall_ms = REQUEST_TTL_MS.saturating_sub(now - self.created_ms);
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(wall_ms))
    }

    pub(crate) fn remaining_secs(&self) -> u64 {
        let remaining = self.remaining();
        remaining.as_secs() + u64::from(u8::from(remaining.subsec_nanos() != 0))
    }
}

/// Public fields captured atomically from an accepted, still-pending request.
pub(crate) struct Submission {
    pub id: String,
    pub fingerprint: String,
    pub expires_in: u64,
    pub reused: bool,
}

impl Submission {
    fn from_pending(pending: &Pending, reused: bool) -> Self {
        Self {
            id: pending.id.clone(),
            fingerprint: pending.fingerprint(),
            expires_in: pending.remaining_secs(),
            reused,
        }
    }
}

pub(crate) fn fingerprint(public_key: &[u8]) -> String {
    let hash = blake3::hash(public_key);
    let hex: String = hash.as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("blake3:{hex}")
}

/// Ein nicht näher begründeter Abbruch (die Antwort bleibt generisch).
#[derive(Debug)]
pub(crate) struct Rejected;

#[derive(Debug)]
pub(crate) enum SubmitError {
    Refused,
    Storage(String),
}

impl From<Rejected> for SubmitError {
    fn from(_: Rejected) -> Self {
        Self::Refused
    }
}

pub(crate) struct EnrollState {
    nonces: HashMap<[u8; 32], Instant>,
    pending: Vec<Pending>,
    store: PendingStore,
}

struct PendingStore {
    path: PathBuf,
    binding: String,
    // One writer per admin home, also across processes/bind addresses.
    _lock: File,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingSnapshot {
    version: u32,
    binding: String,
    pending: Vec<PendingRecord>,
}

/// Only the public result of verified enrollment is written; no proof,
/// challenge nonce, login token or browser session is persisted.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRecord {
    id: String,
    node_id: String,
    public_key_hex: String,
    label: String,
    ip: String,
    created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<RecordedApproval>,
}

impl From<&Pending> for PendingRecord {
    fn from(pending: &Pending) -> Self {
        Self {
            id: pending.id.clone(),
            node_id: pending.node_id.clone(),
            public_key_hex: hex_of(&pending.public_key),
            label: pending.label.clone(),
            ip: pending.ip.clone(),
            created_ms: pending.created_ms,
            approval: pending.approval,
        }
    }
}

impl PendingStore {
    fn open(home: &Path, server: &ServerIdentity, tenant: &str) -> Result<Self, String> {
        let dir = ensure_admin_dir(home)?;
        TenantId::try_from_str(tenant)
            .map_err(|_| "[admin] tenant ist keine gültige Mandanten-ID".to_owned())?;
        let mut binding = blake3::Hasher::new();
        binding.update(b"harw-admin-pending-v1\0");
        binding.update(tenant.as_bytes());
        binding.update(b"\0");
        binding.update(server.node_id.as_bytes());
        binding.update(b"\0");
        binding.update(server.public_key_hex.as_bytes());

        let lock_path = dir.join(PENDING_LOCK);
        private_file_if_present(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|error| format!("Enrollment-Sperre nicht zugänglich: {error}"))?;
        lock.try_lock()
            .map_err(|error| format!("Enrollment-Speicher nicht exklusiv sperrbar: {error:?}"))?;
        Ok(Self {
            path: dir.join(PENDING_FILE),
            binding: binding.finalize().to_hex().to_string(),
            _lock: lock,
        })
    }

    fn load(&self) -> Result<Vec<Pending>, String> {
        let invalid = || "Gespeicherte Enrollment-Anfragen sind ungültig.".to_owned();
        if !private_file_if_present(&self.path)? {
            return Ok(Vec::new());
        }
        let mut data = Vec::new();
        File::open(&self.path)
            .and_then(|file| file.take(MAX_PENDING_BYTES as u64 + 1).read_to_end(&mut data))
            .map_err(|error| format!("Enrollment-Speicher nicht lesbar: {error}"))?;
        if data.len() > MAX_PENDING_BYTES {
            return Err(invalid());
        }
        let snapshot: PendingSnapshot = serde_json::from_slice(&data).map_err(|_| invalid())?;
        if snapshot.version != PENDING_VERSION || snapshot.binding != self.binding ||
            snapshot.pending.len() > MAX_PENDING
        {
            return Err(invalid());
        }
        let now = unix_ms().ok_or_else(|| "Systemzeit für Enrollment nicht verfügbar.".to_owned())?;
        let monotonic = Instant::now();
        let mut ids = HashSet::new();
        let mut nodes = HashSet::new();
        let mut ips = HashMap::<IpAddr, usize>::new();
        let mut pending = Vec::new();
        for record in snapshot.pending {
            if record.id.len() != 32 ||
                !record.id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) ||
                !ids.insert(record.id.clone()) || !nodes.insert(record.node_id.clone()) ||
                NodeId::try_from_str(&record.node_id).is_err() ||
                clean_label(&record.label).as_deref() != Some(record.label.as_str()) ||
                record.created_ms == 0 ||
                record.approval.is_some_and(|approval| approval.tier == PermissionTier::Owner)
            {
                return Err(invalid());
            }
            if record.public_key_hex.len() != ML_DSA_65_PUBLIC_KEY_LEN * 2 {
                return Err(invalid());
            }
            let public_key = unhex(&record.public_key_hex).ok_or_else(&invalid)?;
            if derived_node_id(&public_key) != record.node_id {
                return Err(invalid());
            }
            let ip: IpAddr = record.ip.parse().map_err(|_| invalid())?;
            let count = ips.entry(ip).or_default();
            *count += 1;
            if *count > MAX_PENDING_PER_IP {
                return Err(invalid());
            }
            // Never recreate the original 10-minute budget after a restart.
            // A future creation time (clock rollback) is not eligible either.
            let Some(elapsed_ms) = now.checked_sub(record.created_ms) else { continue };
            if elapsed_ms >= REQUEST_TTL_MS {
                continue;
            }
            let deadline = monotonic
                .checked_add(Duration::from_millis(REQUEST_TTL_MS - elapsed_ms))
                .ok_or_else(&invalid)?;
            pending.push(Pending {
                id: record.id,
                node_id: record.node_id,
                public_key,
                label: record.label,
                ip: record.ip,
                created_ms: record.created_ms,
                deadline,
                approval: record.approval,
            });
        }
        Ok(pending)
    }

    fn save(&self, pending: &[Pending]) -> Result<(), String> {
        let snapshot = PendingSnapshot {
            version: PENDING_VERSION,
            binding: self.binding.clone(),
            pending: pending.iter().map(PendingRecord::from).collect(),
        };
        let data = serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?;
        if data.len() > MAX_PENDING_BYTES {
            return Err("Enrollment-Speicher überschreitet das Größenlimit.".to_owned());
        }
        write_private_atomic(&self.path, &data)?;
        #[cfg(unix)]
        if let Some(dir) = self.path.parent() {
            File::open(dir).and_then(|file| file.sync_all())
                .map_err(|error| format!("Enrollment-Verzeichnis nicht synchronisierbar: {error}"))?;
        }
        Ok(())
    }
}

fn private_file_if_present(path: &Path) -> Result<bool, String> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Enrollment-Speicher nicht prüfbar: {error}")),
    };
    if !meta.is_file() {
        return Err("Enrollment-Speicher ist keine reguläre private Datei.".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
            return Err("Enrollment-Dateirechte sind nicht privat.".to_owned());
        }
    }
    Ok(true)
}

fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes)
}

/// `phone-` + Hex der ersten 16 Bytes des BLAKE3-Hashs: die Node-ID ist an den
/// Schlüssel gebunden (kein Squatting fremder IDs).
fn derived_node_id(public_key: &[u8]) -> String {
    let hash = blake3::hash(public_key);
    format!("phone-{}", hex_of(&hash.as_bytes()[..16]))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Bereinigt ein Label: Steuerzeichen raus, Länge begrenzt, nicht leer.
pub(crate) fn clean_label(label: &str) -> Option<String> {
    let cleaned: String = label.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim().to_owned();
    (!cleaned.is_empty() && cleaned.chars().count() <= MAX_LABEL_CHARS).then_some(cleaned)
}

impl EnrollState {
    pub(crate) fn open(home: &Path, server: &ServerIdentity, tenant: &str) -> Result<Self, String> {
        let store = PendingStore::open(home, server, tenant)?;
        let pending = store.load()?;
        Ok(Self {
            nonces: HashMap::new(),
            pending,
            store,
        })
    }

    fn prune(&mut self) {
        self.nonces.retain(|_, issued| issued.elapsed() < NONCE_TTL);
        self.pending.retain(|pending| !pending.remaining().is_zero());
    }

    /// Stellt ein einmaliges Nonce aus (Hex).
    pub(crate) fn issue_nonce(&mut self) -> Result<String, Rejected> {
        self.prune();
        if self.nonces.len() >= MAX_NONCES {
            return Err(Rejected);
        }
        let nonce = random::<32>().ok_or(Rejected)?;
        self.nonces.insert(nonce, Instant::now());
        Ok(hex_of(&nonce))
    }

    /// Nimmt eine Anfrage an, wenn Nonce und Signatur stimmen. Das Nonce ist
    /// danach in jedem Fall verbraucht. Ein neu signierter Versuch mit
    /// identischer Geräteidentität und Bezeichnung behält die ausstehende
    /// Anfrage-ID und deren ursprüngliche Frist.
    pub(crate) fn submit(
        &mut self,
        nonce_hex: &str,
        node_id: &str,
        public_key_hex: &str,
        label: &str,
        signature_hex: &str,
        ip: &str,
    ) -> Result<Submission, SubmitError> {
        self.prune();
        let nonce: [u8; 32] = unhex(nonce_hex)
            .and_then(|b| b.try_into().ok())
            .ok_or(Rejected)?;
        if self.nonces.remove(&nonce).is_none() {
            return Err(Rejected.into());
        }
        let public_key = unhex(public_key_hex).ok_or(Rejected)?;
        let signature = unhex(signature_hex).ok_or(Rejected)?;
        if public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN
            || signature.len() != ML_DSA_65_SIGNATURE_LEN
            || node_id != derived_node_id(&public_key)
        {
            return Err(Rejected.into());
        }
        let label = clean_label(label).ok_or(Rejected)?;
        let node = NodeId::try_from_str(node_id).map_err(|_| Rejected)?;
        let mut pins = PinnedPeers::new();
        pins.pin(node.clone(), public_key.clone()).map_err(|_| Rejected)?;
        let message = enrollment_proof_message(&nonce, node_id, &public_key, &label);
        pins.verify(&node, &message, &signature).map_err(|_| Rejected)?;

        // A second browser tab (or a lost API response) must not invalidate
        // the first authenticated review link. Leave its signed fields,
        // original source, and deadline unchanged; never remove it on refusal.
        // The proof can take time; expire entries again before deciding.
        self.pending.retain(|pending| !pending.remaining().is_zero());
        if let Some(pending) = self.pending.iter().find(|p| p.node_id == node_id) {
            if pending.public_key != public_key || pending.label != label {
                return Err(Rejected.into());
            }
            return Ok(Submission::from_pending(pending, true));
        }
        if self.pending.len() >= MAX_PENDING
            || self.pending.iter().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP
        {
            return Err(Rejected.into());
        }
        let created_ms = unix_ms().ok_or_else(|| {
            SubmitError::Storage("Systemzeit für Enrollment nicht verfügbar.".to_owned())
        })?;
        let deadline = Instant::now().checked_add(REQUEST_TTL).ok_or_else(|| {
            SubmitError::Storage("Enrollment-Frist außerhalb des Wertebereichs.".to_owned())
        })?;
        let pending = Pending {
            id: hex_of(&random::<16>().ok_or(Rejected)?),
            node_id: node_id.to_owned(),
            public_key,
            label,
            ip: ip.to_owned(),
            created_ms,
            deadline,
            approval: None,
        };
        let mut next = self.pending.clone();
        next.push(pending);
        self.store.save(&next).map_err(SubmitError::Storage)?;
        self.pending = next;
        self.pending
            .last()
            .filter(|pending| !pending.remaining().is_zero())
            .map(|pending| Submission::from_pending(pending, false))
            .ok_or(SubmitError::Refused)
    }

    pub(crate) fn list(&mut self) -> Vec<Pending> {
        self.prune();
        self.pending.iter().filter(|request| request.approval.is_none()).cloned().collect()
    }

    pub(crate) fn get(&mut self, id: &str) -> Option<Pending> {
        self.prune();
        self.pending.iter().find(|p| p.id == id).cloned()
    }

    /// Persist an authenticated decision before touching the pin or registry.
    /// A recorded decision is readback-only: this method never authorizes it to be applied again.
    pub(crate) fn begin_approval(
        &mut self,
        id: &str,
        tier: PermissionTier,
        approve_optin: bool,
    ) -> Result<ApprovalStart, String> {
        self.prune();
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return Ok(ApprovalStart::Missing);
        };
        if let Some(approval) = self.pending[index].approval {
            return Ok(if approval.tier == tier && approval.approve_optin == approve_optin {
                ApprovalStart::Recorded(self.pending[index].clone())
            } else {
                ApprovalStart::DifferentDecision
            });
        }
        if tier == PermissionTier::Owner {
            return Ok(ApprovalStart::DifferentDecision);
        }
        let mut next = self.pending.clone();
        next[index].approval = Some(RecordedApproval {
            tier,
            approve_optin,
            phase: ApprovalPhase::Applying,
        });
        self.store.save(&next)?;
        self.pending = next;
        let pending = self.pending[index].clone();
        Ok(if pending.remaining().is_zero() {
            ApprovalStart::Missing
        } else {
            ApprovalStart::Applying(pending)
        })
    }

    /// Only callers that just verified both authoritative stores may call this.
    pub(crate) fn record_completion(&mut self, id: &str) -> Result<(), String> {
        // Even if the request expired during the write, never reset its deadline.
        let Some(index) = self.pending.iter().position(|p| p.id == id && p.approval.is_some()) else {
            return Ok(());
        };
        let Some(mut approval) = self.pending[index].approval else { return Ok(()) };
        if approval.phase == ApprovalPhase::Completed {
            return Ok(());
        }
        approval.phase = ApprovalPhase::Completed;
        let mut next = self.pending.clone();
        next[index].approval = Some(approval);
        self.store.save(&next)?;
        self.pending = next;
        Ok(())
    }

    /// Rejection consumes a pending request, not an already recorded decision.
    pub(crate) fn take(&mut self, id: &str) -> Result<Option<Pending>, String> {
        self.prune();
        let Some(index) = self.pending.iter().position(|p| p.id == id && p.approval.is_none()) else {
            return Ok(None);
        };
        let mut next = self.pending.clone();
        let removed = next.remove(index);
        self.store.save(&next)?;
        self.pending = next;
        Ok((!removed.remaining().is_zero()).then_some(removed))
    }
}

const MAX_APPROVAL_STORE_BYTES: usize = 8 * 1024 * 1024;

fn approved_record(
    tenant: &str,
    pending: &Pending,
    tier: PermissionTier,
    approve_optin: bool,
) -> Result<DeviceRecord, String> {
    let node_id = NodeId::try_from_str(&pending.node_id).map_err(|_| "Node-ID ungültig".to_owned())?;
    let device =
        DeviceId::try_from_str(&pending.node_id).map_err(|_| "Geräte-ID ungültig".to_owned())?;
    let tenant = TenantId::try_from_str(tenant)
        .map_err(|_| "[admin] tenant ist keine gültige Mandanten-ID".to_owned())?;
    Ok(DeviceRecord {
        node_id,
        device,
        tenant,
        tier,
        revoked: false,
        label: pending.label.replace('|', "/"),
        approve_optin,
    })
}

fn require_exact_pin(text: &str, pending: &Pending) -> Result<bool, String> {
    let expected = hex_of(&pending.public_key);
    let mut matching = text.lines().filter_map(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let (node, key) = line.split_once('|')?;
        (node.trim() == pending.node_id).then(|| key.trim())
    });
    match (matching.next(), matching.next()) {
        (None, None) => Ok(false),
        (Some(key), None) if key.eq_ignore_ascii_case(&expected) => Ok(true),
        _ => Err("Die Node-ID hat eine widersprüchliche oder mehrdeutige Schlüsselbindung.".to_owned()),
    }
}

fn approval_registry_records(registry: &DeviceRegistry) -> Result<Vec<DeviceRecord>, String> {
    let scan = registry
        .scan_bounded(MAX_APPROVAL_STORE_BYTES)
        .map_err(|error| format!("Geräte-Registry nicht lesbar: {error}"))?;
    if !scan.rejected.is_empty() {
        return Err("Geräte-Registry enthält ungültige Einträge.".to_owned());
    }
    Ok(scan.records)
}

fn approval_peers_text(path: &Path) -> Result<String, String> {
    let file = match harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("{PEERS_FILE} nicht lesbar: {error}")),
    };
    let metadata = file.metadata().map_err(|error| format!("{PEERS_FILE} nicht prüfbar: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_APPROVAL_STORE_BYTES as u64 {
        return Err(format!("{PEERS_FILE} ist keine reguläre Datei oder zu groß."));
    }
    let mut text = String::new();
    file.take(MAX_APPROVAL_STORE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("{PEERS_FILE} nicht lesbar: {error}"))?;
    if text.len() > MAX_APPROVAL_STORE_BYTES {
        return Err(format!("{PEERS_FILE} ist zu groß."));
    }
    Ok(text)
}

/// Readback only: both current files must match before offering the phone's link.
/// A revoked, modified, duplicate or partial entry never authorizes a repair.
pub(crate) fn verify_approval(
    remote_dir: &Path,
    tenant: &str,
    pending: &Pending,
) -> Result<bool, String> {
    let approval = pending.approval.ok_or_else(|| "Keine gespeicherte Freigabe.".to_owned())?;
    let expected = approved_record(tenant, pending, approval.tier, approval.approve_optin)?;
    let registry = DeviceRegistry::new(remote_dir);
    let records = approval_registry_records(&registry)?;
    let mut matches = records.iter().filter(|record| {
        record.node_id == expected.node_id || record.device == expected.device
    });
    if matches.next() != Some(&expected) || matches.next().is_some() {
        return Ok(false);
    }
    let peers = approval_peers_text(&remote_dir.join(PEERS_FILE))?;
    require_exact_pin(&peers, pending)
}

fn require_absent_record(registry: &DeviceRegistry, record: &DeviceRecord) -> Result<(), String> {
    if approval_registry_records(registry)?
        .iter()
        .any(|r| r.node_id == record.node_id || r.device == record.device)
    {
        return Err("Dieses Gerät ist bereits in der Registry eingetragen.".to_owned());
    }
    Ok(())
}

/// Do not journal an approval for a pre-existing enrollment, exact or otherwise.
pub(crate) fn preflight_approval(
    remote_dir: &Path,
    tenant: &str,
    pending: &Pending,
    tier: PermissionTier,
    approve_optin: bool,
) -> Result<(), String> {
    let record = approved_record(tenant, pending, tier, approve_optin)?;
    require_absent_record(&DeviceRegistry::new(remote_dir), &record)?;
    let text = approval_peers_text(&remote_dir.join(PEERS_FILE))?;
    require_exact_pin(&text, pending).map(|_| ())
}

/// Apply only the freshly journaled in-process decision. Never call this from
/// restart/readback code or from a recorded POST.
pub(crate) fn apply_approval(remote_dir: &Path, tenant: &str, pending: &Pending) -> Result<(), String> {
    let approval = pending.approval.ok_or_else(|| "Keine gespeicherte Freigabe.".to_owned())?;
    let record = approved_record(tenant, pending, approval.tier, approval.approve_optin)?;
    std::fs::create_dir_all(remote_dir)
        .map_err(|error| format!("{} nicht anlegbar: {error}", remote_dir.display()))?;
    let registry = DeviceRegistry::new(remote_dir);
    require_absent_record(&registry, &record)?;
    let peers_path = remote_dir.join(PEERS_FILE);
    let mut text = approval_peers_text(&peers_path)?;
    if !require_exact_pin(&text, pending)? {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{}|{}\n", pending.node_id, hex_of(&pending.public_key)));
        write_private_atomic(&peers_path, text.as_bytes())?;
    }
    registry
        .enroll(&record)
        .map_err(|error| format!("Geräte-Registry nicht schreibbar: {error}"))?;
    let registry_file = harw_fsutil::open_nofollow(registry.path(), harw_fsutil::OpenMode::read_write())
        .map_err(|error| format!("Geräte-Registry nicht zur Synchronisierung lesbar: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        registry_file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Geräte-Registry nicht privat setzbar: {error}"))?;
    }
    registry_file.sync_all()
        .map_err(|error| format!("Geräte-Registry nicht synchronisierbar: {error}"))?;
    #[cfg(unix)]
    File::open(remote_dir).and_then(|file| file.sync_all())
        .map_err(|error| format!("Geräte-Verzeichnis nicht synchronisierbar: {error}"))?;
    Ok(())
}

/// Öffentliche Serverseite des Profils (nur öffentliche Werte).
#[derive(Clone)]
pub(crate) struct ServerIdentity {
    pub node_id: String,
    pub public_key_hex: String,
    pub endpoint: String,
    /// Datei mit dem Onion-Locator (wird bei jedem Profil neu gelesen).
    pub onion_locator_file: std::path::PathBuf,
    /// Öffentliche Admin-URL (aus `[admin] public_url`), falls gesetzt.
    pub admin_url: Option<String>,
}

impl ServerIdentity {
    fn onion(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.onion_locator_file).ok()?;
        let text = text.trim();
        (text.len() <= 100 && text.contains(".onion:") && text.bytes().all(|b| b.is_ascii_graphic()))
            .then(|| text.to_owned())
    }

    /// Profil-JSON, wie `parseServerProfile` der App es erwartet.
    pub(crate) fn profile_json(&self) -> String {
        let mut profile = json!({
            "version": 1,
            "label": format!("Harw {}", self.node_id),
            "endpoint": self.endpoint,
            "server_node_id": self.node_id,
            "server_public_key_hex": self.public_key_hex,
        });
        if let Some(onion) = self.onion() {
            profile["onion"] = json!(onion);
        }
        if let Some(url) = &self.admin_url {
            profile["admin_url"] = json!(url);
        }
        profile.to_string()
    }

    /// `harw://profile?req=<id>&data=<base64url Profil>`.
    pub(crate) fn deep_link(&self, request_id: &str) -> String {
        format!(
            "harw://profile?req={request_id}&data={}",
            data_encoding::BASE64URL_NOPAD.encode(self.profile_json().as_bytes())
        )
    }
}

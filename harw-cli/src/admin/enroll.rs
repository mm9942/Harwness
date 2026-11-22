//! Enrollment-Handshake: Nonces, ausstehende Anfragen, Besitznachweis,
//! Eintrag in Registry und Pin-Datei sowie das öffentliche Server-Profil.
//!
//! # Beschreibung
//! Das Handy holt ein einmaliges Nonce (`GET /api/enroll/nonce`) und sendet
//! seine öffentlichen Felder zusammen mit einer ML-DSA-65-Signatur über
//! [`harw_node_transport::enrollment_proof_message`] (Besitznachweis, derselbe
//! Verifizierer wie der Transport: [`PinnedPeers`]). Erst ein angemeldeter
//! Admin kann die Anfrage freigeben. Nonces sind einmalig und kurzlebig,
//! Anfragen laufen nach 10 Minuten ab; alles ist mengenmäßig begrenzt, damit
//! ein anonymer Aufrufer weder Speicher füllen noch etwas Brauchbares
//! erhalten kann.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use harw_node_listener::{DeviceRecord, DeviceRegistry};
use harw_node_transport::{
    ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeVerifier as _, PinnedPeers,
    enrollment_proof_message,
};
use harw_types::{DeviceId, NodeId, PermissionTier, TenantId};
use serde_json::json;

use super::account::write_private_atomic;
use crate::session_listener::{PEERS_FILE, parse_peers};

/// Lebensdauer einer Anfrage.
pub(crate) const REQUEST_TTL: Duration = Duration::from_secs(600);
/// Lebensdauer eines Nonces.
const NONCE_TTL: Duration = Duration::from_secs(120);
const MAX_NONCES: usize = 512;
const MAX_PENDING: usize = 20;
const MAX_PENDING_PER_IP: usize = 3;
const MAX_LABEL_CHARS: usize = 64;

/// Eine ausstehende, bereits bewiesene Anfrage.
#[derive(Clone)]
pub(crate) struct Pending {
    pub id: String,
    pub node_id: String,
    pub public_key: Vec<u8>,
    pub label: String,
    pub ip: String,
    pub created: Instant,
}

impl Pending {
    /// Kurzer BLAKE3-Fingerabdruck des Schlüssels (wie `harw device list`).
    pub(crate) fn fingerprint(&self) -> String {
        fingerprint(&self.public_key)
    }

    pub(crate) fn remaining_secs(&self) -> u64 {
        REQUEST_TTL.saturating_sub(self.created.elapsed()).as_secs()
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

#[derive(Default)]
pub(crate) struct EnrollState {
    nonces: HashMap<[u8; 32], Instant>,
    pending: Vec<Pending>,
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
    fn prune(&mut self) {
        self.nonces.retain(|_, issued| issued.elapsed() < NONCE_TTL);
        self.pending.retain(|p| p.created.elapsed() < REQUEST_TTL);
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
    /// danach in jedem Fall verbraucht.
    pub(crate) fn submit(
        &mut self,
        nonce_hex: &str,
        node_id: &str,
        public_key_hex: &str,
        label: &str,
        signature_hex: &str,
        ip: &str,
    ) -> Result<String, Rejected> {
        self.prune();
        let nonce: [u8; 32] = unhex(nonce_hex)
            .and_then(|b| b.try_into().ok())
            .ok_or(Rejected)?;
        if self.nonces.remove(&nonce).is_none() {
            return Err(Rejected);
        }
        let public_key = unhex(public_key_hex).ok_or(Rejected)?;
        let signature = unhex(signature_hex).ok_or(Rejected)?;
        if public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN
            || signature.len() != ML_DSA_65_SIGNATURE_LEN
            || node_id != derived_node_id(&public_key)
        {
            return Err(Rejected);
        }
        let label = clean_label(label).ok_or(Rejected)?;
        let node = NodeId::try_from_str(node_id).map_err(|_| Rejected)?;
        let mut pins = PinnedPeers::new();
        pins.pin(node.clone(), public_key.clone()).map_err(|_| Rejected)?;
        let message = enrollment_proof_message(&nonce, node_id, &public_key, &label);
        pins.verify(&node, &message, &signature).map_err(|_| Rejected)?;

        self.pending.retain(|p| p.node_id != node_id);
        if self.pending.len() >= MAX_PENDING
            || self.pending.iter().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP
        {
            return Err(Rejected);
        }
        let id = hex_of(&random::<16>().ok_or(Rejected)?);
        self.pending.push(Pending {
            id: id.clone(),
            node_id: node_id.to_owned(),
            public_key,
            label,
            ip: ip.to_owned(),
            created: Instant::now(),
        });
        Ok(id)
    }

    pub(crate) fn list(&mut self) -> Vec<Pending> {
        self.prune();
        self.pending.clone()
    }

    pub(crate) fn get(&mut self, id: &str) -> Option<Pending> {
        self.prune();
        self.pending.iter().find(|p| p.id == id).cloned()
    }

    /// Entfernt und liefert die Anfrage.
    pub(crate) fn take(&mut self, id: &str) -> Option<Pending> {
        self.prune();
        let index = self.pending.iter().position(|p| p.id == id)?;
        Some(self.pending.remove(index))
    }
}

/// Trägt das Gerät in `node-devices.conf` ein und pinnt den Schlüssel in
/// `node-peers.conf`. Schreibt nichts, wenn die Node-ID oder das Gerät schon
/// vorkommt oder ein anderer Schlüssel gepinnt ist.
///
/// # Errors
/// Ein deutscher Text ohne Geheimnisse.
pub(crate) fn apply_approval(
    remote_dir: &Path,
    tenant: &str,
    pending: &Pending,
    tier: PermissionTier,
    approve_optin: bool,
) -> Result<(), String> {
    let node = NodeId::try_from_str(&pending.node_id).map_err(|_| "Node-ID ungültig".to_owned())?;
    let device =
        DeviceId::try_from_str(&pending.node_id).map_err(|_| "Geräte-ID ungültig".to_owned())?;
    let tenant = TenantId::try_from_str(tenant)
        .map_err(|_| "[admin] tenant ist keine gültige Mandanten-ID".to_owned())?;
    std::fs::create_dir_all(remote_dir)
        .map_err(|error| format!("{} nicht anlegbar: {error}", remote_dir.display()))?;
    let registry = DeviceRegistry::new(remote_dir);
    let scan = registry
        .scan()
        .map_err(|error| format!("Geräte-Registry nicht lesbar: {error}"))?;
    if scan
        .records
        .iter()
        .any(|r| r.node_id == node || r.device == device)
    {
        return Err("Dieses Gerät ist bereits in der Registry eingetragen.".to_owned());
    }
    let peers_path = remote_dir.join(PEERS_FILE);
    let peers_text = match std::fs::read_to_string(&peers_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{PEERS_FILE} nicht lesbar: {error}")),
    };
    let (pins, _) = parse_peers(&peers_text);
    let mut write_pin = true;
    if pins.is_known(&node) {
        let same = peers_text.lines().any(|line| {
            line.split_once('|').is_some_and(|(n, k)| {
                n.trim() == pending.node_id && k.trim().eq_ignore_ascii_case(&hex_of(&pending.public_key))
            })
        });
        if !same {
            return Err("Für diese Node-ID ist bereits ein anderer Schlüssel gepinnt.".to_owned());
        }
        write_pin = false;
    }
    if write_pin {
        let mut text = peers_text;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{}|{}\n", pending.node_id, hex_of(&pending.public_key)));
        write_private_atomic(&peers_path, text.as_bytes())?;
    }
    let record = DeviceRecord {
        node_id: node,
        device,
        tenant,
        tier,
        revoked: false,
        label: pending.label.replace('|', "/"),
        approve_optin,
    };
    registry
        .enroll(&record)
        .map_err(|error| format!("Geräte-Registry nicht schreibbar: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(registry.path(), std::fs::Permissions::from_mode(0o600));
    }
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

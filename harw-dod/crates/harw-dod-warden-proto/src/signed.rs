//! Warden-Proof v2: signierte Autorisierung (C-WPROTO, W3; PL-100 WP 3.1;
//! Befunde F-001, F-088; Bedrohungen B1, B12, B13, B21, B24).
//!
//! # Zweck
//! Ersetzt den fälschbaren v1-Beleg ([`crate::proof::AuthorizationProof`],
//! ohne MAC/Nonce/Ablauf) durch [`SignedAuthorization`]: einen mit
//! **keyed BLAKE3** authentisierten Datensatz, den nur ein Halter eines
//! [`ProofKey`] (der Escalator) ausstellen kann und den der Warden gegen
//! einen [`KeyRing`], eine [`ProofPolicy`] und einen [`NonceLedger`] prüft.
//!
//! # Gebundene Felder (Leitprinzip I7, Entscheidungen O3, O4)
//! Der Escalator füllt einen [`AuthorizationBody`] und signiert ihn mit
//! [`SignedAuthorization::sign`]. Der MAC deckt **jedes** Feld ab: Befund
//! (`finding_id`), Freigebender (`approver`), Aktion (über ihren
//! Bindungsdigest), Stufe, Ziel-Tripel aus cgroup-Pfad, Kernel-ID und
//! `first_seen` ([`CgroupTarget`]), optionale Haltedauer (`hold_secs`;
//! `None` = bis zur Aufhebung), die Bestätigung „nicht umkehrbar“
//! (`irreversible_ack`), Nonce, Ausstellungs- und Ablaufzeit sowie Key-ID und
//! Version.
//!
//! # MAC-Eingabe (kanonisch)
//! `PROOF_DOMAIN = "harw:warden-proof:v2\0"`, danach je Feld genau ein
//! längenpräfixierter Block (`u64`-BE-Länge + Bytes, siehe `canonical.rs`), in
//! dieser Reihenfolge:
//! 1. `version` (u16 BE)
//! 2. `key_id` (UTF-8)
//! 3. `finding_id` (UTF-8)
//! 4. `approver` (zusammengesetzt: Tag `operator` + `id` bzw.
//!    `channel_peer` + `channel` + `peer`)
//! 5. `action_digest` (32 B, [`WardenAction::binding_digest`]: Variante + cgroup)
//! 6. `stage` (Wire-Name)
//! 7. `target` (zusammengesetzt: Pfad, Kernel-ID als u64 BE, `first_seen`)
//! 8. `hold_secs` (`[0]` = bis zur Aufhebung, sonst `[1] ‖ u32 BE`)
//! 9. `irreversible_ack` (`[0]` oder `[1]`)
//! 10. `nonce` (16 B)
//! 11. `issued_at` (i64 BE Sekunden ‖ i32 BE Nanosekunden)
//! 12. `expires_at` (wie `issued_at`)
//!
//! Der MAC selbst ist `blake3::keyed_hash(key, bytes)`.
//!
//! # Prüfreihenfolge von [`SignedAuthorization::verify`]
//! 1. Version (`== WARDEN_PROTOCOL_VERSION`, v1 abgelehnt)
//! 2. Key-ID im [`KeyRing`]
//! 3. MAC — konstante Zeit (`blake3::Hash: PartialEq` nutzt `constant_time_eq`)
//! 4. Zeitfenster — `issued_at < expires_at <= issued_at + min(policy.max_ttl_secs, 120)`,
//!    `issued_at <= now + skew`, `now < expires_at`
//! 5. Aktion — `action.binding_digest() == action_digest`, danach keine
//!    stillgelegte Aktion ([`WardenAction::is_retired`])
//! 6. Stufe (`action.is_admissible_from(stage)`: Thaw ab `RuleTriggered`,
//!    LiftIsolation nur `Escalated`)
//! 7. `irreversible_ack == true` für Kill und Isolation (O4)
//! 8. Hold — nur `FreezeCgroup` darf einen tragen, nie `Some(0)`; der Hold
//!    taut nach Ablauf nur auf (Durchsetzung im Warden)
//! 9. Freigebender — lokaler Operator mit nicht leerer Kennung (≤ 128 Bytes,
//!    ohne Steuerzeichen); ein Kanal-Peer gibt nie einen Eingriff frei (I1)
//! 10. cgroup — Zielpfad == Aktions-cgroup und strikt unterhalb eines
//!     erlaubten Präfixes (Vergleich nach Pfadkomponenten)
//! 11. Nonce — **zuletzt**, damit gefälschte oder unpassende Anfragen keine
//!     Nonces verbrauchen; der Ledger muss vor `Ok` dauerhaft schreiben.
//!
//! Nicht in dieser Crate: der Peer-UID-Check (`SO_PEERCRED` == Escalator, im
//! Warden-Binary vor dem Dekodieren), die Warden-eigene Deny-Liste und die
//! Zielprüfung unmittelbar vor dem Eingriff. Für Letztere liefert
//! [`VerifiedAuthorization::confirm_target`] den Vergleich gegen die vom
//! Warden beobachtete cgroup. Die ausdrückliche Ablehnung eines v1-Auftrags
//! liegt in [`crate::response::WardenRequest::decode`].
//!
//! # Schlüsselmaterial
//! [`ProofKey`] ist nicht `Clone`, nicht `PartialEq`, `Debug` gibt nur
//! `ProofKey(<redacted>)` aus, und `Drop` überschreibt die Bytes (best effort:
//! ohne `zeroize`-Crate kann der Compiler das Schreiben theoretisch entfernen;
//! `zeroize` ist als dep-request im Ledger `C-WPROTO.md` beantragt). Der MAC
//! ist symmetrisch: wer einen Schlüssel aus `proof_key_dir` besitzt, kann
//! Aufträge ausstellen (THREAT-MODEL §4); `irreversible_ack` schützt deshalb
//! nicht gegen einen kompromittierten Schlüssel, nur gegen einen Escalator,
//! der ohne ausdrückliche Bestätigung signiert.
//!
//! # Nebenläufigkeit
//! Alle Werttypen sind `Send + Sync`. [`MemoryNonceLedger`] nutzt einen
//! `Mutex`; ein vergifteter Mutex wird als [`ProofError::LedgerIo`] gemeldet
//! (fail closed). Keine Systemuhr: `now` ist immer Parameter.
//!
//! # Fehler
//! [`ProofError`] (siehe `error.rs`).
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_proto::{
//!     AuthorizationBody, CgroupKernelId, CgroupTarget, EscalationStage, KeyId, KeyRing,
//!     MemoryNonceLedger, ProofKey, ProofPolicy, SignedAuthorization, WardenAction,
//! };
//! use harw_types::{ApprovalActor, CgroupId, FindingId};
//!
//! let (Ok(key_id), Ok(cgroup), Ok(finding_id), Ok(kernel_id), Ok(now)) = (
//!     KeyId::new("k2026"),
//!     CgroupId::try_from_str("harw.slice/job-1"),
//!     FindingId::try_from_str("finding-1"),
//!     CgroupKernelId::new(4242),
//!     jiff::Timestamp::from_second(1_800_000_000),
//! ) else {
//!     return;
//! };
//! let Ok(expires_at) = now.checked_add(jiff::SignedDuration::from_secs(60)) else {
//!     return;
//! };
//! let mut ring = KeyRing::new();
//! assert!(ring.insert(key_id.clone(), ProofKey::from_bytes([7u8; 32])).is_ok());
//!
//! let action = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
//! let body = AuthorizationBody {
//!     finding_id,
//!     approver: ApprovalActor::Operator { id: "operator-1".to_owned() },
//!     action: action.clone(),
//!     stage: EscalationStage::RuleTriggered,
//!     target: CgroupTarget::new(cgroup, kernel_id, now),
//!     hold_secs: None,
//!     irreversible_ack: false,
//!     nonce: [1u8; 16],
//!     issued_at: now,
//!     expires_at,
//! };
//! let auth = SignedAuthorization::sign(&ProofKey::from_bytes([7u8; 32]), &key_id, body);
//! let Ok(policy) = ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()]) else {
//!     return;
//! };
//! let ledger = MemoryNonceLedger::new();
//! assert!(auth.verify(&ring, &action, now, &policy, &ledger).is_ok());
//! // Replay derselben Nonce scheitert.
//! assert!(auth.verify(&ring, &action, now, &policy, &ledger).is_err());
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use harw_types::{ApprovalActor, CgroupId, ContentDigest, FindingId};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::action::WardenAction;
use crate::canonical::{PROOF_DOMAIN, put_bool, put_field, put_nested, put_opt_u32, put_timestamp};
use crate::error::ProofError;
use crate::stage::EscalationStage;
use crate::target::CgroupTarget;

/// Protokoll- und Proof-Version, die dieser Stand ausstellt und akzeptiert.
pub const WARDEN_PROTOCOL_VERSION: u16 = 2;

/// Harte Obergrenze für jede TTL, unabhängig von der Policy (Plan: TTL ≤ 120 s).
pub const MAX_TTL_SECS_CAP: u32 = 120;

/// Harte Obergrenze für den tolerierten Uhrenversatz (Zukunftsrichtung).
pub const MAX_CLOCK_SKEW_SECS_CAP: u32 = 30;

// Maximale Länge einer Key-ID in Bytes.
const KEY_ID_MAX_LEN: usize = 64;

// Maximale Länge der Operator-Kennung des Freigebenden in Bytes.
const APPROVER_ID_MAX_LEN: usize = 128;

/// Kennung eines Proof-Schlüssels (für Rotation mit mehreren gültigen Schlüsseln).
///
/// # Description
/// 1–64 Zeichen aus `[A-Za-z0-9._-]`. Wird auf der Leitung als String
/// übertragen und beim Deserialisieren validiert.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::KeyId;
///
/// assert!(KeyId::new("proof-2026-09").is_ok());
/// assert!(KeyId::new("bad id").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyId(String);

impl KeyId {
    /// Validates and wraps a key id.
    ///
    /// # Arguments
    /// - `id` (`impl Into<String>`): die Kennung.
    ///
    /// # Errors
    /// - [`ProofError::InvalidKeyId`]: leer, länger als 64 Bytes oder mit
    ///   Zeichen außerhalb `[A-Za-z0-9._-]`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::KeyId;
    /// assert_eq!(KeyId::new("k1").unwrap().as_str(), "k1");
    /// ```
    pub fn new(id: impl Into<String>) -> Result<Self, ProofError> {
        let id = id.into();
        let well_formed = !id.is_empty()
            && id.len() <= KEY_ID_MAX_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if well_formed {
            Ok(Self(id))
        } else {
            Err(ProofError::InvalidKeyId)
        }
    }

    /// Returns the key id as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for KeyId {
    type Error = ProofError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<KeyId> for String {
    fn from(value: KeyId) -> Self {
        value.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 32-Byte-Geheimschlüssel für keyed BLAKE3.
///
/// # Description
/// Bewusst weder `Clone` noch `PartialEq`/`Serialize`; `Debug` ist redigiert.
/// `Drop` überschreibt die Bytes (best effort, siehe Moduldoku). Der Aufrufer
/// ist dafür verantwortlich, eigene Kopien des Rohmaterials zu vermeiden.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::ProofKey;
///
/// let key = ProofKey::from_bytes([9u8; 32]);
/// assert_eq!(format!("{key:?}"), "ProofKey(<redacted>)");
/// ```
pub struct ProofKey([u8; blake3::KEY_LEN]);

impl ProofKey {
    /// Wraps 32 bytes of key material (moved in).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; blake3::KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Builds a key from a slice that must be exactly 32 bytes long.
    ///
    /// # Errors
    /// - [`ProofError::InvalidKeyMaterial`]: Länge ≠ 32.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::ProofKey;
    /// assert!(ProofKey::try_from_slice(&[0u8; 32]).is_ok());
    /// assert!(ProofKey::try_from_slice(&[0u8; 31]).is_err());
    /// ```
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self, ProofError> {
        let array: [u8; blake3::KEY_LEN] = bytes
            .try_into()
            .map_err(|_| ProofError::InvalidKeyMaterial)?;
        Ok(Self(array))
    }

    // Keyed BLAKE3 über `input`.
    fn mac(&self, input: &[u8]) -> blake3::Hash {
        blake3::keyed_hash(&self.0, input)
    }
}

impl fmt::Debug for ProofKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProofKey(<redacted>)")
    }
}

impl Drop for ProofKey {
    fn drop(&mut self) {
        self.0 = [0u8; blake3::KEY_LEN];
        // `black_box` hält das Überschreiben für den Optimierer beobachtbar
        // (best effort, siehe Moduldoku); den zurückgegebenen Verweis braucht
        // niemand.
        let _ = std::hint::black_box(&self.0);
    }
}

/// Menge gültiger Proof-Schlüssel, adressiert per [`KeyId`] (Rotation).
///
/// # Description
/// Während einer Rotation liegen alter und neuer Schlüssel gleichzeitig im
/// Ring; der Escalator signiert mit dem neuen, der Warden akzeptiert beide,
/// bis der alte per [`KeyRing::remove`] entfernt wird. `Debug` listet nur
/// Key-IDs.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::{KeyId, KeyRing, ProofKey};
///
/// let mut ring = KeyRing::new();
/// ring.insert(KeyId::new("old").unwrap(), ProofKey::from_bytes([1; 32])).unwrap();
/// ring.insert(KeyId::new("new").unwrap(), ProofKey::from_bytes([2; 32])).unwrap();
/// assert_eq!(ring.len(), 2);
/// ```
#[derive(Default)]
pub struct KeyRing {
    keys: HashMap<KeyId, ProofKey>,
}

impl KeyRing {
    /// Creates an empty key ring (verifies nothing).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a key under `key_id`.
    ///
    /// # Errors
    /// - [`ProofError::DuplicateKeyId`]: `key_id` ist bereits belegt (ein
    ///   Schlüssel wird nie still ersetzt).
    pub fn insert(&mut self, key_id: KeyId, key: ProofKey) -> Result<(), ProofError> {
        if self.keys.contains_key(&key_id) {
            return Err(ProofError::DuplicateKeyId);
        }
        self.keys.insert(key_id, key);
        Ok(())
    }

    /// Removes the key under `key_id`; returns whether one was present.
    pub fn remove(&mut self, key_id: &KeyId) -> bool {
        self.keys.remove(key_id).is_some()
    }

    /// Returns whether a key with `key_id` is present.
    #[must_use]
    pub fn contains(&self, key_id: &KeyId) -> bool {
        self.keys.contains_key(key_id)
    }

    /// Returns the key ids in the ring, sorted (for logs and rotation checks).
    ///
    /// # Description
    /// Gibt nur Kennungen heraus, nie Schlüsselmaterial.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{KeyId, KeyRing, ProofKey};
    ///
    /// let mut ring = KeyRing::new();
    /// let (Ok(b), Ok(a)) = (KeyId::new("b"), KeyId::new("a")) else { return };
    /// assert!(ring.insert(b, ProofKey::from_bytes([1; 32])).is_ok());
    /// assert!(ring.insert(a, ProofKey::from_bytes([2; 32])).is_ok());
    /// let ids: Vec<&str> = ring.key_ids().into_iter().map(KeyId::as_str).collect();
    /// assert_eq!(ids, ["a", "b"]);
    /// ```
    #[must_use]
    pub fn key_ids(&self) -> Vec<&KeyId> {
        let mut ids: Vec<&KeyId> = self.keys.keys().collect();
        ids.sort_unstable();
        ids
    }

    /// Returns the number of keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Returns whether the ring holds no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    // Schlüssel für `key_id`, falls vorhanden.
    fn get(&self, key_id: &KeyId) -> Option<&ProofKey> {
        self.keys.get(key_id)
    }
}

impl fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<&str> = self.key_ids().into_iter().map(KeyId::as_str).collect();
        f.debug_struct("KeyRing").field("key_ids", &ids).finish()
    }
}

/// Prüfpolitik des Wardens für [`SignedAuthorization::verify`].
///
/// # Description
/// - `max_ttl_secs`: größte akzeptierte Gültigkeit (`expires_at - issued_at`);
///   wirksam ist immer `min(max_ttl_secs, MAX_TTL_SECS_CAP)`.
/// - `max_clock_skew_secs`: wie weit `issued_at` in der Zukunft liegen darf;
///   wirksam `min(.., MAX_CLOCK_SKEW_SECS_CAP)`.
/// - `allowed_cgroup_prefixes`: relative cgroup-Pfade (z. B. `"harw.slice"`).
///   Eine Ziel-cgroup ist erlaubt, wenn sie **strikt unterhalb** eines Präfixes
///   liegt (komponentenweise; `"harw.slice2/x"` passt nicht auf `"harw.slice"`,
///   `"harw.slice"` selbst auch nicht). Leere Liste ⇒ nichts erlaubt.
///
/// `#[non_exhaustive]`: Konstruktion nur über [`ProofPolicy::new`]; spätere
/// Mutation der öffentlichen Felder wird bei `verify` erneut begrenzt bzw.
/// (ungültige Präfixe) als nicht passend behandelt.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::ProofPolicy;
/// use harw_types::CgroupId;
///
/// let policy = ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]).unwrap();
/// assert!(policy.allows_cgroup(&CgroupId::try_from_str("harw.slice/job-1").unwrap()));
/// assert!(!policy.allows_cgroup(&CgroupId::try_from_str("user.slice").unwrap()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProofPolicy {
    /// Größte akzeptierte Gültigkeit in Sekunden (1..=120).
    pub max_ttl_secs: u32,
    /// Tolerierter Uhrenversatz in Sekunden (0..=30).
    pub max_clock_skew_secs: u32,
    /// Erlaubte cgroup-Präfixe (relative Pfade, komponentenweise verglichen).
    pub allowed_cgroup_prefixes: Vec<String>,
}

impl ProofPolicy {
    /// Builds a validated policy.
    ///
    /// # Errors
    /// - [`ProofError::InvalidPolicy`]: `max_ttl_secs` nicht in `1..=120`,
    ///   `max_clock_skew_secs > 30`, oder ein Präfix ist kein wohlgeformter
    ///   relativer Pfad (leere Komponente, `.`, `..`, Steuerzeichen).
    pub fn new(
        max_ttl_secs: u32,
        max_clock_skew_secs: u32,
        allowed_cgroup_prefixes: Vec<String>,
    ) -> Result<Self, ProofError> {
        let ttl_ok = (1..=MAX_TTL_SECS_CAP).contains(&max_ttl_secs);
        let skew_ok = max_clock_skew_secs <= MAX_CLOCK_SKEW_SECS_CAP;
        let prefixes_ok = allowed_cgroup_prefixes
            .iter()
            .all(|p| path_components(p).is_some());
        if ttl_ok && skew_ok && prefixes_ok {
            Ok(Self {
                max_ttl_secs,
                max_clock_skew_secs,
                allowed_cgroup_prefixes,
            })
        } else {
            Err(ProofError::InvalidPolicy)
        }
    }

    /// Returns whether `cgroup` lies strictly below one of the allowed prefixes.
    ///
    /// # Description
    /// Beide Seiten werden in Komponenten zerlegt; ungültige Pfade (leer,
    /// führender/doppelter `/`, `.`, `..`, Steuerzeichen) passen nie.
    #[must_use]
    pub fn allows_cgroup(&self, cgroup: &CgroupId) -> bool {
        let Some(target) = path_components(cgroup.as_str()) else {
            return false;
        };
        self.allowed_cgroup_prefixes.iter().any(|prefix| {
            path_components(prefix).is_some_and(|prefix| {
                target.len() > prefix.len() && target.iter().zip(&prefix).all(|(t, p)| t == p)
            })
        })
    }

    // Wirksame TTL-Obergrenze.
    fn effective_max_ttl(&self) -> u32 {
        self.max_ttl_secs.min(MAX_TTL_SECS_CAP)
    }

    // Wirksamer Uhrenversatz.
    fn effective_skew(&self) -> u32 {
        self.max_clock_skew_secs.min(MAX_CLOCK_SKEW_SECS_CAP)
    }
}

// Zerlegt einen relativen cgroup-Pfad in Komponenten; `None` bei ungültiger Form.
fn path_components(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').collect();
    let valid = parts.iter().all(|part| {
        !part.is_empty() && *part != "." && *part != ".." && !part.chars().any(char::is_control)
    });
    valid.then_some(parts)
}

/// Dauerhafter Replay-Schutz für Nonces.
///
/// # Description
/// Vertrag für Implementierungen (W5 D-WARDEN: persistent):
/// - `check_and_record` ist **atomar** (prüfen und eintragen unter einem Lock).
/// - Ist `nonce` bereits eingetragen: [`ProofError::NonceReplayed`].
/// - Vor `Ok(())` ist der Eintrag dauerhaft gespeichert (`fsync`), sonst
///   [`ProofError::LedgerIo`] — fail closed.
/// - Ein Eintrag darf frühestens nach `expires_at` (plus eigenem
///   Sicherheitsabstand für Uhrensprünge) entfernt werden.
pub trait NonceLedger {
    /// Atomically checks `nonce` and records it until `expires_at`.
    ///
    /// # Errors
    /// - [`ProofError::NonceReplayed`]: Nonce schon verbraucht.
    /// - [`ProofError::LedgerIo`]: Speicher nicht verfügbar.
    fn check_and_record(&self, nonce: &[u8; 16], expires_at: Timestamp) -> Result<(), ProofError>;
}

/// Nicht-dauerhafter In-Memory-[`NonceLedger`] (Tests, Referenz).
///
/// # Description
/// **Nicht** für den produktiven Warden: überlebt keinen Neustart. Einträge
/// werden nur über [`MemoryNonceLedger::prune_expired`] entfernt.
///
/// # Concurrency
/// Intern `Mutex`; `Send + Sync`. Vergifteter Mutex ⇒ [`ProofError::LedgerIo`].
#[derive(Debug, Default)]
pub struct MemoryNonceLedger {
    seen: Mutex<HashMap<[u8; 16], Timestamp>>,
}

impl MemoryNonceLedger {
    /// Creates an empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Removes entries whose `expires_at` is strictly before `cutoff`; returns how many.
    ///
    /// # Errors
    /// - [`ProofError::LedgerIo`]: Mutex vergiftet.
    pub fn prune_expired(&self, cutoff: Timestamp) -> Result<usize, ProofError> {
        let mut seen = self.seen.lock().map_err(|_| poisoned())?;
        let before = seen.len();
        seen.retain(|_, expires_at| *expires_at >= cutoff);
        Ok(before.saturating_sub(seen.len()))
    }

    /// Returns the number of recorded nonces.
    ///
    /// # Errors
    /// - [`ProofError::LedgerIo`]: Mutex vergiftet.
    pub fn recorded_count(&self) -> Result<usize, ProofError> {
        Ok(self.seen.lock().map_err(|_| poisoned())?.len())
    }
}

// Fehler für einen vergifteten Ledger-Mutex.
fn poisoned() -> ProofError {
    ProofError::LedgerIo(std::io::Error::other("nonce ledger mutex poisoned"))
}

impl NonceLedger for MemoryNonceLedger {
    fn check_and_record(&self, nonce: &[u8; 16], expires_at: Timestamp) -> Result<(), ProofError> {
        let mut seen = self.seen.lock().map_err(|_| poisoned())?;
        if seen.contains_key(nonce) {
            return Err(ProofError::NonceReplayed);
        }
        seen.insert(*nonce, expires_at);
        Ok(())
    }
}

/// Ungesignierter Inhalt einer Autorisierung, den der Escalator füllt
/// (PL-100 WP 3.1).
///
/// # Description
/// Jedes Feld wird vom MAC abgedeckt (siehe Moduldoku, „MAC-Eingabe“).
/// [`SignedAuthorization::sign`] validiert nichts; alle Regeln prüft erst
/// [`SignedAuthorization::verify`]. Bewusst ohne `#[non_exhaustive]` und ohne
/// `Default`: ein neues Feld soll jeden Aussteller zum Compile-Fehler zwingen,
/// damit kein Feld unbedacht leer bleibt.
///
/// # Examples
/// Siehe Moduldoku von [`crate::signed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationBody {
    /// Der auslösende Befund.
    pub finding_id: FindingId,
    /// Wer freigegeben hat; nur ein lokaler Operator wird akzeptiert.
    pub approver: ApprovalActor,
    /// Die autorisierte Aktion (gebunden über [`WardenAction::binding_digest`]).
    pub action: WardenAction,
    /// Die Stufe laut Eskalationsleiter.
    pub stage: EscalationStage,
    /// Ziel-Tripel; `target.path` muss der cgroup von `action` entsprechen.
    pub target: CgroupTarget,
    /// Optionale Haltedauer eines Freeze in Sekunden; `None` = bis zur
    /// Aufhebung (O3). Nur bei `FreezeCgroup` zulässig, nie `Some(0)`.
    pub hold_secs: Option<u32>,
    /// Die vom Operator ausdrücklich bestätigte Unumkehrbarkeit (O4); Pflicht
    /// für Kill und Isolation.
    pub irreversible_ack: bool,
    /// Einmalwert aus einer CSPRNG; nie wiederverwenden.
    pub nonce: [u8; 16],
    /// Ausstellungszeit (injiziert).
    pub issued_at: Timestamp,
    /// Ablaufzeit; `expires_at - issued_at` liegt in `(0, max_ttl]`.
    pub expires_at: Timestamp,
}

/// Signierte Autorisierung einer Warden-Aktion (Proof v2).
///
/// # Description
/// Alle Felder privat; Konstruktion ausschließlich über
/// [`SignedAuthorization::sign`] (Escalator) oder Deserialisierung
/// (Warden). Siehe Moduldoku für MAC-Eingabe und Prüfreihenfolge.
///
/// # Wire-Format
/// JSON-Objekt, `deny_unknown_fields`; `nonce` 32 und `mac` 64
/// Kleinbuchstaben-Hexzeichen, `action_digest` Hex (harw-types),
/// `issued_at`/`expires_at`/`target.first_seen` RFC 3339, `hold_secs`
/// Zahl oder `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedAuthorization {
    version: u16,
    key_id: KeyId,
    finding_id: FindingId,
    approver: ApprovalActor,
    action_digest: ContentDigest,
    stage: EscalationStage,
    target: CgroupTarget,
    hold_secs: Option<u32>,
    irreversible_ack: bool,
    #[serde(with = "crate::canonical::hex16")]
    nonce: [u8; 16],
    issued_at: Timestamp,
    expires_at: Timestamp,
    #[serde(with = "crate::canonical::hex32")]
    mac: [u8; 32],
}

impl SignedAuthorization {
    /// Signs an authorization body with `key` (Escalator side).
    ///
    /// # Description
    /// Setzt `version = WARDEN_PROTOCOL_VERSION`, bindet
    /// [`WardenAction::binding_digest`] von `body.action` und berechnet den
    /// keyed-BLAKE3-MAC über die kanonischen Bytes aller Felder. Validiert
    /// nichts (TTL, Präfix, Stufe, Bestätigung, Hold und Freigebenden prüft
    /// ausschließlich `verify`); `body.nonce` muss vom Aufrufer aus einer
    /// CSPRNG stammen und darf nie wiederverwendet werden.
    ///
    /// # Arguments
    /// - `key` (`&ProofKey`): geheimer Schlüssel.
    /// - `key_id` (`&KeyId`): Kennung dieses Schlüssels im Warden-Ring.
    /// - `body` (`AuthorizationBody`): der zu signierende Inhalt.
    ///
    /// # Returns
    /// Die signierte Autorisierung.
    ///
    /// # Examples
    /// Siehe Moduldoku von [`crate::signed`].
    #[must_use]
    pub fn sign(key: &ProofKey, key_id: &KeyId, body: AuthorizationBody) -> Self {
        let AuthorizationBody {
            finding_id,
            approver,
            action,
            stage,
            target,
            hold_secs,
            irreversible_ack,
            nonce,
            issued_at,
            expires_at,
        } = body;
        let mut signed = Self {
            version: WARDEN_PROTOCOL_VERSION,
            key_id: key_id.clone(),
            finding_id,
            approver,
            action_digest: action.binding_digest(),
            stage,
            target,
            hold_secs,
            irreversible_ack,
            nonce,
            issued_at,
            expires_at,
            // Platzhalter; der MAC deckt dieses Feld nicht ab und wird
            // unmittelbar danach gesetzt.
            mac: [0u8; 32],
        };
        signed.mac = *key.mac(&signed.authenticated_bytes()).as_bytes();
        signed
    }

    /// Verifies this authorization for `action` (Warden side).
    ///
    /// # Description
    /// Prüft strikt in der Reihenfolge der Moduldoku und bricht beim ersten
    /// Fehler ab. Der Nonce-Ledger wird nur erreicht, wenn alle vorherigen
    /// Schritte bestanden wurden.
    ///
    /// # Arguments
    /// - `ring` (`&KeyRing`): gültige Schlüssel.
    /// - `action` (`&WardenAction`): die tatsächlich angeforderte Aktion.
    /// - `now` (`Timestamp`): Warden-Uhr (injiziert).
    /// - `policy` (`&ProofPolicy`): TTL-, Versatz- und Präfixgrenzen.
    /// - `ledger` (`&dyn NonceLedger`): Replay-Schutz.
    ///
    /// # Returns
    /// [`VerifiedAuthorization`] — der einzige Weg, diesen Typ zu erhalten.
    ///
    /// # Errors
    /// In Prüfreihenfolge: [`ProofError::UnsupportedVersion`],
    /// [`ProofError::UnknownKeyId`], [`ProofError::BadMac`],
    /// [`ProofError::TtlOutOfPolicy`] / [`ProofError::TimestampOutOfRange`] /
    /// [`ProofError::NotYetValid`] / [`ProofError::Expired`],
    /// [`ProofError::ActionMismatch`] / [`ProofError::RetiredAction`],
    /// [`ProofError::NotAdmissibleAtStage`],
    /// [`ProofError::IrreversibleAckMissing`], [`ProofError::InvalidHold`],
    /// [`ProofError::InvalidApprover`],
    /// [`ProofError::CgroupMismatch`] / [`ProofError::CgroupNotAllowed`],
    /// [`ProofError::NonceReplayed`] / [`ProofError::LedgerIo`].
    ///
    /// # Concurrency
    /// `&self`; Nebenläufigkeit des Ledgers liegt bei dessen Implementierung.
    pub fn verify(
        &self,
        ring: &KeyRing,
        action: &WardenAction,
        now: Timestamp,
        policy: &ProofPolicy,
        ledger: &dyn NonceLedger,
    ) -> Result<VerifiedAuthorization, ProofError> {
        // 1. Version
        if self.version != WARDEN_PROTOCOL_VERSION {
            return Err(ProofError::UnsupportedVersion(self.version));
        }
        // 2. Key-ID
        let key = ring.get(&self.key_id).ok_or(ProofError::UnknownKeyId)?;
        // 3. MAC (konstante Zeit über blake3::Hash::eq)
        let expected = key.mac(&self.authenticated_bytes());
        if blake3::Hash::from_bytes(self.mac) != expected {
            return Err(ProofError::BadMac);
        }
        // 4. Zeitfenster
        let latest_expiry = self
            .issued_at
            .checked_add(SignedDuration::from_secs(i64::from(
                policy.effective_max_ttl(),
            )))
            .map_err(|_| ProofError::TimestampOutOfRange)?;
        if self.expires_at <= self.issued_at || self.expires_at > latest_expiry {
            return Err(ProofError::TtlOutOfPolicy);
        }
        let latest_issue = now
            .checked_add(SignedDuration::from_secs(i64::from(
                policy.effective_skew(),
            )))
            .map_err(|_| ProofError::TimestampOutOfRange)?;
        if self.issued_at > latest_issue {
            return Err(ProofError::NotYetValid);
        }
        if now >= self.expires_at {
            return Err(ProofError::Expired);
        }
        // 5. Aktion: Bindung, dann keine stillgelegte Aktion
        if action.binding_digest() != self.action_digest {
            return Err(ProofError::ActionMismatch);
        }
        if action.is_retired() {
            return Err(ProofError::RetiredAction);
        }
        // 6. Stufe
        if !action.is_admissible_from(self.stage) {
            return Err(ProofError::NotAdmissibleAtStage);
        }
        // 7. Bestätigung „nicht umkehrbar“ (O4)
        if action.requires_irreversible_ack() && !self.irreversible_ack {
            return Err(ProofError::IrreversibleAckMissing);
        }
        // 8. Hold
        if !hold_is_valid(action, self.hold_secs) {
            return Err(ProofError::InvalidHold);
        }
        // 9. Freigebender
        if !approver_is_acceptable(&self.approver) {
            return Err(ProofError::InvalidApprover);
        }
        // 10. cgroup
        if action.cgroup() != &self.target.path {
            return Err(ProofError::CgroupMismatch);
        }
        if !policy.allows_cgroup(&self.target.path) {
            return Err(ProofError::CgroupNotAllowed);
        }
        // 11. Nonce (zuletzt)
        ledger.check_and_record(&self.nonce, self.expires_at)?;
        Ok(VerifiedAuthorization {
            key_id: self.key_id.clone(),
            finding_id: self.finding_id.clone(),
            approver: self.approver.clone(),
            action: action.clone(),
            action_digest: self.action_digest,
            stage: self.stage,
            target: self.target.clone(),
            hold_secs: self.hold_secs,
            irreversible_ack: self.irreversible_ack,
            nonce: self.nonce,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }

    /// Returns the proof format version.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.version
    }

    /// Returns the signing key id.
    #[must_use]
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    /// Returns the signed finding id.
    #[must_use]
    pub fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    /// Returns the signed approver.
    #[must_use]
    pub fn approver(&self) -> &ApprovalActor {
        &self.approver
    }

    /// Returns the signed action binding digest.
    #[must_use]
    pub fn action_digest(&self) -> ContentDigest {
        self.action_digest
    }

    /// Returns the signed escalation stage.
    #[must_use]
    pub fn stage(&self) -> EscalationStage {
        self.stage
    }

    /// Returns the signed target identity (path, kernel id, first seen).
    #[must_use]
    pub fn target(&self) -> &CgroupTarget {
        &self.target
    }

    /// Returns the signed target cgroup path.
    #[must_use]
    pub fn cgroup(&self) -> &CgroupId {
        &self.target.path
    }

    /// Returns the signed hold duration (`None` = until released).
    #[must_use]
    pub fn hold_secs(&self) -> Option<u32> {
        self.hold_secs
    }

    /// Returns whether the operator acknowledged irreversibility.
    #[must_use]
    pub fn irreversible_ack(&self) -> bool {
        self.irreversible_ack
    }

    /// Returns the nonce.
    #[must_use]
    pub fn nonce(&self) -> &[u8; 16] {
        &self.nonce
    }

    /// Returns the issue time.
    #[must_use]
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Returns the expiry time.
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    // Kanonische MAC-Eingabe (siehe Moduldoku, Feldreihenfolge 1–12).
    fn authenticated_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(512);
        buf.extend_from_slice(PROOF_DOMAIN);
        put_field(&mut buf, &self.version.to_be_bytes());
        put_field(&mut buf, self.key_id.as_str().as_bytes());
        put_field(&mut buf, self.finding_id.as_str().as_bytes());
        put_approver(&mut buf, &self.approver);
        put_field(&mut buf, self.action_digest.as_bytes());
        put_field(&mut buf, self.stage.wire_name().as_bytes());
        self.target.put_canonical(&mut buf);
        put_opt_u32(&mut buf, self.hold_secs);
        put_bool(&mut buf, self.irreversible_ack);
        put_field(&mut buf, &self.nonce);
        put_timestamp(&mut buf, self.issued_at);
        put_timestamp(&mut buf, self.expires_at);
        buf
    }
}

// Kanonische Form des Freigebenden als ein zusammengesetztes Feld (Tag wie serde, snake_case).
fn put_approver(buf: &mut Vec<u8>, approver: &ApprovalActor) {
    put_nested(buf, |inner| match approver {
        ApprovalActor::Operator { id } => {
            put_field(inner, b"operator");
            put_field(inner, id.as_bytes());
        }
        ApprovalActor::ChannelPeer { channel, peer } => {
            put_field(inner, b"channel_peer");
            put_field(inner, channel.as_str().as_bytes());
            put_field(inner, peer.as_str().as_bytes());
        }
    });
}

// Ein Hold ist nur bei `FreezeCgroup` erlaubt und nie `Some(0)`; `None` passt immer.
fn hold_is_valid(action: &WardenAction, hold_secs: Option<u32>) -> bool {
    match hold_secs {
        None => true,
        Some(0) => false,
        Some(_) => matches!(action, WardenAction::FreezeCgroup { .. }),
    }
}

// Nur ein lokaler Operator mit nicht leerer, begrenzter Kennung ohne Steuerzeichen.
fn approver_is_acceptable(approver: &ApprovalActor) -> bool {
    match approver {
        ApprovalActor::Operator { id } => {
            !id.trim().is_empty()
                && id.len() <= APPROVER_ID_MAX_LEN
                && !id.chars().any(char::is_control)
        }
        ApprovalActor::ChannelPeer { .. } => false,
    }
}

/// Ergebnis einer erfolgreichen [`SignedAuthorization::verify`].
///
/// # Description
/// Kein öffentlicher Konstruktor, kein `Deserialize`: wer diesen Wert hält,
/// hat alle elf Prüfschritte bestanden. Der Warden bindet seine
/// Executor-Aufrufe an diesen Typ ([`Self::action`]) und prüft unmittelbar
/// vor dem Eingriff das Ziel erneut ([`Self::confirm_target`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAuthorization {
    key_id: KeyId,
    finding_id: FindingId,
    approver: ApprovalActor,
    action: WardenAction,
    action_digest: ContentDigest,
    stage: EscalationStage,
    target: CgroupTarget,
    hold_secs: Option<u32>,
    irreversible_ack: bool,
    nonce: [u8; 16],
    issued_at: Timestamp,
    expires_at: Timestamp,
}

impl VerifiedAuthorization {
    /// Returns the key id that verified.
    #[must_use]
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    /// Returns the verified finding id.
    #[must_use]
    pub fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    /// Returns the verified approver (always a local operator).
    #[must_use]
    pub fn approver(&self) -> &ApprovalActor {
        &self.approver
    }

    /// Returns the verified action (the only action the warden may execute).
    #[must_use]
    pub fn action(&self) -> &WardenAction {
        &self.action
    }

    /// Returns the verified action binding digest.
    #[must_use]
    pub fn action_digest(&self) -> ContentDigest {
        self.action_digest
    }

    /// Returns the verified stage.
    #[must_use]
    pub fn stage(&self) -> EscalationStage {
        self.stage
    }

    /// Returns the verified target identity.
    #[must_use]
    pub fn target(&self) -> &CgroupTarget {
        &self.target
    }

    /// Returns the verified target cgroup path.
    #[must_use]
    pub fn cgroup(&self) -> &CgroupId {
        &self.target.path
    }

    /// Returns the verified hold duration (`None` = until released; only freezes carry one).
    #[must_use]
    pub fn hold_secs(&self) -> Option<u32> {
        self.hold_secs
    }

    /// Returns whether the operator acknowledged irreversibility.
    #[must_use]
    pub fn irreversible_ack(&self) -> bool {
        self.irreversible_ack
    }

    /// Returns the consumed nonce (for audit).
    #[must_use]
    pub fn nonce(&self) -> &[u8; 16] {
        &self.nonce
    }

    /// Returns the issue time.
    #[must_use]
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Returns the signed expiry time.
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Confirms that the cgroup the warden observes right now is the authorized target.
    ///
    /// # Description
    /// Kernel-IDs werden wiederverwendet (B21): der Warden beobachtet das Ziel
    /// unmittelbar vor dem Eingriff erneut und übergibt das beobachtete
    /// Tripel. Pfad, Kernel-ID und `first_seen` müssen exakt übereinstimmen.
    ///
    /// # Arguments
    /// - `observed` (`&CgroupTarget`): das vom Warden beobachtete Ziel.
    ///
    /// # Errors
    /// - [`ProofError::TargetChanged`]: mindestens ein Teil weicht ab.
    pub fn confirm_target(&self, observed: &CgroupTarget) -> Result<(), ProofError> {
        if self.target == *observed {
            Ok(())
        } else {
            Err(ProofError::TargetChanged)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuthorizationBody, KeyId, KeyRing, MemoryNonceLedger, NonceLedger, ProofKey, ProofPolicy,
        SignedAuthorization, WARDEN_PROTOCOL_VERSION,
    };
    use crate::action::WardenAction;
    use crate::error::ProofError;
    use crate::stage::EscalationStage;
    use crate::target::{CgroupKernelId, CgroupTarget};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{ApprovalActor, CgroupId, ChannelId, FindingId, PeerId};
    use jiff::Timestamp;
    use serde_json::{Map, Value};

    const KEY_A: [u8; 32] = [0xa1; 32];
    const KEY_B: [u8; 32] = [0xb2; 32];
    const NONCE: [u8; 16] = [0x42; 16];
    const KERNEL_ID: u64 = 4242;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn t0() -> TestResult<Timestamp> {
        Timestamp::new(1_800_000_000, 123_456_789).map_err(ctx("valid timestamp"))
    }

    fn at(offset_secs: i64) -> TestResult<Timestamp> {
        let second = 1_800_000_000_i64
            .checked_add(offset_secs)
            .ok_or(TestError::Missing("offset in range"))?;
        Timestamp::new(second, 123_456_789).map_err(ctx("valid timestamp"))
    }

    fn key_id(id: &str) -> TestResult<KeyId> {
        KeyId::new(id).map_err(ctx("valid key id"))
    }

    fn ring_with(entries: &[(&str, [u8; 32])]) -> TestResult<KeyRing> {
        let mut ring = KeyRing::new();
        for (id, bytes) in entries {
            ring.insert(key_id(id)?, ProofKey::from_bytes(*bytes))
                .map_err(ctx("unique id"))?;
        }
        Ok(ring)
    }

    fn policy() -> TestResult<ProofPolicy> {
        ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()]).map_err(ctx("valid policy"))
    }

    fn freeze(id: &str) -> TestResult<WardenAction> {
        Ok(WardenAction::FreezeCgroup {
            cgroup: cgroup(id)?,
        })
    }

    fn operator(id: &str) -> ApprovalActor {
        ApprovalActor::Operator { id: id.to_owned() }
    }

    fn target_for(action: &WardenAction) -> TestResult<CgroupTarget> {
        Ok(CgroupTarget::new(
            action.cgroup().clone(),
            CgroupKernelId::new(KERNEL_ID).map_err(ctx("kernel id"))?,
            at(-3600)?,
        ))
    }

    // Vollständiger, gültiger Inhalt; `irreversible_ack` folgt der Aktion (O4).
    fn body_for(action: &WardenAction, stage: EscalationStage) -> TestResult<AuthorizationBody> {
        Ok(AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
            approver: operator("operator-1"),
            action: action.clone(),
            stage,
            target: target_for(action)?,
            hold_secs: None,
            irreversible_ack: action.requires_irreversible_ack(),
            nonce: NONCE,
            issued_at: t0()?,
            expires_at: at(60)?,
        })
    }

    fn sign_body(
        key: [u8; 32],
        kid: &str,
        body: AuthorizationBody,
    ) -> TestResult<SignedAuthorization> {
        Ok(SignedAuthorization::sign(
            &ProofKey::from_bytes(key),
            &key_id(kid)?,
            body,
        ))
    }

    fn sign_with(
        key: [u8; 32],
        kid: &str,
        action: &WardenAction,
        stage: EscalationStage,
        nonce: [u8; 16],
        ttl: u32,
    ) -> TestResult<SignedAuthorization> {
        let mut body = body_for(action, stage)?;
        body.nonce = nonce;
        body.expires_at = at(i64::from(ttl))?;
        sign_body(key, kid, body)
    }

    fn verify_fresh(
        auth: &SignedAuthorization,
        ring: &KeyRing,
        action: &WardenAction,
        now: Timestamp,
    ) -> TestResult<Result<super::VerifiedAuthorization, ProofError>> {
        Ok(auth.verify(ring, action, now, &policy()?, &MemoryNonceLedger::new()))
    }

    // Serialisiert, ändert das JSON-Objekt und dekodiert wieder (Form muss gültig bleiben).
    fn tamper(
        auth: &SignedAuthorization,
        edit: impl FnOnce(&mut Map<String, Value>) -> TestResult,
    ) -> TestResult<SignedAuthorization> {
        let mut value = serde_json::to_value(auth).map_err(ctx("to_value"))?;
        edit(
            value
                .as_object_mut()
                .ok_or(TestError::Missing("json object"))?,
        )?;
        serde_json::from_value(value).map_err(ctx("tampered shape stays valid"))
    }

    fn expect_err<T>(result: Result<T, ProofError>, what: &str) -> TestResult<ProofError> {
        match result {
            Ok(_) => Err(TestError::Unexpected(format!(
                "{what} must fail verification"
            ))),
            Err(err) => Ok(err),
        }
    }

    // Ledger, der nur Aufrufe zählt — beweist, ob er erreicht wurde.
    struct CountingLedger(std::cell::Cell<u32>);

    impl NonceLedger for CountingLedger {
        fn check_and_record(&self, _: &[u8; 16], _: Timestamp) -> Result<(), ProofError> {
            self.0.set(self.0.get().saturating_add(1));
            Ok(())
        }
    }

    // -- Roundtrip ---------------------------------------------------------

    #[test]
    fn test_verify_roundtrip_succeeds_and_reports_bound_values() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let mut body = body_for(&action, EscalationStage::RuleTriggered)?;
        body.hold_secs = Some(900);
        let auth = sign_body(KEY_A, "k1", body)?;
        let ledger = MemoryNonceLedger::new();
        let verified = auth
            .verify(
                &ring_with(&[("k1", KEY_A)])?,
                &action,
                at(10)?,
                &policy()?,
                &ledger,
            )
            .map_err(ctx("verifies"))?;
        assert_eq!(verified.key_id().as_str(), "k1");
        assert_eq!(verified.finding_id().as_str(), "finding-1");
        assert_eq!(verified.approver(), &operator("operator-1"));
        assert_eq!(verified.action(), &action);
        assert_eq!(verified.stage(), EscalationStage::RuleTriggered);
        assert_eq!(verified.cgroup(), action.cgroup());
        assert_eq!(verified.target(), &target_for(&action)?);
        assert_eq!(verified.hold_secs(), Some(900));
        assert!(!verified.irreversible_ack());
        assert_eq!(verified.action_digest(), action.binding_digest());
        assert_eq!(verified.nonce(), &NONCE);
        assert_eq!(verified.issued_at(), t0()?);
        assert_eq!(verified.expires_at(), at(60)?);
        assert_eq!(ledger.recorded_count().map_err(ctx("recorded_count"))?, 1);
        Ok(())
    }

    #[test]
    fn test_sign_serde_roundtrip_preserves_mac_validity() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 30)?;
        let json = serde_json::to_string(&auth).map_err(ctx("serializes"))?;
        let back: SignedAuthorization = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(back, auth);
        assert_eq!(back.version(), WARDEN_PROTOCOL_VERSION);
        assert!(verify_fresh(&back, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?.is_ok());
        Ok(())
    }

    #[test]
    fn test_deserialize_rejects_unknown_field_and_bad_hex() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 30)?;
        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("extra".to_owned(), Value::Bool(true));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());

        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("mac".to_owned(), Value::String("AB".repeat(32)));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());

        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value
            .get_mut("target")
            .and_then(Value::as_object_mut)
            .ok_or(TestError::Missing("target object"))?
            .insert("kernel_id".to_owned(), Value::from(0_u64));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());
        Ok(())
    }

    // -- MAC deckt jedes Feld ab ---------------------------------------------

    #[test]
    fn test_authenticated_bytes_change_with_every_field_except_mac() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let base = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let reference = base.authenticated_bytes();

        let mut variants: Vec<(&str, SignedAuthorization)> = Vec::new();
        let mut v = base.clone();
        v.version = 3;
        variants.push(("version", v));
        let mut v = base.clone();
        v.key_id = key_id("k2")?;
        variants.push(("key_id", v));
        let mut v = base.clone();
        v.finding_id = FindingId::try_from_str("finding-2").map_err(ctx("finding id"))?;
        variants.push(("finding_id", v));
        let mut v = base.clone();
        v.approver = operator("operator-2");
        variants.push(("approver", v));
        let mut v = base.clone();
        v.action_digest = freeze("harw.slice/job-2")?.binding_digest();
        variants.push(("action_digest", v));
        let mut v = base.clone();
        v.stage = EscalationStage::Escalated;
        variants.push(("stage", v));
        let mut v = base.clone();
        v.target.path = cgroup("harw.slice/job-2")?;
        variants.push(("target.path", v));
        let mut v = base.clone();
        v.target.kernel_id =
            CgroupKernelId::new(KERNEL_ID.saturating_add(1)).map_err(ctx("kid"))?;
        variants.push(("target.kernel_id", v));
        let mut v = base.clone();
        v.target.first_seen = at(-3599)?;
        variants.push(("target.first_seen", v));
        let mut v = base.clone();
        v.hold_secs = Some(1);
        variants.push(("hold_secs", v));
        let mut v = base.clone();
        v.irreversible_ack = true;
        variants.push(("irreversible_ack", v));
        let mut v = base.clone();
        v.nonce = [0x43; 16];
        variants.push(("nonce", v));
        let mut v = base.clone();
        v.issued_at = at(1)?;
        variants.push(("issued_at", v));
        let mut v = base.clone();
        v.expires_at = at(61)?;
        variants.push(("expires_at", v));

        for (field, variant) in &variants {
            assert_ne!(variant.authenticated_bytes(), reference, "field {field}");
        }

        let mut mac_only = base.clone();
        mac_only.mac = [0x11; 32];
        assert_eq!(mac_only.authenticated_bytes(), reference);
        Ok(())
    }

    #[test]
    fn test_verify_wire_tampering_of_every_field_returns_bad_mac() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        // `k2` trägt denselben Schlüssel: ein umgeschriebenes `key_id` scheitert
        // also nur, weil die Key-ID selbst im MAC steht.
        let ring = ring_with(&[("k1", KEY_A), ("k2", KEY_A)])?;
        assert!(verify_fresh(&auth, &ring, &action, t0()?)?.is_ok());

        let other_digest = serde_json::to_value(freeze("harw.slice/job-2")?.binding_digest())
            .map_err(ctx("digest to_value"))?;
        let top_level: Vec<(&str, Value)> = vec![
            ("key_id", Value::from("k2")),
            ("finding_id", Value::from("finding-2")),
            (
                "approver",
                serde_json::to_value(operator("operator-2")).map_err(ctx("approver"))?,
            ),
            ("action_digest", other_digest),
            ("stage", Value::from("escalated")),
            ("hold_secs", Value::from(600_u32)),
            ("irreversible_ack", Value::Bool(true)),
            ("nonce", Value::from("43".repeat(16))),
            (
                "issued_at",
                serde_json::to_value(at(1)?).map_err(ctx("issued_at"))?,
            ),
            (
                "expires_at",
                serde_json::to_value(at(59)?).map_err(ctx("expires_at"))?,
            ),
        ];
        for (field, replacement) in top_level {
            let forged = tamper(&auth, |object| {
                object.insert(field.to_owned(), replacement);
                Ok(())
            })?;
            let err = expect_err(verify_fresh(&forged, &ring, &action, t0()?)?, field)?;
            assert!(matches!(err, ProofError::BadMac), "field {field}: {err}");
        }

        let target_level: Vec<(&str, Value)> = vec![
            ("path", Value::from("harw.slice/job-2")),
            ("kernel_id", Value::from(KERNEL_ID.saturating_add(1))),
            (
                "first_seen",
                serde_json::to_value(at(-3599)?).map_err(ctx("first_seen"))?,
            ),
        ];
        for (field, replacement) in target_level {
            let forged = tamper(&auth, |object| {
                object
                    .get_mut("target")
                    .and_then(Value::as_object_mut)
                    .ok_or(TestError::Missing("target object"))?
                    .insert(field.to_owned(), replacement);
                Ok(())
            })?;
            let err = expect_err(verify_fresh(&forged, &ring, &action, t0()?)?, field)?;
            assert!(matches!(err, ProofError::BadMac), "target.{field}: {err}");
        }
        Ok(())
    }

    // -- falscher Key / Key-ID ----------------------------------------------

    #[test]
    fn test_verify_wrong_key_returns_bad_mac() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_B,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?,
            "wrong key",
        )?;
        assert!(matches!(err, ProofError::BadMac));
        Ok(())
    }

    #[test]
    fn test_verify_unknown_key_id_returns_unknown_key_id() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k9",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?,
            "unknown key id",
        )?;
        assert!(matches!(err, ProofError::UnknownKeyId));
        Ok(())
    }

    // -- Manipulation --------------------------------------------------------

    #[test]
    fn test_verify_tampered_action_returns_action_mismatch() -> TestResult {
        let signed_for = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &signed_for,
            EscalationStage::Escalated,
            NONCE,
            60,
        )?;
        let requested = WardenAction::KillProcessTree {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let ledger = MemoryNonceLedger::new();
        let err = expect_err(
            auth.verify(
                &ring_with(&[("k1", KEY_A)])?,
                &requested,
                t0()?,
                &policy()?,
                &ledger,
            ),
            "tampered action",
        )?;
        assert!(matches!(err, ProofError::ActionMismatch));
        assert_eq!(
            ledger.recorded_count().map_err(ctx("recorded_count"))?,
            0,
            "rejected request must not consume the nonce"
        );
        Ok(())
    }

    #[test]
    fn test_verify_signed_stage_too_low_returns_not_admissible() -> TestResult {
        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?,
            "stage too low",
        )?;
        assert!(matches!(err, ProofError::NotAdmissibleAtStage));
        Ok(())
    }

    #[test]
    fn test_verify_signed_cgroup_differs_from_action_returns_cgroup_mismatch() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let mut body = body_for(&action, EscalationStage::RuleTriggered)?;
        body.target.path = cgroup("harw.slice/job-2")?;
        let auth = sign_body(KEY_A, "k1", body)?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?,
            "cgroup mismatch",
        )?;
        assert!(matches!(err, ProofError::CgroupMismatch));
        Ok(())
    }

    #[test]
    fn test_verify_cgroup_outside_prefix_returns_cgroup_not_allowed() -> TestResult {
        let ring = ring_with(&[("k1", KEY_A)])?;
        for target in [
            "user.slice",
            "harw.slice",
            "harw.slice2/job",
            "harw.slice/../user.slice",
        ] {
            let action = freeze(target)?;
            let auth = sign_with(
                KEY_A,
                "k1",
                &action,
                EscalationStage::RuleTriggered,
                NONCE,
                60,
            )?;
            let err = expect_err(verify_fresh(&auth, &ring, &action, t0()?)?, target)?;
            assert!(
                matches!(err, ProofError::CgroupNotAllowed),
                "target {target}"
            );
        }
        Ok(())
    }

    // -- Aktionsmenge, Stufen, irreversible_ack (O4) ----------------------------

    #[test]
    fn test_verify_thaw_is_admissible_from_rule_triggered() -> TestResult {
        let action = WardenAction::ThawCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let verified = verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?
            .map_err(ctx("thaw verifies"))?;
        assert_eq!(verified.action(), &action);
        Ok(())
    }

    #[test]
    fn test_verify_lift_isolation_only_from_escalated() -> TestResult {
        let ring = ring_with(&[("k1", KEY_A)])?;
        let action = WardenAction::LiftIsolation {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let low = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let err = expect_err(
            verify_fresh(&low, &ring, &action, t0()?)?,
            "lift at rule-triggered",
        )?;
        assert!(matches!(err, ProofError::NotAdmissibleAtStage));

        let high = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 60)?;
        assert!(
            !high.irreversible_ack(),
            "lifting needs no irreversible ack"
        );
        assert!(verify_fresh(&high, &ring, &action, t0()?)?.is_ok());
        Ok(())
    }

    #[test]
    fn test_verify_kill_and_isolate_without_irreversible_ack_are_refused() -> TestResult {
        let ring = ring_with(&[("k1", KEY_A)])?;
        for action in [
            WardenAction::KillProcessTree {
                cgroup: cgroup("harw.slice/job-1")?,
            },
            WardenAction::IsolateNetwork {
                cgroup: cgroup("harw.slice/job-1")?,
            },
        ] {
            let mut body = body_for(&action, EscalationStage::Escalated)?;
            body.irreversible_ack = false;
            let auth = sign_body(KEY_A, "k1", body)?;
            let ledger = CountingLedger(std::cell::Cell::new(0));
            let err = expect_err(
                auth.verify(&ring, &action, t0()?, &policy()?, &ledger),
                action.kind_name(),
            )?;
            assert!(
                matches!(err, ProofError::IrreversibleAckMissing),
                "{}",
                action.kind_name()
            );
            assert_eq!(ledger.0.get(), 0, "refusal must not consume the nonce");

            let mut acked = body_for(&action, EscalationStage::Escalated)?;
            acked.irreversible_ack = true;
            let auth = sign_body(KEY_A, "k1", acked)?;
            assert!(verify_fresh(&auth, &ring, &action, t0()?)?.is_ok());
        }
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_verify_retired_release_cgroup_returns_retired_action() -> TestResult {
        let action = WardenAction::ReleaseCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let auth = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 60)?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?,
            "retired release",
        )?;
        assert!(matches!(err, ProofError::RetiredAction));
        Ok(())
    }

    // -- Hold (O3) -------------------------------------------------------------

    #[test]
    fn test_verify_hold_only_on_freeze_and_never_zero() -> TestResult {
        let ring = ring_with(&[("k1", KEY_A)])?;
        let freeze_action = freeze("harw.slice/job-1")?;
        for (hold, ok) in [
            (None, true),
            (Some(1), true),
            (Some(86_400), true),
            (Some(0), false),
        ] {
            let mut body = body_for(&freeze_action, EscalationStage::RuleTriggered)?;
            body.hold_secs = hold;
            let auth = sign_body(KEY_A, "k1", body)?;
            let result = verify_fresh(&auth, &ring, &freeze_action, t0()?)?;
            if ok {
                assert!(result.is_ok(), "hold {hold:?}");
            } else {
                assert!(
                    matches!(result, Err(ProofError::InvalidHold)),
                    "hold {hold:?}"
                );
            }
        }
        for action in [
            WardenAction::ThawCgroup {
                cgroup: cgroup("harw.slice/job-1")?,
            },
            WardenAction::IsolateNetwork {
                cgroup: cgroup("harw.slice/job-1")?,
            },
            WardenAction::LiftIsolation {
                cgroup: cgroup("harw.slice/job-1")?,
            },
            WardenAction::KillProcessTree {
                cgroup: cgroup("harw.slice/job-1")?,
            },
        ] {
            let mut body = body_for(&action, EscalationStage::Escalated)?;
            body.hold_secs = Some(60);
            let auth = sign_body(KEY_A, "k1", body)?;
            let result = verify_fresh(&auth, &ring, &action, t0()?)?;
            assert!(
                matches!(result, Err(ProofError::InvalidHold)),
                "{}",
                action.kind_name()
            );
        }
        Ok(())
    }

    // -- Freigebender (I1) -------------------------------------------------------

    #[test]
    fn test_verify_rejects_channel_peer_and_malformed_operator() -> TestResult {
        let ring = ring_with(&[("k1", KEY_A)])?;
        let action = freeze("harw.slice/job-1")?;
        let peer = ApprovalActor::ChannelPeer {
            channel: ChannelId::try_from_str("telegram").map_err(ctx("channel id"))?,
            peer: PeerId::try_from_str("peer-1").map_err(ctx("peer id"))?,
        };
        for approver in [
            peer,
            operator(""),
            operator("   "),
            operator("op\u{1b}[31m"),
            operator(&"x".repeat(129)),
        ] {
            let mut body = body_for(&action, EscalationStage::RuleTriggered)?;
            body.approver = approver;
            let auth = sign_body(KEY_A, "k1", body)?;
            let result = verify_fresh(&auth, &ring, &action, t0()?)?;
            assert!(matches!(result, Err(ProofError::InvalidApprover)));
        }
        let mut body = body_for(&action, EscalationStage::RuleTriggered)?;
        body.approver = operator(&"x".repeat(128));
        let auth = sign_body(KEY_A, "k1", body)?;
        assert!(verify_fresh(&auth, &ring, &action, t0()?)?.is_ok());
        Ok(())
    }

    // -- Zielprüfung vor dem Eingriff (B21) ------------------------------------

    #[test]
    fn test_confirm_target_requires_path_kernel_id_and_first_seen() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let verified = verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, t0()?)?
            .map_err(ctx("verifies"))?;
        let observed = target_for(&action)?;
        assert!(verified.confirm_target(&observed).is_ok());

        let mut reused_id = observed.clone();
        reused_id.first_seen = at(-10)?;
        let mut other_id = observed.clone();
        other_id.kernel_id =
            CgroupKernelId::new(KERNEL_ID.saturating_add(7)).map_err(ctx("kid"))?;
        let mut moved = observed.clone();
        moved.path = cgroup("harw.slice/job-9")?;
        for changed in [reused_id, other_id, moved] {
            assert!(matches!(
                verified.confirm_target(&changed),
                Err(ProofError::TargetChanged)
            ));
        }
        Ok(())
    }

    // -- Zeitfenster ---------------------------------------------------------

    #[test]
    fn test_verify_expired_returns_expired() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        let err = expect_err(
            verify_fresh(&auth, &ring, &action, at(60)?)?,
            "expired proof",
        )?;
        assert!(matches!(err, ProofError::Expired));
        assert!(verify_fresh(&auth, &ring, &action, at(59)?)?.is_ok());
        Ok(())
    }

    #[test]
    fn test_verify_issued_in_future_beyond_skew_returns_not_yet_valid() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        let err = expect_err(
            verify_fresh(&auth, &ring, &action, at(-6)?)?,
            "future-issued proof",
        )?;
        assert!(matches!(err, ProofError::NotYetValid));
        assert!(verify_fresh(&auth, &ring, &action, at(-5)?)?.is_ok());
        Ok(())
    }

    #[test]
    fn test_verify_ttl_above_max_or_zero_returns_ttl_out_of_policy() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        for ttl in [0, 121, u32::MAX] {
            let auth = sign_with(
                KEY_A,
                "k1",
                &action,
                EscalationStage::RuleTriggered,
                NONCE,
                ttl,
            )?;
            let err = expect_err(
                verify_fresh(&auth, &ring, &action, t0()?)?,
                "ttl out of policy",
            )?;
            assert!(matches!(err, ProofError::TtlOutOfPolicy), "ttl {ttl}");
        }
        let tight =
            ProofPolicy::new(30, 0, vec!["harw.slice".to_owned()]).map_err(ctx("valid policy"))?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            31,
        )?;
        let err = expect_err(
            auth.verify(&ring, &action, t0()?, &tight, &MemoryNonceLedger::new()),
            "ttl above tight policy",
        )?;
        assert!(matches!(err, ProofError::TtlOutOfPolicy));
        Ok(())
    }

    #[test]
    fn test_verify_expiry_before_issue_returns_ttl_out_of_policy() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let mut body = body_for(&action, EscalationStage::RuleTriggered)?;
        body.expires_at = at(-1)?;
        let auth = sign_body(KEY_A, "k1", body)?;
        let err = expect_err(
            verify_fresh(&auth, &ring_with(&[("k1", KEY_A)])?, &action, at(-2)?)?,
            "expiry before issue",
        )?;
        assert!(matches!(err, ProofError::TtlOutOfPolicy));
        Ok(())
    }

    #[test]
    fn test_verify_mutated_policy_fields_are_capped() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        let mut loose = policy()?;
        loose.max_ttl_secs = 10_000;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            600,
        )?;
        let err = expect_err(
            auth.verify(&ring, &action, t0()?, &loose, &MemoryNonceLedger::new()),
            "mutated policy",
        )?;
        assert!(matches!(err, ProofError::TtlOutOfPolicy));
        Ok(())
    }

    // -- Replay ---------------------------------------------------------------

    #[test]
    fn test_verify_same_nonce_twice_returns_nonce_replayed() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        let ledger = MemoryNonceLedger::new();
        assert!(
            auth.verify(&ring, &action, t0()?, &policy()?, &ledger)
                .is_ok()
        );
        let err = expect_err(
            auth.verify(&ring, &action, at(1)?, &policy()?, &ledger),
            "replayed nonce",
        )?;
        assert!(matches!(err, ProofError::NonceReplayed));
        Ok(())
    }

    #[test]
    fn test_verify_forged_request_does_not_reach_ledger() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let forged = sign_with(
            KEY_B,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let ledger = CountingLedger(std::cell::Cell::new(0));
        assert!(
            forged
                .verify(
                    &ring_with(&[("k1", KEY_A)])?,
                    &action,
                    t0()?,
                    &policy()?,
                    &ledger
                )
                .is_err()
        );
        assert_eq!(ledger.0.get(), 0);
        Ok(())
    }

    // -- Version --------------------------------------------------------------

    #[test]
    fn test_verify_unknown_version_returns_unsupported_version() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let ring = ring_with(&[("k1", KEY_A)])?;
        for version in [1u16, 3] {
            let other = tamper(&auth, |object| {
                object.insert("version".to_owned(), Value::from(version));
                Ok(())
            })?;
            let err = expect_err(
                verify_fresh(&other, &ring, &action, t0()?)?,
                "other version",
            )?;
            assert!(matches!(err, ProofError::UnsupportedVersion(v) if v == version));
        }
        Ok(())
    }

    // -- Key-Rotation -----------------------------------------------------------

    #[test]
    fn test_verify_key_rotation_accepts_old_and_new_until_removed() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let mut ring = ring_with(&[("old", KEY_A), ("new", KEY_B)])?;
        let by_old = sign_with(
            KEY_A,
            "old",
            &action,
            EscalationStage::RuleTriggered,
            [1; 16],
            60,
        )?;
        let by_new = sign_with(
            KEY_B,
            "new",
            &action,
            EscalationStage::RuleTriggered,
            [2; 16],
            60,
        )?;
        let ledger = MemoryNonceLedger::new();
        assert!(
            by_old
                .verify(&ring, &action, t0()?, &policy()?, &ledger)
                .is_ok()
        );
        assert!(
            by_new
                .verify(&ring, &action, t0()?, &policy()?, &ledger)
                .is_ok()
        );

        assert!(ring.remove(&key_id("old")?));
        let by_old_again = sign_with(
            KEY_A,
            "old",
            &action,
            EscalationStage::RuleTriggered,
            [3; 16],
            60,
        )?;
        let err = expect_err(
            by_old_again.verify(&ring, &action, t0()?, &policy()?, &ledger),
            "removed key",
        )?;
        assert!(matches!(err, ProofError::UnknownKeyId));
        // Ein unter der neuen Key-ID mit dem alten Schlüssel signierter Proof scheitert am MAC.
        let old_key_new_id = sign_with(
            KEY_A,
            "new",
            &action,
            EscalationStage::RuleTriggered,
            [4; 16],
            60,
        )?;
        let err = expect_err(
            old_key_new_id.verify(&ring, &action, t0()?, &policy()?, &ledger),
            "old key under new id",
        )?;
        assert!(matches!(err, ProofError::BadMac));
        Ok(())
    }

    // -- Konstruktoren / Hilfstypen -------------------------------------------

    #[test]
    fn test_key_id_new_enforces_grammar() -> TestResult {
        assert!(KeyId::new("a.B_9-z").is_ok());
        assert!(KeyId::new("").is_err());
        assert!(KeyId::new("x".repeat(65)).is_err());
        assert!(KeyId::new("ü").is_err());
        assert!(serde_json::from_str::<KeyId>("\"has space\"").is_err());
        assert_eq!(key_id("k1")?.to_string(), "k1");
        Ok(())
    }

    #[test]
    fn test_proof_key_debug_is_redacted_and_slice_length_checked() {
        let key = ProofKey::from_bytes([0x5a; 32]);
        let debug = format!("{key:?}");
        assert_eq!(debug, "ProofKey(<redacted>)");
        assert!(!debug.contains("90"));
        assert!(matches!(
            ProofKey::try_from_slice(&[0; 33]),
            Err(ProofError::InvalidKeyMaterial)
        ));
        assert!(ProofKey::try_from_slice(&[0; 32]).is_ok());
    }

    #[test]
    fn test_key_ring_insert_rejects_duplicate_and_debug_lists_only_ids() -> TestResult {
        let mut ring = KeyRing::new();
        assert!(ring.is_empty());
        ring.insert(key_id("k1")?, ProofKey::from_bytes(KEY_A))
            .map_err(ctx("unique id"))?;
        assert!(matches!(
            ring.insert(key_id("k1")?, ProofKey::from_bytes(KEY_B)),
            Err(ProofError::DuplicateKeyId)
        ));
        assert!(ring.contains(&key_id("k1")?));
        assert_eq!(ring.len(), 1);
        assert_eq!(format!("{ring:?}"), "KeyRing { key_ids: [\"k1\"] }");
        assert!(!ring.remove(&key_id("k2")?));
        Ok(())
    }

    #[test]
    fn test_key_ring_key_ids_are_sorted_and_follow_rotation() -> TestResult {
        let mut ring = ring_with(&[("k-new", KEY_B), ("k-old", KEY_A)])?;
        let ids: Vec<&str> = ring.key_ids().into_iter().map(KeyId::as_str).collect();
        assert_eq!(ids, ["k-new", "k-old"]);
        assert!(ring.remove(&key_id("k-old")?));
        let ids: Vec<&str> = ring.key_ids().into_iter().map(KeyId::as_str).collect();
        assert_eq!(ids, ["k-new"]);
        Ok(())
    }

    #[test]
    fn test_proof_policy_new_validates_bounds_and_prefixes() {
        assert!(ProofPolicy::new(0, 5, vec![]).is_err());
        assert!(ProofPolicy::new(121, 5, vec![]).is_err());
        assert!(ProofPolicy::new(120, 31, vec![]).is_err());
        assert!(ProofPolicy::new(60, 5, vec!["/abs".to_owned()]).is_err());
        assert!(ProofPolicy::new(60, 5, vec!["a/../b".to_owned()]).is_err());
        assert!(ProofPolicy::new(60, 5, vec!["harw.slice/jobs".to_owned()]).is_ok());
    }

    #[test]
    fn test_proof_policy_allows_cgroup_compares_components() -> TestResult {
        let policy = ProofPolicy::new(60, 5, vec!["harw.slice/jobs".to_owned()])
            .map_err(ctx("valid policy"))?;
        assert!(policy.allows_cgroup(&cgroup("harw.slice/jobs/j1")?));
        assert!(policy.allows_cgroup(&cgroup("harw.slice/jobs/j1/sub")?));
        assert!(!policy.allows_cgroup(&cgroup("harw.slice/jobs")?));
        assert!(!policy.allows_cgroup(&cgroup("harw.slice/jobsX/j1")?));
        assert!(!policy.allows_cgroup(&cgroup("harw.slice/jobs//j1")?));
        assert!(!policy.allows_cgroup(&cgroup("harw.slice/jobs/./j1")?));
        let empty = ProofPolicy::new(60, 5, vec![]).map_err(ctx("valid policy"))?;
        assert!(!empty.allows_cgroup(&cgroup("harw.slice/jobs/j1")?));
        Ok(())
    }

    #[test]
    fn test_memory_nonce_ledger_prune_expired_removes_only_old_entries() -> TestResult {
        let ledger = MemoryNonceLedger::new();
        ledger
            .check_and_record(&[1; 16], at(10)?)
            .map_err(ctx("check_and_record"))?;
        ledger
            .check_and_record(&[2; 16], at(100)?)
            .map_err(ctx("check_and_record"))?;
        assert_eq!(
            ledger
                .prune_expired(at(50)?)
                .map_err(ctx("prune_expired"))?,
            1
        );
        assert_eq!(ledger.recorded_count().map_err(ctx("recorded_count"))?, 1);
        assert!(ledger.check_and_record(&[1; 16], at(200)?).is_ok());
        assert!(matches!(
            ledger.check_and_record(&[2; 16], at(200)?),
            Err(ProofError::NonceReplayed)
        ));
        Ok(())
    }

    #[test]
    fn test_accessors_return_signed_values() -> TestResult {
        let action = WardenAction::IsolateNetwork {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let auth = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 45)?;
        assert_eq!(auth.key_id().as_str(), "k1");
        assert_eq!(auth.finding_id().as_str(), "finding-1");
        assert_eq!(auth.approver(), &operator("operator-1"));
        assert_eq!(auth.action_digest(), action.binding_digest());
        assert_eq!(auth.stage(), EscalationStage::Escalated);
        assert_eq!(auth.cgroup(), action.cgroup());
        assert_eq!(auth.target(), &target_for(&action)?);
        assert_eq!(auth.hold_secs(), None);
        assert!(auth.irreversible_ack());
        assert_eq!(auth.nonce(), &NONCE);
        assert_eq!(auth.issued_at(), t0()?);
        assert_eq!(auth.expires_at(), at(45)?);
        Ok(())
    }
}

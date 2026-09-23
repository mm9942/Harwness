//! Warden-Proof v2: signierte Autorisierung (C-WPROTO, W3; Befunde F-001, F-088).
//!
//! # Zweck
//! Ersetzt den fälschbaren v1-Beleg ([`crate::proof::AuthorizationProof`],
//! ohne MAC/Nonce/Ablauf) durch [`SignedAuthorization`]: einen mit
//! **keyed BLAKE3** authentisierten Datensatz, den nur ein Halter eines
//! [`ProofKey`] (der Escalator) ausstellen kann und den der Warden gegen
//! einen [`KeyRing`], eine [`ProofPolicy`] und einen [`NonceLedger`] prüft.
//!
//! # MAC-Eingabe (kanonisch)
//! `PROOF_DOMAIN = "harw:warden-proof:v2\0"`, danach je Feld `u64`-BE-Länge +
//! Rohbytes (siehe `canonical.rs`), in dieser Reihenfolge:
//! `version` (u16 BE) · `key_id` (UTF-8) · `action_digest` (32 B, siehe
//! [`WardenAction::binding_digest`]) · `stage` (Wire-Name) · `cgroup` (UTF-8) ·
//! `nonce` (16 B) · `issued_at` (i64 BE Sekunden ‖ i32 BE Nanosekunden) ·
//! `ttl_secs` (u32 BE). Der MAC selbst ist `blake3::keyed_hash(key, bytes)`.
//!
//! # Prüfreihenfolge von [`SignedAuthorization::verify`]
//! 1. Version (`== WARDEN_PROTOCOL_VERSION`, v1 abgelehnt)
//! 2. Key-ID im [`KeyRing`]
//! 3. MAC — konstante Zeit (`blake3::Hash: PartialEq` nutzt `constant_time_eq`)
//! 4. Zeitfenster — `1 <= ttl <= min(policy.max_ttl_secs, 120)`,
//!    `issued_at <= now + skew`, `now < issued_at + ttl`
//! 5. Action-Digest (`action.binding_digest() == action_digest`)
//! 6. Stufe (`action.is_admissible_from(stage)`)
//! 7. cgroup — signierte cgroup == Aktions-cgroup und strikt unterhalb eines
//!    erlaubten Präfixes (Vergleich nach Pfadkomponenten)
//! 8. Nonce — **zuletzt**, damit gefälschte oder unpassende Anfragen keine
//!    Nonces verbrauchen; der Ledger muss vor `Ok` dauerhaft schreiben.
//!
//! Der Peer-UID-Check (`SO_PEERCRED == harw-escalate`) liegt vor Schritt 1 im
//! Warden-Binary (W5 D-WARDEN), nicht in dieser Crate.
//!
//! # Schlüsselmaterial
//! [`ProofKey`] ist nicht `Clone`, nicht `PartialEq`, `Debug` gibt nur
//! `ProofKey(<redacted>)` aus, und `Drop` überschreibt die Bytes (best effort:
//! ohne `zeroize`-Crate kann der Compiler das Schreiben theoretisch entfernen;
//! `zeroize` ist als dep-request im Ledger `C-WPROTO.md` beantragt).
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
//!     EscalationStage, KeyId, KeyRing, MemoryNonceLedger, ProofKey, ProofPolicy,
//!     SignedAuthorization, WardenAction,
//! };
//! use harw_types::CgroupId;
//!
//! let key_id = KeyId::new("k2026").unwrap();
//! let signing_key = ProofKey::from_bytes([7u8; 32]);
//! let mut ring = KeyRing::new();
//! ring.insert(key_id.clone(), ProofKey::from_bytes([7u8; 32])).unwrap();
//!
//! let cgroup = CgroupId::try_from_str("harw.slice/job-1").unwrap();
//! let action = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
//! let now = jiff::Timestamp::from_second(1_800_000_000).unwrap();
//! let auth = SignedAuthorization::sign(
//!     &signing_key, &key_id, &action, EscalationStage::RuleTriggered, &cgroup,
//!     [1u8; 16], now, 60,
//! );
//! let policy = ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()]).unwrap();
//! let ledger = MemoryNonceLedger::new();
//! assert!(auth.verify(&ring, &action, now, &policy, &ledger).is_ok());
//! // Replay derselben Nonce scheitert.
//! assert!(auth.verify(&ring, &action, now, &policy, &ledger).is_err());
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use harw_types::{CgroupId, ContentDigest};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::action::WardenAction;
use crate::canonical::{PROOF_DOMAIN, put_field};
use crate::error::ProofError;
use crate::stage::EscalationStage;

/// Protokoll- und Proof-Version, die dieser Stand ausstellt und akzeptiert.
pub const WARDEN_PROTOCOL_VERSION: u16 = 2;

/// Harte Obergrenze für jede TTL, unabhängig von der Policy (Plan: TTL ≤ 120 s).
pub const MAX_TTL_SECS_CAP: u32 = 120;

/// Harte Obergrenze für den tolerierten Uhrenversatz (Zukunftsrichtung).
pub const MAX_CLOCK_SKEW_SECS_CAP: u32 = 30;

// Maximale Länge einer Key-ID in Bytes.
const KEY_ID_MAX_LEN: usize = 64;

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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        let mut ids: Vec<&str> = self.keys.keys().map(KeyId::as_str).collect();
        ids.sort_unstable();
        f.debug_struct("KeyRing").field("key_ids", &ids).finish()
    }
}

/// Prüfpolitik des Wardens für [`SignedAuthorization::verify`].
///
/// # Description
/// - `max_ttl_secs`: größte akzeptierte TTL; wirksam ist immer
///   `min(max_ttl_secs, MAX_TTL_SECS_CAP)`.
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
    /// Größte akzeptierte TTL in Sekunden (1..=120).
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
        Ok(before - seen.len())
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
/// `issued_at` RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedAuthorization {
    version: u16,
    key_id: KeyId,
    action_digest: ContentDigest,
    stage: EscalationStage,
    cgroup: CgroupId,
    #[serde(with = "crate::canonical::hex16")]
    nonce: [u8; 16],
    issued_at: Timestamp,
    ttl_secs: u32,
    #[serde(with = "crate::canonical::hex32")]
    mac: [u8; 32],
}

impl SignedAuthorization {
    /// Signs an authorization for `action` with `key` (Escalator side).
    ///
    /// # Description
    /// Setzt `version = WARDEN_PROTOCOL_VERSION`, bindet
    /// [`WardenAction::binding_digest`] und berechnet den keyed-BLAKE3-MAC über
    /// die kanonischen Bytes. Validiert nichts (TTL, Präfix, Stufe prüft
    /// ausschließlich `verify`); `nonce` muss vom Aufrufer aus einer CSPRNG
    /// stammen und darf nie wiederverwendet werden.
    ///
    /// # Arguments
    /// - `key` (`&ProofKey`): geheimer Schlüssel.
    /// - `key_id` (`&KeyId`): Kennung dieses Schlüssels im Warden-Ring.
    /// - `action` (`&WardenAction`): die autorisierte Aktion.
    /// - `stage` (`EscalationStage`): Stufe laut Leiter.
    /// - `cgroup` (`&CgroupId`): Ziel-cgroup (muss der Aktions-cgroup entsprechen).
    /// - `nonce` (`[u8; 16]`): Einmalwert.
    /// - `issued_at` (`Timestamp`): Ausstellungszeit (injiziert).
    /// - `ttl_secs` (`u32`): Gültigkeit in Sekunden (≤ 120 wird akzeptiert).
    ///
    /// # Returns
    /// Die signierte Autorisierung.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn sign(
        key: &ProofKey,
        key_id: &KeyId,
        action: &WardenAction,
        stage: EscalationStage,
        cgroup: &CgroupId,
        nonce: [u8; 16],
        issued_at: Timestamp,
        ttl_secs: u32,
    ) -> Self {
        let mut unsigned = Self {
            version: WARDEN_PROTOCOL_VERSION,
            key_id: key_id.clone(),
            action_digest: action.binding_digest(),
            stage,
            cgroup: cgroup.clone(),
            nonce,
            issued_at,
            ttl_secs,
            mac: [0u8; 32],
        };
        unsigned.mac = *key.mac(&unsigned.authenticated_bytes()).as_bytes();
        unsigned
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
    /// [`ProofError::TtlOutOfPolicy`] / [`ProofError::NotYetValid`] /
    /// [`ProofError::TimestampOutOfRange`] / [`ProofError::Expired`],
    /// [`ProofError::ActionMismatch`], [`ProofError::NotAdmissibleAtStage`],
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
        if self.ttl_secs == 0 || self.ttl_secs > policy.effective_max_ttl() {
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
        let expires_at = self
            .issued_at
            .checked_add(SignedDuration::from_secs(i64::from(self.ttl_secs)))
            .map_err(|_| ProofError::TimestampOutOfRange)?;
        if now >= expires_at {
            return Err(ProofError::Expired);
        }
        // 5. Action-Digest
        if action.binding_digest() != self.action_digest {
            return Err(ProofError::ActionMismatch);
        }
        // 6. Stufe
        if !action.is_admissible_from(self.stage) {
            return Err(ProofError::NotAdmissibleAtStage);
        }
        // 7. cgroup
        if action.cgroup() != &self.cgroup {
            return Err(ProofError::CgroupMismatch);
        }
        if !policy.allows_cgroup(&self.cgroup) {
            return Err(ProofError::CgroupNotAllowed);
        }
        // 8. Nonce (zuletzt)
        ledger.check_and_record(&self.nonce, expires_at)?;
        Ok(VerifiedAuthorization {
            key_id: self.key_id.clone(),
            action_digest: self.action_digest,
            stage: self.stage,
            cgroup: self.cgroup.clone(),
            nonce: self.nonce,
            issued_at: self.issued_at,
            expires_at,
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

    /// Returns the signed target cgroup.
    #[must_use]
    pub fn cgroup(&self) -> &CgroupId {
        &self.cgroup
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

    /// Returns the TTL in seconds.
    #[must_use]
    pub fn ttl_secs(&self) -> u32 {
        self.ttl_secs
    }

    // Kanonische MAC-Eingabe (siehe Moduldoku).
    fn authenticated_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(PROOF_DOMAIN);
        put_field(&mut buf, &self.version.to_be_bytes());
        put_field(&mut buf, self.key_id.as_str().as_bytes());
        put_field(&mut buf, self.action_digest.as_bytes());
        put_field(&mut buf, self.stage.wire_name().as_bytes());
        put_field(&mut buf, self.cgroup.as_str().as_bytes());
        put_field(&mut buf, &self.nonce);
        let mut time = [0u8; 12];
        time[..8].copy_from_slice(&self.issued_at.as_second().to_be_bytes());
        time[8..].copy_from_slice(&self.issued_at.subsec_nanosecond().to_be_bytes());
        put_field(&mut buf, &time);
        put_field(&mut buf, &self.ttl_secs.to_be_bytes());
        buf
    }
}

/// Ergebnis einer erfolgreichen [`SignedAuthorization::verify`].
///
/// # Description
/// Kein öffentlicher Konstruktor, kein `Deserialize`: wer diesen Wert hält,
/// hat alle acht Prüfschritte bestanden. Der Warden (W5 D-WARDEN) soll seine
/// Executor-Aufrufe an diesen Typ binden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAuthorization {
    key_id: KeyId,
    action_digest: ContentDigest,
    stage: EscalationStage,
    cgroup: CgroupId,
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

    /// Returns the verified target cgroup.
    #[must_use]
    pub fn cgroup(&self) -> &CgroupId {
        &self.cgroup
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

    /// Returns `issued_at + ttl`.
    #[must_use]
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KeyId, KeyRing, MemoryNonceLedger, NonceLedger, ProofKey, ProofPolicy, SignedAuthorization,
        WARDEN_PROTOCOL_VERSION,
    };
    use crate::action::WardenAction;
    use crate::error::ProofError;
    use crate::stage::EscalationStage;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::CgroupId;
    use jiff::Timestamp;

    const KEY_A: [u8; 32] = [0xa1; 32];
    const KEY_B: [u8; 32] = [0xb2; 32];
    const NONCE: [u8; 16] = [0x42; 16];

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn t0() -> TestResult<Timestamp> {
        Timestamp::new(1_800_000_000, 123_456_789).map_err(ctx("valid timestamp"))
    }

    fn at(offset_secs: i64) -> TestResult<Timestamp> {
        Timestamp::new(1_800_000_000 + offset_secs, 123_456_789).map_err(ctx("valid timestamp"))
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

    fn sign_with(
        key: [u8; 32],
        kid: &str,
        action: &WardenAction,
        stage: EscalationStage,
        nonce: [u8; 16],
        ttl: u32,
    ) -> TestResult<SignedAuthorization> {
        Ok(SignedAuthorization::sign(
            &ProofKey::from_bytes(key),
            &key_id(kid)?,
            action,
            stage,
            action.cgroup(),
            nonce,
            t0()?,
            ttl,
        ))
    }

    // Ledger, der nur Aufrufe zählt — beweist, ob er erreicht wurde.
    struct CountingLedger(std::cell::Cell<u32>);

    impl NonceLedger for CountingLedger {
        fn check_and_record(&self, _: &[u8; 16], _: Timestamp) -> Result<(), ProofError> {
            self.0.set(self.0.get() + 1);
            Ok(())
        }
    }

    // -- Roundtrip ---------------------------------------------------------

    #[test]
    fn test_verify_roundtrip_succeeds_and_reports_bound_values() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
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
        assert_eq!(verified.stage(), EscalationStage::RuleTriggered);
        assert_eq!(verified.cgroup(), action.cgroup());
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
        assert!(
            back.verify(
                &ring_with(&[("k1", KEY_A)])?,
                &action,
                t0()?,
                &policy()?,
                &MemoryNonceLedger::new()
            )
            .is_ok()
        );
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
            .insert("extra".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());

        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value["mac"] = serde_json::Value::String("AB".repeat(32));
        assert!(serde_json::from_value::<SignedAuthorization>(value).is_err());
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
        let Err(err) = auth.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &action,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "wrong key must fail verification".to_owned(),
            ));
        };
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
        let Err(err) = auth.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &action,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "unknown key id must fail verification".to_owned(),
            ));
        };
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
        let Err(err) = auth.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &requested,
            t0()?,
            &policy()?,
            &ledger,
        ) else {
            return Err(TestError::Unexpected(
                "tampered action must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::ActionMismatch));
        assert_eq!(
            ledger.recorded_count().map_err(ctx("recorded_count"))?,
            0,
            "rejected request must not consume the nonce"
        );
        Ok(())
    }

    #[test]
    fn test_verify_tampered_stage_in_wire_returns_bad_mac() -> TestResult {
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
        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value["stage"] = serde_json::Value::String("escalated".to_owned());
        let forged: SignedAuthorization =
            serde_json::from_value(value).map_err(ctx("shape is valid"))?;
        let Err(err) = forged.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &action,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "tampered stage must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::BadMac));
        Ok(())
    }

    #[test]
    fn test_verify_tampered_cgroup_in_wire_returns_bad_mac() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(
            KEY_A,
            "k1",
            &action,
            EscalationStage::RuleTriggered,
            NONCE,
            60,
        )?;
        let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
        value["cgroup"] = serde_json::Value::String("harw.slice/job-2".to_owned());
        let forged: SignedAuthorization =
            serde_json::from_value(value).map_err(ctx("shape is valid"))?;
        let Err(err) = forged.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &freeze("harw.slice/job-2")?,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "tampered cgroup must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::BadMac));
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
        let Err(err) = auth.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &action,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "stage too low must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::NotAdmissibleAtStage));
        Ok(())
    }

    #[test]
    fn test_verify_signed_cgroup_differs_from_action_returns_cgroup_mismatch() -> TestResult {
        let action = freeze("harw.slice/job-1")?;
        let auth = SignedAuthorization::sign(
            &ProofKey::from_bytes(KEY_A),
            &key_id("k1")?,
            &action,
            EscalationStage::RuleTriggered,
            &cgroup("harw.slice/job-2")?,
            NONCE,
            t0()?,
            60,
        );
        let Err(err) = auth.verify(
            &ring_with(&[("k1", KEY_A)])?,
            &action,
            t0()?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "cgroup mismatch must fail verification".to_owned(),
            ));
        };
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
            let Err(err) =
                auth.verify(&ring, &action, t0()?, &policy()?, &MemoryNonceLedger::new())
            else {
                return Err(TestError::Unexpected(format!(
                    "target {target} must fail verification"
                )));
            };
            assert!(
                matches!(err, ProofError::CgroupNotAllowed),
                "target {target}"
            );
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
        let Err(err) = auth.verify(
            &ring,
            &action,
            at(60)?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "expired proof must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::Expired));
        assert!(
            auth.verify(
                &ring,
                &action,
                at(59)?,
                &policy()?,
                &MemoryNonceLedger::new()
            )
            .is_ok()
        );
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
        let Err(err) = auth.verify(
            &ring,
            &action,
            at(-6)?,
            &policy()?,
            &MemoryNonceLedger::new(),
        ) else {
            return Err(TestError::Unexpected(
                "future-issued proof must fail verification".to_owned(),
            ));
        };
        assert!(matches!(err, ProofError::NotYetValid));
        assert!(
            auth.verify(
                &ring,
                &action,
                at(-5)?,
                &policy()?,
                &MemoryNonceLedger::new()
            )
            .is_ok()
        );
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
            let Err(err) =
                auth.verify(&ring, &action, t0()?, &policy()?, &MemoryNonceLedger::new())
            else {
                return Err(TestError::Unexpected(format!("ttl {ttl} must fail")));
            };
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
        let Err(err) = auth.verify(&ring, &action, t0()?, &tight, &MemoryNonceLedger::new()) else {
            return Err(TestError::Unexpected(
                "ttl above tight policy must fail".to_owned(),
            ));
        };
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
        let Err(err) = auth.verify(&ring, &action, t0()?, &loose, &MemoryNonceLedger::new()) else {
            return Err(TestError::Unexpected(
                "mutated policy must still cap ttl".to_owned(),
            ));
        };
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
        let Err(err) = auth.verify(&ring, &action, at(1)?, &policy()?, &ledger) else {
            return Err(TestError::Unexpected(
                "replayed nonce must fail verification".to_owned(),
            ));
        };
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
            let mut value = serde_json::to_value(&auth).map_err(ctx("to_value"))?;
            value["version"] = serde_json::Value::from(version);
            let other: SignedAuthorization =
                serde_json::from_value(value).map_err(ctx("shape is valid"))?;
            let Err(err) =
                other.verify(&ring, &action, t0()?, &policy()?, &MemoryNonceLedger::new())
            else {
                return Err(TestError::Unexpected(format!(
                    "version {version} must be rejected"
                )));
            };
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
        let Err(err) = by_old_again.verify(&ring, &action, t0()?, &policy()?, &ledger) else {
            return Err(TestError::Unexpected(
                "removed key must fail verification".to_owned(),
            ));
        };
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
        let Err(err) = old_key_new_id.verify(&ring, &action, t0()?, &policy()?, &ledger) else {
            return Err(TestError::Unexpected(
                "old key under new id must fail MAC".to_owned(),
            ));
        };
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
        let action = freeze("harw.slice/job-1")?;
        let auth = sign_with(KEY_A, "k1", &action, EscalationStage::Escalated, NONCE, 45)?;
        assert_eq!(auth.key_id().as_str(), "k1");
        assert_eq!(auth.action_digest(), action.binding_digest());
        assert_eq!(auth.stage(), EscalationStage::Escalated);
        assert_eq!(auth.cgroup(), action.cgroup());
        assert_eq!(auth.nonce(), &NONCE);
        assert_eq!(auth.issued_at(), t0()?);
        assert_eq!(auth.ttl_secs(), 45);
        Ok(())
    }
}

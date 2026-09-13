//! `SecretStore` — the durable, DEK-sealed secret store `harw-config` reads at
//! process start (spec §1a). On disk every value is a CGv2 envelope; in memory,
//! after unsealing, callers get `secrecy::SecretBox<[u8]>`.
//!
//! The in-process create/get/delete path is fully sealed through the CGv2
//! envelope module. Durable record loading and key-provenance resolution stay
//! explicit separate boundaries: a caller must supply verified KEK material,
//! and no guessed key-file format is silently accepted.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};

use crate::audit::chain::{
    canonical_bytes, AuditLog, PersistedChainStatus, AUDIT_FORMAT_VERSION, AUDIT_MAGIC,
};
use crate::audit::checkpoint::CheckpointLog;
use crate::audit::event::{Actor, SubjectRef};
use crate::error::{AuditResult, SecretsError, SecretsResult};
use crate::id::{KeyVersion, SecretId};
use crate::kek::KekProvenance;
use crate::policy::CryptoPolicy;
use crate::record::{SecretMetadata, SecretRecord};

/// Verified KEK material supplied by the configured key-provenance boundary.
/// The public bytes are the canonical serialization of a validated HPKE
/// recipient key. The matching deterministic HPKE seed is retained in a
/// zeroizing container and exposed only to the v3 envelope open call.
pub struct KekMaterial {
    public_key: Vec<u8>,
    hpke_seed: SecretBox<[u8]>,
}

impl KekMaterial {
    /// Construct material from validated recipient public bytes and its exact
    /// 32-byte deterministic HPKE seed.
    ///
    /// This constructor deliberately does not accept a serialized ML-KEM
    /// secret key. Callers must derive and validate the recipient key pair at
    /// the provenance boundary, then retain only the seed necessary to
    /// reproduce the HPKE recipient private key during envelope opening.
    pub fn new(public_key: Vec<u8>, hpke_seed: SecretBox<[u8]>) -> SecretsResult<Self> {
        let actual = hpke_seed.expose_secret().len();
        if actual != 32 {
            return Err(SecretsError::InvalidHpkeSeedLength { actual });
        }

        Ok(Self {
            public_key,
            hpke_seed,
        })
    }
}

/// One in-memory store entry: the sealed record plus its safe-to-log metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSecret {
    record: SecretRecord,
    metadata: SecretMetadata,
}

/// The local, filesystem-backed secret store.
pub struct SecretStore {
    root: PathBuf,
    policy: CryptoPolicy,
    provenance: KekProvenance,
    key_version: KeyVersion,
    key_material: Option<KekMaterial>,
    index: HashMap<SecretId, StoredSecret>,
    audit: AuditLog,
    checkpoints: CheckpointLog,
}

impl SecretStore {
    /// Build an empty store rooted at `root` with the given policy, KEK
    /// provenance, and current key generation. Loading records from disk is a
    /// separate (deferred) step.
    #[must_use]
    pub fn new(
        root: PathBuf,
        policy: CryptoPolicy,
        provenance: KekProvenance,
        key_version: KeyVersion,
    ) -> Self {
        Self {
            root,
            policy,
            provenance,
            key_version,
            key_material: None,
            index: HashMap::new(),
            audit: AuditLog::new(),
            checkpoints: CheckpointLog::new(),
        }
    }

    /// Build an empty store with validated KEK material available for the
    /// in-process sealed create/get/delete flow. The key material deliberately
    /// is not derived from `provenance` here: that boundary must first verify
    /// the provenance-specific file, keyring, or environment representation.
    #[must_use]
    pub fn with_key_material(
        root: PathBuf,
        policy: CryptoPolicy,
        provenance: KekProvenance,
        key_version: KeyVersion,
        key_material: KekMaterial,
    ) -> Self {
        Self {
            root,
            policy,
            provenance,
            key_version,
            key_material: Some(key_material),
            index: HashMap::new(),
            audit: AuditLog::new(),
            checkpoints: CheckpointLog::new(),
        }
    }

    /// Open a store and reconstruct its index from the sealed entries already
    /// persisted under `root`. This constructor intentionally does not resolve
    /// KEK material; callers that need to unseal values must use
    /// [`Self::open_with_key_material`] after verifying that separate boundary.
    pub fn open(
        root: PathBuf,
        policy: CryptoPolicy,
        provenance: KekProvenance,
        key_version: KeyVersion,
    ) -> SecretsResult<Self> {
        let mut store = Self::new(root, policy, provenance, key_version);
        store.index = load_index(&store.root, store.key_version)?;
        Ok(store)
    }

    /// Open a store and reconstruct its index from the sealed entries already
    /// persisted under `root`. Every entry is validated before it is indexed;
    /// malformed, duplicate, or ID-mismatched data refuses startup rather than
    /// silently dropping or replacing a secret.
    pub fn open_with_key_material(
        root: PathBuf,
        policy: CryptoPolicy,
        provenance: KekProvenance,
        key_version: KeyVersion,
        key_material: KekMaterial,
    ) -> SecretsResult<Self> {
        let mut store = Self::open(root, policy, provenance, key_version)?;
        store.key_material = Some(key_material);
        Ok(store)
    }

    /// The directory this store persists under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The policy governing new seals.
    #[must_use]
    pub fn policy(&self) -> CryptoPolicy {
        self.policy
    }

    /// The configured KEK provenance.
    #[must_use]
    pub fn provenance(&self) -> &KekProvenance {
        &self.provenance
    }

    /// The current KEK generation.
    #[must_use]
    pub fn key_version(&self) -> KeyVersion {
        self.key_version
    }

    /// Number of secrets currently indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the store holds no secrets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The in-memory audit log, including every successful store mutation.
    #[must_use]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }

    /// The in-memory checkpoint state. Checkpoint signing is currently not
    /// available from the audit types, so this remains empty until that API is
    /// implemented.
    #[must_use]
    pub fn checkpoint_log(&self) -> &CheckpointLog {
        &self.checkpoints
    }

    /// Load this store's persisted `audit.log` from disk and verify its hash
    /// chain end to end, independent of `self`'s in-memory
    /// [`Self::audit_log`] (which only accumulates mutations made through
    /// this particular handle — see the module docs above for why `open`
    /// does not populate it from disk).
    ///
    /// This is a deliberately separate, explicitly-called path rather than
    /// something `open`/`open_with_key_material` runs automatically: the
    /// persisted chain matters to a periodic external tamper check (e.g. a
    /// gateway re-verifying every 15 minutes), not to the in-process
    /// create/get/delete path. Loading and rehashing the full audit history
    /// on every `open` would tax every secret operation for a check most
    /// callers never need.
    ///
    /// # Errors
    /// See [`crate::audit::chain::load_and_verify_persisted_chain`] for the
    /// full breakdown of "nothing recorded yet" vs. "unreadable" vs.
    /// "manipulated".
    pub fn verify_persisted_audit_chain(&self) -> AuditResult<PersistedChainStatus> {
        crate::audit::chain::load_and_verify_persisted_chain(&self.root.join(AUDIT_FILE))
    }

    /// Whether a secret with `id` is present.
    #[must_use]
    pub fn contains(&self, id: &SecretId) -> bool {
        self.index.contains_key(id)
    }

    /// All secrets' safe-to-log metadata (for `harw secrets list`), sorted by
    /// name and then by raw `SecretId` bytes for a stable deterministic order.
    #[must_use]
    pub fn list(&self) -> Vec<SecretMetadata> {
        let mut metadata: Vec<_> = self.index.values().map(|s| s.metadata.clone()).collect();
        metadata.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
        });
        metadata
    }

    /// Metadata for one secret, or [`SecretsError::NotFound`].
    pub fn metadata(&self, id: &SecretId) -> SecretsResult<SecretMetadata> {
        self.index
            .get(id)
            .map(|s| s.metadata.clone())
            .ok_or(SecretsError::NotFound { id: *id })
    }

    /// The sealed record for one secret, or [`SecretsError::NotFound`].
    pub fn record(&self, id: &SecretId) -> SecretsResult<SecretRecord> {
        self.index
            .get(id)
            .map(|s| s.record.clone())
            .ok_or(SecretsError::NotFound { id: *id })
    }

    /// Seal `secret` under a fresh CGv2 session key and return its new id.
    ///
    /// The sealed entry is durably written before either the index or audit
    /// state advances.
    pub fn create(
        &mut self,
        name: &str,
        purpose: &str,
        secret: &SecretBox<[u8]>,
    ) -> SecretsResult<SecretId> {
        let material = self
            .key_material
            .as_ref()
            .ok_or_else(|| SecretsError::KekUnavailable {
                kind: provenance_kind(&self.provenance).to_owned(),
                reason: "no validated KEK material was supplied".to_owned(),
            })?;
        let id = SecretId::new();
        let sealed = crate::envelope::seal(
            &self.policy,
            id,
            self.key_version,
            &material.public_key,
            secret.expose_secret().as_ref(),
        )?;
        let now = jiff::Timestamp::now();
        let stored = StoredSecret {
            record: SecretRecord {
                id,
                envelope_format: sealed.envelope_format,
                ciphertext: sealed.ciphertext,
                nonce: sealed.nonce,
                wrapped_dek: sealed.wrapped_dek,
                kem_algo: sealed.kem_algo,
                aead_algo: sealed.aead_algo,
                key_version: self.key_version,
            },
            metadata: SecretMetadata {
                id,
                name: name.to_owned(),
                purpose: purpose.to_owned(),
                key_version: self.key_version,
                created_at: now,
                updated_at: now,
            },
        };
        persist_secret(&self.root, &stored)?;
        let mut next_audit = self.audit.clone();
        next_audit.append(
            Actor::System,
            "secret.create",
            vec![SubjectRef::new("secret", &format_secret_id(&id))],
        );
        if let Err(error) = persist_audit_state(&self.root, &next_audit, &self.checkpoints) {
            // The secret write committed before the audit state. Undo that
            // write so a failed mutation is not visible after restart. Keep
            // the audit error: a rollback failure is secondary diagnostic
            // information and must not obscure the failed audit persistence.
            let _ = remove_persisted_secret(&self.root, &id);
            return Err(error);
        }
        self.audit = next_audit;
        let previous = self.index.insert(id, stored);
        debug_assert!(
            previous.is_none(),
            "fresh SecretId must not replace an entry"
        );
        Ok(id)
    }

    /// Unseal and return the secret bytes with the verified deterministic HPKE seed.
    pub fn get(&self, id: &SecretId) -> SecretsResult<SecretBox<[u8]>> {
        let stored = self
            .index
            .get(id)
            .ok_or(SecretsError::NotFound { id: *id })?;
        if stored.record.key_version != self.key_version {
            return Err(SecretsError::RotationIncomplete {
                id: *id,
                found: stored.record.key_version,
                expected: self.key_version,
            });
        }
        let material = self
            .key_material
            .as_ref()
            .ok_or_else(|| SecretsError::KekUnavailable {
                kind: provenance_kind(&self.provenance).to_owned(),
                reason: "no validated KEK material was supplied".to_owned(),
            })?;
        crate::envelope::open(material.hpke_seed.expose_secret(), &stored.record)
    }

    /// Resolve a `secrets:` reference and unseal its secret bytes.
    ///
    /// The reference target is first interpreted as a canonical [`SecretId`].
    /// Non-ID targets are matched only against an exact metadata name. A name
    /// must select exactly one stored secret; missing and duplicate names are
    /// refused rather than selecting an arbitrary record. Blank targets are
    /// rejected before either lookup, so `secrets:` cannot resolve a blank
    /// metadata name.
    pub fn get_by_reference(&self, reference: &str) -> SecretsResult<SecretBox<[u8]>> {
        let target = reference.strip_prefix("secrets:").unwrap_or(reference);

        if target.trim().is_empty() {
            return Err(SecretsError::SecretNameNotFound {
                name: target.to_owned(),
            });
        }

        if let Ok(id) = SecretId::parse(target) {
            return self.get(&id);
        }

        let mut matching_ids = self
            .index
            .iter()
            .filter_map(|(id, stored)| (stored.metadata.name == target).then_some(*id));
        let id = matching_ids
            .next()
            .ok_or_else(|| SecretsError::SecretNameNotFound {
                name: target.to_owned(),
            })?;
        let additional_matches = matching_ids.count();
        if additional_matches != 0 {
            return Err(SecretsError::AmbiguousSecretName {
                name: target.to_owned(),
                matches: additional_matches + 1,
            });
        }

        self.get(&id)
    }

    /// Rotate every V2 record's wrapped DEK to caller-supplied next KEK
    /// material. Payload ciphertext and nonce are never re-encrypted.
    ///
    /// The full replacement directory is staged and synced before it is
    /// promoted. The previous active directory remains as a recovery/rollback
    /// copy until the rotation audit state is durable. Neither the in-memory
    /// generation nor KEK material changes until that transaction succeeds.
    pub fn rotate(&mut self, next_key_material: KekMaterial) -> SecretsResult<()> {
        let current_material =
            self.key_material
                .as_ref()
                .ok_or_else(|| SecretsError::KekUnavailable {
                    kind: provenance_kind(&self.provenance).to_owned(),
                    reason: "no validated KEK material was supplied".to_owned(),
                })?;
        let next_key_version = self.key_version.next();
        let now = jiff::Timestamp::now();
        let mut rotated_index = self.index.clone();

        for stored in rotated_index.values_mut() {
            if stored.record.key_version != self.key_version {
                return Err(SecretsError::RotationIncomplete {
                    id: stored.record.id,
                    found: stored.record.key_version,
                    expected: self.key_version,
                });
            }
            stored.record.wrapped_dek = crate::envelope::rewrap_v2(
                current_material.hpke_seed.expose_secret(),
                &stored.record,
                &next_key_material.public_key,
                next_key_version,
            )?;
            stored.record.key_version = next_key_version;
            stored.metadata.key_version = next_key_version;
            stored.metadata.updated_at = now;
        }

        stage_secrets_directory(&self.root, rotated_index.values())?;
        let replacement = match promote_staged_secrets_directory(&self.root) {
            Ok(replacement) => replacement,
            Err(error) => {
                let _ = remove_staged_secrets_directory(&self.root);
                return Err(error);
            }
        };

        let mut next_audit = self.audit.clone();
        let subjects = rotated_index
            .keys()
            .map(|id| SubjectRef::new("secret", &format_secret_id(id)))
            .collect();
        next_audit.append(Actor::System, "secret.rotate", subjects);
        if let Err(error) = persist_audit_state(&self.root, &next_audit, &self.checkpoints) {
            let _ = replacement.rollback();
            return Err(error);
        }

        // The old directory is only cleanup after the promoted replacement and
        // audit state are durable. If cleanup fails, startup recovery safely
        // removes it while retaining the active directory.
        let _ = replacement.finalize();
        self.index = rotated_index;
        self.key_version = next_key_version;
        self.key_material = Some(next_key_material);
        self.audit = next_audit;
        Ok(())
    }

    /// Durably remove a secret before dropping it from the in-process index.
    pub fn delete(&mut self, id: &SecretId) -> SecretsResult<()> {
        let stored = self
            .index
            .get(id)
            .cloned()
            .ok_or(SecretsError::NotFound { id: *id })?;
        remove_persisted_secret(&self.root, id)?;
        let mut next_audit = self.audit.clone();
        next_audit.append(
            Actor::System,
            "secret.delete",
            vec![SubjectRef::new("secret", &format_secret_id(id))],
        );
        if let Err(error) = persist_audit_state(&self.root, &next_audit, &self.checkpoints) {
            // Re-persist the pre-delete sealed entry, never plaintext or KEK
            // material. Preserve the audit error if this best-effort rollback
            // also fails.
            let _ = persist_secret(&self.root, &stored);
            return Err(error);
        }
        self.audit = next_audit;
        let _ = self.index.remove(id);
        Ok(())
    }
}

const AUDIT_FILE: &str = "audit.log";
const CHECKPOINT_FILE: &str = "checkpoints.log";
const SECRETS_DIRECTORY: &str = "secrets";
const SECRETS_STAGE_DIRECTORY: &str = ".secrets-rotation-stage";
const SECRETS_BACKUP_DIRECTORY: &str = ".secrets-rotation-backup";
const SECRET_FILE_EXTENSION: &str = "json";
const CHECKPOINT_MAGIC: &[u8] = b"HARW-CHECKPOINT\0";
static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

fn format_secret_id(id: &SecretId) -> String {
    id.as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn secrets_root(root: &Path) -> PathBuf {
    root.join(SECRETS_DIRECTORY)
}

fn staged_secrets_root(root: &Path) -> PathBuf {
    root.join(SECRETS_STAGE_DIRECTORY)
}

fn backup_secrets_root(root: &Path) -> PathBuf {
    root.join(SECRETS_BACKUP_DIRECTORY)
}

fn secret_file_name(id: &SecretId) -> String {
    format!("{}.{}", format_secret_id(id), SECRET_FILE_EXTENSION)
}

/// A promoted replacement whose old active directory remains available until
/// the caller finishes its wider durability transaction (currently audit
/// persistence). Keeping that backup makes a process death between renames
/// recoverable without ever treating a missing active directory as data loss.
struct SecretsDirectoryReplacement {
    root: PathBuf,
    had_active_directory: bool,
}

impl SecretsDirectoryReplacement {
    fn rollback(self) -> SecretsResult<()> {
        let active = secrets_root(&self.root);
        let stage = staged_secrets_root(&self.root);
        let backup = backup_secrets_root(&self.root);
        if self.had_active_directory {
            if active.exists() {
                fs::rename(&active, &stage)?;
                sync_directory(&self.root)?;
            }
            fs::rename(&backup, &active)?;
        } else if active.exists() {
            fs::rename(&active, &stage)?;
        }
        sync_directory(&self.root)?;
        if stage.exists() {
            fs::remove_dir_all(stage)?;
            sync_directory(&self.root)?;
        }
        Ok(())
    }

    fn finalize(self) -> SecretsResult<()> {
        if self.had_active_directory {
            let backup = backup_secrets_root(&self.root);
            if backup.exists() {
                fs::remove_dir_all(backup)?;
                sync_directory(&self.root)?;
            }
        }
        Ok(())
    }
}

/// Reconcile a previous interrupted directory promotion before inspecting the
/// active store. The backup always wins when the active directory is absent;
/// the staged directory is promoted only when no prior active directory exists
/// to recover. When active exists, either sibling is stale cleanup.
fn recover_secrets_directory(root: &Path) -> SecretsResult<()> {
    fs::create_dir_all(root)?;
    let active = secrets_root(root);
    let stage = staged_secrets_root(root);
    let backup = backup_secrets_root(root);

    ensure_directory_or_absent(&active, "active secrets path")?;
    ensure_directory_or_absent(&stage, "staged secrets path")?;
    ensure_directory_or_absent(&backup, "backup secrets path")?;

    if !active.exists() {
        if backup.exists() {
            fs::rename(&backup, &active)?;
        } else if stage.exists() {
            fs::rename(&stage, &active)?;
        } else {
            return Ok(());
        }
        sync_directory(root)?;
    }

    // An active directory is authoritative after a completed promotion. Any
    // siblings are safe remnants: stage from before promotion and backup from
    // after promotion. Remove only after active is known present.
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    sync_directory(root)
}

fn ensure_directory_or_absent(path: &Path, description: &str) -> SecretsResult<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(invalid_durable_data(format!(
            "{description} is not a directory"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SecretsError::Io(error)),
    }
}

fn sync_directory(directory: &Path) -> SecretsResult<()> {
    File::open(directory)
        .map_err(SecretsError::Io)?
        .sync_all()?;
    Ok(())
}

fn remove_staged_secrets_directory(root: &Path) -> SecretsResult<()> {
    let stage = staged_secrets_root(root);
    if stage.exists() {
        fs::remove_dir_all(stage)?;
        sync_directory(root)?;
    }
    Ok(())
}

fn stage_secrets_directory<'a>(
    root: &Path,
    records: impl Iterator<Item = &'a StoredSecret>,
) -> SecretsResult<()> {
    recover_secrets_directory(root)?;
    let stage = staged_secrets_root(root);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
        sync_directory(root)?;
    }
    fs::create_dir(&stage)?;
    for stored in records {
        persist_secret_in_directory(&stage, stored)?;
    }
    sync_directory(&stage)
}

fn promote_staged_secrets_directory(root: &Path) -> SecretsResult<SecretsDirectoryReplacement> {
    let active = secrets_root(root);
    let stage = staged_secrets_root(root);
    let backup = backup_secrets_root(root);
    ensure_directory_or_absent(&stage, "staged secrets path")?;
    if !stage.exists() {
        return Err(invalid_durable_data(
            "cannot promote a missing staged secrets directory",
        ));
    }
    ensure_directory_or_absent(&active, "active secrets path")?;
    if backup.exists() {
        return Err(invalid_durable_data(
            "cannot promote while a backup secrets directory exists",
        ));
    }

    let had_active_directory = active.exists();
    if had_active_directory {
        fs::rename(&active, &backup)?;
        sync_directory(root)?;
    }
    if let Err(error) = fs::rename(&stage, &active) {
        if had_active_directory {
            let _ = fs::rename(&backup, &active);
            let _ = sync_directory(root);
        }
        return Err(SecretsError::Io(error));
    }
    if let Err(error) = sync_directory(root) {
        let replacement = SecretsDirectoryReplacement {
            root: root.to_owned(),
            had_active_directory,
        };
        let _ = replacement.rollback();
        return Err(error);
    }
    Ok(SecretsDirectoryReplacement {
        root: root.to_owned(),
        had_active_directory,
    })
}

fn persist_secret(root: &Path, stored: &StoredSecret) -> SecretsResult<()> {
    if stored.record.id != stored.metadata.id {
        return Err(invalid_durable_data(
            "record and metadata IDs must match before persistence",
        ));
    }
    if stored.record.key_version != stored.metadata.key_version {
        return Err(invalid_durable_data(
            "record and metadata key versions must match before persistence",
        ));
    }

    let directory = secrets_root(root);
    fs::create_dir_all(&directory)?;
    persist_secret_in_directory(&directory, stored)
}

fn persist_secret_in_directory(directory: &Path, stored: &StoredSecret) -> SecretsResult<()> {
    if stored.record.id != stored.metadata.id {
        return Err(invalid_durable_data(
            "record and metadata IDs must match before persistence",
        ));
    }
    if stored.record.key_version != stored.metadata.key_version {
        return Err(invalid_durable_data(
            "record and metadata key versions must match before persistence",
        ));
    }
    let serialized = serde_json::to_vec(stored).map_err(|_| {
        persistence_format_error("encode durable secret JSON", "serialization failed")
    })?;
    atomic_write(directory, &secret_file_name(&stored.record.id), &serialized)
}

fn load_index(
    root: &Path,
    current_key_version: KeyVersion,
) -> SecretsResult<HashMap<SecretId, StoredSecret>> {
    recover_secrets_directory(root)?;
    let directory = secrets_root(root);
    match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(SecretsError::Io(error)),
    }
    .try_fold(HashMap::new(), |mut index, entry| {
        let entry = entry.map_err(SecretsError::Io)?;
        let file_type = entry.file_type().map_err(SecretsError::Io)?;
        if !file_type.is_file() {
            return Err(invalid_durable_data(
                "secret store contains a non-file entry",
            ));
        }
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_durable_data("secret store filename is not UTF-8"))?;
        let Some(id_text) = file_name.strip_suffix(".json") else {
            return Err(invalid_durable_data(
                "secret store entry has an unexpected filename",
            ));
        };
        if id_text.len() != 32 || !id_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_durable_data(
                "secret store entry has a non-canonical ID filename",
            ));
        }

        let bytes = fs::read(entry.path()).map_err(SecretsError::Io)?;
        let stored = serde_json::from_slice::<StoredSecret>(&bytes)
            .map_err(|_| persistence_format_error("decode durable secret JSON", "invalid JSON"))?;
        if format_secret_id(&stored.record.id) != id_text
            || stored.metadata.id != stored.record.id
            || stored.metadata.key_version != stored.record.key_version
        {
            return Err(invalid_durable_data(
                "secret store entry identity or key version does not match its contents",
            ));
        }
        if stored.record.key_version != current_key_version {
            return Err(SecretsError::RotationIncomplete {
                id: stored.record.id,
                found: stored.record.key_version,
                expected: current_key_version,
            });
        }
        if index.insert(stored.record.id, stored).is_some() {
            return Err(invalid_durable_data(
                "secret store contains duplicate secret IDs",
            ));
        }
        Ok(index)
    })
}

fn remove_persisted_secret(root: &Path, id: &SecretId) -> SecretsResult<()> {
    let directory = secrets_root(root);
    let path = directory.join(secret_file_name(id));
    fs::remove_file(path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn invalid_durable_data(reason: impl Into<String>) -> SecretsError {
    persistence_format_error("validate durable secret entry", reason)
}

fn persistence_format_error(
    operation: impl Into<String>,
    reason: impl Into<String>,
) -> SecretsError {
    SecretsError::PersistenceFormat {
        operation: operation.into(),
        reason: reason.into(),
    }
}

fn persist_audit_state(
    root: &Path,
    audit: &AuditLog,
    checkpoints: &CheckpointLog,
) -> SecretsResult<()> {
    fs::create_dir_all(root)?;
    let mut audit_bytes = Vec::from(AUDIT_MAGIC);
    audit_bytes.extend_from_slice(&AUDIT_FORMAT_VERSION.to_be_bytes());
    audit_bytes.extend_from_slice(&(audit.len() as u64).to_be_bytes());
    for event in audit.events() {
        audit_bytes.extend_from_slice(&canonical_bytes(event));
        write_actor(&mut audit_bytes, &event.actor);
        write_string(&mut audit_bytes, &event.action);
        audit_bytes.extend_from_slice(&(event.subjects.len() as u64).to_be_bytes());
        for subject in &event.subjects {
            write_string(&mut audit_bytes, &subject.kind);
            write_string(&mut audit_bytes, &subject.id);
        }
        audit_bytes.extend_from_slice(&event.recorded_at.as_second().to_be_bytes());
        audit_bytes.extend_from_slice(&event.recorded_at.subsec_nanosecond().to_be_bytes());
        audit_bytes.extend_from_slice(&event.prev_hash);
    }

    let mut checkpoint_bytes = Vec::from(CHECKPOINT_MAGIC);
    checkpoint_bytes.extend_from_slice(&1u32.to_be_bytes());
    checkpoint_bytes.extend_from_slice(&(checkpoints.len() as u64).to_be_bytes());
    for checkpoint in checkpoints.checkpoints() {
        checkpoint_bytes.extend_from_slice(&checkpoint.chain_head_hash);
        checkpoint_bytes.extend_from_slice(&checkpoint.event_count.to_be_bytes());
        checkpoint_bytes.extend_from_slice(&checkpoint.signed_at.as_second().to_be_bytes());
        checkpoint_bytes.extend_from_slice(&checkpoint.signed_at.subsec_nanosecond().to_be_bytes());
        write_bytes(&mut checkpoint_bytes, &checkpoint.signature);
        checkpoint_bytes.extend_from_slice(&checkpoint.prev_checkpoint_hash);
    }

    atomic_write(root, AUDIT_FILE, &audit_bytes)?;
    atomic_write(root, CHECKPOINT_FILE, &checkpoint_bytes)
}

fn write_actor(out: &mut Vec<u8>, actor: &Actor) {
    match actor {
        Actor::Operator(name) => {
            out.push(0);
            write_string(out, name);
        }
        Actor::Agent { session_id } => {
            out.push(1);
            write_string(out, session_id);
        }
        Actor::System => out.push(2),
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> SecretsResult<()> {
    atomic_write_with_sequence(root, name, bytes, &NEXT_TEMPORARY_FILE_ID)
}

fn atomic_write_with_sequence(
    root: &Path,
    name: &str,
    bytes: &[u8],
    sequence: &AtomicU64,
) -> SecretsResult<()> {
    let target = root.join(name);
    let (temporary, mut file) = create_temporary_file(root, name, sequence)?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        File::open(root)?.sync_all()
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(SecretsError::Io(error));
    }
    Ok(())
}

fn create_temporary_file(
    root: &Path,
    name: &str,
    sequence: &AtomicU64,
) -> SecretsResult<(PathBuf, File)> {
    loop {
        let temporary_id = sequence.fetch_add(1, Ordering::Relaxed);
        let temporary = root.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            temporary_id
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SecretsError::Io(error)),
        }
    }
}

fn provenance_kind(provenance: &KekProvenance) -> &'static str {
    match provenance {
        KekProvenance::KeyFile { .. } => "key_file",
        KekProvenance::OsKeyring { .. } => "os_keyring",
        KekProvenance::EnvSeed { .. } => "env_seed",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::AtomicU64, Arc, Barrier};

    use crypt_guard::pq_hpke::{derive_recipient_key_pair, Kem};
    use secrecy::ExposeSecret;

    use crate::audit::chain::sha256;
    use crate::error::AuditError;

    use super::*;

    fn store_with_key_material() -> SecretStore {
        let policy = CryptoPolicy::ml_kem_512();
        SecretStore::with_key_material(
            test_root("in-process"),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            test_material(policy),
        )
    }

    fn store_with_root(root: PathBuf) -> SecretStore {
        let policy = CryptoPolicy::ml_kem_512();
        SecretStore::with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            test_material(policy),
        )
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harw-secrets-{label}-{}-{}",
            std::process::id(),
            format_secret_id(&SecretId::new())
        ))
    }

    fn hpke_kem(policy: CryptoPolicy) -> Kem {
        match policy.kem {
            crate::policy::KemAlgo::MlKem512 => Kem::MlKem512,
            crate::policy::KemAlgo::MlKem768 => Kem::MlKem768,
            crate::policy::KemAlgo::MlKem1024 => Kem::MlKem1024,
        }
    }

    fn keypair_bytes(policy: CryptoPolicy) -> (Vec<u8>, [u8; 32]) {
        let seed = [0xA5; 32];
        let recipient = derive_recipient_key_pair(hpke_kem(policy), &seed)
            .expect("deterministic HPKE recipient keypair");
        (recipient.public_key().as_bytes().to_vec(), seed)
    }

    fn test_material(policy: CryptoPolicy) -> KekMaterial {
        let (public, seed) = keypair_bytes(policy);
        KekMaterial::new(public, SecretBox::new(seed.to_vec().into_boxed_slice()))
            .expect("32-byte deterministic HPKE seed")
    }

    fn material_from_hpke_parts(public: Vec<u8>, seed: [u8; 32]) -> KekMaterial {
        KekMaterial::new(public, SecretBox::new(seed.to_vec().into_boxed_slice()))
            .expect("32-byte deterministic HPKE seed")
    }

    fn material_from_seed(policy: CryptoPolicy, seed: [u8; 32]) -> KekMaterial {
        let recipient = derive_recipient_key_pair(hpke_kem(policy), &seed)
            .expect("deterministic HPKE recipient keypair");
        material_from_hpke_parts(recipient.public_key().as_bytes().to_vec(), seed)
    }

    #[test]
    fn kek_material_rejects_non_32_byte_hpke_seeds() {
        let seed = SecretBox::new(vec![0xA5; 31].into_boxed_slice());

        assert!(matches!(
            KekMaterial::new(vec![0x01], seed),
            Err(SecretsError::InvalidHpkeSeedLength { actual: 31 })
        ));
    }

    #[test]
    fn create_get_and_delete_are_sealed_in_process() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let id = store
            .create("provider-token", "provider-auth", &secret)
            .expect("create sealed record");

        assert_eq!(store.len(), 1);
        assert_eq!(
            store
                .get(&id)
                .expect("open sealed record")
                .expose_secret()
                .as_ref(),
            b"token-value"
        );
        assert_eq!(
            store.metadata(&id).expect("metadata").name,
            "provider-token"
        );

        store.delete(&id).expect("delete record");
        assert!(matches!(store.get(&id), Err(SecretsError::NotFound { .. })));
    }

    #[test]
    fn get_by_reference_resolves_a_canonical_secret_id() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &secret)
            .expect("create sealed record");

        let resolved = store
            .get_by_reference(&format!("secrets:{id}"))
            .expect("resolve canonical id reference");

        assert_eq!(resolved.expose_secret().as_ref(), b"token-value");
    }

    #[test]
    fn get_by_reference_resolves_a_unique_exact_metadata_name() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store
            .create("provider-token", "provider-auth", &secret)
            .expect("create sealed record");

        let resolved = store
            .get_by_reference("secrets:provider-token")
            .expect("resolve unique metadata name");

        assert_eq!(resolved.expose_secret().as_ref(), b"token-value");
    }

    #[test]
    fn get_by_reference_rejects_an_unknown_metadata_name() {
        let store = store_with_key_material();

        assert!(matches!(
            store.get_by_reference("secrets:missing-provider-token"),
            Err(SecretsError::SecretNameNotFound { name }) if name == "missing-provider-token"
        ));
    }

    #[test]
    fn get_by_reference_rejects_blank_and_whitespace_targets() {
        let store = store_with_key_material();

        for reference in ["", "   ", "secrets:", "secrets: \t"] {
            let target = reference.strip_prefix("secrets:").unwrap_or(reference);
            assert!(matches!(
                store.get_by_reference(reference),
                Err(SecretsError::SecretNameNotFound { name }) if name == target
            ));
        }
    }

    #[test]
    fn get_by_reference_rejects_a_blank_prefixed_target_even_when_metadata_matches() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store
            .create("", "blank-name", &secret)
            .expect("create blank-name metadata");

        assert!(matches!(
            store.get_by_reference("secrets:"),
            Err(SecretsError::SecretNameNotFound { name }) if name.is_empty()
        ));
    }

    #[test]
    fn get_by_reference_rejects_ambiguous_metadata_names() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store
            .create("provider-token", "first-provider-auth", &secret)
            .expect("create first sealed record");
        store
            .create("provider-token", "second-provider-auth", &secret)
            .expect("create second sealed record");

        assert!(matches!(
            store.get_by_reference("secrets:provider-token"),
            Err(SecretsError::AmbiguousSecretName { name, matches: 2 }) if name == "provider-token"
        ));
    }

    #[test]
    fn create_without_validated_key_material_is_refused() {
        let mut store = SecretStore::new(
            PathBuf::from("/tmp/harw-secrets-test"),
            CryptoPolicy::ml_kem_512(),
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
        );
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        assert!(matches!(
            store.create("provider-token", "provider-auth", &secret),
            Err(SecretsError::KekUnavailable { .. })
        ));
    }

    #[test]
    fn list_is_sorted_by_name_then_secret_id_bytes() {
        let mut store = store_with_key_material();
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let zulu = store
            .create("zulu", "provider-auth", &secret)
            .expect("zulu");
        let alpha_first = store
            .create("alpha", "provider-auth", &secret)
            .expect("first alpha");
        let alpha_second = store
            .create("alpha", "provider-auth", &secret)
            .expect("second alpha");

        let mut alpha_ids = [alpha_first, alpha_second];
        alpha_ids.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let metadata = store.list();

        assert_eq!(
            metadata
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "alpha", "zulu"]
        );
        assert_eq!(metadata[0].id, alpha_ids[0]);
        assert_eq!(metadata[1].id, alpha_ids[1]);
        assert_eq!(metadata[2].id, zulu);
    }

    #[test]
    fn concurrent_atomic_writes_use_distinct_temporary_files() {
        let root = std::env::temp_dir().join(format!(
            "harw-secrets-atomic-write-{}-{}",
            std::process::id(),
            format_secret_id(&SecretId::new())
        ));
        fs::create_dir_all(&root).expect("create test directory");

        let sequence = Arc::new(AtomicU64::new(0));
        let start = Arc::new(Barrier::new(2));
        let mut writes = Vec::new();
        for contents in [b"first write".as_slice(), b"second write".as_slice()] {
            let root = root.clone();
            let sequence = Arc::clone(&sequence);
            let start = Arc::clone(&start);
            writes.push(std::thread::spawn(move || {
                start.wait();
                atomic_write_with_sequence(&root, "shared.log", contents, &sequence)
            }));
        }

        for write in writes {
            write.join().expect("write thread").expect("atomic write");
        }

        let persisted = fs::read(root.join("shared.log")).expect("persisted file");
        assert!(persisted == b"first write" || persisted == b"second write");
        assert!(fs::read_dir(&root)
            .expect("test directory entries")
            .all(|entry| !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn successful_mutations_persist_audit_and_checkpoint_state_atomically() {
        let root = test_root("audit");
        let mut store = store_with_root(root.clone());
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let id = store
            .create("provider-token", "provider-auth", &secret)
            .expect("create sealed record");
        store.delete(&id).expect("delete record");

        assert_eq!(store.audit_log().len(), 2);
        assert_eq!(store.audit_log().events()[0].action, "secret.create");
        assert_eq!(store.audit_log().events()[1].action, "secret.delete");
        assert!(store.audit_log().verify().is_ok());
        assert!(root.join(AUDIT_FILE).is_file());
        assert!(root.join(CHECKPOINT_FILE).is_file());
        assert!(!std::fs::read(root.join(AUDIT_FILE))
            .expect("audit file")
            .is_empty());
        assert!(!std::fs::read(root.join(CHECKPOINT_FILE))
            .expect("checkpoint file")
            .is_empty());
    }

    #[test]
    fn sealed_entries_survive_a_store_restart() {
        let root = test_root("restart-round-trip");
        let policy = CryptoPolicy::ml_kem_512();
        let (public, seed) = keypair_bytes(policy);
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed),
        );
        let value = SecretBox::new(b"restart-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create");

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public, seed),
        )
        .expect("reopen sealed store");

        assert_eq!(reopened.len(), 1);
        assert_eq!(
            reopened.metadata(&id).expect("metadata").name,
            "provider-token"
        );
        assert_eq!(
            reopened
                .get(&id)
                .expect("unseal after restart")
                .expose_secret()
                .as_ref(),
            b"restart-token"
        );
    }

    #[test]
    fn opening_with_a_new_generation_rejects_durable_old_generation_records() {
        let root = test_root("rotation-incomplete-open");
        let policy = CryptoPolicy::ml_kem_512();
        let (public, seed) = keypair_bytes(policy);
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed),
        );
        let value = SecretBox::new(b"old-generation-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create old-generation record");
        let expected = KeyVersion::initial().next();

        assert!(matches!(
            SecretStore::open(
                root.clone(),
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                expected,
            ),
            Err(SecretsError::RotationIncomplete {
                id: found_id,
                found,
                expected: found_expected,
            }) if found_id == id && found == KeyVersion::initial() && found_expected == expected
        ));

        assert!(matches!(
            SecretStore::open_with_key_material(
                root,
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                expected,
                material_from_hpke_parts(public, seed),
            ),
            Err(SecretsError::RotationIncomplete {
                id: found_id,
                found,
                expected: found_expected,
            }) if found_id == id && found == KeyVersion::initial() && found_expected == expected
        ));
    }

    #[test]
    fn rotation_rewraps_only_deks_and_reopens_with_next_material() {
        let root = test_root("rotation-rewrap");
        let policy = CryptoPolicy::ml_kem_512();
        let old_material = material_from_seed(policy, [0xA5; 32]);
        let next_material = material_from_seed(policy, [0x5A; 32]);
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            old_material,
        );
        let value = SecretBox::new(b"rotation-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create record for rotation");
        let before = store.record(&id).expect("record before rotation");

        store.rotate(next_material).expect("rotate V2 record");

        let after = store.record(&id).expect("record after rotation");
        assert_eq!(after.envelope_format, before.envelope_format);
        assert_eq!(after.ciphertext, before.ciphertext);
        assert_eq!(after.nonce, before.nonce);
        assert_ne!(after.wrapped_dek, before.wrapped_dek);
        assert_eq!(after.key_version, KeyVersion::initial().next());
        assert_eq!(
            store
                .metadata(&id)
                .expect("metadata after rotation")
                .key_version,
            KeyVersion::initial().next()
        );
        assert_eq!(
            store
                .audit_log()
                .events()
                .last()
                .expect("rotation audit")
                .action,
            "secret.rotate"
        );

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial().next(),
            material_from_seed(policy, [0x5A; 32]),
        )
        .expect("reopen rotated store with next material");
        assert_eq!(
            reopened
                .get(&id)
                .expect("open with next material")
                .expose_secret()
                .as_ref(),
            b"rotation-token"
        );
    }

    #[test]
    fn legacy_rotation_is_rejected_without_mutating_memory_or_disk() {
        let root = test_root("legacy-rotation");
        let policy = CryptoPolicy::ml_kem_512();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32]),
        );
        let value = SecretBox::new(b"legacy-token".to_vec().into_boxed_slice());
        let id = store
            .create("legacy", "test", &value)
            .expect("create record");
        let legacy = store.index.get_mut(&id).expect("stored record");
        legacy.record.envelope_format = crate::record::SecretEnvelopeFormat::LegacyDirectHpke;
        persist_secret(&root, legacy).expect("persist legacy discriminator");
        let before_record = store.record(&id).expect("legacy record before rotation");
        let before_bytes = fs::read(secrets_root(&root).join(secret_file_name(&id)))
            .expect("legacy durable entry before rotation");

        assert!(matches!(
            store.rotate(material_from_seed(policy, [0x5A; 32])),
            Err(SecretsError::PersistenceFormat { .. })
        ));

        assert_eq!(store.key_version(), KeyVersion::initial());
        assert_eq!(
            store.record(&id).expect("record after failure"),
            before_record
        );
        assert_eq!(
            fs::read(secrets_root(&root).join(secret_file_name(&id)))
                .expect("legacy durable entry after failure"),
            before_bytes
        );
        assert_eq!(store.audit_log().len(), 1);
    }

    #[test]
    fn open_recovers_a_backup_when_interrupted_promotion_has_no_active_directory() {
        let root = test_root("rotation-recovery");
        let policy = CryptoPolicy::ml_kem_512();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32]),
        );
        let value = SecretBox::new(b"recovery-token".to_vec().into_boxed_slice());
        let id = store
            .create("recovery", "test", &value)
            .expect("create record");
        fs::rename(secrets_root(&root), backup_secrets_root(&root))
            .expect("simulate crash after active-to-backup rename");

        let reopened = SecretStore::open_with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32]),
        )
        .expect("recover backup on open");

        assert!(secrets_root(&root).is_dir());
        assert!(!backup_secrets_root(&root).exists());
        assert_eq!(
            reopened
                .get(&id)
                .expect("open recovered secret")
                .expose_secret()
                .as_ref(),
            b"recovery-token"
        );
    }

    #[test]
    fn get_rejects_an_in_memory_record_from_a_different_generation_before_opening() {
        let mut store = store_with_key_material();
        let value = SecretBox::new(b"generation-guard-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create sealed record");
        let expected = store.key_version();
        let found = expected.next();
        let stored = store.index.get_mut(&id).expect("created record in index");
        stored.record.key_version = found;
        stored.metadata.key_version = found;

        assert!(matches!(
            store.get(&id),
            Err(SecretsError::RotationIncomplete {
                id: found_id,
                found: found_version,
                expected: expected_version,
            }) if found_id == id && found_version == found && expected_version == expected
        ));
    }

    #[test]
    fn durable_delete_survives_a_store_restart() {
        let root = test_root("delete-restart");
        let policy = CryptoPolicy::ml_kem_512();
        let (public, seed) = keypair_bytes(policy);
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed),
        );
        let value = SecretBox::new(b"delete-token".to_vec().into_boxed_slice());
        let id = store
            .create("delete-me", "provider-auth", &value)
            .expect("create");
        store.delete(&id).expect("durable delete");

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public, seed),
        )
        .expect("reopen after delete");

        assert!(reopened.is_empty());
        assert!(matches!(
            reopened.metadata(&id),
            Err(SecretsError::NotFound { .. })
        ));
    }

    #[test]
    fn malformed_durable_entries_are_rejected_on_open() {
        let root = test_root("malformed-entry");
        let id = SecretId::new();
        let directory = secrets_root(&root);
        fs::create_dir_all(&directory).expect("create secrets directory");
        fs::write(directory.join(secret_file_name(&id)), b"not valid JSON")
            .expect("write malformed entry");
        let policy = CryptoPolicy::ml_kem_512();
        let (public, seed) = keypair_bytes(policy);

        assert!(matches!(
            SecretStore::open_with_key_material(
                root,
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                KeyVersion::initial(),
                material_from_hpke_parts(public, seed),
            ),
            Err(SecretsError::PersistenceFormat { .. })
        ));
    }

    #[test]
    fn failed_durable_create_does_not_advance_memory_state() {
        let root = test_root("create-failure");
        fs::write(&root, b"not a directory").expect("create blocking file");
        let mut store = store_with_root(root);
        let value = SecretBox::new(b"unpersisted-token".to_vec().into_boxed_slice());

        assert!(matches!(
            store.create("provider-token", "provider-auth", &value),
            Err(SecretsError::Io(_))
        ));
        assert!(store.is_empty());
        assert_eq!(store.audit_log().len(), 0);
    }

    #[test]
    fn failed_audit_after_create_removes_the_newly_persisted_secret() {
        let root = test_root("create-audit-rollback");
        fs::create_dir_all(root.join(CHECKPOINT_FILE)).expect("block checkpoint file");
        let mut store = store_with_root(root.clone());
        let value = SecretBox::new(b"rolled-back-token".to_vec().into_boxed_slice());

        assert!(matches!(
            store.create("provider-token", "provider-auth", &value),
            Err(SecretsError::Io(_))
        ));

        assert!(store.is_empty());
        assert_eq!(store.audit_log().len(), 0);
        assert!(fs::read_dir(secrets_root(&root))
            .expect("secrets directory")
            .next()
            .is_none());
    }

    #[test]
    fn failed_audit_after_delete_restores_the_exact_sealed_entry() {
        let root = test_root("delete-audit-rollback");
        let mut store = store_with_root(root.clone());
        let value = SecretBox::new(b"restored-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create sealed record");
        let path = secrets_root(&root).join(secret_file_name(&id));
        let original_entry = fs::read(&path).expect("original sealed entry");

        fs::remove_file(root.join(CHECKPOINT_FILE)).expect("remove checkpoint file");
        fs::create_dir(root.join(CHECKPOINT_FILE)).expect("block checkpoint file");

        assert!(matches!(store.delete(&id), Err(SecretsError::Io(_))));

        assert!(store.contains(&id));
        assert_eq!(store.audit_log().len(), 1);
        assert_eq!(
            fs::read(path).expect("restored sealed entry"),
            original_entry,
            "rollback must restore the exact sealed record bytes"
        );
    }

    #[test]
    fn verify_persisted_audit_chain_confirms_a_real_disk_round_trip() {
        let root = test_root("persisted-chain-intact");
        let mut store = store_with_root(root);
        let value = SecretBox::new(b"chain-check-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create sealed record");
        store.delete(&id).expect("delete record");

        match store.verify_persisted_audit_chain() {
            Ok(PersistedChainStatus::Intact {
                event_count,
                chain_head,
            }) => {
                assert_eq!(event_count, 2);
                assert_eq!(chain_head, store.audit_log().chain_head());
            }
            other => panic!("expected an intact persisted chain, got {other:?}"),
        }
    }

    #[test]
    fn verify_persisted_audit_chain_reports_absent_for_a_store_with_no_mutations() {
        let root = test_root("persisted-chain-absent");
        let store = store_with_root(root);

        assert!(matches!(
            store.verify_persisted_audit_chain(),
            Ok(PersistedChainStatus::Absent)
        ));
    }

    #[test]
    fn verify_persisted_audit_chain_detects_a_disk_file_tampered_after_the_fact() {
        let root = test_root("persisted-chain-tampered");
        let mut store = store_with_root(root.clone());
        let value = SecretBox::new(b"tamper-check-token".to_vec().into_boxed_slice());
        let id = store
            .create("provider-token", "provider-auth", &value)
            .expect("create sealed record");
        store.delete(&id).expect("delete record");

        // The first event's hash is event index 1's `prev_hash`, present
        // once in its primary encoding and once in its redundant trailer
        // (see `persist_audit_state`). Overwriting both occurrences with a
        // different value breaks only the chain link, not the file's
        // framing, so this must surface as `ChainBroken`, not a load error.
        let event0_hash = sha256(&canonical_bytes(&store.audit_log().events()[0]));
        let mut bytes = fs::read(root.join(AUDIT_FILE)).expect("read persisted audit log");
        let replacement = [0xEE_u8; 32];
        let mut replaced = 0;
        let mut i = 0;
        while i + 32 <= bytes.len() {
            if bytes[i..i + 32] == event0_hash {
                bytes[i..i + 32].copy_from_slice(&replacement);
                replaced += 1;
                i += 32;
            } else {
                i += 1;
            }
        }
        assert_eq!(
            replaced, 2,
            "expected the prev_hash to appear in both the primary and redundant encodings"
        );
        fs::write(root.join(AUDIT_FILE), &bytes).expect("write tampered audit log");

        let error = store
            .verify_persisted_audit_chain()
            .expect_err("a tampered disk file must not verify as intact");
        assert!(matches!(error, AuditError::ChainBroken { index: 1, .. }));
        assert!(!error.to_string().contains("provider-token"));
        assert!(!error.to_string().contains("chain-check-token"));
        assert!(!error.to_string().contains("tamper-check-token"));
    }

    #[test]
    fn verify_persisted_audit_chain_rejects_a_truncated_disk_file() {
        let root = test_root("persisted-chain-truncated");
        let mut store = store_with_root(root.clone());
        let value = SecretBox::new(b"truncate-check-token".to_vec().into_boxed_slice());
        store
            .create("provider-token", "provider-auth", &value)
            .expect("create sealed record");

        let bytes = fs::read(root.join(AUDIT_FILE)).expect("read persisted audit log");
        let truncated = &bytes[..bytes.len() / 2];
        fs::write(root.join(AUDIT_FILE), truncated).expect("write truncated audit log");

        assert!(matches!(
            store.verify_persisted_audit_chain(),
            Err(AuditError::Io(_))
        ));
    }
}

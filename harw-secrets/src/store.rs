//! `SecretStore` — the durable, DEK-sealed secret store `harw-config` reads at
//! process start (spec §1a). On disk every new value is a V2 DEK-wrapped
//! record (payload under a per-secret DEK, DEK wrapped by crypt_guard v3 PQ
//! HPKE), or a V3 KMS-wrapped record when a [`DekWrapper`] is configured; in
//! memory, after unsealing, callers get `secrecy::SecretBox<[u8]>`.
//!
//! The in-process create/get/delete path is fully sealed through the
//! versioned envelope module. Durable record loading and key-provenance resolution stay
//! explicit separate boundaries: a caller must supply verified KEK material,
//! and no guessed key-file format is silently accepted.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};

use crate::audit::chain::{
    AUDIT_FORMAT_VERSION, AUDIT_MAGIC, AuditLog, PersistedChainStatus, canonical_bytes,
};
use crate::audit::checkpoint::{
    CHECKPOINT_FORMAT_VERSION, CHECKPOINT_MAGIC, Checkpoint, CheckpointLog,
    PersistedCheckpointStatus,
};
use crate::audit::event::{Actor, SubjectRef};
use crate::dek_wrapper::DekWrapper;
use crate::error::{AuditError, AuditResult, SecretsError, SecretsResult};
use crate::id::{KeyVersion, SecretId};
use crate::kek::KekProvenance;
use crate::policy::{CryptoPolicy, KemAlgo};
use crate::record::{SecretEnvelopeFormat, SecretMetadata, SecretRecord};

/// Verified KEK material supplied by the configured key-provenance boundary.
/// The public bytes are the canonical serialization of the validated hybrid
/// HPKE recipient key for the store policy's KEM. The matching 32-byte root
/// KEK seed is retained in a zeroizing container and exposed only to envelope
/// open/rewrap calls, which derive the per-KEM recipient seed from it.
pub struct KekMaterial {
    public_key: Vec<u8>,
    hpke_seed: SecretBox<[u8]>,
}

impl KekMaterial {
    /// Construct material from validated recipient public bytes and its exact
    /// 32-byte deterministic HPKE seed.
    ///
    /// This constructor deliberately does not accept an expanded or
    /// KEM-specific recipient private key (such as the output of
    /// [`crate::kek::derive_secret_key`]): `hpke_seed` must be the root KEK
    /// seed. Callers must derive and validate the recipient public key at the
    /// provenance boundary (see [`crate::kek::load_kek_material`]).
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

/// Counts returned by [`SecretStore::migrate_all_to_v3`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V3MigrationReport {
    /// Legacy direct-HPKE (V1) records migrated (payload re-encrypted).
    pub migrated_v1: usize,
    /// DEK-wrapped V2 records migrated (payload ciphertext unchanged).
    pub migrated_v2: usize,
    /// Records that already were V3 and were left untouched.
    pub already_v3: usize,
}

impl V3MigrationReport {
    /// Total number of records migrated by this run.
    #[must_use]
    pub fn migrated(&self) -> usize {
        self.migrated_v1 + self.migrated_v2
    }
}

/// The local, filesystem-backed secret store.
///
/// New secrets are sealed as V2 (local KEK) unless a [`DekWrapper`] is
/// configured via [`Self::with_dek_wrapper`]; then they are sealed as
/// [`SecretEnvelopeFormat::KmsWrappedV3`]. Existing records are never
/// rewritten implicitly: migration is the explicit
/// [`Self::migrate_all_to_v3`].
pub struct SecretStore {
    root: PathBuf,
    policy: CryptoPolicy,
    provenance: KekProvenance,
    key_version: KeyVersion,
    key_material: Option<KekMaterial>,
    dek_wrapper: Option<Arc<dyn DekWrapper>>,
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
            dek_wrapper: None,
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
            dek_wrapper: None,
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

    /// Configure a [`DekWrapper`] (AuthHub KMS or
    /// [`crate::dek_wrapper::LocalHpkeDekWrapper`]): new secrets are then
    /// sealed as [`SecretEnvelopeFormat::KmsWrappedV3`] and V3 records become
    /// readable. Existing V1/V2 records are not touched; see
    /// [`Self::migrate_all_to_v3`].
    #[must_use]
    pub fn with_dek_wrapper(mut self, wrapper: Arc<dyn DekWrapper>) -> Self {
        self.dek_wrapper = Some(wrapper);
        self
    }

    /// The configured DEK wrapper, if any.
    #[must_use]
    pub fn dek_wrapper(&self) -> Option<&Arc<dyn DekWrapper>> {
        self.dek_wrapper.as_ref()
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

    /// Die In-Memory-Checkpoint-Kette dieses Handles: jeder über
    /// [`Self::sign_checkpoint`] erfolgreich signierte und dauerhaft
    /// geschriebene Checkpoint, in Signierreihenfolge. Wie
    /// [`Self::audit_log`] enthält sie nur, was über dieses Handle entstand;
    /// `open` lädt keine persistierten Checkpoints (dafür gibt es
    /// [`Self::verify_persisted_checkpoints`]).
    #[must_use]
    pub fn checkpoint_log(&self) -> &CheckpointLog {
        &self.checkpoints
    }

    /// Signiert den aktuellen Kettenkopf des In-Memory-Audit-Logs (§4.2) als
    /// neuen ML-DSA-65-Checkpoint, hängt ihn an die Checkpoint-Kette an und
    /// schreibt `audit.log` und `checkpoints.log` (jede Datei für sich atomar).
    ///
    /// Der Checkpoint trägt den Kettenkopf-Hash, die Ereigniszahl als
    /// Sequenznummer, den Signierzeitpunkt und die Signatur über
    /// [`crate::audit::checkpoint::checkpoint_canonical_bytes`]; er ist über
    /// `prev_checkpoint_hash` mit seinem Vorgänger verkettet.
    ///
    /// Der Signierschlüssel ist bewusst **nicht** aus dem KEK abgeleitet
    /// (Spezifikation §4.2: „The signing key is distinct from the KEK"): wer
    /// den KEK kompromittiert, soll Checkpoints nicht fälschen können. Der
    /// Aufrufer liefert den 32-Byte-ML-DSA-65-Seed aus seiner eigenen
    /// Schlüssel-Provenienz; der passende Verifikationsschlüssel wird für
    /// [`Self::verify_checkpoints`] bzw. [`Self::verify_persisted_checkpoints`]
    /// verwendet.
    ///
    /// Transaktional wie die Mutationen: Die In-Memory-Checkpoint-Kette
    /// ändert sich erst, nachdem der Zustand dauerhaft geschrieben wurde.
    ///
    /// # Errors
    /// - [`AuditError::NonMonotonicCheckpoint`]: seit dem letzten Checkpoint
    ///   wurde kein neues Audit-Ereignis aufgezeichnet.
    /// - [`AuditError::CheckpointSigning`]: `crypt_guard` konnte mit
    ///   `signing_key` nicht signieren (z. B. ungültige Seed-Länge).
    /// - [`AuditError::Io`]: der Audit-/Checkpoint-Zustand konnte nicht
    ///   dauerhaft geschrieben werden.
    pub fn sign_checkpoint(&mut self, signing_key: &SecretBox<[u8]>) -> AuditResult<Checkpoint> {
        let mut next_checkpoints = self.checkpoints.clone();
        let checkpoint = next_checkpoints
            .emit(
                self.audit.chain_head(),
                self.audit.len() as u64,
                signing_key,
            )?
            .clone();
        persist_audit_state(&self.root, &self.audit, &next_checkpoints)
            .map_err(audit_persistence_error)?;
        self.checkpoints = next_checkpoints;
        Ok(checkpoint)
    }

    /// Verifiziert die In-Memory-Checkpoint-Kette gegen das In-Memory-Audit-Log
    /// (§4.3, Schritte 1–3):
    ///
    /// 1. die Audit-Hash-Kette selbst ([`AuditLog::verify`]),
    /// 2. die innere Checkpoint-Verkettung, strikte Monotonie der
    ///    `event_count`-Werte und keinen Checkpoint jenseits der Log-Länge,
    /// 3. für jeden Checkpoint: sein `chain_head_hash` ist exakt der
    ///    Kettenkopf des Audit-Logs nach `event_count` Ereignissen, und seine
    ///    ML-DSA-Signatur verifiziert gegen `verification_key`.
    ///
    /// # Errors
    /// - [`AuditError::ChainBroken`]: die Audit- oder Checkpoint-Verkettung ist
    ///   gebrochen, **oder** ein Checkpoint attestiert einen anderen
    ///   Kettenkopf als das Audit-Log an dieser Position hat (dann ist
    ///   `index` die `event_count` des Checkpoints, `expected` der
    ///   tatsächliche Audit-Kettenkopf und `found` der signierte Wert).
    /// - [`AuditError::NonMonotonicCheckpoint`] /
    ///   [`AuditError::CheckpointBeyondLog`]: strukturelle Verletzungen
    ///   (z. B. ein nach dem Checkpoint gekürztes Audit-Log).
    /// - [`AuditError::InvalidCheckpointSignature`]: eine Signatur verifiziert
    ///   nicht gegen `verification_key`.
    pub fn verify_checkpoints(&self, verification_key: &[u8]) -> AuditResult<()> {
        self.audit.verify()?;
        self.checkpoints.verify_chain()?;
        self.checkpoints.check_monotonic()?;
        self.checkpoints.check_within_log(self.audit.len() as u64)?;
        for checkpoint in self.checkpoints.checkpoints() {
            let expected = audit_head_after(&self.audit, checkpoint.event_count).ok_or(
                AuditError::CheckpointBeyondLog {
                    referenced: checkpoint.event_count,
                    actual: self.audit.len() as u64,
                },
            )?;
            if expected != checkpoint.chain_head_hash {
                return Err(AuditError::ChainBroken {
                    index: checkpoint.event_count,
                    expected,
                    found: checkpoint.chain_head_hash,
                });
            }
            self.checkpoints
                .verify_signature(checkpoint, verification_key)?;
        }
        Ok(())
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

    /// Load this store's persisted `checkpoints.log` from disk and verify it
    /// end to end against `audit_event_count` — the checkpoint-file
    /// counterpart to [`Self::verify_persisted_audit_chain`], for the same
    /// reason kept as a deliberately separate, explicitly-called path rather
    /// than something `open`/`open_with_key_material` runs automatically.
    ///
    /// # Arguments
    /// - `audit_event_count` (`u64`): the already-verified event count of
    ///   this store's audit chain (from [`Self::verify_persisted_audit_chain`]).
    ///   Pass `u64::MAX` when that count could not be established (e.g. the
    ///   audit chain itself failed to verify) so the range check becomes a
    ///   no-op instead of comparing against a fabricated `0`.
    /// - `verification_key` (`Option<&[u8]>`): ML-DSA verification key;
    ///   `None` explicitly skips signature verification instead of silently
    ///   reporting it as passed.
    ///
    /// # Errors
    /// See [`crate::audit::checkpoint::load_and_verify_persisted_checkpoints`]
    /// for the full breakdown of "nothing signed yet" vs. "unreadable" vs.
    /// "manipulated" (broken chain, non-monotonic, beyond the log, or an
    /// invalid signature).
    pub fn verify_persisted_checkpoints(
        &self,
        audit_event_count: u64,
        verification_key: Option<&[u8]>,
    ) -> AuditResult<PersistedCheckpointStatus> {
        crate::audit::checkpoint::load_and_verify_persisted_checkpoints(
            &self.root.join(CHECKPOINT_FILE),
            audit_event_count,
            verification_key,
        )
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

    /// Seal `secret` under a fresh per-secret DEK and return its new id: as
    /// a [`SecretEnvelopeFormat::KmsWrappedV3`] record when a DEK wrapper is
    /// configured (no local KEK material needed), otherwise as a V2 record
    /// under the local KEK (the default, unchanged).
    ///
    /// The sealed entry is durably written before either the index or audit
    /// state advances.
    pub fn create(
        &mut self,
        name: &str,
        purpose: &str,
        secret: &SecretBox<[u8]>,
    ) -> SecretsResult<SecretId> {
        let id = SecretId::new();
        let record = match &self.dek_wrapper {
            Some(wrapper) => crate::envelope::seal_v3(
                &**wrapper,
                &self.policy,
                id,
                secret.expose_secret().as_ref(),
            )?,
            None => {
                let material = self.require_key_material()?;
                let sealed = crate::envelope::seal(
                    &self.policy,
                    id,
                    self.key_version,
                    &material.public_key,
                    secret.expose_secret().as_ref(),
                )?;
                SecretRecord {
                    id,
                    envelope_format: sealed.envelope_format,
                    ciphertext: sealed.ciphertext,
                    nonce: sealed.nonce,
                    wrapped_dek: sealed.wrapped_dek,
                    kem_algo: sealed.kem_algo,
                    aead_algo: sealed.aead_algo,
                    key_version: self.key_version,
                    key_id: None,
                    key_generation: None,
                    crypto_profile_id: None,
                }
            }
        };
        let now = jiff::Timestamp::now();
        let stored = StoredSecret {
            metadata: SecretMetadata {
                id,
                name: name.to_owned(),
                purpose: purpose.to_owned(),
                key_version: record.key_version,
                created_at: now,
                updated_at: now,
            },
            record,
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

    /// Unseal and return the secret bytes: V1/V2 with the verified
    /// deterministic HPKE seed, V3 through the configured DEK wrapper (fails
    /// closed with [`SecretsError::DekWrapperUnavailable`] when none is set).
    pub fn get(&self, id: &SecretId) -> SecretsResult<SecretBox<[u8]>> {
        let stored = self
            .index
            .get(id)
            .ok_or(SecretsError::NotFound { id: *id })?;
        if stored.record.envelope_format == SecretEnvelopeFormat::KmsWrappedV3 {
            // The wrapper owns the V3 key generation; the local KEK
            // generation does not apply.
            return crate::envelope::open_with_wrapper(
                None,
                self.dek_wrapper.as_deref(),
                &stored.record,
            );
        }
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
        // Next-generation public keys for record KEMs other than the store
        // policy's, derived at most once per KEM from the next root seed.
        let mut derived_next_public_keys: Vec<(KemAlgo, Vec<u8>)> = Vec::new();

        for stored in rotated_index.values_mut() {
            // V3 DEKs are wrapped by the DEK wrapper's key lifecycle, not by
            // the local KEK: a local rotation leaves them untouched.
            if stored.record.envelope_format == SecretEnvelopeFormat::KmsWrappedV3 {
                continue;
            }
            if stored.record.key_version != self.key_version {
                return Err(SecretsError::RotationIncomplete {
                    id: stored.record.id,
                    found: stored.record.key_version,
                    expected: self.key_version,
                });
            }
            // Each record is rewrapped under the KEM it was sealed with, not
            // the current store policy (a record from before a policy change
            // would otherwise fail with `Seal(InvalidRecipientPublicKey)`).
            // Non-V2 records keep the supplied key so `rewrap_v2` reports its
            // format error first, exactly as before.
            let next_public: &[u8] = if stored.record.kem_algo == self.policy.kem
                || stored.record.envelope_format != SecretEnvelopeFormat::DekWrappedV2
            {
                &next_key_material.public_key
            } else {
                next_public_key_for_kem(
                    &mut derived_next_public_keys,
                    CryptoPolicy {
                        kem: stored.record.kem_algo,
                        aead: stored.record.aead_algo,
                    },
                    &next_key_material.hpke_seed,
                )?
            };
            stored.record.wrapped_dek = crate::envelope::rewrap_v2(
                current_material.hpke_seed.expose_secret(),
                &stored.record,
                next_public,
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
            .values()
            .filter(|stored| stored.record.envelope_format != SecretEnvelopeFormat::KmsWrappedV3)
            .map(|stored| SubjectRef::new("secret", &format_secret_id(&stored.record.id)))
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

    /// Explicitly migrate every V1/V2 record to
    /// [`SecretEnvelopeFormat::KmsWrappedV3`] under the configured DEK
    /// wrapper (Crypto-Masterplan v2 §16.3/§37: never implicit, never at
    /// startup). V2 payload ciphertext and nonce stay byte-identical; V1
    /// payloads are re-encrypted under a fresh DEK. Records that already are
    /// V3 are left untouched.
    ///
    /// Transactional like [`Self::rotate`]: the full replacement directory is
    /// staged and promoted, then the `secret.migrate_v3` audit event is made
    /// durable; any failure leaves memory and disk unchanged. With nothing to
    /// migrate, nothing is written and no audit event is recorded.
    ///
    /// # Errors
    /// - [`SecretsError::DekWrapperUnavailable`]: no DEK wrapper configured.
    /// - [`SecretsError::KekUnavailable`]: V1/V2 records exist but no local
    ///   KEK material was supplied to open them.
    /// - [`SecretsError::RotationIncomplete`]: a V1/V2 record is not at the
    ///   store's current KEK generation.
    /// - Every error of [`crate::envelope::rewrap_to_v3`] and of persistence.
    pub fn migrate_all_to_v3(&mut self) -> SecretsResult<V3MigrationReport> {
        let wrapper =
            self.dek_wrapper
                .clone()
                .ok_or_else(|| SecretsError::DekWrapperUnavailable {
                    profile: "none".to_owned(),
                    reason: "V3 migration requires a configured DEK wrapper".to_owned(),
                })?;
        let mut report = V3MigrationReport::default();
        let mut migrated_index = self.index.clone();
        let mut subjects = Vec::new();
        let now = jiff::Timestamp::now();

        for stored in migrated_index.values_mut() {
            match stored.record.envelope_format {
                SecretEnvelopeFormat::KmsWrappedV3 => {
                    report.already_v3 += 1;
                    continue;
                }
                SecretEnvelopeFormat::LegacyDirectHpke => report.migrated_v1 += 1,
                SecretEnvelopeFormat::DekWrappedV2 => report.migrated_v2 += 1,
            }
            if stored.record.key_version != self.key_version {
                return Err(SecretsError::RotationIncomplete {
                    id: stored.record.id,
                    found: stored.record.key_version,
                    expected: self.key_version,
                });
            }
            let material = self.require_key_material()?;
            stored.record = crate::envelope::rewrap_to_v3(
                material.hpke_seed.expose_secret(),
                &stored.record,
                &*wrapper,
            )?;
            stored.metadata.key_version = stored.record.key_version;
            stored.metadata.updated_at = now;
            subjects.push(SubjectRef::new(
                "secret",
                &format_secret_id(&stored.record.id),
            ));
        }

        if report.migrated() == 0 {
            return Ok(report);
        }

        stage_secrets_directory(&self.root, migrated_index.values())?;
        let replacement = match promote_staged_secrets_directory(&self.root) {
            Ok(replacement) => replacement,
            Err(error) => {
                let _ = remove_staged_secrets_directory(&self.root);
                return Err(error);
            }
        };

        let mut next_audit = self.audit.clone();
        next_audit.append(Actor::System, "secret.migrate_v3", subjects);
        if let Err(error) = persist_audit_state(&self.root, &next_audit, &self.checkpoints) {
            let _ = replacement.rollback();
            return Err(error);
        }

        let _ = replacement.finalize();
        self.index = migrated_index;
        self.audit = next_audit;
        Ok(report)
    }

    fn require_key_material(&self) -> SecretsResult<&KekMaterial> {
        self.key_material
            .as_ref()
            .ok_or_else(|| SecretsError::KekUnavailable {
                kind: provenance_kind(&self.provenance).to_owned(),
                reason: "no validated KEK material was supplied".to_owned(),
            })
    }

    /// Durably remove a secret before dropping it from the in-process index.
    ///
    /// If the audit state cannot be persisted afterwards, the removed entry is
    /// restored byte for byte from the raw durable bytes read before removal.
    pub fn delete(&mut self, id: &SecretId) -> SecretsResult<()> {
        if !self.index.contains_key(id) {
            return Err(SecretsError::NotFound { id: *id });
        }
        let directory = secrets_root(&self.root);
        let file_name = secret_file_name(id);
        // Keep the raw durable bytes, not a re-serialization: records under a
        // retired pure ML-KEM level can no longer be serialized
        // (`skip_serializing`), and only a byte-exact restore cannot alter a
        // record. These bytes are sealed ciphertext, never plaintext or KEK
        // material.
        let original_entry = fs::read(directory.join(&file_name))?;
        remove_persisted_secret(&self.root, id)?;
        let mut next_audit = self.audit.clone();
        next_audit.append(
            Actor::System,
            "secret.delete",
            vec![SubjectRef::new("secret", &format_secret_id(id))],
        );
        if let Err(error) = persist_audit_state(&self.root, &next_audit, &self.checkpoints) {
            // Preserve the audit error if this best-effort rollback also fails.
            let _ = atomic_write(&directory, &file_name, &original_entry);
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

/// Public key of the next KEK generation for `policy.kem`, derived from the
/// next root seed at most once per KEM and cached in `cache`.
///
/// # Errors
/// Same as [`crate::kek::derive_public_key`], notably
/// [`SecretsError::UnsupportedLegacyKem`] for a retired pure ML-KEM record.
fn next_public_key_for_kem<'a>(
    cache: &'a mut Vec<(KemAlgo, Vec<u8>)>,
    policy: CryptoPolicy,
    next_seed: &SecretBox<[u8]>,
) -> SecretsResult<&'a [u8]> {
    let position = match cache.iter().position(|(kem, _)| *kem == policy.kem) {
        Some(position) => position,
        None => {
            let public_key = crate::kek::derive_public_key(&policy, next_seed)?;
            cache.push((policy.kem, public_key));
            cache.len() - 1
        }
    };
    Ok(&cache[position].1)
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
        // V3 records carry the DEK wrapper's key generation, which is
        // independent of the local KEK generation.
        if stored.record.envelope_format != SecretEnvelopeFormat::KmsWrappedV3
            && stored.record.key_version != current_key_version
        {
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

/// Kettenkopf des Audit-Logs nach genau `event_count` Ereignissen, oder
/// `None`, wenn das Log kürzer ist. Setzt eine bereits verifizierte Kette
/// voraus: dann ist `events[k].prev_hash` der Kopf nach `k` Ereignissen.
fn audit_head_after(audit: &AuditLog, event_count: u64) -> Option<[u8; 32]> {
    let index = usize::try_from(event_count).ok()?;
    if index == audit.len() {
        return Some(audit.chain_head());
    }
    audit.events().get(index).map(|event| event.prev_hash)
}

/// Überführt einen Persistenzfehler von [`persist_audit_state`] in den
/// Audit-Fehlertyp. `persist_audit_state` erzeugt nur I/O-Fehler; jede andere
/// Variante wird inhaltsfrei als `InvalidData` gemeldet.
fn audit_persistence_error(error: SecretsError) -> AuditError {
    match error {
        SecretsError::Io(source) => AuditError::Io(source),
        other => AuditError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            other.to_string(),
        )),
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
    checkpoint_bytes.extend_from_slice(&CHECKPOINT_FORMAT_VERSION.to_be_bytes());
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
    use std::sync::{Arc, Barrier, atomic::AtomicU64};

    use secrecy::ExposeSecret;

    use crate::audit::chain::sha256;
    use crate::error::AuditError;
    use crate::kek::derive_public_key;
    use crate::policy::KemAlgo;
    use crate::test_support::{TestError, TestResult};

    use super::*;

    fn store_with_key_material() -> TestResult<SecretStore> {
        let policy = CryptoPolicy::strongest();
        Ok(SecretStore::with_key_material(
            test_root("in-process"),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            test_material(policy)?,
        ))
    }

    fn store_with_root(root: PathBuf) -> TestResult<SecretStore> {
        let policy = CryptoPolicy::strongest();
        Ok(SecretStore::with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            test_material(policy)?,
        ))
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harw-secrets-{label}-{}-{}",
            std::process::id(),
            format_secret_id(&SecretId::new())
        ))
    }

    /// Hybrid public key via the production derivation boundary
    /// (`RecipientPrivateKey::from_seed_bytes` + `public_key`).
    fn hybrid_public_key(policy: CryptoPolicy, seed: [u8; 32]) -> TestResult<Vec<u8>> {
        let public = derive_public_key(&policy, &SecretBox::new(seed.to_vec().into_boxed_slice()))?;
        Ok(public)
    }

    fn keypair_bytes(policy: CryptoPolicy) -> TestResult<(Vec<u8>, [u8; 32])> {
        let seed = [0xA5; 32];
        Ok((hybrid_public_key(policy, seed)?, seed))
    }

    fn test_material(policy: CryptoPolicy) -> TestResult<KekMaterial> {
        let (public, seed) = keypair_bytes(policy)?;
        let material = KekMaterial::new(public, SecretBox::new(seed.to_vec().into_boxed_slice()))?;
        Ok(material)
    }

    fn material_from_hpke_parts(public: Vec<u8>, seed: [u8; 32]) -> TestResult<KekMaterial> {
        let material = KekMaterial::new(public, SecretBox::new(seed.to_vec().into_boxed_slice()))?;
        Ok(material)
    }

    fn material_from_seed(policy: CryptoPolicy, seed: [u8; 32]) -> TestResult<KekMaterial> {
        material_from_hpke_parts(hybrid_public_key(policy, seed)?, seed)
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
    fn create_get_and_delete_are_sealed_in_process() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let id = store.create("provider-token", "provider-auth", &secret)?;

        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&id)?.expose_secret().as_ref(), b"token-value");
        assert_eq!(store.metadata(&id)?.name, "provider-token");

        store.delete(&id)?;
        assert!(matches!(store.get(&id), Err(SecretsError::NotFound { .. })));
        Ok(())
    }

    #[test]
    fn get_by_reference_resolves_a_canonical_secret_id() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &secret)?;

        let resolved = store.get_by_reference(&format!("secrets:{id}"))?;

        assert_eq!(resolved.expose_secret().as_ref(), b"token-value");
        Ok(())
    }

    #[test]
    fn get_by_reference_resolves_a_unique_exact_metadata_name() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &secret)?;

        let resolved = store.get_by_reference("secrets:provider-token")?;

        assert_eq!(resolved.expose_secret().as_ref(), b"token-value");
        Ok(())
    }

    #[test]
    fn get_by_reference_rejects_an_unknown_metadata_name() -> TestResult {
        let store = store_with_key_material()?;

        assert!(matches!(
            store.get_by_reference("secrets:missing-provider-token"),
            Err(SecretsError::SecretNameNotFound { name }) if name == "missing-provider-token"
        ));
        Ok(())
    }

    #[test]
    fn get_by_reference_rejects_blank_and_whitespace_targets() -> TestResult {
        let store = store_with_key_material()?;

        for reference in ["", "   ", "secrets:", "secrets: \t"] {
            let target = reference.strip_prefix("secrets:").unwrap_or(reference);
            assert!(matches!(
                store.get_by_reference(reference),
                Err(SecretsError::SecretNameNotFound { name }) if name == target
            ));
        }
        Ok(())
    }

    #[test]
    fn get_by_reference_rejects_a_blank_prefixed_target_even_when_metadata_matches() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store.create("", "blank-name", &secret)?;

        assert!(matches!(
            store.get_by_reference("secrets:"),
            Err(SecretsError::SecretNameNotFound { name }) if name.is_empty()
        ));
        Ok(())
    }

    #[test]
    fn get_by_reference_rejects_ambiguous_metadata_names() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());
        store.create("provider-token", "first-provider-auth", &secret)?;
        store.create("provider-token", "second-provider-auth", &secret)?;

        assert!(matches!(
            store.get_by_reference("secrets:provider-token"),
            Err(SecretsError::AmbiguousSecretName { name, matches: 2 }) if name == "provider-token"
        ));
        Ok(())
    }

    #[test]
    fn create_without_validated_key_material_is_refused() {
        let mut store = SecretStore::new(
            PathBuf::from("/tmp/harw-secrets-test"),
            CryptoPolicy::strongest(),
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
    fn list_is_sorted_by_name_then_secret_id_bytes() -> TestResult {
        let mut store = store_with_key_material()?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let zulu = store.create("zulu", "provider-auth", &secret)?;
        let alpha_first = store.create("alpha", "provider-auth", &secret)?;
        let alpha_second = store.create("alpha", "provider-auth", &secret)?;

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
        Ok(())
    }

    #[test]
    fn concurrent_atomic_writes_use_distinct_temporary_files() -> TestResult {
        let root = std::env::temp_dir().join(format!(
            "harw-secrets-atomic-write-{}-{}",
            std::process::id(),
            format_secret_id(&SecretId::new())
        ));
        fs::create_dir_all(&root)?;

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
            match write.join() {
                Ok(result) => result?,
                Err(_) => {
                    return Err(TestError::Unexpected("write thread panicked".into()));
                }
            }
        }

        let persisted = fs::read(root.join("shared.log"))?;
        assert!(persisted == b"first write" || persisted == b"second write");
        let mut saw_tmp_leftover = false;
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".tmp") {
                saw_tmp_leftover = true;
            }
        }
        assert!(!saw_tmp_leftover);
        Ok(())
    }

    #[test]
    fn successful_mutations_persist_audit_and_checkpoint_state_atomically() -> TestResult {
        let root = test_root("audit");
        let mut store = store_with_root(root.clone())?;
        let secret = SecretBox::new(b"token-value".to_vec().into_boxed_slice());

        let id = store.create("provider-token", "provider-auth", &secret)?;
        store.delete(&id)?;

        assert_eq!(store.audit_log().len(), 2);
        assert_eq!(store.audit_log().events()[0].action, "secret.create");
        assert_eq!(store.audit_log().events()[1].action, "secret.delete");
        assert!(store.audit_log().verify().is_ok());
        assert!(root.join(AUDIT_FILE).is_file());
        assert!(root.join(CHECKPOINT_FILE).is_file());
        assert!(!std::fs::read(root.join(AUDIT_FILE))?.is_empty());
        assert!(!std::fs::read(root.join(CHECKPOINT_FILE))?.is_empty());
        Ok(())
    }

    #[test]
    fn sealed_entries_survive_a_store_restart() -> TestResult {
        let root = test_root("restart-round-trip");
        let policy = CryptoPolicy::strongest();
        let (public, seed) = keypair_bytes(policy)?;
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed)?,
        );
        let value = SecretBox::new(b"restart-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public, seed)?,
        )?;

        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened.metadata(&id)?.name, "provider-token");
        assert_eq!(
            reopened.get(&id)?.expose_secret().as_ref(),
            b"restart-token"
        );
        Ok(())
    }

    #[test]
    fn opening_with_a_new_generation_rejects_durable_old_generation_records() -> TestResult {
        let root = test_root("rotation-incomplete-open");
        let policy = CryptoPolicy::strongest();
        let (public, seed) = keypair_bytes(policy)?;
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed)?,
        );
        let value = SecretBox::new(b"old-generation-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
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

        let next_material = material_from_hpke_parts(public, seed)?;
        assert!(matches!(
            SecretStore::open_with_key_material(
                root,
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                expected,
                next_material,
            ),
            Err(SecretsError::RotationIncomplete {
                id: found_id,
                found,
                expected: found_expected,
            }) if found_id == id && found == KeyVersion::initial() && found_expected == expected
        ));
        Ok(())
    }

    #[test]
    fn rotation_rewraps_only_deks_and_reopens_with_next_material() -> TestResult {
        for policy in [
            CryptoPolicy::ml_kem_768_p256(),
            CryptoPolicy::ml_kem_768_x25519(),
            CryptoPolicy::strongest(),
        ] {
            let root = test_root("rotation-rewrap");
            let old_material = material_from_seed(policy, [0xA5; 32])?;
            let next_material = material_from_seed(policy, [0x5A; 32])?;
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
            let id = store.create("provider-token", "provider-auth", &value)?;
            let before = store.record(&id)?;
            assert_eq!(before.kem_algo, policy.kem);

            store.rotate(next_material)?;

            let after = store.record(&id)?;
            assert_eq!(after.envelope_format, before.envelope_format);
            assert_eq!(after.kem_algo, policy.kem);
            assert_eq!(after.ciphertext, before.ciphertext);
            assert_eq!(after.nonce, before.nonce);
            assert_ne!(after.wrapped_dek, before.wrapped_dek);
            assert_eq!(after.key_version, KeyVersion::initial().next());
            assert_eq!(
                store.metadata(&id)?.key_version,
                KeyVersion::initial().next()
            );
            let last_event = store
                .audit_log()
                .events()
                .last()
                .ok_or(TestError::Missing("audit event after rotation"))?;
            assert_eq!(last_event.action, "secret.rotate");

            let reopen_material = material_from_seed(policy, [0x5A; 32])?;
            let reopened = SecretStore::open_with_key_material(
                root,
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                KeyVersion::initial().next(),
                reopen_material,
            )?;
            assert_eq!(
                reopened.get(&id)?.expose_secret().as_ref(),
                b"rotation-token"
            );
        }
        Ok(())
    }

    /// Durable store at `root` holding one record named `provider-token` whose
    /// KEM wire name was reset to the retired pure `ml_kem_768`, as written
    /// before the hybrid switch. Returns its id, entry path, and raw bytes.
    fn legacy_kem_store_on_disk(root: &Path) -> TestResult<(SecretId, PathBuf, Vec<u8>)> {
        let mut store = store_with_root(root.to_owned())?;
        let value = SecretBox::new(b"legacy-kem-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;

        let path = secrets_root(root).join(secret_file_name(&id));
        let mut durable: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        durable["record"]["kem_algo"] = serde_json::Value::String("ml_kem_768".to_owned());
        let legacy_bytes = serde_json::to_vec(&durable)?;
        fs::write(&path, &legacy_bytes)?;
        Ok((id, path, legacy_bytes))
    }

    fn reopen_strongest(root: &Path) -> TestResult<SecretStore> {
        let policy = CryptoPolicy::strongest();
        let material = material_from_seed(policy, [0xA5; 32])?;
        let store = SecretStore::open_with_key_material(
            root.to_owned(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material,
        )?;
        Ok(store)
    }

    #[test]
    fn legacy_pure_ml_kem_records_load_but_can_neither_be_opened_nor_rotated() -> TestResult {
        let root = test_root("legacy-kem");
        let policy = CryptoPolicy::strongest();
        let (id, path, legacy_bytes) = legacy_kem_store_on_disk(&root)?;

        let mut reopened = reopen_strongest(&root)?;

        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.record(&id)?.kem_algo, KemAlgo::LegacyMlKem768);
        assert!(matches!(
            reopened.get(&id),
            Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == "ml_kem_768"
        ));
        for reference in ["secrets:provider-token".to_owned(), format!("secrets:{id}")] {
            assert!(matches!(
                reopened.get_by_reference(&reference),
                Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == "ml_kem_768"
            ));
        }
        let without_key_material = SecretStore::open(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
        )?;
        assert_eq!(without_key_material.list().len(), 1);
        assert_eq!(without_key_material.metadata(&id)?.name, "provider-token");
        let rotate_material = material_from_seed(policy, [0x5A; 32])?;
        assert!(matches!(
            reopened.rotate(rotate_material),
            Err(SecretsError::UnsupportedLegacyKem { algo }) if algo == "ml_kem_768"
        ));
        assert_eq!(reopened.key_version(), KeyVersion::initial());
        assert_eq!(fs::read(&path)?, legacy_bytes);
        Ok(())
    }

    #[test]
    fn failed_audit_after_deleting_a_legacy_record_restores_its_raw_bytes() -> TestResult {
        let root = test_root("legacy-delete-audit-rollback");
        let (id, path, legacy_bytes) = legacy_kem_store_on_disk(&root)?;
        let mut reopened = reopen_strongest(&root)?;
        // A re-serialization based rollback cannot restore this record.
        assert!(serde_json::to_vec(&reopened.index[&id]).is_err());

        // Same audit-failure injection as
        // `failed_audit_after_delete_restores_the_exact_sealed_entry`.
        fs::remove_file(root.join(CHECKPOINT_FILE))?;
        fs::create_dir(root.join(CHECKPOINT_FILE))?;

        assert!(matches!(reopened.delete(&id), Err(SecretsError::Io(_))));

        assert!(reopened.contains(&id));
        assert_eq!(reopened.audit_log().len(), 0);
        assert_eq!(
            fs::read(&path)?,
            legacy_bytes,
            "rollback must restore the exact raw legacy record bytes"
        );

        // Disk and index stay consistent across a restart.
        fs::remove_dir(root.join(CHECKPOINT_FILE))?;
        let restarted = reopen_strongest(&root)?;
        assert_eq!(restarted.record(&id)?.kem_algo, KemAlgo::LegacyMlKem768);
        Ok(())
    }

    #[test]
    fn rotation_rewraps_each_record_under_its_own_recorded_kem() -> TestResult {
        let root = test_root("rotation-mixed-kem");
        let old_policy = CryptoPolicy::ml_kem_768_x25519();
        let new_policy = CryptoPolicy::strongest();
        let provenance = || KekProvenance::EnvSeed {
            var: "HARW_TEST_SEED".to_owned(),
        };

        let mut old_store = SecretStore::with_key_material(
            root.clone(),
            old_policy,
            provenance(),
            KeyVersion::initial(),
            material_from_seed(old_policy, [0xA5; 32])?,
        );
        let old_id = old_store.create(
            "old-token",
            "provider-auth",
            &SecretBox::new(b"old-kem-token".to_vec().into_boxed_slice()),
        )?;
        drop(old_store);

        // Policy change: same root seed, new store KEM.
        let mut store = SecretStore::open_with_key_material(
            root.clone(),
            new_policy,
            provenance(),
            KeyVersion::initial(),
            material_from_seed(new_policy, [0xA5; 32])?,
        )?;
        let new_id = store.create(
            "new-token",
            "provider-auth",
            &SecretBox::new(b"new-kem-token".to_vec().into_boxed_slice()),
        )?;

        store.rotate(material_from_seed(new_policy, [0x5A; 32])?)?;

        assert_eq!(store.record(&old_id)?.kem_algo, old_policy.kem);
        assert_eq!(store.record(&new_id)?.kem_algo, new_policy.kem);
        let reopened = SecretStore::open_with_key_material(
            root,
            new_policy,
            provenance(),
            KeyVersion::initial().next(),
            material_from_seed(new_policy, [0x5A; 32])?,
        )?;
        for (id, expected) in [
            (old_id, b"old-kem-token".as_slice()),
            (new_id, b"new-kem-token".as_slice()),
        ] {
            assert_eq!(reopened.get(&id)?.expose_secret().as_ref(), expected);
        }
        Ok(())
    }

    #[test]
    fn legacy_rotation_is_rejected_without_mutating_memory_or_disk() -> TestResult {
        let root = test_root("legacy-rotation");
        let policy = CryptoPolicy::strongest();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32])?,
        );
        let value = SecretBox::new(b"legacy-token".to_vec().into_boxed_slice());
        let id = store.create("legacy", "test", &value)?;
        let legacy = store
            .index
            .get_mut(&id)
            .ok_or(TestError::Missing("stored record"))?;
        legacy.record.envelope_format = crate::record::SecretEnvelopeFormat::LegacyDirectHpke;
        persist_secret(&root, legacy)?;
        let before_record = store.record(&id)?;
        let before_bytes = fs::read(secrets_root(&root).join(secret_file_name(&id)))?;

        let rotate_material = material_from_seed(policy, [0x5A; 32])?;
        assert!(matches!(
            store.rotate(rotate_material),
            Err(SecretsError::PersistenceFormat { .. })
        ));

        assert_eq!(store.key_version(), KeyVersion::initial());
        assert_eq!(store.record(&id)?, before_record);
        assert_eq!(
            fs::read(secrets_root(&root).join(secret_file_name(&id)))?,
            before_bytes
        );
        assert_eq!(store.audit_log().len(), 1);
        Ok(())
    }

    #[test]
    fn open_recovers_a_backup_when_interrupted_promotion_has_no_active_directory() -> TestResult {
        let root = test_root("rotation-recovery");
        let policy = CryptoPolicy::strongest();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32])?,
        );
        let value = SecretBox::new(b"recovery-token".to_vec().into_boxed_slice());
        let id = store.create("recovery", "test", &value)?;
        fs::rename(secrets_root(&root), backup_secrets_root(&root))?;

        let reopened = SecretStore::open_with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32])?,
        )?;

        assert!(secrets_root(&root).is_dir());
        assert!(!backup_secrets_root(&root).exists());
        assert_eq!(
            reopened.get(&id)?.expose_secret().as_ref(),
            b"recovery-token"
        );
        Ok(())
    }

    #[test]
    fn get_rejects_an_in_memory_record_from_a_different_generation_before_opening() -> TestResult {
        let mut store = store_with_key_material()?;
        let value = SecretBox::new(b"generation-guard-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        let expected = store.key_version();
        let found = expected.next();
        let stored = store
            .index
            .get_mut(&id)
            .ok_or(TestError::Missing("created record in index"))?;
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
        Ok(())
    }

    #[test]
    fn durable_delete_survives_a_store_restart() -> TestResult {
        let root = test_root("delete-restart");
        let policy = CryptoPolicy::strongest();
        let (public, seed) = keypair_bytes(policy)?;
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public.clone(), seed)?,
        );
        let value = SecretBox::new(b"delete-token".to_vec().into_boxed_slice());
        let id = store.create("delete-me", "provider-auth", &value)?;
        store.delete(&id)?;

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            KekProvenance::EnvSeed {
                var: "HARW_TEST_SEED".to_owned(),
            },
            KeyVersion::initial(),
            material_from_hpke_parts(public, seed)?,
        )?;

        assert!(reopened.is_empty());
        assert!(matches!(
            reopened.metadata(&id),
            Err(SecretsError::NotFound { .. })
        ));
        Ok(())
    }

    #[test]
    fn malformed_durable_entries_are_rejected_on_open() -> TestResult {
        let root = test_root("malformed-entry");
        let id = SecretId::new();
        let directory = secrets_root(&root);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(secret_file_name(&id)), b"not valid JSON")?;
        let policy = CryptoPolicy::strongest();
        let (public, seed) = keypair_bytes(policy)?;

        assert!(matches!(
            SecretStore::open_with_key_material(
                root,
                policy,
                KekProvenance::EnvSeed {
                    var: "HARW_TEST_SEED".to_owned(),
                },
                KeyVersion::initial(),
                material_from_hpke_parts(public, seed)?,
            ),
            Err(SecretsError::PersistenceFormat { .. })
        ));
        Ok(())
    }

    #[test]
    fn failed_durable_create_does_not_advance_memory_state() -> TestResult {
        let root = test_root("create-failure");
        fs::write(&root, b"not a directory")?;
        let mut store = store_with_root(root)?;
        let value = SecretBox::new(b"unpersisted-token".to_vec().into_boxed_slice());

        assert!(matches!(
            store.create("provider-token", "provider-auth", &value),
            Err(SecretsError::Io(_))
        ));
        assert!(store.is_empty());
        assert_eq!(store.audit_log().len(), 0);
        Ok(())
    }

    #[test]
    fn failed_audit_after_create_removes_the_newly_persisted_secret() -> TestResult {
        let root = test_root("create-audit-rollback");
        fs::create_dir_all(root.join(CHECKPOINT_FILE))?;
        let mut store = store_with_root(root.clone())?;
        let value = SecretBox::new(b"rolled-back-token".to_vec().into_boxed_slice());

        assert!(matches!(
            store.create("provider-token", "provider-auth", &value),
            Err(SecretsError::Io(_))
        ));

        assert!(store.is_empty());
        assert_eq!(store.audit_log().len(), 0);
        assert!(fs::read_dir(secrets_root(&root))?.next().is_none());
        Ok(())
    }

    #[test]
    fn failed_audit_after_delete_restores_the_exact_sealed_entry() -> TestResult {
        let root = test_root("delete-audit-rollback");
        let mut store = store_with_root(root.clone())?;
        let value = SecretBox::new(b"restored-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        let path = secrets_root(&root).join(secret_file_name(&id));
        let original_entry = fs::read(&path)?;

        fs::remove_file(root.join(CHECKPOINT_FILE))?;
        fs::create_dir(root.join(CHECKPOINT_FILE))?;

        assert!(matches!(store.delete(&id), Err(SecretsError::Io(_))));

        assert!(store.contains(&id));
        assert_eq!(store.audit_log().len(), 1);
        assert_eq!(
            fs::read(path)?,
            original_entry,
            "rollback must restore the exact sealed record bytes"
        );
        Ok(())
    }

    #[test]
    fn verify_persisted_audit_chain_confirms_a_real_disk_round_trip() -> TestResult {
        let root = test_root("persisted-chain-intact");
        let mut store = store_with_root(root)?;
        let value = SecretBox::new(b"chain-check-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        store.delete(&id)?;

        match store.verify_persisted_audit_chain() {
            Ok(PersistedChainStatus::Intact {
                event_count,
                chain_head,
            }) => {
                assert_eq!(event_count, 2);
                assert_eq!(chain_head, store.audit_log().chain_head());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected an intact persisted chain, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn verify_persisted_audit_chain_reports_absent_for_a_store_with_no_mutations() -> TestResult {
        let root = test_root("persisted-chain-absent");
        let store = store_with_root(root)?;

        assert!(matches!(
            store.verify_persisted_audit_chain(),
            Ok(PersistedChainStatus::Absent)
        ));
        Ok(())
    }

    #[test]
    fn verify_persisted_audit_chain_detects_a_disk_file_tampered_after_the_fact() -> TestResult {
        let root = test_root("persisted-chain-tampered");
        let mut store = store_with_root(root.clone())?;
        let value = SecretBox::new(b"tamper-check-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        store.delete(&id)?;

        // The first event's hash is event index 1's `prev_hash`, present
        // once in its primary encoding and once in its redundant trailer
        // (see `persist_audit_state`). Overwriting both occurrences with a
        // different value breaks only the chain link, not the file's
        // framing, so this must surface as `ChainBroken`, not a load error.
        let event0_hash = sha256(&canonical_bytes(&store.audit_log().events()[0]));
        let mut bytes = fs::read(root.join(AUDIT_FILE))?;
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
        fs::write(root.join(AUDIT_FILE), &bytes)?;

        let Err(error) = store.verify_persisted_audit_chain() else {
            return Err(TestError::Unexpected(
                "Err erwartet (tampered disk file must not verify as intact)".into(),
            ));
        };
        assert!(matches!(error, AuditError::ChainBroken { index: 1, .. }));
        assert!(!error.to_string().contains("provider-token"));
        assert!(!error.to_string().contains("chain-check-token"));
        assert!(!error.to_string().contains("tamper-check-token"));
        Ok(())
    }

    #[test]
    fn verify_persisted_audit_chain_rejects_a_truncated_disk_file() -> TestResult {
        let root = test_root("persisted-chain-truncated");
        let mut store = store_with_root(root.clone())?;
        let value = SecretBox::new(b"truncate-check-token".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &value)?;

        let bytes = fs::read(root.join(AUDIT_FILE))?;
        let truncated = &bytes[..bytes.len() / 2];
        fs::write(root.join(AUDIT_FILE), truncated)?;

        assert!(matches!(
            store.verify_persisted_audit_chain(),
            Err(AuditError::Io(_))
        ));
        Ok(())
    }

    fn checkpoint_keypair() -> TestResult<(SecretBox<[u8]>, Vec<u8>)> {
        let mut rng = crypt_guard::kem::backend::OsRng;
        let (signing_key, verification_key) =
            <crypt_guard::sign::ml_dsa::MlDsa65Impl as crypt_guard::sign::SignAlgorithm>::keypair(
                &mut rng,
            )
            .map_err(crate::test_support::ctx(
                "generate ML-DSA-65 checkpoint keypair",
            ))?;
        Ok((
            SecretBox::new(signing_key.as_bytes().to_vec().into_boxed_slice()),
            verification_key.as_bytes().to_vec(),
        ))
    }

    #[test]
    fn sign_checkpoint_attests_the_audit_head_and_verifies() -> TestResult {
        let root = test_root("checkpoint-sign");
        let mut store = store_with_root(root)?;
        let (signing_key, verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;

        let first = store.sign_checkpoint(&signing_key)?;
        assert_eq!(first.event_count, 1);
        assert_eq!(first.chain_head_hash, store.audit_log().chain_head());
        assert_eq!(
            first.prev_checkpoint_hash,
            crate::audit::chain::GENESIS_HASH
        );
        assert!(!first.signature.is_empty());

        store.delete(&id)?;
        let second = store.sign_checkpoint(&signing_key)?;
        assert_eq!(second.event_count, 2);
        assert_eq!(second.chain_head_hash, store.audit_log().chain_head());
        assert_eq!(
            second.prev_checkpoint_hash,
            crate::audit::checkpoint::checkpoint_hash(&first)
        );

        assert_eq!(store.checkpoint_log().len(), 2);
        store.verify_checkpoints(&verification_key)?;
        Ok(())
    }

    #[test]
    fn sign_checkpoint_without_new_events_is_refused_and_leaves_state_unchanged() -> TestResult {
        let root = test_root("checkpoint-no-new-events");
        let mut store = store_with_root(root)?;
        let (signing_key, _) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &value)?;
        store.sign_checkpoint(&signing_key)?;

        assert!(matches!(
            store.sign_checkpoint(&signing_key),
            Err(AuditError::NonMonotonicCheckpoint { index: 1 })
        ));
        assert_eq!(store.checkpoint_log().len(), 1);
        Ok(())
    }

    #[test]
    fn failed_checkpoint_persistence_does_not_advance_memory_state() -> TestResult {
        let root = test_root("checkpoint-persist-failure");
        let mut store = store_with_root(root.clone())?;
        let (signing_key, _) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &value)?;
        fs::remove_file(root.join(CHECKPOINT_FILE))?;
        fs::create_dir(root.join(CHECKPOINT_FILE))?;

        assert!(matches!(
            store.sign_checkpoint(&signing_key),
            Err(AuditError::Io(_))
        ));
        assert!(store.checkpoint_log().is_empty());
        Ok(())
    }

    #[test]
    fn signed_checkpoints_persist_and_verify_on_disk() -> TestResult {
        let root = test_root("checkpoint-persisted");
        let mut store = store_with_root(root.clone())?;
        let (signing_key, verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        store.sign_checkpoint(&signing_key)?;
        store.delete(&id)?;
        store.sign_checkpoint(&signing_key)?;

        assert_eq!(
            store.verify_persisted_checkpoints(2, Some(&verification_key))?,
            PersistedCheckpointStatus::Intact {
                checkpoint_count: 2,
                signatures_checked: true,
            }
        );

        // Ein einzelnes umgekipptes Signaturbyte muss auf der Platte auffallen.
        let signature = store
            .checkpoint_log()
            .checkpoints()
            .last()
            .ok_or(TestError::Missing("second checkpoint"))?
            .signature
            .clone();
        let mut bytes = fs::read(root.join(CHECKPOINT_FILE))?;
        let position = bytes
            .windows(signature.len())
            .position(|window| window == signature.as_slice())
            .ok_or(TestError::Missing("signature bytes in checkpoints.log"))?;
        bytes[position] ^= 0x01;
        fs::write(root.join(CHECKPOINT_FILE), &bytes)?;

        assert!(matches!(
            store.verify_persisted_checkpoints(2, Some(&verification_key)),
            Err(AuditError::InvalidCheckpointSignature { event_count: 2 })
        ));
        Ok(())
    }

    #[test]
    fn verify_checkpoints_rejects_a_wrong_verification_key() -> TestResult {
        let root = test_root("checkpoint-wrong-key");
        let mut store = store_with_root(root)?;
        let (signing_key, _) = checkpoint_keypair()?;
        let (_, other_verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &value)?;
        store.sign_checkpoint(&signing_key)?;

        assert!(matches!(
            store.verify_checkpoints(&other_verification_key),
            Err(AuditError::InvalidCheckpointSignature { event_count: 1 })
        ));
        Ok(())
    }

    #[test]
    fn verify_checkpoints_detects_a_rewritten_audit_history() -> TestResult {
        let root = test_root("checkpoint-rewritten-history");
        let mut store = store_with_root(root)?;
        let (signing_key, verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        store.create("provider-token", "provider-auth", &value)?;
        let checkpoint = store.sign_checkpoint(&signing_key)?;

        // Ein Angreifer ersetzt die Historie durch eine in sich konsistente
        // Kette gleicher Länge: die Audit-Kette allein verifiziert, der
        // signierte Kettenkopf aber nicht mehr.
        let mut forged = store.audit_log().events().to_vec();
        let first = forged
            .first_mut()
            .ok_or(TestError::Missing("audit event to forge"))?;
        first.action = "secret.forged".to_owned();
        store.audit = AuditLog::from_raw_events_for_test(forged);
        store.audit_log().verify()?;

        match store.verify_checkpoints(&verification_key) {
            Err(AuditError::ChainBroken {
                index: 1,
                expected,
                found,
            }) => {
                assert_eq!(expected, store.audit_log().chain_head());
                assert_eq!(found, checkpoint.chain_head_hash);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected a checkpoint head mismatch, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn verify_checkpoints_detects_a_truncated_audit_log() -> TestResult {
        let root = test_root("checkpoint-truncated-history");
        let mut store = store_with_root(root)?;
        let (signing_key, verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        store.delete(&id)?;
        store.sign_checkpoint(&signing_key)?;

        let mut truncated = store.audit_log().events().to_vec();
        truncated.truncate(1);
        store.audit = AuditLog::from_raw_events_for_test(truncated);

        assert!(matches!(
            store.verify_checkpoints(&verification_key),
            Err(AuditError::CheckpointBeyondLog {
                referenced: 2,
                actual: 1,
            })
        ));
        Ok(())
    }

    #[test]
    fn verify_checkpoints_accepts_an_intermediate_checkpoint_after_more_events() -> TestResult {
        let root = test_root("checkpoint-intermediate");
        let mut store = store_with_root(root)?;
        let (signing_key, verification_key) = checkpoint_keypair()?;
        let value = SecretBox::new(b"checkpoint-token".to_vec().into_boxed_slice());
        let id = store.create("provider-token", "provider-auth", &value)?;
        store.sign_checkpoint(&signing_key)?;
        store.delete(&id)?;

        // Checkpoint bei Ereignis 1, Log inzwischen bei 2: der Kopf nach
        // einem Ereignis ist `events[1].prev_hash`.
        store.verify_checkpoints(&verification_key)?;
        assert_eq!(store.checkpoint_log().len(), 1);
        Ok(())
    }

    // ---- V3 (KMS-wrapped) store integration --------------------------------

    use crate::dek_wrapper::LocalHpkeDekWrapper;

    fn local_wrapper(generation: u32) -> TestResult<Arc<dyn DekWrapper>> {
        Ok(Arc::new(LocalHpkeDekWrapper::new(
            "secrets/dek-wrap",
            generation,
            CryptoPolicy::strongest(),
            SecretBox::new(vec![0x3C_u8; 32].into_boxed_slice()),
        )?))
    }

    fn env_provenance() -> KekProvenance {
        KekProvenance::EnvSeed {
            var: "HARW_TEST_SEED".to_owned(),
        }
    }

    #[test]
    fn store_with_a_dek_wrapper_creates_v3_records_without_local_kek() -> TestResult {
        let root = test_root("v3-create");
        let policy = CryptoPolicy::strongest();
        let mut store = SecretStore::new(
            root.clone(),
            policy,
            env_provenance(),
            KeyVersion::initial(),
        )
        .with_dek_wrapper(local_wrapper(5)?);
        let value = SecretBox::new(b"kms-token".to_vec().into_boxed_slice());

        let id = store.create("provider-token", "provider-auth", &value)?;

        let record = store.record(&id)?;
        assert_eq!(record.envelope_format, SecretEnvelopeFormat::KmsWrappedV3);
        assert_eq!(record.key_generation, Some(5));
        assert_eq!(record.key_version, KeyVersion(5));
        assert_eq!(store.metadata(&id)?.key_version, KeyVersion(5));
        assert_eq!(store.get(&id)?.expose_secret().as_ref(), b"kms-token");
        let on_disk = fs::read_to_string(secrets_root(&root).join(secret_file_name(&id)))?;
        assert!(on_disk.contains(r#""envelope_format":"kms_wrapped_v3""#));
        assert!(on_disk.contains(r#""key_id":"secrets/dek-wrap""#));

        // Reopen (local KEK generation 0 differs from the wrapper's 5).
        let reopened = SecretStore::open(
            root.clone(),
            policy,
            env_provenance(),
            KeyVersion::initial(),
        )?
        .with_dek_wrapper(local_wrapper(5)?);
        assert_eq!(reopened.get(&id)?.expose_secret().as_ref(), b"kms-token");

        // Without a wrapper a V3 record fails closed.
        let without = SecretStore::open(root, policy, env_provenance(), KeyVersion::initial())?;
        assert!(matches!(
            without.get(&id),
            Err(SecretsError::DekWrapperUnavailable { .. })
        ));
        // A wrapper serving another generation is refused with a typed error.
        let wrong_generation = SecretStore::open(
            without.root().to_owned(),
            policy,
            env_provenance(),
            KeyVersion::initial(),
        )?
        .with_dek_wrapper(local_wrapper(6)?);
        assert!(matches!(
            wrong_generation.get(&id),
            Err(SecretsError::KeyGenerationMismatch {
                expected: 6,
                found: 5,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn store_without_a_dek_wrapper_still_writes_v2_json_without_kms_fields() -> TestResult {
        let root = test_root("v2-default");
        let mut store = store_with_root(root.clone())?;
        let value = SecretBox::new(b"token".to_vec().into_boxed_slice());

        let id = store.create("provider-token", "provider-auth", &value)?;

        assert_eq!(
            store.record(&id)?.envelope_format,
            SecretEnvelopeFormat::DekWrappedV2
        );
        let on_disk = fs::read_to_string(secrets_root(&root).join(secret_file_name(&id)))?;
        for absent in ["key_id", "key_generation", "crypto_profile_id"] {
            assert!(
                !on_disk.contains(absent),
                "V2 entry must not contain {absent}"
            );
        }
        Ok(())
    }

    #[test]
    fn migrate_all_to_v3_rewraps_v1_and_v2_explicitly_and_is_idempotent() -> TestResult {
        let root = test_root("v3-migrate");
        let policy = CryptoPolicy::strongest();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            env_provenance(),
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32])?,
        );
        let first = store.create(
            "first",
            "test",
            &SecretBox::new(b"first-token".to_vec().into_boxed_slice()),
        )?;
        let second = store.create(
            "second",
            "test",
            &SecretBox::new(b"second-token".to_vec().into_boxed_slice()),
        )?;
        // A genuine legacy direct-HPKE record.
        let legacy_id = SecretId::new();
        let legacy_record = crate::envelope::seal_legacy_v1_for_tests(
            &policy,
            legacy_id,
            KeyVersion::initial(),
            &hybrid_public_key(policy, [0xA5; 32])?,
            b"legacy-token",
        )?;
        let now = jiff::Timestamp::now();
        let legacy = StoredSecret {
            record: legacy_record,
            metadata: SecretMetadata {
                id: legacy_id,
                name: "legacy".to_owned(),
                purpose: "test".to_owned(),
                key_version: KeyVersion::initial(),
                created_at: now,
                updated_at: now,
            },
        };
        persist_secret(&root, &legacy)?;
        store.index.insert(legacy_id, legacy);
        let before_first = store.record(&first)?;

        // Configuring the wrapper alone migrates nothing.
        let mut store = store.with_dek_wrapper(local_wrapper(0)?);
        assert_eq!(
            store.record(&first)?.envelope_format,
            SecretEnvelopeFormat::DekWrappedV2
        );
        let events_before = store.audit_log().len();

        let report = store.migrate_all_to_v3()?;

        assert_eq!(
            report,
            V3MigrationReport {
                migrated_v1: 1,
                migrated_v2: 2,
                already_v3: 0,
            }
        );
        assert_eq!(report.migrated(), 3);
        for id in [first, second, legacy_id] {
            assert_eq!(
                store.record(&id)?.envelope_format,
                SecretEnvelopeFormat::KmsWrappedV3
            );
        }
        let after_first = store.record(&first)?;
        assert_eq!(after_first.ciphertext, before_first.ciphertext);
        assert_eq!(after_first.nonce, before_first.nonce);
        assert_eq!(store.get(&first)?.expose_secret().as_ref(), b"first-token");
        assert_eq!(
            store.get(&second)?.expose_secret().as_ref(),
            b"second-token"
        );
        assert_eq!(
            store.get(&legacy_id)?.expose_secret().as_ref(),
            b"legacy-token"
        );
        assert_eq!(store.audit_log().len(), events_before + 1);
        let last_event = store
            .audit_log()
            .events()
            .last()
            .ok_or(TestError::Missing("audit event after migration"))?;
        assert_eq!(last_event.action, "secret.migrate_v3");
        assert_eq!(last_event.subjects.len(), 3);

        // Durable: a restart with only the wrapper (no local KEK) reads all.
        let reopened = SecretStore::open(root, policy, env_provenance(), KeyVersion::initial())?
            .with_dek_wrapper(local_wrapper(0)?);
        assert_eq!(
            reopened.get(&legacy_id)?.expose_secret().as_ref(),
            b"legacy-token"
        );

        // Idempotent: a second run migrates nothing and records nothing.
        let again = store.migrate_all_to_v3()?;
        assert_eq!(
            again,
            V3MigrationReport {
                migrated_v1: 0,
                migrated_v2: 0,
                already_v3: 3,
            }
        );
        assert_eq!(store.audit_log().len(), events_before + 1);
        Ok(())
    }

    #[test]
    fn migrate_all_to_v3_without_a_wrapper_is_refused_and_changes_nothing() -> TestResult {
        let root = test_root("v3-migrate-no-wrapper");
        let mut store = store_with_root(root.clone())?;
        let id = store.create(
            "token",
            "test",
            &SecretBox::new(b"token".to_vec().into_boxed_slice()),
        )?;
        let before = fs::read(secrets_root(&root).join(secret_file_name(&id)))?;

        assert!(matches!(
            store.migrate_all_to_v3(),
            Err(SecretsError::DekWrapperUnavailable { .. })
        ));
        assert_eq!(
            store.record(&id)?.envelope_format,
            SecretEnvelopeFormat::DekWrappedV2
        );
        assert_eq!(
            fs::read(secrets_root(&root).join(secret_file_name(&id)))?,
            before
        );
        Ok(())
    }

    #[test]
    fn local_rotation_leaves_v3_records_untouched() -> TestResult {
        let root = test_root("v3-rotation");
        let policy = CryptoPolicy::strongest();
        let mut store = SecretStore::with_key_material(
            root.clone(),
            policy,
            env_provenance(),
            KeyVersion::initial(),
            material_from_seed(policy, [0xA5; 32])?,
        );
        let v2_id = store.create(
            "v2",
            "test",
            &SecretBox::new(b"v2-token".to_vec().into_boxed_slice()),
        )?;
        let mut store = store.with_dek_wrapper(local_wrapper(3)?);
        let v3_id = store.create(
            "v3",
            "test",
            &SecretBox::new(b"v3-token".to_vec().into_boxed_slice()),
        )?;
        let v3_before = store.record(&v3_id)?;

        store.rotate(material_from_seed(policy, [0x5A; 32])?)?;

        assert_eq!(store.record(&v3_id)?, v3_before);
        assert_eq!(store.record(&v2_id)?.key_version, KeyVersion(1));
        let last_event = store
            .audit_log()
            .events()
            .last()
            .ok_or(TestError::Missing("audit event after rotation"))?;
        assert_eq!(last_event.subjects.len(), 1);

        let reopened = SecretStore::open_with_key_material(
            root,
            policy,
            env_provenance(),
            KeyVersion(1),
            material_from_seed(policy, [0x5A; 32])?,
        )?
        .with_dek_wrapper(local_wrapper(3)?);
        assert_eq!(reopened.get(&v2_id)?.expose_secret().as_ref(), b"v2-token");
        assert_eq!(reopened.get(&v3_id)?.expose_secret().as_ref(), b"v3-token");
        Ok(())
    }
}

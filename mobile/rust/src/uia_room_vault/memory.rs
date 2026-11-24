//! Explicitly chosen turn references only. No model ingestion or separate plaintext store.
use super::*;

const MEMORY_MAGIC: &[u8; 8] = b"HUAM\x01\0\0\0";
const MEMORY_INFO: &[u8] = b"harw/android/vault/uia-room/manual-memory/cgh3/v1";
const MEMORY_LIMIT: usize = 8192;
pub(crate) const MEMORY_WIRE: usize = 24;

pub(crate) struct MemoryChange {
    pub(crate) expected_memory_revision: u64,
    pub(crate) ordinal: u16,
    pub(crate) save: bool,
}

impl SealedRoomVault {
    pub(crate) fn manual_memory(
        &self, expected_room_revision: u64, change: Option<MemoryChange>,
    ) -> Result<Option<[u8; MEMORY_WIRE]>, VaultError> {
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _exclusive = self.lock()?;
        self.require_active()?;
        let room = self.load_record(&keys, &self.id)?.ok_or(VaultError::Corrupt)?;
        if room.revision() != expected_room_revision {
            return Ok(None);
        }
        let rel = PathBuf::from(format!("{}.memory.cgr", blake3::hash(self.id.as_str().as_bytes())));
        let aad = [MEMORY_MAGIC.as_slice(), &self.scope, self.id.as_str().as_bytes()].concat();
        let mut value = self.read_memory(&keys, &rel, &aad)?;
        let revision = u64::from_be_bytes(value[..8].try_into().map_err(|_| VaultError::Corrupt)?);
        let position = |ordinal: usize| (8 + (ordinal - 1) / 8, 1u8 << ((ordinal - 1) % 8));
        for ordinal in (room.completed_turns().len() + 1)..=128 {
            let (byte, bit) = position(ordinal);
            if value[byte] & bit != 0 {
                return Err(VaultError::Corrupt);
            }
        }
        if let Some(change) = change {
            if change.expected_memory_revision != revision
                || !(1..=room.completed_turns().len()).contains(&usize::from(change.ordinal))
            {
                return Ok(None);
            }
            let (byte, bit) = position(usize::from(change.ordinal));
            if (value[byte] & bit != 0) != change.save {
                if change.save { value[byte] |= bit; } else { value[byte] &= !bit; }
                value[..8].copy_from_slice(&revision.checked_add(1).ok_or(VaultError::Corrupt)?.to_be_bytes());
                let wire = HpkeEnvelope::seal(SUITE, keys.recipient.public_key(), MEMORY_INFO, &aad, &value)
                    .map_err(|_| VaultError::Crypto)?.try_to_bytes().map_err(|_| VaultError::Crypto)?;
                let sig = MlDsa65Impl::sign(&keys.signing_key, signature_digest(&aad, &wire).as_bytes())
                    .map_err(|_| VaultError::Crypto)?;
                let record = [MEMORY_MAGIC.as_slice(), sig.as_ref(), &wire].concat();
                if sig.as_ref().len() != SIGNATURE_BYTES || record.len() > MEMORY_LIMIT {
                    return Err(VaultError::Crypto);
                }
                self.confirm_private_root()?;
                self.require_active()?;
                match write_atomic_with_outcome(&self.root.join(&rel), &record, AtomicWriteOptions::private())? {
                    AtomicWriteOutcome::Committed =>
                        self.root_fd.sync_all().map_err(|_| VaultError::UncertainCommit)?,
                    AtomicWriteOutcome::DurabilityUncertain(_) => return Err(VaultError::UncertainCommit),
                }
            }
        }
        self.require_active()?;
        Ok(Some(value))
    }

    fn read_memory(&self, keys: &SealingKeys, path: &Path, aad: &[u8]) -> Result<[u8; MEMORY_WIRE], VaultError> {
        let file = match open_beneath(self.root_fd.as_fd(), path, OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok([0; MEMORY_WIRE]),
            Err(error) => return Err(error.into()),
        };
        check_private_file(&file)?;
        let length = usize::try_from(file.metadata()?.len()).map_err(|_| VaultError::Corrupt)?;
        if length <= MEMORY_MAGIC.len() + SIGNATURE_BYTES || length > MEMORY_LIMIT {
            return Err(VaultError::Corrupt);
        }
        let mut bytes = Vec::with_capacity(length);
        file.take((MEMORY_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() != length {
            return Err(VaultError::Corrupt);
        }
        let (sig, wire) = bytes.strip_prefix(MEMORY_MAGIC).ok_or(VaultError::Corrupt)?
            .split_at_checked(SIGNATURE_BYTES).ok_or(VaultError::Corrupt)?;
        let signature = MlDsaSignature::from_bytes(sig.to_vec());
        MlDsa65Impl::verify(&keys.trusted_verifier, signature_digest(aad, wire).as_bytes(), &signature)
            .map_err(|_| VaultError::Corrupt)?;
        let envelope = HpkeEnvelope::from_bytes(wire).map_err(|_| VaultError::Corrupt)?;
        if envelope.suite() != SUITE { return Err(VaultError::Corrupt); }
        let opened = envelope.open_zeroizing(keys.recipient.private_key(), MEMORY_INFO, aad)
            .map_err(|_| VaultError::Corrupt)?;
        opened.as_slice().try_into().map_err(|_| VaultError::Corrupt)
    }
}

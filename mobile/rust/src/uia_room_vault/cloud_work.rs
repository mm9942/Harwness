//! One immutable operation per local room, pinned to its authenticated remote peer.
use super::*;

const MAGIC_WORK: &[u8; 8] = b"HUWC\x01\0\0\0";
const INFO_WORK: &[u8] = b"harw/android/vault/manual-cloud-work/v1";
const MAX_WORK_FILE: u64 = 8192;

impl SealedRoomVault {
    /// The returned true flag is unique: it exists only in the call that durably
    /// reserved this intent. Reopening an existing intent always resolves.
    pub(crate) fn cloud_work_intent(&self, peer: [u8; 32], explicitly_create: bool)
        -> Result<Option<(String, bool)>, VaultError>
    {
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _exclusive = self.lock()?;
        self.require_active()?;
        if self.load_record(&keys, &self.id)?.is_none() { return Err(VaultError::Corrupt); }
        let name = PathBuf::from("manual-cloud-work.cgr");
        let aad = [MAGIC_WORK.as_slice(), &self.scope, self.id.as_str().as_bytes()].concat();
        match open_beneath(self.root_fd.as_fd(), &name, OpenMode::read_only()) {
            Ok(file) => {
                check_private_file(&file)?;
                let mut bytes = Vec::new();
                file.take(MAX_WORK_FILE + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_WORK_FILE { return Err(VaultError::Corrupt); }
                let (signature, wire) = bytes.strip_prefix(MAGIC_WORK).ok_or(VaultError::Corrupt)?
                    .split_at_checked(SIGNATURE_BYTES).ok_or(VaultError::Corrupt)?;
                MlDsa65Impl::verify(&keys.trusted_verifier, signature_digest(&aad, wire).as_bytes(),
                    &MlDsaSignature::from_bytes(signature.to_vec())).map_err(|_| VaultError::Corrupt)?;
                let envelope = HpkeEnvelope::from_bytes(wire).map_err(|_| VaultError::Corrupt)?;
                if envelope.suite() != SUITE { return Err(VaultError::Corrupt); }
                let plain = envelope.open_zeroizing(keys.recipient.private_key(), INFO_WORK, &aad)
                    .map_err(|_| VaultError::Corrupt)?;
                let (scope, id) = plain.split_at_checked(32).ok_or(VaultError::Corrupt)?;
                if scope != peer.as_slice() { return Err(VaultError::InvalidKeyScope); }
                let id = valid_id(id)?;
                self.require_active()?;
                return Ok(Some((id, false)));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if !explicitly_create { return Ok(None); }
        let id = harw_types::SessionId::new();
        let id = valid_id(id.as_str().as_bytes())?;
        let plain = Zeroizing::new([peer.as_slice(), id.as_bytes()].concat());
        let wire = HpkeEnvelope::seal(SUITE, keys.recipient.public_key(), INFO_WORK, &aad, &plain)
            .map_err(|_| VaultError::Crypto)?.try_to_bytes().map_err(|_| VaultError::Crypto)?;
        let signature = MlDsa65Impl::sign(&keys.signing_key, signature_digest(&aad, &wire).as_bytes())
            .map_err(|_| VaultError::Crypto)?;
        let record = [MAGIC_WORK.as_slice(), signature.as_ref(), &wire].concat();
        if record.len() as u64 > MAX_WORK_FILE || signature.as_ref().len() != SIGNATURE_BYTES {
            return Err(VaultError::Crypto);
        }
        self.confirm_private_root()?;
        self.require_active()?;
        match write_atomic_with_outcome(&self.root.join(name), &record, AtomicWriteOptions::private())? {
            AtomicWriteOutcome::Committed => self.root_fd.sync_all().map_err(|_| VaultError::UncertainCommit)?,
            AtomicWriteOutcome::DurabilityUncertain(_) => return Err(VaultError::UncertainCommit),
        }
        self.require_active()?;
        Ok(Some((id, true)))
    }
}

fn valid_id(bytes: &[u8]) -> Result<String, VaultError> {
    if !(1..=128).contains(&bytes.len()) || !bytes.iter().copied().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(VaultError::Corrupt);
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| VaultError::Corrupt)
}

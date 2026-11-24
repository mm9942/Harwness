//! One bounded, sealed, content-free attempt marker for the private room.
use super::*;
use crate::uia_room_store::{RecordedRoomRun, RoomPhotoIdentity, RoomResponseApproval, RoomRunId, next_turn, room_input_fingerprint, validate_user_message};
use harw_fsutil::{rename_at, unlink_at};
use harw_types::cancel::CancelToken;
use tokio::{sync::oneshot, task::JoinHandle};
use std::{fs::Permissions, io::Write as _};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};

const ATTEMPT_MAGIC: &[u8; 8] = b"HUAT\x01\0\0\0";
const ATTEMPT_INFO: &[u8] = b"harw/android/vault/uia-room/turn-attempt/cgh3/v1";
const ATTEMPT_LIMIT: usize = 8192;
const LEGACY_PAYLOAD_BYTES: usize = 24;
const PINNED_PAYLOAD_BYTES: usize = LEGACY_PAYLOAD_BYTES + 32;
// At most one encrypted orphan after process death, never a replay source.
const COMPLETION_STAGE: &str = ".turn-completion-v1.tmp";

struct TurnAttempt {
    revision: u64,
    /// Legacy markers are interruption evidence only, not a commit witness.
    base_frame: Option<[u8; 32]>,
}

impl SealedRoomVault {
    /// Record only a random attempt ID, existing committed revision and
    /// fingerprint of the exact already-authenticated ciphertext frame.
    /// Neither draft content, photo identity, model output, nor a resume
    /// credential is written. An unrelated refresh cannot create this file.
    pub(crate) fn record_turn_attempt(
        &self, current: RoomLog, input: blake3::Hash,
    ) -> Result<Option<RecordedRoomRun>, VaultError> {
        if current.id() != &self.id { return Err(VaultError::InvalidKeyScope); }
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _guard = self.lock_mode(false)?;
        self.require_active()?;
        let sealed = self.read_record_frame(&self.id)?.ok_or(VaultError::Corrupt)?;
        let existing = self.decode_record(&keys, &self.id, &sealed)?;
        if existing != current { return Ok(None); }
        let room_frame = blake3::hash(&sealed);
        let base_frame = *room_frame.as_bytes();
        // A same-revision replacement is not the frame we authenticated for
        // that attempt. Do not make questionable evidence disappear.
        if self.load_turn_attempt(&keys)?.is_some_and(|prior| {
            prior.revision > current.revision() ||
                (prior.revision == current.revision() &&
                    prior.base_frame.is_some_and(|pinned| pinned != base_frame))
        }) {
            return Err(VaultError::Corrupt);
        }
        let mut random = [0; 16];
        SystemRandom::new().fill(&mut random).map_err(|_| VaultError::Crypto)?;
        let attempt = RoomRunId::from_bytes(random).ok_or(VaultError::Crypto)?;
        let mut payload = [0; PINNED_PAYLOAD_BYTES];
        payload[..8].copy_from_slice(&current.revision().to_be_bytes());
        payload[8..LEGACY_PAYLOAD_BYTES].copy_from_slice(&attempt.bytes());
        payload[LEGACY_PAYLOAD_BYTES..].copy_from_slice(&base_frame);
        let aad = self.turn_attempt_aad();
        let wire = HpkeEnvelope::seal(SUITE, keys.recipient.public_key(), ATTEMPT_INFO, &aad, &payload)
            .map_err(|_| VaultError::Crypto)?.try_to_bytes().map_err(|_| VaultError::Crypto)?;
        let signature = MlDsa65Impl::sign(&keys.signing_key, signature_digest(&aad, &wire).as_bytes())
            .map_err(|_| VaultError::Crypto)?;
        let file = [ATTEMPT_MAGIC.as_slice(), signature.as_ref(), &wire].concat();
        if signature.as_ref().len() != SIGNATURE_BYTES || file.len() > ATTEMPT_LIMIT {
            return Err(VaultError::Crypto);
        }
        self.confirm_private_root()?;
        self.require_active()?;
        self.write_turn_atomic(&self.root.join(self.turn_attempt_path()), &file)?;
        Ok(Some(RecordedRoomRun {
            base: current, scope: self.scope, room_frame,
            attempt_frame: blake3::hash(&file), input,
        }))
    }

    /// Mint in the authenticated foreground, only for this recorded send.
    /// The actor below holds no recipient private key, vault owner, or JNI
    /// identity. Only the fixed successor protocol can reach its signer.
    pub(crate) fn start_text_completion(
        &self, attempt: RecordedRoomRun, user: &str,
        same_profile: Arc<dyn Fn() -> bool + Send + Sync>, cancel: CancelToken,
    ) -> Result<RoomCompletionMailbox, VaultError> {
        self.start_completion(attempt, user, CompletionScope { photo: None, same_profile }, cancel)
    }

    /// The photo was read by the exact authorized foreground draft path.
    /// The sealer gets the authenticated identity and a status-only
    /// exact-profile predicate, never the JPEG or any Library authority.
    pub(crate) fn start_photo_completion(
        &self, attempt: RecordedRoomRun, user: &str, identity: RoomPhotoIdentity,
        same_profile: Arc<dyn Fn() -> bool + Send + Sync>, cancel: CancelToken,
    ) -> Result<RoomCompletionMailbox, VaultError> {
        if identity.digest().is_none() {
            return Err(VaultError::Locked);
        }
        self.start_completion(attempt, user, CompletionScope { photo: Some(identity), same_profile }, cancel)
    }

    fn start_completion(
        &self, attempt: RecordedRoomRun, user: &str, input: CompletionScope, cancel: CancelToken,
    ) -> Result<RoomCompletionMailbox, VaultError> {
        if attempt.scope != self.scope || attempt.base.id() != &self.id ||
            validate_user_message(user).ok() != Some(user) ||
            room_input_fingerprint(user, input.photo) != attempt.input ||
            attempt.base.admit_user(user).is_err()
        {
            return Err(VaultError::InvalidKeyScope);
        }
        if !input.allows(&cancel) { return Err(VaultError::Locked); }
        self.confirm_private_root()?;
        let keys = self.active_keys()?;
        let one = ReplySealer {
            root: self.root.clone(),
            dir: self.root_fd.try_clone()?,
            room: Self::record_name(&self.id),
            marker: self.turn_attempt_path(),
            aad: self.aad(&self.id),
            recipient: keys.recipient.public_key().clone(),
            signer: Arc::clone(&keys.signing_key),
            user: Zeroizing::new(user.to_owned()),
            attempt,
            input,
        };
        drop(keys);
        self.require_active()?;
        if cancel.is_cancelled() { return Err(VaultError::Locked); }
        let (input, request) = oneshot::channel();
        let join = tokio::spawn(async move {
            let (answer, approval) = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(false),
                request = request => match request {
                    Ok(request) => request,
                    Err(_) => return Ok(false),
                }
            };
            tokio::task::spawn_blocking(move || one.finish(&answer, &cancel, approval))
                .await.map_err(|_| VaultError::UncertainCommit)?
        });
        Ok(RoomCompletionMailbox { input, join })
    }

    /// True only when the same authenticated revision has an uncompleted send.
    pub(crate) fn prior_attempt_interrupted(&self, expected: u64) -> Result<bool, VaultError> {
        let keys = self.active_keys()?;
        self.confirm_private_root()?;
        let _guard = self.lock_mode(false)?;
        self.require_active()?;
        let sealed = self.read_record_frame(&self.id)?.ok_or(VaultError::Corrupt)?;
        let revision = self.decode_record(&keys, &self.id, &sealed)?.revision();
        if revision != expected { return Err(VaultError::Busy); }
        let value = self.load_turn_attempt(&keys)?;
        self.require_active()?;
        match value {
            Some(marker) if marker.revision > revision => Err(VaultError::Corrupt),
            Some(marker) if marker.revision == revision => {
                if marker.base_frame.is_some_and(|base| base != *blake3::hash(&sealed).as_bytes()) {
                    return Err(VaultError::Corrupt);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn turn_attempt_path(&self) -> PathBuf {
        PathBuf::from(format!("{}.turn-attempt.cgr", blake3::hash(self.id.as_str().as_bytes())))
    }

    fn turn_attempt_aad(&self) -> Vec<u8> {
        [ATTEMPT_MAGIC.as_slice(), &self.scope, self.id.as_str().as_bytes()].concat()
    }

    fn load_turn_attempt(&self, keys: &SealingKeys) -> Result<Option<TurnAttempt>, VaultError> {
        read_private_frame(
            &self.root_fd, &self.turn_attempt_path(), ATTEMPT_MAGIC.len() + SIGNATURE_BYTES, ATTEMPT_LIMIT,
        )?.map(|frame| self.decode_turn_attempt(keys, &frame)).transpose()
    }

    fn decode_turn_attempt(&self, keys: &SealingKeys, bytes: &[u8]) -> Result<TurnAttempt, VaultError> {
        let (signature, wire) = bytes.strip_prefix(ATTEMPT_MAGIC).ok_or(VaultError::Corrupt)?
            .split_at_checked(SIGNATURE_BYTES).ok_or(VaultError::Corrupt)?;
        let signature = MlDsaSignature::from_bytes(signature.to_vec());
        let aad = self.turn_attempt_aad();
        MlDsa65Impl::verify(&keys.trusted_verifier, signature_digest(&aad, wire).as_bytes(), &signature)
            .map_err(|_| VaultError::Corrupt)?;
        let envelope = HpkeEnvelope::from_bytes(wire).map_err(|_| VaultError::Corrupt)?;
        if envelope.suite() != SUITE { return Err(VaultError::Corrupt); }
        let payload = envelope.open_zeroizing(keys.recipient.private_key(), ATTEMPT_INFO, &aad)
            .map_err(|_| VaultError::Corrupt)?;
        let base_frame = match payload.len() {
            LEGACY_PAYLOAD_BYTES => None,
            PINNED_PAYLOAD_BYTES => Some(
                payload[LEGACY_PAYLOAD_BYTES..].try_into().map_err(|_| VaultError::Corrupt)?,
            ),
            _ => return Err(VaultError::Corrupt),
        };
        let revision = u64::from_be_bytes(payload[..8].try_into().map_err(|_| VaultError::Corrupt)?);
        RoomRunId::from_bytes(payload[8..LEGACY_PAYLOAD_BYTES].try_into().map_err(|_| VaultError::Corrupt)?)
            .ok_or(VaultError::Corrupt)?;
        Ok(TurnAttempt { revision, base_frame })
    }

    fn write_turn_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), VaultError> {
        match write_atomic_with_outcome(path, bytes, AtomicWriteOptions::private())? {
            AtomicWriteOutcome::Committed => self.root_fd.sync_all().map_err(|_| VaultError::UncertainCommit),
            AtomicWriteOutcome::DurabilityUncertain(_) => Err(VaultError::UncertainCommit),
        }
    }
}

/// The model worker can deliver one validated response and get only a commit
/// status. Dropping the sender or cancelling the exact attempt drops the
/// waiting actor; it exposes no arbitrary sign/verify/decrypt/read operation.
pub(crate) struct RoomCompletionMailbox {
    input: oneshot::Sender<(Zeroizing<String>, RoomResponseApproval)>,
    join: JoinHandle<Result<bool, VaultError>>,
}

impl RoomCompletionMailbox {
    pub(crate) async fn abandon(self) {
        let Self { input, join } = self;
        drop(input);
        let _ = join.await;
    }

    pub(crate) async fn finish(
        self, answer: Zeroizing<String>, approval: RoomResponseApproval,
    ) -> Result<bool, VaultError> {
        let Self { input, join } = self;
        // On an expired/revoked actor the undelivered answer and approval
        // are dropped here; the actor outcome stays the source of truth.
        let _ = input.send((answer, approval));
        join.await.map_err(|_| VaultError::UncertainCommit)?
    }
}

struct CompletionScope {
    photo: Option<RoomPhotoIdentity>,
    // Status-only and profile-specific. No room/Library owner, store,
    // PIN keys, readers or resolvers are captured by the sole caller.
    same_profile: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl CompletionScope {
    fn allows(&self, cancel: &CancelToken) -> bool {
        !cancel.is_cancelled() && (self.same_profile)()
    }
}

/// Private to this file. The existing ML-DSA key is held only by the trusted
/// actor, never by the model worker. Struct ownership and its consuming
/// method bind the operation to this scope, files, predecessor and input.
struct ReplySealer {
    root: PathBuf,
    dir: File,
    room: PathBuf,
    marker: PathBuf,
    aad: Vec<u8>,
    recipient: RecipientPublicKey,
    signer: Arc<MlDsaSigningKey>,
    user: Zeroizing<String>,
    attempt: RecordedRoomRun,
    input: CompletionScope,
}

impl ReplySealer {
    fn finish(
        self, answer: &str, cancel: &CancelToken, approval: RoomResponseApproval,
    ) -> Result<bool, VaultError> {
        let Self { root, dir, room, marker, aad, recipient, signer, user, attempt, input } = self;
        if !input.allows(cancel) || !approval.matches(&attempt, answer) ||
            room_input_fingerprint(&user, input.photo) != attempt.input
        {
            return Ok(false);
        }
        let next = next_turn(&attempt.base, &user, answer, input.photo).map_err(|_| VaultError::Corrupt)?;
        if attempt.base.revision().checked_add(1) != Some(next.revision()) {
            return Err(VaultError::Corrupt);
        }
        let room_base = attempt.room_frame;
        let attempt_base = attempt.attempt_frame;
        drop(attempt);
        drop(user);
        SealedRoomVault::confirm_pinned_root(&root, &dir)?;
        let _transaction = SealedRoomVault::lock_pinned(&dir, false)?;
        if frame_fingerprint(&dir, &room, MAGIC.len() + SIGNATURE_BYTES, MAX_FILE_BYTES)? != Some(room_base) ||
            frame_fingerprint(&dir, &marker, ATTEMPT_MAGIC.len() + SIGNATURE_BYTES, ATTEMPT_LIMIT)? != Some(attempt_base) ||
            !input.allows(cancel) || !approval.arm()
        {
            return Ok(false);
        }
        // Apply the exact monotonic parked deadline even if its asynchronous
        // cancel task has not fired. A concurrent Stop during crypto still
        // fences the sealed bytes before staging and the final rename.
        if !approval.is_live() {
            return Ok(false);
        }
        let response = SealedRoomVault::seal_room_record(&recipient, &signer, &aad, &next)?;
        drop(signer);
        drop(next);
        // From here on, only opaque ciphertext and exact witness digests.
        PreparedCompletion { dir: &dir, room, room_base, attempt: marker, attempt_base, response }
            .publish_once(|| {
                SealedRoomVault::confirm_pinned_root(&root, &dir)?;
                Ok(input.allows(cancel))
            }, approval)
    }
}

/// No keys or model data: this capability can publish one already-sealed
/// replacement at a fixed room path. It is minted and consumed under the
/// existing room transaction lock. Its internal reads compare opaque bytes;
/// nothing can be decrypted or returned to callers.
struct PreparedCompletion<'a> {
    dir: &'a File,
    room: PathBuf,
    room_base: blake3::Hash,
    attempt: PathBuf,
    attempt_base: blake3::Hash,
    response: Vec<u8>,
}

impl PreparedCompletion<'_> {
    fn publish_once(
        self, authorized: impl Fn() -> Result<bool, VaultError>, approval: RoomResponseApproval,
    ) -> Result<bool, VaultError> {
        if !authorized()? || !approval.is_live() { return Ok(false); }
        let mut stage = CompletionStage::write(self.dir, &self.response)?;
        if !approval.is_live() { return Ok(false); }
        let room_hash = frame_fingerprint(self.dir, &self.room, MAGIC.len() + SIGNATURE_BYTES, MAX_FILE_BYTES)?;
        let attempt_hash = frame_fingerprint(
            self.dir, &self.attempt, ATTEMPT_MAGIC.len() + SIGNATURE_BYTES, ATTEMPT_LIMIT,
        )?;
        if room_hash != Some(self.room_base) || attempt_hash != Some(self.attempt_base) ||
            !stage.owns_name()? || !authorized()?
        {
            return Ok(false);
        }
        // User Stop, hard revoke and publication serialize on the native
        // attempt lock for the rename only, not the file preparation or fsync.
        let result = approval.publish(&mut || {
            if !stage.live { return Err(io::Error::other("completion already published")); }
            rename_at(self.dir.as_fd(), Path::new(COMPLETION_STAGE), &self.room)?;
            stage.live = false;
            Ok(())
        });
        match (result, stage.live) {
            (Ok(true), false) => {
                self.dir.sync_all().map_err(|_| VaultError::UncertainCommit)?;
                Ok(true)
            }
            (Ok(false), true) => Ok(false),
            (Err(error), true) => Err(error.into()),
            (_, false) => Err(VaultError::UncertainCommit),
            (Ok(true), true) => Err(VaultError::InvalidKeyScope),
        }
    }
}

struct CompletionStage<'a> {
    dir: &'a File,
    fd: File,
    live: bool,
}

impl<'a> CompletionStage<'a> {
    fn write(dir: &'a File, sealed: &[u8]) -> Result<Self, VaultError> {
        if !(MAGIC.len() + SIGNATURE_BYTES + 1..=MAX_FILE_BYTES).contains(&sealed.len()) {
            return Err(VaultError::Corrupt);
        }
        let path = Path::new(COMPLETION_STAGE);
        // The fixed, private noncanonical file may be a partial encrypted
        // orphan. Never read it back as a response or attempt to publish it.
        match open_beneath(dir.as_fd(), path, OpenMode::read_only()) {
            Ok(orphan) => {
                check_private_file(&orphan)?;
                if orphan.metadata()?.len() > MAX_FILE_BYTES as u64 { return Err(VaultError::Corrupt); }
                unlink_at(dir.as_fd(), path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let fd = open_beneath(dir.as_fd(), path, OpenMode::write_create_new(0o600))?;
        let mut stage = Self { dir, fd, live: true };
        stage.fd.set_permissions(Permissions::from_mode(0o600))?;
        check_private_file(&stage.fd)?;
        stage.fd.write_all(sealed)?;
        stage.fd.sync_all()?;
        Ok(stage)
    }

    fn owns_name(&self) -> Result<bool, VaultError> {
        let candidate = match open_beneath(self.dir.as_fd(), Path::new(COMPLETION_STAGE), OpenMode::read_only()) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        check_private_file(&candidate)?;
        let expected = self.fd.metadata()?;
        let actual = candidate.metadata()?;
        Ok((expected.dev(), expected.ino(), expected.len()) == (actual.dev(), actual.ino(), actual.len()))
    }
}

impl Drop for CompletionStage<'_> {
    fn drop(&mut self) {
        if self.live && self.owns_name().unwrap_or(false) {
            let _ = unlink_at(self.dir.as_fd(), Path::new(COMPLETION_STAGE));
        }
    }
}

fn read_private_frame(
    dir: &File, path: &Path, exclusive_min: usize, max: usize,
) -> Result<Option<Vec<u8>>, VaultError> {
    let file = match open_beneath(dir.as_fd(), path, OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    check_private_file(&file)?;
    let len = usize::try_from(file.metadata()?.len()).map_err(|_| VaultError::Corrupt)?;
    if len <= exclusive_min || len > max { return Err(VaultError::Corrupt); }
    let mut bytes = Vec::with_capacity(len);
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() != len { return Err(VaultError::Corrupt); }
    Ok(Some(bytes))
}

fn frame_fingerprint(dir: &File, path: &Path, exclusive_min: usize, max: usize) -> Result<Option<blake3::Hash>, VaultError> {
    Ok(read_private_frame(dir, path, exclusive_min, max)?.map(|frame| blake3::hash(&frame)))
}

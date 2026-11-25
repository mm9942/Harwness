//! Dormant native owner for a pre-enrolled room. There is no JNI export here.
//! Future Android caller contract: exact hardware-bound PIN unseal, independent
//! authenticated enrollment anchor, exclusive direct buffer and worker thread.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};
use zeroize::{Zeroize, Zeroizing};

use crate::uia_room_store::{RoomId, RoomLog, RoomVault};
use crate::uia_room_vault::{SealedRoomVault, UnwrappedMaterial, VaultError};

const FORMAT: u8 = 1;
const VERIFIER_BYTES: usize = 1952; // FIPS 204 ML-DSA-65.
pub(crate) const DIRECT_HANDOFF_BYTES: usize = 89;

#[derive(Debug)]
pub(crate) enum RoomHandoffError {
    EnrollmentMissing,
    RecoveryRequired,
    Scope,
    Locked,
    Busy,
    Frame,
    Vault(VaultError),
}

/// Data independently authenticated by the native app's enrollment owner.
/// Never construct from a direct frame, a caller path, or the room ciphertext.
/// This type does not itself load/verify the Android enrollment anchor.
pub(crate) struct VerifiedRoomEnrollment {
    root: PathBuf,
    id: RoomId,
    vault_id: [u8; 16],
    key_generation: u64,
    verifier: Vec<u8>,
}

impl VerifiedRoomEnrollment {
    pub(crate) fn from_authenticated_anchor(
        root: PathBuf,
        id: RoomId,
        vault_id: [u8; 16],
        key_generation: u64,
        verifier: Vec<u8>,
    ) -> Result<Self, RoomHandoffError> {
        if !root.is_absolute()
            || vault_id == [0; 16]
            || key_generation == 0
            || verifier.len() != VERIFIER_BYTES
        {
            return Err(RoomHandoffError::Scope);
        }
        Ok(Self {
            root,
            id,
            vault_id,
            key_generation,
            verifier,
        })
    }
}

struct Attempt;
/// Identity only; not serialized, a pointer into Kotlin, or a PIN proof.
pub(crate) struct HandoffTicket(Arc<Attempt>);

enum Phase {
    Locked,
    Pending(Arc<Attempt>),
    Opening(Arc<Attempt>),
    Open(Arc<AuthenticatedRoomLease>),
}

pub(crate) struct UiaRoomHandoff {
    enrolled: VerifiedRoomEnrollment,
    phase: Mutex<Phase>,
}

impl UiaRoomHandoff {
    pub(crate) fn from_enrollment(
        enrolled: Option<VerifiedRoomEnrollment>,
    ) -> Result<Self, RoomHandoffError> {
        let enrolled = enrolled.ok_or(RoomHandoffError::EnrollmentMissing)?;
        Ok(Self {
            enrolled,
            phase: Mutex::new(Phase::Locked),
        })
    }

    /// Reserve one handoff from the trusted owner, at most one per vault.
    /// A handoff can complete only while the same ticket still owns the slot.
    pub(crate) fn reserve(&self) -> Result<HandoffTicket, RoomHandoffError> {
        let mut state = self.phase.lock().map_err(|_| RoomHandoffError::Locked)?;
        if !matches!(*state, Phase::Locked) {
            return Err(RoomHandoffError::Busy);
        }
        let stamp = Arc::new(Attempt);
        *state = Phase::Pending(Arc::clone(&stamp));
        Ok(HandoffTicket(stamp))
    }

    fn mark_opening(&self, ticket: &HandoffTicket) -> Result<(), RoomHandoffError> {
        let mut state = self.phase.lock().map_err(|_| RoomHandoffError::Locked)?;
        if !matches!(&*state, Phase::Pending(stamp) if Arc::ptr_eq(stamp, &ticket.0)) {
            return Err(RoomHandoffError::Locked);
        }
        *state = Phase::Opening(Arc::clone(&ticket.0));
        Ok(())
    }

    /// Invoke synchronously after authenticated hardware+PIN unseal, never by
    /// arbitrary JNI/UniFFI callers. The accepted frame has fixed layout:
    /// {version:u8, vault_uuid:[u8;16], generation:be-u64, root:[u8;32],
    /// independent_mldsa65_seed:[u8;32]}. No path, room, or verifier in frame.
    /// The future JNI wrapper must check capacity and exclusive mutable access
    /// before calling; this layer wipes its entire borrowed slice on all paths.
    pub(crate) fn accept_authenticated_frame(
        &self,
        ticket: &HandoffTicket,
        direct_frame: &mut [u8],
    ) -> Result<Arc<AuthenticatedRoomLease>, RoomHandoffError> {
        let frame = BorrowedSecretFrame(direct_frame);
        self.mark_opening(ticket)?;
        let material = self.decode_material(&frame);
        drop(frame); // Before crypto or I/O, even when parsing failed.
        let candidate = material.and_then(|material| self.open_existing(material));
        self.finish(ticket, candidate)
    }

    fn decode_material(
        &self,
        frame: &BorrowedSecretFrame<'_>,
    ) -> Result<UnwrappedMaterial, RoomHandoffError> {
        let bytes = &*frame.0;
        if bytes.len() != DIRECT_HANDOFF_BYTES || bytes[0] != FORMAT {
            return Err(RoomHandoffError::Frame);
        }
        let expected = self.enrolled.key_generation.to_be_bytes();
        if bytes[1..17] != self.enrolled.vault_id || bytes[17..25] != expected {
            return Err(RoomHandoffError::Scope);
        }
        let mut root = Zeroizing::new([0; 32]);
        let mut seed = Zeroizing::new([0; 32]);
        root.copy_from_slice(&bytes[25..57]);
        seed.copy_from_slice(&bytes[57..89]);
        if *root == *seed {
            return Err(RoomHandoffError::Scope);
        }
        UnwrappedMaterial::from_authenticated_bytes(
            root,
            self.enrolled.vault_id,
            self.enrolled.key_generation,
            seed,
            self.enrolled.verifier.clone(),
        )
        .map_err(RoomHandoffError::Vault)
    }

    fn open_existing(
        &self,
        material: UnwrappedMaterial,
    ) -> Result<AuthenticatedRoomLease, RoomHandoffError> {
        let store = SealedRoomVault::open_bound_room(
            self.enrolled.root.clone(),
            self.enrolled.id.clone(),
            material,
        )
        .map_err(RoomHandoffError::Vault)?;
        // Read an actually sealed record (even for revision zero). An old
        // enrollment never obtains initial-write authority from this path.
        store
            .load_existing(&self.enrolled.id)
            .map_err(RoomHandoffError::Vault)?
            .ok_or(RoomHandoffError::RecoveryRequired)?;
        Ok(AuthenticatedRoomLease { store })
    }

    fn finish(
        &self,
        ticket: &HandoffTicket,
        candidate: Result<AuthenticatedRoomLease, RoomHandoffError>,
    ) -> Result<Arc<AuthenticatedRoomLease>, RoomHandoffError> {
        let Ok(mut state) = self.phase.lock() else {
            if let Ok(candidate) = candidate {
                candidate.store.revoke();
            }
            return Err(RoomHandoffError::Locked);
        };
        if !matches!(&*state, Phase::Opening(stamp) if Arc::ptr_eq(stamp, &ticket.0)) {
            if let Ok(candidate) = candidate {
                candidate.store.revoke();
            }
            return Err(RoomHandoffError::Locked);
        }
        let lease = match candidate {
            Ok(candidate) => Arc::new(candidate),
            Err(error) => {
                *state = Phase::Locked;
                return Err(error);
            }
        };
        *state = Phase::Open(Arc::clone(&lease));
        Ok(lease)
    }

    /// Use after a rejected/cancelled attempt that did not reach the handoff.
    pub(crate) fn abort(&self, ticket: &HandoffTicket) {
        let mut state = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(&*state, Phase::Pending(stamp) | Phase::Opening(stamp)
            if Arc::ptr_eq(stamp, &ticket.0))
        {
            *state = Phase::Locked;
        }
    }

    /// Owner must also hide UI, drop snapshots, cancel work and discard late
    /// outcomes. Revocation invalidates every clone of the live native lease.
    pub(crate) fn revoke(&self) {
        let mut state = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if let Phase::Open(lease) = std::mem::replace(&mut *state, Phase::Locked) {
            lease.store.revoke();
        }
    }
}

impl Drop for UiaRoomHandoff {
    fn drop(&mut self) {
        self.revoke();
    }
}

struct BorrowedSecretFrame<'a>(&'a mut [u8]);
impl Drop for BorrowedSecretFrame<'_> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Scoped native read/append only. It cannot create a missing pre-enrolled room.
pub(crate) struct AuthenticatedRoomLease {
    store: SealedRoomVault,
}

#[cfg(feature = "vault-uia-chat")]
impl AuthenticatedRoomLease {
    pub(crate) fn manual_memory(
        &self, expected_room_revision: u64,
        change: Option<crate::uia_room_vault::MemoryChange>,
    ) -> Result<Option<[u8; crate::uia_room_vault::MEMORY_WIRE]>, RoomHandoffError> {
        self.store.manual_memory(expected_room_revision, change).map_err(RoomHandoffError::Vault)
    }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
impl AuthenticatedRoomLease {
    pub(crate) fn cloud_work_intent(&self, peer: [u8; 32], create: bool)
        -> Result<Option<(String, bool)>, RoomHandoffError>
    {
        self.store.cloud_work_intent(peer, create).map_err(RoomHandoffError::Vault)
    }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
impl AuthenticatedRoomLease {
    pub(crate) fn start_text_completion(
        &self, attempt: crate::uia_room_store::RecordedRoomRun, user: &str,
        same_profile: Arc<dyn Fn() -> bool + Send + Sync>,
        cancel: harw_types::cancel::CancelToken,
    ) -> Result<crate::uia_room_vault::RoomCompletionMailbox, RoomHandoffError> {
        self.store.start_text_completion(attempt, user, same_profile, cancel)
            .map_err(RoomHandoffError::Vault)
    }

    pub(crate) fn start_photo_completion(
        &self, attempt: crate::uia_room_store::RecordedRoomRun, user: &str,
        identity: crate::uia_room_store::RoomPhotoIdentity,
        same_profile: Arc<dyn Fn() -> bool + Send + Sync>,
        cancel: harw_types::cancel::CancelToken,
    ) -> Result<crate::uia_room_vault::RoomCompletionMailbox, RoomHandoffError> {
        self.store.start_photo_completion(attempt, user, identity, same_profile, cancel)
            .map_err(RoomHandoffError::Vault)
    }

    pub(crate) fn prior_attempt_interrupted(&self, expected_room_revision: u64) -> Result<bool, RoomHandoffError> {
        self.store.prior_attempt_interrupted(expected_room_revision).map_err(RoomHandoffError::Vault)
    }

    pub(crate) fn record_run(
        &self, base: RoomLog, input: blake3::Hash,
    ) -> Result<Option<crate::uia_room_store::RecordedRoomRun>, RoomHandoffError> {
        self.store.record_turn_attempt(base, input).map_err(RoomHandoffError::Vault)
    }
}

impl RoomVault for AuthenticatedRoomLease {
    type Error = RoomHandoffError;

    fn load(&self, id: &RoomId) -> Result<Option<RoomLog>, Self::Error> {
        self.store
            .load_existing(id)
            .map_err(RoomHandoffError::Vault)?
            .map(Some)
            .ok_or(RoomHandoffError::RecoveryRequired)
    }

    fn compare_and_swap(
        &self,
        id: &RoomId,
        expected_revision: Option<u64>,
        next: &RoomLog,
    ) -> Result<bool, Self::Error> {
        if expected_revision.is_none() {
            return Err(RoomHandoffError::RecoveryRequired);
        }
        self.store
            .compare_and_swap(id, expected_revision, next)
            .map_err(RoomHandoffError::Vault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uia_room_store::{RoomFailure, commit_turn, open_room};
    use crypt_guard::{
        kem::backend::OsRng,
        pq_hpke::generate_recipient_seed,
        sign::{SignAlgorithm, ml_dsa::MlDsa65Impl},
    };
    use std::os::unix::fs::PermissionsExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Fixture {
        dir: tempfile::TempDir,
        id: RoomId,
        root: Zeroizing<[u8; 32]>,
        seed: Zeroizing<[u8; 32]>,
        verifier: Vec<u8>,
        vault_id: [u8; 16],
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let dir = tempfile::tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
            let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef")
                .expect("valid test room");
            let root = generate_recipient_seed()?;
            let (signer, verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
            let mut seed = Zeroizing::new([0; 32]);
            seed.copy_from_slice(signer.as_bytes());
            Ok(Self {
                dir,
                id,
                root,
                seed,
                verifier: verifier.as_bytes().to_vec(),
                vault_id: [9; 16],
            })
        }

        fn enrollment(&self) -> VerifiedRoomEnrollment {
            VerifiedRoomEnrollment::from_authenticated_anchor(
                self.dir.path().to_owned(),
                self.id.clone(),
                self.vault_id,
                1,
                self.verifier.clone(),
            )
            .expect("trusted enrollment metadata")
        }

        fn provision(&self) -> SealedRoomVault {
            let material = UnwrappedMaterial::from_authenticated_bytes(
                self.root.clone(),
                self.vault_id,
                1,
                self.seed.clone(),
                self.verifier.clone(),
            )
            .expect("fresh key material");
            let store = SealedRoomVault::open_bound_room(
                self.dir.path().to_owned(),
                self.id.clone(),
                material,
            )
            .expect("provision test room");
            let empty = open_room(&store, &self.id).expect("create sealed durable room");
            assert_eq!(empty.revision(), 0);
            store
        }

        fn frame(&self) -> Zeroizing<[u8; DIRECT_HANDOFF_BYTES]> {
            let mut frame = Zeroizing::new([0; DIRECT_HANDOFF_BYTES]);
            frame[0] = 1;
            frame[1..17].copy_from_slice(&self.vault_id);
            frame[17..25].copy_from_slice(&1u64.to_be_bytes());
            frame[25..57].copy_from_slice(&self.root[..]);
            frame[57..89].copy_from_slice(&self.seed[..]);
            frame
        }
    }

    #[test]
    fn authentic_reopen_and_revoke_all_shared_leases() -> TestResult {
        let fixture = Fixture::new()?;
        let store = fixture.provision();
        let current = open_room(&store, &fixture.id).expect("authenticated room");
        commit_turn(&store, &current, "Canary question", "Canary answer").expect("durable CAS");
        drop(store);
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment()))
            .expect("owner with enrollment");
        let ticket = owner.reserve().expect("admit");
        let mut frame = fixture.frame();
        let lease = owner
            .accept_authenticated_frame(&ticket, &mut frame[..])
            .expect("real authenticated load");
        assert!(frame.iter().all(|byte| *byte == 0));
        let snapshot = open_room(&*lease, &fixture.id).expect("load sealed data");
        assert_eq!(snapshot.revision(), 1);
        assert_eq!(snapshot.completed_turns()[0].answer(), "Canary answer");
        let another = Arc::clone(&lease);
        owner.revoke();
        assert!(matches!(
            another.load(&fixture.id),
            Err(RoomHandoffError::Vault(VaultError::Locked))
        ));
        let new_ticket = owner
            .reserve()
            .expect("fresh independently authenticated attempt");
        let reopened = owner
            .accept_authenticated_frame(&new_ticket, &mut fixture.frame()[..])
            .expect("fresh lease");
        assert_eq!(
            open_room(&*reopened, &fixture.id)
                .expect("persisted")
                .revision(),
            1
        );
        drop(owner);
        assert!(matches!(
            reopened.load(&fixture.id),
            Err(RoomHandoffError::Vault(VaultError::Locked))
        ));
        Ok(())
    }

    #[test]
    fn no_enrollment_or_sealed_room_never_provisions() -> TestResult {
        assert!(matches!(
            UiaRoomHandoff::from_enrollment(None),
            Err(RoomHandoffError::EnrollmentMissing)
        ));
        let fixture = Fixture::new()?;
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment())).expect("owner");
        let ticket = owner.reserve().expect("admit");
        let mut frame = fixture.frame();
        assert!(matches!(
            owner.accept_authenticated_frame(&ticket, &mut frame[..]),
            Err(RoomHandoffError::RecoveryRequired)
        ));
        assert!(frame.iter().all(|b| *b == 0));
        assert!(!std::fs::read_dir(fixture.dir.path())?.any(|f| {
            f.ok()
                .is_some_and(|e| e.path().extension().is_some_and(|ext| ext == "cgr"))
        }));
        Ok(())
    }

    #[test]
    fn room_deletion_after_unlock_does_not_create_empty_replacement() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.provision();
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment())).expect("owner");
        let lease = owner
            .accept_authenticated_frame(&owner.reserve().expect("admit"), &mut fixture.frame()[..])
            .expect("lease");
        let record = std::fs::read_dir(fixture.dir.path())?
            .filter_map(Result::ok)
            .find(|entry| entry.path().extension().is_some_and(|x| x == "cgr"))
            .ok_or("room record")?;
        std::fs::remove_file(record.path())?;
        assert!(matches!(
            open_room(&*lease, &fixture.id),
            Err(RoomFailure::Vault(RoomHandoffError::RecoveryRequired))
        ));
        Ok(())
    }

    #[test]
    fn changed_scope_and_unpinned_signer_fail_and_wipe_frame() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.provision();
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment())).expect("owner");
        let mut incorrect = fixture.frame();
        incorrect[24] = 2;
        assert!(matches!(
            owner.accept_authenticated_frame(&owner.reserve().expect("admit"), &mut incorrect[..]),
            Err(RoomHandoffError::Scope)
        ));
        assert!(incorrect.iter().all(|b| *b == 0));
        let (wrong_signer, _wrong_verifier) = MlDsa65Impl::keypair(&mut OsRng)?;
        let mut incorrect = fixture.frame();
        incorrect[57..89].copy_from_slice(wrong_signer.as_bytes());
        assert!(matches!(
            owner.accept_authenticated_frame(
                &owner.reserve().expect("new attempt"),
                &mut incorrect[..]
            ),
            Err(RoomHandoffError::Vault(VaultError::InvalidKeyScope))
        ));
        assert!(incorrect.iter().all(|b| *b == 0));
        Ok(())
    }

    #[test]
    fn authentic_late_candidate_cannot_publish_after_relock() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.provision();
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment())).expect("owner");
        let old = owner.reserve().expect("initial attempt");
        owner.mark_opening(&old).expect("started");
        // Pause at the publication barrier with an actual candidate from
        // the correctly encrypted room, as an Android worker could do.
        let material = {
            let mut frame = fixture.frame();
            owner
                .decode_material(&BorrowedSecretFrame(&mut frame[..]))
                .expect("unsealed root")
        };
        let candidate = owner.open_existing(material).expect("candidate was valid");
        owner.revoke();
        let fresh = owner.reserve().expect("fresh attempt");
        assert!(matches!(
            owner.finish(&old, Ok(candidate)),
            Err(RoomHandoffError::Locked)
        ));
        let fresh = owner
            .accept_authenticated_frame(&fresh, &mut fixture.frame()[..])
            .expect("live");
        assert_eq!(
            open_room(&*fresh, &fixture.id)
                .expect("real room")
                .revision(),
            0
        );
        let other =
            RoomId::parse("uia-local-v1:1123456789abcdef0123456789abcdef").expect("different room");
        assert!(matches!(
            fresh.load(&other),
            Err(RoomHandoffError::Vault(VaultError::InvalidKeyScope))
        ));
        Ok(())
    }

    #[test]
    fn stale_ticket_cannot_consume_fresh_attempt_after_lock() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.provision();
        let owner = UiaRoomHandoff::from_enrollment(Some(fixture.enrollment())).expect("owner");
        let stale = owner.reserve().expect("first");
        owner.revoke();
        let fresh = owner.reserve().expect("second");
        let mut frame = fixture.frame();
        assert!(matches!(
            owner.accept_authenticated_frame(&stale, &mut frame[..]),
            Err(RoomHandoffError::Locked)
        ));
        assert!(frame.iter().all(|b| *b == 0));
        let mut bad = Zeroizing::new([9u8; DIRECT_HANDOFF_BYTES - 1]);
        assert!(matches!(
            owner.accept_authenticated_frame(&fresh, &mut bad[..]),
            Err(RoomHandoffError::Frame)
        ));
        assert!(bad.iter().all(|b| *b == 0));
        Ok(())
    }
}

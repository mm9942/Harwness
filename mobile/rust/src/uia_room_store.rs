//! Private local UIA room state. A caller must supply the authenticated vault;
//! no filesystem or in-memory production fallback is constructed here.

const ROOM_PREFIX: &str = "uia-local-v1:";
const MAX_TURNS: usize = 128;
const MAX_LOG_BYTES: usize = 256 * 1024;
const MAX_USER_BYTES: usize = 8 * 1024;
const MAX_ANSWER_BYTES: usize = 16 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RoomId(String);

impl RoomId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn parse(value: &str) -> Result<Self, InvalidRoom> {
        let id = value.strip_prefix(ROOM_PREFIX).ok_or(InvalidRoom::RoomId)?;
        if id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || id.bytes().all(|byte| byte == b'0')
        {
            return Err(InvalidRoom::RoomId);
        }
        Ok(Self(value.to_owned()))
    }
}

/// Authenticated room-only image provenance. The digest is never projected
/// to Kotlin or usable as a media lookup key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoomPhotoIdentity {
    item: [u8; 16],
    digest: Option<[u8; 32]>,
}

impl RoomPhotoIdentity {
    pub(crate) fn new(item: [u8; 16], digest: Option<[u8; 32]>) -> Result<Self, InvalidRoom> {
        if item == [0; 16] || digest == Some([0; 32]) { return Err(InvalidRoom::Attachment); }
        Ok(Self { item, digest })
    }

    pub(crate) fn item(self) -> [u8; 16] { self.item }
    pub(crate) fn digest(self) -> Option<[u8; 32]> { self.digest }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CompletedTurn {
    user: String,
    answer: String,
    library_image: Option<RoomPhotoIdentity>,
}

impl CompletedTurn {
    pub(crate) fn new(user: &str, answer: &str) -> Result<Self, InvalidRoom> {
        Self::with_library_image(user, answer, None)
    }

    pub(crate) fn with_library_image(
        user: &str, answer: &str, library_image: Option<RoomPhotoIdentity>,
    ) -> Result<Self, InvalidRoom> {
        let user = validate_user_message(user)?;
        let answer = validate_answer(answer)?;
        Ok(Self {
            user: user.to_owned(),
            answer: answer.to_owned(),
            library_image,
        })
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }
    pub(crate) fn answer(&self) -> &str {
        &self.answer
    }
    pub(crate) fn library_image(&self) -> Option<RoomPhotoIdentity> {
        self.library_image
    }
    fn bytes(&self) -> usize {
        self.user.len() + self.answer.len()
    }
}

fn validate_text(text: &str, max: usize) -> Result<&str, InvalidRoom> {
    if text.len() > max {
        return Err(InvalidRoom::Text);
    }
    let clean = text.trim();
    if clean.is_empty()
        || clean
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Err(InvalidRoom::Text);
    }
    Ok(clean)
}

pub(crate) fn validate_user_message(text: &str) -> Result<&str, InvalidRoom> {
    validate_text(text, MAX_USER_BYTES)
}

pub(crate) fn validate_answer(text: &str) -> Result<&str, InvalidRoom> {
    validate_text(text, MAX_ANSWER_BYTES)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RoomLog {
    id: RoomId,
    completed_turns: Vec<CompletedTurn>,
}

impl RoomLog {
    /// Call only after the vault has authenticated this record, enforcing
    /// its encoded length before decoding payload into completed turns.
    pub(crate) fn restore(
        id: RoomId,
        revision: u64,
        completed_turns: Vec<CompletedTurn>,
    ) -> Result<Self, InvalidRoom> {
        if completed_turns.len() > MAX_TURNS
            || completed_turns
                .iter()
                .map(CompletedTurn::bytes)
                .sum::<usize>()
                > MAX_LOG_BYTES
        {
            return Err(InvalidRoom::Full);
        }
        if revision != completed_turns.len() as u64 {
            return Err(InvalidRoom::Revision);
        }
        Ok(Self {
            id,
            completed_turns,
        })
    }

    pub(crate) fn admit_user(&self, user: &str) -> Result<(), InvalidRoom> {
        let user = validate_user_message(user)?;
        let existing = self
            .completed_turns
            .iter()
            .map(CompletedTurn::bytes)
            .sum::<usize>();
        if self.completed_turns.len() >= MAX_TURNS
            || existing + user.len() + MAX_ANSWER_BYTES > MAX_LOG_BYTES
        {
            return Err(InvalidRoom::Full);
        }
        Ok(())
    }

    pub(crate) fn id(&self) -> &RoomId {
        &self.id
    }
    pub(crate) fn revision(&self) -> u64 {
        self.completed_turns.len() as u64
    }
    pub(crate) fn completed_turns(&self) -> &[CompletedTurn] {
        &self.completed_turns
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InvalidRoom {
    RoomId,
    Revision,
    Text,
    Attachment,
    Full,
}

/// A successful compare-and-swap is persistent and performed within the
/// already-authorized local scope. The adapter must enforce authenticated
/// decode and bounds before constructing a RoomLog. A lock, corruption or
/// unauthorized scope is an error, never a missing room.
pub(crate) trait RoomVault {
    type Error;
    fn load(&self, id: &RoomId) -> Result<Option<RoomLog>, Self::Error>;
    fn compare_and_swap(
        &self,
        id: &RoomId,
        expected_revision: Option<u64>,
        next: &RoomLog,
    ) -> Result<bool, Self::Error>;
}

/// Opaque native-only attempt, bound to the exact prior room revision by the vault.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoomRunId([u8; 16]);

impl RoomRunId {
    pub(crate) fn from_bytes(wire: [u8; 16]) -> Option<Self> {
        (wire != [0; 16]).then_some(Self(wire))
    }
    pub(crate) fn bytes(self) -> [u8; 16] { self.0 }
}

/// Model path bound to the one admitted room attempt. A signal from one
/// path cannot make the other path parkable.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomActorKind { Text, Photo }

/// Native attempt gate. It binds the model worker and one complete response
/// to an exact sealed attempt. Only the final atomic rename runs under it.
pub(crate) trait RoomPublish: Send + Sync {
    fn begin_response(&self, attempt: blake3::Hash, kind: RoomActorKind) -> bool;
    fn generation_observed(&self, attempt: blake3::Hash, kind: RoomActorKind) -> bool;
    fn accept_response(&self, stamp: RoomResponseStamp) -> bool;
    fn abandon_response(&self, attempt: blake3::Hash);
    fn arm(&self, stamp: RoomResponseStamp) -> bool;
    fn is_live(&self, stamp: RoomResponseStamp) -> bool;
    fn publish(
        &self, stamp: RoomResponseStamp, rename: &mut dyn FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<bool>;
}

/// RAM-only. Neither the answer fingerprint nor this authority is persisted or exported.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct RoomResponseStamp {
    pub(crate) attempt: blake3::Hash,
    answer: blake3::Hash,
}

impl RoomResponseStamp {
    fn new(attempt: blake3::Hash, answer: &str) -> Self {
        let mut hash = blake3::Hasher::new_derive_key("harw private Assistant completed response v1");
        hash.update(answer.as_bytes());
        Self { attempt, answer: hash.finalize() }
    }
}

/// Single owner of the already-authorized local model response; it conveys
/// no store, key, or new-turn rights. Drop before a complete response closes
/// the exact native slot and cancels any lingering model work.
pub(crate) struct RoomResponseOwner {
    gate: std::sync::Arc<dyn RoomPublish>,
    attempt: blake3::Hash,
    kind: RoomActorKind,
    passed: bool,
}

impl RoomResponseOwner {
    pub(crate) fn begin(
        gate: std::sync::Arc<dyn RoomPublish>, attempt: &RecordedRoomRun, kind: RoomActorKind,
    ) -> Option<Self> {
        gate.begin_response(attempt.attempt_frame, kind)
            .then(|| Self { gate, attempt: attempt.attempt_frame, kind, passed: false })
    }

    /// Payload-free, single-use observer for the dedicated private vision
    /// engine. It carries exact gate/attempt identity, not any vault capability.
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) fn photo_generation_observer(&self) -> impl FnOnce() + Send + 'static {
        let gate = self.gate.clone();
        let attempt = self.attempt;
        let is_photo = self.kind == RoomActorKind::Photo;
        move || { if is_photo { gate.generation_observed(attempt, RoomActorKind::Photo); } }
    }

    /// The pinned provider reports actual work via its first local text or
    /// reasoning delta. The sink retains no tokens; it passes only the exact
    /// attempt's readiness signal. Supplying no sink to Android still means
    /// no partial reply is ever exported.
    #[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
    pub(crate) fn text_start_sink(&self) -> harw_core::StreamSink {
        use harw_core::ModelStreamEvent;
        use std::sync::atomic::{AtomicBool, Ordering};
        use zeroize::Zeroize;

        let gate = self.gate.clone();
        let attempt = self.attempt;
        let seen = AtomicBool::new(false);
        let is_text = self.kind == RoomActorKind::Text;
        harw_core::StreamSink::new(move |event| {
            if let ModelStreamEvent::TextDelta(mut text) | ModelStreamEvent::ReasoningDelta(mut text) = event {
                if is_text && !text.is_empty() && !seen.swap(true, Ordering::AcqRel) {
                    gate.generation_observed(attempt, RoomActorKind::Text);
                }
                text.zeroize();
            }
        })
    }

    pub(crate) fn complete(mut self, answer: &str) -> Option<RoomResponseApproval> {
        let answer = validate_answer(answer).ok()?;
        let stamp = RoomResponseStamp::new(self.attempt, answer);
        if !self.gate.accept_response(stamp) { return None; }
        self.passed = true;
        Some(RoomResponseApproval { gate: self.gate.clone(), stamp })
    }
}

impl Drop for RoomResponseOwner {
    fn drop(&mut self) {
        if !self.passed { self.gate.abandon_response(self.attempt); }
    }
}

/// One non-cloneable completion from the admitted model owner. This does not
/// authorize sealing alone: it needs the foreground vault or the dedicated
/// restricted actor bound to the exact already-authenticated predecessor.
pub(crate) struct RoomResponseApproval {
    gate: std::sync::Arc<dyn RoomPublish>,
    stamp: RoomResponseStamp,
}

impl RoomResponseApproval {
    pub(crate) fn matches(&self, attempt: &RecordedRoomRun, answer: &str) -> bool {
        self.stamp == RoomResponseStamp::new(attempt.attempt_frame, answer)
    }

    pub(crate) fn arm(&self) -> bool { self.gate.arm(self.stamp) }
    pub(crate) fn is_live(&self) -> bool { self.gate.is_live(self.stamp) }

    pub(crate) fn publish(&self, rename: &mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<bool> {
        self.gate.publish(self.stamp, rename)
    }
}

impl Drop for RoomResponseApproval {
    fn drop(&mut self) { self.gate.abandon_response(self.stamp.attempt); }
}

/// Native-only witness for one admitted inference. No handle, PIN, signer,
/// recipient key, or persisted prompt hash. The immutable base is the same
/// already-authorized in-memory transcript that the worker uses to form context.
pub(crate) struct RecordedRoomRun {
    pub(crate) base: RoomLog,
    pub(crate) scope: [u8; 24],
    pub(crate) room_frame: blake3::Hash,
    pub(crate) attempt_frame: blake3::Hash,
    pub(crate) input: blake3::Hash,
}

pub(crate) fn room_input_fingerprint(user: &str, photo: Option<RoomPhotoIdentity>) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new_derive_key("harw private Assistant admitted turn input v1");
    hasher.update(&(user.len() as u64).to_be_bytes());
    hasher.update(user.as_bytes());
    if let Some(photo) = photo {
        hasher.update(&[1]);
        hasher.update(&photo.item());
        match photo.digest() {
            Some(digest) => { hasher.update(&[1]); hasher.update(&digest); }
            None => { hasher.update(&[0]); }
        }
    } else {
        hasher.update(&[0]);
    }
    hasher.finalize()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RoomFailure<E> {
    Vault(E),
    Conflict,
    WrongRoom,
    Invalid(InvalidRoom),
}

pub(crate) fn open_room<V: RoomVault>(
    vault: &V,
    id: &RoomId,
) -> Result<RoomLog, RoomFailure<V::Error>> {
    let check = |room: RoomLog| {
        if room.id() == id {
            Ok(room)
        } else {
            Err(RoomFailure::WrongRoom)
        }
    };
    if let Some(room) = vault.load(id).map_err(RoomFailure::Vault)? {
        return check(room);
    }
    let empty = RoomLog {
        id: id.clone(),
        completed_turns: Vec::new(),
    };
    if vault
        .compare_and_swap(id, None, &empty)
        .map_err(RoomFailure::Vault)?
    {
        return Ok(empty);
    }
    check(
        vault
            .load(id)
            .map_err(RoomFailure::Vault)?
            .ok_or(RoomFailure::Conflict)?,
    )
}

/// Persist a full user/assistant pair before calling it committed. Stale
/// writers cannot replace the log or accidentally rerun inference.
pub(crate) fn commit_turn<V: RoomVault>(
    vault: &V,
    current: &RoomLog,
    user: &str,
    answer: &str,
) -> Result<RoomLog, RoomFailure<V::Error>> {
    commit_turn_with_library_image(vault, current, user, answer, None)
}

pub(crate) fn commit_turn_with_library_image<V: RoomVault>(
    vault: &V, current: &RoomLog, user: &str, answer: &str, library_image: Option<RoomPhotoIdentity>,
) -> Result<RoomLog, RoomFailure<V::Error>> {
    let next = next_turn(current, user, answer, library_image).map_err(RoomFailure::Invalid)?;
    if vault.compare_and_swap(current.id(), Some(current.revision()), &next).map_err(RoomFailure::Vault)? {
        Ok(next)
    } else {
        Err(RoomFailure::Conflict)
    }
}

pub(crate) fn next_turn(
    current: &RoomLog, user: &str, answer: &str, library_image: Option<RoomPhotoIdentity>,
) -> Result<RoomLog, InvalidRoom> {
    let turn = CompletedTurn::with_library_image(user, answer, library_image)?;
    let existing_bytes: usize = current
        .completed_turns
        .iter()
        .map(CompletedTurn::bytes)
        .sum();
    if current.completed_turns.len() >= MAX_TURNS || existing_bytes + turn.bytes() > MAX_LOG_BYTES {
        return Err(InvalidRoom::Full);
    }
    let mut next = current.clone();
    next.completed_turns.push(turn);
    Ok(next)
}

/// Keep the stored transcript intact; only the prompt gets a recent complete
/// suffix. Never take half a pair or skip an oversized recent pair.
pub(crate) fn context_tail(turns: &[CompletedTurn], budget: usize) -> &[CompletedTurn] {
    let mut remaining = budget;
    let count = turns
        .iter()
        .rev()
        .take_while(|turn| {
            if let Some(next) = remaining.checked_sub(turn.bytes()) {
                remaining = next;
                true
            } else {
                false
            }
        })
        .count();
    &turns[turns.len() - count..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestVault(std::sync::Mutex<Option<RoomLog>>);

    impl RoomVault for TestVault {
        type Error = &'static str;

        fn load(&self, id: &RoomId) -> Result<Option<RoomLog>, Self::Error> {
            let data = self.0.lock().unwrap();
            Ok(data.as_ref().filter(|room| room.id() == id).cloned())
        }

        fn compare_and_swap(
            &self,
            id: &RoomId,
            expected_revision: Option<u64>,
            next: &RoomLog,
        ) -> Result<bool, Self::Error> {
            let mut data = self.0.lock().unwrap();
            if next.id() != id
                || data.as_ref().map(RoomLog::revision) != expected_revision
                || data.as_ref().is_some_and(|room| room.id() != id)
            {
                return Ok(false);
            }
            *data = Some(next.clone());
            Ok(true)
        }
    }

    #[test]
    fn room_id_is_a_distinct_typed_namespace() {
        assert!(RoomId::parse("session-123").is_err());
        assert!(RoomId::parse("uia-local-v1:../../x").is_err());
        assert!(RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn committed_history_survives_reopening_and_rejects_a_stale_writer() {
        let vault = TestVault::default();
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").unwrap();
        let room = open_room(&vault, &id).unwrap();
        let stale = room.clone();
        let committed = commit_turn(&vault, &room, "Erste Frage", "Antwort").unwrap();
        assert_eq!(committed.revision(), 1);
        let reopened = open_room(&vault, &id).unwrap();
        assert_eq!(reopened.completed_turns().len(), 1);
        assert_eq!(reopened.completed_turns()[0].user(), "Erste Frage");
        assert!(matches!(
            commit_turn(&vault, &stale, "Stale", "Data"),
            Err(RoomFailure::Conflict)
        ));
        assert!(open_room(&vault, &id).unwrap() == committed);
    }

    #[test]
    fn malformed_and_over_budget_turns_cannot_reach_the_store() {
        let vault = TestVault::default();
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").unwrap();
        let room = open_room(&vault, &id).unwrap();
        assert!(commit_turn(&vault, &room, " ", "Antwort").is_err());
        assert!(commit_turn(&vault, &room, "Frage", &"x".repeat(65_537)).is_err());
        assert!(open_room(&vault, &id).unwrap() == room);
    }

    #[test]
    fn a_locked_or_corrupt_vault_is_never_treated_like_a_missing_room() {
        struct Locked;
        impl RoomVault for Locked {
            type Error = &'static str;
            fn load(&self, _: &RoomId) -> Result<Option<RoomLog>, Self::Error> {
                Err("locked")
            }
            fn compare_and_swap(
                &self,
                _: &RoomId,
                _: Option<u64>,
                _: &RoomLog,
            ) -> Result<bool, Self::Error> {
                panic!("must not write a new room to a locked vault")
            }
        }
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").unwrap();
        assert!(matches!(
            open_room(&Locked, &id),
            Err(RoomFailure::Vault("locked"))
        ));
    }

    #[test]
    fn restoration_rejects_wrong_revision_and_oversize_payloads() {
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").unwrap();
        let turn = CompletedTurn::new("user", "answer").unwrap();
        assert_eq!(id.as_str(), "uia-local-v1:0123456789abcdef0123456789abcdef");
        assert!(RoomLog::restore(id.clone(), 2, vec![turn.clone()]).is_err());
        assert!(RoomLog::restore(id.clone(), 129, vec![turn.clone(); 129]).is_err());
        assert!(RoomLog::restore(id, 1, vec![turn]).is_ok());
    }

    #[test]
    fn prompt_context_is_recent_bounded_and_whole_turns_only() {
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").unwrap();
        let vault = TestVault::default();
        let room = open_room(&vault, &id).unwrap();
        let room = commit_turn(&vault, &room, "erste", &"x".repeat(8_000)).unwrap();
        let room = commit_turn(&vault, &room, "zweite", "zweite Antwort").unwrap();
        assert_eq!(context_tail(room.completed_turns(), 40).len(), 1);
        assert_eq!(context_tail(room.completed_turns(), 40)[0].user(), "zweite");
        assert!(context_tail(room.completed_turns(), 3).is_empty());
    }
}

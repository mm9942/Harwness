//! The local UIA inference boundary. The room is not a coding SessionHost:
//! only explicit room text enters the GGUF model; no tools or context providers.

use std::sync::Arc;

use harw_core::ConversationHistory;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
use harw_core::cancel::CancelToken;
use harw_core::model::{ModelProvider, ModelRequest, ModelResponse, StopReason};
use harw_extension_api::LoadedInstructions;

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
use crate::local::SwitchingModel;
use crate::uia_room_store::{RoomFailure, RoomLog, context_tail, validate_answer};
#[cfg(any(test, all(feature = "vault-uia-chat", target_os = "android")))]
use crate::uia_room_store::RoomId;
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
use crate::{
    uia_room_handoff::{AuthenticatedRoomLease, RoomHandoffError},
    uia_room_store::{RecordedRoomRun, RoomActorKind, RoomPublish, RoomResponseOwner, open_room,
        room_input_fingerprint, validate_user_message},
    uia_room_vault::RoomCompletionMailbox,
};

const ROOM_SYSTEM: &str = "Du bist Harws persönlicher Assistent auf diesem Gerät. \
    Antworte in der Sprache der Nutzerin. Du führst keine Befehle aus, liest keine Dateien \
    und kontaktierst weder Server noch andere Agenten. Behaupte keine Aktionen, die du nicht \
    ausgeführt hast. Nutze nur die bereitgestellte Unterhaltung als Kontext. \
    Bilder aus älteren Nachrichten siehst du nicht; erfinde keine weiteren sichtbaren Details.";
const HISTORY_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UiaRoomError<E> {
    Room(RoomFailure<E>),
    Unavailable,
    Inference,
    Persistence,
    IncompleteAnswer,
    Busy,
    Cancelled,
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
static LOCAL_ROOM_INFERENCE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Invoke only with a local authenticated vault adapter. No text is sent to a
/// general chat host, synchronised, or copied into a plaintext fallback.
/// Return only after the user/assistant pair has committed to the vault.
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) async fn reply_and_commit(
    model: &SwitchingModel,
    vault: Arc<AuthenticatedRoomLease>,
    room_id: &RoomId,
    expected_revision: u64,
    user: &str,
    cancel: CancelToken,
    publish: Arc<dyn RoomPublish>,
    same_profile: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), UiaRoomError<RoomHandoffError>> {
    let user = validate_user_message(user)
        .map_err(|error| UiaRoomError::Room(RoomFailure::Invalid(error)))?;
    if cancel.is_cancelled() || !same_profile() {
        return Err(UiaRoomError::Cancelled);
    }
    let _slot = LOCAL_ROOM_INFERENCE
        .try_acquire()
        .map_err(|_| UiaRoomError::Busy)?;
    let current = tokio::task::spawn_blocking({
        let vault = Arc::clone(&vault);
        let room_id = room_id.clone();
        move || open_room(vault.as_ref(), &room_id)
    })
    .await
    .map_err(|_| UiaRoomError::Persistence)?
    .map_err(UiaRoomError::Room)?;
    if cancel.is_cancelled() || !same_profile() {
        return Err(UiaRoomError::Cancelled);
    }
    if current.revision() != expected_revision {
        return Err(UiaRoomError::Room(RoomFailure::Conflict));
    }
    current
        .admit_user(user)
        .map_err(|error| UiaRoomError::Room(RoomFailure::Invalid(error)))?;
    let foreground = Arc::downgrade(&vault);
    drop(vault);
    let pinned = model
        .pin_local_assistant_model()
        .await
        .map_err(|_| UiaRoomError::Unavailable)?;
    if cancel.is_cancelled() || !same_profile() { return Err(UiaRoomError::Cancelled); }
    let attempt = record_attempt(
        foreground.upgrade().ok_or(UiaRoomError::Cancelled)?,
        current, room_input_fingerprint(user, None), cancel.clone(),
    ).await?;
    let request = build_request(&attempt.base, user).with_cancel_token(cancel.clone());
    let vault = foreground.upgrade().ok_or(UiaRoomError::Cancelled)?;
    let owner = RoomResponseOwner::begin(publish, &attempt, RoomActorKind::Text)
        .ok_or(UiaRoomError::Cancelled)?;
    let mailbox = vault.start_text_completion(attempt, user, same_profile.clone(), cancel.clone())
        .map_err(|error| UiaRoomError::Room(RoomFailure::Vault(error)))?;
    drop(vault);
    // The real local provider can wait for its inference slot even after
    // pinning. Do not park that queue: until the first real generation
    // delta, hide takes the ordinary hard-lock path.
    let request = request.with_stream_sink(owner.text_start_sink());
    let evaluated = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(UiaRoomError::Cancelled),
        result = pinned.respond(request) => match result {
            Ok(response) if !cancel.is_cancelled() && same_profile() => validate_response(&response)
                .map(|answer| zeroize::Zeroizing::new(answer.to_owned()))
                .ok_or(UiaRoomError::IncompleteAnswer),
            Ok(_) => Err(UiaRoomError::Cancelled),
            Err(_) => Err(UiaRoomError::Inference),
        },
    };
    let (answer, approval) = match evaluated.and_then(|answer| {
        // Native approval can seal a complete foreground answer without
        // turning it into evidence of active generation for park.
        let approval = owner.complete(&answer).ok_or(UiaRoomError::Cancelled)?;
        Ok((answer, approval))
    }) {
        Ok(value) => value,
        Err(error) => {
            // The key worker must be gone before releasing the parked
            // owner lock; cancelled model computations have no vault key.
            mailbox.abandon().await;
            return Err(error);
        }
    };
    finish_reply(mailbox, answer, approval, &|| !cancel.is_cancelled() && same_profile()).await
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) enum PrivateVisionError { Model, Busy, Inference, Incomplete, Cancelled }

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) struct PrivateRoomPhoto {
    pub(crate) identity: crate::uia_room_store::RoomPhotoIdentity,
    pub(crate) jpeg: zeroize::Zeroizing<Vec<u8>>,
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
struct PhotoBytes(zeroize::Zeroizing<Vec<u8>>);

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
impl AsRef<[u8]> for PhotoBytes {
    fn as_ref(&self) -> &[u8] { self.0.as_slice() }
}

/// Explicitly attach exactly this authorized photo to the current user turn.
/// Past image references are never silently rehydrated into a later request.
#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
pub(crate) async fn reply_with_photo_and_commit(
    model: &SwitchingModel,
    vault: Arc<AuthenticatedRoomLease>,
    room_id: &RoomId,
    expected_revision: u64,
    user: &str,
    photo: PrivateRoomPhoto,
    cancel: CancelToken,
    publish: Arc<dyn RoomPublish>,
    foreground_admission: impl Fn() -> bool,
    same_profile: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<(), UiaRoomError<RoomHandoffError>> {
    let user = validate_user_message(user)
        .map_err(|error| UiaRoomError::Room(RoomFailure::Invalid(error)))?;
    if cancel.is_cancelled() || !foreground_admission() { return Err(UiaRoomError::Cancelled); }
    let _slot = LOCAL_ROOM_INFERENCE.try_acquire().map_err(|_| UiaRoomError::Busy)?;
    let current = tokio::task::spawn_blocking({
        let vault = Arc::clone(&vault);
        let room_id = room_id.clone();
        move || open_room(vault.as_ref(), &room_id)
    }).await.map_err(|_| UiaRoomError::Persistence)?.map_err(UiaRoomError::Room)?;
    if cancel.is_cancelled() || !foreground_admission() { return Err(UiaRoomError::Cancelled); }
    if current.revision() != expected_revision {
        return Err(UiaRoomError::Room(RoomFailure::Conflict));
    }
    current.admit_user(user).map_err(|error| UiaRoomError::Room(RoomFailure::Invalid(error)))?;
    let foreground = Arc::downgrade(&vault);
    drop(vault);
    let pinned = model.pin_private_assistant_vision(&cancel).await.map_err(private_vision_error)?;
    if cancel.is_cancelled() || !foreground_admission() { return Err(UiaRoomError::Cancelled); }
    if !pinned.is_current() { return Err(UiaRoomError::Unavailable); }
    let attempt = record_attempt(
        foreground.upgrade().ok_or(UiaRoomError::Cancelled)?,
        current, room_input_fingerprint(user, Some(photo.identity)), cancel.clone(),
    ).await?;
    if !foreground_admission() { return Err(UiaRoomError::Cancelled); }
    let owner = RoomResponseOwner::begin(publish, &attempt, RoomActorKind::Photo)
        .ok_or(UiaRoomError::Cancelled)?;
    let request = build_request(&attempt.base, user).with_cancel_token(cancel.clone());
    let vault = foreground.upgrade().ok_or(UiaRoomError::Cancelled)?;
    let mailbox = vault.start_photo_completion(attempt, user, photo.identity, same_profile.clone(), cancel.clone())
        .map_err(|error| UiaRoomError::Room(RoomFailure::Vault(error)))?;
    drop(vault);
    drop(foreground_admission);

    // The inference side owns only the already-read, scrubbed bytes and
    // their saved identity. The sealer receives no bytes or Library reader.
    let response = pinned
        .respond(request, PhotoBytes(photo.jpeg), owner.photo_generation_observer())
        .await.map_err(private_vision_error);
    let evaluated = response.and_then(|response| {
        validate_response(&response)
            .map(|answer| zeroize::Zeroizing::new(answer.to_owned()))
            .ok_or(UiaRoomError::IncompleteAnswer)
    });
    // A successful park revokes foreground admission by design. The same
    // exact cancel/publication gate and selected local profile still bind
    // this already accepted turn; there is no Library reopen or read.
    let context_current = || !cancel.is_cancelled() && same_profile();
    let (answer, approval) = match evaluated.and_then(|answer| {
        if !context_current() { return Err(UiaRoomError::Cancelled); }
        let approval = owner.complete(&answer).ok_or(UiaRoomError::Cancelled)?;
        Ok((answer, approval))
    }) {
        Ok(value) => value,
        Err(error) => {
            mailbox.abandon().await;
            return Err(error);
        }
    };
    finish_reply(mailbox, answer, approval, &context_current).await
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
fn private_vision_error<E>(error: PrivateVisionError) -> UiaRoomError<E> {
    match error {
        PrivateVisionError::Model => UiaRoomError::Unavailable,
        PrivateVisionError::Busy => UiaRoomError::Busy,
        PrivateVisionError::Inference => UiaRoomError::Inference,
        PrivateVisionError::Incomplete => UiaRoomError::IncompleteAnswer,
        PrivateVisionError::Cancelled => UiaRoomError::Cancelled,
    }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
async fn finish_reply(
    mailbox: RoomCompletionMailbox, answer: zeroize::Zeroizing<String>,
    approval: crate::uia_room_store::RoomResponseApproval, current: &(dyn Fn() -> bool + Sync),
) -> Result<(), UiaRoomError<RoomHandoffError>> {
    match mailbox.finish(answer, approval).await {
        Ok(true) => Ok(()),
        Ok(false) if !current() => Err(UiaRoomError::Cancelled),
        Ok(false) => Err(UiaRoomError::Room(RoomFailure::Conflict)),
        Err(error) => Err(UiaRoomError::Room(RoomFailure::Vault(RoomHandoffError::Vault(error)))),
    }
}

#[cfg(all(feature = "vault-uia-chat", target_os = "android"))]
async fn record_attempt(
    vault: Arc<AuthenticatedRoomLease>, current: RoomLog, input: blake3::Hash, cancel: CancelToken,
) -> Result<RecordedRoomRun, UiaRoomError<RoomHandoffError>> {
    tokio::task::spawn_blocking(move || {
        if cancel.is_cancelled() { return Err(UiaRoomError::Cancelled); }
        let attempt = vault.record_run(current, input).map_err(|error| UiaRoomError::Room(RoomFailure::Vault(error)))?
            .ok_or(UiaRoomError::Room(RoomFailure::Conflict))?;
        if cancel.is_cancelled() { return Err(UiaRoomError::Cancelled); }
        Ok(attempt)
    }).await.map_err(|_| UiaRoomError::Persistence)?
}

fn build_request(room: &RoomLog, user: &str) -> ModelRequest {
    let mut history = ConversationHistory::new();
    for turn in context_tail(room.completed_turns(), HISTORY_BYTES) {
        if turn.library_image().is_some() {
            history.push_user_text(format!("{}\n[Früheres Bild nicht erneut bereitgestellt]", turn.user()));
        } else {
            history.push_user_text(turn.user());
        }
        history.push_assistant_text(turn.answer(), None);
    }
    history.push_user_text(user);
    ModelRequest::new(
        LoadedInstructions {
            system_prompt: ROOM_SYSTEM.to_owned(),
            fragments: Vec::new(),
        },
        Vec::new(),
        history,
        Vec::new(),
    )
    .with_max_output_tokens(Some(MAX_OUTPUT_TOKENS))
}

fn validate_response(response: &ModelResponse) -> Option<&str> {
    if !response.tool_calls.is_empty()
        || !matches!(
            response.stop,
            StopReason::EndTurn | StopReason::StopSequence
        )
    {
        return None;
    }
    validate_answer(response.message.as_deref()?).ok()
}

/// Match both immutable backend and model pins. Never feed room text to a
/// SwitchingModel or to the generic HTTP route, even on a selection race.
pub(crate) fn is_pinned_local(provider: &Arc<dyn ModelProvider>, expected_model_id: &str) -> bool {
    provider.pinned_provider_id().as_deref() == Some(harw_llama::LOCAL_PROVIDER_ID)
        && provider.pinned_model_id().as_deref() == Some(expected_model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_has_no_authority_or_external_context() {
        let id = RoomId::parse("uia-local-v1:0123456789abcdef0123456789abcdef").expect("room id");
        let turn = crate::uia_room_store::CompletedTurn::new("Erste Frage", "Erste Antwort")
            .expect("turn");
        let room = RoomLog::restore(id, 1, vec![turn]).expect("restore");
        let request = build_request(&room, "Nächste Frage");
        assert!(request.tools.is_empty());
        assert!(request.context.is_empty());
        assert!(request.instruction_fragments.is_empty());
        assert!(request.identity.is_none());
        assert!(request.data_block.is_none());
        assert_eq!(request.history.items().len(), 3);
        assert_eq!(request.history.last_user_text(), Some("Nächste Frage"));
        assert_eq!(request.max_output_tokens, Some(MAX_OUTPUT_TOKENS));
    }

    #[test]
    fn only_complete_visible_text_can_be_committed() {
        let mut response = ModelResponse::text("Hallo!");
        assert_eq!(validate_response(&response), Some("Hallo!"));
        response.stop = StopReason::MaxTokens;
        assert_eq!(validate_response(&response), None);
        response.stop = StopReason::ToolUse;
        assert_eq!(validate_response(&response), None);
        response.stop = StopReason::EndTurn;
        response.message = Some(" ".into());
        assert_eq!(validate_response(&response), None);
    }
}

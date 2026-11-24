//! Explicit native-only captioning of exactly one sealed Library item.
//! No Java image material, provider lookup, model load, chat turn, or save here.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use harw_llama::LocalImageDescriptionOutcome;
use harw_protocol::ImageFormat;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    LocalVisionChoice,
    private_library::{LibraryError, LibraryItemId},
    private_library_owner::PrivateLibraryOwner,
};

pub(crate) const MODEL_ID_MAX: usize = 384;
pub(crate) const MAX_CAPTION: usize = 4096;

pub(crate) enum CaptionError {
    Vault(LibraryError),
    MissingModel,
    Incomplete,
    InvalidPhoto,
}

pub(crate) fn loaded_choice(files: &Path) -> Result<LocalVisionChoice, CaptionError> {
    let model = crate::local::uia_runtime_for(files).ok_or(CaptionError::MissingModel)?;
    let loaded = model.local_vision_choices().map_err(|_| CaptionError::MissingModel)?;
    match loaded.as_slice() {
        [only] if !only.model_id.is_empty() && only.model_id.len() <= MODEL_ID_MAX => Ok(only.clone()),
        _ => Err(CaptionError::MissingModel),
    }
}

pub(crate) fn run(
    owner: &PrivateLibraryOwner,
    files: &Path,
    revision: u64,
    id: LibraryItemId,
    pinned_model_id: &str,
    pinned_model_revision: u64,
) -> Result<Zeroizing<String>, CaptionError> {
    let current = loaded_choice(files)?;
    if current.model_id != pinned_model_id || current.loaded_revision != pinned_model_revision {
        return Err(CaptionError::MissingModel);
    }
    let model = crate::local::uia_runtime_for(files).ok_or(CaptionError::MissingModel)?;
    let caption_job = owner.begin_caption().map_err(CaptionError::Vault)?;
    let store = owner.open_existing().map_err(CaptionError::Vault)?
        .ok_or(CaptionError::Vault(LibraryError::NotFound))?;

    let jpeg = store.jpeg(revision, id).map_err(CaptionError::Vault)?;
    let media = harw_media::inspect(&jpeg).map_err(|_| CaptionError::InvalidPhoto)?;
    if media.format != ImageFormat::Jpeg || media.width > 1280 || media.height > 1280 {
        return Err(CaptionError::InvalidPhoto);
    }
    let clean = Zeroizing::new(
        harw_media::sanitize(&jpeg, media.format).map_err(|_| CaptionError::InvalidPhoto)?,
    );
    let edited: Arc<[u8]> = Arc::from(clean.as_slice());
    drop(clean);
    drop(jpeg);
    if caption_job.token().is_cancelled() { return Err(CaptionError::Vault(LibraryError::Locked)); }

    let output = Mutex::new(None);
    let consume = |result: harw_llama::LocalImageDescription| match result.outcome {
        LocalImageDescriptionOutcome::Caption { text } => {
            let text = Zeroizing::new(text);
            if result.id == 1 && !caption_job.token().is_cancelled()
                && let Ok(mut slot) = output.lock()
            {
                *slot = Some(text);
            }
        }
        LocalImageDescriptionOutcome::OutputLimit { mut text }
        | LocalImageDescriptionOutcome::ContextLimit { mut text } => text.zeroize(),
        LocalImageDescriptionOutcome::InvalidInput | LocalImageDescriptionOutcome::Failed => {}
    };
    crate::local_vision::supervisor_runtime().map_err(|_| CaptionError::MissingModel)?
        .block_on(model.describe_pinned_local_vision(
            &current, vec![(1, edited)], caption_job.token(), &consume,
        ))
        .map_err(|_| CaptionError::Incomplete)?;

    if !owner.active() || caption_job.token().is_cancelled() {
        return Err(CaptionError::Vault(LibraryError::Locked));
    }
    let caption = output.into_inner().map_err(|_| CaptionError::Incomplete)?
        .ok_or(CaptionError::Incomplete)?;
    let trimmed = caption.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CAPTION ||
        trimmed.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(CaptionError::Incomplete);
    }
    let latest = store.snapshot().map_err(CaptionError::Vault)?;
    if latest.revision != revision || !latest.items.iter().any(|entry| entry.id == id) {
        return Err(CaptionError::Vault(LibraryError::Conflict));
    }
    Ok(Zeroizing::new(trimmed.to_owned()))
}

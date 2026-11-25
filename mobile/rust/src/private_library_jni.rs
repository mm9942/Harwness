//! Scoped worker-only local Library transport; separate from chat/model operations.
#![deny(unsafe_op_in_unsafe_fn)]

use jni::{
    Env, EnvUnowned, Outcome, errors, jni_sig, jni_str,
    objects::{JByteBuffer, JObject, JString},
};
use std::{path::PathBuf, sync::Mutex};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    private_library::{LibraryError, LibraryItemId, PrivateLibraryStore},
    private_library_owner::PrivateLibraryOwner,
    private_library_caption::{self, CaptionError, MODEL_ID_MAX},
    uia_room_jni::{UIA_ROOM_REGISTRY, vault_root},
    uia_room_registry::{ChatLease, SelectedScope},
    uia_room_store::open_room,
};

const FRAME_BYTES: usize = 288 * 1024;
const LOCKED: i64 = -1;
const CHANGED: i64 = -2;
const FULL: i64 = -3;
const INVALID: i64 = -4;
const INTERRUPTED: i64 = -5;
const UNAVAILABLE: i64 = -6;
const BUSY: i64 = -7;
const NEW: i64 = -8;
const NO_MODEL: i64 = -9;
const INCOMPLETE: i64 = -10;
const DESCRIPTION_MISSING: i64 = -11;
const ROOM_PHOTO_MISSING: i64 = -12;
const STATUS: i32 = 0;
const PROVISION: i32 = 1;
const SNAPSHOT: i32 = 2;
const PREVIEW: i32 = 3;
const IMPORT: i32 = 4;
const RENAME: i32 = 5;
const DESCRIBE: i32 = 6;
const REMOVE: i32 = 7;
const CAPTION_CHOICE: i32 = 8;
const CAPTION: i32 = 9;
const SAVED_DESCRIPTION: i32 = 10;
const ROOM_PHOTO_PREVIEW: i32 = 11;
const VISION_MODELS: i32 = 12;
const MAX_NAME: usize = 160;
const MAX_DESCRIPTION: usize = 4096;
const MAX_JPEG: usize = 180_000;
static BORROW: Mutex<()> = Mutex::new(());

/// Only the installed, verified Application can identify the actual current
/// native profile. Never accept a path or caller-selected profile through JNI.
pub(crate) fn profile(env: &mut Env<'_>, app: &JObject<'_>) -> errors::Result<Option<[u8; 32]>> {
    Ok(profile_files(env, app)?.map(|(_, profile)| profile))
}

fn profile_files(env: &mut Env<'_>, app: &JObject<'_>) -> errors::Result<Option<(PathBuf, [u8; 32])>> {
    if vault_root(env, app)?.is_none() { return Ok(None); }
    let dir = env.call_method(app, jni_str!("getFilesDir"), jni_sig!("()Ljava/io/File;"), &[])?.l()?;
    let path = env.call_method(&dir, jni_str!("getAbsolutePath"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
    let path = PathBuf::from(env.cast_local::<JString>(path)?.try_to_string(env)?);
    let Some(opaque) = crate::local::uia_profile_key(&path) else { return Ok(None); };
    let mut exact = [0u8; 32];
    if hex::decode_to_slice(opaque, &mut exact).is_err() || exact == [0; 32] {
        return Ok(None);
    }
    Ok(Some((path, exact)))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_libraryNative<'l>(
    mut caller: EnvUnowned<'l>,
    _instance: JObject<'l>,
    app: JObject<'l>,
    token: i64,
    most: i64,
    least: i64,
    generation: i64,
    operation: i32,
    size: i32,
    direct: JByteBuffer<'l>,
) -> i64 {
    let result = caller.with_env(|env| -> errors::Result<i64> {
        let result = (|| -> errors::Result<i64> {
            let Some(scope) = SelectedScope::new(most, least, generation) else { return Ok(LOCKED); };
            let Some((files, before)) = profile_files(env, &app)? else { return Ok(LOCKED); };
            let Some(owner) = UIA_ROOM_REGISTRY.library(token, scope, before) else { return Ok(LOCKED); };
            if env.get_direct_buffer_capacity(&direct).ok() != Some(FRAME_BYTES) ||
                env.call_method(&direct, jni_str!("isReadOnly"), jni_sig!(() -> boolean), &[])?.z()?
            {
                return Ok(INVALID);
            }
            let Ok(input_len) = usize::try_from(size) else { return Ok(INVALID); };
            if input_len > FRAME_BYTES { return Ok(INVALID); }
            let ptr = env.get_direct_buffer_address(&direct)?;
            let result = if operation == CAPTION {
                let selection = {
                    let Ok(_borrow) = BORROW.try_lock() else { return Ok(BUSY); };
                    // SAFETY: an exact-size, trusted fresh writable direct frame,
                    // exclusively borrowed here without Java calls. No borrow
                    // survives the block (and none spans model execution).
                    let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
                    let selection = caption_input(&frame[..input_len]);
                    frame.zeroize();
                    selection
                };
                selection.and_then(|(revision, item, pin, model)| {
                    let completed = private_library_caption::run(&owner, &files, revision, item, &model, pin)
                        .map_err(caption_status)?;
                    Ok(Zeroizing::new(completed.as_bytes().to_vec()))
                })
            } else if operation == ROOM_PHOTO_PREVIEW {
                let selection = {
                    let Ok(_borrow) = BORROW.try_lock() else { return Ok(BUSY); };
                    // SAFETY: bounded writable frame, exclusively borrowed;
                    // no borrow or Java operation survives parsing.
                    let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
                    let parsed = room_photo_input(&frame[..input_len]);
                    frame.zeroize();
                    parsed
                };
                selection.and_then(|(revision, ordinal)| {
                    let chat = UIA_ROOM_REGISTRY.chat(token, scope).ok_or(LOCKED)?;
                    room_photo_preview(&owner, &chat, revision, ordinal)
                })
            } else {
                let Ok(_borrow) = BORROW.try_lock() else { return Ok(BUSY); };
                // SAFETY: same exact-size, trusted fresh writable frame, no Java calls.
                let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
                let result = if operation == CAPTION_CHOICE && input_len == 0 {
                    vision_choice(&owner, &files)
                } else if operation == VISION_MODELS && input_len == 0 {
                    vision_models(&owner, &files)
                } else {
                    dispatch(&owner, operation, &frame[..input_len])
                };
                frame.zeroize();
                result
            };
            if profile(env, &app)? != Some(before) || !owner.active() ||
                UIA_ROOM_REGISTRY.library(token, scope, before).is_none()
            {
                return Ok(if matches!(operation, PROVISION | IMPORT | RENAME | DESCRIBE | REMOVE) {
                    INTERRUPTED
                } else { LOCKED });
            }
            match result {
                Ok(result) if result.is_empty() => Ok(0),
                Ok(result) if result.len() <= FRAME_BYTES => {
                    let Ok(_borrow) = BORROW.try_lock() else { return Ok(BUSY); };
                    if !owner.active() {
                        return Ok(if matches!(operation, PROVISION | IMPORT | RENAME | DESCRIBE | REMOVE) {
                            INTERRUPTED
                        } else { LOCKED });
                    }
                    // SAFETY: same pinned direct buffer, no Java callback in this borrow.
                    let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
                    frame[..result.len()].copy_from_slice(&result);
                    Ok(result.len() as i64)
                }
                Ok(_) => Ok(UNAVAILABLE),
                Err(error) => Ok(error),
            }
        })();
        if result.is_err() {
            env.exception_clear();
            return Ok(UNAVAILABLE);
        }
        result
    });
    match result.into_outcome() {
        Outcome::Ok(code) => code,
        Outcome::Err(_) | Outcome::Panic(_) => UNAVAILABLE,
    }
}

fn room_photo_input(mut input: &[u8]) -> Result<(u64, usize), i64> {
    let revision = u64::from_be_bytes(take(&mut input, 8)?.try_into().map_err(|_| INVALID)?);
    let ordinal = u16::from_be_bytes(take(&mut input, 2)?.try_into().map_err(|_| INVALID)?);
    if !input.is_empty() || !(1..=128).contains(&revision) || ordinal == 0 || u64::from(ordinal) > revision {
        return Err(INVALID);
    }
    Ok((revision, usize::from(ordinal - 1)))
}

fn room_photo_preview(
    owner: &PrivateLibraryOwner, chat: &ChatLease, revision: u64, index: usize,
) -> Result<Zeroizing<Vec<u8>>, i64> {
    let exact_room = || {
        if !chat.ops.active() || !owner.active() { return Err(LOCKED); }
        let room = open_room(chat.owner.lease().as_ref(), chat.owner.room_id())
            .map_err(|_| if chat.ops.active() { UNAVAILABLE } else { LOCKED })?;
        if room.revision() != revision { return Err(CHANGED); }
        Ok(room)
    };
    let room = exact_room()?;
    let identity = room.completed_turns().get(index)
        .and_then(|turn| turn.library_image()).ok_or(ROOM_PHOTO_MISSING)?;
    // The caller never supplies a Library ID, digest, alternate vault, or path.
    let library = owner.open_existing().map_err(status)?.ok_or(ROOM_PHOTO_MISSING)?;
    let jpeg = library.jpeg_for_room_photo(identity).map_err(|error| match error {
        LibraryError::NotFound | LibraryError::Conflict => ROOM_PHOTO_MISSING,
        other => status(other),
    })?;
    let image = harw_media::inspect(&jpeg).map_err(|_| INVALID)?;
    if image.format != harw_protocol::ImageFormat::Jpeg || image.width > 1280 || image.height > 1280 {
        return Err(INVALID);
    }
    let clean = Zeroizing::new(harw_media::sanitize(&jpeg, image.format).map_err(|_| INVALID)?);
    let after = exact_room()?;
    if after.completed_turns().get(index).and_then(|turn| turn.library_image()) != Some(identity) {
        return Err(CHANGED);
    }
    Ok(clean)
}

fn caption_input(mut input: &[u8]) -> Result<(u64, LibraryItemId, u64, String), i64> {
    let catalogue = u64::from_be_bytes(take(&mut input, 8)?.try_into().map_err(|_| INVALID)?);
    let item = LibraryItemId::parse(take(&mut input, 16)?.try_into().map_err(|_| INVALID)?)
        .map_err(status)?;
    let revision = u64::from_be_bytes(take(&mut input, 8)?.try_into().map_err(|_| INVALID)?);
    let model = text(&mut input, MODEL_ID_MAX)?;
    if model.is_empty() || !input.is_empty() { return Err(INVALID); }
    Ok((catalogue, item, revision, model.to_owned()))
}

fn vision_choice(
    owner: &PrivateLibraryOwner,
    files: &std::path::Path,
) -> Result<Zeroizing<Vec<u8>>, i64> {
    owner.open_existing().map_err(status)?.ok_or(NEW)?;
    let model = private_library_caption::loaded_choice(files).map_err(caption_status)?;
    let mut out = Zeroizing::new(Vec::with_capacity(8 + 4 + MODEL_ID_MAX));
    out.extend_from_slice(&model.loaded_revision.to_be_bytes());
    output_field(&mut out, model.model_id.as_bytes())?;
    Ok(out)
}

fn vision_models(
    owner: &PrivateLibraryOwner,
    files: &std::path::Path,
) -> Result<Zeroizing<Vec<u8>>, i64> {
    owner.open_existing().map_err(status)?.ok_or(NEW)?;
    let runtime = crate::local::uia_runtime_for(files).ok_or(NO_MODEL)?;
    let models = runtime.private_library_vision_models().map_err(|_| NO_MODEL)?;
    let mut out = Zeroizing::new(Vec::new());
    let ids: Vec<_> = models.iter().filter(|id| !id.is_empty() && id.len() <= MODEL_ID_MAX)
        .take(256).collect();
    out.extend_from_slice(&(ids.len() as u16).to_be_bytes());
    for id in ids { output_field(&mut out, id.as_bytes())?; }
    Ok(out)
}

fn caption_status(error: CaptionError) -> i64 {
    match error {
        CaptionError::Vault(vault) => status(vault),
        CaptionError::MissingModel => NO_MODEL,
        CaptionError::Incomplete => INCOMPLETE,
        CaptionError::InvalidPhoto => INVALID,
    }
}

fn status(error: LibraryError) -> i64 {
    match error {
        LibraryError::Locked | LibraryError::BadScope => LOCKED,
        LibraryError::Busy => BUSY,
        LibraryError::NotFound | LibraryError::Conflict => CHANGED,
        LibraryError::Full => FULL,
        LibraryError::InvalidInput => INVALID,
        LibraryError::UncertainCommit => INTERRUPTED,
        _ => UNAVAILABLE,
    }
}

fn dispatch(owner: &PrivateLibraryOwner, op: i32, mut input: &[u8]) -> Result<Zeroizing<Vec<u8>>, i64> {
    let empty = || Ok(Zeroizing::new(Vec::new()));
    if op == STATUS || op == PROVISION {
        if !input.is_empty() { return Err(INVALID); }
        if op == PROVISION {
            owner.provision().map_err(status)?;
        } else {
            owner.open_existing().map_err(status)?.ok_or(NEW)?;
        }
        return empty();
    }
    let store = owner.open_existing().map_err(status)?.ok_or(NEW)?;
    if op == SNAPSHOT {
        if !input.is_empty() { return Err(INVALID); }
        return snapshot(&store);
    }
    let revision = u64::from_be_bytes(take(&mut input, 8)?.try_into().map_err(|_| INVALID)?);
    if op == IMPORT {
        let name = text(&mut input, MAX_NAME)?;
        let image = field(&mut input, MAX_JPEG)?;
        if !input.is_empty() { return Err(INVALID); }
        store.insert(revision, name, Zeroizing::new(image.to_vec())).map_err(status)?;
        return empty();
    }
    let id = LibraryItemId::parse(take(&mut input, 16)?.try_into().map_err(|_| INVALID)?).map_err(status)?;
    match op {
        PREVIEW if input.is_empty() => return store.jpeg(revision, id).map_err(status),
        SAVED_DESCRIPTION if input.is_empty() => {
            let saved = store.saved_description(revision, id).map_err(status)?
                .ok_or(DESCRIPTION_MISSING)?;
            return Ok(Zeroizing::new(saved.as_bytes().to_vec()));
        }
        RENAME => {
            let name = text(&mut input, MAX_NAME)?;
            if !input.is_empty() { return Err(INVALID); }
            store.rename(revision, id, name).map_err(status)?;
        }
        DESCRIBE => {
            let description = text(&mut input, MAX_DESCRIPTION)?;
            if !input.is_empty() { return Err(INVALID); }
            store.describe(revision, id, (!description.is_empty()).then_some(description)).map_err(status)?;
        }
        REMOVE if input.is_empty() => store.remove(revision, id).map_err(status)?,
        _ => return Err(INVALID),
    }
    empty()
}

fn snapshot(store: &PrivateLibraryStore) -> Result<Zeroizing<Vec<u8>>, i64> {
    let actual = store.snapshot().map_err(status)?;
    // Exact fixed capacity avoids freed copies of old plaintext allocations.
    let mut out = Zeroizing::new(Vec::with_capacity(FRAME_BYTES));
    out.extend_from_slice(&actual.revision.to_be_bytes());
    let count = u16::try_from(actual.items.len()).map_err(|_| UNAVAILABLE)?;
    out.extend_from_slice(&count.to_be_bytes());
    for item in actual.items {
        out.extend_from_slice(&item.id.bytes());
        output_field(&mut out, item.name.as_bytes())?;
        output_field(&mut out, item.description.as_ref().map_or(&[][..], |text| text.as_bytes()))?;
    }
    Ok(out)
}

fn output_field(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), i64> {
    if out.len().saturating_add(4).saturating_add(bytes.len()) > FRAME_BYTES { return Err(UNAVAILABLE); }
    let size = u32::try_from(bytes.len()).map_err(|_| UNAVAILABLE)?;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn take<'a>(input: &mut &'a [u8], size: usize) -> Result<&'a [u8], i64> {
    let (part, rest) = input.split_at_checked(size).ok_or(INVALID)?;
    *input = rest;
    Ok(part)
}

fn field<'a>(input: &mut &'a [u8], max: usize) -> Result<&'a [u8], i64> {
    let size = u32::from_be_bytes(take(input, 4)?.try_into().map_err(|_| INVALID)?) as usize;
    if size > max { return Err(INVALID); }
    take(input, size)
}

fn text<'a>(input: &mut &'a [u8], max: usize) -> Result<&'a str, i64> {
    std::str::from_utf8(field(input, max)?).map_err(|_| INVALID)
}

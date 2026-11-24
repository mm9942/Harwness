//! Private worker-only room transport from an already registered native lease.
use jni::{
    Env, EnvUnowned, Outcome, errors, jni_sig, jni_str,
    objects::{JByteBuffer, JObject, JString},
};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    uia_room::{self, UiaRoomError},
    uia_room_handoff::RoomHandoffError,
    uia_room_jni::{UIA_ROOM_REGISTRY, vault_root},
    uia_room_operations::Admission,
    uia_room_registry::{ChatLease, SelectedScope},
    uia_room_store::{RoomFailure, open_room},
    uia_room_vault::VaultError,
};

const LOCKED: i64 = -1;
const VAULT: i64 = -2;
const BUSY: i64 = -3;
const MODEL: i64 = -4;
const CANCELLED: i64 = -5;
const INCOMPLETE: i64 = -6;
const INFERENCE: i64 = -7;
const RECONCILE: i64 = -8;
const INVALID: i64 = -9;
const PHOTO_MODEL: i64 = -10;
const PHOTO_CHANGED: i64 = -11;
const PHOTO_INVALID: i64 = -12;
const INPUT: usize = 8192 + 4;
const PHOTO_INPUT: usize = INPUT + 8 + 16;
const OUTPUT: usize = 256 * 1024 + 9 * 128 + 56;
const ATTEMPT_INTERRUPTED: u8 = 0xA1;
const ATTEMPT_STATUS_UNKNOWN: u8 = 0xA2;
static FRAME: Mutex<()> = Mutex::new(());
static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();

fn status(a: Admission) -> i64 {
    match a {
        Admission::Locked => LOCKED,
        Admission::Busy => BUSY,
        Admission::Invalid => INVALID,
    }
}
fn vault_status(e: RoomHandoffError) -> i64 {
    match e {
        RoomHandoffError::Locked | RoomHandoffError::Vault(VaultError::Locked) => LOCKED,
        RoomHandoffError::Busy | RoomHandoffError::Vault(VaultError::Busy) => BUSY,
        RoomHandoffError::Vault(VaultError::UncertainCommit) => RECONCILE,
        _ => VAULT,
    }
}
fn store_status(e: RoomFailure<RoomHandoffError>) -> i64 {
    match e {
        RoomFailure::Vault(e) => vault_status(e),
        RoomFailure::Conflict => RECONCILE,
        RoomFailure::Invalid(_) => INVALID,
        RoomFailure::WrongRoom => LOCKED,
    }
}
fn model_status(e: UiaRoomError<RoomHandoffError>) -> i64 {
    match e {
        UiaRoomError::Room(e) => store_status(e),
        UiaRoomError::Unavailable => MODEL,
        UiaRoomError::Inference => INFERENCE,
        UiaRoomError::Persistence => RECONCILE,
        UiaRoomError::IncompleteAnswer => INCOMPLETE,
        UiaRoomError::Busy => BUSY,
        UiaRoomError::Cancelled => CANCELLED,
    }
}

fn chat(token: i64, most: i64, least: i64, epoch: i64) -> Option<ChatLease> {
    UIA_ROOM_REGISTRY.chat(token, SelectedScope::new(most, least, epoch)?)
}

fn status_jni(
    mut caller: EnvUnowned<'_>,
    f: impl FnOnce(&mut Env<'_>) -> errors::Result<i64>,
) -> i64 {
    match caller
        .with_env(|env| -> errors::Result<i64> {
            match f(env) {
                Ok(code) => Ok(code),
                Err(_) => {
                    env.exception_clear();
                    Ok(VAULT)
                }
            }
        })
        .into_outcome()
    {
        Outcome::Ok(v) => v,
        Outcome::Err(_) | Outcome::Panic(_) => VAULT,
    }
}

/// Env never calls back into Java after this exclusive borrow begins.
fn frame<const N: usize, T>(
    env: &mut Env<'_>,
    direct: &JByteBuffer<'_>,
    f: impl FnOnce(&mut [u8; N]) -> T,
) -> errors::Result<Option<T>> {
    if env.get_direct_buffer_capacity(direct).ok() != Some(N)
        || env
            .call_method(direct, jni_str!("isReadOnly"), jni_sig!(() -> boolean), &[])?
            .z()?
    {
        return Ok(None);
    }
    let Ok(_borrow) = FRAME.lock() else {
        return Ok(None);
    };
    let ptr = env.get_direct_buffer_address(direct)?;
    // SAFETY: the exact trusted Kotlin backend creates a fresh mutable direct
    // buffer, held alive by this JNI ref. Access is exclusive and synchronous.
    Ok(Some(f(unsafe { &mut *ptr.cast::<[u8; N]>() })))
}

fn files(env: &mut Env<'_>, app: &JObject<'_>) -> errors::Result<Option<PathBuf>> {
    if vault_root(env, app)?.is_none() {
        return Ok(None);
    }
    let dir = env
        .call_method(
            app,
            jni_str!("getFilesDir"),
            jni_sig!("()Ljava/io/File;"),
            &[],
        )?
        .l()?;
    let path = env
        .call_method(
            &dir,
            jni_str!("getAbsolutePath"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    Ok(Some(PathBuf::from(
        env.cast_local::<JString>(path)?.try_to_string(env)?,
    )))
}
fn read_snapshot(chat: &ChatLease) -> Result<Zeroizing<Vec<u8>>, i64> {
    if !chat.ops.active() {
        return Err(LOCKED);
    }
    let snapshot =
        open_room(chat.owner.lease().as_ref(), chat.owner.room_id()).map_err(store_status)?;
    let mut encoded = Zeroizing::new(Vec::with_capacity(OUTPUT));
    encoded.extend_from_slice(snapshot.id().as_str().as_bytes());
    encoded.extend_from_slice(&snapshot.revision().to_be_bytes());
    let count = u16::try_from(snapshot.completed_turns().len()).map_err(|_| VAULT)?;
    encoded.extend_from_slice(&count.to_be_bytes());
    for pair in snapshot.completed_turns() {
        for text in [pair.user(), pair.answer()] {
            let n = u32::try_from(text.len()).map_err(|_| VAULT)?;
            encoded.extend_from_slice(&n.to_be_bytes());
            encoded.extend_from_slice(text.as_bytes());
        }
    }
    if snapshot.completed_turns().iter().any(|turn| turn.library_image().is_some()) {
        // Project only an occurrence/capability flag, never a Library ID or digest.
        encoded.extend(snapshot.completed_turns().iter().map(|turn| match turn.library_image() {
            None => 0,
            Some(photo) if photo.digest().is_some() => 2,
            Some(_) => 1, // legacy ID-only occurrence: display marker, never open image
        }));
    }
    // This trailing byte is disjoint from the existing 0/1/2 photo flags.
    // No attempt ID, draft, or model output is disclosed.
    match chat.owner.lease().prior_attempt_interrupted(snapshot.revision()) {
        Ok(false) => {}
        Ok(true) => encoded.push(ATTEMPT_INTERRUPTED),
        Err(RoomHandoffError::Locked | RoomHandoffError::Vault(VaultError::Locked)) => return Err(LOCKED),
        Err(_) => encoded.push(ATTEMPT_STATUS_UNKNOWN),
    }
    if encoded.len() > OUTPUT || !chat.ops.active() {
        return Err(LOCKED);
    }
    Ok(encoded)
}
struct Finish {
    token: i64,
    scope: SelectedScope,
    attempt: i64,
}
impl Drop for Finish {
    fn drop(&mut self) {
        UIA_ROOM_REGISTRY.finish_chat(self.token, self.scope, self.attempt);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_readRoomNative<'l>(
    caller: EnvUnowned<'l>,
    _self: JObject<'l>,
    token: i64,
    most: i64,
    least: i64,
    epoch: i64,
    out: JByteBuffer<'l>,
) -> i64 {
    status_jni(caller, |env| {
        let Some(chat) = chat(token, most, least, epoch) else {
            return Ok(LOCKED);
        };
        let payload = match read_snapshot(&chat) {
            Ok(v) => v,
            Err(code) => return Ok(code),
        };
        let written = frame::<OUTPUT, _>(env, &out, |buf| {
            buf.zeroize();
            if !chat.ops.active() {
                return LOCKED;
            }
            buf[..payload.len()].copy_from_slice(&payload);
            payload.len() as i64
        })?
        .unwrap_or(INVALID);
        Ok(written)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_cloudWorkNative<'l>(
    caller: EnvUnowned<'l>, _self: JObject<'l>, app: JObject<'l>, token: i64, most: i64, least: i64, generation: i64,
    local_profile: JString<'l>, ticket: JString<'l>, action: i32, output: JByteBuffer<'l>,
) -> i64 {
    use crate::core::manual_cloud_work::ManualCloudWorkAccess;
    use harw_protocol::session_wire::ManualCloudWorkReplyV1;
    const SIZE: usize = 128;
    status_jni(caller, |env| {
        let Some(chat) = chat(token, most, least, generation) else { return Ok(LOCKED); };
        if !matches!(action, 0..=2) { return Ok(INVALID); }
        let Some(files) = files(env, &app)? else { return Ok(LOCKED); };
        let claimed_profile = local_profile.try_to_string(env)?;
        let active = || chat.ops.active()
            && crate::local::uia_profile_key(&files).as_deref() == Some(claimed_profile.as_str());
        if !active() { return Ok(LOCKED); }
        let ticket = ticket.try_to_string(env)?;
        let Some(peer) = ManualCloudWorkAccess::lookup(&ticket).filter(|peer| peer.is_current()) else {
            return Ok(LOCKED);
        };
        if frame::<SIZE, _>(env, &output, |buf| buf.zeroize())?.is_none() { return Ok(INVALID); }
        let intent = match chat.owner.lease().cloud_work_intent(peer.authority(), action == 1) {
            Ok(intent) => intent,
            Err(e) => return Ok(vault_status(e)),
        };
        if action == 0 { return Ok(if intent.is_some() { 1 } else { 0 }); }
        let Some((id, publish)) = intent else { return Ok(0); };
        if !active() || !peer.is_current() { return Ok(LOCKED); }
        let Some(rt) = RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).max_blocking_threads(4).enable_all().build().ok()).as_ref()
        else { return Ok(VAULT); };
        let result = rt.block_on(peer.dispatch(id, publish, active));
        let Ok(ManualCloudWorkReplyV1::Created { session }) = result else { return Ok(RECONCILE); };
        let id = session.session_id.as_str().as_bytes();
        if !(1..=SIZE).contains(&id.len()) { return Ok(VAULT); }
        Ok(frame::<SIZE, _>(env, &output, |buf| {
            if !active() || !peer.is_current() { return LOCKED; }
            buf[..id.len()].copy_from_slice(id);
            id.len() as i64
        })?.unwrap_or(INVALID))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_memoryNative<'l>(
    caller: EnvUnowned<'l>, _self: JObject<'l>,
    token: i64, most: i64, least: i64, epoch: i64,
    revision: i64, memory_revision: i64, ordinal: i32, operation: i32, out: JByteBuffer<'l>,
) -> i64 {
    use crate::uia_room_vault::{MEMORY_WIRE, MemoryChange};
    status_jni(caller, |env| {
        let Some(chat) = chat(token, most, least, epoch) else { return Ok(LOCKED); };
        let Ok(room_revision) = u64::try_from(revision) else { return Ok(INVALID); };
        let change = match operation {
            0 if memory_revision == 0 && ordinal == 0 => None,
            1 | 2 if memory_revision >= 0 && (1..=128).contains(&ordinal) =>
                Some(MemoryChange {
                    expected_memory_revision: memory_revision as u64, ordinal: ordinal as u16,
                    save: operation == 1,
                }),
            _ => return Ok(INVALID),
        };
        // Validate (and borrow) the exact output buffer before an irreversible write.
        if frame::<MEMORY_WIRE, _>(env, &out, |buf| buf.zeroize())?.is_none() {
            return Ok(INVALID);
        }
        if !chat.ops.active() { return Ok(LOCKED); }
        let value = match chat.owner.lease().manual_memory(room_revision, change) {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(RECONCILE),
            Err(error) => return Ok(vault_status(error)),
        };
        let status = frame::<MEMORY_WIRE, _>(env, &out, |buf| {
            if !chat.ops.active() { return LOCKED; }
            *buf = value;
            MEMORY_WIRE as i64
        })?.unwrap_or(INVALID);
        Ok(status)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_reserveRoomNative<'l>(
    caller: EnvUnowned<'l>,
    _self: JObject<'l>,
    token: i64,
    most: i64,
    least: i64,
    epoch: i64,
    attempt: i64,
) -> i64 {
    status_jni(caller, |_| {
        Ok(chat(token, most, least, epoch).map_or(LOCKED, |chat| {
            chat.ops.reserve(attempt).map_or_else(status, |()| 1)
        }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_cancelRoomNative<'l>(
    caller: EnvUnowned<'l>,
    _self: JObject<'l>,
    token: i64,
    most: i64,
    least: i64,
    epoch: i64,
    attempt: i64,
) -> i64 {
    status_jni(caller, |_| {
        Ok(chat(token, most, least, epoch)
            .map_or(LOCKED, |chat| i64::from(chat.ops.cancel(attempt))))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_parkRoomVersionNative(
    _caller: EnvUnowned<'_>, _self: JObject<'_>,
) -> i32 { 2 }

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_parkRoomNative<'l>(
    caller: EnvUnowned<'l>, _self: JObject<'l>,
    token: i64, most: i64, least: i64, epoch: i64, attempt: i64,
) -> i64 {
    status_jni(caller, |_| {
        let Some(scope) = SelectedScope::new(most, least, epoch) else { return Ok(0); };
        let Some(rt) = RUNTIME.get().and_then(Option::as_ref) else { return Ok(0); };
        let Some(expiry) = UIA_ROOM_REGISTRY.park(token, scope, attempt) else { return Ok(0); };
        drop(rt.spawn(expiry.run()));
        Ok(1)
    })
}

/// The Kotlin controller calls this for the exact active local profile's
/// existing vault before charging PIN. No authentication or lock bypass:
/// ordinary native unlock still independently enforces all owner locks.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_hasPendingParkedResponseNative(
    _caller: EnvUnowned<'_>, _self: JObject<'_>,
    most: i64, least: i64, generation: i64,
) -> i32 {
    SelectedScope::new(most, least, generation)
        .and_then(|scope| UIA_ROOM_REGISTRY.has_pending_parked(scope))
        .map_or(-1, i32::from)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_cancelParkedRoomNative<'l>(
    caller: EnvUnowned<'l>, _self: JObject<'l>,
    token: i64, most: i64, least: i64, epoch: i64, attempt: i64,
) -> i64 {
    status_jni(caller, |_| {
        let Some(scope) = SelectedScope::new(most, least, epoch) else { return Ok(0); };
        Ok(i64::from(UIA_ROOM_REGISTRY.cancel_parked(token, scope, attempt)))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_runRoomNative<'l>(
    caller: EnvUnowned<'l>,
    _self: JObject<'l>,
    app: JObject<'l>,
    token: i64,
    most: i64,
    least: i64,
    epoch: i64,
    attempt: i64,
    revision: i64,
    input: JByteBuffer<'l>,
) -> i64 {
    status_jni(caller, |env| {
        let Some(scope) = SelectedScope::new(most, least, epoch) else { return Ok(LOCKED); };
        let Some(chat) = UIA_ROOM_REGISTRY.chat(token, scope) else { return Ok(LOCKED); };
        let cancel = match chat.ops.take(attempt) {
            Ok(v) => v,
            Err(e) => return Ok(status(e)),
        };
        let _finish = Finish { token, scope, attempt };
        let Some(publish) = chat.ops.publication(attempt) else { return Ok(LOCKED); };
        let text_field = |buf: &[u8]| {
            let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            (1..=8192).contains(&n).then(|| Zeroizing::new(buf[4..4 + n].to_vec()))
        };
        let mut selection = None;
        let payload = if env.get_direct_buffer_capacity(&input).ok() == Some(PHOTO_INPUT) {
            frame::<PHOTO_INPUT, _>(env, &input, |buf| {
                let mut revision = [0; 8];
                revision.copy_from_slice(&buf[INPUT..INPUT + 8]);
                let mut id = [0; 16];
                id.copy_from_slice(&buf[INPUT + 8..]);
                selection = Some((u64::from_be_bytes(revision), id));
                let bytes = text_field(buf);
                buf.zeroize();
                bytes
            })?.flatten()
        } else {
            frame::<INPUT, _>(env, &input, |buf| {
                let bytes = text_field(buf);
                buf.zeroize();
                bytes
            })?.flatten()
        };
        let (Some(payload), Ok(rev)) = (payload, u64::try_from(revision)) else {
            return Ok(INVALID);
        };
        let Ok(text) = std::str::from_utf8(&payload) else {
            return Ok(INVALID);
        };
        if cancel.is_cancelled() || !chat.ops.active() {
            return Ok(CANCELLED);
        }
        let Some(files) = files(env, &app)? else {
            return Ok(LOCKED);
        };
        let Some(model) = crate::local::uia_runtime_for(&files) else {
            return Ok(MODEL);
        };
        let Some(admitted_profile) = crate::local::uia_runtime_profile_key(&model, &files) else {
            return Ok(LOCKED);
        };
        let pinned_profile = admitted_profile.clone();
        let same_profile: std::sync::Arc<dyn Fn() -> bool + Send + Sync> =
            std::sync::Arc::new(move || crate::local::uia_profile_key(&files).as_deref() == Some(pinned_profile.as_str()));
        let Some(rt) = RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .max_blocking_threads(4)
                    .enable_all()
                    .build()
                    .ok()
            })
            .as_ref()
        else {
            return Ok(MODEL);
        };
        if let Some((catalogue_revision, id)) = selection {
            use crate::private_library::{LibraryError, LibraryItemId};

            let Some(scope) = SelectedScope::new(most, least, epoch) else { return Ok(LOCKED); };
            let Some(profile) = crate::private_library_jni::profile(env, &app)? else { return Ok(LOCKED); };
            if hex::encode(profile) != admitted_profile { return Ok(LOCKED); }
            let Some(owner) = UIA_ROOM_REGISTRY.library(token, scope, profile) else { return Ok(LOCKED); };
            let id = match LibraryItemId::parse(id) {
                Ok(id) => id,
                Err(_) => return Ok(PHOTO_INVALID),
            };
            fn image_status(error: LibraryError) -> i64 {
                match error {
                    LibraryError::Conflict | LibraryError::NotFound => PHOTO_CHANGED,
                    LibraryError::InvalidInput => PHOTO_INVALID,
                    LibraryError::Locked | LibraryError::BadScope => LOCKED,
                    LibraryError::Busy => BUSY,
                    _ => VAULT,
                }
            }
            let store = match owner.open_existing() {
                Ok(Some(store)) => store,
                Ok(None) => return Ok(PHOTO_CHANGED),
                Err(error) => return Ok(image_status(error)),
            };
            let jpeg = match store.jpeg(catalogue_revision, id) {
                Ok(jpeg) => jpeg,
                Err(error) => return Ok(image_status(error)),
            };
            let Ok(info) = harw_media::inspect(&jpeg) else { return Ok(PHOTO_INVALID); };
            if info.format != harw_protocol::ImageFormat::Jpeg || info.width > 1280 || info.height > 1280 {
                return Ok(PHOTO_INVALID);
            }
            let mut sanitized = Zeroizing::new(Vec::new());
            if harw_media::sanitize_jpeg_into(&jpeg, &mut sanitized).is_err() {
                return Ok(PHOTO_INVALID);
            }
            let Ok(identity) = crate::uia_room_store::RoomPhotoIdentity::new(
                id.bytes(), Some(*blake3::hash(&jpeg).as_bytes()),
            ) else { return Ok(PHOTO_INVALID); };
            let photo = uia_room::PrivateRoomPhoto { identity, jpeg: sanitized };
            if cancel.is_cancelled() || !chat.ops.active() || !owner.active() {
                return Ok(LOCKED);
            }
            // Once the exact catalogue selection has been authenticated and
            // materialized, detach all Library ownership before inference.
            // Subsequent checks address only original profile/attempt liveness,
            // never selecting a replacement image or reopening a reader.
            drop(jpeg);
            drop(store);
            drop(owner);
            let admission_profile = same_profile.clone();
            let ops = chat.ops;
            let admission_ops = ops.clone();
            let foreground_admission = move || admission_ops.active() && admission_profile();
            let room_id = chat.owner.room_id().clone();
            let foreground = chat.owner.lease();
            drop(chat.owner);
            let result = rt.block_on(uia_room::reply_with_photo_and_commit(
                &model, foreground, &room_id, rev, text, photo, cancel, publish,
                foreground_admission, same_profile,
            ));
            if !ops.active() || crate::private_library_jni::profile(env, &app)? != Some(profile) {
                return Ok(LOCKED);
            }
            return Ok(result.map_or_else(|error| match error {
                UiaRoomError::Unavailable => PHOTO_MODEL,
                other => model_status(other),
            }, |_| 1));
        }
        let room_id = chat.owner.room_id().clone();
        let foreground = chat.owner.lease();
        let ops = chat.ops;
        drop(chat.owner);
        let result = rt.block_on(uia_room::reply_and_commit(
            &model, foreground, &room_id, rev, text, cancel, publish, same_profile.clone(),
        ));
        if !ops.active() || !same_profile() {
            return Ok(LOCKED);
        }
        Ok(result.map_or_else(model_status, |_| 1))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_LocalUiaDevCapability_activeProfileNative<'l>(
    mut caller: EnvUnowned<'l>,
    _self: JObject<'l>,
    app: JObject<'l>,
) -> jni::sys::jstring {
    let outcome = caller.with_env(|env| -> errors::Result<jni::sys::jstring> {
        let value = (|| -> errors::Result<jni::sys::jstring> {
            if !harw_llama::engine_available() {
                return Ok(std::ptr::null_mut());
            }
            let Some(files) = files(env, &app)? else {
                return Ok(std::ptr::null_mut());
            };
            let Some(key) = crate::local::uia_profile_key(&files) else {
                return Ok(std::ptr::null_mut());
            };
            Ok(env.new_string(key)?.into_raw())
        })();
        if value.is_err() {
            env.exception_clear();
            return Ok(std::ptr::null_mut());
        }
        value
    });
    match outcome.into_outcome() {
        Outcome::Ok(value) => value,
        Outcome::Err(_) | Outcome::Panic(_) => std::ptr::null_mut(),
    }
}

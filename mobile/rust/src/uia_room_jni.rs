//! Private JNI names for the exact trusted app-process backend. Not UniFFI;
//! default APK builds neither define these symbols nor instantiate the owner.
#![deny(unsafe_op_in_unsafe_fn)]

use jni::{
    Env, EnvUnowned, Outcome, errors, jni_sig, jni_str,
    objects::{JByteBuffer, JObject, JString},
};
use std::{
    path::PathBuf,
    sync::{LazyLock, Mutex},
};
use zeroize::Zeroize;

use crate::uia_room_registry::{
    FRAME_BYTES, Mode, NativeError, Registry, SelectedScope, decode_frame,
};

pub(crate) static UIA_ROOM_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::default);
static FRAME_BORROW: Mutex<()> = Mutex::new(());

pub(crate) fn vault_root(env: &mut Env<'_>, app: &JObject<'_>) -> errors::Result<Option<PathBuf>> {
    if app.is_null() || !env.is_instance_of(app, jni_str!("dev/harw/mobile/HarwApplication"))? {
        return Ok(None);
    }
    let context = env
        .call_method(
            app,
            jni_str!("getApplicationContext"),
            jni_sig!("()Landroid/content/Context;"),
            &[],
        )?
        .l()?;
    if !env.is_same_object(app, &context)?
        || env
            .call_method(
                app,
                jni_str!("isDeviceProtectedStorage"),
                jni_sig!(() -> boolean),
                &[],
            )?
            .z()?
    {
        return Ok(None);
    }
    let package = env
        .call_method(
            app,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    if env.cast_local::<JString>(package)?.try_to_string(env)? != "dev.harw.mobile" {
        return Ok(None);
    }
    let root = env
        .call_method(
            app,
            jni_str!("getNoBackupFilesDir"),
            jni_sig!("()Ljava/io/File;"),
            &[],
        )?
        .l()?;
    let path = env
        .call_method(
            &root,
            jni_str!("getAbsolutePath"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    Ok(Some(PathBuf::from(
        env.cast_local::<JString>(path)?.try_to_string(env)?,
    )))
}

fn open<'local>(
    mut caller: EnvUnowned<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    buffer: JByteBuffer<'local>,
    mode: Mode,
) -> i64 {
    let result = caller.with_env(|env| -> errors::Result<i64> {
        let value = (|| -> errors::Result<i64> {
            if env.get_direct_buffer_capacity(&buffer).ok() != Some(FRAME_BYTES)
                || env
                    .call_method(
                        &buffer,
                        jni_str!("isReadOnly"),
                        jni_sig!(() -> boolean),
                        &[],
                    )?
                    .z()?
            {
                return Ok(NativeError::Denied as i64);
            }
            let (scope, proof) = {
                let Ok(_exclusive) = FRAME_BORROW.try_lock() else {
                    return Ok(NativeError::Busy as i64);
                };
                let ptr = env.get_direct_buffer_address(&buffer)?;
                // SAFETY: actual exclusive mutable direct allocation is kept alive by
                // this JNI local reference; length and writability are checked above.
                // The one Android backend allocates fresh memory per call. Borrow never
                // escapes or overlaps Java callbacks. Kotlin still wipes in finally.
                let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
                let Some(scope) = SelectedScope::new(most, least, generation) else {
                    frame.zeroize();
                    return Ok(NativeError::Denied as i64);
                };
                let proof = match decode_frame(scope, frame) {
                    Ok(proof) => proof,
                    Err(error) => return Ok(error as i64),
                };
                (scope, proof)
            };
            // The direct frame has been wiped before crossing back into Java to
            // resolve the actual Application's private path. No argument path.
            let path = match vault_root(env, &app) {
                Ok(Some(path)) => path,
                Ok(None) => return Ok(mode.enrollment_error(NativeError::Denied, NativeError::EnrollmentApp) as i64),
                Err(_) => {
                    env.exception_clear();
                    return Ok(mode.enrollment_error(NativeError::Unavailable, NativeError::EnrollmentApp) as i64);
                }
            };
            #[cfg(feature = "vault-uia-chat")]
            let library = match crate::private_library_jni::profile(env, &app) {
                Ok(Some(profile)) => proof.library_authority(profile).ok().map(|authority| (profile, authority)),
                Ok(None) => None,
                Err(_) => {
                    env.exception_clear();
                    None
                }
            };
            let token = match UIA_ROOM_REGISTRY.open(&path, scope, mode, proof) {
                Ok(token) => token,
                Err(error) => return Ok(error as i64),
            };
            #[cfg(feature = "vault-uia-chat")]
            if let Some((profile, authority)) = library {
                UIA_ROOM_REGISTRY.register_library(token, scope, &path, profile, authority);
            }
            Ok(token)
        })();
        if value.is_err() {
            // Do not propagate stack traces, absolute file paths, or JNI detail.
            env.exception_clear();
            return Ok(NativeError::Unavailable as i64);
        }
        value
    });
    match result.into_outcome() {
        Outcome::Ok(status) => status,
        Outcome::Err(_) | Outcome::Panic(_) => NativeError::Unavailable as i64,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_provisionNewNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    frame: JByteBuffer<'local>,
) -> i64 {
    open(caller, app, most, least, generation, frame, Mode::Provision)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_resumeEnrollmentNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    frame: JByteBuffer<'local>,
) -> i64 {
    open(caller, app, most, least, generation, frame, Mode::Resume)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_openExistingNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    frame: JByteBuffer<'local>,
) -> i64 {
    open(caller, app, most, least, generation, frame, Mode::Existing)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_openExistingBiometricNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    frame: JByteBuffer<'local>,
) -> i64 {
    open(caller, app, most, least, generation, frame, Mode::ExistingBiometric)
}

fn restart_uncommitted<'local>(
    mut caller: EnvUnowned<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
    retire: bool,
) -> i32 {
    let result = caller.with_env(|env| -> errors::Result<i32> {
        let Some(scope) = SelectedScope::new(most, least, generation) else {
            return Ok(0);
        };
        match vault_root(env, &app) {
            Ok(Some(root)) => Ok(i32::from(
                UIA_ROOM_REGISTRY.restart_uncommitted(&root, scope, retire).is_ok(),
            )),
            Ok(None) => Ok(0),
            Err(_) => {
                env.exception_clear();
                Ok(0)
            }
        }
    });
    match result.into_outcome() {
        Outcome::Ok(status) => status,
        Outcome::Err(_) | Outcome::Panic(_) => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_canRestartUncommittedNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
) -> i32 {
    restart_uncommitted(caller, app, most, least, generation, false)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_retireUncommittedNative<'local>(
    caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    app: JObject<'local>,
    most: i64,
    least: i64,
    generation: i64,
) -> i32 {
    restart_uncommitted(caller, app, most, least, generation, true)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidUiaRoomPinNative_revokeNative<'local>(
    _caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    token: i64,
    most: i64,
    least: i64,
    generation: i64,
) -> i32 {
    if token <= 0 {
        return 0;
    }
    let Some(scope) = SelectedScope::new(most, least, generation) else {
        return 0;
    };
    i32::from(UIA_ROOM_REGISTRY.revoke(token, scope))
}

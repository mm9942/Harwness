//! Direct, bounded JNI buffer. Secrets must not use generated UniFFI RustBuffer copies.
#![deny(unsafe_op_in_unsafe_fn)]

use jni::{
    EnvUnowned, Outcome, errors, jni_sig, jni_str,
    objects::{JByteBuffer, JObject},
};
use std::sync::Mutex;

use crate::vault_pin_kdf::{FRAME_BYTES, derive_v1_frame};

static DERIVATION: Mutex<()> = Mutex::new(());

/// Called only with a fresh, private, mutable direct buffer from AndroidVaultPinKdf.
/// Success is 1; any failure (including busy) is 0. Errors never contain input material.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_harw_mobile_AndroidVaultPinKdf_deriveInPlace<'local>(
    mut caller: EnvUnowned<'local>,
    _instance: JObject<'local>,
    buffer: JByteBuffer<'local>,
) -> i32 {
    let result = caller.with_env(|env| -> errors::Result<i32> {
        // JNI can return a pointer even for a read-only direct buffer. Never write to it.
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
            return Ok(0);
        }
        // Take the permit before forming any native mutable borrow.
        let Ok(_permit) = DERIVATION.try_lock() else {
            return Ok(0);
        };
        let ptr = env.get_direct_buffer_address(&buffer)?;
        // SAFETY: capacity is exactly FRAME_BYTES, the Java reference lives through this
        // method, and the private Kotlin caller exclusively owns a fresh mutable buffer.
        // The native borrow is synchronous, never handed to callbacks, and never escapes.
        let frame = unsafe { &mut *ptr.cast::<[u8; FRAME_BYTES]>() };
        Ok(i32::from(derive_v1_frame(frame)))
    });
    match result.into_outcome() {
        Outcome::Ok(status) => status,
        Outcome::Err(_) | Outcome::Panic(_) => 0,
    }
}

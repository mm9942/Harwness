package dev.harw.mobile

import java.nio.ByteBuffer

private const val V1FrameBytes = 66
private const val SaltOffset = 18
private const val SaltBytes = 16
private const val OutputOffset = SaltOffset + SaltBytes
private const val OutputBytes = 32

/** Internal and dormant: no UI, vault enablement, or fallback. Worker thread only. */
internal object AndroidVaultPinKdf {
    private val nativeLoaded by lazy { System.loadLibrary("harw_mobile_ffi") }

    /**
     * Inputs are borrowed. The upstream PIN protector wipes the original PIN and takes
     * ownership of the returned 32 bytes, using then wiping them synchronously.
     */
    fun deriveArgon2idV1(pin: ByteArray, salt: ByteArray): ByteArray {
        require(pin.size in 8..16 && pin.all { it >= 0x30 && it <= 0x39 })
        require(salt.size == SaltBytes)

        val frame = ByteBuffer.allocateDirect(V1FrameBytes)
        try {
            nativeLoaded
            frame.put(0, 1.toByte())
            frame.put(1, pin.size.toByte())
            for (index in pin.indices) frame.put(2 + index, pin[index])
            for (index in salt.indices) frame.put(SaltOffset + index, salt[index])
            check(deriveInPlace(frame) == 1) { "Vault PIN derivation unavailable" }
            return ByteArray(OutputBytes) { frame.get(OutputOffset + it) }
        } catch (_: LinkageError) {
            throw IllegalStateException("Vault PIN derivation unavailable")
        } finally {
            for (index in 0 until V1FrameBytes) frame.put(index, 0.toByte())
        }
    }

    // The native side borrows an exclusively held direct frame; never expose this API.
    private external fun deriveInPlace(frame: ByteBuffer): Int
}

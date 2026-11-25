package dev.harw.mobile

import java.nio.ByteBuffer
import java.security.SecureRandom
import java.util.UUID
import java.util.concurrent.CancellationException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

private const val PinSlotV1: Byte = 1
private const val SaltBytes = 16
private const val NonceBytes = 12
private const val VaultSecretBytes = 32
private const val TagBytes = 16
private const val CiphertextOffset = 1 + SaltBytes + NonceBytes
internal const val VaultPinSlotBytes = CiphertextOffset + VaultSecretBytes + TagBytes
private const val Transformation = "AES/GCM/NoPadding"

internal data class VaultPinScope(val vaultId: UUID, val keyGeneration: Long) {
    init {
        require(keyGeneration > 0)
    }
}

internal enum class VaultPinFailure { InvalidPin, InvalidSlot, Rejected, DeviceKeyMissing, DeviceKeyInvalidated, KdfUnavailable, Unavailable }

internal class VaultPinException(val failure: VaultPinFailure) : Exception(failure.name)

/**
 * Required production implementation: RFC 9106 Argon2id v1.3, m=65_536 KiB, t=3, p=4,
 * producing an owned 32-byte output. No platform substitute or default implementation.
 * Borrowed inputs must not be retained or modified. Call off the main thread.
 */
internal fun interface VaultPinKdf {
    fun deriveArgon2idV1(pin: ByteArray, salt: ByteArray): ByteArray
}

/** Hardware device binding in production; result is owned, inputs are borrowed. */
internal fun interface VaultPinDeviceMac {
    fun authenticate(context: ByteArray, stretchedPin: ByteArray): ByteArray
}

/**
 * Dormant Android PIN slot boundary. Never persists a PIN verifier or a key.
 * PIN arrays are consumed and wiped on every path. The caller owns the seed supplied to [seal].
 * [withUnsealed] lends the seed synchronously; its consumer must not save or retain the buffer.
 * JVM/provider internal key copies are not under this class's control.
 */
internal class VaultPinProtector(
    private val kdf: VaultPinKdf,
    private val device: VaultPinDeviceMac,
) {
    private val randomness = SecureRandom()

    fun seal(scope: VaultPinScope, pin: ByteArray, rootUnlockSecret: ByteArray): ByteArray =
        withPin(pin) {
            require(rootUnlockSecret.size == VaultSecretBytes)
            val salt = ByteArray(SaltBytes).also(randomness::nextBytes)
            val nonce = ByteArray(NonceBytes).also(randomness::nextBytes)
            val encrypted = useKey(scope, pin, salt) { key ->
                crypt(Cipher.ENCRYPT_MODE, key, scope, salt, nonce, rootUnlockSecret)
            }
            ByteBuffer.allocate(VaultPinSlotBytes).put(PinSlotV1).put(salt).put(nonce).put(encrypted).array()
        }

    fun <T> withUnsealed(
        scope: VaultPinScope,
        pin: ByteArray,
        slot: ByteArray,
        consume: (ByteArray) -> T,
    ): T {
        val secret = withPin(pin) {
            if (slot.size != VaultPinSlotBytes || slot[0] != PinSlotV1) {
                throw VaultPinException(VaultPinFailure.InvalidSlot)
            }
            val salt = slot.copyOfRange(1, 1 + SaltBytes)
            val nonce = slot.copyOfRange(1 + SaltBytes, CiphertextOffset)
            val encrypted = slot.copyOfRange(CiphertextOffset, VaultPinSlotBytes)
            useKey(scope, pin, salt) { key ->
                crypt(Cipher.DECRYPT_MODE, key, scope, salt, nonce, encrypted)
            }
        }
        return try {
            consume(secret)
        } finally {
            secret.fill(0)
        }
    }

    private inline fun <T> withPin(pin: ByteArray, block: () -> T): T = try {
        if (pin.size !in 8..16 || !pin.all { it >= 0x30 && it <= 0x39 }) {
            throw VaultPinException(VaultPinFailure.InvalidPin)
        }
        block()
    } finally {
        pin.fill(0)
    }

    private inline fun <T> useKey(
        scope: VaultPinScope,
        pin: ByteArray,
        salt: ByteArray,
        block: (ByteArray) -> T,
    ): T = try {
        val stretched = try {
            kdf.deriveArgon2idV1(pin, salt)
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            throw VaultPinException(VaultPinFailure.KdfUnavailable)
        }
        try {
            if (stretched.size != VaultSecretBytes) throw VaultPinException(VaultPinFailure.KdfUnavailable)
            val key = device.authenticate(context("Harw/VaultPin/Key", scope, salt), stretched)
            try {
                if (key.size != VaultSecretBytes) throw VaultPinException(VaultPinFailure.Unavailable)
                block(key)
            } finally {
                key.fill(0)
            }
        } finally {
            stretched.fill(0)
        }
    } catch (e: VaultPinException) {
        throw e
    } catch (e: CancellationException) {
        throw e
    } catch (_: AEADBadTagException) {
        // Do not distinguish wrong PIN from invalid/tampered ciphertext.
        throw VaultPinException(VaultPinFailure.Rejected)
    } catch (_: Exception) {
        throw VaultPinException(VaultPinFailure.Unavailable)
    }

    private fun crypt(
        mode: Int,
        key: ByteArray,
        scope: VaultPinScope,
        salt: ByteArray,
        nonce: ByteArray,
        payload: ByteArray,
    ): ByteArray = Cipher.getInstance(Transformation).run {
        init(mode, SecretKeySpec(key, "AES"), GCMParameterSpec(TagBytes * Byte.SIZE_BITS, nonce))
        updateAAD(context("Harw/VaultPin/Slot", scope, salt))
        doFinal(payload)
    }
}

private fun context(domain: String, scope: VaultPinScope, salt: ByteArray): ByteArray {
    val label = domain.encodeToByteArray()
    return ByteBuffer.allocate(label.size + 1 + 3 * Long.SIZE_BYTES + SaltBytes)
        .put(label)
        .put(PinSlotV1)
        .putLong(scope.vaultId.mostSignificantBits)
        .putLong(scope.vaultId.leastSignificantBits)
        .putLong(scope.keyGeneration)
        .put(salt)
        .array()
}

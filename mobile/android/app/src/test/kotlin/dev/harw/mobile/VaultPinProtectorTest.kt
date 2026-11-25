package dev.harw.mobile

import java.security.MessageDigest
import java.util.UUID
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class VaultPinProtectorTest {
    private val scope = VaultPinScope(UUID.fromString("e276b311-11c9-45b1-bf76-f0c3522a6379"), 4)
    private val seed = ByteArray(32) { (it + 17).toByte() }

    @Test
    fun roundTripOnlyLendsSeedAndWipesTransferredPin() {
        val fixture = Fixture()
        val enrollmentPin = pin()
        val envelope = fixture.protector.seal(scope, enrollmentPin, seed)
        assertTrue(enrollmentPin.all { it == 0.toByte() })
        assertFalse(envelope.toList().windowed(seed.size).any { it == seed.toList() })
        val unlockPin = pin()
        lateinit var borrowed: ByteArray
        fixture.protector.withUnsealed(scope, unlockPin, envelope) {
            assertArrayEquals(seed, it)
            borrowed = it
        }
        assertTrue(unlockPin.all { it == 0.toByte() })
        assertTrue(borrowed.all { it == 0.toByte() })
        assertTrue(fixture.derived.all { result -> result.all { it == 0.toByte() } })
        assertTrue(fixture.pepperOutputs.all { result -> result.all { it == 0.toByte() } })
    }

    @Test
    fun wrongPinWrongDeviceAndWrongScopeNeverLendSeed() {
        val fixture = Fixture()
        val envelope = fixture.protector.seal(scope, pin(), seed)
        rejected(fixture.protector, scope, "86420317".encodeToByteArray(), envelope)
        rejected(Fixture(2).protector, scope, pin(), envelope)
        rejected(fixture.protector, scope.copy(vaultId = UUID.randomUUID()), pin(), envelope)
        rejected(fixture.protector, scope.copy(keyGeneration = 5), pin(), envelope)
    }

    @Test
    fun malformedSlotsFailBeforeDeviceOrKdf() {
        val fixture = Fixture()
        val envelope = fixture.protector.seal(scope, pin(), seed)
        val before = fixture.derived.size
        for (bad in listOf(envelope.copyOf(envelope.size + 1), envelope.copyOf(2), envelope.copyOf().apply { this[0] = 0 })) {
            val unlockingPin = pin()
            val error = assertThrows(VaultPinException::class.java) {
                fixture.protector.withUnsealed(scope, unlockingPin, bad) { error("must not unlock") }
            }
            assertEquals(VaultPinFailure.InvalidSlot, error.failure)
            assertTrue(unlockingPin.all { it == 0.toByte() })
        }
        assertEquals(before, fixture.derived.size)
        assertEquals(before, fixture.pepperOutputs.size)
    }

    @Test
    fun saltNonceAndCiphertextAreAuthenticated() {
        val fixture = Fixture()
        val envelope = fixture.protector.seal(scope, pin(), seed)
        val next = fixture.protector.seal(scope, pin(), seed)
        assertFalse(envelope.contentEquals(next))
        for (index in listOf(1, 17, envelope.lastIndex)) {
            val changed = envelope.copyOf().apply { this[index] = (this[index].toInt() xor 1).toByte() }
            rejected(fixture.protector, scope, pin(), changed)
        }
    }

    @Test
    fun lostPlatformKeyOrKdfFailureHasNoFallback() {
        val fixture = Fixture()
        val envelope = fixture.protector.seal(scope, pin(), seed)
        val protector = VaultPinProtector(fixture.kdf, VaultPinDeviceMac { _, _ ->
            throw VaultPinException(VaultPinFailure.DeviceKeyMissing)
        })
        val entered = pin()
        val locked = assertThrows(VaultPinException::class.java) {
            protector.withUnsealed(scope, entered, envelope) { error("must not unlock") }
        }
        assertEquals(VaultPinFailure.DeviceKeyMissing, locked.failure)
        assertTrue(entered.all { it == 0.toByte() })

        val missingKdf = VaultPinProtector(VaultPinKdf { _, _ -> error("synthetic KDF failure") }, fixture.device)
        val failure = assertThrows(VaultPinException::class.java) {
            missingKdf.withUnsealed(scope, pin(), envelope) { error("must not unlock") }
        }
        assertEquals(VaultPinFailure.KdfUnavailable, failure.failure)
    }

    @Test
    fun invalidPinRejectedBeforeHashAndBorrowWipedWhenConsumerThrows() {
        val fixture = Fixture()
        for (bad in listOf("1234567", "1234567x", "12345678\n", "12345678901234567", "1234567９")) {
            val bytes = bad.encodeToByteArray()
            val invalid = assertThrows(VaultPinException::class.java) { fixture.protector.seal(scope, bytes, seed) }
            assertEquals(VaultPinFailure.InvalidPin, invalid.failure)
            assertTrue(bytes.all { it == 0.toByte() })
        }
        assertTrue(fixture.derived.isEmpty())
        val envelope = fixture.protector.seal(scope, pin(), seed)
        lateinit var borrowed: ByteArray
        assertThrows(IllegalStateException::class.java) {
            fixture.protector.withUnsealed(scope, pin(), envelope) {
                borrowed = it
                error("consumer failed")
            }
        }
        assertTrue(borrowed.all { it == 0.toByte() })
    }

    private fun rejected(protector: VaultPinProtector, target: VaultPinScope, pin: ByteArray, envelope: ByteArray) {
        val failure = assertThrows(VaultPinException::class.java) {
            protector.withUnsealed(target, pin, envelope) { error("must not unlock") }
        }
        assertEquals(VaultPinFailure.Rejected, failure.failure)
    }

    private fun pin(): ByteArray = "78631029".encodeToByteArray()

    /** Test-only fake KDF and synthetic JVM software key; impossible to select in the app. */
    private class Fixture(keyByte: Byte = 1) {
        val derived = mutableListOf<ByteArray>()
        val pepperOutputs = mutableListOf<ByteArray>()
        val kdf = VaultPinKdf { pin, salt ->
            val hash = MessageDigest.getInstance("SHA-256")
            hash.update(salt)
            hash.digest(pin).also(derived::add)
        }
        val device = VaultPinDeviceMac { context, input ->
            val mac = Mac.getInstance("HmacSHA256")
            mac.init(SecretKeySpec(ByteArray(32) { keyByte }, "HmacSHA256"))
            mac.update(context)
            mac.doFinal(input).also(pepperOutputs::add)
        }
        val protector = VaultPinProtector(kdf, device)
    }
}

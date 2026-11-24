package dev.harw.mobile

import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.security.MessageDigest
import java.util.UUID
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UiaRoomPinBackendTest {
    @Test
    fun newPathPersistsAndAuthenticatesReadbackBeforeProducingExactProof() = withFixture { f ->
        val pin = f.pin()
        assertTrue(f.backend.provisionNew(f.scope, pin) is VaultPinProof.Granted)
        assertArrayEquals(f.pin(), pin) // The controller, not the borrowing backend, wipes its input.
        assertEquals(1, f.provisions)
        assertEquals(0, f.opens)
        assertEquals(2, f.derivations)
        assertEquals(1, f.proofs)
        assertEquals(1, f.created)
        assertEquals(0, f.reopened)
        f.checkScrubbed()
        val encrypted = Files.readAllBytes(f.directory.resolve("pin.v1"))
        assertEquals(77, encrypted.size)
        assertFalse(encrypted.toList().windowed(32).any { it == f.syntheticRoot.toList() })
        assertEquals(VaultPinRegistration.Provisioning, f.store.read().registration)
        // No overwriting an installed slot, even if a previous setup never returned as Ready.
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.SlotStorage), f.backend.provisionNew(f.scope, f.pin()))
        assertEquals(1, f.provisions)
        assertArrayEquals(encrypted, Files.readAllBytes(f.directory.resolve("pin.v1")))
    }

    @Test
    fun existingPathRejectsWrongPinWithoutProofAndNeverProvisions() = withFixture { f ->
        f.completeSetup()
        val before = f.created + f.reopened
        val wrong = "78631029".encodeToByteArray()
        assertEquals(VaultPinProof.Rejected, f.backend.openExisting(f.scope, wrong))
        assertEquals(before, f.created + f.reopened)
        assertEquals(1, f.proofs)
        assertTrue(f.backend.openExisting(f.scope, f.pin()) is VaultPinProof.Granted)
        assertEquals(1, f.provisions)
        assertEquals(2, f.opens)
        assertEquals(1, f.reopened)
        f.checkScrubbed()
    }

    @Test
    fun scopedPhaseMissingSlotAndKeyErrorsCannotBootstrapOrReachNative() = withFixture { f ->
        assertEquals(VaultPinProof.Unavailable, f.backend.provisionNew(f.scope.copy(vaultId = UUID(7, 8)), f.pin()))
        assertEquals(VaultPinProof.Unavailable, f.backend.openExisting(f.scope, f.pin()))
        assertEquals(0, f.opens + f.provisions)
        f.completeSetup()
        for ((failure, expected) in listOf(
            VaultPinFailure.DeviceKeyMissing to VaultPinRecovery.MissingKey,
            VaultPinFailure.DeviceKeyInvalidated to VaultPinRecovery.InvalidatedKey,
        )) {
            f.readHardwareFailure = failure
            assertEquals(VaultPinProof.RecoveryRequired(expected), f.backend.openExisting(f.scope, f.pin()))
        }
        f.readHardwareFailure = null
        Files.delete(f.directory.resolve("pin.v1"))
        assertEquals(VaultPinProof.RecoveryRequired(VaultPinRecovery.DamagedVault), f.backend.openExisting(f.scope, f.pin()))
        assertFalse(Files.exists(f.directory.resolve("pin.v1")))
        assertEquals(1, f.provisions)
        assertEquals(0, f.reopened)
    }

    @Test
    fun nativeFailureWipesDirectFrameProofAndBorrowedUnwrapRoot() = withFixture { f ->
        f.invokeNative = { throw IOException("synthetic native error") }
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.NativeBridge), f.backend.provisionNew(f.scope, f.pin()))
        assertEquals(VaultPinRegistration.Provisioning, f.store.read().registration)
        f.checkScrubbed()
        val before = f.provisions
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.SlotStorage), f.backend.provisionNew(f.scope, f.pin()))
        assertEquals(before, f.provisions)
    }

    @Test
    fun proofIsNotDeliveredForARevisionChangedDuringPinVerification() = withFixture { f ->
        f.completeSetup()
        f.onDerive = {
            val old = f.store.read()
            assertTrue(f.store.replace(old, old.copy(revision = old.revision + 1)))
        }
        assertEquals(VaultPinProof.Unavailable, f.backend.openExisting(f.scope, f.pin()))
        assertEquals(0, f.reopened)
        // The hardware may already have completed; even unused proofs are wiped.
        assertTrue(f.lastProof!!.all { it == 0.toByte() })
    }

    @Test
    fun partialPinPublicationNeverAllowsRetryOrExistingRoomOpen() = withFixture { f ->
        Files.createFile(f.directory.resolve("pin.pending"))
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.SlotStorage), f.backend.provisionNew(f.scope, f.pin()))
        assertEquals(0, f.provisions)
        assertEquals(0, f.created)
        assertTrue(Files.exists(f.directory.resolve("pin.pending")))
        assertNull(f.lastFrame)
    }

    @Test
    fun aNativeLeaseForAnotherScopeIsRevokedBeforeItCanEscape() = withFixture { f ->
        f.completeSetup()
        val wrongHandle = Handle(f.scope.copy(keyGeneration = 2))
        f.invokeNative = { VaultPinProof.Granted(wrongHandle) }
        assertEquals(VaultPinProof.Unavailable, f.backend.openExisting(f.scope, f.pin()))
        assertTrue(wrongHandle.revoked)
        f.checkScrubbed()
    }

    @Test
    fun ciphertextReplacedDuringPinVerificationNeverReachesNative() = withFixture { f ->
        f.completeSetup()
        f.onDerive = {
            val file = f.directory.resolve("pin.v1")
            val incorrect = Files.readAllBytes(file).apply { this[lastIndex] = (this[lastIndex].toInt() xor 1).toByte() }
            Files.write(file, incorrect)
        }
        assertEquals(VaultPinProof.Unavailable, f.backend.openExisting(f.scope, f.pin()))
        assertEquals(0, f.reopened)
        assertTrue(f.lastProof!!.all { it == 0.toByte() })
    }

    @Test
    fun uncertainAliasProvisioningLeavesAPersistentPinClaim() = withFixture { f ->
        f.newHardwareFailure = true
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.DeviceKey), f.backend.provisionNew(f.scope, f.pin()))
        assertTrue(Files.exists(f.directory.resolve("pin.pending")))
        assertFalse(Files.exists(f.directory.resolve("pin.v1")))
        f.newHardwareFailure = false
        assertEquals(VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.SlotStorage), f.backend.provisionNew(f.scope, f.pin()))
        assertEquals(1, f.provisions)
        assertEquals(0, f.created)
    }

    private fun withFixture(test: (Fixture) -> Unit) {
        val root = Files.createTempDirectory("synthetic-uia-pin")
        try { test(Fixture(root)) } finally { root.toFile().deleteRecursively() }
    }

    private class Fixture(root: Path) {
        private val hostSync = VaultPinDirectorySync { dir -> FileChannel.open(dir, READ).use { it.force(true) } }
        val store = VaultPinDiskStateStore.createNew(root, hostSync)
        val scope = store.scope
        val directory = root.resolve("harw-vault-pin-v1").resolve(scope.vaultId.toString())
        var derivations = 0
        var provisions = 0
        var opens = 0
        var proofs = 0
        var created = 0
        var reopened = 0
        var lastFrame: ByteBuffer? = null
        var lastProof: ByteArray? = null
        var lastBorrowedRoot: ByteArray? = null
        var syntheticRoot: ByteArray = ByteArray(32)
        var readHardwareFailure: VaultPinFailure? = null
        var newHardwareFailure = false
        var onDerive: () -> Unit = {}
        var invokeNative: () -> VaultPinProof = { VaultPinProof.Granted(Handle(scope)) }
        private val fixtureKey = SecretKeySpec(ByteArray(32) { 5 }, "HmacSHA256")
        private fun hmac(context: ByteArray, material: ByteArray): ByteArray = Mac.getInstance("HmacSHA256").run {
            init(fixtureKey)
            update(context)
            doFinal(material)
        }
        private val device = object : UiaRoomPinDevice {
            override fun authenticate(context: ByteArray, stretchedPin: ByteArray): ByteArray = hmac(context, stretchedPin)

            override fun enrollmentProof(scope: VaultPinScope, root: ByteArray): ByteArray {
                proofs++
                lastBorrowedRoot = root
                return hmac(uiaRoomDeviceMacContext(scope), root).also { lastProof = it }
            }
        }
        private val devices = object : UiaRoomPinDevices {
            override fun provisionNew(scope: VaultPinScope): UiaRoomPinDevice {
                assertTrue(Files.exists(directory.resolve("pin.pending")))
                provisions++
                if (newHardwareFailure) throw VaultPinException(VaultPinFailure.Unavailable)
                return device
            }
            override fun openExisting(scope: VaultPinScope): UiaRoomPinDevice {
                opens++
                readHardwareFailure?.let { throw VaultPinException(it) }
                return device
            }
        }

        private val kdf = VaultPinKdf { pin, salt ->
            derivations++
            onDerive()
            MessageDigest.getInstance("SHA-256").run { update(salt); digest(pin) }
        }

        private val native = object : UiaRoomPinNative {
            override fun provisionNew(frame: ByteBuffer): VaultPinProof {
                created++
                return verifyFrame(frame)
            }
            override fun openExisting(frame: ByteBuffer): VaultPinProof {
                reopened++
                return verifyFrame(frame)
            }
        }

        val backend = UiaRoomPinBackend(store, kdf, devices, native)

        init {
            val start = store.read()
            assertTrue(store.replace(start, start.copy(revision = 1, registration = VaultPinRegistration.Provisioning)))
        }

        fun completeSetup() {
            assertTrue(backend.provisionNew(scope, pin()) is VaultPinProof.Granted)
            val pending = store.read()
            val ready = pending.copy(revision = 2, registration = VaultPinRegistration.Ready())
            assertTrue(store.replace(pending, ready))
            val charged = VaultPinRegistration.Ready(VaultPinDelay(1, VaultPinTime(7, 1)))
            assertTrue(store.replace(ready, ready.copy(revision = 3, registration = charged)))
        }

        fun pin() = "078631029".encodeToByteArray()

        fun checkScrubbed() {
            assertTrue((0 until 89).all { lastFrame!!.get(it) == 0.toByte() })
            assertTrue(lastProof!!.all { it == 0.toByte() })
            assertTrue(lastBorrowedRoot!!.all { it == 0.toByte() })
        }

        private fun verifyFrame(frame: ByteBuffer): VaultPinProof {
            assertTrue(frame.isDirect)
            assertFalse(frame.isReadOnly)
            assertEquals(89, frame.capacity())
            assertEquals(1.toByte(), frame.get(0))
            val data = frame.duplicate().order(ByteOrder.BIG_ENDIAN)
            assertEquals(scope.vaultId, UUID(data.getLong(1), data.getLong(9)))
            assertEquals(scope.keyGeneration, data.getLong(17))
            val root = ByteArray(32) { frame.get(25 + it) }
            syntheticRoot.fill(0)
            syntheticRoot = root
            val proof = ByteArray(32) { frame.get(57 + it) }
            val domain = "harw/android/uia/enrollment-device-mac/v1".toByteArray(Charsets.US_ASCII)
            val expectedContext = domain + ByteBuffer.allocate(24).order(ByteOrder.BIG_ENDIAN)
                .putLong(scope.vaultId.mostSignificantBits)
                .putLong(scope.vaultId.leastSignificantBits)
                .putLong(scope.keyGeneration).array()
            assertArrayEquals(hmac(expectedContext, root), proof)
            proof.fill(0)
            lastFrame = frame
            return invokeNative()
        }
    }

    private class Handle(override val scope: VaultPinScope) : VaultPinSession {
        var revoked = false
            private set
        override fun revoke() { revoked = true }
    }
}

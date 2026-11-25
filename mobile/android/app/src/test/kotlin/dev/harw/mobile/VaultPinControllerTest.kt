package dev.harw.mobile

import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class VaultPinControllerTest {
    private val scope = VaultPinScope(UUID(17, 19), 1)

    @Test
    fun createsOnlyWithConfirmationAndDurableFreshClaim() {
        val fixture = Fixture(VaultPinRegistration.Fresh)
        val wrong = digits("71828393")
        val first = digits()
        assertEquals(VaultPinAction.ConfirmationMismatch, fixture.controller.create(first, wrong))
        assertWiped(first, wrong)
        assertEquals(0, fixture.provisions)
        val entered = digits()
        val confirm = digits()
        assertEquals(VaultPinAction.Unlocked, fixture.controller.create(entered, confirm))
        assertWiped(entered, confirm)
        assertEquals(1, fixture.provisions)
        assertEquals(VaultPinRegistration.Ready(), fixture.store.read().registration)
        assertSame(fixture.handle, fixture.controller.session())
        fixture.controller.relock()
        assertTrue(fixture.handle.revoked)
        assertNull(fixture.controller.session())
        assertEquals(VaultPinAction.AlreadyEnrolled, fixture.controller.create(digits(), digits()))
        assertEquals(1, fixture.provisions)
    }

    @Test
    fun interruptedCreationStaysPendingAndIsNeverRetriedOrSilentlyReplaced() {
        val fixture = Fixture(VaultPinRegistration.Fresh)
        fixture.provide = { VaultPinProof.Unavailable }
        assertEquals(VaultPinAction.Unavailable, fixture.controller.create(digits(), digits()))
        val restart = fixture.restarted()
        val recovery = VaultPinAction.RecoveryRequired(VaultPinRecovery.InterruptedEnrollment)
        assertEquals(recovery, restart.create(digits(), digits()))
        assertEquals(recovery, restart.unlock(digits()))
        assertEquals(1, fixture.provisions)
        assertEquals(0, fixture.opens)
    }

    @Test
    fun failedAttemptsReserveDelayBeforeCryptoAndSurviveRestart() {
        val fixture = Fixture()
        fixture.prove = { VaultPinProof.Rejected }
        val initial = digits()
        assertEquals(VaultPinAction.Rejected, fixture.controller.unlock(initial))
        assertWiped(initial)
        assertEquals(1, fixture.opens)
        val restart = fixture.restarted()
        val blocked = digits()
        assertEquals(VaultPinAction.Delayed(2_000), restart.unlock(blocked))
        assertWiped(blocked)
        assertEquals(1, fixture.opens)
        fixture.now = VaultPinTime(7, 2_100)
        assertEquals(VaultPinAction.Rejected, restart.unlock(digits()))
        assertEquals(2, fixture.opens)
        assertEquals(VaultPinAction.Delayed(4_000), fixture.restarted().unlock(digits()))
        assertEquals(2, fixture.store.read().asReady().delay?.strike)
    }

    @Test
    fun rebootAndSameBootClockRollbackCannotEraseDelay() {
        val fixture = Fixture()
        fixture.prove = { VaultPinProof.Rejected }
        assertEquals(VaultPinAction.Rejected, fixture.controller.unlock(digits()))
        fixture.now = VaultPinTime(8, 10)
        assertEquals(VaultPinAction.Delayed(2_000), fixture.restarted().unlock(digits()))
        assertEquals(fixture.now, fixture.store.read().asReady().delay?.started)
        fixture.now = VaultPinTime(8, 3)
        assertEquals(VaultPinAction.Delayed(2_000), fixture.restarted().unlock(digits()))
        assertEquals(fixture.now, fixture.store.read().asReady().delay?.started)
        assertEquals(1, fixture.opens)
    }

    @Test
    fun missingOrInvalidatedKeyNeverProvisionsAndDoesNotUnlock() {
        for (reason in listOf(VaultPinRecovery.MissingKey, VaultPinRecovery.InvalidatedKey)) {
            val fixture = Fixture()
            fixture.prove = { VaultPinProof.RecoveryRequired(reason) }
            assertEquals(VaultPinAction.RecoveryRequired(reason), fixture.controller.unlock(digits()))
            assertNull(fixture.controller.session())
            assertEquals(0, fixture.provisions)
            assertEquals(VaultPinAction.AlreadyEnrolled, fixture.controller.create(digits(), digits()))
        }
    }

    @Test
    fun storageFailureEitherStopsBeforeBackendOrRevokesAuthenticatedHandle() {
        val before = Fixture()
        before.store.refuseCommits = true
        assertEquals(VaultPinAction.Unavailable, before.controller.unlock(digits()))
        assertEquals(0, before.opens)
        val after = Fixture()
        after.prove = {
            after.store.refuseCommits = true
            VaultPinProof.Granted(after.handle)
        }
        assertEquals(VaultPinAction.Unavailable, after.controller.unlock(digits()))
        assertTrue(after.handle.revoked)
        assertNull(after.controller.session())
        assertEquals(1, after.store.read().asReady().delay?.strike)
    }

    @Test
    fun verifiedUnlockResetsThrottleButWrongScopeIsRevoked() {
        val fixture = Fixture()
        fixture.prove = { VaultPinProof.Rejected }
        assertEquals(VaultPinAction.Rejected, fixture.controller.unlock(digits()))
        fixture.now = VaultPinTime(7, 2_100)
        val other = Handle(scope.copy(keyGeneration = 2))
        fixture.prove = { VaultPinProof.Granted(other) }
        assertEquals(VaultPinAction.Unavailable, fixture.controller.unlock(digits()))
        assertTrue(other.revoked)
        assertNull(fixture.controller.session())
        fixture.now = VaultPinTime(7, 6_100)
        fixture.prove = { VaultPinProof.Granted(fixture.handle) }
        assertEquals(VaultPinAction.Unlocked, fixture.controller.unlock(digits()))
        assertEquals(VaultPinRegistration.Ready(), fixture.store.read().registration)
        assertFalse(fixture.handle.revoked)
    }

    @Test
    fun relockFencesLateAuthenticationAndAvoidsASecondBackendCall() {
        val fixture = Fixture()
        val entered = CountDownLatch(1)
        val returnProof = CountDownLatch(1)
        fixture.prove = {
            entered.countDown()
            check(returnProof.await(5, TimeUnit.SECONDS))
            VaultPinProof.Granted(fixture.handle)
        }
        val worker = Executors.newSingleThreadExecutor()
        try {
            val pending = worker.submit<VaultPinAction> { fixture.controller.unlock(digits()) }
            assertTrue(entered.await(5, TimeUnit.SECONDS))
            fixture.controller.relock()
            assertNull(fixture.controller.session())
            assertEquals(VaultPinAction.Busy, fixture.controller.unlock(digits()))
            returnProof.countDown()
            assertEquals(VaultPinAction.LockedDuringAttempt, pending.get(5, TimeUnit.SECONDS))
            assertTrue(fixture.handle.revoked)
            assertNull(fixture.controller.session())
            assertEquals(1, fixture.opens)
        } finally {
            returnProof.countDown()
            worker.shutdownNow()
        }
    }

    @Test
    fun rejectsAliasedConfirmationAndKeepsUnknownOrForeignScopeLocked() {
        val fixture = Fixture(VaultPinRegistration.Fresh)
        val reused = digits()
        assertEquals(VaultPinAction.ConfirmationMismatch, fixture.controller.create(reused, reused))
        assertWiped(reused)
        assertEquals(0, fixture.provisions)
        val foreign = fixture.restarted(scope.copy(vaultId = UUID(23, 29)))
        assertEquals(VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.Claim), foreign.create(digits(), digits()))
        fixture.store.refuseReads = true
        assertEquals(VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.Claim), fixture.controller.create(digits(), digits()))
        assertEquals(0, fixture.provisions)
    }

    @Test
    fun unlockCannotOverlapTheOldSessionsSynchronousRevoke() {
        val fixture = Fixture()
        val revokeStarted = CountDownLatch(1)
        val allowRevoke = CountDownLatch(1)
        val handle = Handle(scope) {
            revokeStarted.countDown()
            check(allowRevoke.await(5, TimeUnit.SECONDS))
        }
        fixture.prove = { VaultPinProof.Granted(handle) }
        assertEquals(VaultPinAction.Unlocked, fixture.controller.unlock(digits()))
        val worker = Executors.newSingleThreadExecutor()
        try {
            val revoking = worker.submit { fixture.controller.relock() }
            assertTrue(revokeStarted.await(5, TimeUnit.SECONDS))
            assertNull(fixture.controller.session())
            assertEquals(VaultPinAction.Busy, fixture.controller.unlock(digits()))
            assertEquals(1, fixture.opens)
            allowRevoke.countDown()
            revoking.get(5, TimeUnit.SECONDS)
            assertTrue(handle.revoked)
        } finally {
            allowRevoke.countDown()
            worker.shutdownNow()
        }
    }

    @Test
    fun aBackendRevocationFailureKeepsControllerDisabled() {
        val fixture = Fixture()
        fixture.prove = { VaultPinProof.Granted(Handle(scope) { error("synthetic revoke failure") }) }
        assertEquals(VaultPinAction.Unlocked, fixture.controller.unlock(digits()))
        fixture.controller.relock()
        assertNull(fixture.controller.session())
        assertEquals(VaultPinAction.Unavailable, fixture.controller.unlock(digits()))
        assertEquals(1, fixture.opens)
    }

    private fun digits(text: String = "078631029") = text.encodeToByteArray()

    private fun assertWiped(vararg pin: ByteArray) = pin.forEach { assertTrue(it.all { byte -> byte == 0.toByte() }) }

    private class Handle(override val scope: VaultPinScope, private val onRevoke: () -> Unit = {}) : VaultPinSession {
        @Volatile var revoked = false
        override fun revoke() {
            onRevoke()
            revoked = true
        }
    }

    private fun VaultPinRecord.asReady() = registration as VaultPinRegistration.Ready

    private inner class Fixture(registration: VaultPinRegistration = VaultPinRegistration.Ready()) {
        val store = FakeStore(VaultPinRecord(scope, 0, registration))
        var now = VaultPinTime(7, 100)
        val handle = Handle(scope)
        var provisions = 0
        var opens = 0
        var provide: () -> VaultPinProof = { VaultPinProof.Granted(handle) }
        var prove: () -> VaultPinProof = { VaultPinProof.Granted(handle) }
        private val backend = object : VaultPinBackend {
            override fun provisionNew(scope: VaultPinScope, pin: ByteArray): VaultPinProof {
                assertEquals(this@VaultPinControllerTest.scope, scope)
                assertSame(VaultPinRegistration.Provisioning, store.read().registration)
                provisions++
                return provide()
            }
            override fun openExisting(scope: VaultPinScope, pin: ByteArray): VaultPinProof {
                assertEquals(this@VaultPinControllerTest.scope, scope)
                assertTrue((store.read().registration as VaultPinRegistration.Ready).delay != null)
                opens++
                return prove()
            }
        }
        val controller = restarted()
        fun restarted(requested: VaultPinScope = scope) = VaultPinController(requested, store, VaultPinClock { now }, backend)
    }

    private class FakeStore(private var record: VaultPinRecord) : VaultPinStateStore {
        @Volatile var refuseCommits = false
        @Volatile var refuseReads = false
        @Synchronized override fun read(): VaultPinRecord {
            check(!refuseReads)
            return record
        }
        @Synchronized override fun replace(expected: VaultPinRecord, next: VaultPinRecord): Boolean {
            if (refuseCommits || record != expected) return false
            require(next.scope == expected.scope && next.revision == expected.revision + 1)
            record = next
            return true
        }
    }
}

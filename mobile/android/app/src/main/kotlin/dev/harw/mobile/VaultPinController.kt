package dev.harw.mobile

import java.security.MessageDigest
import java.util.concurrent.CancellationException

internal const val MaxVaultPinStrikes = 10
private const val DelayBaseMillis = 2_000L
private const val DelayCeilingMillis = 900_000L

/** A stable trusted platform boot identity and a monotonic clock that survives process restart. */
internal data class VaultPinTime(val boot: Long, val elapsedMillis: Long) {
    init { require(boot >= 0 && elapsedMillis >= 0) }
}

internal fun interface VaultPinClock {
    /** Throw if no trustworthy reading is available. Never substitute wall time. */
    fun read(): VaultPinTime
}

internal data class VaultPinDelay(val strike: Int, val started: VaultPinTime) {
    init { require(strike in 1..MaxVaultPinStrikes) }

    val durationMillis: Long
        get() = (DelayBaseMillis shl (strike - 1)).coerceAtMost(DelayCeilingMillis)
}

internal sealed interface VaultPinRegistration {
    /** Validated pristine namespace, not a missing/unreadable state file for an existing vault. */
    data object Fresh : VaultPinRegistration
    data object Provisioning : VaultPinRegistration
    data class Recovering(val delay: VaultPinDelay) : VaultPinRegistration
    data class Ready(val delay: VaultPinDelay? = null) : VaultPinRegistration
}

internal data class VaultPinRecord(
    val scope: VaultPinScope,
    val revision: Long,
    val registration: VaultPinRegistration,
) {
    init { require(revision >= 0) }
}

/** Metadata only. Must be private to one vault; no PIN, verifier, key, handle, or content. */
internal interface VaultPinStateStore {
    fun read(): VaultPinRecord

    /**
     * Compare-and-set the exact record and durably replace it (or throw/return false).
     * Must never report success before the replacement survives a crash. An adapter
     * must fail closed on corrupt/uncertain state; no empty fallback or honest-rollback claim.
     */
    fun replace(expected: VaultPinRecord, next: VaultPinRecord): Boolean
}

internal enum class VaultPinRecovery { InterruptedEnrollment, MissingKey, InvalidatedKey, DamagedVault }

/** Fixed public diagnostics. Do not include the exception, path, identifier or key status details. */
internal enum class VaultPinEnrollmentStage(val code: String) {
    Profile("PIN-01"), Claim("PIN-02"), DeviceKey("PIN-03"), Derivation("PIN-04"),
    SlotSeal("PIN-05"), SlotStorage("PIN-06"), SlotVerify("PIN-07"), DeviceProof("PIN-08"),
    NativeBridge("PIN-09"), NativeApp("PIN-10"), NativeScope("PIN-11"),
    NativeRoomDirectory("PIN-12"), NativeRoomWrite("PIN-13"), NativeRoomVerify("PIN-14"),
    FinalLedger("PIN-15"), RetryLedger("PIN-16"), AssistantHandoff("PIN-17"),
}

/** An actual revocable native authority, not a UI "unlocked" flag. Never serialized. */
internal interface VaultPinSession {
    val scope: VaultPinScope
    /** Synchronous, idempotent, no-throw revocation covering all copies and in-flight work. */
    fun revoke()
}

internal sealed interface VaultPinProof {
    class Granted(val session: VaultPinSession) : VaultPinProof
    data object Rejected : VaultPinProof
    data class RecoveryRequired(val reason: VaultPinRecovery) : VaultPinProof
    data class EnrollmentFailed(val stage: VaultPinEnrollmentStage) : VaultPinProof
    data object Unavailable : VaultPinProof
}

internal interface VaultPinBackend {
    /** Only invoked after an explicit, durably claimed fresh enrollment. Inputs are borrowed. */
    fun provisionNew(scope: VaultPinScope, pin: ByteArray): VaultPinProof

    /** Existing written room only; never create, replace, or unlock another scope. */
    fun resumeEnrollment(scope: VaultPinScope, pin: ByteArray): VaultPinProof =
        VaultPinProof.RecoveryRequired(VaultPinRecovery.InterruptedEnrollment)

    /**
     * Authenticate the existing slot and open the real vault. Must never provision or replace.
     * A backend that throws must revoke any handle it created before propagating the error.
     */
    fun openExisting(scope: VaultPinScope, pin: ByteArray): VaultPinProof
}

internal sealed interface VaultPinAction {
    /** For presentation only; read the scoped native handle with [VaultPinController.session]. */
    data object Unlocked : VaultPinAction
    data object AlreadyUnlocked : VaultPinAction
    data object AlreadyEnrolled : VaultPinAction
    data object NeedsEnrollment : VaultPinAction
    data object InvalidPin : VaultPinAction
    data object ConfirmationMismatch : VaultPinAction
    data object Rejected : VaultPinAction
    data object Busy : VaultPinAction
    data object LockedDuringAttempt : VaultPinAction
    data object Unavailable : VaultPinAction
    data class Delayed(val remainingMillis: Long) : VaultPinAction
    data class RecoveryRequired(val reason: VaultPinRecovery) : VaultPinAction
    data class EnrollmentFailed(val stage: VaultPinEnrollmentStage) : VaultPinAction
}

/**
 * Dormant state machine. A single owner per vault must run create/unlock on a worker and
 * relock on lifecycle transitions. Storage, clock, authority and recovery stay separate.
 * Persistent backoff is app defense-in-depth, not Gatekeeper or rollback-resistant protection.
 */
internal class VaultPinController(
    private val scope: VaultPinScope,
    private val store: VaultPinStateStore,
    private val clock: VaultPinClock,
    private val backend: VaultPinBackend,
) {
    private val guard = Any()
    private var fence = Any()
    private var busy = false
    private var revoking = false
    private var revokedWithError = false
    private var live: VaultPinSession? = null

    fun session(): VaultPinSession? = synchronized(guard) { live }

    fun relock() {
        val previous = synchronized(guard) {
            fence = Any()
            val current = live
            if (current != null) {
                live = null
                revoking = true
            }
            current
        } ?: return
        try {
            revokeOrDisable(previous)
        } finally {
            synchronized(guard) { revoking = false }
        }
    }

    fun create(pin: ByteArray, confirmation: ByteArray): VaultPinAction = execute(pin, confirmation) { ticket ->
        if (!validPin(pin) || !validPin(confirmation)) return@execute VaultPinAction.InvalidPin
        if (pin === confirmation || !MessageDigest.isEqual(pin, confirmation)) {
            return@execute VaultPinAction.ConfirmationMismatch
        }
        confirmation.fill(0)
        try {
            val record = read()
            when (record.registration) {
                VaultPinRegistration.Provisioning, is VaultPinRegistration.Recovering -> return@execute interruptedEnrollment()
                is VaultPinRegistration.Ready -> return@execute VaultPinAction.AlreadyEnrolled
                VaultPinRegistration.Fresh -> Unit
            }
            val claimed = replace(record, VaultPinRegistration.Provisioning)
                ?: return@execute VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.Claim)
            if (!isCurrent(ticket)) return@execute VaultPinAction.LockedDuringAttempt
            finish(ticket, claimed, backend.provisionNew(scope, pin))
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.Claim)
        }
    }

    fun resumeEnrollment(pin: ByteArray): VaultPinAction = execute(pin) { ticket ->
        if (!validPin(pin)) return@execute VaultPinAction.InvalidPin
        try {
            val record = read()
            val previous = when (val registration = record.registration) {
                VaultPinRegistration.Provisioning -> null
                is VaultPinRegistration.Recovering -> registration.delay
                VaultPinRegistration.Fresh, is VaultPinRegistration.Ready -> return@execute interruptedEnrollment()
            }
            chargedPinAttempt(
                ticket, record, previous, { delay -> VaultPinRegistration.Recovering(delay) },
                { backend.resumeEnrollment(scope, pin) },
            )
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.RetryLedger)
        }
    }

    fun unlock(pin: ByteArray): VaultPinAction = unlockUsing(pin) { backend.openExisting(scope, pin) }

    /** Explicit opt-in uses exactly the ordinary fresh PIN throttle, never a cached live session. */
    fun unlockAndEnableBiometric(
        pin: ByteArray,
        grant: AndroidVaultBiometricSlot.EnrollmentGrant,
    ): VaultPinAction {
        return try {
            val uia = backend as? UiaRoomPinBackend ?: return VaultPinAction.Unavailable
            unlockUsing(pin) { uia.openExistingForBiometricOptIn(scope, pin, grant) }
        } finally {
            pin.fill(0)
            grant.close()
        }
    }

    /** A disabled optional key cannot be a PIN failure or a route to the PIN alias. */
    fun unlockAndDisableBiometric(
        pin: ByteArray,
        slot: AndroidVaultBiometricSlot,
        onDisabled: (VaultBiometricAvailability) -> Unit,
    ): VaultPinAction {
        return try {
            val uia = backend as? UiaRoomPinBackend ?: return VaultPinAction.Unavailable
            unlockUsing(pin) {
                uia.openExistingForBiometricDisable(scope, pin) { requested, pinnedPin ->
                    val result = try {
                        slot.disableAfterAuthenticatedPin(requested, pinnedPin)
                    } catch (_: VaultBiometricException) {
                        VaultBiometricAvailability.Unavailable
                    }
                    onDisabled(result)
                }
            }
        } finally { pin.fill(0) }
    }

    /** Adopt a scoped real CryptoObject proof without writing, clearing or charging PIN delay. */
    fun unlockWithBiometric(grant: AndroidVaultBiometricSlot.UnlockGrant): VaultPinAction {
        return try {
            val uia = backend as? UiaRoomPinBackend ?: return VaultPinAction.Unavailable
            execute { ticket ->
                val record = read()
                if (record.registration !is VaultPinRegistration.Ready) return@execute VaultPinAction.Unavailable
                if (!isCurrent(ticket)) return@execute VaultPinAction.LockedDuringAttempt
                when (val proof = uia.openExistingWithBiometricGrant(scope, grant)) {
                    VaultPinProof.Rejected -> VaultPinAction.Rejected
                    VaultPinProof.Unavailable -> VaultPinAction.Unavailable
                    is VaultPinProof.RecoveryRequired -> VaultPinAction.RecoveryRequired(proof.reason)
                    is VaultPinProof.EnrollmentFailed -> VaultPinAction.EnrollmentFailed(proof.stage)
                    is VaultPinProof.Granted -> {
                        val candidate = proof.session
                        var adopted = false
                        try {
                            // The backend compared both slots through native open under the
                            // state lock. Reject an intervening change rather than altering it.
                            if (candidate.scope != scope || read() != record) return@execute VaultPinAction.Unavailable
                            synchronized(guard) {
                                if (ticket !== fence) VaultPinAction.LockedDuringAttempt else {
                                    live = candidate
                                    adopted = true
                                    VaultPinAction.Unlocked
                                }
                            }
                        } finally {
                            if (!adopted) revokeOrDisable(candidate)
                        }
                    }
                }
            }
        } finally { grant.close() }
    }

    private fun unlockUsing(pin: ByteArray, authenticate: () -> VaultPinProof): VaultPinAction = execute(pin) { ticket ->
        if (!validPin(pin)) return@execute VaultPinAction.InvalidPin
        val record = read()
        val ready = when (val registration = record.registration) {
            VaultPinRegistration.Fresh -> return@execute VaultPinAction.NeedsEnrollment
            VaultPinRegistration.Provisioning, is VaultPinRegistration.Recovering -> return@execute interruptedEnrollment()
            is VaultPinRegistration.Ready -> registration
        }
        chargedPinAttempt(ticket, record, ready.delay, { VaultPinRegistration.Ready(it) }, authenticate)
    }

    private fun chargedPinAttempt(
        ticket: Any,
        record: VaultPinRecord,
        old: VaultPinDelay?,
        charge: (VaultPinDelay) -> VaultPinRegistration,
        authenticate: () -> VaultPinProof,
    ): VaultPinAction {
        val unavailable = if (record.registration is VaultPinRegistration.Ready) VaultPinAction.Unavailable
            else VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.RetryLedger)
        val now = clock.read()
        if (old != null) {
            if (now.boot != old.started.boot || now.elapsedMillis < old.started.elapsedMillis) {
                replace(record, charge(old.copy(started = now))) ?: return unavailable
                return VaultPinAction.Delayed(old.durationMillis)
            }
            val remaining = old.durationMillis - (now.elapsedMillis - old.started.elapsedMillis)
            if (remaining > 0) return VaultPinAction.Delayed(remaining)
        }

        // Charge the durable delay before even opening the PIN slot or running the KDF.
        val strike = ((old?.strike ?: 0) + 1).coerceAtMost(MaxVaultPinStrikes)
        val reserved = replace(record, charge(VaultPinDelay(strike, now))) ?: return unavailable
        if (!isCurrent(ticket)) return VaultPinAction.LockedDuringAttempt
        return finish(ticket, reserved, authenticate())
    }

    private inline fun execute(vararg pins: ByteArray, operation: (Any) -> VaultPinAction): VaultPinAction {
        try {
            val ticket = synchronized(guard) {
                if (revokedWithError) return VaultPinAction.Unavailable
                if (live != null) return VaultPinAction.AlreadyUnlocked
                if (busy || revoking) return VaultPinAction.Busy
                busy = true
                fence
            }
            return try {
                operation(ticket)
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                VaultPinAction.Unavailable
            } finally {
                synchronized(guard) { busy = false }
            }
        } finally {
            pins.forEach { it.fill(0) }
        }
    }

    private fun revokeOrDisable(session: VaultPinSession) {
        try {
            session.revoke()
        } catch (_: Exception) {
            // An adapter violating its revoke contract must not let another session open.
            synchronized(guard) { revokedWithError = true }
        }
    }

    private fun read(): VaultPinRecord = store.read().also {
        check(it.scope == scope)
    }

    private fun replace(record: VaultPinRecord, state: VaultPinRegistration): VaultPinRecord? {
        val next = record.copy(revision = Math.addExact(record.revision, 1), registration = state)
        return next.takeIf { store.replace(record, it) }
    }

    private fun isCurrent(ticket: Any): Boolean = synchronized(guard) { ticket === fence }

    private fun finish(ticket: Any, claimed: VaultPinRecord, proof: VaultPinProof): VaultPinAction = when (proof) {
        VaultPinProof.Rejected -> VaultPinAction.Rejected
        VaultPinProof.Unavailable -> VaultPinAction.Unavailable
        is VaultPinProof.RecoveryRequired -> VaultPinAction.RecoveryRequired(proof.reason)
        is VaultPinProof.EnrollmentFailed -> VaultPinAction.EnrollmentFailed(proof.stage)
        is VaultPinProof.Granted -> publish(ticket, claimed, proof.session)
    }

    private fun publish(ticket: Any, claimed: VaultPinRecord, candidate: VaultPinSession): VaultPinAction {
        var adopted = false
        try {
            if (!isCurrent(ticket)) return VaultPinAction.LockedDuringAttempt
            if (candidate.scope != scope) return VaultPinAction.Unavailable
            val unavailable = if (claimed.registration is VaultPinRegistration.Ready) VaultPinAction.Unavailable
                else VaultPinAction.EnrollmentFailed(VaultPinEnrollmentStage.FinalLedger)
            val installed = try {
                replace(claimed, VaultPinRegistration.Ready())
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                return unavailable
            }
            if (installed == null) return unavailable
            return synchronized(guard) {
                if (ticket !== fence) VaultPinAction.LockedDuringAttempt else {
                    live = candidate
                    adopted = true
                    VaultPinAction.Unlocked
                }
            }
        } finally {
            if (!adopted) revokeOrDisable(candidate)
        }
    }
}

private fun validPin(pin: ByteArray): Boolean = pin.size in 8..16 && pin.all { it >= 0x30 && it <= 0x39 }

private fun interruptedEnrollment() = VaultPinAction.RecoveryRequired(VaultPinRecovery.InterruptedEnrollment)

package dev.harw.mobile

import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.concurrent.CancellationException

private const val SecretBytes = 32
private const val FrameBytes = 89
private val DeviceDomain = "harw/android/uia/enrollment-device-mac/v1".toByteArray(Charsets.US_ASCII)

internal enum class UiaRoomPinMode { ProvisionNew, ResumeEnrollment, OpenExisting, OpenExistingBiometric }

internal class UiaRoomPinSlot(val record: VaultPinRecord, val ciphertext: ByteArray)
internal class UiaRoomPinSlotDamaged : IOException("UIA PIN ciphertext is unavailable")

/** Opaque transport; implementation must be authenticated native-only and scoped by the profile owner. */
internal interface UiaRoomPinNative {
    /**
     * A fresh private v1 direct frame is borrowed only for this synchronous call.
     * Do not copy it into an ordinary JNI array, UniFFI RustBuffer, log, or asynchronous task.
     * Must revoke any native handle on an exception and never infer recovery by making a new room.
     */
    fun provisionNew(frame: ByteBuffer): VaultPinProof

    fun openExisting(frame: ByteBuffer): VaultPinProof

    fun resumeEnrollment(frame: ByteBuffer): VaultPinProof = VaultPinProof.Unavailable

    fun openExistingBiometric(frame: ByteBuffer): VaultPinProof = VaultPinProof.Unavailable
}

internal interface UiaRoomPinDevice : VaultPinDeviceMac {
    /** Distinct fixed domain, same existing hardware slot scoped to this vault UUID. */
    fun enrollmentProof(scope: VaultPinScope, root: ByteArray): ByteArray
}

internal interface UiaRoomPinDevices {
    fun provisionNew(scope: VaultPinScope): UiaRoomPinDevice
    fun openExisting(scope: VaultPinScope): UiaRoomPinDevice
}

/** Must match Rust EnrollmentAuthority::device_mac_context byte for byte. */
internal fun uiaRoomDeviceMacContext(scope: VaultPinScope): ByteArray = ByteBuffer
    .allocate(DeviceDomain.size + 24)
    .order(ByteOrder.BIG_ENDIAN)
    .put(DeviceDomain)
    .putLong(scope.vaultId.mostSignificantBits)
    .putLong(scope.vaultId.leastSignificantBits)
    .putLong(scope.keyGeneration)
    .array()

/**
 * Dormant backend for the single selected profile's vault owner. Only VaultPinController
 * calls it: the journal phase is a second fence, not independent evidence of consent or PIN.
 * No JNI registry, profile lookup, routing, or lifecycle activation lives here.
 */
internal class UiaRoomPinBackend(
    private val store: VaultPinDiskStateStore,
    private val kdf: VaultPinKdf,
    private val devices: UiaRoomPinDevices,
    private val native: UiaRoomPinNative,
) : VaultPinBackend {
    private val randomness = SecureRandom()

    override fun provisionNew(scope: VaultPinScope, pin: ByteArray): VaultPinProof {
        var stage = VaultPinEnrollmentStage.SlotStorage
        return protect(scope, pin, UiaRoomPinMode.ProvisionNew, failureStage = { stage }) {
            lateinit var device: UiaRoomPinDevice
            lateinit var protector: VaultPinProtector
            val slot = store.installNewUiaPinSlot {
                stage = VaultPinEnrollmentStage.DeviceKey
                device = devices.provisionNew(scope)
                protector = VaultPinProtector(kdf, device)
                stage = VaultPinEnrollmentStage.SlotSeal
                val root = ByteArray(SecretBytes)
                val pinForSeal = pin.copyOf()
                try {
                    randomness.nextBytes(root)
                    check(root.any { it != 0.toByte() })
                    protector.seal(scope, pinForSeal, root).also { stage = VaultPinEnrollmentStage.SlotStorage }
                } finally {
                    root.fill(0)
                    pinForSeal.fill(0)
                }
            }
            // Authenticate only the *durable read-back*, never the in-memory seal.
            stage = VaultPinEnrollmentStage.SlotVerify
            unwrapAndDispatch(scope, pin, slot, protector, device, UiaRoomPinMode.ProvisionNew) { stage = it }
        }
    }

    override fun resumeEnrollment(scope: VaultPinScope, pin: ByteArray): VaultPinProof {
        val mode = UiaRoomPinMode.ResumeEnrollment
        var stage = VaultPinEnrollmentStage.SlotStorage
        return protect(scope, pin, mode, failureStage = { stage }) {
            val slot = store.readExistingUiaPinSlot(mode)
            try {
                stage = VaultPinEnrollmentStage.DeviceKey
                val device = devices.openExisting(scope)
                stage = VaultPinEnrollmentStage.SlotVerify
                unwrapAndDispatch(scope, pin, slot, VaultPinProtector(kdf, device), device, mode) { stage = it }
            } finally {
                slot.ciphertext.fill(0)
            }
        }
    }

    override fun openExisting(scope: VaultPinScope, pin: ByteArray): VaultPinProof =
        openWithPin(scope, pin, enrollment = null)

    /** Called only by the controller's normal throttled PIN attempt for an explicit opt-in. */
    internal fun openExistingForBiometricOptIn(
        scope: VaultPinScope,
        pin: ByteArray,
        enrollment: AndroidVaultBiometricSlot.EnrollmentGrant,
    ): VaultPinProof = openWithPin(scope, pin, enrollment)

    /** Disable authority can only originate in the normal charged PIN path. */
    internal fun openExistingForBiometricDisable(
        scope: VaultPinScope,
        pin: ByteArray,
        disable: (VaultPinScope, UiaRoomPinSlot) -> Unit,
    ): VaultPinProof = openWithPin(scope, pin, enrollment = null, disable = disable)

    private fun openWithPin(
        scope: VaultPinScope,
        pin: ByteArray,
        enrollment: AndroidVaultBiometricSlot.EnrollmentGrant?,
        disable: ((VaultPinScope, UiaRoomPinSlot) -> Unit)? = null,
    ): VaultPinProof = protect(scope, pin, UiaRoomPinMode.OpenExisting) {
        val slot = store.readExistingUiaPinSlot()
        try {
            val device = devices.openExisting(scope)
            val protector = VaultPinProtector(kdf, device)
            unwrapAndDispatch(scope, pin, slot, protector, device, UiaRoomPinMode.OpenExisting, enrollment, disable)
        } finally {
            slot.ciphertext.fill(0)
        }
    }

    /**
     * Trusted, existing-only handoff for a real CryptoObject grant. It deliberately does not
     * schedule, charge, reset or synthesize a PIN attempt. A distinct existing-only native
     * path verifies the same scoped Ready record and durable biometric ciphertext.
     */
    internal fun openExistingWithBiometricGrant(
        scope: VaultPinScope,
        grant: AndroidVaultBiometricSlot.UnlockGrant,
    ): VaultPinProof {
        return try {
            if (scope != store.scope) return VaultPinProof.Unavailable
            val mode = UiaRoomPinMode.OpenExistingBiometric
            val slot = store.readExistingUiaPinSlot(mode)
            try {
                val device = devices.openExisting(scope)
                grant.withAuthenticatedRoot(scope, slot) { root, biometricCiphertext ->
                    dispatchAuthorizedRoot(scope, slot, device, mode, root, biometricCiphertext)
                }
            } finally {
                slot.ciphertext.fill(0)
            }
        } catch (failure: VaultPinException) {
            when (failure.failure) {
                VaultPinFailure.DeviceKeyMissing -> VaultPinProof.RecoveryRequired(VaultPinRecovery.MissingKey)
                VaultPinFailure.DeviceKeyInvalidated -> VaultPinProof.RecoveryRequired(VaultPinRecovery.InvalidatedKey)
                else -> VaultPinProof.Unavailable
            }
        } catch (failure: CancellationException) {
            throw failure
        } catch (_: Exception) {
            VaultPinProof.Unavailable
        } finally {
            grant.close()
        }
    }

    private fun unwrapAndDispatch(
        scope: VaultPinScope,
        pin: ByteArray,
        slot: UiaRoomPinSlot,
        protector: VaultPinProtector,
        device: UiaRoomPinDevice,
        mode: UiaRoomPinMode,
        enrollment: AndroidVaultBiometricSlot.EnrollmentGrant? = null,
        disable: ((VaultPinScope, UiaRoomPinSlot) -> Unit)? = null,
        onStage: (VaultPinEnrollmentStage) -> Unit = {},
    ): VaultPinProof {
        val consumedPin = pin.copyOf()
        var unreturnedSession: VaultPinSession? = null
        try {
            val result = protector.withUnsealed(scope, consumedPin, slot.ciphertext) { root ->
                val opened = dispatchAuthorizedRoot(scope, slot, device, mode, root, onStage = onStage)
                if (opened is VaultPinProof.Granted) {
                    unreturnedSession = opened.session
                    // Publishing an optional slot is independent of PIN authority and only happens
                    // after the existing sealed room itself was opened from the verified PIN root.
                    enrollment?.finishAfterAuthenticatedPin(scope, slot, root)
                    disable?.invoke(scope, slot)
                }
                opened
            }
            unreturnedSession = null
            return result
        } finally {
            try {
                unreturnedSession?.revoke()
            } finally {
                consumedPin.fill(0)
                slot.ciphertext.fill(0)
            }
        }
    }

    private fun dispatchAuthorizedRoot(
        scope: VaultPinScope,
        slot: UiaRoomPinSlot,
        device: UiaRoomPinDevice,
        mode: UiaRoomPinMode,
        root: ByteArray,
        biometricCiphertext: ByteArray? = null,
        onStage: (VaultPinEnrollmentStage) -> Unit = {},
    ): VaultPinProof {
        onStage(VaultPinEnrollmentStage.DeviceProof)
        check(root.size == SecretBytes && root.any { it != 0.toByte() })
        val deviceProof = device.enrollmentProof(scope, root)
        var unreturnedSession: VaultPinSession? = null
        try {
            check(deviceProof.size == SecretBytes && deviceProof.any { it != 0.toByte() })
            check(!MessageDigest.isEqual(deviceProof, root))
            onStage(VaultPinEnrollmentStage.NativeBridge)
            val opened = store.withNativeRoomAuthority(slot, mode, biometricCiphertext) {
                val frame = ByteBuffer.allocateDirect(FrameBytes).order(ByteOrder.BIG_ENDIAN)
                try {
                    frame.put(1.toByte())
                        .putLong(scope.vaultId.mostSignificantBits)
                        .putLong(scope.vaultId.leastSignificantBits)
                        .putLong(scope.keyGeneration)
                        .put(root)
                        .put(deviceProof)
                        .flip()
                    val result = when (mode) {
                        UiaRoomPinMode.ProvisionNew -> native.provisionNew(frame)
                        UiaRoomPinMode.ResumeEnrollment -> native.resumeEnrollment(frame)
                        UiaRoomPinMode.OpenExisting -> native.openExisting(frame)
                        UiaRoomPinMode.OpenExistingBiometric -> native.openExistingBiometric(frame)
                    }
                    if (result is VaultPinProof.Granted) {
                        unreturnedSession = result.session
                        check(result.session.scope == scope)
                    }
                    if (result == VaultPinProof.Rejected) VaultPinProof.Unavailable else result
                } finally {
                    frame.clear()
                    while (frame.hasRemaining()) frame.put(0.toByte())
                }
            }
            unreturnedSession = null
            return opened
        } finally {
            try {
                unreturnedSession?.revoke()
            } finally {
                deviceProof.fill(0)
            }
        }
    }

    private inline fun protect(
        scope: VaultPinScope,
        pin: ByteArray,
        mode: UiaRoomPinMode,
        noinline failureStage: (() -> VaultPinEnrollmentStage)? = null,
        work: () -> VaultPinProof,
    ): VaultPinProof {
        if (scope != store.scope) return VaultPinProof.Unavailable
        if (pin.size !in 8..16 || pin.any { it < 0x30 || it > 0x39 }) return VaultPinProof.Rejected
        return try {
            val proof = work()
            if (proof == VaultPinProof.Unavailable && failureStage != null) {
                VaultPinProof.EnrollmentFailed(failureStage())
            } else proof
        } catch (e: CancellationException) {
            throw e
        } catch (e: VaultPinException) {
            if (failureStage != null && (e.failure != VaultPinFailure.Rejected || mode == UiaRoomPinMode.ProvisionNew)) {
                return VaultPinProof.EnrollmentFailed(when (e.failure) {
                    VaultPinFailure.KdfUnavailable -> VaultPinEnrollmentStage.Derivation
                    VaultPinFailure.DeviceKeyMissing, VaultPinFailure.DeviceKeyInvalidated -> VaultPinEnrollmentStage.DeviceKey
                    else -> failureStage()
                })
            }
            when (e.failure) {
                VaultPinFailure.DeviceKeyMissing -> VaultPinProof.RecoveryRequired(VaultPinRecovery.MissingKey)
                VaultPinFailure.DeviceKeyInvalidated -> VaultPinProof.RecoveryRequired(VaultPinRecovery.InvalidatedKey)
                VaultPinFailure.InvalidSlot -> VaultPinProof.RecoveryRequired(VaultPinRecovery.DamagedVault)
                VaultPinFailure.Rejected ->
                    if (mode == UiaRoomPinMode.OpenExisting || mode == UiaRoomPinMode.ResumeEnrollment) VaultPinProof.Rejected
                    else VaultPinProof.RecoveryRequired(VaultPinRecovery.DamagedVault)
                VaultPinFailure.InvalidPin, VaultPinFailure.KdfUnavailable, VaultPinFailure.Unavailable -> VaultPinProof.Unavailable
            }
        } catch (_: UiaRoomPinSlotDamaged) {
            if (failureStage != null) VaultPinProof.EnrollmentFailed(VaultPinEnrollmentStage.SlotStorage)
            else VaultPinProof.RecoveryRequired(VaultPinRecovery.DamagedVault)
        } catch (_: Exception) {
            if (failureStage != null) VaultPinProof.EnrollmentFailed(failureStage()) else VaultPinProof.Unavailable
        }
    }

    companion object {
        /**
         * Only production constructor: native app PIN KDF and existing Android hardware alias.
         * Unreachable from startup or UI while the native owner is absent/disabled.
         */
        fun forAndroid(store: VaultPinDiskStateStore, native: UiaRoomPinNative): VaultPinBackend = UiaRoomPinBackend(
            store = store,
            kdf = VaultPinKdf(AndroidVaultPinKdf::deriveArgon2idV1),
            devices = object : UiaRoomPinDevices {
                override fun provisionNew(scope: VaultPinScope): UiaRoomPinDevice =
                    AndroidVaultPinMac.provisionNew(scope.vaultId)

                override fun openExisting(scope: VaultPinScope): UiaRoomPinDevice =
                    AndroidVaultPinMac.openExisting(scope.vaultId)
            },
            native = native,
        )
    }
}

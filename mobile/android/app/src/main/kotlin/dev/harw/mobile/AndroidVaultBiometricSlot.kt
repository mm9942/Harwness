package dev.harw.mobile

import android.app.Activity
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import android.os.Looper
import android.os.SystemClock
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

private const val BioStore = "AndroidKeyStore"
// Keystore2 key equality compares the platform key ID; unknown providers fail closed.
private const val BioKeystore2SecretKey = "android.security.keystore2.AndroidKeyStoreSecretKey"
private const val BioCipher = "AES/GCM/NoPadding"
private const val BioNonceBytes = 12
private const val BioRootBytes = 32
private const val BioTagBits = 128
private const val BioGrantMillis = 30_000L
private val BioAadDomain = "harw/android/uia/biometric-slot/v1".toByteArray(Charsets.US_ASCII)

internal const val VaultBiometricSlotBytes = 1 + BioNonceBytes + BioRootBytes + BioTagBits / 8

internal enum class VaultBiometricFailure { Unsupported, Unavailable, Invalidated, Cancelled, InterruptedOptIn, Revoked }

internal enum class VaultBiometricAvailability { Unavailable, Eligible, Enrolled, Interrupted, RevocationPending, Revoked }

/** No Android exception text, PIN or key material crosses this boundary. */
internal class VaultBiometricException(val failure: VaultBiometricFailure) : Exception(failure.name)

internal enum class VaultBiometricMode { OptIn, Unlock }

internal enum class VaultBiometricEnrollmentState { Pending, Enabled, Failed }


/** Object identity, not a secret, Boolean or authority by itself. Never persist or pass through UI saved state. */
internal class VaultBiometricPromptTicket internal constructor()

/**
 * Real optional strong biometric slot, bound to one registry-selected vault UUID/generation.
 * No UI/profile owner is registered by constructing this class. Keep the PIN route separate.
 */
internal class AndroidVaultBiometricSlot private constructor(
    private val application: HarwApplication,
    private val store: VaultPinDiskStateStore,
) : AutoCloseable {
    private val scope = store.scope
    private val guard = Any()
    private var closed = false
    private var preparing = false
    private var active: Operation? = null
    private var cancelledOptIn: OptInCancellation? = null

    private data class OptInCancellation(
        val ticket: VaultBiometricPromptTicket,
        val claim: ByteArray,
        val generatedKey: SecretKey,
    )

    private class Operation(
        val ticket: VaultBiometricPromptTicket,
        val mode: VaultBiometricMode,
        val cipher: Cipher,
        val claim: ByteArray?,
        val wire: ByteArray?,
        val generatedKey: SecretKey? = null,
    ) {
        val cancellation = CancellationSignal()
        var shown = false
        var approvedAt: Long? = null
        var consumed = false
    }

    /** Keep this capability only for the explicit opt-in PIN attempt. It holds no plaintext. */
    internal class EnrollmentGrant internal constructor(
        private val owner: AndroidVaultBiometricSlot,
        private val ticket: VaultBiometricPromptTicket,
    ) : AutoCloseable {
        @Volatile
        var state: VaultBiometricEnrollmentState = VaultBiometricEnrollmentState.Pending
            private set

        /** Called only while the actual co-owned PIN-unseal callback lends this root. */
        @Synchronized
        internal fun finishAfterAuthenticatedPin(scope: VaultPinScope, pinSlot: UiaRoomPinSlot, root: ByteArray) {
            if (state != VaultBiometricEnrollmentState.Pending) return
            state = try {
                owner.sealAfterPin(ticket, scope, pinSlot, root)
                VaultBiometricEnrollmentState.Enabled
            } catch (_: VaultBiometricException) {
                VaultBiometricEnrollmentState.Failed
            } finally {
                owner.cancel(ticket)
            }
        }

        @Synchronized
        override fun close() {
            owner.cancel(ticket)
            if (state == VaultBiometricEnrollmentState.Pending) state = VaultBiometricEnrollmentState.Failed
        }
    }

    /** One-use capability from the exact successful cryptographic Android prompt. Never returns a root. */
    internal class UnlockGrant internal constructor(
        private val owner: AndroidVaultBiometricSlot,
        private val ticket: VaultBiometricPromptTicket,
    ) : AutoCloseable {
        internal fun withAuthenticatedRoot(
            scope: VaultPinScope,
            pinnedSlot: UiaRoomPinSlot,
            consume: (ByteArray, ByteArray) -> VaultPinProof,
        ): VaultPinProof = owner.withRoot(ticket, scope, pinnedSlot, consume)

        override fun close() = owner.cancel(ticket)
    }

    /** Worker only, explicit user opt-in, after a complete mandatory PIN enrollment. */
    fun prepareOptIn(): VaultBiometricPromptTicket = prepare(VaultBiometricMode.OptIn) { ticket ->
        store.claimUiaBiometricOptIn { claim ->
            val key = provisionOptInKey()
            val cipher = Cipher.getInstance(BioCipher).apply { init(Cipher.ENCRYPT_MODE, key) }
            Operation(ticket, VaultBiometricMode.OptIn, cipher, claim, wire = null, generatedKey = key)
        }
    }

    /** Only inside the real existing-room PIN callback, never via a biometric grant. */
    internal fun disableAfterAuthenticatedPin(requested: VaultPinScope, pinSlot: UiaRoomPinSlot): VaultBiometricAvailability =
        biometricOperation {
            synchronized(guard) {
                if (requested != scope || closed || active != null || preparing) {
                    bioFail(VaultBiometricFailure.Unavailable)
                }
                store.revokeUiaBiometricSlot(pinSlot, resetForEnrollment = true) {
                    val keys = keyStore()
                    val exact = alias()
                    if (keys.containsAlias(exact)) keys.deleteEntry(exact)
                    !keys.containsAlias(exact)
                }
            }
        }

    internal fun availability(): VaultBiometricAvailability = try {
        val state = store.uiaBiometricAvailability()
        if (state == VaultBiometricAvailability.Enrolled &&
            runCatching { keyStore().getKey(alias(), null) }.getOrNull() == null
        ) VaultBiometricAvailability.Interrupted else state
    } catch (_: Exception) {
        VaultBiometricAvailability.Unavailable
    }

    /** Worker only. Existing ciphertext and exact existing alias only, never creates or replaces. */
    fun prepareUnlock(): VaultBiometricPromptTicket = prepare(VaultBiometricMode.Unlock) { ticket ->
        val wire = store.readUiaBiometricSlot()
        var handedOff = false
        try {
            val key = existingBiometricKey()
            val iv = wire.copyOfRange(1, 1 + BioNonceBytes)
            val cipher = Cipher.getInstance(BioCipher).apply {
                init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(BioTagBits, iv))
            }
            Operation(ticket, VaultBiometricMode.Unlock, cipher, claim = null, wire = wire).also { handedOff = true }
        } finally {
            if (!handedOff) wire.fill(0)
        }
    }

    /** Main thread. An approval is never the PIN proof needed to finish enrollment. */
    fun showOptIn(
        activity: Activity,
        ticket: VaultBiometricPromptTicket,
        result: (Result<EnrollmentGrant>) -> Unit,
    ) = show(activity, ticket, VaultBiometricMode.OptIn, { EnrollmentGrant(this, ticket) }, result)

    /** Main thread. A successful callback only creates a cancellable one-use cryptographic grant. */
    fun showUnlock(
        activity: Activity,
        ticket: VaultBiometricPromptTicket,
        result: (Result<UnlockGrant>) -> Unit,
    ) = show(activity, ticket, VaultBiometricMode.Unlock, { UnlockGrant(this, ticket) }, result)

    override fun close() {
        val old = synchronized(guard) {
            closed = true
            cancelledOptIn?.claim?.fill(0)
            cancelledOptIn = null
            active.also { active = null }
        }
        old?.let(::cancelOperation)
    }

    fun cancel(ticket: VaultBiometricPromptTicket) {
        val old = synchronized(guard) {
            active?.takeIf { it.ticket === ticket }?.also { active = null }
        }
        old?.let(::cancelOperation)
    }

    /**
     * Worker only. No cleanup capability is produced for timeouts, platform failures,
     * background cancellation, grant consumption, ambiguous writes, or old providers.
     */
    fun finishExplicitOptInCancellation(ticket: VaultBiometricPromptTicket) {
        if (Looper.myLooper() == Looper.getMainLooper()) return
        synchronized(guard) {
            val cancelled = cancelledOptIn?.takeIf { !closed && it.ticket === ticket } ?: return
            cancelledOptIn = null
            try {
                store.cancelUncommittedBiometricOptIn(cancelled.claim) {
                    // The store lock serializes every app provision/delete for this alias.
                    val generated = cancelled.generatedKey
                    if (Build.VERSION.SDK_INT < 31 || generated.javaClass.name != BioKeystore2SecretKey) {
                        return@cancelUncommittedBiometricOptIn false
                    }
                    val keys = keyStore()
                    val exact = alias()
                    val current = keys.getKey(exact, null) as? SecretKey
                        ?: return@cancelUncommittedBiometricOptIn false
                    if (current.javaClass != generated.javaClass || current != generated) {
                        return@cancelUncommittedBiometricOptIn false
                    }
                    // Recheck usability without processing plaintext. Android can report
                    // permanent invalidation when it initializes the operation, not at lookup.
                    requireStrongHardware(current)
                    Cipher.getInstance(BioCipher).init(Cipher.ENCRYPT_MODE, current)
                    keys.deleteEntry(exact)
                    !keys.containsAlias(exact)
                }
            } catch (_: Exception) {
                // Keep the claim and continue using the independent PIN.
            } finally {
                cancelled.claim.fill(0)
            }
        }
    }

    private fun cancelOperation(operation: Operation) {
        try {
            operation.cancellation.cancel()
        } catch (_: Exception) {
            // Owner already invalidates the capability even if the platform cannot dismiss.
        } finally {
            if (!operation.consumed) {
                operation.claim?.fill(0)
                operation.wire?.fill(0)
            }
        }
    }

    private fun prepare(
        mode: VaultBiometricMode,
        create: (VaultBiometricPromptTicket) -> Operation,
    ): VaultBiometricPromptTicket = biometricOperation {
        if (Build.VERSION.SDK_INT < 30) bioFail(VaultBiometricFailure.Unsupported)
        if (Looper.myLooper() == Looper.getMainLooper()) bioFail(VaultBiometricFailure.Unavailable)
        if (application.getSystemService(BiometricManager::class.java)
                ?.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) != BiometricManager.BIOMETRIC_SUCCESS
        ) bioFail(VaultBiometricFailure.Unsupported)

        synchronized(guard) {
            if (closed || active != null || preparing || cancelledOptIn != null) bioFail(VaultBiometricFailure.Unavailable)
            preparing = true
        }
        try {
            val ticket = VaultBiometricPromptTicket()
            val operation = create(ticket)
            check(operation.mode == mode)
            val accepted = synchronized(guard) {
                if (closed) false else {
                    active = operation
                    true
                }
            }
            if (!accepted) {
                cancelOperation(operation)
                bioFail(VaultBiometricFailure.Cancelled)
            }
            ticket
        } finally {
            synchronized(guard) { preparing = false }
        }
    }

    private fun <T> show(
        activity: Activity,
        ticket: VaultBiometricPromptTicket,
        mode: VaultBiometricMode,
        makeGrant: () -> T,
        receive: (Result<T>) -> Unit,
    ) {
        if (Build.VERSION.SDK_INT < 30 || Looper.myLooper() != Looper.getMainLooper() ||
            activity.applicationContext !== application || activity.isFinishing || activity.isDestroyed
        ) {
            cancel(ticket)
            receive(Result.failure(VaultBiometricException(VaultBiometricFailure.Unavailable)))
            return
        }
        val operation = synchronized(guard) {
            active?.takeIf { !closed && it.ticket === ticket && it.mode == mode && !it.shown }?.also {
                it.shown = true
            }
        }
        if (operation == null) {
            receive(Result.failure(VaultBiometricException(VaultBiometricFailure.Unavailable)))
            return
        }

        fun fail(reason: VaultBiometricFailure) {
            val removed = synchronized(guard) {
                if (active !== operation || operation.approvedAt != null || operation.consumed) false else {
                    active = null
                    val key = operation.generatedKey
                    val claim = operation.claim
                    if (reason == VaultBiometricFailure.Cancelled && operation.mode == VaultBiometricMode.OptIn &&
                        Build.VERSION.SDK_INT >= 31 && key != null && key.javaClass.name == BioKeystore2SecretKey &&
                        claim != null
                    ) {
                        cancelledOptIn = OptInCancellation(ticket, claim.copyOf(), key)
                    }
                    true
                }
            }
            if (!removed) return
            cancelOperation(operation)
            receive(Result.failure(VaultBiometricException(reason)))
        }

        try {
            val prompt = BiometricPrompt.Builder(activity)
                .setTitle(if (mode == VaultBiometricMode.OptIn) "Biometrie für Harw aktivieren" else "Harw-Tresor entsperren")
                .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                .setConfirmationRequired(mode == VaultBiometricMode.OptIn)
                .setNegativeButton(if (mode == VaultBiometricMode.Unlock) "Harw-PIN" else "Abbrechen", activity.mainExecutor) { _, _ ->
                    fail(VaultBiometricFailure.Cancelled)
                }
                .build()
            prompt.authenticate(
                BiometricPrompt.CryptoObject(operation.cipher),
                operation.cancellation,
                activity.mainExecutor,
                object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                        if (result.authenticationType != BiometricPrompt.AUTHENTICATION_RESULT_TYPE_BIOMETRIC ||
                            result.cryptoObject?.cipher !== operation.cipher
                        ) {
                            fail(VaultBiometricFailure.Unavailable)
                            return
                        }
                        val approved = synchronized(guard) {
                            if (closed || active !== operation || operation.approvedAt != null) false else {
                                operation.approvedAt = SystemClock.elapsedRealtime()
                                true
                            }
                        }
                        if (approved) receive(Result.success(makeGrant()))
                    }

                    override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                        fail(
                            if (errorCode == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED) VaultBiometricFailure.Cancelled
                            else VaultBiometricFailure.Unavailable
                        )
                    }

                    override fun onAuthenticationFailed() {
                        if (mode == VaultBiometricMode.Unlock) fail(VaultBiometricFailure.Cancelled)
                    }
                },
            )
        } catch (_: Exception) {
            fail(VaultBiometricFailure.Unavailable)
        }
    }

    private fun take(
        ticket: VaultBiometricPromptTicket,
        requested: VaultPinScope,
        mode: VaultBiometricMode,
    ): Operation = synchronized(guard) {
        if (Looper.myLooper() == Looper.getMainLooper() || scope != requested || closed) {
            bioFail(VaultBiometricFailure.Unavailable)
        }
        val operation = active?.takeIf { it.ticket === ticket && it.mode == mode && !it.consumed }
            ?: bioFail(VaultBiometricFailure.Unavailable)
        val approved = operation.approvedAt ?: bioFail(VaultBiometricFailure.Unavailable)
        val now = SystemClock.elapsedRealtime()
        if (now < approved || now - approved > BioGrantMillis) bioFail(VaultBiometricFailure.Cancelled)
        operation.consumed = true
        operation
    }

    private fun checkLive(operation: Operation) = synchronized(guard) {
        if (closed || active !== operation) bioFail(VaultBiometricFailure.Cancelled)
    }

    private fun finish(operation: Operation) {
        synchronized(guard) { if (active === operation) active = null }
        operation.claim?.fill(0)
        operation.wire?.fill(0)
    }

    private fun sealAfterPin(
        ticket: VaultBiometricPromptTicket,
        requested: VaultPinScope,
        pinSlot: UiaRoomPinSlot,
        root: ByteArray,
    ) = biometricOperation {
        val operation = take(ticket, requested, VaultBiometricMode.OptIn)
        try {
            check(root.size == BioRootBytes && root.any { it != 0.toByte() })
            checkLive(operation)
            val cipher = operation.cipher
            cipher.updateAAD(biometricAad())
            val encrypted = cipher.doFinal(root)
            try {
                val iv = cipher.iv ?: bioFail(VaultBiometricFailure.Unavailable)
                if (iv.size != BioNonceBytes || encrypted.size != BioRootBytes + BioTagBits / 8) {
                    bioFail(VaultBiometricFailure.Unavailable)
                }
                val wire = ByteBuffer.allocate(VaultBiometricSlotBytes)
                    .put(1.toByte()).put(iv).put(encrypted).array()
                try {
                    synchronized(guard) {
                        checkLive(operation)
                        val claim = operation.claim ?: bioFail(VaultBiometricFailure.InterruptedOptIn)
                        store.publishUiaBiometricSlot(claim, pinSlot, wire)
                    }
                } finally {
                    wire.fill(0)
                }
            } finally {
                encrypted.fill(0)
            }
        } finally {
            finish(operation)
        }
    }

    private fun withRoot(
        ticket: VaultBiometricPromptTicket,
        requested: VaultPinScope,
        pinnedSlot: UiaRoomPinSlot,
        consume: (ByteArray, ByteArray) -> VaultPinProof,
    ): VaultPinProof = biometricOperation {
        val operation = take(ticket, requested, VaultBiometricMode.Unlock)
        try {
            checkLive(operation)
            val encrypted = operation.wire ?: bioFail(VaultBiometricFailure.Unavailable)
            store.requireSameUiaBiometricSlot(pinnedSlot, encrypted)
            operation.cipher.updateAAD(biometricAad())
            val root = operation.cipher.doFinal(encrypted, 1 + BioNonceBytes, encrypted.size - 1 - BioNonceBytes)
            try {
                if (root.size != BioRootBytes || root.all { it == 0.toByte() }) bioFail(VaultBiometricFailure.Unavailable)
                checkLive(operation)
                val result = consume(root, encrypted)
                try {
                    checkLive(operation)
                    result
                } catch (failure: VaultBiometricException) {
                    if (result is VaultPinProof.Granted) result.session.revoke()
                    throw failure
                }
            } finally {
                root.fill(0)
            }
        } finally {
            finish(operation)
        }
    }

    private fun biometricAad(): ByteArray = ByteBuffer.allocate(BioAadDomain.size + 25)
        .order(ByteOrder.BIG_ENDIAN)
        .put(BioAadDomain).put(1.toByte())
        .putLong(scope.vaultId.mostSignificantBits).putLong(scope.vaultId.leastSignificantBits)
        .putLong(scope.keyGeneration).array()

    private fun provisionOptInKey(): SecretKey {
        val store = keyStore()
        if (store.containsAlias(alias())) bioFail(VaultBiometricFailure.InterruptedOptIn)
        val spec = KeyGenParameterSpec.Builder(alias(), KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setUserAuthenticationRequired(true)
            .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
            .setInvalidatedByBiometricEnrollment(true)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, BioStore).run {
            init(spec)
            generateKey().also(::requireStrongHardware)
        }
    }

    private fun existingBiometricKey(): SecretKey =
        (keyStore().getKey(alias(), null) as? SecretKey ?: bioFail(VaultBiometricFailure.Invalidated))
            .also(::requireStrongHardware)

    private fun alias(): String = alias(scope)

    private fun keyStore() = KeyStore.getInstance(BioStore).apply { load(null) }

    @Suppress("DEPRECATION")
    private fun requireStrongHardware(key: SecretKey) {
        val info = SecretKeyFactory.getInstance(KeyProperties.KEY_ALGORITHM_AES, BioStore)
            .getKeySpec(key, KeyInfo::class.java) as KeyInfo
        val hardware = if (Build.VERSION.SDK_INT >= 31) {
            info.securityLevel == KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT ||
                info.securityLevel == KeyProperties.SECURITY_LEVEL_STRONGBOX
        } else {
            info.isInsideSecureHardware
        }
        if (!hardware || !info.isUserAuthenticationRequired ||
            !info.isUserAuthenticationRequirementEnforcedBySecureHardware ||
            info.userAuthenticationType != KeyProperties.AUTH_BIOMETRIC_STRONG ||
            info.userAuthenticationValidityDurationSeconds != -1 ||
            !info.isInvalidatedByBiometricEnrollment
        ) bioFail(VaultBiometricFailure.Unsupported)
    }

    companion object {
        fun alias(scope: VaultPinScope) = "dev.harw.mobile.vault.bio.v1.${scope.vaultId}.${scope.keyGeneration}"
        fun supported(application: HarwApplication): Boolean = Build.VERSION.SDK_INT >= 30 &&
            runCatching {
                application.getSystemService(BiometricManager::class.java)
                    ?.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) == BiometricManager.BIOMETRIC_SUCCESS
            }.getOrDefault(false)

        fun forSelectedStore(application: HarwApplication, store: VaultPinDiskStateStore): AndroidVaultBiometricSlot {
            require(application.applicationContext === application &&
                !application.isDeviceProtectedStorage && application.packageName == "dev.harw.mobile")
            return AndroidVaultBiometricSlot(application, store)
        }
    }
}

private fun bioFail(failure: VaultBiometricFailure): Nothing = throw VaultBiometricException(failure)

private inline fun <T> biometricOperation(action: () -> T): T = try {
    action()
} catch (failure: VaultBiometricException) {
    throw failure
} catch (failure: Exception) {
    var cause: Throwable? = failure
    repeat(8) {
        if (cause is KeyPermanentlyInvalidatedException) bioFail(VaultBiometricFailure.Invalidated)
        cause = cause?.cause
    }
    bioFail(VaultBiometricFailure.Unavailable)
}

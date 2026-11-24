package dev.harw.mobile

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.util.UUID
import javax.crypto.KeyGenerator
import javax.crypto.Mac
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory

private const val StoreName = "AndroidKeyStore"
private const val MacAlgorithm = "HmacSHA256"

/**
 * Dedicated device binding, not the provider-token alias and not a biometric credential.
 * A custom app PIN has no Android Gatekeeper retry control. Never expose this as an IPC oracle.
 * No software downgrade. No operation is called by the app until vault integration is reviewed.
 */
internal class AndroidVaultPinMac private constructor(
    private val vaultId: UUID,
    private val key: SecretKey,
) : UiaRoomPinDevice {
    override fun enrollmentProof(scope: VaultPinScope, root: ByteArray): ByteArray {
        if (scope.vaultId != vaultId || root.size != 32 || root.all { it == 0.toByte() }) {
            throw VaultPinException(VaultPinFailure.Unavailable)
        }
        return authenticate(uiaRoomDeviceMacContext(scope), root)
    }

    override fun authenticate(context: ByteArray, stretchedPin: ByteArray): ByteArray = platformOperation {
        Mac.getInstance(MacAlgorithm).run {
            init(key)
            update(context)
            doFinal(stretchedPin)
        }
    }

    companion object {
        /** Open never generates or silently replaces missing keys. */
        fun openExisting(vaultId: UUID): AndroidVaultPinMac = platformOperation {
            val key = store().getKey(alias(vaultId), null) as? SecretKey
                ?: throw VaultPinException(VaultPinFailure.DeviceKeyMissing)
            requireHardware(key)
            AndroidVaultPinMac(vaultId, key)
        }

        /**
         * Explicit one-time provisioning for an already authorized new vault only. The vault
         * initializer must rule out an existing vault/slot before invoking this method.
         * A failure after generation is not permission to replace the alias.
         */
        @Synchronized
        fun provisionNew(vaultId: UUID): AndroidVaultPinMac = platformOperation {
            val keyStore = store()
            val alias = alias(vaultId)
            if (keyStore.containsAlias(alias)) throw VaultPinException(VaultPinFailure.Unavailable)
            val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setKeySize(256)
                .build()
            val key = KeyGenerator.getInstance(MacAlgorithm, StoreName).run {
                init(spec)
                generateKey()
            }
            try {
                requireHardware(key)
            } catch (failure: Exception) {
                // Delete only the unusable alias provisioned by this invocation.
                runCatching { keyStore.deleteEntry(alias) }
                throw failure
            }
            AndroidVaultPinMac(vaultId, key)
        }

        private fun alias(vaultId: UUID): String = "dev.harw.mobile.vault.pin.v1.$vaultId"

        private fun store(): KeyStore = KeyStore.getInstance(StoreName).apply { load(null) }

        @Suppress("DEPRECATION")
        private fun requireHardware(key: SecretKey) {
            val info = SecretKeyFactory.getInstance(MacAlgorithm, StoreName)
                .getKeySpec(key, KeyInfo::class.java) as KeyInfo
            val hardware = if (Build.VERSION.SDK_INT >= 31) {
                info.securityLevel == KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT ||
                    info.securityLevel == KeyProperties.SECURITY_LEVEL_STRONGBOX
            } else {
                info.isInsideSecureHardware
            }
            if (!hardware) throw VaultPinException(VaultPinFailure.Unavailable)
        }
    }
}

private inline fun <T> platformOperation(action: () -> T): T = try {
    action()
} catch (failure: VaultPinException) {
    throw failure
} catch (failure: Exception) {
    // Recognize only the documented platform type, not vendor-specific messages.
    var cause: Throwable? = failure
    repeat(8) {
        if (cause is KeyPermanentlyInvalidatedException) {
            throw VaultPinException(VaultPinFailure.DeviceKeyInvalidated)
        }
        cause = cause?.cause
    }
    throw VaultPinException(VaultPinFailure.Unavailable)
}

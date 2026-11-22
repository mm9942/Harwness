package dev.harw.mobile

import android.content.Context
import android.content.pm.PackageManager
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.AtomicFile
import java.io.File
import java.io.FileNotFoundException
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import uniffi.harw_mobile_ffi.KeystoreException
import uniffi.harw_mobile_ffi.PlatformKeystore

/**
 * Android-Keystore-backed secret slots for the Rust core (provider API keys).
 *
 * Each slot is one file `<slot>.aes` under `noBackupFilesDir/secrets`, encrypted with an
 * AES-256-GCM key-encryption key that never leaves the Keystore (StrongBox when available).
 * The ciphertext is bound to its slot through the AAD, so files cannot be swapped between slots.
 *
 * Why `noBackupFilesDir`: the ciphertext is useless without the device-bound KEK, so it must
 * never be restored onto another device by Auto Backup or device transfer. The manifest already
 * sets `allowBackup=false`; the directory choice keeps that true if the flag ever changes.
 *
 * These secrets live outside the agent-visible `harw-home`, so a tool-using agent cannot read them.
 *
 * Residual risk: UniFFI copies the plaintext returned by [unwrap] into the Rust buffer; the
 * Kotlin copy is only as short-lived as the caller keeps it. [wrap] zeroes its input array.
 *
 * Nothing here logs; exception types carry no plaintext.
 */
internal class AndroidPlatformKeystore(private val context: Context) : PlatformKeystore {
    private val lock = Any()
    private val slotDir = File(context.noBackupFilesDir, "secrets")

    override fun wrap(slot: String, plain: ByteArray) {
        try {
            val file = slotFile(slot)
            if (plain.isEmpty() || plain.size > MaxPlainBytes) throw KeystoreException.Unavailable()
            synchronized(lock) {
                try {
                    if (!slotDir.isDirectory && !slotDir.mkdirs() && !slotDir.isDirectory) {
                        throw KeystoreException.Unavailable()
                    }
                    val encryptor = Cipher.getInstance(Transformation)
                    encryptor.init(Cipher.ENCRYPT_MODE, existingKey() ?: createKey())
                    if (encryptor.iv.size != IvBytes) throw KeystoreException.Unavailable()
                    encryptor.updateAAD(aad(slot))
                    val blob = byteArrayOf(FormatVersion) + encryptor.iv + encryptor.doFinal(plain)
                    file.writeAtomically(blob)
                } catch (_: GeneralSecurityException) {
                    throw KeystoreException.Unavailable()
                } catch (_: ProviderException) {
                    throw KeystoreException.Unavailable()
                } catch (_: IOException) {
                    throw KeystoreException.Unavailable()
                }
            }
        } finally {
            plain.fill(0)
        }
    }

    override fun unwrap(slot: String): ByteArray {
        val file = slotFile(slot)
        synchronized(lock) {
            if (!atomicFileExists(file.baseFile)) throw KeystoreException.Missing()
            try {
                if (file.baseFile.length() > MaxBlobBytes) throw KeystoreException.Corrupt()
                val blob = try {
                    file.readFully()
                } catch (error: FileNotFoundException) {
                    if (atomicFileExists(file.baseFile)) throw error
                    throw KeystoreException.Missing()
                }
                if (blob.size <= 1 + IvBytes + TagBytes || blob.size > MaxBlobBytes || blob[0] != FormatVersion) {
                    throw KeystoreException.Corrupt()
                }
                val key = existingKey() ?: throw KeystoreException.Unavailable()
                val decryptor = Cipher.getInstance(Transformation)
                decryptor.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TagBytes * 8, blob, 1, IvBytes))
                decryptor.updateAAD(aad(slot))
                return decryptor.doFinal(blob, 1 + IvBytes, blob.size - 1 - IvBytes)
            } catch (_: AEADBadTagException) {
                throw KeystoreException.Corrupt()
            } catch (_: GeneralSecurityException) {
                // Includes KeyPermanentlyInvalidatedException: the KEK is gone or unusable.
                throw KeystoreException.Unavailable()
            } catch (_: ProviderException) {
                throw KeystoreException.Unavailable()
            } catch (_: IOException) {
                throw KeystoreException.Unavailable()
            }
        }
    }

    override fun delete(slot: String) {
        val file = slotFile(slot)
        synchronized(lock) {
            file.delete()
            if (atomicFileExists(file.baseFile)) throw KeystoreException.Unavailable()
        }
    }

    override fun contains(slot: String): Boolean {
        val file = slotFile(slot)
        synchronized(lock) {
            try {
                return atomicFileExists(file.baseFile)
            } catch (_: IOException) {
                throw KeystoreException.Unavailable()
            }
        }
    }

    private fun slotFile(slot: String): AtomicFile {
        if (!SlotPattern.matches(slot)) throw KeystoreException.InvalidSlot()
        return AtomicFile(File(slotDir, "$slot.aes"))
    }

    private fun aad(slot: String): ByteArray = "$KeyAlias:$slot".toByteArray(Charsets.UTF_8)

    private fun existingKey(): SecretKey? {
        val keyStore = KeyStore.getInstance(KeyStoreName).apply { load(null) }
        return keyStore.getKey(KeyAlias, null) as? SecretKey
    }

    private fun createKey(): SecretKey {
        if (context.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE)) {
            try {
                return generateKey(strongBox = true)
            } catch (_: StrongBoxUnavailableException) {
                // Devices can advertise StrongBox while temporarily running out of key slots.
            }
        }
        return generateKey(strongBox = false)
    }

    private fun generateKey(strongBox: Boolean): SecretKey {
        val parameters = KeyGenParameterSpec.Builder(
            KeyAlias,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .setIsStrongBoxBacked(strongBox)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KeyStoreName)
            .apply { init(parameters) }
            .generateKey()
    }

    private fun AtomicFile.writeAtomically(bytes: ByteArray) {
        val output = startWrite()
        try {
            output.write(bytes)
            finishWrite(output)
        } catch (error: IOException) {
            failWrite(output)
            throw error
        }
        if (!readFully().contentEquals(bytes)) throw KeystoreException.Unavailable()
    }

    private companion object {
        const val KeyStoreName = "AndroidKeyStore"
        const val KeyAlias = "dev.harw.mobile.secrets.v1"
        const val Transformation = "AES/GCM/NoPadding"
        const val FormatVersion: Byte = 1
        const val IvBytes = 12
        const val TagBytes = 16
        const val MaxPlainBytes = 16 * 1024
        const val MaxBlobBytes = 1 + IvBytes + MaxPlainBytes + TagBytes
        val SlotPattern = Regex("^[a-z0-9][a-z0-9-]{0,63}$")
    }
}

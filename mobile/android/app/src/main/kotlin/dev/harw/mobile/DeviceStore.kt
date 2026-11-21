package dev.harw.mobile

import android.content.Context
import android.content.pm.PackageManager
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.AtomicFile
import com.google.gson.JsonObject
import java.io.File
import java.io.FileNotFoundException
import java.io.FileOutputStream
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal class DeviceStore(private val context: Context) {
    private val identityFile = AtomicFile(File(context.noBackupFilesDir, "identity.aes"))
    private val profileFile = AtomicFile(File(context.noBackupFilesDir, "server-profile.json"))
    private val resetMarker = AtomicFile(File(context.noBackupFilesDir, "setup-reset.pending"))
    private val siwcHostFile = AtomicFile(File(context.noBackupFilesDir, "chatgpt-host-id"))
    private val siwcRegistrationFile = AtomicFile(File(context.noBackupFilesDir, "chatgpt-registration.json"))

    fun loadOrGenerateIdentity(generate: () -> ByteArray): ByteArray {
        val keyStore = KeyStore.getInstance(KeyStoreName).apply { load(null) }
        val existing = keyStore.getKey(KeyAlias, null) as? SecretKey
        val wrapped = identityFile.readIfPresent { it.readBytes() }
        if (wrapped != null) {
            val key = checkNotNull(existing) { "Android Keystore no longer contains this device identity's wrapping key." }
            check(wrapped.size > 1 + IvBytes + TagBytes && wrapped[0] == FormatVersion) {
                "The saved device identity has an invalid format."
            }
            val decryptor = Cipher.getInstance(Transformation)
            decryptor.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TagBytes * 8, wrapped, 1, IvBytes))
            decryptor.updateAAD(AssociatedData)
            return decryptor.doFinal(wrapped, 1 + IvBytes, wrapped.size - 1 - IvBytes)
        }

        val key = existing ?: createWrappingKey()
        val identity = generate()
        try {
            val encryptor = Cipher.getInstance(Transformation)
            encryptor.init(Cipher.ENCRYPT_MODE, key)
            check(encryptor.iv.size == IvBytes) { "Android Keystore returned an unexpected AES-GCM IV." }
            encryptor.updateAAD(AssociatedData)
            val wrapped = byteArrayOf(FormatVersion) + encryptor.iv + encryptor.doFinal(identity)
            identityFile.writeAtomically(wrapped)
            return identity
        } catch (error: Exception) {
            identity.fill(0)
            throw error
        }
    }

    fun readProfile(): String? = profileFile.readIfPresent(::readProfileDocument)

    fun saveProfile(json: String) {
        profileFile.writeAtomically(json.toByteArray(Charsets.UTF_8))
    }

    fun hasSavedSetup(): Boolean = atomicFileExists(identityFile.baseFile) || atomicFileExists(profileFile.baseFile)

    fun hasPendingReset(): Boolean = atomicFileExists(resetMarker.baseFile)

    fun chatgptHostId(): String {
        val saved = siwcHostFile.readIfPresent(::readProfileDocument)
        if (saved != null) {
            require(saved.startsWith("urn:uuid:") && UUID.fromString(saved.removePrefix("urn:uuid:")).version() == 4) {
                "The saved ChatGPT host identifier is invalid."
            }
            return saved
        }
        val hostId = "urn:uuid:${UUID.randomUUID()}"
        siwcHostFile.writeAtomically(hostId.encodeToByteArray())
        return hostId
    }

    fun chatgptRegistration(serverScope: String): SiwcRegistration? {
        val saved = siwcRegistrationFile.readIfPresent(::readProfileDocument) ?: return null
        val fields = SiwcJson.fromJson(saved, JsonObject::class.java)
        if (fields.string("server_scope") != serverScope) return null
        val client = requireNotNull(fields.string("client_id"))
        val subject = requireNotNull(fields.string("subject"))
        require(isSiwcClientId(client) && subject.isNotBlank())
        return SiwcRegistration(client, subject)
    }

    fun saveChatgptRegistration(serverScope: String, registration: SiwcRegistration) {
        val fields = JsonObject().apply {
            addProperty("server_scope", serverScope)
            addProperty("client_id", registration.clientId)
            addProperty("subject", registration.subject)
        }
        siwcRegistrationFile.writeAtomically(SiwcJson.toJson(fields).encodeToByteArray())
    }

    fun clearChatgptRegistration(serverScope: String) {
        if (chatgptRegistration(serverScope) != null) siwcRegistrationFile.deleteChecked()
    }

    fun purgeSetup() {
        // A marker failure must still lead directly to secure erasure. Attempt each
        // deletion independently so either removal of the seed or of its key fences it.
        val markerFailure = runCatching(::markResetPending).exceptionOrNull()
        val failures = listOf(
            runCatching { identityFile.deleteChecked() }.exceptionOrNull(),
            runCatching { deleteWrappingKey() }.exceptionOrNull(),
            runCatching { profileFile.deleteChecked() }.exceptionOrNull(),
            runCatching { siwcHostFile.deleteChecked() }.exceptionOrNull(),
            runCatching { siwcRegistrationFile.deleteChecked() }.exceptionOrNull(),
        ).filterNotNull()
        if (failures.isNotEmpty()) {
            val failure = failures.first()
            failures.drop(1).forEach(failure::addSuppressed)
            if (markerFailure != null) {
                failure.addSuppressed(markerFailure)
                // Deleting the profile and wrapped seed can free space for the marker.
                runCatching(::markResetPending).exceptionOrNull()?.let(failure::addSuppressed)
            }
            throw failure
        }
        resetMarker.deleteChecked()
    }

    private fun markResetPending() {
        // Existence is the marker. Preserve it even if sync fails so another launch
        // can detect it and retry the purge.
        FileOutputStream(resetMarker.baseFile, true).use { it.fd.sync() }
    }

    private fun deleteWrappingKey() {
        val keyStore = KeyStore.getInstance(KeyStoreName).apply { load(null) }
        if (keyStore.containsAlias(KeyAlias)) keyStore.deleteEntry(KeyAlias)
        check(!keyStore.containsAlias(KeyAlias)) { "Android Keystore did not remove the wrapping key." }
    }

    private fun createWrappingKey(): SecretKey {
        if (context.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE)) {
            try {
                return generateWrappingKey(strongBox = true)
            } catch (_: StrongBoxUnavailableException) {
                // Devices can advertise StrongBox while temporarily running out of key slots.
            }
        }
        return generateWrappingKey(strongBox = false)
    }

    private fun generateWrappingKey(strongBox: Boolean): SecretKey {
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
        } catch (error: Exception) {
            failWrite(output)
            throw error
        }
        check(readFully().contentEquals(bytes)) { "Android could not confirm that connection data was saved." }
    }

    private fun AtomicFile.deleteChecked() {
        delete()
        check(!atomicFileExists(baseFile)) {
            "Android could not remove saved connection data."
        }
    }

    private fun <T> AtomicFile.readIfPresent(read: (java.io.InputStream) -> T): T? {
        val input = try {
            openRead()
        } catch (error: FileNotFoundException) {
            if (atomicFileExists(baseFile)) throw error
            return null
        }
        return input.use(read)
    }

    private companion object {
        const val KeyStoreName = "AndroidKeyStore"
        const val KeyAlias = "dev.harw.mobile.identity.v1"
        const val Transformation = "AES/GCM/NoPadding"
        const val FormatVersion: Byte = 1
        const val IvBytes = 12
        const val TagBytes = 16
        val AssociatedData = KeyAlias.toByteArray(Charsets.UTF_8)
    }
}

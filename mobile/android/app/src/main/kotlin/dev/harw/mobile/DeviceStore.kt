package dev.harw.mobile

import android.content.Context
import android.content.pm.PackageManager
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.system.Os
import android.system.OsConstants
import android.util.AtomicFile
import java.io.File
import java.io.FileNotFoundException
import java.io.FileOutputStream
import java.io.InputStream
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
    private val gatewaySiwcFile = AtomicFile(File(context.noBackupFilesDir, "chatgpt-gateway-registrations-v1.json"))

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

    /** Call under the app-wide connection storage lock. Never send the legacy UUID to another gateway. */
    fun chatgptGatewayRegistration(gatewayScope: String, legacyScope: String): GatewaySiwcRegistration {
        val rows = readGatewaySiwcRegistrations()
        rows[gatewayScope]?.let { return it }
        val legacy = legacyGatewaySiwcRegistration(
            siwcRegistrationFile.readIfPresent(::readProfileDocument), legacyScope,
        ) { siwcHostFile.readIfPresent(::readProfileDocument) }
        // A legacy UUID can be migrated only once, even if an old route is
        // imported a second time. Retain all other gateway state on disk.
        val selected = legacy?.takeUnless { candidate -> rows.values.any { it.hostId == candidate.hostId } }
            ?: GatewaySiwcRegistration("urn:uuid:${UUID.randomUUID()}")
        writeGatewaySiwcRegistrations(rows + (gatewayScope to selected))
        return selected
    }

    fun saveChatgptGatewayRegistration(
        gatewayScope: String,
        expected: GatewaySiwcRegistration,
        next: GatewaySiwcRegistration,
    ) {
        val rows = readGatewaySiwcRegistrations()
        if (rows[gatewayScope] != expected || next.hostId != expected.hostId ||
            (next != expected && (next.revision <= expected.revision || next.revision - expected.revision != 1L))
        ) {
            throw SiwcFailure("The saved ChatGPT gateway sign-in changed. Start sign-in again.")
        }
        // Also sync an unchanged registration before token exchange. A
        // preceding failed directory sync may have left visible but not
        // durably confirmed bytes; mere readback cannot recover that.
        writeGatewaySiwcRegistrations(rows + (gatewayScope to next))
    }

    private fun readGatewaySiwcRegistrations(): Map<String, GatewaySiwcRegistration> =
        gatewaySiwcFile.readIfPresent { decodeGatewaySiwcRegistrations(readGatewaySiwcDocument(it)) }
            ?: emptyMap()

    private fun writeGatewaySiwcRegistrations(rows: Map<String, GatewaySiwcRegistration>) {
        val bytes = encodeGatewaySiwcRegistrations(rows).encodeToByteArray()
        require(bytes.size <= GatewaySiwcMaxBytes)
        gatewaySiwcFile.writeAtomically(bytes, durableDirectory = true)
    }

    private fun readGatewaySiwcDocument(input: InputStream): String {
        val bytes = java.io.ByteArrayOutputStream()
        val chunk = ByteArray(8 * 1024)
        while (true) {
            val count = input.read(chunk)
            if (count < 0) break
            require(bytes.size() + count <= GatewaySiwcMaxBytes)
            bytes.write(chunk, 0, count)
        }
        return bytes.toByteArray().decodeToString(throwOnInvalidSequence = true)
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
            runCatching { gatewaySiwcFile.deleteChecked() }.exceptionOrNull(),
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

    private fun AtomicFile.writeAtomically(bytes: ByteArray, durableDirectory: Boolean = false) {
        val output = startWrite()
        try {
            output.write(bytes)
            if (durableDirectory) output.fd.sync()
            finishWrite(output)
        } catch (error: Exception) {
            failWrite(output)
            throw error
        }
        if (durableDirectory) {
            val directory = Os.open(
                requireNotNull(baseFile.parent),
                OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW or OsConstants.O_CLOEXEC or OsConstants.O_NONBLOCK,
                0,
            )
            try {
                check(OsConstants.S_ISDIR(Os.fstat(directory).st_mode)) {
                    "Connection data sync target is not a directory."
                }
                Os.fsync(directory)
            } finally {
                Os.close(directory)
            }
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
        const val GatewaySiwcMaxBytes = 1024 * 1024
        const val TagBytes = 16
        val AssociatedData = KeyAlias.toByteArray(Charsets.UTF_8)
    }
}

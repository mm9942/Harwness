package dev.harw.mobile

import android.content.Context
import android.net.Uri
import android.util.AtomicFile
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.InputStream
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.security.SecureRandom
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import javax.crypto.Cipher
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.PBEKeySpec
import javax.crypto.spec.SecretKeySpec

internal object LocalBackup {
    private val magic = "HARWBK01".toByteArray(Charsets.US_ASCII)
    private const val saltSize = 16
    private const val nonceSize = 12
    private const val limit = 32 * 1024 * 1024
    private const val maxEntries = 8192
    private val paths = listOf(
        "profiles/default" to Regex("config[.]toml|chat-uias[.]json"),
        "profiles/default/providers" to Regex("[a-z0-9][a-z0-9_-]{0,63}\\.toml"),
        "state/local-host/host/hosted-sessions" to Regex("[A-Za-z0-9_-]{1,128}[.]json"),
        "state/local-host/sessions" to Regex("[A-Za-z0-9_-]{1,128}\\.(jsonl|meta\\.json)"),
    )

    private const val agents = "profiles/default/agents"
    private val uiaDirectory = Regex("(phone-uia|uia-[a-z0-9_-]{1,128})")
    private val uiaFile = Regex("definition[.]toml|system[.]md")

    private fun accepted(directory: String, name: String): Boolean =
        paths.any { (dir, pattern) -> dir == directory && pattern.matches(name) } ||
            (directory.substringBeforeLast('/') == agents &&
                uiaDirectory.matches(directory.substringAfterLast('/')) && uiaFile.matches(name))

    @Synchronized
    fun export(context: Context, uri: Uri, password: CharArray) {
        val encrypted = encode(File(context.filesDir, "harw-home"), password)
        checkNotNull(context.contentResolver.openOutputStream(uri, "wt")).use { it.write(encrypted) }
    }

    internal fun encode(root: File, password: CharArray): ByteArray {
        require(password.size >= 12)
        val archive = ByteArrayOutputStream()
        var remaining = limit
        var count = 0
        ZipOutputStream(archive).use { zip ->
            val uias = File(root, agents).listFiles().orEmpty()
                .filter { uiaDirectory.matches(it.name) && Files.isDirectory(it.toPath(), NOFOLLOW_LINKS) }
                .map { "$agents/${it.name}" to uiaFile }
            for ((directory, pattern) in paths + uias) {
                val source = File(root, directory)
                for (file in source.listFiles().orEmpty().sortedBy { it.name }) {
                    if (!pattern.matches(file.name) || !Files.isRegularFile(file.toPath(), NOFOLLOW_LINKS)) continue
                    val bytes = file.inputStream().use { readBounded(it, remaining) }
                    try {
                        remaining -= bytes.size
                        require(++count <= maxEntries)
                        zip.putNextEntry(ZipEntry("$directory/${file.name}"))
                        zip.write(bytes)
                        zip.closeEntry()
                    } finally {
                        bytes.fill(0)
                    }
                }
            }
        }
        require(archive.size() <= limit)
        val header = magic + ByteArray(saltSize + nonceSize).apply { SecureRandom().nextBytes(this) }
        val plaintext = archive.toByteArray()
        try {
            return header + cipher(Cipher.ENCRYPT_MODE, password, header).doFinal(plaintext)
        } finally {
            plaintext.fill(0)
        }
    }

    @Synchronized
    fun restore(context: Context, uri: Uri, password: CharArray): Int {
        val headerSize = magic.size + saltSize + nonceSize
        val bytes = checkNotNull(context.contentResolver.openInputStream(uri)).use {
            readBounded(it, limit + headerSize + 16)
        }
        return restoreInto(File(context.filesDir, "harw-home"), bytes, password, ::writeAtomically)
    }

    internal fun restoreInto(root: File, bytes: ByteArray, password: CharArray, write: (File, ByteArray) -> Unit): Int {
        val headerSize = magic.size + saltSize + nonceSize
        require(bytes.size in (headerSize + 16)..(limit + headerSize + 16))
        require(bytes.copyOfRange(0, magic.size).contentEquals(magic))
        val header = bytes.copyOfRange(0, headerSize)
        val decoded = cipher(Cipher.DECRYPT_MODE, password, header).doFinal(bytes, headerSize, bytes.size - headerSize)
        val entries = linkedMapOf<String, ByteArray>()
        try {
            var remaining = limit
            ZipInputStream(ByteArrayInputStream(decoded)).use { zip ->
                while (true) {
                    val entry = zip.nextEntry ?: break
                    val directory = entry.name.substringBeforeLast('/', "")
                    val name = entry.name.substringAfterLast('/')
                    require(!entry.isDirectory && accepted(directory, name))
                    require(entries.size < maxEntries && entry.name !in entries)
                    val value = readBounded(zip, remaining)
                    remaining -= value.size
                    entries[entry.name] = value
                }
            }
            val conflicts = entries.filterKeys { it.startsWith("state/") }.mapNotNull { (name, value) ->
                val target = File(root, name)
                if (target.exists() && !matches(target, value)) chatId(name) else null
            }.toSet()
            var restored = 0
            // Publish the hosted-session record after its transcript and metadata.
            for ((name, value) in entries.entries.sortedBy { it.key.startsWith("state/local-host/host/") }) {
                if (name.startsWith("state/") && (chatId(name) in conflicts || File(root, name).exists())) continue
                val target = File(root, name)
                check(target.parentFile!!.isDirectory || target.parentFile!!.mkdirs())
                write(target, value)
                restored++
            }
            return restored
        } finally {
            decoded.fill(0)
            entries.values.forEach { it.fill(0) }
        }
    }

    private fun chatId(name: String): String =
        name.substringAfterLast('/').removeSuffix(".meta.json").removeSuffix(".jsonl").removeSuffix(".json")

    private fun matches(target: File, value: ByteArray): Boolean {
        if (!Files.isRegularFile(target.toPath(), NOFOLLOW_LINKS) || target.length() != value.size.toLong()) return false
        val saved = target.inputStream().use { readBounded(it, value.size) }
        return try {
            saved.contentEquals(value)
        } finally {
            saved.fill(0)
        }
    }

    private fun writeAtomically(target: File, value: ByteArray) {
        val file = AtomicFile(target)
        val output = file.startWrite()
        try {
            output.write(value)
            output.fd.sync()
            file.finishWrite(output)
        } catch (error: Exception) {
            file.failWrite(output)
            throw error
        }
    }

    private fun cipher(mode: Int, password: CharArray, header: ByteArray): Cipher {
        val salt = header.copyOfRange(magic.size, magic.size + saltSize)
        val spec = PBEKeySpec(password, salt, 600_000, 256)
        val key = try {
            SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256").generateSecret(spec).encoded
        } finally {
            spec.clearPassword()
        }
        return try {
            Cipher.getInstance("AES/GCM/NoPadding").apply {
                init(mode, SecretKeySpec(key, "AES"), GCMParameterSpec(128, header.takeLast(nonceSize).toByteArray()))
                updateAAD(header)
            }
        } finally {
            key.fill(0)
        }
    }

    private fun readBounded(input: InputStream, max: Int): ByteArray {
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) return output.toByteArray()
            require(count <= max - output.size())
            output.write(buffer, 0, count)
        }
    }
}

package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.security.MessageDigest
import uniffi.harw_mobile_ffi.DeviceException
import uniffi.harw_mobile_ffi.FileAccess
import uniffi.harw_mobile_ffi.SafEntry
import uniffi.harw_mobile_ffi.SafRoot

private const val SAF_MAX_PATH_BYTES = 1024
private const val SAF_MAX_SEGMENT_BYTES = 255
private const val SAF_MAX_TRANSFER_BYTES = 8L * 1024 * 1024
private const val SAF_PREFS = "harw_saf_roots"
private const val SAF_BUFFER_BYTES = 64 * 1024

private fun invalidPath(detail: String): DeviceException = DeviceException.InvalidPath(detail)

internal fun normalizeSafPath(path: String): String {
    if (path.toByteArray(Charsets.UTF_8).size > SAF_MAX_PATH_BYTES) throw invalidPath("path too long")
    if (path.indexOf('\\') >= 0) throw invalidPath("backslash")
    if (path.indexOf('\u0000') >= 0) throw invalidPath("nul byte")
    if (path.startsWith("/")) throw invalidPath("absolute path")
    val trimmed = path.removeSuffix("/")
    if (trimmed.isEmpty()) return ""
    for (segment in trimmed.split('/')) {
        if (segment.isEmpty()) throw invalidPath("empty segment")
        if (segment == "." || segment == "..") throw invalidPath("dot segment")
        if (segment.toByteArray(Charsets.UTF_8).size > SAF_MAX_SEGMENT_BYTES) throw invalidPath("segment too long")
    }
    return trimmed
}

internal fun sha256Hex(data: ByteArray): String {
    val digest = MessageDigest.getInstance("SHA-256").digest(data)
    val hex = StringBuilder(digest.size * 2)
    for (byte in digest) {
        hex.append("0123456789abcdef"[(byte.toInt() shr 4) and 0xF])
        hex.append("0123456789abcdef"[byte.toInt() and 0xF])
    }
    return hex.toString()
}

internal fun rootIdFor(uri: String): String {
    val digest = MessageDigest.getInstance("SHA-256").digest(uri.toByteArray(Charsets.UTF_8))
    val hex = StringBuilder(digest.size * 2)
    for (byte in digest) {
        hex.append("0123456789abcdef"[(byte.toInt() shr 4) and 0xF])
        hex.append("0123456789abcdef"[byte.toInt() and 0xF])
    }
    return hex.substring(0, 16)
}

internal fun readSafBytes(stream: InputStream, cap: Long): ByteArray {
    val out = ByteArrayOutputStream()
    val buffer = ByteArray(SAF_BUFFER_BYTES)
    var total = 0L
    while (true) {
        val count = stream.read(buffer)
        if (count < 0) break
        total += count
        if (total > cap) throw DeviceException.TooLarge()
        out.write(buffer, 0, count)
    }
    return out.toByteArray()
}

internal class SafStore(private val context: Context) : FileAccess {
    private class Stored(val uri: String, val label: String, val writable: Boolean)

    private class Child(
        val docId: String,
        val name: String,
        val isDir: Boolean,
        val size: Long,
        val modified: Long?,
    )

    private val prefs = context.getSharedPreferences(SAF_PREFS, Context.MODE_PRIVATE)
    private val resolver get() = context.contentResolver

    fun addRoot(treeUri: Uri, label: String): SafRoot {
        val both = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        var writable = true
        try {
            resolver.takePersistableUriPermission(treeUri, both)
        } catch (_: SecurityException) {
            writable = false
            try {
                resolver.takePersistableUriPermission(treeUri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            } catch (_: SecurityException) {
                throw DeviceException.Failed("permission revoked")
            }
        }
        val uriString = treeUri.toString()
        val id = rootIdFor(uriString)
        val cleanLabel = label.replace('\t', ' ').replace('\n', ' ').replace('\r', ' ')
        val encoded = listOf(uriString, cleanLabel, writable.toString()).joinToString("\t")
        prefs.edit().putString(id, encoded).apply()
        return SafRoot(id = id, label = cleanLabel, writable = writable)
    }

    fun removeRoot(rootId: String) {
        val stored = readStored(rootId)
        if (stored != null) {
            try {
                val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                resolver.releasePersistableUriPermission(Uri.parse(stored.uri), flags)
            } catch (_: SecurityException) {
                // The grant is already gone; the entry is removed below.
            }
        }
        prefs.edit().remove(rootId).apply()
    }

    override fun roots(): List<SafRoot> = guard {
        val grants = resolver.persistedUriPermissions
        val result = ArrayList<SafRoot>()
        for (id in prefs.all.keys.sorted()) {
            val stored = readStored(id) ?: continue
            val grant = grants.firstOrNull { it.uri.toString() == stored.uri && it.isReadPermission }
                ?: continue
            result.add(SafRoot(id = id, label = stored.label, writable = stored.writable && grant.isWritePermission))
        }
        result
    }

    override fun list(rootId: String, path: String): List<SafEntry> = guard {
        val normalized = normalizeSafPath(path)
        val tree = treeFor(rootId)
        val dir = resolveDir(tree, normalized)
        val prefix = if (normalized.isEmpty()) "" else "$normalized/"
        children(tree, dir).map {
            SafEntry(
                name = it.name,
                path = prefix + it.name,
                isDir = it.isDir,
                sizeBytes = it.size.coerceAtLeast(0L).toULong(),
                modifiedMs = it.modified,
            )
        }
    }

    override fun read(rootId: String, path: String, maxBytes: ULong): ByteArray = guard {
        val normalized = normalizeSafPath(path)
        if (normalized.isEmpty()) throw DeviceException.Invalid("path is a directory")
        val tree = treeFor(rootId)
        val cap = if (maxBytes > SAF_MAX_TRANSFER_BYTES.toULong()) SAF_MAX_TRANSFER_BYTES else maxBytes.toLong()
        val child = resolveChild(tree, normalized)
        if (child.isDir) throw DeviceException.Invalid("path is a directory")
        if (child.size > cap) throw DeviceException.TooLarge()
        val uri = DocumentsContract.buildDocumentUriUsingTree(tree, child.docId)
        val input = resolver.openInputStream(uri) ?: throw DeviceException.Failed("i/o error")
        input.use { readSafBytes(it, cap) }
    }

    override fun write(rootId: String, path: String, data: ByteArray) = guard {
        val normalized = normalizeSafPath(path)
        if (data.size.toLong() > SAF_MAX_TRANSFER_BYTES) throw DeviceException.TooLarge()
        if (normalized.isEmpty()) throw DeviceException.Invalid("path is a directory")
        val tree = treeFor(rootId)
        val slash = normalized.lastIndexOf('/')
        val parentPath = if (slash < 0) "" else normalized.substring(0, slash)
        val name = normalized.substring(slash + 1)
        val parent = resolveDir(tree, parentPath)
        if (children(tree, parent).any { it.name == name }) throw DeviceException.Exists()
        val parentUri = DocumentsContract.buildDocumentUriUsingTree(tree, parent)
        val created = DocumentsContract.createDocument(resolver, parentUri, "application/octet-stream", name)
            ?: throw DeviceException.Failed("i/o error")
        try {
            val output = resolver.openOutputStream(created, "w") ?: throw IOException("no stream")
            output.use { it.write(data) }
        } catch (e: IOException) {
            discard(created)
            throw e
        }
    }

    /**
     * Overwrites an existing file for the in-app editor. Refuses with a conflict when the file
     * changed since [expectedSha] (SHA-256 hex of the text that was loaded). Not part of the
     * agent-facing [FileAccess]: agent writes stay create-only.
     */
    fun replace(rootId: String, path: String, data: ByteArray, expectedSha: String) = guard {
        val normalized = normalizeSafPath(path)
        if (data.size.toLong() > SAF_MAX_TRANSFER_BYTES) throw DeviceException.TooLarge()
        if (normalized.isEmpty()) throw DeviceException.Invalid("path is a directory")
        val tree = treeFor(rootId)
        val child = resolveChild(tree, normalized)
        if (child.isDir) throw DeviceException.Invalid("path is a directory")
        val current = read(rootId, normalized, SAF_MAX_TRANSFER_BYTES.toULong())
        if (sha256Hex(current) != expectedSha) throw DeviceException.Failed("conflict")
        val uri = DocumentsContract.buildDocumentUriUsingTree(tree, child.docId)
        val output = resolver.openOutputStream(uri, "wt") ?: throw DeviceException.Failed("i/o error")
        output.use { it.write(data) }
    }

    private fun discard(uri: Uri) {
        try {
            DocumentsContract.deleteDocument(resolver, uri)
        } catch (_: Exception) {
            // Best effort cleanup of a half-written file.
        }
    }

    private inline fun <T> guard(block: () -> T): T =
        try {
            block()
        } catch (_: SecurityException) {
            throw DeviceException.Failed("permission revoked")
        } catch (_: IOException) {
            throw DeviceException.Failed("i/o error")
        }

    private fun readStored(id: String): Stored? =
        try {
            val parts = prefs.getString(id, null)?.split('\t')
            if (parts == null || parts.size != 3 || rootIdFor(parts[0]) != id) {
                null
            } else {
                Stored(parts[0], parts[1], parts[2] == "true")
            }
        } catch (_: ClassCastException) {
            null
        }

    private fun treeFor(rootId: String): Uri {
        val stored = readStored(rootId) ?: throw DeviceException.NotFound()
        val uri = Uri.parse(stored.uri)
        val live = resolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }
        if (!live) throw DeviceException.NotFound()
        return uri
    }

    private fun resolveDir(tree: Uri, path: String): String {
        var current = DocumentsContract.getTreeDocumentId(tree)
        if (path.isEmpty()) return current
        for (segment in path.split('/')) {
            val child = children(tree, current).firstOrNull { it.name == segment }
                ?: throw DeviceException.NotFound()
            if (!child.isDir) throw DeviceException.NotFound()
            current = child.docId
        }
        return current
    }

    private fun resolveChild(tree: Uri, path: String): Child {
        val slash = path.lastIndexOf('/')
        val parent = resolveDir(tree, if (slash < 0) "" else path.substring(0, slash))
        val name = path.substring(slash + 1)
        return children(tree, parent).firstOrNull { it.name == name } ?: throw DeviceException.NotFound()
    }

    private fun children(tree: Uri, parentDocId: String): List<Child> {
        val uri = DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentDocId)
        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
        val result = ArrayList<Child>()
        val cursor = resolver.query(uri, projection, null, null, null) ?: throw DeviceException.Failed("i/o error")
        cursor.use {
            while (it.moveToNext()) {
                val docId = it.getString(0) ?: continue
                val name = it.getString(1) ?: continue
                val mime = it.getString(2)
                result.add(
                    Child(
                        docId = docId,
                        name = name,
                        isDir = mime == DocumentsContract.Document.MIME_TYPE_DIR,
                        size = if (it.isNull(3)) 0L else it.getLong(3),
                        modified = if (it.isNull(4)) null else it.getLong(4),
                    ),
                )
            }
        }
        return result
    }
}

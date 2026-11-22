package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.security.SecureRandom
import kotlin.coroutines.coroutineContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ImportCheck
import uniffi.harw_mobile_ffi.ImportVerdict
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ModelProgress
import uniffi.harw_mobile_ffi.commitImport
import uniffi.harw_mobile_ffi.discardImport
import uniffi.harw_mobile_ffi.importModel

internal const val ExternalStorageAuthority = "com.android.externalstorage.documents"
internal const val HiddenTreeDocId = "primary:Documents/.harw"
internal const val DocumentsTreeDocId = "primary:Documents"
internal const val ImportCopyBufferBytes = 1 shl 20
internal const val StagingPrefix = ".harw-import-"
internal const val StagingSuffix = ".gguf.part"
internal const val ErrNameInUse = "model name already in use"
internal const val ErrCancelled = "model check cancelled"
internal const val ErrNotGguf = "not a GGUF file"

private const val StagingRandomBytes = 8
private const val MaxAlternateName = 10
private const val HexDigits = "0123456789abcdef"
private const val GgufSuffix = ".gguf"
private const val SidecarSuffix = ".json"

/** Staging file name for an import copy: prefix, 16 lowercase hex digits (8 bytes), suffix. */
internal fun stagingName(random: ByteArray): String {
    val hex = StringBuilder(random.size * 2)
    for (byte in random) {
        hex.append(HexDigits[(byte.toInt() shr 4) and 0xF])
        hex.append(HexDigits[byte.toInt() and 0xF])
    }
    return StagingPrefix + hex + StagingSuffix
}

/**
 * Document ids that may hold the model folder, in order of preference, for a granted tree.
 * The user may pick the models folder itself, its parent, or the whole Documents folder.
 */
internal fun candidateModelDirDocIds(treeDocId: String): List<String> {
    val id = treeDocId.trimEnd('/')
    return when {
        id.endsWith("/.harw/models") || id.endsWith("/harw/models") -> listOf(id)
        id.endsWith("/.harw") || id.endsWith("/harw") -> listOf("$id/models")
        id == DocumentsTreeDocId || id.endsWith("/Documents") || id.endsWith(":Documents") ->
            listOf("$id/.harw/models", "$id/harw/models")
        else -> listOf(id)
    }
}

internal fun isGgufName(displayName: String): Boolean =
    !displayName.startsWith(".") && displayName.endsWith(GgufSuffix, ignoreCase = true)

/** "X.gguf" -> "X-2.gguf" (n = 2); the stem stays, only one counter is ever appended. */
internal fun alternateName(finalName: String, n: Int): String {
    val dot = finalName.lastIndexOf('.')
    return if (dot <= 0) "$finalName-$n" else finalName.substring(0, dot) + "-" + n + finalName.substring(dot)
}

internal enum class ImportPhase { Copying, Verifying, Replacing }

internal data class ImportProgress(val phase: ImportPhase, val done: Long, val total: Long)

internal data class ForeignModel(
    val documentUri: Uri,
    val sidecarUri: Uri?,
    val displayName: String,
    val sizeBytes: Long,
)

internal sealed interface ImportResult {
    data class Imported(
        val model: LocalModel,
        val check: ImportCheck,
        val originalDeleted: Boolean,
    ) : ImportResult

    data class AlreadyPresent(val modelId: String) : ImportResult

    data class Mismatch(val label: String, val expectedSize: Long, val actualSize: Long) : ImportResult

    data class NoSpace(val neededBytes: Long, val freeBytes: Long) : ImportResult

    data object NotGguf : ImportResult

    data object Busy : ImportResult

    data object Cancelled : ImportResult

    data class Failed(val message: String) : ImportResult
}

private class CopyCancelled : Exception()

internal class ModelImport(context: Context, private val store: ModelStore) {
    private val appContext = context.applicationContext
    private val resolver get() = appContext.contentResolver
    private val random = SecureRandom()

    private fun treeIntent(docId: String): Intent =
        Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            putExtra(
                DocumentsContract.EXTRA_INITIAL_URI,
                DocumentsContract.buildDocumentUri(ExternalStorageAuthority, docId),
            )
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        }

    /** Tree picker starting at Documents/.harw. The grant is used once and never made persistent. */
    fun reimportTreeIntent(): Intent = treeIntent(HiddenTreeDocId)

    /** Fallback when the dot folder cannot be chosen: grant on Documents, the models folder is resolved below it. */
    fun documentsTreeIntent(): Intent = treeIntent(DocumentsTreeDocId)

    fun importFileIntent(): Intent =
        Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
        }

    suspend fun findForeignModels(treeUri: Uri): List<ForeignModel> =
        withContext(Dispatchers.IO) {
            val treeDocId = try {
                DocumentsContract.getTreeDocumentId(treeUri)
            } catch (e: IllegalArgumentException) {
                return@withContext emptyList()
            }
            for (docId in candidateModelDirDocIds(treeDocId)) {
                val found = listChildren(treeUri, docId)
                if (found.isNotEmpty()) return@withContext found
            }
            emptyList()
        }

    private class Entry(val docId: String, val name: String, val size: Long, val mime: String?)

    private fun listChildren(treeUri: Uri, parentDocId: String): List<ForeignModel> {
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocId)
        val projection = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
        )
        val entries = ArrayList<Entry>()
        try {
            resolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
                while (cursor.moveToNext()) {
                    val name = cursor.getString(1) ?: continue
                    entries.add(Entry(cursor.getString(0), name, if (cursor.isNull(2)) -1L else cursor.getLong(2), cursor.getString(3)))
                }
            }
        } catch (e: Exception) {
            // Missing folder or provider refusal: the next candidate is tried.
            return emptyList()
        }
        val byName = entries.associateBy { it.name }
        return entries
            .filter { it.mime != DocumentsContract.Document.MIME_TYPE_DIR && isGgufName(it.name) }
            .map { entry ->
                val sidecar = byName[entry.name + SidecarSuffix]
                ForeignModel(
                    documentUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, entry.docId),
                    sidecarUri = sidecar?.let { DocumentsContract.buildDocumentUriUsingTree(treeUri, it.docId) },
                    displayName = entry.name,
                    sizeBytes = entry.size,
                )
            }
            .sortedBy { it.displayName }
    }

    suspend fun reimport(model: ForeignModel, onProgress: (ImportProgress) -> Unit): ImportResult =
        runImport(model.documentUri, model.displayName, model.sizeBytes, model, onProgress)

    suspend fun importDocument(uri: Uri, onProgress: (ImportProgress) -> Unit): ImportResult {
        val (name, size) = withContext(Dispatchers.IO) { queryNameAndSize(uri) }
        if (name == null) return ImportResult.Failed("file name unavailable")
        return runImport(uri, name, size, null, onProgress)
    }

    private fun queryNameAndSize(uri: Uri): Pair<String?, Long> {
        try {
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val name = cursor.getString(0)
                    val size = if (cursor.isNull(1)) -1L else cursor.getLong(1)
                    return name to size
                }
            }
        } catch (e: Exception) {
            // fall through
        }
        return null to -1L
    }

    /** Removes all staging copies; skipped while a download or import holds the folder. */
    suspend fun cleanStaging() {
        if (!ModelDirLock.mutex.tryLock()) return
        try {
            withContext(Dispatchers.IO) {
                store.modelsDir().listFiles()?.forEach { file ->
                    if (file.name.startsWith(StagingPrefix)) file.delete()
                }
            }
        } finally {
            ModelDirLock.mutex.unlock()
        }
    }

    // reimport: foreign != null (original is deleted after the copy was checked). importDocument: foreign == null.
    private suspend fun runImport(
        uri: Uri,
        displayName: String,
        knownSize: Long,
        foreign: ForeignModel?,
        onProgress: (ImportProgress) -> Unit,
    ): ImportResult {
        if (!ModelDirLock.mutex.tryLock()) return ImportResult.Busy
        try {
            return withContext(Dispatchers.IO) { import(uri, displayName, knownSize, foreign, onProgress) }
        } finally {
            ModelDirLock.mutex.unlock()
        }
    }

    private suspend fun import(
        uri: Uri,
        displayName: String,
        knownSize: Long,
        foreign: ForeignModel?,
        onProgress: (ImportProgress) -> Unit,
    ): ImportResult {
        val dir = store.modelsDir()
        if (knownSize > 0) {
            val free = store.freeBytes()
            val needed = knownSize + StorageMarginBytes
            if (free < needed) return ImportResult.NoSpace(needed, free)
        }
        val staged = stagingName(ByteArray(StagingRandomBytes).also { random.nextBytes(it) })
        val job = coroutineContext[Job]
        try {
            copyToStaging(uri, File(dir, staged), knownSize, onProgress)
            val progress = object : ModelProgress {
                override fun onProgress(doneBytes: ULong, totalBytes: ULong): Boolean {
                    onProgress(ImportProgress(ImportPhase.Verifying, doneBytes.toLong(), totalBytes.toLong()))
                    return job?.isActive ?: true
                }
            }
            val check = importModel(dir.path, staged, displayName, progress)
            when (val verdict = check.verdict) {
                is ImportVerdict.Mismatch ->
                    return ImportResult.Mismatch(verdict.label, verdict.expectedSize.toLong(), verdict.actualSize.toLong())
                is ImportVerdict.AlreadyPresent -> return ImportResult.AlreadyPresent(verdict.modelId)
                is ImportVerdict.Verified, is ImportVerdict.Unverified -> Unit
            }
            onProgress(ImportProgress(ImportPhase.Replacing, 0, 1))
            val originalDeleted = if (foreign != null) deleteOriginal(foreign) else false
            val model = commit(dir, staged, check)
            if (model == null) {
                discard(dir, staged)
                return ImportResult.Failed(ErrNameInUse)
            }
            onProgress(ImportProgress(ImportPhase.Replacing, 1, 1))
            return ImportResult.Imported(model, check, originalDeleted)
        } catch (e: CopyCancelled) {
            discard(dir, staged)
            return ImportResult.Cancelled
        } catch (e: CancellationException) {
            discard(dir, staged)
            throw e
        } catch (e: MobileException) {
            val text = e.message ?: "import failed"
            return when {
                text.contains(ErrCancelled) -> { discard(dir, staged); ImportResult.Cancelled }
                text.contains(ErrNotGguf) -> { discard(dir, staged); ImportResult.NotGguf }
                else -> { discard(dir, staged); ImportResult.Failed(text) }
            }
        } catch (e: Exception) {
            discard(dir, staged)
            return ImportResult.Failed(e.message ?: "import failed")
        }
    }

    private suspend fun copyToStaging(uri: Uri, target: File, total: Long, onProgress: (ImportProgress) -> Unit) {
        val input = resolver.openInputStream(uri) ?: throw IOException("file could not be opened")
        input.use { source ->
            FileOutputStream(target).use { out ->
                val buffer = ByteArray(ImportCopyBufferBytes)
                var done = 0L
                onProgress(ImportProgress(ImportPhase.Copying, 0, total))
                while (true) {
                    if (!coroutineContext[Job].let { it?.isActive ?: true }) throw CopyCancelled()
                    val read = source.read(buffer)
                    if (read < 0) break
                    out.write(buffer, 0, read)
                    done += read
                    onProgress(ImportProgress(ImportPhase.Copying, done, total))
                }
                out.flush()
                out.fd.sync()
            }
        }
    }

    // Best effort: the original is the owner's data, a failure only means the space stays in use.
    private fun deleteOriginal(foreign: ForeignModel): Boolean {
        val ok = try {
            DocumentsContract.deleteDocument(resolver, foreign.documentUri)
        } catch (e: Exception) {
            false
        }
        val sidecar = foreign.sidecarUri
        if (sidecar != null) {
            try {
                DocumentsContract.deleteDocument(resolver, sidecar)
            } catch (e: Exception) {
                // sidecar leftovers are harmless
            }
        }
        return ok
    }

    // Returns null when every name is taken; the caller then discards the staging copy.
    private fun commit(dir: File, staged: String, check: ImportCheck): LocalModel? {
        var name = check.finalName
        var attempt = 1
        while (true) {
            try {
                return commitImport(dir.path, staged, name)
            } catch (e: MobileException) {
                val text = e.message ?: ""
                if (!text.contains(ErrNameInUse)) throw e
                attempt += 1
                if (attempt > MaxAlternateName) return null
                name = alternateName(check.finalName, attempt)
            }
        }
    }

    private fun discard(dir: File, staged: String) {
        try {
            discardImport(dir.path, staged)
        } catch (e: Exception) {
            File(dir, staged).delete()
            File(dir, staged + SidecarSuffix).delete()
        }
    }
}
